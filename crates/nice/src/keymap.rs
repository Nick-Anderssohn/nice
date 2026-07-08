//! Keymap wiring — turns the gpui-free `nice_model::shortcuts` table into gpui
//! [`actions!`](gpui::actions) + key bindings, and routes each action to its
//! handler. The Rust replacement for Swift's process-wide
//! `KeyboardShortcutMonitor` (`Sources/Nice/Process/KeyboardShortcutMonitor.swift`)
//! — but built on GPUI's action/keymap dispatch, not an `NSEvent` monitor (the
//! plan's DO-NOT-PORT list forbids re-creating the monitor).
//!
//! ## What lives here
//!
//! * The 13 rebindable actions as gpui action structs (generated to mirror
//!   [`ShortcutAction`], one struct per case).
//! * [`install_shortcuts`] — the one-shot app wiring: it hoists the terminal
//!   [`FontSettings`] to a single process-level entity, registers every action's
//!   handler, and binds all 13 default combos (plus the non-rebindable ⌃⌘F full
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
//! toggles, the tab-cycle, pane stepping, the new-pane path) route through
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
    Action, App, AppContext, Context, Entity, Global, KeyBinding, Modifiers, ModifiersChangedEvent,
    PlatformKeyboardMapper,
};

use nice_model::shortcuts::{default_bindings, default_combo, ShortcutAction};
use nice_model::SidebarMode;
use nice_term_view::FontSettings;

use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// The 13 rebindable actions, one gpui action struct per `ShortcutAction` case
// (same names, `nice` namespace). `actions!` needs compile-time identifiers, so
// the set is spelled out here; [`shortcut_binding`] maps each `ShortcutAction`
// value back to its struct so the *bindings* are still generated from the table.
// The completeness test below pins that this list and `ShortcutAction::ALL` stay
// in lockstep.
gpui::actions!(
    nice,
    [
        NextSidebarTab,
        PrevSidebarTab,
        NextPane,
        PrevPane,
        NewTerminalPane,
        ToggleSidebar,
        ToggleSidebarMode,
        ToggleHiddenFiles,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSizes,
        UndoFileOperation,
        RedoFileOperation,
    ]
);

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
/// all 13 `cx.on_action` handlers, so every dispatch would fire twice (a ⌘B toggle
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
///    sidebar/pane actions through [`WindowRegistry::active_state`]);
/// 3. bind all 13 default combos from the table, plus ⌃⌘F full screen, with
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
    let (seed_px, seed_family, seed_sidebar_px) =
        match cx.try_global::<crate::settings::prefs_store::SettingsPrefsStore>() {
            Some(store) => (
                store.terminal_font_px(),
                store.terminal_font_family(),
                store.sidebar_font_px(),
            ),
            None => (None, None, None),
        };
    let font = cx.new(|cx| {
        let mut f = FontSettings::resolved_default(cx);
        if let Some(px) = seed_px {
            f.set_px(px, cx);
        }
        if let Some(fam) = &seed_family {
            f.set_family(Some(fam.clone().into()), cx);
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
    // Fold R9's ⌃⌘F (non-rebindable — not in the table) into the same wiring.
    // Its handler + the View-menu title live in `crate::app`.
    bindings.push(load_binding(
        "ctrl-cmd-f",
        crate::app::ToggleFullScreen,
        cx.keyboard_mapper().as_ref(),
    ));
    cx.bind_keys(bindings);
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
    cx.on_action(|_: &NextSidebarTab, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            s.model.select_next_sidebar_tab();
            // Re-sync the selection to the new active tab (Swift's active-tab
            // observer) — else the prior-active row lingers as a dim highlight.
            s.sync_selection_to_active_tab();
            trigger_peek_if_collapsed(s);
        });
    });
    cx.on_action(|_: &PrevSidebarTab, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            s.model.select_prev_sidebar_tab();
            s.sync_selection_to_active_tab();
            trigger_peek_if_collapsed(s);
        });
    });
    cx.on_action(|_: &NextPane, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.pane_strip_actions.select_next_pane(&mut s.model));
    });
    cx.on_action(|_: &PrevPane, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.pane_strip_actions.select_prev_pane(&mut s.model));
    });
    cx.on_action(|_: &NewTerminalPane, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            if let Some(active) = s.model.active_tab_id().map(str::to_owned) {
                s.pane_strip_actions.add_terminal_pane(&mut s.model, &active);
            }
        });
    });
    cx.on_action(|_: &ToggleSidebar, cx: &mut App| {
        with_active_state(cx, |s, _cx| s.sidebar.toggle_sidebar());
    });
    cx.on_action(|_: &ToggleSidebarMode, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            s.sidebar.toggle_sidebar_mode();
            // R19: persist the new per-window sidebar mode so it restores (the
            // debounced upsert; a no-op when no store Global is installed).
            s.save_to_store();
        });
    });
    cx.on_action(|_: &ToggleHiddenFiles, cx: &mut App| {
        with_active_state(cx, |s, _cx| {
            // R19: the Swift double gate (`appState.toggleFileBrowserHiddenFiles()`)
            // — flip dotfile visibility for the active tab ONLY when the sidebar is
            // in files mode AND a browser state already exists for that tab
            // (`toggle_hidden_files_if_exists` enforces the second gate, allocating
            // nothing when the user never opened the browser). Registered (not
            // silently missing) so ⌘⇧. is consumed rather than leaking to the pty.
            if s.sidebar.mode() != SidebarMode::Files {
                return;
            }
            if let Some(tab_id) = s.model.active_tab_id().map(str::to_owned) {
                s.file_browser.toggle_hidden_files_if_exists(&tab_id);
            }
        });
    });
}

