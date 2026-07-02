//! Measurement harness (Harness Spec §A–§H): clock, dual-stack FPS counters,
//! keystroke-latency correlation, memory (mach `task_info` phys_footprint), the
//! deterministic synthetic Claude-streaming workload, and CSV/markdown output.
//!
//! All percentile reduction, the workload generator, the memory FFI, and the
//! draw-attempt proxy run HEADLESS (no display). Only present-RATE, latency, and
//! the z-order/key-window proofs need the on-screen key window (gated in main).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

// ===========================================================================
// §A. Clock & math primitives
// ===========================================================================

pub mod clock {
    use std::sync::OnceLock;

    use mach2::mach_time::{mach_absolute_time, mach_timebase_info, mach_timebase_info_data_t};

    static TB: OnceLock<(u64, u64)> = OnceLock::new();

    fn timebase() -> (u64, u64) {
        *TB.get_or_init(|| {
            let mut t = mach_timebase_info_data_t { numer: 0, denom: 0 };
            unsafe { mach_timebase_info(&mut t) };
            (t.numer as u64, t.denom as u64)
        })
    }

    /// Monotonic raw ticks (the single clock for every measurement, Rust + Swift).
    #[inline]
    pub fn now() -> u64 {
        unsafe { mach_absolute_time() }
    }

    #[inline]
    pub fn ns_between(a: u64, b: u64) -> f64 {
        let (n, d) = timebase();
        (b.wrapping_sub(a) as f64) * (n as f64) / (d as f64)
    }

    /// Convert a mach-absolute-time TICK COUNT (a duration, not an instant)
    /// to nanoseconds. Used for `rusage_info_v4` CPU times, which are
    /// reported in mach ticks.
    #[inline]
    pub fn ticks_to_ns(ticks: u64) -> f64 {
        let (n, d) = timebase();
        (ticks as f64) * (n as f64) / (d as f64)
    }

    #[inline]
    pub fn ms_between(a: u64, b: u64) -> f64 {
        ns_between(a, b) / 1.0e6
    }
}

// ===========================================================================
// §B. FPS / frame-timing counters (two INDEPENDENT present counters)
// ===========================================================================

