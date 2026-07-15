//! The window's slim titlebar + pane strip — originally ported from
//! `Sources/Nice/Views/WindowToolbarView.swift` (`WindowToolbarView`,
//! `InlinePaneStrip`, `InlinePanePill`, `CloseXButton`, `OverflowMenuButton`,
//! `NewTabBtn`). The 2026-07 restyle reshaped it (plan
//! `docs/plans/restyle/01-titlebar-restyle.md`) into the fill-less 28pt titlebar: the
//! leading sidebar-collapse toggle (an embedded stroke SVG, right of the native
//! traffic lights), then the horizontally-scrolling row of tabs — plain
//! text + a 1px accent underline on the active tab and a 1px grey underline on
//! every inactive tab, no pills — with the overflow
//! chevron (attention badge + edge fades) and the trailing `+` intact. When the
//! strip holds exactly one pane, single-tab mode renders the sole pane's title as
//! centered titlebar text (no tab chrome) instead. It is the
//! full-width titlebar row in both shell states, carries the window drag region,
//! and drives the R8 model through the injected [`PaneStripActions`] seam. The
//! brand block was removed.
//!
//! ## Real layout replaces the width estimator (binding decision)
//!
//! The Swift version shipped `PaneStripOverflowEstimator` because SwiftUI
//! virtualizes off-screen content and stops reporting frames for scrolled-out
//! pills. GPUI reads **real** layout: the pill row is tracked by a
//! [`gpui::ScrollHandle`], so
//!
//!   * the overflow chevron exists iff `>= 2` panes **and** the handle's
//!     `max_offset().x > 0` ([`nice_model::should_show_overflow_chevron`]),
//!     measured against a viewport whose width is fixed by two always-reserved
//!     trailing slots (chevron + `+`) so showing the chevron can never
//!     retroactively un-overflow the row (the reservation rule that survives the
//!     dead estimator);
//!   * the edge fades and the offscreen-pane set come from
//!     [`nice_model::StripGeometry`] fed by each pill's real
//!     `bounds_for_item(ix)`, translated into the viewport's `[0, visible_width]`
//!     space ([`viewport_relative_rect`]);
//!   * activating a pane auto-scrolls its pill to center via
//!     [`nice_model::center_offset_x`] + `set_offset` (`scroll_to_item` only
//!     reveals).
//!
//! No `PaneStripOverflowEstimator`, no merge-not-replace frame cache, no
//! GeometryReader fallback, and **no drag plumbing** — pill drag / reorder /
//! tear-off is R25, and the trailing update pill is R27.
//!
//! ## Shared per-window state + transient view state (the GPUI shape)
//!
//! Like [`crate::sidebar_shell::SidebarShellView`], the *document* state — the
//! [`TabModel`] and the pane-strip select/close/add seam — lives in the shared
//! per-window [`WindowState`] entity this view holds a handle to and renders
//! from / mutates (R13.5's "one `TabModel` per window" invariant: no divergent
//! model copy in any mounted view). What the view still owns is the transient
//! per-view state (the scroll handle, hovered pill, inline-rename draft, the open
//! context menu). A sibling holder of that same entity — the keymap's
//! window-scoped pane actions (⌘T, pane-step) routed through the `WindowRegistry`
//! — mutating it re-renders this view through the `cx.observe` subscription set
//! in [`new`](WindowToolbarView::new). The pills / buttons are built by helper
//! methods rather than child entities so their handlers reach this state through
//! `cx.listener` — no cross-element interaction flags (the R9 anti-pattern);
//! state is recomputed per event.

// No in-crate caller wires this view until slice 3 adds the `pane-strip` self-test
// scenario; it is a deliberately-exported surface (plan "Exported contracts"). The
// pure label / geometry helpers below ARE exercised by this module's unit tests.
#![allow(dead_code)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    div, linear_color_stop, linear_gradient, point, prelude::*, px, App, Bounds,
    ClickEvent, Context, DismissEvent, DragMoveEvent, Entity, FocusHandle, Focusable, FontWeight,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollHandle,
    SharedString, Subscription, Window,
};

use nice_model::file_browser::TextFieldEditor;
use nice_model::{
    center_offset_x, resolve, should_show_overflow_chevron, Pane, PaneKind, Rect, StripGeometry,
    Tab, TabStatus,
};
use nice_theme::chrome_geometry::{traffic_light_reserved_width, TOP_BAR_HEIGHT};
use nice_theme::palette::Slots;

use crate::app_shell::{PaneHostView, PANE_STRIP_ROOT_LABEL};
use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::inline_rename::{
    apply_rename_click, dispatch_rename_key, edit_spans, rename_field, FieldColors, FieldProbe,
    RenameKeyOutcome,
};
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::status_dot::StatusDot;
use crate::theme::{slot_srgba, slot_to_rgba, srgba_to_rgba, srgba_with_alpha};
use crate::update_popover::UpdatePopover;
use crate::window_state::WindowState;

// ---- Geometry / behaviour constants (Swift provenance) ----------------------

/// Toolbar trailing inset — the mock's `.bar-tail { padding: 0 10px }`
/// (`docs/design/restyle-mocks.html`), so the `+` button's right edge sits
/// 10pt off the window corner like the mock. Was 20 (the Swift-era
/// `WindowToolbarView.swift:57` inset, sized for the retired 52pt bar), which
/// left the `+` visibly adrift of the corner — the round-2 feel-check's
/// "plus not aligned like the mock" finding: the offset was HORIZONTAL, not
/// vertical (the glyph's ink center measures level with the title text).
pub(crate) const TOOLBAR_TRAILING_PAD: f32 = 10.0;

/// Tab box: full-height (the bar), so the active-tab underline seats on the bar's
/// bottom edge (`docs/design/restyle-mocks.html`, `.tabs { height: 100% }`; plan
/// `docs/plans/restyle/01-titlebar-restyle.md`). Equals [`TOP_BAR_HEIGHT`].
const PILL_HEIGHT: f32 = TOP_BAR_HEIGHT;
/// Tab max width before tail-ellipsis (mock `.tab { max-width: 200px }`; was 220).
const PILL_MAX_WIDTH: f32 = 200.0;
/// Tab horizontal padding (mock `.tab { padding: 0 12px }`).
const TAB_PAD_X: f32 = 12.0;
/// Tab inner spacing — dot ↔ title ↔ ✕ (mock `.tab { gap: 7px }`).
const PILL_GAP: f32 = 7.0;
/// Tab title text size (mock `.tab { font-size: 12px }`).
const PILL_TEXT_SIZE: f32 = 12.0;
/// Leading terminal-glyph box for a Terminal pane's tab.
const PILL_ICON_SIZE: f32 = 12.0;
/// Status-dot diameter inside a tab (mock `.tab .dot { width: 6px }`; the
/// [`StatusDot`] component's 8pt default stays elsewhere — only its size
/// parameter changes here, colours + pulse untouched).
const TAB_STATUS_DOT_SIZE: f32 = 6.0;
/// Tab underline: 1px tall, seated on the bar's bottom edge, inset 11px from the
/// tab's outer edges, 0.5px corner radius (mock Style A `.tab::after` /
/// `.tab.active::after` — round-2 thinned both underlines from 2px to 1px). The
/// active tab wears it in the accent; every inactive tab wears it in a grey
/// ([`nice_theme::tab_underline_idle`]) so it reads as clickable (round-2 plan 4
/// "Inactive-tab underline": grammar underline = tab, color = state).
const TAB_UNDERLINE_HEIGHT: f32 = 1.0;
const TAB_UNDERLINE_INSET: f32 = 11.0;
const TAB_UNDERLINE_RADIUS: f32 = 0.5;

/// Single-tab mode: when the strip holds exactly one pane, its title + status
/// dot render as the window's centered titlebar text (macOS window-title
/// convention) instead of a tab box. The centered text is an absolute overlay
/// inset this far from BOTH window edges (symmetric, so it stays centered on the
/// window's true center) — enough to clear the traffic-light cluster on the left
/// and the trailing `+` on the right (mock `.tab-single { left: 90px; right:
/// 90px }`; round-2 plan 4 "Single-tab mode"). The title clamps/ellipsizes
/// within this box so it never collides with either.
const SINGLE_TAB_EDGE_INSET: f32 = 90.0;

/// The sidebar-collapse toggle box in the titlebar (mock `.tb-btn`: 24×22, 5px
/// radius, 4px trailing margin before the tabs).
const COLLAPSE_BTN_W: f32 = 24.0;
const COLLAPSE_BTN_H: f32 = 22.0;
const COLLAPSE_BTN_RADIUS: f32 = 5.0;
const COLLAPSE_BTN_TRAILING_MARGIN: f32 = 4.0;
/// The collapse toggle's hover fill: 8% ink (the chrome icon-button tier).
const COLLAPSE_BTN_HOVER_INK_ALPHA: f32 = 0.08;
/// Inter-pill spacing inside the scroll row (`HStack(spacing: 2)`,
/// `WindowToolbarView.swift:292`).
const PILL_ROW_GAP: f32 = 2.0;

/// The trailing update pill's AX element id (`UpdateAvailablePill.swift:58`).
/// Distinct from the per-pane pills' ids and the status-dot's `pill.<id>` so an
/// AX walk / a scenario can find it unambiguously (R27, the reserved trailing
/// slot).
const UPDATE_PILL_ID: &str = "toolbar.updateAvailable";
/// The trailing update pill's text + AX title (`UpdateAvailablePill.swift:41`).
const UPDATE_PILL_LABEL: &str = "Update available";
/// The pill's leading SF Symbol (`UpdateAvailablePill.swift:36`).
const SF_ARROW_UP_CIRCLE: &str = "arrow.up.circle.fill";
/// Never-blank fallback for [`SF_ARROW_UP_CIRCLE`].
const ICON_ARROW_UP: &str = "\u{2191}"; // ↑

/// The close "×" square (`WindowToolbarView.swift:987`). Its 16pt slot is always
/// reserved so the pill width never jumps on hover.
const CLOSE_BTN_SIZE: f32 = 16.0;
const CLOSE_BTN_RADIUS: f32 = 4.0;
const CLOSE_GLYPH_SIZE: f32 = 9.0;

/// The overflow chevron / new-tab button box (`WindowToolbarView.swift:1048,1137`).
pub(crate) const SQUARE_BTN_SIZE: f32 = 22.0;
const SQUARE_BTN_RADIUS: f32 = 5.0;
const CHEVRON_GLYPH_SIZE: f32 = 10.0;
const PLUS_GLYPH_SIZE: f32 = 11.0;
/// The chevron / new-tab leading pad inside their slot (`.padding(.leading, 4)`,
/// `WindowToolbarView.swift:238,245`).
pub(crate) const SQUARE_BTN_LEADING_PAD: f32 = 4.0;
/// Width of the chevron slot and the `+` slot — each **always** reserved in the
/// tracked scroll layout so the pill viewport is a fixed width and the overflow
/// decision never depends on the chevron's own visibility (the reservation rule,
/// `PaneStripOverflowEstimator.swift:59-65`: 22 button + 4 lead + 2 gap ≈ 28).
pub(crate) const SQUARE_SLOT_WIDTH: f32 = SQUARE_BTN_LEADING_PAD + SQUARE_BTN_SIZE + PILL_ROW_GAP;

/// The chevron's 6pt attention badge (`WindowToolbarView.swift:1061`).
const ATTENTION_BADGE_SIZE: f32 = 6.0;
/// The 16pt edge-fade gradient width (`WindowToolbarView.swift:452`).
const EDGE_FADE_WIDTH: f32 = 16.0;

