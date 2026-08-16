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
//! A third kind of debris, added by the stable-socket work (Phase 4 carve-out):
//! `nice-w-<12hex>.sock` — a window-keyed control socket
//! (`control_socket::mint_window_socket_path`) whose name carries no pid at
//! all, because it survives app restart on purpose. Liveness for these is a
//! `connect(2)` probe instead, and it is a **three-way** verdict (D3): a
//! successful connect proves a live owner ⇒ keep; `ECONNREFUSED` proves an
//! orphaned file ⇒ delete; **any other error is doubt ⇒ leave it alone**.
//!
//! That taxonomy is deliberately NOT the one `control_socket`'s bind probe
//! uses, and the difference is load-bearing (D5): the **sweep** must never
//! delete a maybe-live foreign socket, so it ignores on doubt; the **bind**
//! must end with a working socket for the window it is arming, so it takes on
//! doubt (every connect failure there means "unlink and bind"). A transient
//! connect error here (fd exhaustion, `ENOMEM`, `EINTR`) would otherwise make
//! one Nice build's launch-time sweep delete the other build's live window
//! socket. The probe is reimplemented here rather than imported from
//! `control_socket` both because the verdicts differ and so this module stays
//! a dependency-free leaf the way it already was for the pid probe.
//!
//! The pure classifier [`temp_file_decision`] takes an injected liveness probe
//! so the ownership policy is unit-tested without touching the filesystem or
//! spawning siblings; [`sweep_stale_temp_files_in`] takes the directory plus
//! both liveness probes (pid-based and connect-based) so it can be driven
//! against a synthetic temp dir in tests. Production wiring (the `app::run`
//! bootstrap ordering) is R14 slice 3 — this module only provides the
//! functions.

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

/// Recognize a D1 window-keyed control-socket name — `nice-w-<12hex>.sock` —
/// well-formed enough to be worth a connect probe. The `w-` discriminator
/// already keeps [`parse_pid_from_socket_name`] naturally inert on these
/// (`"w"` fails the `i32` parse), so this only needs to guard against
/// coincidental/malformed lookalikes: the key must be non-empty, at most the
/// 12 hex chars the minter emits, and hex-only. Anything that fails this is
/// left to fall through to [`temp_file_decision`], which will `Ignore` it
/// (it isn't a legacy `nice-<pid>-*` name either).
fn is_new_format_socket_name(name: &str) -> bool {
    let Some(key) = name.strip_prefix("nice-w-").and_then(|s| s.strip_suffix(".sock")) else {
        return false;
    };
    !key.is_empty() && key.len() <= 12 && key.chars().all(|c| c.is_ascii_hexdigit())
}

/// Three-way verdict for a window-keyed (`nice-w-*.sock`) socket file (D3).
/// The sweep deletes on [`Stale`](SocketLiveness::Stale) only — both other
/// verdicts leave the file where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketLiveness {
    /// Something accepted our connect — a live owner. Never delete.
    Live,
    /// `ECONNREFUSED` (an orphaned socket file nobody listens on) or `ENOENT`
    /// (already gone). Safe to unlink.
    Stale,
    /// Any other connect error — `ENOTSOCK` junk residue, `EMFILE`/`ENFILE` fd
    /// exhaustion in THIS process, `ENOMEM`, `EINTR`. Proves nothing about the
    /// owner, so the sweep ignores the entry (D3's conservative default).
    Unknown,
}

/// D3 liveness probe for a `nice-w-*.sock` file: a plain blocking
/// `connect(2)`, no bytes sent. Only a successful connect proves a live owner,
/// and only `ECONNREFUSED`/`ENOENT` prove a dead one; everything else is doubt
/// and the caller leaves the file alone.
///
/// Note this is the deliberate DUAL of `control_socket`'s bind-side probe,
/// which treats every connect failure as stale (D5) because it must end up
/// with a bound socket. Do not "unify" the two — see the module header.
pub fn probe_socket_liveness(path: &Path) -> SocketLiveness {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => SocketLiveness::Live,
        Err(e) => match e.kind() {
            // Nobody is listening behind the file; or the file vanished under
            // us, in which case the unlink is simply a no-op.
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound => {
                SocketLiveness::Stale
            }
            _ => SocketLiveness::Unknown,
        },
    }
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

