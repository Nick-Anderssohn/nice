//! `TermWindowKind`, `SessionStatus`, and `TermWindow` — ported from
//! `Sources/Nice/State/Models.swift`. A `TermWindow` is a single pill in the
//! toolbar; Claude and terminal windows share the same storage and differ only
//! by [`TermWindowKind`].

use serde::{Deserialize, Serialize};

use crate::pane_layout::{Pane, PaneLayout, PaneRect, NOMINAL_EXTENT};

/// Which content a window hosts. Claude and terminal windows share `TermWindow` storage
/// — only this field distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TermWindowKind {
    Claude,
    Terminal,
}

/// Per-window Claude status. Meaningful for `.claude` windows; `.terminal` windows
/// stay [`SessionStatus::Idle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Thinking,
    Waiting,
    #[default]
    Idle,
}

/// A single window (toolbar pill).
///
/// Fields mirror `Models.swift`'s `TermWindow`. Construct with [`TermWindow::new`] (which
/// applies the same field defaults as the Swift memberwise init) and mutate
/// fields directly; status changes should go through [`TermWindow::apply_status_transition`].
///
/// Since tmux-port Phase 2 a window also owns a [`PaneLayout`] — its split
/// tree. A window that was never split carries a single-leaf tree, which is
/// what [`TermWindow::new`] builds, so every pre-splits behavior is unchanged.
///
/// **No `Eq`/`Hash`.** [`PaneLayout`]'s split ratios are `f32`, which has no
/// total equality, so the derives that used to sit here came off with the
/// layout field. Nothing in the workspace keyed a `HashSet`/`HashMap` on a
/// window (or on [`crate::Session`]/[`crate::Project`], which contain one);
/// `PartialEq` — all the tests and diffing actually use — stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermWindow {
    pub id: String,
    pub title: String,
    pub kind: TermWindowKind,
    /// False once the pty for this window has exited. Flipped before the window is
    /// removed so interim UI states render cleanly.
    pub is_alive: bool,
    /// Per-window status (thinking/waiting/idle).
    pub status: SessionStatus,
    /// For `.waiting`: whether the user has already seen this window enter the
    /// waiting state. The dot pulses for waiting only while this is `false`.
    /// Recomputed on every entry into `.waiting` and cleared on any transition
    /// out of it.
    pub waiting_acknowledged: bool,
    /// Runtime-only: true while this window hosts a live `claude` process (as
    /// opposed to a shell rendered as a Claude window — e.g. a restored window
    /// pre-typed with `claude --resume` the user hasn't hit Enter on). Drives
    /// the socket-promotion guard: a session with no *running* Claude reroutes the
    /// next in-session `claude` into the current window instead of a new session (see
    /// [`super::Session::has_running_claude`], asymmetry 1).
    ///
    /// `#[serde(skip)]` — excluded from persistence exactly like the Swift
    /// `CodingKeys`: restored windows always come back `false`, which is what we
    /// want (nothing is actually running yet after a relaunch).
    #[serde(skip)]
    pub is_claude_running: bool,
    /// Last-observed cwd for this window's shell, captured from OSC 7. `None`
    /// until the shell has emitted at least one OSC 7; callers fall back to the
    /// session's cwd in that case.
    pub cwd: Option<String>,
    /// True once the user has explicitly renamed this window. When true, OSC
    /// titles emitted by the running program can't clobber the user's choice.
    pub title_manually_set: bool,
    /// This window's split tree (tmux-port Phase 2). A never-split window holds
    /// a single [`PaneLayout::Leaf`] whose pane id equals the window's own id.
    ///
    /// `#[serde(default)]` only shims the MODEL-level serde surface (snake_case,
    /// exercised by this crate's own tests) — the persisted layer is separate
    /// and owns the frozen camelCase spellings. The default is a degenerate
    /// empty-id leaf; the loader repairs it, see
    /// [`crate::PersistedTermWindow::hydrate`].
    #[serde(default = "default_layout")]
    pub layout: PaneLayout,
    /// The focused pane — always a leaf of [`layout`](Self::layout).
    #[serde(default)]
    pub active_pane_id: String,
    /// Runtime-only: while true only the focused pane is painted, full-size,
    /// and dividers are skipped (P4). Every pty stays live underneath. Any
    /// structural or focus change un-zooms first.
    ///
    /// `#[serde(skip)]` — a transient render flag, never persisted, exactly
    /// like [`is_claude_running`](Self::is_claude_running).
    #[serde(skip)]
    pub zoomed: bool,
}

