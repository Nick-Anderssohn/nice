//! `AppShellView` — the per-window composition root that mounts the shipped
//! Swift-parity shell (`Sources/Nice/Views/AppShellView.swift`): the R11 pane
//! strip riding the chrome band, the R10 floating sidebar card on the leading
//! edge, and terminal content that follows the active pane through R13's
//! [`SessionManager`]. One `AppShellView` per window; the app's window builder
//! (`crate::app::build_window_root`) mints it over the window's shared
//! [`WindowState`] entity for the first window and every ⌘N window.
//!
//! ## How the three surfaces compose (gpui-native, no Swift seam ports)
//!
//! The layout tree is rooted in [`SidebarShellView`], which already owns the two
//! shell modes (expanded floating card / collapsed full-width band), the peek overlay, and
//! the resize handle. R13.5 threads the toolbar band and the pane content into
//! its previously-placeholder content region through two injected `AnyView`
//! slots (`main_toolbar` / `main_body`) — mirroring Swift's `expandedShell`
//! (`HStack { card ; VStack { WindowToolbarView ; mainContent } }`); the
//! collapsed shell is the M2 full-width band (spacer + restore + toolbar — an
//! approved divergence from Swift's `collapsedShell` cap card) — without
//! re-deriving the collapse/peek/resize geometry the sidebar already encodes. `AppShellView` itself is thin: it carries the window-level
//! peek-clear modifier observer (the R12 keymap's `on_window_modifiers_changed`,
//! formerly on `WindowChromeView`) and re-renders the whole shell subtree when
//! any of its parts notify — so a pill click (which notifies only the toolbar)
//! or a sidebar-row click (which notifies only the sidebar) still re-renders the
//! [`PaneHostView`] sibling and switches pane content.
//!
//! The R9 chrome band's drag / double-click / traffic-light-row behaviour is
//! preserved byte-for-byte: in the shipped shell the band role is carried by the
//! [`WindowToolbarView`] band and the [`SidebarShellView`] top strip (each
//! already replicates the R9 press-arbitration + drag-threshold + full-screen
//! gate). `WindowChromeView` itself is unchanged and still exercised by the
//! `chrome` self-test scenario.
//!
//! ## Pane content host (the PROTECTED activation decision)
//!
//! [`PaneHostView`] maps the active `(tab_id, pane_id)` →
//! [`SessionManager::pane_handle`] → a per-pane [`TerminalView`] created lazily
//! on first activation, cached per pane id, and dropped when the pane leaves the
//! model. It uses the shared theme / accent / font exactly as
//! `open_managed_window` does today. Activation flows **only** through
//! [`SessionManager::activate_pane`] (R13's deferred-spawn + focus preserved
//! verbatim — no view-side spawn shortcuts), and the demand-present kick is
//! re-pointed to the active pane's handle on every switch.

// Mounted by `crate::app::build_window_root`; the AX-anchor label constants are a
// deliberately-exported contract (the shipped-surface assertion hooks, §6) whose
// asserting scenario lands in the next slice.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    div, prelude::*, rgb, AnyElement, App, Context, Entity, FocusHandle, Render, Subscription,
    Window,
};

use nice_term_view::{FontSettings, TerminalSessionHandle, TerminalTheme, TerminalView};
use nice_theme::color::Srgba;

use crate::sidebar_shell::SidebarShellView;
use crate::toolbar::WindowToolbarView;
use crate::window_state::WindowState;

/// The AX label + element id the shipped sidebar card root carries so an AX walk
/// (`crate::platform::ax_find_titled_role`) can find it — the exported
/// shipped-surface assertion hook (§6). Placed on [`SidebarShellView`]'s card
/// root by the sidebar view; named here as the composition contract.
pub(crate) const SIDEBAR_ROOT_LABEL: &str = "nice-rs-sidebar-root";

/// The AX label + element id the shipped pane-strip (toolbar) root carries — the
/// sibling of [`SIDEBAR_ROOT_LABEL`]. Placed on [`WindowToolbarView`]'s root.
pub(crate) const PANE_STRIP_ROOT_LABEL: &str = "nice-rs-pane-strip-root";

