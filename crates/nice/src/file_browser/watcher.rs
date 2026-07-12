//! `DirectoryWatcherHub` — the R19 sidebar's shallow directory watcher, one hub
//! per window, ported from Swift's per-row `DirectoryWatcher`
//! (`Sources/Nice/State/FileBrowserState.swift:101-153`).
//!
//! ## Why a hub (not a per-row watcher)
//!
//! Swift minted one `DirectoryWatcher` (one kqueue `DispatchSource`, one fd) per
//! visible `FileTreeRow`. The Rust rewrite consolidates that into **one hub per
//! window**: a single kqueue fd and a single dedicated OS thread. The view diffs
//! a **desired watch set** — the expanded dirs currently in the rendered
//! flattened projection, plus the root — and hands it to [`DirectoryWatcherHub::set_watched`],
//! which opens fds (`O_EVTONLY`) for newly-desired paths and closes fds for
//! dropped ones. This keeps the fd count proportional to what's on screen (capped
//! at [`MAX_WATCHED_FDS`], fail-soft — staleness healing on expand covers the
//! rest) instead of to the whole tree.
//!
//! ## Event mask + debounce (frozen — Swift parity)
//!
//! Each watched dir registers an `EVFILT_VNODE` knote with
//! `NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_EXTEND` (the Swift
//! `[.write, .delete, .rename, .extend]` mask). A burst of changes to a dir
//! within [`DEBOUNCE`] (**120 ms**, PROTECTED constant) coalesces to a single
//! emission — an editor's save-with-multiple-syscalls triggers one reload, not a
//! flurry. The debounce is **thread-side** (the watcher's own thread computes the
//! trailing quiet-window deadline and blocks in `kevent` until then) rather than
//! a gpui timer: a gpui timer is nap-deferred when the window is occluded, and
//! the watcher must keep firing regardless. Threads don't nap.
//!
//! ## Delivery is waker-woken (frozen — the R14 socket-drain precedent)
//!
//! After every emission the thread calls [`crate::platform::wake_main_runloop`]
//! to force the main CFRunLoop out of its wait, so the foreground drain
//! ([`DirectoryWatcherHub::drain_changes`]) runs *now* even under App Nap /
//! timer coalescing — never a gpui timer.
//!
//! ## Teardown
//!
//! [`DirectoryWatcherHub`]'s `Drop` triggers a registered `EVFILT_USER`
//! user-event (`NOTE_TRIGGER`) to wake the thread out of its `kevent` block, then
//! joins it. A bare `close(kq)` is NOT a reliable `kevent` wake on macOS — it can
//! leak a thread blocked in `kevent` forever — so the explicit user-event wake is
//! load-bearing. The thread, on the shutdown signal, closes every watch fd and
//! the kqueue fd, then exits, so both the fd count returns to 0 AND the thread
//! joins within a bounded time.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The trailing quiet-window applied per watched path before a change is
/// emitted. **PROTECTED — 120 ms, Swift parity** (`scheduleDebounced`, 0.12 s).
pub const DEBOUNCE: Duration = Duration::from_millis(120);

/// The vnode event mask — Swift's `[.write, .delete, .rename, .extend]`.
const VNODE_MASK: u32 = libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_EXTEND;

/// The `EVFILT_USER` ident the hub triggers to wake the thread (for a
/// set-watched apply or teardown). Distinct from any fd ident (`EVFILT_VNODE`
/// idents are fds; this uses a dedicated non-fd ident).
const USER_WAKE_IDENT: libc::uintptr_t = 0;

/// Hard cap on simultaneously-watched fds (Swift `FileBrowserState` comment:
/// "stay well under the per-process FD limit"). Fail-soft: paths beyond the cap
/// aren't watched; expand-time staleness healing re-reads them anyway.
pub const MAX_WATCHED_FDS: usize = 256;

/// A control message from the hub (main thread) to the watcher thread.
enum Control {
    /// Replace the desired watch set with these paths (already in visible order;
    /// the thread caps at [`MAX_WATCHED_FDS`]).
    SetWatched(Vec<String>),
    /// Close every fd + the kqueue and exit.
    Shutdown,
}

/// One kqueue watcher per window. Cheap to construct; spawns one OS thread that
/// lives until `Drop`.
pub struct DirectoryWatcherHub {
    kq: RawFd,
    control_tx: Sender<Control>,
    changes_rx: Receiver<String>,
    join: Option<JoinHandle<()>>,
    /// Count of currently-open watch fds (NOT counting the kqueue fd). Shared
    /// with the thread so a test can clone the handle before dropping the hub
    /// and assert it returns to 0.
    #[cfg(test)]
    open_fds: Arc<AtomicUsize>,
}

