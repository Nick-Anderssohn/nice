//! `AppShellView` — the per-window composition root that mounts the shipped
//! Swift-parity shell (`Sources/Nice/Views/AppShellView.swift`): the R11 window
//! strip riding the chrome band, the R10 floating sidebar card on the leading
//! edge, and terminal content that follows the active window through R13's
//! [`PtyManager`]. One `AppShellView` per window; the app's window builder
//! (`crate::app::build_window_root`) mints it over the window's shared
//! [`WindowState`] entity for the first window and every ⌘N window.
//!
//! ## How the three surfaces compose (gpui-native, no Swift seam ports)
//!
//! The layout tree is rooted in [`SidebarShellView`], which already owns the two
//! shell modes (expanded floating card / collapsed full-width band), the peek overlay, and
//! the resize handle. R13.5 threads the toolbar band and the window content into
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
//! [`WindowHostView`] sibling and switches window content.
//!
//! The R9 chrome band's drag / double-click / traffic-light-row behaviour is
//! preserved byte-for-byte: in the shipped shell the band role is carried by the
//! [`WindowToolbarView`] band and the [`SidebarShellView`] top strip (each
//! already replicates the R9 press-arbitration + drag-threshold + full-screen
//! gate). `WindowChromeView` itself is unchanged and still exercised by the
//! `chrome` self-test scenario.
//!
//! ## Window content host (the PROTECTED activation decision)
//!
//! [`WindowHostView`] maps the active `(session_id, term_window_id, pane_id)` →
//! [`PtyManager::pane_handle`] → a per-PANE [`TerminalView`] created lazily on
//! first activation, cached per pane id, and dropped when its pane leaves the
//! model. It uses the shared theme / accent / font exactly as
//! `open_managed_window` does today. Activation flows **only** through
//! [`PtyManager::activate_term_window`] (R13's deferred-spawn — no view-side spawn
//! shortcuts); this host then focuses the newly-active pane's terminal on the
//! same activation render, and re-points the demand-present kick to every
//! visible pane's handle whenever that set changes.
//!
//! ## Splits (tmux-port Phase 2)
//!
//! The active pill's [`PaneLayout`](nice_model::PaneLayout) is mounted
//! recursively: each `Split` node becomes a flex row / column holding its two
//! children with a 6 px divider between them, and each `Leaf` mounts that pane's
//! cached view (or the [`window_placeholder`] when its pty hasn't cached a
//! handle yet). The flex basis of the two children is the split's ratio, so the
//! painted rects match [`PaneLayout::leaf_rects`](nice_model::PaneLayout::leaf_rects)
//! exactly — one geometry for what the user sees, what the keyboard walks, and
//! what a divider drag writes. A never-split pill is a single leaf, i.e. the
//! pre-splits render with no divider and no focus affordance: byte-identical
//! output to before.
//!
//! Three things ride along with the tree: the divider drag (the sidebar
//! resize-handle pattern, root-level move/up so it survives cursor drift), a
//! click anywhere in a pane focusing it, and a bounds probe that stashes the
//! pane area's painted size on [`WindowState`] for the App-level pane actions,
//! which have no `&mut Window` to measure with.

// Mounted by `crate::app::build_window_root`; the AX-anchor label constants are a
// deliberately-exported contract (the shipped-surface assertion hooks, §6) whose
// asserting scenario lands in the next slice.
#![allow(dead_code)]

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, prelude::*, px, relative, rgb, AnyElement, App, Bounds, Context, CursorStyle,
    Entity, FocusHandle, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render,
    Rgba, Subscription, Window,
};

use nice_model::pane_layout::{RATIO_MAX, RATIO_MIN};
use nice_model::{Pane, PaneLayout, PaneRect, Side, SplitOrient};
use nice_term_view::{FontSettings, TerminalSessionHandle, TerminalTheme, TerminalView};
use nice_theme::color::Srgba;

use crate::inline_rename::{apply_rename_click, FieldColors};
use crate::search_bar::{
    dispatch_search_key, search_bar_element, search_bar_is_stale, SearchKeyOutcome,
};
use crate::sidebar_shell::SidebarShellView;
use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};
use crate::toolbar::WindowToolbarView;
use crate::window_state::WindowState;

/// The AX label + element id the shipped sidebar card root carries so an AX walk
/// (`crate::platform::ax_find_titled_role`) can find it — the exported
/// shipped-surface assertion hook (§6). Placed on [`SidebarShellView`]'s card
/// root by the sidebar view; named here as the composition contract.
pub(crate) const SIDEBAR_ROOT_LABEL: &str = "nice-sidebar-root";

/// The AX label + element id the shipped window-strip (toolbar) root carries — the
/// sibling of [`SIDEBAR_ROOT_LABEL`]. Placed on [`WindowToolbarView`]'s root.
pub(crate) const WINDOW_STRIP_ROOT_LABEL: &str = "nice-pane-strip-root";

/// The per-window composition root. Renders the shipped shell (sidebar card +
/// toolbar band + window content) and owns the window-level peek-clear observer.
pub(crate) struct AppShellView {
    /// The shell subtree: the [`SidebarShellView`] with the toolbar band + window
    /// host injected into its content slots. Rendered as this view's sole child.
    sidebar: Entity<SidebarShellView>,
    /// Held so a re-render of the whole shell can be forced when any composed
    /// part notifies (the observers below). Kept alive alongside `sidebar`.
    toolbar: Entity<WindowToolbarView>,
    /// The window-content host mounted in the sidebar's body slot. Held here so
    /// the `app-shell` scenario can reach the SAME host the window renders (the
    /// M2 Item D focus-routing assertions read its active terminal's focus
    /// handle) — not a parallel copy.
    window_host: Entity<WindowHostView>,
    /// Kept for lifetime + future direct routing; the shared per-window state.
    state: Entity<WindowState>,
    /// R20 (F6): the per-window drift banner — a bottom overlay observing the ONE
    /// process-wide file-operation history. Mounted here so every window shows the
    /// same transient message regardless of sidebar mode (Swift parity); renders
    /// nothing when no history Global is installed.
    banner: Entity<crate::file_browser::banner::DriftBannerView>,
    /// Re-render the shell subtree whenever the shared state (keymap-driven
    /// actions), the toolbar (pill clicks), or the sidebar (row clicks) notifies.
    /// A pill/row click notifies only its own view; without observing them here
    /// the [`WindowHostView`] sibling would not re-render, so window content would not
    /// follow a click-driven active-window change. Re-rendering `AppShellView`
    /// re-renders the whole subtree (child views render fresh each parent render),
    /// so all three surfaces stay in lockstep.
    _subs: Vec<Subscription>,
}

impl AppShellView {
    /// Compose the shell over the window's shared state. `sidebar` already holds
    /// the injected toolbar + window-host slots (wired by
    /// `crate::app::build_window_root`); `toolbar` is held only so its notifies
    /// can force a full-shell re-render.
    pub(crate) fn new(
        state: Entity<WindowState>,
        sidebar: Entity<SidebarShellView>,
        toolbar: Entity<WindowToolbarView>,
        window_host: Entity<WindowHostView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let banner = cx.new(crate::file_browser::banner::DriftBannerView::new);
        let subs = vec![
            cx.observe(&state, |_this, _e, cx| cx.notify()),
            cx.observe(&sidebar, |_this, _e, cx| cx.notify()),
            cx.observe(&toolbar, |_this, _e, cx| cx.notify()),
            // The banner drives its own re-render on a published message, but the
            // shell re-renders the whole subtree, so keep it in lockstep.
            cx.observe(&banner, |_this, _e, cx| cx.notify()),
        ];
        Self {
            sidebar,
            toolbar,
            window_host,
            state,
            banner,
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

    /// The shell's toolbar / window-strip view (its pill list + laid-out pill bounds
    /// drive the ⌘T "visible pill" assertion, and its `drive_*` seams the strip-`+`
    /// / close-window assertions).
    pub(crate) fn scenario_toolbar(&self) -> Entity<WindowToolbarView> {
        self.toolbar.clone()
    }

    /// The shell's window-content host (its active terminal's focus handle drives
    /// the M2 Item D focus-routing assertions).
    pub(crate) fn scenario_window_host(&self) -> Entity<WindowHostView> {
        self.window_host.clone()
    }
}

impl Render for AppShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // W5/R18: the confirmation dialog presented over this window, if any. It
        // lives on the shared `WindowState` (so the app-level quit/close paths can
        // present it) and renders as a deferred overlay above the whole shell.
        let modal = self.state.read(cx).pending_modal();
        // The window-level background layer: an opaque backing behind the whole
        // shell. The active terminal theme's background fills the window body
        // (including the gutter around the floating sidebar card) AND the full
        // window height up to the top edge — the 2026-07 restyle removed the
        // separate opaque chrome band + 1pt rule that used to cap the top 52pt
        // (plan `docs/plans/restyle/01-titlebar-restyle.md`: the titlebar is fill-less, so
        // the body backing simply extends to the top). Without this backing,
        // unpainted regions fell through to gpui's Metal clear color (opaque
        // black) — invisible in dark mode but wrong in light mode. It reads the
        // live `SharedThemeState`, so the R21 fan-out's `refresh_windows()`
        // repaints it on any theme / scheme change.
        let (terminal_theme, _) = crate::theme_settings::active_terminal_theme_and_accent(cx);
        // Restyle plan 3: the backing carries the active-scheme surface-fill
        // opacity (1.0 when no live theme is installed — scenarios / tests stay
        // opaque; 1.0 also while full screen, where macOS puts a black backdrop
        // behind the window instead of the wallpaper). `refresh_windows()` in the
        // theme fan-out re-runs this render on any opacity / scheme change.
        let opacity = crate::theme_settings::effective_window_opacity(window, cx);
        div()
            .size_full()
            .bg(terminal_backing_color(&terminal_theme, opacity))
            // App-wide font family: the (single) terminal font-family setting drives
            // the WHOLE app, not just the terminal grid. Set on the shell root so
            // every chrome descendant (sidebar, session strip, settings, buttons,
            // overlays) inherits it via gpui's text-style cascade unless it sets its
            // own family. Only the family cascades — chrome text SIZES stay per-view,
            // and the terminal grid is unaffected (it shapes glyphs with an explicit
            // font, not the inherited style). `None` before the keymap wires the
            // `SharedFontSettings` global (isolated scenarios), leaving gpui's default
            // UI family. Re-runs on any family change via the font pane's
            // `refresh_windows()`.
            .when_some(
                crate::sidebar_shell::resolved_mono_family(cx),
                |el, fam| el.font_family(fam),
            )
            // R12: the window-level peek clear (moved here from `WindowChromeView`
            // when the shell replaced the bare chrome band as the window root). A
            // sidebar-session cycle on a collapsed sidebar floats the peek overlay; this
            // ends it once the shortcut's modifiers are all released. Registered on
            // the root so it observes modifier changes regardless of which
            // descendant holds focus.
            .on_modifiers_changed(|event, _window, cx| {
                crate::keymap::on_window_modifiers_changed(event, cx)
            })
            .child(self.sidebar.clone())
            // R20 (F6): the drift banner floats as a bottom overlay above the shell
            // (below any presented modal).
            .child(self.banner.clone())
            .children(modal)
    }
}

