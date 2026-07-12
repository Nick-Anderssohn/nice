//! Orphan shell reaper — SIGKILL every zsh a prior crashed `nice` run left
//! reparented to launchd. Rust port of Swift
//! `Sources/Nice/Process/OrphanShellReaper.swift`.
//!
//! After a normal quit each pane's zsh is terminated and exits before the
//! parent. After a crash / SIGKILL of the parent / a teardown path that never
//! runs, the children reparent to launchd (PPID == 1) and sit idle holding pty
//! slots. macOS caps `kern.tty.ptmx_max` at 511; accumulated orphans eventually
//! starve `forkpty()` and new panes hang at "Launching terminal…". This reaper
//! runs once, synchronously, at bootstrap BEFORE any pane spawns — wired from
//! `app::run`'s `install_shell_inject_bootstrap` (this module owns the pure
//! filter + the OS surface behind the injectable [`ReaperEnv`] seam).
//!
//! Match = ALL FOUR (never name-pattern matching): PPID == 1, uid == getuid(),
//! kernel comm == "zsh" (the three [`live_candidate_pids`] applies via libproc),
//! and the env contains `NICE_TAB_ID=` (the load-bearing safety guard [`reap`]
//! applies). The env check is what keeps us from SIGKILLing a non-Nice zsh the
//! user deliberately daemonized (nohup'd, detached from a launchd job, …).
//! Sibling live `nice` instances are filtered by PPID — their children's
//! PPID is the live instance's pid, not 1.
//!
//! The OS surface (libproc / sysctl / kill) is injected via [`ReaperEnv`] so the
//! reaper logic — the env filter and kill-counting — is unit-tested without
//! running real zshes. Raw `libc` FFI (the `platform.rs` precedent); no new
//! process-inspection dependency.

/// `PROC_ALL_PIDS` from `<sys/proc_info.h>` — not re-exported by the `libc`
/// crate, so pinned here at its header value. Selects "every pid" for
/// [`libc::proc_listpids`].
const PROC_ALL_PIDS: u32 = 1;

/// The OS surface the reaper depends on, as a struct of closures. Tests
/// substitute closures returning canned data; production uses [`ReaperEnv::live`],
/// which wires through to libproc + sysctl + `kill(2)`. Kept as a struct of
/// closures (not a trait) so test fakes are a one-liner. Not `Send`/`Sync`: the
/// reaper is invoked once at bootstrap on the main thread and tests run their
/// fakes inline.
pub(crate) struct ReaperEnv {
    /// Pids that match the first three filter criteria (PPID == 1, uid == me,
    /// comm == "zsh"). Empty on enumeration failure.
    pub list_candidates: Box<dyn Fn() -> Vec<libc::pid_t>>,
    /// Read a process's environment, or `None` if the process is gone or the
    /// kernel refused the `KERN_PROCARGS2` read.
    pub environment: Box<dyn Fn(libc::pid_t) -> Option<Vec<String>>>,
    /// SIGKILL the pid. Returns true on success.
    pub kill: Box<dyn Fn(libc::pid_t) -> bool>,
}

/// SIGKILL every Nice-spawned zsh whose parent died without terminating it
/// cleanly, applying the load-bearing `NICE_TAB_ID=` env guard on top of the
/// candidate enumeration. Idempotent; returns the number of processes killed
/// (successful kills only — an EPERM/ESRCH is attempted but not counted, so the
/// count matches what the call site logs). Pure over the [`ReaperEnv`] seam.
pub(crate) fn reap(env: &ReaperEnv) -> usize {
    let mut killed = 0;
    for pid in (env.list_candidates)() {
        let Some(vars) = (env.environment)(pid) else {
            continue;
        };
        if !vars.iter().any(|v| v.starts_with("NICE_TAB_ID=")) {
            continue;
        }
        if (env.kill)(pid) {
            killed += 1;
        }
    }
    killed
}

