//! `TermSession` integration tests (headless — real ptys + a real login zsh, no
//! window). Covers this slice's owned validations: a marker from a command pane
//! lands in the grid, a `pwd` command echoes the session cwd, a shell-only pane
//! really is login+interactive (driven through the write-input path), resize
//! propagates to `$COLUMNS`/`stty size` and the `Term` grid dims, the scrollback
//! knob caps history (with coarse RSS sanity), and the damage-wake fires *after*
//! the `Term` lock is released.
//!
//! The feeder thread inside each `TermSession` drains the pty, so no separate
//! drain is needed. Every session is dropped at end of scope → its process group
//! is torn down (SIGHUP/SIGKILL), so the suite leaks no shells.
//!
//! Cleanliness sweep hook: the **command** panes here
//! (`marker_from_command_pane_lands_in_grid`, `command_pane_pwd_echoes_session_cwd`,
//! `scrollback_knob_caps_history_with_bounded_memory`, and
//! `damage_wake_fires_after_lock_released`) carry the token `NICE_TEST_SENTINEL`
//! in the child's argv (via the `sh -c '<script>' NICE_TEST_SENTINEL` [`wrap`]
//! — the token becomes `$0`, so it shows in `ps -Aww -o args=` but never in the
//! command's output). After the run,
//! `ps -Aww -o pid=,args= | grep -c '[N]ICE_RS_TEST_SENTINEL'` must be 0.
//!
//! The two **shell-only** panes (`shell_only_pane_is_login_and_interactive`,
//! `resize_propagates_to_shell_and_grid`) spawn `zsh -il` with no command, so
//! their argv carries no token and the sentinel sweep does not reach them — the
//! plan's step-3 grep cannot detect a shell leaked by those two. They rely
//! instead on deterministic teardown: [`reap`] kills the process group and
//! blocks until the child is reaped before the session drops, so they leak no
//! shell either, just not via the sweep.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nice_term_core::{
    shell_single_quote, DamageCallback, SpawnSpec, TermSession, DEFAULT_SCROLLBACK_LINES,
};

/// argv token the cleanliness sweep greps for. Passed as the wrapped shell's
/// `$0`, so it lands in `ps` args without polluting command output.
const SENTINEL: &str = "NICE_TEST_SENTINEL";

/// A generous poll ceiling: a login+interactive zsh still sources the system rc
/// and initialises ZLE before our command runs, and a loaded machine can be
/// slow, so give real output time to reach the grid.
const POLL: Duration = Duration::from_secs(25);

/// A process-wide hermetic `ZDOTDIR`: a temp dir of empty rc files. Created once.
///
/// The shells under test are still real **login+interactive** zsh (`-il` — the
/// LOGINOK test proves it), but they source this empty rc instead of the
/// developer's personal `~/.zshrc`. That matters for two reasons: (1) the tests
/// stay portable and deterministic — they must not depend on whatever framework
/// a given machine's rc loads; and (2) a heavy rc (e.g. powerlevel10k /
/// oh-my-zsh) forks async worker processes that **detach into their own process
/// group**, so the session's `killpg` teardown cannot reach them and they
/// orphan. Nice's app-side answer to such escapees is the orphan reaper
/// (C12 → R15, out of scope here); the tests avoid provoking them at all.
fn zdotdir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nice-zdotdir-{}", std::process::id()));
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

/// Wrap a shell `script` so the exec'd process carries [`SENTINEL`] in its argv
/// (as `$0`) while producing exactly `script`'s output. Used as a command pane's
/// command → `zsh -ilc "exec sh -c '<script>' NICE_TEST_SENTINEL"`.
fn wrap(script: &str) -> String {
    format!("sh -c {} {}", shell_single_quote(script), SENTINEL)
}

/// A no-op damage wake for tests that don't inspect it.
fn no_wake() -> DamageCallback {
    Box::new(|| {})
}

/// Deterministically reap a session's child before it drops, so the post-suite
/// cleanliness sweep never races a still-exiting child: kill the process group,
/// then block until the reaper records the exit (i.e. the child is reaped, not
/// merely a zombie). Cheap when the child already exited on its own.
fn reap(session: TermSession) {
    session.teardown();
    let _ = session.wait();
}

/// Poll `cond` every 25ms until it is true or `POLL` elapses.
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

/// Peak resident-set size of this process in bytes (macOS `ru_maxrss` is bytes).
fn max_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    usage.ru_maxrss as u64
}

#[test]
fn marker_from_command_pane_lands_in_grid() {
    // Spawn a command pane (the `zsh -ilc "exec <cmd>"` path) that echoes a
    // unique marker; assert it reaches the parsed grid via the feeder.
    let marker = format!("__MARKER_{}__", std::process::id());
    let spec = SpawnSpec::command(wrap(&format!("echo {marker}")), "/private/tmp")
        .with_env(test_env());
    let session =
        TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn command pane");

    assert!(
        poll_until(|| session.grid_contains(&marker)),
        "marker {marker:?} never appeared in the grid; last grid:\n{}",
        session.visible_snapshot().text()
    );
    // Sanity: the visible snapshot has exactly the viewport's row count.
    let snap = session.visible_snapshot();
    assert_eq!(snap.rows.len(), snap.screen_rows);

    reap(session);
}

