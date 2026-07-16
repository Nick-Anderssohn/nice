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
//! insertion line) is ported here with gpui's own drag pipeline (M7.8 feel-check
//! round 2): rows arm an [`TabDragPayload`] drag via `on_drag`, each project
//! group's container hosts `on_drag_move`/`on_drop` with bounds-containment
//! clearing (the R25 listener split), and [`tab_drop_target`] is the pure
//! midpoint resolver (`SidebarDropResolver.tabTarget`,
//! `SidebarView.swift:994-1013`).
//!
//! ## Icons
//!
//! The header/footer/row icons are real SF Symbols rendered at runtime through
//! [`crate::sf_symbols`] (`NSImage(systemSymbolName:)` rasterized + tinted at
//! the window's backing scale, cached per size/weight/colour/scale — M2
//! feel-check Item A). Each keeps its original Unicode stand-in as a
//! never-blank fallback for a symbol name that fails to resolve. The
//! disclosure chevron is an SF-Symbol **swap** (`chevron.right` closed /
//! `chevron.down` open, the file-browser idiom from fix round r4) rather than
//! prod's 0°→90° rotation transform — the pinned gpui exposes no element
//! rotation; the swapped-in `chevron.down` is the same drawn shape. It sits in
//! a fixed-width slot matching `chevron.right`'s natural layout box, so the
//! header text never shifts on toggle (prod's rotation keeps the closed box
//! too, `SidebarView.swift:289-295`).

// The view + its install fn have no in-crate caller until slice 4 wires the
// `sidebar` self-test scenario; it is a deliberately-exported surface (plan
// "Exported contracts"). The pure layout/label helpers below ARE exercised by
// this module's unit tests.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, ClickEvent, Context, CursorStyle,
    DismissEvent, DragMoveEvent, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba,
    SharedString, Subscription, Window,
};

use nice_model::file_browser::TextFieldEditor;
use nice_model::{InlineRenameClickGate, SidebarMode, TabModel, TabStatus};
use nice_theme::chrome_geometry::{
    CARD_CORNER_RADIUS, CARD_SHADOW_OPACITY, CARD_SHADOW_RADIUS, CARD_SHADOW_Y_OFFSET,
    INNER_CORNER_RADIUS, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH,
    SIDEBAR_PEEK_WIDTH, SIDEBAR_RESIZE_HANDLE_WIDTH, TOP_BAR_HEIGHT,
};
use nice_theme::color::Srgba;
use nice_theme::glass::{glass_fill, glass_line};
use nice_theme::palette::{ColorScheme, Slots};