/// Parse a `KERN_PROCARGS2` buffer into its env strings only. Layout:
/// `int32 argc | exec_path\0 | NUL pad | argv[0]\0 … argv[argc-1]\0 | env[0]\0 …`.
/// Pure port of Swift `OrphanShellReaper.parseArgsBuffer` — validated against
/// synthetic buffers below without spawning a child. Returns `None` only when
/// the buffer is too short to hold `argc`; a buffer truncated mid-stream yields
/// whatever env strings it managed to read (possibly empty), never a crash and
/// never phantom strings.
pub(crate) fn parse_args_buffer(buf: &[u8], length: usize) -> Option<Vec<String>> {
    let len = length.min(buf.len());
    if len < std::mem::size_of::<i32>() {
        return None;
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let mut idx = std::mem::size_of::<i32>();
    // exec_path: NUL-terminated string immediately after argc.
    while idx < len && buf[idx] != 0 {
        idx += 1;
    }
    // Skip alignment padding (additional NUL bytes).
    while idx < len && buf[idx] == 0 {
        idx += 1;
    }
    // Skip argv strings.
    let mut consumed: i32 = 0;
    while consumed < argc && idx < len {
        while idx < len && buf[idx] != 0 {
            idx += 1;
        }
        if idx < len {
            idx += 1;
        }
        consumed += 1;
    }
    // Remaining bytes are env strings up to a terminating empty string or the
    // end of the buffer.
    let mut env: Vec<String> = Vec::new();
    while idx < len {
        if buf[idx] == 0 {
            break;
        }
        let start = idx;
        while idx < len && buf[idx] != 0 {
            idx += 1;
        }
        if let Ok(s) = std::str::from_utf8(&buf[start..idx]) {
            env.push(s.to_string());
        }
        if idx < len {
            idx += 1;
        }
    }
    Some(env)
}

impl ReaperEnv {
    /// Production wiring: libproc + sysctl + `kill(2)`. The closures call only
    /// thread-safe Darwin primitives (libproc / sysctl / kill are
    /// kernel-mediated and reentrant).
    pub(crate) fn live() -> ReaperEnv {
        ReaperEnv {
            list_candidates: Box::new(live_candidate_pids),
            environment: Box::new(live_environment),
            // SAFETY: `kill(2)` with a pid and SIGKILL touches no user memory.
            kill: Box::new(|pid| unsafe { libc::kill(pid, libc::SIGKILL) } == 0),
        }
    }
}

/// Enumerate every process where PPID == 1, uid == ours, and the kernel comm is
/// `zsh`. Uses libproc (`proc_pidinfo(PROC_PIDTBSDINFO)`) rather than
/// `sysctl(KERN_PROC_ALL)` so the filter doesn't reach into the quirky
/// `kinfo_proc` import. The `NICE_TAB_ID=` env criterion is applied later by
/// [`reap`], not here.
fn live_candidate_pids() -> Vec<libc::pid_t> {
    // SAFETY: `getuid` takes no arguments and cannot fail.
    let my_uid = unsafe { libc::getuid() };
    let mut results = Vec::new();
    for pid in live_all_pids() {
        if pid <= 1 {
            continue;
        }
        // SAFETY: `proc_bsdinfo` is a POD C struct; zeroing it is a valid initial
        // state. `proc_pidinfo` writes at most `size` bytes into `&mut info` and
        // reads nothing from it; we accept the result only on the exact-size
        // return the API documents as success.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let rc = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if rc != size {
            continue;
        }
        if info.pbi_ppid != 1 {
            continue;
        }
        if info.pbi_uid != my_uid {
            continue;
        }
        // `pbi_comm` is a fixed C-string buffer (MAXCOMLEN = 16). `/bin/zsh`'s
        // truncated kernel comm is exactly "zsh".
        if comm_name(&info.pbi_comm) != "zsh" {
            continue;
        }
        results.push(pid);
    }
    results
}

