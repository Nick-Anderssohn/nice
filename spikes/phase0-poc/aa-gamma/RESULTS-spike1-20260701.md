# Spike 1 results — AA/gamma live pixel diff, 2026-07-01

Ran the rank-1 gate live (8-PNG matrix + 8 diffs) per `RUNBOOK.md`. GPUI side on
pinned zed main `10b07951838e422722e34641f4a9c0bfec9037ff`; SwiftTerm side on
the fork `phase0-txn-present @ 583551f` (= Nice's pin, default rendering). Both
parse the same `scene/scene.bin`, same SF Mono (advance 8.0361 both sides),
scale 2 both sides, cell 8.5×16.

## Verdict: AA/gamma gate CLOSED again — on rendered pixels — no text/atlas-core fork

The visible gap between a GPUI-native terminal and shipping Nice/SwiftTerm is
**the luminance curve, plus two narrow paint-fill cases**. None of it forks
`text_system`/`open_type`/`metal_atlas`. The audit's re-opened gate (it had been
closed by source-reading with zero rendered pixels) is closed again, this time
with pixels.

## The four comparisons (mean|Δ| R channel / % pixels any-channel >8)

| Cut | light | dark | reading |
|---|---|---|---|
| **A** shipping (appleApprox) vs GPUI(off) | 6.97 / 9.9% | 5.64 / 9.9% | the real, visible gap |
| **B** identity (curve OFF) vs GPUI(off) | 3.98 / 4.5% | 3.51 / 4.5% | gap with the curve removed |
| **C** shipping vs GPUI(smoothing ON) | 5.75 / 9.2% | 6.40 / 10.5% | main's fg dilation as curve substitute? |
| **D** curve magnitude within SwiftTerm | 3.23 / 6.4% | 2.25 / 6.4% | how big the curve is, as a yardstick |

Alignment valid on every cut (correlation 0.89–0.99; the −20,−21 px shift on
A/B/C is GPUI's titlebar+inset, correctly recovered; D is same-app, 0,0 shift).

## What the numbers + heatmaps say

1. **The gap is the curve.** `A ≈ B + D`: A−B ≈ 3.0 (light) / 2.1 (dark) mean,
   which equals D (3.2 / 2.3) — the shipping-vs-GPUI gap *minus* the identity-vs-
   GPUI gap IS SwiftTerm's curve magnitude. Visually: the **A heatmap lights up
   every glyph** (the curve shifts AA coverage on all text); the **B heatmap goes
   mostly black** (curve off → they match).
2. **At identity, the curve-critical rows match well.** Thin strokes (`il1|`) row:
   0.04 mean / 0.02% >thr — essentially perfect. white-on-black / black-on-white /
   truecolor extremes: <3.5 mean / <2.1% >thr. Residual is **single-pixel glyph-edge
   placement quantization** (GPUI's baked ¼-px-X variants + integer-Y vs SwiftTerm
   fractional bilinear), i.e. a placement-model difference, **not** a blend/raster
   divergence. (Integer-only alignment also inflates edge deltas, so these
   magnitudes *overstate* the true difference — strengthening the placement reading.)
3. **GPUI main's built-in fg dilation does NOT substitute for the curve.** C ≈ A
   (dark C is even slightly worse). The upstream `AppleFontSmoothing` dilation is
   fg-only; it does not reproduce SwiftTerm's bg-luminance curve. So the bg-luminance
   patch is genuinely needed — we can't get parity by flipping the upstream default.
4. **Two narrow non-curve residuals at identity** (real, fixable in paint, not fork):
   - **Inverse-video background bar** (row 12, 37% >thr): GPUI's `paint_quad`
     background extent/color for inverse video doesn't match SwiftTerm's bar.
   - **Shade / box-drawing blocks** (row 4, 13% >thr): shaded-block fill/dither
     differs. Both are `paint_quad` fill/coverage issues, not glyph AA.
   - Bold row (13) excluded — font-selection axis, not curve.

## What closes it (already sized, NOT yet applied)

`bg-luminance-patch-plan.md`: **~50 LOC across 6–7 files** (scene.rs field →
cbindgen-generated Metal struct, `paint_glyph` luminance plumbing, line.rs
pass-through, ~25-line shader curve; wgsl/hlsl passive mirrors). A −1 sentinel
keeps existing GPUI output byte-identical → upstreamable opt-in. Draft (compile-
untested) at `bg-luminance-draft.patch`.

## Owed to fully close the loop (small)
- **Apply the patch, re-run, confirm A-with-patch collapses toward B.** The patch
  is sized/drafted but not compiled/applied — this is the one remaining step to
  turn "the gap is the curve" into "the curve is closed."
- Targeted `paint_quad` fix + re-check for inverse-video bg and shade blocks.
- (Optional) sub-pixel alignment in aa-diff to separate placement from AA exactly.

## Bottom line
Rank-1 gate resolves **for Path B**: SwiftTerm-class text fidelity is reachable on
GPUI's public paint API with a small additive bg-luminance patch and minor
paint-fill fixes — **no fork of the GPU text/atlas core.** The lone could-flip
item the report hung on (a bespoke blend/shader need) did **not** materialize.
