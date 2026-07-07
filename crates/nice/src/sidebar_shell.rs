//! The R10 sessions-mode sidebar: the shell layout plus the sidebar card,
//! ported from `Sources/Nice/Views/AppShellView.swift` (the shell — layout
//! modes, floating card, resize handle, collapsed band, peek overlay) and
//! `Sources/Nice/Views/SidebarView.swift` (the card content — project groups,
//! tab rows, footer, and the multi-select / rename / Esc behaviour). The pure
//! state it drives ships gpui-free in `nice-model` (slice 1): [`SidebarModel`],
//! [`SidebarTabSelection`], [`InlineRenameClickGate`].
//!
//! ## Shared per-window state + transient view state (the GPUI shape)
//!
//! Swift spreads this across `AppShellView`, `SidebarView`, `ProjectGroup`, and
//! `TabRow` `@State`. GPUI splits it in two: the *document* state a whole window
//! shares — the [`TabModel`] (R8), the sidebar mode/collapse/peek `SidebarModel`,
//! the `SidebarTabSelection`, and the `SidebarActions` seam — lives in the
//! per-window [`WindowState`] entity this view holds a handle to and renders
//! from / mutates (R13.5's "one `TabModel` per window" invariant: no divergent
//! model copy in any mounted view, every mutation flowing through
//! `WindowState`'s seams). A sibling holder of that same entity — the keymap's
//! window-scoped actions, routed through the `WindowRegistry` — mutating it
//! re-renders this view through the `cx.observe` subscription set in [`new`].
//! What the view still owns is only the *transient* per-view state (resize
//! width, peek pin, disclosure-open set, inline-rename draft, the open context
//! menu). The rows and groups are built by helper methods rather than child
//! entities so their tap handlers can reach this state through `cx.listener` —
//! no cross-element interaction flags (the R9 anti-pattern), state is recomputed
//! per event.
//!
//! [`new`]: SidebarShellView::new
//!
//! ## DO-NOT-PORT seams (binding decision)
//!
//! The Esc `NSEvent` monitor, the rename click-away `NSEvent` monitors, and the
//! `WindowFrameReporter` are SwiftUI-seam artifacts. They are replaced with:
//!
//!   * a GPUI key **binding** ([`CollapseSidebarSelection`], installed by
//!     [`install_sidebar_key_bindings`]) whose handler runs before key listeners
//!     and the terminal's input handler — it collapses a >1 multi-selection (or
//!     cancels an in-flight rename) and otherwise `cx.propagate()`s so Esc still
//!     reaches the focused terminal;
//!   * a GPUI focus-out subscription ([`gpui::Context::on_blur`]) that commits an
//!     inline rename on focus loss, plus commit on Enter / row-deactivation /
//!     click-away and cancel on Esc.
//!
//! The S7 drag-reorder machinery (`SidebarDragState`, the drop delegates, the
//! insertion line) is **excluded, not missing** — R25 owns it.
//!
//! ## Icons
//!
//! The header/footer/row icons are real SF Symbols rendered at runtime through
//! [`crate::sf_symbols`] (`NSImage(systemSymbolName:)` rasterized + tinted at
//! the window's backing scale, cached per size/weight/colour/scale — M2
//! feel-check Item A). Each keeps its original Unicode stand-in as a
//! never-blank fallback for a symbol name that fails to resolve. The
//! disclosure "chevron" remains a **glyph swap** (`▸` closed / `▾` open)
//! rather than a rotation transform — the pinned gpui exposes no element
//! rotation, and the swap reads the same 0°→90° affordance.

// The view + its install fn have no in-crate caller until slice 4 wires the
// `sidebar` self-test scenario; it is a deliberately-exported surface (plan
// "Exported contracts"). The pure layout/label helpers below ARE exercised by
// this module's unit tests.
#![allow(dead_code)]

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, Context, CursorStyle, DismissEvent, Entity,
    FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, SharedString, Subscription, Window,
};

use nice_model::file_browser::TextFieldEditor;
use nice_model::{InlineRenameClickGate, SidebarMode, TabModel, TabStatus};
use nice_theme::chrome_geometry::{
    traffic_light_reserved_width, CARD_BORDER_OPACITY, CARD_BORDER_WIDTH, CARD_CORNER_RADIUS,
    CARD_INSET, CARD_SHADOW_OPACITY, CARD_SHADOW_RADIUS, CARD_SHADOW_Y_OFFSET,
    INNER_CORNER_RADIUS, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH,
    SIDEBAR_PEEK_WIDTH, SIDEBAR_RESIZE_HANDLE_WIDTH, TOP_BAR_HEIGHT,
};
use nice_theme::color::Srgba;
use nice_theme::palette::Slots;

use crate::app_shell::{PaneHostView, SIDEBAR_ROOT_LABEL};
use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::file_browser::view::FileBrowserView;
use crate::inline_rename::{
    dispatch_rename_key, edit_spans, rename_field, FieldColors, FieldProbe, RenameKeyOutcome,
};
use crate::session_manager::ClaudeTabPlacement;
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::status_dot::StatusDot;
use crate::theme::{slot_srgba, slot_to_rgba, srgba_to_rgba, srgba_with_alpha};
use crate::window_state::WindowState;

// The Esc key binding is a gpui action (the DO-NOT-PORT replacement for the
// `NSEvent` Esc monitor). Reuses the `nice` action namespace like R9's
// `ToggleFullScreen`; R12 owns the full app-wide keymap.
gpui::actions!(nice, [CollapseSidebarSelection]);

// ---- Geometry / behaviour constants (Swift provenance) ----------------------

/// Row leading inset for a root (non-lineage) tab. `SidebarView.swift:619`.
const ROW_INDENT_ROOT: f32 = 22.0;
/// Row leading inset for a depth-1 `/branch` child (one status-dot width
/// deeper). `SidebarView.swift:619`.
const ROW_INDENT_CHILD: f32 = 38.0;
/// Rename gate: the same click that selects a row must not also start a rename,
/// so the title-click only edits once this interval has elapsed since the row
/// became active — the macOS `NSEvent.doubleClickInterval` default analog
/// (`SidebarView.swift:440`). R12 could inject the user's real value.
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// Window-drag start threshold on the top strip, in pt — parity with the R9
/// band (`ChromeEventRouter.swift:218`, `crate::app`'s `BAND_DRAG_THRESHOLD_PX`).
const BAND_DRAG_THRESHOLD_PX: f32 = 2.0;
/// Hover-tier row fill: 6% ink (`SidebarView.swift:452`).
const HOVER_INK_ALPHA: f32 = 0.06;
/// Dimmed multi-select tier: the active-row selection tint at half alpha
/// (`SidebarView.swift:451` — `niceSel(...).opacity(0.5)`).
const SELECTED_DIM_FACTOR: f32 = 0.5;
/// Dark-scheme selection tint alpha applied to the accent (`Palette.swift:225`).
const SEL_ALPHA_DARK: f32 = 0.22;
/// Count-pill background: 7% ink (`SidebarView.swift:360`).
const COUNT_PILL_INK_ALPHA: f32 = 0.07;
/// Group `+` button hover fill: 10% ink (`SidebarView.swift:387`).
const ADD_BUTTON_HOVER_ALPHA: f32 = 0.10;
/// Icon-button (mode / collapse / footer) hover fill: 8% ink
/// (`SidebarView.swift:1037`, `AppShellView.swift:1112,1159`).
const ICON_BUTTON_HOVER_ALPHA: f32 = 0.08;
/// The mode/collapse toggles' top offset inside the 52pt strip, placing each
/// 24pt button's centre on the y-26 row (`AppShellView.swift:812`).
const TOP_STRIP_CONTROLS_TOP: f32 = 8.0;
/// The mode/collapse toggles' trailing offset (`AppShellView.swift:813`).
const TOP_STRIP_CONTROLS_TRAILING: f32 = 10.0;

// ---- Icons (SF Symbols + their Unicode fallbacks — see module docs) ---------

const ICON_CHEVRON_CLOSED: &str = "\u{25B8}"; // ▸ (disclosure — stays a glyph swap)
const ICON_CHEVRON_OPEN: &str = "\u{25BE}"; // ▾
const ICON_TERMINAL: &str = "\u{276F}"; // ❯ fallback for SF_TERMINAL
const ICON_PLUS: &str = "+"; // fallback for SF_PLUS
const ICON_MODE_TABS: &str = "\u{2630}"; // ☰ fallback for SF_MODE_TABS
const ICON_MODE_FILES: &str = "\u{25A4}"; // ▤ fallback for SF_MODE_FILES
const ICON_SIDEBAR: &str = "\u{25A8}"; // ▨ fallback for SF_SIDEBAR
const ICON_GEAR: &str = "\u{2699}"; // ⚙ fallback for SF_GEAR