/// Rename gate: the same click that selects a pill must not also start a rename,
/// so the title tap only edits once this interval has elapsed since the pill
/// became active (`NSEvent.doubleClickInterval` default,
/// `WindowToolbarView.swift:746`).
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// Window-drag start threshold on the empty band, in pt — parity with the R9
/// band (`ChromeEventRouter.swift:218`).
const BAND_DRAG_THRESHOLD_PX: f32 = 2.0;

/// Update-pill hover fill: 5% ink (`WindowToolbarView.swift:715`).
const PILL_HOVER_INK_ALPHA: f32 = 0.05;
/// Close-"×" hover fill: 10% ink (`WindowToolbarView.swift:992`).
const CLOSE_HOVER_INK_ALPHA: f32 = 0.10;
/// Chevron / new-tab hover fill: 8% ink (`WindowToolbarView.swift:1054,1143`).
const SQUARE_BTN_HOVER_INK_ALPHA: f32 = 0.08;

// ---- Icons (SF Symbols + Unicode fallbacks / stand-ins) ----------------------
//
// The tab/chevron/plus/close icons are real SF Symbols rendered through
// `crate::sf_symbols` (M2 feel-check Item A); each ICON_* glyph remains as the
// never-blank fallback. The overflow-menu rows keep their glyph stand-ins (the
// pinned `ContextMenu` is plain-label). The leading sidebar-collapse toggle is
// the restyle's stroke SVG (`crate::chrome_icons`), not an SF Symbol.

const ICON_TERMINAL: &str = "\u{276F}"; // ❯  fallback for SF_TERMINAL + menu rows
const ICON_CLOSE: &str = "\u{2715}"; // ✕  fallback for SF_CLOSE
const ICON_CHEVRON_DOWN: &str = "\u{25BE}"; // ▾  fallback for SF_CHEVRON_DOWN
const ICON_PLUS: &str = "+"; // fallback for SF_PLUS
const ICON_CHECK: &str = "\u{2713}"; // ✓  (menu-row stand-in, SF "checkmark")
const ICON_CLAUDE_DOT: &str = "\u{25CF}"; // ●  (menu-row stand-in for the StatusDot)

/// Pill leading icon (`WindowToolbarView.swift:903-906`).
const SF_TERMINAL: &str = "terminal";
/// Pill close button (`WindowToolbarView.swift:984-986`).
const SF_CLOSE: &str = "xmark";
/// Overflow chevron (`WindowToolbarView.swift:1045-1047`).
const SF_CHEVRON_DOWN: &str = "chevron.down";
/// New-tab button (`WindowToolbarView.swift:1134-1136`).
const SF_PLUS: &str = "plus";

// ---- Pure helpers (unit-tested; no gpui) ------------------------------------

/// The per-kind context-menu **rename** label (`WindowToolbarView.swift:751`).
fn rename_menu_label(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "Rename Terminal",
        PaneKind::Claude => "Rename Pane",
    }
}

/// The per-kind context-menu **close** label (`WindowToolbarView.swift:755`).
fn close_menu_label(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "Close Terminal",
        PaneKind::Claude => "Close Pane",
    }
}

/// One overflow-menu row's label: a kind/status glyph, the title, and a trailing
/// checkmark on the active pane (`WindowToolbarView.swift:1079-1097`). The pinned
/// `ContextMenu` component is plain-label, so the leading `StatusDot` / terminal
/// icon and the active checkmark are rendered as glyph stand-ins (same philosophy
/// as the sidebar's Unicode icons) — the itest-pinned facts are "lists every
/// pane" and "checkmark on the active row."
fn overflow_row_label(pane: &Pane, active_pane_id: Option<&str>) -> String {
    let icon = match pane.kind {
        PaneKind::Claude => ICON_CLAUDE_DOT,
        PaneKind::Terminal => ICON_TERMINAL,
    };
    let check = if active_pane_id == Some(pane.id.as_str()) {
        format!("  {ICON_CHECK}")
    } else {
        String::new()
    };
    format!("{icon}  {}{check}", pane.title)
}

/// Translate a scroll child's window-space bounds into the viewport-relative
/// `[0, visible_width]` space [`StripGeometry`] reads. GPUI records each child's
/// bounds **without** the current scroll offset applied, and a child's on-screen
/// left is `item_left + offset_x` (`elements/div.rs:2205`, `:3949-3953`), so its
/// position relative to the viewport's leading edge is
/// `item_left + offset_x - viewport_left`.
fn viewport_relative_rect(item_left: f32, item_width: f32, offset_x: f32, viewport_left: f32) -> Rect {
    Rect::new(item_left + offset_x - viewport_left, item_width)
}

/// Has a press→current displacement crossed the [`BAND_DRAG_THRESHOLD_PX`]
/// threshold (compared squared, `ChromeEventRouter.swift:218`)?
fn band_drag_threshold_crossed(dx: f32, dy: f32) -> bool {
    dx * dx + dy * dy >= BAND_DRAG_THRESHOLD_PX * BAND_DRAG_THRESHOLD_PX
}

// ---- Colour helpers (Nice/Dark; matches the shipped chrome band) -------------

/// The active chrome slot table — the live
/// [`SharedThemeState`](crate::theme_settings::SharedThemeState) (Nice/Dark
/// fallback when the theme global is absent). R21: was a fixed Nice/Dark table.
fn active_slots(cx: &App) -> Slots {
    crate::theme_settings::active_chrome_slots(cx)
}

/// The `ink` slot at straight alpha `a` — the translucent hover fills.
fn ink_alpha(s: &Slots, a: f32) -> Rgba {
    srgba_to_rgba(srgba_with_alpha(slot_srgba(s.ink), a))
}

// ---- View-model snapshot (decouples rendering from model borrows) -----------

/// A per-render snapshot of one pane pill.
struct PaneVm {
    id: String,
    title: String,
    kind: PaneKind,
    status: TabStatus,
    waiting_ack: bool,
    is_active: bool,
    is_hovered: bool,
    is_editing: bool,
}

// ---- Pill drag (R25 reorder) ------------------------------------------------

/// The value a pill drag carries: just the dragged pane id and the tab it lives
/// in (D3). `'static`, no pasteboard/string encoding — this is a purely in-app
/// `gpui` drag payload, the type gate `on_drop::<PaneDragPayload>` matches on.
/// Carrying `tab_id` makes the drop's `move_pane` robust to any active-tab change
/// mid-drag. R25 is reorder-within-one-strip only: the CUT cross-window path's
/// `PaneDragOrigin.sourceWindowSessionId` / `sourceIndex` are deliberately absent
/// (scope fence).
#[derive(Clone)]
struct PaneDragPayload {
    pane_id: SharedString,
    tab_id: SharedString,
}

/// The drag "ghost" that follows the cursor: a simplified pill chip (icon-less
/// title at the pill's radius/height, reduced opacity), NOT a bitmap snapshot
/// (D4). gpui lays the ghost out at `mouse - offset` each frame, so it
/// compensates by re-adding `offset` (plus a small lead) as leading padding.
struct PaneDragGhost {
    title: SharedString,
    /// The pointer's position within the dragged pill, captured at drag-arm time.
    /// gpui lays the ghost out at `mouse - offset`, so we re-add it (plus a small
    /// lead) as leading padding to net the ghost to `pointer + 12`.
    offset: Point<Pixels>,
}

impl Render for PaneDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = active_slots(cx);
        // Outer wrapper carries the offset compensation as padding so the visible
        // pill's own background box isn't inflated.
        div().pl(self.offset.x + px(12.0)).pt(self.offset.y + px(12.0)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(PILL_HEIGHT))
                .max_w(px(PILL_MAX_WIDTH))
                .px(px(TAB_PAD_X))
                .rounded(px(6.0))
                .bg(slot_to_rgba(s.panel))
                .border_1()
                .border_color(slot_to_rgba(s.line))
                .opacity(0.85)
                .text_size(px(PILL_TEXT_SIZE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(slot_to_rgba(s.ink))
                .whitespace_nowrap()
                .child(self.title.clone()),
        )
    }
}

// ---- Tab full-title tooltip -------------------------------------------------

/// The hover tooltip showing a tab's full title (NEW work for the restyle — a
/// tab tail-ellipsizes at [`PILL_MAX_WIDTH`], so the tooltip surfaces the rest).
/// A small themed label box built through gpui's `div::tooltip` seam.
struct TabTooltip {
    title: SharedString,
}

impl Render for TabTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = active_slots(cx);
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .text_size(px(PILL_TEXT_SIZE))
            .text_color(slot_to_rgba(s.ink))
            .whitespace_nowrap()
            .child(self.title.clone())
    }
}

// ---- The view ---------------------------------------------------------------

/// The per-window toolbar (the leading sidebar-collapse toggle + the pane
/// strip). Construct with [`WindowToolbarView::new`] over the window's shared
/// [`WindowState`] entity; it renders the shared `model`'s active-tab panes and
/// mutates them through the `pane_strip_actions` seam.
pub(crate) struct WindowToolbarView {
    /// The shared per-window state (the single [`TabModel`] plus the pane-strip
    /// select/close/add seam). This view renders the active tab's panes from it
    /// and mutates it through the seam; it never keeps a private model copy
    /// (R13.5's "one `TabModel` per window" invariant).
    state: Entity<WindowState>,
    /// Re-render this view whenever the shared state notifies — the seam through
    /// which the keymap's window-scoped pane actions (⌘T, pane-step) become
    /// visible in the strip. Held so the subscription lives as long as the view.
    _state_sub: Subscription,

    /// The pill (if any) the cursor is over, keyed by `Pane.id`. Lives in the
    /// container so only one close "×" is ever visible at a time
    /// (`WindowToolbarView.swift:169`).
    hovered_pane_id: Option<String>,

    /// The `(tab_id, pane_id)` currently being inline-renamed, if any.
    editing_pane: Option<(String, String)>,
    /// The in-flight rename editor (cursor + selection; `None` when not editing).
    rename_editor: Option<TextFieldEditor>,
    /// The rename field's painted geometry (text-run + field-box left edges,
    /// window coords), written by the field's layout probes each paint and read
    /// by its click-to-position handler.
    rename_probe: Rc<Cell<FieldProbe>>,
    /// When the active pane last changed — the rename gate reference.
    activated_at: Option<Instant>,
    /// Focus for the inline-rename field (grabbed on begin, released on commit).
    rename_focus: FocusHandle,
    /// Focus-out subscription committing the rename (the DO-NOT-PORT click-away
    /// monitor's replacement). Replaced on each `begin_editing`.
    rename_blur_sub: Option<Subscription>,

    /// The open pill / overflow context menu, if any.
    context_menu: Option<Entity<ContextMenu>>,
    /// The menu's dismiss subscription.
    menu_sub: Option<Subscription>,

    /// The trailing update-pill's popover, if open (R27, D9). The pill click
    /// presents it via `cx.defer_in`; a click-away / Esc / a second pill click
    /// drops it (the `context_menu` field pattern).
    update_popover: Option<Entity<UpdatePopover>>,
    /// The popover's dismiss subscription.
    update_popover_sub: Option<Subscription>,
    /// The update pill's last-painted window-content bounds, recorded by a canvas
    /// probe in [`render_update_pill`](Self::render_update_pill) — the
    /// `update-check` scenario reads it to target the pill's centre for the real
    /// guarded-HID click. `None` until the pill has painted.
    update_pill_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,

