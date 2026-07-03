//! `TermSession` — one live VT session: a spawned [`PtyProcess`] (slice 1)
//! joined to an `alacritty_terminal` `Term` behind a [`FairMutex`], with a
//! per-session **feeder thread** that reads the pty master off the render thread
//! and parses bytes into the `Term`, plus the damage-wake signal the renderer
//! drains on. This is the crate's exported threading shape (parse off-main,
//! share `Arc<FairMutex<Term>>`, wake via a callback).
//!
//! Out of scope here (a later R3 slice owns them, built on top of this value):
//! the deferred-spawn state machine (`NotSpawned → Spawning → Live → Exited`),
//! the typed outward event stream, and held-pane classification. `TermSession`
//! is the eager, already-live session those wrap — it exposes the raw exit
//! status (via the pty) that classification will consume, but does not classify.

use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

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
/// EOF and the thread ends. Drop joins the feeder **before** the pty's master fd
/// closes, so the feeder never reads a stale fd and no thread is left detached.
pub struct TermSession {
    pty: PtyProcess,
    term: SharedTerm,
    feeder: Option<JoinHandle<()>>,
    scrollback_lines: usize,
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
    pub fn spawn(
        spec: &SpawnSpec,
        scrollback_lines: usize,
        on_damage: DamageCallback,
    ) -> io::Result<TermSession> {
        let pty = PtyProcess::spawn(spec)?;
        let fd = pty.master_fd();

        let size = TermSize {
            rows: spec.rows as usize,
            cols: spec.cols as usize,
        };
        let config = Config {
            scrolling_history: scrollback_lines,
            ..Config::default()
        };
        let term: SharedTerm =
            Arc::new(FairMutex::new(Term::new(config, &size, EventProxy::new(fd))));

        let feeder = spawn_feeder(fd, Arc::clone(&term), on_damage)?;

        Ok(TermSession {
            pty,
            term,
            feeder: Some(feeder),
            scrollback_lines,
        })
    }

    /// [`TermSession::spawn`] with the parity [`DEFAULT_SCROLLBACK_LINES`] knob.
    pub fn spawn_default_scrollback(
        spec: &SpawnSpec,
        on_damage: DamageCallback,
    ) -> io::Result<TermSession> {
        TermSession::spawn(spec, DEFAULT_SCROLLBACK_LINES, on_damage)
    }

    /// The shared `Term` the renderer (R4) locks to paint. The renderer holds
    /// the lock only long enough to read cells for one frame; it must not hold
    /// it across a paint/present (mirror the owned grid read API below).
    pub fn term(&self) -> &SharedTerm {
        &self.term
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

    /// Force the child's process group to exit (SIGHUP, then SIGKILL after a
    /// grace) so no orphaned zsh survives. Idempotent; delegates to the pty. The
    /// feeder is joined on drop (once the closed slave EOFs its read).
    pub fn teardown(&self) {
        self.pty.teardown();
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        // Kill the child's group so the pty slave closes; the feeder's blocking
        // read then returns EOF/EIO and the thread ends. Join it here, while
        // `pty` (and the master fd the feeder reads) is still alive — the `pty`
        // field is dropped, closing the master, only after this body returns.
        self.pty.teardown();
        if let Some(handle) = self.feeder.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the per-session feeder thread: the single reader of this pty's master.
/// It blocking-reads bytes, parses them into the shared `Term` under a brief
/// lock, then — **after releasing the lock** — fires the damage wake. Ends when
/// the child's slave side closes (read EOF on Linux, `EIO` on macOS).
fn spawn_feeder(
    fd: RawFd,
    term: SharedTerm,
    on_damage: DamageCallback,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nice-term-feeder".to_string())
        .spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe {
                    libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n > 0 {
                    {
                        // Hold the lock ONLY to parse; the wake below is fired
                        // after this scope drops the guard (damage-wake contract).
                        let mut guard = term.lock();
                        parser.advance(&mut *guard, &buf[..n as usize]);
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