/// SwiftTerm present timestamps (CPU frame-submitted; see Bridge.swift note on
/// why this is not GPU-complete without a fork patch).
static FPS_TERM: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// SwiftTerm draw-attempt timestamps (display-independent — fires headless).
static FPS_DRAW: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// GPUI composite timestamps (the chrome stack over the terminal).
static FPS_GPUI: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Single in-flight keystroke sentinel (set before injecting, cleared on the
/// first terminal present that follows).
static PENDING_T0: AtomicU64 = AtomicU64::new(0);
/// Correlated end-to-end keystroke latencies, in ms.
static LATENCY: Mutex<Vec<f64>> = Mutex::new(Vec::new());

/// extern "C" trampoline registered as the SwiftTerm DRAW-attempt hook.
pub extern "C" fn on_draw_attempt(ts: u64) {
    FPS_DRAW.lock().unwrap().push(ts);
}

/// extern "C" trampoline registered as the SwiftTerm PRESENT hook. Closes the
/// keystroke-latency loop (Harness §C.2).
pub extern "C" fn on_present(ts: u64) {
    FPS_TERM.lock().unwrap().push(ts);
    let t0 = PENDING_T0.swap(0, Ordering::SeqCst);
    if t0 != 0 {
        LATENCY.lock().unwrap().push(clock::ms_between(t0, ts));
    }
}

/// Call right BEFORE injecting a keystroke (Harness §C.1). One at a time.
pub fn arm_keystroke() {
    PENDING_T0.store(clock::now(), Ordering::SeqCst);
}

/// Whether the in-flight keystroke has been answered by a present yet.
pub fn keystroke_pending() -> bool {
    PENDING_T0.load(Ordering::SeqCst) != 0
}

/// Stamp one GPUI composite (call from the gpui render/next-frame loop).
pub fn stamp_gpui_frame() {
    FPS_GPUI.lock().unwrap().push(clock::now());
}

/// Cheap count of correlated keystroke-latency samples so far (the live driver
/// polls this each frame instead of cloning the whole stream).
pub fn latency_len() -> usize {
    LATENCY.lock().unwrap().len()
}

/// Clear every frame/latency stream. The live driver calls this at the moment
/// streaming begins so the IDLE warm-up frames (gpui composites before the first
/// byte is fed) don't pollute the UNDER-LOAD percentiles.
pub fn reset_frame_streams() {
    FPS_TERM.lock().unwrap().clear();
    FPS_DRAW.lock().unwrap().clear();
    FPS_GPUI.lock().unwrap().clear();
    LATENCY.lock().unwrap().clear();
    PENDING_T0.store(0, Ordering::SeqCst);
}

/// Snapshot the three frame-timestamp streams (cloned, sorted later).
pub fn drain_frame_streams() -> FrameStreams {
    FrameStreams {
        term_present: FPS_TERM.lock().unwrap().clone(),
        draw_attempt: FPS_DRAW.lock().unwrap().clone(),
        gpui_composite: FPS_GPUI.lock().unwrap().clone(),
        latency_ms: LATENCY.lock().unwrap().clone(),
    }
}

pub struct FrameStreams {
    pub term_present: Vec<u64>,
    pub draw_attempt: Vec<u64>,
    pub gpui_composite: Vec<u64>,
    pub latency_ms: Vec<f64>,
}

/// p50/p95/p99 of frame INTERVALS (ms) from a timestamp stream, plus a count of
/// "cliff" intervals exceeding `cliff_ms` (Harness §B.3 dropped-frame cliffs).
///
/// §13 harness fix: also reports `max_ms` and a SELF-CALIBRATED cliff count
/// (`cliffs_auto` over threshold `cliff_auto_ms` = 1.5 × the run's own median
/// interval) — the audit flagged the fixed 16.6 ms threshold as inoperative
/// on a 60 Hz panel (it counts nearly every frame). The fixed-threshold count
/// is retained for continuity with previously published numbers.
pub fn interval_stats(timestamps: &[u64], cliff_ms: f64) -> IntervalStats {
    let mut intervals: Vec<f64> = timestamps
        .windows(2)
        .map(|w| clock::ms_between(w[0], w[1]))
        .collect();
    let cliffs = intervals.iter().filter(|&&v| v > cliff_ms).count();
    let max_ms = intervals.iter().cloned().fold(0.0_f64, f64::max);
    let (p50, p95, p99) = percentiles(&mut intervals);
    let cliff_auto_ms = 1.5 * p50;
    let cliffs_auto = if p50 > 0.0 {
        intervals.iter().filter(|&&v| v > cliff_auto_ms).count()
    } else {
        0
    };
    IntervalStats {
        samples: timestamps.len(),
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
        max_ms,
        fps_p50: if p50 > 0.0 { 1000.0 / p50 } else { 0.0 },
        cliffs,
        cliff_auto_ms,
        cliffs_auto,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IntervalStats {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub fps_p50: f64,
    pub cliffs: usize,
    /// Self-calibrated cliff threshold: 1.5 × this run's median interval.
    pub cliff_auto_ms: f64,
    /// Intervals exceeding `cliff_auto_ms`.
    pub cliffs_auto: usize,
}

/// Percentiles of an arbitrary f64 sample set (sorts in place). Capture raw,
/// reduce at the end — never online (Harness §A).
pub fn percentiles(v: &mut Vec<f64>) -> (f64, f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = |p: usize| -> f64 {
        let i = (v.len() * p) / 100;
        v[i.min(v.len() - 1)]
    };
    (v[v.len() / 2], idx(95), idx(99))
}

// ===========================================================================
// §D. Memory — task_info phys_footprint (hand-declared struct; mach2 0.4 omits it)
// ===========================================================================

pub mod mem {
    use mach2::task::task_info;
    use mach2::task_info::TASK_VM_INFO;
    use mach2::traps::mach_task_self;

    /// `struct task_vm_info` from <mach/task_info.h> (rev7), laid out exactly so
    /// `phys_footprint`'s offset is correct. `mach_vm_size_t`/`mach_vm_address_t`
    /// = u64, `integer_t` = i32, ledger fields = i64.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TaskVmInfo {
        pub virtual_size: u64,
        pub region_count: i32,
        pub page_size: i32,
        pub resident_size: u64,
        pub resident_size_peak: u64,
        pub device: u64,
        pub device_peak: u64,
        pub internal: u64,
        pub internal_peak: u64,
        pub external: u64,
        pub external_peak: u64,
        pub reusable: u64,
        pub reusable_peak: u64,
        pub purgeable_volatile_pmap: u64,
        pub purgeable_volatile_resident: u64,
        pub purgeable_volatile_virtual: u64,
        pub compressed: u64,
        pub compressed_peak: u64,
        pub compressed_lifetime: u64,
        pub phys_footprint: u64, // rev1
        pub min_address: u64,    // rev2
        pub max_address: u64,
        pub ledger_phys_footprint_peak: i64, // rev3
        pub ledger_purgeable_nonvolatile: i64,
        pub ledger_purgeable_novolatile_compressed: i64,
        pub ledger_purgeable_volatile: i64,
        pub ledger_purgeable_volatile_compressed: i64,
        pub ledger_tag_network_nonvolatile: i64,
        pub ledger_tag_network_nonvolatile_compressed: i64,
        pub ledger_tag_network_volatile: i64,
        pub ledger_tag_network_volatile_compressed: i64,
        pub ledger_tag_media_footprint: i64,
        pub ledger_tag_media_footprint_compressed: i64,
        pub ledger_tag_media_nofootprint: i64,
        pub ledger_tag_media_nofootprint_compressed: i64,
        pub ledger_tag_graphics_footprint: i64,
        pub ledger_tag_graphics_footprint_compressed: i64,
        pub ledger_tag_graphics_nofootprint: i64,
        pub ledger_tag_graphics_nofootprint_compressed: i64,
        pub ledger_tag_neural_footprint: i64,
        pub ledger_tag_neural_footprint_compressed: i64,
        pub ledger_tag_neural_nofootprint: i64,
        pub ledger_tag_neural_nofootprint_compressed: i64,
        pub limit_bytes_remaining: u64, // rev4
        pub decompressions: i32,        // rev5
        pub ledger_swapins: i64,        // rev6
        pub ledger_tag_neural_nofootprint_total: i64, // rev7
        pub ledger_tag_neural_nofootprint_peak: i64,
    }

    /// (`phys_footprint`, `resident_size`) in bytes for THIS process.
    pub fn sample() -> (u64, u64) {
        let mut info = unsafe { std::mem::zeroed::<TaskVmInfo>() };
        let mut count =
            (std::mem::size_of::<TaskVmInfo>() / std::mem::size_of::<u32>()) as u32;
        let kr = unsafe {
            task_info(
                mach_task_self(),
                TASK_VM_INFO,
                &mut info as *mut _ as *mut i32,
                &mut count,
            )
        };
        if kr != 0 {
            return (0, 0);
        }
        (info.phys_footprint, info.resident_size)
    }

    #[inline]
    pub fn mib(bytes: u64) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }
}

// ===========================================================================
// §D2. CPU / energy proxy — proc_pid_rusage(RUSAGE_INFO_V4) (spike 6).
// No sudo required. `ri_user_time`/`ri_system_time` are in mach ticks;
// `ri_billed_energy` is in nanojoules (0 on machines/kernels that don't
// account it — report it only when non-zero). The struct is hand-declared
// (libc ships the call but not the v4 struct), same approach as TaskVmInfo.
// ===========================================================================

pub mod cpu {
    use std::ffi::c_int;

    const RUSAGE_INFO_V4: c_int = 4;

    /// `struct rusage_info_v4` from <sys/resource.h>, laid out exactly.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RusageInfoV4 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
        ri_cpu_time_qos_default: u64,
        ri_cpu_time_qos_maintenance: u64,
        ri_cpu_time_qos_background: u64,
        ri_cpu_time_qos_utility: u64,
        ri_cpu_time_qos_legacy: u64,
        ri_cpu_time_qos_user_initiated: u64,
        ri_cpu_time_qos_user_interactive: u64,
        ri_billed_system_time: u64,
        ri_serviced_system_time: u64,
        ri_logical_writes: u64,
        ri_lifetime_max_phys_footprint: u64,
        ri_instructions: u64,
        ri_cycles: u64,
        ri_billed_energy: u64,
        ri_serviced_energy: u64,
        ri_interval_max_phys_footprint: u64,
        ri_runnable_time: u64,
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut RusageInfoV4) -> c_int;
    }

    /// One point-in-time CPU/energy sample of THIS process.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct CpuSample {
        pub user_ns: f64,
        pub system_ns: f64,
        pub pkg_idle_wakeups: u64,
        pub interrupt_wakeups: u64,
        /// Nanojoules billed to the process (0 when the kernel doesn't account it).
        pub billed_energy_nj: u64,
        pub instructions: u64,
        pub cycles: u64,
    }

    pub fn sample() -> Option<CpuSample> {
        let mut info = RusageInfoV4::default();
        let pid = std::process::id() as c_int;
        let kr = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V4, &mut info) };
        if kr != 0 {
            return None;
        }
        Some(CpuSample {
            user_ns: super::clock::ticks_to_ns(info.ri_user_time),
            system_ns: super::clock::ticks_to_ns(info.ri_system_time),
            pkg_idle_wakeups: info.ri_pkg_idle_wkups,
            interrupt_wakeups: info.ri_interrupt_wkups,
            billed_energy_nj: info.ri_billed_energy,
            instructions: info.ri_instructions,
            cycles: info.ri_cycles,
        })
    }

    /// Human summary of the delta between two samples over `wall_s` seconds:
    /// CPU time + average core utilization, wakeup rate, and the no-sudo
    /// energy proxy (average mW from billed nanojoules) when available.
    pub fn delta_summary(t0: &CpuSample, t1: &CpuSample, wall_s: f64) -> String {
        let cpu_ms = (t1.user_ns - t0.user_ns + t1.system_ns - t0.system_ns) / 1.0e6;
        let user_ms = (t1.user_ns - t0.user_ns) / 1.0e6;
        let sys_ms = (t1.system_ns - t0.system_ns) / 1.0e6;
        let pct = if wall_s > 0.0 {
            cpu_ms / (wall_s * 1000.0) * 100.0
        } else {
            0.0
        };
        let wk = t1.pkg_idle_wakeups.saturating_sub(t0.pkg_idle_wakeups);
        let iwk = t1.interrupt_wakeups.saturating_sub(t0.interrupt_wakeups);
        let energy = t1.billed_energy_nj.saturating_sub(t0.billed_energy_nj);
        let mut s = format!(
            "cpu {cpu_ms:.0} ms ({pct:.1}% of one core; user {user_ms:.0} / sys {sys_ms:.0}) | \
             wakeups pkg-idle {wk} ({:.1}/s) intr {iwk}",
            if wall_s > 0.0 { wk as f64 / wall_s } else { 0.0 }
        );
        if energy > 0 {
            let mj = energy as f64 / 1.0e6;
            let mw = if wall_s > 0.0 { mj / wall_s } else { 0.0 };
            s.push_str(&format!(" | energy {mj:.1} mJ (~{mw:.1} mW avg, proc_pid_rusage)"));
        } else {
            s.push_str(" | energy n/a (ri_billed_energy=0; use sudo powermetrics)");
        }
        let instr = t1.instructions.saturating_sub(t0.instructions);
        let cyc = t1.cycles.saturating_sub(t0.cycles);
        if instr > 0 {
            s.push_str(&format!(
                " | {:.2}G instr / {:.2}G cycles",
                instr as f64 / 1.0e9,
                cyc as f64 / 1.0e9
            ));
        }
        s
    }
}