    /// The pill row's real-layout scroll state — the source of overflow / fades /
    /// centering (replaces the dead width estimator).
    scroll: ScrollHandle,
    /// The active pane id as of the last render — a change resets the rename gate
    /// and arms an auto-center.
    last_active_pane: Option<String>,
    /// Set when the active pane changed and the strip still owes it a center-on
    /// scroll; cleared once the centering offset is applied (needs a completed
    /// layout so `bounds_for_item` is populated).
    center_pending: bool,

    /// Empty-band window-drag press origin (R9 band pattern), not yet a drag.
    band_press: Option<Point<Pixels>>,

    /// The pill-reorder drop slot the cursor currently resolves to, already gated
    /// through [`TabModel::would_move_pane`] (a no-op slot resolves to `None`, D7).
    /// Recomputed in the scroll row's `on_drag_move` and cleared on drop / when the
    /// cursor leaves the strip (D8). The insertion line reads it, gated additionally
    /// on `cx.has_active_drag()` so a dropped-nowhere release drops the line the same
    /// frame (D10). `None` whenever no pill drag is in flight.
    drag_target: Option<(String, bool)>,
    /// Root focus handle (hosts the toolbar key context).
    focus_handle: FocusHandle,
    /// The window's pane-content host, wired by `crate::app::build_window_root`
    /// (M2 Item D): the seam through which the strip returns key focus to the
    /// active terminal after a rename commit/cancel and on menu dismissal.
    /// `None` in the isolated `pane-strip` scenario (refocus is then a no-op).
    pane_host: Option<Entity<PaneHostView>>,
    /// Chrome-click focus bounce (M2 Item D): a click on empty toolbar chrome
    /// transfers key focus to `focus_handle` via gpui's tracked-focus mouse-down
    /// transfer; this `on_focus` subscription bounces it straight back to the
    /// active terminal (chrome never keeps focus — Swift parity). Installed on
    /// the first render (the subscription needs a `Window`).
    focus_bounce_sub: Option<Subscription>,
    /// The window's backing scale factor, re-sampled at the top of every
    /// [`Render::render`] so the SF Symbol rasterizer draws at device
    /// resolution. The 2.0 initial value only covers code paths before the
    /// first render (none read it).
    window_scale: f32,
}

impl WindowToolbarView {
    /// A toolbar over the window's shared [`WindowState`], nothing hovered or
    /// editing. Observing the state re-renders the strip when a sibling holder
    /// (the keymap) mutates the shared model. The accent is read live per frame
    /// from the shared theme state during render, not cached on the view.
    pub(crate) fn new(state: Entity<WindowState>, cx: &mut Context<Self>) -> Self {
        let state_sub = cx.observe(&state, |_this, _state, cx| cx.notify());
        Self {
            state,
            _state_sub: state_sub,
            hovered_pane_id: None,
            editing_pane: None,
            rename_editor: None,
            rename_probe: Rc::new(Cell::new(FieldProbe::default())),
            activated_at: Some(Instant::now()),
            rename_focus: cx.focus_handle(),
            rename_blur_sub: None,
            context_menu: None,
            menu_sub: None,
            update_popover: None,
            update_popover_sub: None,
            update_pill_bounds: Rc::new(Cell::new(None)),
            scroll: ScrollHandle::new(),
            last_active_pane: None,
            center_pending: false,
            band_press: None,
            drag_target: None,
            focus_handle: cx.focus_handle(),
            pane_host: None,
            focus_bounce_sub: None,
            window_scale: 2.0,
        }
    }

    /// Wire the window's pane host (called once by `build_window_root`) so the
    /// strip can return key focus to the active terminal (M2 Item D).
    pub(crate) fn set_pane_host(&mut self, host: Entity<PaneHostView>) {
        self.pane_host = Some(host);
    }

    // MARK: - Model access / snapshot

