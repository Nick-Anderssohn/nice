//! `TerminalSessionHandle` — the core→GPUI adapter entity.
//!
//! This is the one place the headless `nice-term-core` [`Session`] meets gpui.
//! It is a **view-independent** gpui entity: it owns the session and a single
//! task that drains the session's typed event stream ([`SessionEvent`]) and
//! re-emits it as gpui [`TerminalEvent`]s (via [`EventEmitter`]) plus a
//! `cx.notify()`. Because it is an entity, not a view, it keeps working with
//! **no view attached** — R6's title/cwd events and R7's overlay/held
//! transitions must flow while a pane is hidden (Stage 2 keeps hidden panes'
//! sessions alive), so those rides live on this entity, not on the view.
//!
//! A [`crate::view::TerminalView`] observes this entity to repaint; the entity
//! never reaches back into a view.
//!
//! ## Draining (event-driven, no idle timer)
//!
//! The session's outward events arrive on a plain `std::sync::mpsc` channel fed
//! from the session's feeder / exit-watcher threads, and its damage-wake is a
//! `Send` callback fired from the feeder thread (never under the `Term` lock).
//! Neither may touch gpui from those threads, so this entity bridges them onto
//! the gpui foreground executor via a [`DrainSignal`]: the feeder's damage-wake
//! bumps a `Send` counter **and signals** the drain; every channel send signals
//! it too (feeder events ride their trailing damage-wake; the exit-watcher's
//! `Exited`, which has none, fires an explicit [`nice_term_core::DrainWake`]).
//! One spawned foreground task parks on the signal, and each wake drains the
//! event channel to empty + observes the counter, translating both into
//! on-entity `cx.emit` / `cx.notify`.
//!
//! The signal **coalesces the drain**: the `pending` flag batches a burst of
//! output/events into exactly **one drain pass** (the park future clears the
//! flag, so the pass services the whole backlog — batching preserved). It does
//! NOT gate the wake itself: every signal wakes the parked waker and pokes the
//! main runloop, so a poke lost while the runloop is mid-cycle self-heals on the
//! next signal (see [`signal`] and `control_socket.rs`). A send racing the
//! drain-goes-idle edge is not lost — the park future re-checks the pending flag
//! after storing its waker, and the producer sets that flag before waking. At
//! true idle **nothing re-arms**: there are no signals at all, so the task is
//! parked with zero wakeups (this replaced an 8 ms poll timer that cost ~1.4% CPU
//! per session, even occluded — M3 Bug 3).
//!
//! **App-Nap safety.** The wake must reach gpui's main run loop from a pty
//! background thread even when the app is idle/occluded. macOS App Nap defers
//! *coalescable dispatch timers* indefinitely (the very reason the old poll
//! leaked while occluded and the reason a timer-based re-arm is unusable here),
//! but a **non-timer** main-queue wake is not deferred that way. So [`signal`]
//! does two things, belt-and-suspenders, exactly like R14's control-socket drain
//! (`socket_channel` in `crates/nice`): wake the parked task's `Waker` AND force
//! the main `CFRunLoop` out of its wait via `CFRunLoopWakeUp(CFRunLoopGetMain())`
//! so the foreground executor re-polls now. That runloop poke is the sole
//! CoreFoundation crossing in this crate ([`wake_main_runloop`], hand-declared,
//! process-global — NOT the objc2/AppKit present-kick crossing, which stays
//! injected from `crates/nice/src/platform`).
//!
//! **Damage → present.** A damage bump also yields `cx.notify()` **plus an
//! explicit present kick**. `cx.notify()` alone is enough for a frontmost,
//! continuously-repainting window (the self-test scenarios drive
//! `request_animation_frame`), but it **never presents while a window's
//! CVDisplayLink is stopped** (occluded) — a real pane needs the
//! `setNeedsDisplay` kick to force `displayLayer:` on the next CA commit. That
//! kick is objc2, so it is **injected** as a callback ([`set_present_kick`]) the
//! app constructs in `crates/nice/src/platform`. The kick is cloned out of the
//! entity and fired on the bare `AsyncApp` *outside* the entity update, so
//! re-entering the window handle never nests inside the entity's borrow. The
//! injected kick is itself occlusion-gated app-side (r5d,
//! `platform::present_kick`): on a VISIBLE window it no-ops (the ticking
//! display link presents the `cx.notify()` on its next tick) and it only fires
//! `setNeedsDisplay` while the window is occluded — so this drain may keep
//! invoking it at the throttled cadence without driving gpui's
//! `displayLayer:` link stop/recreate storm on visible windows.
//!
//! **Damage notify/kick throttling (fix round r5 — input-flood freeze, lever
//! 2).** Under a pty flood the drain used to notify + kick per damage delta
//! with no rate bound, keeping the window **permanently dirty** — and gpui's
//! `dispatch_key_event` force-draws a dirty window before dispatching EVERY
//! queued key (window.rs `if self.invalidator.is_dirty() { self.draw(cx) }`),
//! which the 2026-07-10 freeze sample measured as the whole-app freeze's
//! amplifier (79% of a 51 s freeze in per-cell scene builds; see
//! `element.rs`). So the drain now applies a **trailing-edge throttle**
//! ([`PRESENT_THROTTLE`]): a damage-driven notify+kick opens a quiet window;
//! damage landing inside it is **deferred** — the drain parks on a single
//! foreground timer for the remainder instead of on the [`DrainSignal`] — and
//! the pass after the timer issues the final notify+kick. The gate lives in
//! [`present_gate`] (pure, unit-tested). Contracts held by construction:
//! the ead2a6b self-heal is untouched (`DrainSignal::signal` still wakes the
//! waker AND pokes the runloop on EVERY signal — the throttle gates only the
//! notify/kick *issuance* inside the drain pass); the trailing timer ALWAYS
//! fires, so the final frame always presents (the drain never parks on the
//! signal while un-issued damage exists); and at idle no timer exists at all
//! (the M3 win stands — the timer is created only while deferring).
//!
//! [`set_present_kick`]: TerminalSessionHandle::set_present_kick
//! [`signal`]: DrainSignal::signal

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use anyhow::Result;
use gpui::{AppContext, AsyncApp, Context, Entity, EventEmitter, Task};

use nice_term_core::{
    DamageCallback, DrainWake, ExitStatus, Session, SessionEvent, SharedTerm, SpawnSpec,
};

/// The injected demand-present kick: a `setNeedsDisplay`-on-the-window callback
/// the app constructs in `crates/nice/src/platform` (the sole sanctioned objc2
/// crossing) and hands to [`TerminalSessionHandle::set_present_kick`]. It takes
/// the bare [`AsyncApp`] so it can drive the window handle from the drain task
/// *outside* any entity update. `Arc` (not `Box`) so the drain loop can clone it
/// out of the entity and call it after the update returns.
pub type PresentKick = Arc<dyn Fn(&mut AsyncApp)>;

/// The wake bridge between the pty background threads and the parked foreground
/// drain task (see the module "Draining" docs). Held by the entity's drain task
/// and — via the [`DamageCallback`] / [`DrainWake`] closures — by the session's
/// feeder and exit-watcher threads.
///
/// Concurrency: `pending` is the drain-coalescing flag ("work to drain"; the
/// park future clears it, so a burst runs in one pass). It does NOT gate the
/// wake — every [`signal`](Self::signal) wakes the waker and pokes the runloop
/// so a lost poke self-heals (see `signal`). `waker` is the parked drain task's
/// [`Waker`]. `damage` is the monotonic repaint-accounting counter (the drain
/// present-kicks only when it moves). `runloop_wake` is the App-Nap-safe
/// main-runloop poke fired on every signal (`wake_main_runloop` in production; a
/// test double in unit tests).
///
/// `wake_enabled` gates whether [`signal`](Self::signal) actually wakes the gpui
/// task, and defaults **on**. It exists solely for the mocked
/// [`gpui::TestAppContext`]: waking a gpui foreground task from a pty background
/// thread trips gpui's deterministic test scheduler (`schedule_local` must run on
/// the test thread), so the mocked-context test harness turns it off via
/// [`TerminalSessionHandle::set_event_wake_enabled`]. Production, hidden panes,
/// and the live-platform self-tests all run enabled with no wiring — a
/// window-scoped enable would wrongly starve windowless/hidden panes whose
/// title/cwd/exit events must still flow (see the module top docs), so the safe
/// default is on and only the deterministic harness opts out (a forgotten opt-out
/// panics loudly rather than silently dropping production wakes).
struct DrainSignal {
    pending: AtomicBool,
    waker: Mutex<Option<Waker>>,
    damage: AtomicU64,
    wake_enabled: AtomicBool,
    runloop_wake: Box<dyn Fn() + Send + Sync>,
}

