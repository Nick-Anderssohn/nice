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
//! ## Draining
//!
//! The session's outward events arrive on a plain `std::sync::mpsc` channel fed
//! from the session's feeder / exit-watcher threads, and its damage-wake is a
//! `Send` callback fired from the feeder thread (never under the `Term` lock).
//! Neither may touch gpui from those threads, so this entity bridges them onto
//! the gpui foreground executor: the damage-wake bumps a `Send` atomic counter,
//! and one spawned task drains the event channel + observes the counter,
//! translating both into on-entity `cx.emit` / `cx.notify`.
//!
//! **Damage → present.** The drain task turns a damage bump into `cx.notify()`
//! **plus an explicit present kick** on a short poll. `cx.notify()` alone is
//! enough for a frontmost, continuously-repainting window (the self-test
//! scenarios drive `request_animation_frame`), but it **never presents while a
//! window's CVDisplayLink is stopped** (occluded) — a real pane needs the
//! `setNeedsDisplay` kick to force `displayLayer:` on the next CA commit. That
//! kick is objc2, so it is **injected** as a callback ([`set_present_kick`]) the
//! app constructs in `crates/nice/src/platform`; this crate stays objc2-free.
//! The kick is cloned out of the entity and fired on the bare `AsyncApp`
//! *outside* the entity update, so re-entering the window handle never nests
//! inside the entity's borrow. (Replacing the poll itself with an event-driven
//! wake is a still-later optimization; the atomic-counter seam already exists.)
//!
//! [`set_present_kick`]: TerminalSessionHandle::set_present_kick

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use anyhow::Result;
use gpui::{AppContext, AsyncApp, Context, Entity, EventEmitter, Task};

use nice_term_core::{DamageCallback, ExitStatus, Session, SessionEvent, SharedTerm, SpawnSpec};

/// The injected demand-present kick: a `setNeedsDisplay`-on-the-window callback
/// the app constructs in `crates/nice/src/platform` (the sole sanctioned objc2
/// crossing) and hands to [`TerminalSessionHandle::set_present_kick`]. It takes
/// the bare [`AsyncApp`] so it can drive the window handle from the drain task
/// *outside* any entity update. `Arc` (not `Box`) so the drain loop can clone it
/// out of the entity and call it after the update returns.
pub type PresentKick = Arc<dyn Fn(&mut AsyncApp)>;

/// How often the drain task polls the session's event channel + damage counter.
/// A slice-1 stand-in for the event-driven damage wake + present kick a later
/// slice installs; small enough to feel immediate, coarse enough to stay cheap
/// while idle (it only `notify`s when the damage counter actually moved).
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(8);

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
    /// The drain task. Held so it is cancelled when the entity drops (a dropped
    /// `Task` is cancelled), so no task outlives its session. The damage counter
    /// it observes — the seam the demand-present kick hangs off of — lives inside
    /// the task + the session's damage callback.
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
        let damage = Arc::new(AtomicU64::new(0));
        let on_damage: DamageCallback = {
            let damage = Arc::clone(&damage);
            // Non-blocking, never under the `Term` lock, never re-enters gpui —
            // honours nice-term-core's damage-wake contract. Just bumps a flag.
            Box::new(move || {
                damage.fetch_add(1, Ordering::Release);
            })
        };
        let (session, events) = Session::spawn(spec.clone(), scrollback_lines, on_damage)?;

        let entity = cx.new(|cx| {
            let drain = cx.spawn(async move |this, cx| {
                drain_loop(this, cx, events, damage).await;
            });
            TerminalSessionHandle {
                session,
                spec,
                scrollback_lines,
                scroll_accum: 0.0,
                present_kick: None,
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

        let damage = Arc::new(AtomicU64::new(0));
        let on_damage: DamageCallback = {
            let damage = Arc::clone(&damage);
            Box::new(move || {
                damage.fetch_add(1, Ordering::Release);
            })
        };
        // Spawn the fresh session FIRST; only swap it in on success so a failed
        // respawn leaves the held pane intact (its output stays readable) rather
        // than blanking the view to a dead session.
        let (session, events) = Session::spawn(shell_spec.clone(), self.scrollback_lines, on_damage)?;
        self.session = session;
        // Future dismissals of this pane respawn a shell too (the command spec is
        // gone once its held pane is dismissed).
        self.spec = shell_spec;
        self._drain = cx.spawn(async move |this, cx| {
            drain_loop(this, cx, events, damage).await;
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

    /// The shared `Term` the renderer locks (briefly) to read cells for a paint,
    /// or `None` if the session has not spawned. The renderer must copy cells
    /// under the lock and drop it before painting — never hold it across a
    /// present (see [`crate::element::TerminalElement`]).
    pub fn term(&self) -> Option<&SharedTerm> {
        self.session.term()
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

    /// Set a simple (non-block) selection spanning `start ..= end`, in **buffer**
    /// grid coordinates (`(line, column)`; `line` is negative for scrollback).
    ///
    /// This is the **programmatic selection setter test seam** the plan calls
    /// for: mouse selection *input* is R5, but the renderer must paint the core's
    /// selection state correctly now, so this drives that state directly. The
    /// caller should `cx.notify()` after calling it to repaint. No-op if the
    /// session has not spawned.
    pub fn set_selection(&self, start: (i32, usize), end: (i32, usize)) {
        if let Some(term_arc) = self.session.term() {
            let mut term = term_arc.lock();
            let start_pt = Point::new(Line(start.0), Column(start.1));
            let end_pt = Point::new(Line(end.0), Column(end.1));
            // Left side at the start, right side at the end => the resolved range
            // is inclusive of both endpoint cells (see `Selection::range_simple`).
            let mut sel = Selection::new(SelectionType::Simple, start_pt, Side::Left);
            sel.update(end_pt, Side::Right);
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

/// The drain task body: poll the session's event channel and its damage
/// counter, translating both onto the entity. Ends when the entity is gone (any
/// `update` returns `Err`). See the module docs for why this is a poll (the
/// event-driven wake + present kick is a later slice).
async fn drain_loop(
    this: gpui::WeakEntity<TerminalSessionHandle>,
    cx: &mut gpui::AsyncApp,
    events: Receiver<SessionEvent>,
    damage: Arc<AtomicU64>,
) {
    let mut last_damage = 0u64;
    loop {
        // Drain every pending event, emitting + notifying for each.
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
                // No more events queued this tick.
                Err(TryRecvError::Empty) => break,
                // The session's senders live with the `Session` this entity
                // owns, so a disconnect only happens as the entity is torn down.
                // Stop draining events but keep servicing damage until `update`
                // reports the entity gone.
                Err(TryRecvError::Disconnected) => break,
            }
        }

        // Coalesced damage → one notify (repaint request) + one demand-present
        // kick. The kick is cloned out of the entity here and fired below on the
        // bare `AsyncApp`, *outside* the update, so re-entering the window handle
        // never nests inside this entity's borrow (see the module docs).
        let current = damage.load(Ordering::Acquire);
        if current != last_damage {
            last_damage = current;
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

        cx.background_executor().timer(DRAIN_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{take_scroll_steps, to_terminal_event, TerminalEvent};
    use nice_term_core::{ExitStatus, SessionEvent};
    use std::path::PathBuf;

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
}
