//! In-process pane-strip **real-layout** differentials for the R11 toolbar pane
//! strip — **execution model: mocked [`gpui::TestAppContext`], ordinary libtest
//! `#[gpui::test]` cases** (no Metal, no pixels; parallel-safe).
//!
//! The shipped view (`WindowToolbarView` in the `nice` binary) cannot be imported
//! here — `nice-itests` is dev/test-only and the app binary never depends on it
//! (and vice versa), the same constraint the R9 [`crate::chrome_band`] probe and
//! the R10 [`crate::sidebar_multiselect`] probe document. So this mirrors the
//! strip's real-layout logic in a local [`PaneStripProbe`] that drives a **real**
//! [`gpui::ScrollHandle`] over real, fixed-width pill children and the **real**
//! `nice-model` predicates ([`should_show_overflow_chevron`], [`StripGeometry`],
//! [`center_offset_x`], [`Tab::has_offscreen_attention`], [`TabModel`]). The
//! mirrored glue is thin (the `viewport_relative_rect` translation, the
//! chevron/fades/badge derivations, and the select/close/add/rename routing); the
//! real `ScrollHandle` does the layout and the real model does the reasoning, so a
//! drift in either surfaces here.
//!
//! ## Why real layout, not rect fixtures
//!
//! [`nice_model::strip_geometry`]'s pure predicates are already unit-tested with
//! plain rect fixtures (slice 1). What those fixtures **cannot** prove is that the
//! GPUI `ScrollHandle` actually reports the overflow / per-item bounds the view
//! feeds them — that the chevron flips at the exact pane count where the
//! reserved-width viewport first overflows, that a hover-toggled ✕ slot keeps the
//! pill's laid-out width constant, that centering an offscreen pill via
//! `set_offset(center_offset_x(..))` actually reveals it. Those are layout facts;
//! this probe mounts a real scroll row on the mocked context (Taffy layout runs;
//! only Metal/text rendering is stubbed) and asserts them against
//! `ScrollHandle::{max_offset, bounds, bounds_for_item, offset}`.
//!
//! Pills are laid out with **fixed-width children** ([`PILL_W`]) so their geometry
//! is deterministic under `NoopTextSystem` (which measures no glyphs) — the strip's
//! visibility math is width-only, so this loses nothing. The scroll viewport is
//! `flex_1` inside a fixed-width toolbar row with the chevron + `+` slots
//! **always** reserved as `flex_none` siblings, so the viewport width is
//! `TOOLBAR_W − 2·[`RESERVED_SLOT`]` regardless of whether the chevron renders —
//! that reservation is exactly what kills the show-chevron→shrink→hide feedback
//! loop, and it's what these cases pin.
//!
//! The pill's click-to-rename **gate** is R10's shared [`nice_model::InlineRenameClickGate`],
//! already pinned in [`crate::sidebar_multiselect`] with `advance_clock`; this
//! module exercises the pill's rename *commit* path (empty-submit reset) instead,
//! not the gate. Neither this nor any behavior test asserts cadence / perf /
//! wall-clock timing — those live only in the live `pane-strip` self-test
//! scenario.

use std::collections::HashSet;

use gpui::{
    div, point, prelude::*, px, Bounds, Context, Entity, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, Pixels, Point, Render, ScrollHandle, SharedString, TestAppContext,
    VisualTestContext, Window,
};

use nice_model::{
    center_offset_x, should_show_overflow_chevron, Pane, PaneKind, Rect, StripGeometry, Tab,
    TabModel, TabStatus,
};

// ---- Geometry (deterministic, fixed-width pills) ----------------------------
//
// Mirrors the shipped pill anatomy (`WindowToolbarView.swift` / `toolbar.rs`) but
// with every extent an explicit literal so layout is text-system-independent. The
// title is a fixed-width box (real pills truncate a variable title to <= 220pt;
// the strip math is width-only, so a fixed box is faithful and deterministic).

