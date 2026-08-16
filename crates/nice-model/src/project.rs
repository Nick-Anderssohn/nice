//! `Project` — an ordered group of sessions — ported from
//! `Sources/Nice/State/Models.swift`.

use serde::{Deserialize, Serialize};

use crate::session::Session;

/// A project: a named, path-rooted group of [`Session`]s rendered as one sidebar
/// section. The Terminals project + cwd bucketing that populate these are
/// provided by [`crate::WorkspaceModel`] (`add_session_to_projects`); this is the pure
/// value type.
///
/// **No `Eq`/`Hash`** — it contains [`Session`]s, whose windows carry `f32`
/// split ratios since tmux-port Phase 2; see [`crate::TermWindow`]'s note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    /// Serialized as `tabs` — frozen spelling.
    #[serde(rename = "tabs")]
    pub sessions: Vec<Session>,
}

impl Project {
    /// The empty seed set — the projects list starts empty and is populated by
    /// `WorkspaceModel` seeding (`Models.swift:270-272`).
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
