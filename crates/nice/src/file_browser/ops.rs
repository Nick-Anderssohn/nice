//! `ops` — the stateless file-operations engine behind the file-browser context
//! menu (F5). Ported from `FileOperationsService.swift` + `FileOperation.swift`.
//! Knows how to copy, move, and trash paths with Finder-style collision
//! auto-renaming (`foo copy.txt`, `foo copy 2.txt`, …), returning a
//! [`FileOperation`] record so the history layer ([`super::history`]) can undo
//! without re-reading the filesystem.
//!
//! Works over `std::fs` directly against real (temp-dir, in tests) roots — no
//! FS abstraction, the dossier §4 proven pattern. The one injected seam is
//! [`Trasher`], so tests never touch the real user Trash (they inject
//! [`FakeTrasher`] over a temp dir). The objc2 production `Trasher` lands in
//! `platform.rs` in a later slice.
//!
//! ## Frozen contracts (see the plan's PROTECTED decisions)
//!
//! * **Collision auto-rename** is the exact `next_available_name` algorithm:
//!   free name unchanged; else ` copy` (no number at index 1), ` copy 2`, …;
//!   backstop past 9999. `additional_taken` threads through a batch so two
//!   same-named sources land at distinct names.
//! * **Rename bypasses auto-rename**: it calls [`FileOperationsService::apply`]
//!   with a raw single-pair `Move` so a collision surfaces as an error, never a
//!   silent `foo copy`.
//! * **Redo of a trash rewrites the record**: [`FileOperationsService::apply`]
//!   re-recycles the originals and returns a `Trash` op carrying FRESH trash
//!   destinations (the system relocates each pass).
//! * **Undo asymmetry**: undo-copy treats a missing destination as silently
//!   satisfied; undo-move drifts `SourceMissing`; undo-trash drifts
//!   `TrashedItemMissing`.

use nice_model::file_browser::split_name_and_extension;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Errors the engine surfaces. Drift errors are reported back to the history
/// layer, which decides whether to drop the offending op and surface a banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOperationError {
    /// A source path that should still exist is missing.
    SourceMissing(PathBuf),
    /// A trashed item we wanted to restore is gone (user emptied Trash).
    TrashedItemMissing(PathBuf),
    /// Wrapped underlying error with a human-readable description.
    Underlying(String),
}

impl std::fmt::Display for FileOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileOperationError::SourceMissing(p) => {
                write!(f, "'{}' is no longer there", p.display())
            }
            FileOperationError::TrashedItemMissing(p) => {
                write!(f, "'{}' was emptied from Trash", p.display())
            }
            FileOperationError::Underlying(msg) => f.write_str(msg),
        }
    }
}

/// Identifies which window/tab originated an op so undo/redo can follow focus
/// back. `tab_id` is optional; `window_session_id` is the empty string when no
/// window (`FileOperationOrigin`, `FileOperation.swift:22-25`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOperationOrigin {
    pub window_session_id: String,
    pub tab_id: Option<String>,
}

impl FileOperationOrigin {
    pub fn new(window_session_id: impl Into<String>, tab_id: Option<String>) -> Self {
        Self {
            window_session_id: window_session_id.into(),
            tab_id,
        }
    }
}

/// One source→destination pair from a Copy or Move op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOperationItem {
    pub source: PathBuf,
    pub destination: PathBuf,
}

/// One trash record: where the file was, and where it went in Trash. On undo we
/// move `trashed` back to `original`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTrashItem {
    pub original: PathBuf,
    pub trashed: PathBuf,
}

/// Record of a completed file operation. The undo system flips one of these
/// into the inverse, applies it, and pushes the inverse onto the redo stack.
/// `origin` is preserved so undo/redo can route focus back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOperation {
    Copy {
        items: Vec<FileOperationItem>,
        origin: FileOperationOrigin,
    },
    Move {
        items: Vec<FileOperationItem>,
        origin: FileOperationOrigin,
    },
    Trash {
        items: Vec<FileTrashItem>,
        origin: FileOperationOrigin,
    },
}

impl FileOperation {
    /// The originating window/tab.
    pub fn origin(&self) -> &FileOperationOrigin {
        match self {
            FileOperation::Copy { origin, .. }
            | FileOperation::Move { origin, .. }
            | FileOperation::Trash { origin, .. } => origin,
        }
    }

