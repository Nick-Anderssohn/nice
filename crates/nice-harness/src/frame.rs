//! Frame-cadence measurement: a process-global stream of mach-clock frame
//! stamps, the percentile reducer over frame INTERVALS, and the self-test
//! cadence assertion.
//!
//! Capture raw, reduce at the end (never online) — ported from the phase-0
//! spike's `harness` FPS counters, trimmed to what the self-test gate needs.
//!
//! ## Why cadence assertions require a frontmost, focused window
//!
//! Two facts about gpui present timing on the pinned zed-main revision that
//! every scenario author must respect (see also `crate::platform` in the app
//! crate for the demand-present kick these facts motivate):
//!
//! 1. `cx.notify()` alone never PRESENTS while the CVDisplayLink is stopped
//!    (an occluded window stops its link in `window_did_change_occlusion_state`).
//!    A demand-driven repaint on such a window needs an explicit
//!    `setNeedsDisplay` kick to reach `MetalRenderer::draw`. The smoke scenario
//!    sidesteps this by driving continuous `request_animation_frame` repaints on
//!    a visible window, but later demand-driven scenarios must issue the kick.
//! 2. zed-main frame-caps INACTIVE windows to ~33 ms (`min_frame_interval`), so
//!    a backgrounded window animates at ~30 fps regardless of the panel. That
//!    steady 30 fps still passes the jitter gate below, but absolute-throughput
//!    assertions (later cycles) must run on a FRONTMOST, FOCUSED window — which
//!    is why the self-test runbook requires the window be frontmost.

use std::sync::Mutex;

use crate::clock;

/// Frame-composite timestamps (mach ticks), one per self-test repaint.
static FRAMES: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Stamp the current frame (call once per render on the measured window).
pub fn stamp() {
    FRAMES.lock().unwrap().push(clock::now());
}

/// Clear the frame stream — call at the start of each measurement so warm-up
/// frames don't pollute the percentiles.
pub fn reset() {
    FRAMES.lock().unwrap().clear();
}

/// Number of frame stamps collected so far.
pub fn len() -> usize {
    FRAMES.lock().unwrap().len()
}

/// Snapshot the frame stream (cloned).
pub fn drain() -> Vec<u64> {
    FRAMES.lock().unwrap().clone()
}

/// p50/p95/p99 of an arbitrary f64 sample set (sorts in place).
pub fn percentiles(v: &mut [f64]) -> (f64, f64, f64) {
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

/// Reduced frame-interval statistics (ms) from a timestamp stream.
#[derive(Clone, Copy, Debug, Default)]
pub struct IntervalStats {
    /// Number of frame timestamps (interval count is this minus one).
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Frames per second implied by the median interval.
    pub fps_p50: f64,
}

/// Reduce a frame-timestamp stream to interval percentiles.
pub fn interval_stats(timestamps: &[u64]) -> IntervalStats {
    let mut intervals: Vec<f64> = timestamps
        .windows(2)
        .map(|w| clock::ms_between(w[0], w[1]))
        .collect();
    let max_ms = intervals.iter().cloned().fold(0.0_f64, f64::max);
    let (p50, p95, p99) = percentiles(&mut intervals);
    IntervalStats {
        samples: timestamps.len(),
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
        max_ms,
        fps_p50: if p50 > 0.0 { 1000.0 / p50 } else { 0.0 },
    }
}

/// Outcome of the self-test cadence gate.
#[derive(Clone, Debug)]
pub struct CadenceReport {
    pub passed: bool,
    pub stats: IntervalStats,
    /// Human-readable one-line verdict (printed in the suite table).
    pub detail: String,
}

impl CadenceReport {
    /// A scenario that failed before any cadence could be measured (e.g. the
    /// window failed to open).
    pub fn error(msg: impl Into<String>) -> Self {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: msg.into(),
        }
    }
}

/// Assess frame-cadence sanity over the current frame stream and CLEAR it.
///
/// Passes when the window actually animated (`>= min_samples` stamps) and the
/// cadence is not wildly jittery: p95 interval `< cadence_ratio` × the median
/// interval (the plan's "p95 < ~2× median" sanity check).
pub fn assess_cadence(min_samples: usize, cadence_ratio: f64) -> CadenceReport {
    let stamps = drain();
    reset();
    assess_stats(interval_stats(&stamps), min_samples, cadence_ratio)
}