use crate::app_shell::{PaneHostView, SIDEBAR_ROOT_LABEL};
use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::file_browser::view::FileBrowserView;
use crate::inline_rename::{
    apply_rename_click, dispatch_rename_key, edit_spans, rename_field, FieldColors, FieldProbe,
    RenameKeyOutcome,
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
/// Dark-scheme selection tint alpha applied to the accent (`Palette.swift:225`)
/// — the inline-rename field's text-selection highlight (the flattened rows no
/// longer tint with it; hover / multi-select use the over-glass
/// [`glass_fill`] instead).
const SEL_ALPHA_DARK: f32 = 0.22;
/// Count-pill background: 7% ink (`SidebarView.swift:360`).
const COUNT_PILL_INK_ALPHA: f32 = 0.07;
/// Group `+` button hover fill: 10% ink (`SidebarView.swift:387`).
const ADD_BUTTON_HOVER_ALPHA: f32 = 0.10;

// ---- Text line heights (AppKit parity) --------------------------------------
//
// gpui's default line height is `phi()` (≈1.618× the font size), which inflates
// every content-sized sidebar box: a 13px row title gets a ~21px line box where
// AppKit/SwiftUI gives the 13pt system font a 16pt line
// (`NSLayoutManager.defaultLineHeight`), making RS rows ~33px tall vs prod's
// 28pt. These are the measured `defaultLineHeight(for: .systemFont(ofSize:))`
// values for the sizes the sidebar uses; each is scaled through
// [`sidebar_pt`](SidebarShellView::sidebar_pt) alongside its font size.

/// Line height for the 13pt row title (row = max(16, 16+4) + 8 = 28pt, the
/// Swift TabRow height).
const LINE_HEIGHT_13: f32 = 16.0;
/// Line height for the 12pt group-header name.
const LINE_HEIGHT_12: f32 = 15.0;
/// Line height for the 10pt chevron glyph / count-pill text.
const LINE_HEIGHT_10: f32 = 12.0;
/// Fixed width of the group-header disclosure slot — `chevron.right`'s natural
/// layout box at the 10pt symbol size (8×11pt canvas). Prod gives the chevron
/// no frame and rotates it, which keeps this same closed-state box
/// (`SidebarView.swift:289-295`); pinning the slot keeps the header text from
/// shifting when the wider `chevron.down` (11pt canvas) swaps in.
const HEADER_DISCLOSURE_SLOT: f32 = 8.0;

// ---- Icons (SF Symbols + their Unicode fallbacks — see module docs) ---------

const ICON_CHEVRON_CLOSED: &str = "\u{25B8}"; // ▸ fallback for SF_CHEVRON_CLOSED
const ICON_CHEVRON_OPEN: &str = "\u{25BE}"; // ▾ fallback for SF_CHEVRON_OPEN
const ICON_TERMINAL: &str = "\u{276F}"; // ❯ fallback for SF_TERMINAL
const ICON_PLUS: &str = "+"; // fallback for SF_PLUS

/// Group-header disclosure, closed (`SidebarView.swift:289` — prod rotates
/// this one symbol; we swap in `chevron.down` for the open state instead).
const SF_CHEVRON_CLOSED: &str = "chevron.right";
/// Group-header disclosure, open.
const SF_CHEVRON_OPEN: &str = "chevron.down";
/// Tab-row / pill leading icon (`SidebarView.swift:602`).
const SF_TERMINAL: &str = "terminal";
/// Group-header add button (`SidebarView.swift:379`).
const SF_PLUS: &str = "plus";

// The footer mode-switcher (tabs / files) and settings gear now render the
// 2026-07 restyle's stroke SVGs from `crate::chrome_icons` (`MODE_TABS` /
// `MODE_FILES` / `MODE_GEAR`), not SF Symbols — see [`SidebarShellView::build_footer`].

/// Sidebar row status-dot size (pt). Matches the tab-strip dot
/// (`toolbar::TAB_STATUS_DOT_SIZE`, 7pt) so the sidebar and title-bar status
/// dots read the same size; the default is 8pt. Only the size parameter
/// changes — the dot's colours + pulse are untouched
/// (`docs/plans/restyle/02-sidebar-flatten.md`).
const SIDEBAR_ROW_DOT_SIZE: f32 = 7.0;

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

/// The disclosure chevron for an open/closed group as `(sf_symbol_name,
/// unicode_fallback)` (SF-Symbol swap — see the module docs).
fn disclosure_icon(is_open: bool) -> (&'static str, &'static str) {
    if is_open {
        (SF_CHEVRON_OPEN, ICON_CHEVRON_OPEN)
    } else {
        (SF_CHEVRON_CLOSED, ICON_CHEVRON_CLOSED)
    }
}

/// Pick the row slot a cursor y points at within a project group: above the
/// first row (the header area) → before it; below the last row (the trailing
/// gap) → after it; over a row → midpoint split; no match → `None`. The pure
/// port of `SidebarDropResolver.tabTarget` (`SidebarView.swift:994-1013`).
/// `frames` are each painted row's `(min_y, max_y)` in the same coordinate
/// space as `y` (window coords here); a collapsed group paints no rows, so its
/// ids have no frames and every branch misses — same net `nil` as Swift's
/// empty `tabFrames` snapshot.
fn tab_drop_target(
    y: f32,
    tab_order: &[String],
    frames: &HashMap<String, (f32, f32)>,
) -> Option<(String, bool)> {
    let first = tab_order.first()?;
    if let Some(&(min_y, _)) = frames.get(first) {
        if y < min_y {
            return Some((first.clone(), false));
        }
    }
    if let Some(last) = tab_order.last() {
        if let Some(&(_, max_y)) = frames.get(last) {
            if y > max_y {
                return Some((last.clone(), true));
            }
        }
    }
    for id in tab_order {
        if let Some(&(min_y, max_y)) = frames.get(id) {
            if y >= min_y && y <= max_y {
                return Some((id.clone(), y > (min_y + max_y) / 2.0));
            }
        }
    }
    None
}

/// Build the drop-resolution scope for a drag of `dragged` over one project
/// group — the target order and frame spans [`tab_drop_target`] resolves
/// against — subtree-aware (M7.8 round 3, matching [`TabModel::move_tab`]'s
/// block semantics):
///
/// * **Root drag** (dragged has no parent): one unit per top-level BLOCK
///   (root + its depth-1 children), keyed by the root id, spanning the union
///   of the block's painted row frames. Midpoint math then answers
///   before/after the WHOLE block — "after a parent visually at its bottom
///   edge" is after its last child, and no slot inside a group is ever
///   proposed (or a line drawn there).
/// * **Child drag**: only the rows of the dragged tab's own block (its root,
///   then the siblings) — a slot anywhere else resolves to `None`, and
///   `would_move_tab` rejects the boundary cases (before the root / other
///   blocks) the row scope still reaches.
///
/// `rows` is the group's `(tab_id, parent_tab_id)` list in display order;
/// `frames` the per-row painted extents. Pure — unit-tested below.
fn drag_scope(
    rows: &[(String, Option<String>)],
    dragged: &str,
    frames: &HashMap<String, (f32, f32)>,
) -> (Vec<String>, HashMap<String, (f32, f32)>) {
    let dragged_parent = rows
        .iter()
        .find(|(id, _)| id == dragged)
        .and_then(|(_, p)| p.clone());
    match dragged_parent {
        None => {
            // Block units keyed by root id, in first-appearance display order.
            let mut order: Vec<String> = Vec::new();
            let mut spans: HashMap<String, (f32, f32)> = HashMap::new();
            for (id, parent) in rows {
                let root = parent.as_ref().unwrap_or(id);
                if !order.iter().any(|r| r == root) {
                    order.push(root.clone());
                }
                if let Some(&(min_y, max_y)) = frames.get(id) {
                    spans
                        .entry(root.clone())
                        .and_modify(|s| {
                            s.0 = s.0.min(min_y);
                            s.1 = s.1.max(max_y);
                        })
                        .or_insert((min_y, max_y));
                }
            }
            (order, spans)
        }
        Some(root_id) => {
            let order: Vec<String> = rows
                .iter()
                .filter(|(id, p)| *id == root_id || p.as_deref() == Some(root_id.as_str()))
                .map(|(id, _)| id.clone())
                .collect();
            (order, frames.clone())
        }
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

/// The resolved terminal mono family (SF Mono default) the flattened sidebar
/// renders its text in — read from the process `SharedFontSettings` (the same
/// source the tab strip's titles use). Shared by the tabs-mode
/// [`SidebarShellView`] and the files-mode
/// [`FileBrowserView`](crate::file_browser::view::FileBrowserView) so both flat
/// surfaces use one family seam (plan `docs/plans/restyle/02-sidebar-flatten.md`).
/// `None` before the keymap wires that global (the isolated `sidebar` scenario),
/// which leaves gpui's default UI family. Only the FAMILY comes from here; text
/// SIZE stays proportional off each view's own sizing.
pub(crate) fn resolved_mono_family(cx: &App) -> Option<SharedString> {
    crate::keymap::try_shared_font_settings(cx).map(|f| f.read(cx).family())
}

/// The over-glass hairline colour for the flat sidebar's trailing divider — the
/// scheme-scoped [`glass_line`] value (white 8% dark / ink 10% light), converted
/// to gpui's `Rgba`. NOT the opaque theme `line` slot: the flattened sidebar
/// shares the window-body surface, so its divider must read correctly over that
/// shared (later translucent) surface (plan `docs/plans/restyle/02-sidebar-flatten.md`).
fn glass_line_rgba(scheme: ColorScheme) -> Rgba {
    srgba_to_rgba(glass_line(scheme))
}

/// The over-glass active / hover fill for the flat sidebar's rows and footer
/// mode buttons — the scheme-scoped [`glass_fill`] value (white 6% dark / ink 5%
/// light), converted to gpui's `Rgba`. Replaces the old accent `selection_tint`
/// row fills.
fn glass_fill_rgba(scheme: ColorScheme) -> Rgba {
    srgba_to_rgba(glass_fill(scheme))
}

/// The elevated peek-overlay drop shadow — the last surviving sidebar panel
/// shadow after the docked card flattened (`AppShellView.swift:838`; plan
/// `docs/plans/restyle/02-sidebar-flatten.md` keeps the peek elevated for
/// readability over live terminal content).
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
    /// Depth-1 children under this tab (the drag ghost's `+N` group hint).
    child_count: usize,
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

// ---- Row drag (the S7 sidebar reorder, ported on the R25 pattern) -----------

/// The value a sidebar row drag carries: just the dragged tab id (Swift stashes
/// it in `SidebarDragState` + an `NSItemProvider`, `SidebarView.swift:654-657`;
/// here it is a purely in-app gpui payload the `on_drop::<TabDragPayload>` type
/// gate matches on). Same-project-only is enforced by the model
/// ([`TabModel::would_move_tab`] refuses cross-project targets), not the
/// payload.
#[derive(Clone)]
struct TabDragPayload {
    tab_id: SharedString,
}

/// The drag ghost that follows the cursor: a simplified row chip (title only,
/// reduced opacity) — the R25 `PaneDragGhost` pattern, not a bitmap snapshot.
/// gpui lays it out at `mouse - offset` each frame, so it compensates by
/// re-adding `offset` (plus a small lead) as leading padding. A parent dragging
/// its child block appends a dim `+N` so the chip reads as the whole group.
struct TabRowDragGhost {
    title: SharedString,
    /// Depth-1 children coming along with the drag (0 for a childless row).
    child_count: usize,
    /// The pointer's position within the dragged row, captured at drag-arm time.
    /// gpui lays the ghost out at `mouse - offset`, so we re-add it (plus a small
    /// lead) as leading padding to net the ghost to `pointer + 12`.
    offset: Point<Pixels>,
}

impl Render for TabRowDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = active_slots(cx);
        // Outer wrapper carries the offset compensation as padding so the visible
        // chip's own background box isn't inflated.
        div().pl(self.offset.x + px(12.0)).pt(self.offset.y + px(12.0)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(24.0))
                .max_w(px(200.0))
                .px(px(10.0))
                .rounded(px(4.0))
                .bg(slot_to_rgba(s.panel))
                .border_1()
                .border_color(slot_to_rgba(s.line))
                .opacity(0.85)
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(slot_to_rgba(s.ink))
                .whitespace_nowrap()
                .truncate()
                .child(self.title.clone())
                .when(self.child_count > 0, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(slot_to_rgba(s.ink3))
                            .child(SharedString::from(format!("+{}", self.child_count))),
                    )
                }),
        )
    }
}