    /// The active tab — the one whose panes the strip renders. The returned
    /// borrow is tied to `cx` (the shared model lives in the [`WindowState`]
    /// entity), so callers read it and drop the borrow before mutating.
    fn active_tab<'a>(&self, cx: &'a App) -> Option<&'a Tab> {
        let ws = self.state.read(cx);
        let id = ws.model.active_tab_id()?;
        ws.model.tab_for(id)
    }

    /// The active tab's id (owned), if any.
    fn active_tab_id(&self, cx: &App) -> Option<String> {
        self.state.read(cx).model.active_tab_id().map(|s| s.to_string())
    }

    /// A per-render snapshot of the active tab's pills.
    fn snapshot_panes(&self, cx: &App) -> Vec<PaneVm> {
        let Some(tab) = self.active_tab(cx) else {
            return Vec::new();
        };
        let active = tab.active_pane_id.clone();
        let editing = self.editing_pane.as_ref().map(|(_, p)| p.clone());
        tab.panes
            .iter()
            .map(|p| PaneVm {
                id: p.id.clone(),
                title: p.title.clone(),
                kind: p.kind,
                status: p.status,
                waiting_ack: p.waiting_acknowledged,
                is_active: active.as_deref() == Some(p.id.as_str()),
                is_hovered: self.hovered_pane_id.as_deref() == Some(p.id.as_str()),
                is_editing: editing.as_deref() == Some(p.id.as_str()),
            })
            .collect()
    }

    // MARK: - Overflow / fades / attention (real layout)

    /// The pill row's real geometry: each pane's viewport-relative rect + the
    /// viewport width, fed to [`StripGeometry`] for the fades and offscreen set.
    fn strip_geometry(&self, cx: &App) -> StripGeometry {
        let viewport = self.scroll.bounds();
        let viewport_left = f32::from(viewport.origin.x);
        let visible_width = f32::from(viewport.size.width);
        let offset_x = f32::from(self.scroll.offset().x);
        let mut frames = HashMap::new();
        if let Some(tab) = self.active_tab(cx) {
            for (ix, pane) in tab.panes.iter().enumerate() {
                if let Some(b) = self.scroll.bounds_for_item(ix) {
                    frames.insert(
                        pane.id.clone(),
                        viewport_relative_rect(
                            f32::from(b.origin.x),
                            f32::from(b.size.width),
                            offset_x,
                            viewport_left,
                        ),
                    );
                }
            }
        }
        StripGeometry::new(frames, visible_width)
    }

    /// Whether the overflow chevron should render — the `>= 2` panes + reserved
    /// real-overflow rule.
    fn show_chevron(&self, cx: &App) -> bool {
        let pane_count = self.active_tab(cx).map(|t| t.panes.len()).unwrap_or(0);
        should_show_overflow_chevron(pane_count, f32::from(self.scroll.max_offset().x))
    }

    /// Whether some fully-offscreen pane needs attention — reuses the R8
    /// [`Tab::has_offscreen_attention`] fed this cycle's offscreen set (no second
    /// predicate).
    fn has_offscreen_attention(&self, cx: &App) -> bool {
        let offscreen = self.strip_geometry(cx).offscreen_pane_ids();
        self.active_tab(cx)
            .map(|t| t.has_offscreen_attention(&offscreen))
            .unwrap_or(false)
    }

    /// If the active pane changed since last frame, reset the rename gate and try
    /// to center its pill; retry next frame while `bounds_for_item` is not yet
    /// populated (first layout).
    fn sync_active_pane(&mut self, window: &mut Window, cx: &App) {
        let active_now = self.active_tab(cx).and_then(|t| t.active_pane_id.clone());
        if active_now != self.last_active_pane {
            self.last_active_pane = active_now.clone();
            self.activated_at = Some(Instant::now());
            self.center_pending = active_now.is_some();
        }
        if self.center_pending {
            if self.try_center_active(cx) {
                self.center_pending = false;
            } else {
                // Layout not ready — repaint so we retry once the pills lay out.
                window.request_animation_frame();
            }
        }
    }

    /// Apply the centering offset for the active pane. Returns `false` (a retry
    /// signal) when the pill hasn't been laid out yet.
    fn try_center_active(&mut self, cx: &App) -> bool {
        // Resolve the active pane's row index (dropping the model borrow) before
        // touching the scroll handle.
        let ix = {
            let Some(tab) = self.active_tab(cx) else {
                return true; // nothing to center
            };
            let Some(active_id) = tab.active_pane_id.as_deref() else {
                return true;
            };
            match tab.panes.iter().position(|p| p.id == active_id) {
                Some(ix) => ix,
                None => return true,
            }
        };
        let Some(item) = self.scroll.bounds_for_item(ix) else {
            return false; // not laid out yet
        };
        let viewport = self.scroll.bounds();
        let offset_x = center_offset_x(
            f32::from(viewport.origin.x),
            f32::from(viewport.size.width),
            f32::from(item.origin.x),
            f32::from(item.size.width),
            f32::from(self.scroll.max_offset().x),
        );
        let cur = self.scroll.offset();
        self.scroll.set_offset(point(px(offset_x), cur.y));
        true
    }

    // MARK: - Inline rename

    fn begin_editing(&mut self, tab_id: &str, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self
            .state
            .read(cx)
            .model
            .tab_for(tab_id)
            .and_then(|t| t.panes.iter().find(|p| p.id == pane_id))
            .map(|p| p.title.clone())
        else {
            return;
        };
        // Select the whole title on entry (a pane title is not a filename, so the
        // whole name — not base-minus-extension — is the replace target): the
        // first keystroke replaces it.
        self.rename_editor = Some(TextFieldEditor::with_selection(&title, title.chars().count()));
        self.editing_pane = Some((tab_id.to_string(), pane_id.to_string()));
        self.rename_focus.focus(window, cx);
        // Commit on focus loss (the DO-NOT-PORT click-away monitor replacement).
        // Replacing any prior subscription here drops it OUTSIDE its callback.
        self.rename_blur_sub = Some(cx.on_blur(&self.rename_focus, window, |this, window, cx| {
            this.commit_rename(window, cx);
        }));
        cx.notify();
    }

    /// Commit the draft through the R8 [`TabModel::rename_pane`] (empty input
    /// resets to the per-kind auto-default + consumes a counter slot — asymmetry
    /// 3; the pill reimplements none of it). Idempotent: a stray focus-out after
    /// the edit already ended does nothing.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tab_id, pane_id)) = self.editing_pane.take() else {
            return;
        };
        let draft = self.rename_editor.take().map(|e| e.text()).unwrap_or_default();
        self.state
            .update(cx, |ws, _| ws.model.rename_pane(&tab_id, &pane_id, &draft));
        self.refocus_terminal_after_rename(window, cx);
        cx.notify();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_pane.take().is_none() {
            return;
        }
        self.rename_editor = None;
        self.refocus_terminal_after_rename(window, cx);
        cx.notify();
    }

    /// Apply a click hit-test to the rename field — single click drops the caret,
    /// double selects the word, triple selects all ([`apply_rename_click`]) — then
    /// re-grab field focus (the click already stopped propagation, so the pill's
    /// select/rename gate never re-trips).
    fn place_rename_cursor(
        &mut self,
        index: usize,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.rename_editor.as_mut() {
            apply_rename_click(editor, index, click_count);
            self.rename_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Swift's `commitEdit`/`cancelEdit` call `sessions.focusActiveTerminal()`
    /// so the terminal regains first responder after a rename (dossier G10).
    /// Here the window's [`PaneHostView`] owns the hosted terminal views, so
    /// focus routes back through its `focus_active_terminal` (M2 Item D). A
    /// no-op in the isolated `pane-strip` scenario (no pane host wired).
    fn refocus_terminal_after_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(host) = self.pane_host.clone() {
            host.update(cx, |host, cx| host.focus_active_terminal(window, cx));
        }
    }

    fn on_rename_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        // Escape cancels the pill rename HERE — the shared editor leaves Escape
        // Ignored by design ("the owner's Esc binding cancels"), and unlike the
        // sidebar (whose shell-level Esc action cancels its tab rename) the pill
        // has no shell action of its own, so its owner binding is this listener.
        // The sidebar shell's Esc action runs first (ancestor dispatch) and
        // propagates when it has nothing to do (M2 Item D: "Escape cancels").
        if ks.key == "escape" && !ks.modifiers.platform && !ks.modifiers.control {
            self.cancel_rename(window, cx);
            cx.stop_propagation();
            return;
        }
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
                window.capslock().on,
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

    // MARK: - Pill interactions (select / rename gate)

    /// Whether `pane_id` is the pane currently being inline-renamed.
    fn is_editing_pane(&self, pane_id: &str) -> bool {
        self.editing_pane.as_ref().map(|(_, p)| p.as_str()) == Some(pane_id)
    }

    /// A plain (unmodified) press on a pill body: select the pane. Commits any
    /// in-flight rename on another pill first.
    fn select_pane(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id(cx) else {
            return;
        };
        if self.editing_pane.is_some() {
            self.commit_rename(window, cx);
        }
        self.state.update(cx, |ws, _| {
            ws.pane_strip_actions
                .select_pane(&mut ws.model, &tab_id, pane_id)
        });
        cx.notify();
    }

    /// A press on the title of an already-active pill: enter rename past the gate,
    /// else it's a plain select on a non-active pill
    /// (`WindowToolbarView.swift:883-888`).
    fn handle_title_tap(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id(cx) else {
            return;
        };
        let is_active = self
            .active_tab(cx)
            .and_then(|t| t.active_pane_id.as_deref())
            == Some(pane_id);
        if is_active {
            if rename_gate_open(self.activated_at) {
                self.begin_editing(&tab_id, pane_id, window, cx);
            }
            // else: same-click-as-select window — no-op.
        } else {
            self.select_pane(pane_id, window, cx);
        }
    }

    /// Close a pane, committing any in-flight edit first
    /// (`WindowToolbarView.swift:912-916`). R20.5: routes through the busy-close
    /// gate ([`WindowState::request_close_pane`]) — a busy pane (a shell with a
    /// foreground child) interposes the "Force quit" confirmation; an idle pane
    /// still closes immediately (pty release + dissolve cascade + save + terminus),
    /// exactly as before. The gate owns the reconcile + notify + terminus in both
    /// paths.
    fn close_pane(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id(cx) else {
            return;
        };
        if self.editing_pane.is_some() {
            self.commit_rename(window, cx);
        }
        self.state.update(cx, |ws, wcx| {
            ws.request_close_pane(&tab_id, pane_id, window, wcx);
        });
    }

    /// Add a terminal pane to the active tab through the seam
    /// (`WindowToolbarView.swift:242-244`).
    fn add_terminal_pane(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id(cx) else {
            return;
        };
        self.state
            .update(cx, |ws, _| ws.pane_strip_actions.add_terminal_pane(&mut ws.model, &tab_id));
        cx.notify();
    }

    // MARK: - Context menus

    fn open_pill_context_menu(
        &mut self,
        pane_id: &str,
        kind: PaneKind,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        let pid = pane_id.to_string();

        let rename = {
            let w = weak.clone();
            let pid = pid.clone();
            ContextMenuItem::entry(rename_menu_label(kind), move |window, app| {
                let _ = w.update(app, |this, cx| {
                    let Some(tab_id) = this.active_tab_id(cx) else {
                        return;
                    };
                    this.state.update(cx, |ws, _| {
                        ws.pane_strip_actions
                            .select_pane(&mut ws.model, &tab_id, &pid)
                    });
                    this.begin_editing(&tab_id, &pid, window, cx);
                });
            })
        };
        let close = {
            let w = weak.clone();
            let pid = pid.clone();
            ContextMenuItem::entry(close_menu_label(kind), move |window, app| {
                let _ = w.update(app, |this, cx| {
                    this.close_pane(&pid, window, cx);
                });
            })
        };
        self.present_context_menu(vec![rename, close], position, window, cx);
    }

    fn open_overflow_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // Snapshot the (pane id, row label) pairs while the model borrow is held,
        // then build the menu items from owned data.
        let rows: Vec<(String, String)> = {
            let Some(tab) = self.active_tab(cx) else {
                return;
            };
            let active = tab.active_pane_id.clone();
            tab.panes
                .iter()
                .map(|pane| (pane.id.clone(), overflow_row_label(pane, active.as_deref())))
                .collect()
        };
        let weak = cx.weak_entity();
        let items: Vec<ContextMenuItem> = rows
            .into_iter()
            .map(|(pid, label)| {
                let w = weak.clone();
                ContextMenuItem::entry(label, move |window, app| {
                    let _ = w.update(app, |this, cx| {
                        this.select_pane(&pid, window, cx);
                    });
                })
            })
            .collect();
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
                // (the Rename entry focuses the field before the menu dismisses),
                // which must keep the field focused (M2 Item D).
                if this.editing_pane.is_none() {
                    this.refocus_terminal_after_rename(window, cx);
                }
                cx.notify();
            },
        ));
        self.context_menu = Some(menu);
        cx.notify();
    }

    // MARK: - Empty-band window drag (R9 band pattern)

    fn on_band_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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

    fn on_band_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, _cx: &mut Context<Self>) {
        let Some(origin) = self.band_press else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.band_press = None;
            return;
        }
        let dx = f32::from(event.position.x - origin.x);
        let dy = f32::from(event.position.y - origin.y);
        if band_drag_threshold_crossed(dx, dy) {
            self.band_press = None;
            window.start_window_move();
        }
    }

    fn on_band_mouse_up(&mut self, _e: &MouseUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        self.band_press = None;
    }

    // MARK: - Rendering

    /// The leading sidebar-collapse toggle (mock's `.tb-btn`, Finder/Safari
    /// position): the restyle's exact stroke icon rendered via gpui's `svg()`
    /// (tinted `ink3`, hover `ink` + faint fill), sitting right of the native
    /// traffic lights (whose cluster the toolbar's leading reserve clears). It
    /// toggles the shared [`WindowState`]'s collapsed flag — present in both
    /// collapsed and expanded states, replacing the old collapsed-band restore
    /// button + the sidebar strip's collapse toggle.
    fn render_collapse_toggle(&self, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = ink_alpha(s, COLLAPSE_BTN_HOVER_INK_ALPHA);
        let ink = slot_to_rgba(s.ink);
        let ink3 = slot_to_rgba(s.ink3);
        div()
            .id("toolbar.collapseSidebar")
            .group("toolbar.collapseSidebar")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(COLLAPSE_BTN_W))
            .h(px(COLLAPSE_BTN_H))
            .mr(px(COLLAPSE_BTN_TRAILING_MARGIN))
            .rounded(px(COLLAPSE_BTN_RADIUS))
            .hover(move |st| st.bg(hover))
            .child(
                // gpui tints the SVG's alpha mask with the element's own text
                // colour, so it must be set explicitly (`ink3` at rest); the
                // group-hover swaps it to `ink` when the button box is hovered,
                // matching the box's faint fill exactly.
                gpui::svg()
                    .path(crate::chrome_icons::SIDEBAR_TOGGLE)
                    .w(px(crate::chrome_icons::SIDEBAR_TOGGLE_W))
                    .h(px(crate::chrome_icons::SIDEBAR_TOGGLE_H))
                    .text_color(ink3)
                    .group_hover("toolbar.collapseSidebar", move |st| st.text_color(ink)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                    this.toggle_sidebar_collapsed(cx);
                    cx.stop_propagation();
                }),
            )
    }

    /// Toggle the shared sidebar collapsed flag via the one
    /// [`WindowState::toggle_sidebar_collapsed`] seam (expanding also clears any
    /// peek). The state entity notifies from inside that seam, so this view's own
    /// `cx.observe(&state)` subscription re-renders the titlebar and the sibling
    /// shell's observer re-renders the sidebar — no separate `cx.notify()` here.
    /// The collapse control now lives in the titlebar.
    fn toggle_sidebar_collapsed(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, wcx| ws.toggle_sidebar_collapsed(wcx));
    }

    // MARK: - Pill reorder (R25)

    /// The scroll row's `on_drag_move`: recompute the gated drop slot while a pill
    /// drag is in flight. Fires in the Capture phase for EVERY window mouse-move
    /// while a `PaneDragPayload` is dragging — including over the terminal body
    /// below the strip — so it FIRST guards strip containment (the port of Swift's
    /// `dropExited`, `WindowToolbarView.swift:529-536`): a cursor outside the row's
    /// hitbox clears `drag_target` and returns, else the row-only x-resolver would
    /// paint an insertion line while dragging straight down into the terminal (D8).
    /// When contained, the cursor's viewport-relative x is `position.x -
    /// bounds.origin.x` (the row IS the tracked viewport, so `bounds.origin.x ==
    /// viewport_left`), fed with the model pane order + viewport-relative frames to
    /// the pure [`resolve`], whose `would_move` gate closes over
    /// [`TabModel::would_move_pane`]. The result (a no-op slot resolves to `None`)
    /// is stored for the drop to read (D9).
    fn on_pill_drag_move(
        &mut self,
        event: &DragMoveEvent<PaneDragPayload>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Containment guard (dropExited port): a cursor outside the strip clears
        // any pending slot so the line does not linger while dragging into the
        // terminal body.
        if !event.bounds.contains(&event.event.position) {
            if self.drag_target.take().is_some() {
                cx.notify();
            }
            return;
        }
        let payload = event.drag(cx).clone();
        let dragged = payload.pane_id.to_string();
        let tab_id = payload.tab_id.to_string();
        let x_rel = f32::from(event.event.position.x) - f32::from(event.bounds.origin.x);
        let pane_order = self.pane_ids(cx);
        let frames = self.strip_geometry(cx).pane_frames;
        let new_target = {
            let ws = self.state.read(cx);
            resolve(&dragged, x_rel, &pane_order, &frames, |target, place_after| {
                ws.model.would_move_pane(&dragged, &tab_id, target, place_after)
            })
        };
        if self.drag_target != new_target {
            self.drag_target = new_target;
            cx.notify();
        }
    }

    /// The scroll row's `on_drop`: commit the reorder to the slot the last
    /// `on_drag_move` resolved (D9). Reads the stored `drag_target` (the mouse-up
    /// carries no position), calls [`TabModel::move_pane`] synchronously — gpui
    /// clears `active_drag` itself after this listener, so no deferral is needed —
    /// then persists explicitly via [`WindowState::save_to_store`]. The
    /// once-per-window `on_tree_mutation` observer (BUGHUNT1-D) now also fires from
    /// `move_pane`, so this explicit save is belt-and-suspenders (a duplicate
    /// debounced upsert is harmless — kept per that plan's D2).
    /// Selection/focus are untouched (`move_pane` never touches
    /// `active_pane_id`). A drop resolving to `None` (a horizontal inter-pill gap,
    /// or a no-op slot) just clears the field.
    fn on_pill_drop(&mut self, payload: &PaneDragPayload, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, place_after)) = self.drag_target.take() {
            let dragged = payload.pane_id.to_string();
            let tab_id = payload.tab_id.to_string();
            self.state.update(cx, |ws, _| {
                ws.model.move_pane(&dragged, &tab_id, &target, place_after);
                ws.save_to_store();
            });
        }
        cx.notify();
    }

    /// The reorder insertion line: a 2pt vertical accent bar at the resolved target
    /// slot's edge, an absolute child of `scroll_wrap` (D10). `edge_x` is the target
    /// frame's leading edge (`place_after == false`) or trailing edge (`== true`),
    /// read from the viewport-relative `pane_frames`. Painted only while
    /// `drag_target` is set AND gpui still has an active drag — the `has_active_drag`
    /// conjunct drops the line the instant a dropped-nowhere mouse-up clears
    /// `active_drag`, even if a stale `drag_target` somehow survived. Because
    /// `drag_target` is already `would_move_pane`-gated, no line shows for a no-op
    /// slot. Pure paint: no id, no listeners.
    fn insertion_line(&self, cx: &App) -> Option<gpui::AnyElement> {
        if !cx.has_active_drag() {
            return None;
        }
        let (target, place_after) = self.drag_target.as_ref()?;
        let frames = self.strip_geometry(cx).pane_frames;
        let frame = frames.get(target)?;
        let edge_x = if *place_after {
            frame.max_x()
        } else {
            frame.min_x()
        };
        let accent = srgba_to_rgba(crate::theme_settings::active_chrome_accent(cx));
        Some(
            div()
                .absolute()
                .left(px(edge_x - 1.0))
                .top_0()
                .w(px(2.0))
                .h(px(PILL_HEIGHT))
                .bg(accent)
                .into_any_element(),
        )
    }

    /// The pill strip: a horizontally-scrolling row of pills (flex-filling), then
    /// the always-reserved chevron slot, then the always-visible `+`.
    fn render_strip(&self, panes: &[PaneVm], s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        // Single-tab mode (round-2 plan 4): with EXACTLY one pane the strip draws
        // no tab boxes — the title renders as the window's centered titlebar text
        // (the overlay built in `render`), and the overflow chevron is hidden (it
        // cannot be needed; `show_chevron` already reports false below one overflow,
        // but the slot itself is dropped here so the tail is just the `+`). The
        // trailing `+` stays.
        let single = panes.len() == 1;
        let geometry = self.strip_geometry(cx);
        let show_chevron = self.show_chevron(cx);
        let has_attention = self.has_offscreen_attention(cx);

        // The active tab id — captured once and threaded into each pill's drag
        // payload (D3) so the drop's `move_pane` is robust to any active-tab change
        // mid-drag.
        let tab_id = self.active_tab_id(cx).unwrap_or_default();

        // The tracked scroll viewport (fixed width — the two trailing slots are
        // always reserved) hosting the pill row. It is ALSO the drop target: the
        // row is the tracked viewport whose `bounds.origin.x == viewport_left`, so
        // `on_drag_move` here yields a valid viewport-relative `x_rel` (a
        // pill-attached mover would see the pill's own hitbox bounds instead —
        // D8). `on_drag_move` recomputes the gated `drag_target`; `on_drop` commits
        // it. Both key on the `PaneDragPayload` type; no `can_drop` predicate.
        let mut row = div()
            .id("toolbar.paneStrip")
            .track_scroll(&self.scroll)
            .overflow_x_scroll()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(PILL_ROW_GAP))
            .size_full()
            .on_drag_move(cx.listener(Self::on_pill_drag_move))
            .on_drop(cx.listener(Self::on_pill_drop));
        // No pills in single-tab mode — the centered title overlay replaces them.
        if !single {
            for vm in panes {
                row = row.child(self.render_pill(vm, &tab_id, s, cx));
            }
        }

        // The scroll wrapper carries the two edge fades as absolute overlays so
        // they sit at the viewport's own leading / trailing edges. It is also the
        // viewport-fixed host for the reorder insertion line (D10): `scroll_wrap`'s
        // origin is the viewport left, and `pane_frames` are viewport-relative, so
        // the line's x is directly the target frame edge.
        // Restyle plan 3: the edge fade dissolves the fill-less scrolling tabs into
        // the WINDOW-BODY surface (the terminal theme background at the active
        // window opacity), not the `chrome` slot — see `edge_fade`. Computed once
        // here (needs `cx`) and moved into the fade builders.
        let (term_theme, _) = crate::theme_settings::active_terminal_theme_and_accent(cx);
        let fade = Rgba {
            a: crate::theme_settings::active_window_opacity(cx),
            ..gpui::rgb(term_theme.background.to_u32())
        };
        let scroll_wrap = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(PILL_HEIGHT))
            .child(row)
            .when(geometry.can_scroll_leading(), |el| {
                el.child(self.edge_fade(false, fade))
            })
            .when(geometry.can_scroll_trailing(), |el| {
                el.child(self.edge_fade(true, fade))
            })
            .children(self.insertion_line(cx));

        let mut tail = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .child(scroll_wrap);
        // Hide the overflow chevron slot entirely in single-tab mode (it cannot be
        // needed with one pane) — the tail is then just the `+`.
        if !single {
            tail = tail.child(self.render_chevron_slot(show_chevron, has_attention, s, cx));
        }
        tail.child(self.render_new_tab_slot(s, cx))
    }

    /// A 16pt gradient from the window-body surface color (`fade`) to transparent,
    /// at the viewport's leading (`trailing == false`) or trailing edge. Never
    /// hit-tests.
    ///
    /// Restyle-plan-3 conformance (drift `chrome-fill-survivor`): the opaque stop
    /// is the WINDOW-BODY surface — the terminal theme background at the active
    /// window opacity, computed by the caller — NOT the `chrome` slot
    /// (`background @ CHROME_OPACITY 0.70`). Plans 1–2 made the titlebar band
    /// fill-less, so the fill-less tabs scroll directly over the (now possibly
    /// translucent) window body; fading to `chrome @ 0.70` composited a FIXED
    /// 0.70-alpha, wrong-color band over the translucent surface (a double-applied,
    /// differently-tinted stripe at the tab-strip edges). Fading to the same
    /// surface color/alpha the body already paints keeps the edges consistent with
    /// the body at every per-scheme opacity.
    fn edge_fade(&self, trailing: bool, fade: Rgba) -> impl IntoElement {
        let opaque = fade;
        let clear = Rgba { a: 0.0, ..fade };
        // angle 90° points right (opaque→clear, leading fade); 270° points left
        // (opaque→clear from the trailing edge).
        let angle = if trailing { 270.0 } else { 90.0 };
        let gradient = linear_gradient(
            angle,
            linear_color_stop(opaque, 0.0),
            linear_color_stop(clear, 1.0),
        );
        // A pure painted decoration: no id, no listeners, no `occlude()`, so it
        // never registers a hitbox and clicks pass straight through to the pills
        // beneath it (Swift's `.allowsHitTesting(false)`,
        // `WindowToolbarView.swift:453`).
        let fade = div()
            .absolute()
            .top_0()
            .h_full()
            .w(px(EDGE_FADE_WIDTH))
            .bg(gradient);
        if trailing {
            fade.right_0()
        } else {
            fade.left_0()
        }
    }

    /// The tab's leading indicator: a per-pane [`StatusDot`] (6pt) for a Claude
    /// pane (only the size parameter changes here — colours + pulse untouched),
    /// else the `terminal` SF symbol tinted `tab_ink`. Shared by the pill and the
    /// single-tab centered title so both read the same per-kind glyph.
    fn tab_leading(&self, vm: &PaneVm, tab_ink: Rgba, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        match vm.kind {
            PaneKind::Claude => StatusDot::new(
                SharedString::from(format!("pill.{}", vm.id)),
                vm.status,
                slot_srgba(s.ink3),
            )
            .size(TAB_STATUS_DOT_SIZE)
            .suppress_waiting_pulse(vm.waiting_ack)
            .into_any_element(),
            PaneKind::Terminal => div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(PILL_ICON_SIZE))
                .h(px(PILL_ICON_SIZE))
                .child(sf_symbol_icon(
                    SF_TERMINAL,
                    ICON_TERMINAL,
                    PILL_ICON_SIZE,
                    SymbolWeight::Regular,
                    tab_ink,
                    self.window_scale,
                    cx,
                ))
                .into_any_element(),
        }
    }

    /// The single-tab centered titlebar text (round-2 plan 4 "Single-tab mode"):
    /// when the strip holds EXACTLY one pane, its title + leading status dot
    /// render as the window's centered titlebar text (macOS window-title
    /// convention) instead of a tab box. It is an absolute overlay spanning the
    /// FULL window width (inset [`SINGLE_TAB_EDGE_INSET`] from both edges so it
    /// clears the traffic-light cluster on the left and the tail `+` on the
    /// right), mono 12px in `ink`, a 6pt status dot leading the title — the sole
    /// pane is always active, so both wear the active tint. Display-only: no
    /// underline, no ✕, no hover fill (activation is meaningless with one pane;
    /// close/rename stay available in the sidebar). It is a pure painted overlay
    /// — no mouse listeners and no `occlude()` — so the empty-band window drag /
    /// double-click-zoom pass straight through it (it stays in the titlebar drag
    /// region). The title clamps + ellipsizes within the inset box and wears the
    /// same full-title hover tooltip a tab does (plan 1) when clamped.
    fn render_single_tab_title(&self, vm: &PaneVm, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ink = slot_to_rgba(s.ink);
        let leading = self.tab_leading(vm, ink, s, cx);
        // Tab titles use the terminal font family (parity with the pill).
        let tab_family = crate::keymap::try_shared_font_settings(cx).map(|f| f.read(cx).family());
        let full_title = SharedString::from(vm.title.clone());
        let tooltip_title = full_title.clone();
        div()
            .absolute()
            .top_0()
            .h_full()
            .left(px(SINGLE_TAB_EDGE_INSET))
            .right(px(SINGLE_TAB_EDGE_INSET))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(PILL_GAP))
            .when_some(tab_family, |el, fam| el.font_family(fam))
            .child(leading)
            .child(
                // The title span shrinks + tail-ellipsizes (`min_w_0` + `truncate`)
                // within the centered group so it never overruns the inset box; the
                // `.id()` is only the tooltip's hover anchor — it carries no mouse
                // listener and never occludes, so clicks/drag pass through to the
                // band below (parity with the mock's `.tab-single .t`).
                div()
                    .id("toolbar.singleTabTitle")
                    .min_w_0()
                    .whitespace_nowrap()
                    .truncate()
                    .text_size(px(PILL_TEXT_SIZE))
                    .text_color(ink)
                    .child(full_title)
                    .tooltip(move |_window, cx| {
                        let title = tooltip_title.clone();
                        cx.new(|_| TabTooltip { title }).into()
                    }),
            )
            .into_any_element()
    }

    fn render_pill(&self, vm: &PaneVm, tab_id: &str, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        let accent = crate::theme_settings::active_chrome_accent(cx);
        let is_active = vm.is_active;
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let ink3 = slot_to_rgba(s.ink3);
        // The tab's text tint: active → `ink`, inactive → `ink3` (mock Style A
        // `.tab` / `.tab.active`). The leading glyph tracks the same tint.
        let tab_ink = if is_active { ink } else { ink3 };
        // Tab titles use the terminal font family (not the UI sans), read from the
        // shared font settings; `None` before the keymap wires it (isolated
        // scenarios) leaves the default font.
        let tab_family = crate::keymap::try_shared_font_settings(cx).map(|f| f.read(cx).family());

        // Leading icon: per-pane StatusDot for Claude (6pt in the strip), else the
        // `terminal` symbol tinted like the title (shared with the single-tab title).
        let leading = self.tab_leading(vm, tab_ink, s, cx);

        // Title: the shared inline-rename field while editing, else the label.
        let title: gpui::AnyElement = if vm.is_editing {
            let spans = self
                .rename_editor
                .as_ref()
                .map(edit_spans)
                .unwrap_or_else(|| edit_spans(&TextFieldEditor::new("")));
            let colors = FieldColors {
                bg: slot_to_rgba(s.background3),
                border: slot_to_rgba(s.line_strong),
                text: if is_active { ink } else { ink2 },
                caret: srgba_to_rgba(accent),
                selection: srgba_to_rgba(srgba_with_alpha(accent, 0.3)),
            };
            let weak = cx.weak_entity();
            rename_field(
                &self.rename_focus,
                &spans,
                "PaneRename",
                colors,
                PILL_TEXT_SIZE,
                self.rename_probe.clone(),
                cx.listener(Self::on_rename_key),
                move |index, click_count, window, app| {
                    let _ = weak.update(app, |this, cx| {
                        this.place_rename_cursor(index, click_count, window, cx)
                    });
                },
            )
            .into_any_element()
        } else {
            let pid = vm.id.clone();
            let full_title = SharedString::from(vm.title.clone());
            let tooltip_title = full_title.clone();
            // The title tap is a CLICK (mouse-up with no drag), not a mouse-down
            // — prod's `.onTapGesture` on `titleView`
            // (`WindowToolbarView.swift:883-888`) and the same fix the sidebar
            // row title got in M7.8 round 2. gpui's click machinery never fires
            // a click once a drag armed (pending press state is cleared while
            // `active_drag` is set — div.rs:1994-2003), so pressing the active
            // pill's TITLE and dragging now drags the pill instead of falling
            // straight into rename. No left mouse-down listener here: a child's
            // `stop_propagation` on mouse-down would kill the PILL's
            // window-level click/drag arming for presses on the text (most of
            // the pill's width).
            div()
                .id(SharedString::from(format!("toolbar.pill.{}.title", vm.id)))
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .truncate()
                .text_size(px(PILL_TEXT_SIZE))
                .text_color(tab_ink)
                .child(full_title)
                // Full-title hover tooltip (the tail-ellipsized name in full).
                .tooltip(move |_window, cx| {
                    let title = tooltip_title.clone();
                    cx.new(|_| TabTooltip { title }).into()
                })
                .on_click(cx.listener(move |this, _e: &ClickEvent, window, cx| {
                    this.handle_title_tap(&pid, window, cx);
                    // Consume so the pill's own click listener doesn't also
                    // run a second (redundant) select pass.
                    cx.stop_propagation();
                }))
                .into_any_element()
        };

        // Trailing close "×" — its 16pt slot is always reserved (visibility
        // toggled) so the tab width never jumps on hover.
        let show_close = vm.is_hovered || is_active;
        let close = self.render_close_button(&vm.id, show_close, s, cx);

        let pid_select = vm.id.clone();
        let pid_down = vm.id.clone();
        let pid_menu = vm.id.clone();
        let kind = vm.kind;

        // The stable per-pane element id the drag arms from (D11, exported
        // contract `toolbar.pill.<pane_id>`). No `aria_label` — tests drive by
        // geometry, not AX.
        let pill_id = SharedString::from(format!("toolbar.pill.{}", vm.id));
        // The drag payload (D3) + the ghost title (D4). Captured at drag start.
        let drag_payload = PaneDragPayload {
            pane_id: SharedString::from(vm.id.clone()),
            tab_id: SharedString::from(tab_id.to_string()),
        };
        let ghost_title = SharedString::from(vm.title.clone());

        // The underline color: accent on the active tab, a scheme-scoped grey
        // ([`nice_theme::tab_underline_idle`]) on every inactive tab so it reads
        // as clickable (round-2 plan 4 "Inactive-tab underline"). Same geometry
        // for both (inset 11px, 1px tall, on the bar's bottom edge — mock Style A
        // `.tab::after` / `.tab.active::after`).
        let underline = if is_active {
            srgba_to_rgba(accent)
        } else {
            srgba_to_rgba(nice_theme::tab_underline_idle(
                crate::theme_settings::active_chrome_scheme(cx),
            ))
        };

        // The tab is fill-less (no pill background, border, rounding, or shadow —
        // mock Style A): just the dot / title / ✕ row, full bar height, with a 1px
        // underline seated on the bar's bottom edge (accent on the active tab,
        // grey on inactive tabs).
        let pill = div()
            .id(pill_id)
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(PILL_GAP))
            .px(px(TAB_PAD_X))
            .h(px(PILL_HEIGHT))
            .max_w(px(PILL_MAX_WIDTH))
            .when_some(tab_family, |el, fam| el.font_family(fam))
            .child(leading)
            .child(title)
            .child(close)
            // Tab underline (inset 11px from the tab's outer edges, 1px tall, on
            // the bar's bottom edge — mock `.tab::after` / `.tab.active::after`).
            // Every tab wears one: accent when active, grey when inactive.
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(TAB_UNDERLINE_INSET))
                    .right(px(TAB_UNDERLINE_INSET))
                    .h(px(TAB_UNDERLINE_HEIGHT))
                    .rounded(px(TAB_UNDERLINE_RADIUS))
                    .bg(underline),
            )
            // The pill carries `.id()` + `on_drag` ONLY — `on_drag_move` / `on_drop`
            // live on the scroll row (the tracked viewport, D8). gpui subtracts the
            // constructor's `Point` offset (the grab point within the pill) when it
            // lays the ghost out (`window.rs` `mouse - cursor_offset`), so the ghost
            // captures that offset and re-adds it as padding to net to `pointer + 12`
            // (see `PaneDragGhost`). Coexists with the mouse-down select below: gpui's
            // drag-arming recorder is a separate window-level listener keyed on the
            // hitbox hover, not this element's handler (D6, proven by the F9 file
            // drag).
            .on_drag(drag_payload, move |_payload, offset, _window, app| {
                let title = ghost_title.clone();
                app.new(|_| PaneDragGhost { title, offset })
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e: &MouseDownEvent, window, cx| {
                    // A press that bubbles here from inside the editing field (or
                    // on the editing pill's own icon/padding) must keep the edit
                    // alive, not commit + reselect — Swift's `if !isEditing`
                    // onTapGesture guard (`WindowToolbarView.swift:808`).
                    if this.is_editing_pane(&pid_down) {
                        cx.stop_propagation();
                        return;
                    }
                    // Mouse-down only commits ANOTHER pill's in-flight rename
                    // (prod's click-away mouse monitor fires at press time).
                    // Selection moved to `on_click` below so a press that arms
                    // a drag no longer also selects the pane — prod's pill
                    // `.onTapGesture` is a click, not a press
                    // (`WindowToolbarView.swift:803-809`).
                    if this.editing_pane.is_some() {
                        this.commit_rename(window, cx);
                    }
                    // Still consumed: the empty-band window-drag press listener
                    // sits on the strip behind the pills and must not arm from
                    // a pill press. gpui's click/drag arming for this pill is a
                    // window-level recorder that already ran, so this
                    // `stop_propagation` cannot break the pill's own drag/click.
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(move |this, _e: &ClickEvent, window, cx| {
                // Select on CLICK (mouse-up with no drag) — prod's pill-body
                // `.onTapGesture` (`WindowToolbarView.swift:803-809`). A drag
                // that armed suppresses the click, so drag-to-reorder no longer
                // double-fires a select.
                if this.is_editing_pane(&pid_select) {
                    cx.stop_propagation();
                    return;
                }
                this.select_pane(&pid_select, window, cx);
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    this.open_pill_context_menu(&pid_menu, kind, e.position, window, cx);
                    cx.stop_propagation();
                }),
            );
        pill.into_any_element()
    }

    /// The trailing "×" close square. Its slot is always laid out; `visible`
    /// toggles paint + hit-testing so the pill width is hover-invariant.
    fn render_close_button(
        &self,
        pane_id: &str,
        visible: bool,
        s: &Slots,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = ink_alpha(s, CLOSE_HOVER_INK_ALPHA);
        let pid = pane_id.to_string();
        // 9pt semibold `xmark`, ink3 (`WindowToolbarView.swift:984-986`).
        let icon = sf_symbol_icon(
            SF_CLOSE,
            ICON_CLOSE,
            CLOSE_GLYPH_SIZE,
            SymbolWeight::Semibold,
            slot_to_rgba(s.ink3),
            self.window_scale,
            cx,
        );
        let mut btn = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(CLOSE_BTN_SIZE))
            .h(px(CLOSE_BTN_SIZE))
            .rounded(px(CLOSE_BTN_RADIUS))
            .child(icon);
        if visible {
            btn = btn
                .hover(move |st| st.bg(hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e: &MouseDownEvent, window, cx| {
                        this.close_pane(&pid, window, cx);
                        cx.stop_propagation();
                    }),
                );
        } else {
            // Reserved but invisible + inert (opacity 0, no handler).
            btn = btn.opacity(0.0);
        }
        btn
    }

    /// The always-reserved chevron slot: renders the overflow button + attention
    /// badge only when overflowing, but keeps its width either way.
    fn render_chevron_slot(
        &self,
        show_chevron: bool,
        has_attention: bool,
        s: &Slots,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut slot = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(SQUARE_SLOT_WIDTH))
            .h(px(PILL_HEIGHT))
            .pl(px(SQUARE_BTN_LEADING_PAD));
        if show_chevron {
            slot = slot.child(self.render_chevron(has_attention, s, cx));
        }
        slot
    }

    fn render_chevron(&self, has_attention: bool, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = ink_alpha(s, SQUARE_BTN_HOVER_INK_ALPHA);
        let accent = srgba_to_rgba(crate::theme_settings::active_chrome_accent(cx));
        // 10pt semibold `chevron.down`, ink2 (`WindowToolbarView.swift:1045-1047`).
        let icon = sf_symbol_icon(
            SF_CHEVRON_DOWN,
            ICON_CHEVRON_DOWN,
            CHEVRON_GLYPH_SIZE,
            SymbolWeight::Semibold,
            slot_to_rgba(s.ink2),
            self.window_scale,
            cx,
        );
        div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .w(px(SQUARE_BTN_SIZE))
            .h(px(SQUARE_BTN_SIZE))
            .rounded(px(SQUARE_BTN_RADIUS))
            .hover(move |st| st.bg(hover))
            .child(icon)
            // 6pt accent attention badge at the top-trailing corner.
            .when(has_attention, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(3.0))
                        .right(px(3.0))
                        .w(px(ATTENTION_BADGE_SIZE))
                        .h(px(ATTENTION_BADGE_SIZE))
                        .rounded_full()
                        .bg(accent),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    this.open_overflow_menu(e.position, window, cx);
                    cx.stop_propagation();
                }),
            )
    }

    /// The trailing "+" — always visible, pinned in its own reserved slot.
    ///
    /// Round-2 restyle plan 4 flagged the `+`/chevron as "visibly off-center" and
    /// asked for a vertical-centering fix. Direct pixel measurement of the shipped
    /// build (retina, scale 2) found the glyph already centered: the `+` ink
    /// mid-line lands within ~0.5pt of the 28pt bar's center and aligns with the
    /// tab-title text. The SF `plus`/`chevron.down` bitmaps are themselves
    /// vertically symmetric (ink mid == box center), and this slot's
    /// `items_center` centers that box in the full-height bar — so NO optical
    /// nudge is applied (one would de-center it, and the plan forbids a magic
    /// offset absent a genuine metric need). Same construction for the chevron.
    /// The drift the feel-check actually saw was HORIZONTAL — see
    /// [`TOOLBAR_TRAILING_PAD`] (20 → the mock's 10).
    fn render_new_tab_slot(&self, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = ink_alpha(s, SQUARE_BTN_HOVER_INK_ALPHA);
        // 11pt semibold `plus`, ink2 (`WindowToolbarView.swift:1134-1136`).
        let icon = sf_symbol_icon(
            SF_PLUS,
            ICON_PLUS,
            PLUS_GLYPH_SIZE,
            SymbolWeight::Semibold,
            slot_to_rgba(s.ink2),
            self.window_scale,
            cx,
        );
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(SQUARE_SLOT_WIDTH))
            .h(px(PILL_HEIGHT))
            .pl(px(SQUARE_BTN_LEADING_PAD))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(SQUARE_BTN_SIZE))
                    .h(px(SQUARE_BTN_SIZE))
                    .rounded(px(SQUARE_BTN_RADIUS))
                    .hover(move |st| st.bg(hover))
                    .child(icon)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.add_terminal_pane(cx);
                            cx.stop_propagation();
                        }),
                    ),
            )
    }

    /// The trailing update pill (R27, P7) — the conditional `.child` in the
    /// reserved trailing slot, rendered ONLY when a newer release is available
    /// ([`crate::release_check::update_available`] returns `Some`). When absent it
    /// emits NOTHING (no reserved space): the toolbar with no update is
    /// byte-identical to today. Frozen appearance/AX/copy
    /// (`UpdateAvailablePill.swift:25-64`): text `"Update available"`, a leading
    /// `arrow.up.circle.fill` glyph, the house accent tint, the reused `PILL_*`
    /// box metrics + hover fill, `.id`/`.role`/`.aria_label` AX anchor, and
    /// `stop_propagation` on its mouse-down so the R9 empty-band drag/zoom doesn't
    /// fire under it. The click presents the popover via `cx.defer_in` (D9).
    fn render_update_pill(&self, s: &Slots, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        // The pill's render gate: a newer release exists. `None` ⇒ no pill.
        crate::release_check::update_available(cx)?;
        let accent = srgba_to_rgba(crate::theme_settings::active_chrome_accent(cx));
        let hover = ink_alpha(s, PILL_HOVER_INK_ALPHA);
        let icon = sf_symbol_icon(
            SF_ARROW_UP_CIRCLE,
            ICON_ARROW_UP,
            PILL_ICON_SIZE,
            SymbolWeight::Semibold,
            accent,
            self.window_scale,
            cx,
        );
        Some(
            div()
                .id(UPDATE_PILL_ID)
                .role(gpui::Role::Button)
                .aria_label(UPDATE_PILL_LABEL)
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(PILL_GAP))
                .h(px(PILL_HEIGHT))
                .px(px(TAB_PAD_X))
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(move |st| st.bg(hover))
                // A zero-visual probe recording the pill's painted window-content
                // bounds so the `update-check` scenario can target its centre for
                // the real guarded-HID click (the confirmation-modal backdrop-probe
                // idiom). Absolute + zero-inset so it fills the pill without
                // perturbing the flex row.
                .child({
                    let sink = self.update_pill_bounds.clone();
                    gpui::canvas(
                        |_, _, _| (),
                        move |bounds, _, _, _| sink.set(Some(bounds)),
                    )
                    .absolute()
                    .inset_0()
                })
                .child(icon)
                .child(
                    div()
                        .text_size(px(PILL_TEXT_SIZE))
                        .text_color(accent)
                        .child(SharedString::from(UPDATE_PILL_LABEL)),
                )
                // Consume the press so the R9 empty-band drag / zoom never arms
                // under the pill, then present the popover at the click point. The
                // defer is load-bearing (D9): gpui takes the window out of
                // `cx.windows` mid-dispatch, so opening a window-anchored child
                // from this handler must run at the end of the effect cycle
                // (`defer_in` re-fetches the window — the 4875d9c rule).
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, e: &MouseDownEvent, window, cx| {
                        let position = e.position;
                        cx.stop_propagation();
                        cx.defer_in(window, move |this, window, cx| {
                            this.toggle_update_popover(position, window, cx);
                        });
                    }),
                )
                .into_any_element(),
        )
    }

    /// Toggle the update popover: a click while it is open closes it (the Swift
    /// pill's popover toggle, `UpdateAvailablePill.swift:60-64`); otherwise mint a
    /// fresh [`UpdatePopover`] anchored at `position`, subscribe to its dismissal,
    /// and store it.
    fn toggle_update_popover(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_popover.is_some() {
            self.update_popover = None;
            self.update_popover_sub = None;
            cx.notify();
            return;
        }
        let latest = crate::release_check::update_available(cx);
        let popover = cx.new(|cx| UpdatePopover::new(position, latest.as_deref(), window, cx));
        self.update_popover_sub = Some(cx.subscribe_in(
            &popover,
            window,
            |this, _popover, _ev: &DismissEvent, window, cx| {
                this.update_popover = None;
                // The popover grabbed key focus on open; hand it back to the
                // active terminal (unless a rename is mid-flight — parity with the
                // context-menu dismissal).
                if this.editing_pane.is_none() {
                    this.refocus_terminal_after_rename(window, cx);
                }
                cx.notify();
            },
        ));
        self.update_popover = Some(popover);
        cx.notify();
    }
}

