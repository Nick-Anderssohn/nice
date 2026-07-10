//! The window toolbar's pane strip — ported from
//! `Sources/Nice/Views/WindowToolbarView.swift` (`WindowToolbarView`,
//! `InlinePaneStrip`, `InlinePanePill`, `CloseXButton`, `OverflowMenuButton`,
//! `NewTabBtn`) and `Logo.swift`. The brand block, the horizontally-scrolling row
//! of pane pills, the overflow chevron with its attention badge and edge fades,
//! and the trailing `+` — all riding the R9 chrome band and driving the R8 model
//! through the injected [`PaneStripActions`] seam.
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
    div, linear_color_stop, linear_gradient, point, prelude::*, px, App, Bounds, BoxShadow, Context,
    DismissEvent, DragMoveEvent, Entity, FocusHandle, Focusable, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollHandle, SharedString,
    Subscription, Window,
};

use nice_model::file_browser::TextFieldEditor;
use nice_model::{
    center_offset_x, resolve, should_show_overflow_chevron, Pane, PaneKind, Rect, StripGeometry,
    Tab, TabStatus,
};
use nice_theme::chrome_geometry::TOP_BAR_HEIGHT;
use nice_theme::palette::Slots;

use crate::app_shell::{PaneHostView, PANE_STRIP_ROOT_LABEL};
use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::inline_rename::{
    dispatch_rename_key, edit_spans, rename_field, FieldColors, FieldProbe, RenameKeyOutcome,
};
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::status_dot::StatusDot;
use crate::theme::{slot_srgba, slot_to_rgba, srgba_to_rgba, srgba_with_alpha};
use crate::update_popover::UpdatePopover;
use crate::window_state::WindowState;

// ---- Geometry / behaviour constants (Swift provenance) ----------------------

/// Toolbar leading inset (`WindowToolbarView.swift:56`).
const TOOLBAR_LEADING_PAD: f32 = 14.0;
/// Toolbar trailing inset (`WindowToolbarView.swift:57`).
const TOOLBAR_TRAILING_PAD: f32 = 20.0;
/// Brand-block inter-element spacing (the outer `HStack(spacing: 10)`,
/// `WindowToolbarView.swift:31`).
const BRAND_GAP: f32 = 10.0;
/// The brand mark's box (`Logo` default size, `Logo.swift:24`).
const LOGO_SIZE: f32 = 22.0;
/// The brand mark's inner rounded square (`Logo.swift:39-41`).
const LOGO_SQUARE: f32 = 20.0;
const LOGO_SQUARE_RADIUS: f32 = 6.0;
/// The vertical brand/strip separator (`WindowToolbarView.swift:42-45`).
const SEPARATOR_HEIGHT: f32 = 20.0;
const SEPARATOR_MARGIN_X: f32 = 6.0;

/// Pill box (`WindowToolbarView.swift:777-778`).
const PILL_HEIGHT: f32 = 28.0;
const PILL_MAX_WIDTH: f32 = 220.0;
/// Pill leading / trailing padding (`WindowToolbarView.swift:775-776`).
const PILL_LEADING_PAD: f32 = 10.0;
const PILL_TRAILING_PAD: f32 = 6.0;
/// Pill inner spacing (`HStack(spacing: 7)`, `WindowToolbarView.swift:759`).
const PILL_GAP: f32 = 7.0;
/// Pill corner radius (`WindowToolbarView.swift:780`).
const PILL_RADIUS: f32 = 7.0;
/// Pill title / icon text size (`WindowToolbarView.swift:860,904`).
const PILL_TEXT_SIZE: f32 = 12.0;
/// Leading terminal-glyph box (`WindowToolbarView.swift:906`).
const PILL_ICON_SIZE: f32 = 12.0;
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
const SQUARE_BTN_SIZE: f32 = 22.0;
const SQUARE_BTN_RADIUS: f32 = 5.0;
const CHEVRON_GLYPH_SIZE: f32 = 10.0;
const PLUS_GLYPH_SIZE: f32 = 11.0;
/// The chevron / new-tab leading pad inside their slot (`.padding(.leading, 4)`,
/// `WindowToolbarView.swift:238,245`).
const SQUARE_BTN_LEADING_PAD: f32 = 4.0;
/// Width of the chevron slot and the `+` slot — each **always** reserved in the
/// tracked scroll layout so the pill viewport is a fixed width and the overflow
/// decision never depends on the chevron's own visibility (the reservation rule,
/// `PaneStripOverflowEstimator.swift:59-65`: 22 button + 4 lead + 2 gap ≈ 28).
const SQUARE_SLOT_WIDTH: f32 = SQUARE_BTN_LEADING_PAD + SQUARE_BTN_SIZE + PILL_ROW_GAP;

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