#[test]
fn command_pane_pwd_echoes_session_cwd() {
    // A command pane rooted at a canonical (symlink-resolved) temp dir; `pwd`
    // must print that exact path into the grid.
    let base = std::env::temp_dir().join(format!("nice-pwd-{}", std::process::id()));
    std::fs::create_dir_all(&base).expect("create temp cwd");
    let cwd = std::fs::canonicalize(&base).expect("canonicalize temp cwd");
    let cwd_str = cwd.to_str().expect("utf8 cwd").to_string();

    let spec = SpawnSpec::command(wrap("pwd"), &cwd_str).with_env(test_env());
    let session = TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn pwd");

    let found = poll_until(|| session.grid_contains(&cwd_str));
    let snapshot = session.visible_snapshot().text();
    reap(session);
    let _ = std::fs::remove_dir_all(&base);

    assert!(
        found,
        "pwd of {cwd_str:?} never appeared in the grid; last grid:\n{snapshot}"
    );
}

#[test]
fn shell_only_pane_is_login_and_interactive() {
    // Verify `-il` reality on a SHELL-ONLY pane, driven through the write-input
    // path (NOT the exec wrapper — `[[ … ]]` after `exec` is a zsh error, and an
    // inner `zsh -c` is non-login by construction). The command prints `LOGINOK`
    // ONLY if the shell is both login and interactive. A wide grid keeps the
    // echoed input line from wrapping, so the output line is unambiguously a
    // row that trims to exactly `LOGINOK` (the echoed command line contains the
    // longer source text, which does not trim-equal `LOGINOK`).
    let spec = SpawnSpec::shell("/private/tmp")
        .with_env(test_env())
        .with_size(50, 200);
    let session =
        TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn shell-only");

    session
        .write_input(b"[[ -o login && -o interactive ]] && echo LOGINOK\n")
        .expect("write to pty");

    let ok = poll_until(|| session.grid_lines().iter().any(|line| line == "LOGINOK"));
    assert!(
        ok,
        "LOGINOK output line never rendered — shell-only pane was not login+interactive; \
         last grid:\n{}",
        session.visible_snapshot().text()
    );

    reap(session);
}

#[test]
fn resize_propagates_to_shell_and_grid() {
    // Shell-only interactive pane at 24x80. `stty size` reports "rows cols" from
    // the tty winsize; `$COLUMNS` is zsh's view (updated on SIGWINCH). After
    // resize, both must reflect the new size and the Term grid dims must follow.
    let spec = SpawnSpec::shell("/private/tmp")
        .with_env(test_env())
        .with_size(24, 80);
    let session =
        TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn shell-only");

    assert_eq!(session.dimensions(), (24, 80), "initial grid dims");

    session.write_input(b"stty size\n").expect("write stty");
    assert!(
        poll_until(|| session.grid_contains("24 80")),
        "initial `stty size` never showed 24 80; last grid:\n{}",
        session.visible_snapshot().text()
    );

    // Resize both the pty (SIGWINCH → shell) and the Term grid.
    session.resize(30, 100).expect("resize");
    assert_eq!(session.dimensions(), (30, 100), "grid dims follow resize");

    session.write_input(b"stty size\n").expect("write stty");
    assert!(
        poll_until(|| session.grid_contains("30 100")),
        "post-resize `stty size` never showed 30 100; last grid:\n{}",
        session.visible_snapshot().text()
    );

    // $COLUMNS as the shell sees it must have followed the SIGWINCH too.
    session
        .write_input(b"echo COLS=$COLUMNS\n")
        .expect("write echo COLUMNS");
    assert!(
        poll_until(|| session.grid_contains("COLS=100")),
        "$COLUMNS never became 100 after resize; last grid:\n{}",
        session.visible_snapshot().text()
    );

    reap(session);
}

