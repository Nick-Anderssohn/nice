//! In-process sidebar multi-select / rename-gate / disclosure classification
//! differentials for the R10 sessions-mode sidebar — **execution model: mocked
//! [`gpui::TestAppContext`], ordinary libtest `#[gpui::test]` cases** (no Metal,
//! no pixels; parallel-safe).
//!
//! The shipped view (`SidebarShellView` in the `nice` binary) cannot be imported
//! here — `nice-itests` is dev/test-only and the app binary never depends on it
//! (and vice versa), exactly the constraint the R9 [`crate::chrome_band`] probe
//! documents. So this mirrors the view's tap-routing handlers in a local
//! [`SidebarProbe`] that drives the **real** `nice-model` selection / rename-gate
//! types ([`SidebarTabSelection`], [`InlineRenameClickGate`], [`TabModel`]) and
//! **records the classification outcome** of every simulated event — which model
//! mutation the press produced, and whether the press was consumed by the row or
//! leaked to the empty-area / band / background handlers behind it. The mirrored
//! glue is thin (the three `route_click` branches, the Esc action, the right-click
//! menu id resolution); the real model does the selection reasoning, so a drift in
//! the ported selection semantics fails here.
//!
//! ## What these cases verify (and what they deliberately do NOT)
//!
//! Per the plan's differential-pair rule, each assertion is a **classification
//! outcome**, never a frame-motion claim: an in-process simulated event cannot
//! move a real NSWindow frame, so "the top-strip drag moved the window" is
//! vacuous here and lives only in the live `sidebar` scenario. What is asserted
//! is the pair — *a press on a tab row is consumed by the row (`stop_propagation`)
//! so the band's window-drag arm never fires and the empty-area collapse never
//! runs; a press on the empty area DOES reach the collapse handler; a press on the
//! band DOES arm a window drag.* The band handlers therefore record what the band
//! **would** do (arm / promote a drag) instead of calling the real
//! `Window::start_window_move` / `titlebar_double_click`, which need a real
//! NSWindow the mocked context does not have — the same technique `chrome_band`
//! uses. Neither this nor any behavior test asserts cadence / perf / wall-clock
//! timing (those live only in the live suite); the one clock these cases read is
//! gpui's **simulated** clock via `advance_clock`, used to gate rename timing
//! deterministically, never to make a latency claim.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, IntoElement,
    KeyBinding, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, TestAppContext, VisualTestContext, Window,
};

use nice_model::{InlineRenameClickGate, Pane, PaneKind, SidebarTabSelection, Tab, TabModel};

// The Esc key binding is a gpui action — the same shape as the shipped
// `CollapseSidebarSelection` (the DO-NOT-PORT replacement for the `NSEvent` Esc
// monitor). A local action name so it can't collide with the app's.
gpui::actions!(sidebar_itests, [CollapseSelectionProbe]);

/// The `SidebarShell`-shaped key context the Esc binding lives in, so the action
/// reaches the focused probe (mirrors the shipped `key_context("SidebarShell")`).
const PROBE_KEY_CONTEXT: &str = "SidebarShellProbe";

// ---- Geometry (deterministic absolute hitboxes) -----------------------------
//
// A flat mini-sidebar: a top strip (the R9 band pattern), a tab list carrying the
// empty-area collapse handler, and one absolute row per *visible* tab (a row bg +
// a title sub-hitbox), over a full-window background catcher. Absolute positions
// make every simulated click land deterministically, like `chrome_band`.

const CARD_W: f32 = 240.0;
/// The 52pt top strip (`nice_theme::chrome_geometry::TOP_BAR_HEIGHT`), mirrored
/// as a literal so this dev-test crate needn't gain a theme dep just for it.
const BAND_H: f32 = 52.0;
const LIST_TOP: f32 = BAND_H;
const ROW_H: f32 = 30.0;
/// Leading region of a row (dot / indent) — a left-press here hits the row bg,
/// not the title sub-hitbox that begins at [`TITLE_X`].
const TITLE_X: f32 = 60.0;
/// Rename-gate interval — parity with the shipped `DOUBLE_CLICK_INTERVAL`
/// (`sidebar_shell.rs:96`, the macOS `NSEvent.doubleClickInterval` default).
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// Window-drag start threshold squared — parity with the shipped strip / R9 band
/// (`BAND_DRAG_THRESHOLD_PX == 2.0`, compared `dx*dx + dy*dy >= 4`).
const DRAG_THRESHOLD_SQ: f32 = 4.0;

/// Content-view centre of visible-row `i`'s **background** (left of the title).
fn row_bg_point(i: usize) -> Point<Pixels> {
    point(px(20.0), px(LIST_TOP + i as f32 * ROW_H + ROW_H / 2.0))
}

/// Content-view centre of visible-row `i`'s **title** sub-hitbox.
fn row_title_point(i: usize) -> Point<Pixels> {
    point(px(TITLE_X + 40.0), px(LIST_TOP + i as f32 * ROW_H + ROW_H / 2.0))
}

