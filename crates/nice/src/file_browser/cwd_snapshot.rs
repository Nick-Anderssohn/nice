//! `cwd_snapshot` — the `WindowRegistry`-walking builder for the pure
//! [`nice_model::file_browser::cwd_impact`] rule (F8). Ported from
//! `FileBrowserCWDImpactCheck.snapshot(from:)` (`FileBrowserCWDImpactCheck.swift:96-125`).
//!
//! The string-prefix decision ([`affected_by`](nice_model::file_browser::affected_by))
//! and its value types are pure and live in `nice-model`; only this walk —
//! which reaches every live window's [`WindowState`](crate::window_state::WindowState)
//! model through the registry — is registry-dependent, so it lands here.
//!
//! Every window/project/tab contributes: one synthetic tab-anchor entry
//! ([`Tab::cwd`](nice_model::Tab)) plus one entry per `is_alive` pane with a
//! non-empty OSC-7 cwd. The per-tab projection ([`entries_for_tab`]) is pure over
//! a plain [`nice_model::Tab`] so it is table-tested without a gpui `App`; the
//! registry walk ([`build_snapshot`]) is exercised by the rename flow + scenario.

use gpui::App;

use nice_model::file_browser::cwd_impact::normalize_path;
use nice_model::file_browser::{PaneCWDRef, PaneCWDSnapshot};
use nice_model::{PaneKind, Tab};

use crate::window_registry::WindowRegistry;

/// The CWD references a single tab contributes: the tab anchor (`Tab.cwd`, an
/// empty `pane_id` + a [`PaneKind::Terminal`] sentinel — the message only counts,
/// it never distinguishes kinds) plus one entry per `is_alive` pane carrying a
/// non-empty OSC-7 cwd. Each `cwd` is normalized (trailing slash stripped) so the
/// prefix rule in [`affected_by`](nice_model::file_browser::affected_by) sees
/// canonical forms.
pub fn entries_for_tab(window_session_id: &str, tab: &Tab) -> Vec<PaneCWDRef> {
    let mut out = Vec::new();
    // The synthetic tab-anchor entry (always present — Swift adds `Tab.cwd`
    // unconditionally; an empty cwd normalizes to "" and simply never matches).
    out.push(PaneCWDRef {
        window_session_id: window_session_id.to_string(),
        tab_id: tab.id.clone(),
        pane_id: String::new(),
        kind: PaneKind::Terminal,
        cwd: normalize_path(&tab.cwd),
    });
    for pane in tab.panes.iter().filter(|p| p.is_alive) {
        if let Some(cwd) = pane.cwd.as_deref().filter(|c| !c.is_empty()) {
            out.push(PaneCWDRef {
                window_session_id: window_session_id.to_string(),
                tab_id: tab.id.clone(),
                pane_id: pane.id.clone(),
                kind: pane.kind,
                cwd: normalize_path(cwd),
            });
        }
    }
    out
}

/// Build a [`PaneCWDSnapshot`] over every live window's tabs by walking the
/// [`WindowRegistry`]. Runs once at the start of a rename attempt so the CWD-impact
/// check sees a consistent view. Empty when no registry is installed.
pub fn build_snapshot(cx: &App) -> PaneCWDSnapshot {
    let mut entries = Vec::new();
    for state in WindowRegistry::all_states(cx) {
        let ws = state.read(cx);
        let session_id = ws.session_id().to_string();
        for project in &ws.model.projects {
            for tab in &project.tabs {
                entries.extend(entries_for_tab(&session_id, tab));
            }
        }
    }
    PaneCWDSnapshot { entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::{Pane, PaneKind, Tab};

    fn tab_with_cwd(id: &str, cwd: &str) -> Tab {
        Tab::new(id, "title", cwd)
    }

    /// The tab-anchor entry is always present, carrying the (normalized) tab cwd,
    /// an empty pane_id, and the Terminal sentinel kind.
    #[test]
    fn entries_for_tab_includes_tab_anchor() {
        let tab = tab_with_cwd("t1", "/proj/nice/");
        let entries = entries_for_tab("win-A", &tab);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].window_session_id, "win-A");
        assert_eq!(entries[0].tab_id, "t1");
        assert_eq!(entries[0].pane_id, "");
        assert_eq!(entries[0].cwd, "/proj/nice", "trailing slash normalized off");
    }

    /// Each `is_alive` pane with a non-empty OSC-7 cwd contributes an entry;
    /// dead panes and panes without an OSC-7 cwd are skipped.
    #[test]
    fn entries_for_tab_includes_live_panes_with_cwd() {
        let mut tab = tab_with_cwd("t1", "/proj");
        let mut alive = Pane::new("p1", "sh", PaneKind::Terminal);
        alive.cwd = Some("/proj/src/".to_string());
        let mut dead = Pane::new("p2", "sh", PaneKind::Terminal);
        dead.is_alive = false;
        dead.cwd = Some("/proj/dead".to_string());
        let mut no_cwd = Pane::new("p3", "claude", PaneKind::Claude);
        no_cwd.cwd = None;
        tab.panes = vec![alive, dead, no_cwd];

        let entries = entries_for_tab("win-A", &tab);
        // tab-anchor + the one live pane with a cwd (dead + no-cwd skipped).
        assert_eq!(entries.len(), 2);
        let pane_entry = entries.iter().find(|e| e.pane_id == "p1").unwrap();
        assert_eq!(pane_entry.cwd, "/proj/src", "pane cwd normalized");
        assert!(entries.iter().all(|e| e.pane_id != "p2"), "dead pane skipped");
        assert!(entries.iter().all(|e| e.pane_id != "p3"), "no-cwd pane skipped");
    }

    /// A claude pane's kind is preserved on its entry (the message doesn't use it,
    /// but the value carries through — matching the pure `affected_by` test).
    #[test]
    fn entries_for_tab_preserves_pane_kind() {
        let mut tab = tab_with_cwd("t1", "/proj");
        let mut claude = Pane::new("p1", "claude", PaneKind::Claude);
        claude.cwd = Some("/proj/sub".to_string());
        tab.panes = vec![claude];
        let entries = entries_for_tab("win-A", &tab);
        let pane_entry = entries.iter().find(|e| e.pane_id == "p1").unwrap();
        assert_eq!(pane_entry.kind, PaneKind::Claude);
    }
}
