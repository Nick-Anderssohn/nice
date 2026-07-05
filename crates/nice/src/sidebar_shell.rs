//! The R10 sessions-mode sidebar: the shell layout plus the sidebar card,
//! ported from `Sources/Nice/Views/AppShellView.swift` (the shell — layout
//! modes, floating card, resize handle, collapsed cap, peek overlay) and
//! `Sources/Nice/Views/SidebarView.swift` (the card content — project groups,
//! tab rows, footer, and the multi-select / rename / Esc behaviour). The pure
//! state it drives ships gpui-free in `nice-model` (slice 1): [`SidebarModel`],
//! [`SidebarTabSelection`], [`InlineRenameClickGate`].
//!
//! ## One entity owns the per-window state (the GPUI shape)
//!
//! Swift spreads this across `AppShellView`, `SidebarView`, `ProjectGroup`, and
//! `TabRow` `@State`. GPUI collapses the mutable state into one entity —
//! [`SidebarShellView`] — that owns the [`TabModel`] (R8), the sidebar
//! mode/collapse/peek [`SidebarModel`], the [`SidebarTabSelection`], the injected
//! [`SidebarActions`] seam, and the transient view state (resize width, peek pin,
//! disclosure-open set, inline-rename draft, the open context menu). The rows and
//! groups are built by helper methods rather than child entities so their tap
//! handlers can reach this state through `cx.listener` — no cross-element
//! interaction flags (the R9 anti-pattern), state is recomputed per event.
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
//! GPUI has no SF Symbol renderer and adding an SVG asset pipeline is out of
//! scope this cycle, so the header/footer glyphs use Unicode stand-ins (a later
//! cycle can swap in real vector assets). In particular the disclosure "chevron"
//! is a **glyph swap** (`▸` closed / `▾` open) rather than a rotation transform —
//! the pinned gpui exposes no element rotation, and the swap reads the same
//! 0°→90° affordance. These are cosmetic; the behaviour (disclosure toggles row
//! visibility, mode flips, collapse toggles) is what the itests pin.

// The view + its install fn have no in-crate caller until slice 4 wires the
// `sidebar` self-test scenario; it is a deliberately-exported surface (plan
// "Exported contracts"). The pure layout/label helpers below ARE exercised by
// this module's unit tests.
#![allow(dead_code)]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, App, BoxShadow, Context, CursorStyle, DismissEvent, Entity,
    FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, SharedString, Subscription, Window,
};

use nice_model::{
    InlineRenameClickGate, SidebarMode, SidebarModel, SidebarTabSelection, TabModel, TabStatus,
};
use nice_theme::chrome_geometry::{
    traffic_light_reserved_width, CARD_BORDER_OPACITY, CARD_BORDER_WIDTH, CARD_CORNER_RADIUS,
    CARD_INSET, CARD_SHADOW_OPACITY, CARD_SHADOW_RADIUS, CARD_SHADOW_Y_OFFSET, COLLAPSED_CAP_HEIGHT,
    COLLAPSED_CAP_TRAILING_WIDTH, INNER_CORNER_RADIUS, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH,
    SIDEBAR_MIN_WIDTH, SIDEBAR_PEEK_WIDTH, SIDEBAR_RESIZE_HANDLE_WIDTH, TOP_BAR_HEIGHT,
};
use nice_theme::color::Srgba;
use nice_theme::palette::{slots, ColorScheme, Palette, Slots};
use nice_theme::AccentPreset;

use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::status_dot::StatusDot;
use crate::theme::{slot_srgba, slot_to_rgba, srgba_to_rgba, srgba_with_alpha};

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

// ---- Icon glyphs (Unicode stand-ins — see module docs) ----------------------

