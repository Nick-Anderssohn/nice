//! Per-window Unix-domain control socket (R14).
//!
//! Ports Swift `NiceControlSocket` (`Sources/Nice/Process/NiceControlSocket.swift`)
//! — a tiny AF_UNIX listener that lets Nice's shell helpers and Claude Code
//! skills talk to the app. One newline-delimited JSON object per client, then
//! close. Five actions — `claude` / `session_update` / `handoff` are FROZEN
//! (installed helpers on user disks already speak that protocol byte-for-byte,
//! see the plan's "wire protocol is FROZEN" decision):
//!
//!   * `claude`         — the shadowed `claude()` zsh function asking Nice to
//!                        open a new session or promote a window in place.
//!   * `session_update` — the SessionStart hook relaying session-id / cwd
//!                        rotations (fire-and-forget, no reply).
//!   * `handoff`        — the `/nice-handoff` skill's helper asking Nice to open
//!                        a nested handoff session.
//!   * `dispatch`       — the `/nice-dispatch` skill's helper asking Nice to open
//!                        a nested session running `claude --worktree <name> …` on a
//!                        task file the dispatcher wrote.
//!   * `claude_exited`  — the same `claude()` shadow reporting that the Claude it
//!                        ran as a CHILD has returned, so the window is a shell
//!                        prompt again. Only the `attach` reply verb produces
//!                        such a child (Fix D). Fire-and-forget, no reply.
//!
//! ## What differs from Swift (deliberately — plan "do not port the Swift
//! structure")
//!
//! Swift drives accept + self-healing off a `DispatchSource` on a serial
//! `stateQueue` and hops each message to `@MainActor` via a `@Sendable` closure.
//! We instead put **one dedicated OS thread** per window socket on the blocking
//! [`UnixListener::accept`] loop (§Threading below) and bridge parsed messages
//! onto the gpui foreground executor with a **waker-based** channel
//! ([`socket_channel`]) — NOT a coalescable timer poll. The App-Nap rationale is
//! the same one `platform::AppNapSafeDelay` documents: libdispatch timers are
//! deferred indefinitely on an idle/occluded app, and the wrapper only gives us
//! `nc -w 2` ≈ 2 s to reply, so the foreground drain must be woken by a
//! scheduler-level thread event plus `CFRunLoopWakeUp`, never a parked timer.
//!
//! ## Threading
//!
//! * The accept-loop thread owns a listener fd bound with a short
//!   [`SO_RCVTIMEO`](libc::SO_RCVTIMEO) so `accept()` returns on a cadence. That
//!   cadence lets the loop service three things without a second thread: the
//!   idempotent [`stop`](NiceControlSocket::stop) flag, the forced-rebind test
//!   seam ([`force_cancel_accept`](NiceControlSocket::force_cancel_accept)), and
//!   the periodic `stat()` health check that catches an externally-unlinked
//!   socket file. The dedicated thread makes the health cadence nap-proof for
//!   free (no libdispatch timer involved).
//! * Each accepted connection is read + parsed on its own short-lived client
//!   thread, so a stalled writer cannot wedge the accept loop (bounded further
//!   by a client read timeout).
//! * Self-healing: accept error / forced cancel / missing-file all funnel into
//!   the SAME rebind path — drop the listener, then rebind at the same `path`
//!   with capped exponential backoff (0.5 s base, 5 s cap, reset on success), so
//!   `NICE_SOCKET` in already-spawned shells stays correct across restarts.
//!
//! ## Path ownership
//!
//! A window's path is keyed on its persisted window id
//! ([`mint_window_socket_path`]) so it RECURS across app restarts — that is what
//! keeps a daemon-hosted Claude session's frozen `NICE_SOCKET` working after
//! Nice quits and reopens. A recurring path means the bind can no longer blindly
//! unlink what it finds, so an existing file is probed with a `connect(2)` first:
//! nothing answers ⇒ stale residue, unlink and take it; a live listener answers
//! ⇒ we never steal it (the initial `start` falls back to a legacy pid+nonce
//! path for that run, the self-heal loop just keeps retrying).
//!
//! That probe only tells the truth if the listener fd stays inside this process,
//! so it is marked close-on-exec ([`set_cloexec`]) — a pty child that inherited
//! it would answer the next launch's probe on behalf of the app that already
//! quit.
//!
//! ## Reply capability
//!
//! [`Reply`] owns the accepted [`UnixStream`] and is **consumed on use**
//! ([`Reply::send`] takes `self`): at-most-once by construction, stronger than
//! Swift's closure convention. `session_update` and `claude_exited` drop the
//! stream BEFORE dispatch (fire-and-forget); `claude` / `handoff` / `dispatch`
//! carry a `Reply` and answer once from the foreground.
//!
//! The window-side routing point + the five handlers live on
//! [`crate::window_state::WindowState`] (`route_socket_message`); R15/R16/R26
//! filled the first three's bodies, `dispatch` added a fourth arm and Fix D a
//! fifth (`claude_exited`), all without reshaping this socket. The `app::run`
//! bootstrap
//! (mint before the Main window's spawn, start the listener, spawn the foreground
//! drain, stop in teardown) is wired by the R14 env-injection slice — this
//! module only provides the mechanism, hence the module-wide `dead_code` allow
//! (the established pattern for a later-slice production consumer).

#![allow(dead_code)]

use std::future::Future;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// Maximum framed request size — one connection = one line ≤ 64 KiB (matches
/// Swift `readClient`'s `64 * 1024` cap).
const MAX_FRAME: usize = 64 * 1024;

/// macOS `sockaddr_un.sun_path` capacity (`[c_char; 104]`). A path needs one
/// trailing NUL byte, so the usable maximum is 103 bytes; anything `>= 104` is
/// rejected loudly rather than silently truncated (plan: "fail loudly, don't
/// truncate").
const SUN_PATH_CAP: usize = 104;

/// The accept-loop poll ceiling: the dedicated thread wakes at most this often
/// to service stop / forced-rebind / health, independent of the (possibly large)
/// health interval. Small enough that a forced cancel or `stop()` reacts well
/// inside the tests' 2 s budget; large enough that the idle thread is cheap.
const ACCEPT_POLL_CAP: Duration = Duration::from_millis(100);
/// Floor for the accept poll so a tiny health interval can't spin the thread.
const ACCEPT_POLL_MIN: Duration = Duration::from_millis(10);

/// Per-client read deadline: a well-behaved wrapper writes its single request
/// line immediately, so this only bounds a stalled/misbehaving writer. It is the
/// REQUEST read timeout, unrelated to the ~2 s reply deadline the foreground
/// owns.
const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Rate limit for the accept loop's "our path is held by a live foreign owner"
/// warning. The rebind itself retries on the ≤5 s backoff; the log must not.
const CONTESTED_LOG_INTERVAL: Duration = Duration::from_secs(30);

// ===========================================================================
// The FROZEN message enum + reply object
// ===========================================================================

/// Discriminated payload parsed off the control socket. Produced by
/// [`parse_message`], routed by
/// [`crate::window_state::WindowState::route_socket_message`]. R14 fixed the
/// four ported variants and R15/R16/R26 only filled their handler bodies; Fix D
/// added the fifth, [`SocketMessage::ClaudeExited`], for a message the Swift
/// app never had.
///
/// Mirrors Swift `enum SocketMessage`
/// (`NiceControlSocket.swift:43-144`). Every normalization rule the parser
/// applies is documented on [`parse_message`].
pub(crate) enum SocketMessage {
    /// `claude()` shadow asking whether to open a new sidebar session (default) or
    /// promote the sending window in place. `session_id` / `term_window_id` are empty strings
    /// for the Main Terminals session. The handler replies exactly once via `reply`
    /// with `newtab` / `inplace` / `inplace <session>` (+ optional settings
    /// pointer). `cwd` is required (may be empty); `args` defaults to `[]`.
    Claude {
        cwd: String,
        args: Vec<String>,
        session_id: String,
        term_window_id: String,
        reply: Reply,
    },
    /// Claude Code SessionStart hook reporting the active session UUID for the
    /// sending window. Fire-and-forget: the client fd is closed BEFORE dispatch,
    /// so this variant carries no [`Reply`]. `term_window_id` + `claude_session_id` are
    /// required non-empty; `source` / `cwd` are absent / empty / non-string
    /// normalized to `None` (older installed hooks predate these fields and must
    /// NOT be dropped).
    SessionUpdate {
        term_window_id: String,
        claude_session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    },
    /// The `claude()` shadow reporting that the Claude it ran as a CHILD has
    /// returned, so the sending window is a plain shell prompt again. Only the
    /// `attach` reply verb produces such a child — every other verb `exec`s,
    /// and a window whose pty exits clears itself through `window_held` — which
    /// is why nothing but the promotion flag needs undoing here. Fire-and-forget:
    /// the client fd is closed BEFORE dispatch, so this variant carries no
    /// [`Reply`]. `term_window_id` is required non-empty.
    ClaudeExited { term_window_id: String },
    /// `/nice-handoff` skill asking Nice to open a fresh Claude session nested
    /// under the originating session. `cwd` + `handoff_file` are required non-empty;
    /// `instructions` / `model` / `effort` / `session_id` / `term_window_id` are normalized
    /// to `""` (an older installed helper omits `model` / `effort` entirely and
    /// must still dispatch). The handler replies once with `ok` / `error: …`.
    Handoff {
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
        reply: Reply,
    },
    /// `/nice-dispatch` skill asking Nice to open a fresh Claude session nested
    /// under the originating session, running `claude --worktree <name>` on a task
    /// file the dispatcher wrote. `cwd` (the MAIN checkout root, resolved by the
    /// helper — the handler spawns from it, NOT from the originating session's live
    /// cwd), `worktree_name`, `task_file` and `term_window_id` are required non-empty;
    /// `instructions` / `model` / `effort` / `session_id` normalize to `""`. Unlike
    /// `handoff`, empty `model` / `effort` mean "omit the flag entirely" (the
    /// child runs on the user's configured default) — dispatch deliberately does
    /// NOT inherit the dispatcher's. The handler replies once with `ok` /
    /// `error: …`.
    Dispatch {
        cwd: String,
        worktree_name: String,
        task_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
        reply: Reply,
    },
}

