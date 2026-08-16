//! Persisted value types + snapshot/hydrate — ported from the model-shaped
//! half of Swift `SessionStore.swift` (`PersistedTermWindow` / `PersistedSession` /
//! `PersistedProject`) plus the hydration in
//! `WindowSession.addRestoredTabModel` and the snapshot builder in
//! `WindowSession.snapshotPersistedWindow`.
//!
//! These are **separate structs from the model types** on purpose: the model
//! [`TermWindow`] serializes `is_alive`/`status`/`waiting_acknowledged` for other
//! surfaces, none of which is persisted. The persisted schema is Swift's v3
//! **minus `branch`** (roadmap M5): the vestigial `Session.branch` field is not
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

use crate::pane_layout::{Pane, PaneLayout, SplitOrient};
use crate::term_window::{TermWindow, TermWindowKind};
use crate::project::Project;
use crate::session::Session;
use crate::workspace_model::WorkspaceModel;

/// One persisted pane — a leaf of a window's split tree (tmux-port Phase 2).
///
/// Pane ids are internal (P2): they never appear on the frozen surfaces
/// (`NICE_TAB_ID`/`NICE_PANE_ID`, the control-socket `"tabId"`/`"paneId"`
/// keys), so nothing outside Nice depends on the value round-tripping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPane {
    pub id: String,
    pub kind: TermWindowKind,
    /// Per-pane last-observed cwd (OSC 7) — what restore respawns this pane's
    /// shell in. Falls back to the window's, then the session's cwd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// A window's persisted split tree. Externally tagged, so a split reads
/// `{"split": {...}}` and a leaf `{"leaf": {...}}` — self-describing in a
/// hand-inspected `sessions.json`, and impossible to confuse with each other
/// the way an untagged enum could be.
///
/// These key spellings are NEW in Phase 2 (nothing about them was frozen
/// before), and freeze at ship like the rest of `sessions.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PersistedPaneLayout {
    Leaf(PersistedPane),
    #[serde(rename_all = "camelCase")]
    Split {
        orient: SplitOrient,
        /// `first`'s share; re-clamped on hydrate, so a hand-edited or
        /// corrupted value can never reach the render layer.
        ratio: f32,
        first: Box<PersistedPaneLayout>,
        second: Box<PersistedPaneLayout>,
    },
}

/// One persisted window (toolbar pill). Mirrors Swift `PersistedTermWindow`.
///
/// `cwd` and `titleManuallySet` are optional so v3 session files written before
/// those fields existed still decode; hydration fills the model defaults. The
/// Phase-2 `layout`/`activeLeafId` pair follows the same rule (and the
/// `sidebarMode`/`sidebarWidth` precedent in `session_store.rs`): optional,
/// omitted when absent, no `CURRENT_VERSION` bump — the store is tolerant by
/// SHAPE, not by version.
///
/// **No `Eq`** — the tree carries `f32` ratios. `PartialEq` (what every
/// round-trip test asserts with) stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedTermWindow {
    pub id: String,
    pub title: String,
    pub kind: TermWindowKind,
    /// Last-observed cwd (OSC 7). Optional — restore falls back to the session's
    /// cwd when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether the user renamed this window. Written `true`-or-omitted; hydrated
    /// `?? false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_manually_set: Option<bool>,
    /// The window's split tree. Written **only when the window has more than
    /// one pane**: a never-split window hydrates to the identical single-leaf
    /// tree either way, so omitting it keeps `sessions.json` byte-identical for
    /// every user who never splits — and a Phase-1 Nice reading the file simply
    /// ignores the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<PersistedPaneLayout>,
    /// Which leaf of `layout` was focused. Written alongside `layout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_leaf_id: Option<String>,
}