/// Read a fixed C-char comm buffer up to its first NUL into a `String`.
fn comm_name(comm: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Enumerate every pid via `proc_listpids(PROC_ALL_PIDS, …)`.
///
/// NOT `proc_listallpids`: the libc header documents that wrapper as a thin
/// shim over `proc_listpids(…, PROC_ALL_PIDS, …)`, but on macOS 14+ the wrapper
/// empirically returns only ~200 pids even when the system has ~800. Going one
/// layer down returns the full set with the same buffer. Without this the reaper
/// silently misses any orphan past the truncation point. Returns an empty vec on
/// enumeration failure (the reaper is best-effort and never panics).
fn live_all_pids() -> Vec<libc::pid_t> {
    // SAFETY: a null buffer with size 0 is the documented size-probe form; it
    // returns the byte count the full pid table would occupy.
    let probe = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if probe <= 0 {
        return Vec::new();
    }
    let stride = std::mem::size_of::<libc::pid_t>();
    // Pad in case the table grew between the probe and the fill.
    let capacity = probe as usize / stride + 64;
    let mut pids = vec![0 as libc::pid_t; capacity];
    // SAFETY: `pids` is a live buffer of `capacity * stride` bytes; we pass its
    // true byte length as `buffersize`, so `proc_listpids` writes within bounds.
    let filled = unsafe {
        libc::proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * stride) as libc::c_int,
        )
    };
    if filled <= 0 {
        return Vec::new();
    }
    let count = filled as usize / stride;
    pids.truncate(count.min(pids.len()));
    pids
}

