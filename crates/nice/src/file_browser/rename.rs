//! `rename` — the gpui-free orchestration logic behind inline rename (F8),
//! ported from `FileExplorerOrchestrator.swift`'s rename slice (`:416-504`) and
//! `NSAlertRenameConfirmer` (`:512-544`). The `crates/nice/src/file_browser/view.rs`
//! rename UI wires these decisions to the real [`crate::confirmation_modal`]
//! (through [`WindowState::present_confirmation`](crate::window_state::WindowState))
//! and the real terminal refocus; this module keeps the DECISIONS pure so the
//! `FileExplorerOrchestratorRenameTests` semantics are table-tested without gpui.
//!
//! Three responsibilities:
//! * [`evaluate_commit`] — map a typed draft (via the pure
//!   [`nice_model::file_browser::validate_rename`]) to a concrete
//!   [`RenameCommit`] action, folding collisions to the FROZEN banner string.
//! * [`modals_for`] — the ORDERED confirmation modals a commit must clear first:
//!   the extension-change modal (non-directories only) FIRST, then the
//!   CWD-impact modal (runs unconditionally — one wasted walk for file renames,
//!   Swift parity). Verbatim wording pinned below.
//! * [`apply_rename`] — perform the commit as a RAW single-pair
//!   [`FileOperation::Move`] (deliberately bypassing collision auto-rename, the
//!   PROTECTED rename contract) and map an apply-time collision to the same
//!   frozen string.

use std::path::{Path, PathBuf};

use nice_model::file_browser::{
    affected_by, is_extension_change, split_name_and_extension, validate_rename, PaneCWDSnapshot,
    RenameValidation,
};

use super::ops::{
    FileOperation, FileOperationError, FileOperationItem, FileOperationOrigin,
    FileOperationsService,
};

/// The concrete action a rename-commit resolves to, after validating the trimmed
/// draft. The view maps each to a UI effect (commit / cancel / keep-editing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameCommit {
    /// Empty / unchanged / filesystem-root draft — cancel back to the original,
    /// no message (a silent no-op).
    Cancel,
    /// Draft contains `/` or `:` — STAY in edit mode so the user fixes it (no
    /// alert, no cancel). The one "keep the field open" signal.
    StayInEdit,
    /// A sibling already has this name (pre-flight). Surface the FROZEN banner
    /// string and cancel the edit — never a silent `foo copy`.
    Collision(String),
    /// The draft is valid; commit to `dest` (after clearing any [`modals_for`]).
    Proceed { dest: PathBuf },
}

/// One confirmation modal, in R18's exported component surface terms
/// (`title` = the message line, `message` = the informative line). The view
/// feeds these straight into
/// [`WindowState::present_confirmation`](crate::window_state::WindowState::present_confirmation)
/// with `destructive_confirm = true` (both R20 confirmations are destructive-styled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmSpec {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    pub cancel_label: String,
}

/// The FROZEN collision banner (`FileExplorerOrchestrator.swift:469-477`) — the
/// same string a pre-flight sibling collision AND an apply-time `AlreadyExists`
/// map to (vs Swift's `NSCocoaErrorDomain` substring match — identical observable
/// behaviour).
pub fn collision_message(new_name: &str) -> String {
    format!("Couldn't rename: '{new_name}' already exists.")
}

/// Evaluate the trimmed `draft` for renaming `original_path`. `exists(candidate)`
/// is the only filesystem touch (the sibling-collision pre-flight), injected so
/// tests stay hermetic. Order matches Swift (root → empty → unchanged → illegal
/// char → collision → ok).
pub fn evaluate_commit(
    original_path: &str,
    draft: &str,
    exists: impl Fn(&str) -> bool,
) -> RenameCommit {
    match validate_rename(original_path, draft, exists) {
        RenameValidation::Ok(dest) => RenameCommit::Proceed {
            dest: PathBuf::from(dest),
        },
        RenameValidation::Empty
        | RenameValidation::Unchanged
        | RenameValidation::IsFilesystemRoot => RenameCommit::Cancel,
        RenameValidation::ContainsSlash => RenameCommit::StayInEdit,
        RenameValidation::WouldCollide(dest) => {
            RenameCommit::Collision(collision_message(&last_component(&dest)))
        }
    }
}

