//! `FontSettings` — the app-level terminal-font state (T11).
//!
//! This is the shared, **app-level** font state the plan calls for: the terminal
//! font family chain + point size, owned once at the app root (`crates/nice`)
//! and observed by every [`TerminalView`](crate::view::TerminalView). It is a
//! gpui **entity** so the view can `cx.observe` it and the ⌘+/⌘−/⌘0 zoom
//! keybindings can mutate it; a single mutation fans out to every pane, which is
//! exactly the process-wide behavior Nice ships today (`FontSettings.swift` +
//! `TabPtySession.applyTerminalFont`). Stage 2's proportional sidebar scale
//! (R10) subscribes to the same [`FontZoom`] event surface, so it needs no
//! refactor of this entity.
//!
//! ## Why the type lives here, not in `crates/nice`
//!
//! The plan asks for "app-level state, not view state", owned in `crates/nice`.
//! Rust's dependency graph runs `nice → nice-term-view` (one way), and the slice
//! requires that (a) every `TerminalView` hold a handle to this state and
//! `cx.observe` it, and (b) the view's own zoom keybindings `entity.update(...)`
//! it. Both force the *type* to be nameable by `nice-term-view`, so it is
//! declared here. Ownership still lives at the app level: `crates/nice`
//! constructs the single shared `Entity<FontSettings>` (see `app.rs`) and hands
//! it to each view — no view creates its own.
//!
//! ## Font-chain resolution
//!
//! [`resolve_family`] tries the SF Mono → JetBrains Mono NL → system-monospace
//! chain in order for availability, resolved through GPUI's text system
//! ([`TextSystem::all_font_names`]) — a direct port of `TabPtySession`'s
//! `terminalFont(named:size:)`. Cell metrics are then **derived** from the
//! resolved font at the current size ([`cell_metrics`]) so a zoom re-metrics the
//! grid; the deterministic renderer self-tests instead pin an explicit cell box
//! ([`FontSettings::fixed`]) so their pixel math keys off a known pitch.

use std::sync::Arc;

use gpui::{
    px, Context, EventEmitter, Font, FontFeatures, FontStyle, FontWeight, SharedString, TextSystem,
};

use crate::element::TerminalMetrics;

/// Terminal default point size — Xcode's editor default (13pt SF Mono Regular).
/// Ported from `FontSettings.swift`'s `defaultTerminalSize`.
pub const DEFAULT_TERMINAL_FONT_PX: f32 = 13.0;
/// Smallest allowed size (JetBrains Mono NL is still legible at 8pt).
/// `FontSettings.swift`'s `minSize`.
pub const MIN_TERMINAL_FONT_PX: f32 = 8.0;
/// Largest allowed size (accessibility zoom without single-digit column counts).
/// `FontSettings.swift`'s `maxSize`.
pub const MAX_TERMINAL_FONT_PX: f32 = 32.0;

/// The shipped default terminal line-height multiplier (restyle 3/3). Fresh
/// installs get this roomier grid; the existing-user migration pins the legacy
/// `1.0` explicitly so declining the restyle leaves the grid unchanged.
pub const DEFAULT_TERMINAL_LINE_HEIGHT: f32 = 1.3;
/// Tightest allowed line-height (no extra leading — the classic terminal grid).
pub const MIN_TERMINAL_LINE_HEIGHT: f32 = 1.0;
/// Loosest allowed line-height.
pub const MAX_TERMINAL_LINE_HEIGHT: f32 = 1.8;

/// The default terminal font family chain, tried in order for availability.
///
/// Ported exactly from `TabPtySession.terminalFont(named:size:)`
/// (`SFMono-Regular` → `JetBrainsMonoNL-Regular` → `monospacedSystemFont`), but
/// keyed on GPUI's **family** names (what [`TextSystem::all_font_names`] returns
/// and [`Font::family`] matches), not the PostScript names `NSFont(name:)` uses.
/// The final `Menlo` stands in for `NSFont.monospacedSystemFont`: a stock macOS
/// monospace family that is always present, so the chain never comes up empty.
pub fn default_font_chain() -> Vec<SharedString> {
    vec![
        SharedString::from("SF Mono"),
        SharedString::from("JetBrains Mono NL"),
        SharedString::from("Menlo"),
    ]
}