/// After a sidebar-tab cycle on a collapsed sidebar, float the peek overlay so
/// the user can see which tab they're cycling toward (dossier G6). Cleared on
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
/// `AppShellView` / `SidebarShellView` / `WindowToolbarView` / `PaneHostView`
/// each observe this `WindowState` entity, so `gpui::Entity::update` — which does
/// not notify on its own — is followed by an explicit notify. Without it a ⌘T /
/// ⌘S / pane-step would mutate state that nothing re-renders (the gap this cycle
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

/// The 13 default bindings, generated from [`default_bindings`]: each combo's
/// [`chord_str`](nice_model::KeyCombo::chord_str) is bound to its action with
/// `use_key_equivalents` semantics.
fn table_bindings(cx: &App) -> Vec<KeyBinding> {
    let mapper = cx.keyboard_mapper().clone();
    default_bindings()
        .into_iter()
        .map(|(action, combo)| shortcut_binding(action, &combo.chord_str(), mapper.as_ref()))
        .collect()
}

/// Map a [`ShortcutAction`] value to a [`KeyBinding`] for its gpui action struct.
/// The one place the data table meets the compile-time action types; the
/// exhaustive match makes a newly-added `ShortcutAction` a compile error until it
/// is bound here.
fn shortcut_binding(
    action: ShortcutAction,
    chord: &str,
    mapper: &dyn PlatformKeyboardMapper,
) -> KeyBinding {
    match action {
        ShortcutAction::NextSidebarTab => load_binding(chord, NextSidebarTab, mapper),
        ShortcutAction::PrevSidebarTab => load_binding(chord, PrevSidebarTab, mapper),
        ShortcutAction::NextPane => load_binding(chord, NextPane, mapper),
        ShortcutAction::PrevPane => load_binding(chord, PrevPane, mapper),
        ShortcutAction::NewTerminalPane => load_binding(chord, NewTerminalPane, mapper),
        ShortcutAction::ToggleSidebar => load_binding(chord, ToggleSidebar, mapper),
        ShortcutAction::ToggleSidebarMode => load_binding(chord, ToggleSidebarMode, mapper),
        ShortcutAction::ToggleHiddenFiles => load_binding(chord, ToggleHiddenFiles, mapper),
        ShortcutAction::IncreaseFontSize => load_binding(chord, IncreaseFontSize, mapper),
        ShortcutAction::DecreaseFontSize => load_binding(chord, DecreaseFontSize, mapper),
        ShortcutAction::ResetFontSizes => load_binding(chord, ResetFontSizes, mapper),
        ShortcutAction::UndoFileOperation => load_binding(chord, UndoFileOperation, mapper),
        ShortcutAction::RedoFileOperation => load_binding(chord, RedoFileOperation, mapper),
    }
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
/// window's root view): if the focused window is peeking, end the peek once none
/// of the sidebar-tab shortcuts' modifiers are held anymore — Swift's
/// `endPeekIfModifiersReleased`. Routes to the focused window's state through
/// [`WindowRegistry::active_state`], matching the trigger side.
pub(crate) fn on_window_modifiers_changed(event: &ModifiersChangedEvent, cx: &mut App) {
    let Some(state) = WindowRegistry::active_state(cx, true) else {
        return;
    };
    if !state.read(cx).sidebar.peeking() {
        return;
    }
    // The sidebar/peek overlay is not rendered in the managed window this cycle,
    // so there is no pointer-hover pin to consult yet; the render-integration
    // cycle that mounts the overlay passes the real pin here (plan: "unless the
    // mouse is pinning the overlay" — R10's view-layer hover state).
    let mouse_pinned = false;
    if should_end_peek(event.modifiers, peek_relevant_modifiers(), mouse_pinned) {
        state.update(cx, |s, _cx| s.sidebar.end_sidebar_peek());
    }
}

/// The union of the two sidebar-tab-cycle shortcuts' modifier sets (⌘⌥ by
/// default) — the modifiers whose full release ends a peek. Read from the
/// defaults table (Swift reads the live bindings; R24's rebinding will make this
/// read the user's combos).
fn peek_relevant_modifiers() -> Modifiers {
    let mut relevant = Modifiers::default();
    for action in [ShortcutAction::NextSidebarTab, ShortcutAction::PrevSidebarTab] {
        if let Some(combo) = default_combo(action) {
            let m = combo.modifiers;
            relevant.control |= m.control;
            relevant.alt |= m.alt;
            relevant.shift |= m.shift;
            relevant.platform |= m.command;
        }
    }
    relevant
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

    #[test]
    fn peek_relevant_modifiers_are_command_alt() {
        // The default sidebar-tab combos are both ⌘⌥, so the peek is held open by
        // ⌘ or ⌥ (and nothing else).
        let r = peek_relevant_modifiers();
        assert!(r.platform, "⌘ holds the peek");
        assert!(r.alt, "⌥ holds the peek");
        assert!(!r.control && !r.shift, "no other modifier holds the peek");
    }

    #[test]
    fn peek_stays_while_any_relevant_modifier_is_held() {
        let relevant = peek_relevant_modifiers(); // ⌘⌥
        // Both held → stays.
        assert!(!should_end_peek(mods(false, true, false, true), relevant, false));
        // Only ⌘ still held (⌥ released) → stays (Swift keeps it until BOTH lift).
        assert!(!should_end_peek(mods(false, false, false, true), relevant, false));
        // Only ⌥ still held → stays.
        assert!(!should_end_peek(mods(false, true, false, false), relevant, false));
    }

    #[test]
    fn peek_ends_when_all_relevant_modifiers_release() {
        let relevant = peek_relevant_modifiers();
        // Nothing held → end.
        assert!(should_end_peek(mods(false, false, false, false), relevant, false));
        // An unrelated modifier (⇧ / ⌃) held but neither ⌘ nor ⌥ → still end.
        assert!(should_end_peek(mods(true, false, true, false), relevant, false));
    }

    #[test]
    fn mouse_pin_keeps_the_peek_even_with_no_modifiers() {
        // The pointer pinning the overlay wins over modifier release (dossier
        // G6 / R10's hover pin) — never ends while pinned.
        let relevant = peek_relevant_modifiers();
        assert!(!should_end_peek(mods(false, false, false, false), relevant, true));
    }
}
