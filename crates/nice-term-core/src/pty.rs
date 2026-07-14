//! Real pty spawn + lifecycle for one pane's child process — the process layer
//! the session struct (built in later R3 slices) is founded on.
//!
//! Honors the PROTECTED spawn contract (`plan` "Binding technical decisions"):
//! a login + interactive zsh, the `zsh -ilc "exec <cmd>"` wrapper for command
//! panes, cwd tilde-expanded (command never), caller env injection, an initial
//! winsize plus resize/SIGWINCH propagation. On top of that it owns the
//! write-input path, child-exit reaping (status recorded, no zombies), and
//! process-group SIGHUP/SIGKILL teardown so no orphaned zsh survives
//! (`OrphanShellReaper` is why: sessions must kill their children on teardown).
//!
//! Implemented directly on `libc` (openpty + fork + login_tty + execve): the
//! spawn contract and the process-group teardown demand precise control that a
//! higher-level pty wrapper does not expose. There is deliberately no
//! alacritty_terminal / Term here — the VT core, feeder thread, and grid API
//! arrive in later slices and read this pty's [`PtyProcess::master_fd`].

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::spawn::{build_argv, build_env, expand_tilde, SpawnSpec, ZSH_PATH};

/// How long the detached escalation thread gives a hung-up child to die before
/// escalating from SIGHUP to SIGKILL (mirrors `TabPtySession.terminatePane`'s
/// 0.5s grace). The wait happens OFF the calling thread — see
/// [`PtyProcess::teardown`].
const TEARDOWN_GRACE: Duration = Duration::from_millis(500);

/// Test-only fault injection: forces the next [`PtyProcess::spawn`]'s reaper
/// thread spawn to fail, exercising the cleanup arm that must kill + reap the
/// already-forked child. In-crate unit tests only.
#[cfg(test)]
static FORCE_REAPER_SPAWN_FAIL: AtomicBool = AtomicBool::new(false);

/// How a child terminated, recorded by the reaper.
///
/// The distinction is exactly what the held-pane classification (a later
/// slice) consumes: a non-zero [`ExitStatus::Exited`] or any
/// [`ExitStatus::Signaled`] is held-worthy; a clean `Exited(0)` is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// Normal termination with this `waitpid` exit code (`WEXITSTATUS`).
    Exited(i32),
    /// Killed by this signal number (`WTERMSIG`).
    Signaled(i32),
}

impl ExitStatus {
    /// The exit code for a normal exit; `None` if the child was signaled.
    pub fn code(&self) -> Option<i32> {
        match self {
            ExitStatus::Exited(c) => Some(*c),
            ExitStatus::Signaled(_) => None,
        }
    }

    /// The signal number if the child was killed by a signal; else `None`.
    pub fn signal(&self) -> Option<i32> {
        match self {
            ExitStatus::Signaled(s) => Some(*s),
            ExitStatus::Exited(_) => None,
        }
    }

    /// Decode a raw `waitpid` status word.
    fn from_raw(status: libc::c_int) -> ExitStatus {
        if libc::WIFEXITED(status) {
            ExitStatus::Exited(libc::WEXITSTATUS(status))
        } else if libc::WIFSIGNALED(status) {
            ExitStatus::Signaled(libc::WTERMSIG(status))
        } else {
            // Stopped/continued — not expected for our own children (we never
            // ask for WUNTRACED/WCONTINUED). Record a sentinel so waiters
            // unblock rather than hang.
            ExitStatus::Exited(-1)
        }
    }
}

/// A one-shot exit cell the reaper thread fills and teardown/waiters read.
struct ExitCell {
    status: Mutex<Option<ExitStatus>>,
    cvar: Condvar,
}

impl ExitCell {
    fn new() -> ExitCell {
        ExitCell {
            status: Mutex::new(None),
            cvar: Condvar::new(),
        }
    }

    fn set(&self, s: ExitStatus) {
        let mut guard = self.status.lock().unwrap();
        if guard.is_none() {
            *guard = Some(s);
            self.cvar.notify_all();
        }
    }

    fn get(&self) -> Option<ExitStatus> {
        *self.status.lock().unwrap()
    }

