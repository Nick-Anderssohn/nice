//! `Session` — a session (one sidebar row) — ported from
//! `Sources/Nice/State/Models.swift`. Owns an ordered list of [`TermWindow`]s and
//! derives its sidebar-dot status purely from them.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::term_window::{TermWindow, SessionStatus};

/// A session (one sidebar row). Fields mirror `Models.swift`'s `Tab`. Construct
/// with [`Session::new`] and set `windows` / `active_window_id` directly.
///
/// Phase R renamed the fields but not their serialized keys: `windows`,
/// `active_window_id` and `parent_session_id` carry `#[serde(rename = "...")]`
/// pinning the pre-rename spellings.
///
/// `Session.branch` (vestigial, roadmap M5) is deliberately **not** ported into
/// this struct.
///
/// **No `Eq`/`Hash`** — the windows carry `f32` split ratios since tmux-port
/// Phase 2; see [`TermWindow`]'s note. `PartialEq` stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    /// Load-bearing for `claude --resume`'s working dir. Two writers with
    /// different policies feed it (OSC 7 vs. the SessionStart hook —
    /// [`crate::WorkspaceModel::adopt_session_cwd`]); the field is defined here.
    pub cwd: String,
    /// Ordered windows shown as pills. Expected non-empty while the session is
    /// alive. Serialized as `panes` — frozen spelling.
    #[serde(rename = "panes")]
    pub windows: Vec<TermWindow>,
    /// The window currently focused. `None` only when `windows` is empty
    /// (transient, during teardown). Serialized as `active_pane_id` — frozen
    /// spelling.
    #[serde(rename = "active_pane_id")]
    pub active_window_id: Option<String>,
    /// The window that was active before [`active_window_id`](Self::active_window_id)
    /// — tmux `last-window`'s single "previous" slot, NOT an MRU stack. Written
    /// only by [`switch_active_window`](Self::switch_active_window) (the USER-switch
    /// paths: pill click, prev/next step, window-by-index, and the last-active
    /// bounce itself); structural/seed writes to `active_window_id` deliberately
    /// leave it alone. `#[serde(skip)]` — runtime-only, so no key appears in
    /// `sessions.json` (the [`crate::TermWindow::is_claude_running`] precedent).
    #[serde(skip)]
    pub prev_active_window_id: Option<String>,
    /// True once the session label was filled from Claude's OSC-set title rather
    /// than left as "New session".
    pub title_auto_generated: bool,
    /// True once the user has explicitly renamed this session. When true, auto
    /// titling skips this session so OSC-driven titles can't clobber the choice.
    pub title_manually_set: bool,
    /// UUID of the underlying Claude Code session, for `claude --resume`.
    /// `None` for terminal-only sessions (including everything in the Terminals
    /// group).
    pub claude_session_id: Option<String>,
    /// ID of a parent session in the same project that this session nests one indent
    /// under — the depth-1 lineage link. Set by the lineage paths
    /// ([`crate::WorkspaceModel::insert_branch_parent`] /
    /// [`crate::WorkspaceModel::insert_handoff_child`]); `None` for sessions neither path
    /// has placed into a lineage. The renderer maps a non-`None` value to
    /// exactly one indent level; depth never grows past one. Serialized as
    /// `parent_tab_id` — frozen spelling.
    #[serde(rename = "parent_tab_id")]
    pub parent_session_id: Option<String>,
    /// Monotonically incremented per-session counter feeding the auto-name
    /// "Terminal N". Never decremented when a window is closed, so a closed
    /// "Terminal 2" is not reused — the next add becomes "Terminal 4"
    /// (asymmetry 2). Persisted so it survives relaunch.
    pub next_terminal_index: u32,
}

