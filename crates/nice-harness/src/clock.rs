//! Monotonic mach clock — the single time source for every frame stamp and
//! measurement (ported from the phase-0 spike's `harness::clock`).
//!
//! `mach_absolute_time` returns raw ticks; the timebase (numer/denom) converts
//! ticks to nanoseconds. Capture raw, reduce at the end — never online.

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

/// Monotonic raw ticks — the single clock for every measurement.
#[inline]
pub fn now() -> u64 {
    unsafe { mach_absolute_time() }
}

/// Nanoseconds between two tick instants (`a` before `b`).
#[inline]
pub fn ns_between(a: u64, b: u64) -> f64 {
    let (n, d) = timebase();
    (b.wrapping_sub(a) as f64) * (n as f64) / (d as f64)
}

/// Milliseconds between two tick instants (`a` before `b`).
#[inline]
pub fn ms_between(a: u64, b: u64) -> f64 {
    ns_between(a, b) / 1.0e6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_monotonic_nondecreasing() {
        let a = now();
        let b = now();
        assert!(b >= a);
    }

    #[test]
    fn ns_between_equal_instants_is_zero() {
        assert_eq!(ns_between(1_000, 1_000), 0.0);
    }

    #[test]
    fn ns_between_positive_for_forward_delta() {
        assert!(ns_between(0, 1_000_000) > 0.0);
    }

    #[test]
    fn ns_between_scales_with_tick_delta() {
        // Timebase is a fixed linear scale, so a 2x tick delta is exactly 2x ns.
        assert_eq!(ns_between(0, 2_000), 2.0 * ns_between(0, 1_000));
    }

    #[test]
    fn ns_between_wraps_on_counter_rollover() {
        // The deliberate wrapping_sub: a `b` that has rolled past u64::MAX must
        // still yield the true (small, positive) forward delta, not a huge or
        // negative value. 0.wrapping_sub(u64::MAX) == 1, same as 1.wrapping_sub(0).
        assert_eq!(ns_between(u64::MAX, 0), ns_between(0, 1));
        assert!(ns_between(u64::MAX, 4) > 0.0);
    }

    #[test]
    fn ms_between_is_ns_scaled_by_1e6() {
        assert_eq!(ms_between(0, 5_000_000), ns_between(0, 5_000_000) / 1.0e6);
    }
}