    /// Block until the child has exited, or `deadline` passes. Returns the
    /// recorded status if it arrived in time.
    fn wait_until(&self, deadline: Option<Instant>) -> Option<ExitStatus> {
        let mut guard = self.status.lock().unwrap();
        loop {
            if let Some(s) = *guard {
                return Some(s);
            }
            match deadline {
                None => guard = self.cvar.wait(guard).unwrap(),
                Some(dl) => {
                    let now = Instant::now();
                    if now >= dl {
                        return *guard;
                    }
                    let (g, _) = self.cvar.wait_timeout(guard, dl - now).unwrap();
                    guard = g;
                }
            }
        }
    }
}

/// A cloneable, `Send` handle that blocks until the child exits and reads the
/// same reaped [`ExitStatus`] the reaper records. It shares the [`ExitCell`]
/// with the owning [`PtyProcess`], so it stays valid after the `PtyProcess`
/// moves (e.g. into a session state machine): the deferred-spawn state
/// machine's exit watcher waits on this **off-thread** to emit its outward
/// `Exited` event without owning — or blocking — the session it belongs to.
#[derive(Clone)]
pub struct ExitWaiter {
    exit: Arc<ExitCell>,
}

impl ExitWaiter {
    /// Block until the child has exited and return its status. The reaper
    /// always fills the [`ExitCell`], so this cannot block forever.
    pub fn wait(&self) -> ExitStatus {
        self.exit.wait_until(None).unwrap()
    }

    /// The recorded exit status if the child has already exited, else `None`.
    pub fn get(&self) -> Option<ExitStatus> {
        self.exit.get()
    }
}

/// One pane's child process behind a real pty. Owns the master fd, the child
/// pid, and the reaper thread. Teardown (explicit or on drop) SIGHUPs the
/// child's process group and arms a detached escalation thread that SIGKILLs
/// the group if the child has not died within the grace — teardown itself
/// never blocks the calling thread (it runs on the app's main thread), yet no
/// orphaned zsh survives.
pub struct PtyProcess {
    master: OwnedFd,
    /// The child pid. Because the child calls `setsid` (via `login_tty`) it is
    /// its own session and process-group leader, so its pgid equals this pid —
    /// which is what makes `killpg(pid, …)` reach the whole group.
    pid: libc::pid_t,
    exit: Arc<ExitCell>,
    reaper: Option<JoinHandle<()>>,
    /// Latched by the first [`teardown`](PtyProcess::teardown): the SIGHUP is
    /// sent and the SIGKILL escalation armed exactly once, no matter how many
    /// times teardown runs (explicit close + the layered drops all call it).
    teardown_started: AtomicBool,
}