/// Where the reorder insertion line paints: the owning project group, the
/// already-gated slot, and the line's group-relative y (the target row's top or
/// bottom edge). Single-valued on the view so only one group ever paints a line
/// at a time — a new group's `on_drag_move` writing it implicitly clears any
/// stale line in another group (Swift's single-valued `SidebarDropTarget`,
/// `SidebarView.swift:823-831`).
#[derive(Clone, PartialEq)]
struct SidebarDropTarget {
    project_id: String,
    target_tab_id: String,
    place_after: bool,
    line_y: f32,
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

    /// Projects whose disclosure is collapsed (absent == open, the default).
    collapsed_projects: HashSet<String>,
    /// The project whose header is hovered (reveals its `+` button).
    hovered_project: Option<String>,

    /// Per-paint window-coord vertical extents `(min_y, max_y)` of each painted
    /// tab row, keyed by tab id — the Swift `TabFramesKey` preference analog
    /// (`SidebarView.swift:805-810`), written by a canvas probe inside each row
    /// and cleared at the top of every render so closed tabs / collapsed groups
    /// can't leave stale frames.
    row_frames: Rc<RefCell<HashMap<String, (f32, f32)>>>,
    /// The row-reorder drop slot the cursor currently resolves to, already gated
    /// through [`TabModel::would_move_tab`] (a no-op / cross-project slot
    /// resolves to `None`). Recomputed in each group's `on_drag_move`, cleared
    /// on drop / when the cursor leaves the owning group (the R25 `drag_target`
    /// pattern + Swift's `dropExited`). The insertion line reads it, gated
    /// additionally on `cx.has_active_drag()` so a dropped-nowhere release
    /// drops the line the same frame.
    drag_target: Option<SidebarDropTarget>,

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
    /// R23 (D3): the sidebar base point size, read from the app-level
    /// [`SharedSidebarFontSettings`](crate::settings::sidebar_font) each render.
    /// Sidebar text sizes scale proportionally off it ([`sidebar_pt`](Self::sidebar_pt));
    /// the 12pt default is identity, so an absent entity (the isolated scenarios)
    /// leaves the chrome pixel-identical.
    sidebar_font_px: f32,
    /// Re-render when the sidebar-font entity notifies (a Font-pane size change).
    /// `None` when the entity is absent (isolated scenarios).
    _sidebar_font_sub: Option<Subscription>,
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
        let state_sub = cx.observe(&state, |this, state, cx| {
            // Any sidebar expand — from ⌘B, the titlebar collapse toggle, or this
            // view's own toggle — routes through `WindowState::toggle_sidebar_collapsed`,
            // which notifies here. Drop the view-local hover pin whenever the
            // sidebar is not collapsed so a later collapse doesn't render a stale
            // peek overlay (`build_collapsed_shell`: `peeking_model || peek_mouse_pinned`).
            if !state.read(cx).sidebar.collapsed() {
                this.peek_mouse_pinned = false;
            }
            cx.notify();
        });
        // R23 (D3): observe the app-level sidebar-font entity so a Font-pane size
        // change repaints the sidebar chrome. Absent in isolated scenarios.
        let sidebar_font = crate::settings::sidebar_font::shared_sidebar_font(cx);
        let sidebar_font_px = sidebar_font
            .as_ref()
            .map(|e| e.read(cx).px())
            .unwrap_or(crate::settings::sidebar_font::DEFAULT_SIDEBAR_FONT_PX);
        let sidebar_font_sub =
            sidebar_font.map(|e| cx.observe(&e, |_this, _e, cx| cx.notify()));
        Self {
            state,
            _state_sub: state_sub,
            main_toolbar: None,
            main_body: None,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            drag_start_width: None,
            resize_origin_x: None,
            peek_mouse_pinned: false,
            collapsed_projects: HashSet::new(),
            hovered_project: None,
            row_frames: Rc::new(RefCell::new(HashMap::new())),
            drag_target: None,
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
            sidebar_font_px,
            _sidebar_font_sub: sidebar_font_sub,
        }
    }

    /// The proportional point size of a `base`-sized sidebar element against the
    /// 12pt anchor (`sidebar_size(sidebar_px, base)`, D3). At the 12pt default this
    /// is identity, so the chrome is unchanged until the user resizes the sidebar.
    fn sidebar_pt(&self, base: f32) -> f32 {
        crate::settings::sidebar_font::sidebar_size(self.sidebar_font_px, base)
    }

    /// The resolved terminal mono family (SF Mono default) the flattened sidebar's
    /// session rows + project headers render in — read from the process
    /// `SharedFontSettings` (the same source the tab strip's titles use). `None`
    /// before the keymap wires that global (the isolated `sidebar` scenario),
    /// which leaves gpui's default UI family. Only the FAMILY comes from here; the
    /// row text SIZE stays proportional off [`sidebar_pt`](Self::sidebar_pt)
    /// (plan `docs/plans/restyle/02-sidebar-flatten.md`).
    fn mono_family(&self, cx: &App) -> Option<SharedString> {
        resolved_mono_family(cx)
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
                            child_count: p
                                .tabs
                                .iter()
                                .filter(|c| c.parent_tab_id.as_deref() == Some(t.id.as_str()))
                                .count(),
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
        // Select the whole title on entry (a tab title is not a filename, so the
        // whole name — not base-minus-extension — is the replace target): the
        // first keystroke replaces it.
        self.rename_editor = Some(TextFieldEditor::with_selection(&title, title.chars().count()));
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

    /// Apply a click hit-test to the rename field — single click drops the caret,
    /// double selects the word, triple selects all ([`apply_rename_click`]) — then
    /// re-grab field focus (the click already stopped propagation, so the tab's
    /// title-tap gate never re-trips).
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

    // MARK: - Toggles / actions

    fn set_mode(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, _| {
            if ws.sidebar.mode() != mode {
                ws.sidebar.toggle_sidebar_mode();
                // Persist the new per-window sidebar mode so it restores — mirrors
                // the ⌘⇧B `ToggleSidebarMode` handler (`keymap.rs`). Without this,
                // a footer mode switch wouldn't persist until some later unrelated
                // tree-mutation save happened to fire (quit/crash before then and
                // the window restored in the old mode). A no-op when no store
                // Global is installed.
                ws.save_to_store();
            }
        });
    }

    /// Toggle the collapsed flag through the one
    /// [`WindowState::toggle_sidebar_collapsed`] seam (expanding also clears any
    /// peek). The seam notifies the state entity, which fires this view's state
    /// observer — and that observer drops the view-local hover pin on expand, so
    /// the cleanup is identical whether the toggle came from here, ⌘B, or the
    /// titlebar control.
    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |ws, wcx| ws.toggle_sidebar_collapsed(wcx));
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

    // MARK: - Row drag-reorder (`ProjectGroupDropDelegate` port, R25 pattern)

    /// A group container's `on_drag_move`: recompute the gated drop slot while a
    /// row drag is in flight. Fires for EVERY window mouse-move while a
    /// [`TabDragPayload`] drags — including over other groups and the pane body —
    /// so it FIRST guards containment (the port of Swift's `dropExited`,
    /// `SidebarView.swift:888-896`, and R25's D8): a cursor outside this group
    /// clears only a slot this group owns (another group's `on_drag_move` may
    /// already have overwritten it — Swift's `ownsCurrentIndicator`). When
    /// contained, the cursor's window y is resolved against this group's painted
    /// row frames — collapsed through [`drag_scope`] into subtree-aware units
    /// ([`tab_drop_target`], midpoint rule) — and gated through
    /// [`TabModel::would_move_tab`] — cross-project, illegal-lineage, and no-op
    /// slots resolve to `None`, so no line paints and a drop is a no-op (prod's
    /// `dropUpdated` proposing `.forbidden`).
    fn on_tab_drag_move(
        &mut self,
        group_id: &str,
        event: &DragMoveEvent<TabDragPayload>,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            if self
                .drag_target
                .as_ref()
                .is_some_and(|t| t.project_id == group_id)
            {
                self.drag_target = None;
                cx.notify();
            }
            return;
        }
        let dragged = event.drag(cx).tab_id.to_string();
        let y = f32::from(event.event.position.y);
        let frames = self.row_frames.borrow().clone();
        let new_target = {
            let ws = self.state.read(cx);
            let rows: Vec<(String, Option<String>)> = ws
                .model
                .projects
                .iter()
                .find(|p| p.id == group_id)
                .map(|p| {
                    p.tabs
                        .iter()
                        .map(|t| (t.id.clone(), t.parent_tab_id.clone()))
                        .collect()
                })
                .unwrap_or_default();
            // Subtree-aware scope: a root drag resolves against whole-block
            // units (the insertion line snaps to block boundaries); a child
            // drag against its own sibling run only.
            let (order, spans) = drag_scope(&rows, &dragged, &frames);
            tab_drop_target(y, &order, &spans)
                .filter(|(target, place_after)| {
                    ws.model.would_move_tab(&dragged, target, *place_after)
                })
                .map(|(target_tab_id, place_after)| {
                    // `tab_drop_target` only returns ids it found spans for.
                    let (min_y, max_y) = spans[&target_tab_id];
                    let edge = if place_after { max_y } else { min_y };
                    (target_tab_id, place_after, edge)
                })
        }
        .map(|(target_tab_id, place_after, edge)| SidebarDropTarget {
            project_id: group_id.to_string(),
            target_tab_id,
            place_after,
            line_y: edge - f32::from(event.bounds.origin.y),
        });
        if self.drag_target != new_target {
            self.drag_target = new_target;
            cx.notify();
        }
    }

    /// A group container's `on_drop`: commit the reorder to the slot the last
    /// `on_drag_move` resolved, guarded to the owning group. Calls
    /// [`TabModel::move_tab`] synchronously — gpui clears `active_drag` itself
    /// after this listener, so prod's next-runloop-tick deferral
    /// (`SidebarView.swift:897-915`) has no analog to port — then persists
    /// explicitly via `WindowState::save_to_store`. The once-per-window
    /// `on_tree_mutation` observer (BUGHUNT1-D) now also fires from `move_tab`, so
    /// this explicit save is belt-and-suspenders (a duplicate debounced upsert is
    /// harmless — kept per that plan's D2). A drop with no resolved slot just
    /// clears the field.
    fn on_tab_drop(&mut self, group_id: &str, payload: &TabDragPayload, cx: &mut Context<Self>) {
        if let Some(target) = self.drag_target.take() {
            if target.project_id == group_id {
                let dragged = payload.tab_id.to_string();
                self.state.update(cx, |ws, _| {
                    ws.model
                        .move_tab(&dragged, &target.target_tab_id, target.place_after);
                    ws.save_to_store();
                });
            }
        }
        cx.notify();
    }

    /// The reorder insertion line for one group: a 2pt accent bar inset 6pt
    /// horizontally to match the row background's rounded rect, centred on the
    /// target row's top or bottom edge (`SidebarView.swift:333-344` and the
    /// `indicatorY` offset at `:262-266`). Painted only while this group owns
    /// the resolved slot AND gpui still has an active drag — the
    /// `has_active_drag` conjunct drops the line the instant a dropped-nowhere
    /// mouse-up clears `active_drag` (R25 D10). Because `drag_target` is
    /// already `would_move_tab`-gated, no line shows for a no-op or
    /// cross-project slot. Pure paint: no id, no listeners.
    fn insertion_line(&self, group_id: &str, cx: &App) -> Option<gpui::AnyElement> {
        if !cx.has_active_drag() {
            return None;
        }
        let target = self.drag_target.as_ref()?;
        if target.project_id != group_id {
            return None;
        }
        let accent = srgba_to_rgba(crate::theme_settings::active_chrome_accent(cx));
        Some(
            div()
                .absolute()
                .left(px(6.0))
                .right(px(6.0))
                .top(px(target.line_y - 1.0))
                .h(px(2.0))
                .bg(accent)
                .into_any_element(),
        )
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

    // The sidebar top strip no longer drags the window: the 2026-07 restyle moved
    // the R9 band drag / double-click-zoom to the titlebar (`WindowToolbarView`).

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
        // The 2026-07 restyle restructures the expanded shell to
        // `column(titlebar, row(sidebar, content))`: the titlebar (the injected
        // `WindowToolbarView`) is a full-width row at the top, and the floating
        // sidebar card begins BELOW it (the traffic lights + drag region now live
        // in the titlebar, not over the sidebar's top strip). No fill-band divider
        // — the titlebar is fill-less (plan `docs/plans/restyle/01-titlebar-restyle.md`).
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(self.build_titlebar_row(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.build_sidebar_card(&groups, mode, cx))
                    .child(self.build_main_body(cx)),
            )
    }

    fn build_collapsed_shell(
        &self,
        groups: Vec<GroupVm>,
        mode: SidebarMode,
        peeking_model: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let peeking = peeking_model || self.peek_mouse_pinned;
        let peek = peeking.then(|| self.build_peek_overlay(&groups, peeking, mode, cx));
        // Collapsed: the full-width titlebar row over the full-width pane body —
        // no cap card, no restore button, no fill, no divider. The
        // sidebar-collapse toggle in the titlebar (a `WindowToolbarView` control)
        // restores the sidebar (plan `docs/plans/restyle/01-titlebar-restyle.md`).
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(self.build_titlebar_row(cx))
            .child(self.build_main_body(cx))
            .children(peek)
    }

    /// The full-width titlebar row atop both shell states: the injected
    /// `WindowToolbarView` (the shipped/composed shell), else a bare
    /// titlebar-height row in the isolated `sidebar` scenario (no toolbar wired).
    /// Fill-less — the window-body backing shows through (the restyle titlebar).
    fn build_titlebar_row(&self, cx: &App) -> gpui::AnyElement {
        let row = div().flex_none().w_full().h(px(TOP_BAR_HEIGHT));
        let _ = cx;
        if let Some(toolbar) = &self.main_toolbar {
            row.child(toolbar.clone()).into_any_element()
        } else {
            row.into_any_element()
        }
    }

    /// The flat docked sidebar column: the body (tab list or file browser) over
    /// the footer (mode switcher + gear), sharing the window-body / terminal
    /// surface — no card inset, fill, border, rounding, or shadow. A single 1px
    /// over-glass hairline sits at the trailing edge; because the column itself
    /// starts below the titlebar row, the hairline begins at the titlebar's
    /// bottom. The trailing resize handle straddles that edge (plan
    /// `docs/plans/restyle/02-sidebar-flatten.md`).
    fn build_sidebar_card(
        &self,
        groups: &[GroupVm],
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = active_slots(cx);
        let scheme = crate::theme_settings::active_chrome_scheme(cx);
        let handle = self.build_resize_handle(cx);
        div()
            // Exported shipped-surface AX anchor (§6): the sidebar column root,
            // found by an AX walk on role + label. gpui exposes an element to the
            // macOS AX tree only with both an `.id()` and a non-generic `.role()`;
            // the `aria_label` becomes its `AXTitle`.
            .id(SIDEBAR_ROOT_LABEL)
            .role(gpui::Role::Group)
            .aria_label(SIDEBAR_ROOT_LABEL)
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .w(px(self.sidebar_width))
            .h_full()
            // No fill: the flattened column shows the shared window-body backing
            // (the terminal-theme background app_shell paints), so the sidebar and
            // pane surfaces are one continuous surface.
            .child(self.build_body(groups, &s, false, mode, cx))
            .child(self.build_footer(&s, mode, cx))
            // The 1px over-glass hairline at the trailing edge, full column height.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .w(px(1.0))
                    .h_full()
                    .bg(glass_line_rgba(scheme)),
            )
            .child(handle)
    }

    /// The elevated peek-overlay panel (collapsed-sidebar hover peek): the same
    /// flat body + footer as the docked column, but on an OPAQUE theme-background
    /// fill with a rounded corner + drop shadow — it floats over live terminal
    /// content, so it keeps the panel treatment for readability (and the opaque
    /// fill avoids double-alpha stacking once plan 3 makes the window
    /// translucent). Fixed [`SIDEBAR_PEEK_WIDTH`], never resizable, no hairline.
    /// `peeking` forces the body to the tabs list — the same effective-peek
    /// predicate that gates the overlay's visibility, so the body and its presence
    /// never disagree (peek overlays always show tabs, even in files mode).
    fn build_peek_card(
        &self,
        groups: &[GroupVm],
        peeking: bool,
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = active_slots(cx);
        div()
            .id(SIDEBAR_ROOT_LABEL)
            .role(gpui::Role::Group)
            .aria_label(SIDEBAR_ROOT_LABEL)
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_PEEK_WIDTH))
            .h_full()
            .bg(slot_to_rgba(s.background))
            .rounded(px(CARD_CORNER_RADIUS))
            .shadow(card_shadow())
            .child(self.build_body(groups, &s, peeking, mode, cx))
            .child(self.build_footer(&s, mode, cx))
    }

    /// A footer mode-switcher icon button rendering one of the restyle's stroke
    /// SVGs (`crate::chrome_icons`). Active: `ink` tint + a faint over-glass
    /// [`glass_fill`] box. Inactive: `ink3` tint, no fill, an over-glass hover
    /// fill (plan `docs/plans/restyle/02-sidebar-flatten.md`).
    #[allow(clippy::too_many_arguments)]
    fn mode_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        icon_w: f32,
        icon_h: f32,
        active: bool,
        scheme: ColorScheme,
        s: &Slots,
        on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let ink = slot_to_rgba(s.ink);
        let ink3 = slot_to_rgba(s.ink3);
        let fill = glass_fill_rgba(scheme);
        div()
            .id(id)
            .group(id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded(px(INNER_CORNER_RADIUS))
            .when(active, |el| el.bg(fill))
            .when(!active, |el| el.hover(move |st| st.bg(fill)))
            .child(
                // gpui tints the SVG's alpha mask with the element's own text
                // colour; set it explicitly (active `ink`, else `ink3`).
                gpui::svg()
                    .path(icon_path)
                    .w(px(icon_w))
                    .h(px(icon_h))
                    .text_color(if active { ink } else { ink3 }),
            )
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
                .text_size(px(self.sidebar_pt(12.0)))
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
            // (`SidebarView.swift:142-163`, a `.onTapGesture` — so a CLICK, not a
            // mouse-down: rows consume their own clicks via `stop_propagation`,
            // and a row drag ending over empty space fires no click at all, so
            // this only ever sees the gaps / padding / unfilled bottom).
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.collapse_selection_to_active(cx);
                cx.notify();
            }))
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
        let family = self.mono_family(cx);
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
            .child({
                let (symbol, fallback) = disclosure_icon(g.is_open);
                // SF Symbol chevron in a fixed slot (see HEADER_DISCLOSURE_SLOT)
                // — 10pt semibold ink2 at 0.7 opacity, the same treatment as the
                // file-browser rows (`SidebarView.swift:289-295`).
                div()
                    .flex_none()
                    .w(px(self.sidebar_pt(HEADER_DISCLOSURE_SLOT)))
                    .flex()
                    .justify_center()
                    .items_center()
                    .opacity(0.7)
                    .child(sf_symbol_icon(
                        symbol,
                        fallback,
                        self.sidebar_pt(10.0),
                        SymbolWeight::Semibold,
                        ink2,
                        self.window_scale,
                        cx,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_disclosure(&gid_chevron, cx);
                            cx.stop_propagation();
                        }),
                    )
            })
            .child(
                div()
                    .flex_1()
                    .text_size(px(self.sidebar_pt(12.0)))
                    .line_height(px(self.sidebar_pt(LINE_HEIGHT_12)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink3)
                    .when_some(family.clone(), |el, fam| el.font_family(fam))
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
                    .text_size(px(self.sidebar_pt(10.0)))
                    .line_height(px(self.sidebar_pt(LINE_HEIGHT_10)))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ink3)
                    .child(SharedString::from(g.count.to_string())),
            );

        // The add button is ALWAYS laid out — prod hover-hides it with
        // `opacity(0)` + `allowsHitTesting(false)`, never removing it from the
        // HStack (`SidebarView.swift:314-316`) — so the 18pt box keeps the
        // header at a constant height (26pt: max(name 15, button 18) + 8)
        // instead of jumping on hover.
        {
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
            let mut btn = div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(18.0))
                .h(px(18.0))
                .rounded(px(4.0))
                .child(add_icon);
            if show_add {
                btn = btn
                    .hover(move |st| st.bg(add_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                            this.add_tab_in_group(&gid_add, is_terminals, cx);
                            cx.stop_propagation();
                        }),
                    );
            } else {
                // Reserved but invisible + inert (opacity 0, no handler) — the
                // R25 close-"×" slot pattern.
                btn = btn.opacity(0.0);
            }
            header = header.child(btn);
        }

        let rows: Vec<gpui::AnyElement> = g
            .tabs
            .iter()
            .map(|t| self.build_tab_row(t, s, cx))
            .collect();

        // The group container is the drop region (Swift attaches the
        // `ProjectGroupDropDelegate` to this same VStack, `SidebarView.swift:270`):
        // it spans the header, every row, and the 4pt trailing padding, so a row
        // can drop "above the first tab" (header area) or "below the last tab"
        // (trailing gap). `relative` hosts the absolute insertion line.
        let gid_move = gid.clone();
        let gid_drop = gid.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .pb(px(4.0))
            .on_drag_move(cx.listener(
                move |this, e: &DragMoveEvent<TabDragPayload>, _w, cx| {
                    this.on_tab_drag_move(&gid_move, e, cx);
                },
            ))
            .on_drop(cx.listener(move |this, payload: &TabDragPayload, _w, cx| {
                this.on_tab_drop(&gid_drop, payload, cx);
            }))
            .child(header)
            .children(rows)
            .children(self.insertion_line(&g.id, cx))
            .into_any_element()
    }

    fn build_tab_row(&self, t: &TabVm, s: &Slots, cx: &mut Context<Self>) -> gpui::AnyElement {
        let accent = crate::theme_settings::active_chrome_accent(cx);
        let scheme = crate::theme_settings::active_chrome_scheme(cx);
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let ink3 = slot_to_rgba(s.ink3);
        let family = self.mono_family(cx);
        // Hover / multi-select fill: the over-glass faint fill (replacing the old
        // accent `selection_tint`), on the flat shared surface.
        let glass = glass_fill_rgba(scheme);
        let indent = row_indent(t.indented);

        // Leading icon: the status dot for a Claude tab, else the `terminal`
        // symbol — 12pt regular ink3 in a 16pt box (`SidebarView.swift:602-607`).
        // Row dots render small (5pt) — only the size parameter changes; colours
        // + pulse are untouched.
        let leading = if t.has_claude {
            StatusDot::new(
                SharedString::from(t.id.clone()),
                t.status,
                slot_srgba(s.ink3),
            )
            .size(SIDEBAR_ROW_DOT_SIZE)
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
            let field = rename_field(
                &self.rename_focus,
                &spans,
                "SidebarRename",
                colors,
                self.sidebar_pt(13.0),
                self.rename_probe.clone(),
                cx.listener(Self::on_rename_key),
                move |index, click_count, window, app| {
                    let _ = weak.update(app, |this, cx| {
                        this.place_rename_cursor(index, click_count, window, cx)
                    });
                },
            );
            // Wrap rather than touch the shared `rename_field`: the line height
            // cascades into the field's text runs, keeping the editing row near
            // the label row's 28pt (the field's 1px borders still add 2px —
            // prod's `strokeBorder` overlay adds none).
            div()
                .flex_1()
                .flex()
                .line_height(px(self.sidebar_pt(LINE_HEIGHT_13)))
                .child(field)
                .into_any_element()
        } else {
            let tid = t.id.clone();
            let is_active = t.is_active;
            // The title tap is a CLICK (mouse-up with no drag), not a mouse-down
            // — Swift's `.onTapGesture` (`SidebarView.swift:795`). gpui's click
            // machinery never fires a click once a drag armed (pending press
            // state is cleared while `active_drag` is set), so pressing the
            // active row's title and dragging can no longer fall into rename.
            // No left mouse-down listener here: a child's `stop_propagation` on
            // mouse-down would kill the ROW's window-level click/drag arming
            // for presses on the title (most of the row's width).
            div()
                .id(SharedString::from(format!("sidebar.tab.{}.title", t.id)))
                .flex_1()
                .px(px(6.0))
                .py(px(2.0))
                .whitespace_nowrap()
                .truncate()
                .text_size(px(self.sidebar_pt(13.0)))
                .line_height(px(self.sidebar_pt(LINE_HEIGHT_13)))
                .font_weight(if is_active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                // Active row: accent text is its marker (no fill, no bar). Others
                // in normal `ink2`.
                .text_color(if is_active {
                    srgba_to_rgba(accent)
                } else {
                    ink2
                })
                .when_some(family.clone(), |el, fam| el.font_family(fam))
                .child(SharedString::from(t.title.clone()))
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    let mods = e.modifiers();
                    this.handle_title_tap(&tid, mods.platform, mods.shift, window, cx);
                    cx.notify();
                    // Consume so the row's own click listener doesn't also
                    // route a second (redundant) selection pass.
                    cx.stop_propagation();
                }))
                .into_any_element()
        };

        let tid_down = t.id.clone();
        let tid_click = t.id.clone();
        let tid_menu = t.id.clone();
        let is_active = t.is_active;
        let is_selected = t.is_selected;

        // Row-frame probe: an absolute inset-0 canvas recording this row's
        // painted vertical extent in window coords each paint — the Swift
        // `TabFramesKey` GeometryReader analog (`SidebarView.swift:249-257`).
        // The drop resolver reads these frames at drag time.
        let frames = self.row_frames.clone();
        let probe_id = t.id.clone();
        let frame_probe = gpui::canvas(
            |_, _, _| (),
            move |bounds: gpui::Bounds<Pixels>, _, _, _| {
                frames.borrow_mut().insert(
                    probe_id,
                    (
                        f32::from(bounds.origin.y),
                        f32::from(bounds.origin.y + bounds.size.height),
                    ),
                );
            },
        )
        .absolute()
        .inset_0();

        // The drag payload + ghost title, captured at build time (R25 D3/D4).
        let drag_payload = TabDragPayload {
            tab_id: SharedString::from(t.id.clone()),
        };
        let ghost_title = SharedString::from(t.title.clone());
        let ghost_child_count = t.child_count;

        // The inner row (colored rounded rect), inset 6pt from the card edges.
        //
        // Gesture wiring (Swift parity, `SidebarView.swift:588-657`):
        //   * selection routes on CLICK (`.onTapGesture` fires on mouse-up and
        //     never after a drag) — `on_click`, not `on_mouse_down`, so a
        //     drag-to-reorder press doesn't double as a tap;
        //   * the row carries `.id()` + `on_drag` ONLY — `on_drag_move` /
        //     `on_drop` live on the GROUP container (the R25 pill-vs-row
        //     listener split);
        //   * the left mouse-DOWN listener only mirrors prod's click-away
        //     monitor (`SidebarView.swift:484-498`): it commits another row's
        //     in-flight rename at press time. It deliberately does NOT
        //     `stop_propagation` — gpui's click/drag arming for this row is a
        //     window-level recorder that already ran, but the tab list's
        //     empty-area click machinery must also keep seeing the press.
        let inner = div()
            .id(SharedString::from(format!("sidebar.tab.{}", t.id)))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(indent))
            .pr(px(10.0))
            .py(px(4.0))
            .rounded(px(4.0))
            // Active row has NO fill (accent text is its marker). A multi-selected
            // non-active row keeps a persistent faint over-glass fill; a plain
            // non-active row shows the same fill only on hover.
            .when(!is_active && is_selected, |el| el.bg(glass))
            .when(!is_active && !is_selected, |el| {
                el.hover(move |st| st.bg(glass))
            })
            .child(frame_probe)
            .child(leading)
            .child(title)
            .on_drag(drag_payload, move |_payload, offset, _window, app| {
                let title = ghost_title.clone();
                app.new(|_| TabRowDragGhost {
                    title,
                    child_count: ghost_child_count,
                    offset,
                })
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e: &MouseDownEvent, window, cx| {
                    if this.editing_tab_id.as_deref() == Some(tid_down.as_str()) {
                        // A press on the editing row's own icon/padding keeps
                        // the edit alive (the field swallows its own presses)
                        // — Swift's `guard !isEditing` (`SidebarView.swift:522`).
                        cx.stop_propagation();
                        return;
                    }
                    if this.editing_tab_id.is_some() {
                        this.commit_rename(window, cx);
                    }
                }),
            )
            .on_click(cx.listener(move |this, e: &ClickEvent, _window, cx| {
                if this.editing_tab_id.as_deref() == Some(tid_click.as_str()) {
                    cx.stop_propagation();
                    return;
                }
                let mods = e.modifiers();
                this.route_click(&tid_click, mods.platform, mods.shift, cx);
                cx.notify();
                // Consume so the tab list's empty-area click (selection
                // collapse) doesn't also fire — rows absorb their own taps
                // (`SidebarView.swift:155-160`).
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    this.open_tab_context_menu(&tid_menu, e.position, window, cx);
                    cx.stop_propagation();
                }),
            );

        div().px(px(6.0)).w_full().child(inner).into_any_element()
    }

    /// The footer: the tabs/files mode switcher at the leading edge and the
    /// Settings gear at the trailing edge, sitting flush on the flat sidebar
    /// surface (no top rule — the 2026-07 restyle removed it). The mode buttons
    /// and gear render the restyle's stroke SVGs (`crate::chrome_icons`). The
    /// gear dispatches R23's [`OpenSettings`](crate::settings::window::OpenSettings)
    /// — the same action the "Settings…" app-menu item and ⌘, fire — so all three
    /// routes share the singleton open-or-focus handler.
    fn build_footer(&self, s: &Slots, mode: SidebarMode, cx: &mut Context<Self>) -> impl IntoElement {
        let scheme = crate::theme_settings::active_chrome_scheme(cx);
        let tabs_active = mode == SidebarMode::Tabs;
        let files_active = mode == SidebarMode::Files;
        let gear_ink3 = slot_to_rgba(s.ink3);
        let gear_ink = slot_to_rgba(s.ink);
        let gear_hover = glass_fill_rgba(scheme);
        div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .w_full()
            .px(px(8.0))
            .py(px(6.0))
            .child(
                // Leading: the tabs ↔ files mode switcher.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(self.mode_button(
                        "sidebar.mode.tabs",
                        crate::chrome_icons::MODE_TABS,
                        crate::chrome_icons::MODE_TABS_W,
                        crate::chrome_icons::MODE_TABS_H,
                        tabs_active,
                        scheme,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.set_mode(SidebarMode::Tabs, cx);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ))
                    .child(self.mode_button(
                        "sidebar.mode.files",
                        crate::chrome_icons::MODE_FILES,
                        crate::chrome_icons::MODE_FILES_W,
                        crate::chrome_icons::MODE_FILES_H,
                        files_active,
                        scheme,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.set_mode(SidebarMode::Files, cx);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )),
            )
            .child(
                // Trailing: the Settings gear — opens (or focuses) the singleton
                // Settings window through the shared `OpenSettings` action.
                // `.id()` + `Role::Button` expose it to the macOS AX tree (the
                // settings scenario's anchor idiom).
                div()
                    .id("sidebar.settings")
                    .group("sidebar.settings")
                    .role(gpui::Role::Button)
                    .aria_label("Settings")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(24.0))
                    .h(px(24.0))
                    .rounded(px(INNER_CORNER_RADIUS))
                    .hover(move |st| st.bg(gear_hover))
                    .child(
                        // The new thin-stroke gear SVG (not SF_GEAR / a font
                        // glyph); gpui tints the alpha mask with the element's
                        // text colour — `ink3` at rest, `ink` on hover.
                        gpui::svg()
                            .path(crate::chrome_icons::MODE_GEAR)
                            .w(px(crate::chrome_icons::MODE_GEAR_W))
                            .h(px(crate::chrome_icons::MODE_GEAR_H))
                            .text_color(gear_ink3)
                            .group_hover("sidebar.settings", move |st| st.text_color(gear_ink)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _e: &MouseDownEvent, window, cx| {
                            window.dispatch_action(
                                Box::new(crate::settings::window::OpenSettings),
                                cx,
                            );
                            cx.stop_propagation();
                        }),
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

    /// The content column beside (expanded) or beneath (collapsed) the sidebar.
    /// A plain background when no content is injected (the isolated `sidebar`
    /// scenario); the composed shell replaces it with the pane-content host via
    /// the [`main_body`](Self::main_body) slot.
    fn build_content(&self, cx: &App) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .size_full()
            .bg(slot_to_rgba(active_slots(cx).background))
    }

    /// The pane content fill below the titlebar row: the injected pane-host
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
            .child(self.build_peek_card(groups, peeking, mode, cx))
    }
}