/// Pill leading / trailing padding (`toolbar.rs` `PILL_LEADING_PAD` /
/// `PILL_TRAILING_PAD`).
const PILL_PAD_L: f32 = 10.0;
const PILL_PAD_R: f32 = 6.0;
/// Inter-child spacing inside a pill (`toolbar.rs` `PILL_GAP`).
const PILL_GAP: f32 = 7.0;
/// Leading icon box (`toolbar.rs` `PILL_ICON_SIZE`).
const ICON_W: f32 = 12.0;
/// The always-reserved close-"×" slot (`toolbar.rs` `CLOSE_BTN_SIZE`): laid out
/// whether or not the ✕ is visible, so the pill width never jumps on hover.
const CLOSE_SLOT: f32 = 16.0;
/// Fixed title box — a deterministic stand-in for the truncating title.
const TITLE_W: f32 = 80.0;
/// Pill height (`toolbar.rs` `PILL_HEIGHT`).
const PILL_H: f32 = 28.0;
/// A pill's total laid-out width: `pad_l + icon + gap + title + gap + close + pad_r`.
const PILL_W: f32 = PILL_PAD_L + ICON_W + PILL_GAP + TITLE_W + PILL_GAP + CLOSE_SLOT + PILL_PAD_R;
/// Inter-pill spacing in the scroll row (`toolbar.rs` `PILL_ROW_GAP`).
const ROW_GAP: f32 = 2.0;
/// The chevron slot / `+` slot width, each **unconditionally** reserved as a
/// `flex_none` sibling of the scroll viewport (`toolbar.rs` `SQUARE_SLOT_WIDTH`).
const RESERVED_SLOT: f32 = 28.0;

/// The laid-out width of `n` pills plus their inter-pill gaps.
fn pills_content_width(n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * PILL_W + (n as f32 - 1.0) * ROW_GAP
    }
}

/// Translate a scroll child's window-space (offset-free) bounds into the
/// viewport-relative `[0, visible_width]` space [`StripGeometry`] reads — the
/// same arithmetic the shipped view's private `viewport_relative_rect` performs
/// (`toolbar.rs`): a child's on-screen left is `item_left + offset_x`, so its
/// position relative to the viewport's leading edge is
/// `item_left + offset_x − viewport_left`.
fn viewport_relative_rect(item_left: f32, item_width: f32, offset_x: f32, viewport_left: f32) -> Rect {
    Rect::new(item_left + offset_x - viewport_left, item_width)
}

// ---- The probe --------------------------------------------------------------

/// A flat pane strip mirroring `WindowToolbarView`'s real-layout logic over the
/// real `nice-model` state and a real [`ScrollHandle`], recording nothing beyond
/// the model + scroll state the accessors read back.
struct PaneStripProbe {
    /// The real R8 document the routing mutates.
    model: TabModel,
    /// The active tab whose panes render (fixed for a probe's lifetime).
    tab_id: String,
    /// The total toolbar-row width; the scroll viewport is this minus the two
    /// always-reserved trailing slots (the reservation the overflow rule needs).
    toolbar_w: f32,
    /// The pill row's real scroll state — the source of overflow / fades /
    /// centering (mirrors the view's `scroll`).
    scroll: ScrollHandle,
    /// The pill (if any) the cursor is over — toggles its ✕ visibility; the slot
    /// is always laid out either way (the hover-invariant-width contract).
    hovered_pane_id: Option<String>,
    /// The pane being inline-renamed + its draft, if any.
    editing_pane: Option<String>,
    draft: String,
    /// Monotonic id source for added panes (mirrors `ModelPaneStripActions`).
    next_id: u64,
}

impl PaneStripProbe {
    fn new(model: TabModel, tab_id: String, toolbar_w: f32, _cx: &mut Context<Self>) -> Self {
        Self {
            model,
            tab_id,
            toolbar_w,
            scroll: ScrollHandle::new(),
            hovered_pane_id: None,
            editing_pane: None,
            draft: String::new(),
            next_id: 0,
        }
    }

    // ---- model access ------------------------------------------------------

    fn tab(&self) -> &Tab {
        self.model.tab_for(&self.tab_id).expect("probe tab exists")
    }

    fn pane_ids(&self) -> Vec<String> {
        self.tab().panes.iter().map(|p| p.id.clone()).collect()
    }

    fn active_pane_id(&self) -> Option<String> {
        self.tab().active_pane_id.clone()
    }

    // ---- real-layout derivations (mirror of the view) ----------------------

