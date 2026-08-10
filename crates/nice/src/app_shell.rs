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
//! [`WindowHostView`] maps the active `(session_id, term_window_id)` →
//! [`PtyManager::term_window_handle`] → a per-window [`TerminalView`] created lazily
//! on first activation, cached per window id, and dropped when the window leaves the
//! model. It uses the shared theme / accent / font exactly as
//! `open_managed_window` does today. Activation flows **only** through
//! [`PtyManager::activate_term_window`] (R13's deferred-spawn — no view-side spawn
//! shortcuts); this host then focuses the newly-active window's terminal on the
//! same activation render, and re-points the demand-present kick to the active
//! window's handle on every switch.

// Mounted by `crate::app::build_window_root`; the AX-anchor label constants are a
// deliberately-exported contract (the shipped-surface assertion hooks, §6) whose
// asserting scenario lands in the next slice.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgb, AnyElement, App, Context, Entity, FocusHandle, Render, Rgba,
    Subscription, Window,
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
    /// Per-window [`TerminalView`] cache (`term_window_id -> view`). Created lazily on
    /// first activation; an entry is dropped when its window leaves the model.
    cache: HashMap<String, Entity<TerminalView>>,
    /// The `(session_id, term_window_id)` hosted as of the last render — a change drives the
    /// single [`PtyManager::activate_term_window`] activation + a present-kick
    /// re-point.
    last_active: Option<(String, String)>,
}

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
        }
    }

    /// Build a fresh [`TerminalView`] over `handle` with the shared theme / accent
    /// / font and the same platform injections `open_managed_window` wires (the
    /// keyCode side-channel, the raw-image drop fallback, the ⌘+click URL opener,
    /// the App-Nap-safe launch deadline) — the sole objc2 crossings, injected
    /// here so `nice-term-view` stays foreign-code-free.
    fn make_terminal_view(
        &self,
        handle: Entity<TerminalSessionHandle>,
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

    /// Move key focus to the active window's hosted terminal, if any — the
    /// app-side focus-routing seam (M2 Item D). The toolbar / sidebar call it
    /// after an inline-rename commit/cancel and on context-menu dismissal, and
    /// the chrome roots bounce stray chrome-click focus back through it. A
    /// no-op when the active window has no hosted view (a model-only Claude window).
    pub(crate) fn focus_active_terminal(&self, window: &mut Window, cx: &mut App) {
        if let Some(fh) = self.active_terminal_focus_handle(cx) {
            window.focus(&fh, cx);
        }
    }

    /// The active window's terminal focus handle, if a view is hosted for it —
    /// the `app-shell` scenario's "focus returned to the terminal" read.
    pub(crate) fn active_terminal_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let (_, term_window) = self.last_active.as_ref()?;
        let view = self.cache.get(term_window)?;
        Some(view.read(cx).focus_handle_ref().clone())
    }

    /// The cached [`TerminalView`] hosting `term_window_id`, if the host has built one —
    /// the `claude-lifecycle` /branch-overlay leg's read: it activates the deferred
    /// branch parent, then asserts that window's view never flashed the stray
    /// "Launching…" overlay (its `OutputStarted` fired while it had no view).
    pub(crate) fn scenario_terminal_for(&self, term_window_id: &str) -> Option<Entity<TerminalView>> {
        self.cache.get(term_window_id).cloned()
    }
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
fn active_window_target(model: &nice_model::WorkspaceModel) -> Option<(String, String)> {
    let session = model.active_session_id()?;
    let term_window = model.session_for(session)?.active_window_id.clone()?;
    Some((session.to_string(), term_window))
}

/// Every window id present in the model right now — the live set the cache is
/// pruned against (a cached view whose window id is absent has left the model).
fn model_window_ids(model: &nice_model::WorkspaceModel) -> HashSet<String> {
    let mut all = HashSet::new();
    for project in &model.projects {
        for session in &project.sessions {
            for term_window in &session.windows {
                all.insert(term_window.id.clone());
            }
        }
    }
    all
}

/// The cached window ids to evict — those no longer present in `live` (the window
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

#[cfg(test)]
mod tests {
    //! Pure host-logic tests (target resolution + eviction). The render-level
    //! placeholder→`TerminalView` swap (a model-only Claude window shows the
    //! placeholder; its spawn/promotion swaps to a cached view) is asserted in the
    //! `claude-lifecycle` scenario — `WindowHostView` needs a live gpui window a plain
    //! `#[test]` can't build, per the placement rule.
    use super::{active_window_target, model_window_ids, stale_cache_ids};
    use nice_model::{TermWindow, TermWindowKind, Session, WorkspaceModel};
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
    fn active_window_target_resolves_the_active_sessions_active_window() {
        let (model, claude_id, _term) = model_with_claude_session();
        assert_eq!(
            active_window_target(&model),
            Some(("t1".to_string(), claude_id)),
            "the host follows the active session's active window (a model-only Claude window \
             still resolves as the target — the render shows the placeholder until a \
             handle exists)"
        );
    }

