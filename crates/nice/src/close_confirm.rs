//! Busy-pane close-confirmation copy — the Rust twin of Swift's
//! `CloseRequestCoordinator.describe(pane:)` (`CloseRequestCoordinator.swift:
//! 281-286`) and `AppShellView.pendingCloseMessage` / `runningPrefix`
//! (`AppShellView.swift:178-211`), plus the alert chrome itself
//! (`AppShellView.swift:350-362`).
//!
//! This is a DISTINCT system from [`crate::lifecycle`]'s R18 quit/window-close
//! copy (D0/D6): different strings, different buttons ("Force quit" vs
//! "Quit"/"Close"), different counting (busy panes, not every alive pane) —
//! co-locating the two would blur two contracts. Unlike `lifecycle`'s
//! `quit_dialog_copy` / `close_dialog_copy` (title + confirm label vary by
//! caller), R20.5's title and both button labels are constant across every
//! scope, so they are plain constants here rather than fields threaded
//! through a per-scope builder.
//!
//! Only the pure, table-tested copy lives here: [`describe`] (per-pane text)
//! and the four scope message builders. The busy CLASSIFICATION (which panes
//! count as busy) reads both the model and `SessionManager` and lives on
//! `WindowState::request_close_*` (D6) — a later slice, not this module.

// Consumed by `WindowState::request_close_*` (D1/D6, a later slice) when it
// assembles the `present_confirmation` call for a busy close; not wired to
// any caller yet. The pure builders below are exercised by this module's
// `#[test]`s in the meantime.
#![allow(dead_code)]

use nice_model::{Pane, PaneKind};

/// The alert title — constant across every busy-close scope (Swift's
/// `AppShellView.swift:351`).
pub(crate) const TITLE: &str = "Processes are still running";

/// The confirm button's label — destructive/red (`destructive_confirm =
/// true`, D8). Swift's `AppShellView.swift:359`.
pub(crate) const CONFIRM_LABEL: &str = "Force quit";

/// The cancel button's label. Swift's `AppShellView.swift:358`.
pub(crate) const CANCEL_LABEL: &str = "Cancel";