    /// The pill row's real geometry: each pane's viewport-relative rect + the
    /// viewport width, fed to [`StripGeometry`] (mirror of the view's
    /// `strip_geometry`).
    fn strip_geometry(&self) -> StripGeometry {
        let viewport = self.scroll.bounds();
        let viewport_left = f32::from(viewport.origin.x);
        let visible_width = f32::from(viewport.size.width);
        let offset_x = f32::from(self.scroll.offset().x);
        let mut frames = std::collections::HashMap::new();
        for (ix, pane) in self.tab().panes.iter().enumerate() {
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
        StripGeometry::new(frames, visible_width)
    }

    /// The `>= 2` panes + reserved-real-overflow rule (mirror of `show_chevron`).
    fn show_chevron(&self) -> bool {
        should_show_overflow_chevron(self.tab().panes.len(), f32::from(self.scroll.max_offset().x))
    }

    /// The fully-offscreen pane ids (drives the fades + badge).
    fn offscreen_ids(&self) -> HashSet<String> {
        self.strip_geometry().offscreen_pane_ids()
    }

    /// Whether some fully-offscreen pane needs attention — reuses the R8
    /// [`Tab::has_offscreen_attention`] fed this cycle's offscreen set (no second
    /// predicate — dossier G2).
    fn has_offscreen_attention(&self) -> bool {
        self.tab().has_offscreen_attention(&self.offscreen_ids())
    }

    fn pill_bounds(&self, pane_id: &str) -> Option<Bounds<Pixels>> {
        let ix = self.tab().panes.iter().position(|p| p.id == pane_id)?;
        self.scroll.bounds_for_item(ix)
    }

    // ---- routing (mirror of ModelPaneStripActions + the view) --------------

    /// A plain press on a pill body: select the pane (mirror of the view's
    /// `select_pane` → `ModelPaneStripActions::select_pane`, guarded against a
    /// dangling active id), then auto-center it (mirror of `try_center_active`).
    fn select_pane(&mut self, pane_id: &str) {
        if let Some((pi, ti)) = self.model.project_tab_index(&self.tab_id) {
            let tab = &mut self.model.projects[pi].tabs[ti];
            if tab.panes.iter().any(|p| p.id == pane_id)
                && tab.active_pane_id.as_deref() != Some(pane_id)
            {
                tab.active_pane_id = Some(pane_id.to_string());
                self.center_active();
            }
        }
    }

    /// Close a pane via the single [`TabModel::extract_pane`] entry point (mirror
    /// of `close_pane` → `ModelPaneStripActions::close_pane`; no busy-close
    /// confirmation — that is R18).
    fn close_pane(&mut self, pane_id: &str) {
        self.model.extract_pane(pane_id, &self.tab_id);
    }

    /// Append an auto-named terminal pane (mirror of `add_terminal_pane` →
    /// `ModelPaneStripActions::add_terminal_pane` → the R8 "Terminal N" counter).
    fn add_terminal_pane(&mut self) -> Option<String> {
        self.next_id += 1;
        let id = format!("added-{}", self.next_id);
        self.model.add_pane(&self.tab_id, id, None)
    }

    /// Center the active pane in the viewport using the real laid-out bounds +
    /// [`center_offset_x`] + `set_offset` — the shipped `try_center_active` math.
    fn center_active(&mut self) {
        let Some(active) = self.active_pane_id() else {
            return;
        };
        let Some(ix) = self.tab().panes.iter().position(|p| p.id == active) else {
            return;
        };
        let Some(item) = self.scroll.bounds_for_item(ix) else {
            return; // not laid out yet
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
    }

    // ---- inline rename (mirror of the view; the pill reimplements no policy) ---

    fn begin_editing(&mut self, pane_id: &str) {
        let title = self
            .tab()
            .panes
            .iter()
            .find(|p| p.id == pane_id)
            .map(|p| p.title.clone());
        if let Some(title) = title {
            self.editing_pane = Some(pane_id.to_string());
            self.draft = title;
        }
    }

    fn set_draft(&mut self, draft: &str) {
        self.draft = draft.to_string();
    }

    /// Commit the draft through the R8 [`TabModel::rename_pane`] (empty input
    /// resets to the per-kind auto-default + consumes a "Terminal N" counter slot
    /// — asymmetry 3; the pill reimplements none of it).
    fn commit_rename(&mut self) {
        let Some(pane_id) = self.editing_pane.take() else {
            return;
        };
        let draft = std::mem::take(&mut self.draft);
        self.model.rename_pane(&self.tab_id, &pane_id, &draft);
    }

    // ---- event handlers ----------------------------------------------------

    fn on_pill_down(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        self.select_pane(pane_id);
        cx.notify();
        cx.stop_propagation();
    }

    /// The ✕ closes and **consumes** the press (`stop_propagation`) so the pill's
    /// own select never runs — the differential that keeps a ✕-click from
    /// activating the pane it closes.
    fn on_close_down(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        self.close_pane(pane_id);
        cx.notify();
        cx.stop_propagation();
    }

    // ---- render ------------------------------------------------------------

    fn pill(&self, pane: &Pane, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = self.tab().active_pane_id.as_deref() == Some(pane.id.as_str());
        let hovered = self.hovered_pane_id.as_deref() == Some(pane.id.as_str());
        let close_visible = active || hovered;

        let id_body = pane.id.clone();
        let id_close = pane.id.clone();

        // The always-reserved ✕ slot: laid out at CLOSE_SLOT wide regardless of
        // visibility; only the handler + paint are gated (mirror of the view's
        // reserved-but-inert close button).
        let mut close = div().flex_none().w(px(CLOSE_SLOT)).h(px(CLOSE_SLOT));
        if close_visible {
            close = close.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                    this.on_close_down(&id_close, cx)
                }),
            );
        } else {
            close = close.opacity(0.0);
        }

        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(PILL_GAP))
            .pl(px(PILL_PAD_L))
            .pr(px(PILL_PAD_R))
            .h(px(PILL_H))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                    this.on_pill_down(&id_body, cx)
                }),
            )
            // Leading icon (fixed box).
            .child(div().flex_none().w(px(ICON_W)).h(px(ICON_W)))
            // Title (fixed box; clipped so a stray glyph can't widen the pill).
            .child(
                div()
                    .flex_none()
                    .w(px(TITLE_W))
                    .h(px(PILL_H))
                    .overflow_hidden()
                    .child(SharedString::from(pane.title.clone())),
            )
            .child(close)
            .into_any_element()
    }
}

