//! `PaneStripActions` — the select / close / add-terminal seam the toolbar pane
//! strip drives.
//!
//! The pane-level sibling of [`crate::sidebar_actions::SidebarActions`]: every
//! pill control that selects, closes, or adds a pane funnels through this trait
//! rather than reaching into the [`TabModel`] directly, so R13's swap from
//! "model-only" to "real sessions" is mechanical — replace the injected
//! implementation and nothing in the strip views changes (plan "Actions ride the
//! R10 seam" / "R13 rewires the seam without touching strip internals").
//!
//! ## R11 is model-only — nothing spawns
//!
//! [`ModelPaneStripActions`] mutates **only** the R8 value tree. Selecting a pane
//! moves the tab's `active_pane_id` (real focus / session activation is R13);
//! closing a pane routes through [`TabModel::extract_pane`], which re-points the
//! active pane to a neighbor and has **no** busy-pane close confirmation (that is
//! R18); adding a pane routes through [`TabModel::add_pane`], the R8 auto-naming
//! ("Terminal N", monotonic counter) — R13 spawns the pty behind it.

// The seam's in-crate caller is the toolbar view (`toolbar`); the trait's full
// method set is the R13 rewiring contract, so a method may have no live caller
// until a control that invokes it is wired. The model behavior below IS exercised
// by this module's tests.
#![allow(dead_code)]

use nice_model::TabModel;

/// The select / close / add-terminal actions the toolbar pill strip invokes. The
/// toolbar view owns a boxed instance; R13 swaps the implementation to drive real
/// sessions. Keeping it a single trait is what makes R13's rewiring mechanical.
pub(crate) trait PaneStripActions {
    /// Make `pane_id` the active pane of `tab_id`. Model-only: it moves
    /// `active_pane_id` (a no-op if the pane isn't on the tab, so selection can
    /// never dangle). R13 replaces this with real focus / session activation.
    fn select_pane(&mut self, model: &mut TabModel, tab_id: &str, pane_id: &str);

    /// Close `pane_id` on `tab_id` via the single [`TabModel::extract_pane`] entry
    /// point, which re-points `active_pane_id` to a neighbor when the closed pane
    /// was active. No busy-pane confirmation — that is R18.
    fn close_pane(&mut self, model: &mut TabModel, tab_id: &str, pane_id: &str);

    /// Append an auto-named terminal pane to `tab_id` and focus it via the R8
    /// [`TabModel::add_pane`] (the "Terminal N" monotonic counter). Returns the
    /// new pane id, or `None` if the tab is unknown. R13 spawns the pty.
    fn add_terminal_pane(&mut self, model: &mut TabModel, tab_id: &str) -> Option<String>;

    /// Move focus to the next pane within the active tab, **wrapping**. A no-op
    /// when the active tab has fewer than two panes (or has no active pane). The
    /// R12 ⌘⌥→ shortcut drives this; model-only — it moves the active tab's
    /// `active_pane_id`. R13 re-implements it behind this same name so real focus
    /// activation + the deferred-spawn acknowledgment ride along without touching
    /// callers. Ported from `SessionsModel.stepActivePane(by: +1)`.
    fn select_next_pane(&mut self, model: &mut TabModel);

    /// Move focus to the previous pane within the active tab, **wrapping**. The
    /// ⌘⌥← counterpart of [`select_next_pane`](Self::select_next_pane); same
    /// <2-panes no-op and same R13 rewiring contract. Ported from
    /// `SessionsModel.stepActivePane(by: -1)`.
    fn select_prev_pane(&mut self, model: &mut TabModel);
}

/// The R11 model-only [`PaneStripActions`] implementation. Holds a monotonic id
/// counter so the panes it mints get unique ids without a UUID dependency
/// (nothing here is persisted this cycle — R18 owns persistence). R13 replaces
/// this whole type with the session-spawning implementation.
#[derive(Default)]
pub(crate) struct ModelPaneStripActions {
    /// Monotonic counter feeding minted pane ids.
    next_id: u64,
}

