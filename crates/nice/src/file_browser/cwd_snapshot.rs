//! `cwd_snapshot` — the `WindowRegistry`-walking builder for the pure
//! [`nice_model::file_browser::cwd_impact`] rule (F8). Ported from
//! `FileBrowserCWDImpactCheck.snapshot(from:)` (`FileBrowserCWDImpactCheck.swift:96-125`).
//!
//! The string-prefix decision ([`affected_by`](nice_model::file_browser::affected_by))
//! and its value types are pure and live in `nice-model`; only this walk —
//! which reaches every live window's [`WindowState`](crate::window_state::WindowState)
//! model through the registry — is registry-dependent, so it lands here.
//!
//! Every window/project/session contributes: one synthetic session-anchor entry
//! ([`Session::cwd`](nice_model::Session)) plus one entry per `is_alive` window with a
//! non-empty OSC-7 cwd. The per-session projection ([`entries_for_session`]) is pure over
//! a plain [`nice_model::Session`] so it is table-tested without a gpui `App`; the
//! registry walk ([`build_snapshot`]) is exercised by the rename flow + scenario.

use gpui::App;

use nice_model::file_browser::cwd_impact::normalize_path;
use nice_model::file_browser::{TermWindowCWDRef, TermWindowCWDSnapshot};
use nice_model::{TermWindowKind, Session};

use crate::window_registry::WindowRegistry;

/// The CWD references a single session contributes: the session anchor (`Session.cwd`, an
/// empty `term_window_id` + a [`TermWindowKind::Terminal`] sentinel — the message only counts,
/// it never distinguishes kinds) plus one entry per `is_alive` window carrying a
/// non-empty OSC-7 cwd. Each `cwd` is normalized (trailing slash stripped) so the
/// prefix rule in [`affected_by`](nice_model::file_browser::affected_by) sees
/// canonical forms.
pub fn entries_for_session(window_session_id: &str, session: &Session) -> Vec<TermWindowCWDRef> {
    let mut out = Vec::new();
    // The synthetic session-anchor entry (always present — Swift adds `Session.cwd`
    // unconditionally; an empty cwd normalizes to "" and simply never matches).
    out.push(TermWindowCWDRef {
        window_session_id: window_session_id.to_string(),
        session_id: session.id.clone(),
        window_id: String::new(),
        kind: TermWindowKind::Terminal,
        cwd: normalize_path(&session.cwd),
    });
    for term_window in session.windows.iter().filter(|w| w.is_alive) {
        if let Some(cwd) = term_window.cwd.as_deref().filter(|c| !c.is_empty()) {
            out.push(TermWindowCWDRef {
                window_session_id: window_session_id.to_string(),
                session_id: session.id.clone(),
                window_id: term_window.id.clone(),
                kind: term_window.kind,
                cwd: normalize_path(cwd),
            });
        }
    }
    out
}

/// Build a [`TermWindowCWDSnapshot`] over every live window's sessions by walking the
/// [`WindowRegistry`]. Runs once at the start of a rename attempt so the CWD-impact
/// check sees a consistent view. Empty when no registry is installed.
pub fn build_snapshot(cx: &App) -> TermWindowCWDSnapshot {
    let mut entries = Vec::new();
    for state in WindowRegistry::all_states(cx) {
        let ws = state.read(cx);
        let window_session_id = ws.window_session_id().to_string();
        for project in &ws.workspace.projects {
            for session in &project.sessions {
                entries.extend(entries_for_session(&window_session_id, session));
            }
        }
    }
    TermWindowCWDSnapshot { entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::{TermWindow, TermWindowKind, Session};

    fn session_with_cwd(id: &str, cwd: &str) -> Session {
        Session::new(id, "title", cwd)
    }

    /// The session-anchor entry is always present, carrying the (normalized) session cwd,
    /// an empty term_window_id, and the Terminal sentinel kind.
    #[test]
    fn entries_for_session_includes_session_anchor() {
        let session = session_with_cwd("t1", "/proj/nice/");
        let entries = entries_for_session("win-A", &session);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].window_session_id, "win-A");
        assert_eq!(entries[0].session_id, "t1");
        assert_eq!(entries[0].window_id, "");
        assert_eq!(entries[0].cwd, "/proj/nice", "trailing slash normalized off");
    }

    /// Each `is_alive` window with a non-empty OSC-7 cwd contributes an entry;
    /// dead windows and windows without an OSC-7 cwd are skipped.
    #[test]
    fn entries_for_session_includes_live_windows_with_cwd() {
        let mut session = session_with_cwd("t1", "/proj");
        let mut alive = TermWindow::new("p1", "sh", TermWindowKind::Terminal);
        alive.cwd = Some("/proj/src/".to_string());
        let mut dead = TermWindow::new("p2", "sh", TermWindowKind::Terminal);
        dead.is_alive = false;
        dead.cwd = Some("/proj/dead".to_string());
        let mut no_cwd = TermWindow::new("p3", "claude", TermWindowKind::Claude);
        no_cwd.cwd = None;
        session.windows = vec![alive, dead, no_cwd];

        let entries = entries_for_session("win-A", &session);
        // session-anchor + the one live window with a cwd (dead + no-cwd skipped).
        assert_eq!(entries.len(), 2);
        let window_entry = entries.iter().find(|e| e.window_id == "p1").unwrap();
        assert_eq!(window_entry.cwd, "/proj/src", "window cwd normalized");
        assert!(entries.iter().all(|e| e.window_id != "p2"), "dead window skipped");
        assert!(entries.iter().all(|e| e.window_id != "p3"), "no-cwd window skipped");
    }

    /// A claude window's kind is preserved on its entry (the message doesn't use it,
    /// but the value carries through — matching the pure `affected_by` test).
    #[test]
    fn entries_for_session_preserves_window_kind() {
        let mut session = session_with_cwd("t1", "/proj");
        let mut claude = TermWindow::new("p1", "claude", TermWindowKind::Claude);
        claude.cwd = Some("/proj/sub".to_string());
        session.windows = vec![claude];
        let entries = entries_for_session("win-A", &session);
        let window_entry = entries.iter().find(|e| e.window_id == "p1").unwrap();
        assert_eq!(window_entry.kind, TermWindowKind::Claude);
    }
}
