//! In-process multi-window isolation / shortcut-routing / all-actions-fire /
//! peek set-clear differentials for R12 — **execution model: mocked
//! [`gpui::TestAppContext`], ordinary libtest `#[gpui::test]` cases** (no Metal,
//! no pixels; parallel-safe).
//!
//! The shipped `WindowState` / `WindowRegistry` / `keymap` live in the `nice`
//! binary, which a dev/test crate cannot import (and vice versa) — exactly the
//! constraint the R9 [`crate::chrome_band`] and R10 [`crate::sidebar_multiselect`]
//! probes document. So this mirrors that wiring in local probes that drive the
//! **real** `nice-model` types ([`WorkspaceModel`], [`SidebarModel`]) and gpui's **real**
//! action/keymap dispatch: a [`RegistryProbe`] gpui global (the `WindowRegistry`
//! mirror — MRU + `active_state`), a [`WindowStateProbe`] per window (the
//! `WindowState` mirror — one `WorkspaceModel` + `SidebarModel` each), 14 gpui actions
//! generated to mirror `nice_model::shortcuts`, and the app-level-vs-window-level
//! handler split routed through the registry — the whole R12 dispatch shape, but
//! over probes so a drift in the *ported* routing / isolation / peek semantics
//! fails here. The real end-to-end path (real `NSWindow`s, real CGEvents, the real
//! registry + keymap) is the live `multiwindow` scenario in the `nice` binary; this
//! is its fast, deterministic in-process half.
//!
//! ## What these cases verify (and what they deliberately do NOT)
//!
//! Per the plan's differential-pair rule, each assertion is a **classification /
//! state outcome**, never a frame-motion or timing claim — an in-process simulated
//! event cannot move a real `NSWindow`, and the one clock here is gpui's simulated
//! one. The four concerns:
//!
//! * **Isolation** — two windows own independent `WorkspaceModel`s, so a mutation to
//!   one's tree leaves the other **byte-identical** (a full session/window snapshot).
//! * **Routing** — a window-scoped action dispatched while window B is key mutates
//!   **B** (through the registry's `active_state`), never A; with B deregistered
//!   (its close), dispatch falls back to the most-recently-keyed surviving window.
//! * **All 14 fire** — every default combo dispatches to a live handler or, for the
//!   three deferred actions (hidden-files R19, undo/redo R20), to a **declared
//!   no-op marker** counter — consumed, not silently missing.
//! * **Peek set/clear** — a sidebar-session cycle on a *collapsed* sidebar sets the
//!   peek; an *expanded* one does not (the differential); the peek clears once the
//!   shortcut's modifiers all release, unless the pointer pins it.
//!
//! Neither this nor any behavior test asserts cadence / perf / wall-clock timing.

use std::collections::{HashMap, HashSet};

use gpui::{
    div, prelude::*, AnyWindowHandle, App, AppContext, Capslock, Context, Entity, FocusHandle,
    Focusable, Global, IntoElement, KeyBinding, Modifiers, ModifiersChangedEvent, PlatformInput,
    Render, TestAppContext, Window, WindowHandle, WindowId,
};

use nice_model::shortcuts::{default_bindings, default_combo, ShortcutAction};
use nice_model::{TermWindow, TermWindowKind, SidebarMode, SidebarModel, Session, WorkspaceModel};

// ---------------------------------------------------------------------------
// The 14 gpui actions — one struct per `ShortcutAction` case, in a local
// `r12_itests` namespace so they can't collide with the app's own actions. The
// `all_actions_map_to_a_binding` test pins that this set and `ShortcutAction::ALL`
// stay in lockstep, mirroring the app keymap's completeness test.
// ---------------------------------------------------------------------------
gpui::actions!(
    r12_itests,
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
    ]
);

// ---------------------------------------------------------------------------
// WindowStateProbe — the per-window composition root mirror. Holds this window's
// own `WorkspaceModel` + `SidebarModel` (isolation IS each window owning its own tree)
// and the model-only ops the app keymap's window-scoped handlers drive. Ported
// thin: the real model does the reasoning.
// ---------------------------------------------------------------------------

