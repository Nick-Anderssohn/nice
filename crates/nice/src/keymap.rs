//! Keymap wiring — turns the gpui-free `nice_model::shortcuts` table into gpui
//! [`actions!`](gpui::actions) + key bindings, and routes each action to its
//! handler. The Rust replacement for Swift's process-wide
//! `KeyboardShortcutMonitor` (`Sources/Nice/Process/KeyboardShortcutMonitor.swift`)
//! — but built on GPUI's action/keymap dispatch, not an `NSEvent` monitor (the
//! plan's DO-NOT-PORT list forbids re-creating the monitor).
//!
//! ## What lives here
//!
//! * Every rebindable action as a gpui action struct (generated to mirror
//!   [`ShortcutAction`], one struct per case).
//! * [`install_shortcuts`] — the one-shot app wiring: it hoists the terminal
//!   [`FontSettings`] to a single process-level entity, registers every action's
//!   handler, and binds every default combo (plus the non-rebindable ⌃⌘F full
//!   screen accelerator) from the table.
//! * The peek trigger's clear half ([`on_window_modifiers_changed`]) — the
//!   window-level modifier-release observer the shipped window installs.
//!
//! ## App-level vs window-level dispatch (the plan's "dispatch-order" decision)
//!
//! Swift's monitor dispatched font + undo/redo **before** the focused-window
//! lookup, so ⌘=/⌘−/⌘0 zoom (and, later, undo/redo) fire even when no Nice main
//! window is key. GPUI gives us that split for free: those actions get
//! **app-level** [`cx.on_action`](gpui::App::on_action) handlers that touch only
//! the process-level [`FontSettings`] (or, for undo/redo, the future shared
//! history) and never look a window up. The **window-scoped** actions (sidebar
//! toggles, the session-cycle, window stepping, the new-window path) route through
//! [`WindowRegistry::active_state`] — the key window, else the most-recently
//! keyed, else the first registered — exactly Swift's
//! `registry.activeAppState(preferKey: true)`.
//!
//! Font zoom **fans out to every window**: there is one process-level
//! `FontSettings` entity, observed by every [`TerminalView`], so a single zoom
//! re-metrics every open window's grid (the plan's requirement; it replaces the
//! per-window `FontSettings` the app previously minted inside each window
//! builder).
//!
//! ## Documented divergence — character-based matching at the gpui pin
//!
//! Swift matched layout-independent physical `keyCode`s. GPUI keymaps match on
//! the produced key **character**; there is no keycode-binding API at the pin
//! (verified). So the combos are bound from the [`ShortcutAction`] table's gpui
//! key *tokens* with **`use_key_equivalents` semantics** (via
//! [`KeyBinding::load`] + the app's [`PlatformKeyboardMapper`]), which is how
//! GPUI reproduces macOS key-equivalent behavior across layouts. This mirrors
//! `nice_model::shortcuts`'s own divergence note; full layout-parity is R24's
//! question (it owns rebinding). We do not patch gpui for this — a pin change is
//! a human decision.
//!
//! The pass-through contract (dossier G10) falls out of GPUI's dispatch order
//! (`window.rs` — actions → key listeners → input handler): a keystroke that
//! matches a binding fires the action, which stops propagation by default, so it
//! never reaches the terminal's key-down listener and leaks **zero** bytes into
//! the pty; an unmatched keystroke falls through to the terminal's platform
//! input path byte-identically. Kitty-CSI-u encoding stays entirely in R5's
//! layer — this layer never encodes.

use gpui::{
    Action, App, AppContext, Context, Entity, Global, InvalidKeystrokeError, KeyBinding, Modifiers,
    ModifiersChangedEvent, PlatformKeyboardMapper,
};

use nice_model::shortcuts::{
    default_bindings, default_combo, OwnedCombo, ShortcutAction, RESERVED_CLOSE_WINDOW,
    RESERVED_NEW_WINDOW, RESERVED_OPEN_SETTINGS, RESERVED_QUIT, RESERVED_TOGGLE_FULL_SCREEN,
    WINDOW_INDEX_KEYS,
};
use nice_model::SidebarMode;
use nice_term_view::FontSettings;

use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// The rebindable actions, one gpui action struct per `ShortcutAction` case
// (same names, `nice` namespace). `actions!` needs compile-time identifiers, so
// the set is spelled out here; [`shortcut_binding`] maps each `ShortcutAction`
// value back to its struct so the *bindings* are still generated from the table.
// The completeness test below pins that this list and `ShortcutAction::ALL` stay
// in lockstep.
gpui::actions!(
    nice,
    [
        NextSidebarSession,
        PrevSidebarSession,
        NextWindow,
        PrevWindow,
        NewTerminalWindow,
        ToggleSidebar,
        ToggleSidebarMode,
        ToggleHiddenFiles,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSizes,
        UndoFileOperation,
        RedoFileOperation,
        CommandCompose,
        FocusPaneLeft,
        FocusPaneDown,
        FocusPaneUp,
        FocusPaneRight,
        LastActiveWindow,
        ScrollHalfPageUp,
        ScrollHalfPageDown,
    ]
);

/// The action [`ShortcutAction::WindowByIndex`]'s nine bindings dispatch (D2) —
/// data-carrying, because ONE settings row stands for ⌃⌘1…⌃⌘9 and the handler
/// needs to know which digit was pressed. [`KeyBinding::load`] takes a
/// `Box<dyn Action>`, so nine bindings each carrying a distinct `index` coexist and
/// `cx.on_action::<SelectWindowIndex>` receives the bound instance.
///
/// `no_json` skips the derive's serde/schemars requirement (`crates/nice` has no
/// schemars dependency) — this action is never built from a JSON keymap file.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = nice, no_json)]
pub(crate) struct SelectWindowIndex {
    /// The 1-based window slot to focus (1..=9), matching the pressed digit.
    pub(crate) index: u8,
}

/// The single process-level terminal [`FontSettings`], hoisted out of the
/// per-window builder so one entity is observed by every window's
/// [`TerminalView`] — a ⌘=/⌘−/⌘0 zoom fans out to all of them (the plan's font
/// fan-out). Installed by [`install_shortcuts`]; read by the window builder
/// (`crate::app::open_managed_window`) and the font action handlers.
struct SharedFontSettings(Entity<FontSettings>);

impl Global for SharedFontSettings {}

/// One-shot install guard. The shipped app calls [`install_shortcuts`] exactly
/// once, but the self-test suite runs every scenario in ONE process and several
/// scenarios (`niceties-zoom`, `multiwindow`) each install the keymap before
/// opening their window; without this guard a second install would re-register
/// every `cx.on_action` handler, so every dispatch would fire twice (a ⌘B toggle
/// would net-cancel, a ⌘= would zoom twice). The presence of this global marks the
/// keymap installed.
struct ShortcutsInstalled;

impl Global for ShortcutsInstalled {}

/// The process-level terminal font entity. Panics only if called before
/// [`install_shortcuts`] set it; the app installs shortcuts before opening any
/// window, so every caller runs after it exists.
pub(crate) fn shared_font_settings(cx: &App) -> Entity<FontSettings> {
    cx.global::<SharedFontSettings>().0.clone()
}