    /// Human-readable label for the transient drift / status banner — FROZEN
    /// strings (`FileOperation.swift:60-66`): "Copy" / "Move" / "Move to Trash".
    pub fn label(&self) -> &'static str {
        match self {
            FileOperation::Copy { .. } => "Copy",
            FileOperation::Move { .. } => "Move",
            FileOperation::Trash { .. } => "Move to Trash",
        }
    }
}

/// Boundary trait so tests stub Trash without invoking the real AppKit
/// trash-item selector. `recycle` moves `urls` to the Trash, returning
/// `(original, trashed)` pairs in input order, throwing on the first failure
/// (earlier items stay trashed — ops are NOT transactional). The production
/// objc2 impl lands in `platform.rs` (a later slice).
pub trait Trasher {
    fn recycle(&self, urls: &[PathBuf]) -> Result<Vec<(PathBuf, PathBuf)>, FileOperationError>;
}

/// In-test / scenario Trasher that "moves" items to a subdir of a temp
/// `trash_root` rather than the user's actual Trash. Mirrors the contract:
/// returns the new paths in input order, throws on failure. Each item lands
/// under a unique subdir so two trashes of the same name don't collide.
pub struct FakeTrasher {
    trash_root: PathBuf,
}

impl FakeTrasher {
    pub fn new(trash_root: impl Into<PathBuf>) -> Self {
        Self {
            trash_root: trash_root.into(),
        }
    }
}

impl Trasher for FakeTrasher {
    fn recycle(&self, urls: &[PathBuf]) -> Result<Vec<(PathBuf, PathBuf)>, FileOperationError> {
        let mut out = Vec::with_capacity(urls.len());
        for url in urls {
            // Unique subdir so identically-named trashed items don't collide
            // inside the fake trash. `unique_token` replaces Swift's UUID (no
            // `uuid` crate dependency, per the plan's non-goals).
            let dir = self.trash_root.join(unique_token());
            std::fs::create_dir_all(&dir).map_err(underlying)?;
            let name = url.file_name().unwrap_or_default();
            let target = dir.join(name);
            std::fs::rename(url, &target).map_err(underlying)?;
            out.push((url.clone(), target));
        }
        Ok(out)
    }
}

/// The shipped [`Trasher`]: forwards to the objc2 NSFileManager recycle selector
/// in [`crate::platform`] (the only
/// module that touches the AppKit recycle selector). `app::run` injects this into
/// the process-wide history's service; `run_selftest` / tests inject a
/// [`FakeTrasher`] over a temp dir instead. Zero state.
pub struct ProductionTrasher;

impl Trasher for ProductionTrasher {
    fn recycle(&self, urls: &[PathBuf]) -> Result<Vec<(PathBuf, PathBuf)>, FileOperationError> {
        crate::platform::trash_items(urls).map_err(FileOperationError::Underlying)
    }
}