/// The window-content host: maps the active `(session_id, term_window_id)` to a per-window
/// [`TerminalView`] through the window's [`PtyManager`], following the active
/// window as sessions / windows switch. See the module docs' PROTECTED activation
/// decision.
pub(crate) struct WindowHostView {
    /// The shared per-window state (its [`PtyManager`](crate::pty_manager::PtyManager)
    /// owns the live window sessions; its [`WorkspaceModel`](nice_model::WorkspaceModel) names
    /// the active window).
    state: Entity<WindowState>,
    /// Re-render when the shared state notifies (a keymap action switched the
    /// active window / session). Click-driven switches are covered by [`AppShellView`]'s
    /// observers cascading a full re-render; this covers keymap-driven switches
    /// directly too.
    _state_sub: Subscription,
    /// The shared terminal theme (Nice/Dark), exactly as `open_managed_window`
    /// builds it.
    theme: TerminalTheme,
    /// The user's accent (Terracotta default) — the caret / launch overlay tint.
    accent: Srgba,
    /// The active-scheme surface-fill opacity (0.55–1.0) each hosted terminal
    /// paints its DEFAULT background at (restyle plan 3). Seeded from the live
    /// theme at construction and refreshed by the theme fan-out, so a window built
    /// later inherits it too. `1.0` ⇒ the opaque pre-restyle grid.
    background_opacity: f32,
    /// The process-level shared [`FontSettings`] every window observes, so a
    /// ⌘=/⌘−/⌘0 zoom fans out across windows and windows.
    font: Entity<FontSettings>,
    /// Per-PANE [`TerminalView`] cache (`pane_id -> view`). Created lazily when a
    /// pane is first painted; an entry is dropped when its pane leaves the model
    /// (its pill closed, or the pane itself did). Pane ids are unique across the
    /// whole window, so one flat map covers every pill.
    cache: HashMap<String, Entity<TerminalView>>,
    /// The `(session_id, term_window_id, pane_id)` hosted as of the last render.
    /// A change in the first two drives the single
    /// [`PtyManager::activate_term_window`] activation; a change in any of them
    /// moves key focus.
    last_active: Option<(String, String, String)>,
    /// The pane ids the demand-present kick is currently pointed at. Re-pointed
    /// whenever the painted set changes — every visible pane needs it, not just
    /// the focused one, or a background pane's output would never present on an
    /// occluded window.
    kicked: HashSet<String>,
    /// The pane area's window-space bounds, refreshed by a `canvas` probe on
    /// every paint. Read at the top of the next render and stashed onto
    /// [`WindowState`] as the px context the pane actions measure against; the
    /// divider drag reads it back from there.
    pane_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The divider drag in flight, if any (see [`DividerDrag`]).
    divider_drag: Option<DividerDrag>,
}

/// A divider drag in progress — the sidebar resize-handle pattern
/// (`crate::sidebar_shell`), applied to a split node.
///
/// The node is held as a PATH from the tree root, never as a reference: a pane
/// can exit mid-drag and collapse its split, in which case the path resolves to
/// nothing and the drag ends as a no-op instead of writing some other node's
/// ratio.
struct DividerDrag {
    session_id: String,
    term_window_id: String,
    /// The split node whose ratio this drag writes.
    path: Vec<Side>,
    /// Which axis the drag travels along.
    orient: SplitOrient,
    /// Pointer position along that axis at mouse-down.
    origin: f32,
    /// The split's ratio when the press landed — the baseline every move is
    /// measured against, so a drag never accumulates rounding.
    baseline: f32,
    /// The split's extent along its axis MINUS the divider: the px the ratio
    /// divides, i.e. the denominator turning a px delta into a ratio delta.
    available: f32,
    /// Whether the press actually moved. A zero-movement click commits nothing.
    moved: bool,
}

/// Width of the invisible hit zone between two panes, and the gap
/// [`PaneLayout::leaf_rects`](nice_model::PaneLayout::leaf_rects) reserves for
/// it. Matches the sidebar resize handle's 6 pt zone.
pub(crate) const PANE_DIVIDER_PX: f32 = 6.0;

/// Inner content inset every pane gets in a SPLIT pill (px). A single-pane
/// pill keeps the window-level `CONTENT_INSET_*` alone; in a split, each pane
/// adds its own breathing room so glyphs never touch a divider or the corner
/// ticks (the padding half of the 2026-08-13 styling revision — the shipped
/// affordance drew its ring flush against the grid).
pub(crate) const PANE_CONTENT_INSET_X: f32 = 12.0;
/// The vertical twin of [`PANE_CONTENT_INSET_X`].
pub(crate) const PANE_CONTENT_INSET_Y: f32 = 10.0;
/// Corner-tick bracket size (px, square). The 2px stroke lives in the
/// `border_*_2` builders at the draw site.
pub(crate) const PANE_TICK_SIZE: f32 = 11.0;
/// The tick's outer corner radius.
const PANE_TICK_RADIUS: f32 = 4.0;

/// P6 minimum pane extents. A split that would produce a pane under these is
/// refused (Slice 4's action handlers), and divider drags / keyboard resizes
/// clamp against them. The window-level `TERMINAL_MIN_WIDTH` is unrelated and
/// unchanged.
pub(crate) const PANE_MIN_WIDTH: f32 = 120.0;
/// The [`PANE_MIN_WIDTH`] twin for stacked splits.
pub(crate) const PANE_MIN_HEIGHT: f32 = 80.0;