/// One window's state. The keymap handlers below mutate exactly these — routed to
/// the *key* window via [`RegistryProbe::active_state`], exactly like the app's
/// `with_active_state`.
struct WindowStateProbe {
    /// This window's projects/sessions/windows tree (the isolation unit).
    workspace: WorkspaceModel,
    /// This window's sidebar collapse / mode / peek state.
    sidebar: SidebarModel,
    /// Monotonic window-id source (mirrors `ModelWindowStripActions`'s minted ids).
    next_window_seq: u64,
    /// The `ToggleHiddenFiles` declared-no-op marker: incremented when ⌘⇧. reaches
    /// the (window-scoped, R19-deferred) handler, so "the action fired" is
    /// observable rather than silence.
    hidden_files_noop_fires: u32,
    /// The `CommandCompose` declared marker: incremented when ⌘↩ reaches the
    /// window-scoped handler. The probe has no live pty sessions, so the marker
    /// (not trigger bytes) is the honest mirror of "the handler was reached";
    /// the gate + byte routing are the app's `compose_route` unit tests and the
    /// live scenario.
    compose_fires: u32,
}

impl WindowStateProbe {
    fn new(workspace: WorkspaceModel, collapsed: bool) -> Self {
        Self {
            workspace,
            sidebar: SidebarModel::new(collapsed, SidebarMode::Sessions),
            next_window_seq: 0,
            hidden_files_noop_fires: 0,
            compose_fires: 0,
        }
    }

    // -- window-scoped handler bodies (mirror `keymap::register_window_scoped_actions`)

    /// ⌘⌥↓ — cycle to the next sidebar session, then float the peek if collapsed.
    fn next_sidebar_session(&mut self) {
        self.workspace.select_next_sidebar_session();
        self.trigger_peek_if_collapsed();
    }

    /// ⌘⌥↑ — cycle to the previous sidebar session, then float the peek if collapsed.
    fn prev_sidebar_session(&mut self) {
        self.workspace.select_prev_sidebar_session();
        self.trigger_peek_if_collapsed();
    }

    /// ⌘⌥→ — step the active session's active window forward (wrapping). Mirror of
    /// `ModelWindowStripActions::select_next_window` (`step_active_window(+1)`).
    fn next_window(&mut self) {
        step_active_window(&mut self.workspace, 1);
    }

    /// ⌘⌥← — step the active session's active window backward (wrapping).
    fn prev_window(&mut self) {
        step_active_window(&mut self.workspace, -1);
    }

    /// ⌘T — append an auto-named terminal window to the active session and focus it.
    /// Mirror of `ModelWindowStripActions::add_terminal_window` over `WorkspaceModel::add_window`.
    fn new_terminal_window(&mut self) {
        let Some(active) = self.workspace.active_session_id().map(str::to_owned) else {
            return;
        };
        self.next_window_seq += 1;
        let term_window_id = format!("probe-pane-{}", self.next_window_seq);
        self.workspace.add_window(&active, term_window_id, None);
    }

    /// ⌘B — flip the sidebar collapsed flag.
    fn toggle_sidebar(&mut self) {
        self.sidebar.toggle_sidebar();
    }

    /// ⌘⇧B — flip the sidebar between sessions and files mode.
    fn toggle_sidebar_mode(&mut self) {
        self.sidebar.toggle_sidebar_mode();
    }

    /// ⌘⇧. — the R19-deferred hidden-files toggle. Registered (routed through
    /// `active_state`), no-op body; records that it reached the active window.
    fn toggle_hidden_files_noop(&mut self) {
        self.hidden_files_noop_fires += 1;
    }

    /// After a sidebar-session cycle on a collapsed sidebar, float the peek overlay —
    /// mirror of the app keymap's `trigger_peek_if_collapsed`.
    fn trigger_peek_if_collapsed(&mut self) {
        if self.sidebar.collapsed() {
            self.sidebar.begin_sidebar_peek();
        }
    }
}

/// Wrapping step of the active session's `active_window_id` by `offset`, a <2-windows /
/// no-active no-op — a verbatim mirror of `ModelWindowStripActions::step_active_window`
/// (`rem_euclid`), so the ported window-step semantics are pinned here too.
fn step_active_window(model: &mut WorkspaceModel, offset: isize) {
    let Some(session_id) = model.active_session_id().map(str::to_owned) else {
        return;
    };
    let Some((pi, ti)) = model.project_session_index(&session_id) else {
        return;
    };
    let session = &model.projects[pi].sessions[ti];
    let count = session.windows.len();
    if count < 2 {
        return;
    }
    let Some(active) = session.active_window_id.clone() else {
        return;
    };
    let Some(cur) = session.windows.iter().position(|p| p.id == active) else {
        return;
    };
    let next = (cur as isize + offset).rem_euclid(count as isize) as usize;
    let next_id = session.windows[next].id.clone();
    model.projects[pi].sessions[ti].active_window_id = Some(next_id);
}

// ---------------------------------------------------------------------------
// RegistryProbe — the `WindowRegistry` mirror. App-global map of live windows to
// their state, plus the MRU order, and the `active_state(prefer_key)` contract the
// window-scoped handlers route through.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RegistryProbe {
    entries: HashMap<WindowId, Entity<WindowStateProbe>>,
    order: Vec<WindowId>,
}