/// Emitted on every zoom / reset that actually changes the size. Carries the new
/// point size so a subscriber can scale off it. This is the event surface R10's
/// proportional sidebar scale subscribes to (`cx.subscribe(&font, …)`); the
/// terminal views themselves ride the entity's `notify` via `cx.observe`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontZoom {
    /// The new terminal point size after the change.
    pub px: f32,
}

/// How a [`FontSettings`]' cell metrics are produced.
#[derive(Clone, Copy, Debug)]
enum MetricsMode {
    /// Derived from the resolved font at the current size via the text system
    /// (the shipped path): a zoom recomputes them.
    Derived,
    /// A caller-pinned cell box (the deterministic renderer self-tests): a zoom
    /// scales it proportionally from the base, so the pitch stays predictable.
    Fixed {
        base_px: f32,
        base_metrics: TerminalMetrics,
    },
}

/// App-level terminal font state: the family chain + resolved family + point
/// size + derived cell metrics. See the module docs.
pub struct FontSettings {
    /// The ordered family chain (kept for diagnostics / a future settings UI).
    chain: Vec<SharedString>,
    /// The resolved family (first available in `chain`). Fixed this cycle — the
    /// settings UI that lets the user override it is R23.
    family: SharedString,
    /// Current point size, clamped to `[MIN, MAX]`.
    px: f32,
    /// Terminal line-height multiplier, clamped to
    /// `[MIN_TERMINAL_LINE_HEIGHT, MAX_TERMINAL_LINE_HEIGHT]`. Multiplies the
    /// cell HEIGHT only (width untouched); the natural glyph height is preserved
    /// as `TerminalMetrics::glyph_h` for cursor centering.
    line_height: f32,
    /// Cell box for `family` at `px` and `line_height` (derived or pinned per
    /// `mode`).
    metrics: TerminalMetrics,
    mode: MetricsMode,
}

impl EventEmitter<FontZoom> for FontSettings {}

impl FontSettings {
    /// The shipped default: the [`default_font_chain`] resolved through GPUI's
    /// text system at [`DEFAULT_TERMINAL_FONT_PX`], with cell metrics **derived**
    /// from the resolved font. Construct inside `cx.new(|cx| …)`.
    pub fn resolved_default(cx: &mut Context<Self>) -> Self {
        let chain = default_font_chain();
        let ts = cx.text_system().clone();
        let family = resolve_family(&ts, &chain);
        let px = DEFAULT_TERMINAL_FONT_PX;
        let line_height = DEFAULT_TERMINAL_LINE_HEIGHT;
        let metrics = cell_metrics(&ts, &family, px, line_height);
        Self {
            chain,
            family,
            px,
            line_height,
            metrics,
            mode: MetricsMode::Derived,
        }
    }

    /// A **fixed-metrics** font state: an explicit family + size + cell box, with
    /// no text-system derivation. The deterministic renderer self-tests
    /// (`term-render`/`term-layout`/`term-scroll`/`term-perf`, `input-*`) use
    /// this so their pixel assertions key off a known pitch, independent of the
    /// machine's installed fonts. Needs no `Context` (nothing is resolved).
    pub fn fixed(family: SharedString, px: f32, metrics: TerminalMetrics) -> Self {
        Self {
            chain: vec![family.clone()],
            family,
            px,
            // The deterministic renderer self-tests pin their pixel pitch and
            // want the cursor to fill the whole cell — line-height 1.0.
            line_height: MIN_TERMINAL_LINE_HEIGHT,
            metrics,
            mode: MetricsMode::Fixed {
                base_px: px,
                base_metrics: metrics,
            },
        }
    }

    /// The ordered family chain this state resolved from (SF Mono → JetBrains
    /// Mono NL → system mono for the shipped default). Kept for diagnostics and
    /// the future settings UI (R23) that lets the user reorder / override it.
    pub fn chain(&self) -> &[SharedString] {
        &self.chain
    }

    /// The resolved font family the views paint with.
    pub fn family(&self) -> SharedString {
        self.family.clone()
    }

    /// The current point size.
    pub fn px(&self) -> f32 {
        self.px
    }

    /// The current terminal line-height multiplier.
    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    /// The current cell metrics for `family` at `px`.
    pub fn metrics(&self) -> TerminalMetrics {
        self.metrics
    }

