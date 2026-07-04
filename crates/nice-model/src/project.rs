//! `Project` — an ordered group of tabs — ported from
//! `Sources/Nice/State/Models.swift`.

use serde::{Deserialize, Serialize};

use crate::tab::Tab;

/// A project: a named, path-rooted group of [`Tab`]s rendered as one sidebar
/// section. The Terminals project + cwd bucketing that populate these are
/// provided by [`crate::TabModel`] (`add_tab_to_projects`); this is the pure
/// value type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub tabs: Vec<Tab>,
}

impl Project {
    /// The empty seed set — the projects list starts empty and is populated by
    /// `TabModel` seeding (`Models.swift:270-272`).
    pub fn seed() -> Vec<Project> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_is_empty() {
        assert!(Project::seed().is_empty());
    }
}