impl Global for RegistryProbe {}

impl RegistryProbe {
    /// Register `state` as window `id`'s state and append it to the back of the MRU
    /// (so, before anything is keyed, the first-registered window is still the
    /// active-window fallback) — mirror of `WindowRegistry::register`.
    fn register(cx: &mut App, id: WindowId, state: Entity<WindowStateProbe>) {
        let reg = cx.default_global::<RegistryProbe>();
        reg.entries.insert(id, state);
        if !reg.order.iter().any(|x| *x == id) {
            reg.order.push(id);
        }
    }

    /// Note that window `id` became key — move it to the MRU front. Driven by the
    /// window's `observe_window_activation` (mirror of `WindowRegistry::note_active`).
    fn note_active(cx: &mut App, id: WindowId) {
        let reg = cx.default_global::<RegistryProbe>();
        if reg.entries.contains_key(&id) {
            reg.order.retain(|x| *x != id);
            reg.order.insert(0, id);
        }
    }

    /// Deregister window `id` (its close). Mirror of `WindowRegistry::deregister`.
    fn deregister(cx: &mut App, id: WindowId) {
        let reg = cx.default_global::<RegistryProbe>();
        reg.order.retain(|x| *x != id);
        reg.entries.remove(&id);
    }

    /// The per-window state a window-scoped shortcut routes to: the key window when
    /// `prefer_key` and it is still registered, else the most-recently-keyed live
    /// window, else the first registered — mirror of `WindowRegistry::active_state`
    /// (reading gpui's real `active_window`).
    fn active_state(cx: &App, prefer_key: bool) -> Option<Entity<WindowStateProbe>> {
        let key = cx.active_window().map(|w| w.window_id());
        let reg = cx.try_global::<RegistryProbe>()?;
        let live: HashSet<WindowId> = reg.entries.keys().copied().collect();
        let chosen = select(prefer_key, key, &reg.order, &live)?;
        reg.entries.get(&chosen).cloned()
    }

    /// Live registered window count (mirror of `WindowRegistry::count`).
    fn count(cx: &App) -> usize {
        cx.try_global::<RegistryProbe>()
            .map_or(0, |r| r.entries.len())
    }
}

/// The active-window selection rule — a verbatim mirror of the app registry's
/// `mru::select`: the key window when live, else the front-most live MRU entry,
/// else `None` (a stale key or dropped entry can never win).
fn select(
    prefer_key: bool,
    key: Option<WindowId>,
    order: &[WindowId],
    live: &HashSet<WindowId>,
) -> Option<WindowId> {
    if prefer_key {
        if let Some(k) = key {
            if live.contains(&k) {
                return Some(k);
            }
        }
    }
    order.iter().copied().find(|id| live.contains(id))
}

// ---------------------------------------------------------------------------
// AppLevelProbe — records the app-level actions (font zoom + the deferred
// undo/redo), which fire regardless of which window is key (the plan's
// dispatch-order split). The app keymap drives the process-level `FontSettings`
// entity + the future shared history; the probe records the equivalent fires so
// "the action fired" is observable without a live font/history dependency.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AppLevelProbe {
    /// Net font zoom delta (⌘= +1 / ⌘− −1); ⌘0 resets it to 0.
    font_delta: i32,
    /// ⌘0 reset count.
    font_resets: u32,
    /// ⌘Z declared-no-op marker (R20).
    undo_fires: u32,
    /// ⌘⇧Z declared-no-op marker (R20).
    redo_fires: u32,
}

impl Global for AppLevelProbe {}

// ---------------------------------------------------------------------------
// install — wire the whole probe keymap once per test: the two globals, the 14
// action handlers (app-level font/undo/redo + window-scoped through the registry),
// and the bindings generated from the REAL `nice_model::shortcuts` table.
// ---------------------------------------------------------------------------

fn install(cx: &mut TestAppContext) {
    cx.update(|app| {
        app.set_global(RegistryProbe::default());
        app.set_global(AppLevelProbe::default());
        register_app_level_actions(app);
        register_window_scoped_actions(app);
        app.bind_keys(table_bindings());
    });
}

