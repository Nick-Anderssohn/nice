//! `Tab` — a session (one sidebar row) — ported from
//! `Sources/Nice/State/Models.swift`. Owns an ordered list of [`Pane`]s and
//! derives its sidebar-dot status purely from them.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::pane::{Pane, PaneKind, TabStatus};

/// A tab (session). Fields mirror `Models.swift`'s `Tab`. Construct with
/// [`Tab::new`] and set `panes` / `active_pane_id` directly.
///
/// `Tab.branch` (vestigial, roadmap M5) is deliberately **not** ported into
/// this struct.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub title: String,
    /// Load-bearing for `claude --resume`'s working dir. Two writers with
    /// different policies feed it (OSC 7 vs. the SessionStart hook —
    /// [`crate::TabModel::adopt_tab_cwd`]); the field is defined here.
    pub cwd: String,
    /// Ordered panes shown as pills. Expected non-empty while the tab is alive.
    pub panes: Vec<Pane>,
    /// The pane currently focused. `None` only when `panes` is empty
    /// (transient, during teardown).
    pub active_pane_id: Option<String>,
    /// True once the tab label was filled from Claude's OSC-set title rather
    /// than left as "New tab".
    pub title_auto_generated: bool,
    /// True once the user has explicitly renamed this tab. When true, auto
    /// titling skips this tab so OSC-driven titles can't clobber the choice.
    pub title_manually_set: bool,
    /// UUID of the underlying Claude Code session, for `claude --resume`.
    /// `None` for terminal-only tabs (including everything in the Terminals
    /// group).
    pub claude_session_id: Option<String>,
    /// ID of a parent tab in the same project that this tab nests one indent
    /// under — the depth-1 lineage link. Set by the lineage paths
    /// ([`crate::TabModel::insert_branch_parent`] /
    /// [`crate::TabModel::insert_handoff_child`]); `None` for tabs neither path
    /// has placed into a lineage. The renderer maps a non-`None` value to
    /// exactly one indent level; depth never grows past one.
    pub parent_tab_id: Option<String>,
    /// Monotonically incremented per-tab counter feeding the auto-name
    /// "Terminal N". Never decremented when a pane is closed, so a closed
    /// "Terminal 2" is not reused — the next add becomes "Terminal 4"
    /// (asymmetry 2). Persisted so it survives relaunch.
    pub next_terminal_index: u32,
}

impl Tab {
    /// Construct a tab with `Models.swift`'s default field values (no panes,
    /// no active pane, auto/manual title flags off, no session id, no lineage
    /// parent, counter primed at 1).
    pub fn new(id: impl Into<String>, title: impl Into<String>, cwd: impl Into<String>) -> Self {
        Tab {
            id: id.into(),
            title: title.into(),
            cwd: cwd.into(),
            panes: Vec::new(),
            active_pane_id: None,
            title_auto_generated: false,
            title_manually_set: false,
            claude_session_id: None,
            parent_tab_id: None,
            next_terminal_index: 1,
        }
    }

    /// The alive Claude panes on this tab — the only panes the sidebar dot
    /// derives from, so it can't drift from the toolbar pill.
    fn live_claude_panes(&self) -> impl Iterator<Item = &Pane> {
        self.panes
            .iter()
            .filter(|p| p.kind == PaneKind::Claude && p.is_alive)
    }

    /// True if any alive pane on this tab is a Claude pane (`Models.swift:215`).
    pub fn has_claude(&self) -> bool {
        self.live_claude_panes().next().is_some()
    }

    /// Whether any *running* Claude occupies this tab — the promotion-refusal
    /// predicate (asymmetry 1). When true, the next in-tab `claude` reroutes
    /// into the current pane instead of opening a new tab; a running Claude and
    /// a deferred-resume Claude (`is_claude_running == false`) can coexist, and
    /// only the running one trips this. Keys on [`Pane::is_claude_running`],
    /// exactly like the Swift promotion guard (`SessionsModel.swift:855-859`).
    pub fn has_running_claude(&self) -> bool {
        self.live_claude_panes().any(|p| p.is_claude_running)
    }

