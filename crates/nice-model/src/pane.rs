//! `PaneKind`, `TabStatus`, and `Pane` — ported from
//! `Sources/Nice/State/Models.swift`. A `Pane` is a single pill in the
//! toolbar; Claude and terminal panes share the same storage and differ only
//! by [`PaneKind`].

use serde::{Deserialize, Serialize};

/// Which content a pane hosts. Claude and terminal panes share `Pane` storage
/// — only this field distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    Claude,
    Terminal,
}

/// Per-pane Claude status. Meaningful for `.claude` panes; `.terminal` panes
/// stay [`TabStatus::Idle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabStatus {
    Thinking,
    Waiting,
    #[default]
    Idle,
}

/// A single pane (toolbar pill).
///
/// Fields mirror `Models.swift`'s `Pane`. Construct with [`Pane::new`] (which
/// applies the same field defaults as the Swift memberwise init) and mutate
/// fields directly; status changes should go through [`Pane::apply_status_transition`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    pub title: String,
    pub kind: PaneKind,
    /// False once the pty for this pane has exited. Flipped before the pane is
    /// removed so interim UI states render cleanly.
    pub is_alive: bool,
    /// Per-pane status (thinking/waiting/idle).
    pub status: TabStatus,
    /// For `.waiting`: whether the user has already seen this pane enter the
    /// waiting state. The dot pulses for waiting only while this is `false`.
    /// Recomputed on every entry into `.waiting` and cleared on any transition
    /// out of it.
    pub waiting_acknowledged: bool,
    /// Runtime-only: true while this pane hosts a live `claude` process (as
    /// opposed to a shell rendered as a Claude pane — e.g. a restored pane
    /// pre-typed with `claude --resume` the user hasn't hit Enter on). Drives
    /// the socket-promotion guard: a tab with no *running* Claude reroutes the
    /// next in-tab `claude` into the current pane instead of a new tab (see
    /// [`super::Tab::has_running_claude`], asymmetry 1).
    ///
    /// `#[serde(skip)]` — excluded from persistence exactly like the Swift
    /// `CodingKeys`: restored panes always come back `false`, which is what we
    /// want (nothing is actually running yet after a relaunch).
    #[serde(skip)]
    pub is_claude_running: bool,
    /// Last-observed cwd for this pane's shell, captured from OSC 7. `None`
    /// until the shell has emitted at least one OSC 7; callers fall back to the
    /// tab's cwd in that case.
    pub cwd: Option<String>,
    /// True once the user has explicitly renamed this pane. When true, OSC
    /// titles emitted by the running program can't clobber the user's choice.
    pub title_manually_set: bool,
}

impl Pane {
    /// Construct a pane with `Models.swift`'s default field values (alive,
    /// idle, unacknowledged, not-running, no cwd, not manually titled).
    pub fn new(id: impl Into<String>, title: impl Into<String>, kind: PaneKind) -> Self {
        Pane {
            id: id.into(),
            title: title.into(),
            kind,
            is_alive: true,
            status: TabStatus::Idle,
            waiting_acknowledged: false,
            is_claude_running: false,
            cwd: None,
            title_manually_set: false,
        }
    }

    /// Apply a status transition and recompute `waiting_acknowledged` as a
    /// side effect: entering `.waiting` marks it acknowledged iff the user is
    /// already looking at this pane; any other status resets it. No-op when
    /// `new_status` matches the current status, so re-reporting the same state
    /// doesn't clobber a prior acknowledgment (`Models.swift:87-99`).
    pub fn apply_status_transition(&mut self, new_status: TabStatus, is_currently_being_viewed: bool) {
        if self.status == new_status {
            return;
        }
        self.status = new_status;
        match new_status {
            TabStatus::Waiting => self.waiting_acknowledged = is_currently_being_viewed,
            TabStatus::Thinking | TabStatus::Idle => self.waiting_acknowledged = false,
        }
    }

    /// The user just looked at this pane — if it is waiting, dismiss the
    /// attention signal. No-op otherwise (`Models.swift:103-107`).
    pub fn mark_acknowledged_if_waiting(&mut self) {
        if self.status == TabStatus::Waiting {
            self.waiting_acknowledged = true;
        }
    }

    /// Whether this pane is currently competing for the user's attention.
    /// `.thinking` always counts; `.waiting` counts only until acknowledged;
    /// `.idle` never counts (`Models.swift:113-119`).
    pub fn needs_attention(&self) -> bool {
        match self.status {
            TabStatus::Thinking => true,
            TabStatus::Waiting => !self.waiting_acknowledged,
            TabStatus::Idle => false,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from `Tests/NiceUnitTests/PaneAcknowledgmentTests.swift`
    //! (the `applyStatusTransition` + `markAcknowledgedIfWaiting` cases; the
    //! Tab-aggregation cases live with `Tab` in `tab.rs`).
    use super::*;

    // MARK: - apply_status_transition

    #[test]
    fn entering_waiting_while_being_viewed_marks_acknowledged() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, true);

        assert_eq!(pane.status, TabStatus::Waiting);
        assert!(
            pane.waiting_acknowledged,
            "A waiting state that arrives while the user is on the pane should not pulse."
        );
    }