/// App-level handlers: fire even with no window key (mirror of
/// `keymap::register_app_level_actions`).
fn register_app_level_actions(cx: &mut App) {
    cx.on_action(|_: &IncreaseFontSize, cx: &mut App| {
        cx.default_global::<AppLevelProbe>().font_delta += 1;
    });
    cx.on_action(|_: &DecreaseFontSize, cx: &mut App| {
        cx.default_global::<AppLevelProbe>().font_delta -= 1;
    });
    cx.on_action(|_: &ResetFontSizes, cx: &mut App| {
        let p = cx.default_global::<AppLevelProbe>();
        p.font_delta = 0;
        p.font_resets += 1;
    });
    cx.on_action(|_: &UndoFileOperation, cx: &mut App| {
        cx.default_global::<AppLevelProbe>().undo_fires += 1;
    });
    cx.on_action(|_: &RedoFileOperation, cx: &mut App| {
        cx.default_global::<AppLevelProbe>().redo_fires += 1;
    });
}

/// Window-scoped handlers: mutate the key window's state through the registry
/// (mirror of `keymap::register_window_scoped_actions`).
fn register_window_scoped_actions(cx: &mut App) {
    cx.on_action(|_: &NextSidebarSession, cx: &mut App| with_active(cx, |s| s.next_sidebar_session()));
    cx.on_action(|_: &PrevSidebarSession, cx: &mut App| with_active(cx, |s| s.prev_sidebar_session()));
    cx.on_action(|_: &NextWindow, cx: &mut App| with_active(cx, |s| s.next_window()));
    cx.on_action(|_: &PrevWindow, cx: &mut App| with_active(cx, |s| s.prev_window()));
    cx.on_action(|_: &NewTerminalWindow, cx: &mut App| with_active(cx, |s| s.new_terminal_window()));
    cx.on_action(|_: &ToggleSidebar, cx: &mut App| with_active(cx, |s| s.toggle_sidebar()));
    cx.on_action(|_: &ToggleSidebarMode, cx: &mut App| with_active(cx, |s| s.toggle_sidebar_mode()));
    cx.on_action(|_: &ToggleHiddenFiles, cx: &mut App| {
        with_active(cx, |s| s.toggle_hidden_files_noop())
    });
    cx.on_action(|_: &CommandCompose, cx: &mut App| {
        with_active(cx, |s| s.compose_fires += 1)
    });
}

/// Route a window-scoped action to the key window's state (mirror of
/// `keymap::with_active_state`): a no-op when no window is registered.
fn with_active(cx: &mut App, f: impl FnOnce(&mut WindowStateProbe)) {
    if let Some(state) = RegistryProbe::active_state(cx, true) {
        state.update(cx, |s, _cx| f(s));
    }
}

/// Build the 14 default bindings from the real `nice_model::shortcuts` table — the
/// mirror of `keymap::table_bindings`. Uses `KeyBinding::new` (no context = active
/// everywhere, like the app's process-wide monitor); the app additionally applies
/// `use_key_equivalents` layout semantics, the documented divergence the live
/// scenario exercises. That every table chord parses into a gpui binding here is
/// the completeness proof the app's own generation relies on.
fn table_bindings() -> Vec<KeyBinding> {
    default_bindings()
        .into_iter()
        .map(|(action, combo)| shortcut_binding(action, &combo.chord_str()))
        .collect()
}

/// Map a [`ShortcutAction`] to a [`KeyBinding`] for its probe action struct — the
/// exhaustive match makes a newly-added `ShortcutAction` a compile error until it
/// is bound (mirror of `keymap::shortcut_binding`).
fn shortcut_binding(action: ShortcutAction, chord: &str) -> KeyBinding {
    match action {
        ShortcutAction::NextSidebarSession => KeyBinding::new(chord, NextSidebarSession, None),
        ShortcutAction::PrevSidebarSession => KeyBinding::new(chord, PrevSidebarSession, None),
        ShortcutAction::NextWindow => KeyBinding::new(chord, NextWindow, None),
        ShortcutAction::PrevWindow => KeyBinding::new(chord, PrevWindow, None),
        ShortcutAction::NewTerminalWindow => KeyBinding::new(chord, NewTerminalWindow, None),
        ShortcutAction::ToggleSidebar => KeyBinding::new(chord, ToggleSidebar, None),
        ShortcutAction::ToggleSidebarMode => KeyBinding::new(chord, ToggleSidebarMode, None),
        ShortcutAction::ToggleHiddenFiles => KeyBinding::new(chord, ToggleHiddenFiles, None),
        ShortcutAction::IncreaseFontSize => KeyBinding::new(chord, IncreaseFontSize, None),
        ShortcutAction::DecreaseFontSize => KeyBinding::new(chord, DecreaseFontSize, None),
        ShortcutAction::ResetFontSizes => KeyBinding::new(chord, ResetFontSizes, None),
        ShortcutAction::UndoFileOperation => KeyBinding::new(chord, UndoFileOperation, None),
        ShortcutAction::RedoFileOperation => KeyBinding::new(chord, RedoFileOperation, None),
        ShortcutAction::CommandCompose => KeyBinding::new(chord, CommandCompose, None),
    }
}