/// Tab-row / pill leading icon (`SidebarView.swift:602`).
const SF_TERMINAL: &str = "terminal";
/// Group-header add button (`SidebarView.swift:379`).
const SF_PLUS: &str = "plus";
/// Sidebar mode toggle: tabs (`AppShellView.swift`'s `SidebarModeIconButton`).
const SF_MODE_TABS: &str = "list.bullet";
/// Sidebar mode toggle: files.
const SF_MODE_FILES: &str = "folder";
/// Collapse / restore toggle (`AppShellView.swift:1153`).
const SF_SIDEBAR: &str = "sidebar.left";
/// Footer Settings gear (`SidebarView.swift`'s footer `SidebarIconButton`).
const SF_GEAR: &str = "gearshape";

// ---- Pure helpers (unit-tested; no gpui) ------------------------------------

/// Clamp a candidate sidebar width to the resizable range (`AppShellView.swift:882`).
fn clamp_sidebar_width(width: f32) -> f32 {
    width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
}

/// The new sidebar width for a resize drag: baseline + horizontal delta, clamped.
fn resize_width(baseline: f32, delta_x: f32) -> f32 {
    clamp_sidebar_width(baseline + delta_x)
}

/// Row leading inset for a tab, given whether it is a depth-1 lineage child.
fn row_indent(indented: bool) -> f32 {
    if indented {
        ROW_INDENT_CHILD
    } else {
        ROW_INDENT_ROOT
    }
}

/// The context-menu close label for a right-click acting on `count` tabs
/// (`SidebarView.swift:644`).
fn close_menu_label(count: usize) -> String {
    if count > 1 {
        format!("Close {count} Tabs")
    } else {
        "Close Tab".to_string()
    }
}

/// The disclosure chevron glyph for an open/closed group (glyph swap — see the
/// module docs).
fn disclosure_glyph(is_open: bool) -> &'static str {
    if is_open {
        ICON_CHEVRON_OPEN
    } else {
        ICON_CHEVRON_CLOSED
    }
}

// ---- Colour helpers (Nice/Dark; the SidebarBackground palette seam) ----------

/// The active chrome slot table — the live
/// [`SharedThemeState`](crate::theme_settings::SharedThemeState) (Nice/Dark
/// fallback when the theme global is absent, i.e. the isolated `sidebar`
/// scenario). R21: was a fixed Nice/Dark table.
fn active_slots(cx: &App) -> Slots {
    crate::theme_settings::active_chrome_slots(cx)
}

/// The `SidebarBackground` palette-switch seam (`SidebarBackground.swift:21-46`).
/// R21 (S9): the sidebar column paints the ACTIVE palette's `background2` slot as
/// a flat panel — Nice reads as the flat `niceBg2` panel, Catppuccin as its flat
/// tinted `background2` (real vibrancy + the macOS arm are deferred with the macOS
/// palette). Because `s` is now the active palette's slots (via
/// [`active_slots`]), this seam follows the live theme with no extra branch.
fn sidebar_background(s: &Slots) -> Rgba {
    slot_to_rgba(s.background2)
}

/// The floating-card border colour — the `line` slot at [`CARD_BORDER_OPACITY`]
/// (`AppShellView.swift:829`).
fn card_border_color(s: &Slots) -> Rgba {
    srgba_to_rgba(srgba_with_alpha(slot_srgba(s.line), CARD_BORDER_OPACITY))
}

/// The floating-card drop shadow (`AppShellView.swift:838`).
fn card_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: CARD_SHADOW_OPACITY,
        }
        .into(),
        offset: point(px(0.0), px(CARD_SHADOW_Y_OFFSET)),
        blur_radius: px(CARD_SHADOW_RADIUS),
        spread_radius: px(0.0),
        inset: false,
    }]
}

/// The ink slot at straight alpha `a` — the translucent hover / pill fills.
fn ink_alpha(s: &Slots, a: f32) -> Rgba {
    srgba_to_rgba(srgba_with_alpha(slot_srgba(s.ink), a))
}

/// The accent selection tint at `factor × SEL_ALPHA_DARK` — the active-row fill
/// (`factor == 1.0`) and the dimmed multi-select fill (`factor == 0.5`).
fn selection_tint(accent: Srgba, factor: f32) -> Rgba {
    srgba_to_rgba(srgba_with_alpha(accent, SEL_ALPHA_DARK * factor))
}

// ---- View-model snapshot (decouples rendering from model borrows) -----------

/// A per-render snapshot of one tab row.
struct TabVm {
    id: String,
    title: String,
    indented: bool,
    has_claude: bool,
    status: TabStatus,
    waiting_ack: bool,
    is_active: bool,
    is_selected: bool,
    is_editing: bool,
}

/// A per-render snapshot of one project group.
struct GroupVm {
    id: String,
    name: String,
    is_terminals: bool,
    count: usize,
    is_open: bool,
    hovered: bool,
    tabs: Vec<TabVm>,
}

// ---- The view ---------------------------------------------------------------

/// The per-window sessions-mode sidebar shell. Construct with
/// [`SidebarShellView::new`] over the window's shared [`WindowState`] entity; it
/// renders the shared `model` / `sidebar` / `selection` and mutates them through
/// `WindowState`'s seams.
pub(crate) struct SidebarShellView {
    /// The shared per-window state (the single [`TabModel`], the sidebar
    /// collapse/mode/peek model, the multi-selection, and the create/close/select
    /// [`SidebarActions`] seam). This view renders from and mutates it; it never
    /// keeps a private copy (R13.5's "one `TabModel` per window" invariant).
    state: Entity<WindowState>,
    /// Re-render this view whenever the shared state notifies — the seam through
    /// which the keymap's window-scoped actions (⌘S toggle, tab cycle, …) become
    /// visible in the shell. Held so the subscription lives as long as the view.
    _state_sub: Subscription,

    /// R13.5 composition slot: the toolbar band (the R11 `WindowToolbarView`),
    /// rendered in the 52pt top-bar-accessory position — right of the card in the
    /// expanded shell, right of the restore button in the collapsed shell's
    /// full-width band. `None` in the isolated `sidebar` scenario, which mounts
    /// the shell standalone and keeps the placeholder content region.
    main_toolbar: Option<AnyView>,
    /// R13.5 composition slot: the pane-content host (`PaneHostView`), rendered as
    /// the shell's fill body below the toolbar. `None` in the isolated scenario.
    main_body: Option<AnyView>,

    /// The user-resizable docked sidebar width (in-memory; resets on relaunch).
    sidebar_width: f32,
    /// The width at the start of a resize drag (baseline for the clamp).
    drag_start_width: Option<f32>,
    /// Window-x of the resize drag's initial press (delta reference).
    resize_origin_x: Option<f32>,

    /// True while the cursor pins an open peek overlay (the view's own hover
    /// pin, OR'd with `SidebarModel::peeking` which R12 drives).
    peek_mouse_pinned: bool,
    /// Top-strip window-drag press origin (R9 band pattern), not yet a drag.
    band_press: Option<Point<Pixels>>,

    /// Projects whose disclosure is collapsed (absent == open, the default).
    collapsed_projects: HashSet<String>,
    /// The project whose header is hovered (reveals its `+` button).
    hovered_project: Option<String>,

    /// The tab currently being inline-renamed, if any.
    editing_tab_id: Option<String>,
    /// The in-flight rename editor (cursor + selection; `None` when not editing).
    rename_editor: Option<TextFieldEditor>,
    /// The rename field's painted geometry (text-run + field-box left edges,
    /// window coords), written by the field's layout probes each paint and read
    /// by its click-to-position handler.
    rename_probe: Rc<Cell<FieldProbe>>,
    /// When the current active tab became active — the rename gate reference.
    activated_at: Option<Instant>,
    /// Focus for the inline-rename field (grabbed on begin, released on commit).
    rename_focus: FocusHandle,
    /// Focus-out subscription committing the rename (the DO-NOT-PORT click-away
    /// monitor's replacement). Replaced on each `begin_editing`.
    rename_blur_sub: Option<Subscription>,

    /// The open tab context menu, if any.
    context_menu: Option<Entity<ContextMenu>>,
    /// The menu's dismiss subscription.
    menu_sub: Option<Subscription>,

