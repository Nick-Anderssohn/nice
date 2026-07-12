//! Stale `$TMPDIR` artifact sweep (R14).
//!
//! Ports Swift `NiceServices.cleanupStaleTempFiles` / `tempFileDecision`
//! (`Sources/Nice/State/NiceServices.swift:448-527`). Prior nice / Nice runs
//! that crashed or were `SIGKILL`ed without running teardown leave two kinds of
//! debris in the process `$TMPDIR`:
//!
//!   * `nice-<pid>-<uuid8>.sock` — the per-window control socket (R14's path
//!     mint), and
//!   * legacy `nice-zdotdir-<pid>` directories — the pre-Application-Support
//!     ZDOTDIR location that older builds (and the Swift app) wrote into
//!     `$TMPDIR`.
//!
//! The sweep removes only debris whose embedded pid names a process that is
//! **gone**. The pid-liveness rule is load-bearing for cross-app safety during
//! the migration: running one Nice variant while a Swift `Nice` (or a second
//! nice) is open must NOT wipe the other live process's `nice-zdotdir-<pid>`
//! dir, or that process's zsh children suddenly source nothing and silently drop
//! every alias in the user's `~/.zshrc`. `kill(pid, 0)` returning anything other
//! than `ESRCH` (in particular `EPERM` — a live process owned by another user)
//! counts as alive.
//!
//! The pure classifier [`temp_file_decision`] takes an injected liveness probe
//! so the ownership policy is unit-tested without touching the filesystem or
//! spawning siblings; [`sweep_stale_temp_files_in`] takes both the directory and
//! the probe so it can be driven against a synthetic temp dir in tests.
//! Production wiring (the `app::run` bootstrap ordering) is R14 slice 3 — this
//! module only provides the functions.

#![allow(dead_code)]

use std::path::Path;

/// Decision for a single entry encountered during the `$TMPDIR` sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempFileDecision {
    /// Not a Nice artifact — leave it alone.
    Ignore,
    /// A Nice artifact whose owning pid is still alive — keep it.
    Keep,
    /// Leftover from a prior crashed run — remove it.
    Remove,
}

/// Pure classifier for one temp-dir entry. `is_alive` probes whether a pid is
/// still running (production passes [`pid_is_alive`]; tests inject a set). A
/// `nice-zdotdir-<pid>` dir or a `nice-<pid>-<suffix>.sock` file is kept when
/// its owner is alive and removed when it is gone; anything else is ignored.
pub fn temp_file_decision(filename: &str, is_alive: &impl Fn(i32) -> bool) -> TempFileDecision {
    if let Some(pid) = parse_pid_from_zdotdir_name(filename) {
        return if is_alive(pid) {
            TempFileDecision::Keep
        } else {
            TempFileDecision::Remove
        };
    }
    if let Some(pid) = parse_pid_from_socket_name(filename) {
        return if is_alive(pid) {
            TempFileDecision::Keep
        } else {
            TempFileDecision::Remove
        };
    }
    TempFileDecision::Ignore
}

/// Extract `<pid>` from a legacy `nice-zdotdir-<pid>` directory name. Returns
/// `None` when the name lacks the prefix or the remainder is not an integer
/// (mirrors Swift `pid_t("...")` returning nil on empty / non-numeric input).
fn parse_pid_from_zdotdir_name(name: &str) -> Option<i32> {
    name.strip_prefix("nice-zdotdir-")?.parse::<i32>().ok()
}

/// Extract `<pid>` from a `nice-<pid>-<suffix>.sock` control-socket name (the
/// naming R14's control socket mints). Requires the `nice-` prefix, the `.sock`
/// suffix, and a `-`-delimited leading integer between them.
fn parse_pid_from_socket_name(name: &str) -> Option<i32> {
    let body = name.strip_prefix("nice-")?.strip_suffix(".sock")?;
    let dash = body.find('-')?;
    body[..dash].parse::<i32>().ok()
}