/// Consume-on-use reply capability owning the accepted client [`UnixStream`].
///
/// [`send`](Reply::send) takes `self`, so a reply is at-most-once by
/// construction (the move-semantics upgrade over Swift's `@Sendable (String) ->
/// Void` closure convention). Dropping a `Reply` without replying simply closes
/// the fd — the wrapper's `nc -U … -w 2` then falls back to running `claude`
/// directly, preserving the "user always gets claude" property.
pub(crate) struct Reply {
    stream: UnixStream,
}

impl Reply {
    fn new(stream: UnixStream) -> Self {
        Reply { stream }
    }

    /// Write exactly one newline-terminated reply line and close the fd (drop).
    /// The installed wrapper parses replies with zsh `read -r mode sid settings`
    /// — NEVER append diagnostics (plan: replies are ≤ 3 whitespace-separated
    /// positional fields, one line). Write errors (peer closed early → `EPIPE`,
    /// which Rust's default `SIGPIPE`-ignore surfaces as an error rather than
    /// killing the process) are swallowed.
    pub(crate) fn send(self, line: &str) {
        let mut stream = self.stream;
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        let _ = stream.write_all(&buf);
        let _ = stream.flush();
        // `stream` drops here → fd closed.
    }

    /// Test seam: wrap an arbitrary stream (e.g. one half of
    /// [`UnixStream::pair`]) so the window-state routing tests can drive a
    /// handler with a real `Reply` and read the bytes off the other half.
    #[cfg(test)]
    pub(crate) fn for_test(stream: UnixStream) -> Self {
        Reply::new(stream)
    }
}

/// A parsed, normalized snapshot of a routed [`SocketMessage`] WITHOUT its reply
/// capability, recorded by the window routing point for the `shell-socket`
/// scenario and the routing unit tests (the raw-socket headless driver asserts
/// against these). Production accumulates nothing unless the `selftest` feature
/// is on (see `WindowState::record_socket_message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordedSocketMessage {
    Claude {
        cwd: String,
        args: Vec<String>,
        session_id: String,
        term_window_id: String,
    },
    SessionUpdate {
        term_window_id: String,
        claude_session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    },
    ClaudeExited {
        term_window_id: String,
    },
    Handoff {
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
    },
    Dispatch {
        cwd: String,
        worktree_name: String,
        task_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
    },
}

// ===========================================================================
// The listener
// ===========================================================================

/// Handler invoked once per parsed message, from a client thread. `Send + Sync`
/// because concurrent connections each call it from their own thread.
type Handler = Arc<dyn Fn(SocketMessage) + Send + Sync + 'static>;

/// Shared, thread-reachable listener state.
struct SocketShared {
    /// Bound socket path — exported via `NICE_SOCKET` into every pty; reused
    /// across rebinds so already-spawned shells stay correct. Immutable once the
    /// accept-loop thread exists: the only mutation is the D2 legacy fallback,
    /// which rebuilds the whole `SocketShared` inside the synchronous
    /// [`start`](NiceControlSocket::start), before any thread can read it.
    path: String,
    /// Set by [`NiceControlSocket::stop`] (and `Drop`) to suppress healing and
    /// unblock the accept loop.
    stop: AtomicBool,
    /// Set by the [`force_cancel_accept`](NiceControlSocket::force_cancel_accept)
    /// test seam to force a rebind as if the kernel had dropped the accept fd.
    force_rebind: AtomicBool,
    health_check_interval: Duration,
    initial_restart_delay: Duration,
}

/// One AF_UNIX control socket, owned by a window's state.
///
/// Allocation ([`new`](NiceControlSocket::new)) only mints the path (two-phase,
/// so the path can ride pty env before the listener arms);
/// [`start`](NiceControlSocket::start) binds + listens + spawns the accept-loop
/// thread.
pub(crate) struct NiceControlSocket {
    shared: Arc<SocketShared>,
    started: AtomicBool,
}

impl NiceControlSocket {
    /// Production defaults (Swift `init` defaults): 30 s health `stat()`, 0.5 s
    /// base restart backoff.
    pub(crate) fn new() -> Self {
        Self::with_intervals(Duration::from_secs(30), Duration::from_millis(500))
    }

    /// Allocate a socket with explicit healing intervals (tests pass small
    /// values). Mints the path immediately WITHOUT binding — honoring a
    /// `NICE_SOCKET_PATH` override (test seam), else
    /// `$TMPDIR/nice-<pid>-<suffix>.sock` (the exact pattern the `$TMPDIR` sweep
    /// parses; `<suffix>` is 8 hex chars, no `-`, so the pid is unambiguously the
    /// segment after `nice-`).
    pub(crate) fn with_intervals(
        health_check_interval: Duration,
        initial_restart_delay: Duration,
    ) -> Self {
        Self::with_path_and_intervals(
            mint_socket_path(),
            health_check_interval,
            initial_restart_delay,
        )
    }

    /// Allocate a socket at a caller-chosen `path` with production healing
    /// intervals. The window-keyed production entry point: `app::arm_window_control_socket`
    /// passes [`mint_window_socket_path`]`(window_id)` so the path is stable
    /// across app restarts and a long-lived session's frozen `NICE_SOCKET` keeps
    /// working. Still two-phase — nothing binds until [`start`](Self::start).
    pub(crate) fn with_path(path: String) -> Self {
        Self::with_path_and_intervals(path, Duration::from_secs(30), Duration::from_millis(500))
    }

    /// [`with_path`](Self::with_path) with explicit healing intervals — the
    /// scenario/test combination (a window-keyed path AND a fast health cadence).
    pub(crate) fn with_path_and_intervals(
        path: String,
        health_check_interval: Duration,
        initial_restart_delay: Duration,
    ) -> Self {
        NiceControlSocket {
            shared: Arc::new(SocketShared {
                path,
                stop: AtomicBool::new(false),
                force_rebind: AtomicBool::new(false),
                health_check_interval,
                initial_restart_delay,
            }),
            started: AtomicBool::new(false),
        }
    }

    /// The bound (or to-be-bound) socket path — injected into pty env as
    /// `NICE_SOCKET` at window construction, before any pty forks. Read it
    /// **after** [`start`](Self::start): that call resolves the final path (the
    /// D2 fallback can move it off a contested one).
    pub(crate) fn path(&self) -> &str {
        &self.shared.path
    }

    /// Bind, listen, and spawn the accept-loop thread with `handler`. Safe to
    /// call once; a second call is a no-op. Bind failure is **non-fatal** and
    /// reported as `Err` (the caller logs + continues — shells fall back to
    /// direct `command claude`, preserving "user always gets claude"). On the
    /// happy path the listener is accepting by the time this returns, so a client
    /// may connect immediately.
    ///
    /// **The path can change here (D2).** With a window-keyed path, another live
    /// listener may already own it (a same-user squatter, or the negligible
    /// truncated-id collision). We never steal a live socket: this run falls back
    /// once to a legacy `nice-<pid>-<8hex>` path and logs. Callers must therefore
    /// read [`path`](Self::path) **after** `start` returns — that is the path
    /// shells must get as `NICE_SOCKET`.
    ///
    /// Takes `&mut self` for that swap; the fallback rebuilds `SocketShared`
    /// before the accept thread spawns, so no reader can observe it and no lock
    /// is needed.
    pub(crate) fn start<F>(&mut self, handler: F) -> io::Result<()>
    where
        F: Fn(SocketMessage) + Send + Sync + 'static,
    {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        let accept_poll = accept_poll_for(self.shared.health_check_interval);
        // Synchronous initial bind so the caller sees success/failure now and a
        // client can connect on return (matches Swift `start` throwing on bind
        // failure before the source resumes).
        let listener = match bind_and_listen(&self.shared.path) {
            Ok(l) => l,
            Err(e) if path_contested(&e) => {
                // D2: a live owner holds our stable path. Fall back to a legacy
                // pid+nonce path for this run — this window stays fully
                // functional, just not restart-stable (the pre-fix behavior).
                let fallback = mint_socket_path();
                eprintln!(
                    "nice: control socket path {} is held by a live listener ({e}); using \
                     {fallback} for this run (this window is not restart-stable)",
                    self.shared.path
                );
                self.replace_path(fallback);
                // Single-shot: if the fallback bind fails too, report it. Never
                // loop — under a `NICE_SOCKET_PATH` override the "fallback" path
                // is the SAME path, so a retry would spin forever.
                bind_and_listen(&self.shared.path)?
            }
            Err(e) => return Err(e),
        };
        let handler: Handler = Arc::new(handler);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("nice-control-socket".into())
            .spawn(move || accept_loop(listener, shared, handler, accept_poll));
        match spawned {
            Ok(_) => {
                self.started.store(true, Ordering::Release);
                Ok(())
            }
            Err(e) => {
                // Could not spawn the loop — undo the bind so no dead socket file
                // lingers, and report non-fatally.
                let _ = std::fs::remove_file(&self.shared.path);
                Err(io::Error::new(io::ErrorKind::Other, e))
            }
        }
    }