/// Serde shim for [`TermWindow::layout`]; see the field docs.
fn default_layout() -> PaneLayout {
    PaneLayout::single(Pane::new(String::new(), TermWindowKind::Terminal))
}

impl TermWindow {
    /// Construct a window with `Models.swift`'s default field values (alive,
    /// idle, unacknowledged, not-running, no cwd, not manually titled) and the
    /// single-leaf pane tree every pill starts as.
    pub fn new(id: impl Into<String>, title: impl Into<String>, kind: TermWindowKind) -> Self {
        let id = id.into();
        TermWindow {
            layout: PaneLayout::single(Pane::new(id.clone(), kind)),
            active_pane_id: id.clone(),
            zoomed: false,
            id,
            title: title.into(),
            kind,
            is_alive: true,
            status: SessionStatus::Idle,
            waiting_acknowledged: false,
            is_claude_running: false,
            cwd: None,
            title_manually_set: false,
        }
    }

    /// Collapse this window back to the pre-splits shape: one leaf mirroring
    /// the window's own id / kind / cwd, focused.
    ///
    /// Two callers: [`TermWindow::new`]'s shape, restated after `cwd` has been
    /// filled in, and the loader's fallback when a persisted tree fails
    /// validation — restoring a mangled layout as a single pane is always
    /// better than refusing to restore the session.
    pub fn reset_layout_to_single_leaf(&mut self) {
        self.layout =
            PaneLayout::single(Pane::new(self.id.clone(), self.kind).with_cwd(self.cwd.clone()));
        self.active_pane_id = self.id.clone();
    }

    /// The focused pane, if [`active_pane_id`](Self::active_pane_id) still
    /// names a leaf.
    pub fn active_pane(&self) -> Option<&Pane> {
        self.layout.pane(&self.active_pane_id)
    }

    /// Which pane a window-level (pane-less) call means: the focused leaf, or
    /// the sole leaf when focus has gone stale.
    ///
    /// This is what keeps the pre-splits surfaces honest — `window_title_changed`
    /// and friends still take a `(session, window)` pair and land on the one
    /// pane a never-split pill has.
    pub fn effective_pane_id(&self) -> String {
        if self.layout.contains(&self.active_pane_id) {
            return self.active_pane_id.clone();
        }
        match self.layout.single_leaf() {
            Some(pane) => pane.id.clone(),
            None => self.active_pane_id.clone(),
        }
    }

    /// Mark which leaf hosts Claude, and return whether one now does.
    ///
    /// The socket's in-place promotion flips the pill to Claude kind; this is
    /// the tree's half of that flip, so the invariant "`kind == Claude` iff
    /// exactly one Claude leaf" survives it. The mark goes to the focused pane
    /// — the best a pill-scoped socket identity can resolve (P2: the socket's
    /// `paneId` names the pill, not the pane) — and any other leaf carrying a
    /// stale mark loses it, so the tree can never hold two.
    ///
    /// Promotion only runs when the session has no *running* Claude, so a leaf
    /// demoted here is either a deferred-resume shell or a held corpse, neither
    /// of which is a Claude anyone can talk to.
    pub fn mark_claude_pane(&mut self) -> bool {
        let target = self.effective_pane_id();
        let stale: Vec<String> = self
            .layout
            .leaves()
            .iter()
            .filter(|p| p.kind == TermWindowKind::Claude && p.id != target)
            .map(|p| p.id.clone())
            .collect();
        for id in stale {
            if let Some(pane) = self.layout.pane_mut(&id) {
                pane.kind = TermWindowKind::Terminal;
            }
        }
        match self.layout.pane_mut(&target) {
            Some(pane) => {
                pane.kind = TermWindowKind::Claude;
                pane.is_alive = true;
                true
            }
            None => false,
        }
    }

    /// Whether any leaf of this window still has a live pty — the window-level
    /// `is_alive` semantics since Phase 2. A held Claude pane beside a running
    /// shell leaves the pill alive.
    pub fn any_pane_alive(&self) -> bool {
        self.layout.leaves().iter().any(|p| p.is_alive)
    }

