//! The value types that carry a session mid-transfer between OS windows.
//!
//! A session normally lives inside exactly one window's `WorkspaceModel` +
//! `PtyManager`. Moving it to another window — the tear-off ⌃⌘N verb, and the
//! Move-to-Window verbs — lifts the whole thing out of the source: its model
//! subtree, its live pty payload, and the project it came out of, bundled into a
//! [`DetachedEntry`]. The receiving window lands it through the one
//! [`WindowState::adopt_entry`](crate::window_state::WindowState::adopt_entry)
//! primitive.
//!
//! The name "detached" describes the session's state WHILE in flight — lifted
//! out of its source window, not yet landed in a destination — not a persistent
//! pool (there is none). The live `Entity<TerminalSessionHandle>`s travel inside
//! the payload, so nothing respawns and no child ever notices the move.

use nice_model::Session;

use crate::pty_manager::DetachedPtys;

/// Where a transferred session came from, so adoption can re-home it into the
/// destination window's projects.
#[derive(Debug, Clone)]
pub(crate) struct DetachedProject {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: String,
}

/// One session in flight between windows: the model subtree, its live pty
/// payload, and the project it was lifted out of.
pub(crate) struct DetachedEntry {
    /// The whole model subtree, removed verbatim from its source window's
    /// `WorkspaceModel`.
    pub(crate) session: Session,
    /// The live half — opaque, owned by `pty_manager`. Empty for a ptyless
    /// (structural) payload: a never-activated restored session, or a
    /// model-alive-but-ptyless session that lazy-respawns on adopt-activate.
    pub(crate) ptys: DetachedPtys,
    /// Provenance for re-homing on adopt.
    pub(crate) project: DetachedProject,
}

#[cfg(test)]
mod adopt_tests;
#[cfg(test)]
mod tearoff_tests;