impl Session {
    /// Construct a session with `Models.swift`'s default field values (no windows,
    /// no active window, auto/manual title flags off, no session id, no lineage
    /// parent, counter primed at 1).
    pub fn new(id: impl Into<String>, title: impl Into<String>, cwd: impl Into<String>) -> Self {
        Session {
            id: id.into(),
            title: title.into(),
            cwd: cwd.into(),
            windows: Vec::new(),
            active_window_id: None,
            prev_active_window_id: None,
            title_auto_generated: false,
            title_manually_set: false,
            claude_session_id: None,
            parent_session_id: None,
            next_terminal_index: 1,
        }
    }

    /// The alive Claude windows on this session — the only windows the sidebar dot
    /// derives from, so it can't drift from the toolbar pill.
    ///
    /// "Alive" is the Claude **pane**'s liveness, not the pill's
    /// ([`TermWindow::claude_pane_is_live`]): since Phase 2 a pill outlives its
    /// Claude pane whenever a shell pane beside it keeps running, and a dead
    /// Claude must stop lighting the sidebar the moment it dies. For a
    /// never-split pill the two are the same fact.
    fn live_claude_windows(&self) -> impl Iterator<Item = &TermWindow> {
        self.windows.iter().filter(|w| w.claude_pane_is_live())
    }

    /// The window whose split tree contains `pane_id`.
    ///
    /// A pane's events are routed by pane id alone, because break-pane can
    /// re-home a pane under a different pill between two subscription sweeps —
    /// so the owning window is resolved from the model at event time rather
    /// than captured when the subscription was made.
    pub fn window_for_pane(&self, pane_id: &str) -> Option<&TermWindow> {
        self.windows.iter().find(|w| w.layout.contains(pane_id))
    }

    /// True if any alive window on this session is a Claude window (`Models.swift:215`).
    pub fn has_claude(&self) -> bool {
        self.live_claude_windows().next().is_some()
    }

    /// Whether any *running* Claude occupies this session — the promotion-refusal
    /// predicate (asymmetry 1). When true, the next in-session `claude` reroutes
    /// into the current window instead of opening a new session; a running Claude and
    /// a deferred-resume Claude (`is_claude_running == false`) can coexist, and
    /// only the running one trips this. Keys on [`TermWindow::is_claude_running`],
    /// exactly like the Swift promotion guard (`SessionsModel.swift:855-859`).
    pub fn has_running_claude(&self) -> bool {
        self.live_claude_windows().any(|w| w.is_claude_running)
    }

    /// Point `active_window_id` at `term_window_id`, remembering the window it
    /// displaced in [`prev_active_window_id`](Self::prev_active_window_id) — the
    /// single choke point for a **user** window switch (tmux `last-window`
    /// semantics). Returns whether the active window actually moved.
    ///
    /// Selecting the already-active window is a no-op that does NOT rewrite the
    /// previous slot, so mashing the same pill can't erase the bounce target.
    /// Callers must have verified `term_window_id` is on this session — the helper
    /// does not re-check membership, because every call site already does (and
    /// `PtyManager::set_active_window` needs the found window anyway).
    ///
    /// Deliberately NOT used by structural / seed writes (add_window, lineage
    /// insert, rename repair, restore seeding): opening a window or restoring a
    /// saved layout is not a switch the user can bounce back from.
    pub fn switch_active_window(&mut self, term_window_id: &str) -> bool {
        if self.active_window_id.as_deref() == Some(term_window_id) {
            return false;
        }
        self.prev_active_window_id = self.active_window_id.take();
        self.active_window_id = Some(term_window_id.to_string());
        true
    }

    /// The bounce target for tmux `last-window`: the previously-active window's id,
    /// but only when that window is still on the session. A closed window's id is
    /// cleared eagerly by [`crate::WorkspaceModel::extract_window`]; this is the
    /// second line of defense for any other removal path.
    pub fn last_active_window_id(&self) -> Option<&str> {
        let id = self.prev_active_window_id.as_deref()?;
        self.windows.iter().any(|w| w.id == id).then_some(id)
    }

