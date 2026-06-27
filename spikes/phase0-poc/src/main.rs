//! Phase-0 PoC — ONE throwaway GPUI window hosting the REAL SwiftTerm Metal
//! NSView (or the headless stub) under GPUI chrome, plus the measurement
//! harness that resolves the §10 decision tree (Path A / objc2-hybrid / Path B).
//!
//! Two run modes (env `NICE_POC_RUN`):
//!   * UNSET / "0"  — HEADLESS SELF-TEST (default). Exercises the pure-Rust
//!     harness (workload generator, mach memory sampler, percentile reducer,
//!     CSV/markdown emit) with NO window and NO AppKit. Safe on a box with no
//!     display; this is what `cargo run` does in CI. Prints the markdown report
//!     with every display-gated proof marked UNPROVEN.
//!   * "1"          — DISPLAY-GATED LIVE RUN. Opens the transparent GPUI window,
//!     brings it to the front + makes it key, embeds the terminal NSView below
//!     the chrome, makes it first responder, and runs a SELF-TERMINATING
//!     measurement: idle baseline -> stream the workload for NICE_POC_SECS
//!     (default 25) while injecting one keystroke/frame, probe the mouse
//!     hit-test seam + metal rebind, then print a POPULATED Results table and
//!     exit(0). Ctrl-C (SIGINT/SIGTERM) and window-close/Cmd-Q also report+exit.
//!     REQUIRES a display + key window. The subagent that wrote this MUST NOT
//!     run this mode.
//!
//! See README.md for the full runbook, the 7 proofs, and the decision tree.

// Much of the input/harness API surface (synth_mouse, flags_changed, IME hooks,
// remove_monitor, the hit-test shim, etc.) is only invoked by the DISPLAY-GATED
// live driver (a documented TODO in README §Display-gated), so it reads as dead
// code under a headless `cargo check`. That is expected for this scaffold.
#![allow(dead_code)]

mod bridge;
mod embed;
mod harness;
mod input;

use std::path::Path;

use harness::{Results, WorkloadProfile, Workload};

fn proof_gates_unproven() -> Vec<harness::ProofGate> {
    vec![
        harness::ProofGate {
            item: 4,
            name: "live responder / IME / first-responder arbitration",
            pass: None,
            note: "keyboard+IME EXPECTED PASS (no sendEvent: override); inject via NSApp.sendEvent in live run",
        },
        harness::ProofGate {
            item: 5,
            name: "transparent GPUI region over terminal (no z-order/blanking)",
            pass: None,
            note: "EXPECTED PASS: gpui metal layer opaque=false, clears alpha 0; verify with CGWindowList capture",
        },
        harness::ProofGate {
            item: 6,
            name: "cross-window Metal-layer rebind on tear-off",
            pass: None,
            note: "reparent term to 2nd window + set_use_metal(false->true); assert presents resume",
        },
        harness::ProofGate {
            item: 7,
            name: "process-wide swallow/passthrough NSEvent monitor + GPUI focus",
            pass: None,
            note: "install_swallow_monitor coexists with gpui (no competing swallow monitor)",
        },
    ]
}

fn main() {
    let live = matches!(std::env::var("NICE_POC_RUN").as_deref(), Ok("1") | Ok("true"));
    if live {
        gui::run_live();
    } else {
        run_headless();
    }
}

/// Headless harness plumbing check — proves the measurement code compiles and
/// runs end-to-end WITHOUT a display. NOT real render FPS (no Metal present
/// loop without a window); it validates workload + memory + reducer + emit.
fn run_headless() {
    eprintln!("{}", harness::banner());
    eprintln!("NICE_POC_RUN unset -> HEADLESS SELF-TEST (no window, no AppKit).");
    eprintln!("Set NICE_POC_RUN=1 on a machine WITH a display for the real measurement.\n");

    let prof = WorkloadProfile::default();
    let mut wl = Workload::new(prof);

    // 1) Workload generator: produce + checksum a deterministic stream.
    let stream = wl.stream(1_000_000);
    let sum: u64 = stream.iter().map(|&b| b as u64).sum();
    eprintln!(
        "workload: generated {} bytes (seed {}), checksum {}",
        stream.len(),
        prof.seed,
        sum
    );

    // Optionally dump the replay fixture for the baseline (Harness §E.3/§F).
    if let Ok(path) = std::env::var("NICE_POC_FIXTURE") {
        let mut wl2 = Workload::new(prof);
        match wl2.dump_fixture(Path::new(&path), 4_000_000) {
            Ok(()) => eprintln!("wrote fixture.bin -> {path}"),
            Err(e) => eprintln!("fixture write failed: {e}"),
        }
    }

    // 2) Memory sampler (real task_info on THIS process).
    let (phys, rss) = harness::mem::sample();
    eprintln!(
        "mem: phys_footprint={:.1} MiB, resident={:.1} MiB",
        harness::mem::mib(phys),
        harness::mem::mib(rss)
    );

    // 3) Reducer plumbing: drain (empty) streams + reduce.
    let streams = harness::drain_frame_streams();
    let term = harness::interval_stats(&streams.term_present, 16.6);
    let draw = harness::interval_stats(&streams.draw_attempt, 16.6);
    let gpui = harness::interval_stats(&streams.gpui_composite, 16.6);

    let results = Results {
        seed: prof.seed,
        bytes_per_sec: prof.bytes_per_sec,
        term,
        draw,
        gpui,
        latency_seam: (0.0, 0.0, 0.0),
        latency_pty: (0.0, 0.0, 0.0),
        mem_idle_phys_mib: harness::mem::mib(phys),
        mem_load_phys_mib: 0.0,
        mem_peak_phys_mib: 0.0,
        mem_idle_rss_mib: harness::mem::mib(rss),
        metal_active: false,
        proofs: proof_gates_unproven(),
    };

    println!("{}", results.markdown());
    eprintln!("\nheadless self-test OK.");
}

