//! `Session` integration tests (headless — real ptys + a real login zsh, no
//! window). Covers slice 3's owned validations: deferred spawn holds no child
//! until triggered (via a `ps`-args sentinel scan) and re-triggering is
//! idempotent; a non-zero (`exit 3`) exit fires `Exited{status:3}`, is held, and
//! leaves the grid readable; a clean `exit 0` and an explicit user close are
//! both not held; and `OutputStarted` fires on the child's first output.
//!
//! Cleanliness sweep hook: every command session here carries the token
//! `NICE_RS_TEST_SENTINEL` (the deferred test uses the longer
//! `NICE_RS_TEST_SENTINEL_DEFERRED`, which still contains it) in the child's
//! argv, via the `sh -c '<script>' <TOKEN>` wrapper — the token becomes the
//! wrapped shell's `$0`, so it shows in `ps -Aww -o args=` but never in the
//! command's output. After the run,
//! `ps -Aww -o pid=,args= | grep -c '[N]ICE_RS_TEST_SENTINEL'` must be 0. Every
//! session is deliberately dropped at end of scope → its process group is torn
//! down (SIGHUP/SIGKILL) and its child reaped, so the suite leaks no shells.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use nice_term_core::{
    shell_single_quote, DamageCallback, DrainWake, ExitStatus, Phase, Session, SessionEvent,
    SpawnSpec, DEFAULT_SCROLLBACK_LINES,
};

/// Base argv token the cleanliness sweep greps for.
const SENTINEL: &str = "NICE_RS_TEST_SENTINEL";
/// A per-test-unique token for the deferred `ps`-args scan. It CONTAINS
/// [`SENTINEL`], so the global sweep still catches a leak, while no other test's
/// child argv contains this longer string — the scan sees only this test's child.
const SENTINEL_DEFERRED: &str = "NICE_RS_TEST_SENTINEL_DEFERRED";

/// A generous poll ceiling: a login+interactive zsh still sources the system rc
/// before our command runs, and a loaded machine can be slow.
const POLL: Duration = Duration::from_secs(25);

/// A process-wide hermetic `ZDOTDIR` of empty rc files (created once). Keeps the
/// shells real **login+interactive** zsh while sourcing an empty rc instead of
/// the developer's `~/.zshrc` — portable/deterministic AND avoids a heavy rc
/// (powerlevel10k / oh-my-zsh) forking detached async workers that escape the
/// process-group teardown (the orphan reaper is C12 → R15, out of scope here).
fn zdotdir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nice-rs-zdotdir-def-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create ZDOTDIR");
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(dir.join(rc), "").expect("write empty rc");
        }
        dir.to_str().expect("utf8 ZDOTDIR").to_string()
    })
}

/// Env injected into every test session: point zsh at the hermetic [`zdotdir`]
/// via the spawn contract's caller-supplied env pairs.
fn test_env() -> Vec<(String, String)> {
    vec![("ZDOTDIR".to_string(), zdotdir().to_string())]
}

/// Wrap a shell `script` so the exec'd process carries `token` in its argv (as
/// `$0`) while producing exactly `script`'s output → a command pane's command
/// becomes `zsh -ilc "exec sh -c '<script>' <token>"`. Prefer a compound script
/// (`a; b`) when the child must stay observable in `ps`, so `sh` does not
/// exec-optimize itself away and keep the token.
fn wrap(script: &str, token: &str) -> String {
    format!("sh -c {} {}", shell_single_quote(script), token)
}

/// A no-op damage wake for tests that don't inspect it.
fn no_wake() -> DamageCallback {
    Box::new(|| {})
}