/// Render an extension for the confirmation wording: `".txt"` or, for an empty
/// extension, the literal `"(no extension)"` (`FileBrowserView.swift:1040`).
fn ext_label(ext: &str) -> String {
    if ext.is_empty() {
        "(no extension)".to_string()
    } else {
        format!(".{ext}")
    }
}

/// The extension-change confirmation (non-directories only). Verbatim wording —
/// title (the message line) is the alert message, `message` (the informative
/// line) names the from/to extensions; the cancel button KEEPS the old extension,
/// the destructive confirm USES the new one.
pub fn extension_change_spec(original_name: &str, new_name: &str) -> ConfirmSpec {
    let (_, old_ext) = split_name_and_extension(original_name);
    let (_, new_ext) = split_name_and_extension(new_name);
    let old_label = ext_label(&old_ext);
    let new_label = ext_label(&new_ext);
    ConfirmSpec {
        title: "Are you sure you want to change the extension?".to_string(),
        message: format!(
            "If you change the extension from {old_label} to {new_label}, the file may open in a different app."
        ),
        confirm_label: format!("Use {new_label}"),
        cancel_label: format!("Keep {old_label}"),
    }
}

/// The CWD-impact confirmation, when renaming `folder_name` would invalidate
/// `count` open terminals' working directories. Literal `terminal(s)` pluralization
/// (Swift parity). Cancel leaves the fs untouched; the destructive confirm renames
/// anyway.
pub fn cwd_impact_spec(folder_name: &str, count: usize) -> ConfirmSpec {
    ConfirmSpec {
        title: format!("Rename will affect {count} open terminal(s)"),
        message: format!(
            "Renaming '{folder_name}' will break the working directory of {count} open terminal(s). \
             The terminal(s) will keep running but `pwd` will report a path that no longer exists. \
             Rename anyway?"
        ),
        confirm_label: "Rename Anyway".to_string(),
        cancel_label: "Cancel".to_string(),
    }
}

/// The ORDERED confirmation modals a rename commit must clear before applying:
/// the extension-change modal FIRST (non-directories on
/// [`is_extension_change`]), then the CWD-impact modal (walked unconditionally —
/// a file rename never matches, one wasted walk, Swift parity). Empty ⇒ commit
/// straight through (the injected-confirmer-absent default).
pub fn modals_for(
    original_path: &str,
    new_name: &str,
    is_dir: bool,
    snapshot: &PaneCWDSnapshot,
) -> Vec<ConfirmSpec> {
    let original_name = last_component(original_path);
    let mut specs = Vec::new();
    if !is_dir && is_extension_change(&original_name, new_name) {
        specs.push(extension_change_spec(&original_name, new_name));
    }
    let affected = affected_by(original_path, snapshot);
    if !affected.is_empty() {
        specs.push(cwd_impact_spec(&original_name, affected.len()));
    }
    specs
}

/// Apply a validated rename as a RAW single-pair [`FileOperation::Move`] —
/// deliberately BYPASSING the collision auto-rename so a race collision surfaces
/// as the frozen banner string, never a silent `foo copy` (the PROTECTED rename
/// contract). A defensive `dest.exists()` pre-flight maps to the same frozen
/// string as an apply-time `AlreadyExists`. Returns the recorded op (for the
/// history) on success, or the banner string on failure.
pub fn apply_rename(
    service: &FileOperationsService,
    source: &Path,
    dest: &Path,
    origin: FileOperationOrigin,
) -> Result<FileOperation, String> {
    let new_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dest.exists() {
        return Err(collision_message(&new_name));
    }
    let op = FileOperation::Move {
        items: vec![FileOperationItem {
            source: source.to_path_buf(),
            destination: dest.to_path_buf(),
        }],
        origin,
    };
    service.apply(op).map_err(|e| match e {
        // The ops engine maps an existing destination to an `AlreadyExists`
        // io error, flattened to `Underlying`; fold it back to the frozen string.
        FileOperationError::Underlying(m) if m.contains("already exists") => {
            collision_message(&new_name)
        }
        other => format!("Couldn't rename: {other}"),
    })
}