// ---------------------------------------------------------------------------
// Peek clear (mirror of `keymap::on_window_modifiers_changed` + its pure helpers).
// ---------------------------------------------------------------------------

/// Window-level modifier-release handler: end the key window's peek once none of
/// the sidebar-session shortcuts' modifiers remain (mirror of
/// `keymap::on_window_modifiers_changed`).
fn on_window_modifiers_changed_probe(event: &ModifiersChangedEvent, cx: &mut App) {
    let Some(state) = RegistryProbe::active_state(cx, true) else {
        return;
    };
    if !state.read(cx).sidebar.peeking() {
        return;
    }
    // No overlay is mounted in this probe, so there is no pointer-hover pin yet.
    let mouse_pinned = false;
    if should_end_peek(event.modifiers, peek_relevant_modifiers(), mouse_pinned) {
        state.update(cx, |s, _cx| s.sidebar.end_sidebar_peek());
    }
}

/// The union of the sidebar-session-cycle combos' modifiers (⌘⌥ by default), read from
/// the real table — mirror of `keymap::peek_relevant_modifiers`.
fn peek_relevant_modifiers() -> Modifiers {
    let mut relevant = Modifiers::default();
    for action in [ShortcutAction::NextSidebarSession, ShortcutAction::PrevSidebarSession] {
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

/// End the peek when none of the `relevant` modifiers remain in `current`, unless
/// the pointer pins the overlay — mirror of `keymap::should_end_peek`.
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

// ---------------------------------------------------------------------------
// WindowRootProbe — a focusable root that (a) is where key events originate for
// simulated combos, and (b) carries the window-level modifier-release observer,
// mirroring the shipped `WindowChromeView`.
// ---------------------------------------------------------------------------

struct WindowRootProbe {
    focus_handle: FocusHandle,
}

impl Focusable for WindowRootProbe {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WindowRootProbe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            // Mirror of `WindowChromeView`'s peek-clear observer: end the key
            // window's peek once the shortcut's modifiers all release.
            .on_modifiers_changed(|event, _window, cx| on_window_modifiers_changed_probe(event, cx))
    }
}

// ---------------------------------------------------------------------------
// Harness helpers
// ---------------------------------------------------------------------------

/// A model of four navigable terminal sessions (Main + `t1`..`t3`), Main active — so a
/// sidebar-session cycle genuinely moves the active session.
fn seed_multi_session() -> WorkspaceModel {
    let mut m = WorkspaceModel::new("/home/u");
    for i in 1..=3 {
        let id = format!("t{i}");
        let term_window_id = format!("{id}-p");
        let mut session = Session::new(id.clone(), format!("Tab {i}"), "/home/u");
        session.windows = vec![TermWindow::new(term_window_id.clone(), "Terminal 1", TermWindowKind::Terminal)];
        session.active_window_id = Some(term_window_id);
        m.projects[0].sessions.push(session);
    }
    m.select_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID);
    m
}

/// A model whose active session (`multi`) has three windows (`a` active) — so a
/// window-step genuinely moves the active window.
fn seed_multi_window() -> WorkspaceModel {
    let mut m = seed_multi_session();
    let mut session = Session::new("multi", "Multi", "/home/u");
    session.windows = vec![
        TermWindow::new("a", "A", TermWindowKind::Terminal),
        TermWindow::new("b", "B", TermWindowKind::Terminal),
        TermWindow::new("c", "C", TermWindowKind::Terminal),
    ];
    session.active_window_id = Some("a".into());
    m.projects[0].sessions.push(session);
    m.select_session("multi");
    m
}

/// Mint a window-state entity over `model`, seeding the sidebar collapsed or not.
fn new_state(
    cx: &mut TestAppContext,
    workspace: WorkspaceModel,
    collapsed: bool,
) -> Entity<WindowStateProbe> {
    cx.new(|_cx| WindowStateProbe::new(workspace, collapsed))
}

/// Open a mocked window rooting a focusable probe over `state`, register it in the
/// [`RegistryProbe`], focus the root (so simulated combos originate there), and
/// wire its activation observer to the registry MRU (gated on `is_window_active`,
/// exactly like the shipped `build_window_root`). Returns the window handle.
fn open_probe_window(
    cx: &mut TestAppContext,
    state: Entity<WindowStateProbe>,
) -> WindowHandle<WindowRootProbe> {
    let handle = cx.add_window(move |window, cx| {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let wid = window.window_handle().window_id();
        RegistryProbe::register(cx, wid, state);
        cx.observe_window_activation(window, move |_this, window, cx| {
            if window.is_window_active() {
                RegistryProbe::note_active(cx, wid);
            }
        })
        .detach();
        WindowRootProbe { focus_handle }
    });
    cx.run_until_parked();
    handle
}

