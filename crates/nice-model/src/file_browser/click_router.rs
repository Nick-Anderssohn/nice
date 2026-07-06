//! `FileBrowserClickRouter` — the hand-rolled double-click detector + modifier
//! router for file-tree rows. There is no single Swift file to port; this
//! captures the interaction contract frozen in the R19 plan (Swift parity with
//! `FileBrowserView.swift:751-838`, the `FileTreeRow.handleTap` model).
//!
//! Why hand-rolled and not gpui's native `click_count`: the 280 ms window is
//! deliberate (a plain first click fires its single action **immediately** so
//! expand/collapse feels instant — gpui's native disambig delay makes it feel
//! laggy), and the detector's per-selection `activated_at` stamp is the hook
//! R20's slow-second-click rename trigger feeds into
//! [`crate::rename_gate::InlineRenameClickGate`]. **This slice builds the hook,
//! not the rename** (R20 owns the rename scheduling).
//!
//! Routing (`modifiers` intersected to the relevant mask upstream):
//! * **⌘** → [`ClickAction::Toggle`] — adjust selection only; never
//!   expand/open, never touch the double-click state or `activated_at`.
//! * **⇧** → [`ClickAction::Extend`] — range-select only; the caller supplies
//!   `visible_order` to [`crate::file_browser::selection::FileBrowserSelection::extend`].
//! * **plain**, first click on a path (or after the window lapses / a different
//!   path) → [`ClickAction::SingleActivate`]: the caller replaces the selection
//!   with this row, and the router stamps `activated_at = now`. The caller then
//!   runs the primary action (folder → toggle expansion; file → no-op).
//! * **plain**, second click on the **same** path within 280 ms →
//!   [`ClickAction::DoubleActivate`]: the caller runs the double action (folder
//!   → re-root; file → open). The detector then resets so a third click starts
//!   a fresh single.
//!
//! The clock is injected — callers pass `now: Instant` (the injected-clock
//! idiom [`crate::rename_gate::InlineRenameClickGate`] already uses), keeping
//! the router a pure, testable value type.

use std::time::{Duration, Instant};

/// The deliberate double-click window (frozen — PROTECTED in the R19 plan).
pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(280);

/// The modifier bucket a click arrived with. The view collapses the raw
/// modifier mask to one of these (⌘ takes precedence over ⇧, matching the
/// Swift order that checks `.command` before `.shift`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickModifier {
    Plain,
    Command,
    Shift,
}

/// What the caller should do in response to a routed click. The router owns the
/// timing/stamp bookkeeping; the caller applies the effect to the selection +
/// tree state (which the router deliberately does not hold).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickAction {
    /// ⌘-click: toggle `path` in the selection.
    Toggle { path: String },
    /// ⇧-click: extend the selection through `path` using the visible order.
    Extend { path: String },
    /// Plain first click: replace the selection with `path`, then run the
    /// primary action (folder → toggle expansion; file → no-op). The router has
    /// stamped `activated_at = now`.
    SingleActivate { path: String },
    /// Plain second click on the same path within the window: run the double
    /// action (folder → re-root; file → open).
    DoubleActivate { path: String },
}

/// The hand-rolled detector (see the module docs). Construct with
/// [`FileBrowserClickRouter::new`]; drive it with [`FileBrowserClickRouter::route`].
#[derive(Debug, Default)]
pub struct FileBrowserClickRouter {
    /// The path + time of the last plain click, for same-path double detection.
    last_plain: Option<(String, Instant)>,
    /// The path that was last single-activated and when — the R20 rename hook.
    activated: Option<(String, Instant)>,
}

impl FileBrowserClickRouter {
    /// A fresh router with no prior clicks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Route a click on `path` arriving with `modifier` at time `now`. Mutates
    /// the detector's timing/stamp state and returns the [`ClickAction`] the
    /// caller should apply. See the module docs for the full contract.
    pub fn route(&mut self, path: &str, modifier: ClickModifier, now: Instant) -> ClickAction {
        match modifier {
            // ⌘ / ⇧ only adjust selection — they never touch the double-click
            // window or `activated_at` (Swift returns early before the
            // plain-click path).
            ClickModifier::Command => ClickAction::Toggle {
                path: path.to_string(),
            },
            ClickModifier::Shift => ClickAction::Extend {
                path: path.to_string(),
            },
            ClickModifier::Plain => {
                let is_double = matches!(
                    &self.last_plain,
                    Some((p, t))
                        if p == path && now.saturating_duration_since(*t) < DOUBLE_CLICK_WINDOW
                );
                if is_double {
                    // Reset so a third click starts a fresh single (Swift sets
                    // `lastTapTime = .distantPast`).
                    self.last_plain = None;
                    ClickAction::DoubleActivate {
                        path: path.to_string(),
                    }
                } else {
                    self.last_plain = Some((path.to_string(), now));
                    self.activated = Some((path.to_string(), now));
                    ClickAction::SingleActivate {
                        path: path.to_string(),
                    }
                }
            }
        }
    }

