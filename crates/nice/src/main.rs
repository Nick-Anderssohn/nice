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
//!   * [`chrome_live`] — the R9 live window-chrome self-test scenario
//!     (`chrome`): real mouse CGEvents drive the shipped band + repositioned
//!     traffic lights + full-screen wiring, judged against AppKit frame/state
//!     reads (geometry, drag differential, double-click, full-screen toggle).
//!   * [`niceties_zoom`] — the R7/T11 live zoom + pty re-metric self-test
//!     (`niceties-zoom`): real ⌘+/⌘0 CGEvents grow the shared font, and the
//!     grid re-fits + the pty winsize follows.
//!   * [`niceties_drop`] — the R7/T7 file/image drag-drop self-test
//!     (`niceties-drop`): the drop handler is driven with constructed
//!     `ExternalPaths` events and asserts byte-exact escaped-path typing.
//!   * [`niceties_overlay`] — the R7/T9 "Launching…" overlay self-test
//!     (`niceties-overlay`): a slow silent pane shows the overlay past a short
//!     grace window and clears it on first output, while an instant-prompt pane
//!     never flashes it (the state-machine counter).
//!   * [`niceties_held`] — the R7/T10 held-pane self-test (`niceties-held`): a
//!     non-zero exit stays mounted with the dim in-buffer footer + the dismiss
//!     affordance, typing is inert, and dismiss respawns a fresh shell.
//!
//! Entry dispatch: `NICE_RS_SELFTEST=<scenario>` runs the measurement harness
//! (see `nice_harness::selftest`); otherwise the normal app opens its window.

mod app;
mod chrome_live;
mod input_live;
mod niceties_drop;
mod niceties_held;
mod niceties_overlay;
mod niceties_zoom;
mod platform;

fn main() {
    match std::env::var("NICE_RS_SELFTEST") {
        Ok(selector) if !selector.trim().is_empty() => app::run_selftest(selector),
        _ => app::run(),
    }
}
