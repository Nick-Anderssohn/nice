//! `rename_validator` — the pure inline-rename validator, ported from
//! `FileBrowserRenameValidator.swift`. Three responsibilities: [`can_rename`]
//! (the cheap pre-flight gate all three rename triggers consult, refusing only
//! the filesystem root `/`), [`validate`] (the typed-draft rules Finder uses
//! plus a sibling-collision pre-flight), and [`is_extension_change`] (whether
//! the rename changes the file's extension, so the row can present the
//! Finder-style confirmation).
//!
//! The Swift `validate` takes a `FileManager` for the sibling-collision check;
//! this port injects an `exists` predicate so tests pass a closure over a temp
//! dir (or a plain set) — no filesystem ownership. `can_rename` and
//! `is_extension_change` are pure-string predicates.

use super::naming::split_name_and_extension;
use std::path::Path;

/// Outcome of evaluating a rename draft. The row (a later slice) maps each case
/// to a concrete commit / cancel / keep-editing action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameValidation {
    /// Draft is fine; commit to this destination path.
    Ok(String),
    /// Empty / whitespace-only draft. Cancel back to original.
    Empty,
    /// Draft equals the original last path component. Cancel silently.
    Unchanged,
    /// Draft contains `/` or `:` — both illegal in a single path component on
    /// macOS. Stay in edit mode so the user fixes it.
    ContainsSlash,
    /// A sibling at the parent already has this name. The destination path is
    /// included so the row can quote the offending name in the drift message.
    WouldCollide(String),
    /// `original_path` is the filesystem root `/`. Defense in depth — the
    /// trigger gates already block opening the field for `/`.
    IsFilesystemRoot,
}

/// Cheap pre-flight gate consulted by the trigger paths. Returns `false` for
/// the filesystem root `/`, `true` everywhere else. The `/` has no parent, so a
/// rename can never produce a valid destination. A trailing slash is stripped
/// first so `/` and `//` both normalize to root (Swift relies on `URL.path`
/// stripping it).
pub fn can_rename(path: &str) -> bool {
    strip_trailing_slash(path) != "/"
}

/// Evaluate `draft` against `original_path`. `exists(candidate)` is the only
/// filesystem touch — the sibling-collision check — and is injected so tests
/// stay hermetic. Order matches Swift: root → empty → unchanged → illegal char
/// → collision → ok.
pub fn validate(
    original_path: &str,
    draft: &str,
    exists: impl Fn(&str) -> bool,
) -> RenameValidation {
    if !can_rename(original_path) {
        return RenameValidation::IsFilesystemRoot;
    }

    let trimmed = draft.trim();
    if trimmed.is_empty() {
        return RenameValidation::Empty;
    }
    if trimmed == last_component(original_path) {
        return RenameValidation::Unchanged;
    }
    // Slash separates path components and `:` is the legacy HFS separator; both
    // Finder and POSIX reject either inside a single name. Slash is the
    // canonical "stay in edit mode" signal — don't silently treat the input as
    // a path.
    if trimmed.contains('/') || trimmed.contains(':') {
        return RenameValidation::ContainsSlash;
    }

    let parent = Path::new(original_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let candidate = if parent.is_empty() {
        trimmed.to_string()
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), trimmed)
    };
    if exists(&candidate) {
        RenameValidation::WouldCollide(candidate)
    } else {
        RenameValidation::Ok(candidate)
    }
}

/// True iff `original_name` and `new_name` differ in extension. Defers to
/// [`split_name_and_extension`], which handles dotfiles (`.zshrc` whole-name
/// base, `.zshrc.bak` split at last dot). Pinned cases match
/// `FileBrowserRenameValidator.swift:96-102`.
pub fn is_extension_change(original_name: &str, new_name: &str) -> bool {
    let (_, old_ext) = split_name_and_extension(original_name);
    let (_, new_ext) = split_name_and_extension(new_name);
    old_ext != new_ext
}

/// The last path component of an absolute path, trailing slash ignored.
fn last_component(path: &str) -> &str {
    let trimmed = strip_trailing_slash(path);
    match trimmed.rfind('/') {
        None => trimmed,
        Some(idx) => &trimmed[idx + 1..],
    }
}