/// One persisted session (session / sidebar row). Mirrors Swift `PersistedSession`
/// **minus `branch`** (M5).
///
/// **No `Eq`** — see [`PersistedTermWindow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSession {
    pub id: String,
    pub title: String,
    /// Required — the restore spawn dir. Older files always carried it.
    pub cwd: String,
    /// Non-nil for Claude sessions — THE restore discriminator (`claude --resume
    /// <uuid>`). Nil terminal-only sessions come back as a fresh shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// Serialized as `activePaneId` — the v3 spelling is FROZEN (Phase R renamed
    /// the field, never the JSON key).
    #[serde(rename = "activePaneId", skip_serializing_if = "Option::is_none")]
    pub active_window_id: Option<String>,
    /// Serialized as `panes` — FROZEN v3 spelling.
    #[serde(rename = "panes")]
    pub windows: Vec<PersistedTermWindow>,
    /// Whether the user renamed this session. Written `true`-or-omitted; hydrated
    /// `?? false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_manually_set: Option<bool>,
    /// Depth-1 lineage link. Optional so pre-/branch files decode (comes back
    /// nil, session renders at root). Serialized as `parentTabId` — FROZEN v3
    /// spelling.
    #[serde(rename = "parentTabId", skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Monotonic "Terminal N" counter. Optional — older files recompute it from
    /// window titles via [`Session::recover_next_terminal_index`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_terminal_index: Option<u32>,
}

/// One persisted sidebar project grouping. Mirrors Swift `PersistedProject`.
///
/// `name`/`path` persist verbatim: re-deriving them from each session's cwd on
/// restore would split a multi-worktree project (no common cwd prefix between
/// worktrees) into one project per worktree dir.
///
/// **No `Eq`** — see [`PersistedTermWindow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedProject {
    pub id: String,
    pub name: String,
    pub path: String,
    /// Serialized as `tabs` — FROZEN v3 spelling.
    #[serde(rename = "tabs")]
    pub sessions: Vec<PersistedSession>,
}

impl PersistedPane {
    /// Snapshot one leaf.
    pub fn from_model(pane: &Pane) -> Self {
        PersistedPane {
            id: pane.id.clone(),
            kind: pane.kind,
            cwd: pane.cwd.clone(),
        }
    }

    /// Hydrate one leaf.
    pub fn hydrate(&self) -> Pane {
        Pane::new(self.id.clone(), self.kind).with_cwd(self.cwd.clone())
    }
}

impl PersistedPaneLayout {
    /// Snapshot a model split tree.
    pub fn from_model(layout: &PaneLayout) -> Self {
        match layout {
            PaneLayout::Leaf(pane) => PersistedPaneLayout::Leaf(PersistedPane::from_model(pane)),
            PaneLayout::Split {
                orient,
                ratio,
                first,
                second,
            } => PersistedPaneLayout::Split {
                orient: *orient,
                ratio: *ratio,
                first: Box::new(PersistedPaneLayout::from_model(first)),
                second: Box::new(PersistedPaneLayout::from_model(second)),
            },
        }
    }

    /// Hydrate a model split tree. Ratios are re-clamped into the model's legal
    /// band by [`PaneLayout::normalize_ratios`] at the top of the recursion's
    /// caller, so a corrupted number never reaches geometry.
    pub fn hydrate(&self) -> PaneLayout {
        let mut layout = self.hydrate_raw();
        layout.normalize_ratios();
        layout
    }

    fn hydrate_raw(&self) -> PaneLayout {
        match self {
            PersistedPaneLayout::Leaf(pane) => PaneLayout::Leaf(pane.hydrate()),
            PersistedPaneLayout::Split {
                orient,
                ratio,
                first,
                second,
            } => PaneLayout::Split {
                orient: *orient,
                ratio: *ratio,
                first: Box::new(first.hydrate_raw()),
                second: Box::new(second.hydrate_raw()),
            },
        }
    }
}

