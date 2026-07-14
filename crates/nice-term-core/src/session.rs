//! `TermSession` — one live VT session: a spawned [`PtyProcess`] (slice 1)
//! joined to an `alacritty_terminal` `Term` behind a [`FairMutex`], with a
//! per-session **feeder thread** that reads the pty master off the render thread
//! and parses bytes into the `Term`, plus the damage-wake signal the renderer
//! drains on. This is the crate's exported threading shape (parse off-main,
//! share `Arc<FairMutex<Term>>`, wake via a callback).
//!
//! The deferred-spawn state machine (`NotSpawned → Spawning → Live → Exited`)
//! and held-pane classification live one layer up in [`crate::deferred`], built
//! on top of this value: `TermSession` is the eager, already-live session those
//! wrap — it exposes the raw exit status (via the pty) that classification
//! consumes, but does not classify.
//!
//! `TermSession` does own the two R6 escape-sequence side-channels, since both
//! straddle the VT core it holds: OSC 0/2 **titles** flow through the `Term`'s
//! [`EventProxy`], and OSC 7 **cwd** is teed off the raw pty bytes in the feeder
//! (see [`crate::osc7`]). Neither is surfaced when a `TermSession` is used bare
//! (the outward [`SessionEvent`] `Sender` is `None`); the [`Session`] layer
//! passes its `Sender` down via [`TermSession::spawn_with_events`] so they reach
//! the typed stream. The synchronous [`TermSession::bracketed_paste_active`]
//! query reads the same `Term`'s tracked DECSET 2004 mode.
//!
//! [`Session`]: crate::deferred::Session

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::deferred::SessionEvent;
use crate::osc7::Osc7Scanner;
use crate::pty::{ExitStatus, ExitWaiter, PtyProcess};
use crate::spawn::SpawnSpec;
use crate::vt::{self, EventProxy, GridSnapshot, SharedTerm, TermSize, DEFAULT_SCROLLBACK_LINES};

/// The damage-wake signal the feeder fires after each parsed chunk to tell the
/// UI "there is new terminal content — grab the lock and paint".
///
/// **Damage-wake callback contract (binding — see the plan):** the wake is a
/// signal *only*. The feeder invokes it
/// - **after** releasing the `FairMutex<Term>` lock (never while holding it), and
/// - expecting it to be **async / non-blocking** and to **never synchronously
///   re-enter the UI framework** — the callback should do no more than nudge an
///   executor / set a flag / send on a channel; the UI side drains and paints on
///   its own executor (R4's session-host entity owns the receiving end).
///
/// It runs on the feeder thread, so it is `Send`; it is called many times, so it
/// is `Fn` (not `FnOnce`).
pub type DamageCallback = Box<dyn Fn() + Send + 'static>;

/// One live terminal session: pty child + shared `Term` + feeder thread.
///
/// Teardown (explicit [`TermSession::teardown`] or drop) kills the child's
/// process group so the pty slave closes; the feeder's blocking read then hits
/// EOF and the thread ends. Drop never blocks the calling thread: if the
/// feeder has not yet finished, the join is handed to a janitor thread that
/// holds the `Arc`'d pty — so the master fd stays open until the feeder exits
/// and is never read stale — and closes it there.
pub struct TermSession {
    /// `Arc` so Drop can hand the pty (and thus the master fd's lifetime) to
    /// the janitor thread that joins a still-running feeder off-thread.
    pty: Arc<PtyProcess>,
    term: SharedTerm,
    feeder: Option<JoinHandle<()>>,
    scrollback_lines: usize,
    /// Out-of-band "the whole viewport changed" flag for grid mutations
    /// alacritty's own damage tracking cannot see (today: the parity in-place
    /// ED(2) erase — see [`crate::vt::ParityTerm`]). The feeder's parity
    /// handler raises it under the `Term` lock; the damage-gated renderer
    /// (fix round r5b) takes-and-clears it via
    /// [`take_forced_full_damage`](Self::take_forced_full_damage) while
    /// holding the same lock, folding it into a full-invalidate verdict.
    forced_full_damage: Arc<AtomicBool>,
}

