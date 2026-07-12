//! Behavior exemplar harness proofs — **execution model: mocked
//! [`gpui::TestAppContext`], ordinary libtest `#[gpui::test]` cases** (no Metal,
//! no pixels; parallel-safe). These prove the behavior half of the harness
//! against landed surfaces; they are proofs, not feature coverage.
//!
//! Neither case asserts cadence / perf / wall-clock timing — those belong only to
//! the live `NICE_SELFTEST` suite. The file-poll timeouts below are real-clock
//! *readiness* waits on an OS-thread pty child (bounded, fail-loud), not timing
//! assertions.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{ExternalPaths, TestAppContext};
use nice_term_core::DEFAULT_SCROLLBACK_LINES;
use nice_term_view::{TerminalMetrics, TerminalSessionHandle};

use crate::{behavior, session};

/// Fixed cell box for the fixtures (font-independent, like the live renderer
/// self-tests' `FontSettings::fixed`).
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;
/// Bounded wait for the OS-thread pty child / `tee` to deliver bytes to the
/// capture file. Generous; a readiness poll, not a latency assertion.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// **Templates: a future test that drives a real gpui keystroke through the view
/// and asserts the byte sequence the terminal session receives.** (Execution
/// model: mocked `TestAppContext`, libtest.)
///
/// Mounts a real [`nice_term_view::TerminalView`] over a capture-`tee` fixture
/// session, drives a simulated Arrow-Up through gpui's real key-dispatch path
/// (the same `on_key_down` the live app runs), and asserts the encoder's bytes
/// (`ESC [ A`, legacy cursor mode) reach the pty verbatim. The real pty runs on
/// OS threads outside the simulated dispatcher, so the capture file is polled for
/// readiness first (a probe byte written straight to the pty) and then for the
/// encoded bytes, each with a bounded, fail-loud timeout.
#[gpui::test]
fn keystroke_encoder_reaches_session(cx: &mut TestAppContext) {
    let dir = session::temp_dir("kbd").expect("temp dir");
    let cap = dir.join("capture.bin");
    let spec = session::capture_tee_spec(&dir, &cap, 24, 80);
    let handle =
        TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES).expect("spawn session");

    // Mount the real view (fixed metrics + nice-theme tokens) and run to a first
    // paint, which registers `on_key_down` and takes focus. The view is kept
    // mounted in the window root; we drive input through `vcx`.
    let (_terminal, vcx) =
        behavior::mount_terminal(cx, handle.clone(), TerminalMetrics::new(CELL_W, CELL_H));

    // Readiness: write a probe straight to the pty and wait for `tee` to copy it
    // into the capture file (proving raw mode is live) before recording the
    // offset the real keystroke's bytes are asserted against.
    handle
        .update(vcx, |h, _cx| h.session().write_input(b"__ready__"))
        .expect("pty accepted the readiness probe");
    session::poll_capture_contains(&cap, b"__ready__", CAPTURE_TIMEOUT)
        .expect("capture-tee pipeline ready");

    let start = session::cap_len(&cap);
    behavior::press_keys(vcx, "up");
    let got = session::poll_capture_after(&cap, start, 3, CAPTURE_TIMEOUT)
        .expect("encoder bytes reached the session");

    assert_eq!(
        got, b"\x1b[A",
        "Arrow-Up must encode to CSI A (legacy cursor mode) and reach the pty verbatim"
    );
}

/// **Templates: a future test that asserts deterministic timer-driven behavior by
/// advancing the simulated clock, with no wall-clock flakiness.** (Execution
/// model: mocked `TestAppContext`, libtest.)
///
/// Mounts a terminal over a silent session (`sleep`, so no output ever clears the
/// launch overlay), arms the T9 "Launching…" grace deadline with a short grace on
/// first paint, and asserts the overlay is still pending — then advances the
/// **simulated** clock past the grace and asserts the overlay promoted to visible.
/// The clock is driven explicitly (`advance_clock`), so the test is deterministic
/// and never sleeps on the real clock waiting for a timer.
#[gpui::test]
fn advance_clock_promotes_launch_overlay(cx: &mut TestAppContext) {
    let dir = session::temp_dir("overlay").expect("temp dir");
    // A silent pane: no output, so `OutputStarted` never fires to clear the
    // overlay before the grace elapses.
    let spec = session::silent_command_spec(&dir, "sleep 30", 24, 80);
    let handle =
        TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES).expect("spawn session");

    let terminal = behavior::make_terminal(cx, handle, TerminalMetrics::new(CELL_W, CELL_H));
    // Set a short grace BEFORE the first paint arms the deadline.
    let grace = Duration::from_millis(50);
    terminal.update(cx, |view, _cx| view.set_overlay_grace(grace));

    let vcx = behavior::mount_view(cx, terminal.clone());
    assert!(
        !vcx.read(|app| terminal.read(app).overlay_visible()),
        "the launch overlay must not show before the grace deadline elapses"
    );

    // Fire the grace deadline deterministically on the simulated clock.
    vcx.executor().advance_clock(grace + Duration::from_millis(10));
    vcx.run_until_parked();

    assert!(
        vcx.read(|app| terminal.read(app).overlay_visible()),
        "advancing the simulated clock past the grace must promote the launch overlay to visible"
    );
}

/// **R20 (F9): a file-browser row-drag payload is accepted by the terminal's
/// drop target.** The in-tree drag's payload IS a `gpui::ExternalPaths` (the same
/// value the file browser's `on_drag` constructs from a row's drag set), so
/// dragging a tree row onto a terminal must feed T7's landed
/// [`nice_term_view::TerminalView::handle_external_paths_drop`] for free. This
/// pins that contract: a payload built exactly as the browser builds it types the
/// escaped, space-joined, space-padded paths at the prompt. (Execution model:
/// mocked `TestAppContext`, libtest.)
#[gpui::test]
fn file_browser_row_drag_payload_reaches_terminal(cx: &mut TestAppContext) {
    let dir = session::temp_dir("rowdrag").expect("temp dir");
    let cap = dir.join("capture.bin");
    let spec = session::capture_tee_spec(&dir, &cap, 24, 80);
    let handle =
        TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES).expect("spawn session");
    let (terminal, vcx) =
        behavior::mount_terminal(cx, handle.clone(), TerminalMetrics::new(CELL_W, CELL_H));

    // Readiness: prove raw mode is live before recording the assertion offset.
    handle
        .update(vcx, |h, _cx| h.session().write_input(b"__ready__"))
        .expect("pty accepted the readiness probe");
    session::poll_capture_contains(&cap, b"__ready__", CAPTURE_TIMEOUT)
        .expect("capture-tee pipeline ready");

    // Construct the payload EXACTLY as the file browser's row `on_drag` does — an
    // `ExternalPaths` over the row's drag set (here a two-row selection, one path
    // carrying a space so the escape path is exercised).
    let drag_paths = ["/proj/a.txt", "/proj/b c.txt"];
    let payload = ExternalPaths(drag_paths.iter().map(PathBuf::from).collect());

    let start = session::cap_len(&cap);
    terminal.update(vcx, |view, cx| view.handle_external_paths_drop(&payload, cx));

    let got = session::poll_capture_after(&cap, start, 28, CAPTURE_TIMEOUT)
        .expect("the row-drag payload's escaped paths reached the pty");
    assert_eq!(
        got, br#" /proj/a.txt /proj/b\ c.txt "#,
        "the terminal must accept a file-browser row-drag ExternalPaths payload and type the \
         space-joined, backslash-escaped, space-padded paths (T7 target reused by F9)"
    );
}