    /// The `activated_at` stamp for `path`, if `path` was the most recently
    /// single-activated row. This is the R20 hook: R20 feeds it into
    /// [`crate::rename_gate::InlineRenameClickGate::can_begin_edit`] to gate the
    /// slow-second-click rename. Returns `None` for any other path.
    pub fn activated_at(&self, path: &str) -> Option<Instant> {
        match &self.activated {
            Some((p, t)) if p == path => Some(*t),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn command_routes_to_toggle_and_leaves_timing_untouched() {
        let mut r = FileBrowserClickRouter::new();
        let now = t0();
        assert_eq!(
            r.route("/a", ClickModifier::Command, now),
            ClickAction::Toggle { path: "/a".into() }
        );
        // ⌘ must not stamp activated_at or arm the double-click window.
        assert_eq!(r.activated_at("/a"), None);
        assert_eq!(
            r.route("/a", ClickModifier::Plain, now),
            ClickAction::SingleActivate { path: "/a".into() },
            "a ⌘-click must not have armed a double-click window"
        );
    }

    #[test]
    fn shift_routes_to_extend() {
        let mut r = FileBrowserClickRouter::new();
        assert_eq!(
            r.route("/a", ClickModifier::Shift, t0()),
            ClickAction::Extend { path: "/a".into() }
        );
    }

    #[test]
    fn plain_first_click_single_activates_and_stamps_activated_at() {
        let mut r = FileBrowserClickRouter::new();
        let now = t0();
        assert_eq!(
            r.route("/a", ClickModifier::Plain, now),
            ClickAction::SingleActivate { path: "/a".into() }
        );
        assert_eq!(
            r.activated_at("/a"),
            Some(now),
            "single-activate must stamp activated_at for the R20 rename gate"
        );
        assert_eq!(r.activated_at("/b"), None, "stamp is per-path");
    }

    #[test]
    fn plain_second_click_same_path_within_window_double_activates() {
        let mut r = FileBrowserClickRouter::new();
        let now = t0();
        r.route("/a", ClickModifier::Plain, now);
        let second = now + Duration::from_millis(100);
        assert_eq!(
            r.route("/a", ClickModifier::Plain, second),
            ClickAction::DoubleActivate { path: "/a".into() }
        );
    }

    #[test]
    fn plain_second_click_after_window_single_activates_again() {
        let mut r = FileBrowserClickRouter::new();
        let now = t0();
        r.route("/a", ClickModifier::Plain, now);
        let late = now + DOUBLE_CLICK_WINDOW + Duration::from_millis(1);
        assert_eq!(
            r.route("/a", ClickModifier::Plain, late),
            ClickAction::SingleActivate { path: "/a".into() },
            "a second click past the 280 ms window is a fresh single, not a double"
        );
        assert_eq!(r.activated_at("/a"), Some(late), "the re-single re-stamps");
    }

    #[test]
    fn plain_second_click_different_path_within_window_single_activates() {
        let mut r = FileBrowserClickRouter::new();
        let now = t0();
        r.route("/a", ClickModifier::Plain, now);
        let second = now + Duration::from_millis(50);
        assert_eq!(
            r.route("/b", ClickModifier::Plain, second),
            ClickAction::SingleActivate { path: "/b".into() },
            "a fast second click on a DIFFERENT row is a single, not a double"
        );
    }

    #[test]
    fn double_resets_so_third_click_is_single() {
        let mut r = FileBrowserClickRouter::new();
        let now = t0();
        r.route("/a", ClickModifier::Plain, now);
        r.route("/a", ClickModifier::Plain, now + Duration::from_millis(100));
        // Third fast click must NOT be read as another double.
        assert_eq!(
            r.route("/a", ClickModifier::Plain, now + Duration::from_millis(150)),
            ClickAction::SingleActivate { path: "/a".into() },
            "the double reset the detector; a third click starts fresh"
        );
    }
}