// ---- Scenario accessors -----------------------------------------------------
//
// Read/drive surface the live `sidebar` self-test scenario (`crate::sidebar_live`)
// uses to ground-truth the shell against AppKit reads. All `pub(crate)` and
// side-effect-free except the collapse driver, which routes through
// [`SidebarShellView::toggle_collapsed`] into the shared
// [`WindowState::toggle_sidebar_collapsed`] seam — the SAME seam the shipped
// titlebar collapse control ([`crate::toolbar::WindowToolbarView`]) drives — so
// the scenario exercises the shipped collapse behavior (state flip + peek-clear
// on expand), not a bypass.
impl SidebarShellView {
    /// The current docked sidebar width (the resize target the scenario clamps).
    pub(crate) fn sidebar_width(&self) -> f32 {
        self.sidebar_width
    }

    /// Whether the sidebar is collapsed (drives the band-vs-column assertion).
    pub(crate) fn is_collapsed(&self, cx: &App) -> bool {
        self.state.read(cx).sidebar.collapsed()
    }

    /// Drive the shipped collapse behavior (used to enter / leave the collapsed
    /// full-width state in the scenario) via the shared
    /// [`WindowState::toggle_sidebar_collapsed`] seam the titlebar control also drives.
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
        // Drop the previous frame's row extents — the paint that follows this
        // render re-records every VISIBLE row, so closed tabs and collapsed
        // groups can't leave stale frames for the drop resolver to hit.
        self.row_frames.borrow_mut().clear();
        // R23 (D3): re-read the sidebar base size so a live Font-pane change repaints
        // the chrome at the new proportional sizes.
        self.sidebar_font_px = crate::settings::sidebar_font::current_sidebar_px(cx);
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
    // `ColorScheme` + the `glass_*` helpers come through the `super::*` glob (the
    // crate-top imports); only `AccentPreset` is test-local.
    use super::*;
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
    fn disclosure_icon_swaps_on_open() {
        assert_eq!(disclosure_icon(true), (SF_CHEVRON_OPEN, ICON_CHEVRON_OPEN));
        assert_eq!(
            disclosure_icon(false),
            (SF_CHEVRON_CLOSED, ICON_CHEVRON_CLOSED)
        );
        assert_ne!(disclosure_icon(true), disclosure_icon(false));
    }

