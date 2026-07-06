//! Persisted value types + snapshot/hydrate — ported from the model-shaped
//! half of Swift `SessionStore.swift` (`PersistedPane` / `PersistedTab` /
//! `PersistedProject`) plus the hydration in
//! `WindowSession.addRestoredTabModel` and the snapshot builder in
//! `WindowSession.snapshotPersistedWindow`.
//!
//! These are **separate structs from the model types** on purpose: the model
//! [`Pane`] serializes `is_alive`/`status`/`waiting_acknowledged` for other
//! surfaces, none of which is persisted. The persisted schema is Swift's v3
//! **minus `branch`** (roadmap M5): the vestigial `Tab.branch` field is not
//! ported into the model and is likewise dropped here. Migration reads of the
//! Swift file ignore the extra `branch` key (no `deny_unknown_fields`).
//!
//! JSON keys are camelCase (`#[serde(rename_all = "camelCase")]`) to match the
//! Swift-written file byte-for-byte at the key level. Every optional carries
//! `#[serde(skip_serializing_if = "Option::is_none")]` so nil-omitted optionals
//! round-trip and the snapshot JSON stays small (mirroring Swift's
//! `titleManuallySet: … ? true : nil`).
//!
//! The window-level envelope (`PersistedFrame`/`PersistedWindow`/
//! `PersistedState`) plus the store I/O live in `crates/nice`
//! (`session_store.rs`) — this module is gpui-free and owns only the
//! model-shaped leaves that snapshot/hydrate the tree.

use serde::{Deserialize, Serialize};

use crate::pane::{Pane, PaneKind};
use crate::project::Project;
use crate::tab::Tab;
use crate::tab_model::TabModel;

/// One persisted pane (toolbar pill). Mirrors Swift `PersistedPane`.
///
/// `cwd` and `titleManuallySet` are optional so v3 session files written before
/// those fields existed still decode; hydration fills the model defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPane {
    pub id: String,
    pub title: String,
    pub kind: PaneKind,
    /// Last-observed cwd (OSC 7). Optional — restore falls back to the tab's
    /// cwd when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether the user renamed this pane. Written `true`-or-omitted; hydrated
    /// `?? false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_manually_set: Option<bool>,
}

/// One persisted tab (session / sidebar row). Mirrors Swift `PersistedTab`
/// **minus `branch`** (M5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedTab {
    pub id: String,
    pub title: String,
    /// Required — the restore spawn dir. Older files always carried it.
    pub cwd: String,
    /// Non-nil for Claude tabs — THE restore discriminator (`claude --resume
    /// <uuid>`). Nil terminal-only tabs come back as a fresh shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_pane_id: Option<String>,
    pub panes: Vec<PersistedPane>,
    /// Whether the user renamed this tab. Written `true`-or-omitted; hydrated
    /// `?? false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_manually_set: Option<bool>,
    /// Depth-1 lineage link. Optional so pre-/branch files decode (comes back
    /// nil, tab renders at root).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tab_id: Option<String>,
    /// Monotonic "Terminal N" counter. Optional — older files recompute it from
    /// pane titles via [`Tab::recover_next_terminal_index`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_terminal_index: Option<u32>,
}

/// One persisted sidebar project grouping. Mirrors Swift `PersistedProject`.
///
/// `name`/`path` persist verbatim: re-deriving them from each tab's cwd on
/// restore would split a multi-worktree project (no common cwd prefix between
/// worktrees) into one project per worktree dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedProject {
    pub id: String,
    pub name: String,
    pub path: String,
    pub tabs: Vec<PersistedTab>,
}

impl PersistedPane {
    /// Snapshot a model [`Pane`] for persistence. Runtime-only fields
    /// (`is_alive`/`status`/`waiting_acknowledged`/`is_claude_running`) are
    /// dropped; `title_manually_set` is written `true`-or-omitted.
    pub fn from_model(pane: &Pane) -> Self {
        PersistedPane {
            id: pane.id.clone(),
            title: pane.title.clone(),
            kind: pane.kind,
            cwd: pane.cwd.clone(),
            title_manually_set: if pane.title_manually_set {
                Some(true)
            } else {
                None
            },
        }
    }