impl WindowHostView {
    /// A host over the window's shared state, using the shared theme / accent /
    /// font. Starts with an empty cache; the first render activates + hosts the
    /// model's active window.
    pub(crate) fn new(
        state: Entity<WindowState>,
        theme: TerminalTheme,
        accent: Srgba,
        background_opacity: f32,
        font: Entity<FontSettings>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&state, |_this, _e, cx| cx.notify());
        Self {
            state,
            _state_sub: sub,
            theme,
            accent,
            background_opacity,
            font,
            cache: HashMap::new(),
            last_active: None,
            kicked: HashSet::new(),
            pane_bounds: Rc::new(Cell::new(None)),
            divider_drag: None,
        }
    }

    /// Build a fresh [`TerminalView`] over `handle` with the shared theme / accent
    /// / font and the same platform injections `open_managed_window` wires (the
    /// keyCode side-channel, the raw-image drop fallback, the ⌘+click URL opener,
    /// the App-Nap-safe launch deadline) — the sole objc2 crossings, injected
    /// here so `nice-term-view` stays foreign-code-free.
    ///
    /// `focused` says whether this pane is the one the model has focus on. A
    /// pane mounted un-focused skips the view's first-render focus grab, so a
    /// whole pane tree mounting in one pass cannot hand focus to whichever leaf
    /// rendered last (the host focuses the focused pane explicitly afterwards).
    fn make_terminal_view(
        &self,
        handle: Entity<TerminalSessionHandle>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let theme = self.theme.clone();
        let accent = self.accent;
        let background_opacity = self.background_opacity;
        let font = self.font.clone();
        cx.new(|tcx| {
            let mut view = TerminalView::new(handle, theme, accent, font, tcx);
            // Seed the surface-fill opacity so a window built after a slider change
            // (or on window open) inherits the current translucency, not the 1.0
            // default (restyle plan 3).
            view.set_background_opacity(background_opacity, tcx);
            view.set_keycode_probe(Arc::new(crate::platform::current_event_keycode));
            view.set_image_drop_provider(Arc::new(crate::platform::read_dropped_image_to_temp));
            // ⌘+click on a terminal link. The platform wrapper defers the
            // `NSWorkspace openURL:` to its own main-queue turn — the view calls
            // this from a mouse-up listener, i.e. inside a live `App` borrow.
            view.set_url_opener(Arc::new(crate::platform::workspace_open_url));
            view.set_launch_deadline(crate::platform::launch_deadline());
            // M2 Item E: the shipped shell's grid tracks the window — a painted
            // bounds change re-fits the pty (rows/cols → TIOCSWINSZ/SIGWINCH).
            // Fixed-grid scenario embeddings deliberately leave this off.
            view.set_auto_refit(true);
            view.set_focus_on_first_render(focused);
            view
        })
    }

    /// Push a live theme + accent recolor into every hosted window (R21 fan-out).
    /// Updates the host's own `theme`/`accent` so windows built LATER seed with the
    /// new colors too, then pushes into each cached [`TerminalView`] via the
    /// boundary-legal [`TerminalView::set_theme`] setter. Slice 3's
    /// `apply_theme_fanout` calls this per window (walking
    /// [`WindowRegistry::all_states`](crate::window_registry::WindowRegistry)); this
    /// slice provides the push seam.
    pub(crate) fn set_theme(&mut self, theme: TerminalTheme, accent: Srgba, cx: &mut Context<Self>) {
        self.theme = theme.clone();
        self.accent = accent;
        for view in self.cache.values() {
            view.update(cx, |v, vcx| v.set_theme(theme.clone(), accent, vcx));
        }
    }

    /// Push a live surface-fill opacity into every hosted window (restyle plan 3
    /// transparency fan-out). Updates the host's own `background_opacity` so windows
    /// built LATER seed with it too, then pushes into each cached
    /// [`TerminalView`] via [`TerminalView::set_background_opacity`]. The theme
    /// fan-out ([`crate::theme_settings::apply_theme_fanout`]) calls this per window
    /// alongside [`set_theme`](Self::set_theme).
    pub(crate) fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        self.background_opacity = opacity;
        for view in self.cache.values() {
            view.update(cx, |v, vcx| v.set_background_opacity(opacity, vcx));
        }
    }

    /// Push a live accent-only recolor into every hosted window (R21 accent fan-out),
    /// leaving the terminal theme untouched. Updates the host's `accent` so later
    /// windows seed with it too. The accent-only companion to
    /// [`set_theme`](Self::set_theme).
    pub(crate) fn set_accent(&mut self, accent: Srgba, cx: &mut Context<Self>) {
        self.accent = accent;
        for view in self.cache.values() {
            view.update(cx, |v, vcx| v.set_accent(accent, vcx));
        }
    }

    /// Move key focus to the focused pane's hosted terminal, if any — the
    /// app-side focus-routing seam (M2 Item D). The toolbar / sidebar call it
    /// after an inline-rename commit/cancel and on context-menu dismissal, and
    /// the chrome roots bounce stray chrome-click focus back through it. A
    /// no-op when the focused pane has no hosted view (a model-only Claude window).
    pub(crate) fn focus_active_terminal(&self, window: &mut Window, cx: &mut App) {
        if let Some(fh) = self.active_terminal_focus_handle(cx) {
            window.focus(&fh, cx);
        }
    }

    /// The focused PANE's terminal focus handle, if a view is hosted for it —
    /// the `app-shell` scenario's "focus returned to the terminal" read. In a
    /// split pill this is the focused leaf, never merely "the pill's terminal".
    pub(crate) fn active_terminal_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let (_, _, pane_id) = self.last_active.as_ref()?;
        let view = self.cache.get(pane_id)?;
        Some(view.read(cx).focus_handle_ref().clone())
    }

    /// The cached [`TerminalView`] hosting `pane_id`, if the host has built one.
    /// The cache is pane-keyed since Phase 2; a never-split pill's sole pane
    /// carries the pill's own id, which is why the window-level twin below can
    /// resolve through the model.
    pub(crate) fn scenario_terminal_for(&self, pane_id: &str) -> Option<Entity<TerminalView>> {
        self.cache.get(pane_id).cloned()
    }

    /// The cached [`TerminalView`] hosting `term_window_id`'s FOCUSED pane — the
    /// `claude-lifecycle` /branch-overlay leg's read (it activates the deferred
    /// branch parent, then asserts that window's view never flashed the stray
    /// "Launching…" overlay), and the theme-fan-out / shortcuts scenarios' read
    /// of "the pill's terminal". Resolves the pill's focused pane through the
    /// model, so it keeps meaning the same thing once a pill can hold several.
    pub(crate) fn scenario_terminal_for_window(
        &self,
        term_window_id: &str,
        cx: &App,
    ) -> Option<Entity<TerminalView>> {
        let pane_id = {
            let ws = self.state.read(cx);
            ws.workspace
                .projects
                .iter()
                .flat_map(|p| p.sessions.iter())
                .flat_map(|s| s.windows.iter())
                .find(|w| w.id == term_window_id)
                .map(|w| w.effective_pane_id())?
        };
        self.scenario_terminal_for(&pane_id)
    }
}

/// What every leaf / divider of one render pass needs to know about the pill it
/// belongs to. Built once per render and passed down the recursion.
struct TreeCtx {
    session_id: String,
    term_window_id: String,
    /// The pane the model has focus on.
    active_pane_id: String,
    /// Whether this pill paints more than one pane right now — the gate on the
    /// focused-pane affordance, so a never-split pill renders exactly as before.
    multi: bool,
    /// The corner-tick tint.
    accent: Srgba,
}