/// Drive `window` frontmost + key (so `active_window()` returns it and its
/// activation observer fires), then run to quiescence.
fn activate(cx: &mut TestAppContext, window: WindowHandle<WindowRootProbe>) {
    cx.update_window(window.into(), |_root, window, _app| window.activate_window())
        .unwrap();
    cx.run_until_parked();
}

/// A full session/window snapshot of a window's model — the "byte-identical" proxy
/// (`Session` is `Clone + Eq`, so this captures ids, titles, windows, active window, and
/// per-window status).
fn model_snapshot(s: &WindowStateProbe) -> (Option<String>, Vec<Session>) {
    let active = s.workspace.active_session_id().map(str::to_string);
    let sessions = s
        .workspace
        .navigable_sidebar_session_ids()
        .iter()
        .filter_map(|id| s.workspace.session_for(id).cloned())
        .collect();
    (active, sessions)
}

fn snapshot(cx: &mut TestAppContext, state: &Entity<WindowStateProbe>) -> (Option<String>, Vec<Session>) {
    cx.update(|app| model_snapshot(state.read(app)))
}

fn collapsed(cx: &mut TestAppContext, state: &Entity<WindowStateProbe>) -> bool {
    cx.update(|app| state.read(app).sidebar.collapsed())
}

fn active_session(cx: &mut TestAppContext, state: &Entity<WindowStateProbe>) -> Option<String> {
    cx.update(|app| state.read(app).workspace.active_session_id().map(str::to_string))
}

fn app_probe<R>(cx: &mut TestAppContext, f: impl FnOnce(&AppLevelProbe) -> R) -> R {
    cx.update(|app| f(app.global::<AppLevelProbe>()))
}

// ============================================================================
// isolation
// ============================================================================

/// Two windows own independent `WorkspaceModel`s: mutating A's tree through A's own seam
/// leaves B **byte-identical**, and A genuinely diverges. The state-level isolation
/// guarantee, exercised through two real gpui windows + entities.
#[gpui::test]
fn two_windows_own_isolated_models_mutating_a_leaves_b_byte_identical(cx: &mut TestAppContext) {
    install(cx);
    let a = new_state(cx, seed_multi_session(), false);
    let b = new_state(cx, seed_multi_session(), false);
    let _aw = open_probe_window(cx, a.clone());
    let _bw = open_probe_window(cx, b.clone());

    let before_b = snapshot(cx, &b);

    // Mutate A's tree through its own seam (add a terminal window to the active session).
    a.update(cx, |s, _cx| s.new_terminal_window());
    cx.run_until_parked();

    assert_eq!(
        before_b,
        snapshot(cx, &b),
        "B's session/window tree is byte-identical after A mutates its own"
    );
    assert_ne!(
        snapshot(cx, &a),
        before_b,
        "A's tree diverged — the mutation landed on A only"
    );
}

// ============================================================================
// routing
// ============================================================================

/// A window-scoped action dispatched while window B is key mutates **B**'s state
/// (through the registry's `active_state`), never A's — the focused-window routing
/// contract.
#[gpui::test]
fn window_scoped_action_routes_to_the_key_window_only(cx: &mut TestAppContext) {
    install(cx);
    let a = new_state(cx, seed_multi_session(), false);
    let b = new_state(cx, seed_multi_session(), false);
    let _aw = open_probe_window(cx, a.clone());
    let bw = open_probe_window(cx, b.clone());

    activate(cx, bw); // B becomes the key window
    cx.dispatch_action(bw.into(), ToggleSidebar);

    assert!(collapsed(cx, &b), "B (the key window) received the ⌘B toggle");
    assert!(!collapsed(cx, &a), "A did not — the action routed to B only");
}