    /// Hydrate a model [`Pane`] from this record. [`Pane::new`] supplies the
    /// exact model defaults `is_alive = true`, `status = Idle`,
    /// `waiting_acknowledged = false`, `is_claude_running = false`; only `cwd`
    /// and the `?? false` title lock are carried over.
    pub fn hydrate(&self) -> Pane {
        let mut pane = Pane::new(self.id.clone(), self.title.clone(), self.kind);
        pane.cwd = self.cwd.clone();
        pane.title_manually_set = self.title_manually_set.unwrap_or(false);
        pane
    }
}

impl PersistedTab {
    /// Snapshot a model [`Tab`] for persistence — never carries `branch`.
    /// `next_terminal_index` is always written (the model value is
    /// non-optional).
    pub fn from_model(tab: &Tab) -> Self {
        PersistedTab {
            id: tab.id.clone(),
            title: tab.title.clone(),
            cwd: tab.cwd.clone(),
            claude_session_id: tab.claude_session_id.clone(),
            active_pane_id: tab.active_pane_id.clone(),
            panes: tab.panes.iter().map(PersistedPane::from_model).collect(),
            title_manually_set: if tab.title_manually_set {
                Some(true)
            } else {
                None
            },
            parent_tab_id: tab.parent_tab_id.clone(),
            next_terminal_index: Some(tab.next_terminal_index),
        }
    }

    /// Hydrate a model [`Tab`] with the exact restore defaults
    /// (`WindowSession.addRestoredTabModel`):
    ///
    /// * panes hydrate individually (`title_manually_set ?? false`);
    /// * `active_pane_id = persisted ?? first-claude ?? first`;
    /// * `title_auto_generated = claude_session_id.is_some()`;
    /// * `title_manually_set = persisted ?? false`;
    /// * `next_terminal_index = persisted ?? recover_next_terminal_index(pane
    ///   titles)`.
    pub fn hydrate(&self) -> Tab {
        let panes: Vec<Pane> = self.panes.iter().map(PersistedPane::hydrate).collect();
        let default_active = panes
            .iter()
            .find(|p| p.kind == PaneKind::Claude)
            .or_else(|| panes.first())
            .map(|p| p.id.clone());
        let next_terminal_index = self.next_terminal_index.unwrap_or_else(|| {
            let titles: Vec<&str> = self.panes.iter().map(|p| p.title.as_str()).collect();
            Tab::recover_next_terminal_index(&titles)
        });

        let mut tab = Tab::new(self.id.clone(), self.title.clone(), self.cwd.clone());
        tab.panes = panes;
        tab.active_pane_id = self.active_pane_id.clone().or(default_active);
        tab.title_auto_generated = self.claude_session_id.is_some();
        tab.title_manually_set = self.title_manually_set.unwrap_or(false);
        tab.claude_session_id = self.claude_session_id.clone();
        tab.parent_tab_id = self.parent_tab_id.clone();
        tab.next_terminal_index = next_terminal_index;
        tab
    }
}

impl PersistedProject {
    /// Snapshot a model [`Project`] (all its tabs) for persistence. Empty-drop
    /// rules are applied at the window level by [`snapshot_projects`], not
    /// here.
    pub fn from_model(project: &Project) -> Self {
        PersistedProject {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project.path.clone(),
            tabs: project.tabs.iter().map(PersistedTab::from_model).collect(),
        }
    }

    /// Hydrate a model [`Project`] with its hydrated tabs.
    pub fn hydrate(&self) -> Project {
        Project {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            tabs: self.tabs.iter().map(PersistedTab::hydrate).collect(),
        }
    }
}