/// Strip a single trailing `/` (other than for the root `/` itself).
fn strip_trailing_slash(path: &str) -> &str {
    if path.len() > 1 && path.ends_with('/') {
        &path[..path.len() - 1]
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// An `exists` predicate over an explicit set of sibling paths.
    fn siblings(paths: &[&str]) -> impl Fn(&str) -> bool {
        let set: HashSet<String> = paths.iter().map(|s| s.to_string()).collect();
        move |p: &str| set.contains(p)
    }

    // MARK: - can_rename

    /// `FileBrowserRenameValidatorTests.test_canRename_isFalse_forFilesystemRoot`
    #[test]
    fn can_rename_is_false_for_filesystem_root() {
        assert!(!can_rename("/"));
    }

    /// `FileBrowserRenameValidatorTests.test_canRename_isTrue_forAnythingElse`
    #[test]
    fn can_rename_is_true_for_anything_else() {
        assert!(can_rename("/Users/nick/Projects/foo.txt"));
        assert!(can_rename("/private/etc"));
    }

    // MARK: - validate

    /// `FileBrowserRenameValidatorTests.test_validate_emptyDraft_returnsEmpty`
    #[test]
    fn validate_empty_draft_returns_empty() {
        let none = siblings(&[]);
        assert_eq!(
            validate("/tmp/foo.txt", "", &none),
            RenameValidation::Empty
        );
        assert_eq!(
            validate("/tmp/foo.txt", "   ", &none),
            RenameValidation::Empty
        );
    }

    /// `FileBrowserRenameValidatorTests.test_validate_unchangedDraft_returnsUnchanged`
    #[test]
    fn validate_unchanged_draft_returns_unchanged() {
        let none = siblings(&[]);
        assert_eq!(
            validate("/tmp/foo.txt", "foo.txt", &none),
            RenameValidation::Unchanged
        );
        // Trailing whitespace is trimmed.
        assert_eq!(
            validate("/tmp/foo.txt", " foo.txt ", &none),
            RenameValidation::Unchanged
        );
    }

    /// `FileBrowserRenameValidatorTests.test_validate_draftWithSlash_returnsContainsSlash`
    #[test]
    fn validate_draft_with_slash_returns_contains_slash() {
        let none = siblings(&[]);
        assert_eq!(
            validate("/tmp/foo.txt", "bar/baz.txt", &none),
            RenameValidation::ContainsSlash
        );
    }

    /// `FileBrowserRenameValidatorTests.test_validate_draftWithColon_returnsContainsSlash`
    #[test]
    fn validate_draft_with_colon_returns_contains_slash() {
        let none = siblings(&[]);
        assert_eq!(
            validate("/tmp/foo.txt", "bar:baz.txt", &none),
            RenameValidation::ContainsSlash
        );
    }

    /// `FileBrowserRenameValidatorTests.test_validate_draftCollidesWithSibling_returnsWouldCollide`
    #[test]
    fn validate_draft_collides_with_sibling_returns_would_collide() {
        let exists = siblings(&["/tmp/bar.txt"]);
        assert_eq!(
            validate("/tmp/foo.txt", "bar.txt", &exists),
            RenameValidation::WouldCollide("/tmp/bar.txt".into())
        );
    }

    /// `FileBrowserRenameValidatorTests.test_validate_filesystemRoot_returnsIsFilesystemRoot`
    #[test]
    fn validate_filesystem_root_returns_is_filesystem_root() {
        let none = siblings(&[]);
        assert_eq!(
            validate("/", "newroot", &none),
            RenameValidation::IsFilesystemRoot
        );
    }

    /// `FileBrowserRenameValidatorTests.test_validate_okDraft_returnsOkWithDestinationURL`
    #[test]
    fn validate_ok_draft_returns_ok_with_destination() {
        let none = siblings(&[]);
        assert_eq!(
            validate("/tmp/foo.txt", "renamed.txt", &none),
            RenameValidation::Ok("/tmp/renamed.txt".into())
        );
    }

    // MARK: - is_extension_change

    /// `FileBrowserRenameValidatorTests.test_isExtensionChange_extensionDiffers_isTrue`
    #[test]
    fn is_extension_change_extension_differs_is_true() {
        assert!(is_extension_change("foo.txt", "foo.md"));
    }

    /// `FileBrowserRenameValidatorTests.test_isExtensionChange_basenameOnly_isFalse`
    #[test]
    fn is_extension_change_basename_only_is_false() {
        assert!(!is_extension_change("foo.txt", "bar.txt"));
    }

    /// `FileBrowserRenameValidatorTests.test_isExtensionChange_addedOrRemovedExtension_isTrue`
    #[test]
    fn is_extension_change_added_or_removed_extension_is_true() {
        assert!(is_extension_change("foo", "foo.txt"));
        assert!(is_extension_change("foo.txt", "foo"));
    }

    /// `FileBrowserRenameValidatorTests.test_isExtensionChange_dotfileToDotfileWithExt_isTrue`
    #[test]
    fn is_extension_change_dotfile_to_dotfile_with_ext_is_true() {
        assert!(is_extension_change(".zshrc", ".zshrc.bak"));
    }

    /// `FileBrowserRenameValidatorTests.test_isExtensionChange_dotfileRenameWithinDotfile_isFalse`
    #[test]
    fn is_extension_change_dotfile_rename_within_dotfile_is_false() {
        assert!(!is_extension_change(".zshrc", ".gitignore"));
    }
}
