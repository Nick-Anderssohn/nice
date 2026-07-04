//! The two R7 terminal-niceties state machines — pure logic, split from paint so
//! both are unit-testable without a window:
//!
//! * [`LaunchOverlay`] (T9) — the "Launching…" placeholder timing machine. A
//!   freshly-spawned pane starts `Pending`; if the child stays **silent** past a
//!   grace window it promotes to `Visible` (the overlay shows); the first output
//!   byte (or the child exiting) clears it permanently. Port of
//!   `SessionsModel.registerPaneLaunch`/`clearPaneLaunch` +
//!   `PaneLaunchStatus` (`AppState.swift`): `pending → visible` on the grace
//!   timer, cleared on `onFirstData` / pane exit, and — the whole point of the
//!   grace window — a fast-starting process never flashes the overlay.
//!
//! * [`HeldPane`] (T10) — the held-pane machine. When the session reports
//!   `Exited { held: true }` (see [`nice_term_core::should_hold_on_exit`]) the
//!   pane is *held*: its view stays mounted and its scrollback readable. A
//!   [`held_exit_footer`] is written into the buffer (the exact dim ANSI footer
//!   `TabPtySession.paneExitFooter` writes) and the single-pane-era dismiss
//!   affordance respawns a fresh shell — the only path that frees the held term.
//!
//! ## The App-Nap-safe grace deadline (T9)
//!
//! The overlay-worthy case is precisely a **silent** pane: no output means no
//! damage, so nothing repaints and nothing re-evaluates "has the grace elapsed?"
//! — the deadline must be *self-driving*. Per the spike-6 App-Nap finding a bare
//! coalescable `background_executor().timer` can be deferred indefinitely on an
//! idle/occluded app, so the deadline uses the harness watchdog pattern (a
//! dedicated OS-thread sleep — scheduler-level, not a libdispatch timer — that
//! wakes the main runloop). That mechanism is objc2/CF-adjacent, so it is
//! **injected** from `crates/nice/src/platform` as a [`LaunchDeadline`] future
//! factory, keeping this crate free of foreign platform code (mirrors the
//! present-kick / keycode-probe injection). Without an injected factory the view
//! falls back to a plain gpui timer, which is fine for the only case a self-test
//! can exercise (a frontmost window).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use nice_term_core::ExitStatus;

/// Default grace window before the "Launching…" overlay appears — the pane must
/// stay silent this long. Ported from `SessionsModel.launchOverlayGraceSeconds`
/// (0.75 s); kept a **test-settable seam** (see
/// [`TerminalView::set_overlay_grace`](crate::view::TerminalView::set_overlay_grace))
/// exactly as the Swift model exposes it (unit tests set it to 0 for synchronous
/// promotion).
pub const DEFAULT_LAUNCH_OVERLAY_GRACE: Duration = Duration::from_millis(750);

/// The footer label for a held pane's in-buffer exit line. Single-pane era has
/// one pane kind, so this is the fixed `"Process"` label
/// `TabPtySession.paneExitFooter` uses for non-Claude panes.
pub const HELD_FOOTER_LABEL: &str = "Process";

/// A future that resolves after the grace delay — the App-Nap-safe deadline seam.
/// Built by a [`LaunchDeadline`] factory and awaited inside the view's overlay
/// task. `!Send` is fine (it is awaited on gpui's foreground executor).
pub type LaunchDeadlineFuture = Pin<Box<dyn Future<Output = ()>>>;

/// Injected factory for the App-Nap-safe grace deadline (T9): given the grace
/// `Duration`, it returns a [`LaunchDeadlineFuture`] that resolves after that
/// delay via the spike-6 watchdog pattern (a dedicated OS-thread sleep that wakes
/// the main runloop, immune to libdispatch timer coalescing). Constructed in
/// `crates/nice/src/platform` (the sole foreign-code home) and installed with
/// [`TerminalView::set_launch_deadline`](crate::view::TerminalView::set_launch_deadline);
/// this crate stays platform-code-free.
pub type LaunchDeadline = Arc<dyn Fn(Duration) -> LaunchDeadlineFuture>;

