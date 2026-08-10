//! Busy-window close-confirmation copy — the Rust twin of Swift's
//! `CloseRequestCoordinator.describe(pane:)` (`CloseRequestCoordinator.swift:
//! 281-286`) and `AppShellView.pendingCloseMessage` / `runningPrefix`
//! (`AppShellView.swift:178-211`), plus the alert chrome itself
//! (`AppShellView.swift:350-362`).
//!
//! This is a DISTINCT system from [`crate::lifecycle`]'s R18 quit/window-close
//! copy (D0/D6): different strings, different buttons ("Force quit" vs
//! "Quit"/"Close"), different counting (busy windows, not every alive window) —
//! co-locating the two would blur two contracts. Unlike `lifecycle`'s
//! `quit_dialog_copy` / `close_dialog_copy` (title + confirm label vary by
//! caller), R20.5's title and both button labels are constant across every
//! scope, so they are plain constants here rather than fields threaded
//! through a per-scope builder.
//!
//! Only the pure, table-tested copy lives here: [`describe`] (per-window text)
//! and the four scope message builders. The busy CLASSIFICATION (which windows
//! count as busy) reads both the model and `PtyManager` and lives on
//! `WindowState::request_close_*` (D6) — a later slice, not this module.

// Consumed by `WindowState::request_close_*` (D1/D6, a later slice) when it
// assembles the `present_confirmation` call for a busy close; not wired to
// any caller yet. The pure builders below are exercised by this module's
// `#[test]`s in the meantime.
#![allow(dead_code)]

use nice_model::{TermWindow, TermWindowKind};

/// The alert title — constant across every busy-close scope (Swift's
/// `AppShellView.swift:351`).
pub(crate) const TITLE: &str = "Processes are still running";

/// The confirm button's label — destructive/red (`destructive_confirm =
/// true`, D8). Swift's `AppShellView.swift:359`.
pub(crate) const CONFIRM_LABEL: &str = "Force quit";

/// The cancel button's label. Swift's `AppShellView.swift:358`.
pub(crate) const CANCEL_LABEL: &str = "Cancel";

/// One busy window's description for the alert body — Swift's
/// `CloseRequestCoordinator.describe(pane:)` (`:281-286`). A Claude window is
/// prefixed `"Claude (…)"`; a terminal window's title is used bare (its
/// `status` is meaningless — see D-BUSY §1).
pub(crate) fn describe(term_window: &TermWindow) -> String {
    match term_window.kind {
        TermWindowKind::Claude => format!("Claude ({})", term_window.title),
        TermWindowKind::Terminal => term_window.title.clone(),
    }
}

/// The shared "X is still running." / "These are still running: X, Y."
/// prefix used by the three singular scopes — Swift's `runningPrefix(_:
/// joiner:)` (`AppShellView.swift:206-211`), always called with `joiner =
/// ", "`.
fn running_prefix(busy: &[String]) -> String {
    let list = busy.join(", ");
    if busy.len() == 1 {
        format!("{list} is still running.")
    } else {
        format!("These are still running: {list}.")
    }
}

/// The `.pane` scope's alert body (Swift `AppShellView.swift:180-182`).
pub(crate) fn window_message(busy: &[String]) -> String {
    format!(
        "{} Closing this window will force it to quit.",
        running_prefix(busy)
    )
}

/// The `.tab` scope's alert body (Swift `AppShellView.swift:183-185`).
pub(crate) fn session_message(busy: &[String]) -> String {
    format!(
        "{} Closing this session will force everything in it to quit.",
        running_prefix(busy)
    )
}

/// The `.project` scope's alert body (Swift `AppShellView.swift:186-188`).
pub(crate) fn project_message(busy: &[String]) -> String {
    format!(
        "{} Closing this project will force every session in it to quit.",
        running_prefix(busy)
    )
}

/// One busy session's per-line summary inside the `.tabs` list — Swift's
/// `BusyTabEntry.summary` (`CloseRequestCoordinator.swift:198-211`):
/// `"<SessionTitle> (<Window1>, <Window2>)"`, the session's busy windows already
/// `describe`d and comma+space joined inside the parens.
pub(crate) fn busy_session_summary(title: &str, busy_windows: &[String]) -> String {
    format!("{title} ({})", busy_windows.join(", "))
}