// ===========================================================================
// §D3. Deadline watchdog (spike 6 hang fix, 2026-07-02) — a GUARANTEED-FIRE
// auto-exit for demand-driven windows.
//
// Why it exists: the energy `idle` live run hung forever on the previous
// mechanism (gpui BackgroundExecutor::timer -> foreground entity update).
// That path is dispatch_after on a global queue + a repoll dispatched to the
// main queue — mechanically runloop-independent, but a fully idle app (no
// draws, no events, display link stopped, window occluded) is exactly what
// macOS App Nap targets: its timers get coalesced/deferred, and the observed
// live behavior was a 60 s deadline not firing within 8 minutes. The
// interactive mode's identical timer only ever fired in runs kept un-napped
// by real input.
//
// This mechanism cannot starve under any of those conditions:
//   * a dedicated OS thread sleeps to the deadline in drift-corrected 500 ms
//     slices (nanosleep wakeups are scheduler-level, NOT coalescable timers);
//   * at the deadline it enqueues the registered main-thread callback onto
//     the libdispatch MAIN queue (dispatch_async_f — an enqueue + runloop
//     port wakeup, not a timer) AND force-wakes the main CFRunLoop
//     (CFRunLoopWakeUp), retrying every 500 ms;
//   * if the main thread still hasn't serviced it after 20 s, the watchdog
//     prints a diagnostic and hard-exits(3) so a run can never wedge.
//
// One watchdog per process. `arm()` must be called on the MAIN thread and
// the callback runs on the MAIN thread (it may safely touch gpui entities
// via WeakEntity::update + AsyncApp).
// ===========================================================================