// ===========================================================================
// DISPLAY-GATED live run (only entered with NICE_POC_RUN=1).
//
// The live DRIVER is now fully wired (was a documented §C.4 TODO):
//   * promotes the process to a foreground app + activates so injected
//     NSApp.sendEvent traverses the REAL responder chain;
//   * SIGINT/SIGTERM flip an atomic stop-flag so Ctrl-C tears down cleanly;
//   * auto-exits after NICE_POC_SECS (default 25): drains the harness streams,
//     builds the SAME Results the headless path builds, prints the POPULATED
//     markdown table, and exit(0);
//   * the gpui render loop is the main-thread timer (request_animation_frame):
//     each tick pumps the workload, injects ONE keystroke (latency loop), and
//     samples under-load memory;
//   * at the deadline it runs the §5 mouse hit-test-seam probe and the §6
//     metal-rebind probe, then reports.
// ===========================================================================
mod gui {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
    use std::sync::Mutex;

    use gpui::{
        div, prelude::*, px, rgb, rgba, size, App, Application, Bounds, Context, Point,
        TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
        WindowOptions,
    };
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSApplication, NSView};
    use objc2_foundation::{MainThreadMarker, NSPoint};

    use crate::bridge::Terminal;
    use crate::embed::{self, NativeHandles};
    use crate::harness::{self, ProofGate, Results, Workload, WorkloadProfile};

    /// Height of the top chrome bar (points). Must match the bar drawn in
    /// `render` AND the chrome-exclusion the hit-test swizzle uses.
    const CHROME_H: f64 = 40.0;

    // ---- driver-wide state (read by every shutdown path: render deadline,
    // ---- SIGINT poll, window-close callback) ---------------------------------
    static STOP: AtomicBool = AtomicBool::new(false); // set by SIGINT/SIGTERM
    static FINISHED: AtomicBool = AtomicBool::new(false); // print-once guard
    static METAL_ACTIVE: AtomicBool = AtomicBool::new(false);
    /// Which present scheme the run actually used (true once the decoupled
    /// CADisplayLink loop is live; false = synchronous per-GPUI-frame present).
    /// Recorded into the report so the captured numbers are self-describing.
    static DECOUPLED: AtomicBool = AtomicBool::new(false);
    static SEED: AtomicU64 = AtomicU64::new(0);
    static BPS: AtomicU64 = AtomicU64::new(0);
    static WINDOW_NUMBER: AtomicI64 = AtomicI64::new(0);
    /// Display the window paced to, captured at embed (and re-checked at exit so
    /// a hot-plugged monitor mid-run is flagged, not silently averaged in).
    static DISPLAY_INFO: Mutex<String> = Mutex::new(String::new());
    static DISPLAY_FPS: AtomicI64 = AtomicI64::new(0);
    // memory (MiB stored as f64 bits)
    static IDLE_PHYS: AtomicU64 = AtomicU64::new(0);
    static IDLE_RSS: AtomicU64 = AtomicU64::new(0);
    static PEAK_PHYS: AtomicU64 = AtomicU64::new(0);
    static LOAD_SAMPLES: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    // proof outcomes: 0 = UNPROVEN, 1 = PASS, 2 = FAIL
    static RES_KEY: AtomicU8 = AtomicU8::new(0);
    static RES_MOUSE: AtomicU8 = AtomicU8::new(0);
    static RES_TEAROFF: AtomicU8 = AtomicU8::new(0);
    static RES_MONITOR: AtomicU8 = AtomicU8::new(0);
    /// Count of `send` callbacks from the terminal — fires ONLY when an injected
    /// keystroke actually reached the TerminalView (keyDown -> insertText ->
    /// delegate.send). This is the real proof-4 routing signal, vs the latency
    /// loop which closes on every present regardless of who handled the key.
    static KEY_ECHO: AtomicU64 = AtomicU64::new(0);

    /// SIGINT/SIGTERM handler — async-signal-safe (only flips an atomic). The
    /// Cocoa NSApplication.run loop swallows the default SIGINT, so without this
    /// Ctrl-C did nothing and the user had to `kill -9`.
    extern "C" fn handle_signal(_sig: i32) {
        STOP.store(true, Ordering::SeqCst);
    }

    unsafe fn install_signal_handlers() {
        let h = handle_signal as *const () as usize;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }

    fn store_f64(a: &AtomicU64, v: f64) {
        a.store(v.to_bits(), Ordering::SeqCst);
    }
    fn load_f64(a: &AtomicU64) -> f64 {
        f64::from_bits(a.load(Ordering::SeqCst))
    }

    fn median(v: &mut [f64]) -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    fn gate(item: u8, name: &'static str, code: u8, note: &'static str) -> ProofGate {
        ProofGate {
            item,
            name,
            pass: match code {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            note,
        }
    }

    /// Build the four live proof gates from the recorded outcome codes, choosing
    /// a static note per branch (so `Results`/`ProofGate` stay zero-alloc).
    fn live_proof_gates() -> Vec<ProofGate> {
        let key = RES_KEY.load(Ordering::SeqCst);
        let mouse = RES_MOUSE.load(Ordering::SeqCst);
        let tear = RES_TEAROFF.load(Ordering::SeqCst);
        let mon = RES_MONITOR.load(Ordering::SeqCst);
        vec![
            gate(
                4,
                "live keyboard responder + echo via NSApp.sendEvent",
                key,
                match key {
                    1 => "PASS: injected keyDown routed through key window -> terminal first responder -> loopback echo -> present closed the latency loop (see seam p50/p95/p99). IME marked-text still manual (needs a real input source).",
                    2 => "FAIL: keystrokes injected but no terminal present/echo closed the latency loop — responder chain not reached.",
                    _ => "UNPROVEN: run ended before any keystroke latency closed (e.g. window never became key, or interrupted before streaming).",
                },
            ),
            gate(
                5,
                "MOUSE hit-test seam — swizzled GPUIView.hitTest: routes terminal region to the terminal, chrome to gpui (THE load-bearing Path A vs objc2-hybrid test)",
                mouse,
                match mouse {
                    1 => "PASS (Path A viable): class_addMethod override on GPUIView.hitTest: returns the TerminalView for terminal-region points and gpui for the chrome bar; a synthetic NSApp.sendEvent drag produced a non-empty terminal selection. Transparent-over-terminal compositing already visually confirmed.",
                    2 => "FAIL (-> objc2-hybrid): the hit-test swizzle did not route terminal-region points to the terminal (or chrome leaked to it). Mouse/selection cannot reach the terminal below GPUIView; fall back to embed_above_in_rect.",
                    _ => "UNPROVEN: swizzle installed and routes correctly at the hit-test layer, but the synthetic drag produced no selection text to corroborate end-to-end delivery — drag manually to confirm (see stderr evidence).",
                },
            ),
            gate(
                6,
                "Metal-layer rebind (tear-off proof 6, same-window proxy)",
                tear,
                match tear {
                    1 => "PASS (rebind half): set_use_metal(false)->(true) tore down + rebuilt the CAMetalLayer and presents RESUMED. Full cross-window reparent (open 2nd gpui window + reparent_to) is wired in embed.rs but driven manually — see README.",
                    2 => "FAIL: after set_use_metal(false)->(true) the terminal present counter did not resume.",
                    _ => "UNPROVEN: real Metal renderer unavailable (stub bridge) or run ended first; cross-window reparent remains manual.",
                },
            ),
            gate(
                7,
                "process-wide swallow/passthrough NSEvent monitor + GPUI focus",
                mon,
                match mon {
                    1 => "PASS: addLocalMonitorForEventsMatchingMask installed alongside gpui focus (no competing swallow monitor); handler returns null for keyCode 53 (Escape) and passes everything else. End-to-end swallow of a windowserver Escape is manual (local monitors fire on queued events, not synthetic sendEvent).",
                    2 => "FAIL: the local event monitor could not be installed.",
                    _ => "UNPROVEN: monitor install not reached.",
                },
            ),
        ]
    }

    /// Drain the harness streams + driver globals into the SAME `Results` the
    /// headless path builds, print the (now POPULATED) markdown to stdout, and
    /// exit(0). Safe to call from any shutdown path; prints exactly once.
    fn finish_and_exit(reason: &str) -> ! {
        if FINISHED.swap(true, Ordering::SeqCst) {
            std::process::exit(0); // another path already reported
        }
        let streams = harness::drain_frame_streams();
        let term = harness::interval_stats(&streams.term_present, 16.6);
        let draw = harness::interval_stats(&streams.draw_attempt, 16.6);
        let gpui = harness::interval_stats(&streams.gpui_composite, 16.6);
        let mut lat = streams.latency_ms.clone();
        let latency_seam = harness::percentiles(&mut lat);

        let mut load = LOAD_SAMPLES.lock().unwrap().clone();
        let load_steady = median(&mut load);

        let results = Results {
            seed: SEED.load(Ordering::SeqCst),
            bytes_per_sec: BPS.load(Ordering::SeqCst) as usize,
            term,
            draw,
            gpui,
            latency_seam,
            latency_pty: (0.0, 0.0, 0.0), // loopback seam profile has no real pty
            mem_idle_phys_mib: load_f64(&IDLE_PHYS),
            mem_load_phys_mib: load_steady,
            mem_peak_phys_mib: load_f64(&PEAK_PHYS),
            mem_idle_rss_mib: load_f64(&IDLE_RSS),
            metal_active: METAL_ACTIVE.load(Ordering::SeqCst),
            proofs: live_proof_gates(),
        };

        let scheme = if DECOUPLED.load(Ordering::SeqCst) {
            "decoupled CADisplayLink present loop (terminal presents at refresh on its own run-loop source)"
        } else {
            "synchronous per-GPUI-frame present (naive: two vsync-locked presents per cycle)"
        };
        println!("<!-- present scheme: {scheme} -->");
        println!(
            "<!-- display: {} (max {} Hz) — present interval below pace to THIS display's vsync -->",
            DISPLAY_INFO.lock().unwrap(),
            DISPLAY_FPS.load(Ordering::SeqCst)
        );
        println!("{}", results.markdown());
        if let Ok(p) = std::env::var("NICE_POC_CSV") {
            match results.write_csv(Path::new(&p), &streams) {
                Ok(()) => eprintln!("[phase0-poc] wrote raw CSV -> {p}"),
                Err(e) => eprintln!("[phase0-poc] CSV write failed: {e}"),
            }
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
        eprintln!("\n[phase0-poc] LIVE run complete ({reason}); exiting cleanly.");
        std::process::exit(0);
    }

    /// Reverse-FFI `send` sink. The bridge calls this whenever the TerminalView
    /// wants to write to the pty — i.e. exactly when an injected keystroke was
    /// processed by the terminal. Counting it is the concrete proof-4 evidence
    /// that keystrokes ROUTE to the terminal (loopback also echoes them back).
    extern "C" fn on_send(_ud: *mut std::ffi::c_void, _ptr: *const u8, _len: usize) {
        KEY_ECHO.fetch_add(1, Ordering::Relaxed);
    }

    pub struct ChromeRoot {
        embedded: bool,
        handles: Option<NativeHandles>,
        terminal: Option<Terminal>,
        term_nsview: *mut NSView,
        workload: Workload,
        per_frame_budget: usize,
        frame: u64,
        metal_ok: bool,
        // present scheme (NICE_POC_PRESENT: link=default, sync, async, none)
        decouple_present: bool,    // link mode: drive terminal present off a CADisplayLink
        async_present: bool,       // async mode: coalesced DispatchQueue.main.async present
        present_terminal: bool,    // false only in `none` mode (GPUI-compositor-alone baseline)
        present_link_active: bool, // the CADisplayLink loop actually started
        // driver
        deadline_secs: f64,
        start_tick: u64, // mach ticks at first streaming frame; 0 until embedded
        last_mem_frame: u64,
        key_marked: bool,
        seam_done: bool,
        // keep the swallow-monitor token alive for the whole run
        swallow_monitor: Option<Retained<AnyObject>>,
    }

    impl ChromeRoot {
        fn new(
            deadline_secs: f64,
            decouple_present: bool,
            async_present: bool,
            present_terminal: bool,
        ) -> Self {
            let prof = WorkloadProfile::default();
            ChromeRoot {
                embedded: false,
                handles: None,
                terminal: None,
                term_nsview: std::ptr::null_mut(),
                workload: Workload::new(prof),
                per_frame_budget: prof.bytes_per_sec / 120, // assume ProMotion 120 Hz
                frame: 0,
                metal_ok: false,
                decouple_present,
                async_present,
                present_terminal,
                present_link_active: false,
                deadline_secs,
                start_tick: 0,
                last_mem_frame: 0,
                key_marked: false,
                seam_done: false,
                swallow_monitor: None,
            }
        }

        fn elapsed_secs(&self) -> f64 {
            if self.start_tick == 0 {
                return 0.0;
            }
            harness::clock::ms_between(self.start_tick, harness::clock::now()) / 1000.0
        }

        /// One-time embed once the window is realized (RECON 2 §1b/§2). On the
        /// embed frame it also samples the IDLE dual-stack memory baseline, resets
        /// the under-load streams, and starts the measurement clock.
        fn try_embed(&mut self, window: &mut Window) {
            if self.embedded {
                return;
            }
            let Some(handles) = embed::native_handles(window) else {
                return; // window not on screen yet; retry next frame
            };

            // Real Metal terminal (or stub) created on the main thread.
            let term = Terminal::new(0.0, 0.0, 900.0, 560.0);
            self.metal_ok = term.set_use_metal(true); // 0 with the stub
            term.set_loopback(true); // seam-latency profile (§C.3)
            term.register_callbacks(
                std::ptr::null_mut(),
                Some(on_send),
                None,
                None,
                None,
                None,
                None,
            );
            // Harness frame hooks (present + draw-attempt) live in the bridge.
            crate::bridge::install_frame_hooks(harness::on_draw_attempt, harness::on_present);

            let nsview = term.nsview_ptr();
            self.term_nsview = nsview as *mut NSView;
            unsafe {
                embed::embed_below_chrome(&handles, nsview);
                embed::make_terminal_first_responder(&handles, nsview);
                // Make the gpui window the genuine key+front window so injected
                // NSApp.sendEvent traverses the real responder chain.
                crate::input::make_key_front(&handles.window);
            }
            WINDOW_NUMBER.store(handles.window.windowNumber() as i64, Ordering::SeqCst);

            // PoC item 7: process-wide swallow monitor coexisting with gpui.
            // keyCode 53 = Escape; swallow it to prove the rebindable-shortcut path.
            let token = unsafe { crate::input::install_swallow_monitor(53) };
            RES_MONITOR.store(if token.is_some() { 1 } else { 2 }, Ordering::SeqCst);
            self.swallow_monitor = token;

            METAL_ACTIVE.store(self.metal_ok, Ordering::SeqCst);

            // Record which display (+ refresh rate) the window paced to, so the
            // numbers self-document the monitor and a hot-plug is detectable.
            let (dfps, dname) = embed::screen_info(&handles);
            DISPLAY_FPS.store(dfps, Ordering::SeqCst);
            *DISPLAY_INFO.lock().unwrap() = dname.clone();
            eprintln!("[phase0-poc] window on display: {dname} (max {dfps} Hz)");

            // Decoupled present loop: drive the terminal's Metal present off its
            // own CADisplayLink (a vsync-paced run-loop source) instead of
            // calling present_now() synchronously inside each GPUI render frame.
            // The naive scheme serialized two vsync-locked presents per cycle
            // (~half refresh); this lets the terminal repaint at refresh
            // independently of GPUI's compositor. Falls back to the synchronous
            // present if NICE_POC_PRESENT=sync or the link can't start (stub).
            self.present_link_active = self.present_terminal
                && self.decouple_present
                && self.metal_ok
                && term.start_present_link();
            DECOUPLED.store(self.present_link_active, Ordering::SeqCst);

            // IDLE dual-stack baseline BEFORE any byte is streamed, then clear the
            // warm-up frames so the report's UNDER-LOAD percentiles are clean.
            let (phys, rss) = harness::mem::sample();
            store_f64(&IDLE_PHYS, harness::mem::mib(phys));
            store_f64(&IDLE_RSS, harness::mem::mib(rss));
            store_f64(&PEAK_PHYS, harness::mem::mib(phys));
            harness::reset_frame_streams();
            self.start_tick = harness::clock::now();

            self.handles = Some(handles);
            self.terminal = Some(term);
            self.embedded = true;
            eprintln!(
                "[phase0-poc] embedded terminal (metal={}, present={}); streaming + measuring \
                 for {:.0}s. Mouse hit-test seam probed at the deadline.",
                self.metal_ok,
                if self.present_link_active { "decoupled CADisplayLink" } else { "synchronous per-frame" },
                self.deadline_secs
            );
        }

        /// Inject ONE keystroke per tick via NSApp.sendEvent through the key
        /// window, timestamped (arm) so the following present closes the seam
        /// latency loop (proof 4 / Harness §C.1–C.2).
        fn inject_keystroke(&mut self) {
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            let wn = WINDOW_NUMBER.load(Ordering::SeqCst) as isize;
            harness::arm_keystroke();
            if let Some(ev) = crate::input::key_down(wn, "a", 0) {
                crate::input::send_event(&app, &ev);
            }
            if let Some(ev) = crate::input::key_up(wn, "a", 0) {
                crate::input::send_event(&app, &ev);
            }
        }

        /// Stream one frame's byte budget into the terminal and force a present
        /// (the burst-FPS driver, Harness §B/§E). The present also closes the
        /// armed keystroke's latency loop.
        fn pump_workload(&mut self) {
            let Some(term) = &self.terminal else { return };
            let mut fed = 0usize;
            while fed < self.per_frame_budget {
                let chunk = self.workload.next_chunk();
                term.feed_bytes(&chunk);
                fed += chunk.len();
            }
            // With the decoupled present loop the CADisplayLink drives the
            // terminal present at refresh (and closes the keystroke-latency
            // loop); feed only here. In sync mode present synchronously per
            // frame (the naive scheme); in async mode queue a coalesced
            // DispatchQueue.main.async present (the fork's production path). In
            // `none` mode never present — isolates GPUI's standalone rate.
            if self.present_terminal && !self.present_link_active {
                if self.async_present {
                    term.present_async();
                } else {
                    term.present_now();
                }
            }

            // Periodic reflow stressor (§E.2): every ~3 s @120 Hz nudge cols.
            if self.frame % 360 == 0 {
                let cols = 80 + ((self.frame / 360) % 2) as i32 * 10;
                term.resize(cols, 24);
            }
        }

        /// Track under-load steady + peak phys_footprint (~every 15 frames).
        fn sample_load_mem(&mut self) {
            if self.frame.wrapping_sub(self.last_mem_frame) < 15 {
                return;
            }
            self.last_mem_frame = self.frame;
            let (phys, _rss) = harness::mem::sample();
            let mib = harness::mem::mib(phys);
            LOAD_SAMPLES.lock().unwrap().push(mib);
            if mib > load_f64(&PEAK_PHYS) {
                store_f64(&PEAK_PHYS, mib);
            }
        }

        /// THE load-bearing §5 test. Install the GPUIView.hitTest: swizzle, prove
        /// deterministically that terminal-region points resolve to the terminal
        /// and chrome points to gpui, then synthesize a real mouse drag and check
        /// the terminal formed a selection.
        fn run_mouse_seam(&mut self) {
            let Some(h) = &self.handles else {
                RES_MOUSE.store(0, Ordering::SeqCst);
                return;
            };
            let term = self.term_nsview;
            if term.is_null() {
                RES_MOUSE.store(0, Ordering::SeqCst);
                return;
            }
            let installed = unsafe { crate::input::install_hittest_shim(&h.gpui_view, term, CHROME_H) };
            if !installed {
                eprintln!("[§5 mouse] hitTest swizzle FAILED to install -> objc2-hybrid fallback");
                RES_MOUSE.store(2, Ordering::SeqCst);
                return;
            }

            // Deterministic routing check (contentView coords, no windowserver).
            let b = h.content.bounds();
            let (w, ht) = (b.size.width, b.size.height);
            let term_pt = NSPoint::new(w * 0.5, (ht - CHROME_H) * 0.5);
            let chrome_pt = NSPoint::new(w * 0.5, ht - CHROME_H * 0.5);
            let hit_term = unsafe { crate::input::hittest_resolves(&h.gpui_view, term_pt) };
            let hit_chrome = unsafe { crate::input::hittest_resolves(&h.gpui_view, chrome_pt) };
            let gpui_ptr = Retained::as_ptr(&h.gpui_view) as *mut NSView;
            let routes_term = hit_term == term;
            let chrome_stays = hit_chrome != term; // chrome must NOT go to the terminal
            let routing_ok = routes_term && chrome_stays;

            // Real synthetic drag through the responder chain over the terminal.
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            let wn = WINDOW_NUMBER.load(Ordering::SeqCst) as isize;
            let from = NSPoint::new(w * 0.25, (ht - CHROME_H) * 0.5);
            let to = NSPoint::new(w * 0.60, (ht - CHROME_H) * 0.5);
            unsafe { crate::input::synth_drag_select(&app, wn, from, to, 6) };
            let sel = self
                .terminal
                .as_ref()
                .and_then(|t| t.selection())
                .filter(|s| !s.trim().is_empty());
            let (hits_term, hits_chrome) = crate::input::hittest_counts();

            eprintln!(
                "[§5 mouse] swizzle installed; routing: term_pt->terminal={routes_term}, \
                 chrome_pt->gpui(not terminal)={chrome_stays}; hit-test counts term={hits_term} \
                 chrome={hits_chrome}; gpui_ptr={gpui_ptr:?} hit_chrome={hit_chrome:?}; selection={sel:?}"
            );

            let code = if !routing_ok {
                2 // FAIL -> objc2-hybrid
            } else if sel.is_some() {
                1 // PASS -> Path A
            } else {
                0 // routing proven but selection not corroborated synthetically
            };
            RES_MOUSE.store(code, Ordering::SeqCst);
        }

        /// §6 proxy: exercise the Metal-layer rebind (the load-bearing half of
        /// tear-off) on the SAME window — set_use_metal(false)->(true) and assert
        /// the terminal present counter resumes. Full cross-window reparent stays
        /// manual (embed::reparent_to is wired but needs a 2nd realized window).
        fn run_tearoff_proxy(&mut self) {
            let Some(term) = &self.terminal else {
                RES_TEAROFF.store(0, Ordering::SeqCst);
                return;
            };
            if !self.metal_ok {
                eprintln!("[§6 tear-off] stub bridge (no real Metal) -> rebind UNPROVEN");
                RES_TEAROFF.store(0, Ordering::SeqCst);
                return;
            }
            let before = harness::drain_frame_streams().term_present.len();
            let off = term.set_use_metal(false); // tears down the CAMetalLayer/MTKView
            let _ = term.present_now(); // no MTKView -> no present while off
            let on = term.set_use_metal(true); // rebinds CAMetalLayer / drawableSize
            for _ in 0..5 {
                term.present_now();
            }
            let after = harness::drain_frame_streams().term_present.len();
            let resumed = after > before;
            eprintln!(
                "[§6 tear-off] same-window metal rebind off={off} on={on}; \
                 term presents before={before} after={after} resumed={resumed}"
            );
            RES_TEAROFF.store(if on && resumed { 1 } else { 2 }, Ordering::SeqCst);
        }

        /// Run the end-of-window proofs (mouse seam + metal rebind) once, then
        /// build the report and exit. Used by both the deadline and Ctrl-C paths.
        fn finalize_and_exit(&mut self, reason: &str) -> ! {
            if self.embedded && !self.seam_done {
                self.seam_done = true;
                // Detect a hot-plugged / changed display mid-run — it silently
                // re-paces the present loop to a different vsync and would
                // corrupt the numbers. Flag loudly rather than average it in.
                if let Some(h) = &self.handles {
                    let (now_fps, now_name) = embed::screen_info(h);
                    let embed_name = DISPLAY_INFO.lock().unwrap().clone();
                    if now_name != embed_name || now_fps != DISPLAY_FPS.load(Ordering::SeqCst) {
                        eprintln!(
                            "[phase0-poc] ⚠️ DISPLAY CHANGED MID-RUN: embed='{embed_name}' \
                             ({} Hz) -> exit='{now_name}' ({now_fps} Hz). Numbers are \
                             CONTAMINATED — re-run on a single stable display.",
                            DISPLAY_FPS.load(Ordering::SeqCst)
                        );
                        *DISPLAY_INFO.lock().unwrap() =
                            format!("{embed_name} -> {now_name} (CHANGED MID-RUN — CONTAMINATED)");
                    }
                }
                // Stop the decoupled present loop first so the end-of-window
                // probes (mouse seam + §6 metal rebind) drive present_now()
                // deterministically — no CADisplayLink presents racing the
                // before/after counters or the metal-off teardown window.
                if self.present_link_active {
                    if let Some(t) = &self.terminal {
                        t.stop_present_link();
                    }
                    self.present_link_active = false;
                }
                // Proof 4 verdict: keystrokes were injected every tick; if none
                // ever echoed back through the terminal, routing is broken.
                let echoes = KEY_ECHO.load(Ordering::Relaxed);
                if echoes == 0 && self.start_tick != 0 {
                    RES_KEY.store(2, Ordering::SeqCst);
                }
                eprintln!(
                    "[§4 keyboard] injected keystrokes echoed by terminal: {echoes} (latency samples: {})",
                    harness::latency_len()
                );
                self.run_mouse_seam();
                self.run_tearoff_proxy();
            }
            finish_and_exit(reason)
        }
    }

    impl Render for ChromeRoot {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            harness::stamp_gpui_frame(); // count this GPUI composite (item over terminal)
            self.frame += 1;

            self.try_embed(window);

            if self.embedded {
                // Drive the measurement: one keystroke + one workload burst + a
                // memory sample per main-thread tick.
                self.inject_keystroke();
                self.pump_workload();
                self.sample_load_mem();

                // Mark the keyboard proof PASS as soon as an injected keystroke
                // actually reaches the terminal (a `send`/echo fired) AND the
                // latency loop has a sample to report.
                if !self.key_marked
                    && KEY_ECHO.load(Ordering::Relaxed) > 0
                    && harness::latency_len() > 0
                {
                    RES_KEY.store(1, Ordering::SeqCst);
                    self.key_marked = true;
                }
            }

            // Clean shutdown: Ctrl-C/SIGTERM (stop-flag) or the elapsed deadline.
            if STOP.load(Ordering::SeqCst) {
                self.finalize_and_exit("interrupt (Ctrl-C / SIGTERM)");
            }
            if self.embedded && self.elapsed_secs() >= self.deadline_secs {
                self.finalize_and_exit("measurement window elapsed");
            }

            // Keep compositing continuously so the GPUI (blade/metal) stack does
            // REAL work over the live terminal — the dual-stack story. This is the
            // main-thread timer that drives the whole measurement.
            window.request_animation_frame();

            // Animated "streaming pill" + a frosted chrome strip. The ROOT is
            // left transparent so the terminal sibling shows through (item 5).
            let pill_x = 24.0 + ((self.frame % 240) as f64) * 1.5;
            div()
                .size_full()
                .child(
                    // Top chrome bar — translucent, tints the terminal beneath.
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(CHROME_H as f32))
                        .bg(rgba(0x10141CCC))
                        .border_b_1()
                        .border_color(rgba(0xFFFFFF22)),
                )
                .child(
                    // The continuously-moving transparent overlay element OVER
                    // the terminal — exercises GPUI compositing every frame.
                    div()
                        .absolute()
                        .top(px(120.0))
                        .left(px(pill_x as f32))
                        .w(px(160.0))
                        .h(px(28.0))
                        .rounded(px(14.0))
                        .bg(rgba(0x6E59F5AA))
                        .text_color(rgb(0xFFFFFF))
                        .text_sm()
                        .child("streaming…"),
                )
        }
    }

    pub fn run_live() {
        // The subagent must NOT reach here (no display). Present for the user.
        let _mtm = MainThreadMarker::new().expect("must run on main thread");

        // SIGINT/SIGTERM -> atomic stop-flag so Ctrl-C works (the Cocoa run loop
        // otherwise swallows the default SIGINT).
        unsafe { install_signal_handlers() };

        let prof = WorkloadProfile::default();
        SEED.store(prof.seed, Ordering::SeqCst);
        BPS.store(prof.bytes_per_sec as u64, Ordering::SeqCst);

        let deadline = std::env::var("NICE_POC_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(25.0);

        // Present scheme (NICE_POC_PRESENT): default `link` = decoupled CADisplayLink
        // loop; `sync` = naive per-GPUI-frame present (the A/B baseline); `async` =
        // coalesced DispatchQueue.main.async present (the fork's production path,
        // paired with GPUI's RAF); `none` = never present the terminal, isolating
        // GPUI's standalone compositor rate.
        let present_env = std::env::var("NICE_POC_PRESENT").unwrap_or_default();
        // (decouple_present, async_present, present_terminal, label)
        let (decouple_present, async_present, present_terminal, scheme_label) =
            match present_env.as_str() {
                "sync" | "synchronous" | "0" => (false, false, true, "synchronous-per-frame"),
                "async" => (false, true, true, "async-coalesced (production path)"),
                "none" | "off" => (false, false, false, "none (GPUI compositor alone)"),
                _ => (true, false, true, "decoupled-CADisplayLink"),
            };
        eprintln!(
            "[phase0-poc] LIVE run: streaming ~{deadline:.0}s then auto-exit with a populated \
             Results table. present={scheme_label} (NICE_POC_PRESENT=link|sync|async|none). \
             Ctrl-C exits early + reports. (NICE_POC_SECS overrides the window.)"
        );

        Application::new().run(move |cx: &mut App| {
            // Promote to a regular foreground app + bring to front so injected
            // NSApp.sendEvent events traverse the real responder chain (§C/§G4).
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            unsafe { crate::input::activate_front(&app) };
            cx.activate(true);

            // Window-close / Cmd-Q -> emit the report and exit cleanly.
            cx.on_window_closed(|_cx| finish_and_exit("window closed / Cmd-Q"))
                .detach();

            let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // Transparent = cleanest over-terminal test (no blur layer).
                    window_background: WindowBackgroundAppearance::Transparent,
                    titlebar: Some(TitlebarOptions {
                        title: Some("Nice Phase-0 PoC".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(Point {
                            x: px(14.0),
                            y: px(14.0),
                        }),
                    }),
                    kind: WindowKind::Normal,
                    is_resizable: true,
                    ..Default::default()
                },
                |_window, cx| {
                    cx.new(|_cx| {
                        ChromeRoot::new(deadline, decouple_present, async_present, present_terminal)
                    })
                },
            )
            .unwrap();
        });
    }
}
