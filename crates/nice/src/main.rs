//! nice-rs — the Nice rewrite's GPUI application binary (Path B, all-Rust
//! single Metal stack). Process/binary name `nice-rs`, distinct from the Swift
//! `Nice` / `Nice Dev` builds.
//!
//! Structure (grows over later cycles):
//!   * [`app`] — owns window creation + the root view (shipped window and the
//!     self-test scenario window).
//!   * [`platform`] — the single home for foreign AppKit / objc2 access
//!     (all-Rust rule). For R1: the demand-present kick + present-timing facts.
//!
//! Entry dispatch: `NICE_RS_SELFTEST=<scenario>` runs the measurement harness
//! (see `nice_harness::selftest`); otherwise the normal app opens its window.

mod app;
mod platform;

fn main() {
    match std::env::var("NICE_RS_SELFTEST") {
        Ok(selector) if !selector.trim().is_empty() => app::run_selftest(selector),
        _ => app::run(),
    }
}