pub mod watchdog {
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct DispatchQueueS {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        /// libdispatch's main queue object (what dispatch_get_main_queue()
        /// expands to). Always linked (libSystem).
        static _dispatch_main_q: DispatchQueueS;
        fn dispatch_async_f(
            queue: *const DispatchQueueS,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
        /// CoreFoundation (linked via gpui/AppKit): force the main runloop
        /// out of its wait so the just-enqueued main-queue block runs NOW —
        /// immune to timer coalescing/App Nap.
        fn CFRunLoopGetMain() -> *mut c_void;
        fn CFRunLoopWakeUp(rl: *mut c_void);
    }

    /// Main-thread-only callback smuggled through a global. Safety: the
    /// watchdog thread never touches the contents — only the main-queue
    /// trampoline (which runs on the main thread) takes and calls it.
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
    /// `on_deadline` runs on the main thread at ~`deadline` and is expected
    /// to print the summary and exit the process. Streaming modes' render-
    /// path deadline usually exits first — the watchdog is the backstop that
    /// makes auto-exit unconditional for demand-driven/occluded windows.
    pub fn arm(deadline: Duration, label: &'static str, on_deadline: impl FnMut() + 'static) {
        *CB.lock().unwrap() = Some(ForceSend(Box::new(on_deadline)));
        std::thread::Builder::new()
            .name("poc-deadline-watchdog".into())
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
}

// ===========================================================================
// §E2. Real-trace workload (spike 7) — record/replay a REAL pty byte stream
// with timing.
//
// FORMAT ("nicetrace v1", little-endian):
//   magic   8 bytes  b"NICEPTY1"
//   start   u64      unix epoch millis at capture start (metadata only)
//   records repeated until EOF:
//     off_ns u64     monotonic offset from capture start (nanoseconds)
//     len    u32     chunk byte length
//     data   len bytes  raw pty OUTPUT bytes (child -> master)
// A truncated trailing record (crash mid-write) is tolerated on load.
// ===========================================================================

pub mod trace {
    use std::fs::File;
    use std::io::{BufReader, BufWriter, Read, Write};
    use std::path::Path;
    use std::time::Instant;

    pub const MAGIC: &[u8; 8] = b"NICEPTY1";

    pub struct TraceRecord {
        pub offset_ns: u64,
        pub data: Vec<u8>,
    }

    pub struct Trace {
        pub start_unix_ms: u64,
        pub records: Vec<TraceRecord>,
        pub total_bytes: u64,
        pub duration_ns: u64,
    }

    impl Trace {
        pub fn load(path: &Path) -> std::io::Result<Trace> {
            let f = File::open(path)?;
            let mut r = BufReader::new(f);
            let mut magic = [0u8; 8];
            r.read_exact(&mut magic)?;
            if &magic != MAGIC {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "not a nicetrace file (magic {:02x?}; want {MAGIC:02x?})",
                        magic
                    ),
                ));
            }
            let mut u64buf = [0u8; 8];
            r.read_exact(&mut u64buf)?;
            let start_unix_ms = u64::from_le_bytes(u64buf);
            let mut records = Vec::new();
            let mut total_bytes = 0u64;
            let mut duration_ns = 0u64;
            loop {
                match r.read_exact(&mut u64buf) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                }
                let offset_ns = u64::from_le_bytes(u64buf);
                let mut u32buf = [0u8; 4];
                if r.read_exact(&mut u32buf).is_err() {
                    break; // truncated tail — keep what we have
                }
                let len = u32::from_le_bytes(u32buf) as usize;
                let mut data = vec![0u8; len];
                if r.read_exact(&mut data).is_err() {
                    break; // truncated tail
                }
                total_bytes += len as u64;
                duration_ns = duration_ns.max(offset_ns);
                records.push(TraceRecord { offset_ns, data });
            }
            Ok(Trace {
                start_unix_ms,
                records,
                total_bytes,
                duration_ns,
            })
        }

        pub fn duration_secs(&self) -> f64 {
            self.duration_ns as f64 / 1.0e9
        }
    }

    /// Streaming writer used by the `pty-capture` bin (and self-tests).
    pub struct TraceWriter {
        out: BufWriter<File>,
        t0: Instant,
        last_flush: Instant,
        pub records: u64,
        pub bytes: u64,
    }

    impl TraceWriter {
        pub fn create(path: &Path) -> std::io::Result<TraceWriter> {
            let f = File::create(path)?;
            let mut out = BufWriter::new(f);
            out.write_all(MAGIC)?;
            let start_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            out.write_all(&start_unix_ms.to_le_bytes())?;
            let now = Instant::now();
            Ok(TraceWriter {
                out,
                t0: now,
                last_flush: now,
                records: 0,
                bytes: 0,
            })
        }

        /// Append one output chunk stamped with "now" (offset from create()).
        pub fn record(&mut self, data: &[u8]) -> std::io::Result<()> {
            self.record_at(self.t0.elapsed().as_nanos() as u64, data)
        }

        /// Append one output chunk at an explicit offset (converter path).
        pub fn record_at(&mut self, offset_ns: u64, data: &[u8]) -> std::io::Result<()> {
            self.out.write_all(&offset_ns.to_le_bytes())?;
            self.out.write_all(&(data.len() as u32).to_le_bytes())?;
            self.out.write_all(data)?;
            self.records += 1;
            self.bytes += data.len() as u64;
            // Bound data loss on an abrupt kill without per-record fsync.
            if self.last_flush.elapsed().as_millis() > 500 {
                self.out.flush()?;
                self.last_flush = Instant::now();
            }
            Ok(())
        }

        /// Flush + return (records, bytes, capture wall seconds).
        pub fn finish(mut self) -> std::io::Result<(u64, u64, f64)> {
            self.out.flush()?;
            Ok((self.records, self.bytes, self.t0.elapsed().as_secs_f64()))
        }
    }
}