impl Render for PaneStripProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panes: Vec<Pane> = self.tab().panes.clone();
        let mut row = div()
            .id("probe.paneStrip")
            .track_scroll(&self.scroll)
            .overflow_x_scroll()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(ROW_GAP))
            .size_full();
        for pane in &panes {
            row = row.child(self.pill(pane, cx));
        }

        // The toolbar row: a fixed-width band holding the flex_1 scroll viewport
        // and the two ALWAYS-reserved trailing slots. The viewport therefore
        // measures `toolbar_w − 2·RESERVED_SLOT` whether or not the chevron
        // renders — the reservation the overflow rule depends on.
        div()
            .flex()
            .flex_row()
            .items_center()
            .w(px(self.toolbar_w))
            .h(px(PILL_H))
            .child(div().flex_1().min_w_0().h(px(PILL_H)).child(row))
            .child(div().flex_none().w(px(RESERVED_SLOT)).h(px(PILL_H)))
            .child(div().flex_none().w(px(RESERVED_SLOT)).h(px(PILL_H)))
    }
}

// ---- harness ----------------------------------------------------------------

/// Seed a model whose pinned Main terminal tab holds `n` fixed-width terminal
/// pills (`p0..p{n-1}`, "Terminal 1".."Terminal n"), `p0` active,
/// `next_terminal_index = n + 1`.
fn seed_terminals(n: usize) -> (TabModel, String) {
    let mut m = TabModel::new("/tmp");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID.to_string();
    let (pi, ti) = m.project_tab_index(&tab_id).expect("main tab exists");
    let panes: Vec<Pane> = (0..n)
        .map(|i| Pane::new(format!("p{i}"), format!("Terminal {}", i + 1), PaneKind::Terminal))
        .collect();
    m.projects[pi].tabs[ti].panes = panes;
    m.projects[pi].tabs[ti].active_pane_id = Some("p0".to_string());
    m.projects[pi].tabs[ti].next_terminal_index = n as u32 + 1;
    (m, tab_id)
}

/// The overflow-fixture toolbar width: a scroll viewport of exactly
/// `OVERFLOW_VIEWPORT` once the two reserved slots are subtracted.
const OVERFLOW_VIEWPORT: f32 = 404.0;
const OVERFLOW_TOOLBAR_W: f32 = OVERFLOW_VIEWPORT + 2.0 * RESERVED_SLOT; // 460

/// A wide toolbar whose viewport never overflows a handful of pills — for the
/// click-routing / hover-width / rename cases where every pill stays visible at
/// offset 0.
const WIDE_TOOLBAR_W: f32 = 2.0 * RESERVED_SLOT + 900.0;

/// Mount a probe over `model` in a fresh mocked window and run to a first paint so
/// the scroll handle + hitboxes are registered.
fn mount<'a>(
    cx: &'a mut TestAppContext,
    model: TabModel,
    tab_id: String,
    toolbar_w: f32,
) -> (Entity<PaneStripProbe>, &'a mut VisualTestContext) {
    let (probe, vcx) = cx.add_window_view(|_window, cx| PaneStripProbe::new(model, tab_id, toolbar_w, cx));
    vcx.run_until_parked();
    (probe, vcx)
}

fn read<T>(
    probe: &Entity<PaneStripProbe>,
    vcx: &mut VisualTestContext,
    f: impl Fn(&PaneStripProbe) -> T,
) -> T {
    probe.read_with(vcx, |p, _| f(p))
}