impl DrainSignal {
    fn new(runloop_wake: impl Fn() + Send + Sync + 'static) -> Self {
        DrainSignal {
            pending: AtomicBool::new(false),
            waker: Mutex::new(None),
            damage: AtomicU64::new(0),
            wake_enabled: AtomicBool::new(true),
            runloop_wake: Box::new(runloop_wake),
        }
    }

    /// Wake the drain task (coalesced, App-Nap-safe). Called from a pty
    /// background thread on every channel send and every damage bump.
    ///
    /// `pending` still **coalesces the drain scheduling** — it flags "there is
    /// work to drain" and the park future clears it, so a backlog runs in one
    /// pass, not one pass per signal. But the waker-wake and the runloop poke
    /// below fire on **every** signal, NOT only the idle→pending edge. This is
    /// the self-healing R14 semantics (`SocketSender::post` in
    /// `crates/nice/src/control_socket.rs`, ~:807): `CFRunLoopWakeUp` only wakes
    /// a *waiting* runloop, so a poke fired while the main loop is mid-cycle is a
    /// silent no-op — and an idle/App-Nap-eligible main queue can defer the woken
    /// runnable. If only the edge poked, one such lost poke would strand
    /// `pending == true` forever and every later signal would early-return: the
    /// drain would never run again (typed chars stop echoing until an unrelated
    /// runloop event limps it forward). Re-poking on every signal lets the next
    /// signal recover a lost wake. It costs nothing at true idle: at idle there
    /// are NO signals at all (the M3 win was deleting the 8 ms poll re-arm, not
    /// the per-signal poke).
    ///
    /// `pending` is set with `Release` (a `swap`, keeping the release-sequence
    /// property the park future's `Acquire`/`AcqRel` reads rely on — the prior
    /// return value is now simply unused) so the producer's writes (the enqueued
    /// event, the damage bump) are visible to the woken drain.
    fn signal(&self) {
        // Coalesce the drain scheduling — but do NOT branch on the prior value:
        // the wake below must fire on every signal, not just the edge.
        let _ = self.pending.swap(true, Ordering::Release);
        if !self.wake_enabled.load(Ordering::Acquire) {
            // Disabled only under the mocked TestAppContext (see the struct docs):
            // set `pending` but never touch the gpui task from this background
            // thread. Never reached in production / on a live platform.
            return;
        }
        if let Some(w) = self.waker.lock().unwrap().take() {
            w.wake();
        }
        // Belt-and-suspenders App-Nap wake, fired on EVERY signal (the self-heal):
        // a coalescable timer would be deferred while idle/occluded, and a poke
        // that lands mid-cycle is a no-op; forcing the main runloop out of its
        // wait on the next signal recovers a lost poke (see the module "App-Nap
        // safety" note and `control_socket.rs`).
        (self.runloop_wake)();
    }

    /// Record output damage (repaint accounting) then wake the drain. Fired by
    /// the feeder's [`DamageCallback`] after each parsed chunk.
    fn note_damage(&self) {
        self.damage.fetch_add(1, Ordering::Release);
        self.signal();
    }
}

/// The park future the drain task awaits between passes. Resolves as soon as a
/// signal is (or already was) pending, storing the task's waker where a producer
/// thread reaches it. The double-check after storing the waker closes the
/// classic lost-wakeup race: a producer that flips `pending` between the first
/// check and the store is caught by the second check (it set the flag before it
/// woke us).
struct DrainReady {
    signal: Arc<DrainSignal>,
}

impl Future for DrainReady {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        if self.signal.pending.swap(false, Ordering::AcqRel) {
            return Poll::Ready(());
        }
        *self.signal.waker.lock().unwrap() = Some(cx.waker().clone());
        if self.signal.pending.swap(false, Ordering::AcqRel) {
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

/// Force the app's main `CFRunLoop` out of its wait so the foreground executor
/// re-polls the parked drain task NOW — immune to App-Nap timer coalescing (see
/// the module "App-Nap safety" note). Process-global (`CFRunLoopGetMain`),
/// window-independent, safe from any thread.
///
/// This is the sole CoreFoundation crossing in this crate; it is deliberately
/// NOT the injected objc2/AppKit present-kick crossing — `CFRunLoopWakeUp` needs
/// nothing window-specific, so replicating it locally (the spec's steer) is
/// leaner than threading another injected callback through every window-wiring
/// site. Mirrors `wake_main_runloop` in `crates/nice/src/platform`.
#[cfg(target_os = "macos")]
fn wake_main_runloop() {
    // CoreFoundation, hand-declared (already linked into the app via gpui); the
    // explicit `link` also pulls it into this crate's own test binary.
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRunLoopGetMain() -> *mut std::ffi::c_void;
        fn CFRunLoopWakeUp(rl: *mut std::ffi::c_void);
    }
    // SAFETY: `CFRunLoopGetMain` returns the app's main runloop (or, implausibly,
    // null, which `CFRunLoopWakeUp` tolerates as a no-op); neither takes ownership.
    unsafe {
        CFRunLoopWakeUp(CFRunLoopGetMain());
    }
}

/// Non-macOS stand-in (this crate only ships on macOS; keeps a `cargo check` on
/// another host honest). The plain `Waker` wake is the whole mechanism there.
#[cfg(not(target_os = "macos"))]
fn wake_main_runloop() {}

/// A typed event re-emitted onto the gpui side, mirroring
/// [`nice_term_core::SessionEvent`]. `#[non_exhaustive]` so a still-later cycle
/// can add variants without a breaking change — do not narrow consumers to
/// today's set.
///
/// **Terminal-stack library boundary (R13, TRANCHE-2-NOTES §4):** the OSC
/// title/cwd variants carry **plain types only** (`String`, `PathBuf`) — no
/// `nice-model` types, no Nice-specific config. The app adapts these into its
/// document; the stack never learns about tabs/panes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TerminalEvent {
    /// The child produced its first output byte (mirror of Nice's `onFirstData`
    /// — the "dismiss the Launching… overlay" signal). Fires at most once.
    OutputStarted,
    /// The child exited. `status` is the raw exit; `held` is the held-pane
    /// classification (see [`nice_term_core::should_hold_on_exit`]).
    Exited { status: ExitStatus, held: bool },
    /// OSC 0 / OSC 2 set the window/tab title (R6). The already-trimmed decoded
    /// title (mirror of [`nice_term_core::SessionEvent::TitleChanged`]). Rides
    /// this entity — not the view — so a hidden pane's title still flows to the
    /// app (R13). Plain `String`; the app maps it onto its pane/tab titles.
    TitleChanged(String),
    /// The title was reset to the terminal default (alacritty `ResetTitle`;
    /// mirror of [`nice_term_core::SessionEvent::TitleReset`]).
    TitleReset,
    /// OSC 7 reported a new working directory (R6; mirror of
    /// [`nice_term_core::SessionEvent::CwdChanged`]). Plain [`PathBuf`]; the app
    /// stashes it on its per-pane cwd.
    CwdChanged(PathBuf),
}