/// Remove `nice-*.sock` (both legacy pid-keyed and D1 window-keyed) and legacy
/// `nice-zdotdir-*` leftovers from `dir` whose owner is gone, using the
/// injected `is_alive` (pid) and `socket_liveness` (connect) probes. The
/// window-keyed branch runs ahead of the legacy pid branch (D3) since its
/// name carries no pid for [`temp_file_decision`] to find, and it deletes only
/// on a [`SocketLiveness::Stale`] verdict — [`Live`](SocketLiveness::Live) and
/// [`Unknown`](SocketLiveness::Unknown) both leave the file alone. A
/// missing/unreadable directory is a no-op. Directories are removed
/// recursively; socket files are unlinked. Best-effort — individual removal
/// errors are ignored (a racing live sibling may recreate or hold an entry).
pub fn sweep_stale_temp_files_in(
    dir: &Path,
    is_alive: &impl Fn(i32) -> bool,
    socket_liveness: &impl Fn(&Path) -> SocketLiveness,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_new_format_socket_name(name) {
            let path = entry.path();
            if socket_liveness(&path) == SocketLiveness::Stale {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
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
/// and connect-probe liveness checks. Wired into the `app::run` bootstrap
/// ordering by R14 slice 3 (before the first window's socket is minted);
/// never called from `run_selftest`.
pub fn sweep_stale_temp_files() {
    sweep_stale_temp_files_in(&std::env::temp_dir(), &pid_is_alive, &probe_socket_liveness);
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

        // Alive iff pid == this process. No new-format sockets in this test.
        sweep_stale_temp_files_in(&root, &|p| p == me, &|_| SocketLiveness::Stale);

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
        sweep_stale_temp_files_in(&unique_tmp(), &|_| false, &|_| SocketLiveness::Stale);
    }

    // D1 window-keyed `nice-w-<12hex>.sock` names: connect-probe liveness
    // (D3), not pid liveness — these names carry no pid at all.

    #[test]
    fn new_format_socket_name_is_recognized() {
        assert!(is_new_format_socket_name("nice-w-3f2a1b9c77d4.sock"));
        assert!(is_new_format_socket_name("nice-w-a.sock"));
    }

    #[test]
    fn new_format_socket_name_rejects_malformed() {
        // Empty key.
        assert!(!is_new_format_socket_name("nice-w-.sock"));
        // Non-hex chars.
        assert!(!is_new_format_socket_name("nice-w-nothex!!.sock"));
        // Key longer than the 12 hex chars the minter ever emits.
        assert!(!is_new_format_socket_name("nice-w-0123456789abcdef.sock"));
        // Missing suffix / prefix entirely.
        assert!(!is_new_format_socket_name("nice-w-3f2a1b9c77d4"));
        assert!(!is_new_format_socket_name("nice-3f2a1b9c77d4.sock"));
    }

    #[test]
    fn new_format_malformed_names_are_ignored_by_the_sweep() {
        let root = unique_tmp();
        std::fs::create_dir_all(&root).unwrap();

        std::fs::write(root.join("nice-w-.sock"), b"").unwrap();
        std::fs::write(root.join("nice-w-nothex!!.sock"), b"").unwrap();

        // Neither probe is ever consulted for a malformed name, and
        // temp_file_decision's own Ignore branch leaves them alone too.
        sweep_stale_temp_files_in(&root, &|_| false, &|_| SocketLiveness::Stale);

        assert!(root.join("nice-w-.sock").exists(), "empty key left alone");
        assert!(
            root.join("nice-w-nothex!!.sock").exists(),
            "non-hex key left alone"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_format_stale_socket_is_removed() {
        let root = unique_tmp();
        std::fs::create_dir_all(&root).unwrap();

        // A real orphaned socket file: bind, then drop the listener. macOS
        // leaves the file behind, so the production probe (real connect(2))
        // sees ECONNREFUSED → Stale end to end.
        let path = root.join("nice-w-3f2a1b9c77d4.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "orphaned socket file survives its listener");

        sweep_stale_temp_files_in(&root, &|_| false, &probe_socket_liveness);

        assert!(!path.exists(), "stale window-keyed socket removed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_format_live_socket_is_kept() {
        let root = unique_tmp();
        std::fs::create_dir_all(&root).unwrap();

        let path = root.join("nice-w-3f2a1b9c77d4.sock");
        // A real listener on the path — the production probe (real
        // connect(2)) proves this live end to end.
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        sweep_stale_temp_files_in(&root, &|_| false, &probe_socket_liveness);

        assert!(path.exists(), "live window-keyed socket kept");

        drop(listener);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// D3's conservative default, and the whole reason the verdict is
    /// three-way: a probe that can't decide (fd exhaustion, ENOMEM, EINTR in
    /// THIS process) must never cost a possibly-live foreign socket its file.
    #[test]
    fn new_format_socket_is_kept_when_the_probe_cannot_decide() {
        let root = unique_tmp();
        std::fs::create_dir_all(&root).unwrap();

        let path = root.join("nice-w-3f2a1b9c77d4.sock");
        std::fs::write(&path, b"").unwrap();

        sweep_stale_temp_files_in(&root, &|_| false, &|_| SocketLiveness::Unknown);

        assert!(path.exists(), "unknown-liveness socket left in place");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Junk residue that is not a socket at all probes as `ENOTSOCK`, which is
    /// doubt, not proof of death — the sweep leaves it (the bind-side probe,
    /// which must end up bound, is the one that clears it).
    #[test]
    fn probe_verdicts_match_the_d3_taxonomy() {
        let root = unique_tmp();
        std::fs::create_dir_all(&root).unwrap();

        let live = root.join("live.sock");
        let listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        assert_eq!(probe_socket_liveness(&live), SocketLiveness::Live);
        drop(listener);
        assert_eq!(probe_socket_liveness(&live), SocketLiveness::Stale);

        assert_eq!(
            probe_socket_liveness(&root.join("never-existed.sock")),
            SocketLiveness::Stale,
            "ENOENT: unlinking is a no-op anyway"
        );

        let junk = root.join("junk.sock");
        std::fs::write(&junk, b"not a socket").unwrap();
        assert_eq!(
            probe_socket_liveness(&junk),
            SocketLiveness::Unknown,
            "ENOTSOCK is doubt, not proof of death"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
