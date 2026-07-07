//! `drop_resolver` — the pure drag-to-folder decision rules, ported from
//! `FileBrowserDropResolver.swift`. AppKit/SwiftUI-free helpers the row drop
//! delegate (a later slice, in `crates/nice`) calls: folder-into-self /
//! folder-into-descendant rejection, same-parent no-op, Option-as-copy,
//! cross-volume-as-copy.
//!
//! The Swift `operation(modifierFlags:sameVolume:)` reads
//! `NSEvent.ModifierFlags`; the modifier is read at drop time in the view
//! layer (`window.modifiers()`), so this pure rule takes a plain `option_held`
//! bool. `areOnSameVolume` (the `URLResourceValues` volume probe) touches the
//! filesystem and stays in the `crates/nice` DnD slice; `same_volume` is
//! hoisted to a parameter here exactly as Swift hoists it for its unit tests.

/// Whether a drop should move or copy — ported from `FileDragOperation`
/// (`FileBrowserDragState.swift:21-24`). Resolved per-drop from the Option
/// modifier + same/cross-volume comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDragOperation {
    Move,
    Copy,
}

/// Whether a drop of `sources` into `dest` would do anything. Returns `false`
/// for any of: empty `sources`; `dest` equals one of the sources; `dest` is a
/// descendant of one of the sources (would form a cycle); or every source
/// already lives directly inside `dest`. Returns `true` as long as at least
/// one source would actually move/copy and none of the cycle rules apply.
///
/// Pure path-string logic — no filesystem reads. Paths are lexically
/// standardized (trailing slash stripped, `.`/`..`/empty components resolved)
/// so `/tmp/a` and `/tmp/a/` agree; callers ensure `dest` is an existing
/// directory. The descendant check uses a `src + "/"` prefix so `/tmp/abc` is
/// NOT treated as a descendant of `/tmp/a` (the Swift substring-guard case).
pub fn can_drop(sources: &[&str], dest: &str) -> bool {
    if sources.is_empty() {
        return false;
    }
    let dest_path = standardize(dest);
    let mut any_would_move = false;
    for src in sources {
        let src_path = standardize(src);
        if dest_path == src_path {
            return false;
        }
        if dest_path.starts_with(&format!("{src_path}/")) {
            return false;
        }
        let parent = parent_path(&src_path);
        if parent != dest_path {
            any_would_move = true;
        }
    }
    any_would_move
}

/// Resolve move-vs-copy for a drop, matching Finder defaults: Option held →
/// always copy; cross-volume → copy (a raw cross-volume rename fails anyway,
/// and Finder's UX is to copy); same-volume + no Option → move.
pub fn operation(option_held: bool, same_volume: bool) -> FileDragOperation {
    if option_held {
        return FileDragOperation::Copy;
    }
    if same_volume {
        FileDragOperation::Move
    } else {
        FileDragOperation::Copy
    }
}

/// Lexically standardize an absolute path for prefix/equality comparisons:
/// resolve empty/`.` components, pop on `..`, strip trailing slashes. Mirrors
/// `URL.standardizedFileURL.path` for the lexical cases the resolver relies on
/// (no symlink resolution — the drop delegate never needs it).
fn standardize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut comps: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            x => comps.push(x),
        }
    }
    let joined = comps.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The parent directory of a standardized path (the Swift
/// `deletingLastPathComponent().standardizedFileURL.path`).
fn parent_path(standardized: &str) -> String {
    match standardized.rfind('/') {
        None => String::new(),
        Some(0) => "/".to_string(),
        Some(idx) => standardized[..idx].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::FileDragOperation::{Copy, Move};
    use super::*;

    // MARK: - can_drop

    /// `FileBrowserDropResolverTests.test_canDrop_acceptsSibling`
    #[test]
    fn can_drop_accepts_sibling() {
        assert!(can_drop(&["/tmp/a/file.txt"], "/tmp/b"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsEmptySources`
    #[test]
    fn can_drop_rejects_empty_sources() {
        assert!(!can_drop(&[], "/tmp/b"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsSelfDrop`
    #[test]
    fn can_drop_rejects_self_drop() {
        assert!(!can_drop(&["/tmp/a"], "/tmp/a"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsDescendantDrop`
    #[test]
    fn can_drop_rejects_descendant_drop() {
        assert!(!can_drop(&["/tmp/a"], "/tmp/a/sub"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsDeepDescendantDrop`
    #[test]
    fn can_drop_rejects_deep_descendant_drop() {
        assert!(!can_drop(&["/tmp/a"], "/tmp/a/sub/inner"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_acceptsSiblingPrefixedFolder`
    #[test]
    fn can_drop_accepts_sibling_prefixed_folder() {
        // `/tmp/abc` shares a prefix with `/tmp/a` but isn't a descendant.
        assert!(can_drop(&["/tmp/a"], "/tmp/abc"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsParentEqualsDest`
    #[test]
    fn can_drop_rejects_parent_equals_dest() {
        assert!(!can_drop(&["/tmp/a/file.txt"], "/tmp/a"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_acceptsBatchEvenIfOneSourceAlreadyHere`
    #[test]
    fn can_drop_accepts_batch_even_if_one_source_already_here() {
        assert!(can_drop(
            &["/tmp/a/file.txt", "/tmp/other/file2.txt"],
            "/tmp/a"
        ));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsBatchWhenAllSourcesAlreadyHere`
    #[test]
    fn can_drop_rejects_batch_when_all_sources_already_here() {
        assert!(!can_drop(&["/tmp/x/a.txt", "/tmp/x/b.txt"], "/tmp/x"));
    }

    /// `FileBrowserDropResolverTests.test_canDrop_rejectsBatchContainingSelfDrop`
    #[test]
    fn can_drop_rejects_batch_containing_self_drop() {
        assert!(!can_drop(&["/tmp/other/file.txt", "/tmp/a"], "/tmp/a/sub"));
    }

    /// Trailing-slash forms agree after standardization.
    #[test]
    fn can_drop_standardizes_trailing_slash() {
        assert!(!can_drop(&["/tmp/a/"], "/tmp/a"));
        assert!(!can_drop(&["/tmp/a/file.txt"], "/tmp/a/"));
    }

    // MARK: - operation

    /// `FileBrowserDropResolverTests.test_operation_sameVolumeNoModifier_isMove`
    #[test]
    fn operation_same_volume_no_modifier_is_move() {
        assert_eq!(operation(false, true), Move);
    }

    /// `FileBrowserDropResolverTests.test_operation_sameVolumeOptionHeld_isCopy`
    #[test]
    fn operation_same_volume_option_held_is_copy() {
        assert_eq!(operation(true, true), Copy);
    }

    /// `FileBrowserDropResolverTests.test_operation_crossVolume_isCopyWithoutModifier`
    #[test]
    fn operation_cross_volume_is_copy_without_modifier() {
        assert_eq!(operation(false, false), Copy);
    }

    /// `FileBrowserDropResolverTests.test_operation_crossVolume_optionHeld_stillCopy`
    #[test]
    fn operation_cross_volume_option_held_still_copy() {
        assert_eq!(operation(true, false), Copy);
    }

    /// `FileBrowserDropResolverTests.test_operation_ignoresOtherModifiers` —
    /// only Option flips move→copy; Cmd/Shift/Control (i.e. `option_held ==
    /// false`) leave a same-volume drop a move.
    #[test]
    fn operation_ignores_other_modifiers() {
        assert_eq!(operation(false, true), Move);
    }
}