impl ModelPaneStripActions {
    /// A fresh instance.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mint a unique id with the given prefix.
    fn mint(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    /// Wrapping step of the active tab's `active_pane_id` by `offset` panes. A
    /// no-op when there is no active tab, the active tab has fewer than two panes,
    /// or its `active_pane_id` is unset / not on the tab (selection can never
    /// dangle). Mirrors Swift's `SessionsModel.stepActivePane`
    /// (`((i + off) % n + n) % n`, expressed here with `rem_euclid`).
    fn step_active_pane(model: &mut TabModel, offset: isize) {
        let Some(tab_id) = model.active_tab_id().map(str::to_owned) else {
            return;
        };
        let Some((pi, ti)) = model.project_tab_index(&tab_id) else {
            return;
        };
        let tab = &model.projects[pi].tabs[ti];
        let count = tab.panes.len();
        if count < 2 {
            return;
        }
        let Some(active) = tab.active_pane_id.clone() else {
            return;
        };
        let Some(cur) = tab.panes.iter().position(|p| p.id == active) else {
            return;
        };
        let next = (cur as isize + offset).rem_euclid(count as isize) as usize;
        let next_id = tab.panes[next].id.clone();
        model.projects[pi].tabs[ti].active_pane_id = Some(next_id);
    }
}

impl PaneStripActions for ModelPaneStripActions {
    fn select_pane(&mut self, model: &mut TabModel, tab_id: &str, pane_id: &str) {
        let Some((pi, ti)) = model.project_tab_index(tab_id) else {
            return;
        };
        let tab = &mut model.projects[pi].tabs[ti];
        // Guard against selecting a pane that isn't on the tab — never leave a
        // dangling active_pane_id.
        if tab.panes.iter().any(|p| p.id == pane_id) {
            tab.active_pane_id = Some(pane_id.to_string());
        }
    }

    fn close_pane(&mut self, model: &mut TabModel, tab_id: &str, pane_id: &str) {
        model.extract_pane(pane_id, tab_id);
    }

    fn add_terminal_pane(&mut self, model: &mut TabModel, tab_id: &str) -> Option<String> {
        let pane_id = self.mint("pane");
        model.add_pane(tab_id, pane_id, None)
    }

    fn select_next_pane(&mut self, model: &mut TabModel) {
        Self::step_active_pane(model, 1);
    }

