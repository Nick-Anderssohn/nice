//! `PaneLayout` — one toolbar pill's split tree (tmux-port Phase 2).
//!
//! Vocabulary (roadmap Phase 2): a sidebar row is a [`crate::Session`], an
//! upper-bar pill is a [`crate::TermWindow`], and a **pane is one leaf of a
//! pill's split tree**. Pre-splits every pill is a single-leaf tree, which is
//! exactly what [`crate::TermWindow::new`] builds — so a user who never splits
//! sees no behavior change anywhere.
//!
//! Pure data, no `gpui` (crates/README.md "Layering rule"): the geometry here
//! is plain `f32` arithmetic over [`PaneRect`], and the render layer converts
//! to gpui pixels at the call site.
//!
//! ## Shape (plan decision P1 — binary tree)
//!
//! [`PaneLayout`] is a binary tree: `Leaf(Pane)` or `Split { orient, ratio,
//! first, second }`. Every split bisects exactly one leaf. tmux's n-ary layout
//! is equivalent in practice and a binary tree keeps geometry, resize, and
//! persistence simple.
//!
//! `ratio` is **`first`'s share** of the split's extent (after the divider is
//! subtracted) and is clamped to [`RATIO_MIN`]..=[`RATIO_MAX`] on every write.
//!
//! ## Orientation names (decision D2 — no "vertical"/"horizontal")
//!
//! [`SplitOrient::Beside`] = side-by-side (the `^⌘\` "Split Right" verb);
//! [`SplitOrient::Stacked`] = one above the other (the `^⌘-` "Split Down"
//! verb). vim and tmux assign "vertical"/"horizontal" opposite meanings, so
//! those words appear nowhere in this codebase's pane vocabulary.
//!
//! ## Pane identity (plan decision P2)
//!
//! Pane ids are plain `String`s, internal to this model and the pty map. They
//! never reach the frozen surfaces: `NICE_TAB_ID`/`NICE_PANE_ID` and the
//! control-socket `"tabId"`/`"paneId"` keys keep meaning session-id / **pill**
//! id. Every pane of a pill shares its pill's env, so socket traffic from any
//! pane still routes to the pill, which is where status lives.
//!
//! Ids must be unique across the whole window (the view cache keys on them),
//! not merely within one tree. [`crate::TermWindow::new`] seeds the sole leaf
//! with the window's own id — globally unique by construction — and split
//! panes are minted with fresh ids by `crates/nice`.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::term_window::TermWindowKind;

/// Smallest share a split's `first` child may hold. Every ratio write clamps
/// into [`RATIO_MIN`]..=[`RATIO_MAX`] so a tree can never carry a degenerate
/// (or `NaN`) ratio, whatever produced it — a dragged divider, a keyboard
/// resize step, or a hand-edited `sessions.json`.
pub const RATIO_MIN: f32 = 0.05;

/// Largest share a split's `first` child may hold — the mirror of
/// [`RATIO_MIN`].
pub const RATIO_MAX: f32 = 1.0 - RATIO_MIN;

/// How far apart two leaf rects may sit and still count as touching, in the
/// same px units as [`PaneLayout::leaf_rects`]' `bounds`. Neighbors are always
/// separated by exactly one divider width, so this only has to exceed the
/// divider (≈6 px) with room for float drift; it is deliberately far below any
/// plausible pane extent, so it can never make two non-neighbors adjacent.
pub const ADJACENCY_TOLERANCE: f32 = 16.0;

/// The stand-in extent [`crate::TermWindow::nominal_leaf_rects`] lays panes out
/// in when no painted size is available (a pane exiting in a background pill, a
/// keyboard focus move before the first paint).
///
/// Adjacency and shared-edge ranking are scale-invariant, so every
/// [`directional_neighbor`] / [`spatial_refocus`] answer computed here matches
/// the one the painted bounds would give. It is large relative to
/// [`ADJACENCY_TOLERANCE`] so deeply nested panes stay comfortably wider than
/// the tolerance.
pub const NOMINAL_EXTENT: f32 = 4096.0;

/// Which way a [`PaneLayout::Split`] divides its bounds.
///
/// D2 naming: no "vertical"/"horizontal" anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitOrient {
    /// Side by side — `first` is the left child, `second` the right one. The
    /// divider reads as `|`, which is why "Split Right" is `^⌘\`.
    Beside,
    /// One above the other — `first` is the top child, `second` the bottom
    /// one. The divider reads as `-`, which is why "Split Down" is `^⌘-`.
    Stacked,
}

/// A compass direction for the pane verbs: focus (`^⌘⇧hjkl`), resize
/// (`^⌥⌘hjkl`), and swap (`^⌥⌘⇧hjkl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneDirection {
    Left,
    Down,
    Up,
    Right,
}

impl PaneDirection {
    /// The split orientation this direction operates within — the one
    /// [`PaneLayout::resize`] hunts for when it walks to the nearest matching
    /// ancestor (P7).
    pub fn orient(self) -> SplitOrient {
        match self {
            PaneDirection::Left | PaneDirection::Right => SplitOrient::Beside,
            PaneDirection::Up | PaneDirection::Down => SplitOrient::Stacked,
        }
    }
}

/// Which child of a [`PaneLayout::Split`] a step descends into. A `Vec<Side>`
/// from the tree root names one node unambiguously — the identity the render
/// layer's divider drags hold on to, since a pane can exit mid-drag and
/// collapse the split out from under a raw reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    First,
    Second,
}

/// An axis-aligned rectangle in the render layer's px space. Local to this
/// crate because `nice-model` stays gpui-free; the strip's 1-D
/// [`crate::Rect`] is a different (horizontal-only) type and the two never
/// mix.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PaneRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PaneRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        PaneRect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn min_x(&self) -> f32 {
        self.x
    }

    pub fn max_x(&self) -> f32 {
        self.x + self.width
    }

    pub fn min_y(&self) -> f32 {
        self.y
    }

    pub fn max_y(&self) -> f32 {
        self.y + self.height
    }

    pub fn center_x(&self) -> f32 {
        self.x + self.width / 2.0
    }

    pub fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }

    /// Straight-line distance between the two rects' centers — the tie-break
    /// both [`directional_neighbor`] and [`spatial_refocus`] fall back on.
    pub fn center_distance(&self, other: &PaneRect) -> f32 {
        let dx = self.center_x() - other.center_x();
        let dy = self.center_y() - other.center_y();
        (dx * dx + dy * dy).sqrt()
    }
}