    /// Rebuild [`SocketShared`] at a new `path` (the D2 fallback). Only legal
    /// from the synchronous part of [`start`](Self::start), before the accept
    /// thread spawns: the sole `Arc::clone` happens after the bind succeeds, so
    /// nothing else can be reading the old state.
    fn replace_path(&mut self, path: String) {
        self.shared = Arc::new(SocketShared {
            path,
            stop: AtomicBool::new(self.shared.stop.load(Ordering::Acquire)),
            force_rebind: AtomicBool::new(false),
            health_check_interval: self.shared.health_check_interval,
            initial_restart_delay: self.shared.initial_restart_delay,
        });
    }

    /// Stop accepting, suppress healing, and unlink the socket file. Idempotent
    /// (Swift `stop` contract). The accept-loop thread observes the flag within
    /// one accept-poll and exits, cleaning up its listener fd.
    pub(crate) fn stop(&self) {
        // Set the flag BEFORE unlinking so the loop's top-of-iteration stop check
        // wins over a health-check-driven rebind racing the unlink.
        self.shared.stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.shared.path);
    }

    /// Test seam: force the accept loop to drop its listener and rebind at the
    /// same path, as if the kernel had dropped the accept fd. The self-healing
    /// path rebuilds without any external trigger. Production never calls this.
    #[cfg(test)]
    pub(crate) fn force_cancel_accept(&self) {
        self.shared.force_rebind.store(true, Ordering::Release);
    }
}

impl Drop for NiceControlSocket {
    fn drop(&mut self) {
        // Signal the accept-loop thread to exit even if `stop` was never called
        // explicitly, so a dropped socket never leaks its background thread.
        self.stop();
    }
}

/// The accept-loop body (one dedicated OS thread per window socket). Owns the
/// initial listener and every rebind; ends only when `stop` is set.
///
/// The listener is **non-blocking**, and the loop parks in `poll()` for at most
/// `accept_poll` waiting for a connection. `poll()` (unlike a blocking
/// `accept()` under `SO_RCVTIMEO`, which BSD does not honor for accept) is the
/// portable way to wake the loop on a cadence so it can service the `stop` flag,
/// the forced-rebind seam, and the periodic health `stat()` — all on the
/// dedicated thread, which makes the health cadence nap-proof for free.
fn accept_loop(
    initial: UnixListener,
    shared: Arc<SocketShared>,
    handler: Handler,
    accept_poll: Duration,
) {
    let mut listener: Option<UnixListener> = Some(initial);
    // Mirrors Swift `restartAttempt`: 0 while healthy; drives the backoff and
    // resets to 0 on a successful bind.
    let mut restart_attempt: u32 = 0;
    let mut last_health = Instant::now();
    // Rate limiter for the "someone else owns our path" rebind log, so a
    // persistent foreign owner is visible without a ≤5 s spam loop.
    let mut last_contested_log: Option<Instant> = None;
    let poll_ms = accept_poll.as_millis().min(i32::MAX as u128) as i32;

    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }

        if listener.is_none() {
            // Backoff then rebind. First rebind after a healthy run uses exp=0 →
            // `initial_restart_delay`; consecutive failures grow it, capped 5 s.
            let exp = restart_attempt.min(20);
            let delay = shared
                .initial_restart_delay
                .checked_mul(1u32 << exp)
                .unwrap_or(Duration::from_secs(5))
                .min(Duration::from_secs(5));
            restart_attempt = restart_attempt.saturating_add(1);
            if !sleep_interruptible(delay, &shared.stop, accept_poll) {
                break; // stop() fired during the backoff
            }
            match bind_and_listen(&shared.path) {
                Ok(l) => {
                    listener = Some(l);
                    restart_attempt = 0;
                    last_health = Instant::now();
                }
                Err(e) => {
                    // Includes the "live foreign owner" verdict: keep retrying at
                    // the same path (never switch — `NICE_SOCKET` is already
                    // stamped in this window's shells), so we reclaim it the
                    // moment the peer frees it.
                    if path_contested(&e)
                        && last_contested_log
                            .is_none_or(|t| t.elapsed() >= CONTESTED_LOG_INTERVAL)
                    {
                        last_contested_log = Some(Instant::now());
                        eprintln!("nice: control socket cannot rebind — {e}; retrying");
                    }
                    continue; // retry with more backoff
                }
            }
        }

        let l = listener.as_ref().expect("listener present after rebind");
        let revents = poll_readable(l.as_raw_fd(), poll_ms);

        // Service the healing signals on every wake (poll timeout OR readable);
        // all three are cheap and idempotent.
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        if shared.force_rebind.swap(false, Ordering::AcqRel) {
            // Forced cancel: rebind now (restart_attempt is 0 after a healthy
            // run, so no backoff before the immediate rebind).
            listener = None;
            continue;
        }
        if last_health.elapsed() >= shared.health_check_interval {
            last_health = Instant::now();
            if !Path::new(&shared.path).exists() {
                // Socket file vanished (unlinked externally) — funnel into the
                // same single rebind path, not a second one.
                listener = None;
                continue;
            }
        }
        if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            // Listener fd went bad — drop + rebind with backoff.
            listener = None;
            continue;
        }
        if revents & libc::POLLIN != 0 {
            match l.accept() {
                Ok((stream, _)) => dispatch_client(stream, &handler),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => listener = None, // real accept error → rebind
            }
        }
    }

    // Stopped: drop the listener (closes the fd) and unlink the file so no stale
    // socket lingers (idempotent with stop()'s own unlink).
    drop(listener);
    let _ = std::fs::remove_file(&shared.path);
}