/// A point on the empty band (clear of any row; on the y-26 strip row).
fn band_point() -> Point<Pixels> {
    point(px(CARD_W / 2.0), px(BAND_H / 2.0))
}

/// A point in the tab list below every row (the empty gap the collapse handler
/// owns), for a list holding `n_rows` visible rows.
fn empty_area_point(n_rows: usize) -> Point<Pixels> {
    point(px(CARD_W / 2.0), px(LIST_TOP + n_rows as f32 * ROW_H + 40.0))
}

/// The context-menu close label for `count` tabs — parity with the shipped
/// `close_menu_label` (`sidebar_shell.rs:155`, `SidebarView.swift:644`).
fn close_menu_label(count: usize) -> String {
    if count > 1 {
        format!("Close {count} Tabs")
    } else {
        "Close Tab".to_string()
    }
}

/// What a right-click on a row would put in its context menu — recorded so a test
/// can assert the id set the menu acts on, the close label, and whether "Rename"
/// is offered (single-selection only), all without minting a real popup entity.
#[derive(Default, Clone)]
struct MenuDescriptor {
    /// The ids the menu's actions would act on (`selection_ids_for_right_click_on`).
    action_ids: Vec<String>,
    /// The close item's label (`close_menu_label`).
    close_label: String,
    /// Whether "Rename Tab" is offered — single-row selection only.
    has_rename: bool,
}

/// The probe: a flat mini-sidebar mirroring `SidebarShellView`'s tap routing over
/// the real `nice-model` state, recording every classification outcome.
struct SidebarProbe {
    // --- real model state (the routing acts on these) ---
    model: TabModel,
    selection: SidebarTabSelection,
    collapsed_projects: HashSet<String>,
    editing_tab_id: Option<String>,
    draft_title: String,
    /// When the active tab became active — the rename-gate reference, read from
    /// the **simulated** clock so `advance_clock` moves it deterministically.
    activated_at: Option<Instant>,

    // --- band drag arm (the ONLY remembered press state, like the shipped
    //     `band_press`; never a cross-element interaction flag) ---
    band_press: Option<Point<Pixels>>,

    // --- classification counters ---
    /// Presses that reached the empty-area collapse handler.
    empty_area_collapses: u32,
    /// Presses that armed the band's window-drag (reached the strip).
    band_presses: u32,
    /// Band drags promoted past the ~2pt threshold (where the band would call
    /// `start_window_move`).
    band_window_moves: u32,
    /// Presses that leaked all the way to the background catcher (not consumed by
    /// a row / the band / the list).
    escaped_to_background: u32,
    /// Escape keystrokes that reached the "terminal" key-down listener — i.e. the
    /// Esc action `cx.propagate()`d instead of consuming.
    esc_reached_terminal: u32,

    // --- last recorded context menu (right-click) ---
    last_menu: Option<MenuDescriptor>,

    focus_handle: FocusHandle,
}

impl SidebarProbe {
    fn new(model: TabModel, cx: &mut Context<Self>) -> Self {
        let mut selection = SidebarTabSelection::new();
        selection.sync_active_tab_id(model.active_tab_id());
        // Seed the rename-gate clock so a later same-instant tap on the seeded
        // active row is (correctly) inside the gate.
        let activated_at = Some(cx.background_executor().now());
        Self {
            model,
            selection,
            collapsed_projects: HashSet::new(),
            editing_tab_id: None,
            draft_title: String::new(),
            activated_at,
            band_press: None,
            empty_area_collapses: 0,
            band_presses: 0,
            band_window_moves: 0,
            escaped_to_background: 0,
            esc_reached_terminal: 0,
            last_menu: None,
            focus_handle: cx.focus_handle(),
        }
    }

    // ---- snapshot ----------------------------------------------------------

    /// The tab ids that render as rows — navigable order minus any tab whose
    /// project's disclosure is collapsed (mirrors `snapshot_groups`: a collapsed
    /// project contributes no rows).
    fn visible_tab_ids(&self) -> Vec<String> {
        self.model
            .projects
            .iter()
            .flat_map(|p| {
                if self.collapsed_projects.contains(&p.id) {
                    Vec::new()
                } else {
                    p.tabs.iter().map(|t| t.id.clone()).collect()
                }
            })
            .collect()
    }