impl TermSession {
    /// Spawn `spec`'s child behind a fresh pty and wire up the `Term` + feeder.
    ///
    /// `scrollback_lines` is the per-session scrollback knob (pass
    /// [`DEFAULT_SCROLLBACK_LINES`] for parity, or a larger value for
    /// perf/memory validation). `on_damage` is the wake — see [`DamageCallback`]
    /// for its binding contract.
    ///
    /// The pty is created first (slice 1's `PtyProcess`, honouring the PROTECTED
    /// spawn contract); the `Term` is sized to `spec`'s initial rows/cols and its
    /// `EventProxy` is wired to the pty so terminal replies reach the child.
    ///
    /// Used bare, a `TermSession` has no outward event sink: OSC 0/2 titles and
    /// OSC 7 cwd are recognised but dropped. The [`Session`](crate::deferred::Session)
    /// layer uses [`TermSession::spawn_with_events`] to receive them.
    pub fn spawn(
        spec: &SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
    ) -> io::Result<TermSession> {
        TermSession::spawn_inner(spec, scrollback_lines, on_damage, true, None)
    }

    /// [`TermSession::spawn`] with the parity [`DEFAULT_SCROLLBACK_LINES`] knob.
    pub fn spawn_default_scrollback(
        spec: &SpawnSpec,
        on_damage: DamageCallback,
    ) -> io::Result<TermSession> {
        TermSession::spawn(spec, DEFAULT_SCROLLBACK_LINES, on_damage)
    }

    /// Like [`TermSession::spawn`], but the caller supplies the outward event
    /// [`Sender`] so the R6 side-channels reach the typed stream: OSC 0/2 title
    /// changes (via the `Term`'s [`EventProxy`]) become
    /// [`SessionEvent::TitleChanged`] / [`SessionEvent::TitleReset`], and OSC 7
    /// cwd changes (via the feeder's byte tee) become
    /// [`SessionEvent::CwdChanged`]. Crate-internal: only the
    /// [`Session`](crate::deferred::Session) layer wires this, passing a clone of
    /// its own `Sender`.
    pub(crate) fn spawn_with_events(
        spec: &SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
        events: Sender<SessionEvent>,
    ) -> io::Result<TermSession> {
        TermSession::spawn_inner(spec, scrollback_lines, on_damage, true, Some(events))
    }

    /// Like [`TermSession::spawn`], but with the OSC 7 cwd tee **disabled**.
    ///
    /// The tee is a pure observer that never alters the bytes handed to the VT
    /// parser (see [`crate::osc7`]); this hook exists only so the byte-
    /// transparency test can spawn one session with the tee on and one with it
    /// off and assert the resulting grids are identical. Not part of the stable
    /// surface — normal callers use [`spawn`](TermSession::spawn) (tee on) or the
    /// [`Session`](crate::deferred::Session) layer.
    #[doc(hidden)]
    pub fn spawn_teeless(
        spec: &SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
    ) -> io::Result<TermSession> {
        TermSession::spawn_inner(spec, scrollback_lines, on_damage, false, None)
    }

    /// The shared spawn path: build the pty, the `Term` (wiring the `EventProxy`
    /// to the pty fd and the optional title sink), and the feeder (which runs
    /// the OSC 7 tee when `osc7_tee` and forwards cwd on `events`).
    fn spawn_inner(
        spec: &SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
        osc7_tee: bool,
        events: Option<Sender<SessionEvent>>,
    ) -> io::Result<TermSession> {
        let pty = Arc::new(PtyProcess::spawn(spec)?);
        let fd = pty.master_fd();

        let size = TermSize {
            rows: spec.rows as usize,
            cols: spec.cols as usize,
        };
        let config = Config {
            scrolling_history: scrollback_lines,
            ..Config::default()
        };
        let term: SharedTerm = Arc::new(FairMutex::new(Term::new(
            config,
            &size,
            EventProxy::new(fd, events.clone()),
        )));

        let forced_full_damage = Arc::new(AtomicBool::new(false));
        let feeder = spawn_feeder(
            fd,
            Arc::clone(&term),
            on_damage,
            osc7_tee,
            events,
            Arc::clone(&forced_full_damage),
        )?;

        Ok(TermSession {
            pty,
            term,
            feeder: Some(feeder),
            scrollback_lines,
            forced_full_damage,
        })
    }