/// Resolve the destination directory a paste / drop lands in for a right-clicked
/// `target`: INTO the directory when it is one, else its PARENT (the file's
/// containing folder) — `FileExplorerOrchestrator.swift:139-153`. A raw
/// path/`is_dir` split (the caller already `lstat`ed the row), so it stays a pure
/// function.
pub fn paste_destination(target: &Path, is_dir: bool) -> PathBuf {
    if is_dir {
        target.to_path_buf()
    } else {
        target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// The stateless engine. Holds no mutable state beyond its injected [`Trasher`];
/// instances are interchangeable.
pub struct FileOperationsService {
    trasher: Box<dyn Trasher>,
}

impl FileOperationsService {
    /// Construct over an injected [`Trasher`].
    pub fn new(trasher: Box<dyn Trasher>) -> Self {
        Self { trasher }
    }

    // MARK: - Copy / Move

    /// Copy each item in `items` into `dest`, collision-resolving names. Returns
    /// a `Copy` record describing every resulting source→dest pair.
    pub fn copy(
        &self,
        items: &[PathBuf],
        dest: &Path,
        origin: FileOperationOrigin,
    ) -> Result<FileOperation, FileOperationError> {
        let pairs = self.resolve_destinations(items, dest);
        self.apply(FileOperation::Copy {
            items: pairs,
            origin,
        })
    }

    /// Move each item into `dest`, same collision policy. Returns a `Move`
    /// record.
    pub fn move_(
        &self,
        items: &[PathBuf],
        dest: &Path,
        origin: FileOperationOrigin,
    ) -> Result<FileOperation, FileOperationError> {
        let pairs = self.resolve_destinations(items, dest);
        self.apply(FileOperation::Move {
            items: pairs,
            origin,
        })
    }

    // MARK: - Trash

    /// Trash each item. Returns a `Trash` record carrying the resulting trash
    /// paths so undo can restore. Seeds a record whose `original`s are the
    /// inputs; `apply` overwrites `trashed` on the first-time recycle.
    pub fn trash(
        &self,
        items: &[PathBuf],
        origin: FileOperationOrigin,
    ) -> Result<FileOperation, FileOperationError> {
        let seed = items
            .iter()
            .map(|u| FileTrashItem {
                original: u.clone(),
                trashed: u.clone(),
            })
            .collect();
        self.apply(FileOperation::Trash {
            items: seed,
            origin,
        })
    }

    // MARK: - Apply / Undo (used by FileOperationHistory)

    /// Re-apply `op` exactly as first performed (the history's redo). Copy/move
    /// replay the recorded pairs verbatim; trash re-recycles the originals and
    /// returns a record carrying the FRESH trash paths.
    pub fn apply(&self, op: FileOperation) -> Result<FileOperation, FileOperationError> {
        match op {
            FileOperation::Copy { items, origin } => {
                for item in &items {
                    check_exists(&item.source)?;
                    copy_recursively(&item.source, &item.destination).map_err(underlying)?;
                }
                Ok(FileOperation::Copy { items, origin })
            }
            FileOperation::Move { items, origin } => {
                for item in &items {
                    check_exists(&item.source)?;
                    // `FileManager.moveItem` throws on an existing destination —
                    // it never overwrites. POSIX `rename(2)` (and Rust's
                    // `std::fs::rename`) WOULD clobber a colliding file, so guard
                    // it explicitly. Batch copy/move never trips this (names are
                    // collision-resolved first); the rename bypass relies on it
                    // to surface a collision as an error rather than a silent
                    // `foo copy` (the FROZEN rename contract). `AlreadyExists`
                    // kind so the rename slice can map it to the frozen string
                    // (Swift matched `NSCocoaErrorDomain` the same way).
                    if item.destination.exists() {
                        return Err(underlying(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            format!("'{}' already exists", item.destination.display()),
                        )));
                    }
                    std::fs::rename(&item.source, &item.destination).map_err(underlying)?;
                }
                Ok(FileOperation::Move { items, origin })
            }
            FileOperation::Trash { items, origin } => {
                let originals: Vec<PathBuf> = items.iter().map(|i| i.original.clone()).collect();
                for url in &originals {
                    check_exists(url)?;
                }
                let recycled = self.trasher.recycle(&originals)?;
                let new_items = recycled
                    .into_iter()
                    .map(|(original, trashed)| FileTrashItem { original, trashed })
                    .collect();
                Ok(FileOperation::Trash {
                    items: new_items,
                    origin,
                })
            }
        }
    }

    /// Undo `op`. State is moved back to "before `op` was applied". Throws on
    /// drift; the history layer catches and reports it.
    pub fn undo(&self, op: &FileOperation) -> Result<(), FileOperationError> {
        match op {
            FileOperation::Copy { items, .. } => {
                // Inverse of a copy is to delete each destination. If it's
                // already gone (user deleted it in Finder) the undo is silently
                // satisfied — the world already matches the desired post-undo
                // state for that item.
                for item in items {
                    if item.destination.exists() {
                        remove_path(&item.destination).map_err(underlying)?;
                    }
                }
                Ok(())
            }
            FileOperation::Move { items, .. } => {
                // Inverse of a move is dest → source. Drift on the destination
                // (file gone) is reported so the user knows the undo couldn't
                // complete.
                for item in items {
                    check_exists(&item.destination)?;
                    std::fs::rename(&item.destination, &item.source).map_err(underlying)?;
                }
                Ok(())
            }
            FileOperation::Trash { items, .. } => {
                for item in items {
                    if !item.trashed.exists() {
                        return Err(FileOperationError::TrashedItemMissing(item.trashed.clone()));
                    }
                    std::fs::rename(&item.trashed, &item.original).map_err(underlying)?;
                }
                Ok(())
            }
        }
    }

    // MARK: - Collision naming

    /// Build the destination paths for `items` inside `dest`, collision-resolved
    /// against both existing entries and earlier pairs in the same batch (so two
    /// same-named sources land at distinct names).
    fn resolve_destinations(&self, items: &[PathBuf], dest: &Path) -> Vec<FileOperationItem> {
        let mut taken: HashSet<String> = HashSet::new();
        let mut out = Vec::with_capacity(items.len());
        for src in items {
            let resolved = next_available_name(src, dest, &taken);
            if let Some(name) = resolved.file_name().and_then(|n| n.to_str()) {
                taken.insert(name.to_string());
            }
            out.push(FileOperationItem {
                source: src.clone(),
                destination: resolved,
            });
        }
        out
    }
}

/// Return a destination path for copying/moving `src` into `dest`. If
/// `dest/<name>` is free (on disk AND not in `additional_taken`), return it
/// unchanged; else ` copy`, ` copy 2`, … until free; backstop past 9999. The
/// FROZEN algorithm (`FileOperationsService.swift:267-299`). Public so the
/// scenario / menu slice can preview a destination name.
pub fn next_available_name(
    src: &Path,
    dest: &Path,
    additional_taken: &HashSet<String>,
) -> PathBuf {
    let original_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let candidate = dest.join(&original_name);
    if !candidate.exists() && !additional_taken.contains(&original_name) {
        return candidate;
    }

    let (base, ext) = split_name_and_extension(&original_name);
    let mut index = 1u32;
    loop {
        let suffix = if index == 1 {
            " copy".to_string()
        } else {
            format!(" copy {index}")
        };
        let name = if ext.is_empty() {
            format!("{base}{suffix}")
        } else {
            format!("{base}{suffix}.{ext}")
        };
        let url = dest.join(&name);
        if !url.exists() && !additional_taken.contains(&name) {
            return url;
        }
        index += 1;
        if index > 9999 {
            // Defensive backstop; real filesystems never reach here. A unique
            // token keeps callers from looping (Swift uses a UUID; we avoid the
            // `uuid` crate dependency).
            let token = unique_token();
            let name = if ext.is_empty() {
                format!("{base} copy {token}")
            } else {
                format!("{base} copy {token}.{ext}")
            };
            return dest.join(name);
        }
    }
}

/// Recursively copy `src` to `dst` (file or directory tree), mirroring
/// `FileManager.copyItem`. `dst` must not already exist (callers collision-
/// resolve first).
fn copy_recursively(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursively(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst)?;
        Ok(())
    }
}