// Pane-tree render + divider drag.
impl WindowHostView {
    /// Mount one node of the active pill's tree: a leaf's terminal, or a split's
    /// two children with the divider between them.
    ///
    /// The two children carry the split's ratio as their flex basis and shrink
    /// against the divider's fixed 6 px, which lands them on exactly the rects
    /// [`PaneLayout::leaf_rects`] computes — the one geometry every pane verb
    /// shares. Each node is threaded its path from the root, the identity a
    /// divider drag holds on to (a pane can exit and collapse a split, which no
    /// reference would survive).
    fn render_node(
        &self,
        node: &PaneLayout,
        path: Vec<Side>,
        ctx: &TreeCtx,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PaneLayout::Leaf(pane) => self.leaf_element(pane, ctx, cx),
            PaneLayout::Split {
                orient,
                ratio,
                first,
                second,
            } => {
                let ratio = ratio.clamp(RATIO_MIN, RATIO_MAX);
                let mut first_path = path.clone();
                first_path.push(Side::First);
                let mut second_path = path.clone();
                second_path.push(Side::Second);
                let first_el = self.render_node(first, first_path, ctx, cx);
                let second_el = self.render_node(second, second_path, ctx, cx);
                let row = div().flex().size_full().min_w_0().min_h_0();
                let row = match orient {
                    SplitOrient::Beside => row.flex_row(),
                    SplitOrient::Stacked => row.flex_col(),
                };
                row.child(
                    div()
                        .flex()
                        .flex_basis(relative(ratio))
                        .min_w_0()
                        .min_h_0()
                        .child(first_el),
                )
                .child(self.divider_element(path, *orient, ctx, cx))
                .child(
                    div()
                        .flex()
                        .flex_basis(relative(1.0 - ratio))
                        .min_w_0()
                        .min_h_0()
                        .child(second_el),
                )
                .into_any_element()
            }
        }
    }

    /// One pane: its cached terminal (or the placeholder until its pty caches a
    /// handle), the focused-pane affordance in a split pill, and the click that
    /// moves focus here.
    fn leaf_element(&self, pane: &Pane, ctx: &TreeCtx, cx: &mut Context<Self>) -> AnyElement {
        let focused = pane.id == ctx.active_pane_id;
        // Phase 3: the scrollback search field, when one is open over THIS pane.
        // Read as an owned snapshot so the state borrow ends before the
        // listeners below (which mutate that same entity) are built.
        let search_bar = focused
            .then(|| {
                let ws = self.state.read(cx);
                ws.search_bar()
                    .filter(|bar| bar.targets_pane(&pane.id))
                    .map(|bar| bar.paint_snapshot())
            })
            .flatten()
            .map(|bar| self.search_bar_child(bar, ctx.accent, cx));
        let inner: AnyElement = match self.cache.get(&pane.id) {
            Some(view) => view.clone().into_any_element(),
            None => window_placeholder().into_any_element(),
        };
        let (session_id, term_window_id, pane_id) = (
            ctx.session_id.clone(),
            ctx.term_window_id.clone(),
            pane.id.clone(),
        );
        div()
            .flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            // A single-pane pill draws no clip, no inset, and no affordance —
            // pre-splits pills render pixel-identically.
            .when(ctx.multi, |el| {
                // Clipped to its own rect, so nothing a grid paints at its edge
                // can bleed across a divider into its neighbour; `relative`
                // roots the corner ticks.
                el.relative().overflow_hidden()
            })
            // The search bar is absolute, so its pane's box has to be its
            // positioning root. In a split that is already true (above); a
            // single-pane pill becomes `relative` only while a bar is open, so
            // a pill with no search running still renders exactly as before.
            .when(!ctx.multi && search_bar.is_some(), |el| el.relative())
            // Capture phase: the terminal's own mouse-down handling (selection,
            // app mouse reporting) stops propagation in some modes, so a bubble
            // listener would miss exactly the clicks that matter most.
            .capture_any_mouse_down(cx.listener(
                move |this, _e: &MouseDownEvent, window, cx| {
                    this.focus_pane(&session_id, &term_window_id, &pane_id, window, cx);
                },
            ))
            .child({
                // In a split, every pane carries its own content inset — the
                // breathing room a single-pane pill gets from the window-level
                // insets. Inside the clip so the inset area still paints the
                // terminal background.
                let content = div().flex().size_full().min_w_0().min_h_0();
                let content = if ctx.multi {
                    content
                        .px(px(PANE_CONTENT_INSET_X))
                        .py(px(PANE_CONTENT_INSET_Y))
                } else {
                    content
                };
                content.child(inner)
            })
            // The focused-pane affordance: four accent corner ticks. An
            // ENCLOSURE reads unambiguously in every layout — an edge bar at a
            // stacked boundary could belong to either neighbour — and unlike
            // dimming it costs the unfocused panes' legibility nothing.
            // Decided over mocks 2026-08-13: `docs/mocks/pane-focus-mocks.html`.
            .when(ctx.multi && focused, |el| el.children(corner_ticks(ctx.accent)))
            // Last child, so the field paints over the grid and the ticks.
            .children(search_bar)
            .into_any_element()
    }

    /// Build the open search field for the focused pane (Phase 3, P2). Its
    /// parentage — an absolute child of the pane's own box — is what makes zoom,
    /// divider drags, window resizes and tree restructures place it correctly
    /// for free, and it keeps the field off the `TerminalView`'s focus/bubble
    /// path (the terminal never sees the keys the field is typing).
    fn search_bar_child(
        &self,
        bar: crate::search_bar::SearchBarPaint,
        accent: Srgba,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slots = crate::theme_settings::active_chrome_slots(cx);
        let colors = FieldColors {
            bg: slot_to_rgba(slots.panel),
            border: slot_to_rgba(slots.line_strong),
            text: slot_to_rgba(slots.ink),
            caret: srgba_to_rgba(accent),
            selection: srgba_to_rgba(srgba_with_alpha(accent, 0.3)),
        };
        let focus = bar.focus.clone();
        let click_view = cx.weak_entity();
        let drag_view = cx.weak_entity();
        search_bar_element(
            bar,
            colors,
            cx.listener(Self::on_search_key),
            move |index, click_count, window, app| {
                // The pane wrapper's CAPTURE-phase mouse-down focuses the pane's
                // terminal before this bubble-phase handler runs, so a click in
                // the field would otherwise hand the keyboard back to the grid
                // mid-edit. Re-take focus here, after that has happened.
                window.focus(&focus, app);
                let _ = click_view.update(app, |this, cx| {
                    this.state.update(cx, |ws, wcx| {
                        if let Some(bar) = ws.search_bar_mut() {
                            apply_rename_click(&mut bar.editor, index, click_count);
                            wcx.notify();
                        }
                    });
                    cx.notify();
                });
            },
            move |index, _window, app| {
                let _ = drag_view.update(app, |this, cx| {
                    this.state.update(cx, |ws, wcx| {
                        if let Some(bar) = ws.search_bar_mut() {
                            bar.editor.extend_to(index);
                            wcx.notify();
                        }
                    });
                    cx.notify();
                });
            },
        )
        .into_any_element()
    }

    /// One keystroke into the open search field (P7). Editing is the shared
    /// inline-rename dispatch; the two verbs that are this field's own are Enter
    /// (confirm: jump to the nearest match, close, stay in copy mode) and Escape
    /// (close, stay in copy mode where you are).
    ///
    /// Every edit pushes the query down to the pane's handle, which is what
    /// makes the highlighting incremental (P8) — there is no separate "run the
    /// search" step while typing.
    fn on_search_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let capslock = window.capslock().on;
        let Some(handle) = self.search_bar_handle(cx) else {
            return;
        };
        let outcome = self.state.update(cx, |ws, wcx| {
            let bar = ws.search_bar_mut()?;
            let outcome = dispatch_search_key(
                &mut bar.editor,
                // The clipboard chords read/write through the `App` this
                // `Context` derefs to; the editor borrow above is the entity's,
                // so the two never overlap.
                &mut **wcx,
                &ks.key,
                ks.key_char.as_deref(),
                ks.modifiers.shift,
                ks.modifiers.alt,
                ks.modifiers.platform,
                ks.modifiers.control,
                capslock,
            );
            Some((outcome, bar.query()))
        });
        let Some((outcome, query)) = outcome else {
            return;
        };
        match outcome {
            SearchKeyOutcome::Edited => {
                handle.update(cx, |h, hcx| {
                    h.set_search_query(&query);
                    hcx.notify();
                });
                cx.notify();
                cx.stop_propagation();
            }
            SearchKeyOutcome::Confirm => {
                handle.update(cx, |h, hcx| {
                    // Push the final query first: the last keystroke before
                    // Enter may have been the one that completed it.
                    h.set_search_query(&query);
                    h.confirm_search();
                    hcx.notify();
                });
                self.close_search_bar(window, cx);
                cx.stop_propagation();
            }
            SearchKeyOutcome::Close => {
                self.close_search_bar(window, cx);
                cx.stop_propagation();
            }
            // Not the field's key (⌃⌘c, ⌘Q, …): let it reach the app keymap.
            SearchKeyOutcome::Ignored => {}
        }
    }

    /// The [`TerminalSessionHandle`] of the pane the open search field is
    /// driving, if both still exist.
    fn search_bar_handle(&self, cx: &App) -> Option<Entity<TerminalSessionHandle>> {
        let ws = self.state.read(cx);
        let (session_id, term_window_id, pane_id) = ws.search_bar()?.pane_key();
        ws.ptys.pane_handle(session_id, term_window_id, pane_id)
    }

    /// Close the search field and hand the keyboard back to the pane's terminal.
    /// The refocus is the load-bearing half: the field's focus handle dies with
    /// the element, and a window whose focus points at a handle nothing renders
    /// is a keyboard-dead window.
    fn close_search_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, wcx| {
            if ws.close_search_bar().is_some() {
                wcx.notify();
            }
        });
        self.focus_active_terminal(window, cx);
        cx.notify();
    }

    /// Close an open search field that has gone stale, returning whether it did
    /// — the three conditions are
    /// [`search_bar_is_stale`](crate::search_bar::search_bar_is_stale)'s. Run at
    /// the top of every render, before the tree is built, so a stale bar is
    /// never painted; the caller re-focuses the terminal on a `true`, which is
    /// what recovers the keyboard when `⌃⌘c` ended copy mode while the field
    /// held focus.
    ///
    /// Deliberately silent (no `cx.notify()`): this runs inside a render, and
    /// this pass already reads the closed state.
    fn sweep_stale_search_bar(
        &mut self,
        active: Option<&(String, String, String)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((session_id, term_window_id, pane_id)) = ({
            let ws = self.state.read(cx);
            ws.search_bar()
                .map(|bar| {
                    let (s, w, p) = bar.pane_key();
                    (s.to_string(), w.to_string(), p.to_string())
                })
        }) else {
            return false;
        };
        let copy_mode = self
            .state
            .read(cx)
            .ptys
            .pane_handle(&session_id, &term_window_id, &pane_id)
            .is_some_and(|handle| handle.read(cx).copy_mode_active());
        let focused = active.map(|(_, _, pane)| pane.as_str());
        if !search_bar_is_stale(&pane_id, focused, copy_mode) {
            return false;
        }
        self.state.update(cx, |ws, _| {
            ws.close_search_bar();
        });
        true
    }

    /// The invisible hit zone between a split's two children — the sidebar
    /// resize handle's pattern: cursor flip for discoverability, drag tracked at
    /// the ROOT (so it survives leaving the zone), double-click resets to half.
    fn divider_element(
        &self,
        path: Vec<Side>,
        orient: SplitOrient,
        ctx: &TreeCtx,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (session_id, term_window_id) = (ctx.session_id.clone(), ctx.term_window_id.clone());
        let zone = div().flex_none();
        let zone = match orient {
            SplitOrient::Beside => zone
                .w(px(PANE_DIVIDER_PX))
                .h_full()
                .cursor(CursorStyle::ResizeLeftRight),
            SplitOrient::Stacked => zone
                .h(px(PANE_DIVIDER_PX))
                .w_full()
                .cursor(CursorStyle::ResizeUpDown),
        };
        zone.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.on_divider_mouse_down(&session_id, &term_window_id, &path, orient, event, cx);
            }),
        )
        .into_any_element()
    }

    /// Move focus to `pane_id` (P4: un-zoom first). Called by a click anywhere
    /// in a pane's rect; the render reacts, focusing that pane's view.
    ///
    /// A click while ZOOMED changes nothing, and that is not a hole in P4's
    /// "pane click un-zooms" clause. Zoom paints ONLY the focused pane, so the
    /// only pane a click can land in is the one that already has focus — there
    /// is no focus change for the un-zoom to precede. Collapsing the zoom anyway
    /// would cost mouse selection inside a zoomed pane: the very press that sets
    /// a selection anchor would reflow and refit the pane under the cursor,
    /// moving the columns that anchor was placed in. `⌃⌘z` and the keyboard
    /// focus moves stay the way out of a zoom (tmux does not un-zoom on a click
    /// inside the zoomed pane either).
    fn focus_pane(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let term_window_id = term_window_id.to_string();
        let pane_id = pane_id.to_string();
        // Read before writing: a click on the ALREADY-focused pane (the common
        // case — every press that starts a selection) must not go through
        // `mutate_session`, whose did-mutate signal would queue a session save
        // per mouse-down.
        let needs_move = {
            let ws = self.state.read(cx);
            ws.workspace
                .session_for(session_id)
                .and_then(|s| s.windows.iter().find(|w| w.id == term_window_id))
                .is_some_and(|w| w.layout.contains(&pane_id) && w.active_pane_id != pane_id)
        };
        if needs_move {
            let target = pane_id.clone();
            self.state.update(cx, |ws, wcx| {
                ws.workspace.mutate_session(session_id, |session| {
                    if let Some(term_window) =
                        session.windows.iter_mut().find(|w| w.id == term_window_id)
                    {
                        term_window.active_pane_id = target;
                        // P4: any focus change un-zooms first, then applies.
                        term_window.zoomed = false;
                    }
                });
                wcx.notify();
            });
        }
        // Focus follows the click even when the model already named this pane
        // (a stray chrome interaction can leave the model right and the keyboard
        // elsewhere); the model write above is what a real focus MOVE needs.
        if let Some(view) = self.cache.get(&pane_id) {
            let fh = view.read(cx).focus_handle_ref().clone();
            window.focus(&fh, cx);
        }
        if needs_move {
            cx.notify();
        }
    }

    /// Begin a divider drag, or reset the split on a double-click.
    fn on_divider_mouse_down(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        path: &[Side],
        orient: SplitOrient,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let content = self.pane_content_rect(cx);
        let (layout, baseline) = {
            let ws = self.state.read(cx);
            let Some(term_window) = ws
                .workspace
                .session_for(session_id)
                .and_then(|s| s.windows.iter().find(|w| w.id == term_window_id))
            else {
                return;
            };
            let ratio = match term_window.layout.node_at(path) {
                Some(PaneLayout::Split { ratio, .. }) => *ratio,
                _ => return,
            };
            (term_window.layout.clone(), ratio)
        };

        if event.click_count >= 2 {
            // Double-click resets this split to an even divide (and persists it).
            self.write_ratio(session_id, term_window_id, path, 0.5, cx);
            self.divider_drag = None;
            self.commit_divider_drag(cx);
            cx.stop_propagation();
            return;
        }

        let Some(content) = content else {
            // Never painted: no px context, so a drag has no scale to work in.
            return;
        };
        let Some(available) = split_available_px(&layout, path, content) else {
            return;
        };
        self.divider_drag = Some(DividerDrag {
            session_id: session_id.to_string(),
            term_window_id: term_window_id.to_string(),
            path: path.to_vec(),
            orient,
            origin: match orient {
                SplitOrient::Beside => f32::from(event.position.x),
                SplitOrient::Stacked => f32::from(event.position.y),
            },
            baseline,
            available,
            moved: false,
        });
        cx.stop_propagation();
    }

    /// Root-level move: drives the in-flight drag (and treats a move with the
    /// button already released as the mouse-up we missed outside the window).
    fn on_root_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &self.divider_drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_divider_drag(cx);
            return;
        }
        let pos = match drag.orient {
            SplitOrient::Beside => f32::from(event.position.x),
            SplitOrient::Stacked => f32::from(event.position.y),
        };
        let ratio = drag_ratio(
            drag.baseline,
            pos - drag.origin,
            drag.available,
            min_ratio_for(drag.orient, drag.available),
        );
        let (session_id, term_window_id, path) = (
            drag.session_id.clone(),
            drag.term_window_id.clone(),
            drag.path.clone(),
        );
        if let Some(drag) = &mut self.divider_drag {
            drag.moved = true;
        }
        self.write_ratio(&session_id, &term_window_id, &path, ratio, cx);
    }

    /// Root-level up: ends the drag, committing what it settled at.
    fn on_root_mouse_up(&mut self, _e: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.divider_drag.is_some() {
            self.end_divider_drag(cx);
        }
    }

    /// End an in-flight drag, persisting the settled ratio iff the press
    /// actually moved — a zero-movement click on a divider changes nothing.
    fn end_divider_drag(&mut self, cx: &mut Context<Self>) {
        let moved = self.divider_drag.take().is_some_and(|d| d.moved);
        if moved {
            self.commit_divider_drag(cx);
        }
    }

    /// Persist the layout after a divider settled (the normal debounced store
    /// upsert; per-move writes deliberately do not go through here).
    fn commit_divider_drag(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, _| ws.save_to_store());
    }

    /// Write one split's ratio and repaint. A path that no longer names a split
    /// — a pane exited mid-drag and collapsed it — writes nothing.
    ///
    /// Every write goes through `mutate_session`, so the model's did-mutate
    /// signal rides along per move; the store's own debounce collapses the
    /// resulting saves, and the drag's end commits explicitly.
    fn write_ratio(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        path: &[Side],
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        let term_window_id = term_window_id.to_string();
        let path = path.to_vec();
        self.state.update(cx, |ws, wcx| {
            let mut wrote = false;
            ws.workspace.mutate_session(session_id, |session| {
                if let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id)
                {
                    wrote = term_window.layout.set_ratio_at(&path, ratio);
                }
            });
            if wrote {
                wcx.notify();
            }
        });
        cx.notify();
    }

    /// The pane area's last painted rect, normalized to the origin — the px
    /// context the drag's ratio arithmetic and the P6 clamp work in (only
    /// extents matter, never the window-space offset).
    fn pane_content_rect(&self, cx: &App) -> Option<PaneRect> {
        let (w, h) = self.state.read(cx).pane_content_size()?;
        (w > 0.0 && h > 0.0).then(|| PaneRect::new(0.0, 0.0, w, h))
    }
}