/// The process-level terminal font entity, if installed (`None` before
/// [`install_shortcuts`]). R23's Font pane uses this defensive accessor so a pane
/// build never panics when the keymap has not been wired.
pub(crate) fn try_shared_font_settings(cx: &App) -> Option<Entity<FontSettings>> {
    cx.try_global::<SharedFontSettings>().map(|g| g.0.clone())
}

/// Wire the app-wide shortcut system, once, from `crate::app::run` (and from the
/// self-test scenarios that drive the shortcuts) before the first window opens:
///
/// 1. hoist [`FontSettings`] to one process-level entity (the font fan-out);
/// 2. register every action's handler (app-level font/undo/redo; window-scoped
///    sidebar/window actions through [`WindowRegistry::active_state`]);
/// 3. bind every default combo from the table, plus ⌃⌘F full screen, with
///    `use_key_equivalents` semantics.
///
/// Idempotent: the shipped app calls it once, but the self-test suite has several
/// scenarios install it in one process (see [`ShortcutsInstalled`]); a second call
/// early-returns so the action handlers, bindings, and the shared [`FontSettings`]
/// are each set up exactly once per process.
pub(crate) fn install_shortcuts(cx: &mut App) {
    if cx.try_global::<ShortcutsInstalled>().is_some() {
        return;
    }
    cx.set_global(ShortcutsInstalled);

    // R23: seed the shared terminal font from the persisted `fonts` section (loaded
    // into `SettingsPrefsStore` before this in `app::run`; a defaults+temp store in
    // `run_selftest`). Values are copied out first so the store borrow ends before
    // `cx.new`. Applying them inside the constructor (before any subscriber exists)
    // means the seed emits no observable `FontZoom` to the sidebar.
    let (seed_px, seed_family, seed_sidebar_px, seed_line_height) =
        match cx.try_global::<crate::settings::prefs_store::SettingsPrefsStore>() {
            Some(store) => (
                store.terminal_font_px(),
                store.terminal_font_family(),
                store.sidebar_font_px(),
                store.terminal_line_height(),
            ),
            None => (None, None, None, None),
        };
    let font = cx.new(|cx| {
        let mut f = FontSettings::resolved_default(cx);
        if let Some(px) = seed_px {
            f.set_px(px, cx);
        }
        if let Some(fam) = &seed_family {
            f.set_family(Some(fam.clone().into()), cx);
        }
        // Absent line-height ⇒ leave the shipped default (1.3); a stored value
        // (the existing-user migration pins the legacy 1.0) overrides it.
        if let Some(lh) = seed_line_height {
            f.set_line_height(lh, cx);
        }
        f
    });
    cx.set_global(SharedFontSettings(font.clone()));

    // R23 (D3): the app-level sidebar-font entity, coupled to the terminal
    // `FontZoom` for the proportional rescale, seeded from the persisted sidebar
    // size. Installed alongside the terminal font so the sidebar chrome + the Font
    // pane can always read it.
    let terminal_px = font.read(cx).px();
    let sidebar_px =
        seed_sidebar_px.unwrap_or(crate::settings::sidebar_font::DEFAULT_SIDEBAR_FONT_PX);
    let sidebar = cx.new(|cx| {
        crate::settings::sidebar_font::SharedSidebarFontSettings::new(
            sidebar_px,
            terminal_px,
            &font,
            cx,
        )
    });
    cx.set_global(crate::settings::sidebar_font::SharedSidebarFont(sidebar));

    register_app_level_actions(cx);
    register_window_scoped_actions(cx);

    // The sidebar shell's Esc binding (`CollapseSidebarSelection`, context
    // "SidebarShell") — cancels an in-flight inline rename / collapses a
    // multi-selection, else propagates so Esc still reaches the terminal.
    // Installed here so the shipped app (and every scenario that installs the
    // keymap) gets it; it previously had no caller, so Esc-cancel never fired
    // live (M2 Item D acceptance: "Escape cancels").
    crate::sidebar_shell::install_sidebar_key_bindings(cx);

    let mut bindings = table_bindings(cx);
    // Fold R9's ⌃⌘F (non-rebindable — not in the rebindable table) into the same
    // wiring, spelled by its reserved-table entry so the boot install, the rebuild
    // re-install, and the recorder's guard all read one chord. Its handler + the
    // View-menu title live in `crate::app`.
    bindings.push(load_binding(
        &RESERVED_TOGGLE_FULL_SCREEN.combo.chord_str(),
        crate::app::ToggleFullScreen,
        cx.keyboard_mapper().as_ref(),
    ));
    cx.bind_keys(bindings);
}