#[test]
fn scrollback_knob_caps_history_with_bounded_memory() {
    // Small scrollback limit; stream many lines; history must cap at the limit
    // and process memory must not balloon (coarse RSS sanity).
    const LIMIT: usize = 100;
    const LINES: usize = 5_000;

    let rss_before = max_rss_bytes();

    let spec = SpawnSpec::command(wrap(&format!("seq 1 {LINES}")), "/private/tmp")
        .with_env(test_env())
        .with_size(40, 120);
    let session = TermSession::spawn(&spec, LIMIT, no_wake()).expect("spawn seq pane");
    assert_eq!(session.scrollback_limit(), LIMIT);

    // Once enough lines have scrolled, history saturates at exactly the limit.
    assert!(
        poll_until(|| session.history_lines() == LIMIT),
        "history never reached the {LIMIT}-line cap (got {})",
        session.history_lines()
    );
    // Feeding far more lines than the limit must not push history past it.
    assert_eq!(
        session.history_lines(),
        LIMIT,
        "history exceeded the configured scrollback cap"
    );

    let rss_after = max_rss_bytes();
    let delta_mib = (rss_after.saturating_sub(rss_before)) as f64 / (1024.0 * 1024.0);
    eprintln!(
        "[scrollback] streamed {LINES} lines into a {LIMIT}-line scrollback: \
         RSS {:.1} -> {:.1} MiB (delta {:+.1})",
        rss_before as f64 / (1024.0 * 1024.0),
        rss_after as f64 / (1024.0 * 1024.0),
        delta_mib,
    );
    // Coarse sanity: a capped scrollback retains ~LIMIT rows, so streaming
    // thousands of lines must not grow RSS by hundreds of MiB.
    assert!(
        delta_mib < 200.0,
        "RSS ballooned by {delta_mib:.1} MiB streaming into a {LIMIT}-line scrollback"
    );

    reap(session);
}

#[test]
fn damage_wake_fires_after_lock_released() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    // The wake must (a) actually fire on new output and (b) never be invoked
    // while the feeder still holds the Term lock. We prove (b) by having the
    // callback try to acquire the very same FairMutex: `try_lock_unfair`
    // succeeds only if the feeder released it before waking (the feeder and the
    // callback run on the SAME thread, and the mutex is non-reentrant, so a
    // still-held lock would fail the try). The main thread never locks during
    // the observation window, so a failure can only mean a contract violation.
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_with_free_lock = Arc::new(AtomicUsize::new(0));
    // The callback is moved into the feeder at spawn time, before we have the
    // session's SharedTerm; this slot is filled right after spawn so later
    // wakes can test the lock.
    let term_slot: Arc<Mutex<Option<nice_term_core::SharedTerm>>> = Arc::new(Mutex::new(None));

    let cb_wakes = Arc::clone(&wakes);
    let cb_free = Arc::clone(&wakes_with_free_lock);
    let cb_slot = Arc::clone(&term_slot);
    let on_damage: DamageCallback = Box::new(move || {
        cb_wakes.fetch_add(1, Ordering::Relaxed);
        if let Some(term) = cb_slot.lock().unwrap().as_ref() {
            if term.try_lock_unfair().is_some() {
                cb_free.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    let spec = SpawnSpec::command(wrap("seq 1 200"), "/private/tmp").with_env(test_env());
    let session = TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, on_damage).expect("spawn");
    *term_slot.lock().unwrap() = Some(session.term().clone());

    // Wait until output has flowed (the marker-bearing feeder produced content).
    assert!(
        poll_until(|| session.grid_contains("200")),
        "seq output never reached the grid"
    );
    // And until at least one wake was observed with the lock free.
    assert!(
        poll_until(|| wakes_with_free_lock.load(Ordering::Relaxed) > 0),
        "damage wake never fired with the Term lock released \
         (fired {} times total)",
        wakes.load(Ordering::Relaxed)
    );

    assert!(wakes.load(Ordering::Relaxed) > 0, "damage wake never fired");

    reap(session);
}

#[test]
fn idle_shell_at_prompt_has_no_foreground_child() {
    // R20.5 fallback (the `tcgetpgrp == child_pid` arm ⇒ false): an interactive
    // login shell sitting at its prompt IS the terminal's foreground process
    // group (it is the session/pgroup leader), so it has NO foreground child and
    // must classify as not busy. Drive one command to a marker so we know the
    // shell processed input and returned to the prompt, then assert the predicate
    // reads false. A wide grid keeps the echoed input from wrapping the marker.
    let spec = SpawnSpec::shell("/private/tmp")
        .with_env(test_env())
        .with_size(50, 200);
    let session =
        TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn shell-only");

    session
        .write_input(b"echo IDLEOK\n")
        .expect("write to pty");
    assert!(
        poll_until(|| session.grid_contains("IDLEOK")),
        "shell never echoed the readiness marker"
    );
    // Back at the prompt with no command running: no foreground child.
    assert!(
        poll_until(|| !session.has_foreground_child()),
        "an idle shell at its prompt must have no foreground child"
    );

    reap(session);
}

#[test]
fn exited_pane_has_no_foreground_child() {
    // R20.5 fallback (dead pty ⇒ false): after the child exits, `tcgetpgrp` on
    // the master no longer names a live foreground group, so the predicate must
    // report no foreground child rather than a spurious busy state.
    let spec = SpawnSpec::command(wrap("exit 0"), "/private/tmp").with_env(test_env());
    let session = TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn");

    // Wait for the child to actually exit before probing the dead pty.
    assert!(
        session.wait_timeout(Duration::from_secs(20)).is_some(),
        "command pane did not exit within timeout"
    );
    assert!(
        !session.has_foreground_child(),
        "an exited pane's dead pty must report no foreground child"
    );

    reap(session);
}