    /// R19: the files-mode browser view, created lazily the first time the sidebar
    /// enters files mode and rendered by [`build_body`](Self::build_body) in place
    /// of the tab list (peeking keeps showing the tabs — the preserved invariant).
    /// One per window; owns its own kqueue watcher + scroll handle.
    file_browser: Option<Entity<FileBrowserView>>,

    /// Root focus handle (hosts the `SidebarShell` key context for Esc).
    focus_handle: FocusHandle,
    /// The window's pane-content host, wired by `crate::app::build_window_root`
    /// (M2 Item D): the seam through which the shell returns key focus to the
    /// active terminal after a rename commit/cancel and on menu dismissal.
    /// `None` in the isolated `sidebar` scenario (refocus is then a no-op).
    pane_host: Option<Entity<PaneHostView>>,
    /// Chrome-click focus bounce (M2 Item D): a click on empty shell chrome
    /// (card body, top strip, footer) focuses this root via gpui's tracked-focus
    /// mouse-down transfer; this `on_focus` subscription bounces it straight
    /// back to the active terminal (chrome never keeps focus — Swift parity).
    /// Installed on the first render (the subscription needs a `Window`).
    focus_bounce_sub: Option<Subscription>,
    /// The user's accent — the thinking-dot colour + selection tint. Terracotta
    /// default (palette switching is R21).
    accent: Srgba,
    /// The window's backing scale factor, re-sampled at the top of every
    /// [`Render::render`] so the SF Symbol rasterizer draws at device
    /// resolution. The 2.0 initial value only covers code paths before the
    /// first render (none read it).
    window_scale: f32,
}

impl SidebarShellView {
    /// A shell over the window's shared [`WindowState`]: it reads the sidebar
    /// mode/collapse/peek, the selection, and the tab tree from that entity and
    /// mutates them through its seams. The `sidebar`/`selection` invariants
    /// (expanded, tabs mode, selection seeded from the active tab) are established
    /// by [`WindowState::with_model`] / [`WindowState::new`], not here. Width 240,
    /// Terracotta accent. Observing the state re-renders the shell when a sibling
    /// holder (the keymap) mutates it.
    pub(crate) fn new(state: Entity<WindowState>, cx: &mut Context<Self>) -> Self {
        let state_sub = cx.observe(&state, |_this, _state, cx| cx.notify());
        Self {
            state,
            _state_sub: state_sub,
            main_toolbar: None,
            main_body: None,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            drag_start_width: None,
            resize_origin_x: None,
            peek_mouse_pinned: false,
            band_press: None,
            collapsed_projects: HashSet::new(),
            hovered_project: None,
            editing_tab_id: None,
            rename_editor: None,
            rename_probe: Rc::new(Cell::new(FieldProbe::default())),
            activated_at: Some(Instant::now()),
            rename_focus: cx.focus_handle(),
            rename_blur_sub: None,
            context_menu: None,
            menu_sub: None,
            file_browser: None,
            focus_handle: cx.focus_handle(),
            pane_host: None,
            focus_bounce_sub: None,
            // R21: seed the accent from the live `SharedThemeState` (Terracotta
            // fallback when the theme global is absent, i.e. isolated scenarios).
            // The render path re-reads it live per frame; this seed feeds the
            // `accent()` accessor + the lazily-minted file browser.
            accent: crate::theme_settings::active_chrome_accent(cx),
            window_scale: 2.0,
        }
    }

    /// Wire the window's pane host (called once by `build_window_root`) so the
    /// shell can return key focus to the active terminal (M2 Item D).
    pub(crate) fn set_pane_host(&mut self, host: Entity<PaneHostView>) {
        self.pane_host = Some(host);
    }

    /// The R13.5 composed shell: same shared-state shell as [`new`](Self::new)
    /// with the toolbar band + pane-content host injected into the content
    /// region's top-bar-accessory + body slots. `crate::app::build_window_root`
    /// wires this for the shipped window and every ⌘N window; the isolated
    /// `sidebar` scenario keeps [`new`](Self::new) (placeholder content).
    pub(crate) fn new_composed(
        state: Entity<WindowState>,
        toolbar: AnyView,
        body: AnyView,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(state, cx);
        this.main_toolbar = Some(toolbar);
        this.main_body = Some(body);
        this
    }

    // MARK: - Snapshot

