//! `split_name_and_extension` — the pure last-dot filename split, ported from
//! `FileOperationsService.splitNameAndExtension`
//! (`FileOperationsService.swift:306-327`).
//!
//! Ported ONCE here (the plan's "port ONCE as a pure `nice-model` function")
//! because three R20 consumers share the exact rule: the ops engine's
//! collision auto-rename (`foo copy.txt`), the rename validator's
//! `is_extension_change`, and the rename field's basename preselection. Finder
//! semantics: only the LAST dot separates the extension, and a leading dot is
//! part of the base name, not a separator — so `.zshrc` is all base, while
//! `.zshrc.bak` splits at the last dot.

/// Split `name` into `(base, ext)` at the last `.`, Finder-style.
///
/// * `"archive.tar.gz"` → `("archive.tar", "gz")` (last-dot only).
/// * `"foo.txt"` → `("foo", "txt")`.
/// * `"foo"` → `("foo", "")` (no extension).
/// * `".zshrc"` → `(".zshrc", "")` (leading-dot name: whole name is base).
/// * `".zshrc.bak"` → `(".zshrc", "bak")` (leading-dot name with a later dot).
///
/// `ext` never includes the dot. Byte indices from [`str::rfind`] land on the
/// `.` (a one-byte char) so multi-byte basenames (`café 文件.txt`) split
/// correctly.
pub fn split_name_and_extension(name: &str) -> (String, String) {
    // Names that *start* with a dot (`.zshrc`) treat the leading dot as part of
    // the base name, not a separator.
    if let Some(trimmed) = name.strip_prefix('.') {
        match trimmed.rfind('.') {
            None => (name.to_string(), String::new()),
            Some(dot) => {
                let base = format!(".{}", &trimmed[..dot]);
                let ext = trimmed[dot + 1..].to_string();
                (base, ext)
            }
        }
    } else {
        match name.rfind('.') {
            None => (name.to_string(), String::new()),
            Some(dot) => {
                let base = name[..dot].to_string();
                let ext = name[dot + 1..].to_string();
                (base, ext)
            }
        }
    }
}

/// First free `"untitled folder"`, `"untitled folder 2"`, `"untitled folder 3"`,
/// … under a directory, Finder-style.
///
/// Finder's new-folder suffixing (a bare ` 2`, ` 3`, … after the base — never
/// ` copy`) differs from the ops engine's copy-collision `next_available_name`,
/// so New Folder needs its own helper. `exists` is injected so the function
/// stays pure and table-testable: production passes a `dir.join(n).exists()`
/// probe; tests pass a set membership check.
pub fn new_folder_name(exists: impl Fn(&str) -> bool) -> String {
    const BASE: &str = "untitled folder";
    if !exists(BASE) {
        return BASE.to_string();
    }
    // ` 2`, ` 3`, … until free. Bounded at 9999 to match the ops engine's
    // `next_available_name` backstop: a real filesystem never reaches it, but the
    // cap keeps a pathological injected `exists` predicate (tests) from looping.
    for index in 2u32..=9999 {
        let name = format!("{BASE} {index}");
        if !exists(&name) {
            return name;
        }
    }
    format!("{BASE} 9999")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// `FileOperationsServiceTests.test_splitName_normalFile`
    #[test]
    fn normal_file() {
        assert_eq!(
            split_name_and_extension("foo.txt"),
            ("foo".into(), "txt".into())
        );
    }

    /// `FileOperationsServiceTests.test_splitName_dotfileNoExtension`
    #[test]
    fn dotfile_no_extension() {
        assert_eq!(
            split_name_and_extension(".zshrc"),
            (".zshrc".into(), String::new())
        );
    }

    /// `FileOperationsServiceTests.test_splitName_dotfileWithExtension`
    #[test]
    fn dotfile_with_extension() {
        assert_eq!(
            split_name_and_extension(".zshrc.bak"),
            (".zshrc".into(), "bak".into())
        );
    }

    /// Last-dot-only split — the `nextAvailableName` extension-preservation
    /// pin (`test_nextAvailableName_preservesExtension`).
    #[test]
    fn last_dot_only() {
        assert_eq!(
            split_name_and_extension("archive.tar.gz"),
            ("archive.tar".into(), "gz".into())
        );
    }

    #[test]
    fn no_extension() {
        assert_eq!(
            split_name_and_extension("folder"),
            ("folder".into(), String::new())
        );
    }

    #[test]
    fn multibyte_basename_preserved() {
        assert_eq!(
            split_name_and_extension("café 文件.txt"),
            ("café 文件".into(), "txt".into())
        );
    }

    // MARK: - new_folder_name

    /// Empty directory: the base name is free.
    #[test]
    fn new_folder_name_base_when_free() {
        let taken: HashSet<&str> = HashSet::new();
        assert_eq!(new_folder_name(|n| taken.contains(n)), "untitled folder");
    }

    /// Base taken: first suffix is a bare ` 2` (no ` copy`, no ` 1`).
    #[test]
    fn new_folder_name_appends_2_on_collision() {
        let taken: HashSet<&str> = ["untitled folder"].into_iter().collect();
        assert_eq!(new_folder_name(|n| taken.contains(n)), "untitled folder 2");
    }

    /// Base + ` 2` taken: skips to ` 3`.
    #[test]
    fn new_folder_name_skips_to_3() {
        let taken: HashSet<&str> = ["untitled folder", "untitled folder 2"]
            .into_iter()
            .collect();
        assert_eq!(new_folder_name(|n| taken.contains(n)), "untitled folder 3");
    }

    /// A gap is filled: base + ` 3` taken but ` 2` free lands on ` 2`.
    #[test]
    fn new_folder_name_fills_gap() {
        let taken: HashSet<&str> = ["untitled folder", "untitled folder 3"]
            .into_iter()
            .collect();
        assert_eq!(new_folder_name(|n| taken.contains(n)), "untitled folder 2");
    }
}