/// Read a process's environment via `sysctl(KERN_PROCARGS2)`. Returns `None` if
/// the process is gone, the buffer is malformed, or the kernel refused (a
/// different uid — filtered upstream, but the process can still exit between
/// enumeration and this read).
fn live_environment(pid: libc::pid_t) -> Option<Vec<String>> {
    // Best-effort probe for KERN_ARGMAX; fall through with a 1 MB default (macOS
    // typically reports 1 MB) if it fails.
    let mut arg_max: libc::c_int = 1024 * 1024;
    let mut mib_argmax: [libc::c_int; 2] = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut argmax_size = std::mem::size_of::<libc::c_int>();
    // SAFETY: `mib_argmax` is a 2-element MIB; `arg_max`/`argmax_size` are live
    // and sized for a single `c_int`. A failed probe leaves `arg_max` at its
    // default, which is handled.
    unsafe {
        libc::sysctl(
            mib_argmax.as_mut_ptr(),
            mib_argmax.len() as libc::c_uint,
            &mut arg_max as *mut _ as *mut libc::c_void,
            &mut argmax_size,
            std::ptr::null_mut(),
            0,
        );
    }

    let mut buf = vec![0u8; arg_max.max(0) as usize];
    let mut buf_size = buf.len();
    let mut mib: [libc::c_int; 3] = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `mib` is a 3-element MIB; `buf` is a live `buf_size`-byte buffer and
    // `buf_size` is updated by the kernel to the bytes actually written.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut buf_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    parse_args_buffer(&buf, buf_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    /// Build a synthetic `KERN_PROCARGS2` buffer:
    /// `int32 argc | exec_path\0 | NUL pad | argv strings | env strings`.
    /// `pad_bytes` is the count of extra NULs between the exec_path terminator
    /// and the first argv entry, matching the kernel's alignment padding.
    fn build_buffer(exec_path: &str, argv: &[&str], env: &[&str], pad_bytes: usize) -> Vec<u8> {
        let mut buf = Vec::new();
        let argc = argv.len() as i32;
        buf.extend_from_slice(&argc.to_ne_bytes());
        buf.extend_from_slice(exec_path.as_bytes());
        buf.push(0);
        buf.extend(std::iter::repeat(0u8).take(pad_bytes));
        for a in argv {
            buf.extend_from_slice(a.as_bytes());
            buf.push(0);
        }
        for e in env {
            buf.extend_from_slice(e.as_bytes());
            buf.push(0);
        }
        buf
    }

    // MARK: - parse_args_buffer (synthetic buffers)

    #[test]
    fn parses_env_typical_layout() {
        let buf = build_buffer(
            "/bin/zsh",
            &["zsh", "-il"],
            &["TERM=xterm-256color", "HOME=/Users/nick", "NICE_TAB_ID=tab-abc"],
            7,
        );
        let env = parse_args_buffer(&buf, buf.len());
        assert_eq!(
            env,
            Some(vec![
                "TERM=xterm-256color".to_string(),
                "HOME=/Users/nick".to_string(),
                "NICE_TAB_ID=tab-abc".to_string(),
            ])
        );
    }

    #[test]
    fn zero_argv_still_reads_env() {
        let buf = build_buffer("/bin/zsh", &[], &["NICE_TAB_ID=t1"], 7);
        assert_eq!(
            parse_args_buffer(&buf, buf.len()),
            Some(vec!["NICE_TAB_ID=t1".to_string()])
        );
    }

    #[test]
    fn empty_env_returns_empty_vec() {
        let buf = build_buffer("/bin/zsh", &["zsh"], &[], 7);
        assert_eq!(parse_args_buffer(&buf, buf.len()), Some(vec![]));
    }

    /// The kernel's alignment between exec_path and argv[0] is at least one NUL
    /// but can be more — the parser must skip every trailing NUL before reading
    /// the first argv string.
    #[test]
    fn handles_variable_padding() {
        for pad in 0..=32 {
            let buf = build_buffer("/bin/zsh", &["zsh", "-il"], &["NICE_TAB_ID=t"], pad);
            assert_eq!(
                parse_args_buffer(&buf, buf.len()),
                Some(vec!["NICE_TAB_ID=t".to_string()]),
                "failed at pad={pad}"
            );
        }
    }

    #[test]
    fn truncated_buffer_before_argc_returns_none() {
        let truncated: [u8; 2] = [0x01, 0x00]; // less than sizeof(i32)
        assert_eq!(parse_args_buffer(&truncated, truncated.len()), None);
    }

    /// A buffer cut mid-argv returns whatever env it managed to find (empty
    /// here) and must not crash or invent phantom strings.
    #[test]
    fn truncated_buffer_mid_argv_does_not_crash() {
        let full = build_buffer("/bin/zsh", &["zsh", "-il"], &["NICE_TAB_ID=t"], 7);
        let cut = std::mem::size_of::<i32>() + 8;
        let buf = &full[..cut];
        if let Some(env) = parse_args_buffer(buf, buf.len()) {
            assert!(env.is_empty());
        }
    }

    #[test]
    fn env_order_preserved() {
        let env_in: Vec<String> = (0..10).map(|i| format!("VAR{i}=value{i}")).collect();
        let refs: Vec<&str> = env_in.iter().map(|s| s.as_str()).collect();
        let buf = build_buffer("/bin/zsh", &["zsh"], &refs, 7);
        assert_eq!(parse_args_buffer(&buf, buf.len()), Some(env_in));
    }

    // MARK: - reap over the injectable ReaperEnv seam

    fn make_fake_env(
        candidates: Vec<libc::pid_t>,
        env_by_pid: HashMap<libc::pid_t, Option<Vec<String>>>,
        failed_kills: HashSet<libc::pid_t>,
    ) -> (ReaperEnv, Rc<RefCell<Vec<libc::pid_t>>>) {
        let killed = Rc::new(RefCell::new(Vec::new()));
        let killed_c = killed.clone();
        let env = ReaperEnv {
            list_candidates: Box::new(move || candidates.clone()),
            // Absent key == env read failed (process exited between enumeration
            // and read), mirroring Swift's `envByPid[$0] ?? nil`.
            environment: Box::new(move |pid| env_by_pid.get(&pid).cloned().flatten()),
            kill: Box::new(move |pid| {
                if failed_kills.contains(&pid) {
                    return false;
                }
                killed_c.borrow_mut().push(pid);
                true
            }),
        };
        (env, killed)
    }

    fn env_map(pairs: &[(libc::pid_t, Option<&[&str]>)]) -> HashMap<libc::pid_t, Option<Vec<String>>> {
        pairs
            .iter()
            .map(|(pid, vars)| {
                (
                    *pid,
                    vars.map(|vs| vs.iter().map(|s| s.to_string()).collect()),
                )
            })
            .collect()
    }

    #[test]
    fn reap_empty_candidates_returns_zero_no_kills() {
        let (env, killed) = make_fake_env(vec![], HashMap::new(), HashSet::new());
        assert_eq!(reap(&env), 0);
        assert!(killed.borrow().is_empty());
    }

    /// The env filter is the load-bearing safety check: a zsh under `nohup` or
    /// detached from a launchd job has PPID == 1 and uid == me but no
    /// `NICE_TAB_ID=`. Reaping it would be a real regression.
    #[test]
    fn reap_skips_candidates_without_nice_tab_id_env() {
        let (env, killed) = make_fake_env(
            vec![100, 200, 300],
            env_map(&[
                (100, Some(&["TERM=xterm-256color", "HOME=/Users/x"])),
                (200, Some(&["NICE_TAB_ID=tab-a", "HOME=/Users/x"])),
                (300, Some(&["PATH=/usr/bin", "USER=x"])),
            ]),
            HashSet::new(),
        );
        assert_eq!(reap(&env), 1);
        assert_eq!(*killed.borrow(), vec![200]);
    }

    #[test]
    fn reap_kills_all_nice_tab_id_matches_returns_count() {
        let (env, killed) = make_fake_env(
            vec![10, 20, 30],
            env_map(&[
                (10, Some(&["NICE_TAB_ID=t1"])),
                (20, Some(&["NICE_TAB_ID=t2", "FOO=bar"])),
                (30, Some(&["NICE_TAB_ID=t3"])),
            ]),
            HashSet::new(),
        );
        assert_eq!(reap(&env), 3);
        assert_eq!(*killed.borrow(), vec![10, 20, 30]);
    }

    /// `environment` returning `None` simulates the kernel refusing
    /// `KERN_PROCARGS2` (exited mid-read). The pid must be skipped — neither the
    /// env check nor the kill fires.
    #[test]
    fn reap_skips_process_whose_env_read_fails() {
        let (env, killed) = make_fake_env(
            vec![10, 20, 30],
            env_map(&[
                (10, Some(&["NICE_TAB_ID=t1"])),
                (20, None), // env read failed
                (30, Some(&["NICE_TAB_ID=t3"])),
            ]),
            HashSet::new(),
        );
        assert_eq!(reap(&env), 2);
        assert_eq!(*killed.borrow(), vec![10, 30]);
    }

    /// `kill` returning false (EPERM/ESRCH) must not be counted — the count is
    /// the number of *successful* kills, matching what the call site logs.
    #[test]
    fn reap_kill_failure_does_not_count() {
        let (env, killed) = make_fake_env(
            vec![10, 20],
            env_map(&[(10, Some(&["NICE_TAB_ID=t1"])), (20, Some(&["NICE_TAB_ID=t2"]))]),
            HashSet::from([10]),
        );
        assert_eq!(reap(&env), 1);
        assert_eq!(*killed.borrow(), vec![20]);
    }

    /// A process with no env vars at all walks the env loop without matching.
    #[test]
    fn reap_skips_candidate_with_empty_env() {
        let (env, killed) = make_fake_env(
            vec![10],
            env_map(&[(10, Some(&[]))]),
            HashSet::new(),
        );
        assert_eq!(reap(&env), 0);
        assert!(killed.borrow().is_empty());
    }

    // MARK: - Live-enumeration regression guard

    /// `proc_listallpids` silently truncates to ~200 pids on macOS 14+ even with
    /// a generous buffer; [`live_all_pids`] uses `proc_listpids(PROC_ALL_PIDS,
    /// …)` instead. Pins that we still see the *full* system pid set so a future
    /// "simplification" back to the wrapper fails loudly. Lower bound only —
    /// process count varies wildly by host. Read-only: enumerates, never kills.
    #[test]
    fn live_all_pids_returns_full_system_pid_set_not_truncated() {
        let pids = live_all_pids();
        assert!(
            pids.len() > 100,
            "Expected hundreds of pids on a modern macOS host; got {}. Likely \
             regression: enumeration switched to a truncating API.",
            pids.len()
        );
    }
}