impl PersistedTermWindow {
    /// Snapshot a model [`TermWindow`] for persistence. Runtime-only fields
    /// (`is_alive`/`status`/`waiting_acknowledged`/`is_claude_running`/`zoomed`)
    /// are dropped; `title_manually_set` is written `true`-or-omitted, and the
    /// pane tree is written only once the window actually has more than one
    /// pane (see [`PersistedTermWindow::layout`]).
    pub fn from_model(window: &TermWindow) -> Self {
        let split = window.layout.leaf_count() > 1;
        PersistedTermWindow {
            id: window.id.clone(),
            title: window.title.clone(),
            kind: window.kind,
            cwd: window.cwd.clone(),
            title_manually_set: if window.title_manually_set {
                Some(true)
            } else {
                None
            },
            layout: split.then(|| PersistedPaneLayout::from_model(&window.layout)),
            active_leaf_id: split.then(|| window.active_pane_id.clone()),
        }
    }

    /// Hydrate a model [`TermWindow`] from this record. [`TermWindow::new`] supplies the
    /// exact model defaults `is_alive = true`, `status = Idle`,
    /// `waiting_acknowledged = false`, `is_claude_running = false`; only `cwd`
    /// and the `?? false` title lock are carried over.
    ///
    /// The pane tree restores under a validate-or-fall-back rule, because the
    /// loader must never error (a session file that fails to load loses the
    /// user's work): an absent `layout` — every pre-Phase-2 file — hydrates as
    /// the single-leaf window Nice has always restored, and a `layout` that
    /// violates [`TermWindow::layout_is_valid`] (dangling focus, duplicate pane
    /// ids, a Claude-leaf count that disagrees with the window's kind) is
    /// discarded for that same single-leaf shape.
    pub fn hydrate(&self) -> TermWindow {
        let mut window = TermWindow::new(self.id.clone(), self.title.clone(), self.kind);
        window.cwd = self.cwd.clone();
        window.title_manually_set = self.title_manually_set.unwrap_or(false);
        // Restate the single-leaf tree now that `cwd` is filled in, so the sole
        // pane carries the window's cwd.
        window.reset_layout_to_single_leaf();

        if let Some(persisted) = &self.layout {
            let layout = persisted.hydrate();
            let active = self
                .active_leaf_id
                .clone()
                .or_else(|| layout.leaves().first().map(|p| p.id.clone()));
            if let Some(active) = active {
                let single_leaf = std::mem::replace(&mut window.layout, layout);
                let single_active = std::mem::replace(&mut window.active_pane_id, active);
                if !window.layout_is_valid() {
                    window.layout = single_leaf;
                    window.active_pane_id = single_active;
                }
            }
        }
        window
    }
}

impl PersistedSession {
    /// Snapshot a model [`Session`] for persistence — never carries `branch`.
    /// `next_terminal_index` is always written (the model value is
    /// non-optional).
    pub fn from_model(session: &Session) -> Self {
        PersistedSession {
            id: session.id.clone(),
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            claude_session_id: session.claude_session_id.clone(),
            active_window_id: session.active_window_id.clone(),
            windows: session.windows.iter().map(PersistedTermWindow::from_model).collect(),
            title_manually_set: if session.title_manually_set {
                Some(true)
            } else {
                None
            },
            parent_session_id: session.parent_session_id.clone(),
            next_terminal_index: Some(session.next_terminal_index),
        }
    }