impl DirectoryWatcherHub {
    /// Create the kqueue, register the `EVFILT_USER` wake, and spawn the watcher
    /// thread. Returns an error only if `kqueue()` or the user-event registration
    /// fails at the syscall level.
    pub fn new() -> std::io::Result<Self> {
        // SAFETY: `kqueue()` is a plain syscall returning a new fd or -1.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Register the EVFILT_USER wake channel (EV_CLEAR so each trigger is a
        // one-shot edge). SAFETY: single kevent change against our own kq.
        let user_ev = libc::kevent {
            ident: USER_WAKE_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let rc = unsafe {
            libc::kevent(
                kq,
                &user_ev as *const libc::kevent,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(kq) };
            return Err(err);
        }

        let (control_tx, control_rx) = mpsc::channel::<Control>();
        let (changes_tx, changes_rx) = mpsc::channel::<String>();
        let open_fds = Arc::new(AtomicUsize::new(0));

        let thread_kq = kq;
        let thread_open_fds = Arc::clone(&open_fds);
        let control_rx = Mutex::new(control_rx);
        let join = std::thread::Builder::new()
            .name("nice-dir-watcher".into())
            .spawn(move || {
                let mut worker = Worker {
                    kq: thread_kq,
                    control_rx: control_rx.into_inner().unwrap(),
                    changes_tx,
                    open_fds: thread_open_fds,
                    fd_to_path: HashMap::new(),
                    path_to_fd: HashMap::new(),
                    pending: HashMap::new(),
                };
                worker.run();
            })?;

        Ok(Self {
            kq,
            control_tx,
            changes_rx,
            join: Some(join),
            #[cfg(test)]
            open_fds,
        })
    }

    /// Replace the desired watch set. Paths should be in **visible order** (the
    /// thread caps at [`MAX_WATCHED_FDS`], keeping the first N). Fds are opened
    /// for newly-desired paths and closed for dropped ones on the watcher thread.
    /// A path that can't be opened (missing / not a dir) is silently skipped —
    /// staleness healing on expand covers it.
    pub fn set_watched(&self, paths: Vec<String>) {
        // A send failure means the thread is gone (already torn down) — nothing
        // to watch, so drop it silently.
        if self.control_tx.send(Control::SetWatched(paths)).is_ok() {
            self.wake_thread();
        }
    }

    /// Non-blocking drain of the paths whose watched directories changed since
    /// the last drain (each already past its 120 ms quiet window). The
    /// foreground calls this from its waker-woken drain; a path appears at most
    /// once per debounce window.
    pub fn drain_changes(&self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(path) = self.changes_rx.try_recv() {
            out.push(path);
        }
        out
    }

    /// The current count of open watch fds (excludes the kqueue fd). Primarily a
    /// test hook — after `set_watched(vec![])` or `Drop` it returns to 0.
    #[cfg(test)]
    pub fn open_fd_count(&self) -> usize {
        self.open_fds.load(Ordering::SeqCst)
    }

    /// A clonable handle to the open-fd counter, so a test can observe it AFTER
    /// the hub is dropped (the drop closes every fd → the count reaches 0).
    #[cfg(test)]
    pub fn open_fd_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.open_fds)
    }

    /// Trigger the `EVFILT_USER` wake so the thread breaks out of `kevent` and
    /// drains the control channel.
    fn wake_thread(&self) {
        let trigger = libc::kevent {
            ident: USER_WAKE_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ENABLE,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: single kevent change against our own kq; safe from any thread.
        unsafe {
            libc::kevent(
                self.kq,
                &trigger as *const libc::kevent,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            );
        }
    }
}