    // ---- tab_drop_target (mirrors Tests/NiceUnitTests/SidebarDropResolverTests.swift)

    /// Three 28pt rows stacked from y=100 (a: 100-128, b: 128-156, c: 156-184).
    fn frames3() -> HashMap<String, (f32, f32)> {
        HashMap::from([
            ("a".to_string(), (100.0, 128.0)),
            ("b".to_string(), (128.0, 156.0)),
            ("c".to_string(), (156.0, 184.0)),
        ])
    }

    fn order3() -> Vec<String> {
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    }

    #[test]
    fn drop_over_first_tab_splits_on_midpoint() {
        let (f, o) = (frames3(), order3());
        assert_eq!(tab_drop_target(105.0, &o, &f), Some(("a".to_string(), false)));
        assert_eq!(tab_drop_target(125.0, &o, &f), Some(("a".to_string(), true)));
    }

    #[test]
    fn drop_above_first_tab_header_area_is_before_first() {
        assert_eq!(
            tab_drop_target(80.0, &order3(), &frames3()),
            Some(("a".to_string(), false))
        );
    }

    #[test]
    fn drop_below_last_tab_trailing_gap_is_after_last() {
        assert_eq!(
            tab_drop_target(200.0, &order3(), &frames3()),
            Some(("c".to_string(), true))
        );
    }

