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
    /// Cell box for `family` at `px` (derived or pinned per `mode`).
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
        let metrics = cell_metrics(&ts, &family, px);
        Self {
            chain,
            family,
            px,
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
                cell_metrics(&ts, &self.family, self.px)
            }
            MetricsMode::Fixed {
                base_px,
                base_metrics,
            } => scale_metrics(base_metrics, self.px / base_px),
        };
    }
}

/// Clamp a point size into `[MIN, MAX]` (`FontSettings.swift`'s `clamp`).
pub fn clamp_px(v: f32) -> f32 {
    v.clamp(MIN_TERMINAL_FONT_PX, MAX_TERMINAL_FONT_PX)
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

/// The first family in `chain` present in `available`, else the last candidate
/// (GPUI's own `resolve_font` then substitutes a system font for it, so painting
/// never fails). Pure — the testable core of [`resolve_family`].
fn pick_available(chain: &[SharedString], available: &[String]) -> SharedString {
    for family in chain {
        if available.iter().any(|name| name == family.as_ref()) {
            return family.clone();
        }
    }
    chain
        .last()
        .cloned()
        .unwrap_or_else(|| SharedString::from("Menlo"))
}

/// Derive the cell box for `family` at `px` through GPUI's text system.
///
/// * width — the advance of `M` (all advances are equal in a monospace, so this
///   is the cell pitch), taken through [`TextSystem::advance`];
/// * height — the font's glyph line box `ascent + |descent|`
///   ([`TextSystem::ascent`] + [`TextSystem::descent`]). GPUI reports `descent`
///   as a **negative** offset (below the baseline is −y — see
///   `gpui_macos::text_system`, which negates the font-kit descent), so the box
///   height is `ascent − descent`, i.e. `ascent + descent.abs()`. GPUI's public
///   text system does not expose `line_gap` (it lives on the private
///   `FontMetrics`), so this is the tight ascent+descent box with no extra
///   leading — enough that adjacent rows' glyphs never overlap; both terms scale
///   linearly with `px`, so the box grows/shrinks monotonically under zoom.
///
/// Both are raw logical px (a deterministic function of `px`), so a zoom-out back
/// to a prior size reproduces the earlier metrics **exactly**.
pub fn cell_metrics(
    text_system: &Arc<TextSystem>,
    family: &SharedString,
    px_size: f32,
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
    let cell_w = text_system
        .advance(font_id, size, 'M')
        .map(|s| f32::from(s.width))
        .unwrap_or(px_size * 0.6);
    let ascent = f32::from(text_system.ascent(font_id, size));
    let descent = f32::from(text_system.descent(font_id, size));
    let cell_h = ascent + descent.abs();
    TerminalMetrics::new(cell_w.max(1.0), cell_h.max(1.0))
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
    fn chain_returns_last_candidate_when_none_available() {
        // None of the chain is installed: return the last candidate anyway (GPUI
        // then substitutes a system font). Never panics, never empty.
        let chain = default_font_chain();
        assert_eq!(pick_available(&chain, &names(&["Arial", "Courier"])), s("Menlo"));
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
}
