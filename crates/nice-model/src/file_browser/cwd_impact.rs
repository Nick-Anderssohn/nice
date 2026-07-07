//! `cwd_impact` — the pure CWD-invalidation rule, ported from
//! `FileBrowserCWDImpactCheck.swift`. Decides whether renaming a file or folder
//! would invalidate any open terminal pane's working directory: any pane whose
//! live `cwd` (or its tab's anchor `cwd`) equals the renamed path or is a
//! descendant of it is "affected" — after the on-disk move that pane sits in a
//! path that no longer exists.
//!
//! Only the string-prefix algorithm ([`affected_by`] + [`normalize_path`]) and
//! the snapshot value types live here — the `WindowRegistry`-walking snapshot
//! builder (`FileBrowserCWDImpactCheck.snapshot(from:)`) is registry-dependent
//! and lands with the `crates/nice` rename slice.

use crate::PaneKind;

/// One CWD reference captured by the snapshot: either a live pane or a tab
/// anchor. Tab-anchor entries carry an empty `pane_id` and use
/// [`PaneKind::Terminal`] as a sentinel — the alert message doesn't distinguish
/// kinds, it just counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCWDRef {
    pub window_session_id: String,
    pub tab_id: String,
    /// Empty string for tab-anchor entries (`Tab.cwd` rather than a pane).
    pub pane_id: String,
    pub kind: PaneKind,
    /// Absolute path. Trailing slash normalized off by the snapshot builder so
    /// prefix matching is straightforward.
    pub cwd: String,
}

/// Flat list of every CWD reference across every window. Built once at the
/// start of a rename attempt so the check runs against a consistent view.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneCWDSnapshot {
    pub entries: Vec<PaneCWDRef>,
}

/// Return every snapshot entry whose `cwd` would be invalidated by renaming
/// `old_path`. Match rule: `cwd == old_path` (renaming the exact directory the
/// shell is in) OR `cwd.starts_with(old_path + "/")` (renaming an ancestor).
///
/// `old_path` is normalized to drop a trailing slash so callers can pass either
/// form. `old_path == "/"` is excluded by the `can_rename` gate at the trigger
/// layer; handled here too (every CWD would match) by returning an empty list —
/// there's no useful rename to warn about.
pub fn affected_by(old_path: &str, snapshot: &PaneCWDSnapshot) -> Vec<PaneCWDRef> {
    let normalized = normalize_path(old_path);
    if normalized == "/" {
        return Vec::new();
    }
    let prefix = format!("{normalized}/");
    snapshot
        .entries
        .iter()
        .filter(|entry| {
            let cwd = normalize_path(&entry.cwd);
            cwd == normalized || cwd.starts_with(&prefix)
        })
        .cloned()
        .collect()
}

/// Strip a single trailing `/` from `path` (other than the root `/`). The
/// snapshot builder runs every `cwd` through this so equality and prefix tests
/// in [`affected_by`] see canonical forms.
pub fn normalize_path(path: &str) -> String {
    if path.len() > 1 && path.ends_with('/') {
        path[..path.len() - 1].to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ref(cwd: &str, pane_id: &str, kind: PaneKind) -> PaneCWDRef {
        PaneCWDRef {
            window_session_id: "win-1".into(),
            tab_id: "tab-1".into(),
            pane_id: pane_id.into(),
            kind,
            cwd: cwd.into(),
        }
    }

    fn make_snapshot(cwds: &[&str]) -> PaneCWDSnapshot {
        PaneCWDSnapshot {
            entries: cwds
                .iter()
                .enumerate()
                .map(|(i, cwd)| make_ref(cwd, &format!("p{i}"), PaneKind::Terminal))
                .collect(),
        }
    }

    /// `FileBrowserCWDImpactCheckTests.test_exactMatch_isAffected`
    #[test]
    fn exact_match_is_affected() {
        let snapshot = make_snapshot(&["/Users/nick/Projects/nice"]);
        let affected = affected_by("/Users/nick/Projects/nice", &snapshot);
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].cwd, "/Users/nick/Projects/nice");
    }

    /// `FileBrowserCWDImpactCheckTests.test_ancestor_isAffected`
    #[test]
    fn ancestor_is_affected() {
        let snapshot = make_snapshot(&["/Users/nick/Projects/nice/src"]);
        assert_eq!(affected_by("/Users/nick/Projects/nice", &snapshot).len(), 1);
    }

    /// `FileBrowserCWDImpactCheckTests.test_siblingPrefix_isNotAffected`
    #[test]
    fn sibling_prefix_is_not_affected() {
        // /a/b should NOT match cwd=/a/bc — the trailing-slash guard prevents
        // the false-positive prefix match.
        let snapshot = make_snapshot(&["/Users/nick/Projects/nicely"]);
        assert!(affected_by("/Users/nick/Projects/nice", &snapshot).is_empty());
    }

    /// `FileBrowserCWDImpactCheckTests.test_unrelated_isNotAffected`
    #[test]
    fn unrelated_is_not_affected() {
        let snapshot = make_snapshot(&["/Users/nick/Documents", "/tmp"]);
        assert!(affected_by("/Users/nick/Projects/nice", &snapshot).is_empty());
    }

    /// `FileBrowserCWDImpactCheckTests.test_trailingSlashOnOldPath_isNormalized`
    #[test]
    fn trailing_slash_on_old_path_is_normalized() {
        let snapshot = make_snapshot(&["/Users/nick/Projects/nice/src"]);
        assert_eq!(
            affected_by("/Users/nick/Projects/nice/", &snapshot).len(),
            1
        );
    }

    /// `FileBrowserCWDImpactCheckTests.test_filesystemRoot_isExcluded`
    #[test]
    fn filesystem_root_is_excluded() {
        let snapshot = make_snapshot(&["/Users/nick", "/tmp", "/private"]);
        assert!(affected_by("/", &snapshot).is_empty());
    }

    /// `FileBrowserCWDImpactCheckTests.test_multipleEntries_allMatchingReturned`
    #[test]
    fn multiple_entries_all_matching_returned() {
        let snapshot = PaneCWDSnapshot {
            entries: vec![
                make_ref("/proj/foo", "p1", PaneKind::Terminal),
                make_ref("/proj/foo/sub", "p2", PaneKind::Claude),
                make_ref("/proj/foobar", "p3", PaneKind::Terminal),
                make_ref("/elsewhere", "p4", PaneKind::Terminal),
            ],
        };
        let affected = affected_by("/proj/foo", &snapshot);
        let ids: std::collections::HashSet<&str> =
            affected.iter().map(|r| r.pane_id.as_str()).collect();
        assert_eq!(ids, ["p1", "p2"].into_iter().collect());
        // Kinds are preserved on the returned refs.
        assert_eq!(
            affected.iter().find(|r| r.pane_id == "p2").map(|r| r.kind),
            Some(PaneKind::Claude)
        );
    }

    /// `FileBrowserCWDImpactCheckTests.test_normalizePath_stripsTrailingSlash`
    #[test]
    fn normalize_path_strips_trailing_slash() {
        assert_eq!(normalize_path("/foo/bar/"), "/foo/bar");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/foo/bar"), "/foo/bar");
    }
}