/// One leaf of a pill's split tree — a single terminal surface.
///
/// `kind` marks the (at most one) Claude leaf of a Claude pill; every other
/// leaf is [`TermWindowKind::Terminal`], because Nice never spawns Claude into
/// a split pane (decision D1). `cwd` mirrors [`crate::TermWindow::cwd`]'s
/// OSC-7-updated role per pane, so restore can put each pane back where it
/// was.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    pub kind: TermWindowKind,
    pub cwd: Option<String>,
    /// False once this pane's pty has exited and its view was HELD on screen so
    /// the user can read the scrollback. A pane whose pty exits cleanly leaves
    /// the tree outright, so this only ever marks a held corpse.
    ///
    /// Held-ness lives per leaf because a Claude pane can die while the shell
    /// panes beside it keep running: the pill stays alive, but
    /// `has_claude`-style predicates must stop counting the dead Claude.
    ///
    /// Runtime-only, never persisted — the same rule as
    /// [`crate::TermWindow::is_claude_running`]. `#[serde(skip)]` carries an
    /// explicit `default` because the derived `false` would restore every pane
    /// dead.
    #[serde(skip, default = "pane_alive_default")]
    pub is_alive: bool,
}

/// Serde shim for [`Pane::is_alive`]; see the field docs.
fn pane_alive_default() -> bool {
    true
}

impl Pane {
    /// A live pane with no recorded cwd — callers fall back to the window's or
    /// the session's cwd until OSC 7 lands.
    pub fn new(id: impl Into<String>, kind: TermWindowKind) -> Self {
        Pane {
            id: id.into(),
            kind,
            cwd: None,
            is_alive: true,
        }
    }

    /// Builder form of [`Pane::new`] for the split path, which spawns the new
    /// shell in the focused pane's cwd.
    pub fn with_cwd(mut self, cwd: Option<String>) -> Self {
        self.cwd = cwd;
        self
    }
}

/// One pill's split tree (P1). See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneLayout {
    Leaf(Pane),
    Split {
        orient: SplitOrient,
        /// `first`'s share of the split's extent, always inside
        /// [`RATIO_MIN`]..=[`RATIO_MAX`].
        ratio: f32,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}

impl PaneLayout {
    /// The one-leaf tree every pill starts as.
    pub fn single(pane: Pane) -> Self {
        PaneLayout::Leaf(pane)
    }

    // MARK: - Queries

