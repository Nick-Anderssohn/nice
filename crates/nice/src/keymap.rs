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
use nice_model::{
    directional_neighbor, PaneDirection, PaneRect, SidebarMode, SplitOrient, TermWindowKind,
};
use nice_term_core::SpawnSpec;
use nice_term_view::FontSettings;

use crate::app_shell::{
    min_ratio_for, split_available_px, PANE_DIVIDER_PX, PANE_MIN_HEIGHT, PANE_MIN_WIDTH,
};
use crate::pty_manager::DissolveTerminus;
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
        SplitDown,
        SplitRight,
        ZoomPane,
        BreakPane,
        ResizePaneLeft,
        ResizePaneDown,
        ResizePaneUp,
        ResizePaneRight,
        SwapPaneLeft,
        SwapPaneDown,
        SwapPaneUp,
        SwapPaneRight,
        CopyMode,
        SearchScrollback,
        DetachSession,
        AdoptDetachedSession,
        TearOffPane,
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
    // D3, as revised by the 2026-08-11 hjkl ladder: the four `FocusPane*`
    // actions are the ladder's ⌃⌘⇧ rung — directional PANE focus (tmux
    // `select-pane -L/-D/-U/-R`). Phase 1 shipped them registered-but-inert
    // (there were no panes to move between yet) precisely so Phase 2 would only
    // have to fill the bodies in: no binding moved, nothing migrated.
    cx.on_action(|_: &FocusPaneLeft, cx: &mut App| focus_pane(cx, PaneDirection::Left));
    cx.on_action(|_: &FocusPaneRight, cx: &mut App| focus_pane(cx, PaneDirection::Right));
    cx.on_action(|_: &FocusPaneUp, cx: &mut App| focus_pane(cx, PaneDirection::Up));
    cx.on_action(|_: &FocusPaneDown, cx: &mut App| focus_pane(cx, PaneDirection::Down));
    // -- Phase 2 (tmux port): the pane verbs ----------------------------------
    //
    // Every one of these mutates the MODEL only; the render reacts (spawning a
    // freshly split pane's pty is the single exception, done here so the shell
    // is up before the first paint). That is the established action pattern —
    // `NextWindow` / `PrevWindow` right above work the same way.
    cx.on_action(|_: &SplitDown, cx: &mut App| split_focused_pane(cx, SplitOrient::Stacked));
    cx.on_action(|_: &SplitRight, cx: &mut App| split_focused_pane(cx, SplitOrient::Beside));
    cx.on_action(|_: &ZoomPane, cx: &mut App| toggle_zoom(cx));
    cx.on_action(|_: &BreakPane, cx: &mut App| break_focused_pane(cx));
    cx.on_action(|_: &ResizePaneLeft, cx: &mut App| resize_pane(cx, PaneDirection::Left));
    cx.on_action(|_: &ResizePaneDown, cx: &mut App| resize_pane(cx, PaneDirection::Down));
    cx.on_action(|_: &ResizePaneUp, cx: &mut App| resize_pane(cx, PaneDirection::Up));
    cx.on_action(|_: &ResizePaneRight, cx: &mut App| resize_pane(cx, PaneDirection::Right));
    cx.on_action(|_: &SwapPaneLeft, cx: &mut App| swap_pane(cx, PaneDirection::Left));
    cx.on_action(|_: &SwapPaneDown, cx: &mut App| swap_pane(cx, PaneDirection::Down));
    cx.on_action(|_: &SwapPaneUp, cx: &mut App| swap_pane(cx, PaneDirection::Up));
    cx.on_action(|_: &SwapPaneRight, cx: &mut App| swap_pane(cx, PaneDirection::Right));
    cx.on_action(|_: &LastActiveWindow, cx: &mut App| {
        with_active_state(cx, |s, _cx| last_active_window(&mut s.workspace));
    });
    cx.on_action(|_: &ScrollHalfPageUp, cx: &mut App| scroll_active_window_half_page(cx, true));
    cx.on_action(|_: &ScrollHalfPageDown, cx: &mut App| scroll_active_window_half_page(cx, false));
    // -- Phase 3 (tmux port): copy mode + scrollback search -------------------
    cx.on_action(|_: &CopyMode, cx: &mut App| toggle_copy_mode(cx));
    cx.on_action(|_: &SearchScrollback, cx: &mut App| open_scrollback_search(cx));
    // -- Phase 4 (tmux port): detach ------------------------------------------
    cx.on_action(|_: &DetachSession, cx: &mut App| detach_active_session(cx));
    cx.on_action(|_: &AdoptDetachedSession, cx: &mut App| adopt_detached_session(cx));
    cx.on_action(|_: &TearOffPane, cx: &mut App| tear_off_focused_pane(cx));
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

// ===========================================================================
// Phase 2 (tmux port) — the pane verbs
// ===========================================================================

/// How far one `⌃⌥⌘hjkl` press walks a divider, in px (P7). Converted to a ratio
/// delta against the px that divider's ratio actually divides, so a step moves
/// the same visible distance whatever the split's size or nesting depth.
const RESIZE_STEP_PX: f32 = 40.0;

/// The active session's active window — the pill every pane verb operates on.
/// `None` when no session is active or the active session has no active window,
/// in which case every verb below is a no-op.
fn active_session_window(state: &WindowState) -> Option<(String, String)> {
    let session_id = state.workspace.active_session_id().map(str::to_owned)?;
    let term_window_id = state
        .workspace
        .session_for(&session_id)
        .and_then(|s| s.active_window_id.clone())?;
    Some((session_id, term_window_id))
}

/// Run `f` over the active pill's [`TermWindow`](nice_model::TermWindow) —
/// read-only, so the verbs can decide whether to mutate before they touch
/// `mutate_session` (whose did-mutate signal queues a session save; a refused
/// verb must not queue one).
fn with_active_window<R>(
    state: &WindowState,
    session_id: &str,
    term_window_id: &str,
    f: impl FnOnce(&nice_model::TermWindow) -> R,
) -> Option<R> {
    state
        .workspace
        .session_for(session_id)
        .and_then(|s| s.windows.iter().find(|w| w.id == term_window_id))
        .map(f)
}

/// The pane area's last painted size as a rect at the origin, or `None` before
/// the window's first paint. Only extents matter — the pane tree's geometry is
/// laid out relative to its own bounds.
fn pane_content_rect(state: &WindowState) -> Option<PaneRect> {
    let (w, h) = state.pane_content_size()?;
    (w > 0.0 && h > 0.0).then(|| PaneRect::new(0.0, 0.0, w, h))
}

/// Directional pane focus — the ladder's `⌃⌘⇧hjkl` rung (tmux `select-pane
/// -L/-D/-U/-R`).
///
/// P5: the edge is a **no-op**. Focus does not wrap, and it does not fall
/// through to pill navigation — bare `⌃⌘h`/`⌃⌘l` is how the user leaves a pill,
/// so falling through would make one chord mean two things depending on where
/// the caret happens to sit.
///
/// P4: a real move un-zooms first, then applies. A refused move changes
/// nothing at all, zoom included.
///
/// Adjacency is computed on
/// [`nominal_leaf_rects`](nice_model::TermWindow::nominal_leaf_rects) rather
/// than the painted bounds: the ranking is scale-invariant, so a stand-in extent
/// gives the same answer, and the verb keeps working before the first paint.
fn focus_pane(cx: &mut App, direction: PaneDirection) {
    with_active_state(cx, |s, _cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let target = with_active_window(s, &session_id, &term_window_id, |w| {
            directional_neighbor(&w.nominal_leaf_rects(), &w.effective_pane_id(), direction)
        })
        .flatten();
        let Some(target) = target else {
            return;
        };
        s.workspace.mutate_session(&session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                term_window.active_pane_id = target;
                term_window.zoomed = false;
            }
        });
        s.save_to_store();
    });
}

