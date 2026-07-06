//! `Session` — the value-owning terminal session the rest of Nice drives.
//!
//! This wraps slice 2's eager [`TermSession`] into the full pane lifecycle:
//!
//! 1. **An explicit deferred-spawn state machine.** A restored-but-never-focused
//!    pane keeps its pty spawn *deferred* until first focus. Today's Swift
//!    modelled that as a nil-able pty a consumer force-read — documented BUG A
//!    in `docs/window-chrome-architecture.md` (a deferred pane read as a `nil`
//!    live entry and silently no-op'd). The fix there was a *closed type*
//!    (`PaneClaim`); here it is an explicit [`Phase`]:
//!
//!    ```text
//!    NotSpawned{spec} ──trigger()──▶ Spawning ──▶ Live ──child exits──▶ Exited{status, held}
//!    ```
//!
//!    A caller matches on [`Session::phase`]; it never force-reads a "live but
//!    actually absent" pty. `NotSpawned` carries the [`SpawnSpec`] (on the
//!    `Session`, not the caller) so [`Session::trigger`] can bring the pane up
//!    later. `Spawning` is the transient state *inside* `trigger()` — with the
//!    synchronous `&mut self` trigger it is never externally observable, but it
//!    is modelled explicitly so the machine has no "Live yet pty is nil" state.
//!
//! 2. **A typed outward event stream** ([`SessionEvent`], delivered on the
//!    [`Receiver`] the constructor returns). Lifecycle events:
//!    [`SessionEvent::OutputStarted`] (the child's first byte — mirrors Nice's
//!    `onFirstData`, the "dismiss the Launching… overlay" signal) and
//!    [`SessionEvent::Exited`] (with the raw status and the held classification).
//!    R6 added the escape-sequence side-channels on the same stream:
//!    [`SessionEvent::TitleChanged`] / [`SessionEvent::TitleReset`] (OSC 0/2, via
//!    the `Term`'s [`EventProxy`](crate::vt::EventProxy)) and
//!    [`SessionEvent::CwdChanged`] (OSC 7, teed off the raw pty bytes in the
//!    feeder — see [`crate::osc7`]). The enum is `#[non_exhaustive]`: later
//!    stages add variants without a breaking change — do not narrow consumers.
//!
//! 3. **Held-pane classification** ([`should_hold_on_exit`], a verbatim port of
//!    `TabPtySession.shouldHoldOnExit`): a non-zero or signalled exit the user
//!    did not ask for is *held* — the [`TermSession`] (and its scrollback) is
//!    kept alive so the failed output stays readable — while a clean exit
//!    (`exit 0`) or an explicit user-initiated close is dropped. An explicit
//!    [`Session::close`] (Nice's Cmd+W / `terminatePane`) latches an intentional
//!    flag *before* the process-group kill, so the forced `SIGHUP`/`SIGKILL`
//!    never leaves a spurious "[killed by signal]" hold behind.
//!
//! ## Threading
//!
//! On top of `TermSession`'s per-session feeder thread (pty → `Term`, off the
//! render thread) and the pty's reaper thread (the sole `waitpid`), a `Session`
//! adds one **exit-watcher** thread: it blocks on the pty's [`ExitWaiter`],
//! classifies held (latching the intentional flag *at exit time*), records the
//! outcome, sends [`SessionEvent::Exited`], then fires the optional [`DrainWake`]
//! (see [`Session::spawn_with_drain_wake`]). `Exited` is the one outward event a
//! feeder-driven [`DamageCallback`] fire does NOT trail — the child is dead, so
//! no further output/damage arrives — so an event-driven consumer's drain would
//! never learn of the exit without this explicit poke. `OutputStarted` is emitted
//! from the feeder thread by wrapping the caller's [`DamageCallback`] — the wrap
//! fires the event on the first parsed chunk, then delegates, so it honours the
//! damage-wake contract (non-blocking, never under the `Term` lock). Dropping a
//! `Session` is a deliberate teardown: it latches intentional, kills the child's
//! process group, and joins the watcher, so no thread or zsh is left behind.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