    fn snapshot_groups(&self, cx: &mut Context<Self>) -> Vec<GroupVm> {
        let ws = self.state.read(cx);
        let active = ws.model.active_tab_id().map(|s| s.to_string());
        ws.model
            .projects
            .iter()
            .map(|p| {
                let is_open = !self.collapsed_projects.contains(&p.id);
                let tabs = if is_open {
                    p.tabs
                        .iter()
                        .map(|t| TabVm {
                            id: t.id.clone(),
                            title: t.title.clone(),
                            indented: t.parent_tab_id.is_some(),
                            has_claude: t.has_claude(),
                            status: t.status(),
                            waiting_ack: t.waiting_acknowledged(),
                            is_active: active.as_deref() == Some(t.id.as_str()),
                            is_selected: ws.selection.contains(&t.id),
                            is_editing: self.editing_tab_id.as_deref() == Some(t.id.as_str()),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                GroupVm {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    is_terminals: p.id == TabModel::TERMINALS_PROJECT_ID,
                    count: p.tabs.len(),
                    is_open,
                    hovered: self.hovered_project.as_deref() == Some(p.id.as_str()),
                    tabs,
                }
            })
            .collect()
    }

    // MARK: - Selection routing / active tracking

    /// Route a modifier-aware row click. Plain collapses to `{id}` + activates;
    /// ⌘ toggles (most-recently-clicked stays active, only-and-active refused);
    /// ⇧ extends from the sticky anchor. Resets `activated_at` only when the
    /// active tab actually changes (so a click on the already-active row keeps
    /// the rename gate armed — `SidebarView.swift`'s `onChange(of: isActive)`).
    fn route_click(&mut self, tab_id: &str, cmd: bool, shift: bool, cx: &mut Context<Self>) {
        let changed = self.state.update(cx, |ws, _| {
            let before = ws.model.active_tab_id().map(|s| s.to_string());
            if cmd {
                if let Some(new_active) = ws.selection.toggle(tab_id) {
                    ws.sidebar_actions.select_tab(&mut ws.model, &new_active);
                }
            } else if shift {
                let order = ws.model.navigable_sidebar_tab_ids();
                ws.selection.extend(tab_id, &order);
                ws.sidebar_actions.select_tab(&mut ws.model, tab_id);
            } else {
                ws.selection.replace(tab_id);
                ws.sidebar_actions.select_tab(&mut ws.model, tab_id);
            }
            let after = ws.model.active_tab_id().map(|s| s.to_string());
            // Reconcile the selection's active mirror with the model (a no-op on
            // the tap paths since the mutators already set it; keeps the invariant
            // if a toggle refused).
            let active = ws.model.active_tab_id().map(|s| s.to_string());
            ws.selection.sync_active_tab_id(active.as_deref());
            before != after
        });
        if changed {
            self.activated_at = Some(Instant::now());
        }
    }

    /// Plain title tap: modified clicks route like a row; on the already-active
    /// row a plain tap enters rename only past the gate; otherwise it's a plain
    /// select (`SidebarView.swift:569-586`).
    fn handle_title_tap(
        &mut self,
        tab_id: &str,
        cmd: bool,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cmd || shift {
            self.route_click(tab_id, cmd, shift, cx);
            return;
        }
        let is_active = self.state.read(cx).model.active_tab_id() == Some(tab_id);
        if is_active {
            if InlineRenameClickGate::can_begin_edit(
                self.activated_at,
                Instant::now(),
                DOUBLE_CLICK_INTERVAL,
            ) {
                self.begin_editing(tab_id, window, cx);
            }
            // else: same-click-as-select window — no-op (no redundant reselect).
        } else {
            self.route_click(tab_id, false, false, cx);
        }
    }

    /// Collapse a multi-selection back to the active tab (Esc / empty-area
    /// click). Drops everything only when the tree has no active tab — a
    /// mid-shutdown edge (`SidebarView.swift:86-92`).
    fn collapse_selection_to_active(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, _| {
            if let Some(active) = ws.model.active_tab_id().map(|s| s.to_string()) {
                ws.selection.collapse(&active);
            } else {
                ws.selection.clear();
            }
        });
    }

    /// Re-seed the selection + arm the rename gate after a create/select (the new
    /// tab is already the model's active tab).
    fn reseed_selection_after_create(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, _| {
            let active = ws.model.active_tab_id().map(|s| s.to_string());
            ws.selection.sync_active_tab_id(active.as_deref());
        });
        self.activated_at = Some(Instant::now());
    }

    // MARK: - Inline rename

    fn begin_editing(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self
            .state
            .read(cx)
            .model
            .tab_for(tab_id)
            .map(|t| t.title.clone())
        else {
            return;
        };
        self.editing_tab_id = Some(tab_id.to_string());
        // Cursor at the end (typing appends) — the prior char-append behaviour.
        self.rename_editor = Some(TextFieldEditor::new(&title));
        self.rename_focus.focus(window, cx);
        // Commit on focus loss (the DO-NOT-PORT click-away monitor replacement).
        // Replacing any prior subscription here drops it OUTSIDE its callback.
        self.rename_blur_sub = Some(cx.on_blur(&self.rename_focus, window, |this, window, cx| {
            this.commit_rename(window, cx);
        }));
        cx.notify();
    }

    /// Commit the draft (empty input is a model no-op — asymmetry 3). Idempotent:
    /// a stray focus-out after the edit already ended does nothing.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing_tab_id.take() else {
            return;
        };
        let draft = self.rename_editor.take().map(|e| e.text()).unwrap_or_default();
        self.state.update(cx, |ws, _| ws.model.rename_tab(&id, &draft));
        self.refocus_terminal_after_rename(window, cx);
        cx.notify();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_tab_id.take().is_none() {
            return;
        }
        self.rename_editor = None;
        self.refocus_terminal_after_rename(window, cx);
        cx.notify();
    }

    /// Reposition the caret from a click hit-test — collapse the selection to the
    /// clicked boundary and re-grab field focus (the click already stopped
    /// propagation, so the tab's title-tap gate never re-trips).
    fn place_rename_cursor(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.rename_editor.as_mut() {
            editor.place_cursor(index);
            self.rename_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Swift's rename end paths call `sessions.focusActiveTerminal()` so the
    /// terminal regains first responder (dossier G10). Here the window's
    /// [`PaneHostView`] owns the hosted terminal views, so focus routes back
    /// through its `focus_active_terminal` (M2 Item D — the sidebar-rename
    /// equivalent of the toolbar's refocus). A no-op in the isolated `sidebar`
    /// scenario (no pane host wired).
    fn refocus_terminal_after_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(host) = self.pane_host.clone() {
            host.update(cx, |host, cx| host.focus_active_terminal(window, cx));
        }
    }

    fn on_rename_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        // Escape is consumed by the shell Esc action (which cancels rename) before
        // this bubble-phase listener runs; the shared editor leaves it Ignored so
        // that action still fires.
        let outcome = {
            let Some(editor) = self.rename_editor.as_mut() else {
                return;
            };
            dispatch_rename_key(
                editor,
                &ks.key,
                ks.key_char.as_deref(),
                ks.modifiers.shift,
                ks.modifiers.platform,
                ks.modifiers.control,
            )
        };
        match outcome {
            RenameKeyOutcome::Commit => {
                self.commit_rename(window, cx);
                cx.stop_propagation();
            }
            RenameKeyOutcome::Edited => {
                cx.notify();
                cx.stop_propagation();
            }
            RenameKeyOutcome::Ignored => {}
        }
    }

    // MARK: - Toggles / actions

    fn set_mode(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, _| {
            if ws.sidebar.mode() != mode {
                ws.sidebar.toggle_sidebar_mode();
            }
        });
    }

    /// Toggle the collapsed flag. Expanding also clears any peek state
    /// (`AppShellView`: expand clears peek).
    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        let now_collapsed = self.state.update(cx, |ws, _| {
            ws.sidebar.toggle_sidebar();
            let collapsed = ws.sidebar.collapsed();
            if !collapsed {
                ws.sidebar.end_sidebar_peek();
            }
            collapsed
        });
        if !now_collapsed {
            self.peek_mouse_pinned = false;
        }
        cx.notify();
    }

    fn add_tab_in_group(&mut self, group_id: &str, is_terminals: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, wcx| {
            if is_terminals {
                // Terminal tab: model-only; its pane spawns render-driven on first
                // activation (`ensure_active_pane_spawned`).
                ws.sidebar_actions.create_terminal_tab(&mut ws.model);
            } else {
                // R15: a real Claude tab through the ONE shared constructor — mints
                // the session UUID, registers the session, and spawns the Claude
                // pane immediately (claude-kind panes never lazy-spawn); the
                // companion terminal stays deferred.
                let settings = ws.claude_settings_path_provider();
                let model = &mut ws.model;
                let session = &mut ws.session;
                let _ = session.create_claude_tab(
                    model,
                    ClaudeTabPlacement::Project {
                        project_id: group_id.to_string(),
                    },
                    &[],
                    settings.as_deref(),
                    wcx,
                );
            }
        });
        self.reseed_selection_after_create(cx);
        cx.notify();
    }

    fn toggle_disclosure(&mut self, group_id: &str, cx: &mut Context<Self>) {
        if !self.collapsed_projects.insert(group_id.to_string()) {
            self.collapsed_projects.remove(group_id);
        }
        cx.notify();
    }

    // MARK: - Esc action

    fn on_collapse_esc(
        &mut self,
        _action: &CollapseSidebarSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_tab_id.is_some() {
            self.cancel_rename(window, cx);
            return; // consumed
        }
        let multi = self.state.read(cx).selection.selected_tab_ids().len() > 1;
        if multi {
            self.collapse_selection_to_active(cx);
            cx.notify(); // consumed
        } else {
            // Nothing to collapse — let Esc reach the focused terminal.
            cx.propagate();
        }
    }

    // MARK: - Context menu

    fn open_tab_context_menu(
        &mut self,
        tab_id: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action_ids = self
            .state
            .read(cx)
            .selection
            .selection_ids_for_right_click_on(tab_id);
        let mut items = Vec::new();
        let weak = cx.weak_entity();

        // Rename appears only for a single-row selection (`SidebarView.swift:636`).
        if action_ids.len() == 1 {
            let tid = tab_id.to_string();
            let w = weak.clone();
            items.push(ContextMenuItem::entry("Rename Tab", move |window, app| {
                let _ = w.update(app, |this, cx| {
                    this.state.update(cx, |ws, _| {
                        ws.selection.snap_if_right_click_outside(&tid);
                        ws.sidebar_actions.select_tab(&mut ws.model, &tid);
                    });
                    this.reseed_selection_after_create(cx);
                    this.begin_editing(&tid, window, cx);
                });
            }));
        }

        let close_label = close_menu_label(action_ids.len());
        let ids = action_ids.clone();
        let tid = tab_id.to_string();
        let w = weak.clone();
        // R20.5: route through the busy-close gate. A tab with an alive busy pane
        // (thinking/waiting Claude, or a shell with a foreground child) interposes
        // the "Force quit" confirmation; an idle tab still closes immediately (pty
        // release + dissolve cascade + save + reconcile + terminus). The multi-tab
        // gate is partial-eager: idle members close now, only busy survivors are
        // gated (D5). The gate owns the reconcile + notify + terminus in every path.
        items.push(ContextMenuItem::entry(close_label, move |window, app| {
            let _ = w.update(app, |this, cx| {
                this.state.update(cx, |ws, wcx| {
                    ws.selection.snap_if_right_click_outside(&tid);
                    if ids.len() > 1 {
                        ws.request_close_tabs(&ids, window, wcx);
                    } else {
                        ws.request_close_tab(&tid, window, wcx);
                    }
                });
            });
        }));

        self.present_context_menu(items, position, window, cx);
    }

    /// Open the project-group context menu. "Close Project" is offered only for
    /// non-Terminals groups (`SidebarView.swift:323-330`); the pinned Terminals
    /// group has no menu, so a right-click there opens nothing.
    fn open_project_context_menu(
        &mut self,
        group_id: &str,
        is_terminals: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_terminals {
            return;
        }
        let weak = cx.weak_entity();
        let gid = group_id.to_string();
        // R20.5: route through the busy-close gate — a project with an alive busy
        // pane across its tabs interposes the "Force quit" confirmation; an idle
        // project still closes immediately (pending-removal flag → row drop on last
        // dissolve + pty release + save + reconcile + terminus). The gate owns the
        // reconcile + notify + terminus in both paths.
        let items = vec![ContextMenuItem::entry("Close Project", move |window, app| {
            let _ = weak.update(app, |this, cx| {
                this.state
                    .update(cx, |ws, wcx| ws.request_close_project(&gid, window, wcx));
            });
        })];
        self.present_context_menu(items, position, window, cx);
    }

    /// Mint the popup entity, subscribe to its dismissal, and store it.
    fn present_context_menu(
        &mut self,
        items: Vec<ContextMenuItem>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|cx| ContextMenu::new(position, items, window, cx));
        self.menu_sub = Some(cx.subscribe_in(
            &menu,
            window,
            |this, _menu, _ev: &DismissEvent, window, cx| {
                this.context_menu = None;
                // The menu grabbed key focus on open; hand it back to the active
                // terminal — unless the dismissed action began an inline rename
                // (the Rename Tab entry focuses the field before the menu
                // dismisses), which must keep the field focused (M2 Item D).
                if this.editing_tab_id.is_none() {
                    this.refocus_terminal_after_rename(window, cx);
                }
                cx.notify();
            },
        ));
        self.context_menu = Some(menu);
        cx.notify();
    }

    // MARK: - Top-strip window drag (R9 band pattern)

    fn on_strip_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.band_press = None;
        if window.is_fullscreen() {
            return;
        }
        if event.click_count >= 2 {
            window.titlebar_double_click();
            cx.stop_propagation();
            return;
        }
        self.band_press = Some(event.position);
    }

    fn on_strip_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some(origin) = self.band_press else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.band_press = None;
            return;
        }
        let dx = f32::from(event.position.x - origin.x);
        let dy = f32::from(event.position.y - origin.y);
        if dx * dx + dy * dy >= BAND_DRAG_THRESHOLD_PX * BAND_DRAG_THRESHOLD_PX {
            self.band_press = None;
            window.start_window_move();
        }
    }

    fn on_strip_mouse_up(&mut self, _e: &MouseUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        self.band_press = None;
    }

    // MARK: - Resize handle (root-level move/up so the drag survives cursor drift)

    fn on_resize_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            // Double-click resets to the default width.
            self.sidebar_width = SIDEBAR_DEFAULT_WIDTH;
            self.drag_start_width = None;
            self.resize_origin_x = None;
        } else {
            self.resize_origin_x = Some(f32::from(event.position.x));
            self.drag_start_width = Some(self.sidebar_width);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_root_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(origin), Some(base)) = (self.resize_origin_x, self.drag_start_width) else {
            return;
        };
        if event.pressed_button == Some(MouseButton::Left) {
            let delta = f32::from(event.position.x) - origin;
            self.sidebar_width = resize_width(base, delta);
            cx.notify();
        } else {
            self.resize_origin_x = None;
            self.drag_start_width = None;
        }
    }

    fn on_root_mouse_up(&mut self, _e: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.resize_origin_x.is_some() {
            self.resize_origin_x = None;
            self.drag_start_width = None;
            cx.notify();
        }
    }

    // MARK: - Rendering

    fn build_expanded_shell(
        &self,
        groups: Vec<GroupVm>,
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = active_slots(cx);
        // `flex_row_reverse` keeps the LAYOUT exactly as before — the first
        // child sits at the main-end, so the main column still fills the right
        // and the card still docks left — while flipping the PAINT order to
        // main column → divider → card. That order is what the Item C hairline
        // needs: drawn over the toolbar band's opaque chrome, but under the
        // floating card, which overlaps it by design (the line stays visible
        // in the gutters).
        div()
            .relative()
            .flex()
            .flex_row_reverse()
            .size_full()
            .child(self.build_main_column(cx))
            .child(self.build_top_bar_divider(&s))
            .child(self.build_sidebar_card(&groups, true, false, mode, cx))
    }

    fn build_collapsed_shell(
        &self,
        groups: Vec<GroupVm>,
        mode: SidebarMode,
        peeking_model: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = active_slots(cx);
        let peeking = peeking_model || self.peek_mouse_pinned;
        let peek = peeking.then(|| self.build_peek_overlay(&groups, peeking, mode, cx));
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(
                // Top row: one full-width 52pt title-bar band (M2 feel-check
                // Item B — the floating collapsed cap is GONE, an approved
                // divergence from the Swift parity design): a spacer reserving
                // the native traffic-light zone, the bare restore button (no
                // card, border, rounding, or shadow behind it), then the
                // toolbar accessory; in the isolated scenario the accessory is
                // the R9/R11 chrome filler. The pane content extends
                // full-width beneath.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .h(px(TOP_BAR_HEIGHT))
                    .bg(slot_to_rgba(s.chrome))
                    .child(
                        div()
                            .flex_none()
                            .w(px(traffic_light_reserved_width()))
                            .h_full(),
                    )
                    .child(self.icon_button(
                        SF_SIDEBAR,
                        ICON_SIDEBAR,
                        &s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_collapsed(cx);
                            cx.stop_propagation();
                        }),
                        cx,
                    ))
                    .child(self.build_collapsed_top_accessory(&s)),
            )
            .child(self.build_main_body(cx))
            // The Item C divider paints over the band chrome but under the
            // peek overlay (emitted next).
            .child(self.build_top_bar_divider(&s))
            .children(peek)
    }

    /// The 240pt (or `sidebar_width`) floating card: the top strip, the body
    /// (tab list or files placeholder), and the footer, over a flat `niceBg2`
    /// panel. `resizable` adds the trailing resize handle. `peeking` forces the
    /// body to the tabs list — it is the same effective-peek predicate that
    /// gates the overlay's visibility, so the overlay's body and its presence
    /// never disagree (peek overlays always show tabs, even in files mode).
    fn build_sidebar_card(
        &self,
        groups: &[GroupVm],
        resizable: bool,
        peeking: bool,
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = active_slots(cx);
        let card_width = if resizable {
            self.sidebar_width
        } else {
            SIDEBAR_PEEK_WIDTH
        };
        let handle = resizable.then(|| self.build_resize_handle(cx));
        let inner = div()
            // Exported shipped-surface AX anchor (§6): the sidebar card root, found
            // by an AX walk on role + label. gpui exposes an element to the macOS
            // AX tree only with both an `.id()` and a non-generic `.role()`; the
            // `aria_label` becomes its `AXTitle`.
            .id(SIDEBAR_ROOT_LABEL)
            .role(gpui::Role::Group)
            .aria_label(SIDEBAR_ROOT_LABEL)
            .relative()
            .flex()
            .flex_col()
            .w(px(card_width))
            .h_full()
            .bg(sidebar_background(&s))
            .rounded(px(CARD_CORNER_RADIUS))
            .border(px(CARD_BORDER_WIDTH))
            .border_color(card_border_color(&s))
            .shadow(card_shadow())
            .child(self.build_top_strip(&s, mode, cx))
            .child(self.build_body(groups, &s, peeking, mode, cx))
            .child(self.build_footer(&s, cx))
            .children(handle);
        div()
            .flex_none()
            .h_full()
            .pl(px(CARD_INSET))
            .pt(px(CARD_INSET))
            .pb(px(CARD_INSET))
            .child(inner)
    }

    /// The 52pt drag strip reserving the traffic-light row, with the mode +
    /// collapse toggles at its trailing edge. The strip is the R9 band (drag to
    /// move, double-click zoom); the buttons consume their own presses so the
    /// band passes them through.
    fn build_top_strip(&self, s: &Slots, mode: SidebarMode, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = crate::theme_settings::active_chrome_accent(cx);
        let tabs_active = mode == SidebarMode::Tabs;
        let files_active = mode == SidebarMode::Files;
        div()
            .relative()
            .flex_none()
            .w_full()
            .h(px(TOP_BAR_HEIGHT))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_strip_mouse_down))
            .on_mouse_move(cx.listener(Self::on_strip_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_strip_mouse_up))
            .child(
                div()
                    .absolute()
                    .top(px(TOP_STRIP_CONTROLS_TOP))
                    .right(px(TOP_STRIP_CONTROLS_TRAILING))
                    .flex()
                    .flex_row()
                    .gap(px(4.0))
                    .child(self.mode_button(
                        "sidebar.mode.tabs",
                        SF_MODE_TABS,
                        ICON_MODE_TABS,
                        tabs_active,
                        accent,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.set_mode(SidebarMode::Tabs, cx);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                        cx,
                    ))
                    .child(self.mode_button(
                        "sidebar.mode.files",
                        SF_MODE_FILES,
                        ICON_MODE_FILES,
                        files_active,
                        accent,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.set_mode(SidebarMode::Files, cx);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                        cx,
                    ))
                    .child(self.icon_button(
                        SF_SIDEBAR,
                        ICON_SIDEBAR,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_collapsed(cx);
                            cx.stop_propagation();
                        }),
                        cx,
                    )),
            )
    }

    /// A 24pt header icon button (mode toggle): accent-tinted when active, hover
    /// fill otherwise (`AppShellView.swift:1097-1134`). The 13pt symbol is
    /// semibold + ink when active, regular + ink2 otherwise — each tint its own
    /// [`sf_symbol_icon`] cache entry.
    #[allow(clippy::too_many_arguments)]
    fn mode_button(
        &self,
        _id: &str,
        symbol: &'static str,
        fallback_glyph: &'static str,
        active: bool,
        accent: Srgba,
        s: &Slots,
        on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let hover = ink_alpha(s, ICON_BUTTON_HOVER_ALPHA);
        let icon = sf_symbol_icon(
            symbol,
            fallback_glyph,
            13.0,
            if active {
                SymbolWeight::Semibold
            } else {
                SymbolWeight::Regular
            },
            if active { ink } else { ink2 },
            self.window_scale,
            cx,
        );
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded(px(INNER_CORNER_RADIUS))
            .when(active, |el| el.bg(selection_tint(accent, 1.0)))
            .when(!active, |el| el.hover(move |st| st.bg(hover)))
            .child(icon)
            .on_mouse_down(MouseButton::Left, on_down)
    }

    /// A plain 24pt icon button (the `sidebar.left` collapse / restore toggle):
    /// hover fill only, 14pt regular ink2 (`SidebarView.swift:1018-1044`,
    /// `AppShellView.swift:1145-1166`).
    fn icon_button(
        &self,
        symbol: &'static str,
        fallback_glyph: &'static str,
        s: &Slots,
        on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = ink_alpha(s, ICON_BUTTON_HOVER_ALPHA);
        let icon = sf_symbol_icon(
            symbol,
            fallback_glyph,
            14.0,
            SymbolWeight::Regular,
            slot_to_rgba(s.ink2),
            self.window_scale,
            cx,
        );
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded(px(INNER_CORNER_RADIUS))
            .hover(move |st| st.bg(hover))
            .child(icon)
            .on_mouse_down(MouseButton::Left, on_down)
    }

    /// The card body: the scrollable tab list, or (in files mode while not
    /// peeking) the placeholder browser panel. Peeking always shows the tabs
    /// list even in files mode (`SidebarView.swift:122-128`). `peeking` is the
    /// caller's effective-peek predicate (`sidebar.peeking() ||
    /// peek_mouse_pinned`), threaded so the body agrees with the overlay's own
    /// visibility test rather than re-deriving from a narrower subset of state.
    fn build_body(
        &self,
        groups: &[GroupVm],
        s: &Slots,
        peeking: bool,
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let show_tabs = peeking || mode == SidebarMode::Tabs;
        if show_tabs {
            self.build_tab_list(groups, s, cx).into_any_element()
        } else if let Some(fb) = self.file_browser.clone() {
            // R19: the real file browser (mounted here in place of the landed
            // placeholder). `render` mints it on first entry to files mode.
            fb.into_any_element()
        } else {
            // Defensive fallback: files mode with no browser yet (never happens —
            // `render` creates it before this — but keeps `build_body` total).
            div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.0))
                .text_color(slot_to_rgba(s.ink3))
                .child(SharedString::from("Files"))
                .into_any_element()
        }
    }

    fn build_tab_list(&self, groups: &[GroupVm], s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        let group_els: Vec<gpui::AnyElement> = groups
            .iter()
            .map(|g| self.build_project_group(g, s, cx))
            .collect();
        div()
            .id("sidebar.tabList")
            .overflow_y_scroll()
            .flex_1()
            .w_full()
            // Empty-area click collapses a multi-selection back to the active tab
            // (rows consume their own presses, so this fires only for the gaps /
            // padding / unfilled bottom — `SidebarView.swift:142-163`).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                    this.collapse_selection_to_active(cx);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .py(px(10.0))
                    .children(group_els),
            )
    }

    fn build_project_group(&self, g: &GroupVm, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ink2 = slot_to_rgba(s.ink2);
        let ink3 = slot_to_rgba(s.ink3);
        let show_add = g.is_terminals || g.hovered;
        let gid = g.id.clone();

        // Header: chevron + uppercase name (both toggle disclosure), count pill,
        // add button.
        let gid_chevron = gid.clone();
        let gid_name = gid.clone();
        let gid_add = gid.clone();
        let is_terminals = g.is_terminals;

        let mut header = div()
            .id(SharedString::from(format!("sidebar.group.{}.header", g.id)))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(16.0))
            .py(px(4.0))
            .on_hover(cx.listener({
                let gid = gid.clone();
                move |this, hovering: &bool, _w, cx| {
                    this.hovered_project = hovering.then(|| gid.clone());
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let gid = gid.clone();
                    move |this, e: &MouseDownEvent, window, cx| {
                        this.open_project_context_menu(&gid, is_terminals, e.position, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink2)
                    .child(SharedString::from(disclosure_glyph(g.is_open)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_disclosure(&gid_chevron, cx);
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink2)
                    .child(SharedString::from(g.name.to_uppercase()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_disclosure(&gid_name, cx);
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(
                // Count pill.
                div()
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded_full()
                    .bg(ink_alpha(s, COUNT_PILL_INK_ALPHA))
                    .text_size(px(10.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ink3)
                    .child(SharedString::from(g.count.to_string())),
            );

        if show_add {
            let add_hover = ink_alpha(s, ADD_BUTTON_HOVER_ALPHA);
            // 10pt semibold `plus` in an 18pt box (`SidebarView.swift:379-383`).
            let add_icon = sf_symbol_icon(
                SF_PLUS,
                ICON_PLUS,
                10.0,
                SymbolWeight::Semibold,
                ink2,
                self.window_scale,
                cx,
            );
            header = header.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .hover(move |st| st.bg(add_hover))
                    .child(add_icon)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                            this.add_tab_in_group(&gid_add, is_terminals, cx);
                            cx.stop_propagation();
                        }),
                    ),
            );
        }

        let rows: Vec<gpui::AnyElement> = g
            .tabs
            .iter()
            .map(|t| self.build_tab_row(t, s, cx))
            .collect();

        div()
            .flex()
            .flex_col()
            .w_full()
            .pb(px(4.0))
            .child(header)
            .children(rows)
            .into_any_element()
    }

    fn build_tab_row(&self, t: &TabVm, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        let accent = crate::theme_settings::active_chrome_accent(cx);
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let ink3 = slot_to_rgba(s.ink3);
        let hover = ink_alpha(s, HOVER_INK_ALPHA);
        let indent = row_indent(t.indented);

        // Leading icon: the status dot for a Claude tab, else the `terminal`
        // symbol — 12pt regular ink3 in a 16pt box (`SidebarView.swift:602-607`).
        let leading = if t.has_claude {
            StatusDot::new(
                SharedString::from(t.id.clone()),
                t.status,
                accent,
                slot_srgba(s.ink3),
            )
            .suppress_waiting_pulse(t.waiting_ack)
            .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(16.0))
                .h(px(16.0))
                .child(sf_symbol_icon(
                    SF_TERMINAL,
                    ICON_TERMINAL,
                    12.0,
                    SymbolWeight::Regular,
                    ink3,
                    self.window_scale,
                    cx,
                ))
                .into_any_element()
        };

        // Title view: the inline-rename field while editing, else the label.
        let title: gpui::AnyElement = if t.is_editing {
            let spans = self
                .rename_editor
                .as_ref()
                .map(edit_spans)
                .unwrap_or_else(|| edit_spans(&TextFieldEditor::new("")));
            let colors = FieldColors {
                bg: slot_to_rgba(s.background3),
                border: slot_to_rgba(s.line_strong),
                text: ink,
                caret: srgba_to_rgba(accent),
                selection: selection_tint(accent, 1.0),
            };
            let weak = cx.weak_entity();
            rename_field(
                &self.rename_focus,
                &spans,
                "SidebarRename",
                colors,
                13.0,
                self.rename_probe.clone(),
                cx.listener(Self::on_rename_key),
                move |index, window, app| {
                    let _ = weak.update(app, |this, cx| this.place_rename_cursor(index, window, cx));
                },
            )
            .into_any_element()
        } else {
            let tid = t.id.clone();
            let is_active = t.is_active;
            div()
                .flex_1()
                .px(px(6.0))
                .py(px(2.0))
                .whitespace_nowrap()
                .truncate()
                .text_size(px(13.0))
                .font_weight(if is_active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(if is_active { ink } else { ink2 })
                .child(SharedString::from(t.title.clone()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                        if this.editing_tab_id.as_deref() == Some(tid.as_str()) {
                            cx.stop_propagation();
                            return;
                        }
                        if this.editing_tab_id.is_some() {
                            this.commit_rename(window, cx);
                        }
                        this.handle_title_tap(
                            &tid,
                            e.modifiers.platform,
                            e.modifiers.shift,
                            window,
                            cx,
                        );
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .into_any_element()
        };

        let tid_tap = t.id.clone();
        let tid_menu = t.id.clone();
        let is_active = t.is_active;
        let is_selected = t.is_selected;

        // The inner row (colored rounded rect), inset 6pt from the card edges.
        let inner = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(indent))
            .pr(px(10.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .when(is_active, |el| el.bg(selection_tint(accent, 1.0)))
            .when(!is_active && is_selected, |el| {
                el.bg(selection_tint(accent, SELECTED_DIM_FACTOR))
            })
            .when(!is_active && !is_selected, |el| {
                el.hover(move |st| st.bg(hover))
            })
            .child(leading)
            .child(title)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    if this.editing_tab_id.as_deref() == Some(tid_tap.as_str()) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.editing_tab_id.is_some() {
                        this.commit_rename(window, cx);
                    }
                    this.route_click(&tid_tap, e.modifiers.platform, e.modifiers.shift, cx);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    this.open_tab_context_menu(&tid_menu, e.position, window, cx);
                    cx.stop_propagation();
                }),
            );

        div().px(px(6.0)).w_full().child(inner).into_any_element()
    }

    /// The footer: a trailing Settings gear over a 1pt top rule. The gear is a
    /// disabled placeholder until R23 (the Settings window) — it renders dimmed
    /// and does nothing.
    fn build_footer(&self, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        // 14pt regular `gearshape`, dimmed to ink3 while disabled.
        let gear = sf_symbol_icon(
            SF_GEAR,
            ICON_GEAR,
            14.0,
            SymbolWeight::Regular,
            slot_to_rgba(s.ink3),
            self.window_scale,
            cx,
        );
        div()
            .relative()
            .flex()
            .flex_row()
            .justify_center()
            .items_center()
            .w_full()
            .px(px(8.0))
            .py(px(6.0))
            .child(
                // 1pt top rule.
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(px(1.0))
                    .bg(slot_to_rgba(s.line)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .w_full()
                    .child(
                        // Disabled Settings gear (R23 wires the Settings window).
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(24.0))
                            .h(px(24.0))
                            .rounded(px(INNER_CORNER_RADIUS))
                            .child(gear),
                    ),
            )
    }

    fn build_resize_handle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Invisible 6pt zone straddling the trailing edge (3pt inside / 3pt in
        // the gap), cursor-flip for discoverability. Drag resizes (root-level
        // move/up); double-click resets to 240 (`AppShellView.swift:848-887`).
        div()
            .absolute()
            .top_0()
            .h_full()
            .right(px(-SIDEBAR_RESIZE_HANDLE_WIDTH / 2.0))
            .w(px(SIDEBAR_RESIZE_HANDLE_WIDTH))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_resize_mouse_down))
    }

    /// The full-width 1px title-bar divider (M2 feel-check Item C): one
    /// hairline (`s.line`) spanning the whole window at the bottom of the 52pt
    /// band, in both shell states — it replaces the toolbar's old local bottom
    /// border, which stopped at the sidebar edge. The callers order it OVER the
    /// band/body chrome but UNDER the floating sidebar card / peek overlay, so
    /// the expanded card deliberately overlaps it (the line stays visible in
    /// the gutters). Pure paint: no id, no listeners, no hitbox.
    fn build_top_bar_divider(&self, s: &Slots) -> impl IntoElement {
        div()
            .absolute()
            .left_0()
            .top(px(TOP_BAR_HEIGHT - 1.0))
            .w_full()
            .h(px(1.0))
            .bg(slot_to_rgba(s.line))
    }

    /// The content column to the right of (expanded) or beneath (collapsed) the
    /// sidebar. A plain background when no content is injected (the isolated
    /// `sidebar` scenario); R13.5's composed shell replaces it with the toolbar
    /// band + pane-content host via the [`main_toolbar`](Self::main_toolbar) /
    /// [`main_body`](Self::main_body) slots.
    fn build_content(&self, cx: &App) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .size_full()
            .bg(slot_to_rgba(active_slots(cx).background))
    }

    /// The expanded shell's right column: the toolbar band stacked over the pane
    /// body (Swift's `VStack { WindowToolbarView ; mainContent }`). When no
    /// content is injected this is the placeholder [`build_content`](Self::build_content)
    /// verbatim, so the isolated `sidebar` scenario's layout is unchanged.
    fn build_main_column(&self, cx: &App) -> gpui::AnyElement {
        if self.main_toolbar.is_none() && self.main_body.is_none() {
            return self.build_content(cx).into_any_element();
        }
        let mut col = div().flex().flex_col().flex_1().min_w_0().h_full();
        if let Some(toolbar) = &self.main_toolbar {
            col = col.child(toolbar.clone());
        }
        col.child(self.build_main_body(cx)).into_any_element()
    }

    /// The collapsed shell's top-row accessory beside the cap: the toolbar band
    /// when composed (as a flex-filling wrapper so the toolbar's own `w_full`
    /// resolves against the remaining row width), else the R9/R11 chrome filler
    /// the isolated scenario shows.
    fn build_collapsed_top_accessory(&self, s: &Slots) -> gpui::AnyElement {
        if let Some(toolbar) = &self.main_toolbar {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(toolbar.clone())
                .into_any_element()
        } else {
            div()
                .flex_1()
                .h_full()
                .bg(slot_to_rgba(s.chrome))
                .into_any_element()
        }
    }

    /// The pane content fill below the toolbar / cap row: the injected pane-host
    /// when composed, else the placeholder [`build_content`](Self::build_content).
    fn build_main_body(&self, cx: &App) -> gpui::AnyElement {
        if let Some(body) = &self.main_body {
            div()
                .flex_1()
                .min_h_0()
                .child(body.clone())
                .into_any_element()
        } else {
            self.build_content(cx).into_any_element()
        }
    }

    /// The peek overlay: the full sidebar card floating over the collapsed
    /// content at top-leading, staying open while the cursor pins it
    /// (`AppShellView.swift:908-923`). R12 sets `SidebarModel::peeking`; this
    /// renders it and OR's in the hover pin. `peeking` (the caller's effective
    /// predicate) is threaded through to the card body so it always shows tabs.
    fn build_peek_overlay(
        &self,
        groups: &[GroupVm],
        peeking: bool,
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("sidebar.peek")
            .absolute()
            .top_0()
            .left_0()
            .h_full()
            .on_hover(cx.listener(|this, hovering: &bool, _w, cx| {
                this.peek_mouse_pinned = *hovering;
                cx.notify();
            }))
            .child(self.build_sidebar_card(groups, false, peeking, mode, cx))
    }
}