    /// The window currently focused, if any (`Models.swift:220-223`).
    pub fn active_window(&self) -> Option<&TermWindow> {
        let id = self.active_window_id.as_ref()?;
        self.windows.iter().find(|w| &w.id == id)
    }

    /// Whether any window in `offscreen_ids` currently needs attention — used by
    /// the toolbar's overflow chevron to badge itself when an attention-worthy
    /// window has scrolled out of view (`Models.swift:229-234`).
    pub fn has_offscreen_attention(&self, offscreen_ids: &HashSet<String>) -> bool {
        if offscreen_ids.is_empty() {
            return false;
        }
        self.windows
            .iter()
            .any(|w| offscreen_ids.contains(&w.id) && w.needs_attention())
    }

    /// Aggregate status shown in the sidebar dot. Derived from live Claude
    /// windows only (thinking > waiting > idle) so the sidebar can't drift from
    /// the toolbar pill. Written defensively for transient multi-window states
    /// during creation/teardown (`Models.swift:242-247`).
    pub fn status(&self) -> SessionStatus {
        if self.live_claude_windows().any(|w| w.status == SessionStatus::Thinking) {
            return SessionStatus::Thinking;
        }
        if self.live_claude_windows().any(|w| w.status == SessionStatus::Waiting) {
            return SessionStatus::Waiting;
        }
        SessionStatus::Idle
    }

    /// Sidebar-dot pulse suppression: true iff every waiting Claude window on the
    /// session has been acknowledged. Returns `false` when no Claude window is
    /// waiting — callers only read this while `status() == .waiting`, so the
    /// value is only meaningful then (`Models.swift:254-260`).
    pub fn waiting_acknowledged(&self) -> bool {
        let mut any_waiting = false;
        for window in self
            .live_claude_windows()
            .filter(|w| w.status == SessionStatus::Waiting)
        {
            any_waiting = true;
            if !window.waiting_acknowledged {
                return false;
            }
        }
        any_waiting
    }

    /// Recover `next_terminal_index` from a session's window titles when an older
    /// session file lacks the persisted counter. Parses each title against
    /// `^terminal\s+(\d+)$` (case-insensitive) and returns `1 + max(N)`,
    /// floored at 1 so a session whose terminal windows were all renamed still starts
    /// numbering from 1 (`Models.swift:199-212`).
    pub fn recover_next_terminal_index<S: AsRef<str>>(window_titles: &[S]) -> u32 {
        let max_n = window_titles
            .iter()
            .filter_map(|t| parse_terminal_index(t.as_ref()))
            .max()
            .unwrap_or(0);
        max_n.saturating_add(1).max(1)
    }
}

