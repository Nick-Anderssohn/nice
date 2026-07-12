//! One-time migration for the `nice-rs` → Nice rename (2026-07).
//!
//! When the Rust build replaced the Swift `Nice` app it dropped the interim
//! `nice-rs` identity it had used to coexist during development: bundle
//! "Nice RS Dev", Claude skill `nice-handoff-rs`, theme slug `nice-rs`. Two
//! leftovers from that interim build would otherwise strand a user who upgrades
//! FROM it (essentially the `nice-rs` experimental-cask testers):
//!
//! 1. **Application Support state.** The interim build stored settings/sessions
//!    under `~/Library/Application Support/Nice RS Dev/`. The renamed build uses
//!    the per-variant folder (`Nice` / `Nice Dev`), so without help the interim
//!    user's fonts/theme/shortcuts + restored tabs would be abandoned in the dead
//!    folder. [`migrate_support_folder`] moves the old folder's entries into the
//!    new one on a genuine first launch of the renamed build.
//! 2. **Claude artifacts.** The interim build installed `-rs`-suffixed Claude
//!    skill/theme files. [`cleanup_rs_artifacts`] deletes them so a stale
//!    `/nice-handoff-rs` skill and `nice-rs` theme don't linger as duplicates of
//!    the unsuffixed prod ones the renamed build now installs.
//!
//! Both jobs are best-effort and idempotent; every failure is swallowed (a
//! migration hiccup must NEVER block startup). Production [`run`] resolves the
//! real support-root + `$HOME`; the pure inner fns take injectable paths so tests
//! sandbox against scratch dirs and never touch the developer's real state. Call
//! from `app::run` ONLY, BEFORE the settings-import gate and the stores load
//! (so the moved `ui_settings.json`/`sessions.json` are in place); never
//! `run_selftest`.

use std::fs;
use std::path::Path;

/// The interim `nice-rs` build's Application Support folder name.
const RS_DEV_FOLDER: &str = "Nice RS Dev";

/// Run both one-time migrations against the real environment: the Application
/// Support root (`$NICE_APPLICATION_SUPPORT_ROOT` or `~/Library/Application
/// Support`), this build's per-variant folder ([`crate::session_store::store_folder`]),
/// and `$HOME`. A missing `$HOME` skips the Claude-artifact cleanup only.
pub fn run() {
    let support_root = crate::session_store::support_root();
    let new_folder = crate::session_store::store_folder();
    migrate_support_folder(&support_root, &new_folder);
    if let Some(home) = std::env::var_os("HOME") {
        cleanup_rs_artifacts(Path::new(&home));
    }
}

/// Move each entry of `<support_root>/Nice RS Dev/` into
/// `<support_root>/<new_folder>/` when the new folder has no `ui_settings.json`
/// yet (a genuine first launch of the renamed build) and the old folder exists.
/// An entry whose target already exists — e.g. a `sessions.json` the retired
/// Swift build left in the new folder — is LEFT in place (the new folder's copy
/// wins). Best-effort: any I/O error aborts the move silently. Never migrates a
/// folder onto itself (guards the `"Nice (unbundled)"` fallback and any future
/// name collision).
fn migrate_support_folder(support_root: &Path, new_folder: &str) {
    if new_folder == RS_DEV_FOLDER {
        return;
    }
    let old = support_root.join(RS_DEV_FOLDER);
    if !old.is_dir() {
        return;
    }
    let new = support_root.join(new_folder);
    // Only on a genuine first launch of the renamed build — once its own
    // `ui_settings.json` exists we must never re-pull stale interim state.
    if new.join("ui_settings.json").exists() {
        return;
    }
    if fs::create_dir_all(&new).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(&old) else {
        return;
    };
    for entry in entries.flatten() {
        let dst = new.join(entry.file_name());
        if !dst.exists() {
            let _ = fs::rename(entry.path(), &dst);
        }
    }
}