/// Translate a core [`SessionEvent`] to its gpui [`TerminalEvent`]. Every
/// current core variant maps (R13 wired the OSC title/cwd variants that
/// `7500e55` dropped at the `_ => None` hole); `None` is reserved for a
/// still-later `#[non_exhaustive]` variant this crate hasn't learned to
/// translate yet (dropped rather than mis-emitted on the render thread).
fn to_terminal_event(ev: SessionEvent) -> Option<TerminalEvent> {
    match ev {
        SessionEvent::OutputStarted => Some(TerminalEvent::OutputStarted),
        SessionEvent::Exited { status, held } => Some(TerminalEvent::Exited { status, held }),
        SessionEvent::TitleChanged(title) => Some(TerminalEvent::TitleChanged(title)),
        SessionEvent::TitleReset => Some(TerminalEvent::TitleReset),
        SessionEvent::CwdChanged(path) => Some(TerminalEvent::CwdChanged(path)),
        // A variant added by a still-later cycle: ignore until this crate learns
        // to translate it, rather than panicking on the render thread.
        _ => None,
    }
}

/// The core→GPUI adapter entity (see the module docs). Owns the [`Session`] and
/// the drain task; the view observes it.
pub struct TerminalSessionHandle {
    session: Session,
    /// The spec the session was spawned from, kept so the T10 dismiss affordance
    /// can [`respawn_shell`](Self::respawn_shell) a fresh login shell in the same
    /// cwd / env after a held pane is dismissed (the original may have been a
    /// one-off command that already exited).
    spec: SpawnSpec,
    /// The per-session scrollback knob, kept for [`respawn_shell`](Self::respawn_shell).
    scrollback_lines: usize,
    /// Sub-line scroll remainder, in lines. Wheel/trackpad deltas accumulate
    /// here; only whole lines are stepped into the core's line-quantized display
    /// offset, leaving the fractional part parked as the **deferred smooth-scroll
    /// seam** (roadmap open question 4 — GPUI main pixel-snaps, so scrollback is
    /// line-stepped now; the float offset lets sub-line smooth scroll land later
    /// without a rewrite). See [`take_scroll_steps`].
    scroll_accum: f32,
    /// The injected demand-present kick (see [`PresentKick`] + the module docs).
    /// `None` until the app wires a window via [`set_present_kick`]; the entity
    /// works view- and window-independent until then (Stage 2 keeps hidden
    /// panes' sessions alive, and `cx.notify()` alone drives an on-screen view).
    ///
    /// [`set_present_kick`]: TerminalSessionHandle::set_present_kick
    present_kick: Option<PresentKick>,
    /// The wake bridge the drain task parks on, shared with the session's
    /// feeder + exit-watcher threads (see [`DrainSignal`]). Held here too so
    /// [`set_event_wake_enabled`](Self::set_event_wake_enabled) can reach it (the
    /// mocked-test opt-out); re-pointed at the fresh signal on a respawn.
    drain_signal: Arc<DrainSignal>,
    /// The drain task. Held so it is cancelled when the entity drops (a dropped
    /// `Task` is cancelled), so no task outlives its session. It parks on
    /// [`drain_signal`](Self::drain_signal) between passes (event-driven, no idle
    /// timer — see the module "Draining" docs).
    _drain: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalSessionHandle {}

impl TerminalSessionHandle {
    /// Spawn a session for `spec` and wrap it in a new adapter entity.
    ///
    /// The session is spawned **eagerly** (the pane is live immediately);
    /// `scrollback_lines` is the per-session scrollback knob (pass
    /// [`nice_term_core::DEFAULT_SCROLLBACK_LINES`] for parity). Returns the
    /// entity; the caller hands it to a [`crate::view::TerminalView`] (or holds
    /// it view-detached).
    pub fn spawn(
        cx: &mut impl AppContext,
        spec: SpawnSpec,
        scrollback_lines: usize,
    ) -> Result<Entity<Self>> {
        let (session, events, signal) = spawn_signalled_session(spec.clone(), scrollback_lines)?;

        let entity = cx.new(|cx| {
            let drain_signal = Arc::clone(&signal);
            let drain = cx.spawn(async move |this, cx| {
                drain_loop(this, cx, events, signal).await;
            });
            TerminalSessionHandle {
                session,
                spec,
                scrollback_lines,
                scroll_accum: 0.0,
                present_kick: None,
                drain_signal,
                _drain: drain,
            }
        });
        Ok(entity)
    }

    /// Respawn a **fresh login shell** in place, replacing a held/exited session
    /// (T10 dismiss). This is the ONLY path that frees the held term: dropping the
    /// old [`Session`] tears down its (already-dead) child and releases its
    /// scrollback, and a brand-new `zsh -il` session takes its place — reusing the
    /// original spec's cwd + env but never its command (the held pane's command
    /// already exited; a Stage-2 tab-dissolve will own this later). The entity
    /// identity is preserved, so the view's subscriptions and the app's present
    /// kick survive; only the drain task is restarted over the fresh event stream.
    ///
    /// The fresh pty is sized to the current grid (so the shell comes up filling
    /// the window); the caller re-fits to the live viewport on its next paint.
    pub fn respawn_shell(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let (rows, cols) = self
            .session
            .dimensions()
            .unwrap_or((self.spec.rows, self.spec.cols));
        let shell_spec = SpawnSpec::shell(self.spec.cwd.clone())
            .with_env(self.spec.env.clone())
            .with_size(rows, cols);

        // Spawn the fresh session FIRST; only swap it in on success so a failed
        // respawn leaves the held pane intact (its output stays readable) rather
        // than blanking the view to a dead session.
        let (session, events, signal) =
            spawn_signalled_session(shell_spec.clone(), self.scrollback_lines)?;
        // Carry the event-wake enable state across the respawn (defaults on; a
        // mocked-test handle that opted out keeps opting out on the fresh signal).
        signal
            .wake_enabled
            .store(self.drain_signal.wake_enabled.load(Ordering::Acquire), Ordering::Release);
        self.session = session;
        // Future dismissals of this pane respawn a shell too (the command spec is
        // gone once its held pane is dismissed).
        self.spec = shell_spec;
        self.drain_signal = Arc::clone(&signal);
        self._drain = cx.spawn(async move |this, cx| {
            drain_loop(this, cx, events, signal).await;
        });
        cx.notify();
        Ok(())
    }

    /// Install the demand-present kick (see [`PresentKick`] + the module docs).
    /// The app calls this once, after its window exists, with a closure that
    /// `setNeedsDisplay`s that window's backing view (constructed in
    /// `crates/nice/src/platform`, keeping objc2 out of this crate). Replaces any
    /// prior kick — a re-parent (R13) re-points it at the new window.
    pub fn set_present_kick(&mut self, kick: impl Fn(&mut AsyncApp) + 'static) {
        self.present_kick = Some(Arc::new(kick));
    }

    /// Enable or disable the event-driven drain wake (defaults **enabled**).
    ///
    /// **Only the mocked [`gpui::TestAppContext`] test harness calls this, with
    /// `false`.** The event-driven drain wakes its parked foreground task from the
    /// pty feeder/exit-watcher background threads (App-Nap-safe; see the module
    /// docs). Under gpui's deterministic *test* scheduler that cross-thread wake
    /// trips a determinism guard (`schedule_local` must run on the test thread),
    /// so a mocked-context test — which never needs the drain (it reads the grid /
    /// capture file directly) — turns the wake off. Production, hidden/windowless
    /// panes, and the live-platform self-tests all leave it on; there is no reason
    /// to disable it there. Disable BEFORE the first `run_until_parked` so it lands
    /// before the drain task registers its waker.
    pub fn set_event_wake_enabled(&self, enabled: bool) {
        self.drain_signal
            .wake_enabled
            .store(enabled, Ordering::Release);
    }

    /// The shared `Term` the renderer locks (briefly) to read cells for a paint,
    /// or `None` if the session has not spawned. The renderer must copy cells
    /// under the lock and drop it before painting — never hold it across a
    /// present (see [`crate::element::TerminalElement`]).
    pub fn term(&self) -> Option<&SharedTerm> {
        self.session.term()
    }