/// The on-screen centre of a pill (offset-free bounds + the current scroll
/// offset), for a simulated click. Only used in non-overflowing fixtures where
/// the offset is 0, so the offset term is a formality that stays correct if a
/// case ever scrolls first.
fn pill_center(probe: &Entity<PaneStripProbe>, vcx: &mut VisualTestContext, pane_id: &str) -> Point<Pixels> {
    probe.read_with(vcx, |p, _| {
        let b = p.pill_bounds(pane_id).expect("pill laid out");
        let off = p.scroll.offset().x;
        point(b.origin.x + off + b.size.width / 2.0, b.origin.y + b.size.height / 2.0)
    })
}

/// The on-screen centre of a pill's ✕ slot (its trailing reserved square).
fn close_center(probe: &Entity<PaneStripProbe>, vcx: &mut VisualTestContext, pane_id: &str) -> Point<Pixels> {
    probe.read_with(vcx, |p, _| {
        let b = p.pill_bounds(pane_id).expect("pill laid out");
        let off = p.scroll.offset().x;
        point(
            b.origin.x + off + b.size.width - px(PILL_PAD_R) - px(CLOSE_SLOT / 2.0),
            b.origin.y + b.size.height / 2.0,
        )
    })
}

// ============================================================================
// overflow chevron: onset at the exact reserved-width count, and never flickers
// ============================================================================

/// The chevron flips on at the exact pane count where the pills first overflow the
/// reserved-width viewport, and adding more panes never flips it back
/// (monotonic — the reservation rule kills the show→shrink→hide loop). With the
/// 404pt viewport, two 138pt pills (278pt) fit, three (418pt) do not.
#[gpui::test]
fn chevron_appears_at_the_reserved_width_overflow_count_and_never_flickers(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(2);
    let (probe, vcx) = mount(cx, model, tab_id, OVERFLOW_TOOLBAR_W);

    // The scroll viewport really measures toolbar − the two reserved slots.
    let vw = read(&probe, vcx, |p| f32::from(p.scroll.bounds().size.width));
    assert!(
        (vw - OVERFLOW_VIEWPORT).abs() < 0.5,
        "scroll viewport {vw} != the reserved-reduced width {OVERFLOW_VIEWPORT}"
    );

    // Two pills fit (278 < 404): no measured overflow, no chevron.
    assert!(pills_content_width(2) < OVERFLOW_VIEWPORT);
    assert_eq!(read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x)), 0.0);
    assert!(!read(&probe, vcx, |p| p.show_chevron()), "two fitting pills show no chevron");

    // A third pill overflows (418 > 404): the chevron appears.
    probe.update(vcx, |p, cx| {
        p.add_terminal_pane();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(pills_content_width(3) > OVERFLOW_VIEWPORT);
    assert!(read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x)) > 0.0);
    assert!(read(&probe, vcx, |p| p.show_chevron()), "the reserved-width overflow shows the chevron");

    // A fourth pill only deepens the overflow — the chevron never flickers back.
    let max3 = read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x));
    probe.update(vcx, |p, cx| {
        p.add_terminal_pane();
        cx.notify();
    });
    vcx.run_until_parked();
    let max4 = read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x));
    assert!(max4 > max3, "adding a pill deepens the overflow ({max4} > {max3})");
    assert!(read(&probe, vcx, |p| p.show_chevron()), "the chevron stays shown");
}

/// The chevron shows **only because the reserved slots are counted**: a strip
/// whose pills alone (418pt) would fit the full 460pt toolbar still overflows the
/// reserved-reduced 404pt viewport, so the chevron shows. Real `ScrollHandle`
/// overflow, measured against the reserved-width viewport, is what proves it.
#[gpui::test]
fn reservation_alone_triggers_the_chevron(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(3);
    let (probe, vcx) = mount(cx, model, tab_id, OVERFLOW_TOOLBAR_W);

    let content = pills_content_width(3);
    assert!(content < OVERFLOW_TOOLBAR_W, "pills alone ({content}) fit the full strip ({OVERFLOW_TOOLBAR_W})");
    assert!(content > OVERFLOW_VIEWPORT, "pills ({content}) overflow the reserved-reduced viewport");
    assert!(read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x)) > 0.0);
    assert!(
        read(&probe, vcx, |p| p.show_chevron()),
        "the chevron shows because the reserved chevron + '+' slots pushed the row past the viewport"
    );
}

