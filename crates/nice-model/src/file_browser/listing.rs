//! Pure directory-listing logic for the file browser — read a directory's
//! children, filter hidden entries by `show_hidden`, and sort dirs-first then
//! by the user's chosen criterion + direction. Ported from
//! `Sources/Nice/State/FileBrowserListing.swift`.
//!
//! Side-effect-free apart from the `read_dir` it performs: it reads the
//! filesystem and returns. Errors and missing paths return an empty vec rather
//! than an error — the browser's empty-state UI handles a missing root
//! independently, and a deeper row that vanishes mid-render must not take the
//! whole tree down (`FileBrowserListing.swift:55-58`).
//!
//! ## Documented divergences from the Swift original
//!
//! * **Case-insensitive name compare uses Unicode lowercase folding**
//!   (`str::to_lowercase`), not `localizedCaseInsensitiveCompare`'s locale
//!   collation. A deliberate, test-pinned parity gap (the plan's listing-rules
//!   decision) — pure and locale-independent.
//! * **`lstat` semantics throughout.** [`entries`] classifies each child by its
//!   own file type without following symlinks (`DirEntry::file_type`), so a
//!   symlink-to-directory renders as a non-expandable **file** row (NSURL
//!   parity; forecloses watcher cycles). [`visible_order`] recurses only into
//!   real directories for the same reason.
//! * **Missing modification dates cluster oldest** at [`std::time::UNIX_EPOCH`]
//!   (the Swift `.distantPast` default) so they sort deterministically rather
//!   than scattering by filesystem order.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::file_browser::sort::FileBrowserSortCriterion;

/// BSD `UF_HIDDEN` file flag (`sys/stat.h`) — set by `chflags hidden` and by
/// Finder's "invisible" flag. Checked against `st_flags` so Finder-hidden
/// entries filter even without a dot prefix.
const UF_HIDDEN: u32 = 0x0000_8000;

/// One directory child with the metadata the comparator + hidden filter need,
/// gathered once per `read_dir` so sorting doesn't re-stat.
struct Child {
    path: PathBuf,
    name: String,
    name_lower: String,
    is_dir: bool,
    is_hidden_flagged: bool,
    mtime: SystemTime,
}

/// Read `path`'s children, optionally filtering hidden entries, and return them
/// sorted dirs-first then by `criterion` / `ascending`.
///
/// * **Filtering** (when `show_hidden == false`): drops entries whose name
///   starts with `.` OR whose BSD `UF_HIDDEN` flag is set — the dual check
///   covers dotfiles by name and `chflags hidden` invisibles.
/// * **Sort**: directories before files (always, regardless of `criterion`);
///   within each bucket the chosen criterion decides order. Date sorts
///   tie-break by name (always A→Z) so timestamps that collide (common after a
///   `git checkout`) order stably and don't flip when the user toggles
///   direction.
/// * **Errors / missing paths** return `[]` rather than erroring
///   (`FileBrowserListing.swift:59-97`).
pub fn entries(
    path: &Path,
    show_hidden: bool,
    criterion: FileBrowserSortCriterion,
    ascending: bool,
) -> Vec<PathBuf> {
    let read = match std::fs::read_dir(path) {
        Ok(read) => read,
        Err(_) => return Vec::new(),
    };

    let mut children: Vec<Child> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `DirEntry::metadata` does not traverse symlinks — lstat semantics, so
        // a symlink-to-dir classifies as a file and we read the link's own
        // flags/mtime, not the target's.
        let meta = entry.metadata().ok();
        let is_dir = entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        let is_hidden_flagged = meta
            .as_ref()
            .map(metadata_is_hidden_flagged)
            .unwrap_or(false);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .unwrap_or(UNIX_EPOCH);
        children.push(Child {
            path: entry.path(),
            name_lower: name.to_lowercase(),
            name,
            is_dir,
            is_hidden_flagged,
            mtime,
        });
    }

    if !show_hidden {
        children.retain(|c| !c.name.starts_with('.') && !c.is_hidden_flagged);
    }

    // Stable sort: name-criterion ties (equal lowercased) keep read_dir order.
    children.sort_by(|a, b| compare(a, b, criterion, ascending));
    children.into_iter().map(|c| c.path).collect()
}