const ICON_CHEVRON_CLOSED: &str = "\u{25B8}"; // ▸
const ICON_CHEVRON_OPEN: &str = "\u{25BE}"; // ▾
const ICON_TERMINAL: &str = "\u{276F}"; // ❯ (prompt glyph — the `terminal` symbol)
const ICON_PLUS: &str = "+";
const ICON_MODE_TABS: &str = "\u{2630}"; // ☰ (list.bullet)
const ICON_MODE_FILES: &str = "\u{25A4}"; // ▤ (folder)
const ICON_SIDEBAR: &str = "\u{25A8}"; // ▨ (sidebar.left — collapse & expand)
const ICON_GEAR: &str = "\u{2699}"; // ⚙ (gearshape)
const RENAME_CARET: &str = "\u{258F}"; // ▏

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

/// The width of the collapsed cap: the traffic-light reserve plus room for the
/// restore button and a trailing drag strip (`AppShellView.swift:953`).
fn collapsed_cap_width() -> f32 {
    traffic_light_reserved_width() + COLLAPSED_CAP_TRAILING_WIDTH
}

// ---- Colour helpers (Nice/Dark; the SidebarBackground palette seam) ----------

/// The R10 chrome slot table — Nice/Dark, matching the shipped chrome band
/// (`crate::app::nice_dark_slots`). Palette switching is R21.
fn dark_slots() -> Slots {
    slots(Palette::Nice, ColorScheme::Dark).expect("Nice + Dark is a valid palette/scheme combo")
}

/// The `SidebarBackground` palette-switch seam (`SidebarBackground.swift:21-46`).
/// R10 ships the flat `.nice` arm only: the sidebar column and the collapsed cap
/// paint a flat `niceBg2` panel. R21 slots the vibrancy materials (the `.macOS`
/// / Catppuccin arms) in here; keeping the switch behind one function is what
/// makes that a localized change.
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
/// [`SidebarShellView::new`] over a seeded [`TabModel`]; slice 4 opens it in the
/// `sidebar` self-test window.
pub(crate) struct SidebarShellView {
    /// The R8 document — the projects/tabs/panes tree the sidebar renders and
    /// the [`SidebarActions`] seam mutates.
    model: TabModel,
    /// Sidebar mode / collapse / peek state (slice-1 pure model).
    sidebar: SidebarModel,
    /// The Finder-style multi-selection (slice-1 pure model).
    selection: SidebarTabSelection,
    /// The create/close/select seam — R13 swaps this for real sessions.
    actions: Box<dyn crate::sidebar_actions::SidebarActions>,

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
    /// The in-flight rename draft.
    draft_title: String,
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

    /// Root focus handle (hosts the `SidebarShell` key context for Esc).
    focus_handle: FocusHandle,
    /// The user's accent — the thinking-dot colour + selection tint. Terracotta
    /// default (palette switching is R21).
    accent: Srgba,
}

impl SidebarShellView {
    /// A shell over a seeded model: expanded, tabs mode, selection seeded from
    /// the model's active tab so the invariant holds and the first ⇧-click has an
    /// anchor. Width 240, Terracotta accent.
    pub(crate) fn new(model: TabModel, cx: &mut Context<Self>) -> Self {
        let mut selection = SidebarTabSelection::new();
        selection.sync_active_tab_id(model.active_tab_id());
        Self {
            model,
            sidebar: SidebarModel::new(false, SidebarMode::Tabs),
            selection,
            actions: Box::new(crate::sidebar_actions::ModelSidebarActions::new()),
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            drag_start_width: None,
            resize_origin_x: None,
            peek_mouse_pinned: false,
            band_press: None,
            collapsed_projects: HashSet::new(),
            hovered_project: None,
            editing_tab_id: None,
            draft_title: String::new(),
            activated_at: Some(Instant::now()),
            rename_focus: cx.focus_handle(),
            rename_blur_sub: None,
            context_menu: None,
            menu_sub: None,
            focus_handle: cx.focus_handle(),
            accent: AccentPreset::Terracotta.color(),
        }
    }

    // MARK: - Snapshot