/// Rebuild the live gpui keymap from the current
/// [`ShortcutBindings`](crate::shortcuts_store::ShortcutBindings) map (G7, D2) — the
/// SOLE caller of `clear_key_bindings()` + `bind_keys(..)` on any binding change
/// (boot-load, rebind, per-action reset). gpui has no per-binding remove; the only
/// live-rebind primitive is a total clear followed by a re-bind of the full set
/// (both queue `Effect::RefreshWindows`), so a single owner is the only way to keep
/// the non-rebindable re-install honest.
///
/// It re-emits, on every rebuild:
///
/// * the LIVE rebindable combos from the store (an unbound action is omitted; a
///   persisted/user token that fails to parse is logged and skipped, never a panic
///   — unlike the statically-valid defaults table); and
/// * the PROTECTED non-rebindable set (see [`non_rebindable_bindings`]) — because
///   `clear_key_bindings()` is TOTAL, it wipes those too, so they must be restored
///   here or they vanish after the first rebind (the plan's biggest regression risk).
///
/// It does NOT re-register action handlers — those stay one-shot in
/// [`install_shortcuts`] (double-registration double-fires). When no
/// [`ShortcutBindings`](crate::shortcuts_store::ShortcutBindings) Global is installed
/// (a scenario that never seeded it), the live combos fall back to the defaults
/// table, so the rebuilt board matches the boot board.
pub(crate) fn rebuild_keymap(cx: &mut App) {
    let mapper = cx.keyboard_mapper().clone();
    let mut bindings: Vec<KeyBinding> = Vec::new();

    match cx.try_global::<crate::shortcuts_store::ShortcutBindings>() {
        Some(store) => {
            // Snapshot the live (action, token) pairs so the immutable store borrow
            // ends before we build bindings and before the mutable clear/bind below.
            let live: Vec<(ShortcutAction, String)> = ShortcutAction::ALL
                .into_iter()
                .filter_map(|action| store.binding(action).map(|c| (action, c.to_token())))
                .collect();
            for (action, token) in live {
                match shortcut_binding(action, &token, mapper.as_ref()) {
                    Ok(expanded) => bindings.extend(expanded),
                    Err(e) => eprintln!(
                        "nice: skipping unparseable shortcut token {token:?} for {action:?}: {e}"
                    ),
                }
            }
        }
        // No store yet ⇒ the default board (the try_global-with-defaults idiom).
        None => bindings.extend(table_bindings(cx)),
    }

    bindings.extend(non_rebindable_bindings(cx));

    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

/// The PROTECTED non-rebindable re-install set (R24 — getting this wrong is the
/// plan's biggest regression risk). [`rebuild_keymap`]'s `clear_key_bindings()` is
/// total — it wipes EVERY binding, not just the rebindable ones — so each must be
/// re-emitted on every rebuild exactly as its owner originally installed it
/// (the `use_key_equivalents` bit differs per entry). A missing entry is a shipped
/// regression caught by the dedicated re-install test.
///
/// The five chords also live in `nice_model::shortcuts::RESERVED_COMBOS` (the
/// recorder's guard, group a), so they are installed FROM those entries — one
/// spelling of ⌘Q / ⌘N / ⌘W / ⌘, / ⌃⌘F, shared by the install and the guard, and
/// no way for the two to drift. Only the sixth, context-scoped Esc entry is
/// keymap-only: plain Escape cancels a capture before any reserved lookup runs,
/// so it can never be recorded and needs no table row.
fn non_rebindable_bindings(cx: &App) -> Vec<KeyBinding> {
    vec![
        // 1. ⌃⌘F full screen — `load_binding` (`use_key_equivalents=true` + the
        //    keyboard mapper), mirroring `install_shortcuts`.
        load_binding(
            &RESERVED_TOGGLE_FULL_SCREEN.combo.chord_str(),
            crate::app::ToggleFullScreen,
            cx.keyboard_mapper().as_ref(),
        ),
        // 2. ⌘N new window — `KeyBinding::new`, `crate::app::install_new_window_command`.
        KeyBinding::new(
            &RESERVED_NEW_WINDOW.combo.chord_str(),
            crate::app::NewWindow,
            None,
        ),
        // 3. ⌘Q quit — `KeyBinding::new`, `crate::app::install_lifecycle_commands`.
        KeyBinding::new(&RESERVED_QUIT.combo.chord_str(), crate::app::Quit, None),
        // 4. ⌘W close window — `KeyBinding::new`, same.
        KeyBinding::new(
            &RESERVED_CLOSE_WINDOW.combo.chord_str(),
            crate::app::CloseWindow,
            None,
        ),
        // 5. Esc collapse-sidebar-selection — `KeyBinding::new`, `Some("SidebarShell")`
        //    context, `crate::sidebar_shell::install_sidebar_key_bindings`. The one
        //    entry with no reserved-table row (see the doc comment).
        KeyBinding::new(
            "escape",
            crate::sidebar_shell::CollapseSidebarSelection,
            Some("SidebarShell"),
        ),
        // 6. ⌘, open settings — `KeyBinding::new`, R23's
        //    `crate::settings::window::install_open_settings_command`.
        KeyBinding::new(
            &RESERVED_OPEN_SETTINGS.combo.chord_str(),
            crate::settings::window::OpenSettings,
            None,
        ),
    ]
}

// -- handlers ----------------------------------------------------------------

/// App-level handlers: the actions that must fire even with no Nice main window
/// key (Swift dispatches them before the window lookup).
fn register_app_level_actions(cx: &mut App) {
    // Font zoom → the process-level `FontSettings` (fans out to every window).
    cx.on_action(|_: &IncreaseFontSize, cx: &mut App| zoom_shared_font(cx, 1));
    cx.on_action(|_: &DecreaseFontSize, cx: &mut App| zoom_shared_font(cx, -1));
    cx.on_action(|_: &ResetFontSizes, cx: &mut App| reset_shared_font(cx));

    // Undo / redo file operations (R20). App-level + unconditional (Swift parity,
    // `KeyboardShortcutMonitor.swift:128-133`): the chords dispatch through the ONE
    // process-wide `FileOperationHistory` BEFORE any focused-window/terminal
    // routing, so ⌘Z in window B undoes window A's op and the chord is consumed even
    // with a terminal focused and even on an empty stack (the history's own no-op).
    // The history follows focus back to the originating window internally. No
    // Global (a scenario that never installed one) ⇒ inert, like the font handlers.
    cx.on_action(|_: &UndoFileOperation, cx: &mut App| dispatch_file_history(cx, true));
    cx.on_action(|_: &RedoFileOperation, cx: &mut App| dispatch_file_history(cx, false));
}

/// Window-scoped handlers: they mutate the focused window's [`WindowState`],
/// resolved through [`WindowRegistry::active_state`] (key window → most-recently
/// keyed → first). Matches Swift's window-scoped `dispatch(action, on: appState)`.
fn register_window_scoped_actions(cx: &mut App) {
    cx.on_action(|_: &NextSidebarSession, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            s.workspace.select_next_sidebar_session();
            // Re-sync the selection to the new active session (Swift's active-session
            // observer) — else the prior-active row lingers as a dim highlight.
            s.sync_selection_to_active_session();
            trigger_peek_if_collapsed(s);
        });
    });
    cx.on_action(|_: &PrevSidebarSession, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            s.workspace.select_prev_sidebar_session();
            s.sync_selection_to_active_session();
            trigger_peek_if_collapsed(s);
        });
    });
    cx.on_action(|_: &NextWindow, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.window_strip_actions.select_next_window(&mut s.workspace));
    });
    cx.on_action(|_: &PrevWindow, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.window_strip_actions.select_prev_window(&mut s.workspace));
    });
    cx.on_action(|_: &NewTerminalWindow, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            if let Some(active) = s.workspace.active_session_id().map(str::to_owned) {
                s.window_strip_actions.add_terminal_window(&mut s.workspace, &active);
            }
        });
    });
    cx.on_action(|_: &ToggleSidebar, cx: &mut App| {
        // Route through the shared WindowState seam so ⌘B gets the same
        // peek-clear-on-expand + state notification as the titlebar toggle.
        with_active_state(cx, |s, cx| s.toggle_sidebar_collapsed(cx));
    });
    cx.on_action(|_: &ToggleSidebarMode, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            s.sidebar.toggle_sidebar_mode();
            // R19: persist the new per-window sidebar mode so it restores (the
            // debounced upsert; a no-op when no store Global is installed).
            s.save_to_store();
        });
    });
    cx.on_action(|_: &CommandCompose, cx: &mut App| {
        // Command Compose (⌘↩): gate + route in WindowState (the pure
        // `compose_route` core carries the truth table). At an idle zsh prompt
        // it writes the compose trigger to the active window's pty; in a busy /
        // kitty window it replays the ⌘↩ bytes an unbound chord produced before
        // this feature existed.
        with_active_state(cx, |s, cx| s.dispatch_command_compose(cx));
    });
    // -- Phase 1 (tmux port): the held-⌃⌘ scheme ------------------------------
    //
    // D3: the four `FocusPane*` actions are named for their PHASE 2 spatial
    // meaning (tmux `select-pane -L/-D/-U/-R` over the future split tree).
    // Pre-splits the only spatial axis is the pill strip, so left/right alias
    // prev/next window; Phase 2 swaps these handler bodies without touching a
    // single binding or migrating anything.
    cx.on_action(|_: &FocusPaneLeft, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.window_strip_actions.select_prev_window(&mut s.workspace));
    });
    cx.on_action(|_: &FocusPaneRight, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.window_strip_actions.select_next_window(&mut s.workspace));
    });
    // D3: registered but INERT until Phase 2 lands splits — there is nothing
    // stacked vertically to move between yet. Registered rather than omitted so
    // the chord is consumed by the keymap instead of leaking into the pty, and so
    // Phase 2 only has to fill these bodies in.
    cx.on_action(|_: &FocusPaneUp, _cx: &mut App| {});
    cx.on_action(|_: &FocusPaneDown, _cx: &mut App| {});
    cx.on_action(|_: &LastActiveWindow, cx: &mut App| {
        with_active_state(cx, |s, _cx| last_active_window(&mut s.workspace));
    });
    cx.on_action(|_: &ScrollHalfPageUp, cx: &mut App| scroll_active_window_half_page(cx, true));
    cx.on_action(|_: &ScrollHalfPageDown, cx: &mut App| scroll_active_window_half_page(cx, false));
    cx.on_action(|action: &SelectWindowIndex, cx: &mut App| {
        let index = action.index;
        with_active_state(cx, |s, _cx| {
            s.window_strip_actions.select_window_by_index(&mut s.workspace, index)
        });
    });
    cx.on_action(|_: &ToggleHiddenFiles, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            // R19: the Swift double gate (`appState.toggleFileBrowserHiddenFiles()`)
            // — flip dotfile visibility for the active session ONLY when the sidebar is
            // in files mode AND a browser state already exists for that session
            // (`toggle_hidden_files_if_exists` enforces the second gate, allocating
            // nothing when the user never opened the browser). Registered (not
            // silently missing) so ⌘⇧. is consumed rather than leaking to the pty.
            if s.sidebar.mode() != SidebarMode::Files {
                return;
            }
            if let Some(session_id) = s.workspace.active_session_id().map(str::to_owned) {
                s.file_browser.toggle_hidden_files_if_exists(&session_id);
            }
        });
    });
}

