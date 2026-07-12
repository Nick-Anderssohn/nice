//! Per-window Unix-domain control socket (R14).
//!
//! Ports Swift `NiceControlSocket` (`Sources/Nice/Process/NiceControlSocket.swift`)
//! — a tiny AF_UNIX listener that lets Nice's shell helpers and Claude Code
//! skills talk to the app. One newline-delimited JSON object per client, then
//! close. Three FROZEN actions (installed helpers on user disks already speak
//! this protocol byte-for-byte — see the plan's "wire protocol is FROZEN"
//! decision):
//!
//!   * `claude`         — the shadowed `claude()` zsh function asking Nice to
//!                        open a new tab or promote a pane in place.
//!   * `session_update` — the SessionStart hook relaying session-id / cwd
//!                        rotations (fire-and-forget, no reply).
//!   * `handoff`        — the `/nice-handoff` skill's helper asking Nice to open
//!                        a nested handoff tab.
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
//! ## Reply capability
//!
//! [`Reply`] owns the accepted [`UnixStream`] and is **consumed on use**
//! ([`Reply::send`] takes `self`): at-most-once by construction, stronger than
//! Swift's closure convention. `session_update` drops the stream BEFORE dispatch
//! (fire-and-forget); `claude` / `handoff` carry a `Reply` and answer once from
//! the foreground.
//!
//! The window-side routing point + the three stub handlers live on
//! [`crate::window_state::WindowState`] (`route_socket_message`); R15/R16/R26
//! fill their bodies without reshaping this socket. The `app::run` bootstrap
//! (mint before the Main pane's spawn, start the listener, spawn the foreground
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

// ===========================================================================
// The FROZEN message enum + reply object
// ===========================================================================

/// Discriminated payload parsed off the control socket. Produced by
/// [`parse_message`], routed by
/// [`crate::window_state::WindowState::route_socket_message`]. The enum is
/// finished business after R14 — R15/R16/R26 only fill handler bodies.
///
/// Mirrors Swift `enum SocketMessage`
/// (`NiceControlSocket.swift:43-144`). Every normalization rule the parser
/// applies is documented on [`parse_message`].
pub(crate) enum SocketMessage {
    /// `claude()` shadow asking whether to open a new sidebar tab (default) or
    /// promote the sending pane in place. `tab_id` / `pane_id` are empty strings
    /// for the Main Terminals tab. The handler replies exactly once via `reply`
    /// with `newtab` / `inplace` / `inplace <session>` (+ optional settings
    /// pointer). `cwd` is required (may be empty); `args` defaults to `[]`.
    Claude {
        cwd: String,
        args: Vec<String>,
        tab_id: String,
        pane_id: String,
        reply: Reply,
    },
    /// Claude Code SessionStart hook reporting the active session UUID for the
    /// sending pane. Fire-and-forget: the client fd is closed BEFORE dispatch,
    /// so this variant carries no [`Reply`]. `pane_id` + `session_id` are
    /// required non-empty; `source` / `cwd` are absent / empty / non-string
    /// normalized to `None` (older installed hooks predate these fields and must
    /// NOT be dropped).
    SessionUpdate {
        pane_id: String,
        session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    },
    /// `/nice-handoff` skill asking Nice to open a fresh Claude session nested
    /// under the originating tab. `cwd` + `handoff_file` are required non-empty;
    /// `instructions` / `model` / `effort` / `tab_id` / `pane_id` are normalized
    /// to `""` (an older installed helper omits `model` / `effort` entirely and
    /// must still dispatch). The handler replies once with `ok` / `error: …`.
    Handoff {
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        tab_id: String,
        pane_id: String,
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
        tab_id: String,
        pane_id: String,
    },
    SessionUpdate {
        pane_id: String,
        session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    },
    Handoff {
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        tab_id: String,
        pane_id: String,
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
    /// across rebinds so already-spawned shells stay correct.
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
        NiceControlSocket {
            shared: Arc::new(SocketShared {
                path: mint_socket_path(),
                stop: AtomicBool::new(false),
                force_rebind: AtomicBool::new(false),
                health_check_interval,
                initial_restart_delay,
            }),
            started: AtomicBool::new(false),
        }
    }

    /// The bound (or to-be-bound) socket path — injected into pty env as
    /// `NICE_SOCKET` at window construction, before the listener arms.
    pub(crate) fn path(&self) -> &str {
        &self.shared.path
    }

    /// Bind, listen, and spawn the accept-loop thread with `handler`. Safe to
    /// call once; a second call is a no-op. Bind failure is **non-fatal** and
    /// reported as `Err` (the caller logs + continues — shells fall back to
    /// direct `command claude`, preserving "user always gets claude"). On the
    /// happy path the listener is accepting by the time this returns, so a client
    /// may connect immediately.
    pub(crate) fn start<F>(&self, handler: F) -> io::Result<()>
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
        let listener = bind_and_listen(&self.shared.path)?;
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
                Err(_) => continue, // retry with more backoff
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
/// `claude` / `handoff` can carry it in a [`Reply`]. Returns `None` (dropping the
/// stream → silent close, no reply) for malformed JSON, a non-object, a
/// missing/unknown `action`, or a missing required field.
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
fn parse_message(line: &[u8], stream: UnixStream) -> Option<SocketMessage> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let obj = value.as_object()?; // non-object → drop
    let action = obj.get("action").and_then(|v| v.as_str())?; // missing/non-string → drop