use crate::pty::ExitStatus;
use crate::session::{DamageCallback, TermSession};
use crate::spawn::SpawnSpec;
use crate::vt::{GridSnapshot, SharedTerm};

/// Should a pane that just emitted `status` be **held** open (true — keep the
/// grid/scrollback readable) or **dropped** immediately (false)?
///
/// Verbatim port of `TabPtySession.shouldHoldOnExit` (behavior, not structure):
///
/// - `intentional == true` short-circuits to **not held**. The user asked to
///   close it (Cmd+W / [`Session::close`] / dropping the `Session`), so the
///   forced `SIGHUP`/`SIGKILL` must not leave a "[killed by signal]" footer.
/// - a **signalled** exit the user did not ask for is **held**: Nice did not
///   request it via the UI (it is the OS, an external `kill`, or a parent
///   hangup), so surface whatever the process printed last.
/// - a **normal** exit is held iff its code is non-zero. Clean exits (`exit 0`,
///   `/exit` from claude, `vim` save+quit) are deliberate — the user wants the
///   pane gone; non-zero exits are usually errors whose output they need.
pub fn should_hold_on_exit(status: ExitStatus, intentional: bool) -> bool {
    if intentional {
        return false;
    }
    match status {
        // No waitstatus / a signal: Nice didn't ask for it via the UI — hold.
        ExitStatus::Signaled(_) => true,
        ExitStatus::Exited(code) => code != 0,
    }
}

/// A drain-wake the exit-watcher fires after enqueuing [`SessionEvent::Exited`]
/// (installed via [`Session::spawn_with_drain_wake`]; absent for a plain
/// [`Session::spawn`] / [`Session::deferred`]).
///
/// `Exited` is the **only** outward event NOT trailed by a [`DamageCallback`]
/// fire: the feeder-sourced events (`OutputStarted` / `TitleChanged` /
/// `TitleReset` / `CwdChanged`) are each emitted while the feeder processes a read
/// chunk and are immediately followed by that chunk's damage-wake, whereas the
/// dead child produces no further output. An event-driven UI adapter that wakes
/// its drain off the damage-wake would therefore never learn of an exit; this
/// poke closes that gap.
///
/// `Send + Sync` (fired from the watcher thread; the adapter clones it into a
/// signal shared with the feeder). gpui-free — a plain callback like
/// [`DamageCallback`]; nice-term-core neither knows nor cares that the adapter
/// makes it wake a foreground task App-Nap-safely.
pub type DrainWake = Arc<dyn Fn() + Send + Sync + 'static>;

/// The externally-observable lifecycle state — the closed type a caller matches
/// on instead of force-reading a nil-able pty (designing out BUG A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Constructed with a spec but no child yet ([`Session::deferred`]). A
    /// [`Session::trigger`] moves it forward.
    NotSpawned,
    /// Transient: mid-`trigger()`, the pty is being brought up. Not externally
    /// observable under the synchronous `&mut self` trigger; present so the
    /// machine never has a "Live but pty is nil" state.
    Spawning,
    /// The child is spawned and running.
    Live,
    /// The child has exited. `held` is the [`should_hold_on_exit`] verdict; the
    /// [`TermSession`] is kept alive either way so the grid stays readable.
    Exited { status: ExitStatus, held: bool },
}