/// tmux `last-window` (⌃⌘O): bounce the active session back to the window that was
/// active before the current one, and make the current one the new bounce target.
/// The swap falls out of [`Session::switch_active_window`] itself — it stashes the
/// window it displaces — so this only has to resolve and re-select the target.
///
/// A no-op when there is no active session, no previous window was recorded (a
/// fresh session, or one that never switched), or the recorded window has since
/// closed ([`Session::last_active_window_id`] filters that).
///
/// [`Session::switch_active_window`]: nice_model::Session::switch_active_window
/// [`Session::last_active_window_id`]: nice_model::Session::last_active_window_id
fn last_active_window(model: &mut nice_model::WorkspaceModel) {
    let Some(session_id) = model.active_session_id().map(str::to_owned) else {
        return;
    };
    let Some(target) = model
        .session_for(&session_id)
        .and_then(|s| s.last_active_window_id())
        .map(str::to_owned)
    else {
        return;
    };
    model.mutate_session(&session_id, |session| {
        session.switch_active_window(&target);
    });
}

/// Half-page scrollback (⌃⌘U / ⌃⌘D) on the ACTIVE window of the active session.
///
/// No keymap action reaches a terminal view today (Phase 0's Shift+nav scrollback
/// lives in the view's own key listener, not the keymap), so this rides the seam
/// that does exist: [`PtyManager::term_window_handle`] resolves the window's
/// [`TerminalSessionHandle`], and the scroll + `notify` follow
/// `perform_scrollback`'s repaint discipline (`nice-term-view`'s `view.rs`).
///
/// **Alt-screen gate.** On the alternate screen (vim, less, any full-screen TUI)
/// this does nothing at all. The chord is a keymap binding, so it never encoded to
/// the pty and there is nothing to fall through TO — unlike Shift+PageUp, which
/// falls through and encodes.
///
/// Held (not yet spawned) sessions work automatically: the handle's scroll methods
/// no-op pre-spawn.
///
/// [`PtyManager::term_window_handle`]: crate::pty_manager::PtyManager::term_window_handle
/// [`TerminalSessionHandle`]: nice_term_view::TerminalSessionHandle
fn scroll_active_window_half_page(cx: &mut App, toward_history: bool) {
    with_active_state(cx, |s, cx| {
        let Some(session_id) = s.workspace.active_session_id().map(str::to_owned) else {
            return;
        };
        let Some(term_window_id) = s
            .workspace
            .session_for(&session_id)
            .and_then(|sess| sess.active_window_id.clone())
        else {
            return;
        };
        let Some(handle) = s.ptys.term_window_handle(&session_id, &term_window_id) else {
            return;
        };
        if handle.read(cx).is_alt_screen() {
            return;
        }
        handle.update(cx, |h, hcx| {
            if toward_history {
                h.scroll_half_page_up();
            } else {
                h.scroll_half_page_down();
            }
            hcx.notify();
        });
    });
}

/// After a sidebar-session cycle on a collapsed sidebar, float the peek overlay so
/// the user can see which session they're cycling toward (dossier G6). Cleared on
/// modifier release by [`on_window_modifiers_changed`].
fn trigger_peek_if_collapsed(state: &mut WindowState) {
    if state.sidebar.collapsed() {
        state.sidebar.begin_sidebar_peek();
    }
}

/// Route a window-scoped action to the focused window's state, then notify the
/// state entity so every view observing it re-renders. A no-op when no window is
/// registered (e.g. a self-test that installs the keymap but never stands up the
/// [`WindowRegistry`]) — `active_state` returns `None` and the action harmlessly
/// does nothing.
///
/// The trailing `cx.notify()` is what makes the window-scoped shortcuts produce
/// **visible** results in the shipped shell (R13.5): the shell's
/// `AppShellView` / `SidebarShellView` / `WindowToolbarView` / `WindowHostView`
/// each observe this `WindowState` entity, so `gpui::Entity::update` — which does
/// not notify on its own — is followed by an explicit notify. Without it a ⌘T /
/// ⌘S / window-step would mutate state that nothing re-renders (the gap this cycle
/// closes). Harmless where no view observes the state (e.g. the `multiwindow`
/// scenario asserts the model directly).
fn with_active_state(cx: &mut App, f: impl FnOnce(&mut WindowState, &mut Context<WindowState>)) {
    if let Some(state) = WindowRegistry::active_state(cx, true) {
        state.update(cx, |s, cx| {
            f(s, cx);
            cx.notify();
        });
    }
}

/// Drive the process-wide file-operation history's undo (`undo == true`) or redo.
/// A no-op when no [`FileOperationHistoryGlobal`] is installed (a scenario that
/// never stood the history up); the `cx.notify()` inside the entity update wakes
/// the per-window drift-banner observers so a published message renders.
fn dispatch_file_history(cx: &mut App, undo: bool) {
    use crate::file_browser::history::FileOperationHistoryGlobal;
    let Some(history) = cx
        .try_global::<FileOperationHistoryGlobal>()
        .map(|g| g.0.clone())
    else {
        return;
    };
    // Snapshot the live windows for the cx-less focus-follow closure BEFORE the
    // undo/redo, then drive the resolved cross-window focus routes AFTER (both
    // no-ops when no focus router is installed). See `focus_route`.
    crate::file_browser::focus_route::refresh_live(cx);
    history.update(cx, |h, cx| {
        if undo {
            h.undo();
        } else {
            h.redo();
        }
        cx.notify();
    });
    crate::file_browser::focus_route::drive_pending(cx);
}

fn zoom_shared_font(cx: &mut App, delta: i32) {
    let Some(font) = cx.try_global::<SharedFontSettings>().map(|g| g.0.clone()) else {
        return;
    };
    font.update(cx, |f, cx| f.zoom_by(delta, cx));
}

fn reset_shared_font(cx: &mut App) {
    let Some(font) = cx.try_global::<SharedFontSettings>().map(|g| g.0.clone()) else {
        return;
    };
    font.update(cx, |f, cx| f.reset(cx));
}

// -- bindings ----------------------------------------------------------------