/// The per-window composition root. Renders the shipped shell (sidebar card +
/// toolbar band + pane content) and owns the window-level peek-clear observer.
pub(crate) struct AppShellView {
    /// The shell subtree: the [`SidebarShellView`] with the toolbar band + pane
    /// host injected into its content slots. Rendered as this view's sole child.
    sidebar: Entity<SidebarShellView>,
    /// Held so a re-render of the whole shell can be forced when any composed
    /// part notifies (the observers below). Kept alive alongside `sidebar`.
    toolbar: Entity<WindowToolbarView>,
    /// The pane-content host mounted in the sidebar's body slot. Held here so
    /// the `app-shell` scenario can reach the SAME host the window renders (the
    /// M2 Item D focus-routing assertions read its active terminal's focus
    /// handle) — not a parallel copy.
    pane_host: Entity<PaneHostView>,
    /// Kept for lifetime + future direct routing; the shared per-window state.
    state: Entity<WindowState>,
    /// Re-render the shell subtree whenever the shared state (keymap-driven
    /// actions), the toolbar (pill clicks), or the sidebar (row clicks) notifies.
    /// A pill/row click notifies only its own view; without observing them here
    /// the [`PaneHostView`] sibling would not re-render, so pane content would not
    /// follow a click-driven active-pane change. Re-rendering `AppShellView`
    /// re-renders the whole subtree (child views render fresh each parent render),
    /// so all three surfaces stay in lockstep.
    _subs: Vec<Subscription>,
}

impl AppShellView {
    /// Compose the shell over the window's shared state. `sidebar` already holds
    /// the injected toolbar + pane-host slots (wired by
    /// `crate::app::build_window_root`); `toolbar` is held only so its notifies
    /// can force a full-shell re-render.
    pub(crate) fn new(
        state: Entity<WindowState>,
        sidebar: Entity<SidebarShellView>,
        toolbar: Entity<WindowToolbarView>,
        pane_host: Entity<PaneHostView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subs = vec![
            cx.observe(&state, |_this, _e, cx| cx.notify()),
            cx.observe(&sidebar, |_this, _e, cx| cx.notify()),
            cx.observe(&toolbar, |_this, _e, cx| cx.notify()),
        ];
        Self {
            sidebar,
            toolbar,
            pane_host,
            state,
            _subs: subs,
        }
    }
}

// Scenario accessors — the read surface the `app-shell` self-test scenario
// (`crate::app_shell_live`) uses to ground-truth the mounted shell. They hand back
// the composition root's already-held child entities (no new state, no clones of
// the model), so the scenario asserts the SAME sidebar / toolbar the shipped window
// renders — not a parallel copy.
impl AppShellView {
    /// The shell's sidebar view (its collapse state + rendered leading-column
    /// geometry drive the ⌘B collapse/expand assertion).
    pub(crate) fn scenario_sidebar(&self) -> Entity<SidebarShellView> {
        self.sidebar.clone()
    }

    /// The shell's toolbar / pane-strip view (its pill list + laid-out pill bounds
    /// drive the ⌘T "visible pill" assertion, and its `drive_*` seams the strip-`+`
    /// / close-pane assertions).
    pub(crate) fn scenario_toolbar(&self) -> Entity<WindowToolbarView> {
        self.toolbar.clone()
    }

    /// The shell's pane-content host (its active terminal's focus handle drives
    /// the M2 Item D focus-routing assertions).
    pub(crate) fn scenario_pane_host(&self) -> Entity<PaneHostView> {
        self.pane_host.clone()
    }
}

impl Render for AppShellView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            // R12: the window-level peek clear (moved here from `WindowChromeView`
            // when the shell replaced the bare chrome band as the window root). A
            // sidebar-tab cycle on a collapsed sidebar floats the peek overlay; this
            // ends it once the shortcut's modifiers are all released. Registered on
            // the root so it observes modifier changes regardless of which
            // descendant holds focus.
            .on_modifiers_changed(|event, _window, cx| {
                crate::keymap::on_window_modifiers_changed(event, cx)
            })
            .child(self.sidebar.clone())
    }
}

