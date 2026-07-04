//! Self-test driver + suite runner — the standing UI regression gate.
//!
//! Contract (see the plan's Exported contracts):
//!   * `NICE_RS_SELFTEST=<scenario>` runs one named scenario, prints exactly
//!     `SELFTEST PASS <scenario>` and exits 0 on success, or `SELFTEST FAIL
//!     <scenario>` + nonzero on failure.
//!   * `NICE_RS_SELFTEST=all` runs every registered scenario sequentially,
//!     prints a PASS/FAIL table, and exits nonzero if any scenario fails.
//!   * `NICE_RS_CAPTURE=<path>` additionally writes a PNG of each scenario's
//!     window (requires the app's `selftest` feature — see [`crate::capture`]).
//!
//! Scenarios are supplied by the app crate (which owns the concrete gpui views)
//! and passed to [`drive`] as a `Vec<Scenario>`. Later cycles extend the gate by
//! adding scenarios to that vec — the reducer, watchdog and table printing here
//! stay put. Before it measures, the driver runs a per-scenario activation
//! preamble ([`activate_window`]) that drives the scenario's window frontmost +
//! key and asserts it: a run on an occupied screen FAILs actionably instead of
//! measuring an inactive, frame-capped window (see [`Scenario::activate`]).
//!
//! The whole run lives inside a SINGLE `Application::run`: [`drive`] is called
//! from the app's run closure, foregrounds the app, arms the watchdog, and
//! spawns one async orchestrator that drives each scenario's window in turn.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp};

use crate::frame::CadenceReport;
use crate::{frame, watchdog};

/// A registered self-test scenario.
pub struct Scenario {
    /// Unique selector name (`NICE_RS_SELFTEST=<name>`).
    pub name: &'static str,
    /// Open the scenario's window. The returned view must call
    /// [`crate::frame::stamp`] once per render and drive continuous repaint
    /// (`window.request_animation_frame()`), so the driver can measure cadence
    /// on a frontmost, focused window. Returns the handle so the driver can
    /// capture + close it.
    pub open: fn(&mut AsyncApp) -> anyhow::Result<AnyWindowHandle>,
    /// How the driver measures + gates this scenario (see [`Gate`]). Most
    /// scenarios use [`Gate::Cadence`]; a scenario with an absolute frame-time or
    /// memory budget uses [`Gate::SelfReported`].
    pub gate: Gate,
    /// Whether the driver runs the activation preamble — foreground the app and
    /// drive this scenario's window to frontmost + key, *asserted* — before it
    /// measures. This is the driver's self-activation guarantee: every windowed
    /// scenario measures on the active window, and a run on an occupied screen
    /// FAILs with an actionable message ("window could not become frontmost — is
    /// another app fullscreen?") instead of silently measuring an inactive,
    /// frame-capped window and reporting a mystifying "0 frames" (zed-main
    /// frame-caps inactive windows at ~33 ms). `true` for every windowed scenario;
    /// set `false` only for a deliberately-background scenario (none exist today).
    pub activate: bool,
}

/// How the driver measures and gates a scenario.
#[derive(Clone, Copy)]
pub enum Gate {
    /// Standard: warm up, measure `measure_secs`, assert cadence-jitter sanity
    /// (`p95 < ratio × p50`, at least [`MIN_FRAMES`] frames). The default for a
    /// scenario whose pass criterion is "the window paints at a sane, steady
    /// cadence."
    Cadence,
    /// The scenario runs its OWN measurement + gate inside its `open` task
    /// (self-activating, multi-run, absolute frame-time thresholds, memory) and
    /// reports the verdict via [`report_gate`]. The driver keeps the window open
    /// until that verdict arrives (bounded by `budget`) and imposes no cadence
    /// gate of its own.
    ///
    /// `term-perf` uses this: its absolute p50/p95 + memory gate is a criterion
    /// the jitter check cannot express — a 31 ms tail atop a 16 ms median passes
    /// the jitter ratio yet is exactly the Path-A regression this gate exists to
    /// catch.
    SelfReported {
        /// Upper bound on how long the scenario's task may take to report. The
        /// process watchdog is the hard backstop above this.
        budget: Duration,
    },
}

/// A [`Gate::SelfReported`] scenario posts its own gate verdict here; the driver
/// awaits it instead of running the standard cadence measurement. Process-global
/// (one scenario runs at a time, exactly like the `frame` module's global stamp
/// stream).
static GATE_REPORT: Mutex<Option<CadenceReport>> = Mutex::new(None);

/// Post a self-reported scenario's gate verdict for the driver to collect. The
/// scenario's `open` task calls this once, after its own measurement + gate.
pub fn report_gate(report: CadenceReport) {
    *GATE_REPORT.lock().unwrap() = Some(report);
}