/// The default bindings, generated from [`default_bindings`]: each combo's
/// [`chord_str`](nice_model::KeyCombo::chord_str) is bound to its action with
/// `use_key_equivalents` semantics. One row per action, except `WindowByIndex`
/// which expands into nine (see [`shortcut_binding`]).
fn table_bindings(cx: &App) -> Vec<KeyBinding> {
    let mapper = cx.keyboard_mapper().clone();
    default_bindings()
        .into_iter()
        .flat_map(|(action, combo)| {
            // The default chords are static table data, so a parse failure is a
            // programmer error — hence `expect`. (The LIVE path in `rebuild_keymap`
            // instead logs-and-skips, since a persisted token is not statically valid.)
            shortcut_binding(action, &combo.chord_str(), mapper.as_ref())
                .expect("static default shortcut chord parses")
        })
        .collect()
}

/// Build the [`KeyBinding`]s for `action`'s gpui action struct from `chord`, with
/// `use_key_equivalents` semantics and no context predicate (`None` = active in
/// every dispatch context, like Swift's process-wide monitor). The one place the
/// data table meets the compile-time action types; the exhaustive match makes a
/// newly-added `ShortcutAction` a compile error until it is bound here.
///
/// Returns a **vector** because of the [`ShortcutAction::WindowByIndex`] carve-out
/// (D2): every other action yields exactly one binding, but that single row expands
/// into NINE — one per digit in
/// [`WINDOW_INDEX_KEYS`](nice_model::shortcuts::WINDOW_INDEX_KEYS), each carrying its
/// own [`SelectWindowIndex`]. Both keymap paths ([`table_bindings`] and
/// [`rebuild_keymap`]) go through here, so the expansion can't drift between the
/// default board and the live board.
///
/// Fallible: `chord` may be a user-recorded / persisted token (via the LIVE
/// [`rebuild_keymap`] path), which is not statically guaranteed to parse. The
/// caller decides — the static defaults path `expect`s, the live path logs-and-skips.
fn shortcut_binding(
    action: ShortcutAction,
    chord: &str,
    mapper: &dyn PlatformKeyboardMapper,
) -> Result<Vec<KeyBinding>, InvalidKeystrokeError> {
    if action == ShortcutAction::WindowByIndex {
        return window_index_bindings(chord, mapper);
    }
    let boxed: Box<dyn Action> = match action {
        ShortcutAction::NextSidebarSession => Box::new(NextSidebarSession),
        ShortcutAction::PrevSidebarSession => Box::new(PrevSidebarSession),
        ShortcutAction::NextWindow => Box::new(NextWindow),
        ShortcutAction::PrevWindow => Box::new(PrevWindow),
        ShortcutAction::NewTerminalWindow => Box::new(NewTerminalWindow),
        ShortcutAction::ToggleSidebar => Box::new(ToggleSidebar),
        ShortcutAction::ToggleSidebarMode => Box::new(ToggleSidebarMode),
        ShortcutAction::ToggleHiddenFiles => Box::new(ToggleHiddenFiles),
        ShortcutAction::IncreaseFontSize => Box::new(IncreaseFontSize),
        ShortcutAction::DecreaseFontSize => Box::new(DecreaseFontSize),
        ShortcutAction::ResetFontSizes => Box::new(ResetFontSizes),
        ShortcutAction::UndoFileOperation => Box::new(UndoFileOperation),
        ShortcutAction::RedoFileOperation => Box::new(RedoFileOperation),
        ShortcutAction::CommandCompose => Box::new(CommandCompose),
        ShortcutAction::FocusPaneLeft => Box::new(FocusPaneLeft),
        ShortcutAction::FocusPaneDown => Box::new(FocusPaneDown),
        ShortcutAction::FocusPaneUp => Box::new(FocusPaneUp),
        ShortcutAction::FocusPaneRight => Box::new(FocusPaneRight),
        ShortcutAction::LastActiveWindow => Box::new(LastActiveWindow),
        ShortcutAction::ScrollHalfPageUp => Box::new(ScrollHalfPageUp),
        ShortcutAction::ScrollHalfPageDown => Box::new(ScrollHalfPageDown),
        // Handled above — it is the one action that is not one binding.
        ShortcutAction::WindowByIndex => unreachable!("WindowByIndex expands via window_index_bindings"),
    };
    KeyBinding::load(chord, boxed, None, true, None, mapper).map(|b| vec![b])
}

/// The [`ShortcutAction::WindowByIndex`] nine-binding expansion (D2): take the
/// MODIFIER set out of the stored/default `chord` and re-emit it with each of the
/// nine digits, binding a [`SelectWindowIndex`] carrying that digit's 1-based
/// index. The chord's own key is discarded — the store normalizes it to `1`, and
/// the row *means* "these modifiers + 1…9".
///
/// A degenerate, keyless token (only reachable by hand-editing `ui_settings.json`)
/// can't yield a modifier set, so it falls through to `KeyBinding::load` on the raw
/// chord — which reports the parse error the live path logs and skips.
fn window_index_bindings(
    chord: &str,
    mapper: &dyn PlatformKeyboardMapper,
) -> Result<Vec<KeyBinding>, InvalidKeystrokeError> {
    let Some(base) = OwnedCombo::from_token(chord) else {
        return KeyBinding::load(
            chord,
            Box::new(SelectWindowIndex { index: 1 }),
            None,
            true,
            None,
            mapper,
        )
        .map(|b| vec![b]);
    };
    let mut bindings = Vec::with_capacity(WINDOW_INDEX_KEYS.len());
    for (slot, key) in WINDOW_INDEX_KEYS.into_iter().enumerate() {
        let combo = OwnedCombo {
            modifiers: base.modifiers,
            key: key.to_string(),
        };
        let index = slot as u8 + 1;
        bindings.push(KeyBinding::load(
            &combo.to_token(),
            Box::new(SelectWindowIndex { index }),
            None,
            true,
            None,
            mapper,
        )?);
    }
    Ok(bindings)
}

/// Build one keybinding with `use_key_equivalents = true` (the documented
/// character-matching / key-equivalent divergence) and no context predicate
/// (`None` = active in every dispatch context, like Swift's process-wide
/// monitor). The chords are static table data, so a parse failure is a
/// programmer error — hence `expect`.
fn load_binding<A: Action>(
    chord: &str,
    action: A,
    mapper: &dyn PlatformKeyboardMapper,
) -> KeyBinding {
    KeyBinding::load(chord, Box::new(action), None, true, None, mapper)
        .expect("static shortcut chord parses")
}

// -- peek clear (window-level modifier-release observer) ----------------------

/// Window-level `on_modifiers_changed` handler (installed by the shipped
/// window's root view). Two independent modifier-driven overlays hang off it:
///
/// * the sidebar peek's clear half — if the focused window is peeking, end the
///   peek once none of the sidebar-session shortcuts' modifiers are held anymore
///   (Swift's `endPeekIfModifiersReleased`);
/// * the Phase 1 hold-to-hint overlay ([`update_hint_overlay`], D5).
///
/// Both route to the focused window's state through
/// [`WindowRegistry::active_state`], matching their trigger sides. This observer
/// binds no keys and consumes no keystroke — it only watches modifier state, so
/// it can never swallow a chord.
pub(crate) fn on_window_modifiers_changed(event: &ModifiersChangedEvent, cx: &mut App) {
    let Some(state) = WindowRegistry::active_state(cx, true) else {
        return;
    };
    end_peek_if_modifiers_released(event.modifiers, &state, cx);
    update_hint_overlay(event.modifiers, &state, cx);
}