/// The pane-content host: maps the active `(tab_id, pane_id)` to a per-pane
/// [`TerminalView`] through the window's [`SessionManager`], following the active
/// pane as tabs / panes switch. See the module docs' PROTECTED activation
/// decision.
pub(crate) struct PaneHostView {
    /// The shared per-window state (its [`SessionManager`](crate::session_manager::SessionManager)
    /// owns the live pane sessions; its [`TabModel`](nice_model::TabModel) names
    /// the active pane).
    state: Entity<WindowState>,
    /// Re-render when the shared state notifies (a keymap action switched the
    /// active pane / tab). Click-driven switches are covered by [`AppShellView`]'s
    /// observers cascading a full re-render; this covers keymap-driven switches
    /// directly too.
    _state_sub: Subscription,
    /// The shared terminal theme (Nice/Dark), exactly as `open_managed_window`
    /// builds it.
    theme: TerminalTheme,
    /// The user's accent (Terracotta default) — the caret / launch overlay tint.
    accent: Srgba,
    /// The process-level shared [`FontSettings`] every pane observes, so a
    /// ⌘=/⌘−/⌘0 zoom fans out across panes and windows.
    font: Entity<FontSettings>,
    /// Per-pane [`TerminalView`] cache (`pane_id -> view`). Created lazily on
    /// first activation; an entry is dropped when its pane leaves the model.
    cache: HashMap<String, Entity<TerminalView>>,
    /// The `(tab_id, pane_id)` hosted as of the last render — a change drives the
    /// single [`SessionManager::activate_pane`] activation + a present-kick
    /// re-point.
    last_active: Option<(String, String)>,
}

impl PaneHostView {
    /// A host over the window's shared state, using the shared theme / accent /
    /// font. Starts with an empty cache; the first render activates + hosts the
    /// model's active pane.
    pub(crate) fn new(
        state: Entity<WindowState>,
        theme: TerminalTheme,
        accent: Srgba,
        font: Entity<FontSettings>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&state, |_this, _e, cx| cx.notify());
        Self {
            state,
            _state_sub: sub,
            theme,
            accent,
            font,
            cache: HashMap::new(),
            last_active: None,
        }
    }

    /// Build a fresh [`TerminalView`] over `handle` with the shared theme / accent
    /// / font and the same platform injections `open_managed_window` wires (the
    /// keyCode side-channel, the raw-image drop fallback, the App-Nap-safe launch
    /// deadline) — the sole objc2 crossings, injected here so `nice-term-view`
    /// stays foreign-code-free.
    fn make_terminal_view(
        &self,
        handle: Entity<TerminalSessionHandle>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let theme = self.theme.clone();
        let accent = self.accent;
        let font = self.font.clone();
        cx.new(|tcx| {
            let mut view = TerminalView::new(handle, theme, accent, font, tcx);
            view.set_keycode_probe(Arc::new(crate::platform::current_event_keycode));
            view.set_image_drop_provider(Arc::new(crate::platform::read_dropped_image_to_temp));
            view.set_launch_deadline(crate::platform::launch_deadline());
            // M2 Item E: the shipped shell's grid tracks the window — a painted
            // bounds change re-fits the pty (rows/cols → TIOCSWINSZ/SIGWINCH).
            // Fixed-grid scenario embeddings deliberately leave this off.
            view.set_auto_refit(true);
            view
        })
    }

    /// Move key focus to the active pane's hosted terminal, if any — the
    /// app-side focus-routing seam (M2 Item D). The toolbar / sidebar call it
    /// after an inline-rename commit/cancel and on context-menu dismissal, and
    /// the chrome roots bounce stray chrome-click focus back through it. A
    /// no-op when the active pane has no hosted view (a model-only Claude pane).
    pub(crate) fn focus_active_terminal(&self, window: &mut Window, cx: &mut App) {
        if let Some(fh) = self.active_terminal_focus_handle(cx) {
            window.focus(&fh, cx);
        }
    }

    /// The active pane's terminal focus handle, if a view is hosted for it —
    /// the `app-shell` scenario's "focus returned to the terminal" read.
    pub(crate) fn active_terminal_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let (_, pane) = self.last_active.as_ref()?;
        let view = self.cache.get(pane)?;
        Some(view.read(cx).focus_handle_ref().clone())
    }
}