    /// Take-and-clear the core's out-of-band full-damage flag (fix round r5b):
    /// `true` iff the parity VT handler mutated the grid where alacritty's
    /// damage tracking cannot see it (the in-place ED(2) erase). The element's
    /// damage-gated row cache folds `true` into a full-invalidate verdict —
    /// and must call this **while holding the `Term` lock** (see
    /// [`nice_term_core::Session::take_forced_full_damage`] for the contract).
    pub fn take_forced_full_damage(&self) -> bool {
        self.session.take_forced_full_damage()
    }

    /// The wrapped session, for callers that drive input / resize / lifecycle
    /// (a later slice; exposed now so the entity is the one owner).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Mutable access to the wrapped session (resize / close — later slices).
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Whether the wrapped session's shell has a **foreground child** running —
    /// the terminal-busy signal R20.5's close confirmation reads for a terminal
    /// pane. A thin passthrough to [`nice_term_core::Session::has_foreground_child`]
    /// (which reads `tcgetpgrp(master_fd)` inside `nice-term-core`, next to the
    /// fd it owns): only this `bool` crosses the terminal-stack boundary, never
    /// the raw fd. An unspawned session ⇒ `false` (see that method).
    pub fn has_foreground_child(&self) -> bool {
        self.session.has_foreground_child()
    }

    /// Whether the wrapped session's child has produced its first output byte yet
    /// — the latched `OutputStarted` fact, forwarded from
    /// [`nice_term_core::Session::output_started`]. The view reads it in
    /// [`TerminalView::new`](crate::view::TerminalView) to pre-clear its launch
    /// overlay when the view is built AFTER output already started (a pane spawned
    /// while its tab was inactive, first visited now): the one-shot `OutputStarted`
    /// already fired to zero subscribers, so there is no event left to replay.
    pub fn output_started(&self) -> bool {
        self.session.output_started()
    }

    /// Set a simple (non-block) selection spanning `start ..= end`, in **buffer**
    /// grid coordinates (`(line, column)`; `line` is negative for scrollback).
    ///
    /// This is the **programmatic selection setter test seam** the plan calls
    /// for: mouse selection *input* is R5, but the renderer must paint the core's
    /// selection state correctly now, so this drives that state directly. The
    /// caller should `cx.notify()` after calling it to repaint. No-op if the
    /// session has not spawned.
    pub fn set_selection(&self, start: (i32, usize), end: (i32, usize)) {
        self.set_selection_typed(SelectionType::Simple, start, end);
    }

    /// [`set_selection`](Self::set_selection) with an explicit alacritty
    /// [`SelectionType`]: `Semantic` expands both endpoints to word boundaries
    /// (the double-click gesture) and `Lines` to whole lines (triple-click).
    /// The expansion itself lives in alacritty's `Selection::to_range`, driven
    /// by the `Term`'s `semantic_escape_chars`; this just anchors the typed
    /// selection at `start` and updates it to `end`.
    pub fn set_selection_typed(
        &self,
        ty: SelectionType,
        start: (i32, usize),
        end: (i32, usize),
    ) {
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            let start_pt = Point::new(Line(start.0), Column(start.1));
            let end_pt = Point::new(Line(end.0), Column(end.1));
            let (start_side, end_side) = selection_sides(start_pt, end_pt);
            let mut sel = Selection::new(ty, start_pt, start_side);
            sel.update(end_pt, end_side);
            term.selection = Some(sel);
        }
    }

    /// Clear any active selection. Caller should `cx.notify()` to repaint.
    pub fn clear_selection(&self) {
        if let Some(term_arc) = self.session.term() {
            term_arc.lock().selection = None;
        }
    }

    /// The current selection rendered to a `String` (alacritty's
    /// `selection_to_string`), or `None` if there is no active selection / the
    /// session has not spawned. This is the ⌘C copy source (R5): the view reads
    /// it and writes it to the pasteboard via gpui's clipboard API.
    pub fn selection_text(&self) -> Option<String> {
        self.session
            .term()
            .and_then(|term_arc| term_arc.lock().selection_to_string())
    }

    /// Scroll the viewport through scrollback by `delta_lines` (**positive =
    /// toward history / older output; negative = toward the bottom / newer**).
    ///
    /// This is the wheel/trackpad path: fractional deltas accumulate in
    /// [`scroll_accum`](Self::scroll_accum) and only whole lines are stepped into
    /// the core's line-quantized display offset (the sub-line remainder stays as
    /// the deferred smooth-scroll seam — see [`take_scroll_steps`]). The core
    /// clamps the offset to `[0, history]`, so over-scroll at either end is a
    /// no-op. Caller should `cx.notify()` to repaint. No-op if not yet spawned.
    ///
    /// **Auto-snap-to-bottom is handled by the core, not here:** while parked at
    /// the bottom (offset 0) new output stays pinned to the bottom, and while
    /// scrolled up new output bumps the offset to keep the *same* content visible
    /// (alacritty's `Grid::scroll_up`). So a session at the bottom snaps on new
    /// output, and a scrolled session stays parked — no bookkeeping on this side.
    pub fn scroll_lines(&mut self, delta_lines: f32) {
        let steps = take_scroll_steps(&mut self.scroll_accum, delta_lines);
        if steps != 0 {
            if let Some(term_arc) = self.session.term() {
                term_arc.lock().scroll_display(Scroll::Delta(steps));
            }
        }
    }

    /// Jump to the bottom (newest output), discarding any sub-line remainder.
    /// Caller should `cx.notify()` to repaint. No-op if not yet spawned.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            term_arc.lock().scroll_display(Scroll::Bottom);
        }
    }

    /// The current scrollback display offset in lines (0 == parked at the bottom).
    /// Locks the `Term` briefly to read it; 0 if not yet spawned.
    pub fn display_offset(&self) -> usize {
        self.session
            .term()
            .map(|t| t.lock().grid().display_offset())
            .unwrap_or(0)
    }

    /// Whether the viewport is parked at the bottom (offset 0) — the state in
    /// which new output snaps into view. False if not yet spawned only in the
    /// sense that offset defaults to 0, i.e. this returns `true` pre-spawn.
    pub fn is_at_bottom(&self) -> bool {
        self.display_offset() == 0
    }
}

/// Endpoint sides for a simple `start` (anchor) → `end` (drag point) selection
/// such that BOTH endpoint cells are included whichever direction the drag
/// runs. alacritty's `Selection::range_simple` orders the two anchors and then
/// trims the first cell when the ordered start's side is `Right` and the last
/// when the ordered end's side is `Left` — so the earlier point must carry
/// `Side::Left` and the later `Side::Right`. The old fixed `Left`/`Right`
/// assignment got that backwards for a leftward drag, which made the dragged-to
/// (leftmost) cell impossible to select.
fn selection_sides(start: Point, end: Point) -> (Side, Side) {
    if end < start {
        (Side::Right, Side::Left)
    } else {
        (Side::Left, Side::Right)
    }
}

/// Fold a fractional line `delta` into the scroll accumulator `accum`, returning
/// the whole number of lines to step the core display by and leaving the
/// sub-line remainder in `accum`.
///
/// This is the seam that keeps line-stepped scroll exact while preserving a float
/// offset for later sub-line smooth scroll: e.g. three 0.4-line trackpad ticks
/// yield steps `0, 0, 1` with `0.2` left parked, never dropping or double-counting
/// the fractional travel. `trunc` (toward zero) is symmetric for up/down, so a
/// +0.6 then −0.6 sequence returns to exactly zero offset with an empty
/// remainder.
fn take_scroll_steps(accum: &mut f32, delta: f32) -> i32 {
    *accum += delta;
    let whole = accum.trunc();
    *accum -= whole;
    whole as i32
}