impl PtyProcess {
    /// Spawn `spec`'s child behind a fresh pty.
    ///
    /// Fork/exec sequence: `openpty` (with the initial winsize) → `fork` → in
    /// the child, `login_tty(slave)` (new session, controlling tty, dup to
    /// 0/1/2) then `chdir(cwd)` then `execve(/bin/zsh, argv, env)`. All argv,
    /// env, and cwd C strings are built **before** the fork; the child touches
    /// only async-signal-safe calls, per the fork-in-a-multithreaded-process
    /// rule.
    pub fn spawn(spec: &SpawnSpec) -> io::Result<PtyProcess> {
        // ---- Build everything the child needs BEFORE forking (no allocation
        // is permitted between fork and exec in a multithreaded process). ----
        let program = cstr(ZSH_PATH)?;
        let argv_owned: Vec<CString> = build_argv(spec.command.as_deref())
            .into_iter()
            .map(|a| cstr(&a))
            .collect::<io::Result<_>>()?;
        let mut argv_ptrs: Vec<*const libc::c_char> =
            argv_owned.iter().map(|c| c.as_ptr()).collect();
        argv_ptrs.push(ptr::null());

        let env_owned: Vec<CString> = build_env(&spec.env)
            .into_iter()
            .map(|(k, v)| cstr(&format!("{k}={v}")))
            .collect::<io::Result<_>>()?;
        let mut envp_ptrs: Vec<*const libc::c_char> =
            env_owned.iter().map(|c| c.as_ptr()).collect();
        envp_ptrs.push(ptr::null());

        let cwd_c = cstr(&expand_tilde(&spec.cwd))?;

        // ---- Open the pty with the initial winsize. ----
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut ws = libc::winsize {
            ws_row: spec.rows,
            ws_col: spec.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut ws,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }

        // CLOEXEC BOTH pty fds immediately. The master must not survive into the
        // child (or any other exec'd child) so this pane's child exit yields a
        // correct EOF on the master's parent copy. The slave must not survive
        // into a *sibling* exec either: `openpty` creates neither fd with
        // CLOEXEC, and until the parent closes the slave after fork it is
        // inheritable — so a concurrent `fork`/`execve` on another thread
        // (another pane spawning, a `std::process::Command`/`ps` scan in the
        // tests, R13 spawning several panes) that lands in this window would
        // capture the slave, and because it lacked CLOEXEC it would survive the
        // sibling's execve and hold the pty open forever: this pane's child exit
        // would then never EOF the master, wedging the feeder's blocking read and
        // hanging teardown's feeder join.
        //
        // Setting CLOEXEC on the slave is safe: the child reaches its shell via
        // `login_tty`, which `dup2`s the slave onto fds 0/1/2 — and `dup2` clears
        // CLOEXEC on the new descriptors — so the child's stdio survives its
        // execve while the original (CLOEXEC) slave fd is closed there.
        //
        // The window between `openpty` and these two fcntls is inherently
        // non-atomic (`openpty` has no CLOEXEC-atomic variant); a sibling exec
        // in that sub-microsecond gap is the unavoidable residual, not the
        // long-lived leak closed here.
        set_cloexec(master);
        set_cloexec(slave);

        // ---- Fork. ----
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let err = io::Error::last_os_error();
            unsafe {
                libc::close(master);
                libc::close(slave);
            }
            return Err(err);
        }

        if pid == 0 {
            // ===== CHILD =====
            // Only async-signal-safe calls past this point; no allocation, no
            // Rust std sync. Any failure is a hard `_exit(127)`.
            unsafe {
                // New session + controlling tty + dup slave to 0/1/2, closing
                // the original slave fd. master is CLOEXEC, closed at execve.
                if libc::login_tty(slave) != 0 {
                    libc::_exit(127);
                }
                if libc::chdir(cwd_c.as_ptr()) != 0 {
                    libc::_exit(127);
                }
                libc::execve(program.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr());
                // Only reached if execve failed.
                libc::_exit(127);
            }
        }

        // ===== PARENT =====
        // The parent must close the slave or child-exit will never EOF the
        // master (a live slave fd keeps the pty open).
        unsafe {
            libc::close(slave);
        }
        let master = unsafe { OwnedFd::from_raw_fd(master) };