    #[test]
    fn model_window_ids_collects_every_window_across_projects_and_sessions() {
        let (model, claude_id, term_id) = model_with_claude_session();
        let ids = model_window_ids(&model);
        // The seeded Terminals Main window + the claude session's two windows.
        assert!(ids.contains(&claude_id));
        assert!(ids.contains(&term_id));
        let main_window = model
            .session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID)
            .unwrap()
            .windows[0]
            .id
            .clone();
        assert!(ids.contains(&main_window), "the pinned Main session's window is included");
    }

    #[test]
    fn stale_cache_ids_evicts_windows_that_left_the_model() {
        // Cache holds three windows; two are still live, one has left the model.
        let cached: Vec<String> = vec!["a".into(), "gone".into(), "b".into()];
        let live: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let stale = stale_cache_ids(cached.iter(), &live);
        assert_eq!(stale, vec!["gone".to_string()], "only the departed window is evicted");
    }

    #[test]
    fn stale_cache_ids_evicts_nothing_when_all_cached_windows_are_live() {
        let cached: Vec<String> = vec!["a".into(), "b".into()];
        let live: HashSet<String> = ["a".to_string(), "b".to_string(), "c".to_string()]
            .into_iter()
            .collect();
        assert!(stale_cache_ids(cached.iter(), &live).is_empty());
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
        // Snapshot the active window + the full set of live window ids up front (via the
        // pure resolvers so the target/eviction logic is unit-testable off-view),
        // then drop the state borrow before any mutation.
        let (active, all_window_ids): (Option<(String, String)>, HashSet<String>) = {
            let ws = self.state.read(cx);
            (active_window_target(&ws.workspace), model_window_ids(&ws.workspace))
        };

        // R15 subscription lift: subscribe any freshly-spawned window (the Main window,
        // deferred terminals `activate_term_window` forks below, and Claude windows the socket
        // / sidebar seams spawn) to `route_terminal_event`, so OSC titles / cwd / exits
        // reach the model in the SHIPPED window — not just the `session-lifecycle`
        // scenario. Idempotent (subscribe-once dedupe on the window state), so it is
        // safe to sweep on every render.
        self.state
            .update(cx, |ws, wcx| ws.subscribe_spawned_windows(wcx));

        // Drop cached views for windows that left the model (the PROTECTED
        // "dropped when the window leaves the model"). The window's pty session lives
        // on in the `PtyManager` until window teardown reaps it (SIGHUP→
        // SIGKILL); wiring the UI close actions to the R13 dissolve cascade is
        // out of this composition slice.
        for stale in stale_cache_ids(self.cache.keys(), &all_window_ids) {
            self.cache.remove(&stale);
        }

        // On a switch, run the sole activation path + re-point the present kick.
        let activation_changed = active != self.last_active;
        if activation_changed {
            self.last_active = active.clone();
            if let Some((session, term_window)) = active.clone() {
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
                });
                // Re-point the demand-present kick to the (now-active) window's
                // handle so its damage kicks this window while occluded.
                let handle = self.state.read(cx).ptys.term_window_handle(&session, &term_window);
                if let Some(handle) = handle {
                    crate::app::install_present_kick(&handle, window.window_handle(), cx);
                }
            }
        }

        // Host the active window's view (lazily created + cached), else a
        // placeholder when the window has no live session.
        let content: AnyElement = match &active {
            Some((session, term_window)) => {
                let handle = self.state.read(cx).ptys.term_window_handle(session, term_window);
                match handle {
                    Some(handle) => {
                        if !self.cache.contains_key(term_window) {
                            let view = self.make_terminal_view(handle, cx);
                            self.cache.insert(term_window.clone(), view);
                        }
                        // Safe: just inserted (or already present).
                        self.cache.get(term_window).unwrap().clone().into_any_element()
                    }
                    None => window_placeholder().into_any_element(),
                }
            }
            None => window_placeholder().into_any_element(),
        };

        // Focus follows activation (M2 Item D): the terminal's per-frame render
        // grab is gone (focus-once in `TerminalView`), so the host moves key
        // focus to the newly-active window's terminal on every activation change —
        // window open, ⌘T, pill/row click, window-step, close-refocus. Runs after
        // the cache fill above so a just-activated terminal window (spawned
        // synchronously by `activate_term_window`) is focusable this same render. A
        // window with no hosted view (Claude placeholder) has nothing to focus.
        if activation_changed {
            if let Some((_, term_window)) = &active {
                if let Some(view) = self.cache.get(term_window) {
                    let fh = view.read(cx).focus_handle_ref().clone();
                    window.focus(&fh, cx);
                }
            }
        }

        // Prod-parity content insets around the hosted window (constants above).
        // Applied to placeholder and terminal alike — Swift pads both branches
        // of `mainContent` (the placeholder branch keeps its leading pad).
        div()
            .size_full()
            .pl(px(CONTENT_INSET_LEADING))
            .pt(px(CONTENT_INSET_TOP))
            .pb(px(CONTENT_INSET_BOTTOM))
            .pr(px(CONTENT_INSET_TRAILING))
            .child(content)
    }
}