/// The `>= 2`-panes gate is a real-layout fact too: a single pill wider than the
/// viewport genuinely overflows (`max_offset > 0`) yet shows no chevron (an
/// overflow menu is pointless with one pill); a second pill flips it on.
#[gpui::test]
fn a_single_overflowing_pill_shows_no_chevron(cx: &mut TestAppContext) {
    // A 100pt viewport is narrower than one 138pt pill.
    let (model, tab_id) = seed_terminals(1);
    let (probe, vcx) = mount(cx, model, tab_id, 2.0 * RESERVED_SLOT + 100.0);

    assert!(
        read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x)) > 0.0,
        "the lone wide pill really does overflow the viewport"
    );
    assert!(!read(&probe, vcx, |p| p.show_chevron()), "one pill never shows the chevron");

    probe.update(vcx, |p, cx| {
        p.add_terminal_pane();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(read(&probe, vcx, |p| p.show_chevron()), "a second pill flips the chevron on");
}

// ============================================================================
// edge fades gate on hidden pills (real bounds + scroll offset)
// ============================================================================

/// The leading / trailing edge fades gate on whether a pill is actually hidden
/// past that edge, driven by the real scroll offset over real per-item bounds:
/// parked at the leading edge only the trailing fade shows; scrolled to the middle
/// both show; scrolled fully right only the leading fade shows.
#[gpui::test]
fn edge_fades_gate_on_hidden_pills(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(5); // content 698 in a 404 viewport
    let (probe, vcx) = mount(cx, model, tab_id, OVERFLOW_TOOLBAR_W);

    let max = read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x));
    assert!(max > 0.0);

    // Parked at the leading edge (offset 0): nothing hidden left, pills hidden right.
    let geo = read(&probe, vcx, |p| p.strip_geometry());
    assert!(!geo.can_scroll_leading(), "parked left: no leading fade");
    assert!(geo.can_scroll_trailing(), "parked left: trailing fade shows");

    // Scrolled to the middle: both edges hide pills.
    probe.update(vcx, |p, _| p.scroll.set_offset(point(px(-max / 2.0), px(0.0))));
    vcx.run_until_parked();
    let geo = read(&probe, vcx, |p| p.strip_geometry());
    assert!(geo.can_scroll_leading(), "mid-scroll: leading fade shows");
    assert!(geo.can_scroll_trailing(), "mid-scroll: trailing fade shows");

    // Scrolled fully right: pills hidden left, nothing hidden right.
    probe.update(vcx, |p, _| p.scroll.set_offset(point(px(-max), px(0.0))));
    vcx.run_until_parked();
    let geo = read(&probe, vcx, |p| p.strip_geometry());
    assert!(geo.can_scroll_leading(), "scrolled right: leading fade shows");
    assert!(!geo.can_scroll_trailing(), "scrolled right: no trailing fade");
}

// ============================================================================
// attention badge: only for a FULLY-offscreen pane, driven via the model
// ============================================================================

/// The overflow chevron's attention badge lights only when a **fully-offscreen**
/// pane needs attention (status driven through the model, never a second
/// predicate): a Waiting-unacked Claude pane scrolled fully out lights it; the
/// same pane scrolled into view — or merely partially clipped — does not.
#[gpui::test]
fn badge_lights_only_for_a_fully_offscreen_attention_pane(cx: &mut TestAppContext) {
    // Four terminals then a trailing Claude pane, so at offset 0 the Claude pane
    // sits fully past the trailing edge.
    let (mut model, tab_id) = seed_terminals(4);
    {
        let (pi, ti) = model.project_tab_index(&tab_id).unwrap();
        model.projects[pi].tabs[ti]
            .panes
            .push(Pane::new("claude", "Claude", PaneKind::Claude));
    }
    let (probe, vcx) = mount(cx, model, tab_id, OVERFLOW_TOOLBAR_W);

    // Drive the Claude pane into Waiting-unacked THROUGH THE MODEL (needs_attention).
    probe.update(vcx, |p, _| {
        let (pi, ti) = p.model.project_tab_index(&p.tab_id).unwrap();
        let pane = p.projects_pane(pi, ti, "claude");
        pane.apply_status_transition(TabStatus::Waiting, false);
    });
    vcx.run_until_parked();

    // Fully offscreen (parked left): the Claude pane is in the offscreen set and
    // the badge lights.
    assert!(
        read(&probe, vcx, |p| p.offscreen_ids().contains("claude")),
        "the trailing Claude pane is fully offscreen at rest"
    );
    assert!(read(&probe, vcx, |p| p.has_offscreen_attention()), "badge lights for the offscreen attention pane");

    // Scroll it fully into view: no longer offscreen, badge dark.
    let max = read(&probe, vcx, |p| f32::from(p.scroll.max_offset().x));
    probe.update(vcx, |p, _| p.scroll.set_offset(point(px(-max), px(0.0))));
    vcx.run_until_parked();
    assert!(!read(&probe, vcx, |p| p.offscreen_ids().contains("claude")), "scrolled the Claude pane into view");
    assert!(
        !read(&probe, vcx, |p| p.has_offscreen_attention()),
        "a visible attention pane never badges the chevron"
    );

    // Partially clipped (straddling the trailing edge) also does not count: nudge
    // the offset so the Claude pane's leading edge sits just inside the viewport.
    probe.update(vcx, |p, _| {
        // Place the Claude pane so it straddles the trailing edge: its left edge a
        // little inside the viewport, its right edge past it.
        let ix = p.tab().panes.iter().position(|pane| pane.id == "claude").unwrap();
        let item_left = f32::from(p.scroll.bounds_for_item(ix).unwrap().origin.x);
        let viewport_left = f32::from(p.scroll.bounds().origin.x);
        let vw = f32::from(p.scroll.bounds().size.width);
        // want: item_left + offset - viewport_left == vw - PILL_W/2 (half in view)
        let offset = (vw - PILL_W / 2.0) - (item_left - viewport_left);
        p.scroll.set_offset(point(px(offset), px(0.0)));
    });
    vcx.run_until_parked();
    assert!(
        !read(&probe, vcx, |p| p.offscreen_ids().contains("claude")),
        "a partially-clipped pane is not in the offscreen set"
    );
    assert!(
        !read(&probe, vcx, |p| p.has_offscreen_attention()),
        "a partially-visible attention pane never badges the chevron"
    );
}

