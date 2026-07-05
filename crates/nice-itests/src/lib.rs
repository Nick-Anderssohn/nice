//! nice-itests — the in-process gpui integration-test harness for the Nice
//! rewrite.
//!
//! This crate is **dev/test-only** (it is `publish = false` and the shipped app
//! binary `nice` never depends on it). It holds the shared fixtures the Stage-2
//! chrome/pane cycles (R9–R13) write their tests on, plus the three exemplar
//! harness proofs that pin how those fixtures are meant to be used.
//!
//! ## The two execution models (never conflate them)
//!
//! In-process tests come in two kinds, and where a test lives is dictated by
//! which gpui test context it needs:
//!
//! * **Behavior tests — mocked [`gpui::TestAppContext`].** TestPlatform +
//!   `NoopTextSystem`: no Metal, no pixels, deterministic scheduling. Right for
//!   focus / dispatch / entity behavior and byte-exact input encoding. They are
//!   ordinary libtest `#[gpui::test]` cases and may parallelize under
//!   `cargo test`. Boot / mount / input drivers for this model live in
//!   [`behavior`] (compiled only under `cfg(test)` or the `test-support`
//!   feature). Exemplars: [`behavior_exemplars`].
//! * **Visual/pixel tests — real-MacPlatform `VisualTestAppContext`.** The real
//!   `MacPlatform` wrapped with a `TestDispatcher`: real Metal rendering into an
//!   off-screen window, `capture_screenshot`, a simulated clock. These **cannot**
//!   run under libtest — real NSWindows are main-thread-only, libtest runs every
//!   case on a worker thread, and no `#[gpui::visual_test]` macro exists at the
//!   pin — so they live in one or more `harness = false` integration binaries
//!   whose `main` owns the platform on the main thread and runs cases serially,
//!   exiting nonzero on failure. `cargo test -p nice-itests` still runs those
//!   binaries and gates on their exit code. Exemplar:
//!   `tests/visual_terminal_screenshot.rs`.
//!
//! ## What must never be asserted here
//!
//! Both models run on a **simulated clock**, so neither may assert
//! frames-per-second, frame-pacing percentiles, or wall-clock latency — the A/B
//! program proved simulated/self-reported evidence is untrustworthy on exactly
//! that class of claim. Cadence / perf / latency gates live **only** in the live
//! `NICE_RS_SELFTEST` suite (real windowserver, real CVDisplayLink). A
//! cadence/perf assertion in an in-process test is a blocking review finding.
//!
//! ## Module map
//!
//! * [`pixels`] — pure screenshot-sampling + `±8/255` per-channel pixel-assert
//!   helpers and the bottom-anchored cell-centre geometry. No gpui test-support.
//! * [`session`] — fixture-session builders (the byte-piped `cat` / `ZDOTDIR`
//!   pattern and the raw-mode capture-`tee` pattern) and capture-file readers.
//!   No gpui test-support.
//! * [`behavior`] — boot the mocked app, mount a [`nice_term_view::TerminalView`]
//!   with the `nice-theme` tokens applied, and the simulated keystroke/mouse
//!   drivers. Gated behind `cfg(test)` / the `test-support` feature.

pub mod pixels;
pub mod session;

#[cfg(any(test, feature = "test-support"))]
pub mod behavior;

// The two behavior exemplars (keystroke-encoder + advance_clock) — libtest
// `#[gpui::test]` cases on the mocked context. Compiled only for this crate's
// own tests; each states its execution model in its doc comment.
#[cfg(test)]
mod behavior_exemplars;

// R9 window-chrome band-press classification differentials — libtest
// `#[gpui::test]` cases on the mocked context (double-click consumed; a press on
// an interactive child never reaches the band; the ~2pt drag-threshold split).
#[cfg(test)]
mod chrome_band;

// R10 sidebar multi-select / rename-gate / disclosure classification
// differentials — libtest `#[gpui::test]` cases on the mocked context. Mirrors
// the `SidebarShellView` routing (unimportable from a dev/test crate, like the
// `chrome_band` band) over the REAL `nice-model` selection / rename-gate types,
// driven by simulated clicks + `advance_clock`.
#[cfg(test)]
mod sidebar_multiselect;

// R11 pane-strip real-layout differentials — libtest `#[gpui::test]` cases on the
// mocked context. Mirrors `WindowToolbarView`'s strip logic (unimportable from a
// dev/test crate) over a REAL `ScrollHandle` + the REAL `nice-model` strip
// predicates, asserting overflow onset / fades / badge / ✕-slot reservation /
// select-close-rename routing / centering against real Taffy layout.
#[cfg(test)]
mod pane_strip;