/// Pill hover fill: 5% ink (`WindowToolbarView.swift:715`).
const PILL_HOVER_INK_ALPHA: f32 = 0.05;
/// Close-"×" hover fill: 10% ink (`WindowToolbarView.swift:992`).
const CLOSE_HOVER_INK_ALPHA: f32 = 0.10;
/// Chevron / new-tab hover fill: 8% ink (`WindowToolbarView.swift:1054,1143`).
const SQUARE_BTN_HOVER_INK_ALPHA: f32 = 0.08;
/// Active-pill drop shadow (`WindowToolbarView.swift:787-792`).
const PILL_SHADOW_ALPHA: f32 = 0.04;

// ---- Icons (SF Symbols + Unicode fallbacks / stand-ins) ----------------------
//
// The pill/chevron/plus/close icons are real SF Symbols rendered through
// `crate::sf_symbols` (M2 feel-check Item A); each ICON_* glyph remains as the
// never-blank fallback. The overflow-menu rows keep their glyph stand-ins (the
// pinned `ContextMenu` is plain-label), and the logo mark keeps the `❯`
// stand-in (Swift's custom SVG mark stays out of scope this cycle).

const ICON_TERMINAL: &str = "\u{276F}"; // ❯  fallback for SF_TERMINAL + menu rows
const ICON_CLOSE: &str = "\u{2715}"; // ✕  fallback for SF_CLOSE
const ICON_CHEVRON_DOWN: &str = "\u{25BE}"; // ▾  fallback for SF_CHEVRON_DOWN
const ICON_PLUS: &str = "+"; // fallback for SF_PLUS
const ICON_CHECK: &str = "\u{2713}"; // ✓  (menu-row stand-in, SF "checkmark")
const ICON_CLAUDE_DOT: &str = "\u{25CF}"; // ●  (menu-row stand-in for the StatusDot)
/// The white brand mark inside the accent square — a stand-in for the SVG
/// chevron+underline in `Logo.swift` (no SVG asset pipeline this cycle).
const ICON_LOGO_MARK: &str = "\u{276F}"; // ❯

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

/// Opaque white (the brand mark's stroke colour).
fn white() -> Rgba {
    Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    }
}

/// The active-pill drop shadow (`WindowToolbarView.swift:787-792`).
fn pill_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: PILL_SHADOW_ALPHA,
        }
        .into(),
        offset: point(px(0.0), px(1.0)),
        blur_radius: px(1.0),
        spread_radius: px(0.0),
        inset: false,
    }]
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
/// (D4). gpui positions it under the cursor automatically, so it does not
/// position itself.
struct PaneDragGhost {
    title: SharedString,
}

impl Render for PaneDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = active_slots(cx);
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(PILL_HEIGHT))
            .max_w(px(PILL_MAX_WIDTH))
            .pl(px(PILL_LEADING_PAD))
            .pr(px(PILL_TRAILING_PAD))
            .rounded(px(PILL_RADIUS))
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .opacity(0.85)
            .text_size(px(PILL_TEXT_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(slot_to_rgba(s.ink))
            .whitespace_nowrap()
            .child(self.title.clone())
    }
}

// ---- The view ---------------------------------------------------------------