// ---- Scenario accessors -----------------------------------------------------
//
// Read/drive surface the live `sidebar` self-test scenario (`crate::sidebar_live`)
// uses to ground-truth the shell against AppKit reads. All `pub(crate)` and
// side-effect-free except the collapse driver, which routes through the real
// [`SidebarShellView::toggle_collapsed`] path (peek-clear included) so the
// scenario exercises the shipped toggle, not a shortcut.
impl SidebarShellView {
    /// The current docked sidebar width (the resize target the scenario clamps).
    pub(crate) fn sidebar_width(&self) -> f32 {
        self.sidebar_width
    }

    /// Whether the sidebar is collapsed (drives the band-vs-column assertion).
    pub(crate) fn is_collapsed(&self, cx: &App) -> bool {
        self.state.read(cx).sidebar.collapsed()
    }

    /// Drive the real collapse toggle (used to enter / leave the collapsed
    /// full-width-band state in the scenario).
    pub(crate) fn drive_toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.toggle_collapsed(cx);
    }

    /// The `(status, waiting_acknowledged)` pair the row would feed its
    /// [`StatusDot`] for `tab_id` — the R8 predicates read straight off the model,
    /// never recomputed. `None` if the tab is unknown. The scenario asserts the
    /// dot colour + pulse rule against these.
    pub(crate) fn tab_dot_inputs(&self, tab_id: &str, cx: &App) -> Option<(TabStatus, bool)> {
        self.state
            .read(cx)
            .model
            .tab_for(tab_id)
            .map(|t| (t.status(), t.waiting_acknowledged()))
    }

    /// The user's accent (thinking-dot colour) — resolved once at construction.
    pub(crate) fn accent(&self) -> Srgba {
        self.accent
    }

    /// The width the shell sizes its leading column to right now — the docked card
    /// width (`sidebar_width`) when expanded, and **0 when collapsed**: the M2
    /// collapsed design reserves no leading column at all (the floating cap is
    /// gone; the top row is one full-width band — see
    /// [`build_collapsed_shell`](Self::build_collapsed_shell)). This is the
    /// *intended* column width, re-derived from the collapse flag (and the
    /// `sidebar_width` field), NOT a read of the rendered element's laid-out
    /// `Bounds`. The `app-shell` scenario samples it across a ⌘B toggle to
    /// confirm 240 → 0 → 240; because that scenario never resizes, the change
    /// follows from the collapse flag rather than an independent layout
    /// measurement.
    pub(crate) fn scenario_leading_column_width(&self, cx: &App) -> f32 {
        if self.is_collapsed(cx) {
            0.0
        } else {
            self.sidebar_width
        }
    }

    /// Begin an inline rename of the ACTIVE tab through the real path (the
    /// gate-passed title tap and the context-menu Rename Tab entry both land in
    /// `begin_editing`) — the `app-shell` scenario's focus-routing driver.
    pub(crate) fn drive_begin_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.state.read(cx).model.active_tab_id().map(str::to_owned) else {
            return;
        };
        self.begin_editing(&tab_id, window, cx);
    }

    /// Whether an inline tab rename is in flight.
    pub(crate) fn scenario_tab_rename_editing(&self) -> bool {
        self.editing_tab_id.is_some()
    }

    /// The in-flight tab-rename draft (the scenario's "keys land in the field"
    /// read).
    pub(crate) fn scenario_tab_rename_draft(&self) -> String {
        self.rename_editor.as_ref().map(|e| e.text()).unwrap_or_default()
    }

    /// The in-flight tab-rename selection `(start, end)` as char offsets — the
    /// scenario asserts caret moves / mid-string edits through it.
    pub(crate) fn scenario_tab_rename_selection(&self) -> Option<(usize, usize)> {
        self.rename_editor.as_ref().map(|e| e.selection())
    }

    /// Move the tab-rename caret one char left/right (the scenario's arrow-key
    /// driver — direct so it needn't post an arrow CGEvent).
    pub(crate) fn drive_tab_rename_arrow(&mut self, right: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.rename_editor.as_mut() {
            editor.apply_key(if right {
                nice_model::file_browser::TextFieldKey::Right
            } else {
                nice_model::file_browser::TextFieldKey::Left
            });
            cx.notify();
        }
    }

    /// Whether the tab-rename field currently holds key focus.
    pub(crate) fn scenario_tab_rename_focused(&self, window: &Window) -> bool {
        self.rename_focus.is_focused(window)
    }

    /// The files-mode browser view, if it has been created (the sidebar entered
    /// files mode at least once) — the `file-browser` scenario's handle onto the
    /// mounted tree.
    pub(crate) fn scenario_file_browser(&self) -> Option<Entity<FileBrowserView>> {
        self.file_browser.clone()
    }
}

