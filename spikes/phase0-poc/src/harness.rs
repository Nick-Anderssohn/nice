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
pub fn interval_stats(timestamps: &[u64], cliff_ms: f64) -> IntervalStats {
    let mut intervals: Vec<f64> = timestamps
        .windows(2)
        .map(|w| clock::ms_between(w[0], w[1]))
        .collect();
    let cliffs = intervals.iter().filter(|&&v| v > cliff_ms).count();
    let (p50, p95, p99) = percentiles(&mut intervals);
    IntervalStats {
        samples: timestamps.len(),
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
        fps_p50: if p50 > 0.0 { 1000.0 / p50 } else { 0.0 },
        cliffs,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IntervalStats {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub fps_p50: f64,
    pub cliffs: usize,
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
            "| Term present FPS p50/p95 (ms) | TODO | {:.2}/{:.2} (cliffs {}) | ≤ baseline p95 ×1.15, no cliff cluster |\n",
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
            "| phys_footprint idle (MiB) | TODO | {:.1} | ≤ baseline ×1.2 |\n",
            self.mem_idle_phys_mib
        ));
        s.push_str(&format!(
            "| phys_footprint under-load steady/peak (MiB) | TODO | {:.1}/{:.1} | ≤ baseline ×1.2, no growth |\n",
            self.mem_load_phys_mib, self.mem_peak_phys_mib
        ));
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