/// The per-window toolbar (brand block + pane strip). Construct with
/// [`WindowToolbarView::new`] over the window's shared [`WindowState`] entity; it
/// renders the shared `model`'s active-tab panes and mutates them through the
/// `pane_strip_actions` seam.
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
        // Cursor at the end (typing appends) — the prior char-append behaviour.
        self.rename_editor = Some(TextFieldEditor::new(&title));
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

    /// Reposition the caret from a click hit-test — collapse the selection to the
    /// clicked boundary and re-grab field focus (the click already stopped
    /// propagation, so the pill's select/rename gate never re-trips).
    fn place_rename_cursor(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.rename_editor.as_mut() {
            editor.place_cursor(index);
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

    fn render_brand(&self, s: &Slots, cx: &App) -> impl IntoElement {
        let accent = srgba_to_rgba(crate::theme_settings::active_chrome_accent(cx));
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(BRAND_GAP))
            .flex_none()
            // Brand mark: an accent rounded square with a white chevron stand-in.
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(LOGO_SIZE))
                    .h(px(LOGO_SIZE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(LOGO_SQUARE))
                            .h(px(LOGO_SQUARE))
                            .rounded(px(LOGO_SQUARE_RADIUS))
                            .bg(accent)
                            .text_size(px(11.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(white())
                            .child(SharedString::from(ICON_LOGO_MARK)),
                    ),
            )
            // Wordmark.
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(slot_to_rgba(s.ink))
                    .child(SharedString::from("Nice")),
            )
            // Vertical separator (1×20, 6pt side margins).
            .child(
                div()
                    .flex_none()
                    .w(px(1.0))
                    .h(px(SEPARATOR_HEIGHT))
                    .mx(px(SEPARATOR_MARGIN_X))
                    .bg(slot_to_rgba(s.line)),
            )
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
    /// then persists explicitly via [`WindowState::save_to_store`] (D5: the
    /// `on_tree_mutation` observer is wired nowhere, so a reorder would not
    /// otherwise save). Selection/focus are untouched (`move_pane` never touches
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
        for vm in panes {
            row = row.child(self.render_pill(vm, &tab_id, s, cx));
        }

        // The scroll wrapper carries the two edge fades as absolute overlays so
        // they sit at the viewport's own leading / trailing edges. It is also the
        // viewport-fixed host for the reorder insertion line (D10): `scroll_wrap`'s
        // origin is the viewport left, and `pane_frames` are viewport-relative, so
        // the line's x is directly the target frame edge.
        let scroll_wrap = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(PILL_HEIGHT))
            .child(row)
            .when(geometry.can_scroll_leading(), |el| {
                el.child(self.edge_fade(false, s))
            })
            .when(geometry.can_scroll_trailing(), |el| {
                el.child(self.edge_fade(true, s))
            })
            .children(self.insertion_line(cx));

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .child(scroll_wrap)
            .child(self.render_chevron_slot(show_chevron, has_attention, s, cx))
            .child(self.render_new_tab_slot(s, cx))
    }

    /// A 16pt gradient from the chrome fill (opaque) to transparent, at the
    /// viewport's leading (`trailing == false`) or trailing edge. Never hit-tests.
    fn edge_fade(&self, trailing: bool, s: &Slots) -> impl IntoElement {
        let chrome = slot_srgba(s.chrome);
        let opaque = srgba_to_rgba(chrome);
        let clear = srgba_to_rgba(srgba_with_alpha(chrome, 0.0));
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

    fn render_pill(&self, vm: &PaneVm, tab_id: &str, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        let accent = crate::theme_settings::active_chrome_accent(cx);
        let is_active = vm.is_active;
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let ink3 = slot_to_rgba(s.ink3);

        // Leading icon: per-pane StatusDot for Claude, else the `terminal`
        // symbol — 12pt regular in a 12pt box (`WindowToolbarView.swift:903-906`).
        let leading = match vm.kind {
            PaneKind::Claude => StatusDot::new(
                SharedString::from(format!("pill.{}", vm.id)),
                vm.status,
                accent,
                slot_srgba(s.ink3),
            )
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
                    if is_active { ink2 } else { ink3 },
                    self.window_scale,
                    cx,
                ))
                .into_any_element(),
        };

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
                move |index, window, app| {
                    let _ = weak.update(app, |this, cx| this.place_rename_cursor(index, window, cx));
                },
            )
            .into_any_element()
        } else {
            let pid = vm.id.clone();
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .truncate()
                .text_size(px(PILL_TEXT_SIZE))
                .font_weight(if is_active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(if is_active { ink } else { ink2 })
                .child(SharedString::from(vm.title.clone()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e: &MouseDownEvent, window, cx| {
                        this.handle_title_tap(&pid, window, cx);
                        cx.stop_propagation();
                    }),
                )
                .into_any_element()
        };

        // Trailing close "×" — its 16pt slot is always reserved (visibility
        // toggled) so the pill width never jumps on hover.
        let show_close = vm.is_hovered || is_active;
        let close = self.render_close_button(&vm.id, show_close, s, cx);

        let bg = if is_active {
            slot_to_rgba(s.panel)
        } else if vm.is_hovered {
            ink_alpha(s, PILL_HOVER_INK_ALPHA)
        } else {
            Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }
        };
        let border = if is_active {
            slot_to_rgba(s.line)
        } else {
            Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }
        };

        let pid_select = vm.id.clone();
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

        let mut pill = div()
            .id(pill_id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(PILL_GAP))
            .pl(px(PILL_LEADING_PAD))
            .pr(px(PILL_TRAILING_PAD))
            .h(px(PILL_HEIGHT))
            .max_w(px(PILL_MAX_WIDTH))
            .rounded(px(PILL_RADIUS))
            .bg(bg)
            .border_1()
            .border_color(border)
            .child(leading)
            .child(title)
            .child(close)
            // The pill carries `.id()` + `on_drag` ONLY — `on_drag_move` / `on_drop`
            // live on the scroll row (the tracked viewport, D8). The ghost follows
            // the cursor (gpui positions it), so it ignores the constructor's
            // `Point` offset. Coexists with the mouse-down select below: gpui's
            // drag-arming recorder is a separate window-level listener keyed on the
            // hitbox hover, not this element's handler (D6, proven by the F9 file
            // drag).
            .on_drag(drag_payload, move |_payload, _offset, _window, app| {
                let title = ghost_title.clone();
                app.new(|_| PaneDragGhost { title })
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e: &MouseDownEvent, window, cx| {
                    // A press that bubbles here from inside the editing field (or
                    // on the editing pill's own icon/padding) must keep the edit
                    // alive, not commit + reselect — Swift's `if !isEditing`
                    // onTapGesture guard (`WindowToolbarView.swift:808`).
                    if this.is_editing_pane(&pid_select) {
                        cx.stop_propagation();
                        return;
                    }
                    this.select_pane(&pid_select, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    this.open_pill_context_menu(&pid_menu, kind, e.position, window, cx);
                    cx.stop_propagation();
                }),
            );
        if is_active {
            pill = pill.shadow(pill_shadow());
        }
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
                .pl(px(PILL_LEADING_PAD))
                .pr(px(PILL_TRAILING_PAD))
                .rounded(px(PILL_RADIUS))
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

        div()
            // Exported shipped-surface AX anchor (§6): the pane-strip (toolbar)
            // root, found by an AX walk on role + label. `.id()` + a non-generic
            // `.role()` are what expose an element to the macOS AX tree; the
            // `aria_label` becomes its `AXTitle`.
            .id(PANE_STRIP_ROOT_LABEL)
            .role(gpui::Role::Group)
            .aria_label(PANE_STRIP_ROOT_LABEL)
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("WindowToolbar")
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(TOP_BAR_HEIGHT))
            .pl(px(TOOLBAR_LEADING_PAD))
            .pr(px(TOOLBAR_TRAILING_PAD))
            .bg(slot_to_rgba(s.chrome))
            // Empty-band drag / double-click (the pills + buttons stop_propagation
            // so only the empty chrome reaches these — the R9 differential pair).
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_band_mouse_down))
            .on_mouse_move(cx.listener(Self::on_band_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_band_mouse_up))
            .child(self.render_brand(&s, cx))
            .child(self.render_strip(&panes, &s, cx))
            // Trailing update-pill slot (R27, P7): the conditional update pill,
            // inserted between the strip and the popup layer. It renders ONLY when
            // a newer release is available — absent, it emits nothing, so the
            // toolbar with no update is byte-identical to today. The toolbar's old
            // local bottom hairline is gone — the shell paints one full-width
            // title-bar divider at window level instead
            // (`SidebarShellView::build_top_bar_divider`, M2 Item C).
            .children(self.render_update_pill(&s, cx))
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
    /// target. The `×` is the pill's last child: `PILL_TRAILING_PAD` in from the
    /// right edge, `CLOSE_BTN_SIZE` wide, so its centre sits `PILL_TRAILING_PAD +
    /// CLOSE_BTN_SIZE/2` left of the pill's right edge. The offset-free pill bounds
    /// get the live scroll offset applied (matching `scenario_pill_bounds`'s
    /// coordinate convention). `None` when the pane is not laid out. NB the `×` is
    /// only hit-testable while the pane is hovered/active — select it first.
    pub(crate) fn scenario_close_button_center(&self, pane_id: &str, cx: &App) -> Option<(f32, f32)> {
        let b = self.scenario_pill_bounds(pane_id, cx)?;
        let off = f32::from(self.scroll.offset().x);
        let right = f32::from(b.origin.x) + off + f32::from(b.size.width);
        let x = right - PILL_TRAILING_PAD - CLOSE_BTN_SIZE / 2.0;
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
    fn reserved_square_slot_width_matches_the_estimator_constant() {
        // 4 (leading pad) + 22 (button) + 2 (row gap) = 28 —
        // PaneStripOverflowEstimator.swift:59-65's chevronSlotWidth / newTabSlotWidth.
        assert_eq!(SQUARE_SLOT_WIDTH, 28.0);
    }
}