    /// The count pill for a project — always `tabs.len()`, disclosure-independent.
    fn project_tab_count(&self, project_id: &str) -> Option<usize> {
        self.model
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.tabs.len())
    }

    // ---- routing (mirror of SidebarShellView) ------------------------------

    /// Mirror of `SidebarShellView::route_click` (`sidebar_shell.rs:392`): plain
    /// replaces + activates; ⌘ toggles (only-and-active refused → no reselect);
    /// ⇧ extends from the sticky anchor. Resets `activated_at` only when the
    /// active tab actually changes. The shipped view routes the active-tab write
    /// through `ModelSidebarActions::select_tab`, a `TabModel::select_tab`
    /// passthrough in R10, so the probe calls the model directly.
    fn route_click(&mut self, tab_id: &str, cmd: bool, shift: bool, cx: &mut Context<Self>) {
        let before = self.model.active_tab_id().map(str::to_string);
        if cmd {
            if let Some(new_active) = self.selection.toggle(tab_id) {
                self.model.select_tab(&new_active);
            }
        } else if shift {
            let order = self.model.navigable_sidebar_tab_ids();
            self.selection.extend(tab_id, &order);
            self.model.select_tab(tab_id);
        } else {
            self.selection.replace(tab_id);
            self.model.select_tab(tab_id);
        }
        let after = self.model.active_tab_id().map(str::to_string);
        if before != after {
            self.activated_at = Some(cx.background_executor().now());
        }
        let active = self.model.active_tab_id().map(str::to_string);
        self.selection.sync_active_tab_id(active.as_deref());
    }

    /// Mirror of the shipped `NextSidebarTab` keymap handler (`keymap.rs`, the R10
    /// keyboard tab-cycle ⌘⌥↓): advance the model's active tab, then re-sync the
    /// selection's active mirror — `WindowState::sync_selection_to_active_tab`, the
    /// fix. The handler previously mutated only the model, leaving the prior-active
    /// row a stale selection-set member (the faint-highlight residue). Mouse paths
    /// sync inline in [`route_click`](Self::route_click); this is the keyboard analog.
    fn route_next_sidebar_tab(&mut self) {
        self.model.select_next_sidebar_tab();
        let active = self.model.active_tab_id().map(str::to_string);
        self.selection.sync_active_tab_id(active.as_deref());
    }

    /// Mirror of `SidebarShellView::handle_title_tap` (`sidebar_shell.rs:420`): a
    /// plain tap on the already-active row enters rename only past the gate;
    /// otherwise it routes like a plain select. Reads the simulated clock so a
    /// test drives the gate with `advance_clock`.
    fn handle_title_tap(&mut self, tab_id: &str, cmd: bool, shift: bool, cx: &mut Context<Self>) {
        if cmd || shift {
            self.route_click(tab_id, cmd, shift, cx);
            return;
        }
        let is_active = self.model.active_tab_id() == Some(tab_id);
        if is_active {
            let now = cx.background_executor().now();
            if InlineRenameClickGate::can_begin_edit(self.activated_at, now, DOUBLE_CLICK_INTERVAL) {
                self.begin_editing(tab_id);
            }
            // else: the same-click-as-select window — no-op.
        } else {
            self.route_click(tab_id, false, false, cx);
        }
    }

    fn begin_editing(&mut self, tab_id: &str) {
        let Some(tab) = self.model.tab_for(tab_id) else {
            return;
        };
        self.editing_tab_id = Some(tab_id.to_string());
        self.draft_title = tab.title.clone();
    }

    /// Mirror of `SidebarShellView::collapse_selection_to_active`
    /// (`sidebar_shell.rs:450`): collapse the multi-selection to the active tab
    /// (or clear if the tree has no active tab).
    fn collapse_selection_to_active(&mut self) {
        if let Some(active) = self.model.active_tab_id().map(str::to_string) {
            self.selection.collapse(&active);
        } else {
            self.selection.clear();
        }
    }

    // ---- event handlers (recorded classification) --------------------------

    fn on_row_bg_down(&mut self, tab_id: &str, e: &MouseDownEvent, cx: &mut Context<Self>) {
        self.route_click(tab_id, e.modifiers.platform, e.modifiers.shift, cx);
        cx.notify();
        cx.stop_propagation();
    }

    fn on_title_down(&mut self, tab_id: &str, e: &MouseDownEvent, cx: &mut Context<Self>) {
        self.handle_title_tap(tab_id, e.modifiers.platform, e.modifiers.shift, cx);
        cx.notify();
        cx.stop_propagation();
    }

    /// Mirror of `SidebarShellView::open_tab_context_menu`'s id resolution
    /// (`sidebar_shell.rs:596-621`): the menu acts on the whole selection when the
    /// right-clicked row is inside it, else just the clicked row; "Rename" is
    /// offered only for a single-row selection. Recorded, not minted.
    fn on_row_right_down(&mut self, tab_id: &str, _e: &MouseDownEvent, cx: &mut Context<Self>) {
        let action_ids = self.selection.selection_ids_for_right_click_on(tab_id);
        self.last_menu = Some(MenuDescriptor {
            close_label: close_menu_label(action_ids.len()),
            has_rename: action_ids.len() == 1,
            action_ids,
        });
        cx.stop_propagation();
    }

    /// Mirror of the shipped menu action's pre-run snap
    /// (`SidebarTabSelection::snap_if_right_click_outside`): picking a menu item
    /// for a row outside the selection first snaps the selection to it.
    fn pick_menu_item_snaps(&mut self, clicked_id: &str) {
        self.selection.snap_if_right_click_outside(clicked_id);
    }

    fn on_empty_area_down(&mut self, _e: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // The shipped empty-area handler collapses without consuming (rows
        // consume their own presses, so it only ever sees the gaps).
        self.collapse_selection_to_active();
        self.empty_area_collapses += 1;
        cx.notify();
    }

    fn on_background_down(
        &mut self,
        _e: &MouseDownEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.escaped_to_background += 1;
    }

    // ---- band (mirror; records instead of touching a real Window) ----------

    fn on_strip_down(&mut self, e: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.band_press = None;
        if e.click_count >= 2 {
            // The shipped strip runs `window.titlebar_double_click()` here; the
            // mocked window has no NSWindow, so record nothing beyond consuming.
            cx.stop_propagation();
            return;
        }
        self.band_presses += 1;
        self.band_press = Some(e.position);
    }

    fn on_strip_move(&mut self, e: &MouseMoveEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let Some(origin) = self.band_press else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            self.band_press = None;
            return;
        }
        let dx = f32::from(e.position.x - origin.x);
        let dy = f32::from(e.position.y - origin.y);
        if dx * dx + dy * dy >= DRAG_THRESHOLD_SQ {
            self.band_press = None;
            // The shipped strip calls `window.start_window_move()`; record the
            // promotion instead (a mocked window can't move).
            self.band_window_moves += 1;
        }
    }

    fn on_strip_up(&mut self, _e: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.band_press = None;
    }

    // ---- Esc action + terminal key listener --------------------------------

    /// Mirror of `SidebarShellView::on_collapse_esc` (`sidebar_shell.rs:575`):
    /// cancel a rename, else collapse a >1 selection, else `cx.propagate()` so Esc
    /// reaches the terminal. (No rename-cancel path is exercised here — the tests
    /// drive the >1-vs-≤1 branch.)
    fn on_collapse_esc(
        &mut self,
        _action: &CollapseSelectionProbe,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_tab_id.is_some() {
            self.editing_tab_id = None;
            self.draft_title.clear();
            return; // consumed
        }
        if self.selection.selected_tab_ids().len() > 1 {
            self.collapse_selection_to_active();
            cx.notify(); // consumed
        } else {
            cx.propagate();
        }
    }

    /// The "terminal" key-down listener — fires only when the Esc action
    /// propagated (gpui runs key-down listeners after an action only on
    /// `cx.propagate()`; `window.rs:4997-5010`), so it observes Esc reaching the
    /// terminal.
    fn on_terminal_key(&mut self, e: &KeyDownEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if e.keystroke.key == "escape" {
            self.esc_reached_terminal += 1;
        }
    }

    // ---- render ------------------------------------------------------------

    fn row(&self, i: usize, tab_id: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let id_bg = tab_id.to_string();
        let id_title = tab_id.to_string();
        let id_right = tab_id.to_string();
        // An absolutely-positioned row is itself the containing block for its
        // title child — no `.relative()` (which would clobber `.absolute()`,
        // last-wins, and collapse every row back to flow position).
        div()
            .absolute()
            .top(px(i as f32 * ROW_H))
            .left(px(0.0))
            .w(px(CARD_W))
            .h(px(ROW_H))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                    this.on_row_bg_down(&id_bg, e, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                    this.on_row_right_down(&id_right, e, cx)
                }),
            )
            .child(
                // The title sub-hitbox — a left-press here begins rename (past the
                // gate) rather than routing the row bg.
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(TITLE_X))
                    .w(px(CARD_W - TITLE_X))
                    .h(px(ROW_H))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                            this.on_title_down(&id_title, e, cx)
                        }),
                    ),
            )
    }
}

