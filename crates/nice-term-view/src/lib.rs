//! nice-term-view — the GPUI-native terminal renderer (R4).
//!
//! The from-scratch renderer that paints a [`nice_term_core::Session`]'s grid
//! through gpui's **public** paint API inside gpui's single Metal stack. There
//! is **no AppKit bridging** here: in GPUI the terminal is an ordinary element
//! in the framework's own tree, so the `NSViewRepresentable` dance today's
//! `TerminalHost.swift` needs (one-runloop deferrals, reparent guards, Metal
//! rebind defenses) simply does not exist. If this crate ever reaches for
//! `objc2`, that is a design smell — the one sanctioned crossing (the
//! demand-present kick) is injected from `crates/nice/src/platform`.
//!
//! ## Pieces
//!
//! * [`TerminalSessionHandle`] — the core→GPUI adapter **entity**: it owns the
//!   session and the single task that drains its event stream, re-emitting typed
//!   gpui [`TerminalEvent`]s + `cx.notify()`. It is view-independent (title /
//!   cwd / exit events flow with no view attached — R6 / R7 ride it).
//! * [`TerminalView`] / [`TerminalElement`] — the view (owns a `FocusHandle`;
//!   caret solid/hollow is *computed* from focus, never a stored flag) and the
//!   per-frame paint element (background quads + foreground glyph runs + block
//!   cursor).
//! * [`TerminalTheme`] — the render-half theme value (16 ANSI + bg/fg/cursor/
//!   selection), shaped like `TerminalTheme.swift`.
//! * [`FontSettings`] — the app-level terminal font state (T11): the family
//!   chain + point size + derived cell metrics every [`TerminalView`] observes.
//!   A ⌘+/⌘−/⌘0 zoom mutates it; each view re-metrics and resizes its pty.
//! * [`color`] — the full color model resolver (16 themed ANSI, 256 computed
//!   cube/ramp, 24-bit truecolor).
//! * [`drop`] — the file / image drag-drop escaped-path typer (T7): the pure
//!   byte builder ([`drop_bytes`]) behind [`TerminalView`]'s
//!   `on_drop::<ExternalPaths>` handler, reusing `shell_backslash_escape` +
//!   the R5 bracketed-paste wrap seam.
//! * [`overlay`] — the two R7 terminal-niceties state machines (T9/T10): the
//!   [`LaunchOverlay`] "Launching…" timing machine (silent-pane grace → overlay →
//!   cleared on first output) and the [`HeldPane`] machine (a non-clean exit keeps
//!   the pane mounted + readable, writes the dim in-buffer footer, and a
//!   single-pane-era dismiss respawns a fresh shell). Both are driven off the R3
//!   [`TerminalEvent`] stream the [`TerminalView`] now subscribes to.
//!
//! ## What this crate covers so far
//!
//! The full per-cell paint model: the color model (16 themed ANSI, 256 computed
//! cube/ramp, 24-bit truecolor), text attributes (inverse-video with exact
//! per-channel inversion, bold, italic, dim, underline, strikethrough), wide
//! glyphs / emoji, selection rendering from the core's selection state (with a
//! programmatic setter test seam), and procedural box-drawing + block elements
//! (U+2500–259F) so line glyphs join seamlessly.
//!
//! It also owns the **row-quantized, top-anchored layout** (T4, revised: row 0
//! flush at the element's top, sub-row remainder clipped at the bottom — a
//! deliberate divergence from prod's bottom-anchored `TerminalContainerView`;
//! see [`element`]'s module doc), **line-stepped
//! scrollback scroll** (wheel/trackpad → [`TerminalSessionHandle::scroll_lines`],
//! clamped, core-driven auto-snap-to-bottom, float remainder kept as the deferred
//! smooth-scroll seam), and **damage-driven present**: the session handle's drain
//! wakes `cx.notify()` + an injected [`PresentKick`] (`setNeedsDisplay`,
//! constructed in `crates/nice/src/platform` — the sole objc2 crossing).
//!
//! The renderer is now exercised end-to-end by the app: `crates/nice`'s shipped
//! window hosts one live zsh pane over this crate, and the `term-perf` self-test
//! gates streaming frame time (p50 ≤ 17.5 ms / p95 ≤ 20 ms) + memory (< 200 MiB)
//! under the synthetic workload. The per-frame paint stays within budget because
//! gpui's `LineLayoutCache` absorbs the single-cell `shape_line` reshapes (no
//! full-grid reshape cost accrues), so no damage-gating of the snapshot is needed
//! to hit the gate.

pub mod color;
pub mod drop;
pub mod element;
pub mod font;
pub mod input;
pub mod mouse;
pub mod overlay;
pub mod session_handle;
pub mod theme;
pub mod view;

// Procedural box-drawing (U+2500–257F) + block elements (U+2580–259F). Painted
// from geometry, not the font, so line glyphs join seamlessly. Crate-private:
// the paint path in `element` is the only consumer.
mod boxdraw;

pub use color::{resolve_color, xterm256};
pub use drop::{drop_bytes, is_safe_path, ImageDropProvider};
pub use element::{
    fit_grid, grid_top_y, ImeInput, TerminalElement, TerminalMetrics, TERMINAL_BOTTOM_GAP,
};
pub use font::{
    cell_metrics, clamp_line_height, clamp_px, default_font_chain, resolve_family,
    snap_metrics_to_scale, FontSettings, FontZoom, DEFAULT_TERMINAL_FONT_PX,
    DEFAULT_TERMINAL_LINE_HEIGHT, MAX_TERMINAL_FONT_PX, MAX_TERMINAL_LINE_HEIGHT,
    MIN_TERMINAL_FONT_PX, MIN_TERMINAL_LINE_HEIGHT,
};
pub use input::{KeyCodeProbe, TermInputHandler};
pub use overlay::{
    held_exit_footer, HeldPane, LaunchDeadline, LaunchDeadlineFuture, LaunchOverlay,
    DEFAULT_LAUNCH_OVERLAY_GRACE, HELD_FOOTER_LABEL,
};
pub use session_handle::{PresentKick, TerminalEvent, TerminalSessionHandle};
pub use theme::{TerminalColor, TerminalTheme};
pub use view::TerminalView;