/// A typed event pushed onto the session's outward stream (the [`Receiver`] the
/// constructor hands back). `#[non_exhaustive]` so later stages can add more
/// without a breaking change — do not narrow consumers to today's variants.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionEvent {
    /// The child produced its first output byte (mirror of Nice's `onFirstData`
    /// — the "dismiss the Launching… overlay" signal). Fires at most once.
    OutputStarted,
    /// The child exited. `status` is the raw exit; `held` is the classification
    /// (see [`should_hold_on_exit`]).
    Exited { status: ExitStatus, held: bool },
    /// OSC 0 / OSC 2 set the window/tab title (R6). The string is the decoded,
    /// already-trimmed UTF-8 title (emoji/CJK/braille preserved). Feeds pane
    /// titles / tab auto-titles / Stage 3 Claude-status parsing. OSC 1
    /// (icon-title) is intentionally never surfaced — parity with SwiftTerm's
    /// handling, which drops it.
    TitleChanged(String),
    /// The title was reset to the terminal default (alacritty `ResetTitle`,
    /// reached via the title-stack `CSI 23 t` popping an empty saved title).
    TitleReset,
    /// OSC 7 reported a new working directory (R6). The path is percent-decoded
    /// from `file://<host>/<path>` and validated to be on this host; feeds cwd
    /// persistence and the file explorer in later stages.
    CwdChanged(PathBuf),
}

/// The exit facts, latched once by the watcher thread and read by [`phase`].
///
/// [`phase`]: Session::phase
#[derive(Clone, Copy)]
struct ExitOutcome {
    status: ExitStatus,
    held: bool,
}

/// Internal storage for the spawn side of the machine. The exit side lives in
/// the write-once [`Session::outcome`] cell (filled by the watcher thread), so
/// `Spawned` covers both a live child and an exited-but-held one — the
/// [`TermSession`] is kept in one place and never moved on exit.
enum State {
    NotSpawned,
    Spawning,
    Spawned(TermSession),
}

/// One pane's terminal session: the deferred-spawn state machine + the typed
/// outward event stream + held-pane classification, wrapping a [`TermSession`].
///
/// Construct with [`Session::deferred`] (lazy — trigger on first focus) or
/// [`Session::spawn`] (eager). The constructor returns the [`Receiver`] half of
/// the event stream; the `Session` owns the sending side.
pub struct Session {
    spec: SpawnSpec,
    scrollback_lines: usize,
    /// The caller's damage-wake, held until [`trigger`](Session::trigger) wraps
    /// it (for `OutputStarted`) and hands it to [`TermSession::spawn`]. `None`
    /// once consumed.
    on_damage: Option<DamageCallback>,
    events: Sender<SessionEvent>,
    /// One-shot guard so the wrapped damage-wake emits `OutputStarted` exactly
    /// once (on the first parsed chunk).
    output_started: Arc<AtomicBool>,
    /// Latched by [`close`](Session::close) / drop *before* the process-group
    /// kill so the watcher classifies the forced exit as not-held.
    intentional: Arc<AtomicBool>,
    /// Write-once exit facts, filled by the watcher; the source of truth for the
    /// `Exited` phase.
    outcome: Arc<OnceLock<ExitOutcome>>,
    /// Optional drain-wake the exit-watcher fires after sending `Exited` (see
    /// [`DrainWake`] / [`Session::spawn_with_drain_wake`]). `None` for bare/test
    /// sessions that read the [`Receiver`] synchronously.
    drain_wake: Option<DrainWake>,
    state: State,
    /// The exit-watcher thread (see the module "Threading" note). Joined on drop.
    watcher: Option<JoinHandle<()>>,
}

impl Session {
    /// Construct a **deferred** session: a spec captured but no child spawned
    /// (`Phase::NotSpawned`). The caller brings it up later with
    /// [`Session::trigger`] (on first focus). Returns the session and the
    /// [`Receiver`] end of its event stream.
    ///
    /// `scrollback_lines` is the per-session scrollback knob (pass
    /// [`crate::DEFAULT_SCROLLBACK_LINES`] for parity); `on_damage` is the
    /// renderer wake (see [`DamageCallback`]).
    pub fn deferred(
        spec: SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
    ) -> (Session, Receiver<SessionEvent>) {
        let (tx, rx) = mpsc::channel();
        let session = Session {
            spec,
            scrollback_lines,
            on_damage: Some(on_damage),
            events: tx,
            output_started: Arc::new(AtomicBool::new(false)),
            intentional: Arc::new(AtomicBool::new(false)),
            outcome: Arc::new(OnceLock::new()),
            drain_wake: None,
            state: State::NotSpawned,
            watcher: None,
        };
        (session, rx)
    }

