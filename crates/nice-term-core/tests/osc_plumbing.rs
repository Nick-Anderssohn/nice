//! R6 OSC-plumbing integration tests (headless — real ptys + a real login zsh,
//! no window). Covers the plan's Validation steps 1–4 end to end, through the
//! public `Session` / `TermSession` surface:
//!
//! 1. **Titles** — OSC 0/2 with both BEL and ST terminators arrive in order as
//!    [`SessionEvent::TitleChanged`], UTF-8 (accent/emoji/CJK) decodes intact,
//!    and a braille spinner round-trips exactly; a title-stack pop surfaces
//!    [`SessionEvent::TitleReset`].
//! 2. **OSC 7 cwd** — a real-shell `printf '…file://$(hostname)$PWD…'` yields the
//!    exact decoded path; percent-encoding and an empty host decode; a sequence
//!    split across two pty reads still parses; malformed / foreign-host / non-
//!    file sequences are dropped without wedging the tee or the parser (a
//!    following `echo` still renders).
//! 3. **Tee transparency** — a fixture with OSC 7 + heavy mixed output produces a
//!    byte-identical grid with the tee on ([`TermSession::spawn`]) and off
//!    ([`TermSession::spawn_teeless`]).
//! 4. **Paste state** — [`Session::bracketed_paste_active`] flips with
//!    `ESC[?2004h` / `l` and stays consistent across scrollback churn.
//!
//! Cleanliness sweep hook: every command pane here carries the token
//! `NICE_RS_TEST_SENTINEL` in the child's argv (as the wrapped shell's `$0` via
//! [`wrap`]), so it shows in `ps -Aww -o args=` but never in output. After the
//! run, `ps -Aww -o pid=,args= | grep -c '[N]ICE_RS_TEST_SENTINEL'` must be 0.
//! Every session is dropped at end of scope → its process group is torn down
//! (SIGHUP/SIGKILL) and its child reaped, so the suite leaks no shells.

use std::sync::mpsc::Receiver;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nice_term_core::{
    shell_single_quote, DamageCallback, Session, SessionEvent, SpawnSpec, TermSession,
    DEFAULT_SCROLLBACK_LINES,
};

/// argv token the cleanliness sweep greps for (as the wrapped shell's `$0`).
const SENTINEL: &str = "NICE_RS_TEST_SENTINEL";

/// A generous poll ceiling: a login+interactive zsh sources rc before our
/// command runs, and a loaded machine can be slow.
const POLL: Duration = Duration::from_secs(25);

/// A process-wide hermetic `ZDOTDIR` of empty rc files (created once). Keeps the
/// shells real **login+interactive** zsh while sourcing an empty rc instead of
/// the developer's `~/.zshrc` — portable/deterministic AND avoids a heavy rc
/// forking detached async workers that escape the process-group teardown.
fn zdotdir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nice-rs-zdotdir-osc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create ZDOTDIR");
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(dir.join(rc), "").expect("write empty rc");
        }
        dir.to_str().expect("utf8 ZDOTDIR").to_string()
    })
}

/// Env injected into every test session: point zsh at the hermetic [`zdotdir`].
fn test_env() -> Vec<(String, String)> {
    vec![("ZDOTDIR".to_string(), zdotdir().to_string())]
}

/// Wrap a shell `script` so the exec'd process carries [`SENTINEL`] in its argv
/// (as `$0`) while producing exactly `script`'s output →
/// `zsh -ilc "exec sh -c '<script>' NICE_RS_TEST_SENTINEL"`.
fn wrap(script: &str) -> String {
    format!("sh -c {} {}", shell_single_quote(script), SENTINEL)
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

/// Deterministically reap a `TermSession`'s child before it drops (used by the
/// tee-transparency test, which drives `TermSession` directly).
fn reap(session: TermSession) {
    session.teardown();
    let _ = session.wait();
}

/// Drain the event stream up to [`POLL`], returning every event received up to
/// and including the first `Exited` (which terminates the drain).
fn drain_until_exited(events: &Receiver<SessionEvent>) -> Vec<SessionEvent> {
    let deadline = Instant::now() + POLL;
    let mut out = Vec::new();
    loop {
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(r) => r,
            None => return out,
        };
        match events.recv_timeout(remaining) {
            Ok(ev) => {
                let done = matches!(ev, SessionEvent::Exited { .. });
                out.push(ev);
                if done {
                    return out;
                }
            }
            Err(_) => return out,
        }
    }
}