    #[test]
    fn drop_over_middle_tab_splits_on_midpoint() {
        let (f, o) = (frames3(), order3());
        assert_eq!(tab_drop_target(135.0, &o, &f), Some(("b".to_string(), false)));
        assert_eq!(tab_drop_target(150.0, &o, &f), Some(("b".to_string(), true)));
    }

    #[test]
    fn drop_in_empty_group_is_none() {
        assert_eq!(tab_drop_target(120.0, &[], &frames3()), None);
    }

    #[test]
    fn drop_in_collapsed_group_no_frames_is_none() {
        // A collapsed group paints no rows, so its ids carry no frames — every
        // branch misses (Swift's empty `tabFrames` snapshot).
        assert_eq!(tab_drop_target(120.0, &order3(), &HashMap::new()), None);
    }

    // ---- drag_scope (subtree-aware drop units, M7.8 round 3) ----------------

    /// The repro tree: `A [A1 A2] B [B1] C`, 28pt rows from y=100.
    fn lineage_rows() -> Vec<(String, Option<String>)> {
        [
            ("A", None),
            ("A1", Some("A")),
            ("A2", Some("A")),
            ("B", None),
            ("B1", Some("B")),
            ("C", None),
        ]
        .into_iter()
        .map(|(id, p)| (id.to_string(), p.map(str::to_string)))
        .collect()
    }