/// The last path component of `path` (trailing slash ignored).
fn last_component(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::file_browser::{PaneCWDRef, PaneCWDSnapshot};
    use nice_model::PaneKind;
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn siblings(paths: &[&str]) -> impl Fn(&str) -> bool {
        let set: HashSet<String> = paths.iter().map(|s| s.to_string()).collect();
        move |p: &str| set.contains(p)
    }

    struct TempTree {
        root: PathBuf,
    }
    impl TempTree {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "nice-rename-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn make_file(&self, name: &str, body: &str) -> PathBuf {
            let p = self.root.join(name);
            fs::write(&p, body).unwrap();
            p
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn origin() -> FileOperationOrigin {
        FileOperationOrigin::new("win-1", Some("tab-1".into()))
    }

    fn service(t: &TempTree) -> FileOperationsService {
        FileOperationsService::new(Box::new(super::super::ops::FakeTrasher::new(
            t.root.join("Trash"),
        )))
    }

    // MARK: - evaluate_commit (FileExplorerOrchestratorRenameTests)

    /// Empty draft cancels silently.
    #[test]
    fn evaluate_commit_empty_cancels() {
        assert_eq!(
            evaluate_commit("/tmp/foo.txt", "  ", siblings(&[])),
            RenameCommit::Cancel
        );
    }

    /// Draft equal to the original cancels silently.
    #[test]
    fn evaluate_commit_unchanged_cancels() {
        assert_eq!(
            evaluate_commit("/tmp/foo.txt", "foo.txt", siblings(&[])),
            RenameCommit::Cancel
        );
    }

    /// A `/` (or `:`) keeps the field open — never a silent path-treatment.
    #[test]
    fn evaluate_commit_slash_stays_in_edit() {
        assert_eq!(
            evaluate_commit("/tmp/foo.txt", "a/b.txt", siblings(&[])),
            RenameCommit::StayInEdit
        );
        assert_eq!(
            evaluate_commit("/tmp/foo.txt", "a:b.txt", siblings(&[])),
            RenameCommit::StayInEdit
        );
    }

    /// A sibling collision surfaces the FROZEN string (the attempted new name),
    /// never a `foo copy`.
    #[test]
    fn evaluate_commit_collision_is_frozen_string() {
        let got = evaluate_commit("/tmp/foo.txt", "bar.txt", siblings(&["/tmp/bar.txt"]));
        assert_eq!(
            got,
            RenameCommit::Collision("Couldn't rename: 'bar.txt' already exists.".to_string())
        );
    }

    /// A valid draft proceeds to the sibling destination path.
    #[test]
    fn evaluate_commit_ok_proceeds() {
        assert_eq!(
            evaluate_commit("/tmp/foo.txt", "renamed.txt", siblings(&[])),
            RenameCommit::Proceed {
                dest: PathBuf::from("/tmp/renamed.txt")
            }
        );
    }

    // MARK: - modals_for

    /// A non-directory extension change fires the extension modal FIRST, then the
    /// CWD modal (here empty snapshot ⇒ just the extension modal).
    #[test]
    fn modals_for_extension_change_fires_extension_modal() {
        let specs = modals_for("/tmp/foo.txt", "foo.md", false, &PaneCWDSnapshot::default());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].title, "Are you sure you want to change the extension?");
        assert!(specs[0].message.contains(".txt"));
        assert!(specs[0].message.contains(".md"));
        assert_eq!(specs[0].confirm_label, "Use .md");
        assert_eq!(specs[0].cancel_label, "Keep .txt");
    }

    /// A directory rename never fires the extension modal even when the "name"
    /// looks like it changes extension.
    #[test]
    fn modals_for_directory_skips_extension_modal() {
        let specs = modals_for("/tmp/my.dir", "my.folder", true, &PaneCWDSnapshot::default());
        assert!(specs.is_empty());
    }

    /// No-extension rendering uses the literal "(no extension)".
    #[test]
    fn modals_for_no_extension_uses_placeholder() {
        let specs = modals_for("/tmp/README", "README.md", false, &PaneCWDSnapshot::default());
        assert_eq!(specs.len(), 1);
        assert!(specs[0].message.contains("(no extension)"));
        assert_eq!(specs[0].cancel_label, "Keep (no extension)");
        assert_eq!(specs[0].confirm_label, "Use .md");
    }

    /// A folder rename that invalidates open terminals fires the CWD modal with
    /// the affected count; both modals fire (extension first) when applicable —
    /// here a folder so only the CWD modal.
    #[test]
    fn modals_for_cwd_impact_fires_cwd_modal() {
        let snapshot = PaneCWDSnapshot {
            entries: vec![
                PaneCWDRef {
                    window_session_id: "w".into(),
                    tab_id: "t".into(),
                    pane_id: "p1".into(),
                    kind: PaneKind::Terminal,
                    cwd: "/tmp/proj/src".into(),
                },
                PaneCWDRef {
                    window_session_id: "w".into(),
                    tab_id: "t".into(),
                    pane_id: "p2".into(),
                    kind: PaneKind::Terminal,
                    cwd: "/tmp/proj".into(),
                },
            ],
        };
        let specs = modals_for("/tmp/proj", "renamed", true, &snapshot);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].title, "Rename will affect 2 open terminal(s)");
        assert!(specs[0].message.contains("Renaming 'proj'"));
        assert_eq!(specs[0].confirm_label, "Rename Anyway");
        assert_eq!(specs[0].cancel_label, "Cancel");
    }

    /// Both modals for a file rename inside an affected directory: extension FIRST,
    /// then CWD (the walk runs unconditionally — for a file it usually can't match,
    /// but a pane sitting exactly at the file's path would; assert ordering with a
    /// contrived match).
    #[test]
    fn modals_for_orders_extension_before_cwd() {
        let snapshot = PaneCWDSnapshot {
            entries: vec![PaneCWDRef {
                window_session_id: "w".into(),
                tab_id: "t".into(),
                pane_id: "p1".into(),
                kind: PaneKind::Terminal,
                cwd: "/tmp/foo.txt".into(),
            }],
        };
        let specs = modals_for("/tmp/foo.txt", "foo.md", false, &snapshot);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].title, "Are you sure you want to change the extension?");
        assert!(specs[1].title.starts_with("Rename will affect"));
    }

    // MARK: - apply_rename (auto-rename bypass)

    /// A clean rename applies as a Move and is undoable.
    #[test]
    fn apply_rename_renames_and_records() {
        let t = TempTree::new();
        let src = t.make_file("foo.txt", "data");
        let dest = t.root.join("bar.txt");
        let svc = service(&t);
        let op = apply_rename(&svc, &src, &dest, origin()).expect("rename");
        assert!(!src.exists());
        assert!(dest.exists());
        svc.undo(&op).unwrap();
        assert!(src.exists());
    }

    /// A collision at apply time maps to the FROZEN string and NEVER auto-renames
    /// to `bar copy.txt` (the PROTECTED bypass).
    #[test]
    fn apply_rename_collision_is_frozen_string_not_auto_rename() {
        let t = TempTree::new();
        let src = t.make_file("foo.txt", "data");
        let _occupied = t.make_file("bar.txt", "occupied");
        let dest = t.root.join("bar.txt");
        let svc = service(&t);
        let err = apply_rename(&svc, &src, &dest, origin()).unwrap_err();
        assert_eq!(err, "Couldn't rename: 'bar.txt' already exists.");
        assert!(src.exists(), "source untouched on collision");
        assert!(
            !t.root.join("bar copy.txt").exists(),
            "rename must NOT auto-suffix (the bypass)"
        );
    }
}