/// Take (and clear) a pending self-reported verdict, if one has arrived.
fn take_gate_report() -> Option<CadenceReport> {
    GATE_REPORT.lock().unwrap().take()
}

/// Warm-up window discarded before each measurement (startup frames are
/// irregular; see `frame` module docs).
const WARMUP_SECS: f64 = 0.5;
/// Default per-scenario measurement window (override with
/// `NICE_RS_SELFTEST_SECS`).
const DEFAULT_MEASURE_SECS: f64 = 2.5;
/// Minimum frames a scenario must sustain in the measurement window, else it
/// never really animated.
const MIN_FRAMES: usize = 30;
/// Cadence gate: p95 interval must be `< CADENCE_RATIO ×` the median interval.
const CADENCE_RATIO: f64 = 2.0;
/// How long the activation preamble waits for a scenario's window to become
/// frontmost + key before giving up with an actionable failure. On a free screen
/// activation is near-instant (the preamble returns as soon as the window reports
/// active); this bound only bites on the failure path — another app is fullscreen
/// or owns the display Space, exactly the case that used to silently yield
/// "0 frames."
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

fn env_secs(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// Drive the self-test run. Call from inside the app's `Application::run`
/// closure with the app-provided scenario registry. Never returns normally —
/// the async orchestrator (or the watchdog) exits the process.
pub fn drive(cx: &mut App, selector: &str, scenarios: Vec<Scenario>) {
    let is_suite = selector == "all";
    let selected: Vec<Scenario> = if is_suite {
        scenarios
    } else {
        match scenarios.into_iter().find(|s| s.name == selector) {
            Some(s) => vec![s],
            None => {
                eprintln!("SELFTEST FAIL {selector}: unknown scenario");
                println!("SELFTEST FAIL {selector}");
                std::process::exit(2);
            }
        }
    };

    let measure_secs = env_secs("NICE_RS_SELFTEST_SECS", DEFAULT_MEASURE_SECS);
    let capture_path = std::env::var("NICE_RS_CAPTURE")
        .ok()
        .filter(|s| !s.is_empty());

    // Foreground the app once up front. Per-scenario, the driver additionally
    // runs the activation preamble ([`activate_window`]) that drives each
    // window frontmost + key AND asserts it before measuring — that assertion,
    // not this best-effort kick, is the self-activation guarantee. (zed-main
    // frame-caps inactive windows at ~33 ms, so measurement must run on the
    // active window — see the `frame` module docs.)
    cx.activate(true);

    // Hard backstop: a real-OS-thread deadline across ALL scenarios + grace.
    // App Nap can defer the async orchestrator's timers on an occluded/idle
    // run; the watchdog cannot starve, so the self-test can never hang. On fire
    // it prints the FAIL marker and hard-exits. The budget sums each scenario's
    // own worst-case duration — a `SelfReported` scenario (term-perf) can run for
    // its whole reporting budget, far longer than a cadence scenario's fixed
    // warm-up + measure window, so a flat `n × measure` cap would fire mid-run.
    let mut budget_secs = 5.0; // grace
    for sc in &selected {
        // The activation preamble runs before measurement for every windowed
        // scenario; its worst case (a legitimately slow-but-eventual activation)
        // stacks on top of the gate's own budget, so count it here or the
        // watchdog could false-fire on a slow-to-foreground run.
        if sc.activate {
            budget_secs += ACTIVATE_TIMEOUT.as_secs_f64();
        }
        budget_secs += match sc.gate {
            Gate::Cadence => WARMUP_SECS + measure_secs + 4.0,
            Gate::SelfReported { budget } => budget.as_secs_f64() + 4.0,
        };
    }
    let budget = Duration::from_secs_f64(budget_secs);
    let selector_for_watchdog = selector.to_string();
    watchdog::arm(
        budget,
        "nice-rs selftest",
        move || {
            eprintln!(
                "SELFTEST FAIL {selector_for_watchdog}: watchdog fired — the run wedged \
                 before completing (main thread starved / window never presented)."
            );
            println!("SELFTEST FAIL {selector_for_watchdog}");
            let _ = std::io::stdout().flush();
            std::process::exit(3);
        },
    );

    let selector_owned = selector.to_string();
    cx.spawn(async move |acx: &mut AsyncApp| {
        run_scenarios(
            acx,
            selected,
            is_suite,
            selector_owned,
            measure_secs,
            capture_path,
        )
        .await;
    })
    .detach();
}

/// Sequentially open, drive, measure, (optionally) capture, and close each
/// scenario's window, then print results and exit.
async fn run_scenarios(
    cx: &mut AsyncApp,
    scenarios: Vec<Scenario>,
    is_suite: bool,
    selector: String,
    measure_secs: f64,
    capture_path: Option<String>,
) {
    let warmup = Duration::from_secs_f64(WARMUP_SECS);
    let measure = Duration::from_secs_f64(measure_secs);
    let mut results: Vec<(&'static str, frame::CadenceReport)> = Vec::new();

    for sc in &scenarios {
        eprintln!("[selftest] scenario '{}': opening window", sc.name);
        frame::reset();
        // Drop any stale verdict a previous scenario's task posted after its
        // budget elapsed, so a `SelfReported` scenario never reads the wrong one.
        let _ = take_gate_report();

        let handle = match (sc.open)(cx) {
            Ok(h) => h,
            Err(e) => {
                let report = frame::CadenceReport::error(format!("window open failed: {e}"));
                eprintln!("[selftest] scenario '{}': {}", sc.name, report.detail);
                results.push((sc.name, report));
                continue;
            }
        };

        // Self-activation guarantee: drive the window frontmost + key (asserted)
        // before measuring, so a run on an occupied screen FAILs actionably here
        // instead of measuring an inactive, frame-capped window ("0 frames").
        if sc.activate {
            if let Err(msg) = activate_window(cx, handle).await {
                let report = frame::CadenceReport::error(msg);
                eprintln!("[selftest] scenario '{}': {}", sc.name, report.detail);
                let _ = handle.update(cx, |_view, window, _app| window.remove_window());
                results.push((sc.name, report));
                continue;
            }
        }

        let mut report = match sc.gate {
            Gate::Cadence => {
                // Warm up (discard startup frames), then measure a clean window.
                let t = cx.background_executor().timer(warmup);
                t.await;
                frame::reset();
                let t = cx.background_executor().timer(measure);
                t.await;
                frame::assess_cadence(MIN_FRAMES, CADENCE_RATIO)
            }
            // The scenario's own task self-activates, measures, and posts its
            // verdict; the window stays open (painting) while we wait for it.
            Gate::SelfReported { budget } => await_self_reported(cx, sc.name, budget).await,
        };

        if let Some(path) = &capture_path {
            match crate::capture::capture_window_png(handle, cx, Path::new(path)) {
                Ok(()) => {
                    eprintln!("[selftest] scenario '{}': wrote capture -> {path}", sc.name)
                }
                Err(e) => {
                    eprintln!("[selftest] scenario '{}': capture FAILED: {e}", sc.name);
                    report.passed = false;
                    report.detail = format!("{} | capture failed: {e}", report.detail);
                }
            }
        }

        // Close before the next scenario so windows don't stack.
        let _ = handle.update(cx, |_view, window, _app| window.remove_window());

        eprintln!(
            "[selftest] scenario '{}': {} — {}",
            sc.name,
            if report.passed { "PASS" } else { "FAIL" },
            report.detail
        );
        results.push((sc.name, report));
    }

    finish(results, is_suite, &selector);
}

/// Foreground the app and drive `handle`'s window to frontmost + key, asserting
/// it actually became active. Re-issues `activate` each tick — a single activate
/// can lose the race to the initial paint or a Space switch, and the platform
/// coalesces repeats — and polls [`gpui::Window::is_window_active`]. Returns
/// `Ok(())` as soon as the window reports active, or an actionable `Err` if it
/// never became frontmost within [`ACTIVATE_TIMEOUT`].
///
/// This is the driver's self-activation guarantee (see [`Scenario::activate`]):
/// a run on an occupied screen FAILs with a clear remediation instead of
/// silently measuring an inactive, frame-capped window and reporting "0 frames."
async fn activate_window(cx: &mut AsyncApp, handle: AnyWindowHandle) -> Result<(), String> {
    const POLL: Duration = Duration::from_millis(100);
    let ticks = (ACTIVATE_TIMEOUT.as_secs_f64() / POLL.as_secs_f64()).ceil() as u64;
    let is_active = |cx: &mut AsyncApp| {
        handle
            .update(cx, |_view, window, _app| window.is_window_active())
            .unwrap_or(false)
    };
    for _ in 0..ticks.max(1) {
        // Best-effort foreground each tick (idempotent); then check whether the
        // platform has actually made the window key/active yet.
        let _ = cx.update(|app| app.activate(true));
        if is_active(cx) {
            return Ok(());
        }
        cx.background_executor().timer(POLL).await;
    }
    // A last check after the final sleep, then give up with remediation.
    if is_active(cx) {
        return Ok(());
    }
    Err(format!(
        "window could not become frontmost within {:.0}s — is another app \
         fullscreen or occupying the display Space? Free the screen (no app \
         owning the display Space) and re-run.",
        ACTIVATE_TIMEOUT.as_secs_f64()
    ))
}

/// Poll for a [`Gate::SelfReported`] scenario's verdict, up to `budget`. The
/// scenario's task self-activates, measures, and posts via [`report_gate`]; this
/// only waits (the window stays open and painting meanwhile). Falls back to an
/// error report if the task never reports — the process watchdog is the harder
/// backstop above this.
async fn await_self_reported(cx: &mut AsyncApp, name: &str, budget: Duration) -> CadenceReport {
    const POLL: Duration = Duration::from_millis(150);
    let ticks = (budget.as_secs_f64() / POLL.as_secs_f64()).ceil() as u64;
    for _ in 0..ticks.max(1) {
        if let Some(r) = take_gate_report() {
            return r;
        }
        cx.background_executor().timer(POLL).await;
    }
    // A last check after the final sleep, then give up into an error report.
    take_gate_report().unwrap_or_else(|| {
        CadenceReport::error(format!(
            "self-reported scenario '{name}' did not post a verdict within {:.0}s",
            budget.as_secs_f64()
        ))
    })
}

fn finish(results: Vec<(&'static str, frame::CadenceReport)>, is_suite: bool, selector: &str) -> ! {
    let all_pass = results.iter().all(|(_, r)| r.passed);

    if is_suite {
        println!();
        println!("NICE RS self-test suite ({} scenario(s))", results.len());
        println!("  {:<12} {:<6} detail", "scenario", "result");
        println!("  {}", "-".repeat(60));
        for (name, r) in &results {
            println!(
                "  {:<12} {:<6} {}",
                name,
                if r.passed { "PASS" } else { "FAIL" },
                r.detail
            );
        }
        println!();
        println!(
            "SELFTEST {} {selector}",
            if all_pass { "PASS" } else { "FAIL" }
        );
    } else if let Some((name, r)) = results.first() {
        if r.passed {
            println!("SELFTEST PASS {name}");
        } else {
            eprintln!("SELFTEST FAIL {name}: {}", r.detail);
            println!("SELFTEST FAIL {name}");
        }
    } else {
        // Unreachable in practice (single-scenario runs always have one result).
        eprintln!("SELFTEST FAIL {selector}: no scenario ran");
        println!("SELFTEST FAIL {selector}");
    }

    let _ = std::io::stdout().flush();
    std::process::exit(if all_pass { 0 } else { 1 });
}

#[cfg(test)]
mod tests {
    use super::env_secs;

    // Each test uses a distinct key so parallel test threads never race on the
    // shared process environment.

    #[test]
    fn env_secs_unset_uses_default() {
        let key = "NICE_RS_TEST_ENV_SECS_UNSET";
        std::env::remove_var(key);
        assert_eq!(env_secs(key, 2.5), 2.5);
    }

    #[test]
    fn env_secs_valid_positive_is_parsed() {
        let key = "NICE_RS_TEST_ENV_SECS_VALID";
        std::env::set_var(key, "3.5");
        assert_eq!(env_secs(key, 2.5), 3.5);
        std::env::remove_var(key);
    }

    #[test]
    fn env_secs_unparseable_falls_back_to_default() {
        let key = "NICE_RS_TEST_ENV_SECS_JUNK";
        std::env::set_var(key, "not-a-number");
        assert_eq!(env_secs(key, 2.5), 2.5);
        std::env::remove_var(key);
    }

    #[test]
    fn env_secs_zero_falls_back_to_default() {
        let key = "NICE_RS_TEST_ENV_SECS_ZERO";
        std::env::set_var(key, "0");
        assert_eq!(env_secs(key, 2.5), 2.5);
        std::env::remove_var(key);
    }

    #[test]
    fn env_secs_negative_falls_back_to_default() {
        let key = "NICE_RS_TEST_ENV_SECS_NEG";
        std::env::set_var(key, "-1.0");
        assert_eq!(env_secs(key, 2.5), 2.5);
        std::env::remove_var(key);
    }

    #[test]
    fn gate_report_round_trips_and_clears() {
        use super::{report_gate, take_gate_report};
        use crate::frame::{CadenceReport, IntervalStats};

        // Start from a clean slot (no other test touches GATE_REPORT).
        let _ = take_gate_report();
        assert!(take_gate_report().is_none());

        report_gate(CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "verdict".to_string(),
        });
        let r = take_gate_report().expect("verdict is present after report_gate");
        assert!(r.passed);
        assert_eq!(r.detail, "verdict");
        // A second take sees the slot cleared.
        assert!(take_gate_report().is_none(), "take clears the slot");
    }
}