    /// Hydrate a model [`Session`] with the exact restore defaults
    /// (`WindowSession.addRestoredTabModel`):
    ///
    /// * windows hydrate individually (`title_manually_set ?? false`);
    /// * `active_window_id = persisted ?? first-claude ?? first`;
    /// * `title_auto_generated = claude_session_id.is_some()`;
    /// * `title_manually_set = persisted ?? false`;
    /// * `next_terminal_index = persisted ?? recover_next_terminal_index(window
    ///   titles)`.
    pub fn hydrate(&self) -> Session {
        let windows: Vec<TermWindow> = self.windows.iter().map(PersistedTermWindow::hydrate).collect();
        let default_active = windows
            .iter()
            .find(|w| w.kind == TermWindowKind::Claude)
            .or_else(|| windows.first())
            .map(|w| w.id.clone());
        let next_terminal_index = self.next_terminal_index.unwrap_or_else(|| {
            let titles: Vec<&str> = self.windows.iter().map(|w| w.title.as_str()).collect();
            Session::recover_next_terminal_index(&titles)
        });

        let mut session = Session::new(self.id.clone(), self.title.clone(), self.cwd.clone());
        session.windows = windows;
        session.active_window_id = self.active_window_id.clone().or(default_active);
        session.title_auto_generated = self.claude_session_id.is_some();
        session.title_manually_set = self.title_manually_set.unwrap_or(false);
        session.claude_session_id = self.claude_session_id.clone();
        session.parent_session_id = self.parent_session_id.clone();
        session.next_terminal_index = next_terminal_index;
        session
    }
}

impl PersistedProject {
    /// Snapshot a model [`Project`] (all its sessions) for persistence. Empty-drop
    /// rules are applied at the window level by [`snapshot_projects`], not
    /// here.
    pub fn from_model(project: &Project) -> Self {
        PersistedProject {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project.path.clone(),
            sessions: project.sessions.iter().map(PersistedSession::from_model).collect(),
        }
    }

    /// Hydrate a model [`Project`] with its hydrated sessions.
    pub fn hydrate(&self) -> Project {
        Project {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            sessions: self.sessions.iter().map(PersistedSession::hydrate).collect(),
        }
    }
}