/// In-order traversal of the visible rows in the tree rooted at `root_path`,
/// using the same listing rules [`entries`] produces. A directory's children
/// are emitted only when its path is in `expanded_paths` — this matches what
/// the user actually sees on screen, so it is the right ordering for
/// ⇧-range selection (`FileBrowserListing.swift:99-123`).
pub fn visible_order(
    root_path: &str,
    expanded_paths: &BTreeSet<String>,
    show_hidden: bool,
    criterion: FileBrowserSortCriterion,
    ascending: bool,
) -> Vec<String> {
    // `Path::exists` follows symlinks, matching the Swift `fileExists` guard.
    if !Path::new(root_path).exists() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    visit(
        root_path,
        &mut out,
        expanded_paths,
        show_hidden,
        criterion,
        ascending,
    );
    out
}

fn visit(
    path: &str,
    out: &mut Vec<String>,
    expanded_paths: &BTreeSet<String>,
    show_hidden: bool,
    criterion: FileBrowserSortCriterion,
    ascending: bool,
) {
    out.push(path.to_string());
    if !expanded_paths.contains(path) {
        return;
    }
    // lstat: a symlink-to-dir is not a real directory, so we don't recurse into
    // it — parity with `entries` bucketing and the watcher-cycle foreclosure.
    if !path_is_dir_lstat(path) {
        return;
    }
    for child in entries(Path::new(path), show_hidden, criterion, ascending) {
        visit(
            &child.to_string_lossy(),
            out,
            expanded_paths,
            show_hidden,
            criterion,
            ascending,
        );
    }
}

/// Within-bucket + dirs-first comparator. Dirs sort before files regardless of
/// criterion; within a bucket the criterion decides
/// (`FileBrowserListing.swift:158-183`).
fn compare(
    a: &Child,
    b: &Child,
    criterion: FileBrowserSortCriterion,
    ascending: bool,
) -> Ordering {
    match (a.is_dir, b.is_dir) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    match criterion {
        FileBrowserSortCriterion::Name => {
            let ord = a.name_lower.cmp(&b.name_lower);
            if ascending {
                ord
            } else {
                ord.reverse()
            }
        }
        FileBrowserSortCriterion::DateModified => {
            if a.mtime != b.mtime {
                let ord = a.mtime.cmp(&b.mtime);
                if ascending {
                    ord
                } else {
                    ord.reverse()
                }
            } else {
                // Stable tie-break by name, always A→Z, so two same-mtime
                // neighbors don't swap when the user toggles direction.
                a.name_lower.cmp(&b.name_lower)
            }
        }
    }
}

fn path_is_dir_lstat(path: &str) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn metadata_is_hidden_flagged(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    meta.st_flags() & UF_HIDDEN != 0
}