    /// The pane currently focused, if any (`Models.swift:220-223`).
    pub fn active_pane(&self) -> Option<&Pane> {
        let id = self.active_pane_id.as_ref()?;
        self.panes.iter().find(|p| &p.id == id)
    }

    /// Whether any pane in `offscreen_ids` currently needs attention — used by
    /// the toolbar's overflow chevron to badge itself when an attention-worthy
    /// pane has scrolled out of view (`Models.swift:229-234`).
    pub fn has_offscreen_attention(&self, offscreen_ids: &HashSet<String>) -> bool {
        if offscreen_ids.is_empty() {
            return false;
        }
        self.panes
            .iter()
            .any(|p| offscreen_ids.contains(&p.id) && p.needs_attention())
    }

    /// Aggregate status shown in the sidebar dot. Derived from live Claude
    /// panes only (thinking > waiting > idle) so the sidebar can't drift from
    /// the toolbar pill. Written defensively for transient multi-pane states
    /// during creation/teardown (`Models.swift:242-247`).
    pub fn status(&self) -> TabStatus {
        if self.live_claude_panes().any(|p| p.status == TabStatus::Thinking) {
            return TabStatus::Thinking;
        }
        if self.live_claude_panes().any(|p| p.status == TabStatus::Waiting) {
            return TabStatus::Waiting;
        }
        TabStatus::Idle
    }

    /// Sidebar-dot pulse suppression: true iff every waiting Claude pane on the
    /// tab has been acknowledged. Returns `false` when no Claude pane is
    /// waiting — callers only read this while `status() == .waiting`, so the
    /// value is only meaningful then (`Models.swift:254-260`).
    pub fn waiting_acknowledged(&self) -> bool {
        let mut any_waiting = false;
        for pane in self
            .live_claude_panes()
            .filter(|p| p.status == TabStatus::Waiting)
        {
            any_waiting = true;
            if !pane.waiting_acknowledged {
                return false;
            }
        }
        any_waiting
    }

    /// Recover `next_terminal_index` from a tab's pane titles when an older
    /// session file lacks the persisted counter. Parses each title against
    /// `^terminal\s+(\d+)$` (case-insensitive) and returns `1 + max(N)`,
    /// floored at 1 so a tab whose terminal panes were all renamed still starts
    /// numbering from 1 (`Models.swift:199-212`).
    pub fn recover_next_terminal_index<S: AsRef<str>>(pane_titles: &[S]) -> u32 {
        let max_n = pane_titles
            .iter()
            .filter_map(|t| parse_terminal_index(t.as_ref()))
            .max()
            .unwrap_or(0);
        max_n.saturating_add(1).max(1)
    }
}

