//! Deadline watchdog — a GUARANTEED-FIRE auto-exit for demand-driven windows
//! (ported verbatim in mechanism from the phase-0 spike's `harness::watchdog`,
//! spike-6 App-Nap finding, 2026-07-02).
//!
//! Why it exists: macOS App Nap indefinitely defers coalescable timers in an
//! idle, occluded gpui app (no draws, no events, display link stopped). The
//! spike observed a 60 s libdispatch deadline not firing within 8 minutes. Any
//! self-test deadline/exit logic therefore CANNOT live on a coalescable timer
//! or the gpui render path alone — a wedged or occluded run would hang forever.
//!
//! This mechanism cannot starve under any of those conditions:
//!   * a dedicated OS thread sleeps to the deadline in drift-corrected 500 ms
//!     slices (`nanosleep` wakeups are scheduler-level, NOT coalescable timers);
//!   * at the deadline it enqueues the registered main-thread callback onto the
//!     libdispatch MAIN queue (`dispatch_async_f` — an enqueue + runloop-port
//!     wakeup, not a timer) AND force-wakes the main CFRunLoop
//!     (`CFRunLoopWakeUp`), retrying every 500 ms;
//!   * if the main thread still hasn't serviced it after ~20 s, the watchdog
//!     prints a diagnostic and hard-exits(3) so a run can never wedge.
//!
//! One watchdog per process. [`arm`] must be called on the MAIN thread and the
//! callback runs on the MAIN thread (it may safely touch gpui entities via
//! `WeakEntity::update` + `AsyncApp`).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[repr(C)]
struct DispatchQueueS {
    _private: [u8; 0],
}

unsafe extern "C" {
    /// libdispatch's main queue object (what `dispatch_get_main_queue()`
    /// expands to). Always linked (libSystem).
    static _dispatch_main_q: DispatchQueueS;
    fn dispatch_async_f(
        queue: *const DispatchQueueS,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    /// CoreFoundation (linked via gpui/AppKit): force the main runloop out of
    /// its wait so the just-enqueued main-queue block runs NOW — immune to
    /// timer coalescing / App Nap.
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopWakeUp(rl: *mut c_void);
}

/// Main-thread-only callback smuggled through a global. Safety: the watchdog
/// thread never touches the contents — only the main-queue trampoline (which
/// runs on the main thread) takes and calls it.
struct ForceSend<T>(T);
unsafe impl<T> Send for ForceSend<T> {}

type Cb = Box<dyn FnMut() + 'static>;
static CB: Mutex<Option<ForceSend<Cb>>> = Mutex::new(None);
static SERVICED: AtomicBool = AtomicBool::new(false);

extern "C" fn trampoline(_ctx: *mut c_void) {
    // MAIN thread (dispatched to the main queue).
    SERVICED.store(true, Ordering::SeqCst);
    let cb = CB.lock().unwrap().take();
    if let Some(ForceSend(mut cb)) = cb {
        cb(); // expected to finalize + process::exit
    }
}

/// Arm the process-wide deadline watchdog (call on the MAIN thread).
/// `on_deadline` runs on the main thread at ~`deadline` and is expected to
/// print a summary and exit the process. The self-test's normal exit lives on
/// the async orchestrator; this watchdog is the backstop that makes auto-exit
/// unconditional for demand-driven / occluded / wedged runs.
pub fn arm(deadline: Duration, label: &'static str, on_deadline: impl FnMut() + 'static) {
    *CB.lock().unwrap() = Some(ForceSend(Box::new(on_deadline)));
    std::thread::Builder::new()
        .name("nice-rs-selftest-watchdog".into())
        .spawn(move || {
            let t0 = Instant::now();
            // Drift-corrected sleep to the deadline in short slices.
            while let Some(rest) = deadline.checked_sub(t0.elapsed()) {
                if rest.is_zero() {
                    break;
                }
                std::thread::sleep(rest.min(Duration::from_millis(500)));
            }
            // Fire: enqueue on the main queue + force the runloop awake.
            for _ in 0..40 {
                if SERVICED.load(Ordering::SeqCst) {
                    return;
                }
                unsafe {
                    dispatch_async_f(&_dispatch_main_q, std::ptr::null_mut(), trampoline);
                    CFRunLoopWakeUp(CFRunLoopGetMain());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if !SERVICED.load(Ordering::SeqCst) {
                eprintln!(
                    "[watchdog] {label}: main thread did not service the deadline within \
                     20 s of forced wakeups — hard exit(3); partial results lost."
                );
                std::process::exit(3);
            }
        })
        .expect("failed to spawn deadline watchdog thread");
}