/// Build a [`DrainSignal`] and spawn `spec`'s session wired to wake it: the
/// feeder's [`DamageCallback`] bumps the repaint counter and signals; the
/// exit-watcher's `Exited` (which has no trailing damage-wake) fires the
/// [`DrainWake`]. Returns the session, its event receiver, and the signal to
/// hand the drain task. Shared by [`TerminalSessionHandle::spawn`] and
/// [`TerminalSessionHandle::respawn_shell`].
fn spawn_signalled_session(
    spec: SpawnSpec,
    scrollback_lines: usize,
) -> Result<(Session, Receiver<SessionEvent>, Arc<DrainSignal>)> {
    let signal = Arc::new(DrainSignal::new(wake_main_runloop));
    let on_damage: DamageCallback = {
        let signal = Arc::clone(&signal);
        // Non-blocking, never under the `Term` lock, never re-enters gpui —
        // honours nice-term-core's damage-wake contract: bump the repaint counter
        // and signal the drain.
        Box::new(move || signal.note_damage())
    };
    let drain_wake: DrainWake = {
        let signal = Arc::clone(&signal);
        // The exit-watcher's `Exited` wakes the same drain but bumps NO damage —
        // an exit is not new grid content (present-kick behaviour unchanged).
        Arc::new(move || signal.signal())
    };
    let (session, events) =
        Session::spawn_with_drain_wake(spec, scrollback_lines, on_damage, drain_wake)?;
    Ok((session, events, signal))
}

/// The trailing-edge throttle on damage-driven notify/present kicks (fix round
/// r5, lever 2 — see the module "Damage notify/kick throttling" docs). ~4-8 ms
/// per the freeze brief; 6 ms sits between zed's 4 ms pty-event batching
/// (`terminal.rs` `event_loop`) and a 100 Hz frame's 10 ms budget, so a
/// throttled present still lands within about a frame while leaving the window
/// clean for most of the flooded key events `dispatch_key_event` would
/// otherwise force-draw for. A lone keystroke echo pays nothing: the gate
/// issues immediately whenever the quiet window has already elapsed.
const PRESENT_THROTTLE: Duration = Duration::from_millis(6);

/// Verdict of [`present_gate`]: issue the damage notify+kick now, or defer it
/// to a trailing timer due in the returned remainder of the quiet window.
#[derive(Debug, PartialEq, Eq)]
enum PresentGate {
    Issue,
    Defer(Duration),
}

/// Decide whether a damage-driven notify+kick may issue `now`, given the
/// instant the previous one issued (`None` == never — always issue).
///
/// Pure so the throttle contract is unit-testable without gpui or wall-clock
/// sleeps: inside the quiet window it defers with the exact remainder (what the
/// trailing timer sleeps), at/after the boundary it issues. The caller
/// (`drain_loop`) holds the two hard invariants around this gate: a deferred
/// present is ALWAYS followed by a trailing timer + re-check (never parked on
/// the signal), and nothing here touches [`DrainSignal::signal`]'s per-signal
/// wake + runloop poke (the ead2a6b self-heal).
fn present_gate(now: Instant, last_issue: Option<Instant>, throttle: Duration) -> PresentGate {
    match last_issue {
        Some(prev) => {
            let since = now.saturating_duration_since(prev);
            if since < throttle {
                PresentGate::Defer(throttle - since)
            } else {
                PresentGate::Issue
            }
        }
        None => PresentGate::Issue,
    }
}