// ===========================================================================
// §E. Synthetic Claude-streaming workload (deterministic, seeded)
// ===========================================================================

#[derive(Clone, Copy, Debug)]
pub struct WorkloadProfile {
    pub seed: u64,
    pub bytes_per_sec: usize,
    pub burst_chunk: (usize, usize),
    pub duration_s: u32,
}

impl Default for WorkloadProfile {
    fn default() -> Self {
        WorkloadProfile {
            seed: 42,
            bytes_per_sec: 500_000,
            burst_chunk: (16, 512),
            duration_s: 60,
        }
    }
}

/// Deterministic xorshift64*; identical stream per seed across PoC + baseline.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    #[inline]
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() as usize) % (hi - lo)
    }
}

/// The renderer-stressor content mix (Harness §E.2).
pub struct Workload {
    rng: Rng,
    prof: WorkloadProfile,
}

impl Workload {
    pub fn new(prof: WorkloadProfile) -> Self {
        Workload {
            rng: Rng::new(prof.seed),
            prof,
        }
    }

    /// Produce ONE burst chunk (a few hundred bytes). Weighted: 40% SGR-heavy,
    /// 30% line-redraw/reflow, 15% long lines, 10% unicode/box, 5% plain.
    pub fn next_chunk(&mut self) -> Vec<u8> {
        let pick = self.rng.range(0, 100);
        let target = self.rng.range(self.prof.burst_chunk.0, self.prof.burst_chunk.1);
        let mut out = Vec::with_capacity(target + 32);
        match pick {
            0..=39 => self.sgr_heavy(&mut out, target),
            40..=69 => self.line_redraw(&mut out, target),
            70..=84 => self.long_line(&mut out, target),
            85..=94 => self.unicode_box(&mut out, target),
            _ => self.plain_ascii(&mut out, target),
        }
        out
    }

