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

// `Dimensions` brings `Term::screen_lines()` into scope for the half-page delta.
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;
use alacritty_terminal::Term;
use anyhow::Result;
use gpui::{AppContext, AsyncApp, Context, Entity, EventEmitter, Task};

use crate::hyperlink::hyperlink_at_point;
use crate::search::{
    flip, run_search, step_origin, viewport_matches_in, SearchState, MAX_VIEWPORT_MATCHES,
};

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
    /// The pane asked for the scrollback search field (Phase 3, P2): in-mode
    /// `/` (`backward: false`) or `?` (`backward: true`), pressed while copy
    /// mode is on.
    ///
    /// The query UI lives in the **app** crate (this crate has no `nice-model`
    /// dependency, and the text-field precedent is `inline_rename`), so the view
    /// cannot open it directly — it emits this instead and the app routes it
    /// pane-keyed like every other terminal event. **Not** a core
    /// [`SessionEvent`] mirror: nothing in `nice-term-core` produces it, so
    /// [`to_terminal_event`] never returns it.
    SearchRequested { backward: bool },
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
    /// The copy-mode selection anchor (Phase 3, P5): the point a `v`/`V`/`⌃v`
    /// selection was started from, plus the kind it was started as.
    ///
    /// alacritty owns the `Selection` itself (and rotates it with the grid), but
    /// does **not** expose its anchor — and P5's toggle needs it twice: pressing
    /// the SAME kind again clears the selection, while a DIFFERENT kind rebuilds
    /// it from the same anchor at the new granularity. `None` whenever no
    /// copy-mode selection is live.
    copy_anchor: Option<(Point, SelectionType)>,
    /// The per-pane scrollback-search sub-state (Phase 3, P1): query, lazily
    /// compiled matcher, confirmed direction, active match. It has no alacritty
    /// equivalent (`Term::search_next` is a pure query), so it lives on this
    /// entity — surviving view unmounts, dying with the pane, never persisted.
    search: SearchState,
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
                copy_anchor: None,
                search: SearchState::default(),
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
        // The old `Term` (and its scrollback) is gone, so any copy-mode state
        // pointing into it is stale: a respawned pane starts out of copy mode
        // with no anchor and no search.
        self.copy_anchor = None;
        self.search.clear();
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

    /// Begin a drag selection of type `ty` anchored at `point` (buffer grid
    /// coordinates, like [`set_selection`](Self::set_selection)). The `Term`
    /// owns the `Selection` from here on: alacritty rotates it with the grid as
    /// output streams (`Term::scroll_up` → `Selection::rotate`), so the anchor
    /// stays glued to the clicked content with no bookkeeping in the view —
    /// including before the first [`extend_selection`](Self::extend_selection),
    /// while the selection is still zero-length (a freshly pressed anchor must
    /// rotate too; kitty shipped that bug separately from the drag one).
    ///
    /// A `Simple` selection starts empty (`is_empty`: same point, same side),
    /// so a single click paints nothing until the drag moves — which also
    /// collapses any previous highlight, replacing the old clear-on-click.
    /// `Semantic`/`Lines` paint the word/line immediately via `to_range`
    /// expansion. Caller should `cx.notify()`.
    pub fn start_selection(&self, ty: SelectionType, point: (i32, usize)) {
        if let Some(term_arc) = self.session.term() {
            drag_selection_start(
                &mut term_arc.lock(),
                ty,
                Point::new(Line(point.0), Column(point.1)),
            );
        }
    }

    /// Move the drag END of the live selection to `point`, leaving the anchor
    /// untouched — the other half of the invariant every terminal converges on
    /// (anchor content-locked, end screen-locked; see
    /// [`start_selection`](Self::start_selection) and
    /// `docs/plans/selection-scroll-anchor.md`). The caller re-derives `point`
    /// from the pointer against the *current* display offset on every mouse
    /// move and every mid-drag wheel step; the anchor needs no algebra at all.
    ///
    /// Returns `false` when there is no live selection to extend — the `Term`
    /// dropped it (a clear/erase intersecting it, a column resize, or the whole
    /// selection rotating out of scrollback) — so the caller can end the drag.
    /// Caller should `cx.notify()` on `true`.
    pub fn extend_selection(&self, point: (i32, usize)) -> bool {
        match self.session.term() {
            Some(term_arc) => drag_selection_extend(
                &mut term_arc.lock(),
                Point::new(Line(point.0), Column(point.1)),
            ),
            None => false,
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

    /// Jump to the top (oldest scrollback line), discarding any sub-line
    /// remainder (Phase 0: Shift+Home). Caller should `cx.notify()` to repaint.
    /// No-op if not yet spawned.
    pub fn scroll_to_top(&mut self) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            term_arc.lock().scroll_display(Scroll::Top);
        }
    }

    /// Page the viewport one screen toward history (Phase 0: Shift+PageUp) —
    /// the core derives the page size from the current grid height and clamps
    /// at the history end. Discards any sub-line wheel remainder. Caller should
    /// `cx.notify()` to repaint. No-op if not yet spawned.
    pub fn scroll_page_up(&mut self) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            term_arc.lock().scroll_display(Scroll::PageUp);
        }
    }

    /// Page the viewport one screen toward the bottom (Phase 0:
    /// Shift+PageDown); clamps at offset 0. Discards any sub-line wheel
    /// remainder. Caller should `cx.notify()` to repaint. No-op if not yet
    /// spawned.
    pub fn scroll_page_down(&mut self) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            term_arc.lock().scroll_display(Scroll::PageDown);
        }
    }

    /// Scroll the viewport half a screen toward history (Phase 1: ⌃⌘↑) — tmux
    /// copy-mode `halfpage-up`. `alacritty_terminal::grid::Scroll` has no
    /// half-page variant at this pin, so the delta is computed from the live grid
    /// height by [`half_page_lines`]. Discards any sub-line wheel remainder like
    /// the other jump methods. Caller should `cx.notify()` to repaint. No-op if
    /// not yet spawned, so a held (pre-spawn) session is safe.
    pub fn scroll_half_page_up(&mut self) {
        self.scroll_half_page(true);
    }

    /// Scroll the viewport half a screen toward the bottom (Phase 1: ⌃⌘↓); clamps
    /// at offset 0. The [`scroll_half_page_up`](Self::scroll_half_page_up)
    /// counterpart.
    pub fn scroll_half_page_down(&mut self) {
        self.scroll_half_page(false);
    }

    /// Shared half-page body — the delta (magnitude AND sign) comes from the pure
    /// [`half_page_delta`]. The core clamps the resulting offset to
    /// `[0, history]`, so over-scroll at either end is a no-op.
    fn scroll_half_page(&mut self, toward_history: bool) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            let delta = half_page_delta(term.screen_lines(), toward_history);
            term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Whether the terminal is on the alternate screen (vim, less, any full-screen
    /// TUI). The Phase 1 half-page chords no-op there: they are keymap bindings, so
    /// they never encode to the pty and there is nothing to fall through TO — and
    /// the alt screen has no scrollback to page through. `false` before the session
    /// spawns.
    pub fn is_alt_screen(&self) -> bool {
        self.session
            .term()
            // `Term::mode()` returns a `&TermMode`; read it out under the brief
            // lock and drop the guard (the `current_mode` pattern in `view.rs`).
            .map(|term_arc| term_arc.lock().mode().contains(TermMode::ALT_SCREEN))
            .unwrap_or(false)
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

    // ---- Copy mode (Phase 3) --------------------------------------------------
    //
    // Copy mode IS `TermMode::VI` (P1) — there is no second Nice-side flag to
    // drift out of sync, and every motion is alacritty's. This block is the API
    // over the term lock; the key table that drives it is `input.rs`, the query
    // UI is the app crate's search bar.
    //
    // **Lock discipline** (the `is_alt_screen` / scroll-block rule): alacritty's
    // `FairMutex` is NOT reentrant, so a method either takes the lock once and
    // inlines everything it needs, or calls siblings that each take their own —
    // never a sibling call while holding the lock.

    /// Whether this pane is in copy mode — i.e. the `Term` has `TermMode::VI`
    /// set. `false` before the session spawns. The single source of truth: every
    /// gate (key interception, mouse-report suspension, the badge) reads it.
    pub fn copy_mode_active(&self) -> bool {
        self.session
            .term()
            .map(|term_arc| term_arc.lock().mode().contains(TermMode::VI))
            .unwrap_or(false)
    }

    /// Enter copy mode, seeding the vi cursor at the terminal cursor (or the
    /// viewport's top-left when the terminal cursor is scrolled out of view) —
    /// alacritty's `toggle_vi_mode` behaviour, P6's "entry seeds the cursor".
    /// Already in copy mode ⇒ no-op (so a re-entry never re-seeds the cursor out
    /// from under the user). Caller should `cx.notify()`.
    pub fn enter_copy_mode(&mut self) {
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            if !term.mode().contains(TermMode::VI) {
                term.toggle_vi_mode();
            }
        }
    }

    /// Leave copy mode, returning the pane to live output (P6): clear the
    /// selection → clear the search → scroll to the bottom → flip VI off. tmux's
    /// exit-returns-you-to-live behaviour; the search state is dropped so a later
    /// entry never inherits a stale query or a stale active match.
    ///
    /// Safe to call when not in copy mode (it still parks the viewport at the
    /// bottom and clears any leftovers). Caller should `cx.notify()`.
    pub fn exit_copy_mode(&mut self) {
        // Search state is ours, not the Term's — clear it outside the lock.
        self.search.clear();
        self.scroll_accum = 0.0;
        self.copy_anchor = None;
        if let Some(term_arc) = self.session.term() {
            exit_copy_mode_in(&mut term_arc.lock());
        }
    }

    /// Toggle copy mode, returning whether it is active afterwards — the
    /// `⌃⌘c` action's behaviour (D2), in one call instead of a read plus a write.
    pub fn toggle_copy_mode(&mut self) -> bool {
        if self.copy_mode_active() {
            self.exit_copy_mode();
            false
        } else {
            self.enter_copy_mode();
            true
        }
    }

    /// Move the vi cursor (D3's `hjkl` / `w b e` / `0 $ ^` / `H M L` / `%` /
    /// `{ }` — every [`ViMotion`] the library models). No-op unless copy mode is
    /// on (alacritty enforces that itself), and a live selection follows the
    /// cursor for free: `vi_motion` calls the library's
    /// `vi_mode_recompute_selection`, which is the same `update` + `include_all`
    /// idiom the mouse drag path uses. Caller should `cx.notify()`.
    pub fn vi_motion(&mut self, motion: ViMotion) {
        if let Some(term_arc) = self.session.term() {
            term_arc.lock().vi_motion(motion);
        }
    }

    /// Jump the vi cursor to a **buffer** point, scrolling it into view first
    /// (the mouse-click-in-copy-mode path, P10). No-op unless copy mode is on.
    pub fn vi_goto(&mut self, point: Point) {
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            if term.mode().contains(TermMode::VI) {
                term.vi_goto_point(point);
            }
        }
    }

    /// The vi cursor's current **buffer** point (negative line = scrollback), or
    /// `None` if the session has not spawned. Meaningful only while copy mode is
    /// on; the render path reads the cursor through alacritty's
    /// `RenderableCursor` instead.
    pub fn vi_cursor_point(&self) -> Option<Point> {
        self.session
            .term()
            .map(|term_arc| term_arc.lock().vi_mode_cursor.point)
    }

    /// Page the viewport in copy mode, dragging the vi cursor along so it keeps
    /// its row on screen — `⌃u`/`⌃d` (`half`) and `⌃f`/`⌃b` (full page), D3.
    ///
    /// BOTH halves are needed: `ViModeCursor::scroll` only *computes* the target
    /// cursor point (it is `#[must_use]` and touches no viewport), and
    /// `scroll_display` only moves the viewport (clamping the cursor into it).
    /// Moving the cursor first and scrolling second means the clamp is a no-op
    /// except at the buffer ends, where it is exactly the right correction.
    /// No-op unless copy mode is on. Caller should `cx.notify()`.
    pub fn vi_page(&mut self, toward_history: bool, half: bool) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            vi_page_in(&mut term_arc.lock(), toward_history, half);
        }
    }

    /// `g` — jump the vi cursor to the top of the scrollback (the oldest line
    /// still in history). No-op unless copy mode is on.
    pub fn vi_top(&mut self) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            if term.mode().contains(TermMode::VI) {
                let point = Point::new(term.topmost_line(), Column(0));
                term.vi_goto_point(point);
            }
        }
    }

    /// `G` — jump the vi cursor to the terminal cursor's line, i.e. the newest
    /// output. No-op unless copy mode is on.
    pub fn vi_bottom(&mut self) {
        self.scroll_accum = 0.0;
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            if term.mode().contains(TermMode::VI) {
                let line = term.grid().cursor.point.line;
                term.vi_goto_point(Point::new(line, Column(0)));
            }
        }
    }

    /// `v` / `V` / `⌃v` — toggle a copy-mode selection of `ty` at the vi cursor
    /// (P5, vim's rules):
    ///
    /// * no live selection ⇒ start one at the cursor (one cell / line / block),
    /// * the SAME kind again ⇒ clear it,
    /// * a DIFFERENT kind ⇒ rebuild from the **same anchor** at the new
    ///   granularity.
    ///
    /// Extension is not this method's job: once a selection is live, every
    /// [`vi_motion`](Self::vi_motion) extends it through alacritty's own
    /// recompute. No-op unless copy mode is on. Caller should `cx.notify()`.
    pub fn toggle_copy_selection(&mut self, ty: SelectionType) {
        if let Some(term_arc) = self.session.term().cloned() {
            toggle_copy_selection_in(&mut term_arc.lock(), &mut self.copy_anchor, ty);
        }
    }

    /// `y` (and in-mode Enter / ⌘C) — the selected text, or `None` when nothing
    /// is selected. The copy-mode name for
    /// [`selection_text`](Self::selection_text); whether the caller then stays in
    /// the mode (⌘C) or leaves it (`y`, Enter) is P4's call, not this method's.
    pub fn yank(&self) -> Option<String> {
        self.selection_text()
    }

    // ---- Scrollback search (Phase 3, P7/P8) -----------------------------------

    /// Start a fresh search in `backward` (history-ward, `⌃⌘/` and in-mode `?`)
    /// or forward (in-mode `/`) direction, dropping any previous query. The app's
    /// search bar calls this as it opens.
    pub fn begin_search(&mut self, backward: bool) {
        self.search.restart(if backward {
            Direction::Left
        } else {
            Direction::Right
        });
    }

    /// Push the field's current text down as the live query (P8: highlights are
    /// incremental, so this runs on every keystroke). The regex is compiled
    /// lazily on first use and an invalid pattern simply matches nothing — a
    /// half-typed `(foo` must never surface an error. Caller should `cx.notify()`.
    pub fn set_search_query(&mut self, query: &str) {
        self.search.set_query(query);
    }

    /// Enter in the search field: jump to the nearest match and focus it,
    /// returning whether one was found (P7).
    ///
    /// The origin is the **raw** vi cursor — a match under the cursor counts,
    /// which is what "confirm what you are already looking at" means. `n`/`N`
    /// deliberately do the opposite (see [`next_match`](Self::next_match)).
    /// Searching is whole-buffer, so it wraps at the ends.
    pub fn confirm_search(&mut self) -> bool {
        let Some(term_arc) = self.session.term().cloned() else {
            return false;
        };
        let direction = self.search.direction();
        let mut term = term_arc.lock();
        let origin = term.vi_mode_cursor.point;
        let found = self
            .search
            .with_regex(|regex| run_search(&mut term, regex, origin, direction))
            .flatten();
        drop(term);
        match found {
            Some(m) => {
                self.search.set_active_match(m);
                true
            }
            None => false,
        }
    }

    /// `n` — the next match in the confirmed direction. Returns whether one was
    /// found. Caller should `cx.notify()`.
    pub fn next_match(&mut self) -> bool {
        self.search_step(self.search.direction())
    }

    /// `N` — the next match against the confirmed direction (P7: `N` reverses
    /// the travel without changing the search's direction).
    pub fn prev_match(&mut self) -> bool {
        self.search_step(flip(self.search.direction()))
    }

    /// Shared `n`/`N` body. The origin is advanced one cell off the active match
    /// (see [`step_origin`]) — `search_next` accepts a match *at* its origin, so
    /// stepping from the cursor's own cell would return the active match forever.
    fn search_step(&mut self, direction: Direction) -> bool {
        let Some(term_arc) = self.session.term().cloned() else {
            return false;
        };
        let mut term = term_arc.lock();
        let origin = step_origin(
            &*term,
            self.search.active_match(),
            term.vi_mode_cursor.point,
            direction,
        );
        let found = self
            .search
            .with_regex(|regex| run_search(&mut term, regex, origin, direction))
            .flatten();
        drop(term);
        match found {
            Some(m) => {
                self.search.set_active_match(m);
                true
            }
            None => false,
        }
    }

    /// Drop the query, its matcher and the active match, leaving copy mode and
    /// the cursor exactly where they are (the search-bar Esc path, P7).
    pub fn clear_search(&mut self) {
        self.search.clear();
    }

    /// Whether a search is live — a non-empty query, whether or not it matches
    /// anything. Drives the badge and the render path's highlight channel.
    pub fn search_active(&self) -> bool {
        self.search.is_active()
    }

    /// The live query for the badge, or `None` when no search is running.
    pub fn active_search_query(&self) -> Option<&str> {
        self.search.is_active().then(|| self.search.query())
    }

    /// The focused match (the one the cursor was last jumped to), if any — the
    /// render path paints it with the emphasis tint (P8).
    pub fn active_match(&self) -> Option<Match> {
        self.search.active_match().cloned()
    }

    /// Every match inside the viewport grown by `margin` rows, capped at
    /// [`MAX_VIEWPORT_MATCHES`](crate::search::MAX_VIEWPORT_MATCHES).
    ///
    /// Recomputed per frame by design (P8): the grid rotates under a streaming
    /// pane, so any cached match set would go stale — a viewport-bounded rescan
    /// is cheaper than the invalidation bookkeeping that would avoid it. Empty
    /// when no search is live, the query does not compile, or the session has not
    /// spawned.
    pub fn viewport_matches(&self, margin: usize) -> Vec<Match> {
        let Some(term_arc) = self.session.term() else {
            return Vec::new();
        };
        let term = term_arc.lock();
        self.search
            .with_regex(|regex| viewport_matches_in(&term, regex, margin, MAX_VIEWPORT_MATCHES))
            .unwrap_or_default()
    }

    /// The hyperlink under the given **buffer** cell (`buffer_line` is negative
    /// for scrollback), if any: the URL text with trailing punctuation trimmed,
    /// plus the trimmed match range in buffer coordinates for underline painting.
    /// `None` when the session has not spawned or no URL covers that cell.
    ///
    /// `regex` is the caller's cached, compiled matcher (see
    /// [`crate::hyperlink::UrlRegexCache`]) — matching needs `&mut RegexSearch`,
    /// and this runs on every ⌘-held mouse-move, so recompiling per call is not
    /// an option.
    ///
    /// Locks the `Term` **once**, for the search only, and releases it before
    /// returning — the same brief-lock discipline as every other method here. The
    /// caller must never hold it across a paint or a URL open.
    pub fn hyperlink_at(
        &self,
        buffer_line: i32,
        col: usize,
        regex: &mut RegexSearch,
    ) -> Option<(String, Match)> {
        let term_arc = self.session.term()?;
        let point = Point::new(Line(buffer_line), Column(col));
        let term = term_arc.lock();
        hyperlink_at_point(&term, point, regex)
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

/// Core of [`TerminalSessionHandle::start_selection`], generic over the `Term`
/// listener so the tests drive the production mutation against a
/// `Term<VoidListener>` instead of a mimic that could drift.
fn drag_selection_start<L: alacritty_terminal::event::EventListener>(
    term: &mut alacritty_terminal::Term<L>,
    ty: SelectionType,
    pt: Point,
) {
    // The side is provisional: `drag_selection_extend`'s `include_all` rewrites
    // both sides on every drag step, and a zero-length selection has no sides
    // to render.
    term.selection = Some(Selection::new(ty, pt, Side::Left));
}

/// Core of [`TerminalSessionHandle::extend_selection`]. `update` rewrites only
/// the selection's end anchor; `include_all` then recomputes BOTH endpoint
/// sides from the drag direction — its non-`Block` arms are the same
/// comparison as [`selection_sides`], so the leftward-drag inclusion rule
/// (BUGS.md #11) is preserved without ever rebuilding the content-locked
/// start anchor.
fn drag_selection_extend<L: alacritty_terminal::event::EventListener>(
    term: &mut alacritty_terminal::Term<L>,
    pt: Point,
) -> bool {
    match term.selection.as_mut() {
        Some(sel) => {
            // This side is immediately overwritten by `include_all`.
            sel.update(pt, Side::Right);
            sel.include_all();
            true
        }
        None => false,
    }
}

/// Core of [`TerminalSessionHandle::exit_copy_mode`] — P6's ordering, generic
/// over the `Term` listener so the tests observe the production mutation on a
/// real `Term<VoidListener>`.
///
/// Selection first, then the viewport, then the mode bit: `scroll_display`
/// recomputes a live selection's end while VI is still set, so clearing the
/// selection first keeps the exit from dragging a doomed selection down the
/// buffer on its way out. The search half is the handle's (it is Nice state,
/// not the `Term`'s) and is cleared before this runs.
fn exit_copy_mode_in<L: EventListener>(term: &mut Term<L>) {
    term.selection = None;
    term.scroll_display(Scroll::Bottom);
    if term.mode().contains(TermMode::VI) {
        term.toggle_vi_mode();
    }
}

/// Core of [`TerminalSessionHandle::vi_page`]: move the vi cursor by the page
/// delta, then scroll the viewport by the same delta so the cursor keeps its
/// row on screen.
///
/// A full page is the grid height, a half page reuses [`half_page_lines`] — the
/// same magnitude the Phase 1 ⌃⌘↑/⌃⌘↓ chords step by, so `⌃u` in copy mode and
/// `⌃⌘↑` outside it travel identically.
fn vi_page_in<L: EventListener>(term: &mut Term<L>, toward_history: bool, half: bool) {
    if !term.mode().contains(TermMode::VI) {
        return;
    }
    let screen_lines = term.screen_lines();
    let lines = if half {
        half_page_lines(screen_lines)
    } else {
        // Floored at one line for the same reason `half_page_lines` is: a 0-row
        // grid is reachable mid-resize and must not yield a 0-line page.
        i32::try_from(screen_lines).unwrap_or(i32::MAX).max(1)
    };
    let delta = if toward_history { lines } else { -lines };
    let cursor = term.vi_mode_cursor.scroll(&*term, delta);
    term.vi_mode_cursor = cursor;
    term.scroll_display(Scroll::Delta(delta));
}

/// Core of [`TerminalSessionHandle::toggle_copy_selection`] — P5's toggle
/// matrix, with `anchor` as the caller-owned anchor slot (alacritty does not
/// expose the `Selection`'s own anchor).
///
/// "A live selection" is read off the `Term`, not off `anchor`: the `Term` can
/// drop a selection out from under us (an erase intersecting it, a column
/// resize, a rotation off the top of history), and after that the next `v`
/// must start a fresh selection rather than clear a selection that is
/// already gone.
fn toggle_copy_selection_in<L: EventListener>(
    term: &mut Term<L>,
    anchor: &mut Option<(Point, SelectionType)>,
    ty: SelectionType,
) {
    if !term.mode().contains(TermMode::VI) {
        return;
    }
    let cursor = term.vi_mode_cursor.point;
    let live = term.selection.is_some();
    let start = match *anchor {
        // Same kind again: put the selection away.
        Some((_, prev)) if live && prev == ty => {
            term.selection = None;
            *anchor = None;
            return;
        }
        // Different kind: keep the anchor, rebuild at the new granularity.
        Some((point, _)) if live => point,
        // Nothing live (never started, or the Term dropped it): start here.
        _ => cursor,
    };
    *anchor = Some((start, ty));
    term.selection = Some(copy_mode_selection(ty, start, cursor));
}

/// The `Selection` a copy-mode `v`/`V`/`⌃v` installs: anchored at `anchor`,
/// ending at the vi `cursor`.
///
/// The `update` is not optional even when the two points coincide. A bare
/// `Selection::new` is **empty** (both anchors identical, same side), and
/// alacritty's `vi_mode_recompute_selection` skips empty selections — so a
/// never-updated `v` would sit dead through every subsequent motion. Updating
/// the end makes it a live one-cell selection, which is also what vim paints
/// the instant you press `v`. `include_all` then assigns both endpoint sides
/// from the direction, the same rule [`selection_sides`] encodes.
fn copy_mode_selection(ty: SelectionType, anchor: Point, cursor: Point) -> Selection {
    let mut sel = Selection::new(ty, anchor, Side::Left);
    sel.update(cursor, Side::Right);
    sel.include_all();
    sel
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

/// How many lines "half a page" is for a grid `screen_lines` tall — the magnitude
/// the Phase 1 ⌃⌘↑/⌃⌘↓ chords step the display by (`alacritty_terminal`'s `Scroll`
/// has no half-page variant at this pin, so the delta is computed here).
///
/// Integer division truncates, and the result is floored at **1** so a
/// pathologically short grid (1 row, or a 0-row grid during a resize) still moves
/// by a line instead of silently doing nothing. `screen_lines` never exceeds
/// `i32::MAX` in practice; the cast is saturating anyway.
pub fn half_page_lines(screen_lines: usize) -> i32 {
    i32::try_from(screen_lines / 2).unwrap_or(i32::MAX).max(1)
}

/// The signed [`Scroll::Delta`] for one half-page step on a grid `screen_lines`
/// tall: **positive** toward history (⌃⌘↑), **negative** toward the bottom (⌃⌘↓)
/// — the same sign convention the wheel path
/// ([`TerminalSessionHandle::scroll_lines`]) uses.
pub fn half_page_delta(screen_lines: usize, toward_history: bool) -> i32 {
    let lines = half_page_lines(screen_lines);
    if toward_history {
        lines
    } else {
        -lines
    }
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
        half_page_delta, half_page_lines, present_gate, take_scroll_steps, to_terminal_event,
        DrainReady, DrainSignal, PresentGate, TerminalEvent, PRESENT_THROTTLE,
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

    // ---- Drag selection: Term-owned content-locked anchor ---------------------
    //
    // These pin the drag contract (`start_selection`/`extend_selection`, via the
    // shared `drag_selection_start`/`drag_selection_extend` cores): the anchor
    // lives in the Term's own `Selection`, which alacritty rotates with the grid
    // as output streams, while the end is re-resolved from the pointer per
    // move/wheel step. Design + terminal survey: docs/plans/selection-scroll-anchor.md.
    //
    // Every assertion is on CONTENT (grid coordinates checked against row text)
    // — never viewport rows: at the scrollback cap the viewport legitimately
    // drifts while the selection stays glued to its text.

    use alacritty_terminal::grid::{Dimensions, Scroll};

    /// A term fed `"line 0"..="line 39"`. 41 rows total (40 printed + the
    /// cursor's fresh row) on a 24-row screen leaves history rows `line 0..=16`;
    /// grid `Line(l)` shows `line (17 + l)` for any `l >= -17`.
    fn numbered_term() -> (Term<VoidListener>, Processor) {
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..40 {
            parser.advance(&mut term, format!("line {i}\r\n").as_bytes());
        }
        (term, parser)
    }

    /// The text content of grid row `line`, trailing blanks trimmed.
    fn row_text(term: &Term<VoidListener>, line: i32) -> String {
        let row = &term.grid()[Line(line)];
        let cols = term.columns();
        let s: String = (0..cols).map(|c| row[Column(c)].c).collect();
        s.trim_end().to_string()
    }

    /// Resolve the live drag selection, panicking on "no selection".
    fn drag_range(term: &Term<VoidListener>) -> (Point, Point) {
        let range = term
            .selection
            .as_ref()
            .expect("selection alive")
            .to_range(term)
            .expect("non-empty");
        (range.start, range.end)
    }

    #[test]
    fn streaming_while_parked_keeps_anchor_on_clicked_content() {
        // The 63b6080 regression case, now the library's job: parked in
        // scrollback with output streaming, the Term rotates the selection with
        // the grid, so the anchor follows the clicked text into history.
        let (mut term, mut parser) = numbered_term();
        term.scroll_display(Scroll::Delta(5));
        assert_eq!(term.grid().display_offset(), 5);

        assert_eq!(row_text(&term, 0), "line 17");
        super::drag_selection_start(
            &mut term,
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
        );

        for streamed in ["one\r\n", "two\r\n", "three\r\n"] {
            parser.advance(&mut term, streamed.as_bytes());
        }
        assert_eq!(term.grid().display_offset(), 8, "viewport auto-parked");
        assert_eq!(row_text(&term, -3), "line 17", "clicked content rotated 3 into history");

        // Extend along the SAME content row: exactly one row selected. A
        // drifted anchor would span extra rows (the old bug's shape).
        assert!(super::drag_selection_extend(&mut term, Point::new(Line(-3), Column(5))));
        assert_eq!(
            drag_range(&term),
            (Point::new(Line(-3), Column(0)), Point::new(Line(-3), Column(5))),
            "anchor stayed glued to the clicked content"
        );
    }

    #[test]
    fn user_scroll_mid_drag_extends_from_the_anchored_content() {
        // THE reported bug: drag, then wheel into history without moving the
        // pointer's content along. A display scroll never touches grid
        // coordinates, so the anchor must stay put while the end reaches the
        // newly revealed rows.
        let (mut term, _parser) = numbered_term();
        assert_eq!(row_text(&term, 13), "line 30");
        super::drag_selection_start(
            &mut term,
            SelectionType::Simple,
            Point::new(Line(13), Column(2)),
        );

        term.scroll_display(Scroll::Delta(10));

        // The pointer now rests over content revealed from history — e.g.
        // viewport row 3 resolves to Line(3 - 10) = Line(-7).
        assert_eq!(row_text(&term, -7), "line 10");
        assert!(super::drag_selection_extend(&mut term, Point::new(Line(-7), Column(4))));
        assert_eq!(
            drag_range(&term),
            (Point::new(Line(-7), Column(4)), Point::new(Line(13), Column(2))),
            "selection spans from the revealed content back to the unmoved anchor"
        );
    }

    #[test]
    fn fresh_click_anchor_rotates_before_first_extend() {
        // A just-pressed anchor is a zero-length selection and must still
        // rotate with the grid (kitty shipped exactly this bug, separately
        // from the drag one — commit a13f815591 there).
        let (mut term, mut parser) = numbered_term();
        term.scroll_display(Scroll::Delta(5));
        assert_eq!(row_text(&term, 0), "line 17");
        super::drag_selection_start(
            &mut term,
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
        );

        parser.advance(&mut term, b"one\r\ntwo\r\n");
        assert_eq!(row_text(&term, -2), "line 17");

        assert!(super::drag_selection_extend(&mut term, Point::new(Line(-2), Column(3))));
        assert_eq!(
            drag_range(&term),
            (Point::new(Line(-2), Column(0)), Point::new(Line(-2), Column(3))),
            "zero-length anchor rotated with the content before the first extend"
        );
    }

    #[test]
    fn drag_path_includes_both_endpoint_cells() {
        // `include_all`'s non-Block arms are `selection_sides`' comparison; pin
        // that equivalence through the production path so the leftward-drag fix
        // (BUGS.md #11) survives the anchor move into the Term — including when
        // the drag direction flips mid-gesture, which rewrites BOTH sides.
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut drag = |start: (i32, usize), moves: &[(i32, usize)]| {
            super::drag_selection_start(
                &mut term,
                SelectionType::Simple,
                Point::new(Line(start.0), Column(start.1)),
            );
            for &(l, c) in moves {
                assert!(super::drag_selection_extend(
                    &mut term,
                    Point::new(Line(l), Column(c))
                ));
            }
            let range = term.selection.as_ref().unwrap().to_range(&term).expect("non-empty");
            (range.start, range.end)
        };

        // Rightward, leftward, upward: both endpoint cells included.
        assert_eq!(
            drag((0, 2), &[(0, 5)]),
            (Point::new(Line(0), Column(2)), Point::new(Line(0), Column(5)))
        );
        assert_eq!(
            drag((0, 5), &[(0, 0)]),
            (Point::new(Line(0), Column(0)), Point::new(Line(0), Column(5)))
        );
        assert_eq!(
            drag((3, 4), &[(1, 7)]),
            (Point::new(Line(1), Column(7)), Point::new(Line(3), Column(4)))
        );
        // Direction flip mid-drag: right past the anchor, then back left.
        assert_eq!(
            drag((0, 5), &[(0, 7), (0, 1)]),
            (Point::new(Line(0), Column(1)), Point::new(Line(0), Column(5)))
        );
    }

    #[test]
    fn fresh_simple_click_is_an_empty_selection() {
        // Replaces the old clear-on-click: a single press installs a zero-length
        // Simple selection, which resolves to no range (paints nothing, ⌘C
        // copies nothing) until the drag actually moves.
        let mut term = term_with("hello world");
        super::drag_selection_start(
            &mut term,
            SelectionType::Simple,
            Point::new(Line(0), Column(3)),
        );
        assert!(term.selection.as_ref().unwrap().to_range(&term).is_none());
        assert!(term.selection_to_string().is_none());
    }

    #[test]
    fn extend_without_live_selection_reports_the_drag_dead() {
        // Never-started drag: nothing to extend.
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        assert!(!super::drag_selection_extend(&mut term, Point::new(Line(0), Column(0))));

        // And the mid-gesture case the view actually hits: the Term drops the
        // selection out from under a live drag — ED All (`ESC[2J`) clears the
        // screen and nulls `term.selection`. Extends turn into no-op `false`s;
        // the view keeps the gesture armed but stops extending.
        let (mut term, mut parser) = numbered_term();
        super::drag_selection_start(
            &mut term,
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
        );
        assert!(super::drag_selection_extend(&mut term, Point::new(Line(2), Column(3))));
        parser.advance(&mut term, b"\x1b[2J");
        assert!(term.selection.is_none(), "ED All drops the selection");
        assert!(!super::drag_selection_extend(&mut term, Point::new(Line(3), Column(0))));
    }

    #[test]
    fn semantic_drag_extends_word_by_word_through_the_drag_path() {
        // Double-click mid-"world", then drag into "again" — via the production
        // drag cores, not the set_selection_typed seam. `to_range` re-expands
        // both raw anchors on every read, so end-only extension works for the
        // typed granularities too.
        let mut term = term_with("hello world again");
        super::drag_selection_start(
            &mut term,
            SelectionType::Semantic,
            Point::new(Line(0), Column(8)),
        );
        // The fresh zero-length Semantic already resolves to the clicked word.
        assert_eq!(
            drag_range(&term),
            (Point::new(Line(0), Column(6)), Point::new(Line(0), Column(10))),
            "double-click selects \"world\""
        );
        assert!(super::drag_selection_extend(&mut term, Point::new(Line(0), Column(14))));
        assert_eq!(
            drag_range(&term),
            (Point::new(Line(0), Column(6)), Point::new(Line(0), Column(16))),
            "dragging into \"again\" extends to its far boundary"
        );
    }

    #[test]
    fn anchor_falling_off_the_scrollback_cap_clamps_instead_of_drifting() {
        // With history saturated, further streaming rotates the anchor past the
        // top of scrollback. `to_range` clamps the overshoot to the grid top at
        // read time (alacritty's design: clamp at read, not at rotate) — the
        // selection shrinks; it never drifts onto other content or panics.
        let cfg = Config { scrolling_history: 5, ..Config::default() };
        let mut term = Term::new(cfg, &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..40 {
            parser.advance(&mut term, format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(term.topmost_line(), Line(-5), "history saturated at 5");

        // Anchor at the very top of history, end on screen.
        super::drag_selection_start(
            &mut term,
            SelectionType::Simple,
            Point::new(Line(-5), Column(0)),
        );
        assert!(super::drag_selection_extend(&mut term, Point::new(Line(0), Column(5))));

        // Three more lines rotate the anchor to Line(-8), past the cap.
        parser.advance(&mut term, b"a\r\nb\r\nc\r\n");
        let (start, end) = drag_range(&term);
        // Hard-coded Line(-5), not `term.topmost_line()`: the clamp uses
        // `topmost_line` internally, so asserting against it would be circular.
        assert_eq!(start, Point::new(Line(-5), Column(0)), "start clamped to grid top");
        assert_eq!(end, Point::new(Line(-3), Column(5)), "end rotated with its content");
    }

    // ---- Half-page scrollback delta (Phase 1: ⌃⌘↑ / ⌃⌘↓) --------------------

    /// The magnitude is `screen_lines / 2`, truncating.
    #[test]
    fn half_page_lines_is_half_the_grid_height() {
        assert_eq!(half_page_lines(40), 20);
        assert_eq!(half_page_lines(24), 12);
        // Odd heights truncate (tmux `halfpage-up` does the same).
        assert_eq!(half_page_lines(25), 12);
        assert_eq!(half_page_lines(3), 1);
    }

    /// Floored at one line, so a degenerate grid still moves instead of silently
    /// doing nothing (a 0-row grid is reachable mid-resize).
    #[test]
    fn half_page_lines_floors_at_one() {
        assert_eq!(half_page_lines(2), 1);
        assert_eq!(half_page_lines(1), 1);
        assert_eq!(half_page_lines(0), 1, "a 0-row grid must not yield a 0-line step");
    }

    /// Sign per direction: positive toward history (⌃⌘↑), negative toward the
    /// bottom (⌃⌘↓) — the wheel path's `Scroll::Delta` convention.
    #[test]
    fn half_page_delta_signs_by_direction() {
        assert_eq!(half_page_delta(40, true), 20, "⌃⌘U pages toward history");
        assert_eq!(half_page_delta(40, false), -20, "⌃⌘D pages toward the bottom");
        // Same magnitude both ways, so up-then-down returns to the start.
        assert_eq!(half_page_delta(37, true) + half_page_delta(37, false), 0);
        // The floor applies to both directions.
        assert_eq!(half_page_delta(1, true), 1);
        assert_eq!(half_page_delta(1, false), -1);
    }

    // ---- Copy mode (Phase 3) --------------------------------------------------
    //
    // Copy mode IS `TermMode::VI` (P1), so these drive a real `Term<VoidListener>`
    // through the production cores (`exit_copy_mode_in`, `vi_page_in`,
    // `toggle_copy_selection_in`) and alacritty's own `toggle_vi_mode` /
    // `vi_motion` — never a mimic. The handle methods around them are the same
    // code plus a `FairMutex` lock and an `Option` for the unspawned session.

    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::vi_mode::ViMotion;

    /// Shorthand for a buffer point.
    fn p(line: i32, column: usize) -> Point {
        Point::new(Line(line), Column(column))
    }

    /// A 20x5 grid with content shaped for the motion table:
    ///
    /// ```text
    /// row 0: alpha beta
    /// row 1:   gamma (delta)
    /// row 2:
    /// row 3: omega
    /// row 4:            <- terminal cursor
    /// ```
    ///
    /// Words with and without semantic escape chars, a bracket pair, a leading
    /// indent and a blank line — enough to tell `w` from `W` and to give the
    /// paragraph motions something to find. Copy mode is already on.
    fn motion_grid() -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(20, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"alpha beta\r\n  gamma (delta)\r\n\r\nomega\r\n");
        term.toggle_vi_mode();
        term
    }

    /// Entry seeds the vi cursor where alacritty says it should: at the terminal
    /// cursor when that is visible, at the viewport's top-left when it is not
    /// (P6's "entry seeds the cursor is library behaviour" — pinned so an
    /// alacritty bump that changed it would be caught here, not by feel).
    #[test]
    fn entering_copy_mode_seeds_the_vi_cursor() {
        let (mut term, _parser) = numbered_term();
        let shell_cursor = term.grid().cursor.point;
        term.toggle_vi_mode();
        assert!(term.mode().contains(TermMode::VI));
        assert_eq!(term.vi_mode_cursor.point, shell_cursor, "seeded at the shell cursor");

        // Scrolled far enough that the shell cursor is off-screen: the vi cursor
        // seeds at the top-left of what the user is actually looking at.
        super::exit_copy_mode_in(&mut term);
        term.scroll_display(Scroll::Delta(10));
        term.toggle_vi_mode();
        assert_eq!(term.vi_mode_cursor.point, p(-10, 0), "seeded at the viewport top-left");
    }

    /// P6's exit ordering, observed through what it leaves behind: no selection,
    /// viewport parked at the bottom, VI off. The selection must be cleared
    /// BEFORE the scroll — `scroll_display` recomputes a live selection's end
    /// while VI is still set, so the wrong order would drag the selection down
    /// the buffer on the way out instead of dropping it.
    #[test]
    fn exiting_copy_mode_clears_the_selection_and_returns_to_live_output() {
        let (mut term, _parser) = numbered_term();
        term.scroll_display(Scroll::Delta(6));
        term.toggle_vi_mode();
        let mut anchor = None;
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Simple);
        term.vi_motion(ViMotion::Down);
        assert!(term.selection.is_some(), "a live selection to clear");
        assert_eq!(term.grid().display_offset(), 6, "parked in scrollback");

        super::exit_copy_mode_in(&mut term);
        assert!(term.selection.is_none(), "selection cleared");
        assert_eq!(term.grid().display_offset(), 0, "back at the live bottom");
        assert!(!term.mode().contains(TermMode::VI), "VI off");
    }

    /// Exit is idempotent: calling it on a pane that is not in copy mode still
    /// parks the viewport at the bottom and leaves VI off — it must never
    /// *enter* the mode by toggling a bit that was already clear.
    #[test]
    fn exiting_when_not_in_copy_mode_never_toggles_the_mode_on() {
        let (mut term, _parser) = numbered_term();
        term.scroll_display(Scroll::Delta(4));
        super::exit_copy_mode_in(&mut term);
        assert!(!term.mode().contains(TermMode::VI));
        assert_eq!(term.grid().display_offset(), 0);
    }

    /// Every D3 motion, on the seeded grid, landing where vim lands. The mapping
    /// from keys to these variants is Slice 2's table; this pins the variants
    /// themselves at this alacritty pin.
    #[test]
    fn vi_motions_move_the_cursor_the_way_vim_does() {
        let mut term = motion_grid();
        // (start, motion, expected end)
        let cases: &[(Point, ViMotion, Point)] = &[
            // hjkl
            (p(1, 3), ViMotion::Up, p(0, 3)),
            (p(1, 3), ViMotion::Down, p(2, 3)),
            (p(1, 3), ViMotion::Left, p(1, 2)),
            (p(0, 3), ViMotion::Right, p(0, 4)),
            // 0 / $ / ^
            (p(1, 5), ViMotion::First, p(1, 0)),
            (p(0, 2), ViMotion::Last, p(0, 9)),
            (p(1, 10), ViMotion::FirstOccupied, p(1, 2)),
            // H / M / L — first occupied cell of the top / middle / bottom row.
            (p(3, 4), ViMotion::High, p(0, 0)),
            (p(3, 4), ViMotion::Middle, p(1, 2)),
            (p(0, 0), ViMotion::Low, p(4, 0)),
            // w / b / e (semantic: "(" and ")" are word boundaries)
            (p(0, 0), ViMotion::SemanticRight, p(0, 6)),
            (p(0, 6), ViMotion::SemanticLeft, p(0, 0)),
            (p(0, 0), ViMotion::SemanticRightEnd, p(0, 4)),
            (p(0, 9), ViMotion::SemanticLeftEnd, p(0, 4)),
            // W (whitespace-only words): from "(" it skips the whole "(delta)"
            // and the blank row, where semantic `w` would stop at "delta".
            (p(1, 8), ViMotion::SemanticRight, p(1, 9)),
            (p(1, 8), ViMotion::WordRight, p(3, 0)),
            (p(3, 0), ViMotion::WordLeft, p(1, 8)),
            // % — matching bracket, both ways.
            (p(1, 8), ViMotion::Bracket, p(1, 14)),
            (p(1, 14), ViMotion::Bracket, p(1, 8)),
            // { / } — the blank row 2 is the paragraph break.
            (p(0, 0), ViMotion::ParagraphDown, p(2, 0)),
            (p(3, 0), ViMotion::ParagraphUp, p(0, 0)),
        ];

        for &(from, motion, expected) in cases {
            term.vi_goto_point(from);
            term.vi_motion(motion);
            assert_eq!(term.vi_mode_cursor.point, expected, "{motion:?} from {from:?}");
        }
    }

    /// Motions are inert while copy mode is off — the guarantee that lets the
    /// view fall through to the pty for a bare `h` (P3).
    #[test]
    fn vi_motions_are_inert_outside_copy_mode() {
        let mut term = motion_grid();
        super::exit_copy_mode_in(&mut term);
        let before = term.vi_mode_cursor.point;
        term.vi_motion(ViMotion::Up);
        term.vi_motion(ViMotion::WordRight);
        assert_eq!(term.vi_mode_cursor.point, before);
    }

    /// `⌃u`/`⌃d`/`⌃f`/`⌃b`: the viewport pages AND the cursor keeps its row on
    /// screen. Doing only one of the two is the bug this pins — a bare
    /// `scroll_display` would clamp the cursor to the viewport edge, and a bare
    /// `ViModeCursor::scroll` would move the cursor with no scroll at all.
    #[test]
    fn paging_moves_the_viewport_and_carries_the_cursor() {
        // Deep history on purpose: `numbered_term`'s 17 scrollback rows are
        // shallower than the two pages this walks, and a clamp at the top of
        // history would hide the very thing being asserted.
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..200 {
            parser.advance(&mut term, format!("line {i}\r\n").as_bytes());
        }
        term.toggle_vi_mode();
        // Halfway down a 24-row screen, parked at the bottom.
        term.vi_goto_point(p(12, 0));
        assert_eq!(term.grid().display_offset(), 0);

        super::vi_page_in(&mut term, true, true);
        assert_eq!(term.grid().display_offset(), 12, "half page toward history");
        assert_eq!(term.vi_mode_cursor.point.line, Line(0), "cursor kept its screen row");

        super::vi_page_in(&mut term, true, false);
        assert_eq!(term.grid().display_offset(), 12 + 24, "full page toward history");
        assert_eq!(term.vi_mode_cursor.point.line, Line(-24), "cursor kept its screen row");

        super::vi_page_in(&mut term, false, false);
        assert_eq!(term.grid().display_offset(), 12, "full page back toward the bottom");
        assert_eq!(term.vi_mode_cursor.point.line, Line(0));
    }

    /// Paging is a copy-mode verb only: outside the mode the ⌃⌘↑/⌃⌘↓ chords own
    /// the viewport, and these keys belong to the pty.
    #[test]
    fn paging_is_inert_outside_copy_mode() {
        let (mut term, _parser) = numbered_term();
        super::vi_page_in(&mut term, true, true);
        assert_eq!(term.grid().display_offset(), 0);
    }

    /// `g` / `G`: the ends of the buffer.
    #[test]
    fn top_and_bottom_jump_to_the_ends_of_the_scrollback() {
        let (mut term, _parser) = numbered_term();
        term.toggle_vi_mode();

        let top = Point::new(term.topmost_line(), Column(0));
        term.vi_goto_point(top);
        assert_eq!(term.vi_mode_cursor.point, p(-17, 0), "g — oldest line in history");
        assert_eq!(row_text(&term, -17), "line 0");

        let bottom_line = term.grid().cursor.point.line;
        term.vi_goto_point(Point::new(bottom_line, Column(0)));
        assert_eq!(term.vi_mode_cursor.point, p(23, 0), "G — the terminal cursor's line");
        assert_eq!(term.grid().display_offset(), 0, "and the viewport followed it back");
    }

    /// P5's toggle matrix, in one pass: nothing → a live one-cell selection at
    /// the cursor; a DIFFERENT kind → same anchor, new granularity; the SAME
    /// kind → cleared. Plus the load-bearing part in the middle: motions extend
    /// the selection through alacritty's own recompute, which only happens
    /// because the fresh selection is non-empty.
    #[test]
    fn copy_selection_toggles_vim_style() {
        let mut term = motion_grid();
        let mut anchor = None;

        // (1) Nothing live: start at the cursor, one cell, immediately painted.
        term.vi_goto_point(p(0, 2));
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Simple);
        assert_eq!(anchor, Some((p(0, 2), SelectionType::Simple)));
        assert_eq!(drag_range(&term), (p(0, 2), p(0, 2)), "the cursor cell is selected");

        // Motions extend it; the anchor stays put.
        term.vi_motion(ViMotion::Right);
        term.vi_motion(ViMotion::Right);
        assert_eq!(drag_range(&term), (p(0, 2), p(0, 4)));

        // (2) A different kind: same anchor, line granularity.
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Lines);
        assert_eq!(anchor, Some((p(0, 2), SelectionType::Lines)));
        assert_eq!(drag_range(&term), (p(0, 0), p(0, 19)), "the whole row");

        // (3) The same kind again: put it away.
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Lines);
        assert!(term.selection.is_none());
        assert_eq!(anchor, None);
    }

    /// `⌃v` is the same toggle at block granularity — the one `SelectionType`
    /// variant Nice had never used before Phase 3.
    #[test]
    fn block_selection_selects_a_column_range() {
        let mut term = motion_grid();
        let mut anchor = None;
        term.vi_goto_point(p(0, 2));
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Block);
        term.vi_motion(ViMotion::Down);
        term.vi_motion(ViMotion::Right);

        let range = term.selection.as_ref().unwrap().to_range(&term).expect("non-empty");
        assert!(range.is_block, "⌃v selects a block");
        assert_eq!((range.start, range.end), (p(0, 2), p(1, 3)));
    }

    /// The `Term` can drop a selection out from under copy mode (here an ED All
    /// clear). The next `v` must START a fresh selection, not "clear" one that
    /// is already gone — so liveness is read off the `Term`, never off the
    /// anchor slot alone.
    #[test]
    fn a_dropped_selection_makes_the_next_toggle_start_a_fresh_one() {
        let (mut term, mut parser) = numbered_term();
        term.toggle_vi_mode();
        let mut anchor = None;
        term.vi_goto_point(p(2, 0));
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Simple);
        term.vi_motion(ViMotion::Right);
        assert!(term.selection.is_some());

        parser.advance(&mut term, b"\x1b[2J");
        assert!(term.selection.is_none(), "ED All dropped it");

        term.vi_goto_point(p(4, 1));
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Simple);
        assert_eq!(anchor, Some((p(4, 1), SelectionType::Simple)), "re-anchored, not cleared");
        assert!(term.selection.is_some());
    }

    /// Selection toggling is copy-mode-only, like the motions.
    #[test]
    fn selection_toggle_is_inert_outside_copy_mode() {
        let mut term = motion_grid();
        super::exit_copy_mode_in(&mut term);
        let mut anchor = None;
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Simple);
        assert!(term.selection.is_none());
        assert_eq!(anchor, None);
    }

    /// `y` over a selection that starts in scrollback and ends on screen — the
    /// flagship copy-mode case (grab the thing that scrolled past). The text
    /// comes from alacritty's `selection_to_string`, the same source ⌘C uses.
    #[test]
    fn yank_reads_text_across_the_history_boundary() {
        let (mut term, _parser) = numbered_term();
        term.toggle_vi_mode();
        let mut anchor = None;
        // Line(-1) is "line 16", the last history row; Line(0) is "line 17".
        assert_eq!(row_text(&term, -1), "line 16");
        assert_eq!(row_text(&term, 0), "line 17");

        term.vi_goto_point(p(-1, 0));
        super::toggle_copy_selection_in(&mut term, &mut anchor, SelectionType::Simple);
        term.vi_motion(ViMotion::Down);
        term.vi_motion(ViMotion::Last);

        assert_eq!(term.selection_to_string().as_deref(), Some("line 16\nline 17"));
    }
}