    /// Zoom by `delta` points (⌘+ is `+1`, ⌘− is `-1`), clamped to `[MIN, MAX]`.
    /// A no-op at the clamp bound (never emits / notifies for a size that did not
    /// move). Mirrors `FontSettings.swift`'s `zoom(by:)` (terminal is the anchor;
    /// the sidebar scale it also drives is R10, off the [`FontZoom`] event).
    pub fn zoom_by(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.set_px(self.px + delta as f32, cx);
    }

    /// ⌘0 — snap back to the default size exactly. A no-op if already at default.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.set_px(DEFAULT_TERMINAL_FONT_PX, cx);
    }

    /// Set the terminal point size (R23's Font-pane size slider + the ⌘±/⌘0 zoom).
    /// The input is **clamped** to `[MIN, MAX]` ([`clamp_px`]); a size that does not
    /// actually move is a no-op (never emits / notifies). On a real change: recompute
    /// metrics, emit the typed [`FontZoom`] (the sidebar's proportional subscriber +
    /// R10's surface), and `notify` (the views' `cx.observe` re-metric). Takes plain
    /// `f32` only — boundary-legal (a terminal size IS a terminal concept).
    pub fn set_px(&mut self, px: f32, cx: &mut Context<Self>) {
        let new_px = clamp_px(px);
        if new_px == self.px {
            return;
        }
        self.px = new_px;
        self.recompute_metrics(cx);
        cx.emit(FontZoom { px: new_px });
        cx.notify();
    }

    /// Set the terminal line-height multiplier (restyle 3/3's Font-pane control).
    /// The input is **clamped** to `[MIN_TERMINAL_LINE_HEIGHT,
    /// MAX_TERMINAL_LINE_HEIGHT]` ([`clamp_line_height`]); a value that does not
    /// actually move is a no-op (never notifies). On a real change: recompute the
    /// cell metrics (only the cell HEIGHT changes) and `notify` so every view
    /// re-metrics its grid. Deliberately does NOT emit [`FontZoom`] — the point
    /// size is unchanged, so the sidebar's proportional subscriber must not
    /// rescale off a line-height change.
    pub fn set_line_height(&mut self, multiplier: f32, cx: &mut Context<Self>) {
        let new_lh = clamp_line_height(multiplier);
        if new_lh == self.line_height {
            return;
        }
        self.line_height = new_lh;
        self.recompute_metrics(cx);
        cx.notify();
    }

    /// Override the terminal font family (R23's Font-pane family picker). `Some(f)`
    /// makes `f` the sole chain entry (resolved through the text system, GPUI
    /// substituting a system font if it is unavailable); `None` restores the shipped
    /// [`default_font_chain`]. Re-resolves the family, re-metrics, emits [`FontZoom`]
    /// (so the sidebar's subscriber sees a change event — the point size is
    /// unchanged, so it is a proportional no-op), and `notify`s every view to
    /// re-metric. Takes `Option<SharedString>` only — boundary-legal (a terminal
    /// family IS a terminal concept).
    pub fn set_family(&mut self, family: Option<SharedString>, cx: &mut Context<Self>) {
        self.chain = match family {
            Some(f) => vec![f],
            None => default_font_chain(),
        };
        let ts = cx.text_system().clone();
        self.family = resolve_family(&ts, &self.chain);
        self.recompute_metrics(cx);
        cx.emit(FontZoom { px: self.px });
        cx.notify();
    }

    /// Reset the terminal font to the shipped defaults (R23's Font-pane "Reset to
    /// defaults"): the [`default_font_chain`] at [`DEFAULT_TERMINAL_FONT_PX`]. Unlike
    /// [`set_px`](Self::set_px) this does NOT emit [`FontZoom`] — the sidebar's own
    /// reset is driven explicitly by the Font-pane handler, so a proportional rescale
    /// off this reset would fight it. Re-resolves + re-metrics and `notify`s the views.
    pub fn reset_to_defaults(&mut self, cx: &mut Context<Self>) {
        self.chain = default_font_chain();
        let ts = cx.text_system().clone();
        self.family = resolve_family(&ts, &self.chain);
        self.px = DEFAULT_TERMINAL_FONT_PX;
        self.line_height = DEFAULT_TERMINAL_LINE_HEIGHT;
        self.recompute_metrics(cx);
        cx.notify();
    }

    /// Recompute [`metrics`](Self::metrics) for the current `family` at the current
    /// `px` — derived through the text system (the shipped path) or scaled from the
    /// pinned base box (the renderer self-tests' [`MetricsMode::Fixed`]).
    fn recompute_metrics(&mut self, cx: &mut Context<Self>) {
        self.metrics = match self.mode {
            MetricsMode::Derived => {
                let ts = cx.text_system().clone();
                cell_metrics(&ts, &self.family, self.px, self.line_height)
            }
            MetricsMode::Fixed {
                base_px,
                base_metrics,
            } => {
                let scaled = scale_metrics(base_metrics, self.px / base_px);
                apply_line_height(scaled, self.line_height)
            }
        };
    }
}