impl Focusable for SidebarProbe {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SidebarProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.visible_tab_ids();
        let rows: Vec<gpui::AnyElement> = visible
            .iter()
            .enumerate()
            .map(|(i, id)| self.row(i, id, cx).into_any_element())
            .collect();

        div()
            // Outermost background catcher — painted first, so it is LAST in the
            // bubble phase and sees a press only if nothing inner consumed it.
            // `relative` makes it the containing block the absolute band + list
            // position against (in window coordinates).
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .key_context(PROBE_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_collapse_esc))
            .on_key_down(cx.listener(Self::on_terminal_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_background_down))
            .child(
                // The 52pt top strip (R9 band pattern).
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .w(px(CARD_W))
                    .h(px(BAND_H))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_strip_down))
                    .on_mouse_move(cx.listener(Self::on_strip_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_strip_up)),
            )
            .child(
                // The tab list — carries the empty-area collapse handler; rows are
                // absolute children (the absolute list is their containing block,
                // so no `.relative()`, which would clobber `.absolute()`).
                div()
                    .absolute()
                    .top(px(LIST_TOP))
                    .left(px(0.0))
                    .w(px(CARD_W))
                    .h(px(600.0))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_empty_area_down))
                    .children(rows),
            )
    }
}

// ---- harness ---------------------------------------------------------------