/// The four corner-tick brackets of the focused pane in a split pill, in the
/// user's accent at full strength — small enough that they never need dimming.
/// Absolute children of the pane's clipped `relative` root, so they sit exactly
/// on the pane's corners and can never collide with a divider or a neighbour.
fn corner_ticks(accent: Srgba) -> Vec<AnyElement> {
    let color = Rgba {
        r: accent.r,
        g: accent.g,
        b: accent.b,
        a: 1.0,
    };
    let tick = move || {
        div()
            .absolute()
            .w(px(PANE_TICK_SIZE))
            .h(px(PANE_TICK_SIZE))
            .border_color(color)
    };
    vec![
        tick()
            .top(px(0.0))
            .left(px(0.0))
            .border_t_2()
            .border_l_2()
            .rounded_tl(px(PANE_TICK_RADIUS))
            .into_any_element(),
        tick()
            .top(px(0.0))
            .right(px(0.0))
            .border_t_2()
            .border_r_2()
            .rounded_tr(px(PANE_TICK_RADIUS))
            .into_any_element(),
        tick()
            .bottom(px(0.0))
            .left(px(0.0))
            .border_b_2()
            .border_l_2()
            .rounded_bl(px(PANE_TICK_RADIUS))
            .into_any_element(),
        tick()
            .bottom(px(0.0))
            .right(px(0.0))
            .border_b_2()
            .border_r_2()
            .rounded_br(px(PANE_TICK_RADIUS))
            .into_any_element(),
    ]
}

/// The window-host's fill when the active window has no live session yet (a
/// model-only Claude window, or a terminal window an instant before its deferred
/// spawn caches a handle) — the shipped dark backdrop, matching the terminal
/// theme's background so the swap to a real grid is seamless.
fn window_placeholder() -> impl IntoElement {
    div().size_full().bg(rgb(0x11141b))
}

// ---------------------------------------------------------------------------
// Prod-parity terminal content insets (M7.8 feel-check Bug 2). The effective
// prod inset is the SUM of Swift's app-level `mainContent` padding
// (`Sources/Nice/Views/AppShellView.swift`) and the SwiftTerm view's own cell-
// area geometry (the fork at `SwiftTerm/Sources/SwiftTerm`): SwiftTerm draws
// glyphs from x=0 / no internal padding, EXCEPT that it reserves the scroller
// width on the trailing edge (`MacTerminalView.getEffectiveWidth = width −
// scrollerWidth`). `nice-term-view` likewise paints from its bounds origin
// with zero internal inset (`TERMINAL_BOTTOM_GAP == 0` mirrors prod's
// `TerminalContainerView.bottomInset == 0`), so the whole prod inset is
// applied here, app-side, around the hosted window. Padding shrinks the
// `TerminalView`'s painted bounds, so the grid refit (auto_refit → 200 ms
// debounce → TIOCSWINSZ) re-fits cols/rows to the inset content area — the
// same path any window resize takes. The uncovered gap composites over the
// window backing layer's terminal-theme fill (see `AppShellView::render`), so
// it paints as terminal background exactly like Swift's
// `.padding(…).background(terminalBackgroundColor)`.
// ---------------------------------------------------------------------------