/// Clamp a point size into `[MIN, MAX]` (`FontSettings.swift`'s `clamp`).
pub fn clamp_px(v: f32) -> f32 {
    v.clamp(MIN_TERMINAL_FONT_PX, MAX_TERMINAL_FONT_PX)
}

/// Clamp a line-height multiplier into
/// `[MIN_TERMINAL_LINE_HEIGHT, MAX_TERMINAL_LINE_HEIGHT]`.
pub fn clamp_line_height(v: f32) -> f32 {
    v.clamp(MIN_TERMINAL_LINE_HEIGHT, MAX_TERMINAL_LINE_HEIGHT)
}

/// The padded cell height for a natural glyph height under a line-height
/// multiplier — the single source of truth the whole grid keys off. Rounded to
/// whole logical px **once** (never per-glyph) so every rect below computes from
/// the same integer height (no sub-row seams). At multiplier 1.0 this is
/// `glyph_h` unchanged (`round` of an already-whole value), so the classic grid
/// is bit-identical.
pub(crate) fn padded_cell_h(glyph_h: f32, line_height: f32) -> f32 {
    (glyph_h * line_height).round().max(1.0)
}

/// Re-derive a padded cell box from an existing box treated as the natural glyph
/// height (the [`MetricsMode::Fixed`] line-height path). Width is untouched.
fn apply_line_height(base: TerminalMetrics, line_height: f32) -> TerminalMetrics {
    let glyph_h = base.cell_h;
    TerminalMetrics::with_glyph_h(base.cell_w, padded_cell_h(glyph_h, line_height), glyph_h)
}

/// Resolve a font family from `chain`, tried in order, against the families the
/// OS reports available through GPUI's text system.
///
/// The availability check is membership in [`TextSystem::all_font_names`] (the
/// platform's installed family names + GPUI's fallback stack). The pure ordering
/// logic is [`pick_available`], unit-tested with a mock name set.
pub fn resolve_family(text_system: &Arc<TextSystem>, chain: &[SharedString]) -> SharedString {
    let available = text_system.all_font_names();
    pick_available(chain, &available)
}

/// The first family in `chain` present in `available`, else a guaranteed-present
/// monospace fallback (`Menlo`) rather than the unavailable candidate itself —
/// returning an unresolvable family here would have GPUI's `resolve_font`
/// substitute a *proportional* system font, breaking the terminal's fixed-pitch
/// grid. Pure — the testable core of [`resolve_family`].
fn pick_available(chain: &[SharedString], available: &[String]) -> SharedString {
    for family in chain {
        if available.iter().any(|name| name == family.as_ref()) {
            return family.clone();
        }
    }
    SharedString::from("Menlo")
}

/// The backing scale SwiftTerm's cell-width snap is reproduced at. Prod snaps
/// to the view's live `backingScaleFactor()`; this crate derives metrics at the
/// app level (no window in scope), so the snap is pinned to Retina's 2×. On a
/// 2× display the numbers are bit-identical to prod; on a 1× display prod would
/// snap to whole points where we snap to halves — a ≤0.5px-per-cell divergence
/// accepted for determinism (metrics must not change when a window migrates
/// displays, or a zoom round-trip would not reproduce earlier metrics exactly).
const METRICS_SNAP_SCALE: f32 = 2.0;