impl Focusable for SidebarShellView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SidebarShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Re-sample the backing scale so the SF Symbol cache renders (and hits)
        // at this window's device resolution.
        self.window_scale = window.scale_factor();
        // Chrome-click focus bounce (M2 Item D, installed once — it needs a
        // `Window`, which `new` doesn't have): a click on empty shell chrome
        // focuses this root via gpui's tracked-focus transfer; hand it straight
        // back to the active terminal so chrome never keeps key focus. A rename
        // begin never lands here (the field's own handle takes focus, not this
        // root), so the bounce cannot fight the rename field.
        if self.focus_bounce_sub.is_none() {
            self.focus_bounce_sub = Some(cx.on_focus(&self.focus_handle, window, |this, window, cx| {
                this.refocus_terminal_after_rename(window, cx);
            }));
        }
        let (collapsed, mode, peeking_model) = {
            let ws = self.state.read(cx);
            (ws.sidebar.collapsed(), ws.sidebar.mode(), ws.sidebar.peeking())
        };
        // R19: mint the files-mode browser view the first time we enter files mode
        // (kept afterwards; one kqueue watcher per window, spawned on demand).
        if mode == SidebarMode::Files && self.file_browser.is_none() {
            let state = self.state.clone();
            let accent = crate::theme_settings::active_chrome_accent(cx);
            let fb = cx.new(|cx| FileBrowserView::new(state, accent, cx));
            // R20 (F8): push the pane host down so a rename exit hands key focus
            // back to the active terminal (set_pane_host runs before first render).
            if let Some(host) = self.pane_host.clone() {
                fb.update(cx, |fb, _| fb.set_pane_host(host));
            }
            self.file_browser = Some(fb);
        }
        let groups = self.snapshot_groups(cx);
        let shell = if collapsed {
            self.build_collapsed_shell(groups, mode, peeking_model, cx)
                .into_any_element()
        } else {
            self.build_expanded_shell(groups, mode, cx).into_any_element()
        };
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("SidebarShell")
            .on_action(cx.listener(Self::on_collapse_esc))
            .on_mouse_move(cx.listener(Self::on_root_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_root_mouse_up))
            .child(shell)
            .children(self.context_menu.clone())
    }
}