/// Split the focused pane (`⌃⌘-` stacked / `⌃⌘\` beside, D2's divider
/// mnemonics), putting the new shell down/right of it and moving focus there —
/// tmux's `split-window` behavior.
///
/// **P6 refusal.** A split that would leave either half under
/// [`PANE_MIN_WIDTH`]/[`PANE_MIN_HEIGHT`] is refused outright rather than
/// clamped: there is no ratio at which two panes fit, so the honest answer is to
/// do nothing. With no painted size yet (the window has never rendered) there is
/// no px context to refuse against, so the split proceeds at an even ratio.
///
/// **D1.** The new pane always runs a plain shell in the focused pane's cwd.
/// Nice never spawns Claude into a split pane, so the ≤1-running-Claude-per-
/// session invariant is untouched — Claude stays in the pane it started in.
///
/// The pty is spawned right here rather than left to the next render's
/// `ensure_active_window_spawned`, so the shell is already coming up when the
/// pane first paints. Only when the pane being split is itself live, though: a
/// pill that has not spawned at all restores lazily on activation, and splitting
/// it must not jump that queue.
fn split_focused_pane(cx: &mut App, orient: SplitOrient) {
    with_active_state(cx, |s, cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let content = pane_content_rect(s);
        // Everything the split needs, read before anything is written.
        let Some(Some((focused_id, cwd, source_is_live))) =
            with_active_window(s, &session_id, &term_window_id, |window| {
                let focused_id = window.effective_pane_id();
                let pane = window.layout.pane(&focused_id)?;
                if let Some(content) = content {
                    if !split_fits(&window.layout, &focused_id, orient, content) {
                        return None;
                    }
                }
                let cwd = s
                    .workspace
                    .resolved_spawn_cwd_for_pane(
                        s.workspace.session_for(&session_id)?,
                        window,
                        pane,
                    );
                let live = s.ptys.has_pane(&session_id, &term_window_id, &focused_id);
                Some((focused_id, cwd, live))
            })
        else {
            return;
        };

        let new_pane_id = s
            .ptys
            .mint_pane_id(&format!("{term_window_id}-pane"));
        let mut split = false;
        {
            let (new_pane_id, cwd) = (new_pane_id.clone(), cwd.clone());
            s.workspace.mutate_session(&session_id, |session| {
                let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id)
                else {
                    return;
                };
                let pane = nice_model::Pane::new(new_pane_id.clone(), TermWindowKind::Terminal)
                    .with_cwd(Some(cwd));
                if term_window.layout.split(&focused_id, orient, pane) {
                    // Focus follows the new pane (tmux), and P4 un-zooms: a
                    // zoomed pill that just grew a pane must show it.
                    term_window.active_pane_id = new_pane_id;
                    term_window.zoomed = false;
                    split = true;
                }
            });
        }
        if !split {
            return;
        }
        if source_is_live {
            // A split pane is a normal injected terminal pane: the active
            // profile's argv carries the rc injection (bash `--rcfile`), and the
            // spawn snapshots the CURRENT profile as the pane's `PaneShell` — a
            // pane split off mid-run after a shell change runs the new shell
            // beside its old-shell sibling.
            let spec =
                SpawnSpec::shell(cwd).with_argv(crate::shell::spawn_argv(cx, true, None));
            if let Err(e) = s.ptys.spawn_pane(
                &session_id,
                &term_window_id,
                &new_pane_id,
                spec,
                cx,
            ) {
                eprintln!("nice: split pane failed to spawn: {e}");
            }
            // Wire the new pty's events immediately; the render's own sweep
            // would get there too, but a pane that exits between the split and
            // the first paint must still route.
            s.subscribe_spawned_windows(cx);
        }
        s.save_to_store();
    });
}

/// Whether bisecting `pane_id` leaves both halves at or above the P6 minimum,
/// measured against the painted layout. The divider's own px come out of the
/// splittable extent first — it is real screen the two halves don't get.
fn split_fits(
    layout: &nice_model::PaneLayout,
    pane_id: &str,
    orient: SplitOrient,
    content: PaneRect,
) -> bool {
    let Some((_, rect)) = layout
        .leaf_rects(content, PANE_DIVIDER_PX)
        .into_iter()
        .find(|(id, _)| id == pane_id)
    else {
        return true;
    };
    let (extent, min) = match orient {
        SplitOrient::Beside => (rect.width, PANE_MIN_WIDTH),
        SplitOrient::Stacked => (rect.height, PANE_MIN_HEIGHT),
    };
    (extent - PANE_DIVIDER_PX) / 2.0 >= min
}

/// Toggle "paint only the focused pane, full size" (`⌃⌘z`) — tmux `resize-pane
/// -Z`. P4: a transient render flag, never persisted; every pty keeps running
/// underneath. A single-pane pill has nothing to zoom, so the chord is a no-op
/// there rather than a state change nothing can show.
fn toggle_zoom(cx: &mut App) {
    with_active_state(cx, |s, _cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let multi = with_active_window(s, &session_id, &term_window_id, |w| {
            w.layout.leaf_count() > 1
        })
        .unwrap_or(false);
        if !multi {
            return;
        }
        // No `save_to_store` after this one: `zoomed` is `#[serde(skip)]`, so
        // there is nothing for a save to record.
        s.workspace.mutate_session(&session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                term_window.zoomed = !term_window.zoomed;
            }
        });
    });
}

/// Break the focused shell pane out into a pill of its own (`⌃⌘b`) — tmux
/// `break-pane` (P3). The pty MOVES; whatever was running keeps running.
///
/// [`PtyManager::move_pane_to_new_window`] owns the refusals (single-leaf pill,
/// the Claude pane) and the re-keying; this only has to move focus to the new
/// pill, which is where tmux leaves it.
///
/// [`PtyManager::move_pane_to_new_window`]: crate::pty_manager::PtyManager::move_pane_to_new_window
fn break_focused_pane(cx: &mut App) {
    with_active_state(cx, |s, _cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let Some(pane_id) =
            with_active_window(s, &session_id, &term_window_id, |w| w.effective_pane_id())
        else {
            return;
        };
        let Some(new_window_id) = s.ptys.move_pane_to_new_window(
            &mut s.workspace,
            &session_id,
            &term_window_id,
            &pane_id,
        ) else {
            return;
        };
        s.workspace.mutate_session(&session_id, |session| {
            // P4 on the source pill: it just lost a pane, so a zoom that was
            // hiding the rest of it would now be hiding the wrong thing.
            if let Some(source) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                source.zoomed = false;
            }
            session.switch_active_window(&new_window_id);
        });
        s.save_to_store();
    });
}

// ===========================================================================
// Phase 4 (tmux port) — detach
// ===========================================================================

/// `⌃⌘⇧D` — detach the ACTIVE session out of the active window into the
/// app-global pool (tmux `detach-client` for one session, plan §P7). The session
/// keeps running with no window; nothing is killed.
///
/// A no-op with no pool installed (`run_selftest` and the scenarios install none
/// — the hermeticity rule) or with no active session, so the chord is consumed
/// rather than leaking to the pty either way. The Settings ▸ Advanced
/// detach-on-close toggle is deliberately NOT consulted: it governs what CLOSING
/// a window does, not whether the explicit verb exists.
pub(crate) fn detach_active_session(cx: &mut App) {
    let Some(state) = WindowRegistry::active_state(cx, true) else {
        return;
    };
    let Some(session_id) = state
        .read(cx)
        .workspace
        .active_session_id()
        .map(str::to_owned)
    else {
        return;
    };
    detach_session_from_window(cx, &state, &session_id);
}

/// Detach `session_id` out of `state`'s window, then apply §P7's terminus: a
/// window whose LAST session just detached is now empty, so it closes through
/// the normal window-close path.
///
/// Routing the close through `remove_window()` (rather than
/// [`PtyManager::apply_dissolve_terminus`], which quits outright when it is the
/// only window) is load-bearing under D5: the close observer's pool-aware
/// quit check is what decides whether the app survives, and detaching the last
/// session of the last window is exactly the case where a live pool must keep it
/// alive. The removal is DEFERRED out of the entity lease — driving gpui's
/// synchronous window-removal trail from inside one re-enters this same
/// `WindowState` through the close observer and aborts the process.
///
/// Shared by the `⌃⌘⇧D` chord and the sidebar row's Detach Session menu item.
pub(crate) fn detach_session_from_window(
    cx: &mut App,
    state: &Entity<WindowState>,
    session_id: &str,
) {
    let Some(terminus) = crate::detached_pool::detach_into_pool(cx, state, session_id) else {
        return;
    };
    if terminus != DissolveTerminus::WindowEmptied {
        return;
    }
    // Drop the emptied window's disk slot (else it restores as a broken empty
    // window next launch), set on the state BEFORE the defer.
    state.update(cx, |ws, _cx| ws.mark_removed_if_window_emptied(terminus));
    if let Some(handle) = state.read(cx).window_handle() {
        cx.defer(move |app| {
            let _ = handle.update(app, |_root, window, _app| window.remove_window());
        });
    }
}

/// `⌃⌘A` — adopt the most recently detached session (the pool HEAD) into the
/// active window (tmux `attach-session`, plan §P8). The inverse of ⌃⌘⇧D: a live
/// entry's ptys move back in with no respawn, a structural one rides the lazy
/// spawn on activation.
///
/// A no-op with no pool installed, an empty pool, or no active window — the chord
/// is still registered either way, so it is consumed rather than leaking to the
/// pty.
pub(crate) fn adopt_detached_session(cx: &mut App) {
    let Some(state) = WindowRegistry::active_state(cx, true) else {
        return;
    };
    crate::detached_pool::adopt_head_into(cx, &state);
}