/// `poll()` the listener fd for `POLLIN` with a `timeout_ms` cap, returning the
/// `revents`. A poll error / `EINTR` is reported as a quiet tick (0 revents) so
/// the caller re-services its flags and loops.
fn poll_readable(fd: RawFd, timeout_ms: i32) -> libc::c_short {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` reads/writes the single valid `pollfd` for `timeout_ms`.
    let rc = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, timeout_ms) };
    if rc < 0 {
        0
    } else {
        pfd.revents
    }
}

/// Spawn a short-lived thread to read + parse one connection and invoke the
/// handler, so a stalled writer never wedges the accept loop. If the thread
/// can't spawn, the stream drops here (client sees a closed connection and falls
/// back to direct `claude`).
fn dispatch_client(stream: UnixStream, handler: &Handler) {
    let handler = Arc::clone(handler);
    let _ = std::thread::Builder::new()
        .name("nice-control-client".into())
        .spawn(move || handle_client(stream, &handler));
}

/// Read the framed request line, parse it, and dispatch. On any failure the
/// stream is dropped (fd closed) with no reply — the silent-drop contract.
fn handle_client(mut stream: UnixStream, handler: &Handler) {
    // The listener is non-blocking; force the accepted stream BLOCKING so the
    // timed read below waits for the request line rather than returning
    // `WouldBlock` before the client's write lands (accepted sockets do not
    // reliably inherit the listener's mode across platforms).
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(CLIENT_READ_TIMEOUT));
    let line = match read_framed_line(&mut stream) {
        Some(l) => l,
        None => return, // empty request → close (Swift: `guard !buffer.isEmpty`)
    };
    if let Some(msg) = parse_message(&line, stream) {
        handler(msg);
    }
    // `None` → parse_message already dropped the stream.
}

/// Read up to the first `\n` or [`MAX_FRAME`] bytes, then return the bytes
/// before the newline. `None` when nothing was read (an empty request).
fn read_framed_line(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < MAX_FRAME {
        match stream.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break, // read timeout / other → stop reading
        }
    }
    if buf.is_empty() {
        return None;
    }
    if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        buf.truncate(nl);
    }
    Some(buf)
}

// ===========================================================================
// Parse + normalization (the FROZEN protocol)
// ===========================================================================

/// Parse one request line into a [`SocketMessage`], taking ownership of the
/// client `stream` so `session_update` can close it before dispatch and
/// `claude` / `handoff` / `dispatch` can carry it in a [`Reply`]. Returns `None`
/// (dropping the stream → silent close, no reply) for malformed JSON, a
/// non-object, a missing/unknown `action`, or a missing required field.
///
/// Every rule below is the FROZEN contract shared with installed helpers
/// (Swift `readClient`, `NiceControlSocket.swift:382-511`):
///   * `args`: an all-strings JSON array, else `[]` (Swift `as? [String] ?? []`).
///   * `claude.cwd`: required string (may be empty); `tabId`/`paneId` → `""`.
///   * `session_update`: `paneId` + `sessionId` required non-empty; `source` /
///     `cwd` absent / non-string / empty all normalize to `None`.
///   * `handoff`: `cwd` + `handoffFile` required non-empty; `instructions` /
///     `model` / `effort` / `tabId` / `paneId` normalize to `""` (an older
///     helper omitting `model`/`effort` must still dispatch, not drop).
///   * `dispatch`: `cwd` + `worktreeName` + `taskFile` + `paneId` required
///     non-empty (unlike `handoff`, a dispatch without a sending window cannot
///     nest and is dropped); `instructions` / `model` / `effort` / `tabId`
///     normalize to `""`.
fn parse_message(line: &[u8], stream: UnixStream) -> Option<SocketMessage> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let obj = value.as_object()?; // non-object → drop
    let action = obj.get("action").and_then(|v| v.as_str())?; // missing/non-string → drop

    match action {
        "claude" => {
            let cwd = obj.get("cwd").and_then(|v| v.as_str())?.to_string();
            let args = parse_string_array(obj.get("args"));
            let session_id = str_or_empty(obj, "tabId");
            let term_window_id = str_or_empty(obj, "paneId");
            Some(SocketMessage::Claude {
                cwd,
                args,
                session_id,
                term_window_id,
                reply: Reply::new(stream),
            })
        }
        "session_update" => {
            let term_window_id = non_empty(obj, "paneId")?;
            let claude_session_id = non_empty(obj, "sessionId")?;
            let source = normalize_opt(obj, "source");
            let cwd = normalize_opt(obj, "cwd");
            // Fire-and-forget: close the fd BEFORE dispatch so the hook's `nc`
            // returns promptly even if the foreground is backed up.
            drop(stream);
            Some(SocketMessage::SessionUpdate {
                term_window_id,
                claude_session_id,
                source,
                cwd,
            })
        }
        "claude_exited" => {
            let term_window_id = non_empty(obj, "paneId")?;
            // Fire-and-forget, same as `session_update`: close the fd BEFORE
            // dispatch so the wrapper's `nc` returns to the user's prompt
            // immediately rather than waiting on the foreground.
            drop(stream);
            Some(SocketMessage::ClaudeExited { term_window_id })
        }
        "handoff" => {
            let cwd = non_empty(obj, "cwd")?;
            let handoff_file = non_empty(obj, "handoffFile")?;
            let session_id = str_or_empty(obj, "tabId");
            let term_window_id = str_or_empty(obj, "paneId");
            let instructions = str_or_empty(obj, "instructions");
            let model = str_or_empty(obj, "model");
            let effort = str_or_empty(obj, "effort");
            Some(SocketMessage::Handoff {
                cwd,
                handoff_file,
                instructions,
                model,
                effort,
                session_id,
                term_window_id,
                reply: Reply::new(stream),
            })
        }
        "dispatch" => {
            let cwd = non_empty(obj, "cwd")?;
            let worktree_name = non_empty(obj, "worktreeName")?;
            let task_file = non_empty(obj, "taskFile")?;
            let term_window_id = non_empty(obj, "paneId")?;
            let session_id = str_or_empty(obj, "tabId");
            let instructions = str_or_empty(obj, "instructions");
            let model = str_or_empty(obj, "model");
            let effort = str_or_empty(obj, "effort");
            Some(SocketMessage::Dispatch {
                cwd,
                worktree_name,
                task_file,
                instructions,
                model,
                effort,
                session_id,
                term_window_id,
                reply: Reply::new(stream),
            })
        }
        _ => None, // unknown action → log-and-drop (silent)
    }
}

/// Swift `(obj[key] as? [String]) ?? []`: an array whose every element is a
/// string, else empty.
fn parse_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        match e.as_str() {
            Some(s) => out.push(s.to_string()),
            None => return Vec::new(), // any non-string element → cast fails → []
        }
    }
    out
}

/// Swift `(obj[key] as? String) ?? ""` — a string value, else `""`.
fn str_or_empty(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// A string value that is non-empty, else `None`. Used both for required fields
/// (`?`-propagated to a silent drop) and for `source`/`cwd` normalization — the
/// two share the identical "absent / non-string / empty → None" rule.
fn non_empty(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Alias for [`non_empty`] at the normalization call sites, where the `None`
/// means "not provided" rather than "drop the message".
fn normalize_opt(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    non_empty(obj, key)
}

// ===========================================================================
// Bind / listen helpers
// ===========================================================================

/// Error payload for "a live listener already owns this path" — the probe
/// verdict that must never be stolen (D2/D4). Carried inside an [`io::Error`]
/// so every existing `io::Result` path (notably the accept loop's
/// `Err(_) => continue` retry) handles it unchanged; [`path_contested`]
/// recognizes it where the behavior must differ.
#[derive(Debug)]
struct OwnedElsewhere {
    path: String,
}

impl std::fmt::Display for OwnedElsewhere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "control socket {} is owned by a live listener", self.path)
    }
}

impl std::error::Error for OwnedElsewhere {}

fn owned_elsewhere(path: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::AddrInUse,
        OwnedElsewhere {
            path: path.to_string(),
        },
    )
}

/// True when the bind failed because someone else holds the path: our probe saw
/// a live owner, or `bind` reported `EADDRINUSE` even after the retry. The
/// initial `start()` answers this with the D2 legacy fallback; the accept loop
/// answers it by retrying forever at the same (stable) path — the peer may quit
/// and free it, at which point the already-stamped `NICE_SOCKET` becomes right
/// again.
fn path_contested(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<OwnedElsewhere>())
        || e.raw_os_error() == Some(libc::EADDRINUSE)
}

/// Probe an existing socket file for a live owner (D5): a plain blocking
/// `connect(2)`, no bytes written. On macOS AF_UNIX this returns immediately
/// (`ECONNREFUSED` for an orphaned file — and for a full backlog, which the
/// health-check rebind absorbs), so no deadline machinery is needed. The
/// zero-byte connection is safe by the server's own contract: `read_framed_line`
/// returns `None` on EOF and the handler closes silently.
///
/// **Only a SUCCESSFUL connect proves a live owner** (D5's error taxonomy).
/// Every failure — refused, `ENOENT` (unlinked under us), not-a-socket, junk
/// residue — counts as stale, because this bind must end with a working socket
/// for THIS window.
fn is_live_owner(path: &str) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Bind with the D6 TOCTOU backstop: a live peer can re-create the file between
/// our probe and our `bind`, surfacing as `EADDRINUSE`. Re-run probe+bind once;
/// a second failure is reported to the caller (initial `start()` → D2 fallback;
/// accept loop → retry with backoff).
fn bind_and_listen(path: &str) -> io::Result<UnixListener> {
    match bind_and_listen_once(path) {
        Err(ref e) if e.raw_os_error() == Some(libc::EADDRINUSE) => bind_and_listen_once(path),
        other => other,
    }
}

/// Mark `fd` close-on-exec.
///
/// The listener fd MUST NOT survive into a pty child. Every shell and every
/// `claude` Nice forks would otherwise inherit an open reference to the bound
/// socket, and those children routinely outlive the app (daemon-hosted Claude
/// sessions, `nohup`'d servers, disowned shells). A held reference keeps
/// `connect(2)` on the path succeeding after the app exits, so the next launch's
/// probe ([`is_live_owner`]) reports a live owner and takes the D2 legacy
/// fallback — against Nice's own orphans. The window then loses exactly the
/// restart stability the window-keyed path exists to provide, and the frozen
/// `NICE_SOCKET` of a pre-restart session connects to a socket nobody accepts
/// (hangs to timeout instead of failing fast).
///
/// Only the raw `libc::socket` above needs this: `UnixStream::connect` and
/// `UnixListener::accept` in std already set `FD_CLOEXEC` themselves (macOS has
/// no `SOCK_CLOEXEC`/`accept4`, so std does the same `fcntl` dance), pinned by
/// `socket_fds_are_close_on_exec`.
fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is a valid open fd owned by the caller; `F_SETFD` takes an
    // int, no pointers involved.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `probe(path)` → `socket(AF_UNIX, SOCK_STREAM)` → `cloexec` →
/// `unlink(path)` → `bind` → `chmod 0600` → `listen(8)`. Ports Swift `bindAndListenLocked`
/// (`NiceControlSocket.swift:244-311`), except the unlink is no longer
/// unconditional: with restart-stable paths a blind unlink would steal a live
/// peer's socket, so an existing file is probed first and only cleared when
/// nothing answers. Returns a **non-blocking** [`UnixListener`]; the accept loop
/// parks in `poll()` and accepts only when a connection is pending.
fn bind_and_listen_once(path: &str) -> io::Result<UnixListener> {
    let bytes = path.as_bytes();
    if bytes.len() >= SUN_PATH_CAP {
        // Fail loudly, never truncate (a truncated path would bind the wrong
        // file and silently break every shell's `nc`).
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "control socket path is {} bytes, exceeds sun_path capacity {}: {}",
                bytes.len(),
                SUN_PATH_CAP,
                path
            ),
        ));
    }

    // Arbitrate ownership before touching the file: a live listener keeps it
    // (the caller decides what to do instead), anything else is stale residue we
    // clear — a prior crashed run, the listener we are replacing right now, or a
    // non-socket file squatting the name.
    if Path::new(path).exists() {
        if is_live_owner(path) {
            return Err(owned_elsewhere(path));
        }
        let _ = std::fs::remove_file(path);
    }

    // SAFETY: `socket` with AF_UNIX/SOCK_STREAM returns a new fd (or -1); no
    // arguments are pointers.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Own the fd immediately so every error path below closes it on drop.
    // SAFETY: `fd` is a fresh, exclusively-owned socket fd.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };

    // Close-on-exec, or every pty child inherits the listener (see
    // `set_cloexec`). Load-bearing for restart stability, not hygiene.
    set_cloexec(owned.as_raw_fd())?;

    // Build the AF_UNIX address. The struct is zero-initialized, so the guard
    // above (`len < SUN_PATH_CAP`) guarantees a trailing NUL remains.
    // SAFETY: `sockaddr_un` is plain-old-data; an all-zero value is valid.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i] = *b as libc::c_char;
    }
    let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

    // SAFETY: `owned` is a valid socket fd; `addr` is a fully-initialized
    // sockaddr_un of `addr_len` bytes.
    let rc = unsafe {
        libc::bind(
            owned.as_raw_fd(),
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error()); // `owned` drops → fd closed
    }

    // Defense in depth — $TMPDIR is already per-user, but force 0600 so nothing
    // else on the system can connect.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));

    // SAFETY: `owned` is a bound socket fd; backlog 8 matches Swift `listen(_, 8)`.
    let rc = unsafe { libc::listen(owned.as_raw_fd(), 8) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        let _ = std::fs::remove_file(path);
        return Err(err); // `owned` drops → fd closed
    }

    // SAFETY: transfer the fd's ownership to the UnixListener; `into_raw_fd`
    // releases it from `owned` without closing.
    let listener = unsafe { UnixListener::from_raw_fd(owned.into_raw_fd()) };
    // Non-blocking so the accept loop can park in `poll()` on its own cadence.
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// The accept-poll cadence: the health interval clamped into
/// `[ACCEPT_POLL_MIN, ACCEPT_POLL_CAP]` so a large health interval still lets the
/// loop react to stop / forced-cancel promptly, and a tiny one can't spin.
fn accept_poll_for(health: Duration) -> Duration {
    health.min(ACCEPT_POLL_CAP).max(ACCEPT_POLL_MIN)
}

/// Sleep `delay`, waking every `chunk` to check `stop`. Returns `false` if `stop`
/// was observed (caller should exit), `true` if the full delay elapsed.
fn sleep_interruptible(delay: Duration, stop: &AtomicBool, chunk: Duration) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep(deadline.saturating_duration_since(now).min(chunk));
    }
}

/// Mint the LEGACY socket path: `NICE_SOCKET_PATH` override (test seam) else
/// `$TMPDIR/nice-<pid>-<suffix>.sock`. Still the default for window-less call
/// sites (unit tests, the teardown seam) and the D2 fallback when a window's
/// stable path is held by a live owner.
fn mint_socket_path() -> String {
    if let Ok(over) = std::env::var("NICE_SOCKET_PATH") {
        return over;
    }
    let name = format!("nice-{}-{}.sock", std::process::id(), mint_suffix());
    std::env::temp_dir()
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Mint a window's RESTART-STABLE socket path: `$TMPDIR/nice-w-<12hex>.sock`,
/// where `<12hex>` is the first 12 hex chars of the persisted window id
/// (`PersistedWindow.id`, a UUIDv4) with the dashes stripped.
///
/// The whole point is that this recurs: the id is written to `sessions.json` and
/// restored verbatim, so a session's frozen `NICE_SOCKET` still names its
/// window's socket after Nice quits and reopens. 48 bits of UUID entropy makes a
/// cross-window collision negligible, and the bind probe handles it gracefully
/// if it ever happens. The `w-` discriminator keeps the `$TMPDIR` sweep's legacy
/// pid parser inert on these names (`"w"` is not an `i32`), and the 24-byte
/// filename is shorter than the legacy one, so the `sun_path` headroom only
/// improves. `NICE_SOCKET_PATH` still overrides everything (test seam).
pub(crate) fn mint_window_socket_path(window_id: &str) -> String {
    if let Ok(over) = std::env::var("NICE_SOCKET_PATH") {
        return over;
    }
    let key: String = window_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(12)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    std::env::temp_dir()
        .join(format!("nice-w-{key}.sock"))
        .to_string_lossy()
        .into_owned()
}

/// 8 lowercase hex chars, unique-enough per window within a process (Swift uses
/// `UUID().uuidString.prefix(8)`). No `-`, so the `$TMPDIR` sweep reads the pid
/// as the segment right after `nice-`.
fn mint_suffix() -> String {
    use std::hash::{Hash, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (n, nanos, std::process::id()).hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

// ===========================================================================
// The waker-based foreground-drain bridge (App-Nap-safe, per plan decision)
// ===========================================================================

/// Shared readiness signal between the message poster (client threads) and the
/// gpui foreground drain future. `notified` coalesces many posts into one wake;
/// `waker` is the parked foreground task's waker.
struct DrainShared {
    notified: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

/// Producer half handed to [`NiceControlSocket::start`] as the socket handler:
/// `move |msg| sender.post(msg)`. Cloneable + `Send + Sync` so concurrent client
/// threads can post.
#[derive(Clone)]
pub(crate) struct SocketSender {
    tx: Arc<Mutex<Sender<SocketMessage>>>,
    shared: Arc<DrainShared>,
}

impl SocketSender {
    /// Enqueue a parsed message and wake the foreground drain App-Nap-safely:
    /// fire the parked `Waker` AND `CFRunLoopWakeUp(CFRunLoopGetMain())` — the
    /// same belt-and-suspenders `platform::AppNapSafeDelay` uses, because the
    /// wrapper's `nc -w 2` gives the foreground only ~2 s to reply and a napped
    /// window's coalescable timer would miss that deadline.
    pub(crate) fn post(&self, msg: SocketMessage) {
        match self.tx.lock() {
            Ok(tx) => {
                if tx.send(msg).is_err() {
                    return; // receiver gone (window closed) — drop
                }
            }
            Err(_) => return,
        }
        self.shared.notified.store(true, Ordering::Release);
        if let Some(w) = self.shared.waker.lock().unwrap().take() {
            w.wake();
        }
        crate::platform::wake_main_runloop();
    }
}

/// Consumer half owned by the gpui foreground drain task (spawned by the R14
/// env-injection slice's `open_managed_window` wiring). Each wake, the task
/// drains every queued message through the window routing point, then parks on
/// [`readable`](SocketReceiver::readable) again.
pub(crate) struct SocketReceiver {
    rx: Receiver<SocketMessage>,
    shared: Arc<DrainShared>,
}

impl SocketReceiver {
    /// Pop the next queued message without blocking. `Err(Empty)` = nothing
    /// pending (park via [`readable`](SocketReceiver::readable)); `Err(Disconnected)`
    /// = the socket stopped (all senders dropped) → the drain loop should exit.
    pub(crate) fn try_recv(&self) -> Result<SocketMessage, TryRecvError> {
        self.rx.try_recv()
    }

    /// A future that resolves as soon as a message is (or already was) posted,
    /// parking the foreground task's waker where the poster thread reaches it.
    /// Waker-based, never timer-polled — the App-Nap-safe drain (plan decision).
    pub(crate) fn readable(&self) -> SocketReady {
        SocketReady {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// The park future the foreground drain awaits. Resolves `Ready` if a message is
/// pending, else stores the waker and re-checks (double-check to avoid a lost
/// wakeup racing the poster).
pub(crate) struct SocketReady {
    shared: Arc<DrainShared>,
}

impl Future for SocketReady {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.shared.notified.swap(false, Ordering::AcqRel) {
            return Poll::Ready(());
        }
        *self.shared.waker.lock().unwrap() = Some(cx.waker().clone());
        // Re-check after parking so a post that landed between the first check
        // and the store is not lost.
        if self.shared.notified.swap(false, Ordering::AcqRel) {
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

/// Build the poster/receiver pair bridging client threads → gpui foreground.
pub(crate) fn socket_channel() -> (SocketSender, SocketReceiver) {
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(DrainShared {
        notified: AtomicBool::new(false),
        waker: Mutex::new(None),
    });
    (
        SocketSender {
            tx: Arc::new(Mutex::new(tx)),
            shared: Arc::clone(&shared),
        },
        SocketReceiver { rx, shared },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- client helpers (raw AF_UNIX, hermetic; no shelling to `nc`) --------

    fn connect(path: &str) -> Option<UnixStream> {
        let s = UnixStream::connect(path).ok()?;
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
        Some(s)
    }

    fn read_line(stream: &mut UnixStream) -> Option<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 256];
        while buf.len() < 1024 {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.contains(&b'\n') {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if buf.is_empty() {
            return None;
        }
        if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            buf.truncate(nl);
        }
        String::from_utf8(buf).ok()
    }

    /// Connect, send a `claude` request, read one reply line. `None` if any step
    /// fails (unreachable socket / no reply) — the "not yet recovered" signal.
    fn send_claude(path: &str) -> Option<String> {
        let mut s = connect(path)?;
        s.write_all(br#"{"action":"claude","cwd":"/tmp","args":[],"tabId":"","paneId":""}"#)
            .ok()?;
        s.write_all(b"\n").ok()?;
        read_line(&mut s)
    }

    /// Fire-and-forget: connect, send a raw payload + newline, close.
    fn send_raw(path: &str, payload: &str) {
        if let Some(mut s) = connect(path) {
            let _ = s.write_all(payload.as_bytes());
            let _ = s.write_all(b"\n");
        }
    }

    /// Send a payload and read one reply line (for handoff reply plumbing).
    fn send_and_read(path: &str, payload: &str) -> Option<String> {
        let mut s = connect(path)?;
        s.write_all(payload.as_bytes()).ok()?;
        s.write_all(b"\n").ok()?;
        read_line(&mut s)
    }

    fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cond()
    }

    /// Handler that answers every `claude` as `newtab` (and `handoff` as `ok`)
    /// so the self-healing tests have a live responder. Mirrors Swift
    /// `replyNewtabHandler`.
    fn reply_newtab_handler(msg: SocketMessage) {
        match msg {
            SocketMessage::Claude { reply, .. } => reply.send("newtab"),
            SocketMessage::Handoff { reply, .. } => reply.send("ok"),
            SocketMessage::Dispatch { reply, .. } => reply.send("ok"),
            SocketMessage::SessionUpdate { .. } => {}
            SocketMessage::ClaudeExited { .. } => {}
        }
    }

    /// Thread-safe collector for dispatched `session_update` messages (the
    /// handler fires from a client thread, so a bare Vec would race the test).
    #[derive(Clone, Default)]
    struct CapturedUpdates {
        items: Arc<Mutex<Vec<(String, String, Option<String>, Option<String>)>>>,
    }
    impl CapturedUpdates {
        fn handler(&self) -> impl Fn(SocketMessage) + Send + Sync + 'static {
            let items = Arc::clone(&self.items);
            move |msg| match msg {
                SocketMessage::SessionUpdate {
                    term_window_id,
                    claude_session_id,
                    source,
                    cwd,
                } => items.lock().unwrap().push((term_window_id, claude_session_id, source, cwd)),
                SocketMessage::Claude { reply, .. } => reply.send("newtab"),
                SocketMessage::Handoff { reply, .. } => reply.send("ok"),
                SocketMessage::Dispatch { reply, .. } => reply.send("ok"),
                SocketMessage::ClaudeExited { .. } => {}
            }
        }
        fn count(&self) -> usize {
            self.items.lock().unwrap().len()
        }
        fn wait_one(&self) -> Option<(String, String, Option<String>, Option<String>)> {
            wait_for(Duration::from_secs(1), || self.count() >= 1);
            self.items.lock().unwrap().first().cloned()
        }
    }

    // ---- NiceControlSocketTests (self-healing trio) -------------------------

    #[test]
    fn restarts_after_accept_source_cancel() {
        // Long health-check so ONLY the forced-cancel path is under test.
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(reply_newtab_handler).unwrap();

        assert_eq!(
            send_claude(socket.path()).as_deref(),
            Some("newtab"),
            "socket should respond before the forced cancel"
        );

        socket.force_cancel_accept();

        let path = socket.path().to_string();
        assert!(
            wait_for(Duration::from_secs(2), || send_claude(&path).as_deref()
                == Some("newtab")),
            "socket should self-heal after a forced accept cancel"
        );
    }

    #[test]
    fn restarts_when_socket_file_removed() {
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_millis(50),
            Duration::from_millis(20),
        );
        socket.start(reply_newtab_handler).unwrap();

        assert_eq!(send_claude(socket.path()).as_deref(), Some("newtab"));

        std::fs::remove_file(socket.path()).expect("could not unlink socket for test");
        assert!(
            !Path::new(socket.path()).exists(),
            "precondition: socket file gone after unlink"
        );

        let path = socket.path().to_string();
        assert!(
            wait_for(Duration::from_secs(2), || {
                Path::new(&path).exists() && send_claude(&path).as_deref() == Some("newtab")
            }),
            "health check should rebuild the listener after the file is removed"
        );
    }

    #[test]
    fn stop_prevents_restart() {
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_millis(50),
            Duration::from_millis(20),
        );
        socket.start(reply_newtab_handler).unwrap();
        let path = socket.path().to_string();

        assert_eq!(send_claude(&path).as_deref(), Some("newtab"));

        socket.stop();
        assert!(
            !Path::new(&path).exists(),
            "stop() should unlink the socket file"
        );

        // If stop() failed to suppress restarts, the health check or a pending
        // rebind would bring the file back. Wait well past several intervals.
        std::thread::sleep(Duration::from_millis(500));

        assert!(
            !Path::new(&path).exists(),
            "socket file must not reappear after stop()"
        );
        assert!(
            send_claude(&path).is_none(),
            "no listener should respond after stop()"
        );
    }

    // ---- session_update parse / normalization matrix ------------------------

    #[test]
    fn session_update_dispatches_parsed_fields() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1"}"#,
        );

        let got = captured.wait_one().expect("session_update should dispatch");
        assert_eq!(got.0, "P1");
        assert_eq!(got.1, "S1");
        assert_eq!(got.2, None, "missing source must surface as None");
    }

    #[test]
    fn session_update_parses_source_field() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1","source":"resume"}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.2.as_deref(), Some("resume"));
    }

    #[test]
    fn session_update_empty_source_normalizes_to_none() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1","source":""}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.2, None, "empty source must normalize to None");
    }

    #[test]
    fn session_update_missing_window_id_drops_silently() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","sessionId":"S1"}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "missing paneId must drop");
    }

    #[test]
    fn session_update_empty_strings_drop_silently() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"","sessionId":""}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "empty paneId/sessionId must not dispatch");
    }

    #[test]
    fn session_update_non_string_fields_drop_silently() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":42,"sessionId":["S"]}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            captured.count(),
            0,
            "non-string paneId/sessionId must not dispatch"
        );
    }

    #[test]
    fn session_update_parses_cwd_field() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":"/Users/nick/Projects/notes/.claude/worktrees/foo"}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(
            got.3.as_deref(),
            Some("/Users/nick/Projects/notes/.claude/worktrees/foo"),
            "cwd must arrive verbatim"
        );
    }

    #[test]
    fn session_update_missing_cwd_is_none() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1"}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.3, None, "missing cwd must arrive as None");
    }

    #[test]
    fn session_update_empty_cwd_normalizes_to_none() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":""}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.3, None, "empty cwd must collapse to None");
    }

    #[test]
    fn session_update_null_cwd_is_none() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":null}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.3, None);
    }

    #[test]
    fn session_update_non_string_cwd_is_none() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":42}"#,
        );

        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.0, "P1", "non-string cwd must not block the dispatch");
        assert_eq!(got.3, None, "non-string cwd must surface as None");
    }

    // ---- claude_exited parse ------------------------------------------------

    /// Thread-safe collector for dispatched `claude_exited` window ids.
    #[derive(Clone, Default)]
    struct CapturedExits {
        items: Arc<Mutex<Vec<String>>>,
    }
    impl CapturedExits {
        fn handler(&self) -> impl Fn(SocketMessage) + Send + Sync + 'static {
            let items = Arc::clone(&self.items);
            move |msg| match msg {
                SocketMessage::ClaudeExited { term_window_id } => items.lock().unwrap().push(term_window_id),
                SocketMessage::Claude { reply, .. } => reply.send("newtab"),
                SocketMessage::Handoff { reply, .. } => reply.send("ok"),
                SocketMessage::Dispatch { reply, .. } => reply.send("ok"),
                SocketMessage::SessionUpdate { .. } => {}
            }
        }
        fn count(&self) -> usize {
            self.items.lock().unwrap().len()
        }
    }

    #[test]
    fn claude_exited_dispatches_its_term_window_id() {
        let captured = CapturedExits::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(
            socket.path(),
            r#"{"action":"claude_exited","paneId":"t1-claude"}"#,
        );

        wait_for(Duration::from_secs(1), || captured.count() >= 1);
        assert_eq!(
            captured.items.lock().unwrap().first().map(String::as_str),
            Some("t1-claude")
        );
    }

    #[test]
    fn claude_exited_without_a_term_window_id_drops_silently() {
        // Nothing to clear without a window — the same required-field rule
        // `session_update` applies.
        let captured = CapturedExits::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(socket.path(), r#"{"action":"claude_exited","paneId":""}"#);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "an empty paneId must drop");
    }

    #[test]
    fn unknown_action_drops_silently() {
        let captured = CapturedUpdates::default();
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(captured.handler()).unwrap();

        send_raw(socket.path(), r#"{"action":"frobnicate","x":"y"}"#);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0);
    }

    // ---- NiceControlSocketHandoffTests (PARSE halves) -----------------------
    //
    // The reply-`ok` cases are R26's — the R14 handoff STUB replies
    // `error: handoff is not supported yet` (pinned in
    // `window_state::tests::handoff_stub_replies_error`). These tests use a fake
    // handler and assert the PARSE + normalization only.

    #[derive(Clone, Default)]
    struct CapturedHandoffs {
        items: Arc<Mutex<Vec<Handoff>>>,
    }
    #[derive(Clone)]
    struct Handoff {
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
    }
    impl CapturedHandoffs {
        fn handler(&self) -> impl Fn(SocketMessage) + Send + Sync + 'static {
            let items = Arc::clone(&self.items);
            move |msg| match msg {
                SocketMessage::Handoff {
                    cwd,
                    handoff_file,
                    instructions,
                    model,
                    effort,
                    session_id,
                    term_window_id,
                    reply,
                } => {
                    reply.send("ok"); // drain the fd; the real decision is R26's
                    items.lock().unwrap().push(Handoff {
                        cwd,
                        handoff_file,
                        instructions,
                        model,
                        effort,
                        session_id,
                        term_window_id,
                    });
                }
                SocketMessage::Claude { reply, .. } => reply.send("newtab"),
                SocketMessage::Dispatch { reply, .. } => reply.send("ok"),
                SocketMessage::SessionUpdate { .. } => {}
                SocketMessage::ClaudeExited { .. } => {}
            }
        }
        fn count(&self) -> usize {
            self.items.lock().unwrap().len()
        }
        fn wait_one(&self) -> Option<Handoff> {
            wait_for(Duration::from_secs(1), || self.count() >= 1);
            self.items.lock().unwrap().first().cloned()
        }
    }

    fn socket_with(handler: impl Fn(SocketMessage) + Send + Sync + 'static) -> NiceControlSocket {
        let mut s = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        s.start(handler).unwrap();
        s
    }

    #[test]
    fn handoff_valid_payload_with_instructions_dispatches_all_fields() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());

        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"tab1","paneId":"pane1","instructions":"Focus only on the UI layer","model":"claude-opus-4-8","effort":"xhigh"}"#,
        );

        let got = captured.wait_one().expect("handoff with all fields must dispatch");
        assert_eq!(got.cwd, "/tmp/work");
        assert_eq!(got.handoff_file, "/tmp/work/.claude/handoff/h.md");
        assert_eq!(got.instructions, "Focus only on the UI layer");
        assert_eq!(got.session_id, "tab1");
        assert_eq!(got.term_window_id, "pane1");
        assert_eq!(got.model, "claude-opus-4-8");
        assert_eq!(got.effort, "xhigh");
    }

    #[test]
    fn handoff_valid_payload_reply_round_trips() {
        // Socket reply plumbing: a handler that replies "ok" round-trips "ok" to
        // the client. R26 makes the PRODUCTION handoff handler reply "ok"; until
        // then the R14 stub replies `error: …` (see
        // window_state::tests::handoff_stub_replies_error).
        let socket = socket_with(|msg| {
            if let SocketMessage::Handoff { reply, .. } = msg {
                reply.send("ok");
            }
        });

        let reply = send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","instructions":""}"#,
        );
        assert_eq!(reply.as_deref(), Some("ok"));
    }

    #[test]
    fn handoff_missing_cwd_drops_silently() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_raw(
            socket.path(),
            r#"{"action":"handoff","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "missing cwd must drop");
    }

    #[test]
    fn handoff_missing_handoff_file_drops_silently() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_raw(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","tabId":"t1","paneId":"p1"}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "missing handoffFile must drop");
    }

    #[test]
    fn handoff_empty_cwd_drops_silently() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_raw(
            socket.path(),
            r#"{"action":"handoff","cwd":"","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "empty cwd must drop");
    }

    #[test]
    fn handoff_empty_handoff_file_drops_silently() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_raw(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"","tabId":"t1","paneId":"p1"}"#,
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "empty handoffFile must drop");
    }

    #[test]
    fn handoff_absent_instructions_normalizes_to_empty_string() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.instructions, "", "absent instructions → \"\"");
    }

    #[test]
    fn handoff_empty_instructions_normalizes_to_empty_string() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","instructions":""}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.instructions, "", "empty instructions → \"\"");
    }

    #[test]
    fn handoff_absent_session_id_normalizes_to_empty_string() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","paneId":"p1"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.session_id, "", "absent tabId → \"\"");
    }

    #[test]
    fn handoff_absent_window_id_normalizes_to_empty_string() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.term_window_id, "", "absent paneId → \"\"");
    }

    #[test]
    fn handoff_model_and_effort_present_surface_verbatim() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","model":"claude-sonnet-4-6","effort":"max"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.model, "claude-sonnet-4-6");
        assert_eq!(got.effort, "max");
    }

    #[test]
    fn handoff_absent_model_and_effort_dispatches_with_empty_strings() {
        // Back-compat: an older installed nice-handoff.sh omits both fields; the
        // request must still dispatch (cwd/handoffFile are the only required
        // fields), with model/effort normalized to "".
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#,
        );
        let got = captured
            .wait_one()
            .expect("a payload without model/effort must still dispatch");
        assert_eq!(got.model, "", "absent model → \"\"");
        assert_eq!(got.effort, "", "absent effort → \"\"");
    }

    #[test]
    fn handoff_empty_model_and_effort_normalize_to_empty_strings() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","model":"","effort":""}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.model, "");
        assert_eq!(got.effort, "");
    }

    // ---- `dispatch` PARSE tests ---------------------------------------------
    //
    // Mirrors the handoff set with dispatch's own required-field list: `cwd`,
    // `worktreeName`, `taskFile` AND `paneId` are all required non-empty. The
    // handler's behavior (nesting, payload-cwd spawn, locked title, always-`ok`)
    // lives in `window_state`; these assert PARSE + normalization only.

    /// A minimal VALID dispatch payload (every required field present).
    const DISPATCH_PAYLOAD: &str = r#"{"action":"dispatch","cwd":"/repo","worktreeName":"fix-tabs","taskFile":"/repo/.claude/dispatch/fix-tabs-1.md","tabId":"t1","paneId":"p1"}"#;

    #[derive(Clone, Default)]
    struct CapturedDispatches {
        items: Arc<Mutex<Vec<Dispatch>>>,
    }
    #[derive(Clone)]
    struct Dispatch {
        cwd: String,
        worktree_name: String,
        task_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
    }
    impl CapturedDispatches {
        fn handler(&self) -> impl Fn(SocketMessage) + Send + Sync + 'static {
            let items = Arc::clone(&self.items);
            move |msg| match msg {
                SocketMessage::Dispatch {
                    cwd,
                    worktree_name,
                    task_file,
                    instructions,
                    model,
                    effort,
                    session_id,
                    term_window_id,
                    reply,
                } => {
                    reply.send("ok"); // drain the fd; the real handler is window-side
                    items.lock().unwrap().push(Dispatch {
                        cwd,
                        worktree_name,
                        task_file,
                        instructions,
                        model,
                        effort,
                        session_id,
                        term_window_id,
                    });
                }
                SocketMessage::Claude { reply, .. } => reply.send("newtab"),
                SocketMessage::Handoff { reply, .. } => reply.send("ok"),
                SocketMessage::SessionUpdate { .. } => {}
                SocketMessage::ClaudeExited { .. } => {}
            }
        }
        fn count(&self) -> usize {
            self.items.lock().unwrap().len()
        }
        fn wait_one(&self) -> Option<Dispatch> {
            wait_for(Duration::from_secs(1), || self.count() >= 1);
            self.items.lock().unwrap().first().cloned()
        }
    }

    /// Assert a payload never reaches the handler (missing/empty required field).
    fn assert_dispatch_drops(payload: &str, what: &str) {
        let captured = CapturedDispatches::default();
        let socket = socket_with(captured.handler());
        send_raw(socket.path(), payload);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(captured.count(), 0, "{what} must drop");
    }

    #[test]
    fn dispatch_valid_payload_dispatches_all_fields() {
        let captured = CapturedDispatches::default();
        let socket = socket_with(captured.handler());

        send_and_read(
            socket.path(),
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"fix-tabs","taskFile":"/repo/.claude/dispatch/fix-tabs-1.md","tabId":"tab1","paneId":"pane1","instructions":"Only touch the parser","model":"opus","effort":"xhigh"}"#,
        );

        let got = captured
            .wait_one()
            .expect("dispatch with all fields must dispatch");
        assert_eq!(got.cwd, "/repo");
        assert_eq!(got.worktree_name, "fix-tabs");
        assert_eq!(got.task_file, "/repo/.claude/dispatch/fix-tabs-1.md");
        assert_eq!(got.instructions, "Only touch the parser");
        assert_eq!(got.session_id, "tab1");
        assert_eq!(got.term_window_id, "pane1");
        assert_eq!(got.model, "opus");
        assert_eq!(got.effort, "xhigh");
    }

    #[test]
    fn dispatch_valid_payload_reply_round_trips() {
        let socket = socket_with(|msg| {
            if let SocketMessage::Dispatch { reply, .. } = msg {
                reply.send("ok");
            }
        });
        let reply = send_and_read(socket.path(), DISPATCH_PAYLOAD);
        assert_eq!(reply.as_deref(), Some("ok"));
    }

    #[test]
    fn dispatch_missing_cwd_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","worktreeName":"w","taskFile":"/repo/t.md","tabId":"t1","paneId":"p1"}"#,
            "missing cwd",
        );
    }

    #[test]
    fn dispatch_empty_cwd_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"","worktreeName":"w","taskFile":"/repo/t.md","tabId":"t1","paneId":"p1"}"#,
            "empty cwd",
        );
    }

    #[test]
    fn dispatch_missing_worktree_name_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"/repo","taskFile":"/repo/t.md","tabId":"t1","paneId":"p1"}"#,
            "missing worktreeName",
        );
    }

    #[test]
    fn dispatch_empty_worktree_name_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"","taskFile":"/repo/t.md","tabId":"t1","paneId":"p1"}"#,
            "empty worktreeName",
        );
    }

    #[test]
    fn dispatch_missing_task_file_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"w","tabId":"t1","paneId":"p1"}"#,
            "missing taskFile",
        );
    }

    #[test]
    fn dispatch_empty_task_file_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"w","taskFile":"","tabId":"t1","paneId":"p1"}"#,
            "empty taskFile",
        );
    }

    #[test]
    fn dispatch_missing_window_id_drops_silently() {
        // Unlike handoff (whose paneId is optional), a dispatch without a sending
        // window cannot resolve an originating session to nest under.
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"w","taskFile":"/repo/t.md","tabId":"t1"}"#,
            "missing paneId",
        );
    }

    #[test]
    fn dispatch_empty_window_id_drops_silently() {
        assert_dispatch_drops(
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"w","taskFile":"/repo/t.md","tabId":"t1","paneId":""}"#,
            "empty paneId",
        );
    }

    #[test]
    fn dispatch_absent_optional_fields_normalize_to_empty_strings() {
        // The DEFAULT dispatch: the helper omits model/effort entirely so the
        // child launches on the user's configured default (no inheritance).
        let captured = CapturedDispatches::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"w","taskFile":"/repo/t.md","paneId":"p1"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.session_id, "", "absent tabId → \"\"");
        assert_eq!(got.instructions, "", "absent instructions → \"\"");
        assert_eq!(got.model, "", "absent model → \"\"");
        assert_eq!(got.effort, "", "absent effort → \"\"");
    }

    #[test]
    fn dispatch_empty_optional_fields_normalize_to_empty_strings() {
        let captured = CapturedDispatches::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"dispatch","cwd":"/repo","worktreeName":"w","taskFile":"/repo/t.md","tabId":"","paneId":"p1","instructions":"","model":"","effort":""}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.session_id, "");
        assert_eq!(got.instructions, "");
        assert_eq!(got.model, "");
        assert_eq!(got.effort, "");
    }

    // ---- path mint + sun_path guard -----------------------------------------

    fn file_name_of(path: &str) -> String {
        Path::new(path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }

    /// A scratch path in `$TMPDIR` that no other test (or Nice instance) uses.
    fn scratch_socket_path() -> String {
        std::env::temp_dir()
            .join(format!("nice-test-{}.sock", mint_suffix()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn legacy_mint_path_matches_frozen_pattern() {
        // No NICE_SOCKET_PATH in the test env → `$TMPDIR/nice-<pid>-<8hex>.sock`,
        // the shape the $TMPDIR sweep's LEGACY branch parses (pid right after
        // `nice-`). Still the default for window-less call sites.
        let socket = NiceControlSocket::new();
        let file = file_name_of(socket.path());
        let pid = std::process::id();
        let prefix = format!("nice-{pid}-");
        assert!(
            file.starts_with(&prefix),
            "socket filename {file} must start with {prefix}"
        );
        assert!(file.ends_with(".sock"), "socket filename must end .sock");
        let suffix = &file[prefix.len()..file.len() - ".sock".len()];
        assert_eq!(suffix.len(), 8, "suffix is 8 hex chars");
        assert!(
            suffix.bytes().all(|b| b.is_ascii_hexdigit()),
            "suffix {suffix} must be hex (no '-', so the sweep reads the pid)"
        );
    }

    #[test]
    fn window_mint_path_matches_frozen_pattern() {
        // The restart-stable shape: `$TMPDIR/nice-w-<12hex>.sock`, keyed on the
        // persisted window id with dashes stripped. No pid anywhere — that is the
        // whole point (the path must recur after a relaunch).
        let path = mint_window_socket_path("3f2a1b9c-77d4-4e21-9a55-0b1c2d3e4f50");
        let file = file_name_of(&path);
        assert_eq!(file, "nice-w-3f2a1b9c77d4.sock");
        assert_eq!(
            Path::new(&path).parent(),
            Some(std::env::temp_dir().as_path()),
            "the window socket lives in $TMPDIR like the legacy one"
        );
        assert!(
            !file.contains(&std::process::id().to_string()),
            "a pid in the name would break restart stability"
        );
        assert!(
            path.len() < SUN_PATH_CAP,
            "the window path must stay inside sun_path capacity"
        );
    }

    #[test]
    fn window_mint_path_is_stable_for_the_same_window_id() {
        let id = "8c7d6e5f-4a3b-4291-8d0e-1f2a3b4c5d6e";
        assert_eq!(
            mint_window_socket_path(id),
            mint_window_socket_path(id),
            "the same persisted window id must mint the same path across runs"
        );
        assert_ne!(
            mint_window_socket_path(id),
            mint_window_socket_path("11112222-3333-4444-8555-666677778888"),
            "different windows must not share a path"
        );
    }

    #[test]
    fn window_socket_takes_over_a_stale_file() {
        // A crashed run leaves the socket file behind with nothing listening.
        // The probe finds no owner, so the bind clears it and takes the path —
        // otherwise every restart would abandon its window's stable path.
        let path = scratch_socket_path();
        let dead = UnixListener::bind(&path).expect("precondition: bind the doomed listener");
        drop(dead); // closes the fd; the FILE stays (crash semantics)
        assert!(Path::new(&path).exists(), "precondition: stale file on disk");

        let mut socket = NiceControlSocket::with_path_and_intervals(
            path.clone(),
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(reply_newtab_handler).expect("takeover must bind");

        assert_eq!(socket.path(), path, "takeover keeps the requested path");
        assert_eq!(
            send_claude(&path).as_deref(),
            Some("newtab"),
            "the taken-over path must answer"
        );
    }

    #[test]
    fn live_owner_is_never_stolen_and_forces_the_legacy_fallback() {
        // Someone is already listening on the window-keyed path. We must NOT
        // unlink them; this run falls back to a legacy pid+nonce path (D2).
        let path = scratch_socket_path();
        let owner = UnixListener::bind(&path).expect("precondition: the live owner binds");

        let mut socket = NiceControlSocket::with_path_and_intervals(
            path.clone(),
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket
            .start(reply_newtab_handler)
            .expect("the fallback bind must succeed");

        assert_ne!(
            socket.path(),
            path,
            "start() must have moved off the contested path"
        );
        let file = file_name_of(socket.path());
        assert!(
            file.starts_with(&format!("nice-{}-", std::process::id())),
            "the fallback is a legacy pid+nonce path, got {file}"
        );
        assert_eq!(
            send_claude(socket.path()).as_deref(),
            Some("newtab"),
            "the fallback path must be live"
        );

        // The foreign owner is untouched: same file, still accepting.
        assert!(Path::new(&path).exists(), "the owner's file must survive");
        owner.set_nonblocking(true).unwrap();
        let _client = UnixStream::connect(&path).expect("the owner must still accept connects");
        assert!(
            wait_for(Duration::from_secs(1), || owner.accept().is_ok()),
            "the live owner must still be accepting on its own socket"
        );

        drop(owner);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn zero_byte_connection_is_tolerated() {
        // The bind probe (and the $TMPDIR sweep) connect without writing a byte.
        // The server must treat that as an empty request and keep serving.
        let mut socket = NiceControlSocket::with_intervals(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        socket.start(reply_newtab_handler).unwrap();

        assert!(
            is_live_owner(socket.path()),
            "a probe of our own live socket must read as a live owner"
        );

        assert_eq!(
            send_claude(socket.path()).as_deref(),
            Some("newtab"),
            "a zero-byte connection must not wedge the socket"
        );
    }

    #[test]
    fn probe_of_a_stale_file_reports_no_owner() {
        let path = scratch_socket_path();
        let dead = UnixListener::bind(&path).unwrap();
        drop(dead);
        assert!(
            !is_live_owner(&path),
            "an orphaned socket file has no live owner"
        );
        // A non-socket file squatting the name is stale too (D5: only a
        // successful connect proves an owner).
        let junk = scratch_socket_path();
        std::fs::write(&junk, b"not a socket").unwrap();
        assert!(!is_live_owner(&junk), "a plain file is not a live owner");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&junk);
    }

    #[test]
    fn a_surviving_child_process_does_not_answer_the_probe() {
        // The listener fd must not reach a pty child. When it did, a child that
        // outlived the app (a daemon-hosted Claude session, a nohup'd server)
        // kept `connect(2)` succeeding at the window's stable path, so the next
        // launch probed its OWN orphan as a live owner, took the D2 fallback and
        // silently lost restart stability — the very thing this path buys.
        let path = scratch_socket_path();
        let listener = bind_and_listen_once(&path).expect("precondition: bind the listener");

        // Forked while the listener is open — the pty-child stand-in.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("precondition: spawn a child that could inherit the fd");

        // The app exits: our listener fd closes, the file stays (crash/quit
        // residue), and only the child could still be holding the socket.
        drop(listener);
        let answered = is_live_owner(&path);

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&path);

        assert!(
            !answered,
            "the listener fd leaked into the child, which now answers for an app that is gone"
        );
    }

    #[test]
    fn socket_fds_are_close_on_exec() {
        let path = scratch_socket_path();
        let listener = bind_and_listen_once(&path).expect("precondition: bind the listener");
        let client = UnixStream::connect(&path).expect("precondition: connect a client");
        listener.set_nonblocking(false).unwrap();
        let (accepted, _) = listener.accept().expect("precondition: accept the client");

        // The listener is ours (raw `libc::socket` + `set_cloexec`); the two
        // stream fds come from std, which sets `FD_CLOEXEC` itself — pinned here
        // so nothing has to re-derive that when reading `set_cloexec`.
        for (what, fd) in [
            ("the listener", listener.as_raw_fd()),
            ("a connected client", client.as_raw_fd()),
            ("an accepted stream", accepted.as_raw_fd()),
        ] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0, "F_GETFD failed for {what}");
            assert!(
                flags & libc::FD_CLOEXEC != 0,
                "{what} must be close-on-exec so no pty child inherits it"
            );
        }

        drop(accepted);
        drop(client);
        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn distinct_sockets_mint_distinct_paths() {
        let a = NiceControlSocket::new();
        let b = NiceControlSocket::new();
        assert_ne!(a.path(), b.path(), "each window mints a unique socket path");
    }

    #[test]
    fn bind_rejects_overlong_path_loudly() {
        // A path at/over sun_path capacity must fail loudly, never truncate.
        let long = format!("/tmp/{}", "x".repeat(SUN_PATH_CAP));
        let err = bind_and_listen(&long).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // ---- waker-based foreground bridge (App-Nap-safe drain) ------------------

    #[test]
    fn foreground_bridge_wakes_and_delivers_in_order() {
        use std::task::Wake;

        struct FlagWaker(Arc<AtomicBool>);
        impl Wake for FlagWaker {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::Release);
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.store(true, Ordering::Release);
            }
        }

        let (tx, rx) = socket_channel();
        let woke = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(FlagWaker(Arc::clone(&woke))));
        let mut cx = Context::from_waker(&waker);

        // Nothing pending → parks and stores the waker.
        let mut fut = rx.readable();
        assert!(matches!(Pin::new(&mut fut).poll(&mut cx), Poll::Pending));
        assert!(!woke.load(Ordering::Acquire));

        // Post two messages: the first fires the parked waker.
        tx.post(SocketMessage::SessionUpdate {
            term_window_id: "P1".into(),
            claude_session_id: "S1".into(),
            source: None,
            cwd: None,
        });
        assert!(
            woke.load(Ordering::Acquire),
            "post must fire the parked foreground waker"
        );
        tx.post(SocketMessage::SessionUpdate {
            term_window_id: "P2".into(),
            claude_session_id: "S2".into(),
            source: None,
            cwd: None,
        });

        // Readiness now resolves immediately.
        let mut fut2 = rx.readable();
        assert!(matches!(Pin::new(&mut fut2).poll(&mut cx), Poll::Ready(())));

        // Messages drain in FIFO order, then the channel is empty.
        match rx.try_recv() {
            Ok(SocketMessage::SessionUpdate { term_window_id, .. }) => assert_eq!(term_window_id, "P1"),
            _ => panic!("expected P1 first"),
        }
        match rx.try_recv() {
            Ok(SocketMessage::SessionUpdate { term_window_id, .. }) => assert_eq!(term_window_id, "P2"),
            _ => panic!("expected P2 second"),
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn foreground_bridge_reports_disconnect_when_sender_dropped() {
        let (tx, rx) = socket_channel();
        drop(tx);
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    }
}