/// The peek's clear half (see [`on_window_modifiers_changed`]).
fn end_peek_if_modifiers_released(current: Modifiers, state: &Entity<WindowState>, cx: &mut App) {
    if !state.read(cx).sidebar.peeking() {
        return;
    }
    // The sidebar/peek overlay is not rendered in the managed window this cycle,
    // so there is no pointer-hover pin to consult yet; the render-integration
    // cycle that mounts the overlay passes the real pin here (plan: "unless the
    // mouse is pinning the overlay" — R10's view-layer hover state).
    let mouse_pinned = false;
    if should_end_peek(current, peek_relevant_modifiers(cx), mouse_pinned) {
        state.update(cx, |s, _cx| s.sidebar.end_sidebar_peek());
    }
}

/// D5: how long the scheme's modifier pair must be held, with no chord committed,
/// before the hint overlay appears. Long enough that a fast `⌃⌘]` never flashes
/// the badges, short enough to feel like a held-key affordance rather than a
/// delayed popup — tmux `display-panes` territory.
pub(crate) const HINT_OVERLAY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// The Phase 1 hold-to-hint trigger (D5): arm the debounce while the held set is
/// EXACTLY the scheme's modifier pair, and clear instantly the moment it is
/// anything else — a release, or one more modifier joining. The delay itself (and
/// the cancel-race handling) lives in [`WindowState::arm_key_hint`].
///
/// Exact match, not "any of them held" like the peek: the overlay is a statement
/// about one specific chord family, so ⌃⌘⇧ must not show what ⌃⌘ does.
fn update_hint_overlay(current: Modifiers, state: &Entity<WindowState>, cx: &mut App) {
    if hint_hold_matches(current, hint_relevant_modifiers(cx)) {
        state.update(cx, |s, cx| s.arm_key_hint(HINT_OVERLAY_DELAY, cx));
    } else {
        state.update(cx, |s, cx| s.cancel_key_hint(cx));
    }
}

/// The modifier set the hint overlay watches, read from the LIVE bindings (the
/// [`peek_relevant_modifiers`] pattern) so a user who rebinds the scheme keeps a
/// working overlay: the next-pill chord's modifiers — [`ShortcutAction::FocusPaneRight`]
/// (`⌃⌘L` by default), falling back to [`ShortcutAction::NextWindow`] (`⌃⌘]`) when
/// that one is unbound.
///
/// `None` — never show the overlay — when both are unbound. A combo bound to a
/// BARE key (no modifiers) is rejected by [`hint_hold_matches`], which owns that
/// rule.
///
/// The badges the overlay draws are [`ShortcutAction::WindowByIndex`]'s digits,
/// which share this modifier set in the defaults. A user who rebinds ONLY
/// `WindowByIndex` to a different set still gets the badges on the nav-chord hold;
/// re-deriving them per action is a Phase 2 problem, when panes (not just pills)
/// get numbers.
fn hint_relevant_modifiers(cx: &App) -> Option<nice_model::shortcuts::Modifiers> {
    [ShortcutAction::FocusPaneRight, ShortcutAction::NextWindow]
        .into_iter()
        .find_map(|action| live_combo_modifiers(cx, action))
}

/// Pure hold-match decision: are exactly the hint's modifiers down, and nothing
/// else? The `function` bit is ignored — it is not part of any binding
/// ([`nice_model::shortcuts::Modifiers`] does not even model it), so an fn-key
/// press must not tear the overlay down mid-hold.
///
/// Two shapes never match, so the overlay simply never appears for them: `None`
/// (the nav chords are unbound) and a modifier-LESS combo, which would otherwise
/// mean "hold nothing" and pin the badges on screen permanently.
fn hint_hold_matches(current: Modifiers, hint: Option<nice_model::shortcuts::Modifiers>) -> bool {
    let Some(hint) = hint else {
        return false;
    };
    if !(hint.command || hint.control || hint.alt || hint.shift) {
        return false;
    }
    current.control == hint.control
        && current.alt == hint.alt
        && current.shift == hint.shift
        && current.platform == hint.command
}

/// The union of the two sidebar-session-cycle shortcuts' modifier sets (⌘⌥ by
/// default) — the modifiers whose full release ends a peek. Reads the LIVE
/// bindings (D4): rebinding a sidebar-nav chord to a different modifier set must
/// re-point the peek at the NEW modifiers, else the overlay watches the wrong keys
/// (the landed TODO). Falls back to the defaults when no store Global is installed
/// (a scenario that never seeded it); an unbound sidebar-session action contributes no
/// modifiers.
fn peek_relevant_modifiers(cx: &App) -> Modifiers {
    let mut relevant = Modifiers::default();
    for action in [ShortcutAction::NextSidebarSession, ShortcutAction::PrevSidebarSession] {
        if let Some(m) = live_combo_modifiers(cx, action) {
            relevant.control |= m.control;
            relevant.alt |= m.alt;
            relevant.shift |= m.shift;
            relevant.platform |= m.command;
        }
    }
    relevant
}

/// The modifier set of `action`'s LIVE combo, read from the
/// [`ShortcutBindings`](crate::shortcuts_store::ShortcutBindings) Global (D4). An
/// unbound action yields `None`. Falls back to `action`'s default combo when no
/// store Global is installed, so peek behaves identically pre-store-seed.
fn live_combo_modifiers(cx: &App, action: ShortcutAction) -> Option<nice_model::shortcuts::Modifiers> {
    match cx.try_global::<crate::shortcuts_store::ShortcutBindings>() {
        Some(store) => store.binding(action).map(|c| c.modifiers),
        None => default_combo(action).map(|c| c.modifiers),
    }
}