/// `⌃⌘N` — move the focused pane into an OS window of its own (tmux
/// `break-pane -d` + `move-window`, plan §P9 / D4). The pty MOVES: whatever was
/// running keeps running, scrollback and all.
///
/// [`WindowState::tear_off_pane`] owns both extraction branches and every refusal
/// (unknown ids, and the Claude pane — refused SILENTLY, exactly as ⌃⌘b's
/// break-pane refuses it, so the two pane-to-window verbs behave the same way when
/// they decline). This only routes the synthetic entry into the new window and
/// applies the source window's terminus.
///
/// Two recoveries, neither of which may drop a live child:
///
/// * the window could not open ⇒ the entry comes back and the SOURCE window
///   re-adopts it, so the pane reappears there as its own pill rather than dying.
///   A source window the tear-off had emptied is re-populated and stays open, so
///   the emptying's `user_initiated_close` latch is cleared again — otherwise a
///   later non-user, non-quit close would drop a disk slot holding a live session;
/// * tearing off the source window's LAST pane emptied it ⇒ it closes through the
///   normal window-close path, the same routing the explicit detach uses (§P7), so
///   the pool-aware quit check stays in charge of whether the app survives. The
///   removal is DEFERRED out of the entity lease for the reason
///   [`detach_session_from_window`] records.
///
/// [`WindowState::tear_off_pane`]: crate::window_state::WindowState::tear_off_pane
pub(crate) fn tear_off_focused_pane(cx: &mut App) {
    let Some(state) = WindowRegistry::active_state(cx, true) else {
        return;
    };
    let Some((session_id, term_window_id, pane_id)) = ({
        let s = state.read(cx);
        active_session_window(s).and_then(|(session_id, term_window_id)| {
            with_active_window(s, &session_id, &term_window_id, |w| w.effective_pane_id())
                .map(|pane_id| (session_id, term_window_id, pane_id))
        })
    }) else {
        return;
    };

    let Some((entry, terminus)) = state.update(cx, |ws, wcx| {
        ws.tear_off_pane(&session_id, &term_window_id, &pane_id, wcx)
    }) else {
        return;
    };

    if let Err(entry) = crate::app::open_managed_window_adopting(cx, entry) {
        return_torn_off_entry(cx, &state, entry, terminus);
        return;
    }

    if terminus != DissolveTerminus::WindowEmptied {
        return;
    }
    if let Some(handle) = state.read(cx).window_handle() {
        cx.defer(move |app| {
            let _ = handle.update(app, |_root, window, _app| window.remove_window());
        });
    }
}

/// The tear-off open-failure recovery: the pane is already out of its old pill,
/// so hand it back to the window it came from rather than dropping a running
/// child on the floor.
///
/// `terminus` is what the extraction reported. `WindowEmptied` means
/// [`WindowState::tear_off_pane`] latched `user_initiated_close` (drop my disk
/// slot on close) — but this window just took the pane back and is STAYING OPEN,
/// so the latch is now wrong state: a later close that is neither user-initiated
/// nor part of a quit would read it and drop a disk slot holding a live session.
/// A successful re-adopt therefore undoes exactly what the emptying set, and
/// nothing else (a refused re-adopt leaves the window as the tear-off left it).
///
/// Extracted from [`tear_off_focused_pane`] so the recovery is reachable in a
/// test without having to make `open_window` fail.
///
/// [`WindowState::tear_off_pane`]: crate::window_state::WindowState::tear_off_pane
fn return_torn_off_entry(
    cx: &mut App,
    state: &Entity<WindowState>,
    entry: crate::detached_pool::DetachedEntry,
    terminus: DissolveTerminus,
) {
    state.update(cx, |ws, wcx| {
        if ws.adopt_entry(entry, wcx).is_ok() && terminus == DissolveTerminus::WindowEmptied {
            ws.set_user_initiated_close(false);
        }
    });
}

/// Walk the nearest enclosing split's divider one step (`⌃⌥⌘hjkl`) — tmux
/// `resize-pane -L/-D/-U/-R` (P7).
///
/// The px step becomes a ratio delta against exactly the px that divider
/// divides ([`split_available_px`]), so the divider travels
/// [`RESIZE_STEP_PX`] on screen whatever the split's size or depth, and it
/// clamps against the same P6 band a mouse drag does. `⌃⌥⌘h`/`⌃⌥⌘l` reach the
/// nearest side-by-side ancestor, `⌃⌥⌘j`/`⌃⌥⌘k` the nearest stacked one; no
/// matching ancestor is a no-op.
///
/// Without a painted size there is no px to convert against, so the verb does
/// nothing rather than guess at a ratio step (the plan's "resize no-ops" rule).
fn resize_pane(cx: &mut App, direction: PaneDirection) {
    with_active_state(cx, |s, _cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let Some(content) = pane_content_rect(s) else {
            return;
        };
        // Resize on a CLONE first: a divider already pinned at the clamp must
        // not queue a session save for a layout that did not move.
        let Some(Some(layout)) = with_active_window(s, &session_id, &term_window_id, |window| {
            let focused = window.effective_pane_id();
            let path = window.layout.resize_target_path(&focused, direction)?;
            let available = split_available_px(&window.layout, &path, content)?;
            if available <= 0.0 {
                return None;
            }
            let mut layout = window.layout.clone();
            layout
                .resize(
                    &focused,
                    direction,
                    RESIZE_STEP_PX / available,
                    min_ratio_for(direction.orient(), available),
                )
                .then_some(layout)
        }) else {
            return;
        };
        s.workspace.mutate_session(&session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                term_window.layout = layout;
            }
        });
        s.save_to_store();
    });
}

/// Trade places with the directional neighbor (`⌃⌥⌘⇧hjkl`) — tmux `swap-pane`
/// (P8). Payloads swap; structure and ratios stay exactly where they were, so
/// nothing reflows and the two panes simply exchange rects.
///
/// Focus follows the CONTENT, which falls out of the swap itself: the pane id
/// travels with its payload, so `active_pane_id` still names the same terminal —
/// now painting where its neighbor used to be. No edge neighbor is a no-op, the
/// same rule directional focus follows.
fn swap_pane(cx: &mut App, direction: PaneDirection) {
    with_active_state(cx, |s, _cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let Some(Some((focused, target))) =
            with_active_window(s, &session_id, &term_window_id, |w| {
                let focused = w.effective_pane_id();
                directional_neighbor(&w.nominal_leaf_rects(), &focused, direction)
                    .map(|target| (focused, target))
            })
        else {
            return;
        };
        let mut swapped = false;
        s.workspace.mutate_session(&session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                if term_window.layout.swap(&focused, &target) {
                    term_window.active_pane_id = focused;
                    term_window.zoomed = false;
                    swapped = true;
                }
            }
        });
        if swapped {
            s.save_to_store();
        }
    });
}