    /// The shared `Term` the renderer (R4) locks to paint. The renderer holds
    /// the lock only long enough to read cells for one frame; it must not hold
    /// it across a paint/present (mirror the owned grid read API below).
    pub fn term(&self) -> &SharedTerm {
        &self.term
    }

    /// Take-and-clear the out-of-band full-damage flag (fix round r5b): `true`
    /// iff, since the last take, a parity VT override mutated the grid where
    /// alacritty's damage tracking cannot see it (the in-place ED(2) erase —
    /// see [`crate::vt::ParityTerm`]). The damage-gated renderer folds `true`
    /// into a full-invalidate verdict alongside `Term::damage()`.
    ///
    /// **Call while holding the `Term` lock.** The feeder raises the flag
    /// mid-parse under that lock; taking it without the lock could clear a
    /// raise whose grid mutation the caller's snapshot has not seen yet.
    pub fn take_forced_full_damage(&self) -> bool {
        self.forced_full_damage.swap(false, Ordering::AcqRel)
    }

    /// Write raw input bytes to the child (keystrokes, pastes). No newline is
    /// appended — callers frame their own input. Delegates to the pty.
    pub fn write_input(&self, data: &[u8]) -> io::Result<()> {
        self.pty.write_input(data)
    }

    /// Resize both the `Term` grid and the pty to `rows` x `cols`. The grid is
    /// resized first (brief lock), then the pty winsize is set — the kernel
    /// delivers `SIGWINCH` so the child reflows to match the grid.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        {
            let mut guard = self.term.lock();
            guard.resize(TermSize {
                rows: rows as usize,
                cols: cols as usize,
            });
        }
        self.pty.resize(rows, cols)
    }

    /// The current `Term` grid dimensions as `(rows, cols)` — the viewport, not
    /// the scrollback. Follows [`TermSession::resize`].
    pub fn dimensions(&self) -> (u16, u16) {
        let guard = self.term.lock();
        (guard.screen_lines() as u16, guard.columns() as u16)
    }

    /// Current scrollback history depth in lines (capped at the configured
    /// [`TermSession::scrollback_limit`]).
    pub fn history_lines(&self) -> usize {
        self.term.lock().grid().history_size()
    }

    /// Whether bracketed-paste mode (DECSET 2004) is currently enabled — the
    /// synchronous query the paste (R5) and drop (R7) paths consult before
    /// framing pasted text. Reads alacritty's tracked [`TermMode`] under a brief
    /// lock; the child toggles it with `ESC[?2004h` / `ESC[?2004l`.
    pub fn bracketed_paste_active(&self) -> bool {
        self.term.lock().mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// The configured per-session scrollback limit (lines).
    pub fn scrollback_limit(&self) -> usize {
        self.scrollback_lines
    }

    /// An owned snapshot of the visible viewport (see [`GridSnapshot`]). Locks
    /// briefly, copies, unlocks — never held across a paint.
    pub fn visible_snapshot(&self) -> GridSnapshot {
        vt::visible_snapshot(&self.term.lock())
    }

    /// Every buffer line (scrollback history + visible screen) as owned,
    /// trailing-trimmed `String`s. Locks briefly, copies, unlocks.
    pub fn grid_lines(&self) -> Vec<String> {
        vt::all_buffer_lines(&self.term.lock())
    }

    /// Whether `needle` appears on any buffer line, scrollback included — the
    /// "grid contains string" read, resilient to output having scrolled off the
    /// viewport. Does not hold the lock across the search (it copies first).
    pub fn grid_contains(&self, needle: &str) -> bool {
        self.grid_lines().iter().any(|line| line.contains(needle))
    }

    /// The child's pid (== its process-group id — see [`PtyProcess`]).
    pub fn child_pid(&self) -> libc::pid_t {
        self.pty.child_pid()
    }

    /// Whether the pane's shell has a **foreground child** — i.e. a command is
    /// running in the foreground of this pty other than the login shell itself.
    ///
    /// The pane's shell is a session / process-group leader (its pgid ==
    /// [`child_pid`](Self::child_pid), via `login_tty`), so while it merely sits
    /// at its prompt the terminal's foreground process group *is* the shell and
    /// `tcgetpgrp(master_fd) == child_pid`. When the shell spawns a foreground
    /// command the kernel moves the terminal's foreground group to that command's
    /// pgid, so `tcgetpgrp(master_fd) != child_pid`. That inequality is the busy
    /// signal R20.5's close confirmation reads for a terminal pane (mirrors
    /// Swift's `TabPtySession` `tcgetpgrp` probe: Swift's `getpgid(shellPid)`
    /// collapses to `child_pid()` here because the shell is a session leader).
    ///
    /// **Fallback ⇒ `false` (not busy), never a spurious confirmation:** any
    /// failure — a closed / invalid master fd (`fd < 0`), a non-leader / dead
    /// shell (`child_pid <= 0`), or `tcgetpgrp` itself returning `<= 0` (e.g. the
    /// slave already hung up) — is treated as "no foreground child".
    ///
    /// Only this `bool` leaves the crate; the raw fd never crosses the
    /// terminal-stack boundary (the syscall lives here, next to the fd it owns).
    pub fn has_foreground_child(&self) -> bool {
        let fd = self.pty.master_fd();
        if fd < 0 {
            return false;
        }
        let child = self.pty.child_pid();
        if child <= 0 {
            return false;
        }
        // SAFETY: `fd` is this session's live pty master, owned by `self.pty`
        // (closed only on drop). `tcgetpgrp` only reads the terminal's foreground
        // process-group id and takes no ownership of the fd.
        let fg = unsafe { libc::tcgetpgrp(fd) };
        if fg <= 0 {
            return false;
        }
        fg != child
    }

    /// The child's recorded exit status if it has already exited, else `None`.
    /// Held-pane classification (a later slice) consumes this; this slice does
    /// not classify.
    pub fn try_status(&self) -> Option<ExitStatus> {
        self.pty.try_status()
    }

    /// Block until the child exits and return its status.
    pub fn wait(&self) -> ExitStatus {
        self.pty.wait()
    }

    /// A cloneable handle that blocks until this session's child exits — the
    /// seam the deferred-spawn state machine's exit watcher (built on top of
    /// `TermSession`) waits on to emit its outward `Exited` event without
    /// owning the session. Delegates to the pty.
    pub fn exit_waiter(&self) -> ExitWaiter {
        self.pty.exit_waiter()
    }

    /// Block until the child exits or `timeout` elapses; `None` on timeout.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<ExitStatus> {
        self.pty.wait_timeout(timeout)
    }

    /// Force the child's process group to exit (SIGHUP, then SIGKILL from the
    /// pty's detached escalation thread after a grace) so no orphaned zsh
    /// survives. Never blocks; idempotent; delegates to the pty. The feeder is
    /// joined on drop (inline when already finished, else on the janitor).
    pub fn teardown(&self) {
        self.pty.teardown();
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        // Kill the child's group so the pty slave closes; the feeder's blocking
        // read then returns EOF/EIO and the thread ends. The feeder may only be
        // joined while the master fd it reads is still open (a stale/reused fd
        // read is the hazard), but joining inline would block this (main)
        // thread until the child dies — up to the SIGKILL grace for a
        // SIGHUP-immune child, forever for one in uninterruptible sleep. So:
        // join inline only when the feeder is already done; otherwise hand the
        // join AND the Arc'd pty to a janitor thread, which keeps the master
        // open until the feeder exits and closes it there.
        self.pty.teardown();
        if let Some(handle) = self.feeder.take() {
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                // The slot lets the rare janitor-spawn failure recover the
                // handle and fall back to the old blocking join — never
                // detach the feeder while this frame's pty (and master fd)
                // is about to drop.
                let slot = Arc::new(Mutex::new(Some(handle)));
                let janitor_slot = Arc::clone(&slot);
                let pty = Arc::clone(&self.pty);
                let janitor = std::thread::Builder::new()
                    .name("nice-term-janitor".to_string())
                    .spawn(move || {
                        if let Some(h) = janitor_slot.lock().unwrap().take() {
                            let _ = h.join();
                        }
                        drop(pty);
                    });
                if janitor.is_err() {
                    if let Some(h) = slot.lock().unwrap().take() {
                        let _ = h.join();
                    }
                }
            }
        }
    }
}