/// With the most-recently-keyed window deregistered (its close), a window-scoped
/// action falls back to the surviving most-recently-keyed window — the registry's
/// stale-key / close fallback, at the gpui-dispatch level.
#[gpui::test]
fn dispatch_falls_back_to_surviving_window_after_the_key_window_closes(cx: &mut TestAppContext) {
    install(cx);
    let a = new_state(cx, seed_multi_session(), false);
    let b = new_state(cx, seed_multi_session(), false);
    let aw = open_probe_window(cx, a.clone());
    let bw = open_probe_window(cx, b.clone());

    activate(cx, aw); // A keyed
    activate(cx, bw); // then B keyed → MRU front is B, active_window is B

    // B closes: deregister it (the close hook's core). active_window may still name
    // B, but a stale key can't win — dispatch must fall back to A.
    let b_any: AnyWindowHandle = bw.into();
    let b_id = b_any.window_id();
    cx.update(|app| RegistryProbe::deregister(app, b_id));
    assert_eq!(cx.update(|app| RegistryProbe::count(app)), 1, "the registry dropped B");

    cx.dispatch_action(aw.into(), ToggleSidebar);
    assert!(
        collapsed(cx, &a),
        "with B gone, the window-scoped action fell back to the surviving window A"
    );
}

// ============================================================================
// all 14 actions fire
// ============================================================================

/// The action set and `ShortcutAction::ALL` stay in lockstep, and every default
/// combo parses into a gpui binding — the table-completeness + binding-generation
/// proof (mirror of the app keymap's own completeness test).
#[gpui::test]
fn all_default_combos_generate_a_binding(_cx: &mut TestAppContext) {
    let bindings = table_bindings();
    assert_eq!(bindings.len(), 14, "every default combo produced a binding");
    assert_eq!(ShortcutAction::ALL.len(), 14, "14 rebindable actions");
}

/// Every one of the 14 default combos, dispatched as a real keystroke through the
/// bound keymap, reaches its handler: the live handlers produce their model / app
/// effect, and the three deferred ones (hidden-files, undo, redo) hit their
/// **declared no-op marker** — consumed, never silent.
#[gpui::test]
fn every_default_combo_dispatches_to_its_handler(cx: &mut TestAppContext) {
    install(cx);
    let state = new_state(cx, seed_multi_window(), false);
    let win = open_probe_window(cx, state.clone());
    activate(cx, win);
    let w = win.into();

    let combo = |a| default_combo(a).unwrap().chord_str();

    // -- window-scoped live handlers ------------------------------------------
    let before = active_session(cx, &state);
    cx.simulate_keystrokes(w, &combo(ShortcutAction::NextSidebarSession));
    assert_ne!(active_session(cx, &state), before, "⌘⌥↓ cycled the active sidebar session");
    cx.simulate_keystrokes(w, &combo(ShortcutAction::PrevSidebarSession));
    assert_eq!(active_session(cx, &state), before, "⌘⌥↑ cycled back");

    // Active session is `multi` (a,b,c windows, a active) after the round trip returns to
    // the seeded active session — re-select it explicitly to pin window-step observation.
    state.update(cx, |s, _| s.workspace.select_session("multi"));
    let term_window = |cx: &mut TestAppContext| {
        cx.update(|app| {
            state
                .read(app)
                .workspace
                .session_for("multi")
                .and_then(|t| t.active_window_id.clone())
        })
    };
    cx.simulate_keystrokes(w, &combo(ShortcutAction::NextWindow));
    assert_eq!(term_window(cx).as_deref(), Some("b"), "⌘⌥→ stepped to the next window");
    cx.simulate_keystrokes(w, &combo(ShortcutAction::PrevWindow));
    assert_eq!(term_window(cx).as_deref(), Some("a"), "⌘⌥← stepped back");

    let window_count = |cx: &mut TestAppContext| {
        cx.update(|app| state.read(app).workspace.session_for("multi").map(|t| t.windows.len()))
    };
    let before_windows = window_count(cx);
    cx.simulate_keystrokes(w, &combo(ShortcutAction::NewTerminalWindow));
    assert_eq!(
        window_count(cx),
        before_windows.map(|n| n + 1),
        "⌘T appended a terminal window to the active session"
    );

    assert!(!collapsed(cx, &state));
    cx.simulate_keystrokes(w, &combo(ShortcutAction::ToggleSidebar));
    assert!(collapsed(cx, &state), "⌘B toggled the sidebar collapsed");

    let mode = |cx: &mut TestAppContext| cx.update(|app| state.read(app).sidebar.mode());
    assert_eq!(mode(cx), SidebarMode::Sessions);
    cx.simulate_keystrokes(w, &combo(ShortcutAction::ToggleSidebarMode));
    assert_eq!(mode(cx), SidebarMode::Files, "⌘⇧B toggled the sidebar mode");

    // -- the window-scoped DEFERRED no-op marker (R19) ------------------------
    let hf = |cx: &mut TestAppContext| cx.update(|app| state.read(app).hidden_files_noop_fires);
    assert_eq!(hf(cx), 0);
    cx.simulate_keystrokes(w, &combo(ShortcutAction::ToggleHiddenFiles));
    assert_eq!(hf(cx), 1, "⌘⇧. reached the declared no-op marker (R19), not silence");

    // -- app-level live handlers (font zoom) ----------------------------------
    cx.simulate_keystrokes(w, &combo(ShortcutAction::IncreaseFontSize));
    cx.simulate_keystrokes(w, &combo(ShortcutAction::IncreaseFontSize));
    assert_eq!(app_probe(cx, |p| p.font_delta), 2, "⌘= grew the (shared) font twice");
    cx.simulate_keystrokes(w, &combo(ShortcutAction::DecreaseFontSize));
    assert_eq!(app_probe(cx, |p| p.font_delta), 1, "⌘− shrank it once");
    cx.simulate_keystrokes(w, &combo(ShortcutAction::ResetFontSizes));
    assert_eq!(app_probe(cx, |p| p.font_delta), 0, "⌘0 reset the font delta");
    assert_eq!(app_probe(cx, |p| p.font_resets), 1, "⌘0 fired the reset");

    // -- the app-level DEFERRED no-op markers (R20) ---------------------------
    cx.simulate_keystrokes(w, &combo(ShortcutAction::UndoFileOperation));
    assert_eq!(app_probe(cx, |p| p.undo_fires), 1, "⌘Z reached the undo marker (R20)");
    cx.simulate_keystrokes(w, &combo(ShortcutAction::RedoFileOperation));
    assert_eq!(app_probe(cx, |p| p.redo_fires), 1, "⌘⇧Z reached the redo marker (R20)");

    // -- Command Compose: ⌘↩ parses, binds, and dispatches window-scoped ------
    let compose = |cx: &mut TestAppContext| cx.update(|app| state.read(app).compose_fires);
    assert_eq!(compose(cx), 0);
    cx.simulate_keystrokes(w, &combo(ShortcutAction::CommandCompose));
    assert_eq!(
        compose(cx),
        1,
        "⌘↩ reached the window-scoped compose marker (the probe has no pty; \
         gate + byte routing are the app's compose_route tests + the live scenario)"
    );
}