/// Seed a flat model of four navigable terminal tabs
/// (`terminals-main`, `t1`, `t2`, `t3`), Main selected — the fixture for the
/// multi-select / rename / Esc / band cases.
fn seed_flat_model() -> TabModel {
    let mut m = TabModel::new("/home/u");
    for i in 1..=3 {
        let id = format!("t{i}");
        let pane_id = format!("{id}-p");
        let mut tab = Tab::new(id.clone(), format!("Tab {i}"), "/home/u");
        tab.panes = vec![Pane::new(pane_id.clone(), "Terminal 1", PaneKind::Terminal)];
        tab.active_pane_id = Some(pane_id);
        m.projects[0].tabs.push(tab);
    }
    m.select_tab(TabModel::MAIN_TERMINAL_TAB_ID);
    m
}

/// Seed a two-group model: the pinned Terminals group (Main) plus a project
/// `proj` holding two Claude tabs — the fixture for disclosure + count-pill cases.
fn seed_projects_model() -> TabModel {
    let mut m = TabModel::new("/home/u");
    let pi = m.ensure_project("proj", "Proj", "/home/u/proj");
    for i in 0..2 {
        let id = format!("p{i}");
        let pane_id = format!("{id}-c");
        let mut tab = Tab::new(id.clone(), format!("Claude {i}"), "/home/u/proj");
        tab.panes = vec![Pane::new(pane_id.clone(), "Claude", PaneKind::Claude)];
        tab.active_pane_id = Some(pane_id);
        m.projects[pi].tabs.push(tab);
    }
    m
}

/// Mount a probe over `model` in a fresh mocked window, register the Esc binding
/// in the `SidebarShellProbe` context, focus the probe (so the binding matches),
/// and run to a first paint. Returns the view + the window context.
fn mount_probe<'a>(
    cx: &'a mut TestAppContext,
    model: TabModel,
) -> (Entity<SidebarProbe>, &'a mut VisualTestContext) {
    cx.update(|app| {
        app.bind_keys([KeyBinding::new(
            "escape",
            CollapseSelectionProbe,
            Some(PROBE_KEY_CONTEXT),
        )]);
    });
    let (probe, vcx) = cx.add_window_view(|_window, cx| SidebarProbe::new(model, cx));
    let handle = probe.read_with(vcx, |p, _| p.focus_handle.clone());
    vcx.update(|window, cx| window.focus(&handle, cx));
    vcx.run_until_parked();
    (probe, vcx)
}

/// Read the sorted selected-id set (for order-independent assertions).
fn selection(probe: &Entity<SidebarProbe>, vcx: &mut VisualTestContext) -> Vec<String> {
    let mut ids: Vec<String> =
        probe.read_with(vcx, |p, _| p.selection.selected_tab_ids().iter().cloned().collect());
    ids.sort();
    ids
}

fn active(probe: &Entity<SidebarProbe>, vcx: &mut VisualTestContext) -> Option<String> {
    probe.read_with(vcx, |p, _| p.model.active_tab_id().map(str::to_string))
}

fn anchor(probe: &Entity<SidebarProbe>, vcx: &mut VisualTestContext) -> Option<String> {
    probe.read_with(vcx, |p, _| p.selection.last_clicked_tab_id().map(str::to_string))
}

fn read_u32(
    probe: &Entity<SidebarProbe>,
    vcx: &mut VisualTestContext,
    f: impl Fn(&SidebarProbe) -> u32,
) -> u32 {
    probe.read_with(vcx, |p, _| f(p))
}

fn ids(items: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

// ============================================================================
// multi-select routing (plain / ⌘ / ⇧) — via simulated clicks
// ============================================================================

/// A plain click replaces the selection + activates, and the row **consumes** the
/// press: the band never armed, the empty-area collapse never ran, nothing leaked
/// to the background (the differential-pair rule).
#[gpui::test]
fn plain_click_replaces_selection_and_is_consumed_by_the_row(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(1), Modifiers::none()); // -> "t1"

    assert_eq!(selection(&probe, vcx), ids(&["t1"]), "plain click collapses to the clicked id");
    assert_eq!(active(&probe, vcx).as_deref(), Some("t1"), "clicked row becomes active");
    assert_eq!(anchor(&probe, vcx).as_deref(), Some("t1"), "anchor moves to the clicked row");
    assert_eq!(read_u32(&probe, vcx, |p| p.band_presses), 0, "the band never armed on a row press");
    assert_eq!(
        read_u32(&probe, vcx, |p| p.empty_area_collapses),
        0,
        "the row press did not reach the empty-area collapse handler"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.escaped_to_background),
        0,
        "the row consumed the press — nothing leaked to the background catcher"
    );
}

/// ⌘-click toggles the clicked row in and moves active onto it (most-recently-
/// clicked rule).
#[gpui::test]
fn cmd_click_toggles_in_and_moves_active(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(0), Modifiers::none()); // plain -> "terminals-main"
    vcx.simulate_click(row_bg_point(1), Modifiers::command()); // ⌘ -> add "t1"

    assert_eq!(selection(&probe, vcx), ids(&["terminals-main", "t1"]));
    assert_eq!(active(&probe, vcx).as_deref(), Some("t1"), "⌘-toggle-in moves active to the toggled id");
    assert_eq!(anchor(&probe, vcx).as_deref(), Some("t1"));
}