// ============================================================================
// ✕ slot reservation: hover keeps the pill width constant (bounds equality)
// ============================================================================

/// Hovering a pill reveals its ✕ but the slot was already reserved, so the pill's
/// laid-out bounds are byte-identical hovered vs not — the width never jumps.
#[gpui::test]
fn close_slot_reservation_keeps_pill_width_constant_across_hover(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(3);
    let (probe, vcx) = mount(cx, model, tab_id, WIDE_TOOLBAR_W);

    // p1 is not active and not hovered: its ✕ is hidden but the slot is reserved.
    let before = read(&probe, vcx, |p| p.pill_bounds("p1")).expect("p1 laid out");

    probe.update(vcx, |p, cx| {
        p.hovered_pane_id = Some("p1".to_string());
        cx.notify();
    });
    vcx.run_until_parked();
    let hovered = read(&probe, vcx, |p| p.pill_bounds("p1")).expect("p1 laid out");

    assert_eq!(before, hovered, "hover must not change the pill's laid-out bounds (the ✕ slot is reserved)");
    // And the width is exactly the reserved-slot-inclusive pill width.
    assert!((f32::from(hovered.size.width) - PILL_W).abs() < 0.5, "pill width is the full reserved anatomy");
}

// ============================================================================
// ✕ click closes without activating; a body click activates
// ============================================================================

/// A ✕ click closes its pane and, because the ✕ consumes the press, never
/// activates it: closing a non-active pane leaves the active pane put. A plain
/// body click on a different pane DOES activate it (the differential pair).
#[gpui::test]
fn close_click_closes_without_activating_and_body_click_activates(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(3); // p0 active
    let (probe, vcx) = mount(cx, model, tab_id, WIDE_TOOLBAR_W);
    assert_eq!(read(&probe, vcx, |p| p.active_pane_id()).as_deref(), Some("p0"));

    // Hover p2 so its ✕ is live, then click the ✕: p2 is closed and p0 stays active.
    probe.update(vcx, |p, cx| {
        p.hovered_pane_id = Some("p2".to_string());
        cx.notify();
    });
    vcx.run_until_parked();
    let x = close_center(&probe, vcx, "p2");
    vcx.simulate_click(x, Modifiers::none());

    assert_eq!(read(&probe, vcx, |p| p.pane_ids()), vec!["p0".to_string(), "p1".to_string()], "p2 is closed");
    assert_eq!(
        read(&probe, vcx, |p| p.active_pane_id()).as_deref(),
        Some("p0"),
        "closing a non-active pane must not move the active pane (the ✕ consumed the press)"
    );

    // A plain body click on p1 activates it.
    let b = pill_center(&probe, vcx, "p1");
    vcx.simulate_click(b, Modifiers::none());
    assert_eq!(read(&probe, vcx, |p| p.active_pane_id()).as_deref(), Some("p1"), "a body click activates the pill");
}

// ============================================================================
// empty rename commit resets to the per-kind auto-default (counter consumed)
// ============================================================================