/// The `.tabs` (multi-select) scope's alert body — a vertical list of
/// per-session summaries (Swift `AppShellView.swift:189-199`). `session_summaries`
/// are each a [`busy_session_summary`] line, one per busy session in the batch, in
/// batch order. `.tabs` always has `len() >= 2` in practice (single-id
/// degrades to `.tab`, D5/§T.1), but the `n == 1` lead wording is kept
/// defensively since Swift also branches on it.
pub(crate) fn sessions_message(session_summaries: &[String]) -> String {
    let n = session_summaries.len();
    let lead = if n == 1 {
        "1 session is busy:".to_string()
    } else {
        format!("{n} sessions are busy:")
    };
    format!(
        "{lead}\n{}\nClosing them will force everything in them to quit.",
        session_summaries.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - describe (per-window description)

    #[test]
    fn describe_claude_window_prefixes_claude() {
        let term_window = TermWindow::new("p1", "auth-refactor", TermWindowKind::Claude);
        assert_eq!(describe(&term_window), "Claude (auth-refactor)");
    }

    #[test]
    fn describe_terminal_window_is_bare_title() {
        let term_window = TermWindow::new("p2", "npm run dev", TermWindowKind::Terminal);
        assert_eq!(describe(&term_window), "npm run dev");
    }

    // MARK: - running_prefix / singular-scope tails (§2, VERBATIM)

    #[test]
    fn window_message_singular_item() {
        let busy = vec!["Claude (auth-refactor)".to_string()];
        assert_eq!(
            window_message(&busy),
            "Claude (auth-refactor) is still running. Closing this window will force it to quit."
        );
    }

    #[test]
    fn window_message_multiple_items_lists_them() {
        let busy = vec![
            "Claude (auth-refactor)".to_string(),
            "npm run dev".to_string(),
        ];
        assert_eq!(
            window_message(&busy),
            "These are still running: Claude (auth-refactor), npm run dev. \
             Closing this window will force it to quit."
        );
    }

    #[test]
    fn session_message_singular_and_plural_tail() {
        let one = vec!["Claude (auth-refactor)".to_string()];
        assert_eq!(
            session_message(&one),
            "Claude (auth-refactor) is still running. \
             Closing this session will force everything in it to quit."
        );

        let two = vec!["Claude (a)".to_string(), "Claude (b)".to_string()];
        assert_eq!(
            session_message(&two),
            "These are still running: Claude (a), Claude (b). \
             Closing this session will force everything in it to quit."
        );
    }

    #[test]
    fn project_message_singular_and_plural_tail() {
        let one = vec!["npm run dev".to_string()];
        assert_eq!(
            project_message(&one),
            "npm run dev is still running. \
             Closing this project will force every session in it to quit."
        );

        let two = vec!["npm run dev".to_string(), "Claude (b)".to_string()];
        assert_eq!(
            project_message(&two),
            "These are still running: npm run dev, Claude (b). \
             Closing this project will force every session in it to quit."
        );
    }

    // MARK: - .tabs vertical list + BusyTabEntry-style summary (§2)

    #[test]
    fn busy_session_summary_joins_windows_in_parens() {
        assert_eq!(
            busy_session_summary("my-project", &["Claude (auth-refactor)".to_string()]),
            "my-project (Claude (auth-refactor))"
        );
        assert_eq!(
            busy_session_summary(
                "my-project",
                &["Claude (a)".to_string(), "npm run dev".to_string()]
            ),
            "my-project (Claude (a), npm run dev)"
        );
    }

    #[test]
    fn sessions_message_n_eq_1_lead_is_singular() {
        let summaries = vec!["my-project (Claude (a))".to_string()];
        assert_eq!(
            sessions_message(&summaries),
            "1 session is busy:\nmy-project (Claude (a))\n\
             Closing them will force everything in them to quit."
        );
    }

    #[test]
    fn sessions_message_n_ge_2_lead_counts_and_lists_each_session_on_its_own_line() {
        let summaries = vec![
            "my-project (Claude (a))".to_string(),
            "other-project (npm run dev)".to_string(),
        ];
        assert_eq!(
            sessions_message(&summaries),
            "2 sessions are busy:\nmy-project (Claude (a))\nother-project (npm run dev)\n\
             Closing them will force everything in them to quit."
        );
    }

    // MARK: - constants (§2)

    #[test]
    fn dialog_chrome_constants_are_verbatim() {
        assert_eq!(TITLE, "Processes are still running");
        assert_eq!(CONFIRM_LABEL, "Force quit");
        assert_eq!(CANCEL_LABEL, "Cancel");
    }
}