/// The three-state "Launching…" overlay machine (T9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayPhase {
    /// Spawned, within the grace window, not yet shown.
    Pending,
    /// Grace elapsed with no output — the overlay is showing.
    Visible,
    /// First output byte (or the child exited) arrived — cleared, never shows
    /// again for this launch.
    Cleared,
}

/// The "Launching…" overlay timing state machine (T9) — pure, no gpui.
///
/// Drive it from the R3 [`TerminalEvent`](crate::TerminalEvent) stream + the
/// grace deadline: [`on_grace_elapsed`](Self::on_grace_elapsed) when the deadline
/// fires, [`clear`](Self::clear) on `OutputStarted`/`Exited`. The view reads
/// [`is_visible`](Self::is_visible) to paint and [`ever_visible`](Self::ever_visible)
/// is the "did the overlay ever render?" counter Validation §4's fast-path case
/// asserts stays `false`.
#[derive(Clone, Copy, Debug)]
pub struct LaunchOverlay {
    phase: OverlayPhase,
    ever_visible: bool,
}

impl Default for LaunchOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchOverlay {
    /// A freshly-spawned pane's overlay: `Pending`, never yet visible.
    pub fn new() -> Self {
        Self {
            phase: OverlayPhase::Pending,
            ever_visible: false,
        }
    }

    /// Still within the grace window (not yet shown, not yet cleared) — the state
    /// in which arming the grace deadline is meaningful.
    pub fn is_pending(&self) -> bool {
        self.phase == OverlayPhase::Pending
    }

    /// The overlay is currently showing (grace elapsed with no output yet).
    pub fn is_visible(&self) -> bool {
        self.phase == OverlayPhase::Visible
    }

    /// Whether the overlay has EVER been visible for this launch — the state-
    /// machine counter Validation §4 checks stays `false` for an instant-prompt
    /// pane (first output beat the grace window).
    pub fn ever_visible(&self) -> bool {
        self.ever_visible
    }

    /// The grace deadline fired. Promote `Pending → Visible` (latching the
    /// `ever_visible` counter); a no-op once output already `Cleared` it, so a
    /// deadline that fires after the first byte never resurrects the overlay.
    /// Returns whether the phase changed (the view repaints only then).
    pub fn on_grace_elapsed(&mut self) -> bool {
        if self.phase == OverlayPhase::Pending {
            self.phase = OverlayPhase::Visible;
            self.ever_visible = true;
            true
        } else {
            false
        }
    }

    /// The child produced its first output byte, or exited — clear the overlay
    /// permanently (Swift's `clearPaneLaunch` on `onFirstData` / pane exit).
    /// Idempotent; returns whether the phase changed.
    pub fn clear(&mut self) -> bool {
        if self.phase != OverlayPhase::Cleared {
            self.phase = OverlayPhase::Cleared;
            true
        } else {
            false
        }
    }

    /// Reset to a fresh `Pending` launch — used when a held pane is dismissed and
    /// a fresh shell respawns in place (a new launch gets a new grace window and a
    /// fresh `ever_visible` counter).
    pub fn reset(&mut self) {
        self.phase = OverlayPhase::Pending;
        self.ever_visible = false;
    }
}

/// The held-pane state machine (T10) — pure, no gpui.
///
/// Fed the R3 `Exited { status, held }` classification: it latches the held
/// status (so the view keeps the pane mounted + shows the dismiss affordance) and
/// clears it on dismiss (after which the view respawns a fresh shell).
#[derive(Clone, Copy, Debug, Default)]
pub struct HeldPane {
    status: Option<ExitStatus>,
}

impl HeldPane {
    /// A live (not-yet-held) pane.
    pub fn new() -> Self {
        Self { status: None }
    }