    match action {
        "claude" => {
            let cwd = obj.get("cwd").and_then(|v| v.as_str())?.to_string();
            let args = parse_string_array(obj.get("args"));
            let tab_id = str_or_empty(obj, "tabId");
            let pane_id = str_or_empty(obj, "paneId");
            Some(SocketMessage::Claude {
                cwd,
                args,
                tab_id,
                pane_id,
                reply: Reply::new(stream),
            })
        }
        "session_update" => {
            let pane_id = non_empty(obj, "paneId")?;
            let session_id = non_empty(obj, "sessionId")?;
            let source = normalize_opt(obj, "source");
            let cwd = normalize_opt(obj, "cwd");
            // Fire-and-forget: close the fd BEFORE dispatch so the hook's `nc`
            // returns promptly even if the foreground is backed up.
            drop(stream);
            Some(SocketMessage::SessionUpdate {
                pane_id,
                session_id,
                source,
                cwd,
            })
        }
        "handoff" => {
            let cwd = non_empty(obj, "cwd")?;
            let handoff_file = non_empty(obj, "handoffFile")?;
            let tab_id = str_or_empty(obj, "tabId");
            let pane_id = str_or_empty(obj, "paneId");
            let instructions = str_or_empty(obj, "instructions");
            let model = str_or_empty(obj, "model");
            let effort = str_or_empty(obj, "effort");
            Some(SocketMessage::Handoff {
                cwd,
                handoff_file,
                instructions,
                model,
                effort,
                tab_id,
                pane_id,
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

/// `socket(AF_UNIX, SOCK_STREAM)` → `unlink(path)` → `bind` → `chmod 0600` →
/// `listen(8)`. Ports Swift `bindAndListenLocked`
/// (`NiceControlSocket.swift:244-311`). Returns a **non-blocking**
/// [`UnixListener`]; the accept loop parks in `poll()` and accepts only when a
/// connection is pending.
fn bind_and_listen(path: &str) -> io::Result<UnixListener> {
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

    // SAFETY: `socket` with AF_UNIX/SOCK_STREAM returns a new fd (or -1); no
    // arguments are pointers.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Own the fd immediately so every error path below closes it on drop.
    // SAFETY: `fd` is a fresh, exclusively-owned socket fd.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };

    // Clear any stale socket file — a prior crashed run or the listener we are
    // replacing right now.
    let _ = std::fs::remove_file(path);

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

/// Mint the socket path: `NICE_SOCKET_PATH` override (test seam) else
/// `$TMPDIR/nice-<pid>-<suffix>.sock`.
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
            SocketMessage::SessionUpdate { .. } => {}
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
                    pane_id,
                    session_id,
                    source,
                    cwd,
                } => items.lock().unwrap().push((pane_id, session_id, source, cwd)),
                SocketMessage::Claude { reply, .. } => reply.send("newtab"),
                SocketMessage::Handoff { reply, .. } => reply.send("ok"),
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
    fn session_update_missing_pane_id_drops_silently() {
        let captured = CapturedUpdates::default();
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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
        let socket = NiceControlSocket::with_intervals(
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

    #[test]
    fn unknown_action_drops_silently() {
        let captured = CapturedUpdates::default();
        let socket = NiceControlSocket::with_intervals(
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
        tab_id: String,
        pane_id: String,
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
                    tab_id,
                    pane_id,
                    reply,
                } => {
                    reply.send("ok"); // drain the fd; the real decision is R26's
                    items.lock().unwrap().push(Handoff {
                        cwd,
                        handoff_file,
                        instructions,
                        model,
                        effort,
                        tab_id,
                        pane_id,
                    });
                }
                SocketMessage::Claude { reply, .. } => reply.send("newtab"),
                SocketMessage::SessionUpdate { .. } => {}
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
        let s = NiceControlSocket::with_intervals(
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
        assert_eq!(got.tab_id, "tab1");
        assert_eq!(got.pane_id, "pane1");
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
    fn handoff_absent_tab_id_normalizes_to_empty_string() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","paneId":"p1"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.tab_id, "", "absent tabId → \"\"");
    }

    #[test]
    fn handoff_absent_pane_id_normalizes_to_empty_string() {
        let captured = CapturedHandoffs::default();
        let socket = socket_with(captured.handler());
        send_and_read(
            socket.path(),
            r#"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1"}"#,
        );
        let got = captured.wait_one().expect("dispatch");
        assert_eq!(got.pane_id, "", "absent paneId → \"\"");
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

    // ---- path mint + sun_path guard -----------------------------------------

    #[test]
    fn mint_path_matches_frozen_pattern() {
        // No NICE_SOCKET_PATH in the test env → `$TMPDIR/nice-<pid>-<8hex>.sock`,
        // the exact shape the $TMPDIR sweep parses (pid right after `nice-`).
        let socket = NiceControlSocket::new();
        let file = Path::new(socket.path())
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
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
            pane_id: "P1".into(),
            session_id: "S1".into(),
            source: None,
            cwd: None,
        });
        assert!(
            woke.load(Ordering::Acquire),
            "post must fire the parked foreground waker"
        );
        tx.post(SocketMessage::SessionUpdate {
            pane_id: "P2".into(),
            session_id: "S2".into(),
            source: None,
            cwd: None,
        });

        // Readiness now resolves immediately.
        let mut fut2 = rx.readable();
        assert!(matches!(Pin::new(&mut fut2).poll(&mut cx), Poll::Ready(())));

        // Messages drain in FIFO order, then the channel is empty.
        match rx.try_recv() {
            Ok(SocketMessage::SessionUpdate { pane_id, .. }) => assert_eq!(pane_id, "P1"),
            _ => panic!("expected P1 first"),
        }
        match rx.try_recv() {
            Ok(SocketMessage::SessionUpdate { pane_id, .. }) => assert_eq!(pane_id, "P2"),
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