/// Whether a title tap on the active pill may begin a rename: the R10
/// [`nice_model::InlineRenameClickGate`] read against the current clock, so the
/// same click that selects a pill can't also start a rename.
fn rename_gate_open(activated_at: Option<Instant>) -> bool {
    nice_model::InlineRenameClickGate::can_begin_edit(
        activated_at,
        Instant::now(),
        DOUBLE_CLICK_INTERVAL,
    )
}

impl Focusable for WindowToolbarView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WindowToolbarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Re-sample the backing scale so the SF Symbol cache renders (and hits)
        // at this window's device resolution.
        self.window_scale = window.scale_factor();
        // Chrome-click focus bounce (M2 Item D, installed once — it needs a
        // `Window`, which `new` doesn't have): a click on empty toolbar chrome
        // focuses this root via gpui's tracked-focus transfer; hand it straight
        // back to the active terminal so chrome never keeps key focus. A rename
        // begin never lands here (the field's own handle takes focus, not this
        // root), so the bounce cannot fight the rename field.
        if self.focus_bounce_sub.is_none() {
            self.focus_bounce_sub = Some(cx.on_focus(&self.focus_handle, window, |this, window, cx| {
                this.refocus_terminal_after_rename(window, cx);
            }));
        }
        // Reset the rename gate + auto-center when the active pane changed.
        self.sync_active_pane(window, cx);

        let s = active_slots(cx);
        let panes = self.snapshot_panes(cx);

        // The single-pane centered title (round-2 plan 4). Built here because it is
        // an absolute overlay spanning the FULL window width — not a child of the
        // padded content row (whose residual strip space is off-center). `None`
        // unless the strip holds exactly one pane.
        let single_tab_title =
            (panes.len() == 1).then(|| self.render_single_tab_title(&panes[0], &s, cx));

        // The padded content row: the collapse toggle, the pane strip, and the
        // conditional trailing update pill. Its leading reserve clears the native
        // traffic-light cluster before the collapse toggle (the titlebar is now
        // full-width in both shell states, so its left edge sits under the lights).
        let content = div()
            .flex()
            .flex_row()
            .items_center()
            .size_full()
            .pl(px(traffic_light_reserved_width()))
            .pr(px(TOOLBAR_TRAILING_PAD))
            .child(self.render_collapse_toggle(&s, cx))
            .child(self.render_strip(&panes, &s, cx))
            // Trailing update-pill slot (R27, P7): the conditional update pill,
            // inserted after the strip. It renders ONLY when a newer release is
            // available — absent, it emits nothing, so the toolbar with no update is
            // byte-identical to today.
            .children(self.render_update_pill(&s, cx));

        div()
            // Exported shipped-surface AX anchor (§6): the pane-strip (toolbar)
            // root, found by an AX walk on role + label. `.id()` + a non-generic
            // `.role()` are what expose an element to the macOS AX tree; the
            // `aria_label` becomes its `AXTitle`.
            .id(PANE_STRIP_ROOT_LABEL)
            .role(gpui::Role::Group)
            .aria_label(PANE_STRIP_ROOT_LABEL)
            // Unpadded (the padding lives on `content`) + `relative` so the
            // single-tab title overlay + popups position against the full window
            // width. No fill and no bottom rule — the fill-less restyle titlebar;
            // the window-body backing shows through (plan
            // `docs/plans/restyle/01-titlebar-restyle.md`).
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("WindowToolbar")
            .w_full()
            .h(px(TOP_BAR_HEIGHT))
            // Empty-band drag / double-click on the whole bar (the tabs + buttons
            // stop_propagation so only the empty titlebar reaches these — the R9
            // differential pair; the single-tab title overlay passes clicks through).
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_band_mouse_down))
            .on_mouse_move(cx.listener(Self::on_band_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_band_mouse_up))
            .child(content)
            // The single-pane centered title overlay (absolute, full window width,
            // click pass-through) — painted above the pill-less strip, below the popups.
            .children(single_tab_title)
            .children(self.context_menu.clone())
            // The update popover (D9), rendered as a deferred child while open.
            .children(self.update_popover.clone())
    }
}