    fn snapshot_groups(&self) -> Vec<GroupVm> {
        let active = self.model.active_tab_id().map(|s| s.to_string());
        self.model
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
                            is_selected: self.selection.contains(&t.id),
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
    fn route_click(&mut self, tab_id: &str, cmd: bool, shift: bool) {
        let before = self.model.active_tab_id().map(|s| s.to_string());
        if cmd {
            if let Some(new_active) = self.selection.toggle(tab_id) {
                self.actions.select_tab(&mut self.model, &new_active);
            }
        } else if shift {
            let order = self.model.navigable_sidebar_tab_ids();
            self.selection.extend(tab_id, &order);
            self.actions.select_tab(&mut self.model, tab_id);
        } else {
            self.selection.replace(tab_id);
            self.actions.select_tab(&mut self.model, tab_id);
        }
        let after = self.model.active_tab_id().map(|s| s.to_string());
        if before != after {
            self.activated_at = Some(Instant::now());
        }
        // Reconcile the selection's active mirror with the model (a no-op on the
        // tap paths since the mutators already set it; keeps the invariant if a
        // toggle refused).
        let active = self.model.active_tab_id().map(|s| s.to_string());
        self.selection.sync_active_tab_id(active.as_deref());
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
            self.route_click(tab_id, cmd, shift);
            return;
        }
        let is_active = self.model.active_tab_id() == Some(tab_id);
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
            self.route_click(tab_id, false, false);
        }
    }

    /// Collapse a multi-selection back to the active tab (Esc / empty-area
    /// click). Drops everything only when the tree has no active tab — a
    /// mid-shutdown edge (`SidebarView.swift:86-92`).
    fn collapse_selection_to_active(&mut self) {
        if let Some(active) = self.model.active_tab_id().map(|s| s.to_string()) {
            self.selection.collapse(&active);
        } else {
            self.selection.clear();
        }
    }

    /// Prune + re-sync the selection against the surviving tabs after a close.
    fn reconcile_selection_after_close(&mut self) {
        let valid: HashSet<String> = self.model.navigable_sidebar_tab_ids().into_iter().collect();
        let active = self.model.active_tab_id().map(|s| s.to_string());
        self.selection.prune(&valid);
        self.selection.sync_active_tab_id(active.as_deref());
    }

    /// Re-seed the selection + arm the rename gate after a create/select (the new
    /// tab is already the model's active tab).
    fn reseed_selection_after_create(&mut self) {
        let active = self.model.active_tab_id().map(|s| s.to_string());
        self.selection.sync_active_tab_id(active.as_deref());
        self.activated_at = Some(Instant::now());
    }

    // MARK: - Inline rename

    fn begin_editing(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.model.tab_for(tab_id) else {
            return;
        };
        self.editing_tab_id = Some(tab_id.to_string());
        self.draft_title = tab.title.clone();
        self.rename_focus.focus(window, cx);
        // Commit on focus loss (the DO-NOT-PORT click-away monitor replacement).
        // Replacing any prior subscription here drops it OUTSIDE its callback.
        self.rename_blur_sub = Some(cx.on_blur(&self.rename_focus, window, |this, _w, cx| {
            this.commit_rename(cx);
        }));
        cx.notify();
    }

    /// Commit the draft (empty input is a model no-op — asymmetry 3). Idempotent:
    /// a stray focus-out after the edit already ended does nothing.
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editing_tab_id.take() else {
            return;
        };
        let draft = std::mem::take(&mut self.draft_title);
        self.model.rename_tab(&id, &draft);
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.editing_tab_id.take().is_none() {
            return;
        }
        self.draft_title.clear();
        cx.notify();
    }

    fn on_rename_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "enter" => {
                self.commit_rename(cx);
                cx.stop_propagation();
            }
            "backspace" => {
                self.draft_title.pop();
                cx.notify();
                cx.stop_propagation();
            }
            // Escape is consumed by the shell Esc action (which cancels rename)
            // before this bubble-phase listener runs.
            _ => {
                if !ks.modifiers.platform && !ks.modifiers.control {
                    if let Some(ch) = &ks.key_char {
                        self.draft_title.push_str(ch);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }
            }
        }
    }

    // MARK: - Toggles / actions

    fn set_mode(&mut self, mode: SidebarMode) {
        if self.sidebar.mode() != mode {
            self.sidebar.toggle_sidebar_mode();
        }
    }

    /// Toggle the collapsed flag. Expanding also clears any peek state
    /// (`AppShellView`: expand clears peek).
    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.sidebar.toggle_sidebar();
        if !self.sidebar.collapsed() {
            self.sidebar.end_sidebar_peek();
            self.peek_mouse_pinned = false;
        }
        cx.notify();
    }

    fn add_tab_in_group(&mut self, group_id: &str, is_terminals: bool, cx: &mut Context<Self>) {
        if is_terminals {
            self.actions.create_terminal_tab(&mut self.model);
        } else {
            self.actions
                .create_claude_tab_in_project(&mut self.model, group_id);
        }
        self.reseed_selection_after_create();
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_tab_id.is_some() {
            self.cancel_rename(cx);
            return; // consumed
        }
        if self.selection.selected_tab_ids().len() > 1 {
            self.collapse_selection_to_active();
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
        let action_ids = self.selection.selection_ids_for_right_click_on(tab_id);
        let mut items = Vec::new();
        let weak = cx.weak_entity();

        // Rename appears only for a single-row selection (`SidebarView.swift:636`).
        if action_ids.len() == 1 {
            let tid = tab_id.to_string();
            let w = weak.clone();
            items.push(ContextMenuItem::entry("Rename Tab", move |window, app| {
                let _ = w.update(app, |this, cx| {
                    this.selection.snap_if_right_click_outside(&tid);
                    this.actions.select_tab(&mut this.model, &tid);
                    this.reseed_selection_after_create();
                    this.begin_editing(&tid, window, cx);
                });
            }));
        }

        let close_label = close_menu_label(action_ids.len());
        let ids = action_ids.clone();
        let tid = tab_id.to_string();
        let w = weak.clone();
        items.push(ContextMenuItem::entry(close_label, move |_window, app| {
            let _ = w.update(app, |this, cx| {
                this.selection.snap_if_right_click_outside(&tid);
                if ids.len() > 1 {
                    this.actions.close_tabs(&mut this.model, &ids);
                } else {
                    this.actions.close_tab(&mut this.model, &tid);
                }
                this.reconcile_selection_after_close();
                cx.notify();
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
        let items = vec![ContextMenuItem::entry("Close Project", move |_window, app| {
            let _ = weak.update(app, |this, cx| {
                this.actions.close_project(&mut this.model, &gid);
                this.reconcile_selection_after_close();
                cx.notify();
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
        self.menu_sub = Some(cx.subscribe(&menu, |this, _menu, _ev: &DismissEvent, cx| {
            this.context_menu = None;
            cx.notify();
        }));
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

    fn build_expanded_shell(&self, groups: Vec<GroupVm>, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .size_full()
            .child(self.build_sidebar_card(&groups, true, false, cx))
            .child(self.build_content())
    }

    fn build_collapsed_shell(
        &self,
        groups: Vec<GroupVm>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = dark_slots();
        let peeking = self.sidebar.peeking() || self.peek_mouse_pinned;
        let peek = peeking.then(|| self.build_peek_overlay(&groups, peeking, cx));
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(
                // Top row: the collapsed cap (traffic lights + restore) plus the
                // chrome band area (R9/R11 own it); content extends full-width
                // beneath.
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .h(px(TOP_BAR_HEIGHT))
                    .child(self.build_collapsed_cap(&s, cx))
                    .child(div().flex_1().h_full().bg(slot_to_rgba(s.chrome))),
            )
            .child(self.build_content())
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = dark_slots();
        let card_width = if resizable {
            self.sidebar_width
        } else {
            SIDEBAR_PEEK_WIDTH
        };
        let handle = resizable.then(|| self.build_resize_handle(cx));
        let inner = div()
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
            .child(self.build_top_strip(&s, cx))
            .child(self.build_body(groups, &s, peeking, cx))
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
    fn build_top_strip(&self, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = self.accent;
        let tabs_active = self.sidebar.mode() == SidebarMode::Tabs;
        let files_active = self.sidebar.mode() == SidebarMode::Files;
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
                        ICON_MODE_TABS,
                        tabs_active,
                        accent,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.set_mode(SidebarMode::Tabs);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ))
                    .child(self.mode_button(
                        "sidebar.mode.files",
                        ICON_MODE_FILES,
                        files_active,
                        accent,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.set_mode(SidebarMode::Files);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ))
                    .child(self.icon_button(
                        ICON_SIDEBAR,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_collapsed(cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
    }

    /// A 24pt header icon button (mode toggle): accent-tinted when active, hover
    /// fill otherwise (`AppShellView.swift:1097-1134`).
    fn mode_button(
        &self,
        _id: &str,
        glyph: &'static str,
        active: bool,
        accent: Srgba,
        s: &Slots,
        on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let hover = ink_alpha(s, ICON_BUTTON_HOVER_ALPHA);
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded(px(INNER_CORNER_RADIUS))
            .text_size(px(13.0))
            .font_weight(if active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .text_color(if active { ink } else { ink2 })
            .when(active, |el| el.bg(selection_tint(accent, 1.0)))
            .when(!active, |el| el.hover(move |st| st.bg(hover)))
            .child(SharedString::from(glyph))
            .on_mouse_down(MouseButton::Left, on_down)
    }

    /// A plain 24pt icon button (collapse / footer): hover fill only
    /// (`SidebarView.swift:1018-1044`).
    fn icon_button(
        &self,
        glyph: &'static str,
        s: &Slots,
        on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let hover = ink_alpha(s, ICON_BUTTON_HOVER_ALPHA);
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded(px(INNER_CORNER_RADIUS))
            .text_size(px(14.0))
            .text_color(slot_to_rgba(s.ink2))
            .hover(move |st| st.bg(hover))
            .child(SharedString::from(glyph))
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
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let show_tabs = peeking || self.sidebar.mode() == SidebarMode::Tabs;
        if show_tabs {
            self.build_tab_list(groups, s, cx).into_any_element()
        } else {
            // Files mode placeholder (R19 swaps the real browser in).
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
                    this.collapse_selection_to_active();
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
            header = header.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .text_size(px(10.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink2)
                    .hover(move |st| st.bg(add_hover))
                    .child(SharedString::from(ICON_PLUS))
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
        let accent = self.accent;
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);
        let ink3 = slot_to_rgba(s.ink3);
        let hover = ink_alpha(s, HOVER_INK_ALPHA);
        let indent = row_indent(t.indented);

        // Leading icon: the status dot for a Claude tab, else the terminal glyph.
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
                .text_size(px(12.0))
                .text_color(ink3)
                .child(SharedString::from(ICON_TERMINAL))
                .into_any_element()
        };

        // Title view: the inline-rename field while editing, else the label.
        let title: gpui::AnyElement = if t.is_editing {
            div()
                .track_focus(&self.rename_focus)
                .key_context("SidebarRename")
                .flex_1()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(INNER_CORNER_RADIUS))
                .bg(slot_to_rgba(s.background3))
                .border(px(1.0))
                .border_color(slot_to_rgba(s.line_strong))
                .text_size(px(13.0))
                .text_color(ink)
                .child(SharedString::from(format!(
                    "{}{}",
                    self.draft_title, RENAME_CARET
                )))
                .on_key_down(cx.listener(Self::on_rename_key))
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
                            this.commit_rename(cx);
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
                cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                    if this.editing_tab_id.as_deref() == Some(tid_tap.as_str()) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.editing_tab_id.is_some() {
                        this.commit_rename(cx);
                    }
                    this.route_click(&tid_tap, e.modifiers.platform, e.modifiers.shift);
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
    fn build_footer(&self, s: &Slots, _cx: &mut Context<Self>) -> impl IntoElement {
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
                            .text_size(px(14.0))
                            .text_color(slot_to_rgba(s.ink3))
                            .child(SharedString::from(ICON_GEAR)),
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

    /// The collapsed cap: a small card in the top-bar's upper-left hosting the
    /// three native traffic lights (in the leading reserve) plus the restore
    /// button, centred on the y-26 row (`AppShellView.swift:934-972`).
    fn build_collapsed_cap(&self, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .pl(px(CARD_INSET))
            .py(px(CARD_INSET))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w(px(collapsed_cap_width()))
                    .h(px(COLLAPSED_CAP_HEIGHT))
                    .rounded(px(CARD_CORNER_RADIUS))
                    .bg(sidebar_background(s))
                    .border(px(CARD_BORDER_WIDTH))
                    .border_color(card_border_color(s))
                    .shadow(card_shadow())
                    // Leading reserve for the native traffic lights (they render
                    // over this; the restore button sits just past them).
                    .child(div().w(px(traffic_light_reserved_width())).h_full())
                    .child(self.icon_button(
                        ICON_SIDEBAR,
                        s,
                        cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                            this.toggle_collapsed(cx);
                            cx.stop_propagation();
                        }),
                    ))
                    // Trailing drag strip.
                    .child(div().flex_1().h_full()),
            )
    }

    /// The content column to the right of (expanded) or beneath (collapsed) the
    /// sidebar. A plain background this cycle — R11's toolbar + R13's terminal
    /// fill it later.
    fn build_content(&self) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .size_full()
            .bg(slot_to_rgba(dark_slots().background))
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
            .child(self.build_sidebar_card(groups, false, peeking, cx))
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

    /// Whether the sidebar is collapsed (drives the cap-vs-column assertion).
    pub(crate) fn is_collapsed(&self) -> bool {
        self.sidebar.collapsed()
    }

    /// Drive the real collapse toggle (used to bring up / dismiss the collapsed
    /// cap in the scenario).
    pub(crate) fn drive_toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.toggle_collapsed(cx);
    }

    /// The `(status, waiting_acknowledged)` pair the row would feed its
    /// [`StatusDot`] for `tab_id` — the R8 predicates read straight off the model,
    /// never recomputed. `None` if the tab is unknown. The scenario asserts the
    /// dot colour + pulse rule against these.
    pub(crate) fn tab_dot_inputs(&self, tab_id: &str) -> Option<(TabStatus, bool)> {
        self.model
            .tab_for(tab_id)
            .map(|t| (t.status(), t.waiting_acknowledged()))
    }

    /// The user's accent (thinking-dot colour) — resolved once at construction.
    pub(crate) fn accent(&self) -> Srgba {
        self.accent
    }
}

impl Focusable for SidebarShellView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SidebarShellView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = self.snapshot_groups();
        let shell = if self.sidebar.collapsed() {
            self.build_collapsed_shell(groups, cx).into_any_element()
        } else {
            self.build_expanded_shell(groups, cx).into_any_element()
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
    fn collapsed_cap_width_is_reserve_plus_trailing() {
        // 82 (lights reserve) + 42 (restore + drag strip) = 124
        // (AppShellView.swift:953).
        assert_eq!(collapsed_cap_width(), traffic_light_reserved_width() + 42.0);
        assert_eq!(collapsed_cap_width(), 124.0);
    }

    #[test]
    fn sidebar_background_is_flat_nice_bg2() {
        // The SidebarBackground palette seam ships the flat niceBg2 panel this
        // cycle (SidebarBackground.swift:26-27).
        let s = dark_slots();
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
