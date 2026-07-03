//! Process-level pty tests (headless — no grid, no Term). Covers the slice's
//! owned validations: injected-env visibility, process-level exit status, the
//! write-input path, and no-orphans after teardown of `sleep 300`. The grid /
//! marker-in-grid / LOGINOK checks belong to later slices that own the Term.
//!
//! Every test drains the pty master on a background thread — exactly what the
//! real feeder thread does — so a chatty login shell never wedges on a full
//! pty buffer, and every test tears its session down (explicitly or via drop),
//! so the suite leaks no shells. A leaked child would be caught by the
//! no-orphans probe.
//!
//! Every session injects a hermetic [`test_env`] `ZDOTDIR` (an empty-rc temp
//! dir), exactly as the `term_session`/`deferred_session` suites do: the shells
//! stay real login+interactive zsh but source empty rc files instead of the
//! developer's `~/.zshrc`, so the tests are portable/deterministic AND a heavy
//! rc (powerlevel10k / oh-my-zsh) cannot fork detached async workers that escape
//! the process-group teardown and orphan.

use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nice_term_core::{ExitStatus, PtyProcess, SpawnSpec};

/// A process-wide hermetic `ZDOTDIR`: a temp dir of empty rc files, created once.
///
/// The shells under test remain real **login+interactive** zsh (`-il` / the
/// `-ilc "exec …"` wrapper), but they source these empty rc files rather than
/// the developer's personal `~/.zshrc`. That keeps the tests (1) portable and
/// deterministic — independent of whatever framework a given machine's rc loads
/// — and (2) free of a heavy rc's detached async workers, which fork into their
/// own process group and would escape the session's `killpg` teardown (the
/// orphan reaper that answers such escapees app-side is C12 → R15, out of scope).
fn zdotdir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nice-rs-zdotdir-pty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create ZDOTDIR");
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(dir.join(rc), "").expect("write empty rc");
        }
        dir.to_str().expect("utf8 ZDOTDIR").to_string()
    })
}

/// Env injected into every test session: point zsh at the hermetic [`zdotdir`]
/// via the spawn contract's caller-supplied env pairs (legitimate env injection,
/// not a change to the login/interactive spawn under test).
fn test_env() -> Vec<(String, String)> {
    vec![("ZDOTDIR".to_string(), zdotdir().to_string())]
}

/// A background thread continuously reading the pty master into a buffer, so
/// the child can always flush output and exit cleanly (no reader ⇒ a full pty
/// buffer blocks the child's writes and it never becomes reapable). Ends on
/// EOF / EIO, i.e. when the child's slave side closes.
struct Drain {
    output: Arc<Mutex<Vec<u8>>>,
    handle: JoinHandle<()>,
}

impl Drain {
    fn start(fd: RawFd) -> Drain {
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n =
                    unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n > 0 {
                    sink.lock().unwrap().extend_from_slice(&buf[..n as usize]);
                } else if n == 0 {
                    break; // EOF (Linux-style)
                } else {
                    let e = std::io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    break; // EIO is the macOS "slave closed" EOF
                }
            }
        });
        Drain { output, handle }
    }

    /// Join the reader (it ends once the child's slave closes) and return all
    /// bytes it collected.
    fn join(self) -> Vec<u8> {
        let _ = self.handle.join();
        Arc::try_unwrap(self.output)
            .expect("drain thread still holds output")
            .into_inner()
            .unwrap()
    }
}

/// True while `pid` still exists (kill(pid, 0) succeeds).
fn pid_alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn injected_env_is_visible_to_wrapped_command() {
    // A caller-injected env pair must reach the wrapped command. The command is
    // single-quoted for zsh so `$NICE_TEST_VAR` is expanded by the inner sh
    // (which inherits the pair through the exec), not by zsh.
    let value = format!("NICE_RS_ENVOK_{}", std::process::id());
    let mut env = test_env();
    env.push(("NICE_TEST_VAR".to_string(), value.clone()));
    let spec = SpawnSpec::command("sh -c 'echo $NICE_TEST_VAR'", "/tmp").with_env(env);
    let pty = PtyProcess::spawn(&spec).expect("spawn command pane");
    let drain = Drain::start(pty.master_fd());

    let status = pty
        .wait_timeout(Duration::from_secs(15))
        .expect("wrapped command did not exit within timeout");
    let output = String::from_utf8_lossy(&drain.join()).into_owned();

    assert_eq!(status.code(), Some(0), "echo command should exit cleanly");
    assert!(
        output.contains(&value),
        "injected env value {value:?} never rendered on the pty; got: {output:?}"
    );
}

#[test]
fn command_exit_status_is_reaped() {
    // A command that exits 3 must be reaped with the recorded exit code — no
    // grid needed, this is a pure process-level status assertion.
    let spec = SpawnSpec::command("sh -c 'exit 3'", "/tmp").with_env(test_env());
    let pty = PtyProcess::spawn(&spec).expect("spawn command pane");
    let drain = Drain::start(pty.master_fd());

    let status = pty
        .wait_timeout(Duration::from_secs(15))
        .expect("child did not exit within timeout");
    let _ = drain.join();

    assert_eq!(status, ExitStatus::Exited(3));
    assert_eq!(status.code(), Some(3));
    assert_eq!(status.signal(), None);
}

#[test]
fn write_input_reaches_the_shell() {
    // Exercises the write-input path against a live shell-only (`-il`) pane:
    // typing `exit 7` and a newline must drive the interactive login shell to
    // exit with code 7. This proves write_input delivers bytes the shell reads
    // and acts on, plus shell-only spawn + reaping end to end.
    let spec = SpawnSpec::shell("/tmp").with_env(test_env());
    let pty = PtyProcess::spawn(&spec).expect("spawn shell-only pane");
    let drain = Drain::start(pty.master_fd());

    pty.write_input(b"exit 7\n").expect("write to pty");

    let status = pty
        .wait_timeout(Duration::from_secs(20))
        .expect("shell did not exit after `exit 7` within timeout");
    let _ = drain.join();

    assert_eq!(status, ExitStatus::Exited(7));
}

#[test]
fn teardown_of_sleep_leaves_no_orphan() {
    // A live `sleep 300` command pane: after teardown the child's process group
    // is signaled, the child dies, and it is reaped — so its pid is gone.
    let spec = SpawnSpec::command("sleep 300", "/tmp").with_env(test_env());
    let pty = PtyProcess::spawn(&spec).expect("spawn sleep pane");
    let drain = Drain::start(pty.master_fd());
    let pid = pty.child_pid();

    assert!(pid_alive(pid), "sleep child should be alive right after spawn");
    assert!(pty.try_status().is_none(), "sleep should not have exited yet");

    pty.teardown();
    // teardown blocks until the reaper records the exit, so the zombie is
    // already reaped and the drain's slave has closed.
    let _ = drain.join();

    // The pid must no longer exist. Poll briefly to be robust against reap lag.
    let deadline = Instant::now() + Duration::from_secs(3);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !pid_alive(pid),
        "child pid {pid} still exists after teardown — orphan / group kill failed"
    );
    assert!(
        pty.try_status().is_some(),
        "teardown should have recorded an exit status"
    );
}