    /// Whether the pane is currently held open after a non-clean exit.
    pub fn is_held(&self) -> bool {
        self.status.is_some()
    }

    /// The latched exit status while held, else `None`.
    pub fn status(&self) -> Option<ExitStatus> {
        self.status
    }

    /// The session reported an exit. Hold iff the R3 classification said so
    /// (`held == true`); a clean / intentional exit (`held == false`) leaves the
    /// pane un-held. Idempotent — a second `Exited` while already held is ignored.
    /// Returns whether the pane NEWLY became held (the view then writes the footer
    /// once + shows the affordance).
    pub fn on_exited(&mut self, status: ExitStatus, held: bool) -> bool {
        if held && self.status.is_none() {
            self.status = Some(status);
            true
        } else {
            false
        }
    }

    /// Dismiss the held pane (the view then respawns a fresh shell — the only path
    /// that frees the held term). Returns whether it was actually held.
    pub fn dismiss(&mut self) -> bool {
        self.status.take().is_some()
    }
}

/// The dim in-buffer footer line announcing a held pane's exit — a verbatim
/// behavior port of `TabPtySession.paneExitFooter`
/// (`TabPtySession.swift:518-529`):
///
/// `\r\n` (visual gap + snap to column 0, since the cursor's column when the
/// process died is unknown) + `ESC[2m` (dim) + `[<name> exited (<status>)]` +
/// `ESC[0m` (reset) + `\r\n`.
///
/// The status text mirrors Swift's `exitCode`-optional: a normal exit prints
/// `status <code>`; a signalled exit (Swift's `nil` waitstatus) prints
/// `killed by signal`.
pub fn held_exit_footer(name: &str, status: ExitStatus) -> String {
    let status_str = match status {
        ExitStatus::Exited(code) => format!("status {code}"),
        ExitStatus::Signaled(_) => "killed by signal".to_string(),
    };
    format!("\r\n\x1b[2m[{name} exited ({status_str})]\x1b[0m\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- LaunchOverlay (T9) ---------------------------------------------------

    #[test]
    fn overlay_first_output_beats_grace_never_shows() {
        // Fast pane: first byte arrives before the grace deadline. The overlay
        // must never become visible, and a late-firing deadline can't resurrect
        // it (Validation §1 "no overlay when first output beats the grace window").
        let mut o = LaunchOverlay::new();
        assert!(o.is_pending());
        assert!(o.clear()); // OutputStarted
        assert!(!o.is_visible());
        assert!(!o.ever_visible());
        // A grace deadline that fires afterwards is a no-op.
        assert!(!o.on_grace_elapsed());
        assert!(!o.is_visible());
        assert!(!o.ever_visible());
    }

    #[test]
    fn overlay_grace_before_output_shows_then_clears() {
        // Slow pane: silent past the grace window → visible; then output clears it.
        let mut o = LaunchOverlay::new();
        assert!(o.on_grace_elapsed());
        assert!(o.is_visible());
        assert!(o.ever_visible());
        // Cleared on first output; ever_visible stays latched.
        assert!(o.clear());
        assert!(!o.is_visible());
        assert!(o.ever_visible());
    }

    #[test]
    fn overlay_never_reappears_after_clear() {
        // Once cleared it stays cleared even if the deadline fires later, and a
        // redundant clear/grace is a no-op (Validation §1 "never reappears").
        let mut o = LaunchOverlay::new();
        assert!(o.clear());
        assert!(!o.clear()); // idempotent
        assert!(!o.on_grace_elapsed()); // cannot resurrect
        // And after having been shown + cleared, a further deadline is a no-op.
        let mut o2 = LaunchOverlay::new();
        o2.on_grace_elapsed();
        o2.clear();
        assert!(!o2.on_grace_elapsed());
        assert!(!o2.is_visible());
    }

    #[test]
    fn overlay_exit_clears_pending() {
        // A pane that exits while still pending clears the overlay (Swift clears on
        // pane exit too), so a never-output pane leaves no orphan overlay.
        let mut o = LaunchOverlay::new();
        assert!(o.clear()); // driven from Exited
        assert!(!o.is_visible());
        assert!(!o.ever_visible());
    }

    #[test]
    fn overlay_reset_starts_a_fresh_launch() {
        let mut o = LaunchOverlay::new();
        o.on_grace_elapsed();
        o.clear();
        o.reset();
        assert!(o.is_pending());
        assert!(!o.is_visible());
        assert!(!o.ever_visible());
        // The fresh launch can show again.
        assert!(o.on_grace_elapsed());
        assert!(o.is_visible());
    }

    // -- HeldPane (T10) -------------------------------------------------------

    #[test]
    fn held_holds_only_when_classified_held() {
        // Non-zero exit classified held → held; clean exit not held.
        let mut h = HeldPane::new();
        assert!(!h.is_held());
        assert!(h.on_exited(ExitStatus::Exited(3), true));
        assert!(h.is_held());
        assert_eq!(h.status(), Some(ExitStatus::Exited(3)));

        let mut clean = HeldPane::new();
        assert!(!clean.on_exited(ExitStatus::Exited(0), false));
        assert!(!clean.is_held());
    }

    #[test]
    fn held_signal_exit_holds() {
        // 11 == SIGSEGV (no libc dep in this crate; the number is inert here).
        let mut h = HeldPane::new();
        assert!(h.on_exited(ExitStatus::Signaled(11), true));
        assert!(h.is_held());
        assert_eq!(h.status(), Some(ExitStatus::Signaled(11)));
    }

    #[test]
    fn held_on_exited_is_idempotent() {
        // A second Exited while already held does not re-trigger the footer write.
        let mut h = HeldPane::new();
        assert!(h.on_exited(ExitStatus::Exited(1), true));
        assert!(!h.on_exited(ExitStatus::Exited(2), true));
        assert_eq!(h.status(), Some(ExitStatus::Exited(1)));
    }

    #[test]
    fn held_dismiss_clears_once() {
        let mut h = HeldPane::new();
        h.on_exited(ExitStatus::Exited(3), true);
        assert!(h.dismiss());
        assert!(!h.is_held());
        assert!(!h.dismiss()); // already dismissed
    }

    // -- footer (T10) ---------------------------------------------------------

    #[test]
    fn footer_matches_swift_exact_bytes() {
        // Verbatim port of TabPtySession.paneExitFooter: \r\n ESC[2m [name exited
        // (status N)] ESC[0m \r\n. Independent transcription of the Swift literal.
        assert_eq!(
            held_exit_footer("Process", ExitStatus::Exited(3)),
            "\r\n\x1b[2m[Process exited (status 3)]\x1b[0m\r\n"
        );
        // Named per the Swift claude label, arbitrary code.
        assert_eq!(
            held_exit_footer("claude", ExitStatus::Exited(127)),
            "\r\n\x1b[2m[claude exited (status 127)]\x1b[0m\r\n"
        );
    }

    #[test]
    fn footer_signal_says_killed_by_signal() {
        // Swift's nil-waitstatus branch: "killed by signal", not a numeric code.
        // 9 == SIGKILL; the number is inert (a signalled exit prints no number).
        assert_eq!(
            held_exit_footer("Process", ExitStatus::Signaled(9)),
            "\r\n\x1b[2m[Process exited (killed by signal)]\x1b[0m\r\n"
        );
    }

    #[test]
    fn footer_starts_and_ends_with_crlf() {
        // The leading \r snaps to column 0 (cursor column at death is unknown) and
        // the trailing \r\n leaves the buffer on a fresh line.
        let f = held_exit_footer(HELD_FOOTER_LABEL, ExitStatus::Exited(0));
        assert!(f.starts_with("\r\n"));
        assert!(f.ends_with("\x1b[0m\r\n"));
    }
}