        let exit = Arc::new(ExitCell::new());
        #[cfg(test)]
        let reaper_spawned = if FORCE_REAPER_SPAWN_FAIL.load(Ordering::SeqCst) {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "forced reaper spawn failure (test)",
            ))
        } else {
            spawn_reaper(pid, Arc::clone(&exit))
        };
        #[cfg(not(test))]
        let reaper_spawned = spawn_reaper(pid, Arc::clone(&exit));
        let reaper = match reaper_spawned {
            Ok(handle) => handle,
            Err(e) => {
                // The child is already forked and exec'd, but no `PtyProcess`
                // exists yet — so no Drop/teardown would ever signal or reap
                // it: it would run unowned and turn zombie on exit. Mirror the
                // fork-failure arm's cleanup: kill the group and reap the
                // child synchronously before surfacing the error. The master
                // `OwnedFd` closes on return.
                unsafe { libc::killpg(pid, libc::SIGKILL) };
                let mut status: libc::c_int = 0;
                while unsafe { libc::waitpid(pid, &mut status, 0) } == -1 {
                    if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                        break;
                    }
                }
                return Err(e);
            }
        };

        Ok(PtyProcess {
            master,
            pid,
            exit: Arc::clone(&exit),
            reaper: Some(reaper),
            teardown_started: AtomicBool::new(false),
        })
    }

    /// The child's pid (== its process-group id, since it is a session leader).
    pub fn child_pid(&self) -> libc::pid_t {
        self.pid
    }

    /// The pty master fd. Ownership stays with this `PtyProcess` (it is closed
    /// on drop); later slices' feeder thread reads output bytes from it. Do not
    /// close it or use it past this value's lifetime.
    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    /// Write raw input bytes to the child (the pty master). No newline is
    /// appended — callers frame their own input. Retries short writes and
    /// `EINTR`.
    pub fn write_input(&self, data: &[u8]) -> io::Result<()> {
        let fd = self.master.as_raw_fd();
        let mut written = 0usize;
        while written < data.len() {
            let n = unsafe {
                libc::write(
                    fd,
                    data[written..].as_ptr() as *const libc::c_void,
                    data.len() - written,
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
            written += n as usize;
        }
        Ok(())
    }

    /// Resize the pty to `rows` x `cols`. Setting the master's winsize makes
    /// the kernel deliver `SIGWINCH` to the pty's foreground process group, so
    /// the child (and any full-screen app it runs) reflows.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The recorded exit status if the child has already exited, else `None`.
    pub fn try_status(&self) -> Option<ExitStatus> {
        self.exit.get()
    }

    /// A cloneable handle that blocks until this child exits (see
    /// [`ExitWaiter`]). The deferred-spawn state machine's exit watcher waits
    /// on it to fire the outward `Exited` event off-thread, without owning the
    /// `PtyProcess`.
    pub fn exit_waiter(&self) -> ExitWaiter {
        ExitWaiter {
            exit: Arc::clone(&self.exit),
        }
    }

    /// Block until the child exits and return its status.
    pub fn wait(&self) -> ExitStatus {
        // The reaper always fills the cell, so this cannot block forever.
        self.exit.wait_until(None).unwrap()
    }

    /// Block until the child exits or `timeout` elapses; `None` on timeout.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<ExitStatus> {
        self.exit.wait_until(Some(Instant::now() + timeout))
    }

    /// Force the child (and its whole process group) to exit. Sends SIGHUP —
    /// the traditional "your tty is gone" signal an interactive zsh handles by
    /// exiting cleanly and hanging up its own jobs — and arms a detached
    /// escalation thread that SIGKILLs the group if the child has not died
    /// within [`TEARDOWN_GRACE`]. Signals the **process group** (`killpg`) so
    /// an `exec`'d command and any children go together; no orphaned zsh
    /// survives.
    ///
    /// **Never blocks the caller.** Teardown runs on the app's main thread
    /// (Cmd+W, quit, drop): a synchronous grace wait froze the UI for 500 ms
    /// per pane whenever the direct child ignored SIGHUP (an `exec`'d command
    /// trapping HUP), and indefinitely for a child stuck in uninterruptible
    /// sleep (whose `waitpid` cannot return) — the escalation therefore waits
    /// on its own thread.
    ///
    /// Idempotent and safe to call after the child already exited: it first
    /// checks the recorded status and skips signaling entirely if the child is
    /// already reaped (which also avoids racing a reused pid), and the
    /// SIGHUP + escalation fire only on the first call.
    pub fn teardown(&self) {
        if self.exit.get().is_some() {
            return; // already exited and reaped — nothing to signal.
        }
        if self.teardown_started.swap(true, Ordering::SeqCst) {
            return; // already signaled — the escalation thread takes it from here.
        }
        self.signal_group(libc::SIGHUP);
        let exit = Arc::clone(&self.exit);
        let pid = self.pid;
        let escalation = std::thread::Builder::new()
            .name("nice-term-sigkill".to_string())
            .spawn(move || {
                if exit
                    .wait_until(Some(Instant::now() + TEARDOWN_GRACE))
                    .is_none()
                {
                    // pgid == pid (session leader); ESRCH (group already gone)
                    // is expected and ignored.
                    unsafe { libc::killpg(pid, libc::SIGKILL) };
                }
            });
        if escalation.is_err() {
            // Could not arm the async escalation (thread exhaustion): fall
            // back to the old synchronous grace rather than skip the SIGKILL
            // and risk an orphan.
            if self.wait_timeout(TEARDOWN_GRACE).is_none() {
                self.signal_group(libc::SIGKILL);
            }
        }
    }

    fn signal_group(&self, sig: libc::c_int) {
        // pgid == pid because the child is a session leader (login_tty →
        // setsid). ESRCH (group already gone) is expected and ignored.
        unsafe {
            libc::killpg(self.pid, sig);
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        // Kill the child's group so nothing is orphaned. Join the reaper only
        // when the child is already reaped (the join is then immediate);
        // otherwise detach it — it exits on its own once its waitpid returns,
        // after the escalation thread's SIGKILL at the latest. Joining
        // unconditionally would re-block the main thread for the grace on a
        // SIGHUP-immune child, and forever on a child in uninterruptible
        // sleep. The OwnedFd master is closed after.
        self.teardown();
        if let Some(handle) = self.reaper.take() {
            if self.exit.get().is_some() {
                let _ = handle.join();
            }
        }
    }
}

/// Spawn the reaper thread: the single `waitpid` caller for this child. It
/// blocks until the child exits, records the status, then exits. Being the sole
/// waiter avoids zombies and double-reap races; teardown/waiters read the
/// [`ExitCell`] it fills rather than calling `waitpid` themselves.
fn spawn_reaper(pid: libc::pid_t, exit: Arc<ExitCell>) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nice-term-reaper".to_string())
        .spawn(move || loop {
            let mut status: libc::c_int = 0;
            let r = unsafe { libc::waitpid(pid, &mut status, 0) };
            if r == pid {
                exit.set(ExitStatus::from_raw(status));
                return;
            }
            if r == -1 {
                let e = io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if e == libc::EINTR {
                    continue;
                }
                // ECHILD (already reaped — unreachable given we are the sole
                // reaper) or any other error: unblock waiters with a sentinel
                // rather than spin.
                exit.set(ExitStatus::Exited(-1));
                return;
            }
            // r == 0 only with WNOHANG (we do not pass it) — treat as spurious.
        })
        .map_err(io::Error::from)
}