/// One busy pane's description for the alert body — Swift's
/// `CloseRequestCoordinator.describe(pane:)` (`:281-286`). A Claude pane is
/// prefixed `"Claude (…)"`; a terminal pane's title is used bare (its
/// `status` is meaningless — see D-BUSY §1).
pub(crate) fn describe(pane: &Pane) -> String {
    match pane.kind {
        PaneKind::Claude => format!("Claude ({})", pane.title),
        PaneKind::Terminal => pane.title.clone(),
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
pub(crate) fn pane_message(busy: &[String]) -> String {
    format!(
        "{} Closing this pane will force it to quit.",
        running_prefix(busy)
    )
}

/// The `.tab` scope's alert body (Swift `AppShellView.swift:183-185`).
pub(crate) fn tab_message(busy: &[String]) -> String {
    format!(
        "{} Closing this tab will force everything in it to quit.",
        running_prefix(busy)
    )
}

/// The `.project` scope's alert body (Swift `AppShellView.swift:186-188`).
pub(crate) fn project_message(busy: &[String]) -> String {
    format!(
        "{} Closing this project will force every tab in it to quit.",
        running_prefix(busy)
    )
}

/// One busy tab's per-line summary inside the `.tabs` list — Swift's
/// `BusyTabEntry.summary` (`CloseRequestCoordinator.swift:198-211`):
/// `"<TabTitle> (<Pane1>, <Pane2>)"`, the tab's busy panes already
/// `describe`d and comma+space joined inside the parens.
pub(crate) fn busy_tab_summary(title: &str, busy_panes: &[String]) -> String {
    format!("{title} ({})", busy_panes.join(", "))
}

/// The `.tabs` (multi-select) scope's alert body — a vertical list of
/// per-tab summaries (Swift `AppShellView.swift:189-199`). `tab_summaries`
/// are each a [`busy_tab_summary`] line, one per busy tab in the batch, in
/// batch order. `.tabs` always has `len() >= 2` in practice (single-id
/// degrades to `.tab`, D5/§T.1), but the `n == 1` lead wording is kept
/// defensively since Swift also branches on it.
pub(crate) fn tabs_message(tab_summaries: &[String]) -> String {
    let n = tab_summaries.len();
    let lead = if n == 1 {
        "1 tab is busy:".to_string()
    } else {
        format!("{n} tabs are busy:")
    };
    format!(
        "{lead}\n{}\nClosing them will force everything in them to quit.",
        tab_summaries.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - describe (per-pane description)

    #[test]
    fn describe_claude_pane_prefixes_claude() {
        let pane = Pane::new("p1", "auth-refactor", PaneKind::Claude);
        assert_eq!(describe(&pane), "Claude (auth-refactor)");
    }

    #[test]
    fn describe_terminal_pane_is_bare_title() {
        let pane = Pane::new("p2", "npm run dev", PaneKind::Terminal);
        assert_eq!(describe(&pane), "npm run dev");
    }

    // MARK: - running_prefix / singular-scope tails (§2, VERBATIM)

    #[test]
    fn pane_message_singular_item() {
        let busy = vec!["Claude (auth-refactor)".to_string()];
        assert_eq!(
            pane_message(&busy),
            "Claude (auth-refactor) is still running. Closing this pane will force it to quit."
        );
    }

    #[test]
    fn pane_message_multiple_items_lists_them() {
        let busy = vec![
            "Claude (auth-refactor)".to_string(),
            "npm run dev".to_string(),
        ];
        assert_eq!(
            pane_message(&busy),
            "These are still running: Claude (auth-refactor), npm run dev. \
             Closing this pane will force it to quit."
        );
    }

    #[test]
    fn tab_message_singular_and_plural_tail() {
        let one = vec!["Claude (auth-refactor)".to_string()];
        assert_eq!(
            tab_message(&one),
            "Claude (auth-refactor) is still running. \
             Closing this tab will force everything in it to quit."
        );

        let two = vec!["Claude (a)".to_string(), "Claude (b)".to_string()];
        assert_eq!(
            tab_message(&two),
            "These are still running: Claude (a), Claude (b). \
             Closing this tab will force everything in it to quit."
        );
    }

    #[test]
    fn project_message_singular_and_plural_tail() {
        let one = vec!["npm run dev".to_string()];
        assert_eq!(
            project_message(&one),
            "npm run dev is still running. \
             Closing this project will force every tab in it to quit."
        );

        let two = vec!["npm run dev".to_string(), "Claude (b)".to_string()];
        assert_eq!(
            project_message(&two),
            "These are still running: npm run dev, Claude (b). \
             Closing this project will force every tab in it to quit."
        );
    }

    // MARK: - .tabs vertical list + BusyTabEntry-style summary (§2)

    #[test]
    fn busy_tab_summary_joins_panes_in_parens() {
        assert_eq!(
            busy_tab_summary("my-project", &["Claude (auth-refactor)".to_string()]),
            "my-project (Claude (auth-refactor))"
        );
        assert_eq!(
            busy_tab_summary(
                "my-project",
                &["Claude (a)".to_string(), "npm run dev".to_string()]
            ),
            "my-project (Claude (a), npm run dev)"
        );
    }

    #[test]
    fn tabs_message_n_eq_1_lead_is_singular() {
        let summaries = vec!["my-project (Claude (a))".to_string()];
        assert_eq!(
            tabs_message(&summaries),
            "1 tab is busy:\nmy-project (Claude (a))\n\
             Closing them will force everything in them to quit."
        );
    }

    #[test]
    fn tabs_message_n_ge_2_lead_counts_and_lists_each_tab_on_its_own_line() {
        let summaries = vec![
            "my-project (Claude (a))".to_string(),
            "other-project (npm run dev)".to_string(),
        ];
        assert_eq!(
            tabs_message(&summaries),
            "2 tabs are busy:\nmy-project (Claude (a))\nother-project (npm run dev)\n\
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