/// Leading gap between the sidebar card's trailing edge and the first glyph
/// column — Swift `mainContent`'s `.padding(.leading, 20)`
/// (`AppShellView.swift:1063`; SwiftTerm adds 0).
const CONTENT_INSET_LEADING: f32 = 20.0;

/// Top gap below the toolbar band — Swift `.padding(.top, 12)`
/// (`AppShellView.swift:1054`). The top-anchored grid (T4 revised) starts row 0
/// flush below this, so the visual top gap is exactly this constant at every
/// window height — a deliberate divergence from prod, whose bottom-anchored
/// grid parks the sub-row remainder here (see `nice-term-view`'s element doc).
const CONTENT_INSET_TOP: f32 = 12.0;

/// Bottom gap under the last grid row — Swift `.padding(.bottom, 9)`
/// (`AppShellView.swift:1062`; `TerminalContainerView.bottomInset` is 0). The
/// sub-row remainder (0..cell_h) of the top-anchored grid parks above this, so
/// the visual bottom gap is this constant plus the remainder.
const CONTENT_INSET_BOTTOM: f32 = 9.0;

/// Trailing gap to the window's right edge. Swift adds NO trailing padding,
/// but SwiftTerm reserves the overlay scroller's width out of the cell area
/// (`MacTerminalView.swift:696-699`; `TerminalHost.swift:85` sets
/// `.overlay`), so prod text ends ≥ `NSScroller.scrollerWidth(for: .regular,
/// scrollerStyle: .overlay)` = 17.0 from the edge (measured on macOS 26).
const CONTENT_INSET_TRAILING: f32 = 17.0;

// ---------------------------------------------------------------------------
// Window-level backing layer — the opaque base every per-window surface
// composites over. The 2026-07 restyle removed the separate opaque chrome band +
// 1pt rule (the fill-less titlebar), so only the full-height terminal-theme fill
// remains. The pure color resolver is split out so it is unit-testable off-view
// (same placement rule as the host logic above).
// ---------------------------------------------------------------------------

/// The window-body backing fill — the ACTIVE terminal theme's background bled
/// across the WHOLE window (top edge included, now that the titlebar paints no
/// fill) so the sidebar card's gutter and every other unpainted region match the
/// terminal instead of revealing black.
///
/// Restyle plan 3: this is THE single translucent surface. `opacity` (0.55–1.0)
/// is applied as the fill alpha; the window is made genuinely non-opaque via
/// `WindowBackgroundAppearance` (see `crate::app::build_window_root`), so at
/// `opacity < 1.0` the OS blur / desktop shows through here. The terminal grid
/// SKIPS its own default-bg fill when translucent, so this backing is the ONLY
/// default-background surface (no double-applied alpha under the grid). At
/// `opacity == 1.0` the fill is fully opaque, identical to the pre-restyle window.
///
/// Restyle plan 06: `pub(crate)` so the Settings window
/// ([`crate::settings::root`]) paints THE SAME single translucent surface (the
/// mock's `.window` background = terminal bg × window alpha), visually matching
/// the main window.
pub(crate) fn terminal_backing_color(theme: &TerminalTheme, opacity: f32) -> Rgba {
    let base = rgb(theme.background.to_u32());
    Rgba {
        a: opacity.clamp(0.0, 1.0),
        ..base
    }
}

// ---------------------------------------------------------------------------
// Pure host logic (target resolution + cache eviction) — extracted so it is
// unit-testable off-view (`WindowHostView` lives in the `nice` BINARY, which
// `nice-itests` cannot import, so the render-level placeholder→TerminalView swap
// is asserted in the `claude-lifecycle` scenario and the pure logic here). The
// render path calls exactly these, so the tests cover the code the window runs.
// ---------------------------------------------------------------------------

/// The active `(session_id, term_window_id)` the host should follow — the active session's
/// active window, or `None` when the model has no active session / that session has no
/// active window. Whether a live session (and thus a `TerminalView`) exists for it
/// is a separate question the render answers via
/// [`PtyManager::term_window_handle`](crate::pty_manager::PtyManager::term_window_handle):
/// a model-only Claude window resolves as the target here but has no handle, so the
/// render shows the [`window_placeholder`] until its spawn/promotion caches one.
fn active_window_target(model: &nice_model::WorkspaceModel) -> Option<(String, String, String)> {
    let session = model.active_session_id()?;
    let session = model.session_for(session)?;
    let term_window_id = session.active_window_id.clone()?;
    let term_window = session.windows.iter().find(|w| w.id == term_window_id)?;
    Some((
        session.id.clone(),
        term_window_id,
        term_window.effective_pane_id(),
    ))
}

/// Every PANE id present in the model right now — the live set the cache is
/// pruned against (a cached view whose pane id is absent has left the model,
/// either with its pill or on its own).
fn model_pane_ids(model: &nice_model::WorkspaceModel) -> HashSet<String> {
    let mut all = HashSet::new();
    for project in &model.projects {
        for session in &project.sessions {
            for term_window in &session.windows {
                for pane in term_window.layout.leaves() {
                    all.insert(pane.id.clone());
                }
            }
        }
    }
    all
}

/// The cached pane ids to evict — those no longer present in `live` (the pane
/// left the model). Returns owned ids so the caller can mutate the cache without
/// aliasing its key iterator.
fn stale_cache_ids<'a>(
    cached: impl IntoIterator<Item = &'a String>,
    live: &HashSet<String>,
) -> Vec<String> {
    cached
        .into_iter()
        .filter(|id| !live.contains(*id))
        .cloned()
        .collect()
}

/// The px a split node's ratio divides: the node's own extent along its axis
/// minus the divider it reserves. `None` when the path names no split (a stale
/// drag path after a pane exited, or a leaf).
///
/// Derived from [`PaneLayout::leaf_rects`] rather than re-deriving the ratio
/// arithmetic: the node's extent is the bounding box of its leaves' rects, so
/// the drag divides exactly the px the paint laid out.
///
/// `pub(crate)` because the keyboard resize (`⌃⌥⌘hjkl`, `crate::keymap`) turns a
/// px step into a ratio delta against this exact denominator — one arithmetic
/// for the divider the mouse drags and the divider the keyboard walks.
pub(crate) fn split_available_px(layout: &PaneLayout, path: &[Side], content: PaneRect) -> Option<f32> {
    let node = layout.node_at(path)?;
    let PaneLayout::Split { orient, .. } = node else {
        return None;
    };
    let ids: HashSet<&str> = node.leaves().into_iter().map(|p| p.id.as_str()).collect();
    let rects = layout.leaf_rects(content, PANE_DIVIDER_PX);
    let mut extent: Option<(f32, f32)> = None;
    for (id, rect) in &rects {
        if !ids.contains(id.as_str()) {
            continue;
        }
        let (lo, hi) = match orient {
            SplitOrient::Beside => (rect.min_x(), rect.max_x()),
            SplitOrient::Stacked => (rect.min_y(), rect.max_y()),
        };
        extent = Some(match extent {
            Some((min, max)) => (min.min(lo), max.max(hi)),
            None => (lo, hi),
        });
    }
    let (min, max) = extent?;
    Some((max - min - PANE_DIVIDER_PX).max(0.0))
}

/// The smallest share either side of a split may hold, given the px its ratio
/// divides — P6's minimum pane size expressed as a ratio.
///
/// Never above 0.5 (so the band never inverts) and never below the model's own
/// [`RATIO_MIN`]; on a pane area too small to honour the minimum at all, both
/// sides simply get the model band and the panes shrink together rather than
/// the drag becoming unusable.
///
/// `pub(crate)` for the same reason as [`split_available_px`]: the keyboard
/// resize clamps against the identical band.
pub(crate) fn min_ratio_for(orient: SplitOrient, available: f32) -> f32 {
    let min_px = match orient {
        SplitOrient::Beside => PANE_MIN_WIDTH,
        SplitOrient::Stacked => PANE_MIN_HEIGHT,
    };
    if available <= 0.0 {
        return RATIO_MIN;
    }
    (min_px / available).clamp(RATIO_MIN, 0.5)
}