    /// Whether this window hosts a **live** Claude pane. The Claude-presence
    /// predicates key off this rather than the window's own `is_alive`, so a
    /// held Claude pane in a still-running pill stops counting as a Claude.
    pub fn claude_pane_is_live(&self) -> bool {
        self.is_alive && self.layout.claude_leaf().is_some_and(|p| p.is_alive)
    }

    /// This window's leaf rects at a stand-in extent, for the consumers that
    /// have no painted bounds (a pane exiting in a background pill, a keyboard
    /// focus move before the first paint). Adjacency ranking is
    /// scale-invariant, so these give the same answers the painted rects would.
    pub fn nominal_leaf_rects(&self) -> Vec<(String, PaneRect)> {
        self.layout.leaf_rects(
            PaneRect::new(0.0, 0.0, NOMINAL_EXTENT, NOMINAL_EXTENT),
            0.0,
        )
    }

    /// Whether this window's pane tree satisfies the Phase-2 invariants:
    ///
    /// 1. [`active_pane_id`](Self::active_pane_id) names a leaf of the tree;
    /// 2. no two leaves share an id (the view cache and the pty map key on
    ///    them);
    /// 3. the tree holds **at most one** Claude leaf, and holds one exactly
    ///    when `kind == Claude`.
    ///
    /// "At most one" rather than "exactly one" is deliberate: a Claude leaf can
    /// exit out of a multi-pane pill, which removes the leaf and flips the
    /// pill's `kind` to `Terminal` — so the pair stays consistent while the
    /// count changes.
    pub fn layout_is_valid(&self) -> bool {
        let leaves = self.layout.leaves();
        if !leaves.iter().any(|p| p.id == self.active_pane_id) {
            return false;
        }
        for (i, pane) in leaves.iter().enumerate() {
            if leaves[i + 1..].iter().any(|other| other.id == pane.id) {
                return false;
            }
        }
        let claude_leaves = leaves
            .iter()
            .filter(|p| p.kind == TermWindowKind::Claude)
            .count();
        if claude_leaves > 1 {
            return false;
        }
        (claude_leaves == 1) == (self.kind == TermWindowKind::Claude)
    }

    /// Apply a status transition and recompute `waiting_acknowledged` as a
    /// side effect: entering `.waiting` marks it acknowledged iff the user is
    /// already looking at this window; any other status resets it. No-op when
    /// `new_status` matches the current status, so re-reporting the same state
    /// doesn't clobber a prior acknowledgment (`Models.swift:87-99`).
    pub fn apply_status_transition(&mut self, new_status: SessionStatus, is_currently_being_viewed: bool) {
        if self.status == new_status {
            return;
        }
        self.status = new_status;
        match new_status {
            SessionStatus::Waiting => self.waiting_acknowledged = is_currently_being_viewed,
            SessionStatus::Thinking | SessionStatus::Idle => self.waiting_acknowledged = false,
        }
    }

    /// The user just looked at this window — if it is waiting, dismiss the
    /// attention signal. No-op otherwise (`Models.swift:103-107`).
    pub fn mark_acknowledged_if_waiting(&mut self) {
        if self.status == SessionStatus::Waiting {
            self.waiting_acknowledged = true;
        }
    }

    /// Whether this window is currently competing for the user's attention.
    /// `.thinking` always counts; `.waiting` counts only until acknowledged;
    /// `.idle` never counts (`Models.swift:113-119`).
    pub fn needs_attention(&self) -> bool {
        match self.status {
            SessionStatus::Thinking => true,
            SessionStatus::Waiting => !self.waiting_acknowledged,
            SessionStatus::Idle => false,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from `Tests/NiceUnitTests/PaneAcknowledgmentTests.swift`
    //! (the `applyStatusTransition` + `markAcknowledgedIfWaiting` cases; the
    //! Session-aggregation cases live with `Session` in `session.rs`).
    use super::*;

    // MARK: - apply_status_transition

    #[test]
    fn entering_waiting_while_being_viewed_marks_acknowledged() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, true);

        assert_eq!(window.status, SessionStatus::Waiting);
        assert!(
            window.waiting_acknowledged,
            "A waiting state that arrives while the user is on the window should not pulse."
        );
    }