/// Pure peek-clear decision: end the peek when none of the `relevant` modifiers
/// remain in `current` — unless the pointer pins the overlay. Mirrors Swift's
/// `stillHeld = !(current ∩ relevant).isEmpty` then `if !stillHeld { end }`.
fn should_end_peek(current: Modifiers, relevant: Modifiers, mouse_pinned: bool) -> bool {
    if mouse_pinned {
        return false;
    }
    let still_held = (relevant.control && current.control)
        || (relevant.alt && current.alt)
        || (relevant.shift && current.shift)
        || (relevant.platform && current.platform);
    !still_held
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts_store::ShortcutBindings;
    use nice_model::shortcuts::OwnedCombo;

    /// gpui `Modifiers` with only the given flags set (helper for the peek tests).
    fn mods(control: bool, alt: bool, shift: bool, platform: bool) -> Modifiers {
        Modifiers {
            control,
            alt,
            shift,
            platform,
            function: false,
        }
    }

    /// ⌘⌥ — the default sidebar-session-cycle modifier set the `should_end_peek` tests
    /// exercise (equivalent to `peek_relevant_modifiers` with no store installed).
    fn command_alt() -> Modifiers {
        mods(false, true, false, true)
    }

    /// A unique `ui_settings.json` temp path for a store the gpui tests install.
    /// The store's persist is fail-soft, but a real, unique path keeps the write a
    /// no-op collision-free (never the real support root — hermeticity).
    fn unique_temp_ui_settings(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-keymap-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    #[test]
    fn peek_stays_while_any_relevant_modifier_is_held() {
        let relevant = command_alt(); // ⌘⌥
        // Both held → stays.
        assert!(!should_end_peek(mods(false, true, false, true), relevant, false));
        // Only ⌘ still held (⌥ released) → stays (Swift keeps it until BOTH lift).
        assert!(!should_end_peek(mods(false, false, false, true), relevant, false));
        // Only ⌥ still held → stays.
        assert!(!should_end_peek(mods(false, true, false, false), relevant, false));
    }

    #[test]
    fn peek_ends_when_all_relevant_modifiers_release() {
        let relevant = command_alt();
        // Nothing held → end.
        assert!(should_end_peek(mods(false, false, false, false), relevant, false));
        // An unrelated modifier (⇧ / ⌃) held but neither ⌘ nor ⌥ → still end.
        assert!(should_end_peek(mods(true, false, true, false), relevant, false));
    }

    #[test]
    fn mouse_pin_keeps_the_peek_even_with_no_modifiers() {
        // The pointer pinning the overlay wins over modifier release (dossier
        // G6 / R10's hover pin) — never ends while pinned.
        let relevant = command_alt();
        assert!(!should_end_peek(mods(false, false, false, false), relevant, true));
    }

    /// D4 / Validation §2a(e): `peek_relevant_modifiers` reads the LIVE map. With no
    /// store it falls back to the ⌘⌥ defaults; after rebinding a sidebar-session chord to
    /// a ⌃⇧ combo the relevant set tracks the NEW modifiers (unioned with the other,
    /// still-default sidebar-session combo).
    #[gpui::test]
    fn peek_relevant_modifiers_default_then_live(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // No store yet ⇒ the defaults: both sidebar-session combos are ⌘⌥.
            let r = peek_relevant_modifiers(cx);
            assert!(r.platform && r.alt, "⌘⌥ hold the peek by default");
            assert!(!r.control && !r.shift, "no other modifier holds the peek by default");

            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings("peek-live")));
            // Rebind NextSidebarSession ⌘⌥↓ -> ⌃⇧↓ (persist + rebuild happen inside).
            ShortcutBindings::set_binding(
                cx,
                ShortcutAction::NextSidebarSession,
                OwnedCombo::from_token("ctrl-shift-down"),
            );

            let r = peek_relevant_modifiers(cx);
            assert!(r.control, "the rebound ⌃ now holds the peek");
            assert!(r.shift, "the rebound ⇧ now holds the peek");
            // PrevSidebarSession is still ⌘⌥, so ⌘⌥ remain relevant too (the union).
            assert!(r.platform && r.alt, "the other sidebar-session combo keeps ⌘⌥ relevant");
        });
    }

    // MARK: - Hold-to-hint overlay trigger (Phase 1, D5)

    /// ⌃⌘ as the model spells it — the scheme's default hint pair.
    fn control_command() -> nice_model::shortcuts::Modifiers {
        nice_model::shortcuts::Modifiers::CONTROL_COMMAND
    }

    #[test]
    fn hint_shows_only_for_the_exact_modifier_pair() {
        let hint = Some(control_command());
        // ⌃⌘ held, nothing else.
        assert!(hint_hold_matches(mods(true, false, false, true), hint));
        // Half the pair, or the wrong pair: nothing.
        assert!(!hint_hold_matches(mods(true, false, false, false), hint));
        assert!(!hint_hold_matches(mods(false, false, false, true), hint));
        assert!(!hint_hold_matches(mods(false, true, false, true), hint));
        // Nothing held at all — the release that ends a hold.
        assert!(!hint_hold_matches(mods(false, false, false, false), hint));
        // A THIRD modifier joining is a different chord family (⌃⌘⇧), so the
        // overlay tears down rather than lying about what the digits do.
        assert!(!hint_hold_matches(mods(true, false, true, true), hint));
    }

    #[test]
    fn the_fn_key_does_not_break_a_hold() {
        // `function` is not part of any binding, so it must not count against the
        // exact match (a fn press mid-hold would otherwise flicker the overlay).
        let current = Modifiers {
            control: true,
            alt: false,
            shift: false,
            platform: true,
            function: true,
        };
        assert!(hint_hold_matches(current, Some(control_command())));
    }

    #[test]
    fn no_scheme_modifiers_means_no_overlay() {
        // Both nav chords unbound.
        assert!(!hint_hold_matches(mods(true, false, false, true), None));
        // Bound, but to a bare key: "hold nothing" would pin the overlay on
        // screen forever, so a modifier-less combo never matches — not even the
        // empty held set it equals field-for-field.
        assert!(!hint_hold_matches(
            mods(false, false, false, false),
            Some(nice_model::shortcuts::Modifiers::default())
        ));
    }

    /// The hint modifiers track the LIVE bindings (the `peek_relevant_modifiers`
    /// pattern): with no store they are the ⌃⌘ defaults, and rebinding the
    /// next-pill chord re-points the overlay at the new pair.
    #[gpui::test]
    fn hint_modifiers_follow_the_live_next_pill_binding(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(
                hint_relevant_modifiers(cx),
                Some(control_command()),
                "the ⌃⌘ default drives the overlay before any store exists"
            );

            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings("hint-live")));
            // Rebind FocusPaneRight (the primary source) ⌃⌘L -> ⌥⇧L.
            ShortcutBindings::set_binding(
                cx,
                ShortcutAction::FocusPaneRight,
                OwnedCombo::from_token("alt-shift-l"),
            );

            let live = hint_relevant_modifiers(cx).expect("the rebound chord still has modifiers");
            assert!(live.alt && live.shift, "the overlay follows the rebound pair");
            assert!(!live.control && !live.command, "and drops the old one");
            assert!(
                !hint_hold_matches(mods(true, false, false, true), Some(live)),
                "⌃⌘ no longer shows the overlay"
            );
            assert!(hint_hold_matches(mods(false, true, true, false), Some(live)));
        });
    }

    /// `FocusPaneRight` unbound ⇒ the fallback source (`NextWindow`) supplies the
    /// pair, so the overlay survives clearing one of the two nav chords.
    #[gpui::test]
    fn hint_modifiers_fall_back_to_next_window(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings(
                "hint-fallback",
            )));
            ShortcutBindings::set_binding(cx, ShortcutAction::FocusPaneRight, None);
            assert_eq!(
                hint_relevant_modifiers(cx),
                Some(control_command()),
                "⌃⌘] still defines the hold"
            );

            // Both nav chords cleared ⇒ no overlay at all.
            ShortcutBindings::set_binding(cx, ShortcutAction::NextWindow, None);
            assert_eq!(hint_relevant_modifiers(cx), None);
        });
    }

    /// The non-rebindable re-install audit (the plan's biggest-regression test,
    /// Validation §3). After a rebind + `rebuild_keymap` (both driven by
    /// `set_binding`), EVERY PROTECTED non-rebindable chord (⌃⌘F, ⌘N, ⌘Q, ⌘W,
    /// Esc@SidebarShell, ⌘,) is still present in the keymap — the total
    /// `clear_key_bindings()` must not have dropped them — AND the new live chord is
    /// bound to `NewTerminalWindow` while the old default `cmd-t` is not.
    #[gpui::test]
    fn rebuild_keeps_non_rebindables_and_swaps_live_combo(cx: &mut gpui::TestAppContext) {
        use gpui::Keystroke;

        cx.update(|cx| {
            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings(
                "non-rebindable",
            )));
            // Rebind NewTerminalWindow ⌘T -> ⌘Y; set_binding persists then rebuilds.
            ShortcutBindings::set_binding(
                cx,
                ShortcutAction::NewTerminalWindow,
                OwnedCombo::from_token("cmd-y"),
            );

            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            // A binding for `action` whose (single) keystroke exactly matches `chord`.
            let bound = |action: &dyn Action, chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .bindings_for_action(action)
                    .any(|b| matches!(b.match_keystrokes(std::slice::from_ref(&ks)), Some(false)))
            };

            // Every PROTECTED non-rebindable survives the total clear.
            assert!(bound(&crate::app::ToggleFullScreen, "ctrl-cmd-f"), "⌃⌘F re-installed");
            assert!(bound(&crate::app::NewWindow, "cmd-n"), "⌘N re-installed");
            assert!(bound(&crate::app::Quit, "cmd-q"), "⌘Q re-installed");
            assert!(bound(&crate::app::CloseWindow, "cmd-w"), "⌘W re-installed");
            assert!(
                bound(&crate::sidebar_shell::CollapseSidebarSelection, "escape"),
                "Esc@SidebarShell re-installed"
            );
            assert!(bound(&crate::settings::window::OpenSettings, "cmd-,"), "⌘, re-installed");

            // The rebound live combo is bound; the old default is gone.
            assert!(bound(&NewTerminalWindow, "cmd-y"), "the new ⌘Y drives NewTerminalWindow");
            assert!(!bound(&NewTerminalWindow, "cmd-t"), "the old ⌘T no longer drives NewTerminalWindow");
        });
    }

    /// The shared-table contract (Slice 2): every `FixedAccelerator` entry in
    /// `RESERVED_COMBOS` is actually installed by [`non_rebindable_bindings`]. The
    /// recorder refuses those chords on the promise that Nice itself owns them —
    /// an entry with no install would be a lie, and an install with no entry would
    /// be recordable.
    #[gpui::test]
    fn every_fixed_accelerator_entry_is_installed(cx: &mut gpui::TestAppContext) {
        use gpui::Keystroke;
        use nice_model::shortcuts::{ReservedKind, RESERVED_COMBOS};

        cx.update(|cx| {
            let bindings = non_rebindable_bindings(cx);
            for entry in RESERVED_COMBOS
                .into_iter()
                .filter(|r| r.kind == ReservedKind::FixedAccelerator)
            {
                let chord = entry.combo.chord_str();
                let ks = Keystroke::parse(&chord).expect("a reserved chord parses");
                assert!(
                    bindings.iter().any(|b| matches!(
                        b.match_keystrokes(std::slice::from_ref(&ks)),
                        Some(false)
                    )),
                    "{chord} is reserved as a fixed accelerator but nothing installs it"
                );
            }
        });
    }

    /// D2: the ONE stored `WindowByIndex` combo expands into NINE bindings, each
    /// carrying its own 1-based index, under the STORED modifier set.
    #[gpui::test]
    fn window_by_index_expands_one_combo_into_nine_bindings(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mapper = cx.keyboard_mapper().clone();
            let bindings = shortcut_binding(
                ShortcutAction::WindowByIndex,
                "cmd-ctrl-1",
                mapper.as_ref(),
            )
            .expect("the default WindowByIndex chord expands");

            assert_eq!(bindings.len(), 9, "one row, nine chords");
            for (slot, binding) in bindings.iter().enumerate() {
                let index = slot as u8 + 1;
                let chord = format!("cmd-ctrl-{index}");
                let ks = gpui::Keystroke::parse(&chord).expect("test chord parses");
                assert!(
                    matches!(binding.match_keystrokes(std::slice::from_ref(&ks)), Some(false)),
                    "binding {slot} must match {chord}"
                );
                assert!(
                    binding.action().partial_eq(&SelectWindowIndex { index }),
                    "binding {slot} must carry index {index}"
                );
            }
        });
    }

    /// The expansion takes the MODIFIERS from the stored combo and ignores its key
    /// (the store normalizes the digit to `1`; the row means "these modifiers +
    /// 1…9"). Rebinding to ⌥7 therefore binds ⌥1…⌥9, not ⌥7 alone.
    #[gpui::test]
    fn window_by_index_expansion_follows_the_recorded_modifiers(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mapper = cx.keyboard_mapper().clone();
            let bindings =
                shortcut_binding(ShortcutAction::WindowByIndex, "alt-7", mapper.as_ref())
                    .expect("a rebound WindowByIndex chord expands");

            assert_eq!(bindings.len(), 9);
            for (slot, binding) in bindings.iter().enumerate() {
                let chord = format!("alt-{}", slot + 1);
                let ks = gpui::Keystroke::parse(&chord).expect("test chord parses");
                assert!(
                    matches!(binding.match_keystrokes(std::slice::from_ref(&ks)), Some(false)),
                    "the recorded ⌥ modifier must carry to {chord}"
                );
            }
        });
    }

    /// Every other action still yields exactly one binding — the carve-out is
    /// `WindowByIndex`-only.
    #[gpui::test]
    fn every_other_action_yields_exactly_one_binding(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mapper = cx.keyboard_mapper().clone();
            for (action, combo) in default_bindings() {
                let n = shortcut_binding(action, &combo.chord_str(), mapper.as_ref())
                    .expect("a default chord parses")
                    .len();
                let want = if action == ShortcutAction::WindowByIndex { 9 } else { 1 };
                assert_eq!(n, want, "{action:?} produced {n} bindings");
            }
        });
    }

    /// The default board reaches the live keymap: every Phase 1 chord is bound to
    /// its action, and the D1 flip means ⌘⌥←/→ no longer drive the pill strip.
    #[gpui::test]
    fn phase_one_default_chords_are_live_in_the_keymap(cx: &mut gpui::TestAppContext) {
        use gpui::Keystroke;

        cx.update(|cx| {
            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings("phase1")));
            rebuild_keymap(cx);

            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let bound = |action: &dyn Action, chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .bindings_for_action(action)
                    .any(|b| matches!(b.match_keystrokes(std::slice::from_ref(&ks)), Some(false)))
            };

            // D1: the pill chords moved.
            assert!(bound(&NextWindow, "cmd-ctrl-]"), "⌃⌘] steps to the next window");
            assert!(bound(&PrevWindow, "cmd-ctrl-["), "⌃⌘[ steps to the previous window");
            assert!(!bound(&NextWindow, "cmd-alt-right"), "⌘⌥→ is freed");
            assert!(!bound(&PrevWindow, "cmd-alt-left"), "⌘⌥← is freed");

            // D3 + the rest of the scheme.
            assert!(bound(&FocusPaneLeft, "cmd-ctrl-h"));
            assert!(bound(&FocusPaneDown, "cmd-ctrl-j"));
            assert!(bound(&FocusPaneUp, "cmd-ctrl-k"));
            assert!(bound(&FocusPaneRight, "cmd-ctrl-l"));
            assert!(bound(&LastActiveWindow, "cmd-ctrl-o"));
            assert!(bound(&ScrollHalfPageUp, "cmd-ctrl-u"));
            assert!(bound(&ScrollHalfPageDown, "cmd-ctrl-d"));

            // D2: all nine digits are live off the one stored row.
            for index in 1u8..=9 {
                assert!(
                    bound(&SelectWindowIndex { index }, &format!("cmd-ctrl-{index}")),
                    "⌃⌘{index} must select window {index}"
                );
            }
        });
    }
}