/// Snapshot a window's project list, applying the Swift snapshot drop rules
/// (`WindowSession.snapshotPersistedWindow`): empty non-Terminals projects are
/// dropped, but the pinned Terminals project is ALWAYS persisted even when
/// empty (so its cwd survives after every tab was closed).
pub fn snapshot_projects(projects: &[Project]) -> Vec<PersistedProject> {
    projects
        .iter()
        .filter_map(|project| {
            let persisted = PersistedProject::from_model(project);
            if persisted.tabs.is_empty() && project.id != TabModel::TERMINALS_PROJECT_ID {
                None
            } else {
                Some(persisted)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_pane(id: &str) -> Pane {
        Pane::new(id, "Claude", PaneKind::Claude)
    }
    fn terminal_pane(id: &str) -> Pane {
        Pane::new(id, "Terminal 1", PaneKind::Terminal)
    }

    // MARK: - round-trip (ported from SessionStoreTests round-trip cases, the
    // Pane/Tab/Project leaves)

    #[test]
    fn round_trip_preserves_every_field() {
        // Ported from `test_roundTrip_preservesEveryField` (leaf half).
        let tab = PersistedTab {
            id: "t1".into(),
            title: "Fix top bar height".into(),
            cwd: "/Users/nick/Projects/nice".into(),
            claude_session_id: Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b".into()),
            active_pane_id: Some("p1".into()),
            panes: vec![
                PersistedPane {
                    id: "p1".into(),
                    title: "Claude".into(),
                    kind: PaneKind::Claude,
                    cwd: None,
                    title_manually_set: None,
                },
                PersistedPane {
                    id: "p2".into(),
                    title: "zsh".into(),
                    kind: PaneKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                },
            ],
            title_manually_set: None,
            parent_tab_id: None,
            next_terminal_index: None,
        };
        let project = PersistedProject {
            id: "nice".into(),
            name: "Nice".into(),
            path: "/Users/nick/Projects/nice".into(),
            tabs: vec![tab],
        };
        let json = serde_json::to_string(&project).unwrap();
        let restored: PersistedProject = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, project);
    }

    #[test]
    fn round_trip_preserves_nil_optionals() {
        // Ported from `test_roundTrip_preservesNilOptionals`: a terminal-only
        // tab with every optional nil.
        let tab = PersistedTab {
            id: "t1".into(),
            title: "Main".into(),
            cwd: "/tmp".into(),
            claude_session_id: None,
            active_pane_id: None,
            panes: vec![],
            title_manually_set: None,
            parent_tab_id: None,
            next_terminal_index: None,
        };
        let json = serde_json::to_string(&tab).unwrap();
        // Absent optionals must be OMITTED (skip_serializing_if), not `null`.
        assert!(!json.contains("claudeSessionId"));
        assert!(!json.contains("activePaneId"));
        assert!(!json.contains("titleManuallySet"));
        assert!(!json.contains("parentTabId"));
        assert!(!json.contains("nextTerminalIndex"));
        let restored: PersistedTab = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, tab);
    }

    #[test]
    fn persisted_pane_round_trips_cwd() {
        // Ported from `test_persistedPane_roundTripsCwd`.
        let panes = vec![
            PersistedPane {
                id: "p1".into(),
                title: "zsh".into(),
                kind: PaneKind::Terminal,
                cwd: Some("/usr".into()),
                title_manually_set: None,
            },
            PersistedPane {
                id: "p2".into(),
                title: "zsh".into(),
                kind: PaneKind::Terminal,
                cwd: Some("/var/log".into()),
                title_manually_set: None,
            },
        ];
        let json = serde_json::to_string(&panes).unwrap();
        let restored: Vec<PersistedPane> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.iter().map(|p| p.cwd.clone()).collect::<Vec<_>>(),
            vec![Some("/usr".into()), Some("/var/log".into())]
        );
    }

    // MARK: - tolerance (ported from the decode-tolerance cases)

    #[test]
    fn decodes_with_unknown_fields_forward_compat() {
        // Ported from `test_decodesFutureVersionWithUnknownFields_forwardCompat`
        // (the tab/pane leaves): unknown keys at every level are ignored (NO
        // deny_unknown_fields).
        let json = r#"{
            "id": "t1",
            "title": "Main",
            "cwd": "/tmp",
            "branch": "main",
            "claudeSessionId": "session-uuid",
            "activePaneId": "pane-1",
            "futureTab": {"nested": true},
            "panes": [
                {"id": "pane-1", "title": "Claude", "kind": "claude", "cwd": "/tmp", "futurePane": "ignored"}
            ]
        }"#;
        let tab: PersistedTab = serde_json::from_str(json).unwrap();
        assert_eq!(tab.claude_session_id.as_deref(), Some("session-uuid"));
        assert_eq!(tab.panes[0].kind, PaneKind::Claude);
        // The dropped `branch` key is silently ignored — the struct has no such
        // field (M5).
    }

    #[test]
    fn persisted_pane_decodes_without_cwd_field_backwards_compat() {
        // Ported from `test_persistedPane_decodesWithoutCwdField_backwardsCompat`.
        let json = r#"{"id": "p1", "title": "zsh", "kind": "terminal"}"#;
        let pane: PersistedPane = serde_json::from_str(json).unwrap();
        assert_eq!(pane.id, "p1");
        assert_eq!(pane.cwd, None, "missing cwd must decode as None, not crash");
        assert_eq!(pane.title_manually_set, None);
    }

    #[test]
    fn real_file_shaped_fixture_decodes() {
        // A real-file-shaped v3 tab (dossier §3.3): `branch` present + ignored,
        // absent optionals, `titleManuallySet` true-or-omit mix.
        let json = r#"{
            "id": "t1",
            "title": "Ship it",
            "cwd": "/Users/nick/Projects/nice",
            "branch": null,
            "claudeSessionId": "abc-123",
            "activePaneId": "p1",
            "titleManuallySet": true,
            "parentTabId": "t0",
            "nextTerminalIndex": 4,
            "panes": [
                {"id": "p1", "title": "Claude", "kind": "claude"},
                {"id": "p2", "title": "logs", "kind": "terminal", "cwd": "/var/log", "titleManuallySet": true}
            ]
        }"#;
        let tab: PersistedTab = serde_json::from_str(json).unwrap();
        assert_eq!(tab.title_manually_set, Some(true));
        assert_eq!(tab.parent_tab_id.as_deref(), Some("t0"));
        assert_eq!(tab.next_terminal_index, Some(4));
        assert_eq!(tab.panes[1].cwd.as_deref(), Some("/var/log"));
        assert_eq!(tab.panes[1].title_manually_set, Some(true));
        assert_eq!(tab.panes[0].title_manually_set, None);
    }

    // MARK: - snapshot / hydrate

    #[test]
    fn snapshot_pane_writes_title_lock_true_or_omit() {
        let mut pane = terminal_pane("p1");
        assert_eq!(PersistedPane::from_model(&pane).title_manually_set, None);
        pane.title_manually_set = true;
        assert_eq!(
            PersistedPane::from_model(&pane).title_manually_set,
            Some(true)
        );
    }

    #[test]
    fn hydrate_pane_applies_model_defaults() {
        let persisted = PersistedPane {
            id: "p1".into(),
            title: "Claude".into(),
            kind: PaneKind::Claude,
            cwd: Some("/tmp".into()),
            title_manually_set: None,
        };
        let pane = persisted.hydrate();
        assert!(pane.is_alive);
        assert!(!pane.is_claude_running);
        assert!(!pane.waiting_acknowledged);
        assert_eq!(pane.cwd.as_deref(), Some("/tmp"));
        assert!(!pane.title_manually_set);
    }

    #[test]
    fn hydrate_tab_active_pane_defaults_to_first_claude() {
        // No persisted activePaneId → first claude pane wins over first pane.
        let persisted = PersistedTab {
            id: "t1".into(),
            title: "Tab".into(),
            cwd: "/tmp".into(),
            claude_session_id: Some("sid".into()),
            active_pane_id: None,
            panes: vec![
                PersistedPane {
                    id: "term".into(),
                    title: "Terminal 1".into(),
                    kind: PaneKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                },
                PersistedPane {
                    id: "claude".into(),
                    title: "Claude".into(),
                    kind: PaneKind::Claude,
                    cwd: None,
                    title_manually_set: None,
                },
            ],
            title_manually_set: None,
            parent_tab_id: None,
            next_terminal_index: Some(2),
        };
        let tab = persisted.hydrate();
        assert_eq!(tab.active_pane_id.as_deref(), Some("claude"));
        assert!(
            tab.title_auto_generated,
            "claude_session_id.is_some() → title_auto_generated"
        );
    }

    #[test]
    fn hydrate_tab_active_pane_defaults_to_first_when_no_claude() {
        let persisted = PersistedTab {
            id: "t1".into(),
            title: "Main".into(),
            cwd: "/tmp".into(),
            claude_session_id: None,
            active_pane_id: None,
            panes: vec![PersistedPane {
                id: "term".into(),
                title: "Terminal 1".into(),
                kind: PaneKind::Terminal,
                cwd: None,
                title_manually_set: None,
            }],
            title_manually_set: None,
            parent_tab_id: None,
            next_terminal_index: Some(2),
        };
        let tab = persisted.hydrate();
        assert_eq!(tab.active_pane_id.as_deref(), Some("term"));
        assert!(!tab.title_auto_generated);
    }

    #[test]
    fn hydrate_tab_recovers_next_terminal_index_when_absent() {
        // nextTerminalIndex absent → recovered from pane titles (max+1).
        let persisted = PersistedTab {
            id: "t1".into(),
            title: "Main".into(),
            cwd: "/tmp".into(),
            claude_session_id: None,
            active_pane_id: None,
            panes: vec![
                PersistedPane {
                    id: "a".into(),
                    title: "Terminal 1".into(),
                    kind: PaneKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                },
                PersistedPane {
                    id: "b".into(),
                    title: "Terminal 2".into(),
                    kind: PaneKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                },
            ],
            title_manually_set: None,
            parent_tab_id: None,
            next_terminal_index: None,
        };
        assert_eq!(persisted.hydrate().next_terminal_index, 3);
    }

    #[test]
    fn snapshot_hydrate_tab_round_trips_through_model() {
        let mut tab = Tab::new("t1", "Ship it", "/work");
        tab.claude_session_id = Some("sid-9".into());
        tab.parent_tab_id = Some("t0".into());
        tab.title_manually_set = true;
        tab.next_terminal_index = 5;
        tab.panes = vec![claude_pane("c"), terminal_pane("term")];
        tab.active_pane_id = Some("c".into());

        let persisted = PersistedTab::from_model(&tab);
        let hydrated = persisted.hydrate();
        assert_eq!(hydrated.id, tab.id);
        assert_eq!(hydrated.cwd, tab.cwd);
        assert_eq!(hydrated.claude_session_id, tab.claude_session_id);
        assert_eq!(hydrated.parent_tab_id, tab.parent_tab_id);
        assert_eq!(hydrated.next_terminal_index, 5);
        assert_eq!(hydrated.active_pane_id.as_deref(), Some("c"));
        assert!(hydrated.title_manually_set);
        assert_eq!(hydrated.panes.len(), 2);
    }

    // MARK: - snapshot_projects drop rules

    #[test]
    fn snapshot_projects_drops_empty_non_terminals_keeps_empty_terminals() {
        let terminals = Project {
            id: TabModel::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: "/home".into(),
            tabs: vec![],
        };
        let empty_project = Project {
            id: "nice".into(),
            name: "Nice".into(),
            path: "/work".into(),
            tabs: vec![],
        };
        let full_project = Project {
            id: "notes".into(),
            name: "Notes".into(),
            path: "/notes".into(),
            tabs: vec![Tab::new("t1", "A", "/notes")],
        };
        let snapshot = snapshot_projects(&[terminals, empty_project, full_project]);
        let ids: Vec<&str> = snapshot.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["terminals", "notes"],
            "empty Terminals is always kept; empty non-Terminals is dropped; non-empty is kept"
        );
    }
}