/// ⌘-click on the only-and-active selected row is **refused** — the invariant
/// selection ⊇ {active} survives (`SidebarTabSelection::toggle` returns `None`).
#[gpui::test]
fn cmd_click_only_active_row_is_refused(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(2), Modifiers::none()); // plain -> "t2" (only + active)
    vcx.simulate_click(row_bg_point(2), Modifiers::command()); // ⌘ on the only-and-active row

    assert_eq!(selection(&probe, vcx), ids(&["t2"]), "the set must NOT empty — toggle-out refused");
    assert_eq!(active(&probe, vcx).as_deref(), Some("t2"), "active must NOT clear — invariant survives");
}

/// ⇧-click extends the selection from the sticky anchor to the clicked row,
/// inclusive, and does **not** move the anchor (Finder keeps the original anchor
/// across range extensions); the clicked row becomes active.
#[gpui::test]
fn shift_click_extends_from_sticky_anchor(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(1), Modifiers::none()); // plain -> "t1" (anchor)
    vcx.simulate_click(row_bg_point(3), Modifiers::shift()); // ⇧ -> extend to "t3"

    assert_eq!(
        selection(&probe, vcx),
        ids(&["t1", "t2", "t3"]),
        "⇧-extend spans the anchor..target run inclusive"
    );
    assert_eq!(anchor(&probe, vcx).as_deref(), Some("t1"), "⇧-extend must NOT move the anchor");
    assert_eq!(active(&probe, vcx).as_deref(), Some("t3"), "the ⇧-clicked row becomes active");

    // A second ⇧-extend re-uses the SAME anchor (stickiness across extensions).
    vcx.simulate_click(row_bg_point(2), Modifiers::shift()); // ⇧ -> re-extend to "t2"
    assert_eq!(selection(&probe, vcx), ids(&["t1", "t2"]), "re-extend still measured from the sticky anchor");
    assert_eq!(anchor(&probe, vcx).as_deref(), Some("t1"), "anchor stayed put across the second extension");
}

// ============================================================================
// keyboard sidebar-nav re-syncs the selection (the ⌘⌥↓ residue fix)
// ============================================================================

/// Keyboard sidebar-nav (`NextSidebarTab`, ⌘⌥↓) collapses the multi-selection to
/// the new active tab: the previously-active/selected rows must NOT linger in the
/// selection set (the faint `SELECTED_DIM_FACTOR` highlight residue). This is the
/// gap the file's mouse-only cases missed — the shipped keymap handler mutated the
/// model without re-syncing `selection`, so a prior-active row stayed a
/// dim-tinted set member. Seed a >1 selection whose active tab is one member,
/// cycle, and assert the set is exactly the new active tab.
#[gpui::test]
fn keyboard_nav_resyncs_selection_to_new_active(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    // Build a two-row selection: {terminals-main, t1}, active on t1.
    vcx.simulate_click(row_bg_point(0), Modifiers::none()); // -> {terminals-main}
    vcx.simulate_click(row_bg_point(1), Modifiers::command()); // + t1 (now active)
    assert_eq!(selection(&probe, vcx), ids(&["terminals-main", "t1"]));
    assert_eq!(active(&probe, vcx).as_deref(), Some("t1"));

    // Keyboard-cycle to the next tab: the selection must collapse onto it, dropping
    // both the prior-active row (t1) and the other set member (terminals-main).
    probe.update(vcx, |p, _| p.route_next_sidebar_tab());

    let new_active = active(&probe, vcx).expect("a tab is active after cycling");
    assert_ne!(new_active, "t1", "the cycle moved the active tab off t1");
    assert_eq!(
        selection(&probe, vcx),
        ids(&[new_active.as_str()]),
        "keyboard nav collapses the selection to the new active tab — no stale prior-active row lingers"
    );
}

// ============================================================================
// empty-area click + Esc collapse
// ============================================================================

/// A click in the empty tab-list gap collapses a multi-selection to the active
/// tab and reaches the collapse handler; a row press (the differential half) does
/// not.
#[gpui::test]
fn empty_area_click_collapses_multi_selection(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(0), Modifiers::none()); // "terminals-main"
    vcx.simulate_click(row_bg_point(1), Modifiers::command()); // + "t1" (active)
    assert_eq!(selection(&probe, vcx), ids(&["terminals-main", "t1"]));

    vcx.simulate_click(empty_area_point(4), Modifiers::none());

    assert_eq!(selection(&probe, vcx), ids(&["t1"]), "empty-area click collapses to the active tab");
    assert!(
        read_u32(&probe, vcx, |p| p.empty_area_collapses) >= 1,
        "the empty-area press reached the collapse handler"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_presses),
        0,
        "an empty-area press never arms the band's window-drag"
    );
}