// ---- Scenario accessors -----------------------------------------------------
//
// Read/drive surface the live `pane-strip` self-test scenario (slice 3) uses to
// ground-truth the strip against AppKit reads. All `pub(crate)`; the drive
// methods route through the real select/close/add/rename paths so the scenario
// exercises the shipped behaviour, not a shortcut.
impl WindowToolbarView {
    /// The active tab's pane ids, in order.
    pub(crate) fn pane_ids(&self, cx: &App) -> Vec<String> {
        self.active_tab(cx)
            .map(|t| t.panes.iter().map(|p| p.id.clone()).collect())
            .unwrap_or_default()
    }

    /// The active pane id, if any.
    pub(crate) fn active_pane_id(&self, cx: &App) -> Option<String> {
        self.active_tab(cx)
            .and_then(|t| t.active_pane_id.clone())
    }

    /// The current pill-reorder drop slot `(target_pane_id, place_after)` — the
    /// gated `drag_target` (D7), the deterministic slot the in-process reorder
    /// itests assert against and the live scenario can read.
    pub(crate) fn scenario_drag_target(&self) -> Option<(String, bool)> {
        self.drag_target.clone()
    }

    /// Whether the overflow chevron currently renders.
    pub(crate) fn scenario_show_chevron(&self, cx: &App) -> bool {
        self.show_chevron(cx)
    }