/// Pure cadence verdict over already-reduced [`IntervalStats`]. Split out from
/// [`assess_cadence`] (which drains the process-global frame stream) so every
/// branch of the gate is unit-testable without a gpui window or the global
/// state. See [`assess_cadence`] for the pass criteria.
pub fn assess_stats(
    stats: IntervalStats,
    min_samples: usize,
    cadence_ratio: f64,
) -> CadenceReport {
    if stats.samples < min_samples {
        return CadenceReport {
            passed: false,
            stats,
            detail: format!(
                "only {} frames in the measurement window (need >= {min_samples}); \
                 the window never sustained animated repaint",
                stats.samples
            ),
        };
    }
    if stats.p50_ms <= 0.0 {
        return CadenceReport {
            passed: false,
            stats,
            detail: "median frame interval was 0 ms (degenerate stream)".to_string(),
        };
    }
    let limit = cadence_ratio * stats.p50_ms;
    let passed = stats.p95_ms < limit;
    let detail = format!(
        "{} frames | p50 {:.2} ms ({:.0} fps) | p95 {:.2} ms | max {:.2} ms | \
         p95 {} {:.2} ms ({:.1}x median)",
        stats.samples,
        stats.p50_ms,
        stats.fps_p50,
        stats.p95_ms,
        stats.max_ms,
        if passed { "<" } else { ">=" },
        limit,
        cadence_ratio,
    );
    CadenceReport {
        passed,
        stats,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_empty_is_all_zero() {
        assert_eq!(percentiles(&mut []), (0.0, 0.0, 0.0));
    }

    #[test]
    fn percentiles_single_element_is_that_element() {
        assert_eq!(percentiles(&mut [42.0]), (42.0, 42.0, 42.0));
    }

    #[test]
    fn percentiles_sorts_in_place() {
        let mut v = [3.0, 1.0, 2.0];
        // len 3: p50 = v[1] = 2.0; idx(95) = v[(3*95)/100 = 2] = 3.0; same for 99.
        assert_eq!(percentiles(&mut v), (2.0, 3.0, 3.0));
        assert_eq!(v, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn percentiles_nearest_rank_boundary_at_len_100() {
        // Values 0.0..=99.0. Nearest-rank (no interpolation): idx(p) = v[len*p/100].
        // p50 = v[50] = 50, p95 = v[95] = 95 (NOT v[94]), p99 = v[99] = 99.
        let mut v: Vec<f64> = (0..100).map(|i| i as f64).collect();
        assert_eq!(percentiles(&mut v), (50.0, 95.0, 99.0));
    }

    #[test]
    fn percentiles_clamps_top_rank() {
        // idx(99) on a 3-element set clamps to the last element, never OOB.
        let mut v = [1.0, 2.0, 3.0];
        let (_, _, p99) = percentiles(&mut v);
        assert_eq!(p99, 3.0);
    }

    #[test]
    fn interval_stats_empty_stream_is_default() {
        let s = interval_stats(&[]);
        assert_eq!(s.samples, 0);
        assert_eq!(s.p50_ms, 0.0);
        assert_eq!(s.max_ms, 0.0);
        assert_eq!(s.fps_p50, 0.0);
    }

    #[test]
    fn interval_stats_single_stamp_has_no_intervals() {
        // One timestamp => zero intervals => all-zero reduction, samples == 1.
        let s = interval_stats(&[clock::now()]);
        assert_eq!(s.samples, 1);
        assert_eq!(s.p50_ms, 0.0);
        assert_eq!(s.max_ms, 0.0);
        assert_eq!(s.fps_p50, 0.0);
    }

    #[test]
    fn interval_stats_derives_fps_from_median_interval() {
        // Three increasing stamps => two intervals. Exact ms depends on the
        // mach timebase, so assert the invariants rather than absolute values.
        let a = clock::now();
        let stamps = [a, a + 1_000_000, a + 3_000_000];
        let s = interval_stats(&stamps);
        assert_eq!(s.samples, 3);
        assert!(s.p50_ms > 0.0);
        assert!(s.max_ms >= s.p50_ms);
        assert!((s.fps_p50 - 1000.0 / s.p50_ms).abs() < 1e-9);
    }

    fn stats(samples: usize, p50_ms: f64, p95_ms: f64) -> IntervalStats {
        IntervalStats {
            samples,
            p50_ms,
            p95_ms,
            p99_ms: p95_ms,
            max_ms: p95_ms,
            fps_p50: if p50_ms > 0.0 { 1000.0 / p50_ms } else { 0.0 },
        }
    }

    #[test]
    fn assess_stats_fails_when_too_few_samples() {
        let r = assess_stats(stats(5, 10.0, 15.0), 30, 2.0);
        assert!(!r.passed);
        assert!(r.detail.contains("need >= 30"));
    }

    #[test]
    fn assess_stats_fails_on_degenerate_zero_median() {
        let r = assess_stats(stats(100, 0.0, 0.0), 30, 2.0);
        assert!(!r.passed);
        assert!(r.detail.contains("degenerate"));
    }

    #[test]
    fn assess_stats_fails_on_excess_jitter() {
        // p95 (25) >= ratio (2.0) * p50 (10) == 20 => jitter fail.
        let r = assess_stats(stats(100, 10.0, 25.0), 30, 2.0);
        assert!(!r.passed);
    }

    #[test]
    fn assess_stats_jitter_gate_is_strict_less_than() {
        // p95 exactly at the limit (2.0 * 10 == 20) must FAIL (`p95 < limit`).
        let r = assess_stats(stats(100, 10.0, 20.0), 30, 2.0);
        assert!(!r.passed);
    }

    #[test]
    fn assess_stats_passes_on_steady_cadence() {
        // p95 (15) < ratio (2.0) * p50 (10) == 20 => pass.
        let r = assess_stats(stats(100, 10.0, 15.0), 30, 2.0);
        assert!(r.passed);
        assert!(r.detail.contains("p50 10.00 ms"));
    }
}