/// Install the sidebar's Esc key binding once (from app startup, like R9's
/// `install_fullscreen_command`). Bound in the `SidebarShell` key context so the
/// action reaches the shell view even while the terminal holds focus; the
/// handler collapses a >1 selection (or cancels a rename) and otherwise
/// `cx.propagate()`s so Esc still reaches the terminal.
pub(crate) fn install_sidebar_key_bindings(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        CollapseSidebarSelection,
        Some("SidebarShell"),
    )]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_theme::palette::{slots, ColorScheme, Palette};
    use nice_theme::AccentPreset;

    #[test]
    fn clamp_sidebar_width_bounds_at_160_and_480() {
        assert_eq!(clamp_sidebar_width(100.0), SIDEBAR_MIN_WIDTH);
        assert_eq!(clamp_sidebar_width(600.0), SIDEBAR_MAX_WIDTH);
        assert_eq!(clamp_sidebar_width(300.0), 300.0);
        assert_eq!(SIDEBAR_MIN_WIDTH, 160.0);
        assert_eq!(SIDEBAR_MAX_WIDTH, 480.0);
    }

    #[test]
    fn resize_width_applies_delta_then_clamps() {
        assert_eq!(resize_width(240.0, 60.0), 300.0);
        assert_eq!(resize_width(240.0, -200.0), SIDEBAR_MIN_WIDTH, "clamps low");
        assert_eq!(resize_width(240.0, 400.0), SIDEBAR_MAX_WIDTH, "clamps high");
    }

    #[test]
    fn row_indent_matches_lineage() {
        assert_eq!(row_indent(false), 22.0); // SidebarView.swift:619
        assert_eq!(row_indent(true), 38.0);
    }

    #[test]
    fn close_menu_label_pluralizes_on_multi() {
        assert_eq!(close_menu_label(1), "Close Tab");
        assert_eq!(close_menu_label(3), "Close 3 Tabs");
        assert_eq!(close_menu_label(2), "Close 2 Tabs");
    }

    #[test]
    fn disclosure_glyph_swaps_on_open() {
        assert_eq!(disclosure_glyph(true), ICON_CHEVRON_OPEN);
        assert_eq!(disclosure_glyph(false), ICON_CHEVRON_CLOSED);
        assert_ne!(disclosure_glyph(true), disclosure_glyph(false));
    }

    #[test]
    fn sidebar_background_is_flat_nice_bg2() {
        // The SidebarBackground seam paints the active palette's `background2` as a
        // flat panel; with the Nice/Dark reference table it is the flat niceBg2
        // panel (SidebarBackground.swift:26-27).
        let s = slots(Palette::Nice, ColorScheme::Dark)
            .expect("Nice + Dark is a valid palette/scheme combo");
        let bg = sidebar_background(&s);
        let want = slot_srgba(s.background2);
        assert_eq!((bg.r, bg.g, bg.b), (want.r, want.g, want.b));
    }

    #[test]
    fn selection_tint_dims_by_factor() {
        let accent = AccentPreset::Terracotta.color();
        let active = selection_tint(accent, 1.0);
        let dimmed = selection_tint(accent, SELECTED_DIM_FACTOR);
        assert_eq!(active.a, SEL_ALPHA_DARK);
        assert_eq!(dimmed.a, SEL_ALPHA_DARK * 0.5);
        // Same hue, different alpha (rgb carried straight from the accent).
        assert_eq!((active.r, active.g, active.b), (dimmed.r, dimmed.g, dimmed.b));
    }
}