/// Poll `cond` every 25ms until true or `POLL` elapses.
fn poll_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + POLL;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Count processes whose argv contains `needle` (via `ps -Aww -o args=`). The
/// `ps` invocation's own argv does not contain the sentinel tokens, so it never
/// self-matches.
fn ps_count(needle: &str) -> usize {
    let out = std::process::Command::new("ps")
        .args(["-Aww", "-o", "args="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .count()
}

/// Whether `pid` still exists (a live process or an unreaped zombie). Our reaper
/// reaps the direct child, so once the exit is observed `kill(pid, 0)` → `ESRCH`.
fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Drain the event stream up to [`POLL`], skipping non-`Exited` events, and
/// return the exit `(status, held)`; `None` on timeout.
fn recv_exited(events: &Receiver<SessionEvent>) -> Option<(ExitStatus, bool)> {
    let deadline = Instant::now() + POLL;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        match events.recv_timeout(remaining) {
            Ok(SessionEvent::Exited { status, held }) => return Some((status, held)),
            Ok(_) => continue, // skip OutputStarted etc.
            Err(_) => return None,
        }
    }
}

#[test]
fn deferred_mode_spawns_no_child_until_triggered() {
    // A compound script keeps the wrapping `sh` (which carries the token as $0)
    // alive in `ps` args while `sleep` blocks, so the post-trigger scan works.
    let spec = SpawnSpec::command(wrap("sleep 60; exit 0", SENTINEL_DEFERRED), "/private/tmp")
        .with_env(test_env());
    let (mut session, _events) = Session::deferred(spec, DEFAULT_SCROLLBACK_LINES, no_wake());

    // Deferred: no child, no pid, and nothing in `ps` carries the token.
    assert_eq!(session.phase(), Phase::NotSpawned);
    assert_eq!(session.child_pid(), None);
    assert_eq!(
        ps_count(SENTINEL_DEFERRED),
        0,
        "a child existed before trigger() — spawn was not actually deferred"
    );

    // Trigger → Live: a real child appears.
    session.trigger().expect("trigger deferred spawn");
    assert_eq!(session.phase(), Phase::Live);
    let pid = session.child_pid().expect("live child pid after trigger");
    assert!(alive(pid), "child pid {pid} not alive after trigger");
    assert!(
        poll_until(|| ps_count(SENTINEL_DEFERRED) >= 1),
        "no child carrying {SENTINEL_DEFERRED} appeared in ps after trigger"
    );

    // Double-trigger is idempotent: same child, no second process.
    session.trigger().expect("idempotent re-trigger");
    assert_eq!(
        session.child_pid(),
        Some(pid),
        "re-trigger replaced the child"
    );
    assert_eq!(
        ps_count(SENTINEL_DEFERRED),
        1,
        "re-trigger spawned a second child"
    );

    drop(session); // deliberate teardown: kills the sleep, joins the threads.
    assert!(
        poll_until(|| ps_count(SENTINEL_DEFERRED) == 0),
        "child carrying {SENTINEL_DEFERRED} survived session drop"
    );
}

#[test]
fn command_exit_three_is_held_and_grid_readable() {
    // A command that prints a marker then exits non-zero. The exit fires
    // Exited{status:3}, is held, and the printed marker stays readable in the
    // held grid (the Term/scrollback is kept alive while held).
    let marker = format!("__HELD3_{}__", std::process::id());
    let script = format!("echo {marker}; exit 3");
    let spec = SpawnSpec::command(wrap(&script, SENTINEL), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn command pane");

    let (status, held) = recv_exited(&events).expect("Exited event for `exit 3`");
    assert_eq!(status, ExitStatus::Exited(3), "raw exit status");
    assert!(held, "a non-zero exit the user didn't ask for must be held");
    assert_eq!(
        session.phase(),
        Phase::Exited {
            status: ExitStatus::Exited(3),
            held: true
        }
    );
    assert!(session.is_held());

    // Grid stays readable while held — poll to let the feeder finish parsing.
    assert!(
        poll_until(|| session.grid_contains(&marker)),
        "held pane grid not readable; last grid:\n{}",
        session
            .visible_snapshot()
            .map(|s| s.text())
            .unwrap_or_default()
    );

    drop(session);
}

#[test]
fn command_exit_zero_is_not_held() {
    // A clean `exit 0` is deliberate — the pane is dropped, not held.
    let spec = SpawnSpec::command(wrap("exit 0", SENTINEL), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn command pane");

    let (status, held) = recv_exited(&events).expect("Exited event for `exit 0`");
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(!held, "a clean exit 0 must not be held");
    assert!(!session.is_held());
    assert!(matches!(session.phase(), Phase::Exited { held: false, .. }));

    drop(session);
}

#[test]
fn explicit_close_is_not_held() {
    // A long-running command closed explicitly (Nice's Cmd+W). The intentional
    // flag latches BEFORE the process-group kill, so the forced SIGHUP/SIGKILL
    // classifies as NOT held — no spurious "[killed by signal]" hold.
    let spec = SpawnSpec::command(wrap("sleep 60; exit 0", SENTINEL), "/private/tmp")
        .with_env(test_env());
    let (mut session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn command pane");
    let pid = session.child_pid().expect("live pid before close");

    session.close();

    let (_status, held) = recv_exited(&events).expect("Exited event after explicit close");
    assert!(!held, "an explicit user-initiated close must not be held");
    assert!(matches!(session.phase(), Phase::Exited { held: false, .. }));
    assert!(
        poll_until(|| !alive(pid)),
        "child pid {pid} survived explicit close"
    );

    drop(session);
}

#[test]
fn output_started_fires_on_first_output() {
    // A command that prints then exits: OutputStarted must fire (once) on the
    // first parsed chunk, and Exited must follow.
    let spec =
        SpawnSpec::command(wrap("echo hello; exit 0", SENTINEL), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn command pane");

    let mut saw_output_started = false;
    let mut saw_exited = false;
    let deadline = Instant::now() + POLL;
    while !saw_exited {
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(r) => r,
            None => break,
        };
        match events.recv_timeout(remaining) {
            Ok(SessionEvent::OutputStarted) => saw_output_started = true,
            Ok(SessionEvent::Exited { .. }) => saw_exited = true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(
        saw_output_started,
        "OutputStarted never fired for a command that printed output"
    );
    assert!(saw_exited, "Exited never fired");

    drop(session);
}

#[test]
fn drain_wake_fires_after_exited_event() {
    // `Exited` is the one outward event with no trailing damage-wake, so the
    // exit-watcher must poke the installed DrainWake itself — otherwise an
    // event-driven consumer's drain never learns the child exited. Assert the
    // wake fires, and that once it has, the `Exited` event is already queued
    // (the watcher sends BEFORE it wakes, so a woken drain always finds it).
    let wakes = Arc::new(AtomicUsize::new(0));
    let drain_wake: DrainWake = {
        let wakes = Arc::clone(&wakes);
        Arc::new(move || {
            wakes.fetch_add(1, Ordering::SeqCst);
        })
    };
    let spec = SpawnSpec::command(wrap("exit 0", SENTINEL), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn_with_drain_wake(spec, DEFAULT_SCROLLBACK_LINES, no_wake(), drain_wake)
            .expect("spawn command pane with drain wake");

    assert!(
        poll_until(|| wakes.load(Ordering::SeqCst) >= 1),
        "drain-wake never fired after the child exited"
    );
    // The wake trails the send, so the event is already available now.
    let (status, _held) = recv_exited(&events).expect("Exited queued before the drain-wake fired");
    assert_eq!(status, ExitStatus::Exited(0));

    drop(session);
}

#[test]
fn deferred_session_has_no_foreground_child() {
    // R20.5 fallback: a `NotSpawned` session (no live pty) has no foreground
    // child — the model-only / unspawned-pane path (a lazy companion terminal
    // never focused is idle, not busy). No syscall runs; the delegate short-
    // circuits on the absent `TermSession`.
    let spec = SpawnSpec::command(wrap("sleep 60; exit 0", SENTINEL), "/private/tmp")
        .with_env(test_env());
    let (session, _events) = Session::deferred(spec, DEFAULT_SCROLLBACK_LINES, no_wake());

    assert_eq!(session.phase(), Phase::NotSpawned);
    assert!(
        !session.has_foreground_child(),
        "an unspawned session must report no foreground child"
    );
    // Never triggered → no child was ever spawned, nothing to reap.
    drop(session);
}