/// Snapshot a window's project list, applying the Swift snapshot drop rules
/// (`WindowSession.snapshotPersistedWindow`): empty non-Terminals projects are
/// dropped, but the pinned Terminals project is ALWAYS persisted even when
/// empty (so its cwd survives after every session was closed).
pub fn snapshot_projects(projects: &[Project]) -> Vec<PersistedProject> {
    projects
        .iter()
        .filter_map(|project| {
            let persisted = PersistedProject::from_model(project);
            if persisted.sessions.is_empty() && project.id != WorkspaceModel::TERMINALS_PROJECT_ID {
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

    fn claude_window(id: &str) -> TermWindow {
        TermWindow::new(id, "Claude", TermWindowKind::Claude)
    }
    fn terminal_window(id: &str) -> TermWindow {
        TermWindow::new(id, "Terminal 1", TermWindowKind::Terminal)
    }

    // MARK: - round-trip (ported from SessionStoreTests round-trip cases, the
    // TermWindow/Session/Project leaves)

    #[test]
    fn round_trip_preserves_every_field() {
        // Ported from `test_roundTrip_preservesEveryField` (leaf half).
        let session = PersistedSession {
            id: "t1".into(),
            title: "Fix top bar height".into(),
            cwd: "/Users/nick/Projects/nice".into(),
            claude_session_id: Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b".into()),
            active_window_id: Some("p1".into()),
            windows: vec![
                PersistedTermWindow {
                    id: "p1".into(),
                    title: "Claude".into(),
                    kind: TermWindowKind::Claude,
                    cwd: None,
                    title_manually_set: None,
                    layout: None,
                    active_leaf_id: None,
                },
                PersistedTermWindow {
                    id: "p2".into(),
                    title: "zsh".into(),
                    kind: TermWindowKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                    layout: None,
                    active_leaf_id: None,
                },
            ],
            title_manually_set: None,
            parent_session_id: None,
            next_terminal_index: None,
        };
        let project = PersistedProject {
            id: "nice".into(),
            name: "Nice".into(),
            path: "/Users/nick/Projects/nice".into(),
            sessions: vec![session],
        };
        let json = serde_json::to_string(&project).unwrap();
        let restored: PersistedProject = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, project);
    }

    #[test]
    fn round_trip_preserves_nil_optionals() {
        // Ported from `test_roundTrip_preservesNilOptionals`: a terminal-only
        // session with every optional nil.
        let session = PersistedSession {
            id: "t1".into(),
            title: "Main".into(),
            cwd: "/tmp".into(),
            claude_session_id: None,
            active_window_id: None,
            windows: vec![],
            title_manually_set: None,
            parent_session_id: None,
            next_terminal_index: None,
        };
        let json = serde_json::to_string(&session).unwrap();
        // Absent optionals must be OMITTED (skip_serializing_if), not `null`.
        assert!(!json.contains("claudeSessionId"));
        assert!(!json.contains("activePaneId"));
        assert!(!json.contains("titleManuallySet"));
        assert!(!json.contains("parentTabId"));
        assert!(!json.contains("nextTerminalIndex"));
        let restored: PersistedSession = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, session);
    }

    #[test]
    fn persisted_window_round_trips_cwd() {
        // Ported from `test_persistedPane_roundTripsCwd`.
        let windows = vec![
            PersistedTermWindow {
                id: "p1".into(),
                title: "zsh".into(),
                kind: TermWindowKind::Terminal,
                cwd: Some("/usr".into()),
                title_manually_set: None,
                layout: None,
                active_leaf_id: None,
            },
            PersistedTermWindow {
                id: "p2".into(),
                title: "zsh".into(),
                kind: TermWindowKind::Terminal,
                cwd: Some("/var/log".into()),
                title_manually_set: None,
                layout: None,
                active_leaf_id: None,
            },
        ];
        let json = serde_json::to_string(&windows).unwrap();
        let restored: Vec<PersistedTermWindow> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.iter().map(|w| w.cwd.clone()).collect::<Vec<_>>(),
            vec![Some("/usr".into()), Some("/var/log".into())]
        );
    }

    // MARK: - tolerance (ported from the decode-tolerance cases)

    #[test]
    fn decodes_with_unknown_fields_forward_compat() {
        // Ported from `test_decodesFutureVersionWithUnknownFields_forwardCompat`
        // (the session/window leaves): unknown keys at every level are ignored (NO
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
        let session: PersistedSession = serde_json::from_str(json).unwrap();
        assert_eq!(session.claude_session_id.as_deref(), Some("session-uuid"));
        assert_eq!(session.windows[0].kind, TermWindowKind::Claude);
        // The dropped `branch` key is silently ignored — the struct has no such
        // field (M5).
    }

    #[test]
    fn persisted_window_decodes_without_cwd_field_backwards_compat() {
        // Ported from `test_persistedPane_decodesWithoutCwdField_backwardsCompat`.
        let json = r#"{"id": "p1", "title": "zsh", "kind": "terminal"}"#;
        let window: PersistedTermWindow = serde_json::from_str(json).unwrap();
        assert_eq!(window.id, "p1");
        assert_eq!(window.cwd, None, "missing cwd must decode as None, not crash");
        assert_eq!(window.title_manually_set, None);
    }

    #[test]
    fn real_file_shaped_fixture_decodes() {
        // A real-file-shaped v3 session (dossier §3.3): `branch` present + ignored,
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
        let session: PersistedSession = serde_json::from_str(json).unwrap();
        assert_eq!(session.title_manually_set, Some(true));
        assert_eq!(session.parent_session_id.as_deref(), Some("t0"));
        assert_eq!(session.next_terminal_index, Some(4));
        assert_eq!(session.windows[1].cwd.as_deref(), Some("/var/log"));
        assert_eq!(session.windows[1].title_manually_set, Some(true));
        assert_eq!(session.windows[0].title_manually_set, None);
    }

    // MARK: - snapshot / hydrate

    #[test]
    fn snapshot_window_writes_title_lock_true_or_omit() {
        let mut window = terminal_window("p1");
        assert_eq!(PersistedTermWindow::from_model(&window).title_manually_set, None);
        window.title_manually_set = true;
        assert_eq!(
            PersistedTermWindow::from_model(&window).title_manually_set,
            Some(true)
        );
    }

    #[test]
    fn hydrate_window_applies_model_defaults() {
        let persisted = PersistedTermWindow {
            id: "p1".into(),
            title: "Claude".into(),
            kind: TermWindowKind::Claude,
            cwd: Some("/tmp".into()),
            title_manually_set: None,
            layout: None,
            active_leaf_id: None,
        };
        let window = persisted.hydrate();
        assert!(window.is_alive);
        assert!(!window.is_claude_running);
        assert!(!window.waiting_acknowledged);
        assert_eq!(window.cwd.as_deref(), Some("/tmp"));
        assert!(!window.title_manually_set);
    }

    #[test]
    fn hydrate_session_active_window_defaults_to_first_claude() {
        // No persisted activePaneId → first claude window wins over first window.
        let persisted = PersistedSession {
            id: "t1".into(),
            title: "Session".into(),
            cwd: "/tmp".into(),
            claude_session_id: Some("sid".into()),
            active_window_id: None,
            windows: vec![
                PersistedTermWindow {
                    id: "term".into(),
                    title: "Terminal 1".into(),
                    kind: TermWindowKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                    layout: None,
                    active_leaf_id: None,
                },
                PersistedTermWindow {
                    id: "claude".into(),
                    title: "Claude".into(),
                    kind: TermWindowKind::Claude,
                    cwd: None,
                    title_manually_set: None,
                    layout: None,
                    active_leaf_id: None,
                },
            ],
            title_manually_set: None,
            parent_session_id: None,
            next_terminal_index: Some(2),
        };
        let session = persisted.hydrate();
        assert_eq!(session.active_window_id.as_deref(), Some("claude"));
        assert!(
            session.title_auto_generated,
            "claude_session_id.is_some() → title_auto_generated"
        );
    }

    #[test]
    fn hydrate_session_active_window_defaults_to_first_when_no_claude() {
        let persisted = PersistedSession {
            id: "t1".into(),
            title: "Main".into(),
            cwd: "/tmp".into(),
            claude_session_id: None,
            active_window_id: None,
            windows: vec![PersistedTermWindow {
                id: "term".into(),
                title: "Terminal 1".into(),
                kind: TermWindowKind::Terminal,
                cwd: None,
                title_manually_set: None,
                layout: None,
                active_leaf_id: None,
            }],
            title_manually_set: None,
            parent_session_id: None,
            next_terminal_index: Some(2),
        };
        let session = persisted.hydrate();
        assert_eq!(session.active_window_id.as_deref(), Some("term"));
        assert!(!session.title_auto_generated);
    }

    #[test]
    fn hydrate_session_recovers_next_terminal_index_when_absent() {
        // nextTerminalIndex absent → recovered from window titles (max+1).
        let persisted = PersistedSession {
            id: "t1".into(),
            title: "Main".into(),
            cwd: "/tmp".into(),
            claude_session_id: None,
            active_window_id: None,
            windows: vec![
                PersistedTermWindow {
                    id: "a".into(),
                    title: "Terminal 1".into(),
                    kind: TermWindowKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                    layout: None,
                    active_leaf_id: None,
                },
                PersistedTermWindow {
                    id: "b".into(),
                    title: "Terminal 2".into(),
                    kind: TermWindowKind::Terminal,
                    cwd: None,
                    title_manually_set: None,
                    layout: None,
                    active_leaf_id: None,
                },
            ],
            title_manually_set: None,
            parent_session_id: None,
            next_terminal_index: None,
        };
        assert_eq!(persisted.hydrate().next_terminal_index, 3);
    }

    #[test]
    fn snapshot_hydrate_session_round_trips_through_model() {
        let mut session = Session::new("t1", "Ship it", "/work");
        session.claude_session_id = Some("sid-9".into());
        session.parent_session_id = Some("t0".into());
        session.title_manually_set = true;
        session.next_terminal_index = 5;
        session.windows = vec![claude_window("c"), terminal_window("term")];
        session.active_window_id = Some("c".into());

        let persisted = PersistedSession::from_model(&session);
        let hydrated = persisted.hydrate();
        assert_eq!(hydrated.id, session.id);
        assert_eq!(hydrated.cwd, session.cwd);
        assert_eq!(hydrated.claude_session_id, session.claude_session_id);
        assert_eq!(hydrated.parent_session_id, session.parent_session_id);
        assert_eq!(hydrated.next_terminal_index, 5);
        assert_eq!(hydrated.active_window_id.as_deref(), Some("c"));
        assert!(hydrated.title_manually_set);
        assert_eq!(hydrated.windows.len(), 2);
    }

    // MARK: - pane layout (tmux-port Phase 2)

    /// A Claude pill split beside a shell pane — the D1 layout this phase
    /// exists for.
    fn split_window() -> TermWindow {
        let mut window = claude_window("p1");
        window.cwd = Some("/work".into());
        window.reset_layout_to_single_leaf();
        window.layout.split(
            "p1",
            SplitOrient::Beside,
            Pane::new("pane-2", TermWindowKind::Terminal).with_cwd(Some("/var/log".into())),
        );
        window.layout.set_ratio_at(&[], 0.65);
        window.active_pane_id = "pane-2".into();
        window
    }

    #[test]
    fn single_pane_window_omits_the_layout_keys() {
        let window = terminal_window("p1");
        let persisted = PersistedTermWindow::from_model(&window);
        assert_eq!(persisted.layout, None);
        assert_eq!(persisted.active_leaf_id, None);

        let json = serde_json::to_string(&persisted).unwrap();
        assert!(
            !json.contains("layout") && !json.contains("activeLeafId"),
            "a never-split pill writes byte-identical JSON to pre-Phase-2 Nice: {json}"
        );
    }

    #[test]
    fn split_window_round_trips_its_tree_through_json() {
        let window = split_window();
        let persisted = PersistedTermWindow::from_model(&window);
        assert_eq!(persisted.active_leaf_id.as_deref(), Some("pane-2"));

        let json = serde_json::to_string(&persisted).unwrap();
        let decoded: PersistedTermWindow = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, persisted);

        let hydrated = decoded.hydrate();
        assert_eq!(hydrated.layout, window.layout);
        assert_eq!(hydrated.active_pane_id, "pane-2");
        assert!(hydrated.layout_is_valid());
        assert_eq!(
            hydrated.layout.pane("pane-2").unwrap().cwd.as_deref(),
            Some("/var/log"),
            "each pane restores in its own cwd"
        );
    }

    #[test]
    fn absent_layout_hydrates_as_a_single_leaf_window() {
        // Every pre-Phase-2 `sessions.json` looks exactly like this.
        let json = r#"{"id": "p1", "title": "zsh", "kind": "terminal", "cwd": "/usr"}"#;
        let persisted: PersistedTermWindow = serde_json::from_str(json).unwrap();
        assert_eq!(persisted.layout, None);

        let window = persisted.hydrate();
        assert_eq!(window.layout.leaf_count(), 1);
        assert_eq!(window.active_pane_id, "p1");
        assert_eq!(
            window.layout.single_leaf().unwrap().cwd.as_deref(),
            Some("/usr"),
            "the sole pane inherits the window's cwd"
        );
        assert!(window.layout_is_valid());
    }

    #[test]
    fn mangled_layouts_fall_back_to_a_single_leaf_instead_of_erroring() {
        let base = split_window();
        let good = PersistedTermWindow::from_model(&base);

        // 1. Focus names a pane that isn't in the tree.
        let mut dangling = good.clone();
        dangling.active_leaf_id = Some("ghost".into());
        assert_eq!(dangling.hydrate().layout.leaf_count(), 1);

        // 2. The Claude-leaf count disagrees with the pill's kind — here a
        //    terminal-kind window carrying a Claude leaf.
        let mut wrong_kind = good.clone();
        wrong_kind.kind = TermWindowKind::Terminal;
        let hydrated = wrong_kind.hydrate();
        assert_eq!(hydrated.layout.leaf_count(), 1);
        assert!(hydrated.layout_is_valid());

        // 3. Duplicate pane ids.
        let mut duplicated = good.clone();
        duplicated.layout = Some(PersistedPaneLayout::Split {
            orient: SplitOrient::Beside,
            ratio: 0.5,
            first: Box::new(PersistedPaneLayout::Leaf(PersistedPane {
                id: "dup".into(),
                kind: TermWindowKind::Claude,
                cwd: None,
            })),
            second: Box::new(PersistedPaneLayout::Leaf(PersistedPane {
                id: "dup".into(),
                kind: TermWindowKind::Terminal,
                cwd: None,
            })),
        });
        duplicated.active_leaf_id = Some("dup".into());
        assert_eq!(duplicated.hydrate().layout.leaf_count(), 1);

        // The good record still restores its tree — the fallback is targeted,
        // not a blanket refusal.
        assert_eq!(good.hydrate().layout.leaf_count(), 2);
    }

    #[test]
    fn hydrate_clamps_a_corrupted_ratio() {
        let mut persisted = PersistedTermWindow::from_model(&split_window());
        if let Some(PersistedPaneLayout::Split { ratio, .. }) = persisted.layout.as_mut() {
            *ratio = 42.0;
        }
        match &persisted.hydrate().layout {
            PaneLayout::Split { ratio, .. } => assert_eq!(*ratio, crate::pane_layout::RATIO_MAX),
            _ => panic!("expected the split to survive, just clamped"),
        }
    }

    #[test]
    fn layout_survives_a_full_session_snapshot_hydrate_cycle() {
        let mut session = Session::new("t1", "Ship it", "/work");
        session.claude_session_id = Some("sid-9".into());
        session.windows = vec![split_window(), terminal_window("term")];
        session.active_window_id = Some("p1".into());

        let json = serde_json::to_string(&PersistedSession::from_model(&session)).unwrap();
        let restored: PersistedSession = serde_json::from_str(&json).unwrap();
        let hydrated = restored.hydrate();

        assert_eq!(hydrated.windows[0].layout, session.windows[0].layout);
        assert_eq!(hydrated.windows[0].active_pane_id, "pane-2");
        assert_eq!(
            hydrated.windows[1].layout.leaf_count(),
            1,
            "the unsplit sibling is untouched"
        );
    }

    #[test]
    fn a_phase_one_reader_ignores_the_layout_keys() {
        // The forward-compat rule already pinned at
        // `decodes_with_unknown_fields_forward_compat`, stated for the keys
        // this phase adds: an older Nice decodes the file and restores
        // single-pane pills.
        let json = serde_json::to_string(&PersistedTermWindow::from_model(&split_window())).unwrap();
        assert!(json.contains("activeLeafId") && json.contains("\"split\""));

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PhaseOneTermWindow {
            id: String,
            title: String,
            kind: TermWindowKind,
            #[serde(default)]
            cwd: Option<String>,
        }
        let old: PhaseOneTermWindow = serde_json::from_str(&json).unwrap();
        assert_eq!(old.id, "p1");
        assert_eq!(old.title, "Claude");
        assert_eq!(old.kind, TermWindowKind::Claude);
        assert_eq!(old.cwd.as_deref(), Some("/work"));
    }

    // MARK: - snapshot_projects drop rules

    #[test]
    fn snapshot_projects_drops_empty_non_terminals_keeps_empty_terminals() {
        let terminals = Project {
            id: WorkspaceModel::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: "/home".into(),
            sessions: vec![],
        };
        let empty_project = Project {
            id: "nice".into(),
            name: "Nice".into(),
            path: "/work".into(),
            sessions: vec![],
        };
        let full_project = Project {
            id: "notes".into(),
            name: "Notes".into(),
            path: "/notes".into(),
            sessions: vec![Session::new("t1", "A", "/notes")],
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
