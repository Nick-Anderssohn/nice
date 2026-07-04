//! nice-rs — the Nice rewrite's GPUI application binary (Path B, all-Rust
//! single Metal stack). Process/binary name `nice-rs`, distinct from the Swift
//! `Nice` / `Nice Dev` builds.
//!
//! Structure (grows over later cycles):
//!   * [`app`] — owns window creation + the root view (shipped window and the
//!     self-test scenario window).
//!   * [`platform`] — the single home for foreign AppKit / objc2 / CoreGraphics
//!     access (all-Rust rule): the demand-present kick + present-timing facts
//!     (R1), the keyCode side-channel (R5), and the CGEvent/AX/TIS FFI the live
//!     input scenarios drive (R5).
//!   * [`input_live`] — the R5 live input self-test scenarios (`input-live` /
//!     `input-shell`): real CGEvents posted to our own pid, byte-exact pty
//!     receipt, the item-4 candidate anchor, and the IME go/no-go probe.
//!
//! Entry dispatch: `NICE_RS_SELFTEST=<scenario>` runs the measurement harness
//! (see `nice_harness::selftest`); otherwise the normal app opens its window.

mod app;
mod input_live;
mod platform;

fn main() {
    match std::env::var("NICE_RS_SELFTEST") {
        Ok(selector) if !selector.trim().is_empty() => app::run_selftest(selector),
        _ => app::run(),
    }
}