/// Spawn the per-session feeder thread: the single reader of this pty's master.
/// It blocking-reads bytes, runs the OSC 7 cwd tee over the raw chunk, parses
/// the **same** bytes into the shared `Term` under a brief lock, then — **after
/// releasing the lock** — fires the damage wake. Ends when the child's slave
/// side closes (read EOF on Linux, `EIO` on macOS).
///
/// The tee ([`Osc7Scanner`]) runs when `osc7_tee` is set; it observes the chunk
/// by shared reference (so the exact same slice reaches the parser — the tee
/// never eats or reorders a byte) and forwards each decoded cwd on `events` as
/// [`SessionEvent::CwdChanged`]. With no `events` sink the scan still runs but
/// its emissions are dropped (used by the byte-transparency test).
fn spawn_feeder(
    fd: RawFd,
    term: SharedTerm,
    on_damage: DamageCallback,
    osc7_tee: bool,
    events: Option<Sender<SessionEvent>>,
    forced_full_damage: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nice-term-feeder".to_string())
        .spawn(move || {
            let mut parser: Processor = Processor::new();
            // 64 KiB per read (fix round r5, lever 3 — input-flood freeze).
            // The old 4 KiB buffer meant one Term-lock round-trip + one damage
            // wake (each a CFRunLoopWakeUp poke, `session_handle.rs`) per 4 KiB
            // of flood output. alacritty 0.26 (the pinned VT stack) reads from
            // a 1 MiB buffer and parses up to 65535 bytes per lock hold
            // (`event_loop.rs:24-27` READ_BUFFER_SIZE / MAX_LOCKED_READ);
            // matching its per-lock byte budget cuts the flood-time signal and
            // lock rate 16x. Heap-allocated once so the feeder's stack frame
            // stays small; the parse-under-lock-then-signal-after-unlock
            // structure below is unchanged.
            let mut buf = vec![0u8; 64 * 1024].into_boxed_slice();
            let mut scanner = Osc7Scanner::new();
            loop {
                let n = unsafe {
                    libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n > 0 {
                    let chunk = &buf[..n as usize];
                    // OSC 7 cwd tee: observe the raw chunk BEFORE parsing. The
                    // scanner borrows `chunk` immutably and cannot alter it, so
                    // the byte-identical slice is handed to the parser below —
                    // the transparency contract holds by construction. Emission
                    // is best-effort onto the outward stream.
                    if osc7_tee {
                        scanner.feed(chunk, |path| {
                            if let Some(events) = &events {
                                let _ = events.send(SessionEvent::CwdChanged(path));
                            }
                        });
                    }
                    {
                        // Hold the lock ONLY to parse; the wake below is fired
                        // after this scope drops the guard (damage-wake contract).
                        // Parse through the SwiftTerm-parity handler (ED(2)
                        // erases in place instead of scrolling into history —
                        // see `vt::ParityTerm`), not the bare `Term`. The
                        // handler raises `forced_full_damage` under this same
                        // lock when an override mutates the grid outside
                        // alacritty's damage tracking (r5b renderer contract).
                        let mut guard = term.lock();
                        parser.advance(
                            &mut vt::ParityTerm::new(&mut *guard, &forced_full_damage),
                            chunk,
                        );
                    }
                    on_damage();
                } else if n == 0 {
                    break; // EOF (Linux-style): slave closed.
                } else {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    // EIO is the macOS "slave closed" EOF; any other error also
                    // means we can no longer read this pty — stop the feeder.
                    break;
                }
            }
        })
        .map_err(io::Error::from)
}