/// Esc collapses a multi-selection **only when >1 is selected**: with >1 it
/// collapses and consumes (never reaching the terminal); with ≤1 it `propagate()`s
/// so Esc still reaches the focused terminal (asserted via the terminal key
/// listener). The differential pair in one case.
#[gpui::test]
fn esc_collapses_only_when_more_than_one_selected(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    // >1 selected: Esc collapses and is consumed (terminal sees nothing).
    vcx.simulate_click(row_bg_point(0), Modifiers::none()); // "terminals-main"
    vcx.simulate_click(row_bg_point(1), Modifiers::command()); // + "t1"
    assert_eq!(selection(&probe, vcx), ids(&["terminals-main", "t1"]));

    vcx.simulate_keystrokes("escape");
    assert_eq!(selection(&probe, vcx), ids(&["t1"]), "Esc with >1 selected collapses to the active tab");
    assert_eq!(
        read_u32(&probe, vcx, |p| p.esc_reached_terminal),
        0,
        "Esc with >1 selected is consumed — it must NOT reach the terminal"
    );

    // ≤1 selected: Esc propagates through to the terminal, selection unchanged.
    vcx.simulate_keystrokes("escape");
    assert_eq!(selection(&probe, vcx), ids(&["t1"]), "Esc with 1 selected leaves the selection alone");
    assert_eq!(
        read_u32(&probe, vcx, |p| p.esc_reached_terminal),
        1,
        "Esc with ≤1 selected propagates so the terminal receives it"
    );
}

// ============================================================================
// band drag arm (top-strip vs tab-row) — classification, not frame motion
// ============================================================================

/// A press-drag on the empty top strip arms + promotes a window drag; the same
/// press-drag started on a tab row consumes at the row so the band never arms
/// (the in-process analog of "top-strip drag moves the window, a tab-row drag
/// does not" — real frame motion is vacuous in-process and lives in the live
/// scenario).
#[gpui::test]
fn band_arm_fires_for_strip_press_not_for_row_press(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    // Strip: press then drag past the ~2pt threshold arms exactly one move.
    let start = band_point();
    vcx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    assert_eq!(read_u32(&probe, vcx, |p| p.band_presses), 1, "a strip press arms the band");
    vcx.simulate_mouse_move(point(start.x + px(40.0), start.y), Some(MouseButton::Left), Modifiers::none());
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_window_moves),
        1,
        "crossing the ~2pt threshold on the strip promotes exactly one window move"
    );
    vcx.simulate_mouse_up(point(start.x + px(40.0), start.y), MouseButton::Left, Modifiers::none());

    // Row: a press-drag beginning on a tab row is consumed by the row, so the
    // band never arms and never promotes — no window move.
    let rstart = row_bg_point(1);
    vcx.simulate_mouse_down(rstart, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(point(rstart.x + px(40.0), rstart.y), Some(MouseButton::Left), Modifiers::none());
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_presses),
        1,
        "the row press did not arm the band (still just the one strip arm)"
    );
    assert_eq!(
        read_u32(&probe, vcx, |p| p.band_window_moves),
        1,
        "a drag beginning on a tab row promotes no window move"
    );
    vcx.simulate_mouse_up(point(rstart.x + px(40.0), rstart.y), MouseButton::Left, Modifiers::none());
}

// ============================================================================
// right-click snap policy + menu title + Rename single-selection-only
// ============================================================================

/// Right-clicking a row **outside** the current selection offers a single-tab
/// menu ("Close Tab" + "Rename Tab"); picking an item snaps the selection to that
/// row (Finder's "right-click outside replaces").
#[gpui::test]
fn right_click_outside_selection_is_single_tab_and_snaps(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(0), Modifiers::none()); // selection {terminals-main}
    vcx.simulate_mouse_down(row_bg_point(1), MouseButton::Right, Modifiers::none());

    let menu = probe.read_with(vcx, |p, _| p.last_menu.clone()).expect("a menu was recorded");
    assert_eq!(menu.action_ids, vec!["t1".to_string()], "menu acts on the clicked row only");
    assert_eq!(menu.close_label, "Close Tab");
    assert!(menu.has_rename, "Rename is offered for a single-row selection");
    // The pure read must NOT have mutated the selection (that would loop render).
    assert_eq!(selection(&probe, vcx), ids(&["terminals-main"]), "menu id resolution is a pure read");

    // Picking an item snaps the selection to the right-clicked row.
    probe.update(vcx, |p, _| p.pick_menu_item_snaps("t1"));
    assert_eq!(selection(&probe, vcx), ids(&["t1"]), "picking a menu item outside the selection snaps to it");
}