/// Derive the cell box for `family` at `px` through GPUI's text system —
/// mirroring SwiftTerm's `computeFontDimensions` (`AppleTerminalView.swift:339`)
/// so the grid pitch is bit-identical to prod on a Retina display:
///
/// * width — the advance of `M` (all advances are equal in a monospace, so this
///   is the cell pitch), taken through [`TextSystem::advance`], then snapped up
///   to the pixel grid at [`METRICS_SNAP_SCALE`] (`ceil(w·s)/s`) exactly as
///   SwiftTerm snaps to avoid sub-pixel seams between adjacent cells (prod
///   measures `W`; in a monospace every advance is equal, so `M` ≡ `W`);
/// * height — `ceil(ascent + |descent|)` ([`TextSystem::ascent`] +
///   [`TextSystem::descent`]; GPUI reports `descent` as a **negative** offset —
///   below the baseline is −y, see `gpui_macos::text_system` — hence the
///   `abs`). SwiftTerm computes `ceil(ascent + descent + leading)`; GPUI's
///   public text system does not expose `line_gap` (it lives on the private
///   `FontMetrics`), but every font in the shipped chain (SF Mono, JetBrains
///   Mono NL, Menlo, the Meslo family) carries zero leading, so the ceil'd box
///   matches prod's. The result is already whole logical px, so SwiftTerm's
///   backing-scale height snap is a no-op on it.
///
/// The un-ceil'd `ascent + |descent|` box (pre fix round r6) sat fractionally
/// under prod's (e.g. 16.40 vs 17.0 at MesloLGS NF 13pt), so the two grids
/// drifted ~0.6px per row and the then-bottom-anchored layout surfaced the
/// drift as a different sub-row remainder — a visibly different gap between the
/// tab bar and the first terminal row at the same window height. (The grid is
/// top-anchored now, which would park that drift at the bottom instead.)
///
/// Both are deterministic functions of `px`, so a zoom-out back to a prior size
/// reproduces the earlier metrics **exactly**.
pub fn cell_metrics(
    text_system: &Arc<TextSystem>,
    family: &SharedString,
    px_size: f32,
    line_height: f32,
) -> TerminalMetrics {
    let font = Font {
        family: family.clone(),
        features: FontFeatures::default(),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
        fallbacks: None,
    };
    let font_id = text_system.resolve_font(&font);
    let size = px(px_size);
    let advance = text_system
        .advance(font_id, size, 'M')
        .map(|s| f32::from(s.width))
        .unwrap_or(px_size * 0.6);
    let cell_w = (advance * METRICS_SNAP_SCALE).ceil() / METRICS_SNAP_SCALE;
    let ascent = f32::from(text_system.ascent(font_id, size));
    let descent = f32::from(text_system.descent(font_id, size));
    // The natural glyph box (today's cell height). The padded cell height is
    // `round(glyph_h * line_height)` — the extra leading is split half above /
    // half below the glyph at paint time (gpui's `paint_line` centers the run in
    // `cell_h`; the cursor is centered in `element.rs`).
    let glyph_h = (ascent + descent.abs()).ceil().max(1.0);
    let cell_h = padded_cell_h(glyph_h, line_height);
    TerminalMetrics::with_glyph_h(cell_w.max(1.0), cell_h, glyph_h)
}