/// Best-effort set of `FD_CLOEXEC` on `fd`. A failed `F_GETFD` (or the `F_SETFD`
/// that follows) leaves the fd inheritable — which only widens the already-racy
/// inherit window, not a correctness break — so the error is swallowed rather
/// than aborting the spawn.
fn set_cloexec(fd: libc::c_int) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags != -1 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// Build a `CString`, mapping an interior-NUL error to an `io::Error` so spawn
/// returns `Err` instead of panicking on a pathological command/env/cwd.
fn cstr(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hermetic `ZDOTDIR` (empty rc files) so the spawned zsh never reads the
    /// developer's real `~/.zshrc` — same pattern as the integration suites.
    fn test_env() -> Vec<(String, String)> {
        let dir = std::env::temp_dir().join(format!("nice-zdotdir-ptyunit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create ZDOTDIR");
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(dir.join(rc), "").expect("write empty rc");
        }
        vec![("ZDOTDIR".to_string(), dir.to_str().expect("utf8").to_string())]
    }

    #[test]
    fn reaper_spawn_failure_kills_and_reaps_the_forked_child() {
        // When the reaper thread fails to spawn, the child is already forked
        // and exec'd but no `PtyProcess` exists — the error arm must kill and
        // reap it synchronously, or it runs unowned (and zombies on exit).
        // The command's unique sleep duration doubles as a ps-scan marker.
        let marker = format!("sleep {}", 200_000 + std::process::id());
        let spec = SpawnSpec::command(&marker, "/tmp").with_env(test_env());

        FORCE_REAPER_SPAWN_FAIL.store(true, Ordering::SeqCst);
        let result = PtyProcess::spawn(&spec);
        FORCE_REAPER_SPAWN_FAIL.store(false, Ordering::SeqCst);

        assert!(result.is_err(), "spawn must surface the reaper-spawn failure");
        // The cleanup killpg+waitpid is synchronous, so by the time spawn
        // returned no process carrying the marker may exist — neither the
        // wrapper zsh (whose argv holds the command string) nor an exec'd
        // sleep, and no zombie either (a zombie is reaped, not listed).
        let ps = std::process::Command::new("ps")
            .args(["-Aww", "-o", "args="])
            .output()
            .expect("run ps");
        let listing = String::from_utf8_lossy(&ps.stdout);
        assert!(
            !listing.contains(&marker),
            "child carrying `{marker}` survived the failed spawn"
        );
    }
}