    /// Whether the strip is in single-tab mode — exactly one pane, so the tab
    /// boxes are replaced by the centered titlebar title (round-2 plan 4). The
    /// `pane-strip` scenario reads this after closing the strip down to one pane.
    pub(crate) fn scenario_single_tab_active(&self, cx: &App) -> bool {
        self.active_tab(cx).map(|t| t.panes.len() == 1).unwrap_or(false)
    }

    /// The single-tab centered title (the sole pane's title) when in single-tab
    /// mode, else `None` — the scenario asserts it reads the surviving pane's name.
    pub(crate) fn scenario_single_tab_title(&self, cx: &App) -> Option<String> {
        let tab = self.active_tab(cx)?;
        (tab.panes.len() == 1).then(|| tab.panes[0].title.clone())
    }

    /// Whether the overflow (or a pill) context menu is currently open — the live
    /// scenario reads this after a synthetic click on the chevron to confirm the
    /// menu opened.
    pub(crate) fn scenario_menu_open(&self) -> bool {
        self.context_menu.is_some()
    }

    /// The fully-offscreen pane ids (drives the fades / badge assertions).
    pub(crate) fn scenario_offscreen_pane_ids(&self, cx: &App) -> std::collections::HashSet<String> {
        self.strip_geometry(cx).offscreen_pane_ids()
    }

