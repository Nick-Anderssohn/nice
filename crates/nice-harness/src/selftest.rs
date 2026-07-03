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
//! and passed to [`drive`] as a `Vec<Scenario>`. Later cycles extend the gate
//! by adding scenarios to that vec — the driver, reducer, watchdog and table
//! printing here never change.
//!
//! The whole run lives inside a SINGLE `Application::run`: [`drive`] is called
//! from the app's run closure, foregrounds the app, arms the watchdog, and
//! spawns one async orchestrator that drives each scenario's window in turn.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp};

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

    // Foreground + activate so the scenario windows become frontmost AND
    // focused — zed-main frame-caps inactive windows at ~33 ms, so cadence
    // measurement must run on the active window (see `frame` module docs).
    cx.activate(true);

    // Hard backstop: a real-OS-thread deadline across ALL scenarios + grace.
    // App Nap can defer the async orchestrator's timers on an occluded/idle
    // run; the watchdog cannot starve, so the self-test can never hang. On fire
    // it prints the FAIL marker and hard-exits.
    let n = selected.len().max(1) as f64;
    let budget = n * (WARMUP_SECS + measure_secs + 4.0) + 5.0;
    let selector_for_watchdog = selector.to_string();
    watchdog::arm(
        Duration::from_secs_f64(budget),
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

        let handle = match (sc.open)(cx) {
            Ok(h) => h,
            Err(e) => {
                let report = frame::CadenceReport::error(format!("window open failed: {e}"));
                eprintln!("[selftest] scenario '{}': {}", sc.name, report.detail);
                results.push((sc.name, report));
                continue;
            }
        };

        // Warm up (discard startup frames), then measure a clean window.
        let t = cx.background_executor().timer(warmup);
        t.await;
        frame::reset();
        let t = cx.background_executor().timer(measure);
        t.await;

        let mut report = frame::assess_cadence(MIN_FRAMES, CADENCE_RATIO);

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
}
