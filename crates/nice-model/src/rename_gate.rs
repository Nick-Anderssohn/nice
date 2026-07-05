//! `InlineRenameClickGate` — ported from
//! `Sources/Nice/State/InlineRenameClickGate.swift`. The pure "click an active
//! row's title to enter rename mode" gate shared by the sidebar `TabRow` and
//! the toolbar pane pill (R11 reuses it).
//!
//! The rule: the row must be active **and** at least `double_click_interval`
//! must have elapsed since it became active — so the same click that selects a
//! row can't also start a rename. Extracted so the boundary (`>=` vs `>`) is
//! unit-tested without driving the real UI.
//!
//! The clock is injected: callers pass `activated_at` and `now` explicitly
//! (the Swift `now: Date` parameter), keeping the gate a pure function.

use std::time::{Duration, Instant};

/// Namespace for the click-to-rename time gate (the caseless-enum analog of the
/// Swift `enum InlineRenameClickGate`).
pub struct InlineRenameClickGate;

impl InlineRenameClickGate {
    /// Whether a click on an active row's title may begin an inline rename.
    /// `false` when the row was never activated (`activated_at == None`);
    /// otherwise `true` iff `now - activated_at >= double_click_interval`. The
    /// boundary uses `>=`, so exactly the interval allows the edit. A `now`
    /// earlier than `activated_at` yields a saturated-to-zero elapsed time and
    /// therefore `false`, matching the Swift `>=` on a negative interval
    /// (`InlineRenameClickGate.swift:17-24`).
    pub fn can_begin_edit(
        activated_at: Option<Instant>,
        now: Instant,
        double_click_interval: Duration,
    ) -> bool {
        match activated_at {
            None => false,
            Some(activated_at) => {
                now.saturating_duration_since(activated_at) >= double_click_interval
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_millis(500);

    #[test]
    fn nil_activated_at_disallows_edit() {
        assert!(
            !InlineRenameClickGate::can_begin_edit(None, Instant::now(), INTERVAL),
            "A row that has not been activated must not allow rename."
        );
    }

    #[test]
    fn fresh_activation_disallows_edit() {
        let now = Instant::now();
        assert!(
            !InlineRenameClickGate::can_begin_edit(Some(now), now, INTERVAL),
            "Same-instant activation must not enter edit (catches the 'click that \
             selects also renames' bug)."
        );
    }

    #[test]
    fn just_under_interval_disallows_edit() {
        let activated_at = Instant::now();
        let now = activated_at + (INTERVAL - Duration::from_millis(1));
        assert!(
            !InlineRenameClickGate::can_begin_edit(Some(activated_at), now, INTERVAL),
            "Less than the double-click interval must not enter edit."
        );
    }

    #[test]
    fn exactly_at_interval_allows_edit() {
        let activated_at = Instant::now();
        let now = activated_at + INTERVAL;
        assert!(
            InlineRenameClickGate::can_begin_edit(Some(activated_at), now, INTERVAL),
            "The boundary uses `>=`, so exactly the interval must allow edit."
        );
    }

    #[test]
    fn past_interval_allows_edit() {
        let activated_at = Instant::now();
        let now = activated_at + INTERVAL * 2;
        assert!(InlineRenameClickGate::can_begin_edit(
            Some(activated_at),
            now,
            INTERVAL
        ));
    }
}