    fn select_prev_pane(&mut self, model: &mut TabModel) {
        Self::step_active_pane(model, -1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::{Pane, PaneKind, Tab, TabModel};

    /// The freshly-seeded model's Main terminal tab (one "Terminal 1" pane,
    /// `next_terminal_index = 2`, that pane active).
    fn seeded() -> TabModel {
        TabModel::new("/home/u")
    }

    fn main_tab_id() -> &'static str {
        TabModel::MAIN_TERMINAL_TAB_ID
    }

    #[test]
    fn add_terminal_pane_appends_auto_named_and_focuses_it() {
        let mut model = seeded();
        let mut actions = ModelPaneStripActions::new();

        let new_id = actions.add_terminal_pane(&mut model, main_tab_id()).unwrap();

        let tab = model.tab_for(main_tab_id()).unwrap();
        assert_eq!(tab.panes.len(), 2, "the new terminal pane was appended");
        let created = tab.panes.last().unwrap();
        assert_eq!(created.id, new_id);
        assert_eq!(created.kind, PaneKind::Terminal);
        // Seed consumed slot 1 ("Terminal 1"); the add takes slot 2.
        assert_eq!(created.title, "Terminal 2");
        assert_eq!(
            tab.active_pane_id.as_deref(),
            Some(new_id.as_str()),
            "the new pane is focused"
        );
        assert_eq!(tab.next_terminal_index, 3, "the monotonic counter advanced");
    }

    #[test]
    fn add_terminal_pane_unknown_tab_is_none() {
        let mut model = seeded();
        let mut actions = ModelPaneStripActions::new();
        assert!(actions.add_terminal_pane(&mut model, "nope").is_none());
    }

    #[test]
    fn select_pane_moves_active_when_the_pane_exists() {
        let mut model = seeded();
        let mut actions = ModelPaneStripActions::new();
        // Add a second pane so there's something else to select.
        let other = actions.add_terminal_pane(&mut model, main_tab_id()).unwrap();
        let first = model.tab_for(main_tab_id()).unwrap().panes[0].id.clone();

        // add_terminal_pane focused `other`; select the first pane back.
        actions.select_pane(&mut model, main_tab_id(), &first);
        assert_eq!(
            model.tab_for(main_tab_id()).unwrap().active_pane_id.as_deref(),
            Some(first.as_str())
        );

        actions.select_pane(&mut model, main_tab_id(), &other);
        assert_eq!(
            model.tab_for(main_tab_id()).unwrap().active_pane_id.as_deref(),
            Some(other.as_str())
        );
    }

    #[test]
    fn select_pane_unknown_pane_never_dangles_active() {
        let mut model = seeded();
        let mut actions = ModelPaneStripActions::new();
        let before = model.tab_for(main_tab_id()).unwrap().active_pane_id.clone();

        actions.select_pane(&mut model, main_tab_id(), "ghost-pane");

        assert_eq!(
            model.tab_for(main_tab_id()).unwrap().active_pane_id,
            before,
            "selecting a pane not on the tab must leave active_pane_id untouched"
        );
    }

    #[test]
    fn close_pane_removes_and_refocuses_a_neighbor() {
        // A tab with [a, b, c], b active; closing b re-points active to the pane
        // that slid into b's slot (c), per TabModel::neighbor_active_pane_id.
        let mut model = seeded();
        let pi = 0;
        let ti = 0;
        let mut tab = Tab::new("t2", "Two", "/home/u");
        tab.panes = vec![
            Pane::new("a", "A", PaneKind::Terminal),
            Pane::new("b", "B", PaneKind::Terminal),
            Pane::new("c", "C", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some("b".into());
        model.projects[pi].tabs.insert(ti, tab);
        let mut actions = ModelPaneStripActions::new();

        actions.close_pane(&mut model, "t2", "b");

        let tab = model.tab_for("t2").unwrap();
        assert_eq!(tab.panes.len(), 2);
        assert!(tab.panes.iter().all(|p| p.id != "b"), "b is gone");
        assert_eq!(
            tab.active_pane_id.as_deref(),
            Some("c"),
            "active re-points to the pane that slid into the freed slot"
        );
    }

    /// Seed a `[a, b, c]` three-pane tab (a active) as the active tab of the
    /// model, so pane-stepping has something to wrap over.
    fn seeded_three_pane() -> TabModel {
        let mut model = seeded();
        let mut tab = Tab::new("t3", "Three", "/home/u");
        tab.panes = vec![
            Pane::new("a", "A", PaneKind::Terminal),
            Pane::new("b", "B", PaneKind::Terminal),
            Pane::new("c", "C", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some("a".into());
        model.projects[0].tabs.insert(0, tab);
        model.select_tab("t3");
        model
    }

    fn active_pane(model: &TabModel) -> Option<String> {
        model
            .tab_for("t3")
            .and_then(|t| t.active_pane_id.clone())
    }

    #[test]
    fn select_next_pane_wraps_forward_over_the_active_tab() {
        let mut model = seeded_three_pane();
        let mut actions = ModelPaneStripActions::new();

        actions.select_next_pane(&mut model);
        assert_eq!(active_pane(&model).as_deref(), Some("b"));
        actions.select_next_pane(&mut model);
        assert_eq!(active_pane(&model).as_deref(), Some("c"));
        // Wrap: c → a.
        actions.select_next_pane(&mut model);
        assert_eq!(active_pane(&model).as_deref(), Some("a"), "next wraps c→a");
    }

    #[test]
    fn select_prev_pane_wraps_backward_over_the_active_tab() {
        let mut model = seeded_three_pane();
        let mut actions = ModelPaneStripActions::new();

        // Wrap immediately: a → c.
        actions.select_prev_pane(&mut model);
        assert_eq!(active_pane(&model).as_deref(), Some("c"), "prev wraps a→c");
        actions.select_prev_pane(&mut model);
        assert_eq!(active_pane(&model).as_deref(), Some("b"));
    }

    #[test]
    fn select_next_pane_is_a_noop_with_fewer_than_two_panes() {
        // The seeded Main tab has a single pane; stepping must not move (or panic).
        let mut model = seeded();
        model.select_tab(main_tab_id());
        let before = model.tab_for(main_tab_id()).unwrap().active_pane_id.clone();
        let mut actions = ModelPaneStripActions::new();

        actions.select_next_pane(&mut model);
        actions.select_prev_pane(&mut model);

        assert_eq!(
            model.tab_for(main_tab_id()).unwrap().active_pane_id,
            before,
            "a single-pane tab has nowhere to step — active_pane_id is untouched"
        );
    }

    #[test]
    fn close_pane_of_inactive_pane_keeps_active() {
        let mut model = seeded();
        let mut tab = Tab::new("t2", "Two", "/home/u");
        tab.panes = vec![
            Pane::new("a", "A", PaneKind::Terminal),
            Pane::new("b", "B", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some("a".into());
        model.projects[0].tabs.insert(0, tab);
        let mut actions = ModelPaneStripActions::new();

        actions.close_pane(&mut model, "t2", "b");

        let tab = model.tab_for("t2").unwrap();
        assert_eq!(
            tab.active_pane_id.as_deref(),
            Some("a"),
            "closing a non-active pane must not move the active pane"
        );
    }
}