    /// Construct and **eagerly spawn** a session (`Phase::Live` on success) —
    /// the non-deferred path (Nice's active pane). Equivalent to
    /// [`Session::deferred`] immediately followed by [`Session::trigger`].
    pub fn spawn(
        spec: SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
    ) -> io::Result<(Session, Receiver<SessionEvent>)> {
        let (mut session, rx) = Session::deferred(spec, scrollback_lines, on_damage);
        session.trigger()?;
        Ok((session, rx))
    }

    /// Like [`Session::spawn`], but the caller also supplies a [`DrainWake`] the
    /// exit-watcher fires right after enqueuing [`SessionEvent::Exited`]. The
    /// event-driven UI adapter (`nice-term-view`) needs this: it wakes its drain
    /// off the [`DamageCallback`], and `Exited` is the one event with no trailing
    /// damage-wake, so without this poke the drain would never learn of the exit
    /// (see [`DrainWake`]). Equivalent to [`Session::deferred`] + install the wake
    /// + [`Session::trigger`], so the watcher is wired with the wake in hand.
    pub fn spawn_with_drain_wake(
        spec: SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
        drain_wake: DrainWake,
    ) -> io::Result<(Session, Receiver<SessionEvent>)> {
        let (mut session, rx) = Session::deferred(spec, scrollback_lines, on_damage);
        session.drain_wake = Some(drain_wake);
        session.trigger()?;
        Ok((session, rx))
    }