#[cfg(not(target_os = "macos"))]
fn metadata_is_hidden_flagged(_meta: &std::fs::Metadata) -> bool {
    // The `UF_HIDDEN` BSD flag is macOS-only; elsewhere only the dot-prefix
    // prong of the hidden filter applies. (Nice ships macOS-only; this keeps
    // the crate portable for tooling.)
    let _ = UF_HIDDEN;
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::process::Command;

    const HIDDEN_ALL: bool = true;

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "nice-listing-test-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            TempTree { root }
        }

        fn touch(&self, name: &str) -> PathBuf {
            let p = self.root.join(name);
            fs::write(&p, b"").unwrap();
            p
        }

        fn mkdir(&self, name: &str) -> PathBuf {
            let p = self.root.join(name);
            fs::create_dir(&p).unwrap();
            p
        }

        fn names(
            &self,
            show_hidden: bool,
            criterion: FileBrowserSortCriterion,
            ascending: bool,
        ) -> Vec<String> {
            entries(&self.root, show_hidden, criterion, ascending)
                .into_iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Force a known mtime so date-sort tests don't race the wall clock. Uses
    /// `touch -t` (epoch via a formatted stamp) — pure test scaffolding on a
    /// real temp file.
    fn set_mtime_epoch(path: &Path, secs_since_epoch: u64) {
        // `touch -d @<epoch>` is GNU-only; on macOS use `-t [[CC]YY]MMDDhhmm.SS`
        // in UTC. Convert the epoch to a UTC stamp via `date -u`.
        let stamp = Command::new("date")
            .args([
                "-u",
                "-r",
                &secs_since_epoch.to_string(),
                "+%Y%m%d%H%M.%S",
            ])
            .output()
            .expect("date");
        let stamp = String::from_utf8(stamp.stdout).unwrap().trim().to_string();
        let status = Command::new("touch")
            .args(["-t", &stamp])
            .arg(path)
            .status()
            .expect("touch");
        assert!(status.success(), "touch -t failed for {path:?}");
    }

    // MARK: - Sort order

    /// `FileBrowserListingTests.test_entries_sortsDirsFirstThenAlphaCaseInsensitive`
    #[test]
    fn entries_sorts_dirs_first_then_alpha_case_insensitive() {
        let t = TempTree::new();
        t.touch("regular.txt");
        t.touch("a_file.swift");
        t.mkdir("Z_dir");
        t.mkdir("M_dir");

        let names = t.names(HIDDEN_ALL, FileBrowserSortCriterion::Name, true);
        assert_eq!(names, ["M_dir", "Z_dir", "a_file.swift", "regular.txt"]);
    }

    /// `FileBrowserListingTests.test_entries_nameDescending_reversesEachBucket`
    #[test]
    fn entries_name_descending_reverses_each_bucket() {
        let t = TempTree::new();
        t.touch("regular.txt");
        t.touch("a_file.swift");
        t.mkdir("Z_dir");
        t.mkdir("M_dir");

        let names = t.names(HIDDEN_ALL, FileBrowserSortCriterion::Name, false);
        assert_eq!(names, ["Z_dir", "M_dir", "regular.txt", "a_file.swift"]);
    }

    /// `FileBrowserListingTests.test_entries_dateModifiedAscending_oldestFirstWithinBucket`
    #[test]
    fn entries_date_modified_ascending_oldest_first() {
        let t = TempTree::new();
        let old = t.touch("z_old.txt");
        let new = t.touch("a_new.txt");
        set_mtime_epoch(&old, 1_000_000);
        set_mtime_epoch(&new, 2_000_000);

        let names = t.names(HIDDEN_ALL, FileBrowserSortCriterion::DateModified, true);
        assert_eq!(
            names,
            ["z_old.txt", "a_new.txt"],
            "date-asc must put older mtime first even when alpha-order disagrees"
        );
    }

    /// `FileBrowserListingTests.test_entries_dateModifiedDescending_newestFirstWithinBucket`
    #[test]
    fn entries_date_modified_descending_newest_first() {
        let t = TempTree::new();
        let old = t.touch("a_old.txt");
        let new = t.touch("z_new.txt");
        set_mtime_epoch(&old, 1_000_000);
        set_mtime_epoch(&new, 2_000_000);

        let names = t.names(HIDDEN_ALL, FileBrowserSortCriterion::DateModified, false);
        assert_eq!(names, ["z_new.txt", "a_old.txt"]);
    }

    /// `FileBrowserListingTests.test_entries_dateModifiedTiebreak_byNameAscendingEvenWhenDescending`
    #[test]
    fn entries_date_modified_tiebreak_by_name_ascending_even_when_descending() {
        let t = TempTree::new();
        let a = t.touch("a.txt");
        let b = t.touch("b.txt");
        set_mtime_epoch(&a, 1_500_000);
        set_mtime_epoch(&b, 1_500_000);

        let asc = t.names(HIDDEN_ALL, FileBrowserSortCriterion::DateModified, true);
        let desc = t.names(HIDDEN_ALL, FileBrowserSortCriterion::DateModified, false);
        assert_eq!(asc, ["a.txt", "b.txt"]);
        assert_eq!(
            desc,
            ["a.txt", "b.txt"],
            "same-mtime entries keep stable alpha order under both directions"
        );
    }

    /// `FileBrowserListingTests.test_entries_dirsAlwaysAboveFiles_evenUnderDateModified`
    #[test]
    fn entries_dirs_always_above_files_even_under_date_modified() {
        let t = TempTree::new();
        let dir = t.mkdir("oldDir");
        let file = t.touch("newFile.txt");
        set_mtime_epoch(&dir, 1_000_000);
        set_mtime_epoch(&file, 2_000_000);

        let names = t.names(HIDDEN_ALL, FileBrowserSortCriterion::DateModified, false);
        assert_eq!(
            names.first().map(String::as_str),
            Some("oldDir"),
            "dirs-first must hold even when a file's mtime is newer than the dir's"
        );
    }

    // MARK: - Hidden filter

    /// `FileBrowserListingTests.test_entries_showHiddenFalse_filtersDotPrefixedNames`
    #[test]
    fn entries_show_hidden_false_filters_dot_prefixed() {
        let t = TempTree::new();
        t.touch(".hidden.txt");
        t.touch("visible.txt");
        t.mkdir(".git");
        t.mkdir("Sources");

        let names = t.names(false, FileBrowserSortCriterion::Name, true);
        assert_eq!(names, ["Sources", "visible.txt"]);
    }

    /// `FileBrowserListingTests.test_entries_showHiddenTrue_includesEverything`
    #[test]
    fn entries_show_hidden_true_includes_everything() {
        let t = TempTree::new();
        t.touch(".hidden.txt");
        t.touch("visible.txt");
        t.mkdir(".git");
        t.mkdir("Sources");

        let names = t.names(true, FileBrowserSortCriterion::Name, true);
        assert_eq!(names, [".git", "Sources", ".hidden.txt", "visible.txt"]);
    }

    /// `FileBrowserListingTests.test_entries_showHiddenFalse_filtersIsHiddenFlaggedFiles`
    /// — the BSD `UF_HIDDEN` prong (a plain-named file `chflags hidden`'d).
    #[test]
    fn entries_show_hidden_false_filters_uf_hidden_flagged() {
        let t = TempTree::new();
        let invisible = t.touch("plainName.txt");
        let status = Command::new("chflags")
            .args(["hidden"])
            .arg(&invisible)
            .status()
            .expect("chflags");
        assert!(status.success(), "chflags hidden failed");
        t.touch("regular.txt");

        let names = t.names(false, FileBrowserSortCriterion::Name, true);
        assert_eq!(
            names,
            ["regular.txt"],
            "UF_HIDDEN-flagged files must filter even without a dot prefix"
        );
    }

    // MARK: - Symlink pin (lstat semantics)

    /// The plan's symlink pin: a symlink-to-directory renders as a
    /// non-expandable **file** row — it sorts into the files bucket, and
    /// `visible_order` never recurses into it.
    #[test]
    fn entries_symlink_to_dir_sorts_as_file_and_does_not_expand() {
        let t = TempTree::new();
        let real = t.mkdir("real_dir");
        // real_dir has a child we must NOT surface through the symlink.
        fs::write(real.join("inner.txt"), b"").unwrap();
        t.touch("a_file.txt");
        // z_link points at real_dir — lstat classifies it as a (file) row.
        let link = t.root.join("z_link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let names = t.names(true, FileBrowserSortCriterion::Name, true);
        // real_dir is the only directory; the symlink sorts among files
        // (a_file.txt, z_link) after it.
        assert_eq!(names, ["real_dir", "a_file.txt", "z_link"]);

        // And it does not expand: even with the link path in expanded_paths,
        // its target's children never appear.
        let link_path = link.to_string_lossy().into_owned();
        let mut expanded = BTreeSet::new();
        expanded.insert(t.root.to_string_lossy().into_owned());
        expanded.insert(link_path.clone());
        let order = visible_order(
            &t.root.to_string_lossy(),
            &expanded,
            true,
            FileBrowserSortCriterion::Name,
            true,
        );
        assert!(order.contains(&link_path));
        assert!(
            !order.iter().any(|p| p.ends_with("inner.txt")),
            "a symlink-to-dir must not expand its target's children"
        );
    }

    // MARK: - Fallback

    /// `FileBrowserListingTests.test_entries_missingPath_returnsEmpty`
    #[test]
    fn entries_missing_path_returns_empty() {
        let t = TempTree::new();
        let missing = t.root.join("does-not-exist");
        assert!(entries(&missing, true, FileBrowserSortCriterion::Name, true).is_empty());
    }

    /// `FileBrowserListingTests.test_entries_emptyDirectory_returnsEmpty`
    #[test]
    fn entries_empty_directory_returns_empty() {
        let t = TempTree::new();
        assert!(entries(&t.root, true, FileBrowserSortCriterion::Name, true).is_empty());
    }

    // MARK: - visibleOrder

    fn expanded(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    fn last_component(p: &str) -> String {
        Path::new(p)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// `FileBrowserListingTests.test_visibleOrder_flatRoot_listsRootThenChildren`
    #[test]
    fn visible_order_flat_root_lists_root_then_children() {
        let t = TempTree::new();
        t.touch("a.txt");
        t.touch("b.txt");
        t.mkdir("Z_dir");
        let root = t.root.to_string_lossy().into_owned();

        let order = visible_order(
            &root,
            &expanded(&[&root]),
            true,
            FileBrowserSortCriterion::Name,
            true,
        );
        assert_eq!(order.first(), Some(&root));
        let rest: Vec<String> = order.iter().skip(1).map(|p| last_component(p)).collect();
        assert_eq!(rest, ["Z_dir", "a.txt", "b.txt"]);
    }

    /// `FileBrowserListingTests.test_visibleOrder_collapsedSubdir_omitsItsChildren`
    #[test]
    fn visible_order_collapsed_subdir_omits_children() {
        let t = TempTree::new();
        let sub = t.mkdir("subdir");
        fs::write(sub.join("inner.txt"), b"").unwrap();
        let root = t.root.to_string_lossy().into_owned();

        // subdir is NOT expanded.
        let order = visible_order(
            &root,
            &expanded(&[&root]),
            true,
            FileBrowserSortCriterion::Name,
            true,
        );
        assert!(
            !order.iter().any(|p| p.ends_with("inner.txt")),
            "children of a collapsed directory must not appear"
        );
    }

    /// `FileBrowserListingTests.test_visibleOrder_expandedSubdir_includesChildrenInDirsFirstOrder`
    #[test]
    fn visible_order_expanded_subdir_includes_children_dirs_first() {
        let t = TempTree::new();
        let sub = t.mkdir("subdir");
        fs::write(sub.join("zfile.txt"), b"").unwrap();
        fs::create_dir(sub.join("anest")).unwrap();
        let root = t.root.to_string_lossy().into_owned();

        // Use the exact path `entries` produces for subdir — the same path the
        // production tree stores in expanded_paths.
        let subdir_path = entries(&t.root, true, FileBrowserSortCriterion::Name, true)
            .into_iter()
            .find(|p| p.file_name().unwrap() == "subdir")
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let order = visible_order(
            &root,
            &expanded(&[&root, &subdir_path]),
            true,
            FileBrowserSortCriterion::Name,
            true,
        );
        let names: Vec<String> = order.iter().map(|p| last_component(p)).collect();
        assert_eq!(
            &names[..4],
            [last_component(&root), "subdir".into(), "anest".into(), "zfile.txt".into()]
        );
    }

    /// `FileBrowserListingTests.test_visibleOrder_passesSortParamsThroughExpandedSubdir`
    #[test]
    fn visible_order_passes_sort_params_through_expanded_subdir() {
        let t = TempTree::new();
        let sub = t.mkdir("subdir");
        let old = sub.join("a_old.txt");
        let new = sub.join("z_new.txt");
        fs::write(&old, b"").unwrap();
        fs::write(&new, b"").unwrap();
        set_mtime_epoch(&old, 1_000_000);
        set_mtime_epoch(&new, 2_000_000);
        let root = t.root.to_string_lossy().into_owned();

        let subdir_path = entries(&t.root, true, FileBrowserSortCriterion::Name, true)
            .into_iter()
            .find(|p| p.file_name().unwrap() == "subdir")
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let order = visible_order(
            &root,
            &expanded(&[&root, &subdir_path]),
            true,
            FileBrowserSortCriterion::DateModified,
            false,
        );
        let names: Vec<String> = order.iter().map(|p| last_component(p)).collect();
        assert_eq!(
            &names[..4],
            [last_component(&root), "subdir".into(), "z_new.txt".into(), "a_old.txt".into()],
            "visible_order must pass criterion + ascending through to entries()"
        );
    }

    /// `FileBrowserListingTests.test_visibleOrder_missingRoot_returnsEmpty`
    #[test]
    fn visible_order_missing_root_returns_empty() {
        let t = TempTree::new();
        let missing = t.root.join("does-not-exist").to_string_lossy().into_owned();
        let order = visible_order(
            &missing,
            &expanded(&[&missing]),
            true,
            FileBrowserSortCriterion::Name,
            true,
        );
        assert!(order.is_empty());
    }

    /// `FileBrowserListingTests.test_visibleOrder_respectsShowHidden`
    #[test]
    fn visible_order_respects_show_hidden() {
        let t = TempTree::new();
        t.touch(".hidden.txt");
        t.touch("visible.txt");
        let root = t.root.to_string_lossy().into_owned();

        let with = visible_order(
            &root,
            &expanded(&[&root]),
            true,
            FileBrowserSortCriterion::Name,
            true,
        );
        let without = visible_order(
            &root,
            &expanded(&[&root]),
            false,
            FileBrowserSortCriterion::Name,
            true,
        );
        assert!(with.iter().any(|p| p.ends_with(".hidden.txt")));
        assert!(!without.iter().any(|p| p.ends_with(".hidden.txt")));
    }
}