/// Half-page scrollback (⌃⌘↑ / ⌃⌘↓) on the FOCUSED pane of the active session's
/// active window.
///
/// No keymap action reaches a terminal view today (Phase 0's Shift+nav scrollback
/// lives in the view's own key listener, not the keymap), so this rides the seam
/// that does exist: [`PtyManager::pane_handle`] resolves the focused pane's
/// [`TerminalSessionHandle`], and the scroll + `notify` follow
/// `perform_scrollback`'s repaint discipline (`nice-term-view`'s `view.rs`).
///
/// **The pane comes from the MODEL**, via
/// [`effective_pane_id`](nice_model::TermWindow::effective_pane_id) — the same
/// resolution the render's `active_window_target` uses — not from the pty map's
/// active-pane mirror. The mirror is only re-synced on window activation, so
/// after a `⌃⌘⇧hjkl` move, a click into another pane, or a split (all of which
/// move focus without re-activating the pill) it still names the pane the user
/// LEFT, and the scroll would land on a terminal that isn't on screen.
///
/// **Alt-screen gate.** On the alternate screen (vim, less, any full-screen TUI)
/// this does nothing at all. The chord is a keymap binding, so it never encoded to
/// the pty and there is nothing to fall through TO — unlike Shift+PageUp, which
/// falls through and encodes.
///
/// Held (not yet spawned) sessions work automatically: the handle's scroll methods
/// no-op pre-spawn.
///
/// [`PtyManager::pane_handle`]: crate::pty_manager::PtyManager::pane_handle
/// [`TerminalSessionHandle`]: nice_term_view::TerminalSessionHandle
fn scroll_active_window_half_page(cx: &mut App, toward_history: bool) {
    with_active_state(cx, |s, cx| {
        let Some((session_id, term_window_id)) = active_session_window(s) else {
            return;
        };
        let Some(pane_id) =
            with_active_window(s, &session_id, &term_window_id, |w| w.effective_pane_id())
        else {
            return;
        };
        let Some(handle) = s.ptys.pane_handle(&session_id, &term_window_id, &pane_id) else {
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

// ===========================================================================
// Phase 3 (tmux port) — copy mode + scrollback search
// ===========================================================================

/// The focused pane of the active session's active window — the pane every
/// Phase 3 verb operates on (copy mode and search are per-PANE). The resolution
/// chain is [`scroll_active_window_half_page`]'s: model-resolved
/// [`effective_pane_id`](nice_model::TermWindow::effective_pane_id), never the
/// pty map's active-pane mirror, which goes stale the moment focus moves without
/// re-activating the pill.
fn focused_pane_key(state: &WindowState) -> Option<(String, String, String)> {
    let (session_id, term_window_id) = active_session_window(state)?;
    let pane_id = with_active_window(state, &session_id, &term_window_id, |w| {
        w.effective_pane_id()
    })?;
    Some((session_id, term_window_id, pane_id))
}

/// `⌃⌘C` — toggle the focused pane's copy mode (D2). Entry seeds the vi cursor
/// at the terminal cursor; exit clears the selection and the search and returns
/// the viewport to the bottom (P6). Both halves are the handle's, so this is the
/// resolution plus one call.
///
/// **No alt-screen gate**, unlike the half-page template it otherwise copies:
/// P10 allows copy mode on the alternate screen, where motions and search work
/// over the visible grid — selecting what vim or less is showing is a real use.
///
/// An open search bar closes with the mode: the mode going false is one of the
/// three stale conditions the host checks on its next render
/// ([`search_bar_is_stale`](crate::search_bar::search_bar_is_stale)), which is
/// what makes this action safe to fire while the bar holds key focus.
fn toggle_copy_mode(cx: &mut App) {
    with_active_state(cx, |s, cx| {
        let Some((session_id, term_window_id, pane_id)) = focused_pane_key(s) else {
            return;
        };
        let Some(handle) = s.ptys.pane_handle(&session_id, &term_window_id, &pane_id) else {
            return;
        };
        handle.update(cx, |h, hcx| {
            h.toggle_copy_mode();
            hcx.notify();
        });
    });
}

/// `⌃⌘/` — open the focused pane's scrollback search field, searching BACKWARD
/// (P7: history-ward is the flagship "find the thing that scrolled past"
/// direction; in-mode `/` and `?` keep vim's meanings and arrive as a routed
/// [`SearchRequested`](nice_term_view::TerminalEvent) instead).
///
/// [`WindowState::open_search_bar`] does the work — enter copy mode, start a
/// fresh search (or refocus the identical bar that a click into the pane left
/// open), open the bar and defer its focus. The deferral is not optional here: a
/// keymap handler is an App-level closure with no `&mut Window`, so focusing
/// inline would silently not focus.
fn open_scrollback_search(cx: &mut App) {
    with_active_state(cx, |s, cx| {
        let Some((session_id, term_window_id, pane_id)) = focused_pane_key(s) else {
            return;
        };
        s.open_search_bar(&session_id, &term_window_id, &pane_id, true, cx);
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
        ShortcutAction::SplitDown => Box::new(SplitDown),
        ShortcutAction::SplitRight => Box::new(SplitRight),
        ShortcutAction::ZoomPane => Box::new(ZoomPane),
        ShortcutAction::BreakPane => Box::new(BreakPane),
        ShortcutAction::ResizePaneLeft => Box::new(ResizePaneLeft),
        ShortcutAction::ResizePaneDown => Box::new(ResizePaneDown),
        ShortcutAction::ResizePaneUp => Box::new(ResizePaneUp),
        ShortcutAction::ResizePaneRight => Box::new(ResizePaneRight),
        ShortcutAction::SwapPaneLeft => Box::new(SwapPaneLeft),
        ShortcutAction::SwapPaneDown => Box::new(SwapPaneDown),
        ShortcutAction::SwapPaneUp => Box::new(SwapPaneUp),
        ShortcutAction::SwapPaneRight => Box::new(SwapPaneRight),
        ShortcutAction::CopyMode => Box::new(CopyMode),
        ShortcutAction::SearchScrollback => Box::new(SearchScrollback),
        ShortcutAction::DetachSession => Box::new(DetachSession),
        ShortcutAction::AdoptDetachedSession => Box::new(AdoptDetachedSession),
        ShortcutAction::TearOffPane => Box::new(TearOffPane),
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
/// before the hint overlay appears. Long enough that a fast `⌃⌘L` never flashes
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
/// working overlay: the next-pill chord's modifiers — [`ShortcutAction::NextWindow`]
/// (`⌃⌘L` by default), falling back to [`ShortcutAction::WindowByIndex`] (`⌃⌘1`…`⌃⌘9`)
/// when that one is unbound.
///
/// The fallback is the badge row itself, which is exactly what the overlay
/// PAINTS — the 2026-08-11 hjkl-ladder revision moved `FocusPaneRight` up to the
/// `⌃⌘⇧` rung, where it is an inert Phase-2 action, so it can no longer speak for
/// this hold.
///
/// `None` — never show the overlay — when both are unbound. A combo bound to a
/// BARE key (no modifiers) is rejected by [`hint_hold_matches`], which owns that
/// rule.
///
/// The badges the overlay draws are [`ShortcutAction::WindowByIndex`]'s digits,
/// which share this modifier set in the defaults. A user who rebinds ONLY the
/// next-pill chord still gets the badges on that hold; re-deriving them per action
/// is a Phase 2 problem, when panes (not just pills) get numbers.
fn hint_relevant_modifiers(cx: &App) -> Option<nice_model::shortcuts::Modifiers> {
    [ShortcutAction::NextWindow, ShortcutAction::WindowByIndex]
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

/// The union of the two sidebar-session-cycle shortcuts' modifier sets (⌃⌘ since
/// the hjkl-ladder revision put `⌃⌘J`/`⌃⌘K` on the sessions — so holding the
/// scheme's pair now floats the collapsed-sidebar peek AND, after the debounce,
/// the pill hint badges) — the modifiers whose full release ends a peek. Reads the LIVE
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

    /// ⌃⌘ — the default sidebar-session-cycle modifier set the `should_end_peek`
    /// tests exercise (equivalent to `peek_relevant_modifiers` with no store
    /// installed, since the hjkl ladder put `⌃⌘J`/`⌃⌘K` on the sessions).
    fn peek_pair() -> Modifiers {
        mods(true, false, false, true)
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
        let relevant = peek_pair(); // ⌃⌘
        // Both held → stays.
        assert!(!should_end_peek(mods(true, false, false, true), relevant, false));
        // Only ⌘ still held (⌃ released) → stays (Swift keeps it until BOTH lift).
        assert!(!should_end_peek(mods(false, false, false, true), relevant, false));
        // Only ⌃ still held → stays.
        assert!(!should_end_peek(mods(true, false, false, false), relevant, false));
    }

    #[test]
    fn peek_ends_when_all_relevant_modifiers_release() {
        let relevant = peek_pair();
        // Nothing held → end.
        assert!(should_end_peek(mods(false, false, false, false), relevant, false));
        // An unrelated modifier (⌥ / ⇧) held but neither ⌃ nor ⌘ → still end.
        assert!(should_end_peek(mods(false, true, true, false), relevant, false));
    }

    #[test]
    fn mouse_pin_keeps_the_peek_even_with_no_modifiers() {
        // The pointer pinning the overlay wins over modifier release (dossier
        // G6 / R10's hover pin) — never ends while pinned.
        let relevant = peek_pair();
        assert!(!should_end_peek(mods(false, false, false, false), relevant, true));
    }

    /// D4 / Validation §2a(e): `peek_relevant_modifiers` reads the LIVE map. With no
    /// store it falls back to the ⌃⌘ defaults (the hjkl ladder's session rung); after
    /// rebinding a sidebar-session chord to an ⌥⇧ combo the relevant set tracks the NEW
    /// modifiers (unioned with the other, still-default sidebar-session combo).
    #[gpui::test]
    fn peek_relevant_modifiers_default_then_live(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // No store yet ⇒ the defaults: both sidebar-session combos are ⌃⌘.
            let r = peek_relevant_modifiers(cx);
            assert!(r.platform && r.control, "⌃⌘ hold the peek by default");
            assert!(!r.alt && !r.shift, "no other modifier holds the peek by default");

            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings("peek-live")));
            // Rebind NextSidebarSession ⌃⌘J -> ⌥⇧↓ (persist + rebuild happen inside).
            ShortcutBindings::set_binding(
                cx,
                ShortcutAction::NextSidebarSession,
                OwnedCombo::from_token("alt-shift-down"),
            );

            let r = peek_relevant_modifiers(cx);
            assert!(r.alt, "the rebound ⌥ now holds the peek");
            assert!(r.shift, "the rebound ⇧ now holds the peek");
            // PrevSidebarSession is still ⌃⌘, so ⌃⌘ remain relevant too (the union).
            assert!(r.platform && r.control, "the other sidebar-session combo keeps ⌃⌘ relevant");
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
            // Rebind NextWindow (the primary source) ⌃⌘L -> ⌥⇧L.
            ShortcutBindings::set_binding(
                cx,
                ShortcutAction::NextWindow,
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

    /// `NextWindow` unbound ⇒ the fallback source (`WindowByIndex`, the row whose
    /// digits the overlay actually paints) supplies the pair, so the overlay
    /// survives clearing the next-pill chord.
    #[gpui::test]
    fn hint_modifiers_fall_back_to_window_by_index(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings(
                "hint-fallback",
            )));
            ShortcutBindings::set_binding(cx, ShortcutAction::NextWindow, None);
            assert_eq!(
                hint_relevant_modifiers(cx),
                Some(control_command()),
                "⌃⌘1-9 still defines the hold"
            );

            // Both sources cleared ⇒ no overlay at all.
            ShortcutBindings::set_binding(cx, ShortcutAction::WindowByIndex, None);
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

    /// The default board reaches the live keymap: every chord of the hjkl ladder
    /// is bound to its action on the right rung, and the chords the 2026-08-11
    /// revision freed (⌃⌘[ / ⌃⌘] and the ⌘⌥ arrows) drive nothing at all.
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
            // Nothing AT ALL is bound to `chord` — the freed-chord assertion, which
            // a per-action `!bound` could never make.
            let unbound_entirely = |chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .all_bindings_for_input(std::slice::from_ref(&ks))
                    .is_empty()
            };

            // The ladder's bare-⌃⌘ rung: h/l pills, j/k sidebar sessions.
            assert!(bound(&NextWindow, "cmd-ctrl-l"), "⌃⌘L steps to the next window");
            assert!(bound(&PrevWindow, "cmd-ctrl-h"), "⌃⌘H steps to the previous window");
            assert!(
                bound(&NextSidebarSession, "cmd-ctrl-j"),
                "⌃⌘J steps down the sidebar sessions"
            );
            assert!(
                bound(&PrevSidebarSession, "cmd-ctrl-k"),
                "⌃⌘K steps up the sidebar sessions"
            );

            // The chords the revision freed drive NOTHING (not merely "not this
            // action"): the old pill pair and the old sidebar-session arrows.
            assert!(unbound_entirely("cmd-ctrl-]"), "⌃⌘] is freed");
            assert!(unbound_entirely("cmd-ctrl-["), "⌃⌘[ is freed");
            assert!(unbound_entirely("cmd-alt-down"), "⌘⌥↓ is freed");
            assert!(unbound_entirely("cmd-alt-up"), "⌘⌥↑ is freed");
            assert!(unbound_entirely("cmd-alt-right"), "⌘⌥→ is freed");
            assert!(unbound_entirely("cmd-alt-left"), "⌘⌥← is freed");

            // The ⌃⌘⇧ rung: directional pane focus.
            assert!(bound(&FocusPaneLeft, "cmd-ctrl-shift-h"));
            assert!(bound(&FocusPaneDown, "cmd-ctrl-shift-j"));
            assert!(bound(&FocusPaneUp, "cmd-ctrl-shift-k"));
            assert!(bound(&FocusPaneRight, "cmd-ctrl-shift-l"));

            assert!(bound(&LastActiveWindow, "cmd-ctrl-o"));
            // Half-page scroll lives on the ARROWS. It shipped on ⌃⌘U/⌃⌘D, but
            // macOS's dictionary hotkey swallows a real ⌃⌘D keydown before Nice
            // ever sees it — invisible to the `dispatch_keystroke` scenario, which
            // injects downstream of the OS intercept, so the board is pinned here.
            assert!(bound(&ScrollHalfPageUp, "cmd-ctrl-up"));
            assert!(bound(&ScrollHalfPageDown, "cmd-ctrl-down"));
            assert!(unbound_entirely("cmd-ctrl-u"), "⌃⌘U is freed");
            assert!(
                unbound_entirely("cmd-ctrl-d"),
                "⌃⌘D is freed — a pure reserved chord again"
            );

            // D2: all nine digits are live off the one stored row.
            for index in 1u8..=9 {
                assert!(
                    bound(&SelectWindowIndex { index }, &format!("cmd-ctrl-{index}")),
                    "⌃⌘{index} must select window {index}"
                );
            }
        });
    }

    /// Phase 2's board reaches the live keymap: the pane verbs are bound where
    /// the ladder says, and the chords D2 released drive nothing at all.
    ///
    /// This is the board only. Whether macOS DELIVERS these chords — the
    /// ⌃⌘D-class risk, which sits squarely on the ⌃⌥⌘ rungs — no in-process test
    /// can see: injected keystrokes enter downstream of the OS intercept. The
    /// hand feel-check is the gate for that.
    #[gpui::test]
    fn phase_two_pane_chords_are_live_in_the_keymap(cx: &mut gpui::TestAppContext) {
        use gpui::Keystroke;

        cx.update(|cx| {
            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings("phase2")));
            rebuild_keymap(cx);

            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let bound = |action: &dyn Action, chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .bindings_for_action(action)
                    .any(|b| matches!(b.match_keystrokes(std::slice::from_ref(&ks)), Some(false)))
            };
            let unbound_entirely = |chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .all_bindings_for_input(std::slice::from_ref(&ks))
                    .is_empty()
            };

            // D2's divider mnemonics, on the bare ⌃⌘ rung with the other
            // directionless pane verbs.
            assert!(bound(&SplitDown, "cmd-ctrl--"), "⌃⌘- splits down");
            assert!(bound(&SplitRight, "cmd-ctrl-\\"), "⌃⌘\\ splits right");
            assert!(bound(&ZoomPane, "cmd-ctrl-z"));
            assert!(bound(&BreakPane, "cmd-ctrl-b"));

            // The ladder's top two rungs, no longer reserved: ⌃⌥⌘ resizes,
            // ⌃⌥⌘⇧ swaps, hjkl in both.
            assert!(bound(&ResizePaneLeft, "cmd-ctrl-alt-h"));
            assert!(bound(&ResizePaneDown, "cmd-ctrl-alt-j"));
            assert!(bound(&ResizePaneUp, "cmd-ctrl-alt-k"));
            assert!(bound(&ResizePaneRight, "cmd-ctrl-alt-l"));
            assert!(bound(&SwapPaneLeft, "cmd-ctrl-alt-shift-h"));
            assert!(bound(&SwapPaneDown, "cmd-ctrl-alt-shift-j"));
            assert!(bound(&SwapPaneUp, "cmd-ctrl-alt-shift-k"));
            assert!(bound(&SwapPaneRight, "cmd-ctrl-alt-shift-l"));

            // ⌃⌘V and ⌃⌘S were released, not re-spent — nothing at all is bound
            // to them (the state ⌃⌘U reached in Phase 1).
            assert!(unbound_entirely("cmd-ctrl-v"), "⌃⌘V is freed");
            assert!(unbound_entirely("cmd-ctrl-s"), "⌃⌘S is freed");

            // The plain-⌘ neighbors of the new chords are untouched: ⌘- still
            // shrinks the font and ⌘B still toggles the sidebar. The modifier
            // set is what separates them.
            assert!(bound(&DecreaseFontSize, "cmd--"));
            assert!(bound(&ToggleSidebar, "cmd-b"));
        });
    }

    /// Phase 3's two chords reach the live keymap: ⌃⌘C enters copy mode and
    /// ⌃⌘/ opens the scrollback search — the two remaining directionless pane
    /// verbs on the bare ⌃⌘ rung.
    ///
    /// Board only, same caveat as the Phase 2 test above: whether macOS delivers
    /// them is the hand feel-check's gate. ⌃⌘C's neighbour risk is ⌘C, which is
    /// why the plain-⌘ copy chord is asserted untouched here.
    #[gpui::test]
    fn phase_three_copy_mode_chords_are_live_in_the_keymap(cx: &mut gpui::TestAppContext) {
        use gpui::Keystroke;

        cx.update(|cx| {
            cx.set_global(ShortcutBindings::with_defaults(unique_temp_ui_settings("phase3")));
            rebuild_keymap(cx);

            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let bound = |action: &dyn Action, chord: &str| -> bool {
                let ks = Keystroke::parse(chord).expect("test chord parses");
                keymap
                    .bindings_for_action(action)
                    .any(|b| matches!(b.match_keystrokes(std::slice::from_ref(&ks)), Some(false)))
            };

            assert!(bound(&CopyMode, "cmd-ctrl-c"), "⌃⌘C enters copy mode");
            assert!(
                bound(&SearchScrollback, "cmd-ctrl-/"),
                "⌃⌘/ opens the scrollback search"
            );

            // ⌃⌘/ was the reserved table's last `FuturePhase` entry; a chord may
            // never be both reserved and a default, so it is gone from there.
            assert_eq!(
                nice_model::shortcuts::reserved_combo(
                    &OwnedCombo::from_token("cmd-ctrl-/").expect("token parses")
                ),
                None,
                "⌃⌘/ was promoted out of the reserved table"
            );

            // ⌘C (terminal copy) is a different modifier set and stays free of
            // the keymap entirely — the slipped-Ctrl cost D2 accepts is entering
            // copy mode, never losing a copy.
            let plain_copy = Keystroke::parse("cmd-c").expect("test chord parses");
            assert!(
                keymap
                    .all_bindings_for_input(std::slice::from_ref(&plain_copy))
                    .is_empty(),
                "⌘C stays the terminal's copy, unbound in the keymap"
            );

            // -- Phase 4 (detach) ------------------------------------------
            // ⌃⌘⇧D joins the four pane-focus letters on the ⌃⌘⇧ rung. Bare
            // ⌃⌘D is unusable — macOS's dictionary hotkey eats that keydown
            // before the app sees it — and injected keystrokes enter
            // downstream of that intercept, so THIS assertion proves the
            // binding exists, never that the OS lets it through. That is a
            // hand gate.
            assert!(
                bound(&DetachSession, "cmd-ctrl-shift-d"),
                "⌃⌘⇧D detaches the active session"
            );
            // Its inverse sits on the BARE rung, where `a` was still free — no
            // OS intercept in the way, so this binding is provable end to end.
            assert!(
                bound(&AdoptDetachedSession, "cmd-ctrl-a"),
                "⌃⌘A adopts the pool head into the active window"
            );
            // "N = New window": the ⌃⌘ echo of ⌘N. The reservation on ⌘N is
            // exact-combo, so the held-modifier rung's `n` was free.
            assert!(
                bound(&TearOffPane, "cmd-ctrl-n"),
                "⌃⌘N tears the focused pane into a window of its own"
            );
        });
    }

    // MARK: - Phase 2 pane verbs (handler behavior)

    mod pane_verbs {
        use super::super::*;
        use crate::window_registry::WindowRegistry;
        use crate::window_state::WindowState;
        use gpui::{Entity, TestAppContext};
        use nice_model::{
            Pane, PaneLayout, Session, Side, TermWindow, TermWindowKind, WorkspaceModel,
        };

        const SESSION: &str = "t1";
        const PILL: &str = "t1-claude";

        /// A registered window holding one project / one session / one
        /// never-split Claude pill, active and focused — the shape every pill
        /// starts in. No ptys are stood up, so the verbs exercise their model
        /// half (which is all but the split's eager spawn).
        fn window_with_one_pill(cx: &mut TestAppContext) -> Entity<WindowState> {
            let mut model = WorkspaceModel::new("/home/u");
            model.ensure_project("p", "P", "/home/u/proj");
            let mut session = Session::new(SESSION, "New session", "/home/u/proj");
            session.windows = vec![TermWindow::new(PILL, "Claude", TermWindowKind::Claude)];
            session.active_window_id = Some(PILL.to_string());
            let pi = model.projects.iter().position(|p| p.id == "p").unwrap();
            model.projects[pi].sessions.push(session);
            model.select_session(SESSION);

            let state = cx.new(|_cx| WindowState::with_model(model));
            let id = cx.add_window(|_w, _cx| gpui::Empty).window_id();
            cx.update(|app| {
                app.set_global(WindowRegistry::default());
                WindowRegistry::register(app, id, state.clone());
            });
            state
        }

        /// Split the pill into `Beside{ claude, shell }` by hand (no pty), with
        /// focus back on the Claude pane — the "Claude beside a shell" layout D1
        /// exists for, and the fixture most of these tests start from.
        fn split_by_hand(cx: &mut TestAppContext, state: &Entity<WindowState>, shell_id: &str) {
            let shell_id = shell_id.to_string();
            state.update(cx, |s, _cx| {
                s.workspace.mutate_session(SESSION, |session| {
                    let window = &mut session.windows[0];
                    assert!(window.layout.split(
                        PILL,
                        SplitOrient::Beside,
                        Pane::new(&shell_id, TermWindowKind::Terminal),
                    ));
                    window.active_pane_id = PILL.to_string();
                });
            });
        }

        /// The active pill, for assertions.
        fn pill(cx: &mut TestAppContext, state: &Entity<WindowState>) -> TermWindow {
            state.update(cx, |s, _cx| {
                s.workspace
                    .session_for(SESSION)
                    .unwrap()
                    .windows
                    .iter()
                    .find(|w| w.id == PILL)
                    .cloned()
                    .expect("the pill is still there")
            })
        }

        fn focused(cx: &mut TestAppContext, state: &Entity<WindowState>) -> String {
            pill(cx, state).active_pane_id
        }

        #[gpui::test]
        fn focus_walks_the_tree_and_stops_at_the_edge(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            split_by_hand(cx, &state, "shell");

            cx.update(|app| focus_pane(app, PaneDirection::Right));
            assert_eq!(focused(cx, &state), "shell", "⌃⌘⇧L stepped right");

            // P5: no wrap and no fall-through to pill nav — the right edge is
            // simply where the walk stops.
            cx.update(|app| focus_pane(app, PaneDirection::Right));
            assert_eq!(focused(cx, &state), "shell", "the edge is a no-op");
            // Nothing is stacked, so the vertical directions have no neighbor.
            cx.update(|app| focus_pane(app, PaneDirection::Up));
            assert_eq!(focused(cx, &state), "shell");

            cx.update(|app| focus_pane(app, PaneDirection::Left));
            assert_eq!(focused(cx, &state), PILL, "and back left again");
        }

        #[gpui::test]
        fn a_focus_move_unzooms_but_a_refused_one_does_not(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            split_by_hand(cx, &state, "shell");
            let set_zoom = |cx: &mut TestAppContext| {
                state.update(cx, |s, _cx| {
                    s.workspace.mutate_session(SESSION, |session| {
                        session.windows[0].zoomed = true;
                    });
                });
            };

            // P4: a real move un-zooms first, then applies.
            set_zoom(cx);
            cx.update(|app| focus_pane(app, PaneDirection::Right));
            assert_eq!(focused(cx, &state), "shell");
            assert!(!pill(cx, &state).zoomed, "the move un-zoomed");

            // A refused move changes NOTHING — including the zoom. "No-op" would
            // be a lie if the chord still tore the zoom down.
            set_zoom(cx);
            cx.update(|app| focus_pane(app, PaneDirection::Right));
            assert!(pill(cx, &state).zoomed, "an edge no-op leaves zoom alone");
        }

        #[gpui::test]
        fn split_adds_a_shell_pane_and_focus_follows_it(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            state.update(cx, |s, _cx| s.set_pane_content_size(Some((1000.0, 600.0))));

            cx.update(|app| split_focused_pane(app, SplitOrient::Beside));
            let pill = pill(cx, &state);
            assert_eq!(pill.layout.leaf_count(), 2);
            let new_id = pill.active_pane_id.clone();
            assert_ne!(new_id, PILL, "focus follows the NEW pane (tmux)");

            let new_pane = pill.layout.pane(&new_id).expect("the new leaf");
            assert_eq!(
                new_pane.kind,
                TermWindowKind::Terminal,
                "D1: a split pane is always a plain shell, never a second Claude"
            );
            assert_eq!(
                new_pane.cwd.as_deref(),
                Some("/home/u/proj"),
                "the shell is seeded in the focused pane's cwd"
            );
            assert_eq!(
                pill.kind,
                TermWindowKind::Claude,
                "the pill is still a Claude pill — one Claude leaf, D1 intact"
            );
            assert!(pill.layout_is_valid());

            // Splitting again bisects the pane that now has focus, so the second
            // split lands in the right half, not next to the first.
            cx.update(|app| split_focused_pane(app, SplitOrient::Stacked));
            let pill = self::pill(cx, &state);
            assert_eq!(pill.layout.leaf_count(), 3);
            assert!(pill
                .layout
                .path_to_pane(&pill.active_pane_id)
                .is_some_and(|p| p.first() == Some(&Side::Second)));
        }

        #[gpui::test]
        fn split_is_refused_under_the_minimum_pane_size(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            // P6: (200 - 6) / 2 = 97 px wide halves, under PANE_MIN_WIDTH.
            state.update(cx, |s, _cx| s.set_pane_content_size(Some((200.0, 600.0))));
            cx.update(|app| split_focused_pane(app, SplitOrient::Beside));
            assert_eq!(
                pill(cx, &state).layout.leaf_count(),
                1,
                "a split with no room for both halves does nothing at all"
            );

            // The same pane area is tall enough to stack, so the OTHER verb is
            // still allowed — the refusal is per axis, not per pane.
            cx.update(|app| split_focused_pane(app, SplitOrient::Stacked));
            assert_eq!(pill(cx, &state).layout.leaf_count(), 2);
        }

        #[gpui::test]
        fn split_without_a_painted_size_proceeds(cx: &mut TestAppContext) {
            // Never painted ⇒ no px context to refuse against, so the split goes
            // ahead at an even ratio rather than guessing.
            let state = window_with_one_pill(cx);
            cx.update(|app| split_focused_pane(app, SplitOrient::Beside));
            assert_eq!(pill(cx, &state).layout.leaf_count(), 2);
        }

        #[gpui::test]
        fn zoom_toggles_only_on_a_split_pill(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            cx.update(|app| toggle_zoom(app));
            assert!(
                !pill(cx, &state).zoomed,
                "a single-pane pill has nothing to zoom"
            );

            split_by_hand(cx, &state, "shell");
            cx.update(|app| toggle_zoom(app));
            assert!(pill(cx, &state).zoomed);
            cx.update(|app| toggle_zoom(app));
            assert!(!pill(cx, &state).zoomed, "and toggles back off");
        }

        #[gpui::test]
        fn resize_walks_the_divider_by_a_px_step(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            split_by_hand(cx, &state, "shell");
            state.update(cx, |s, _cx| s.set_pane_content_size(Some((1000.0, 600.0))));

            cx.update(|app| resize_pane(app, PaneDirection::Right));
            let ratio = match &pill(cx, &state).layout {
                PaneLayout::Split { ratio, .. } => *ratio,
                _ => panic!("still a split"),
            };
            // 40 px of a 994 px splittable extent, on top of the even 0.5.
            assert!(
                (ratio - (0.5 + 40.0 / 994.0)).abs() < 1e-4,
                "the step is px-denominated, got {ratio}"
            );

            // The perpendicular direction has no matching ancestor (P7).
            cx.update(|app| resize_pane(app, PaneDirection::Down));
            let after = match &pill(cx, &state).layout {
                PaneLayout::Split { ratio, .. } => *ratio,
                _ => panic!("still a split"),
            };
            assert_eq!(after, ratio, "no Stacked ancestor ⇒ nothing moved");
        }

        #[gpui::test]
        fn resize_without_a_painted_size_is_a_noop(cx: &mut TestAppContext) {
            // No px context ⇒ no scale to convert the step in, so the verb
            // declines rather than inventing a ratio delta.
            let state = window_with_one_pill(cx);
            split_by_hand(cx, &state, "shell");
            cx.update(|app| resize_pane(app, PaneDirection::Right));
            match &pill(cx, &state).layout {
                PaneLayout::Split { ratio, .. } => assert_eq!(*ratio, 0.5),
                _ => panic!("still a split"),
            }
        }

        #[gpui::test]
        fn swap_trades_places_and_focus_follows_the_content(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            split_by_hand(cx, &state, "shell");
            state.update(cx, |s, _cx| {
                s.workspace.mutate_session(SESSION, |session| {
                    session.windows[0].layout.set_ratio_at(&[], 0.7);
                });
            });

            cx.update(|app| swap_pane(app, PaneDirection::Right));
            let pill = pill(cx, &state);
            assert_eq!(
                pill.layout.leaves().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
                vec!["shell", PILL],
                "the payloads traded slots"
            );
            match &pill.layout {
                PaneLayout::Split { ratio, .. } => {
                    assert_eq!(*ratio, 0.7, "P8: structure and ratios don't move")
                }
                _ => panic!("still a split"),
            }
            assert_eq!(
                pill.active_pane_id, PILL,
                "focus followed the content, which now paints on the right"
            );
            assert!(pill.layout_is_valid());

            // Swapping at the edge does nothing (same rule as directional focus).
            cx.update(|app| swap_pane(app, PaneDirection::Right));
            assert_eq!(
                self::pill(cx, &state)
                    .layout
                    .leaves()
                    .iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["shell", PILL]
            );
        }

        #[gpui::test]
        fn break_pane_moves_a_shell_out_and_refuses_the_claude_pane(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            split_by_hand(cx, &state, "shell");

            // Focused on the Claude pane: P3 refuses (a Claude leaf may not
            // become a pill through this path).
            cx.update(|app| break_focused_pane(app));
            let windows = |cx: &mut TestAppContext| {
                state.update(cx, |s, _cx| {
                    s.workspace.session_for(SESSION).unwrap().windows.len()
                })
            };
            assert_eq!(windows(cx), 1, "break-pane on the Claude pane is a no-op");

            // Focused on the shell pane: it leaves as its own pill, and focus
            // goes with it (tmux `break-pane`).
            cx.update(|app| focus_pane(app, PaneDirection::Right));
            cx.update(|app| break_focused_pane(app));
            assert_eq!(windows(cx), 2, "the shell became a pill of its own");

            let (active, source) = state.update(cx, |s, _cx| {
                let session = s.workspace.session_for(SESSION).unwrap();
                (
                    session.active_window_id.clone(),
                    session
                        .windows
                        .iter()
                        .find(|w| w.id == PILL)
                        .unwrap()
                        .clone(),
                )
            });
            assert_ne!(active.as_deref(), Some(PILL), "focus followed the new pill");
            assert_eq!(source.layout.leaf_count(), 1, "the source pill collapsed");
            assert!(source.layout_is_valid());
        }

        /// A pill that was never split behaves exactly as before: every pane
        /// verb that needs a second pane declines, and nothing about the pill
        /// changes. This is the promise the whole phase rests on.
        #[gpui::test]
        fn a_never_split_pill_is_untouched_by_every_pane_verb(cx: &mut TestAppContext) {
            let state = window_with_one_pill(cx);
            let before = pill(cx, &state);
            cx.update(|app| {
                for direction in [
                    PaneDirection::Left,
                    PaneDirection::Down,
                    PaneDirection::Up,
                    PaneDirection::Right,
                ] {
                    focus_pane(app, direction);
                    resize_pane(app, direction);
                    swap_pane(app, direction);
                }
                toggle_zoom(app);
                break_focused_pane(app);
            });
            assert_eq!(pill(cx, &state), before);
        }
    }

    /// P6's split refusal, measured against the painted layout: a pane splits
    /// only while both halves clear the minimum, and the divider's own px come
    /// out of the splittable extent first.
    #[test]
    fn split_fits_refuses_halves_under_the_minimum() {
        use nice_model::{Pane, PaneLayout};

        let layout = PaneLayout::single(Pane::new("a", nice_model::TermWindowKind::Terminal));
        let content = |w: f32, h: f32| PaneRect::new(0.0, 0.0, w, h);

        // Exactly at the limit: 2 * 120 + 6 px of divider.
        assert!(split_fits(&layout, "a", SplitOrient::Beside, content(246.0, 600.0)));
        assert!(!split_fits(&layout, "a", SplitOrient::Beside, content(245.0, 600.0)));
        // The stacked axis has its own (shorter) minimum: 2 * 80 + 6.
        assert!(split_fits(&layout, "a", SplitOrient::Stacked, content(1000.0, 166.0)));
        assert!(!split_fits(&layout, "a", SplitOrient::Stacked, content(1000.0, 165.0)));
        // A pane that is not in the tree cannot be measured, so it is not
        // refused here — the split itself declines it.
        assert!(split_fits(&layout, "zzz", SplitOrient::Beside, content(10.0, 10.0)));
    }
}