    /// Every leaf in left-to-right / top-to-bottom tree order.
    pub fn leaves(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneLayout::Leaf(pane) => out.push(pane),
            PaneLayout::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    /// How many panes this pill hosts. Always ≥ 1 — the enum cannot represent
    /// an empty tree, which is what makes "a pill always has a pane" a
    /// type-level fact rather than an invariant to police.
    pub fn leaf_count(&self) -> usize {
        match self {
            PaneLayout::Leaf(_) => 1,
            PaneLayout::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// The sole pane when this pill has not been split, else `None`.
    pub fn single_leaf(&self) -> Option<&Pane> {
        match self {
            PaneLayout::Leaf(pane) => Some(pane),
            PaneLayout::Split { .. } => None,
        }
    }

    /// Whether `pane_id` names a leaf of this tree.
    pub fn contains(&self, pane_id: &str) -> bool {
        self.pane(pane_id).is_some()
    }

    /// The leaf payload for `pane_id`.
    pub fn pane(&self, pane_id: &str) -> Option<&Pane> {
        self.leaves().into_iter().find(|p| p.id == pane_id)
    }

    /// Mutable leaf payload for `pane_id` — the OSC-7 cwd writer's seam.
    pub fn pane_mut(&mut self, pane_id: &str) -> Option<&mut Pane> {
        match self {
            PaneLayout::Leaf(pane) => (pane.id == pane_id).then_some(pane),
            PaneLayout::Split { first, second, .. } => {
                if let Some(found) = first.pane_mut(pane_id) {
                    return Some(found);
                }
                second.pane_mut(pane_id)
            }
        }
    }

    /// The Claude leaf, if this tree has one. At most one exists (see
    /// [`crate::TermWindow::layout_is_valid`]).
    pub fn claude_leaf(&self) -> Option<&Pane> {
        self.leaves()
            .into_iter()
            .find(|p| p.kind == TermWindowKind::Claude)
    }

    /// Path from the root to `pane_id`'s leaf. Empty when the root *is* that
    /// leaf.
    pub fn path_to_pane(&self, pane_id: &str) -> Option<Vec<Side>> {
        let mut path = Vec::new();
        self.build_path(pane_id, &mut path).then_some(path)
    }

    fn build_path(&self, pane_id: &str, path: &mut Vec<Side>) -> bool {
        match self {
            PaneLayout::Leaf(pane) => pane.id == pane_id,
            PaneLayout::Split { first, second, .. } => {
                path.push(Side::First);
                if first.build_path(pane_id, path) {
                    return true;
                }
                path.pop();
                path.push(Side::Second);
                if second.build_path(pane_id, path) {
                    return true;
                }
                path.pop();
                false
            }
        }
    }

    /// The node a [`path_to_pane`](Self::path_to_pane)-shaped path names.
    /// `None` when the path ran off the tree — which is exactly what a stale
    /// divider-drag path does after a pane exited and collapsed its split, so
    /// the caller ends the drag as a no-op instead of writing the wrong node.
    pub fn node_at(&self, path: &[Side]) -> Option<&PaneLayout> {
        let mut node = self;
        for side in path {
            let PaneLayout::Split { first, second, .. } = node else {
                return None;
            };
            node = match side {
                Side::First => first,
                Side::Second => second,
            };
        }
        Some(node)
    }

    /// Mutable [`node_at`](Self::node_at).
    pub fn node_at_mut(&mut self, path: &[Side]) -> Option<&mut PaneLayout> {
        let mut node = self;
        for side in path {
            let PaneLayout::Split { first, second, .. } = node else {
                return None;
            };
            node = match side {
                Side::First => first,
                Side::Second => second,
            };
        }
        Some(node)
    }

    // MARK: - Mutations

    /// Bisect `target_pane_id`'s leaf, putting `new_pane` down/right of it
    /// (matching D2's "Split Down"/"Split Right" verbs) at an even ratio.
    ///
    /// Refuses (and changes nothing) when the target isn't in this tree or
    /// when `new_pane.id` already is — pane ids must stay unique.
    pub fn split(&mut self, target_pane_id: &str, orient: SplitOrient, new_pane: Pane) -> bool {
        if self.contains(&new_pane.id) {
            return false;
        }
        let Some(path) = self.path_to_pane(target_pane_id) else {
            return false;
        };
        let Some(node) = self.node_at_mut(&path) else {
            return false;
        };
        let existing = std::mem::replace(node, PaneLayout::Leaf(new_pane.clone()));
        *node = PaneLayout::Split {
            orient,
            ratio: 0.5,
            first: Box::new(existing),
            second: Box::new(PaneLayout::Leaf(new_pane)),
        };
        true
    }

    /// Remove `pane_id`'s leaf, collapsing its parent split into the sibling
    /// subtree. Returns the removed payload.
    ///
    /// Refuses on the last leaf (`None`): a pill without a pane is not
    /// representable, and closing the last pane is a *pill* close, which the
    /// pty layer routes through the existing `window_exited` flow instead.
    pub fn remove(&mut self, pane_id: &str) -> Option<Pane> {
        remove_in(self, pane_id)
    }

    /// Swap two leaves' payloads in place (P8) — structure and ratios don't
    /// move, so the panes trade rects and nothing reflows.
    pub fn swap(&mut self, a_id: &str, b_id: &str) -> bool {
        if a_id == b_id {
            return false;
        }
        let (Some(a_path), Some(b_path)) = (self.path_to_pane(a_id), self.path_to_pane(b_id))
        else {
            return false;
        };
        let (Some(a), Some(b)) = (self.pane(a_id).cloned(), self.pane(b_id).cloned()) else {
            return false;
        };
        // Both paths are resolved before either write, so the second lookup
        // can't land on the slot the first one just rewrote.
        if let Some(PaneLayout::Leaf(slot)) = self.node_at_mut(&a_path) {
            *slot = b;
        }
        if let Some(PaneLayout::Leaf(slot)) = self.node_at_mut(&b_path) {
            *slot = a;
        }
        true
    }

    /// Move the divider of the nearest ancestor split whose orientation
    /// matches `direction` (P7 — tmux `resize-pane -L/-D/-U/-R`).
    ///
    /// `delta` is a magnitude (its sign is ignored); `direction` decides which
    /// way the divider travels. Left/Up shrink the ratio, Right/Down grow it —
    /// which, in a binary split, moves the focused pane's own edge that way
    /// whichever child it sits in.
    ///
    /// `min_ratio` is the caller's px-derived floor — P6's
    /// `PANE_MIN_WIDTH`/`PANE_MIN_HEIGHT` enforcement needs painted bounds,
    /// which a gpui-free crate cannot see, so the px→ratio conversion happens
    /// in `crates/nice` and only the resulting band arrives here. It is itself
    /// clamped into the model's own band. Returns whether the ratio actually
    /// moved — `false` for no matching ancestor, an unknown pane, or a divider
    /// already pinned at the clamp.
    pub fn resize(
        &mut self,
        focused_pane_id: &str,
        direction: PaneDirection,
        delta: f32,
        min_ratio: f32,
    ) -> bool {
        let Some(path) = self.resize_target_path(focused_pane_id, direction) else {
            return false;
        };
        let signed = match direction {
            PaneDirection::Left | PaneDirection::Up => -delta.abs(),
            PaneDirection::Right | PaneDirection::Down => delta.abs(),
        };
        let lo = clamp_ratio(min_ratio.max(RATIO_MIN));
        let hi = 1.0 - lo;
        let Some(PaneLayout::Split { ratio, .. }) = self.node_at_mut(&path) else {
            return false;
        };
        let next = (*ratio + signed).clamp(lo, hi);
        if next == *ratio {
            return false;
        }
        *ratio = next;
        true
    }

    /// Which split [`resize`](Self::resize) would move for this pane and
    /// direction: the nearest ancestor whose orientation matches (P7), named by
    /// its path from the root. `None` when the pane is unknown or no ancestor
    /// matches — the "resize is a no-op here" answer.
    ///
    /// Public because the px→ratio conversion happens at the call site in
    /// `crates/nice` (a gpui-free crate cannot see painted bounds): the caller
    /// needs the SAME node this walk picks in order to measure the px its ratio
    /// divides, and one shared walker is what keeps the two from drifting.
    pub fn resize_target_path(
        &self,
        focused_pane_id: &str,
        direction: PaneDirection,
    ) -> Option<Vec<Side>> {
        let path = self.path_to_pane(focused_pane_id)?;
        let want = direction.orient();
        // The deepest split above the leaf is the longest proper prefix of the
        // leaf's path, so walking prefixes from the longest down visits
        // ancestors nearest-first.
        for len in (0..path.len()).rev() {
            let prefix = &path[..len];
            if let Some(PaneLayout::Split { orient, .. }) = self.node_at(prefix) {
                if *orient == want {
                    return Some(prefix.to_vec());
                }
            }
        }
        None
    }

    /// Force `path`'s split to `ratio`, clamped into
    /// [`RATIO_MIN`]..=[`RATIO_MAX`] — the divider-drag and double-click-reset
    /// writer. Returns whether a split was actually written.
    pub fn set_ratio_at(&mut self, path: &[Side], ratio: f32) -> bool {
        let clamped = clamp_ratio(ratio);
        match self.node_at_mut(path) {
            Some(PaneLayout::Split { ratio, .. }) => {
                *ratio = clamped;
                true
            }
            _ => false,
        }
    }

    /// Clamp every ratio in the tree into the legal band, replacing `NaN` with
    /// an even split. Applied on hydrate so a hand-edited or corrupted
    /// `sessions.json` cannot put a degenerate ratio into the render layer.
    pub fn normalize_ratios(&mut self) {
        if let PaneLayout::Split {
            ratio,
            first,
            second,
            ..
        } = self
        {
            *ratio = clamp_ratio(*ratio);
            first.normalize_ratios();
            second.normalize_ratios();
        }
    }

    // MARK: - Geometry

    /// Assign every leaf its rect inside `bounds`, reserving `divider_px`
    /// between the two children of each split.
    ///
    /// The one arithmetic shared by render, hit-testing, directional focus,
    /// swap, and close-refocus — so what the user sees and what the keyboard
    /// walks can never disagree.
    pub fn leaf_rects(&self, bounds: PaneRect, divider_px: f32) -> Vec<(String, PaneRect)> {
        let mut out = Vec::new();
        self.collect_rects(bounds, divider_px.max(0.0), &mut out);
        out
    }

    fn collect_rects(
        &self,
        bounds: PaneRect,
        divider_px: f32,
        out: &mut Vec<(String, PaneRect)>,
    ) {
        match self {
            PaneLayout::Leaf(pane) => out.push((pane.id.clone(), bounds)),
            PaneLayout::Split {
                orient,
                ratio,
                first,
                second,
            } => {
                let ratio = clamp_ratio(*ratio);
                match orient {
                    SplitOrient::Beside => {
                        let available = (bounds.width - divider_px).max(0.0);
                        let first_w = available * ratio;
                        let second_w = available - first_w;
                        first.collect_rects(
                            PaneRect::new(bounds.x, bounds.y, first_w, bounds.height),
                            divider_px,
                            out,
                        );
                        second.collect_rects(
                            PaneRect::new(
                                bounds.x + first_w + divider_px,
                                bounds.y,
                                second_w,
                                bounds.height,
                            ),
                            divider_px,
                            out,
                        );
                    }
                    SplitOrient::Stacked => {
                        let available = (bounds.height - divider_px).max(0.0);
                        let first_h = available * ratio;
                        let second_h = available - first_h;
                        first.collect_rects(
                            PaneRect::new(bounds.x, bounds.y, bounds.width, first_h),
                            divider_px,
                            out,
                        );
                        second.collect_rects(
                            PaneRect::new(
                                bounds.x,
                                bounds.y + first_h + divider_px,
                                bounds.width,
                                second_h,
                            ),
                            divider_px,
                            out,
                        );
                    }
                }
            }
        }
    }
}

/// Clamp a ratio into [`RATIO_MIN`]..=[`RATIO_MAX`]; `NaN` becomes an even
/// split.
fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        return 0.5;
    }
    ratio.clamp(RATIO_MIN, RATIO_MAX)
}

fn remove_in(node: &mut PaneLayout, pane_id: &str) -> Option<Pane> {
    let (first_hit, second_hit) = match node {
        PaneLayout::Leaf(_) => return None,
        PaneLayout::Split { first, second, .. } => (
            matches!(first.as_ref(), PaneLayout::Leaf(p) if p.id == pane_id),
            matches!(second.as_ref(), PaneLayout::Leaf(p) if p.id == pane_id),
        ),
    };

    if first_hit || second_hit {
        // Take the split by value so the surviving sibling subtree can be
        // hoisted into its parent's slot.
        let owned = std::mem::replace(
            node,
            PaneLayout::Leaf(Pane::new("", TermWindowKind::Terminal)),
        );
        let PaneLayout::Split { first, second, .. } = owned else {
            unreachable!("checked above");
        };
        let (removed, sibling) = if first_hit {
            (*first, *second)
        } else {
            (*second, *first)
        };
        *node = sibling;
        let PaneLayout::Leaf(pane) = removed else {
            unreachable!("only leaves match pane_id");
        };
        return Some(pane);
    }

    let PaneLayout::Split { first, second, .. } = node else {
        return None;
    };
    if let Some(pane) = remove_in(first, pane_id) {
        return Some(pane);
    }
    remove_in(second, pane_id)
}

/// The leaf adjacent to `from_id` in `direction`, or `None` at the edge of the
/// tree.
///
/// Ranking, in order: the smallest gap between the two rects' facing edges
/// (the panes that actually border `from_id`), then the largest shared edge
/// overlap, then the shortest center-to-center distance. The gap term is what
/// keeps a wide far-away pane from out-scoring the narrow pane that is really
/// next door.
///
/// Edges do **not** wrap and do not fall through to pill navigation (P5) —
/// bare `^⌘h/l` is how the user leaves a pill.
pub fn directional_neighbor(
    rects: &[(String, PaneRect)],
    from_id: &str,
    direction: PaneDirection,
) -> Option<String> {
    let from = rects
        .iter()
        .find(|(id, _)| id == from_id)
        .map(|(_, r)| *r)?;

    let mut best: Option<(&str, (f32, f32, f32))> = None;
    for (id, rect) in rects {
        if id == from_id {
            continue;
        }
        let (gap, overlap) = match direction {
            PaneDirection::Right => (
                rect.min_x() - from.max_x(),
                edge_overlap(from.min_y(), from.max_y(), rect.min_y(), rect.max_y()),
            ),
            PaneDirection::Left => (
                from.min_x() - rect.max_x(),
                edge_overlap(from.min_y(), from.max_y(), rect.min_y(), rect.max_y()),
            ),
            PaneDirection::Down => (
                rect.min_y() - from.max_y(),
                edge_overlap(from.min_x(), from.max_x(), rect.min_x(), rect.max_x()),
            ),
            PaneDirection::Up => (
                from.min_y() - rect.max_y(),
                edge_overlap(from.min_x(), from.max_x(), rect.min_x(), rect.max_x()),
            ),
        };
        // Strictly on that side (tolerating the divider + float drift), and
        // actually sharing some of the perpendicular edge.
        if gap < -ADJACENCY_TOLERANCE || overlap <= 0.0 {
            continue;
        }
        let key = (gap.max(0.0), -overlap, from.center_distance(rect));
        if best
            .as_ref()
            .is_none_or(|(_, best_key)| rank_less(key, *best_key))
        {
            best = Some((id, key));
        }
    }
    best.map(|(id, _)| id.to_string())
}

/// Which surviving leaf takes focus after `closed_id`'s pane goes away —
/// spatial, not index-neighbor.
///
/// `rects_before_close` is the layout as it stood *with* the closed pane, so
/// the closed rect is still there to compare against. The winner is the leaf
/// sharing the longest border with it; when nothing borders it (a degenerate
/// or already-collapsed layout) the nearest center wins.
///
/// Pill closes are untouched by this — they keep
/// [`crate::WorkspaceModel::neighbor_active_window_id`]'s index semantics.
pub fn spatial_refocus(rects_before_close: &[(String, PaneRect)], closed_id: &str) -> Option<String> {
    let closed = rects_before_close
        .iter()
        .find(|(id, _)| id == closed_id)
        .map(|(_, r)| *r)?;

    let mut best: Option<(&str, (f32, f32))> = None;
    for (id, rect) in rects_before_close {
        if id == closed_id {
            continue;
        }
        let shared = shared_edge(&closed, rect);
        let key = (-shared, closed.center_distance(rect));
        if best
            .as_ref()
            .is_none_or(|(_, best_key)| rank_less2(key, *best_key))
        {
            best = Some((id, key));
        }
    }
    best.map(|(id, _)| id.to_string())
}

/// Length of the border two rects share, 0.0 when they don't touch. Rects a
/// divider apart still count as touching ([`ADJACENCY_TOLERANCE`]).
fn shared_edge(a: &PaneRect, b: &PaneRect) -> f32 {
    let mut best: f32 = 0.0;
    let touches_x = (b.min_x() - a.max_x()).abs() <= ADJACENCY_TOLERANCE
        || (a.min_x() - b.max_x()).abs() <= ADJACENCY_TOLERANCE;
    if touches_x {
        best = best.max(edge_overlap(a.min_y(), a.max_y(), b.min_y(), b.max_y()));
    }
    let touches_y = (b.min_y() - a.max_y()).abs() <= ADJACENCY_TOLERANCE
        || (a.min_y() - b.max_y()).abs() <= ADJACENCY_TOLERANCE;
    if touches_y {
        best = best.max(edge_overlap(a.min_x(), a.max_x(), b.min_x(), b.max_x()));
    }
    best
}

/// Overlap of two 1-D intervals, clamped at 0.
fn edge_overlap(a_min: f32, a_max: f32, b_min: f32, b_max: f32) -> f32 {
    (a_max.min(b_max) - a_min.max(b_min)).max(0.0)
}

fn rank_less(a: (f32, f32, f32), b: (f32, f32, f32)) -> bool {
    match a.0.total_cmp(&b.0) {
        Ordering::Less => return true,
        Ordering::Greater => return false,
        Ordering::Equal => {}
    }
    match a.1.total_cmp(&b.1) {
        Ordering::Less => return true,
        Ordering::Greater => return false,
        Ordering::Equal => {}
    }
    a.2.total_cmp(&b.2) == Ordering::Less
}

fn rank_less2(a: (f32, f32), b: (f32, f32)) -> bool {
    match a.0.total_cmp(&b.0) {
        Ordering::Less => return true,
        Ordering::Greater => return false,
        Ordering::Equal => {}
    }
    a.1.total_cmp(&b.1) == Ordering::Less
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(id: &str) -> Pane {
        Pane::new(id, TermWindowKind::Terminal)
    }
    fn claude(id: &str) -> Pane {
        Pane::new(id, TermWindowKind::Claude)
    }
    fn ids(layout: &PaneLayout) -> Vec<String> {
        layout.leaves().into_iter().map(|p| p.id.clone()).collect()
    }
    /// `bounds` used by every geometry test: a 1000x600 content area.
    fn bounds() -> PaneRect {
        PaneRect::new(0.0, 0.0, 1000.0, 600.0)
    }
    fn rect_of(rects: &[(String, PaneRect)], id: &str) -> PaneRect {
        rects
            .iter()
            .find(|(i, _)| i == id)
            .unwrap_or_else(|| panic!("no rect for {id}"))
            .1
    }

    // MARK: - split

    #[test]
    fn single_leaf_tree_is_the_pre_splits_shape() {
        let layout = PaneLayout::single(term("a"));
        assert_eq!(layout.leaf_count(), 1);
        assert_eq!(layout.single_leaf().map(|p| p.id.as_str()), Some("a"));
        assert!(layout.contains("a"));
        assert!(!layout.contains("b"));
    }

    #[test]
    fn split_puts_the_new_pane_second_at_an_even_ratio() {
        let mut layout = PaneLayout::single(term("a"));
        assert!(layout.split("a", SplitOrient::Beside, term("b")));

        assert_eq!(ids(&layout), vec!["a", "b"], "new pane lands right/down");
        match &layout {
            PaneLayout::Split { orient, ratio, .. } => {
                assert_eq!(*orient, SplitOrient::Beside);
                assert_eq!(*ratio, 0.5);
            }
            _ => panic!("expected a split"),
        }
        assert!(layout.single_leaf().is_none());
    }

    #[test]
    fn split_bisects_only_the_target_leaf() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(layout.split("b", SplitOrient::Stacked, term("c")));

        assert_eq!(ids(&layout), vec!["a", "b", "c"]);
        // `a` is untouched at the top level; the stacked split lives under `second`.
        let second = layout.node_at(&[Side::Second]).unwrap();
        assert!(matches!(
            second,
            PaneLayout::Split {
                orient: SplitOrient::Stacked,
                ..
            }
        ));
    }