/// The pane-host's fill when the active pane has no live session yet (a
/// model-only Claude pane, or a terminal pane an instant before its deferred
/// spawn caches a handle) — the shipped dark backdrop, matching the terminal
/// theme's background so the swap to a real grid is seamless.
fn pane_placeholder() -> impl IntoElement {
    div().size_full().bg(rgb(0x11141b))
}

impl Render for PaneHostView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot the active pane + the full set of live pane ids up front, then
        // drop the state borrow before any mutation.
        let (active, all_pane_ids): (Option<(String, String)>, HashSet<String>) = {
            let ws = self.state.read(cx);
            let model = &ws.model;
            let tab = model.active_tab_id().map(str::to_owned);
            let pane = tab
                .as_deref()
                .and_then(|t| model.tab_for(t))
                .and_then(|t| t.active_pane_id.clone());
            let active = match (tab, pane) {
                (Some(t), Some(p)) => Some((t, p)),
                _ => None,
            };
            let mut all = HashSet::new();
            for project in &model.projects {
                for t in &project.tabs {
                    for p in &t.panes {
                        all.insert(p.id.clone());
                    }
                }
            }
            (active, all)
        };

        // Drop cached views for panes that left the model (the PROTECTED
        // "dropped when the pane leaves the model"). The pane's pty session lives
        // on in the `SessionManager` until window teardown reaps it (SIGHUP→
        // SIGKILL); wiring the UI close actions to the R13 dissolve cascade is
        // out of this composition slice.
        self.cache.retain(|pid, _| all_pane_ids.contains(pid));

        // On a switch, run the sole activation path + re-point the present kick.
        let activation_changed = active != self.last_active;
        if activation_changed {
            self.last_active = active.clone();
            if let Some((tab, pane)) = active.clone() {
                let state = self.state.clone();
                state.update(cx, |ws, wcx| {
                    // Ensure the active tab has a session container so R13's
                    // deferred-spawn precondition holds (Swift makes the session
                    // when a tab is first shown). Idempotent; not a spawn itself —
                    // spawning still flows through `activate_pane`.
                    ws.session.register_tab_session(&tab);
                    let model = &mut ws.model;
                    let session = &mut ws.session;
                    session.activate_pane(model, &tab, &pane, window, wcx);
                });
                // Re-point the demand-present kick to the (now-active) pane's
                // handle so its damage kicks this window while occluded.
                let handle = self.state.read(cx).session.pane_handle(&tab, &pane);
                if let Some(handle) = handle {
                    crate::app::install_present_kick(&handle, window.window_handle(), cx);
                }
            }
        }

        // Host the active pane's view (lazily created + cached), else a
        // placeholder when the pane has no live session.
        let content: AnyElement = match &active {
            Some((tab, pane)) => {
                let handle = self.state.read(cx).session.pane_handle(tab, pane);
                match handle {
                    Some(handle) => {
                        if !self.cache.contains_key(pane) {
                            let view = self.make_terminal_view(handle, cx);
                            self.cache.insert(pane.clone(), view);
                        }
                        // Safe: just inserted (or already present).
                        self.cache.get(pane).unwrap().clone().into_any_element()
                    }
                    None => pane_placeholder().into_any_element(),
                }
            }
            None => pane_placeholder().into_any_element(),
        };

        // Focus follows activation (M2 Item D): the terminal's per-frame render
        // grab is gone (focus-once in `TerminalView`), so the host moves key
        // focus to the newly-active pane's terminal on every activation change —
        // window open, ⌘T, pill/row click, pane-step, close-refocus. Runs after
        // the cache fill above so a just-activated terminal pane (spawned
        // synchronously by `activate_pane`) is focusable this same render. A
        // pane with no hosted view (Claude placeholder) has nothing to focus.
        if activation_changed {
            if let Some((_, pane)) = &active {
                if let Some(view) = self.cache.get(pane) {
                    let fh = view.read(cx).focus_handle_ref().clone();
                    window.focus(&fh, cx);
                }
            }
        }

        div().size_full().child(content)
    }
}