/// Scale a cell box by `ratio` (used only by [`MetricsMode::Fixed`] zoom).
fn scale_metrics(base: TerminalMetrics, ratio: f32) -> TerminalMetrics {
    TerminalMetrics::new((base.cell_w * ratio).max(1.0), (base.cell_h * ratio).max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> SharedString {
        SharedString::from(v.to_string())
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn chain_prefers_the_first_available() {
        let chain = default_font_chain();
        // All present: SF Mono wins.
        assert_eq!(
            pick_available(&chain, &names(&["SF Mono", "JetBrains Mono NL", "Menlo", "Arial"])),
            s("SF Mono")
        );
    }

    #[test]
    fn chain_falls_through_in_order() {
        let chain = default_font_chain();
        // SF Mono absent: JetBrains Mono NL is next.
        assert_eq!(
            pick_available(&chain, &names(&["JetBrains Mono NL", "Menlo"])),
            s("JetBrains Mono NL")
        );
        // Both preferred absent: the system-mono fallback (Menlo).
        assert_eq!(pick_available(&chain, &names(&["Menlo", "Courier"])), s("Menlo"));
    }

    #[test]
    fn chain_falls_back_to_menlo_when_none_available() {
        // None of the chain is installed: fall back to Menlo (never panics, never
        // empty, never the unavailable candidate itself).
        let chain = default_font_chain();
        assert_eq!(pick_available(&chain, &names(&["Arial", "Courier"])), s("Menlo"));
    }

    #[test]
    fn single_unavailable_family_falls_back_to_menlo_not_raw_name() {
        // A single-entry chain (e.g. a user-selected or imported family) that
        // isn't installed must resolve to Menlo, not the unresolvable name
        // itself — returning the raw name here is exactly the proportional-
        // garble regression Fix A closes.
        assert_eq!(pick_available(&[s("Nope")], &names(&["Menlo", "Arial"])), s("Menlo"));
    }

    #[test]
    fn chain_is_exact_match_not_substring() {
        // "SF Mono Something" must NOT satisfy "SF Mono" (avoid a bold/condensed
        // variant masquerading as the family); fall through to Menlo.
        let chain = vec![s("SF Mono"), s("Menlo")];
        assert_eq!(
            pick_available(&chain, &names(&["SF Mono Bold", "Menlo"])),
            s("Menlo")
        );
    }

    #[test]
    fn clamp_bounds() {
        assert_eq!(clamp_px(13.0), 13.0);
        assert_eq!(clamp_px(4.0), MIN_TERMINAL_FONT_PX);
        assert_eq!(clamp_px(100.0), MAX_TERMINAL_FONT_PX);
        assert_eq!(clamp_px(MIN_TERMINAL_FONT_PX), MIN_TERMINAL_FONT_PX);
        assert_eq!(clamp_px(MAX_TERMINAL_FONT_PX), MAX_TERMINAL_FONT_PX);
    }

    #[test]
    fn fixed_metrics_scale_proportionally_from_base() {
        let base = TerminalMetrics::new(8.0, 16.0);
        // 13 → 26 px doubles the box; 13 → 6.5 halves it (each computed from the
        // base, so there is no accumulated drift).
        assert_eq!(scale_metrics(base, 2.0), TerminalMetrics::new(16.0, 32.0));
        assert_eq!(scale_metrics(base, 0.5), TerminalMetrics::new(4.0, 8.0));
    }

    #[test]
    fn clamp_line_height_bounds() {
        assert_eq!(clamp_line_height(1.3), 1.3);
        assert_eq!(clamp_line_height(0.5), MIN_TERMINAL_LINE_HEIGHT);
        assert_eq!(clamp_line_height(3.0), MAX_TERMINAL_LINE_HEIGHT);
        assert_eq!(clamp_line_height(MIN_TERMINAL_LINE_HEIGHT), 1.0);
        assert_eq!(clamp_line_height(MAX_TERMINAL_LINE_HEIGHT), 1.8);
    }

    #[test]
    fn padded_cell_h_rounds_once_and_is_identity_at_1x() {
        // A representative natural glyph height (SF Mono 13pt ≈ 16 logical px).
        let glyph_h = 16.0;
        // 1.0 is bit-identical to the classic grid (round of an already-whole
        // value): existing users pinned to 1.0 see no change.
        assert_eq!(padded_cell_h(glyph_h, 1.0), 16.0);
        // 1.3 → round(20.8) = 21; 1.8 → round(28.8) = 29. Rounded ONCE.
        assert_eq!(padded_cell_h(glyph_h, 1.3), 21.0);
        assert_eq!(padded_cell_h(glyph_h, 1.8), 29.0);
        // A fractional glyph height still yields a whole padded cell.
        assert_eq!(padded_cell_h(17.0, 1.3), 22.0); // round(22.1)
    }

    #[test]
    fn apply_line_height_pads_height_keeps_width_and_glyph_box() {
        // The Fixed-mode line-height path: the base box is the natural glyph box;
        // width is untouched, glyph_h is preserved for cursor centering, cell_h
        // is the padded height.
        let base = TerminalMetrics::new(8.0, 20.0);
        let m = apply_line_height(base, 1.5);
        assert_eq!(m.cell_w, 8.0, "width is never multiplied");
        assert_eq!(m.glyph_h, 20.0, "natural glyph height is preserved");
        assert_eq!(m.cell_h, 30.0, "cell height is round(20 * 1.5)");
        // 1.0 is a full no-op (glyph_h == cell_h, the classic single-height box).
        let same = apply_line_height(base, 1.0);
        assert_eq!(same, TerminalMetrics::new(8.0, 20.0));
    }
}