impl Drop for DirectoryWatcherHub {
    fn drop(&mut self) {
        // Ask the thread to close everything and exit, then wake it out of
        // `kevent` so the join returns promptly (a bare close(kq) is not a
        // reliable kevent wake on macOS — the EVFILT_USER trigger is).
        let _ = self.control_tx.send(Control::Shutdown);
        self.wake_thread();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The watcher-thread state. Owns the kqueue fd and every watch fd for its
/// lifetime; closes them all before exiting.
struct Worker {
    kq: RawFd,
    control_rx: Receiver<Control>,
    changes_tx: Sender<String>,
    open_fds: Arc<AtomicUsize>,
    /// fd → watched path (fd is the `EVFILT_VNODE` ident).
    fd_to_path: HashMap<RawFd, String>,
    /// watched path → fd (the diff index).
    path_to_fd: HashMap<String, RawFd>,
    /// path → the instant its trailing quiet window ends. Emitted once the
    /// deadline passes with no fresh event.
    pending: HashMap<String, Instant>,
}

impl Worker {
    fn run(&mut self) {
        const EVENTS_CAP: usize = 64;
        let mut events: [libc::kevent; EVENTS_CAP] = unsafe { std::mem::zeroed() };
        loop {
            // Compute the kevent timeout: the nearest pending debounce deadline,
            // or block indefinitely when nothing is pending.
            let timeout = self.next_timeout();
            let ts = timeout.map(duration_to_timespec);
            let ts_ptr = ts
                .as_ref()
                .map_or(std::ptr::null(), |t| t as *const libc::timespec);

            // SAFETY: wait for events on our kq into the stack buffer.
            let n = unsafe {
                libc::kevent(
                    self.kq,
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    EVENTS_CAP as libc::c_int,
                    ts_ptr,
                )
            };

            let now = Instant::now();
            if n > 0 {
                for ev in events.iter().take(n as usize) {
                    if ev.filter == libc::EVFILT_USER {
                        // Control wake: drain the channel. A Shutdown ends the
                        // thread after closing every fd.
                        if self.drain_control() {
                            self.close_all();
                            return;
                        }
                    } else if ev.filter == libc::EVFILT_VNODE {
                        if let Some(path) = self.fd_to_path.get(&(ev.ident as RawFd)) {
                            // Push the trailing quiet-window deadline forward —
                            // this is the coalescing: a burst re-arms one timer.
                            self.pending.insert(path.clone(), now + DEBOUNCE);
                        }
                    }
                }
            }

            // Emit every path whose quiet window has elapsed.
            self.emit_ready(now);
        }
    }

    /// Drain queued control messages. Returns `true` if a `Shutdown` was seen.
    fn drain_control(&mut self) -> bool {
        let mut shutdown = false;
        while let Ok(msg) = self.control_rx.try_recv() {
            match msg {
                Control::SetWatched(paths) => self.apply_desired(paths),
                Control::Shutdown => shutdown = true,
            }
        }
        shutdown
    }

    /// Diff the desired watch set against the current fds: open the newcomers,
    /// close the departed. Capped at [`MAX_WATCHED_FDS`] in the given (visible)
    /// order.
    fn apply_desired(&mut self, paths: Vec<String>) {
        let mut desired: Vec<String> = Vec::new();
        for p in paths {
            if desired.len() >= MAX_WATCHED_FDS {
                break;
            }
            if !desired.iter().any(|d| d == &p) {
                desired.push(p);
            }
        }

        // Close departed.
        let to_close: Vec<String> = self
            .path_to_fd
            .keys()
            .filter(|p| !desired.iter().any(|d| d == *p))
            .cloned()
            .collect();
        for path in to_close {
            self.unwatch(&path);
        }

        // Open newcomers.
        for path in desired {
            if !self.path_to_fd.contains_key(&path) {
                self.watch(&path);
            }
        }
    }

    /// Open an `O_EVTONLY` fd for `path` and register the vnode knote. A failed
    /// open (missing path / not a dir) is silently skipped.
    fn watch(&mut self, path: &str) {
        let cpath = match CString::new(path) {
            Ok(c) => c,
            Err(_) => return,
        };
        // SAFETY: open with O_EVTONLY (event-only, no read/write); returns a new
        // fd or -1.
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_EVTONLY) };
        if fd < 0 {
            return;
        }
        let ev = libc::kevent {
            ident: fd as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: VNODE_MASK,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: register the knote on our kq.
        let rc = unsafe {
            libc::kevent(
                self.kq,
                &ev as *const libc::kevent,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            unsafe { libc::close(fd) };
            return;
        }
        self.fd_to_path.insert(fd, path.to_string());
        self.path_to_fd.insert(path.to_string(), fd);
        self.open_fds.fetch_add(1, Ordering::SeqCst);
    }

    /// Close the fd for `path` (the kqueue knote is auto-removed when the fd
    /// closes) and forget its pending debounce.
    fn unwatch(&mut self, path: &str) {
        if let Some(fd) = self.path_to_fd.remove(path) {
            self.fd_to_path.remove(&fd);
            // SAFETY: closing the fd removes its vnode knote from the kq.
            unsafe { libc::close(fd) };
            self.open_fds.fetch_sub(1, Ordering::SeqCst);
        }
        self.pending.remove(path);
    }

    /// Emit — and clear — every pending path whose quiet window ended at or
    /// before `now`. Each send is followed by a main-runloop wake.
    fn emit_ready(&mut self, now: Instant) {
        let ready: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(p, _)| p.clone())
            .collect();
        for path in ready {
            self.pending.remove(&path);
            if self.changes_tx.send(path).is_ok() {
                crate::platform::wake_main_runloop();
            }
        }
    }

    /// The timeout for the next `kevent` block: the soonest pending deadline
    /// (clamped to 0 if already past), or `None` to block indefinitely.
    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        self.pending
            .values()
            .min()
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// Close every watch fd and the kqueue fd (teardown).
    fn close_all(&mut self) {
        let paths: Vec<String> = self.path_to_fd.keys().cloned().collect();
        for path in paths {
            self.unwatch(&path);
        }
        // SAFETY: closing our own kqueue fd; the thread is exiting.
        unsafe { libc::close(self.kq) };
    }
}

fn duration_to_timespec(d: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: d.as_secs() as libc::time_t,
        tv_nsec: d.subsec_nanos() as libc::c_long,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Per-test temp dir under `$TMPDIR` (never the user's real fs — hermeticity).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nice-watcher-{tag}-{}-{}",
            std::process::id(),
            unique()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn unique() -> u64 {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(0);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
            ^ N.fetch_add(1, Ordering::Relaxed)
    }

    fn touch(dir: &std::path::Path, name: &str) {
        fs::write(dir.join(name), b"").unwrap();
    }

    /// Bounded fail-loud poll for a change on `want`. Watcher tests are exempt
    /// from the no-wall-clock rule (this runs against the watcher's OS thread and
    /// mirrors the Swift `DirectoryWatcherTests` timeouts). Returns the number of
    /// emissions observed for `want` within the window.
    fn poll_for(hub: &DirectoryWatcherHub, want: &str, timeout: Duration) -> usize {
        let deadline = Instant::now() + timeout;
        let mut count = 0;
        let mut saw = false;
        while Instant::now() < deadline {
            for p in hub.drain_changes() {
                if p == want {
                    count += 1;
                    saw = true;
                }
            }
            if saw {
                // Keep draining briefly to catch an (undesired) second emission,
                // then stop.
                std::thread::sleep(Duration::from_millis(50));
                for p in hub.drain_changes() {
                    if p == want {
                        count += 1;
                    }
                }
                return count;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        count
    }

    /// `DirectoryWatcherTests.test_creatingFile_firesCallback` — a create in a
    /// watched dir emits the dir's path.
    #[test]
    fn creating_file_fires() {
        let dir = temp_dir("create");
        let hub = DirectoryWatcherHub::new().unwrap();
        let key = dir.to_string_lossy().into_owned();
        hub.set_watched(vec![key.clone()]);
        // Let the thread register the watch before mutating.
        std::thread::sleep(Duration::from_millis(50));
        touch(&dir, "new.txt");
        assert!(poll_for(&hub, &key, Duration::from_secs(2)) >= 1);
        fs::remove_dir_all(&dir).ok();
    }

    /// `DirectoryWatcherTests.test_deletingFile_firesCallback` — a delete emits.
    #[test]
    fn deleting_file_fires() {
        let dir = temp_dir("delete");
        touch(&dir, "doomed.txt");
        let hub = DirectoryWatcherHub::new().unwrap();
        let key = dir.to_string_lossy().into_owned();
        hub.set_watched(vec![key.clone()]);
        std::thread::sleep(Duration::from_millis(50));
        fs::remove_file(dir.join("doomed.txt")).unwrap();
        assert!(poll_for(&hub, &key, Duration::from_secs(2)) >= 1);
        fs::remove_dir_all(&dir).ok();
    }

    /// `DirectoryWatcherTests.test_renamingFile_firesCallback` — a rename emits.
    #[test]
    fn renaming_file_fires() {
        let dir = temp_dir("rename");
        touch(&dir, "before.txt");
        let hub = DirectoryWatcherHub::new().unwrap();
        let key = dir.to_string_lossy().into_owned();
        hub.set_watched(vec![key.clone()]);
        std::thread::sleep(Duration::from_millis(50));
        fs::rename(dir.join("before.txt"), dir.join("after.txt")).unwrap();
        assert!(poll_for(&hub, &key, Duration::from_secs(2)) >= 1);
        fs::remove_dir_all(&dir).ok();
    }

    /// `DirectoryWatcherTests.test_burstOfChangesWithinDebounceWindow_firesCallbackOnce`
    /// — three writes inside the 120 ms window coalesce to a single emission.
    #[test]
    fn burst_within_debounce_coalesces() {
        let dir = temp_dir("burst");
        let hub = DirectoryWatcherHub::new().unwrap();
        let key = dir.to_string_lossy().into_owned();
        hub.set_watched(vec![key.clone()]);
        std::thread::sleep(Duration::from_millis(50));
        touch(&dir, "a.txt");
        touch(&dir, "b.txt");
        touch(&dir, "c.txt");
        assert_eq!(
            poll_for(&hub, &key, Duration::from_secs(2)),
            1,
            "a burst within the 120ms window must coalesce to exactly one emission"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// `DirectoryWatcherTests.test_secondStart_replacesPriorWatch` — a new
    /// desired set drops the old watch; mutating the old dir is silent.
    #[test]
    fn set_watched_replaces_prior_watch() {
        let base = temp_dir("replace");
        let a = base.join("A");
        let b = base.join("B");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let a_key = a.to_string_lossy().into_owned();
        let b_key = b.to_string_lossy().into_owned();

        let hub = DirectoryWatcherHub::new().unwrap();
        hub.set_watched(vec![a_key.clone()]);
        std::thread::sleep(Duration::from_millis(50));
        // Swap the watch to B.
        hub.set_watched(vec![b_key.clone()]);
        std::thread::sleep(Duration::from_millis(50));

        // Mutating B emits.
        touch(&b, "b1.txt");
        assert!(poll_for(&hub, &b_key, Duration::from_secs(2)) >= 1);

        // Mutating A is now silent (its fd was closed by the swap).
        touch(&a, "a1.txt");
        assert_eq!(
            poll_for(&hub, &a_key, Duration::from_millis(400)),
            0,
            "the replaced watch on A must be dead after the swap"
        );
        fs::remove_dir_all(&base).ok();
    }

    /// fd hygiene: watching opens fds; clearing the set closes them (count → 0).
    #[test]
    fn set_watched_empty_closes_all_fds() {
        let base = temp_dir("fds");
        let a = base.join("A");
        let b = base.join("B");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let hub = DirectoryWatcherHub::new().unwrap();
        hub.set_watched(vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        ]);
        // Poll the count up (the open happens on the thread).
        let deadline = Instant::now() + Duration::from_secs(1);
        while hub.open_fd_count() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hub.open_fd_count(), 2, "two watched dirs → two open fds");

        hub.set_watched(vec![]);
        let deadline = Instant::now() + Duration::from_secs(1);
        while hub.open_fd_count() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hub.open_fd_count(), 0, "empty desired set → all fds closed");
        fs::remove_dir_all(&base).ok();
    }

    /// `DirectoryWatcherTests.test_startOnMissingPath_doesNotCrash` — a
    /// non-existent path is skipped, no crash, no fd.
    #[test]
    fn missing_path_is_skipped() {
        let base = temp_dir("missing");
        let nope = base.join("does-not-exist");
        let hub = DirectoryWatcherHub::new().unwrap();
        hub.set_watched(vec![nope.to_string_lossy().into_owned()]);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(hub.open_fd_count(), 0, "a missing path opens no fd");
        fs::remove_dir_all(&base).ok();
    }

    /// fd hygiene + bounded thread-join: after `Drop`, every fd is closed AND the
    /// watcher thread joins within a bounded timeout (the fd count alone won't
    /// catch a leaked thread blocked in `kevent`). Drop runs on a helper thread
    /// so the outer thread can bound the join with a fail-loud poll.
    #[test]
    fn drop_closes_fds_and_joins_thread() {
        let dir = temp_dir("teardown");
        let hub = DirectoryWatcherHub::new().unwrap();
        hub.set_watched(vec![dir.to_string_lossy().into_owned()]);
        let deadline = Instant::now() + Duration::from_secs(1);
        while hub.open_fd_count() < 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hub.open_fd_count(), 1);

        let fds = hub.open_fd_counter();
        let dropped = std::thread::spawn(move || {
            drop(hub); // triggers teardown + joins the watcher thread
        });

        // Bounded fail-loud poll: the drop (and thus the watcher-thread join)
        // must complete promptly thanks to the EVFILT_USER wake.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !dropped.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            dropped.is_finished(),
            "the watcher thread must join within the bounded timeout after Drop"
        );
        dropped.join().unwrap();
        assert_eq!(
            fds.load(Ordering::SeqCst),
            0,
            "every watch fd must be closed after Drop"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