/// Committing an EMPTY rename on a terminal pill resets its title to the next
/// "Terminal N" auto-default and consumes a counter slot — the R8
/// `rename_pane` empty-submit asymmetry (asymmetry 3), routed through the pill's
/// begin/commit path (the pill reimplements none of the policy).
#[gpui::test]
fn empty_rename_resets_to_terminal_auto_default_and_consumes_a_counter_slot(cx: &mut TestAppContext) {
    // Two seeded terminals → next_terminal_index == 3.
    let (model, tab_id) = seed_terminals(2);
    let (probe, vcx) = mount(cx, model, tab_id, WIDE_TOOLBAR_W);
    assert_eq!(read(&probe, vcx, |p| p.tab().next_terminal_index), 3);

    probe.update(vcx, |p, _| {
        p.begin_editing("p0");
        p.set_draft(""); // empty submit
        p.commit_rename();
    });
    vcx.run_until_parked();

    let title = read(&probe, vcx, |p| p.tab().panes[0].title.clone());
    assert_eq!(title, "Terminal 3", "empty submit resets to the next monotonic auto-default");
    assert_eq!(read(&probe, vcx, |p| p.tab().next_terminal_index), 4, "the counter slot was consumed");
    assert!(!read(&probe, vcx, |p| p.tab().panes[0].title_manually_set), "the manual-title lock was cleared");
}

/// A non-empty rename sets the title and locks it — the other half of the
/// asymmetry, so the empty-reset case above can't pass by coincidence.
#[gpui::test]
fn nonempty_rename_sets_and_locks_the_title(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(2);
    let (probe, vcx) = mount(cx, model, tab_id, WIDE_TOOLBAR_W);

    probe.update(vcx, |p, _| {
        p.begin_editing("p0");
        p.set_draft("build");
        p.commit_rename();
    });
    vcx.run_until_parked();

    assert_eq!(read(&probe, vcx, |p| p.tab().panes[0].title.clone()), "build");
    assert!(read(&probe, vcx, |p| p.tab().panes[0].title_manually_set), "a non-empty rename locks the title");
    assert_eq!(read(&probe, vcx, |p| p.tab().next_terminal_index), 3, "a non-empty rename does not touch the counter");
}

// ============================================================================
// activate-from-elsewhere centers the pill (offset math against bounds_for_item)
// ============================================================================

/// Activating a pill that is currently offscreen auto-scrolls it to centre: the
/// applied offset equals `center_offset_x` computed against the pill's real
/// `bounds_for_item`, and the once-offscreen pill is revealed (no longer in the
/// offscreen set).
#[gpui::test]
fn activate_from_elsewhere_centers_the_offscreen_pill(cx: &mut TestAppContext) {
    let (model, tab_id) = seed_terminals(6); // content 838 in a 404 viewport
    let (probe, vcx) = mount(cx, model, tab_id, OVERFLOW_TOOLBAR_W);

    // The last pill is fully offscreen at rest.
    assert!(read(&probe, vcx, |p| p.offscreen_ids().contains("p5")), "p5 starts offscreen");

    // Independently compute the expected centring offset from the real bounds.
    let expected = read(&probe, vcx, |p| {
        let ix = p.tab().panes.iter().position(|pane| pane.id == "p5").unwrap();
        let item = p.scroll.bounds_for_item(ix).unwrap();
        let vp = p.scroll.bounds();
        center_offset_x(
            f32::from(vp.origin.x),
            f32::from(vp.size.width),
            f32::from(item.origin.x),
            f32::from(item.size.width),
            f32::from(p.scroll.max_offset().x),
        )
    });

    // Activate p5 from elsewhere (it was not active) — the view auto-centers.
    probe.update(vcx, |p, _| p.select_pane("p5"));
    vcx.run_until_parked();

    let applied = read(&probe, vcx, |p| f32::from(p.scroll.offset().x));
    assert!((applied - expected).abs() < 0.5, "applied offset {applied} != center_offset_x {expected}");
    assert_eq!(read(&probe, vcx, |p| p.active_pane_id()).as_deref(), Some("p5"), "p5 is now active");
    assert!(
        !read(&probe, vcx, |p| p.offscreen_ids().contains("p5")),
        "centring revealed p5 — it is no longer offscreen"
    );
}

// ---- small helpers on the probe used only by tests --------------------------

impl PaneStripProbe {
    /// Mutable access to a pane by (project, tab) index + id — for driving status
    /// through the model in the badge case.
    fn projects_pane(&mut self, pi: usize, ti: usize, pane_id: &str) -> &mut Pane {
        self.model.projects[pi].tabs[ti]
            .panes
            .iter_mut()
            .find(|p| p.id == pane_id)
            .expect("pane exists")
    }
}