    fn lineage_frames() -> HashMap<String, (f32, f32)> {
        lineage_rows()
            .iter()
            .enumerate()
            .map(|(i, (id, _))| {
                let top = 100.0 + 28.0 * i as f32;
                (id.clone(), (top, top + 28.0))
            })
            .collect()
    }

    #[test]
    fn root_drag_scope_collapses_blocks_into_units() {
        let (order, spans) = drag_scope(&lineage_rows(), "C", &lineage_frames());
        assert_eq!(order, ["A", "B", "C"]);
        // A's unit spans its whole block: rows 0-2 → 100..184.
        assert_eq!(spans["A"], (100.0, 184.0));
        assert_eq!(spans["B"], (184.0, 240.0));
        assert_eq!(spans["C"], (240.0, 268.0));
    }

    #[test]
    fn root_drag_midpoint_is_block_level() {
        let (order, spans) = drag_scope(&lineage_rows(), "C", &lineage_frames());
        // y=150 is over A's SECOND row but above the block midpoint (142):
        // resolves after the whole A block, never between A and its children…
        assert_eq!(tab_drop_target(150.0, &order, &spans), Some(("A".into(), true)));
        // …and y=120 (top half of the block) is before it.
        assert_eq!(tab_drop_target(120.0, &order, &spans), Some(("A".into(), false)));
    }