/// Right-clicking a row **inside** a multi-selection offers a "Close N Tabs" menu
/// acting on the whole set, with **no** Rename (single-selection only); a pick is
/// a no-op snap (the row is already selected).
#[gpui::test]
fn right_click_inside_multi_selection_is_close_n_no_rename(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    vcx.simulate_click(row_bg_point(0), Modifiers::none()); // {terminals-main}
    vcx.simulate_click(row_bg_point(1), Modifiers::command()); // + t1
    vcx.simulate_click(row_bg_point(2), Modifiers::command()); // + t2  -> {main, t1, t2}
    vcx.simulate_mouse_down(row_bg_point(1), MouseButton::Right, Modifiers::none()); // inside

    let menu = probe.read_with(vcx, |p, _| p.last_menu.clone()).expect("a menu was recorded");
    let mut got = menu.action_ids.clone();
    got.sort();
    assert_eq!(got, ids(&["terminals-main", "t1", "t2"]), "menu acts on the whole selection");
    assert_eq!(menu.close_label, "Close 3 Tabs", "close label pluralizes with the selection size");
    assert!(!menu.has_rename, "Rename is hidden for a multi-row selection");

    // A pick inside the selection does not collapse it (no snap).
    probe.update(vcx, |p, _| p.pick_menu_item_snaps("t1"));
    assert_eq!(
        selection(&probe, vcx),
        ids(&["terminals-main", "t1", "t2"]),
        "right-clicking a row already in the selection must not collapse it"
    );
}

// ============================================================================
// rename gate via advance_clock
// ============================================================================

/// The rename gate: the click that **activates** a row never edits, and a quick
/// second title tap (inside the double-click interval) still never edits; only a
/// **slow** second tap, past the interval on the simulated clock, begins rename.
#[gpui::test]
fn rename_gate_blocks_activation_and_quick_taps_allows_slow_tap(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_flat_model());

    // Title-tap a not-yet-active row: this is the ACTIVATING click — it selects,
    // it must NOT edit.
    vcx.simulate_click(row_title_point(1), Modifiers::none());
    assert_eq!(active(&probe, vcx).as_deref(), Some("t1"), "the title tap activated the row");
    assert!(
        probe.read_with(vcx, |p, _| p.editing_tab_id.is_none()),
        "the activating click must never begin a rename"
    );

    // A quick second tap, still inside the interval: must NOT edit.
    vcx.simulate_click(row_title_point(1), Modifiers::none());
    assert!(
        probe.read_with(vcx, |p, _| p.editing_tab_id.is_none()),
        "a second tap inside the double-click interval must not begin a rename"
    );

    // Advance the SIMULATED clock past the interval, then a slow tap edits.
    vcx.executor().advance_clock(DOUBLE_CLICK_INTERVAL + Duration::from_millis(10));
    vcx.simulate_click(row_title_point(1), Modifiers::none());
    assert_eq!(
        probe.read_with(vcx, |p, _| p.editing_tab_id.clone()),
        Some("t1".to_string()),
        "a slow second tap past the interval begins the inline rename"
    );
}

// ============================================================================
// disclosure hides rows + count pill tracks count
// ============================================================================

/// Collapsing a project's disclosure removes its rows from the rendered list
/// (they are hidden, not merely unstyled); expanding restores them. The pinned
/// Terminals group's rows are unaffected.
#[gpui::test]
fn disclosure_collapse_hides_a_projects_rows(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_projects_model());

    // Both groups' rows render: Terminals' Main + proj's two Claude tabs.
    assert_eq!(
        probe.read_with(vcx, |p, _| p.visible_tab_ids()),
        vec!["terminals-main".to_string(), "p0".to_string(), "p1".to_string()]
    );

    // Collapse "proj" — its rows vanish; Terminals' row stays.
    probe.update(vcx, |p, _| {
        p.collapsed_projects.insert("proj".to_string());
    });
    vcx.run_until_parked();
    assert_eq!(
        probe.read_with(vcx, |p, _| p.visible_tab_ids()),
        vec!["terminals-main".to_string()],
        "a collapsed project contributes no rows"
    );

    // Expand again — the rows come back in order.
    probe.update(vcx, |p, _| {
        p.collapsed_projects.remove("proj");
    });
    vcx.run_until_parked();
    assert_eq!(
        probe.read_with(vcx, |p, _| p.visible_tab_ids()),
        vec!["terminals-main".to_string(), "p0".to_string(), "p1".to_string()]
    );
}

/// The count pill tracks the project's tab count — independent of disclosure —
/// and follows an add.
#[gpui::test]
fn count_pill_tracks_tab_count(cx: &mut TestAppContext) {
    let (probe, vcx) = mount_probe(cx, seed_projects_model());

    assert_eq!(probe.read_with(vcx, |p, _| p.project_tab_count("proj")), Some(2));

    // Count is disclosure-independent: collapsing the group must not change it.
    probe.update(vcx, |p, _| {
        p.collapsed_projects.insert("proj".to_string());
    });
    assert_eq!(
        probe.read_with(vcx, |p, _| p.project_tab_count("proj")),
        Some(2),
        "the count pill shows the tab count even while the group is collapsed"
    );

    // Adding a tab bumps the count.
    probe.update(vcx, |p, _| {
        let pi = p.model.projects.iter().position(|pr| pr.id == "proj").unwrap();
        p.model.projects[pi].tabs.push(Tab::new("p2", "Claude 2", "/home/u/proj"));
    });
    assert_eq!(
        probe.read_with(vcx, |p, _| p.project_tab_count("proj")),
        Some(3),
        "the count pill follows an added tab"
    );
}