/// Parse a window title against `^terminal\s+(\d+)$` (case-insensitive),
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
    use crate::term_window::TermWindowKind;

    fn claude(id: &str) -> TermWindow {
        TermWindow::new(id, "Claude", TermWindowKind::Claude)
    }

    fn terminal(id: &str) -> TermWindow {
        TermWindow::new(id, "Terminal", TermWindowKind::Terminal)
    }

    fn session_with(windows: Vec<TermWindow>, active: Option<&str>) -> Session {
        let mut session = Session::new("t", "", "/");
        session.windows = windows;
        session.active_window_id = active.map(|s| s.to_string());
        session
    }

    // MARK: - Session aggregation
    //
    // Ported from PaneAcknowledgmentTests.swift's "Session aggregation" section.
    // `Session::status` / `Session::waiting_acknowledged` are pure functions of `windows`
    // — they never consult `active_window_id`. That is the invariant that keeps
    // the sidebar dot (Session-level) from drifting from the toolbar pill dot
    // (TermWindow-level) when the user focuses a non-Claude window.

    #[test]
    fn session_status_no_claude_window_is_idle() {
        let session = session_with(vec![terminal("p")], Some("p"));
        assert_eq!(session.status(), SessionStatus::Idle);
    }

    #[test]
    fn session_status_claude_thinking_is_thinking_regardless_of_active_window() {
        let mut c = claude("claude");
        c.apply_status_transition(SessionStatus::Thinking, false);

        let mut session = session_with(vec![c, terminal("term")], Some("claude"));
        assert_eq!(session.status(), SessionStatus::Thinking);

        session.active_window_id = Some("term".to_string());
        assert_eq!(
            session.status(),
            SessionStatus::Thinking,
            "Focusing the terminal must not freeze session.status on an old value."
        );
    }

    #[test]
    fn session_status_claude_waiting_is_waiting_regardless_of_active_window() {
        let mut c = claude("claude");
        c.apply_status_transition(SessionStatus::Waiting, false);

        let mut session = session_with(vec![c, terminal("term")], Some("term"));
        assert_eq!(
            session.status(),
            SessionStatus::Waiting,
            "Claude transitions to waiting while the companion terminal is active; sidebar must reflect waiting."
        );
        assert!(
            !session.waiting_acknowledged(),
            "User is not viewing the Claude window, so the pulse must not be suppressed."
        );

        session.active_window_id = Some("claude".to_string());
        assert_eq!(session.status(), SessionStatus::Waiting);
    }

    #[test]
    fn session_status_dead_claude_window_excluded() {
        let mut c = claude("claude");
        c.apply_status_transition(SessionStatus::Thinking, false);
        c.is_alive = false;
        let session = session_with(vec![c], Some("claude"));
        assert_eq!(
            session.status(),
            SessionStatus::Idle,
            "A dead Claude window must not keep the sidebar dot lit."
        );
    }

    #[test]
    fn session_waiting_acknowledged_waiting_acked_returns_true() {
        let mut c = claude("claude");
        c.apply_status_transition(SessionStatus::Waiting, true);
        assert!(c.waiting_acknowledged);

        let mut session = session_with(vec![c, terminal("term")], Some("claude"));
        assert!(session.waiting_acknowledged());

        // Flipping the active window to the terminal MUST NOT change the session's
        // acknowledgment — that was the sidebar/toolbar drift bug.
        session.active_window_id = Some("term".to_string());
        assert!(
            session.waiting_acknowledged(),
            "session.waiting_acknowledged is a pure function of windows; active-window selection must not mutate it."
        );
    }

    #[test]
    fn session_waiting_acknowledged_waiting_unacked_returns_false() {
        let mut c = claude("claude");
        c.apply_status_transition(SessionStatus::Waiting, false);
        let session = session_with(vec![c], Some("claude"));
        assert!(!session.waiting_acknowledged());
    }

    #[test]
    fn session_waiting_acknowledged_no_waiting_window_returns_false() {
        let mut c = claude("claude");
        c.apply_status_transition(SessionStatus::Thinking, true);
        let session = session_with(vec![c], Some("claude"));
        assert!(
            !session.waiting_acknowledged(),
            "No waiting window → acknowledgment is meaningless and reported as false."
        );
    }

    #[test]
    fn session_waiting_acknowledged_no_windows_is_false() {
        let session = session_with(vec![], None);
        assert!(!session.waiting_acknowledged());
        assert_eq!(session.status(), SessionStatus::Idle);
    }

    // MARK: - Status-dot aggregation (model half of AppStateStatusDotTests)
    //
    // Ported from AppStateStatusDotTests.swift. Those tests drive
    // `AppState.paneTitleChanged` / `setActivePane` — the OSC-title→status
    // ROUTING and the pty/session wiring are R13/R15, out of scope here. The
    // MODEL half is the sidebar/toolbar sync invariant: with the Claude window's
    // status driven directly via `apply_status_transition`, `session.status()`
    // always equals that window's status regardless of which window is active.

    #[test]
    fn status_dot_inactive_claude_window_tracks_thinking_then_waiting() {
        // R13/R15: the braille-prefix `\u{2800}` → thinking / `\u{2733}` →
        // waiting OSC-title decoding and the paneTitleChanged wiring.
        let mut session = session_with(vec![claude("claude"), terminal("term")], Some("claude"));

        // User focuses the terminal window — the setup for the original bug.
        session.active_window_id = Some("term".to_string());

        // Claude "emits" thinking while the terminal is active.
        {
            let c = session.windows.iter_mut().find(|w| w.id == "claude").unwrap();
            c.apply_status_transition(SessionStatus::Thinking, false);
        }
        assert_eq!(
            session.status(),
            SessionStatus::Thinking,
            "session.status must track the Claude window even when the companion terminal is active."
        );

        // Claude transitions to waiting — the moment the sidebar used to freeze.
        {
            let c = session.windows.iter_mut().find(|w| w.id == "claude").unwrap();
            c.apply_status_transition(SessionStatus::Waiting, false);
        }
        assert_eq!(
            session.status(),
            SessionStatus::Waiting,
            "Sidebar dot MUST match toolbar dot: both flip to waiting."
        );
        assert!(
            !session.waiting_acknowledged(),
            "User is not on the Claude window, so the pulse must not be suppressed."
        );
    }

    #[test]
    fn status_dot_active_claude_window_acks_waiting() {
        // R13/R15: the OSC-title decoding + paneTitleChanged/activeTab wiring
        // that supplies `is_currently_being_viewed`.
        let mut session = session_with(vec![claude("claude")], Some("claude"));

        // Claude window is active AND the session is being viewed → waiting lands
        // already acknowledged.
        {
            let c = session.windows.iter_mut().find(|w| w.id == "claude").unwrap();
            c.apply_status_transition(SessionStatus::Waiting, true);
        }
        assert_eq!(session.status(), SessionStatus::Waiting);
        assert!(
            session.waiting_acknowledged(),
            "Waiting that arrives while the user is on the Claude window must land already-acked — no pulse."
        );
    }

    #[test]
    fn status_dot_sidebar_toolbar_agree_after_arbitrary_transitions() {
        // R13/R15: OSC-title decoding + paneTitleChanged wiring. The MODEL
        // invariant — session.status() == Claude window status at every step — is
        // what this pins.
        let mut session = session_with(vec![claude("claude"), terminal("term")], Some("claude"));

        // (active window, new claude status, expected session status)
        let steps = [
            ("claude", SessionStatus::Thinking, SessionStatus::Thinking),
            ("term", SessionStatus::Waiting, SessionStatus::Waiting),
            ("term", SessionStatus::Thinking, SessionStatus::Thinking),
            ("claude", SessionStatus::Waiting, SessionStatus::Waiting),
        ];

        for (active, new_status, expected) in steps {
            session.active_window_id = Some(active.to_string());
            let being_viewed = active == "claude";
            {
                let c = session.windows.iter_mut().find(|w| w.id == "claude").unwrap();
                c.apply_status_transition(new_status, being_viewed);
            }
            let claude_status = session.windows.iter().find(|w| w.id == "claude").unwrap().status;
            assert_eq!(session.status(), expected, "session.status (sidebar source) drifted");
            assert_eq!(
                session.status(),
                claude_status,
                "session.status must equal the Claude window's status — sidebar and toolbar must read the same state."
            );
        }
    }

    // R13: test_createTabFromMainTerminal_hasExactlyOneClaudePane and
    // test_addPane_cannotCreateClaudePane drive the creation path
    // (createTabFromMainTerminal / addPane) — the ≤1-running-Claude creation
    // EDGE. The model half of the invariant (no struct-level uniqueness rule;
    // aggregation tolerates coexistence) is pinned by
    // `running_and_deferred_resume_claude_coexist_and_aggregate` below;
    // `add_window` itself (only terminal-kind constructible) lives in `WorkspaceModel`
    // (`workspace_model.rs`).

    // MARK: - Asymmetry 1 spot-probe (plan Validation §4a)

    #[test]
    fn running_and_deferred_resume_claude_coexist_and_aggregate() {
        // A running Claude and a deferred-resume Claude (is_claude_running ==
        // false) legitimately coexist in one session transiently. Aggregation must
        // tolerate that without panicking, and the promotion-refusal predicate
        // must report the running one.
        let mut running = claude("running");
        running.is_claude_running = true;
        running.apply_status_transition(SessionStatus::Thinking, false);

        let mut deferred = claude("deferred");
        deferred.is_claude_running = false; // pre-typed `claude --resume`, not yet entered
        deferred.apply_status_transition(SessionStatus::Waiting, false);

        let session = session_with(vec![running, deferred], Some("running"));

        // Two alive Claude windows coexist — no struct-level uniqueness rule.
        assert_eq!(session.live_claude_windows().count(), 2);
        // thinking > waiting: aggregation is defined and doesn't panic.
        assert_eq!(session.status(), SessionStatus::Thinking);
        // The documented promotion-refusal predicate reports the running Claude.
        assert!(
            session.has_running_claude(),
            "A session already hosting a running Claude must report the refusal condition."
        );
    }

    #[test]
    fn has_running_claude_false_with_only_deferred_resume() {
        let mut deferred = claude("deferred");
        deferred.is_claude_running = false;
        let session = session_with(vec![deferred], Some("deferred"));
        assert!(
            !session.has_running_claude(),
            "A deferred-resume Claude (not yet running) must NOT trip the promotion-refusal predicate."
        );
    }

    // MARK: - switch_active_window / prev_active_window_id (tmux `last-window`)

    #[test]
    fn switch_active_window_records_the_displaced_window() {
        let mut session = session_with(vec![terminal("a"), terminal("b")], Some("a"));
        assert_eq!(session.prev_active_window_id, None, "nothing displaced yet");

        assert!(session.switch_active_window("b"), "the active window moved");
        assert_eq!(session.active_window_id.as_deref(), Some("b"));
        assert_eq!(session.prev_active_window_id.as_deref(), Some("a"));
        assert_eq!(session.last_active_window_id(), Some("a"));
    }

    #[test]
    fn switch_active_window_to_the_same_window_is_a_noop() {
        let mut session = session_with(vec![terminal("a"), terminal("b")], Some("a"));
        session.switch_active_window("b");

        assert!(!session.switch_active_window("b"), "re-selecting b does not move");
        assert_eq!(
            session.prev_active_window_id.as_deref(),
            Some("a"),
            "mashing the active pill must not erase the bounce target"
        );
    }

    #[test]
    fn switch_active_window_bounces_between_the_last_two() {
        // tmux `last-window` semantics: a→b→(bounce)→a→(bounce)→b, forever.
        let mut session = session_with(vec![terminal("a"), terminal("b"), terminal("c")], Some("a"));
        session.switch_active_window("b");
        session.switch_active_window("c");
        assert_eq!(session.last_active_window_id(), Some("b"), "single slot, not an MRU stack");

        let target = session.last_active_window_id().unwrap().to_string();
        session.switch_active_window(&target);
        assert_eq!(session.active_window_id.as_deref(), Some("b"));
        assert_eq!(session.last_active_window_id(), Some("c"), "the bounce swaps the pair");

        let target = session.last_active_window_id().unwrap().to_string();
        session.switch_active_window(&target);
        assert_eq!(session.active_window_id.as_deref(), Some("c"));
        assert_eq!(session.last_active_window_id(), Some("b"));
    }

    #[test]
    fn last_active_window_id_skips_a_window_that_is_gone() {
        let mut session = session_with(vec![terminal("a"), terminal("b")], Some("a"));
        session.switch_active_window("b");
        session.windows.retain(|w| w.id != "a");
        assert_eq!(
            session.last_active_window_id(),
            None,
            "a stale previous id must never be offered as a bounce target"
        );
    }

    /// `prev_active_window_id` is runtime-only: `#[serde(skip)]` keeps it out of
    /// `sessions.json` entirely (no new persisted key), and a decode defaults it.
    #[test]
    fn prev_active_window_id_is_not_serialized() {
        let mut session = session_with(vec![terminal("a"), terminal("b")], Some("a"));
        session.switch_active_window("b");

        let json = serde_json::to_string(&session).expect("session serializes");
        assert!(
            !json.contains("prev_active"),
            "no prev-active key may appear in the persisted session: {json}"
        );

        let decoded: Session = serde_json::from_str(&json).expect("session round-trips");
        assert_eq!(decoded.active_window_id.as_deref(), Some("b"));
        assert_eq!(decoded.prev_active_window_id, None, "the slot defaults on load");
    }

    // MARK: - recover_next_terminal_index (pure helper)
    //
    // Ported from PaneNamingTests.swift's `Session.recoverNextTerminalIndex`
    // section. The addPane monotonic-numbering, renamePane, and hydration cases
    // exercise `WorkspaceModel` (`workspace_model.rs` — `add_window`, `rename_window`); the
    // persistence round-trip cases are R18 (`PersistedSession`) — neither belongs to
    // these value types.

    #[test]
    fn recover_next_terminal_index_takes_max_plus_one() {
        assert_eq!(
            Session::recover_next_terminal_index(&["Terminal 1", "Terminal 2", "logs"]),
            3
        );
    }

    #[test]
    fn recover_next_terminal_index_floors_at_one() {
        assert_eq!(
            Session::recover_next_terminal_index(&["logs", "zsh"]),
            1,
            "No parseable Terminal-N titles → floor at 1."
        );
        assert_eq!(
            Session::recover_next_terminal_index::<&str>(&[]),
            1,
            "Empty window list → floor at 1."
        );
    }

    #[test]
    fn recover_next_terminal_index_case_and_whitespace_tolerant() {
        // The grammar is `^terminal\s+(\d+)$` case-insensitive.
        assert_eq!(
            Session::recover_next_terminal_index(&["terminal 5"]),
            6,
            "Lowercase 'terminal' must parse."
        );
        assert_eq!(
            Session::recover_next_terminal_index(&["Terminal   7"]),
            8,
            "Multiple spaces between 'Terminal' and the number must parse."
        );
        assert_eq!(
            Session::recover_next_terminal_index(&["Terminal42"]),
            1,
            "No whitespace between 'Terminal' and digits must NOT parse."
        );
    }

    // MARK: - window_for_pane
    //
    // The pane-keyed event subscription resolves a pane's owning pill through
    // this at event time — a wrong answer routes a background/re-homed pane's
    // titles, cwd, and exit onto the wrong pill.

    #[test]
    fn window_for_pane_resolves_the_owning_window_after_a_split() {
        use crate::pane_layout::{Pane, SplitOrient};

        let mut session = session_with(vec![claude("claude"), terminal("term")], Some("claude"));
        let split = session.windows[1].layout.split(
            "term",
            SplitOrient::Beside,
            Pane::new("leaf-2", TermWindowKind::Terminal),
        );
        assert!(split, "the split target is the window's own single leaf");

        assert_eq!(
            session.window_for_pane("claude").map(|w| w.id.as_str()),
            Some("claude"),
            "a single-leaf window resolves via its own pane id"
        );
        assert_eq!(
            session.window_for_pane("term").map(|w| w.id.as_str()),
            Some("term")
        );
        assert_eq!(
            session.window_for_pane("leaf-2").map(|w| w.id.as_str()),
            Some("term"),
            "a split-added pane resolves to the window that contains it"
        );
        assert!(
            session.window_for_pane("ghost").is_none(),
            "a pane id absent from every tree resolves to no window"
        );
    }
}