/// The `TitleChanged` payloads from a drained event list, in arrival order.
fn titles(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::TitleChanged(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// The `CwdChanged` paths from a drained event list, in arrival order.
fn cwds(events: &[SessionEvent]) -> Vec<std::path::PathBuf> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::CwdChanged(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Validation 1 — titles
// ---------------------------------------------------------------------------

#[test]
fn titles_osc0_osc2_bel_and_st_utf8_and_braille() {
    // Five titles: OSC 2 (BEL), OSC 2 (ST — `ESC \`), OSC 0 (BEL), a UTF-8 mix
    // (accent + emoji + CJK), and a pure braille spinner frame string. Stage 3
    // depends on the braille/UTF-8 bytes round-tripping exactly.
    let script = concat!(
        "printf '\\033]2;My Title\\007';",
        "printf '\\033]2;ST Title\\033\\\\';",
        "printf '\\033]0;Zero Title\\007';",
        "printf '\\033]2;café ☕ 日本語 😀\\007';",
        "printf '\\033]2;⠋⠙⠹⠸⠼\\007';",
        "printf 'TITLESDONE\\n'"
    );
    let spec = SpawnSpec::command(wrap(script), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn title pane");

    let drained = drain_until_exited(&events);
    assert_eq!(
        titles(&drained),
        vec![
            "My Title".to_string(),
            "ST Title".to_string(),
            "Zero Title".to_string(),
            "café ☕ 日本語 😀".to_string(),
            "⠋⠙⠹⠸⠼".to_string(),
        ],
        "title events must arrive in order with UTF-8/braille fidelity"
    );

    drop(session);
}

#[test]
fn title_stack_pop_emits_reset() {
    // With no title set, `CSI 22 t` pushes an empty title and `CSI 23 t` pops it,
    // which alacritty surfaces as a title reset → SessionEvent::TitleReset.
    let script = "printf '\\033[22t\\033[23t'; printf 'RESETDONE\\n'";
    let spec = SpawnSpec::command(wrap(script), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn reset pane");

    let drained = drain_until_exited(&events);
    assert!(
        drained.iter().any(|e| matches!(e, SessionEvent::TitleReset)),
        "a title-stack pop of an empty title must emit TitleReset; got {drained:?}"
    );

    drop(session);
}

// ---------------------------------------------------------------------------
// Validation 2 — OSC 7 cwd
// ---------------------------------------------------------------------------

#[test]
fn osc7_hostname_and_pwd_exact_path() {
    // The plan's literal form: emit `file://$(hostname)$PWD`. The local hostname
    // must be accepted and the path decoded exactly. Compare canonicalized on
    // both sides to absorb any symlink form of `$PWD`.
    let base = std::env::temp_dir().join(format!("nice-rs-osc7-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&base).expect("create temp cwd");
    let canon = std::fs::canonicalize(&base).expect("canonicalize temp cwd");
    let cwd_str = canon.to_str().expect("utf8 cwd").to_string();

    let script = "printf '\\033]7;file://%s%s\\007' \"$(hostname)\" \"$PWD\"; printf 'CWDMARK\\n'";
    let spec = SpawnSpec::command(wrap(script), &cwd_str).with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn cwd pane");

    let drained = drain_until_exited(&events);
    let got = cwds(&drained);

    // Assert BEFORE removing the temp dir (canonicalize needs it to exist).
    // `$PWD` is already the canonical path here, so a direct match usually
    // holds; canonicalize both sides to absorb any symlinked `$PWD` form.
    let matched = got.iter().any(|p| {
        p == &canon || std::fs::canonicalize(p).ok().as_deref() == Some(canon.as_path())
    });
    assert!(
        matched,
        "expected a CwdChanged of {canon:?}; got {got:?}"
    );

    drop(session);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn osc7_percent_decode_and_empty_host() {
    // Empty host (`file:///…`) + percent-encoded space (`%20`) must decode.
    // `%%20` in the printf format emits a literal `%20`.
    let script = "printf '\\033]7;file:///tmp/a%%20b/c\\007'; printf 'PCTMARK\\n'";
    let spec = SpawnSpec::command(wrap(script), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn pct pane");

    let drained = drain_until_exited(&events);
    assert_eq!(
        cwds(&drained),
        vec![std::path::PathBuf::from("/tmp/a b/c")],
        "empty host + %20 must decode to a spaced path"
    );

    drop(session);
}

#[test]
fn osc7_split_across_two_pty_reads() {
    // Emit the sequence in two halves from separate `printf` PROCESSES (external
    // /usr/bin/printf flushes on exit) with a sleep between, so the feeder reads
    // them in two chunks — the scanner must join them across the boundary.
    let script = "/usr/bin/printf '\\033]7;file://localhost/split/pa'; \
                  sleep 0.3; \
                  /usr/bin/printf 'th/here\\007'; \
                  printf 'SPLITMARK\\n'";
    let spec = SpawnSpec::command(wrap(script), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn split pane");

    let drained = drain_until_exited(&events);
    assert_eq!(
        cwds(&drained),
        vec![std::path::PathBuf::from("/split/path/here")],
        "a split OSC 7 must parse once joined across the read boundary"
    );

    drop(session);
}

#[test]
fn osc7_malformed_dropped_without_wedging_parser() {
    // A foreign host, a non-file scheme, and a truncated percent-escape are each
    // dropped; only the trailing valid sequence emits, and the parser still
    // renders the `echo` after them (tee never corrupts parser output).
    let marker = format!("MALMARK_{}", std::process::id());
    let script = format!(
        "printf '\\033]7;file://some-other-box.example.com/nope\\007';\
         printf '\\033]7;http://localhost/x\\007';\
         printf '\\033]7;file:///bad%%2\\007';\
         printf '\\033]7;file://localhost/good/dir\\007';\
         printf '{marker}\\n'"
    );
    let spec = SpawnSpec::command(wrap(&script), "/private/tmp").with_env(test_env());
    let (session, events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn malformed pane");

    let drained = drain_until_exited(&events);
    assert_eq!(
        cwds(&drained),
        vec![std::path::PathBuf::from("/good/dir")],
        "only the well-formed local OSC 7 must emit"
    );
    // The parser was not wedged: the marker after the bad sequences rendered.
    assert!(
        poll_until(|| session.grid_contains(&marker)),
        "marker after malformed OSC 7 never rendered; last grid:\n{}",
        session
            .visible_snapshot()
            .map(|s| s.text())
            .unwrap_or_default()
    );

    drop(session);
}

// ---------------------------------------------------------------------------
// Validation 3 — tee byte-transparency
// ---------------------------------------------------------------------------

#[test]
fn tee_never_alters_grid_bytes() {
    // A fixture with OSC 7 (teed) plus heavy SGR-colored output. The grid must be
    // byte-identical with the tee on vs. off — the tee is a pure observer.
    let fixture = "printf '\\033]7;file://localhost/tee/a\\007';\
                   for i in $(seq 1 300); do printf 'row %d \\033[32mG\\033[0m\\033[1mB\\033[0m x\\n' \"$i\"; done;\
                   printf '\\033]7;file://localhost/tee/b\\007';\
                   printf 'ENDMARK\\n'";
    let spec = SpawnSpec::command(wrap(fixture), "/private/tmp")
        .with_env(test_env())
        .with_size(40, 120);

    // Tee ON (the normal path).
    let s_on =
        TermSession::spawn(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn tee-on");
    assert!(
        poll_until(|| s_on.grid_contains("ENDMARK")),
        "tee-on fixture never completed"
    );
    let grid_on = s_on.grid_lines();
    reap(s_on);

    // Tee OFF (transparency reference).
    let s_off =
        TermSession::spawn_teeless(&spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn tee-off");
    assert!(
        poll_until(|| s_off.grid_contains("ENDMARK")),
        "tee-off fixture never completed"
    );
    let grid_off = s_off.grid_lines();
    reap(s_off);

    assert_eq!(
        grid_on, grid_off,
        "the OSC 7 tee altered the grid — it must never touch the bytes the parser sees"
    );
}

// ---------------------------------------------------------------------------
// Validation 4 — bracketed-paste state
// ---------------------------------------------------------------------------

#[test]
fn bracketed_paste_toggles_and_survives_scrollback_churn() {
    // A command pane gated on stdin `read`s so each mode can be observed: enable
    // 2004, churn scrollback (still ON), disable 2004, churn again (still OFF).
    // The leading `read z` also gates the very first `ESC[?2004h` so the "starts
    // disabled" assertion below is deterministic, not a race against the child's
    // shell exec + first printf.
    let script = "read z; printf '\\033[?2004h'; read a;\
                  seq 1 300; printf 'CHURN1\\n'; read b;\
                  printf '\\033[?2004l'; read c;\
                  seq 1 300; printf 'CHURN2\\n'; read d";
    let spec = SpawnSpec::command(wrap(script), "/private/tmp")
        .with_env(test_env())
        .with_size(24, 80);
    let (session, _events) =
        Session::spawn(spec, DEFAULT_SCROLLBACK_LINES, no_wake()).expect("spawn paste pane");

    // Before anything, bracketed paste is off: the child is blocked on `read z`
    // and has not yet run its `ESC[?2004h` (and a `-c` command pane never enters
    // zsh's ZLE, so nothing turns it on implicitly). Deterministic — the enable
    // is gated behind the `read` we have not fed yet, so no race with shell start.
    assert!(
        !session.bracketed_paste_active(),
        "bracketed paste must start disabled"
    );

    // Advance past `read z`; the child now runs ESC[?2004h → query flips on.
    session.write_input(b"\n").expect("write past read z");
    assert!(
        poll_until(|| session.bracketed_paste_active()),
        "bracketed paste never became active after ESC[?2004h"
    );

    // Advance past `read a`; churn scrollback while ON.
    session.write_input(b"\n").expect("write past read a");
    assert!(
        poll_until(|| session.grid_contains("CHURN1")),
        "churn-1 output never rendered"
    );
    assert!(
        session.bracketed_paste_active(),
        "bracketed paste must stay ON across scrollback churn"
    );

    // Advance past `read b`; disable → query flips off.
    session.write_input(b"\n").expect("write past read b");
    assert!(
        poll_until(|| !session.bracketed_paste_active()),
        "bracketed paste never cleared after ESC[?2004l"
    );

    // Advance past `read c`; churn scrollback while OFF.
    session.write_input(b"\n").expect("write past read c");
    assert!(
        poll_until(|| session.grid_contains("CHURN2")),
        "churn-2 output never rendered"
    );
    assert!(
        !session.bracketed_paste_active(),
        "bracketed paste must stay OFF across scrollback churn"
    );

    // Let the child exit.
    session.write_input(b"\n").expect("write past read d");
    drop(session);
}