    // --- R27 update pill / popover (the `update-check` scenario read/drive seam) ---

    /// Whether the trailing update pill's render gate is satisfied (a newer
    /// release is available) — the `update-check` scenario's deterministic pill
    /// visibility read, alongside the real AX-tree walk.
    pub(crate) fn scenario_update_pill_visible(&self, cx: &App) -> bool {
        crate::release_check::update_available(cx).is_some()
    }

    /// The update pill's painted centre in window-content coords `(x, y_from_top)`,
    /// or `None` until it has painted — the real guarded-HID click target.
    pub(crate) fn scenario_update_pill_center(&self) -> Option<(f64, f64)> {
        let b = self.update_pill_bounds.get()?;
        let x = f32::from(b.origin.x) + f32::from(b.size.width) / 2.0;
        let y = f32::from(b.origin.y) + f32::from(b.size.height) / 2.0;
        Some((x as f64, y as f64))
    }

    /// Open the update popover in-process (the deterministic drive path — the
    /// content/copy assertions must run even when a synthetic global-HID click on
    /// the pill is DEFERRED). A no-op when already open. Anchored at the window
    /// origin — the anchor point is irrelevant to the asserted content.
    pub(crate) fn drive_open_update_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_popover.is_none() {
            self.toggle_update_popover(point(px(0.0), px(0.0)), window, cx);
        }
    }

    /// Whether the update popover is currently open — the scenario reads this
    /// after a real pill click to confirm it opened.
    pub(crate) fn scenario_update_popover_open(&self) -> bool {
        self.update_popover.is_some()
    }

    /// The open popover's two brew command strings, or `None` when it is closed —
    /// the scenario asserts both exact commands are present, in order.
    pub(crate) fn scenario_update_popover_commands(&self, cx: &App) -> Option<Vec<String>> {
        self.update_popover
            .as_ref()
            .map(|p| p.read(cx).scenario_commands())
    }

    /// Drive one Copy in the open popover (writes command `index` to the
    /// clipboard) — the scenario then asserts the clipboard holds that command. A
    /// no-op when the popover is closed.
    pub(crate) fn drive_copy_update_command(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(popover) = self.update_popover.clone() {
            popover.update(cx, |p, cx| p.copy_command(index, cx));
        }
    }

    /// Drop the update popover (between the scenario's success and error legs).
    pub(crate) fn drive_dismiss_update_popover(&mut self, cx: &mut Context<Self>) {
        self.update_popover = None;
        self.update_popover_sub = None;
        cx.notify();
    }

    /// Whether the attention badge should light (a fully-offscreen pane needs
    /// attention).
    pub(crate) fn scenario_has_offscreen_attention(&self, cx: &App) -> bool {
        self.has_offscreen_attention(cx)
    }

    /// The current horizontal scroll offset (drives the centering assertion).
    pub(crate) fn scenario_scroll_offset_x(&self) -> f32 {
        f32::from(self.scroll.offset().x)
    }

    /// The pill's window-space bounds, if laid out (drives the ×-slot width
    /// equality + centering assertions).
    pub(crate) fn scenario_pill_bounds(&self, pane_id: &str, cx: &App) -> Option<Bounds<Pixels>> {
        let tab = self.active_tab(cx)?;
        let ix = tab.panes.iter().position(|p| p.id == pane_id)?;
        self.scroll.bounds_for_item(ix)
    }

    /// The on-screen content-view centre of `pane_id`'s trailing "×" close square,
    /// as `(x, y_from_top)` — the R20.5 `close-confirmation` scenario's real-CGEvent
    /// target. The `×` is the tab's last child: `TAB_PAD_X` in from the right edge,
    /// `CLOSE_BTN_SIZE` wide, so its centre sits `TAB_PAD_X + CLOSE_BTN_SIZE/2` left
    /// of the tab's right edge. The offset-free pill bounds get the live scroll
    /// offset applied (matching `scenario_pill_bounds`'s coordinate convention).
    /// `None` when the pane is not laid out. NB the `×` is only hit-testable while
    /// the pane is hovered/active — select it first.
    pub(crate) fn scenario_close_button_center(&self, pane_id: &str, cx: &App) -> Option<(f32, f32)> {
        let b = self.scenario_pill_bounds(pane_id, cx)?;
        let off = f32::from(self.scroll.offset().x);
        let right = f32::from(b.origin.x) + off + f32::from(b.size.width);
        let x = right - TAB_PAD_X - CLOSE_BTN_SIZE / 2.0;
        let y = f32::from(b.origin.y) + f32::from(b.size.height) / 2.0;
        Some((x, y))
    }

    /// Drive a pane selection through the real path.
    pub(crate) fn drive_select_pane(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.select_pane(pane_id, window, cx);
    }

    /// Drive a terminal-pane add through the real path.
    pub(crate) fn drive_add_terminal_pane(&mut self, cx: &mut Context<Self>) {
        self.add_terminal_pane(cx);
    }

    /// Drive a pane close through the real path.
    pub(crate) fn drive_close_pane(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pane(pane_id, window, cx);
    }

    /// Begin an inline rename of the ACTIVE pane through the real path (the
    /// gate-passed title tap and the context-menu Rename entry both land in
    /// `begin_editing`) — the `app-shell` scenario's focus-routing driver.
    pub(crate) fn drive_begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id(cx) else {
            return;
        };
        let Some(pane_id) = self.active_pane_id(cx) else {
            return;
        };
        self.begin_editing(&tab_id, &pane_id, window, cx);
    }

    /// Whether an inline pane rename is in flight.
    pub(crate) fn scenario_rename_editing(&self) -> bool {
        self.editing_pane.is_some()
    }

    /// The in-flight rename draft (the scenario's "keys land in the field" read).
    pub(crate) fn scenario_rename_draft(&self) -> String {
        self.rename_editor.as_ref().map(|e| e.text()).unwrap_or_default()
    }

    /// The in-flight rename selection `(start, end)` as char offsets — the
    /// scenario asserts caret moves / mid-string edits through it.
    pub(crate) fn scenario_rename_selection(&self) -> Option<(usize, usize)> {
        self.rename_editor.as_ref().map(|e| e.selection())
    }

    /// Move the rename caret one char left/right (the scenario's arrow-key driver
    /// — direct so it needn't post an arrow CGEvent).
    pub(crate) fn drive_rename_arrow(&mut self, right: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.rename_editor.as_mut() {
            editor.apply_key(if right {
                nice_model::file_browser::TextFieldKey::Right
            } else {
                nice_model::file_browser::TextFieldKey::Left
            });
            cx.notify();
        }
    }

    /// Whether the rename field currently holds key focus.
    pub(crate) fn scenario_rename_focused(&self, window: &Window) -> bool {
        self.rename_focus.is_focused(window)
    }

    /// Set a pane's `(status, viewed)` on the model (the scenario drives attention
    /// via the model, never a second predicate).
    pub(crate) fn drive_pane_status(
        &mut self,
        pane_id: &str,
        status: TabStatus,
        being_viewed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.active_tab_id(cx) else {
            return;
        };
        let changed = self.state.update(cx, |ws, _| {
            if let Some((pi, ti)) = ws.model.project_tab_index(&tab_id) {
                if let Some(pane) = ws.model.projects[pi].tabs[ti]
                    .panes
                    .iter_mut()
                    .find(|p| p.id == pane_id)
                {
                    pane.apply_status_transition(status, being_viewed);
                    return true;
                }
            }
            false
        });
        if changed {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::Pane;

    #[test]
    fn rename_and_close_labels_are_per_kind() {
        // WindowToolbarView.swift:751,755.
        assert_eq!(rename_menu_label(PaneKind::Terminal), "Rename Terminal");
        assert_eq!(rename_menu_label(PaneKind::Claude), "Rename Pane");
        assert_eq!(close_menu_label(PaneKind::Terminal), "Close Terminal");
        assert_eq!(close_menu_label(PaneKind::Claude), "Close Pane");
    }

    #[test]
    fn overflow_row_label_marks_only_the_active_pane() {
        let term = Pane::new("t", "Terminal 1", PaneKind::Terminal);
        let claude = Pane::new("c", "Claude", PaneKind::Claude);

        // The active row carries the checkmark; the others do not.
        assert!(overflow_row_label(&term, Some("t")).ends_with(ICON_CHECK));
        assert!(!overflow_row_label(&term, Some("c")).contains(ICON_CHECK));
        assert!(overflow_row_label(&claude, Some("c")).ends_with(ICON_CHECK));
        // Each row carries its per-kind glyph + title.
        assert!(overflow_row_label(&term, None).contains("Terminal 1"));
        assert!(overflow_row_label(&term, None).starts_with(ICON_TERMINAL));
        assert!(overflow_row_label(&claude, None).starts_with(ICON_CLAUDE_DOT));
    }

    #[test]
    fn viewport_relative_rect_translates_by_offset_and_origin() {
        // Child laid out at window-x 300 (offset-free), scrolled left by 120,
        // viewport starting at window-x 40: viewport-relative x = 300 - 120 - 40.
        let r = viewport_relative_rect(300.0, 100.0, -120.0, 40.0);
        assert_eq!(r.x, 140.0);
        assert_eq!(r.width, 100.0);
        assert_eq!(r.min_x(), 140.0);
        assert_eq!(r.max_x(), 240.0);
    }

    #[test]
    fn viewport_relative_rect_at_rest_is_offset_only_by_the_origin() {
        // No scroll (offset 0), viewport at 0: the child's rect passes through.
        let r = viewport_relative_rect(0.0, 80.0, 0.0, 0.0);
        assert_eq!(r.x, 0.0);
        assert_eq!(r.width, 80.0);
    }

    #[test]
    fn band_drag_threshold_matches_the_r9_two_point_rule() {
        assert!(!band_drag_threshold_crossed(1.0, 1.0)); // 2 < 4
        assert!(band_drag_threshold_crossed(2.0, 0.0)); // 4 >= 4
        assert!(band_drag_threshold_crossed(0.0, 3.0)); // 9 >= 4
    }

    #[test]
    fn tab_underline_is_the_round2_thin_1px_geometry() {
        // Round-2 restyle plan 4 thinned both the active (accent) and inactive
        // (grey) tab underline from 2px to 1px, tracking the updated mock Style A
        // `.tab::after` / `.tab.active::after` (supersedes plan 1's 2px). The
        // inset from the tab's outer edges is unchanged at 11px.
        assert_eq!(TAB_UNDERLINE_HEIGHT, 1.0);
        assert_eq!(TAB_UNDERLINE_RADIUS, 0.5);
        assert_eq!(TAB_UNDERLINE_INSET, 11.0);
    }

    #[test]
    fn single_tab_edge_inset_matches_the_mock() {
        // Round-2 plan 4 "Single-tab mode": the centered titlebar title is inset
        // this far from BOTH window edges (symmetric, keeping it centered on the
        // window's true center) so it clears the traffic lights on the left and the
        // tail `+` on the right — mock `.tab-single { left: 90px; right: 90px }`.
        assert_eq!(SINGLE_TAB_EDGE_INSET, 90.0);
    }

    #[test]
    fn reserved_square_slot_width_matches_the_estimator_constant() {
        // 4 (leading pad) + 22 (button) + 2 (row gap) = 28 —
        // PaneStripOverflowEstimator.swift:59-65's chevronSlotWidth / newTabSlotWidth.
        assert_eq!(SQUARE_SLOT_WIDTH, 28.0);
    }
}