    /// A deterministic byte stream of ~`bytes` total (for fixture.bin).
    pub fn stream(&mut self, bytes: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes + 1024);
        while out.len() < bytes {
            out.extend_from_slice(&self.next_chunk());
        }
        out.truncate(bytes);
        out
    }

    /// Dump a fixed-size deterministic stream to disk (replayed into Nice Dev
    /// for the baseline so input is byte-identical — Harness §E.3 / §F).
    pub fn dump_fixture(&mut self, path: &Path, bytes: usize) -> std::io::Result<()> {
        let data = self.stream(bytes);
        std::fs::write(path, data)
    }

    fn sgr_heavy(&mut self, out: &mut Vec<u8>, target: usize) {
        while out.len() < target {
            let (r, g, b) = (
                self.rng.range(0, 256),
                self.rng.range(0, 256),
                self.rng.range(0, 256),
            );
            out.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
            if self.rng.range(0, 2) == 0 {
                out.extend_from_slice(b"\x1b[1m");
            }
            if self.rng.range(0, 3) == 0 {
                out.extend_from_slice(b"\x1b[4m");
            }
            let words = self.rng.range(2, 8);
            for _ in 0..words {
                out.extend_from_slice(b"token ");
            }
            out.extend_from_slice(b"\x1b[0m");
        }
        out.extend_from_slice(b"\r\n");
    }

    fn line_redraw(&mut self, out: &mut Vec<u8>, target: usize) {
        // The streaming-rewrite idiom: CR + clear-line, re-emit the same line.
        let reps = (target / 24).max(1);
        for i in 0..reps {
            out.extend_from_slice(b"\r\x1b[2K");
            out.extend_from_slice(format!("working... {}%", i % 100).as_bytes());
        }
        // cursor-up overwrite of an N-line status block + save/restore.
        out.extend_from_slice(b"\x1b7\x1b[3A\x1b[2Kspinner\x1b8");
    }

    fn long_line(&mut self, out: &mut Vec<u8>, target: usize) {
        let cols = self.rng.range(200, 2000.min(200 + target * 4).max(201));
        for i in 0..cols {
            out.push(b'a' + (i % 26) as u8);
        }
        out.extend_from_slice(b"\r\n");
    }

    fn unicode_box(&mut self, out: &mut Vec<u8>, target: usize) {
        let glyphs = ["─", "│", "┌", "┐", "└", "┘", "█", "🚀", "é", "中", "🧩"];
        while out.len() < target {
            let g = glyphs[self.rng.range(0, glyphs.len())];
            out.extend_from_slice(g.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }

    fn plain_ascii(&mut self, out: &mut Vec<u8>, target: usize) {
        while out.len() < target {
            out.extend_from_slice(b"the quick brown fox jumps over the lazy dog ");
        }
        out.extend_from_slice(b"\r\n");
    }
}

// ===========================================================================
// §H. Results — raw CSV + reduced markdown
// ===========================================================================

#[derive(Clone)]
pub struct ProofGate {
    pub item: u8,
    pub name: &'static str,
    /// None = UNPROVEN (display-gated / not wired), Some(true/false) = measured.
    pub pass: Option<bool>,
    pub note: &'static str,
}

pub struct Results {
    pub seed: u64,
    pub bytes_per_sec: usize,
    pub term: IntervalStats,
    pub draw: IntervalStats,
    pub gpui: IntervalStats,
    pub latency_seam: (f64, f64, f64), // p50/p95/p99 ms
    pub latency_pty: (f64, f64, f64),
    pub mem_idle_phys_mib: f64,
    pub mem_load_phys_mib: f64,
    pub mem_peak_phys_mib: f64,
    pub mem_idle_rss_mib: f64,
    pub metal_active: bool,
    pub proofs: Vec<ProofGate>,
}

/// One raw CSV row per the §H.1 schema.
fn csv_row(
    w: &mut impl Write,
    metric: &str,
    stack: &str,
    phase: &str,
    seed: u64,
    bps: usize,
    idx: usize,
    value: f64,
    unit: &str,
) -> std::io::Result<()> {
    writeln!(
        w,
        "{metric},{stack},{phase},poc,{seed},{bps},{idx},{value},{unit}"
    )
}

impl Results {
    /// Emit the raw per-sample CSV (Harness §H.1).
    pub fn write_csv(&self, path: &Path, streams: &FrameStreams) -> std::io::Result<()> {
        let f = File::create(path)?;
        let mut w = BufWriter::new(f);
        writeln!(
            w,
            "metric,stack,phase,run,seed,bytes_per_sec,sample_index,value,unit"
        )?;
        for (i, win) in streams.term_present.windows(2).enumerate() {
            csv_row(
                &mut w,
                "fps",
                "term",
                "under_load",
                self.seed,
                self.bytes_per_sec,
                i,
                clock::ms_between(win[0], win[1]),
                "ms_interval",
            )?;
        }
        for (i, win) in streams.gpui_composite.windows(2).enumerate() {
            csv_row(
                &mut w,
                "fps",
                "gpui",
                "under_load",
                self.seed,
                self.bytes_per_sec,
                i,
                clock::ms_between(win[0], win[1]),
                "ms_interval",
            )?;
        }
        for (i, v) in streams.latency_ms.iter().enumerate() {
            csv_row(
                &mut w,
                "latency",
                "seam",
                "loopback",
                self.seed,
                self.bytes_per_sec,
                i,
                *v,
                "ms",
            )?;
        }
        for p in &self.proofs {
            let val = match p.pass {
                Some(true) => "pass",
                Some(false) => "fail",
                None => "unproven",
            };
            writeln!(
                w,
                "proof,item{},na,poc,{},{},0,{},gate",
                p.item, self.seed, self.bytes_per_sec, val
            )?;
        }
        w.flush()
    }

    /// Reduced markdown table (Harness §H.2) — what a human reads to apply §10.
    pub fn markdown(&self) -> String {
        let mut s = String::new();
        s.push_str("## Phase-0 PoC — measured (PoC dual-stack only; fill Baseline from baseline/NOTES.md)\n\n");
        s.push_str(&format!(
            "seed={} bytes_per_sec={} metal_renderer={}\n\n",
            self.seed,
            self.bytes_per_sec,
            if self.metal_active { "REAL" } else { "stub/unavailable" }
        ));
        s.push_str("| Metric | Baseline (Nice Dev) | PoC (dual-stack) | Gate |\n");
        s.push_str("|---|---|---|---|\n");
        s.push_str(&format!(
            "| Term present FPS p50/p95 (ms) | ~16.7/17.1† | {:.2}/{:.2} (cliffs {}) | ≤ baseline p95 ×1.15, no cliff cluster |\n",
            self.term.p50_ms, self.term.p95_ms, self.term.cliffs
        ));
        s.push_str(&format!(
            "| Draw-attempt p50/p95 (ms, headless proxy) | n/a | {:.2}/{:.2} | encode throughput |\n",
            self.draw.p50_ms, self.draw.p95_ms
        ));
        s.push_str(&format!(
            "| GPUI composite FPS p50/p95 (ms) | n/a | {:.2}/{:.2} (cliffs {}) | ≥ refresh sustained |\n",
            self.gpui.p50_ms, self.gpui.p95_ms, self.gpui.cliffs
        ));
        s.push_str(&format!(
            "| Keystroke latency seam p50/p95/p99 (ms) | — | {:.2}/{:.2}/{:.2} | ≤ ~1 ProMotion frame |\n",
            self.latency_seam.0, self.latency_seam.1, self.latency_seam.2
        ));
        s.push_str(&format!(
            "| Keystroke latency pty echo p50/p95/p99 (ms) | TODO | {:.2}/{:.2}/{:.2} | informational |\n",
            self.latency_pty.0, self.latency_pty.1, self.latency_pty.2
        ));
        s.push_str(&format!(
            "| phys_footprint idle (MiB) | ~69† | {:.1} | ≤ baseline ×1.2 |\n",
            self.mem_idle_phys_mib
        ));
        s.push_str(&format!(
            "| phys_footprint under-load steady/peak (MiB) | ~111/114† | {:.1}/{:.1} | ≤ baseline ×1.2, no growth |\n",
            self.mem_load_phys_mib, self.mem_peak_phys_mib
        ));
        s.push_str(
            "\n> † Baseline = current Nice (Nice Dev), single Metal terminal layer + AppKit/SwiftUI \
             chrome, 60 Hz panel. Memory: phys_footprint of Nice Dev 0.29.0 under the same fixture, \
             Metal active (1 pane). Term FPS: the single-Metal-layer-at-refresh rate established by \
             this PoC's own single-stack controls on the identical SwiftTerm Metal renderer/display/ \
             workload (`link` terminal-alone 16.68/17.13, `none` GPUI-alone 16.70/19.18) — a fresh \
             signpost-emitting Nice Dev build is blocked by the missing Xcode Metal Toolchain \
             component and 0.29.0 predates the SWIFTTERM_PROFILE signpost. Keystroke-latency pty-echo \
             baseline deferred (needs Accessibility TCC for Nice Dev). See report §10.\n",
        );
        s.push_str("\n### Proof gates (PoC items 4–7)\n\n");
        for p in &self.proofs {
            let val = match p.pass {
                Some(true) => "PASS",
                Some(false) => "FAIL",
                None => "UNPROVEN (display-gated)",
            };
            s.push_str(&format!("- **({}) {}**: {} — {}\n", p.item, p.name, val, p.note));
        }
        s.push_str("\n### §10 decision tree\n");
        s.push_str("- All FPS+latency+memory+proofs PASS -> **Path A** (reuse renderer, chrome on GPUI).\n");
        s.push_str("- Proofs 4/5 FAIL (z-order/responder seam) but FPS/memory PASS -> **objc2-hybrid**.\n");
        s.push_str("- FPS or memory FAIL (dual-stack tax) -> **Path B** (alacritty_terminal + GPUI-native).\n");
        s.push_str("- Broad FAIL -> revert to in-place AppKit baseline.\n");
        s
    }
}

/// One-time init log line so a headless `cargo run` proves the harness wired up.
pub fn banner() -> &'static str {
    static B: OnceLock<()> = OnceLock::new();
    B.get_or_init(|| {});
    "phase0-poc harness initialized (clock/fps/latency/mem/workload/results ready)"
}