    #[test]
    fn entering_waiting_while_not_being_viewed_stays_unacknowledged() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, false);

        assert_eq!(window.status, SessionStatus::Waiting);
        assert!(
            !window.waiting_acknowledged,
            "A waiting state that arrives while the user is elsewhere should pulse."
        );
    }

    #[test]
    fn transitioning_out_of_waiting_clears_acknowledgment() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, true);
        assert!(window.waiting_acknowledged);

        window.apply_status_transition(SessionStatus::Thinking, true);
        assert_eq!(window.status, SessionStatus::Thinking);
        assert!(
            !window.waiting_acknowledged,
            "Leaving waiting must reset the flag so a future waiting event can pulse."
        );
    }

    #[test]
    fn transitioning_out_of_waiting_to_idle_clears_acknowledgment() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, false);
        // Simulate user later viewing it.
        window.mark_acknowledged_if_waiting();
        assert!(window.waiting_acknowledged);

        window.apply_status_transition(SessionStatus::Idle, false);
        assert_eq!(window.status, SessionStatus::Idle);
        assert!(!window.waiting_acknowledged);
    }

    #[test]
    fn same_status_reassignment_is_noop_preserves_acknowledgment() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, false);
        assert!(!window.waiting_acknowledged);

        // User acknowledges.
        window.mark_acknowledged_if_waiting();
        assert!(window.waiting_acknowledged);

        // Another .waiting report (identical status) must not wipe the
        // acknowledgment — the user has already seen the state.
        window.apply_status_transition(SessionStatus::Waiting, false);
        assert!(
            window.waiting_acknowledged,
            "Repeated waiting reports must not re-raise the pulse once the user has acknowledged it."
        );
    }

    #[test]
    fn reentry_to_waiting_recomputes_against_current_viewing() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);

        // First waiting event while the user was elsewhere — pulses; user
        // later comes and looks.
        window.apply_status_transition(SessionStatus::Waiting, false);
        window.mark_acknowledged_if_waiting();
        assert!(window.waiting_acknowledged);

        // Thinking in between wipes the flag.
        window.apply_status_transition(SessionStatus::Thinking, true);
        assert!(!window.waiting_acknowledged);

        // Second waiting event while the user is NOT on the window — should
        // pulse again. The prior acknowledgment must not linger.
        window.apply_status_transition(SessionStatus::Waiting, false);
        assert!(
            !window.waiting_acknowledged,
            "A fresh waiting event after thinking must re-raise the pulse when the user isn't looking."
        );
    }

    #[test]
    fn entering_thinking_does_not_set_acknowledged() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Thinking, true);

        assert_eq!(window.status, SessionStatus::Thinking);
        assert!(
            !window.waiting_acknowledged,
            "Thinking doesn't use the acknowledgment flag; it always pulses."
        );
    }

    // MARK: - mark_acknowledged_if_waiting

    #[test]
    fn mark_acknowledged_if_waiting_while_idle_is_noop() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.mark_acknowledged_if_waiting();
        assert!(!window.waiting_acknowledged);
    }

    #[test]
    fn mark_acknowledged_if_waiting_while_thinking_is_noop() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Thinking, false);

        window.mark_acknowledged_if_waiting();
        assert!(
            !window.waiting_acknowledged,
            "The flag only matters in the waiting state."
        );
    }

    #[test]
    fn mark_acknowledged_if_waiting_while_waiting_sets_true() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, false);
        assert!(!window.waiting_acknowledged);

        window.mark_acknowledged_if_waiting();
        assert!(window.waiting_acknowledged);
    }

    #[test]
    fn mark_acknowledged_if_waiting_is_idempotent() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.apply_status_transition(SessionStatus::Waiting, true);
        assert!(window.waiting_acknowledged);

        window.mark_acknowledged_if_waiting();
        window.mark_acknowledged_if_waiting();
        assert!(window.waiting_acknowledged);
    }

    // MARK: - #[serde(skip)] rule for is_claude_running
    //
    // `Models.swift`'s `CodingKeys` omits `isClaudeRunning` so a restored window
    // always deserializes `false`. The real persistence schema (PersistedTermWindow /
    // round-trip fixtures) arrives with R18; this pins only the model type's
    // own serde contract.

    // MARK: - the pane tree (tmux-port Phase 2)

    #[test]
    fn new_window_is_a_single_leaf_tree_focused_on_itself() {
        let window = TermWindow::new("p1", "Claude", TermWindowKind::Claude);

        assert_eq!(window.layout.leaf_count(), 1, "a fresh pill has one pane");
        assert_eq!(window.active_pane_id, "p1");
        let pane = window.layout.single_leaf().expect("single leaf");
        assert_eq!(
            pane.id, "p1",
            "the sole pane's id is the window's own — globally unique by construction"
        );
        assert_eq!(pane.kind, TermWindowKind::Claude, "the leaf carries the pill's kind");
        assert_eq!(pane.cwd, None);
        assert!(!window.zoomed);
        assert!(window.layout_is_valid());
    }

    #[test]
    fn reset_layout_to_single_leaf_carries_the_windows_cwd() {
        let mut window = TermWindow::new("p1", "zsh", TermWindowKind::Terminal);
        window.cwd = Some("/var/log".into());
        window.layout.split(
            "p1",
            crate::pane_layout::SplitOrient::Beside,
            Pane::new("p2", TermWindowKind::Terminal),
        );
        window.active_pane_id = "p2".into();

        window.reset_layout_to_single_leaf();
        assert_eq!(window.layout.leaf_count(), 1);
        assert_eq!(window.active_pane_id, "p1");
        assert_eq!(
            window.layout.single_leaf().unwrap().cwd.as_deref(),
            Some("/var/log")
        );
    }

    #[test]
    fn layout_is_valid_requires_the_active_pane_to_be_a_leaf() {
        let mut window = TermWindow::new("p1", "zsh", TermWindowKind::Terminal);
        assert!(window.layout_is_valid());
        assert_eq!(window.active_pane().map(|p| p.id.as_str()), Some("p1"));

        window.active_pane_id = "ghost".into();
        assert!(!window.layout_is_valid());
        assert!(window.active_pane().is_none());
    }

    #[test]
    fn layout_is_valid_rejects_duplicate_pane_ids() {
        let mut window = TermWindow::new("p1", "zsh", TermWindowKind::Terminal);
        window.layout = PaneLayout::Split {
            orient: crate::pane_layout::SplitOrient::Beside,
            ratio: 0.5,
            first: Box::new(PaneLayout::single(Pane::new("dup", TermWindowKind::Terminal))),
            second: Box::new(PaneLayout::single(Pane::new("dup", TermWindowKind::Terminal))),
        };
        window.active_pane_id = "dup".into();

        assert!(
            !window.layout_is_valid(),
            "the view cache and pty map key on pane ids — duplicates would collide"
        );
    }

    #[test]
    fn layout_is_valid_ties_the_claude_leaf_to_the_pill_kind() {
        // A Claude pill with its Claude leaf plus a shell pane: the D1 layout.
        let mut window = TermWindow::new("p1", "Claude", TermWindowKind::Claude);
        window.layout.split(
            "p1",
            crate::pane_layout::SplitOrient::Beside,
            Pane::new("shell", TermWindowKind::Terminal),
        );
        assert!(window.layout_is_valid());

        // Two Claude leaves is never legal — Nice only ever spawns shells into
        // splits (D1).
        window.layout.split(
            "shell",
            crate::pane_layout::SplitOrient::Beside,
            Pane::new("shell2", TermWindowKind::Claude),
        );
        assert!(!window.layout_is_valid());
    }

    #[test]
    fn layout_is_valid_wants_the_kind_flip_after_a_claude_leaf_exits() {
        let mut window = TermWindow::new("p1", "Claude", TermWindowKind::Claude);
        window.layout.split(
            "p1",
            crate::pane_layout::SplitOrient::Beside,
            Pane::new("shell", TermWindowKind::Terminal),
        );
        window.layout.remove("p1");
        window.active_pane_id = "shell".into();

        assert!(
            !window.layout_is_valid(),
            "a Claude-kind pill with no Claude leaf is inconsistent…"
        );
        window.kind = TermWindowKind::Terminal;
        assert!(
            window.layout_is_valid(),
            "…until the clean-exit kind flip lands (Slice 2)"
        );
    }

    #[test]
    fn zoomed_is_skipped_and_restores_false() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.zoomed = true;

        let json = serde_json::to_string(&window).unwrap();
        assert!(
            !json.contains("zoomed"),
            "zoom is a transient render flag (P4) — it must never reach disk"
        );
        let restored: TermWindow = serde_json::from_str(&json).unwrap();
        assert!(!restored.zoomed);
        assert_eq!(
            restored.layout, window.layout,
            "the tree itself does round-trip through model serde"
        );
        assert_eq!(restored.active_pane_id, "p");
    }

    #[test]
    fn is_claude_running_is_skipped_and_restores_false() {
        let mut window = TermWindow::new("p", "Claude", TermWindowKind::Claude);
        window.is_claude_running = true;

        let json = serde_json::to_string(&window).unwrap();
        assert!(
            !json.contains("is_claude_running"),
            "runtime-only field must never be serialized"
        );

        let restored: TermWindow = serde_json::from_str(&json).unwrap();
        assert!(
            !restored.is_claude_running,
            "is_claude_running must deserialize false regardless of the source (Models.swift CodingKeys exclusion)."
        );
    }

    /// A Claude pill split with a shell, the D1 layout — the fixture the
    /// per-leaf liveness cases below all start from.
    fn claude_pill_with_shell() -> TermWindow {
        let mut window = TermWindow::new("p1", "Claude", TermWindowKind::Claude);
        window.layout.split(
            "p1",
            crate::pane_layout::SplitOrient::Beside,
            Pane::new("shell", TermWindowKind::Terminal),
        );
        window
    }

    #[test]
    fn a_held_claude_pane_stops_counting_as_a_claude_while_the_pill_lives() {
        let mut window = claude_pill_with_shell();
        assert!(window.claude_pane_is_live());
        assert!(window.any_pane_alive());

        window.layout.pane_mut("p1").unwrap().is_alive = false;

        assert!(
            !window.claude_pane_is_live(),
            "the sidebar dot must not track a corpse"
        );
        assert!(
            window.any_pane_alive(),
            "the shell beside it keeps the pill alive"
        );
    }

    #[test]
    fn pane_liveness_is_runtime_only() {
        let mut window = TermWindow::new("p", "zsh", TermWindowKind::Terminal);
        window.layout.pane_mut("p").unwrap().is_alive = false;

        let layout_json = serde_json::to_string(&window.layout).unwrap();
        assert!(
            !layout_json.contains("is_alive"),
            "per-pane held-ness is never persisted"
        );

        let json = serde_json::to_string(&window).unwrap();
        let restored: TermWindow = serde_json::from_str(&json).unwrap();
        assert!(
            restored.layout.pane("p").unwrap().is_alive,
            "a restored pane is alive, not dead — the serde default must be true"
        );
    }

    #[test]
    fn effective_pane_id_falls_back_to_the_sole_leaf() {
        let mut window = TermWindow::new("p1", "zsh", TermWindowKind::Terminal);
        window.active_pane_id = "ghost".into();
        assert_eq!(
            window.effective_pane_id(),
            "p1",
            "a pane-less window-level call still lands on a never-split pill's one pane"
        );

        window.layout.split(
            "p1",
            crate::pane_layout::SplitOrient::Beside,
            Pane::new("shell", TermWindowKind::Terminal),
        );
        window.active_pane_id = "shell".into();
        assert_eq!(window.effective_pane_id(), "shell");
    }

    #[test]
    fn mark_claude_pane_moves_the_mark_and_keeps_at_most_one() {
        // A deferred-resume Claude pill split with a shell; the user runs
        // `claude` in the SHELL pane, so the socket promotes the pill with that
        // pane focused.
        let mut window = claude_pill_with_shell();
        window.active_pane_id = "shell".into();

        assert!(window.mark_claude_pane());

        assert_eq!(
            window.layout.claude_leaf().map(|p| p.id.as_str()),
            Some("shell")
        );
        assert_eq!(
            window
                .layout
                .leaves()
                .iter()
                .filter(|p| p.kind == TermWindowKind::Claude)
                .count(),
            1,
            "the old mark must be dropped, not doubled"
        );
        assert!(window.layout_is_valid());
    }

    #[test]
    fn nominal_leaf_rects_rank_adjacency_the_way_painted_ones_do() {
        let window = claude_pill_with_shell();
        let rects = window.nominal_leaf_rects();

        assert_eq!(rects.len(), 2);
        assert_eq!(
            crate::pane_layout::directional_neighbor(
                &rects,
                "p1",
                crate::pane_layout::PaneDirection::Right
            )
            .as_deref(),
            Some("shell"),
            "a Beside split puts the new pane to the right, at any extent"
        );
    }
}