    #[test]
    fn entering_waiting_while_not_being_viewed_stays_unacknowledged() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, false);

        assert_eq!(pane.status, TabStatus::Waiting);
        assert!(
            !pane.waiting_acknowledged,
            "A waiting state that arrives while the user is elsewhere should pulse."
        );
    }

    #[test]
    fn transitioning_out_of_waiting_clears_acknowledgment() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, true);
        assert!(pane.waiting_acknowledged);

        pane.apply_status_transition(TabStatus::Thinking, true);
        assert_eq!(pane.status, TabStatus::Thinking);
        assert!(
            !pane.waiting_acknowledged,
            "Leaving waiting must reset the flag so a future waiting event can pulse."
        );
    }

    #[test]
    fn transitioning_out_of_waiting_to_idle_clears_acknowledgment() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, false);
        // Simulate user later viewing it.
        pane.mark_acknowledged_if_waiting();
        assert!(pane.waiting_acknowledged);

        pane.apply_status_transition(TabStatus::Idle, false);
        assert_eq!(pane.status, TabStatus::Idle);
        assert!(!pane.waiting_acknowledged);
    }

    #[test]
    fn same_status_reassignment_is_noop_preserves_acknowledgment() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, false);
        assert!(!pane.waiting_acknowledged);

        // User acknowledges.
        pane.mark_acknowledged_if_waiting();
        assert!(pane.waiting_acknowledged);

        // Another .waiting report (identical status) must not wipe the
        // acknowledgment — the user has already seen the state.
        pane.apply_status_transition(TabStatus::Waiting, false);
        assert!(
            pane.waiting_acknowledged,
            "Repeated waiting reports must not re-raise the pulse once the user has acknowledged it."
        );
    }

    #[test]
    fn reentry_to_waiting_recomputes_against_current_viewing() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);

        // First waiting event while the user was elsewhere — pulses; user
        // later comes and looks.
        pane.apply_status_transition(TabStatus::Waiting, false);
        pane.mark_acknowledged_if_waiting();
        assert!(pane.waiting_acknowledged);

        // Thinking in between wipes the flag.
        pane.apply_status_transition(TabStatus::Thinking, true);
        assert!(!pane.waiting_acknowledged);

        // Second waiting event while the user is NOT on the pane — should
        // pulse again. The prior acknowledgment must not linger.
        pane.apply_status_transition(TabStatus::Waiting, false);
        assert!(
            !pane.waiting_acknowledged,
            "A fresh waiting event after thinking must re-raise the pulse when the user isn't looking."
        );
    }

    #[test]
    fn entering_thinking_does_not_set_acknowledged() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Thinking, true);

        assert_eq!(pane.status, TabStatus::Thinking);
        assert!(
            !pane.waiting_acknowledged,
            "Thinking doesn't use the acknowledgment flag; it always pulses."
        );
    }

    // MARK: - mark_acknowledged_if_waiting

    #[test]
    fn mark_acknowledged_if_waiting_while_idle_is_noop() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.mark_acknowledged_if_waiting();
        assert!(!pane.waiting_acknowledged);
    }

    #[test]
    fn mark_acknowledged_if_waiting_while_thinking_is_noop() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Thinking, false);

        pane.mark_acknowledged_if_waiting();
        assert!(
            !pane.waiting_acknowledged,
            "The flag only matters in the waiting state."
        );
    }

    #[test]
    fn mark_acknowledged_if_waiting_while_waiting_sets_true() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, false);
        assert!(!pane.waiting_acknowledged);

        pane.mark_acknowledged_if_waiting();
        assert!(pane.waiting_acknowledged);
    }

    #[test]
    fn mark_acknowledged_if_waiting_is_idempotent() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.apply_status_transition(TabStatus::Waiting, true);
        assert!(pane.waiting_acknowledged);

        pane.mark_acknowledged_if_waiting();
        pane.mark_acknowledged_if_waiting();
        assert!(pane.waiting_acknowledged);
    }

    // MARK: - #[serde(skip)] rule for is_claude_running
    //
    // `Models.swift`'s `CodingKeys` omits `isClaudeRunning` so a restored pane
    // always deserializes `false`. The real persistence schema (PersistedPane /
    // round-trip fixtures) arrives with R18; this pins only the model type's
    // own serde contract.

    #[test]
    fn is_claude_running_is_skipped_and_restores_false() {
        let mut pane = Pane::new("p", "Claude", PaneKind::Claude);
        pane.is_claude_running = true;

        let json = serde_json::to_string(&pane).unwrap();
        assert!(
            !json.contains("is_claude_running"),
            "runtime-only field must never be serialized"
        );

        let restored: Pane = serde_json::from_str(&json).unwrap();
        assert!(
            !restored.is_claude_running,
            "is_claude_running must deserialize false regardless of the source (Models.swift CodingKeys exclusion)."
        );
    }
}