/// `kill(pid, 0)` probes liveness without delivering a signal. It returns 0 when
/// the signal *would* have been delivered, `-1`/`ESRCH` when the pid is gone,
/// and `-1`/`EPERM` when the process exists but is not signalable by us
/// (different user). Treat anything other than `ESRCH` as alive so a live sibling
/// process's tempfile is never reaped.
pub fn pid_is_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 performs error checking only (no signal is
    // sent) and is always safe to call with any pid.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Remove `nice-*.sock` and legacy `nice-zdotdir-*` leftovers from `dir` whose
/// owning pid is gone, using the injected `is_alive` probe. A missing/unreadable
/// directory is a no-op. Directories are removed recursively; socket files are
/// unlinked. Best-effort — individual removal errors are ignored (a racing
/// live sibling may recreate or hold an entry).
pub fn sweep_stale_temp_files_in(dir: &Path, is_alive: &impl Fn(i32) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        match temp_file_decision(name, is_alive) {
            TempFileDecision::Ignore | TempFileDecision::Keep => continue,
            TempFileDecision::Remove => {
                let path = entry.path();
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// Production entry: sweep the process `$TMPDIR` with the real `kill(pid, 0)`
/// probe. Wired into the `app::run` bootstrap ordering by R14 slice 3 (before
/// the first window's socket is minted); never called from `run_selftest`.
pub fn sweep_stale_temp_files() {
    sweep_stale_temp_files_in(&std::env::temp_dir(), &pid_is_alive);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Twin of Swift `NiceServicesCleanupTests.decide`: classify `filename`
    /// treating exactly the pids in `alive` as running.
    fn decide(filename: &str, alive: &[i32]) -> TempFileDecision {
        temp_file_decision(filename, &|p| alive.contains(&p))
    }

    // Ports of NiceServicesCleanupTests.

    #[test]
    fn ignores_unrelated_files() {
        assert_eq!(decide("random-file.txt", &[]), TempFileDecision::Ignore);
        assert_eq!(decide(".DS_Store", &[]), TempFileDecision::Ignore);
        assert_eq!(decide("nice-without-pid", &[]), TempFileDecision::Ignore);
        assert_eq!(decide("not-nice-123.sock", &[]), TempFileDecision::Ignore);
    }

    #[test]
    fn zdotdir_live_owner_is_kept() {
        assert_eq!(decide("nice-zdotdir-4242", &[4242]), TempFileDecision::Keep);
    }

    #[test]
    fn zdotdir_dead_owner_is_removed() {
        assert_eq!(decide("nice-zdotdir-4242", &[]), TempFileDecision::Remove);
    }

    /// The current process is (by definition) alive, so our own zdotdir must
    /// never be swept — the next step of init writes into it.
    #[test]
    fn zdotdir_self_pid_is_kept() {
        let me = std::process::id() as i32;
        assert_eq!(
            decide(&format!("nice-zdotdir-{me}"), &[me]),
            TempFileDecision::Keep
        );
    }

    #[test]
    fn zdotdir_unparseable_pid_is_ignored() {
        assert_eq!(decide("nice-zdotdir-notanumber", &[]), TempFileDecision::Ignore);
        assert_eq!(decide("nice-zdotdir-", &[]), TempFileDecision::Ignore);
    }

    #[test]
    fn socket_live_owner_is_kept() {
        assert_eq!(
            decide("nice-4242-C0FFEE.sock", &[4242]),
            TempFileDecision::Keep
        );
    }

    #[test]
    fn socket_dead_owner_is_removed() {
        assert_eq!(decide("nice-4242-C0FFEE.sock", &[]), TempFileDecision::Remove);
    }

    #[test]
    fn socket_missing_suffix_is_ignored() {
        // Matches the `nice-<pid>-` prefix but is not a socket file.
        assert_eq!(decide("nice-4242-scratch", &[]), TempFileDecision::Ignore);
    }

    #[test]
    fn socket_missing_pid_segment_is_ignored() {
        assert_eq!(decide("nice-.sock", &[]), TempFileDecision::Ignore);
        assert_eq!(decide("nice-abc.sock", &[]), TempFileDecision::Ignore);
    }

    // Liveness probe: our own pid is alive; a plainly-dead sentinel is not.

    #[test]
    fn pid_is_alive_reports_self_alive() {
        assert!(pid_is_alive(std::process::id() as i32));
    }

    #[test]
    fn pid_is_alive_reports_dead_pid_gone() {
        // pid 0x7FFF_FFFE is far above any live pid on macOS (pid_max is ~99999)
        // and unallocated, so kill(pid, 0) returns ESRCH → not alive.
        assert!(!pid_is_alive(0x7FFF_FFFE));
    }

    // The sweep over a synthetic temp dir with an injected probe.

    fn unique_tmp() -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "nice-sweep-test-{}-{n}",
            std::process::id()
        ))
    }

    #[test]
    fn sweep_removes_dead_debris_keeps_live_and_ignores_others() {
        let root = unique_tmp();
        std::fs::create_dir_all(&root).unwrap();

        // Dead-owner debris (should be removed).
        std::fs::create_dir_all(root.join("nice-zdotdir-4242")).unwrap();
        std::fs::write(root.join("nice-4242-C0FFEE.sock"), b"").unwrap();
        // Live-owner debris (this test process is alive → keep).
        let me = std::process::id() as i32;
        std::fs::create_dir_all(root.join(format!("nice-zdotdir-{me}"))).unwrap();
        std::fs::write(root.join(format!("nice-{me}-D00D.sock")), b"").unwrap();
        // Unrelated file (ignore).
        std::fs::write(root.join("keepme.txt"), b"hi").unwrap();

        // Alive iff pid == this process.
        sweep_stale_temp_files_in(&root, &|p| p == me);

        assert!(!root.join("nice-zdotdir-4242").exists(), "dead zdotdir removed");
        assert!(!root.join("nice-4242-C0FFEE.sock").exists(), "dead socket removed");
        assert!(
            root.join(format!("nice-zdotdir-{me}")).exists(),
            "live zdotdir kept"
        );
        assert!(
            root.join(format!("nice-{me}-D00D.sock")).exists(),
            "live socket kept"
        );
        assert!(root.join("keepme.txt").exists(), "unrelated file untouched");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_missing_dir_is_noop() {
        // No panic on a non-existent directory.
        sweep_stale_temp_files_in(&unique_tmp(), &|_| false);
    }
}