/// Remove a path whether it's a file or a directory tree.
fn remove_path(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// `.sourceMissing` guard.
fn check_exists(path: &Path) -> Result<(), FileOperationError> {
    if path.exists() {
        Ok(())
    } else {
        Err(FileOperationError::SourceMissing(path.to_path_buf()))
    }
}

/// Wrap an `io::Error` as `.underlying(message)`.
fn underlying(err: std::io::Error) -> FileOperationError {
    FileOperationError::Underlying(err.to_string())
}

/// A process-unique token (nanos + a monotonic counter) — the no-`uuid`
/// replacement for the UUID Swift uses in the collision backstop and the fake
/// trash. Uniqueness, not unpredictability, is all that's required.
fn unique_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A per-test temp-dir tree. Dropped (recursively removed) on scope exit so
    /// the suite stays hermetic — the `nice-fileop-test-*` isolation the Swift
    /// suite gets from its `setUp`/`tearDown`.
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nice-fileop-test-{}", unique_token()));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn make_file(&self, name: &str, body: &str) -> PathBuf {
            let url = self.root.join(name);
            if let Some(parent) = url.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&url, body).unwrap();
            url
        }

        fn make_dir(&self, name: &str) -> PathBuf {
            let url = self.root.join(name);
            fs::create_dir_all(&url).unwrap();
            url
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

    fn service_with_trash(trash_root: &Path) -> FileOperationsService {
        FileOperationsService::new(Box::new(FakeTrasher::new(trash_root)))
    }

    /// A service whose Trasher is never exercised (copy/move-only tests).
    fn service(t: &TempTree) -> FileOperationsService {
        service_with_trash(&t.make_dir("Trash"))
    }

    // MARK: - Copy

    /// `FileOperationsServiceTests.test_copy_intoEmptyDir_writesAllFiles`
    #[test]
    fn copy_into_empty_dir_writes_all_files() {
        let t = TempTree::new();
        let src = t.make_file("a.txt", "alpha");
        let src2 = t.make_file("b.txt", "beta");
        let dest = t.make_dir("dest");
        let op = service(&t)
            .copy(&[src.clone(), src2.clone()], &dest, origin())
            .unwrap();

        assert!(dest.join("a.txt").exists());
        assert!(dest.join("b.txt").exists());
        assert!(src.exists());
        assert!(src2.exists());
        match op {
            FileOperation::Copy { items, .. } => assert_eq!(items.len(), 2),
            _ => panic!("expected Copy"),
        }
    }

    /// `FileOperationsServiceTests.test_copy_recursivelyCopiesDirectory`
    #[test]
    fn copy_recursively_copies_directory() {
        let t = TempTree::new();
        let folder = t.make_dir("folder");
        fs::write(folder.join("inside.txt"), "hi").unwrap();
        let dest = t.make_dir("dest");
        service(&t).copy(&[folder], &dest, origin()).unwrap();
        assert!(dest.join("folder").join("inside.txt").exists());
    }

    /// `FileOperationsServiceTests.test_copy_collidingName_appendsCopySuffix`
    #[test]
    fn copy_colliding_name_appends_copy_suffix() {
        let t = TempTree::new();
        let src = t.make_file("foo.txt", "x");
        let dest = t.make_dir("dest");
        fs::write(dest.join("foo.txt"), "existing").unwrap();
        let op = service(&t).copy(&[src], &dest, origin()).unwrap();
        assert!(dest.join("foo copy.txt").exists());
        match op {
            FileOperation::Copy { items, .. } => {
                assert_eq!(items[0].destination.file_name().unwrap(), "foo copy.txt")
            }
            _ => panic!("expected Copy"),
        }
    }

    /// `FileOperationsServiceTests.test_copy_collidingNameTwice_appendsCopy2`
    #[test]
    fn copy_colliding_name_twice_appends_copy_2() {
        let t = TempTree::new();
        let src = t.make_file("foo.txt", "x");
        let dest = t.make_dir("dest");
        fs::write(dest.join("foo.txt"), "").unwrap();
        fs::write(dest.join("foo copy.txt"), "").unwrap();
        service(&t).copy(&[src], &dest, origin()).unwrap();
        assert!(dest.join("foo copy 2.txt").exists());
    }

    /// `FileOperationsServiceTests.test_copy_collidingDirectory_appendsCopy`
    #[test]
    fn copy_colliding_directory_appends_copy() {
        let t = TempTree::new();
        let folder = t.make_dir("folder");
        let dest = t.make_dir("dest");
        fs::create_dir(dest.join("folder")).unwrap();
        service(&t).copy(&[folder], &dest, origin()).unwrap();
        assert!(dest.join("folder copy").is_dir());
    }

    /// `FileOperationsServiceTests.test_copy_recordIncludesAllSourceDestPairs`
    #[test]
    fn copy_record_includes_all_source_dest_pairs() {
        let t = TempTree::new();
        let a = t.make_file("a.txt", "");
        let b = t.make_file("b.txt", "");
        let dest = t.make_dir("dest");
        let op = service(&t)
            .copy(&[a.clone(), b.clone()], &dest, origin())
            .unwrap();
        match op {
            FileOperation::Copy { items, .. } => {
                assert_eq!(items.iter().map(|i| &i.source).collect::<Vec<_>>(), vec![&a, &b]);
                assert_eq!(
                    items
                        .iter()
                        .map(|i| i.destination.file_name().unwrap().to_str().unwrap())
                        .collect::<Vec<_>>(),
                    vec!["a.txt", "b.txt"]
                );
            }
            _ => panic!("expected Copy"),
        }
    }

    // MARK: - Move

    /// `FileOperationsServiceTests.test_move_intoEmptyDir_relocatesFiles`
    #[test]
    fn move_into_empty_dir_relocates_files() {
        let t = TempTree::new();
        let src = t.make_file("file.txt", "");
        let dest = t.make_dir("dest");
        service(&t).move_(&[src.clone()], &dest, origin()).unwrap();
        assert!(!src.exists());
        assert!(dest.join("file.txt").exists());
    }

    /// `FileOperationsServiceTests.test_move_collidingName_appendsCopySuffix`
    #[test]
    fn move_colliding_name_appends_copy_suffix() {
        let t = TempTree::new();
        let src = t.make_file("file.txt", "");
        let dest = t.make_dir("dest");
        fs::write(dest.join("file.txt"), "existing").unwrap();
        let op = service(&t).move_(&[src], &dest, origin()).unwrap();
        assert!(dest.join("file copy.txt").exists());
        match op {
            FileOperation::Move { items, .. } => {
                assert_eq!(items[0].destination.file_name().unwrap(), "file copy.txt")
            }
            _ => panic!("expected Move"),
        }
    }

    // MARK: - Trash

    /// `FileOperationsServiceTests.test_trash_movesItemToTrash_capturesNewURL`
    #[test]
    fn trash_moves_item_to_trash_captures_new_url() {
        let t = TempTree::new();
        let src = t.make_file("delete-me.txt", "");
        let trash = t.make_dir("Trash");
        let op = service_with_trash(&trash)
            .trash(&[src.clone()], origin())
            .unwrap();
        match op {
            FileOperation::Trash { items, .. } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].original, src);
                assert!(items[0].trashed.exists());
            }
            _ => panic!("expected Trash"),
        }
    }

    /// `FileOperationsServiceTests.test_trash_multipleItems_returnsRecordWithAllPairs`
    #[test]
    fn trash_multiple_items_returns_record_with_all_pairs() {
        let t = TempTree::new();
        let a = t.make_file("a.txt", "");
        let b = t.make_file("b.txt", "");
        let trash = t.make_dir("Trash");
        let op = service_with_trash(&trash)
            .trash(&[a.clone(), b.clone()], origin())
            .unwrap();
        match op {
            FileOperation::Trash { items, .. } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items.iter().map(|i| &i.original).collect::<Vec<_>>(), vec![&a, &b]);
            }
            _ => panic!("expected Trash"),
        }
    }

    // MARK: - Inverse

    /// `FileOperationsServiceTests.test_apply_inverseOfCopy_deletesAllCopiedDests`
    #[test]
    fn inverse_of_copy_deletes_all_copied_dests() {
        let t = TempTree::new();
        let src = t.make_file("a.txt", "");
        let dest = t.make_dir("dest");
        let svc = service(&t);
        let op = svc.copy(&[src.clone()], &dest, origin()).unwrap();
        svc.undo(&op).unwrap();
        assert!(!dest.join("a.txt").exists());
        assert!(src.exists());
    }

    /// `FileOperationsServiceTests.test_apply_inverseOfMove_movesItemsBackToOrigin`
    #[test]
    fn inverse_of_move_moves_items_back_to_origin() {
        let t = TempTree::new();
        let src = t.make_file("a.txt", "hi");
        let dest = t.make_dir("dest");
        let svc = service(&t);
        let op = svc.move_(&[src.clone()], &dest, origin()).unwrap();
        assert!(!src.exists());
        svc.undo(&op).unwrap();
        assert!(src.exists());
        assert!(!dest.join("a.txt").exists());
    }

    /// `FileOperationsServiceTests.test_apply_inverseOfTrash_restoresFromTrashURL`
    #[test]
    fn inverse_of_trash_restores_from_trash_url() {
        let t = TempTree::new();
        let src = t.make_file("a.txt", "hi");
        let trash = t.make_dir("Trash");
        let svc = service_with_trash(&trash);
        let op = svc.trash(&[src.clone()], origin()).unwrap();
        assert!(!src.exists());
        svc.undo(&op).unwrap();
        assert!(src.exists());
    }

    /// `FileOperationsServiceTests.test_apply_inverseOfTrash_missingTrashURL_throwsDriftError`
    #[test]
    fn inverse_of_trash_missing_trash_url_throws_drift() {
        let t = TempTree::new();
        let src = t.make_file("a.txt", "");
        let trash = t.make_dir("Trash");
        let svc = service_with_trash(&trash);
        let op = svc.trash(&[src], origin()).unwrap();
        if let FileOperation::Trash { items, .. } = &op {
            fs::remove_file(&items[0].trashed).unwrap();
        }
        match svc.undo(&op) {
            Err(FileOperationError::TrashedItemMissing(_)) => {}
            other => panic!("expected TrashedItemMissing, got {other:?}"),
        }
    }

    // MARK: - Collision naming

    /// `FileOperationsServiceTests.test_nextAvailableName_skipsExistingNumberedSiblings`
    #[test]
    fn next_available_name_skips_existing_numbered_siblings() {
        let t = TempTree::new();
        let dest = t.make_dir("dest");
        let src = t.root.join("foo.txt");
        fs::write(dest.join("foo.txt"), "").unwrap();
        fs::write(dest.join("foo copy.txt"), "").unwrap();
        fs::write(dest.join("foo copy 2.txt"), "").unwrap();
        let resolved = next_available_name(&src, &dest, &HashSet::new());
        assert_eq!(resolved.file_name().unwrap(), "foo copy 3.txt");
    }

    /// `FileOperationsServiceTests.test_nextAvailableName_preservesExtension`
    #[test]
    fn next_available_name_preserves_extension() {
        let t = TempTree::new();
        let dest = t.make_dir("dest");
        let src = t.root.join("archive.tar.gz");
        fs::write(dest.join("archive.tar.gz"), "").unwrap();
        let resolved = next_available_name(&src, &dest, &HashSet::new());
        assert_eq!(resolved.file_name().unwrap(), "archive.tar copy.gz");
    }

    /// `FileOperationsServiceTests.test_nextAvailableName_directoryHasNoExtension`
    #[test]
    fn next_available_name_directory_has_no_extension() {
        let t = TempTree::new();
        let dest = t.make_dir("dest");
        let src = t.root.join("folder");
        fs::create_dir(dest.join("folder")).unwrap();
        let resolved = next_available_name(&src, &dest, &HashSet::new());
        assert_eq!(resolved.file_name().unwrap(), "folder copy");
    }

    // MARK: - Multi-source / batch

    /// `FileOperationsServiceTests.test_copy_twoSourcesWithSameName_distinctDestinations`
    #[test]
    fn copy_two_sources_with_same_name_distinct_destinations() {
        let t = TempTree::new();
        let a = t.make_file("a/foo.txt", "aa");
        let b = t.make_file("b/foo.txt", "bb");
        let dest = t.make_dir("dest");
        let op = service(&t).copy(&[a, b], &dest, origin()).unwrap();
        match op {
            FileOperation::Copy { items, .. } => {
                let names: HashSet<String> = items
                    .iter()
                    .map(|i| i.destination.file_name().unwrap().to_str().unwrap().to_string())
                    .collect();
                assert_eq!(
                    names,
                    ["foo.txt".to_string(), "foo copy.txt".to_string()]
                        .into_iter()
                        .collect()
                );
            }
            _ => panic!("expected Copy"),
        }
    }

    /// `FileOperationsServiceTests.test_copy_partialFailureMidBatch_leavesEarlierCopiesInPlace_throws`
    #[test]
    fn copy_partial_failure_mid_batch_leaves_earlier_copies_throws() {
        let t = TempTree::new();
        let a = t.make_file("a.txt", "1");
        let missing = t.root.join("ghost.txt");
        let dest = t.make_dir("dest");
        match service(&t).copy(&[a, missing], &dest, origin()) {
            Err(FileOperationError::SourceMissing(_)) => {}
            other => panic!("expected SourceMissing, got {other:?}"),
        }
        assert!(
            dest.join("a.txt").exists(),
            "earlier successful copies remain after a mid-batch drift failure"
        );
    }

    // MARK: - Unicode + spaces

    /// `FileOperationsServiceTests.test_copy_unicodeName_preservedThroughCollisionRename`
    #[test]
    fn copy_unicode_name_preserved_through_collision_rename() {
        let t = TempTree::new();
        let src = t.make_file("café 文件.txt", "data");
        let dest = t.make_dir("dest");
        fs::write(dest.join("café 文件.txt"), "existing").unwrap();
        service(&t).copy(&[src], &dest, origin()).unwrap();
        assert!(dest.join("café 文件 copy.txt").exists());
    }

    /// `FileOperationsServiceTests.test_copy_pathWithSpaces_roundtrips`
    #[test]
    fn copy_path_with_spaces_roundtrips() {
        let t = TempTree::new();
        let src = t.make_file("a folder/with spaces.txt", "data");
        let dest = t.make_dir("dest");
        service(&t).copy(&[src], &dest, origin()).unwrap();
        assert!(dest.join("with spaces.txt").exists());
    }

    // MARK: - Rename via apply(Move)

    /// `FileOperationsServiceTests.test_apply_moveAsRename_inSameParent_renamesFile`
    #[test]
    fn apply_move_as_rename_in_same_parent_renames_file() {
        let t = TempTree::new();
        let src = t.make_file("foo.txt", "data");
        let dest = src.parent().unwrap().join("bar.txt");
        let svc = service(&t);
        let op = svc
            .apply(FileOperation::Move {
                items: vec![FileOperationItem {
                    source: src.clone(),
                    destination: dest.clone(),
                }],
                origin: origin(),
            })
            .unwrap();
        assert!(!src.exists());
        assert!(dest.exists());
        svc.undo(&op).unwrap();
        assert!(src.exists());
        assert!(!dest.exists());
    }

    /// `FileOperationsServiceTests.test_apply_moveAsRename_destinationExists_throws` —
    /// rename must NOT auto-suffix; the collision surfaces as an error and the
    /// source is untouched.
    #[test]
    fn apply_move_as_rename_destination_exists_throws() {
        let t = TempTree::new();
        let src = t.make_file("foo.txt", "data");
        let collision = t.make_file("bar.txt", "occupied");
        let svc = service(&t);
        let result = svc.apply(FileOperation::Move {
            items: vec![FileOperationItem {
                source: src.clone(),
                destination: collision.clone(),
            }],
            origin: origin(),
        });
        assert!(result.is_err());
        assert!(src.exists(), "source must remain when collision blocks rename");
        assert_eq!(
            fs::read_to_string(&collision).unwrap(),
            "occupied",
            "destination contents untouched"
        );
    }

    /// `FileOperationsServiceTests.test_apply_moveAsRename_renamesDirectoryWithContents`
    #[test]
    fn apply_move_as_rename_renames_directory_with_contents() {
        let t = TempTree::new();
        let folder = t.make_dir("oldname");
        fs::write(folder.join("inside.txt"), "hi").unwrap();
        let renamed = folder.parent().unwrap().join("newname");
        service(&t)
            .apply(FileOperation::Move {
                items: vec![FileOperationItem {
                    source: folder.clone(),
                    destination: renamed.clone(),
                }],
                origin: origin(),
            })
            .unwrap();
        assert!(!folder.exists());
        assert!(renamed.exists());
        assert!(renamed.join("inside.txt").exists());
    }

    // MARK: - Labels (frozen)

    #[test]
    fn labels_are_frozen() {
        let o = origin();
        assert_eq!(
            (FileOperation::Copy { items: vec![], origin: o.clone() }).label(),
            "Copy"
        );
        assert_eq!(
            (FileOperation::Move { items: vec![], origin: o.clone() }).label(),
            "Move"
        );
        assert_eq!(
            (FileOperation::Trash { items: vec![], origin: o }).label(),
            "Move to Trash"
        );
    }

    // MARK: - Paste destination resolution

    /// `FileExplorerOrchestrator.swift:139-153` — a paste onto a directory lands
    /// INSIDE it; onto a file, in its parent.
    #[test]
    fn paste_destination_directory_goes_inside_file_goes_to_parent() {
        assert_eq!(
            paste_destination(Path::new("/a/b/dir"), true),
            PathBuf::from("/a/b/dir")
        );
        assert_eq!(
            paste_destination(Path::new("/a/b/file.txt"), false),
            PathBuf::from("/a/b")
        );
    }

    // MARK: - Redo trash rewrite (spec provenance: redo-trash record rewrite)

    /// The redo of a trash re-recycles the originals and returns FRESH trash
    /// paths (`FileOperationsService.swift:154-163`). Pin: apply(Trash) after a
    /// restore produces a different `trashed` path than the first pass.
    #[test]
    fn apply_trash_rewrites_record_with_fresh_urls() {
        let t = TempTree::new();
        let src = t.make_file("a.txt", "hi");
        let trash = t.make_dir("Trash");
        let svc = service_with_trash(&trash);
        let first = svc.trash(&[src.clone()], origin()).unwrap();
        let first_trashed = match &first {
            FileOperation::Trash { items, .. } => items[0].trashed.clone(),
            _ => panic!("expected Trash"),
        };
        // Undo restores the original back to `src`.
        svc.undo(&first).unwrap();
        assert!(src.exists());
        // Redo = apply(op): re-recycles and returns a fresh trash location.
        let redone = svc.apply(first).unwrap();
        match redone {
            FileOperation::Trash { items, .. } => {
                assert_eq!(items[0].original, src);
                assert_ne!(
                    items[0].trashed, first_trashed,
                    "redo of a trash must carry a fresh trash location"
                );
                assert!(items[0].trashed.exists());
            }
            _ => panic!("expected Trash"),
        }
    }
}