    #[test]
    fn split_refuses_unknown_target_and_duplicate_id() {
        let mut layout = PaneLayout::single(term("a"));
        assert!(!layout.split("nope", SplitOrient::Beside, term("b")));
        assert!(!layout.split("a", SplitOrient::Beside, term("a")));
        assert_eq!(layout.leaf_count(), 1, "a refused split changes nothing");
    }

    // MARK: - remove

    #[test]
    fn remove_collapses_the_parent_split_into_the_sibling() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));

        let removed = layout.remove("a").expect("removes a leaf");
        assert_eq!(removed.id, "a");
        assert_eq!(
            layout.single_leaf().map(|p| p.id.as_str()),
            Some("b"),
            "the sibling is hoisted into the split's slot"
        );
    }

    #[test]
    fn remove_hoists_a_whole_sibling_subtree() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.split("b", SplitOrient::Stacked, term("c"));

        layout.remove("a");
        assert_eq!(ids(&layout), vec!["b", "c"]);
        assert!(matches!(
            layout,
            PaneLayout::Split {
                orient: SplitOrient::Stacked,
                ..
            }
        ));
    }

    #[test]
    fn remove_refuses_the_last_leaf() {
        let mut layout = PaneLayout::single(term("a"));
        assert!(
            layout.remove("a").is_none(),
            "closing the last pane is a PILL close, routed through window_exited"
        );
        assert_eq!(layout.leaf_count(), 1);
    }

    #[test]
    fn remove_of_an_unknown_pane_is_a_noop() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(layout.remove("zzz").is_none());
        assert_eq!(layout.leaf_count(), 2);
    }

    // MARK: - swap (P8)

    #[test]
    fn swap_trades_payloads_and_leaves_structure_alone() {
        let mut layout = PaneLayout::single(claude("c"));
        layout.split("c", SplitOrient::Beside, term("s"));
        layout.set_ratio_at(&[], 0.7);

        assert!(layout.swap("c", "s"));
        assert_eq!(ids(&layout), vec!["s", "c"], "payloads traded places");
        match &layout {
            PaneLayout::Split { ratio, orient, .. } => {
                assert_eq!(*ratio, 0.7, "ratios don't move (P8)");
                assert_eq!(*orient, SplitOrient::Beside);
            }
            _ => panic!("expected a split"),
        }
        assert_eq!(
            layout.claude_leaf().map(|p| p.id.as_str()),
            Some("c"),
            "the Claude payload moved with its pane, not with the slot"
        );
    }

    #[test]
    fn swap_across_nested_splits() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.split("b", SplitOrient::Stacked, term("c"));

        assert!(layout.swap("a", "c"));
        assert_eq!(ids(&layout), vec!["c", "b", "a"]);
    }

    #[test]
    fn swap_refuses_self_and_unknown_panes() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(!layout.swap("a", "a"));
        assert!(!layout.swap("a", "zzz"));
        assert_eq!(ids(&layout), vec!["a", "b"]);
    }

    // MARK: - resize (P7) + ratio clamping

    #[test]
    fn resize_moves_the_divider_the_same_way_from_either_child() {
        // `first` child: "resize left" shrinks its share.
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(layout.resize("a", PaneDirection::Left, 0.1, RATIO_MIN));
        let left_from_first = match &layout {
            PaneLayout::Split { ratio, .. } => *ratio,
            _ => panic!(),
        };

        // `second` child: same chord, same divider travel.
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(layout.resize("b", PaneDirection::Left, 0.1, RATIO_MIN));
        let left_from_second = match &layout {
            PaneLayout::Split { ratio, .. } => *ratio,
            _ => panic!(),
        };

        assert_eq!(left_from_first, left_from_second);
        assert!(
            left_from_first < 0.5,
            "resize-left always walks the divider left (tmux resize-pane -L)"
        );
    }

    #[test]
    fn resize_right_grows_the_first_share() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(layout.resize("a", PaneDirection::Right, 0.2, RATIO_MIN));
        match &layout {
            PaneLayout::Split { ratio, .. } => assert!((*ratio - 0.7).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn resize_picks_the_nearest_matching_ancestor() {
        // Beside{ a, Stacked{ b, c } } — resizing `b` left must reach past its
        // Stacked parent to the Beside grandparent.
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.split("b", SplitOrient::Stacked, term("c"));

        assert!(layout.resize("b", PaneDirection::Left, 0.1, RATIO_MIN));
        match &layout {
            PaneLayout::Split { ratio, .. } => assert!((*ratio - 0.4).abs() < 1e-6),
            _ => panic!(),
        }
        // The Stacked split is what `up`/`down` reaches.
        assert!(layout.resize("b", PaneDirection::Down, 0.1, RATIO_MIN));
        match layout.node_at(&[Side::Second]).unwrap() {
            PaneLayout::Split { ratio, .. } => assert!((*ratio - 0.6).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn resize_target_path_names_the_node_resize_moves() {
        // Beside{ a, Stacked{ b, c } } again: left/right reach the root, up/down
        // the inner Stacked split, and a direction with no matching ancestor
        // reports None — the same three answers `resize` acts on, exposed so the
        // px→ratio conversion in `crates/nice` measures the very same node.
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.split("b", SplitOrient::Stacked, term("c"));

        assert_eq!(
            layout.resize_target_path("b", PaneDirection::Left),
            Some(vec![])
        );
        assert_eq!(
            layout.resize_target_path("b", PaneDirection::Down),
            Some(vec![Side::Second])
        );
        assert_eq!(layout.resize_target_path("a", PaneDirection::Up), None);
        assert_eq!(layout.resize_target_path("zzz", PaneDirection::Left), None);
    }

    #[test]
    fn resize_without_a_matching_ancestor_is_a_noop() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert!(
            !layout.resize("a", PaneDirection::Up, 0.1, RATIO_MIN),
            "no Stacked ancestor → no-op (P7)"
        );
        assert!(!layout.resize("nope", PaneDirection::Left, 0.1, RATIO_MIN));
    }

    #[test]
    fn resize_clamps_at_the_caller_supplied_floor_and_then_refuses() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));

        // A big step clamps rather than overshooting.
        assert!(layout.resize("a", PaneDirection::Left, 5.0, 0.2));
        match &layout {
            PaneLayout::Split { ratio, .. } => assert!((*ratio - 0.2).abs() < 1e-6),
            _ => panic!(),
        }
        // Pinned at the clamp, another step reports "nothing moved".
        assert!(!layout.resize("a", PaneDirection::Left, 5.0, 0.2));
    }

    #[test]
    fn set_ratio_and_normalize_keep_ratios_in_band() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));

        assert!(layout.set_ratio_at(&[], 9.0));
        match &layout {
            PaneLayout::Split { ratio, .. } => assert_eq!(*ratio, RATIO_MAX),
            _ => panic!(),
        }
        assert!(!layout.set_ratio_at(&[Side::First], 0.5), "a leaf has no ratio");

        if let PaneLayout::Split { ratio, .. } = &mut layout {
            *ratio = f32::NAN;
        }
        layout.normalize_ratios();
        match &layout {
            PaneLayout::Split { ratio, .. } => assert_eq!(*ratio, 0.5, "NaN becomes an even split"),
            _ => panic!(),
        }
    }

    // MARK: - leaf_rects

    #[test]
    fn leaf_rects_of_a_single_leaf_is_the_whole_bounds() {
        let layout = PaneLayout::single(term("a"));
        let rects = layout.leaf_rects(bounds(), 6.0);
        assert_eq!(rects.len(), 1);
        assert_eq!(rect_of(&rects, "a"), bounds());
    }

    #[test]
    fn leaf_rects_reserves_divider_space_beside() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        let rects = layout.leaf_rects(bounds(), 6.0);

        let a = rect_of(&rects, "a");
        let b = rect_of(&rects, "b");
        assert_eq!(a, PaneRect::new(0.0, 0.0, 497.0, 600.0));
        assert_eq!(b, PaneRect::new(503.0, 0.0, 497.0, 600.0));
        assert_eq!(
            b.min_x() - a.max_x(),
            6.0,
            "exactly one divider between siblings"
        );
        assert_eq!(a.width + b.width + 6.0, bounds().width);
    }

    #[test]
    fn leaf_rects_reserves_divider_space_stacked() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Stacked, term("b"));
        let rects = layout.leaf_rects(bounds(), 6.0);

        let a = rect_of(&rects, "a");
        let b = rect_of(&rects, "b");
        assert_eq!(a, PaneRect::new(0.0, 0.0, 1000.0, 297.0));
        assert_eq!(b, PaneRect::new(0.0, 303.0, 1000.0, 297.0));
    }

    #[test]
    fn leaf_rects_handles_nested_mixed_orientations() {
        // Beside{ a, Stacked{ b, c } } at an uneven top ratio.
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.split("b", SplitOrient::Stacked, term("c"));
        layout.set_ratio_at(&[], 0.25);

        let rects = layout.leaf_rects(PaneRect::new(0.0, 0.0, 1006.0, 606.0), 6.0);
        let a = rect_of(&rects, "a");
        let b = rect_of(&rects, "b");
        let c = rect_of(&rects, "c");

        assert_eq!(a, PaneRect::new(0.0, 0.0, 250.0, 606.0));
        assert_eq!(b, PaneRect::new(256.0, 0.0, 750.0, 300.0));
        assert_eq!(c, PaneRect::new(256.0, 306.0, 750.0, 300.0));
        assert_eq!(
            b.min_x(),
            c.min_x(),
            "both right-column panes share the column's left edge"
        );
    }

    #[test]
    fn leaf_rects_never_goes_negative_on_tiny_bounds() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        let rects = layout.leaf_rects(PaneRect::new(0.0, 0.0, 2.0, 10.0), 6.0);
        for (_, rect) in &rects {
            assert!(rect.width >= 0.0 && rect.height >= 0.0);
        }
    }

    // MARK: - directional_neighbor

    /// Three equal columns: a | b | c.
    fn three_columns() -> Vec<(String, PaneRect)> {
        vec![
            ("a".to_string(), PaneRect::new(0.0, 0.0, 300.0, 600.0)),
            ("b".to_string(), PaneRect::new(306.0, 0.0, 300.0, 600.0)),
            ("c".to_string(), PaneRect::new(612.0, 0.0, 300.0, 600.0)),
        ]
    }

    #[test]
    fn directional_neighbor_walks_one_column_at_a_time() {
        let rects = three_columns();
        assert_eq!(
            directional_neighbor(&rects, "a", PaneDirection::Right).as_deref(),
            Some("b"),
            "the adjacent column wins over the equally-overlapping far one"
        );
        assert_eq!(
            directional_neighbor(&rects, "c", PaneDirection::Left).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn directional_neighbor_no_ops_at_the_edge() {
        let rects = three_columns();
        assert_eq!(directional_neighbor(&rects, "a", PaneDirection::Left), None);
        assert_eq!(directional_neighbor(&rects, "c", PaneDirection::Right), None);
        assert_eq!(
            directional_neighbor(&rects, "a", PaneDirection::Up),
            None,
            "no wrap, no fall-through to pill nav (P5)"
        );
    }

    #[test]
    fn directional_neighbor_prefers_the_bordering_pane_over_a_wider_distant_one() {
        // a | (b over c) | d — `d` overlaps ALL of `a`'s edge, but `b`/`c`
        // are the panes actually next to `a`.
        let rects = vec![
            ("a".to_string(), PaneRect::new(0.0, 0.0, 300.0, 600.0)),
            ("b".to_string(), PaneRect::new(306.0, 0.0, 200.0, 297.0)),
            ("c".to_string(), PaneRect::new(306.0, 303.0, 200.0, 297.0)),
            ("d".to_string(), PaneRect::new(512.0, 0.0, 300.0, 600.0)),
        ];
        let picked = directional_neighbor(&rects, "a", PaneDirection::Right);
        assert!(
            matches!(picked.as_deref(), Some("b") | Some("c")),
            "adjacency beats raw overlap; got {picked:?}"
        );
    }

    #[test]
    fn directional_neighbor_ranks_by_shared_edge_then_center_distance() {
        // `a` spans the left column; `b` shares most of its edge, `c` only a
        // sliver — both are equally adjacent.
        let rects = vec![
            ("a".to_string(), PaneRect::new(0.0, 0.0, 300.0, 600.0)),
            ("b".to_string(), PaneRect::new(306.0, 0.0, 300.0, 500.0)),
            ("c".to_string(), PaneRect::new(306.0, 506.0, 300.0, 94.0)),
        ];
        assert_eq!(
            directional_neighbor(&rects, "a", PaneDirection::Right).as_deref(),
            Some("b"),
            "largest shared edge wins among equally adjacent panes"
        );

        // Equal overlap → the nearer center wins.
        let rects = vec![
            ("a".to_string(), PaneRect::new(0.0, 0.0, 300.0, 200.0)),
            ("b".to_string(), PaneRect::new(306.0, 0.0, 300.0, 200.0)),
            ("c".to_string(), PaneRect::new(306.0, 0.0, 900.0, 200.0)),
        ];
        assert_eq!(
            directional_neighbor(&rects, "a", PaneDirection::Right).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn directional_neighbor_walks_up_and_down_rows() {
        let rects = vec![
            ("top".to_string(), PaneRect::new(0.0, 0.0, 1000.0, 297.0)),
            ("bottom".to_string(), PaneRect::new(0.0, 303.0, 1000.0, 297.0)),
        ];
        assert_eq!(
            directional_neighbor(&rects, "top", PaneDirection::Down).as_deref(),
            Some("bottom")
        );
        assert_eq!(
            directional_neighbor(&rects, "bottom", PaneDirection::Up).as_deref(),
            Some("top")
        );
        assert_eq!(directional_neighbor(&rects, "top", PaneDirection::Up), None);
    }

    #[test]
    fn directional_neighbor_of_an_unknown_pane_is_none() {
        assert_eq!(
            directional_neighbor(&three_columns(), "zzz", PaneDirection::Right),
            None
        );
    }

    #[test]
    fn directional_neighbor_agrees_with_leaf_rects() {
        // The whole point of sharing one geometry: the tree the user sees and
        // the tree the keyboard walks are the same tree.
        let mut layout = PaneLayout::single(claude("c"));
        layout.split("c", SplitOrient::Beside, term("s1"));
        layout.split("s1", SplitOrient::Stacked, term("s2"));
        let rects = layout.leaf_rects(bounds(), 6.0);

        assert_eq!(
            directional_neighbor(&rects, "c", PaneDirection::Right).as_deref(),
            Some("s1")
        );
        assert_eq!(
            directional_neighbor(&rects, "s1", PaneDirection::Down).as_deref(),
            Some("s2")
        );
        assert_eq!(
            directional_neighbor(&rects, "s2", PaneDirection::Left).as_deref(),
            Some("c")
        );
        assert_eq!(directional_neighbor(&rects, "c", PaneDirection::Left), None);
    }

    // MARK: - spatial_refocus

    #[test]
    fn spatial_refocus_picks_the_longest_shared_border() {
        // Closing the tall left pane: `b` shares its whole right edge, `c`
        // only touches a corner-length sliver.
        let rects = vec![
            ("a".to_string(), PaneRect::new(0.0, 0.0, 300.0, 600.0)),
            ("b".to_string(), PaneRect::new(306.0, 0.0, 300.0, 500.0)),
            ("c".to_string(), PaneRect::new(306.0, 506.0, 300.0, 94.0)),
        ];
        assert_eq!(spatial_refocus(&rects, "a").as_deref(), Some("b"));
    }

    #[test]
    fn spatial_refocus_diverges_from_index_order() {
        // Stacked{ Beside{a, b}, c } — closing the top-left pane. `c` runs the
        // full width under it (497 px of shared border) while `b` only shares
        // the short inner column edge (297 px), so the spatial answer is `c`.
        // Tree order is a, b, c, so the index-neighbor rule that governs PILL
        // closes would have said `b`: this is exactly the semantics swap the
        // plan asks for at the pane level, and only at the pane level.
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Stacked, term("c"));
        layout.split("a", SplitOrient::Beside, term("b"));
        assert_eq!(ids(&layout), vec!["a", "b", "c"], "tree order is a, b, c");

        let rects = layout.leaf_rects(bounds(), 6.0);
        assert_eq!(rect_of(&rects, "a"), PaneRect::new(0.0, 0.0, 497.0, 297.0));
        assert_eq!(rect_of(&rects, "b"), PaneRect::new(503.0, 0.0, 497.0, 297.0));
        assert_eq!(rect_of(&rects, "c"), PaneRect::new(0.0, 303.0, 1000.0, 297.0));

        assert_eq!(spatial_refocus(&rects, "a").as_deref(), Some("c"));
    }

    #[test]
    fn spatial_refocus_falls_back_to_the_nearest_center() {
        // Nothing borders the closed rect (a degenerate layout) — nearest
        // center wins rather than returning None.
        let rects = vec![
            ("closed".to_string(), PaneRect::new(0.0, 0.0, 10.0, 10.0)),
            ("far".to_string(), PaneRect::new(900.0, 900.0, 10.0, 10.0)),
            ("near".to_string(), PaneRect::new(200.0, 0.0, 10.0, 10.0)),
        ];
        assert_eq!(spatial_refocus(&rects, "closed").as_deref(), Some("near"));
    }

    #[test]
    fn spatial_refocus_of_the_only_pane_is_none() {
        let rects = vec![("a".to_string(), bounds())];
        assert_eq!(
            spatial_refocus(&rects, "a"),
            None,
            "the last pane's close is a pill close, not a pane refocus"
        );
        assert_eq!(spatial_refocus(&rects, "zzz"), None);
    }

    // MARK: - pane accessors

    #[test]
    fn pane_mut_is_the_per_pane_cwd_writer() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.pane_mut("b").unwrap().cwd = Some("/var/log".into());

        assert_eq!(layout.pane("b").unwrap().cwd.as_deref(), Some("/var/log"));
        assert_eq!(layout.pane("a").unwrap().cwd, None);
        assert!(layout.pane_mut("zzz").is_none());
    }

    #[test]
    fn claude_leaf_finds_the_one_claude_pane() {
        let mut layout = PaneLayout::single(claude("c"));
        layout.split("c", SplitOrient::Beside, term("s"));
        assert_eq!(layout.claude_leaf().map(|p| p.id.as_str()), Some("c"));

        layout.remove("c");
        assert!(
            layout.claude_leaf().is_none(),
            "a Claude leaf that exits leaves a shells-only tree (kind flips in Slice 2)"
        );
    }

    #[test]
    fn path_and_node_at_round_trip_and_tolerate_a_stale_path() {
        let mut layout = PaneLayout::single(term("a"));
        layout.split("a", SplitOrient::Beside, term("b"));
        layout.split("b", SplitOrient::Stacked, term("c"));

        assert_eq!(layout.path_to_pane("a").unwrap(), vec![Side::First]);
        assert_eq!(
            layout.path_to_pane("c").unwrap(),
            vec![Side::Second, Side::Second]
        );
        assert!(layout.path_to_pane("zzz").is_none());

        // A divider drag holds a path; the pane under it exits mid-drag.
        let stale = vec![Side::Second, Side::Second];
        layout.remove("c");
        assert!(
            layout.node_at(&stale).is_none(),
            "a stale drag path resolves to nothing, never to the wrong node"
        );
        assert!(!layout.set_ratio_at(&stale, 0.3));
    }

    #[test]
    fn model_serde_round_trips_the_tree() {
        let mut layout = PaneLayout::single(claude("c"));
        layout.split("c", SplitOrient::Beside, term("s"));
        layout.pane_mut("s").unwrap().cwd = Some("/tmp".into());

        let json = serde_json::to_string(&layout).unwrap();
        let restored: PaneLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, layout);
    }
}