/// Delete the interim `nice-rs` build's Claude skill/theme artifacts by EXACT
/// path (never a glob — the new unsuffixed `nice-handoff` skill dir and `nice`
/// theme file are prefix-siblings and must never be touched). Best-effort;
/// already-absent paths are a clean no-op.
fn cleanup_rs_artifacts(home: &Path) {
    let _ = fs::remove_dir_all(home.join(".claude/skills/nice-handoff-rs"));
    let _ = fs::remove_file(home.join(".nice/nice-handoff-rs.sh"));
    let _ = fs::remove_file(home.join(".claude/themes/nice-rs.json"));
    let _ = fs::remove_file(home.join(".nice/claude-theme-settings-rs.json"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn scratch(tag: &str) -> Scratch {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-rename-mig-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// First launch of the renamed build: the whole interim `Nice RS Dev/`
    /// state (settings, sessions, a subdir) lands in the new per-variant folder.
    #[test]
    fn migrates_interim_folder_on_first_launch() {
        let root = scratch("move");
        let old = root.0.join(RS_DEV_FOLDER);
        fs::create_dir_all(old.join("terminal-themes")).unwrap();
        fs::write(old.join("ui_settings.json"), b"{\"fonts\":1}").unwrap();
        fs::write(old.join("sessions.json"), b"{\"version\":3}").unwrap();
        fs::write(old.join("terminal-themes/x.ghostty"), b"theme").unwrap();

        migrate_support_folder(&root.0, "Nice Dev");

        let new = root.0.join("Nice Dev");
        assert_eq!(fs::read(new.join("ui_settings.json")).unwrap(), b"{\"fonts\":1}");
        assert_eq!(fs::read(new.join("sessions.json")).unwrap(), b"{\"version\":3}");
        assert!(new.join("terminal-themes/x.ghostty").exists());
    }

    /// A second launch (new folder already has its own `ui_settings.json`) is a
    /// no-op — interim state is never re-pulled over the user's live settings.
    #[test]
    fn skips_when_new_folder_already_initialized() {
        let root = scratch("skip");
        fs::create_dir_all(root.0.join(RS_DEV_FOLDER)).unwrap();
        fs::write(root.0.join(RS_DEV_FOLDER).join("ui_settings.json"), b"OLD").unwrap();
        let new = root.0.join("Nice Dev");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("ui_settings.json"), b"LIVE").unwrap();

        migrate_support_folder(&root.0, "Nice Dev");

        assert_eq!(fs::read(new.join("ui_settings.json")).unwrap(), b"LIVE");
    }

    /// A target that already exists in the new folder (e.g. Swift left a
    /// `sessions.json` there) is NOT overwritten — the new folder's copy wins —
    /// while non-colliding interim entries still migrate.
    #[test]
    fn does_not_clobber_existing_target() {
        let root = scratch("noclobber");
        let old = root.0.join(RS_DEV_FOLDER);
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("sessions.json"), b"INTERIM").unwrap();
        fs::write(old.join("ui_settings.json"), b"INTERIM_SETTINGS").unwrap();
        let new = root.0.join("Nice");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("sessions.json"), b"SWIFT").unwrap();

        migrate_support_folder(&root.0, "Nice");

        // Existing sessions.json kept; the absent ui_settings.json migrated in.
        assert_eq!(fs::read(new.join("sessions.json")).unwrap(), b"SWIFT");
        assert_eq!(fs::read(new.join("ui_settings.json")).unwrap(), b"INTERIM_SETTINGS");
    }

    /// No interim folder ⇒ clean no-op.
    #[test]
    fn no_interim_folder_is_a_noop() {
        let root = scratch("absent");
        migrate_support_folder(&root.0, "Nice Dev");
        assert!(!root.0.join("Nice Dev").exists());
    }

    /// Cleanup removes exactly the four `-rs` artifacts and leaves the unsuffixed
    /// prod-name siblings (the prefix-sibling skill dir + theme file) untouched.
    #[test]
    fn cleanup_removes_rs_artifacts_only() {
        let home = scratch("cleanup");
        let h = &home.0;
        fs::create_dir_all(h.join(".claude/skills/nice-handoff-rs")).unwrap();
        fs::write(h.join(".claude/skills/nice-handoff-rs/SKILL.md"), b"x").unwrap();
        fs::create_dir_all(h.join(".claude/skills/nice-handoff")).unwrap();
        fs::write(h.join(".claude/skills/nice-handoff/SKILL.md"), b"keep").unwrap();
        fs::create_dir_all(h.join(".claude/themes")).unwrap();
        fs::write(h.join(".claude/themes/nice-rs.json"), b"x").unwrap();
        fs::write(h.join(".claude/themes/nice.json"), b"keep").unwrap();
        fs::create_dir_all(h.join(".nice")).unwrap();
        fs::write(h.join(".nice/nice-handoff-rs.sh"), b"x").unwrap();
        fs::write(h.join(".nice/nice-handoff.sh"), b"keep").unwrap();
        fs::write(h.join(".nice/claude-theme-settings-rs.json"), b"x").unwrap();
        fs::write(h.join(".nice/claude-theme-settings.json"), b"keep").unwrap();

        cleanup_rs_artifacts(h);

        // -rs artifacts gone.
        assert!(!h.join(".claude/skills/nice-handoff-rs").exists());
        assert!(!h.join(".nice/nice-handoff-rs.sh").exists());
        assert!(!h.join(".claude/themes/nice-rs.json").exists());
        assert!(!h.join(".nice/claude-theme-settings-rs.json").exists());
        // Unsuffixed prod-name siblings survive.
        assert!(h.join(".claude/skills/nice-handoff/SKILL.md").exists());
        assert!(h.join(".claude/themes/nice.json").exists());
        assert!(h.join(".nice/nice-handoff.sh").exists());
        assert!(h.join(".nice/claude-theme-settings.json").exists());
    }

    /// Cleanup over an empty home never errors (idempotent).
    #[test]
    fn cleanup_on_absent_paths_is_a_noop() {
        let home = scratch("cleanup-absent");
        cleanup_rs_artifacts(&home.0);
    }
}