/// The drain task body: park on the [`DrainSignal`], and on each wake drain the
/// session's event channel to empty + observe the damage counter, translating
/// both onto the entity. Event-driven — **no idle timer** (M3 Bug 3): at idle
/// the task is parked with zero wakeups until a pty background thread signals.
/// The only timer that ever exists is the r5 trailing-edge throttle timer,
/// while damage is actively being deferred (see [`present_gate`] + the module
/// throttling docs). Ends when the entity is gone (any `update` returns `Err`)
/// or the session's senders are dropped (`Disconnected`).
async fn drain_loop(
    this: gpui::WeakEntity<TerminalSessionHandle>,
    cx: &mut gpui::AsyncApp,
    events: Receiver<SessionEvent>,
    signal: Arc<DrainSignal>,
) {
    let mut last_damage = 0u64;
    // Instant of the last *issued* damage notify+kick — the throttle anchor.
    // `None` until the first damage, so a session's first output presents with
    // zero added latency.
    let mut last_present: Option<Instant> = None;
    loop {
        // Drain every queued event, emitting + notifying for each. One wake
        // drains everything available — no per-event wakeups under heavy output.
        let mut disconnected = false;
        loop {
            match events.try_recv() {
                Ok(ev) => {
                    if let Some(mapped) = to_terminal_event(ev) {
                        if this
                            .update(cx, |_this, cx| {
                                cx.emit(mapped);
                                cx.notify();
                            })
                            .is_err()
                        {
                            return; // entity dropped
                        }
                    }
                }
                // No more events queued this pass.
                Err(TryRecvError::Empty) => break,
                // The session's senders live with the `Session` this entity owns,
                // so a disconnect means the session was dropped (teardown, or a
                // respawn that will restart this task over a fresh stream). Do a
                // final damage sweep, then exit — nothing more will arrive.
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        // Coalesced damage → one notify (repaint request) + one demand-present
        // kick, rate-bounded by the r5 trailing-edge throttle: inside the quiet
        // window the issuance is deferred to the trailing timer below (which
        // ALWAYS ends in a re-check, so the final frame always presents —
        // `last_damage` only advances when the notify+kick actually issues).
        // The final sweep of a disconnected session bypasses the gate: the
        // stream is over, there is no flood left to bound, and no timer may
        // outlive this task. The kick is cloned out of the entity here and
        // fired below on the bare `AsyncApp`, *outside* the update, so
        // re-entering the window handle never nests inside this entity's
        // borrow (see the module docs).
        let current = signal.damage.load(Ordering::Acquire);
        let mut trailing: Option<Duration> = None;
        if current != last_damage {
            let gate = if disconnected {
                PresentGate::Issue
            } else {
                present_gate(Instant::now(), last_present, PRESENT_THROTTLE)
            };
            match gate {
                PresentGate::Issue => {
                    last_damage = current;
                    last_present = Some(Instant::now());
                    let kick = match this.update(cx, |this, cx| {
                        cx.notify();
                        this.present_kick.clone()
                    }) {
                        Ok(k) => k,
                        Err(_) => return, // entity dropped
                    };
                    if let Some(kick) = kick {
                        (*kick)(cx);
                    }
                }
                PresentGate::Defer(remaining) => trailing = Some(remaining),
            }
        }

        if disconnected {
            return;
        }

        match trailing {
            // Un-issued damage is pending: park on the trailing timer, NOT the
            // signal, then loop — the next pass re-reads the damage counter and
            // (now outside the quiet window) issues. This is the always-fires
            // trailing edge: no damage edge can strand a deferred present,
            // because the drain never waits on a signal while one is pending.
            // Signals landing during the sleep still set `pending` + poke the
            // runloop (`DrainSignal::signal` is untouched); their work is
            // simply folded into the pass after the timer.
            Some(remaining) => cx.background_executor().timer(remaining).await,
            // Nothing deferred: park until the next event/damage signal —
            // event-driven, zero timers at idle.
            None => {
                DrainReady {
                    signal: Arc::clone(&signal),
                }
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        present_gate, take_scroll_steps, to_terminal_event, DrainReady, DrainSignal, PresentGate,
        TerminalEvent, PRESENT_THROTTLE,
    };
    use nice_term_core::{ExitStatus, SessionEvent};
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context as TaskContext, Poll, Wake, Waker};
    use std::time::{Duration, Instant};

    // ---- Drain gating (event-driven wake) -----------------------------------
    //
    // These test the pure gating logic — the pending-flag edge, the parked-waker
    // wake, and the App-Nap-safe runloop poke — with NO gpui and NO wall-clock /
    // cadence asserts (banned). "One scheduled drain" == one wake of the parked
    // task's `Waker` (and one runloop poke); "idle" == the flag stays clear and
    // the park future returns `Pending`.

    /// A `Waker` that counts how many times it was woken.
    struct CountingWaker {
        wakes: AtomicUsize,
    }
    impl CountingWaker {
        fn new() -> Arc<Self> {
            Arc::new(CountingWaker {
                wakes: AtomicUsize::new(0),
            })
        }
        fn count(&self) -> usize {
            self.wakes.load(Ordering::SeqCst)
        }
    }
    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A [`DrainSignal`] whose runloop-wake increments a shared counter, so the
    /// App-Nap belt-and-suspenders poke is observable without a real runloop.
    fn signal_with_counters() -> (Arc<DrainSignal>, Arc<AtomicUsize>) {
        let runloop = Arc::new(AtomicUsize::new(0));
        let signal = {
            let runloop = Arc::clone(&runloop);
            Arc::new(DrainSignal::new(move || {
                runloop.fetch_add(1, Ordering::SeqCst);
            }))
        };
        (signal, runloop)
    }

    /// Poll a fresh [`DrainReady`] over `signal` with `waker` once.
    fn poll_ready(signal: &Arc<DrainSignal>, waker: &Waker) -> Poll<()> {
        let mut fut = DrainReady {
            signal: Arc::clone(signal),
        };
        let mut cx = TaskContext::from_waker(waker);
        Pin::new(&mut fut).poll(&mut cx)
    }

    #[test]
    fn idle_schedules_no_work() {
        // No signal → nothing wakes, nothing pokes the runloop, and the drain
        // parks (Pending). This is the whole point of the fix: zero idle wakeups.
        let (signal, runloop) = signal_with_counters();
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        assert!(
            poll_ready(&signal, &waker).is_pending(),
            "a signal with nothing pending must park the drain"
        );
        assert_eq!(counter.count(), 0, "no wake without a signal");
        assert_eq!(runloop.load(Ordering::SeqCst), 0, "no runloop poke while idle");
    }

    #[test]
    fn one_signal_schedules_exactly_one_drain() {
        // Parked drain + one signal → exactly one waker wake + one runloop poke,
        // and the park future then resolves once (Ready) before parking again.
        let (signal, runloop) = signal_with_counters();
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        assert!(poll_ready(&signal, &waker).is_pending(), "drain parks first");

        signal.signal();
        assert_eq!(counter.count(), 1, "one signal wakes the parked drain once");
        assert_eq!(
            runloop.load(Ordering::SeqCst),
            1,
            "one signal pokes the runloop once (App-Nap belt-and-suspenders)"
        );

        assert!(
            poll_ready(&signal, &waker).is_ready(),
            "the woken drain runs exactly one pass"
        );
        assert!(
            poll_ready(&signal, &waker).is_pending(),
            "after the pass the drain parks again — no residual work"
        );
        assert_eq!(counter.count(), 1, "re-parking never re-wakes");
    }

    #[test]
    fn burst_coalesces_to_one_drain() {
        // A burst (many events/damage before the drain runs) schedules ONE drain
        // pass, not one wake per event — batching preserved, no per-event wakeups.
        let (signal, runloop) = signal_with_counters();
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        assert!(poll_ready(&signal, &waker).is_pending(), "drain parks first");

        for _ in 0..8 {
            signal.signal();
        }
        // The parked waker is *taken* on the first signal, so the drain is woken
        // exactly once for the burst — the batching that matters (one drain pass
        // per backlog) is preserved.
        assert_eq!(counter.count(), 1, "a burst wakes the parked drain exactly once");
        // The runloop poke, by contrast, fires on EVERY signal (self-healing):
        // one poke lost mid-cycle must not strand the drain, so each signal
        // re-pokes. Batching lives in `pending`/the waker, not in throttling pokes.
        assert_eq!(
            runloop.load(Ordering::SeqCst),
            8,
            "every signal re-pokes the runloop (self-heal); coalescing is the single drain pass, not fewer pokes"
        );

        // One pass clears the coalesced pending; then it parks (nothing residual).
        assert!(poll_ready(&signal, &waker).is_ready(), "one pass drains the burst");
        assert!(
            poll_ready(&signal, &waker).is_pending(),
            "the whole burst was one drain"
        );
    }

    #[test]
    fn send_during_drain_schedules_one_followup() {
        // The race edge: a signal that lands while the drain is mid-pass (after it
        // cleared pending, before it re-parks) must still produce a follow-up
        // drain — exactly one, then idle.
        let (signal, _runloop) = signal_with_counters();
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        // Drain parks, an event arrives, the drain wakes and runs a pass.
        assert!(poll_ready(&signal, &waker).is_pending());
        signal.signal();
        assert!(
            poll_ready(&signal, &waker).is_ready(),
            "the woken drain begins a pass (pending consumed)"
        );

        // A second event lands WHILE that pass is still running (before re-park).
        signal.signal();

        // Re-parking sees it and schedules exactly one follow-up pass...
        assert!(
            poll_ready(&signal, &waker).is_ready(),
            "a send during the drain is not lost — it schedules one follow-up"
        );
        // ...and nothing after that.
        assert!(
            poll_ready(&signal, &waker).is_pending(),
            "no spurious extra drain after the follow-up"
        );
    }

    #[test]
    fn disabled_signal_sets_pending_but_never_wakes() {
        // The mocked-TestAppContext opt-out: when disabled, a signal sets pending
        // (so a drain driven by other means still sees the work) but NEVER wakes
        // the gpui task or pokes the runloop — that cross-thread wake is what the
        // deterministic test scheduler forbids.
        let (signal, runloop) = signal_with_counters();
        signal.wake_enabled.store(false, Ordering::Release);
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        assert!(poll_ready(&signal, &waker).is_pending(), "drain parks first");

        signal.signal();
        assert_eq!(counter.count(), 0, "a disabled signal must not wake the task");
        assert_eq!(
            runloop.load(Ordering::SeqCst),
            0,
            "a disabled signal must not poke the runloop"
        );
        // Pending is still set, so a drain that IS polled would run a pass.
        assert!(
            poll_ready(&signal, &waker).is_ready(),
            "pending is set even while disabled"
        );
    }

    #[test]
    fn note_damage_bumps_counter_and_signals() {
        // The feeder path: note_damage records damage (repaint accounting) AND
        // wakes the drain, coalescing like any other signal.
        let (signal, runloop) = signal_with_counters();
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        assert!(poll_ready(&signal, &waker).is_pending(), "drain parks first");
        assert_eq!(signal.damage.load(Ordering::Acquire), 0);

        signal.note_damage();
        assert_eq!(
            signal.damage.load(Ordering::Acquire),
            1,
            "note_damage bumps the repaint counter"
        );
        assert_eq!(counter.count(), 1, "note_damage wakes the drain");
        assert_eq!(runloop.load(Ordering::SeqCst), 1, "note_damage pokes the runloop");
    }

    #[test]
    fn signal_repokes_when_prior_poke_was_lost() {
        // The drain-wake starvation wedge (fix/drain-wake-starvation).
        //
        // `CFRunLoopWakeUp` only wakes a *waiting* runloop: a poke fired while the
        // main loop is mid-cycle is a silent no-op, and an idle/App-Nap-eligible
        // main queue can defer the woken runnable. Model that lost poke here as a
        // parked drain (waker stored, `pending` left true by the edge signal) that
        // is NEVER re-polled — the runnable the first poke would have run does not
        // run. A *second* signal must STILL re-poke the runloop so that deferred
        // runnable gets another chance to run: the R14 control-socket self-heal
        // (`SocketSender::post` re-pokes on every call — control_socket.rs ~:807).
        //
        // PRE-FIX this hit `if pending.swap(true) { return; }` on the second
        // signal and did nothing — the runloop poke count stayed at 1 and the drain
        // wedged forever (typed chars stop echoing until an unrelated runloop event
        // limps it forward, exactly the reported freeze). The assertion below is
        // the FIXED contract: every signal with unserviced work re-pokes.
        let (signal, runloop) = signal_with_counters();
        let counter = CountingWaker::new();
        let waker = Waker::from(Arc::clone(&counter));

        // Drain parks, storing its waker; `pending` is clear.
        assert!(poll_ready(&signal, &waker).is_pending(), "drain parks first");

        // First signal — the idle→pending edge. It takes+wakes the waker and pokes
        // the runloop once. We MODEL the lost poke by NOT re-polling the park
        // future: the drain stays parked and `pending` stays stuck true.
        signal.signal();
        assert_eq!(counter.count(), 1, "the edge signal wakes the parked waker once");
        assert_eq!(runloop.load(Ordering::SeqCst), 1, "the edge signal pokes once");

        // Second signal, with `pending` still true and the drain still parked. This
        // is the wedge case. Post-fix it MUST re-poke (the self-heal); pre-fix it
        // early-returned and this stayed 1.
        signal.signal();
        assert_eq!(
            runloop.load(Ordering::SeqCst),
            2,
            "every signal with unserviced work re-pokes the runloop (self-heal); \
             pre-fix this stayed 1 and the drain wedged"
        );

        // And the invariant `pending` actually owns still holds: however many
        // signals fired, ONE drain pass services the whole coalesced backlog.
        assert!(
            poll_ready(&signal, &waker).is_ready(),
            "one pass services the coalesced backlog"
        );
        assert!(
            poll_ready(&signal, &waker).is_pending(),
            "…then the drain parks — the backlog was drained in a single pass"
        );
    }

    // ---- Present throttle (fix round r5, lever 2) ----------------------------
    //
    // These pin the pure gate the drain's notify/kick issuance runs through.
    // Synthetic `Instant`s only — no wall-clock sleeps, no cadence asserts
    // (banned above). The two loop invariants the gate relies on — a deferred
    // present is always followed by a trailing timer + re-check, and
    // `DrainSignal::signal`'s per-signal wake + runloop poke is untouched — are
    // held by `drain_loop`'s structure and the signal tests above
    // (`signal_repokes_when_prior_poke_was_lost` is the ead2a6b contract).

    #[test]
    fn first_damage_presents_immediately() {
        // No prior present → issue now: a lone keystroke echo (and a session's
        // first output) pays zero added latency.
        let now = Instant::now();
        assert_eq!(present_gate(now, None, PRESENT_THROTTLE), PresentGate::Issue);
    }

    #[test]
    fn damage_inside_the_quiet_window_defers_with_the_exact_remainder() {
        // 2 ms into a 6 ms window → defer, and the trailing timer must sleep
        // exactly the remaining 4 ms (the trailing edge lands at window end,
        // not a full window later — the throttle bounds rate, it never
        // staircases latency).
        let t0 = Instant::now();
        let now = t0 + Duration::from_millis(2);
        assert_eq!(
            present_gate(now, Some(t0), Duration::from_millis(6)),
            PresentGate::Defer(Duration::from_millis(4))
        );
    }

    #[test]
    fn damage_at_the_window_boundary_issues() {
        // The trailing timer wakes the drain at exactly `last + throttle`; the
        // re-check must issue then (`since < throttle` is strict), or a
        // boundary wake would defer forever in 0-remainder steps.
        let t0 = Instant::now();
        assert_eq!(
            present_gate(t0 + PRESENT_THROTTLE, Some(t0), PRESENT_THROTTLE),
            PresentGate::Issue
        );
        assert_eq!(
            present_gate(
                t0 + PRESENT_THROTTLE + Duration::from_millis(3),
                Some(t0),
                PRESENT_THROTTLE
            ),
            PresentGate::Issue
        );
    }

    #[test]
    fn trailing_edge_always_issues_after_a_deferral() {
        // The full deferral round-trip, as drain_loop drives it: issue at t0,
        // damage at t0+2ms defers with 4 ms remaining, the drain sleeps that
        // remainder, and the post-timer re-check at t0+6ms issues the final
        // frame. No damage sequence may end un-presented.
        let t0 = Instant::now();
        let throttle = Duration::from_millis(6);
        let deferred_at = t0 + Duration::from_millis(2);
        let remaining = match present_gate(deferred_at, Some(t0), throttle) {
            PresentGate::Defer(r) => r,
            PresentGate::Issue => panic!("damage inside the window must defer"),
        };
        assert_eq!(
            present_gate(deferred_at + remaining, Some(t0), throttle),
            PresentGate::Issue,
            "the pass after the trailing timer must issue the final present"
        );
    }

    /// A scripted core event stream — every current [`SessionEvent`] variant —
    /// must surface through the entity's translator instead of being dropped.
    /// The OSC title/cwd variants used to fall into the `_ => None` hole
    /// (`session_handle.rs` at `7500e55`); R13 maps them so a hidden pane's
    /// title/cwd still reach the app on this view-independent entity.
    #[test]
    fn scripted_core_stream_maps_every_variant_including_title_and_cwd() {
        let scripted: Vec<(SessionEvent, Option<TerminalEvent>)> = vec![
            (
                SessionEvent::OutputStarted,
                Some(TerminalEvent::OutputStarted),
            ),
            (
                SessionEvent::TitleChanged("build watcher".into()),
                Some(TerminalEvent::TitleChanged("build watcher".into())),
            ),
            (SessionEvent::TitleReset, Some(TerminalEvent::TitleReset)),
            (
                SessionEvent::CwdChanged(PathBuf::from("/tmp/proj")),
                Some(TerminalEvent::CwdChanged(PathBuf::from("/tmp/proj"))),
            ),
            (
                SessionEvent::Exited {
                    status: ExitStatus::Exited(0),
                    held: false,
                },
                Some(TerminalEvent::Exited {
                    status: ExitStatus::Exited(0),
                    held: false,
                }),
            ),
            (
                SessionEvent::Exited {
                    status: ExitStatus::Signaled(9),
                    held: true,
                },
                Some(TerminalEvent::Exited {
                    status: ExitStatus::Signaled(9),
                    held: true,
                }),
            ),
        ];

        for (core, want) in scripted {
            assert_eq!(
                to_terminal_event(core.clone()),
                want,
                "core event {core:?} must translate to {want:?}, not drop"
            );
        }
    }

    #[test]
    fn title_and_cwd_payloads_survive_translation_verbatim() {
        // The plain-typed payloads (String / PathBuf) cross the boundary
        // unchanged — no re-decoding, no app-type coercion in the stack.
        assert_eq!(
            to_terminal_event(SessionEvent::TitleChanged("Fix top bar height".into())),
            Some(TerminalEvent::TitleChanged("Fix top bar height".into()))
        );
        assert_eq!(
            to_terminal_event(SessionEvent::CwdChanged(PathBuf::from(
                "/Users/nick/Projects/nice"
            ))),
            Some(TerminalEvent::CwdChanged(PathBuf::from(
                "/Users/nick/Projects/nice"
            )))
        );
    }

    #[test]
    fn sub_line_ticks_accumulate_then_step_once() {
        // Three 0.4-line ticks: 0.4, 0.8 → no whole line yet; 1.2 → one line, and
        // the 0.2 remainder is preserved (the smooth-scroll seam), not dropped.
        let mut accum = 0.0f32;
        assert_eq!(take_scroll_steps(&mut accum, 0.4), 0);
        assert!((accum - 0.4).abs() < 1e-6);
        assert_eq!(take_scroll_steps(&mut accum, 0.4), 0);
        assert!((accum - 0.8).abs() < 1e-6);
        assert_eq!(take_scroll_steps(&mut accum, 0.4), 1);
        assert!((accum - 0.2).abs() < 1e-6);
    }

    #[test]
    fn whole_line_delta_steps_immediately_no_remainder() {
        let mut accum = 0.0f32;
        assert_eq!(take_scroll_steps(&mut accum, 3.0), 3);
        assert_eq!(accum, 0.0);
    }

    #[test]
    fn multi_line_fractional_delta_steps_floor_toward_zero() {
        // 2.7 lines → step 2 whole lines, 0.7 parked.
        let mut accum = 0.0f32;
        assert_eq!(take_scroll_steps(&mut accum, 2.7), 2);
        assert!((accum - 0.7).abs() < 1e-6);
    }

    #[test]
    fn opposite_deltas_cancel_to_zero_offset() {
        // +0.6 then −0.6 returns to exactly zero travel with an empty remainder:
        // `trunc` toward zero is symmetric, so up/down never drift.
        let mut accum = 0.0f32;
        assert_eq!(take_scroll_steps(&mut accum, 0.6), 0);
        assert_eq!(take_scroll_steps(&mut accum, -0.6), 0);
        assert!(accum.abs() < 1e-6);
    }

    #[test]
    fn negative_delta_steps_toward_bottom() {
        // Positive = into history, negative = toward the bottom: a −1.5 delta
        // steps −1 line (toward the bottom) with −0.5 parked.
        let mut accum = 0.0f32;
        assert_eq!(take_scroll_steps(&mut accum, -1.5), -1);
        assert!((accum + 0.5).abs() < 1e-6);
    }

    // ---- Selection endpoint sides (set_selection) ----------------------------
    //
    // These pin the fix for the leftward-drag bug: the resolved selection range
    // must include BOTH endpoint cells whichever direction the drag runs. The
    // end-to-end tests resolve through a real alacritty `Term`, so a change in
    // its `range_simple` side-trimming semantics fails here, not in the GUI.

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::{test::TermSize, Config, Term};

    /// Resolve a selection built exactly the way `set_selection` builds it.
    fn resolved_range(
        start: (i32, usize),
        end: (i32, usize),
    ) -> Option<(Point, Point)> {
        let term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let start_pt = Point::new(Line(start.0), Column(start.1));
        let end_pt = Point::new(Line(end.0), Column(end.1));
        let (start_side, end_side) = super::selection_sides(start_pt, end_pt);
        let mut sel = Selection::new(SelectionType::Simple, start_pt, start_side);
        sel.update(end_pt, end_side);
        sel.to_range(&term).map(|r| (r.start, r.end))
    }

    #[test]
    fn rightward_selection_includes_both_endpoints() {
        let range = resolved_range((0, 2), (0, 5)).expect("non-empty");
        assert_eq!(range, (Point::new(Line(0), Column(2)), Point::new(Line(0), Column(5))));
    }

    #[test]
    fn leftward_selection_includes_both_endpoints() {
        // The reported bug: dragging left stopped one cell short of the
        // leftmost dragged-to cell (and silently trimmed the anchor cell too).
        let range = resolved_range((0, 5), (0, 0)).expect("non-empty");
        assert_eq!(
            range,
            (Point::new(Line(0), Column(0)), Point::new(Line(0), Column(5))),
            "both the col-0 drag target and the col-5 anchor are included"
        );
    }

    #[test]
    fn upward_selection_includes_both_endpoints() {
        // Same ordering rule across lines: dragging up-left must include the
        // dragged-to cell on the earlier line.
        let range = resolved_range((3, 4), (1, 7)).expect("non-empty");
        assert_eq!(range, (Point::new(Line(1), Column(7)), Point::new(Line(3), Column(4))));
    }

    #[test]
    fn single_cell_click_drag_selects_that_cell() {
        let range = resolved_range((2, 3), (2, 3)).expect("non-empty");
        assert_eq!(range, (Point::new(Line(2), Column(3)), Point::new(Line(2), Column(3))));
    }

    // ---- Typed selections (set_selection_typed) -------------------------------
    //
    // Double-click = `Semantic` (word), triple-click = `Lines`. These resolve
    // through a real `Term` fed real content, so they pin the whole gesture
    // contract: anchoring a typed selection at the clicked cell expands to the
    // word / line, and a drag update extends at that granularity.

    use alacritty_terminal::vte::ansi::Processor;

    /// A term showing `text` on its top row.
    fn term_with(text: &str) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    /// Resolve a selection built exactly the way `set_selection_typed` builds it.
    fn resolved_range_typed(
        term: &Term<VoidListener>,
        ty: SelectionType,
        start: (i32, usize),
        end: (i32, usize),
    ) -> Option<(Point, Point)> {
        let start_pt = Point::new(Line(start.0), Column(start.1));
        let end_pt = Point::new(Line(end.0), Column(end.1));
        let (start_side, end_side) = super::selection_sides(start_pt, end_pt);
        let mut sel = Selection::new(ty, start_pt, start_side);
        sel.update(end_pt, end_side);
        sel.to_range(term).map(|r| (r.start, r.end))
    }

    #[test]
    fn double_click_selects_the_word_under_the_pointer() {
        // Click mid-"world" (col 8): the selection expands to the full word,
        // stopping at the space (a semantic-escape char) on both sides.
        let term = term_with("hello world again");
        let range = resolved_range_typed(&term, SelectionType::Semantic, (0, 8), (0, 8))
            .expect("non-empty");
        assert_eq!(range, (Point::new(Line(0), Column(6)), Point::new(Line(0), Column(10))));
    }

    #[test]
    fn semantic_drag_extends_word_by_word() {
        // Double-click in "hello", drag into "world": both words are covered
        // end to end, not just the dragged cells.
        let term = term_with("hello world again");
        let range = resolved_range_typed(&term, SelectionType::Semantic, (0, 2), (0, 8))
            .expect("non-empty");
        assert_eq!(range, (Point::new(Line(0), Column(0)), Point::new(Line(0), Column(10))));
    }

    #[test]
    fn triple_click_selects_the_whole_line() {
        // Click anywhere in the row: the selection covers the full grid line.
        let term = term_with("hello world again");
        let range = resolved_range_typed(&term, SelectionType::Lines, (0, 8), (0, 8))
            .expect("non-empty");
        assert_eq!(range, (Point::new(Line(0), Column(0)), Point::new(Line(0), Column(79))));
    }

    // ---- Scrolled-drag anchor tracks streaming content ------------------------
    //
    // Pins the fix for the scrolled-selection bug: while the viewport is scrolled
    // up and the process keeps printing, the drag anchor must stay on the clicked
    // row. The view stores the anchor's click-time *viewport row* and re-derives
    // its grid line against the current display offset each move (mirroring the
    // end point). The old code froze the grid line, so a streamed line drifted the
    // anchor one row down. This reproduces that sequence against a real `Term`.

    #[test]
    fn scrolled_streaming_drag_anchor_stays_on_clicked_row() {
        use alacritty_terminal::grid::Scroll;

        // Fill the 24-row screen and build scrollback.
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..40 {
            parser.advance(&mut term, format!("line {i}\r\n").as_bytes());
        }

        // Scroll up into history: the viewport parks at display_offset D0.
        term.scroll_display(Scroll::Delta(5));
        let d0 = term.grid().display_offset() as i32;
        assert_eq!(d0, 5, "scrolled 5 lines up");

        // The user presses at viewport row `vr` while parked at D0. The old code
        // froze the anchor as this grid line and never updated it.
        let vr: usize = 3;
        let frozen_anchor_line = vr as i32 - d0;

        // One more line streams in. Scrolled up, alacritty bumps the offset to
        // keep the same content parked, so D_now = D0 + 1 and the clicked content
        // is still shown at row `vr`.
        parser.advance(&mut term, b"streamed\r\n");
        let d_now = term.grid().display_offset() as i32;
        assert_eq!(d_now, d0 + 1, "streaming while scrolled up bumps the offset");

        // The drag's end point is re-hit-tested live, so it tracks the clicked row.
        let end_line = vr as i32 - d_now;
        // The fix re-derives the anchor the same way; it stays on the clicked row.
        let fixed_anchor_line = vr as i32 - d_now;

        // Frozen anchor: spans two rows (the clicked row plus one below it).
        let frozen = resolved_range_typed(
            &term,
            SelectionType::Simple,
            (frozen_anchor_line, 0),
            (end_line, 5),
        )
        .expect("non-empty");
        assert_ne!(
            frozen.0.line, frozen.1.line,
            "frozen grid-line anchor drifts: selection wrongly spans two rows"
        );

        // Re-derived anchor: exactly the clicked row.
        let fixed = resolved_range_typed(
            &term,
            SelectionType::Simple,
            (fixed_anchor_line, 0),
            (end_line, 5),
        )
        .expect("non-empty");
        assert_eq!(
            fixed.0.line, fixed.1.line,
            "re-derived anchor stays on the clicked row"
        );
        assert_eq!(fixed.0.line, Line(end_line));
    }
}