// ===========================================================================
// Phase 4 (tmux port) — the detach action's handler behavior
// ===========================================================================

#[cfg(test)]
mod detach_action_tests {
    use super::{detach_active_session, detach_session_from_window};
    use crate::detached_pool::{DetachedPool, DetachedPoolGlobal};
    use crate::pty_manager::DissolveTerminus;
    use crate::window_registry::WindowRegistry;
    use crate::window_state::WindowState;
    use gpui::{AppContext, Entity, TestAppContext};
    use nice_model::{Session, TermWindow, TermWindowKind, WorkspaceModel};

    /// A registered window with the seeded Terminals/Main session plus one extra
    /// live project session, and an installed (empty) pool.
    fn window_with_two_sessions(cx: &mut TestAppContext) -> Entity<WindowState> {
        let mut model = WorkspaceModel::new("/home/u");
        let pi = model.ensure_project("p", "P", "/home/u/proj");
        let mut session = Session::new("t1", "Work", "/home/u/proj");
        session.windows = vec![TermWindow::new("t1-w", "Terminal 1", TermWindowKind::Terminal)];
        session.active_window_id = Some("t1-w".to_string());
        model.projects[pi].sessions.push(session);
        model.select_session("t1");

        let state = cx.new(|_cx| WindowState::with_model(model));
        let id = cx.add_window(|_w, _cx| gpui::Empty).window_id();
        cx.update(|app| {
            app.set_global(WindowRegistry::default());
            WindowRegistry::register(app, id, state.clone());
            let pool = app.new(|_cx| DetachedPool::new());
            app.set_global(DetachedPoolGlobal(pool));
        });
        state
    }