/// Parse a pane title against `^terminal\s+(\d+)$` (case-insensitive),
/// returning the captured N. ASCII digits only, mirroring the Swift path where
/// `Int(_:)` rejects the non-ASCII digits ICU's `\d` would otherwise admit.
fn parse_terminal_index(title: &str) -> Option<u32> {
    let lower = title.to_lowercase();
    // `^terminal`
    let after_word = lower.strip_prefix("terminal")?;
    // `\s+` — at least one whitespace char between the word and the number.
    let digits = after_word.trim_start_matches(char::is_whitespace);
    if digits.len() == after_word.len() {
        return None;
    }
    // `(\d+)$` — a non-empty, all-ASCII-digit remainder anchored to the end.
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(id: &str) -> Pane {
        Pane::new(id, "Claude", PaneKind::Claude)
    }

    fn terminal(id: &str) -> Pane {
        Pane::new(id, "Terminal", PaneKind::Terminal)
    }

    fn tab_with(panes: Vec<Pane>, active: Option<&str>) -> Tab {
        let mut tab = Tab::new("t", "", "/");
        tab.panes = panes;
        tab.active_pane_id = active.map(|s| s.to_string());
        tab
    }

    // MARK: - Tab aggregation
    //
    // Ported from PaneAcknowledgmentTests.swift's "Tab aggregation" section.
    // `Tab::status` / `Tab::waiting_acknowledged` are pure functions of `panes`
    // — they never consult `active_pane_id`. That is the invariant that keeps
    // the sidebar dot (Tab-level) from drifting from the toolbar pill dot
    // (Pane-level) when the user focuses a non-Claude pane.

    #[test]
    fn tab_status_no_claude_pane_is_idle() {
        let tab = tab_with(vec![terminal("p")], Some("p"));
        assert_eq!(tab.status(), TabStatus::Idle);
    }

    #[test]
    fn tab_status_claude_thinking_is_thinking_regardless_of_active_pane() {
        let mut c = claude("claude");
        c.apply_status_transition(TabStatus::Thinking, false);

        let mut tab = tab_with(vec![c, terminal("term")], Some("claude"));
        assert_eq!(tab.status(), TabStatus::Thinking);

        tab.active_pane_id = Some("term".to_string());
        assert_eq!(
            tab.status(),
            TabStatus::Thinking,
            "Focusing the terminal must not freeze tab.status on an old value."
        );
    }

    #[test]
    fn tab_status_claude_waiting_is_waiting_regardless_of_active_pane() {
        let mut c = claude("claude");
        c.apply_status_transition(TabStatus::Waiting, false);

        let mut tab = tab_with(vec![c, terminal("term")], Some("term"));
        assert_eq!(
            tab.status(),
            TabStatus::Waiting,
            "Claude transitions to waiting while the companion terminal is active; sidebar must reflect waiting."
        );
        assert!(
            !tab.waiting_acknowledged(),
            "User is not viewing the Claude pane, so the pulse must not be suppressed."
        );

        tab.active_pane_id = Some("claude".to_string());
        assert_eq!(tab.status(), TabStatus::Waiting);
    }

    #[test]
    fn tab_status_dead_claude_pane_excluded() {
        let mut c = claude("claude");
        c.apply_status_transition(TabStatus::Thinking, false);
        c.is_alive = false;
        let tab = tab_with(vec![c], Some("claude"));
        assert_eq!(
            tab.status(),
            TabStatus::Idle,
            "A dead Claude pane must not keep the sidebar dot lit."
        );
    }

    #[test]
    fn tab_waiting_acknowledged_waiting_acked_returns_true() {
        let mut c = claude("claude");
        c.apply_status_transition(TabStatus::Waiting, true);
        assert!(c.waiting_acknowledged);

        let mut tab = tab_with(vec![c, terminal("term")], Some("claude"));
        assert!(tab.waiting_acknowledged());

        // Flipping the active pane to the terminal MUST NOT change the tab's
        // acknowledgment — that was the sidebar/toolbar drift bug.
        tab.active_pane_id = Some("term".to_string());
        assert!(
            tab.waiting_acknowledged(),
            "tab.waiting_acknowledged is a pure function of panes; active-pane selection must not mutate it."
        );
    }

    #[test]
    fn tab_waiting_acknowledged_waiting_unacked_returns_false() {
        let mut c = claude("claude");
        c.apply_status_transition(TabStatus::Waiting, false);
        let tab = tab_with(vec![c], Some("claude"));
        assert!(!tab.waiting_acknowledged());
    }

    #[test]
    fn tab_waiting_acknowledged_no_waiting_pane_returns_false() {
        let mut c = claude("claude");
        c.apply_status_transition(TabStatus::Thinking, true);
        let tab = tab_with(vec![c], Some("claude"));
        assert!(
            !tab.waiting_acknowledged(),
            "No waiting pane → acknowledgment is meaningless and reported as false."
        );
    }

    #[test]
    fn tab_waiting_acknowledged_no_panes_is_false() {
        let tab = tab_with(vec![], None);
        assert!(!tab.waiting_acknowledged());
        assert_eq!(tab.status(), TabStatus::Idle);
    }

    // MARK: - Status-dot aggregation (model half of AppStateStatusDotTests)
    //
    // Ported from AppStateStatusDotTests.swift. Those tests drive
    // `AppState.paneTitleChanged` / `setActivePane` — the OSC-title→status
    // ROUTING and the pty/session wiring are R13/R15, out of scope here. The
    // MODEL half is the sidebar/toolbar sync invariant: with the Claude pane's
    // status driven directly via `apply_status_transition`, `tab.status()`
    // always equals that pane's status regardless of which pane is active.

    #[test]
    fn status_dot_inactive_claude_pane_tracks_thinking_then_waiting() {
        // R13/R15: the braille-prefix `\u{2800}` → thinking / `\u{2733}` →
        // waiting OSC-title decoding and the paneTitleChanged wiring.
        let mut tab = tab_with(vec![claude("claude"), terminal("term")], Some("claude"));

        // User focuses the terminal pane — the setup for the original bug.
        tab.active_pane_id = Some("term".to_string());

        // Claude "emits" thinking while the terminal is active.
        {
            let c = tab.panes.iter_mut().find(|p| p.id == "claude").unwrap();
            c.apply_status_transition(TabStatus::Thinking, false);
        }
        assert_eq!(
            tab.status(),
            TabStatus::Thinking,
            "tab.status must track the Claude pane even when the companion terminal is active."
        );

        // Claude transitions to waiting — the moment the sidebar used to freeze.
        {
            let c = tab.panes.iter_mut().find(|p| p.id == "claude").unwrap();
            c.apply_status_transition(TabStatus::Waiting, false);
        }
        assert_eq!(
            tab.status(),
            TabStatus::Waiting,
            "Sidebar dot MUST match toolbar dot: both flip to waiting."
        );
        assert!(
            !tab.waiting_acknowledged(),
            "User is not on the Claude pane, so the pulse must not be suppressed."
        );
    }

    #[test]
    fn status_dot_active_claude_pane_acks_waiting() {
        // R13/R15: the OSC-title decoding + paneTitleChanged/activeTab wiring
        // that supplies `is_currently_being_viewed`.
        let mut tab = tab_with(vec![claude("claude")], Some("claude"));

        // Claude pane is active AND the tab is being viewed → waiting lands
        // already acknowledged.
        {
            let c = tab.panes.iter_mut().find(|p| p.id == "claude").unwrap();
            c.apply_status_transition(TabStatus::Waiting, true);
        }
        assert_eq!(tab.status(), TabStatus::Waiting);
        assert!(
            tab.waiting_acknowledged(),
            "Waiting that arrives while the user is on the Claude pane must land already-acked — no pulse."
        );
    }

    #[test]
    fn status_dot_sidebar_toolbar_agree_after_arbitrary_transitions() {
        // R13/R15: OSC-title decoding + paneTitleChanged wiring. The MODEL
        // invariant — tab.status() == Claude pane status at every step — is
        // what this pins.
        let mut tab = tab_with(vec![claude("claude"), terminal("term")], Some("claude"));

        // (active pane, new claude status, expected tab status)
        let steps = [
            ("claude", TabStatus::Thinking, TabStatus::Thinking),
            ("term", TabStatus::Waiting, TabStatus::Waiting),
            ("term", TabStatus::Thinking, TabStatus::Thinking),
            ("claude", TabStatus::Waiting, TabStatus::Waiting),
        ];

        for (active, new_status, expected) in steps {
            tab.active_pane_id = Some(active.to_string());
            let being_viewed = active == "claude";
            {
                let c = tab.panes.iter_mut().find(|p| p.id == "claude").unwrap();
                c.apply_status_transition(new_status, being_viewed);
            }
            let claude_status = tab.panes.iter().find(|p| p.id == "claude").unwrap().status;
            assert_eq!(tab.status(), expected, "tab.status (sidebar source) drifted");
            assert_eq!(
                tab.status(),
                claude_status,
                "tab.status must equal the Claude pane's status — sidebar and toolbar must read the same state."
            );
        }
    }

    // R13: test_createTabFromMainTerminal_hasExactlyOneClaudePane and
    // test_addPane_cannotCreateClaudePane drive the creation path
    // (createTabFromMainTerminal / addPane) — the ≤1-running-Claude creation
    // EDGE. The model half of the invariant (no struct-level uniqueness rule;
    // aggregation tolerates coexistence) is pinned by
    // `running_and_deferred_resume_claude_coexist_and_aggregate` below;
    // `add_pane` itself (only terminal-kind constructible) lives in `TabModel`
    // (`tab_model.rs`).

    // MARK: - Asymmetry 1 spot-probe (plan Validation §4a)

    #[test]
    fn running_and_deferred_resume_claude_coexist_and_aggregate() {
        // A running Claude and a deferred-resume Claude (is_claude_running ==
        // false) legitimately coexist in one tab transiently. Aggregation must
        // tolerate that without panicking, and the promotion-refusal predicate
        // must report the running one.
        let mut running = claude("running");
        running.is_claude_running = true;
        running.apply_status_transition(TabStatus::Thinking, false);

        let mut deferred = claude("deferred");
        deferred.is_claude_running = false; // pre-typed `claude --resume`, not yet entered
        deferred.apply_status_transition(TabStatus::Waiting, false);

        let tab = tab_with(vec![running, deferred], Some("running"));

        // Two alive Claude panes coexist — no struct-level uniqueness rule.
        assert_eq!(tab.live_claude_panes().count(), 2);
        // thinking > waiting: aggregation is defined and doesn't panic.
        assert_eq!(tab.status(), TabStatus::Thinking);
        // The documented promotion-refusal predicate reports the running Claude.
        assert!(
            tab.has_running_claude(),
            "A tab already hosting a running Claude must report the refusal condition."
        );
    }

    #[test]
    fn has_running_claude_false_with_only_deferred_resume() {
        let mut deferred = claude("deferred");
        deferred.is_claude_running = false;
        let tab = tab_with(vec![deferred], Some("deferred"));
        assert!(
            !tab.has_running_claude(),
            "A deferred-resume Claude (not yet running) must NOT trip the promotion-refusal predicate."
        );
    }

    // MARK: - recover_next_terminal_index (pure helper)
    //
    // Ported from PaneNamingTests.swift's `Tab.recoverNextTerminalIndex`
    // section. The addPane monotonic-numbering, renamePane, and hydration cases
    // exercise `TabModel` (`tab_model.rs` — `add_pane`, `rename_pane`); the
    // persistence round-trip cases are R18 (`PersistedTab`) — neither belongs to
    // these value types.

    #[test]
    fn recover_next_terminal_index_takes_max_plus_one() {
        assert_eq!(
            Tab::recover_next_terminal_index(&["Terminal 1", "Terminal 2", "logs"]),
            3
        );
    }

    #[test]
    fn recover_next_terminal_index_floors_at_one() {
        assert_eq!(
            Tab::recover_next_terminal_index(&["logs", "zsh"]),
            1,
            "No parseable Terminal-N titles → floor at 1."
        );
        assert_eq!(
            Tab::recover_next_terminal_index::<&str>(&[]),
            1,
            "Empty pane list → floor at 1."
        );
    }

    #[test]
    fn recover_next_terminal_index_case_and_whitespace_tolerant() {
        // The grammar is `^terminal\s+(\d+)$` case-insensitive.
        assert_eq!(
            Tab::recover_next_terminal_index(&["terminal 5"]),
            6,
            "Lowercase 'terminal' must parse."
        );
        assert_eq!(
            Tab::recover_next_terminal_index(&["Terminal   7"]),
            8,
            "Multiple spaces between 'Terminal' and the number must parse."
        );
        assert_eq!(
            Tab::recover_next_terminal_index(&["Terminal42"]),
            1,
            "No whitespace between 'Terminal' and digits must NOT parse."
        );
    }
}
