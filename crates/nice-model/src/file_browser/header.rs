//! The file-browser header title helper, ported from
//! `WorkspaceModel.fileBrowserHeaderTitle(forTab:)` (`TabModel.swift:160-173`).
//!
//! Encapsulates the rule "use the owning project's name, unless the session is in
//! the pinned Terminals project (whose name is generic), in which case fall
//! back to the session's own title." Kept out of the browser view so the view
//! never has to know about [`WorkspaceModel::TERMINALS_PROJECT_ID`]. Three branches:
//! unknown session ⇒ `"Files"`; Terminals-project session ⇒ the session's title (or the
//! project name if somehow absent); real-project session ⇒ the project name.

use crate::WorkspaceModel;

/// The title to show at the top of the file browser for `session_id`. See the
/// module docs for the rule.
pub fn file_browser_header_title(model: &WorkspaceModel, session_id: &str) -> String {
    let session_title = model.session_for(session_id).map(|s| s.title.clone());
    let owning_project = model
        .projects
        .iter()
        .find(|p| p.sessions.iter().any(|s| s.id == session_id));

    let Some(project) = owning_project else {
        return session_title.unwrap_or_else(|| "Files".to_string());
    };
    if project.id == WorkspaceModel::TERMINALS_PROJECT_ID {
        return session_title.unwrap_or_else(|| project.name.clone());
    }
    project.name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Session, WorkspaceModel};

    fn project(id: &str, name: &str, sessions: Vec<Session>) -> Project {
        Project {
            id: id.to_string(),
            name: name.to_string(),
            path: "/tmp".to_string(),
            sessions,
        }
    }

    /// `AppStateFileBrowserTests.test_fileBrowserHeaderTitle_unknownTab_returnsFiles`
    #[test]
    fn header_title_unknown_session_returns_files() {
        let model = WorkspaceModel::new("/tmp");
        assert_eq!(
            file_browser_header_title(&model, "no-such-session"),
            "Files",
            "an unknown session has no project to name; fall back to a generic label"
        );
    }

    /// `AppStateFileBrowserTests.test_fileBrowserHeaderTitle_terminalsProjectTab_returnsTabTitle`
    #[test]
    fn header_title_terminals_project_session_returns_session_title() {
        // `WorkspaceModel::new` seeds the Terminals project with a "Main" session.
        let model = WorkspaceModel::new("/tmp");
        let main_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
        let expected = model.session_for(main_id).unwrap().title.clone();
        assert_eq!(file_browser_header_title(&model, main_id), expected);
    }

    /// `AppStateFileBrowserTests.test_fileBrowserHeaderTitle_realProjectTab_returnsProjectName`
    #[test]
    fn header_title_real_project_session_returns_project_name() {
        let session = Session::new("claude-1", "some session title", "/tmp/proj");
        let model = WorkspaceModel::from_parts_std(
            vec![project("proj-uuid", "MyCoolProject", vec![session])],
            Some("claude-1".to_string()),
        );
        assert_eq!(
            file_browser_header_title(&model, "claude-1"),
            "MyCoolProject",
            "a real project's name wins over the session title"
        );
    }
}
