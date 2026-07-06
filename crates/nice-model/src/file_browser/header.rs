//! The file-browser header title helper, ported from
//! `TabModel.fileBrowserHeaderTitle(forTab:)` (`TabModel.swift:160-173`).
//!
//! Encapsulates the rule "use the owning project's name, unless the tab is in
//! the pinned Terminals project (whose name is generic), in which case fall
//! back to the tab's own title." Kept out of the browser view so the view
//! never has to know about [`TabModel::TERMINALS_PROJECT_ID`]. Three branches:
//! unknown tab ⇒ `"Files"`; Terminals-project tab ⇒ the tab's title (or the
//! project name if somehow absent); real-project tab ⇒ the project name.

use crate::TabModel;

/// The title to show at the top of the file browser for `tab_id`. See the
/// module docs for the rule.
pub fn file_browser_header_title(model: &TabModel, tab_id: &str) -> String {
    let tab_title = model.tab_for(tab_id).map(|t| t.title.clone());
    let owning_project = model
        .projects
        .iter()
        .find(|p| p.tabs.iter().any(|t| t.id == tab_id));

    let Some(project) = owning_project else {
        return tab_title.unwrap_or_else(|| "Files".to_string());
    };
    if project.id == TabModel::TERMINALS_PROJECT_ID {
        return tab_title.unwrap_or_else(|| project.name.clone());
    }
    project.name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Tab, TabModel};

    fn project(id: &str, name: &str, tabs: Vec<Tab>) -> Project {
        Project {
            id: id.to_string(),
            name: name.to_string(),
            path: "/tmp".to_string(),
            tabs,
        }
    }

    /// `AppStateFileBrowserTests.test_fileBrowserHeaderTitle_unknownTab_returnsFiles`
    #[test]
    fn header_title_unknown_tab_returns_files() {
        let model = TabModel::new("/tmp");
        assert_eq!(
            file_browser_header_title(&model, "no-such-tab"),
            "Files",
            "an unknown tab has no project to name; fall back to a generic label"
        );
    }

    /// `AppStateFileBrowserTests.test_fileBrowserHeaderTitle_terminalsProjectTab_returnsTabTitle`
    #[test]
    fn header_title_terminals_project_tab_returns_tab_title() {
        // `TabModel::new` seeds the Terminals project with a "Main" tab.
        let model = TabModel::new("/tmp");
        let main_id = TabModel::MAIN_TERMINAL_TAB_ID;
        let expected = model.tab_for(main_id).unwrap().title.clone();
        assert_eq!(file_browser_header_title(&model, main_id), expected);
    }

    /// `AppStateFileBrowserTests.test_fileBrowserHeaderTitle_realProjectTab_returnsProjectName`
    #[test]
    fn header_title_real_project_tab_returns_project_name() {
        let tab = Tab::new("claude-1", "some tab title", "/tmp/proj");
        let model = TabModel::from_parts_std(
            vec![project("proj-uuid", "MyCoolProject", vec![tab])],
            Some("claude-1".to_string()),
        );
        assert_eq!(
            file_browser_header_title(&model, "claude-1"),
            "MyCoolProject",
            "a real project's name wins over the tab title"
        );
    }
}
