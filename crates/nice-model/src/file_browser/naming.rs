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

#[cfg(test)]
mod tests {
    use super::*;

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
}