// ============================================================================
// peek set / clear
// ============================================================================

/// A sidebar-session cycle on a **collapsed** sidebar floats the peek; the same cycle
/// on an **expanded** sidebar does not (the differential); and once the shortcut's
/// modifiers all release, the window-level observer clears it — but a pinning
/// pointer keeps it. Driven through real gpui dispatch + a real modifiers event.
#[gpui::test]
fn peek_sets_on_collapsed_cycle_and_clears_on_modifier_release(cx: &mut TestAppContext) {
    install(cx);

    // Expanded sidebar: a cycle must NOT peek.
    let expanded = new_state(cx, seed_multi_session(), false);
    let ew = open_probe_window(cx, expanded.clone());
    activate(cx, ew);
    cx.dispatch_action(ew.into(), NextSidebarSession);
    assert!(
        !cx.update(|app| expanded.read(app).sidebar.peeking()),
        "an expanded-sidebar cycle does not peek"
    );

    // Collapsed sidebar: a cycle DOES peek.
    let collapsed_state = new_state(cx, seed_multi_session(), true);
    let cw = open_probe_window(cx, collapsed_state.clone());
    activate(cx, cw);
    cx.dispatch_action(cw.into(), NextSidebarSession);
    let peeking = |cx: &mut TestAppContext| cx.update(|app| collapsed_state.read(app).sidebar.peeking());
    assert!(peeking(cx), "a collapsed-sidebar cycle floats the peek");

    // A modifiers change that still holds ⌘ keeps the peek (⌘⌥ → ⌘).
    on_modifiers(cx, cw, Modifiers { platform: true, ..Default::default() });
    assert!(peeking(cx), "the peek stays while a relevant modifier (⌘) is still held");

    // All relevant modifiers released → the observer clears it.
    on_modifiers(cx, cw, Modifiers::default());
    assert!(!peeking(cx), "releasing all relevant modifiers clears the peek");
}

/// Drive a modifiers-changed event into `window` (the shipped app's flagsChanged
/// analog) through gpui's real dispatch, so the root's `on_modifiers_changed`
/// observer runs, and settle.
fn on_modifiers(cx: &mut TestAppContext, window: WindowHandle<WindowRootProbe>, modifiers: Modifiers) {
    let event = ModifiersChangedEvent {
        modifiers,
        capslock: Capslock { on: false },
    };
    cx.update_window(window.into(), |_root, window, cx| {
        window.dispatch_event(PlatformInput::ModifiersChanged(event), cx);
    })
    .unwrap();
    cx.run_until_parked();
}
