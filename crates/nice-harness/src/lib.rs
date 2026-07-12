//! nice-harness — measurement + self-test harness for the Nice RS rewrite.
//!
//! Ports the *learnings* of the phase-0 spike's `harness.rs` (not the file
//! verbatim) into the permanent home every later cycle builds on:
//!
//! - [`clock`]     — monotonic mach clock (frame stamps, measurement timing).
//! - [`mem`]       — `task_info(TASK_VM_INFO)` phys_footprint + RSS sampler.
//! - [`signpost`]  — os_signpost emission on subsystem
//!                   `dev.nickanderssohn.nice` (C shim in `signpost.c`).
//! - [`frame`]     — frame-stamp stream, percentile reducer, cadence gate.
//! - [`workload`]  — deterministic synthetic "Claude-streaming" stressor (the
//!                   `term-perf` gate's renderer workload; ported from the spike).
//! - [`watchdog`]  — App-Nap-immune OS-thread deadline (guaranteed auto-exit).
//! - [`capture`]   — screenshot via `Window::render_to_image()` (feature
//!                   `capture` only; enables gpui `test-support`).
//! - [`selftest`]  — the `NICE_SELFTEST` driver + `all` suite runner, and
//!                   the [`selftest::Scenario`] registry later cycles extend.
//!
//! Layering rule for the rewrite: crates mirroring today's pure-Swift model
//! code must NOT depend on gpui. This harness is inherently a gpui/measurement
//! crate, so it does; the rule governs future model crates, not this one.

pub mod capture;
pub mod clock;
pub mod frame;
pub mod mem;
pub mod selftest;
pub mod signpost;
pub mod watchdog;
pub mod workload;