    /// Fire the deferred spawn: `NotSpawned → Spawning → Live`. Brings up the
    /// pty, the feeder, and the exit watcher.
    ///
    /// **Idempotent after a successful spawn:** calling it again once the child
    /// is up (or has since exited) is a no-op `Ok(())` — it never spawns a
    /// second child. A failed spawn returns the `io::Error` and leaves the
    /// session `NotSpawned`; because the attempt consumes the damage callback,
    /// a subsequent `trigger()` returns an error — construct a fresh `Session`
    /// to retry (fork/openpty failure is catastrophic and not expected).
    pub fn trigger(&mut self) -> io::Result<()> {
        match &self.state {
            // Already up (Live or Exited) or mid-spawn — idempotent no-op.
            State::Spawned(_) | State::Spawning => return Ok(()),
            State::NotSpawned => {}
        }

        let user_cb = self.on_damage.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "damage callback already consumed by a prior failed trigger; \
                 construct a fresh Session to retry",
            )
        })?;
        self.state = State::Spawning;

        // Wrap the caller's damage-wake so the FIRST parsed chunk emits
        // OutputStarted, then delegates. The wrap runs on the feeder thread
        // after the Term lock is released (the damage-wake contract), so sending
        // on the channel here is a non-blocking signal, never re-entrant.
        let events = self.events.clone();
        let started = Arc::clone(&self.output_started);
        let wrapped: DamageCallback = Box::new(move || {
            if !started.swap(true, Ordering::SeqCst) {
                let _ = events.send(SessionEvent::OutputStarted);
            }
            user_cb();
        });

        // Hand the core a clone of our outward `Sender` so OSC 0/2 titles (via
        // the `Term`'s `EventProxy`) and OSC 7 cwd (via the feeder's byte tee)
        // land on the same stream as `OutputStarted`/`Exited`.
        let ts = match TermSession::spawn_with_events(
            &self.spec,
            self.scrollback_lines,
            wrapped,
            self.events.clone(),
        ) {
            Ok(ts) => ts,
            Err(e) => {
                self.state = State::NotSpawned;
                return Err(e);
            }
        };

        // Exit watcher: block until the child exits, latch held (reading the
        // intentional flag AT exit time), record the outcome, then emit Exited.
        let waiter = ts.exit_waiter();
        let events = self.events.clone();
        let intentional = Arc::clone(&self.intentional);
        let outcome = Arc::clone(&self.outcome);
        let drain_wake = self.drain_wake.clone();
        let watcher = match std::thread::Builder::new()
            .name("nice-term-exit-watch".to_string())
            .spawn(move || {
                let status = waiter.wait();
                let held = should_hold_on_exit(status, intentional.load(Ordering::SeqCst));
                // Write-once: the first (and only) exit wins.
                let _ = outcome.set(ExitOutcome { status, held });
                let _ = events.send(SessionEvent::Exited { status, held });
                // Poke the event-driven consumer's drain: `Exited` is the one
                // outward event with no trailing damage-wake, so the drain would
                // otherwise never learn the child exited. Fired AFTER the send so
                // the drain observes the event once woken. No-op when unset.
                if let Some(wake) = &drain_wake {
                    wake();
                }
            }) {
            Ok(h) => h,
            Err(e) => {
                // `ts` drops here → its teardown kills the child and joins the
                // feeder, so a failed watcher spawn leaks nothing.
                self.state = State::NotSpawned;
                return Err(io::Error::from(e));
            }
        };

        self.state = State::Spawned(ts);
        self.watcher = Some(watcher);
        Ok(())
    }

    /// Explicitly close the session — Nice's Cmd+W / `terminatePane`. Latches
    /// the intentional flag *before* killing the child's process group, so the
    /// resulting exit classifies as **not held** regardless of how the child
    /// dies. Idempotent and safe on a `NotSpawned` or already-exited session.
    pub fn close(&mut self) {
        self.intentional.store(true, Ordering::SeqCst);
        if let State::Spawned(ts) = &self.state {
            ts.teardown();
        }
    }

    /// The current lifecycle [`Phase`]. `Live`/`Exited` are distinguished by the
    /// watcher-latched outcome, so once the `Exited` event has been delivered
    /// this reports `Exited` consistently.
    pub fn phase(&self) -> Phase {
        match &self.state {
            State::NotSpawned => Phase::NotSpawned,
            State::Spawning => Phase::Spawning,
            State::Spawned(_) => match self.outcome.get() {
                Some(o) => Phase::Exited {
                    status: o.status,
                    held: o.held,
                },
                None => Phase::Live,
            },
        }
    }

    /// Whether the session is in a held-exited state (exited, and
    /// [`should_hold_on_exit`] said hold).
    pub fn is_held(&self) -> bool {
        matches!(self.phase(), Phase::Exited { held: true, .. })
    }

    /// Whether the child has produced its first output byte yet — the latched
    /// `OutputStarted` fact (`true` once the first parsed chunk fired the wrapped
    /// damage-wake, and thus the one-shot [`SessionEvent::OutputStarted`]). A view
    /// built AFTER this latched — a pane spawned while its tab was inactive and
    /// first visited later, whose one-shot event fired to zero subscribers — reads
    /// it to start its launch overlay already-cleared instead of arming the grace.
    pub fn output_started(&self) -> bool {
        self.output_started.load(Ordering::SeqCst)
    }

    /// The child's pid while spawned (== its process-group id), else `None`.
    pub fn child_pid(&self) -> Option<libc::pid_t> {
        self.session().map(|ts| ts.child_pid())
    }

    /// The recorded exit status if the child has exited, else `None`. (`held`
    /// is not part of this — read [`Session::phase`] / [`Session::is_held`].)
    pub fn try_status(&self) -> Option<ExitStatus> {
        self.session().and_then(|ts| ts.try_status())
    }

    /// Write raw input bytes to the child (keystrokes, pastes). Errors if the
    /// session has not been spawned yet.
    pub fn write_input(&self, data: &[u8]) -> io::Result<()> {
        match self.session() {
            Some(ts) => ts.write_input(data),
            None => Err(not_spawned()),
        }
    }

    /// Resize both the `Term` grid and the pty. Errors if not yet spawned.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        match self.session() {
            Some(ts) => ts.resize(rows, cols),
            None => Err(not_spawned()),
        }
    }

    /// Current `(rows, cols)` grid dimensions, or `None` if not yet spawned.
    pub fn dimensions(&self) -> Option<(u16, u16)> {
        self.session().map(|ts| ts.dimensions())
    }

    /// An owned snapshot of the visible viewport, or `None` if not yet spawned.
    /// Held exits keep the `Term` alive, so this stays readable after exit.
    pub fn visible_snapshot(&self) -> Option<GridSnapshot> {
        self.session().map(|ts| ts.visible_snapshot())
    }

    /// Every buffer line (scrollback + visible) as owned strings; empty if not
    /// yet spawned. Readable after a held exit.
    pub fn grid_lines(&self) -> Vec<String> {
        self.session().map(|ts| ts.grid_lines()).unwrap_or_default()
    }

    /// Whether `needle` appears on any buffer line (scrollback included). False
    /// if not yet spawned. Readable after a held exit.
    pub fn grid_contains(&self, needle: &str) -> bool {
        self.session().is_some_and(|ts| ts.grid_contains(needle))
    }

    /// Whether bracketed-paste mode (DECSET 2004) is currently enabled in the
    /// VT — the synchronous query the paste (R5) and drop (R7) paths consult
    /// before deciding whether to frame pasted text in `ESC[200~`…`ESC[201~`.
    /// `false` before the session is spawned. Reads alacritty's tracked terminal
    /// mode under a brief `Term` lock.
    pub fn bracketed_paste_active(&self) -> bool {
        self.session()
            .is_some_and(|ts| ts.bracketed_paste_active())
    }

    /// The shared `Term` the renderer (R4) locks to paint, or `None` if not yet
    /// spawned.
    pub fn term(&self) -> Option<&SharedTerm> {
        self.session().map(|ts| ts.term())
    }

    /// The configured per-session scrollback limit (lines).
    pub fn scrollback_limit(&self) -> usize {
        self.scrollback_lines
    }

    /// Current scrollback history depth, or `None` if not yet spawned.
    pub fn history_lines(&self) -> Option<usize> {
        self.session().map(|ts| ts.history_lines())
    }

    /// The live/held `TermSession`, or `None` in `NotSpawned`/`Spawning`.
    fn session(&self) -> Option<&TermSession> {
        match &self.state {
            State::Spawned(ts) => Some(ts),
            State::NotSpawned | State::Spawning => None,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Dropping the session is a deliberate teardown (like Nice's Cmd+W /
        // terminatePane): latch intentional so the forced process-group kill
        // classifies as not-held, then kill the child so the exit watcher
        // unblocks, and join the watcher so no detached thread lingers.
        self.intentional.store(true, Ordering::SeqCst);
        if let State::Spawned(ts) = &self.state {
            ts.teardown();
        }
        if let Some(h) = self.watcher.take() {
            let _ = h.join();
        }
        // `state` drops after this body: TermSession::drop tears down again
        // (idempotent) and joins the feeder before the pty master fd closes.
    }
}

/// The error returned by pty-facing methods called before the session is
/// spawned — a typed refusal, never a force-read of an absent pty.
fn not_spawned() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "session not spawned")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_hold_matches_swift_matrix() {
        // Clean exit → drop. (`exit 0`, `/exit`, vim save+quit.)
        assert!(!should_hold_on_exit(ExitStatus::Exited(0), false));
        // Non-zero exit the user didn't ask for → hold.
        assert!(should_hold_on_exit(ExitStatus::Exited(3), false));
        // Signalled exit the user didn't ask for → hold.
        assert!(should_hold_on_exit(ExitStatus::Signaled(libc::SIGSEGV), false));
        // Intentional close short-circuits BOTH held-worthy exits → not held.
        assert!(!should_hold_on_exit(ExitStatus::Exited(3), true));
        assert!(!should_hold_on_exit(
            ExitStatus::Signaled(libc::SIGHUP),
            true
        ));
        // Intentional + clean is trivially not held too.
        assert!(!should_hold_on_exit(ExitStatus::Exited(0), true));
    }
}