    fn pooled_ids(cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|app| {
            app.global::<DetachedPoolGlobal>()
                .0
                .read(app)
                .entries()
                .iter()
                .map(|e| e.session.id.clone())
                .collect()
        })
    }

    /// Pool row titles in pool order — what an assertion about ORDER has to read,
    /// since the ids are minted at detach.
    fn pooled_titles(cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|app| {
            app.global::<DetachedPoolGlobal>()
                .0
                .read(app)
                .entries()
                .iter()
                .map(|e| e.session.title.clone())
                .collect()
        })
    }

    /// ⌃⌘⇧D pools the ACTIVE session, leaves the rest of the window alone, and
    /// falls the active session back in navigable order. The pooled row carries a
    /// FRESH id: a session is re-keyed on its way out of a window
    /// ([`WindowState::detach_session`]), because the pool is app-global while a
    /// `WorkspaceModel`'s ids are only unique inside one window.
    #[gpui::test]
    fn detach_action_pools_the_active_session(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        cx.update(detach_active_session);

        let pooled = pooled_ids(cx);
        assert_eq!(pooled.len(), 1, "one row on the pool: {pooled:?}");
        assert_ne!(pooled[0], "t1", "re-keyed on the way out");
        state.update(cx, |s, _cx| {
            assert!(
                s.workspace.session_for("t1").is_none(),
                "the detached session left this window"
            );
            assert!(
                s.workspace.session_for("terminals-main").is_some(),
                "its neighbours stay attached"
            );
            assert_eq!(
                s.workspace.active_session_id(),
                Some("terminals-main"),
                "the active session falls back in navigable order"
            );
        });
    }

    /// With no pool installed the chord is inert — it is still registered (so it
    /// never leaks to the pty), but nothing is extracted. This is the shape every
    /// scenario and `run_selftest` run in.
    #[gpui::test]
    fn detach_action_is_inert_without_a_pool(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        cx.update(|app| app.remove_global::<DetachedPoolGlobal>());
        cx.update(detach_active_session);
        state.update(cx, |s, _cx| {
            assert!(
                s.workspace.session_for("t1").is_some(),
                "no pool ⇒ the chord changes nothing"
            );
        });
    }

    /// §P7: detaching the LAST session empties the window, which then closes
    /// through the normal close path — so its disk slot is marked for removal
    /// rather than restoring next launch as a broken empty window.
    #[gpui::test]
    fn detaching_the_last_session_marks_the_window_for_close(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        cx.update(|app| {
            detach_session_from_window(app, &state, "t1");
            assert!(
                !state.read(app).user_initiated_close(),
                "one of two sessions detached — the window stays"
            );
            detach_session_from_window(app, &state, "terminals-main");
        });

        let pooled = pooled_ids(cx);
        assert_eq!(pooled.len(), 2, "both sessions pooled: {pooled:?}");
        assert_ne!(pooled[0], pooled[1], "each row took its own fresh id");
        // Ordering is asserted on the titles, since the ids are minted: the
        // second detach is the head.
        assert_eq!(
            pooled_titles(cx),
            vec!["Main".to_string(), "Work".to_string()],
            "most recently detached first"
        );
        cx.update(|app| {
            assert!(
                state.read(app).user_initiated_close(),
                "the emptied window is marked so its disk slot is dropped"
            );
        });
    }

    /// ⌃⌘N refuses the Claude pane, and refuses it BEFORE it opens anything —
    /// the break-pane scope guard (§P9), silent exactly as ⌃⌘b's refusal is. A
    /// Claude session moves whole through Detach ▸ Open in New Window instead.
    #[gpui::test]
    fn tear_off_action_refuses_a_claude_pane(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        state.update(cx, |s, _cx| {
            s.workspace.mutate_session("t1", |session| {
                let window = session.windows.iter_mut().find(|w| w.id == "t1-w").unwrap();
                window.kind = TermWindowKind::Claude;
                window.layout = nice_model::PaneLayout::single(nice_model::Pane::new(
                    "t1-w",
                    TermWindowKind::Claude,
                ));
            });
        });

        cx.update(super::tear_off_focused_pane);

        state.update(cx, |s, _cx| {
            let session = s
                .workspace
                .session_for("t1")
                .expect("the refused session is untouched");
            assert_eq!(session.windows.len(), 1, "its pill never left");
            assert_eq!(session.windows[0].layout.leaf_count(), 1);
        });
    }

    /// The tear-off open-failure recovery on a source window the tear-off had
    /// EMPTIED: the pane comes back, and the `user_initiated_close` latch the
    /// emptying set comes off with it. Left latched, a later non-user, non-quit
    /// close of this (re-populated, still-open) window would drop a disk slot that
    /// holds a live session.
    #[gpui::test]
    fn tear_off_recovery_unlatches_the_emptied_source_window(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        // Leave exactly one session, so tearing off its only pane empties the window.
        cx.update(|app| detach_session_from_window(app, &state, "t1"));

        let (window_id, pane_id) = state.read_with(cx, |s, _cx| {
            let window = &s.workspace.session_for("terminals-main").unwrap().windows[0];
            (window.id.clone(), window.effective_pane_id())
        });
        let (entry, terminus) = state
            .update(cx, |ws, wcx| {
                ws.tear_off_pane("terminals-main", &window_id, &pane_id, wcx)
            })
            .expect("the only pane tears off");
        assert_eq!(
            terminus,
            DissolveTerminus::WindowEmptied,
            "its last session dissolved with it"
        );
        let torn_off = entry.session.id.clone();
        cx.update(|app| {
            assert!(
                state.read(app).user_initiated_close(),
                "the emptying latched the drop-my-disk-slot flag"
            );
            // The new window could not open: the entry comes home.
            super::return_torn_off_entry(app, &state, entry, terminus);
        });

        state.update(cx, |s, _cx| {
            assert!(
                s.workspace.session_for(&torn_off).is_some(),
                "the pane is back in the window it came from"
            );
            assert!(
                !s.user_initiated_close(),
                "the window is re-populated and staying open — the latch is off"
            );
        });
    }

    /// ⌃⌘A adopts the pool HEAD into the active window — the round trip through
    /// both chords, which is the shape the hand gate exercises.
    #[gpui::test]
    fn adopt_action_pulls_the_pool_head_into_the_active_window(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        cx.update(detach_active_session);
        let pooled = pooled_ids(cx);
        assert_eq!(pooled.len(), 1, "one row on the pool: {pooled:?}");
        let detached = pooled[0].clone();

        cx.update(super::adopt_detached_session);
        assert!(pooled_ids(cx).is_empty(), "the head left the pool");
        state.update(cx, |s, _cx| {
            assert!(
                s.workspace.session_for(&detached).is_some(),
                "it came back to the window it left"
            );
            assert_eq!(
                s.workspace.active_session_id(),
                Some(detached.as_str()),
                "and it is selected"
            );
        });
    }

    /// The chord is inert on an empty pool and with no pool installed — it is
    /// still registered either way, so it is consumed rather than leaking to the
    /// pty.
    #[gpui::test]
    fn adopt_action_is_inert_without_anything_to_adopt(cx: &mut TestAppContext) {
        let state = window_with_two_sessions(cx);
        let before = state.read_with(cx, |s, _| s.workspace.navigable_sidebar_session_ids());

        cx.update(super::adopt_detached_session);
        cx.update(|app| app.remove_global::<DetachedPoolGlobal>());
        cx.update(super::adopt_detached_session);

        state.update(cx, |s, _cx| {
            assert_eq!(
                s.workspace.navigable_sidebar_session_ids(),
                before,
                "nothing to adopt ⇒ nothing changes"
            );
        });
    }
}