/// Where a divider drag has moved the ratio: the baseline plus the pointer's px
/// travel expressed as a share of the px the ratio divides, clamped into the
/// P6 band. A zero (or negative) `available` leaves the baseline untouched.
fn drag_ratio(baseline: f32, delta_px: f32, available: f32, min_ratio: f32) -> f32 {
    if available <= 0.0 {
        return baseline;
    }
    let lo = min_ratio.clamp(RATIO_MIN, 0.5);
    let hi = (1.0 - lo).min(RATIO_MAX);
    (baseline + delta_px / available).clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    //! Pure host-logic tests (target resolution + eviction). The render-level
    //! placeholder→`TerminalView` swap (a model-only Claude window shows the
    //! placeholder; its spawn/promotion swaps to a cached view) is asserted in the
    //! `claude-lifecycle` scenario — `WindowHostView` needs a live gpui window a plain
    //! `#[test]` can't build, per the placement rule.
    use super::{
        active_window_target, drag_ratio, min_ratio_for, model_pane_ids, split_available_px,
        stale_cache_ids, PANE_DIVIDER_PX, PANE_MIN_WIDTH,
    };
    use nice_model::pane_layout::{RATIO_MIN, Pane, PaneLayout, PaneRect, Side, SplitOrient};
    use nice_model::{Session, TermWindow, TermWindowKind, WorkspaceModel};
    use std::collections::HashSet;

    /// A model with a non-Terminals project holding one `[Claude, Terminal 1]`
    /// session (Claude focused + active), returning `(model, claude_id, term_id)`.
    fn model_with_claude_session() -> (WorkspaceModel, String, String) {
        let mut model = WorkspaceModel::new("/home/u");
        model.ensure_project("p", "P", "/home/u/proj");
        let claude_id = "t1-claude".to_string();
        let term_id = "t1-t1".to_string();
        let mut session = Session::new("t1", "New session", "/home/u/proj");
        session.windows = vec![
            TermWindow::new(&claude_id, "Claude", TermWindowKind::Claude),
            TermWindow::new(&term_id, "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(claude_id.clone());
        let pi = model.projects.iter().position(|p| p.id == "p").unwrap();
        model.projects[pi].sessions.push(session);
        model.select_session("t1");
        (model, claude_id, term_id)
    }

    #[test]
    fn active_window_target_resolves_the_active_sessions_active_window_and_pane() {
        let (model, claude_id, _term) = model_with_claude_session();
        assert_eq!(
            active_window_target(&model),
            // A never-split pill's sole pane carries the pill's own id.
            Some(("t1".to_string(), claude_id.clone(), claude_id)),
            "the host follows the active session's active window's focused pane (a model-only \
             Claude window still resolves as the target — the render shows the placeholder \
             until a handle exists)"
        );
    }

    #[test]
    fn active_window_target_follows_the_focused_pane_of_a_split_pill() {
        let (mut model, claude_id, _term) = model_with_claude_session();
        model.mutate_session("t1", |session| {
            let window = &mut session.windows[0];
            window
                .layout
                .split(&claude_id, SplitOrient::Beside, Pane::new("p-shell", TermWindowKind::Terminal));
            window.active_pane_id = "p-shell".into();
        });
        assert_eq!(
            active_window_target(&model).map(|(_, _, pane)| pane),
            Some("p-shell".to_string())
        );
    }

    #[test]
    fn model_pane_ids_collects_every_pane_across_projects_and_sessions() {
        let (mut model, claude_id, term_id) = model_with_claude_session();
        model.mutate_session("t1", |session| {
            session.windows[0].layout.split(
                &claude_id,
                SplitOrient::Beside,
                Pane::new("p-shell", TermWindowKind::Terminal),
            );
        });
        let ids = model_pane_ids(&model);
        // The seeded Terminals Main window + the claude session's two windows,
        // and the split pane that exists only inside a tree.
        assert!(ids.contains(&claude_id));
        assert!(ids.contains(&term_id));
        assert!(ids.contains("p-shell"), "a split pane is a cache key of its own");
        let main_window = model
            .session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID)
            .unwrap()
            .windows[0]
            .id
            .clone();
        assert!(ids.contains(&main_window), "the pinned Main session's window is included");
    }

    #[test]
    fn stale_cache_ids_evicts_panes_that_left_the_model() {
        // Cache holds three panes; two are still live, one has left the model.
        let cached: Vec<String> = vec!["a".into(), "gone".into(), "b".into()];
        let live: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let stale = stale_cache_ids(cached.iter(), &live);
        assert_eq!(stale, vec!["gone".to_string()], "only the departed pane is evicted");
    }

    #[test]
    fn stale_cache_ids_evicts_nothing_when_all_cached_panes_are_live() {
        let cached: Vec<String> = vec!["a".into(), "b".into()];
        let live: HashSet<String> = ["a".to_string(), "b".to_string(), "c".to_string()]
            .into_iter()
            .collect();
        assert!(stale_cache_ids(cached.iter(), &live).is_empty());
    }

    // MARK: - divider drag arithmetic

    /// `Beside{ a, Stacked{ b, c } }` — the mixed tree the drags run against.
    fn split_tree() -> PaneLayout {
        let mut layout = PaneLayout::single(Pane::new("a", TermWindowKind::Terminal));
        layout.split("a", SplitOrient::Beside, Pane::new("b", TermWindowKind::Terminal));
        layout.split("b", SplitOrient::Stacked, Pane::new("c", TermWindowKind::Terminal));
        layout
    }

    #[test]
    fn split_available_px_is_the_nodes_own_extent_minus_its_divider() {
        let layout = split_tree();
        let content = PaneRect::new(0.0, 0.0, 1006.0, 606.0);

        // The root Beside split divides the full width.
        assert_eq!(
            split_available_px(&layout, &[], content),
            Some(1006.0 - PANE_DIVIDER_PX)
        );
        // The nested Stacked split divides only the right column's HEIGHT.
        assert_eq!(
            split_available_px(&layout, &[Side::Second], content),
            Some(606.0 - PANE_DIVIDER_PX)
        );
        // A leaf, and a path that ran off the tree (a pane exited mid-drag),
        // have no ratio to divide.
        assert_eq!(split_available_px(&layout, &[Side::First], content), None);
        assert_eq!(
            split_available_px(&layout, &[Side::First, Side::First], content),
            None
        );
    }

    #[test]
    fn split_available_px_matches_what_the_paint_laid_out() {
        // The drag denominator must be the px the ratio actually divides: move
        // the divider by exactly `available/4` and the leaf rects must follow.
        let mut layout = split_tree();
        let content = PaneRect::new(0.0, 0.0, 1006.0, 606.0);
        let available = split_available_px(&layout, &[], content).unwrap();
        layout.set_ratio_at(&[], 0.75);
        let rects = layout.leaf_rects(content, PANE_DIVIDER_PX);
        let a = rects.iter().find(|(id, _)| id == "a").unwrap().1;
        assert!((a.width - available * 0.75).abs() < 1e-3);
    }

    #[test]
    fn min_ratio_for_expresses_the_pane_minimum_as_a_share() {
        // 1200 px wide: the 120 px minimum is a tenth of the split.
        assert!((min_ratio_for(SplitOrient::Beside, 1200.0) - PANE_MIN_WIDTH / 1200.0).abs() < 1e-6);
        // A split narrower than two minimums can't honour them — the band
        // collapses to half rather than inverting.
        assert_eq!(min_ratio_for(SplitOrient::Beside, 100.0), 0.5);
        // Never painted / degenerate: the model's own band.
        assert_eq!(min_ratio_for(SplitOrient::Beside, 0.0), RATIO_MIN);
    }

    #[test]
    fn drag_ratio_tracks_the_pointer_and_clamps_at_the_minimum() {
        let available = 1000.0;
        let min = min_ratio_for(SplitOrient::Beside, available);
        // A quarter-width drag right moves the ratio by a quarter.
        assert!((drag_ratio(0.5, 250.0, available, min) - 0.75).abs() < 1e-6);
        // Dragging far past the edge pins at the minimum, never below it.
        assert!((drag_ratio(0.5, -900.0, available, min) - min).abs() < 1e-6);
        assert!((drag_ratio(0.5, 900.0, available, min) - (1.0 - min)).abs() < 1e-6);
        // No painted extent: nothing moves.
        assert_eq!(drag_ratio(0.42, 250.0, 0.0, min), 0.42);
    }

    #[test]
    fn terminal_backing_is_the_active_theme_background_at_the_given_opacity() {
        let light = nice_term_view::TerminalTheme::nice_default_light();
        // Opacity 1.0: the pre-restyle opaque backing (unchanged look).
        let opaque = super::terminal_backing_color(&light, 1.0);
        assert_eq!(opaque.a, 1.0, "opacity 1.0 is a fully opaque backing");
        assert_eq!(
            (opaque.r, opaque.g, opaque.b),
            (1.0, 252.0 / 255.0, 252.0 / 255.0),
            "niceDefaultLight background (0xfffcfc)"
        );
        // Restyle plan 3: a translucent opacity is applied as the fill alpha, the
        // rgb channels unchanged, so the OS blur / desktop shows through.
        let translucent = super::terminal_backing_color(&light, 0.8);
        assert_eq!(translucent.a, 0.8, "opacity is carried as the fill alpha");
        assert_eq!(
            (translucent.r, translucent.g, translucent.b),
            (opaque.r, opaque.g, opaque.b),
            "opacity leaves the theme rgb channels untouched"
        );
    }
}

impl Render for WindowHostView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot the active target, the full set of live pane ids, and the
        // active pill's tree up front (via the pure resolvers so the
        // target/eviction logic is unit-testable off-view), then drop the state
        // borrow before any mutation.
        #[allow(clippy::type_complexity)]
        let (active, all_pane_ids, tree): (
            Option<(String, String, String)>,
            HashSet<String>,
            Option<(PaneLayout, bool)>,
        ) = {
            let ws = self.state.read(cx);
            let active = active_window_target(&ws.workspace);
            let tree = active.as_ref().and_then(|(session, term_window, _)| {
                ws.workspace
                    .session_for(session)?
                    .windows
                    .iter()
                    .find(|w| &w.id == term_window)
                    .map(|w| (w.layout.clone(), w.zoomed))
            });
            (active, model_pane_ids(&ws.workspace), tree)
        };

        // R15 subscription lift: subscribe any freshly-spawned PANE (the Main window,
        // deferred terminals `activate_term_window` forks below, split panes, and
        // Claude windows the socket / sidebar seams spawn) to
        // `route_terminal_event`, so OSC titles / cwd / exits reach the model in the
        // SHIPPED window — not just the `session-lifecycle` scenario. Idempotent
        // (subscribe-once dedupe on the window state), so it is safe to sweep on
        // every render.
        self.state
            .update(cx, |ws, wcx| ws.subscribe_spawned_windows(wcx));

        // Hand the keymap layer the px context it cannot measure for itself: the
        // pane area's size as of the last paint (P6 refusals, the resize step's
        // px → ratio conversion, and this view's own divider drag all read it).
        let painted = self.pane_bounds.get().map(|b| {
            (
                f32::from(b.size.width),
                f32::from(b.size.height),
            )
        });
        self.state.update(cx, |ws, _| ws.set_pane_content_size(painted));

        // Drop cached views for panes that left the model (the PROTECTED
        // "dropped when the window leaves the model", now per pane — a pane can
        // also leave on its own when its pty exits out of a multi-pane pill).
        // The pty session lives on in the `PtyManager` until window teardown
        // reaps it (SIGHUP→SIGKILL).
        for stale in stale_cache_ids(self.cache.keys(), &all_pane_ids) {
            self.cache.remove(&stale);
        }

        // Phase 3: an open search bar whose pane lost focus, died, or left copy
        // mode closes here — before the tree is built, so it is never painted
        // stale. The keyboard follows it back to the terminal in the focus block
        // below (a bar that closed while focused would otherwise leave key focus
        // on a handle nothing renders).
        let bar_closed = self.sweep_stale_search_bar(active.as_ref(), cx);

        // On a PILL switch, run the sole activation path (which now spawns every
        // un-live leaf of the pill's tree, not just one).
        let window_changed = active.as_ref().map(|(s, w, _)| (s.clone(), w.clone()))
            != self.last_active.as_ref().map(|(s, w, _)| (s.clone(), w.clone()));
        let activation_changed = active != self.last_active;
        if activation_changed {
            self.last_active = active.clone();
        }
        if window_changed {
            if let Some((session, term_window, _)) = active.clone() {
                let state = self.state.clone();
                state.update(cx, |ws, wcx| {
                    // Ensure the active session has a session container so R13's
                    // deferred-spawn precondition holds (Swift makes the session
                    // when a session is first shown). Idempotent; not a spawn itself —
                    // spawning still flows through `activate_term_window`.
                    ws.ptys.register_session_pty(&session);
                    // R18 (L3): a restored Claude active window lazy-spawns its
                    // deferred-resume shell here — thread the window's `--settings`
                    // provider in before the workspace/ptys split borrows `ws`.
                    let settings = ws.claude_settings_path_provider();
                    let workspace = &mut ws.workspace;
                    let ptys = &mut ws.ptys;
                    ptys.activate_term_window(workspace, &session, &term_window, settings.as_deref(), wcx);
                    // Subscribe whatever that just spawned NOW, in this same update:
                    // the sweep above ran BEFORE the spawn, so deferring to the next
                    // render would leave the fresh window unsubscribed for a frame.
                    // The L3 restore arm reached through here arms an app-typed pane's
                    // prefill, and a readiness OSC 7 that beats the subscription is
                    // dropped outright — see `subscribe_spawned_windows`' ordering
                    // rule. Idempotent: a `HashSet` lookup per live window.
                    ws.subscribe_spawned_windows(wcx);
                });
            }
        }

        // Which panes are painted this pass: every leaf, or only the focused one
        // while zoomed (P4 — every pty stays live underneath either way).
        let visible: Vec<Pane> = match (&active, &tree) {
            (Some((_, _, active_pane)), Some((layout, zoomed))) => {
                match zoomed.then(|| layout.pane(active_pane)).flatten() {
                    Some(pane) => vec![pane.clone()],
                    None => layout.leaves().into_iter().cloned().collect(),
                }
            }
            _ => Vec::new(),
        };

        // Fill the view cache for every painted pane. A pane mounted while it is
        // NOT the focused one skips its first-render focus grab, so the focus
        // move below decides who ends up with the keyboard.
        let mut mounted_new = false;
        if let Some((session, term_window, active_pane)) = &active {
            for pane in &visible {
                if self.cache.contains_key(&pane.id) {
                    continue;
                }
                let handle = self
                    .state
                    .read(cx)
                    .ptys
                    .pane_handle(session, term_window, &pane.id);
                let Some(handle) = handle else {
                    continue;
                };
                let view = self.make_terminal_view(handle, &pane.id == active_pane, cx);
                self.cache.insert(pane.id.clone(), view);
                mounted_new = true;
            }
        }

        // Re-point the demand-present kick at EVERY painted pane's handle when
        // that set changes, so a background pane's damage still kicks an
        // occluded window (`install_present_kick` is occlusion-gated and cheap).
        let visible_ids: HashSet<String> = visible.iter().map(|p| p.id.clone()).collect();
        if visible_ids != self.kicked {
            // Record only the panes that actually took a kick: a pane still
            // waiting on its pty (the placeholder) must be retried on a later
            // render, so it may not count as covered here.
            let mut installed = HashSet::new();
            if let Some((session, term_window, _)) = &active {
                for pane in &visible {
                    let handle = self
                        .state
                        .read(cx)
                        .ptys
                        .pane_handle(session, term_window, &pane.id);
                    if let Some(handle) = handle {
                        crate::app::install_present_kick(&handle, window.window_handle(), cx);
                        installed.insert(pane.id.clone());
                    }
                }
            }
            self.kicked = installed;
        }

        // Mount the active pill's tree — one leaf for a never-split pill (the
        // pre-splits render), else the recursive split layout. A pane whose pty
        // has cached no handle yet shows the placeholder in its own rect.
        let content: AnyElement = match (&active, &tree) {
            (Some((session, term_window, active_pane)), Some((layout, zoomed))) => {
                let ctx = TreeCtx {
                    session_id: session.clone(),
                    term_window_id: term_window.clone(),
                    active_pane_id: active_pane.clone(),
                    // Zoom paints one pane full-size: no divider, no affordance.
                    multi: layout.leaf_count() > 1 && !zoomed,
                    accent: self.accent,
                };
                match zoomed.then(|| layout.pane(active_pane)).flatten() {
                    Some(pane) => self.leaf_element(pane, &ctx, cx),
                    None => self.render_node(layout, Vec::new(), &ctx, cx),
                }
            }
            _ => window_placeholder().into_any_element(),
        };

        // Focus follows activation (M2 Item D): the terminal's per-frame render
        // grab is gone (focus-once in `TerminalView`), so the host moves key
        // focus to the focused pane's terminal on every activation change —
        // window open, ⌘T, pill/row click, window-step, close-refocus, and every
        // pane-level focus move. It also runs after any pass that mounted new
        // views, so restoring a 3-pane pill focuses `active_pane_id` rather than
        // whichever leaf rendered last. Runs after the cache fill above so a
        // just-activated terminal window (spawned synchronously by
        // `activate_term_window`) is focusable this same render. A pane with no
        // hosted view (Claude placeholder) has nothing to focus.
        if activation_changed || mounted_new || bar_closed {
            if let Some((_, _, pane)) = &active {
                if let Some(view) = self.cache.get(pane) {
                    let fh = view.read(cx).focus_handle_ref().clone();
                    window.focus(&fh, cx);
                }
            }
        }

        // The divider drag is tracked at the ROOT (the sidebar resize-handle
        // pattern): it survives the pointer leaving the 6 px zone, and a move
        // arriving with the button already up is treated as the mouse-up that
        // happened outside the window.
        let sink = self.pane_bounds.clone();
        // Prod-parity content insets around the hosted window (constants above).
        // Applied to placeholder and pane tree alike — Swift pads both branches
        // of `mainContent` (the placeholder branch keeps its leading pad).
        div()
            .size_full()
            .pl(px(CONTENT_INSET_LEADING))
            .pt(px(CONTENT_INSET_TOP))
            .pb(px(CONTENT_INSET_BOTTOM))
            .pr(px(CONTENT_INSET_TRAILING))
            .on_mouse_move(cx.listener(Self::on_root_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_root_mouse_up))
            .child(
                // The pane area proper: its own box is exactly what the tree is
                // laid out in, so the bounds probe measures the region the model
                // geometry divides (an inset-0 probe on the padded root would
                // report the padding box instead).
                div()
                    .size_full()
                    .relative()
                    .child(
                        canvas(
                            move |bounds, _window, _cx| sink.set(Some(bounds)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(content),
            )
    }
}