    #[test]
    fn child_drag_scope_is_its_own_block_rows() {
        let frames = lineage_frames();
        let (order, spans) = drag_scope(&lineage_rows(), "A2", &frames);
        assert_eq!(order, ["A", "A1", "A2"]);
        // Row-granularity spans (untouched frames): a slot between siblings
        // stays resolvable.
        assert_eq!(spans["A1"], frames["A1"]);
        // Rows outside the block are not in the order, so a cursor over B's
        // row (y=190) falls into the below-last branch → after A2 (the last
        // sibling), which the model gates as a no-op when unchanged.
        assert_eq!(tab_drop_target(190.0, &order, &spans), Some(("A2".into(), true)));
    }

    #[test]
    fn selection_tint_dims_by_factor() {
        // `selection_tint` now survives only as the inline-rename field's
        // text-selection highlight (the flattened rows use the over-glass
        // `glass_fill` instead). The factor math is still exercised here.
        let accent = AccentPreset::Terracotta.color();
        let active = selection_tint(accent, 1.0);
        let dimmed = selection_tint(accent, 0.5);
        assert_eq!(active.a, SEL_ALPHA_DARK);
        assert_eq!(dimmed.a, SEL_ALPHA_DARK * 0.5);
        // Same hue, different alpha (rgb carried straight from the accent).
        assert_eq!((active.r, active.g, active.b), (dimmed.r, dimmed.g, dimmed.b));
    }

    #[test]
    fn glass_fill_and_line_convert_the_scheme_scoped_primitives() {
        // The flat sidebar's row / footer fills + trailing hairline resolve the
        // over-glass primitives (plan docs/plans/restyle/02-sidebar-flatten.md),
        // not palette slots. The gpui `Rgba` is a lossless field copy of the
        // nice-theme `Srgba`.
        for scheme in [ColorScheme::Dark, ColorScheme::Light] {
            let fill = glass_fill_rgba(scheme);
            let want_fill = glass_fill(scheme);
            assert_eq!(
                (fill.r, fill.g, fill.b, fill.a),
                (want_fill.r, want_fill.g, want_fill.b, want_fill.a)
            );
            let line = glass_line_rgba(scheme);
            let want_line = glass_line(scheme);
            assert_eq!(
                (line.r, line.g, line.b, line.a),
                (want_line.r, want_line.g, want_line.b, want_line.a)
            );
        }
    }
}
