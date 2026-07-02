# AA/gamma spike (rank-1) — RUNBOOK

Closes the re-opened AA/gamma gate with **rendered pixels**: does a
GPUI-native terminal on **current GPUI main** reproduce the text appearance of
Nice's shipping SwiftTerm renderer (`fontSmoothing=false`,
`NiceTerminalView.swift:216`), and how big is the visible gap the
bg-luminance patch must close?

**Everything here needs a real display and must be run from the MAIN session
(sandbox disabled). Subagents: build-only.**

## What's in this directory

| Path | What |
|------|------|
| `scene/gen_scene.py`, `scene/scene.bin` | The deterministic byte scene (60×16; cursor hidden; ASCII + thin strokes + box drawing + fg/bg pairs incl. true white-on-black / black-on-white; registration blocks for alignment). Both sides parse the SAME bytes. |
| `gpui-term-main/` | GPUI side. alacritty_terminal grid → GPUI paint (per-cell `shape_line().paint()` + `paint_quad`) on **pinned zed main `10b07951838e422722e34641f4a9c0bfec9037ff`** (2026-07-01). Readback = GPUI's first-party visual-test capture (`VisualTestAppContext::capture_screenshot` → `MetalRenderer::render_to_image`: production shaders into the layer's drawable, `get_bytes`, BGRA→RGBA). |
| `../swiftterm-fixture/` | SwiftTerm side. The fork's `TerminalView` + Metal renderer with Nice's EXACT shipping config; readback = `CAMetalLayer.nextDrawable` swizzle (stashes the presented drawable, forces `framebufferOnly=false` before the pool builds) + blit → PNG. |
| `diff-tool/` | `aa-diff`: translation-aligns two PNGs (coarse→fine gradient cross-correlation), reports per-channel max/mean delta, RMSE, %pixels>threshold, per-scene-row breakdown; writes amplified heatmap + aligned crops + report.json. (Rust, not python/PIL — this machine has no numpy/Pillow/uv.) |
| `bg-luminance-patch-plan.md`, `bg-luminance-draft.patch` | Deliverable 6: sized plan (~50 LOC / 6-7 files) + compile-untested draft diff of the real curve patch against the pinned rev. |

## 0. Builds (safe anywhere, no display)

```sh
cd spikes/phase0-poc/aa-gamma/gpui-term-main && cargo build          # ~6 min cold
cd spikes/phase0-poc/swiftterm-fixture       && swift build          # ~10 s
cd spikes/phase0-poc/aa-gamma/diff-tool      && cargo build          # ~2 s
# regenerate the scene only if you edit gen_scene.py:
cd spikes/phase0-poc/aa-gamma/scene && python3 gen_scene.py scene.bin
```

All three were verified green on 2026-07-01 (rustc 1.96.0, Swift 6.x).

> **PATCHED BUILD (since 2026-07-01, spike-1 closure):** `gpui-term-main`
> now builds against `../zed-main-patched/` — a local copy of the pinned rev
> (rsync of `~/.cargo/git/checkouts/zed-a70e2ad075855582/10b0795/`, minus
> `.git`) with the bg-luminance curve patch applied
> (`bg-luminance-applied.patch`; plan in `bg-luminance-patch-plan.md`). The
> binary also grew a `--curve appleApprox|identity` axis and two always-on
> paint fixes (inverse-video default-color inversion, procedural block
> elements) and writes `gpui-main-patched-*` filenames — the §1 matrix below
> documents the ORIGINAL baseline names; to reproduce those exactly, flip
> `gpui-term-main/Cargo.toml` back to the commented git deps and rebuild.
> The patched re-run lives in §3.
The SwiftTerm fixture depends on `/Users/nick/Projects/SwiftTerm` by absolute
path — READ-ONLY checkout, must stay on `phase0-txn-present @ 583551f`
(= Nice's project.yml pin `5f07dc6` + a docs commit + the OFF-by-default
transactional-present opt-in; default rendering identical to what Nice ships).

## 1. Run the scene matrix (display required, main session only)

Both windows briefly appear on screen; each binary self-exits after writing
its PNG + meta.json. **Run everything on the same (2×) display** — the
fixture uses the main screen's backingScaleFactor and the GPUI window opens at
(100,100) on the active display; the metas record `scale_factor` and the diff
is only valid when they match.

```sh
cd spikes/phase0-poc
OUT=aa-gamma/out
SCENE=aa-gamma/scene/scene.bin

# SwiftTerm fixture: theme × curve (fontSmoothing=false fixed = Nice parity)
FIX=swiftterm-fixture/.build/debug/swiftterm-fixture
"$FIX" --scene "$SCENE" --out "$OUT" --theme light --curve appleApprox
"$FIX" --scene "$SCENE" --out "$OUT" --theme light --curve identity
"$FIX" --scene "$SCENE" --out "$OUT" --theme dark  --curve appleApprox
"$FIX" --scene "$SCENE" --out "$OUT" --theme dark  --curve identity

# GPUI main: theme × AppleFontSmoothing (off = Nice-parity target,
# on = GPUI-main out-of-the-box fg-luminance dilation)
GPUI=aa-gamma/gpui-term-main/target/debug/gpui-term-main
"$GPUI" --scene "$SCENE" --out "$OUT" --theme light --smoothing off
"$GPUI" --scene "$SCENE" --out "$OUT" --theme light --smoothing on
"$GPUI" --scene "$SCENE" --out "$OUT" --theme dark  --smoothing off
"$GPUI" --scene "$SCENE" --out "$OUT" --theme dark  --smoothing on
```

Expected outputs in `aa-gamma/out/`:

```
swiftterm-{light,dark}-{appleApprox,identity}.png + .meta.json   (4 pairs)
gpui-main-{light,dark}-smoothing-{off,on}.png + .meta.json       (4 pairs)
```

Sanity checks before diffing (abort and investigate if any fail):

* every `.meta.json` has `"scale_factor": 2`;
* fixture metas: `cell_w_pt: 8.5, cell_h_pt: 16` and `drawable_w/h = 1020×512`;
* gpui metas: `advance_w_px ≈ 8.036` (proves "SF Mono" resolved — both sides
  load Terminal.app's bundled `SF-Mono-Regular.otf`/`SF-Mono-Bold.otf`; the
  gpui binary warns and falls back if the family didn't resolve, which would
  invalidate the comparison);
* fixture stderr printed a `drawables_seen` ≥ 1 count (in meta).

## 2. Diff the pairs that answer the gate

```sh
DIFF=aa-gamma/diff-tool/target/debug/aa-diff
D=aa-gamma/out/diff && mkdir -p "$D"

# (A) THE GATE — Nice's shipping look vs stock GPUI main at Nice parity:
"$DIFF" --ref "$OUT"/swiftterm-light-appleApprox.png --img "$OUT"/gpui-main-light-smoothing-off.png --out "$D"/A-light-ship-vs-gpui
"$DIFF" --ref "$OUT"/swiftterm-dark-appleApprox.png  --img "$OUT"/gpui-main-dark-smoothing-off.png  --out "$D"/A-dark-ship-vs-gpui

# (B) the ".identity analytically equivalent" claim, now in pixels:
"$DIFF" --ref "$OUT"/swiftterm-light-identity.png --img "$OUT"/gpui-main-light-smoothing-off.png --out "$D"/B-light-identity-vs-gpui
"$DIFF" --ref "$OUT"/swiftterm-dark-identity.png  --img "$OUT"/gpui-main-dark-smoothing-off.png  --out "$D"/B-dark-identity-vs-gpui

# (C) does GPUI main's new fg-luminance dilation approximate the curve?
"$DIFF" --ref "$OUT"/swiftterm-light-appleApprox.png --img "$OUT"/gpui-main-light-smoothing-on.png --out "$D"/C-light-ship-vs-gpui-dilated
"$DIFF" --ref "$OUT"/swiftterm-dark-appleApprox.png  --img "$OUT"/gpui-main-dark-smoothing-on.png  --out "$D"/C-dark-ship-vs-gpui-dilated

# (D) curve magnitude within SwiftTerm itself (context scale for A-C):
"$DIFF" --ref "$OUT"/swiftterm-light-appleApprox.png --img "$OUT"/swiftterm-light-identity.png --out "$D"/D-light-curve-magnitude
"$DIFF" --ref "$OUT"/swiftterm-dark-appleApprox.png  --img "$OUT"/swiftterm-dark-identity.png  --out "$D"/D-dark-curve-magnitude
```

Each run prints the metric table and writes
`*-heatmap.png` (|Δ|×8; red = over threshold), `*-ref.png`/`*-img.png`
(aligned crops for eyeballing), `*-report.json`.

### Reading the results

* `correlation` < 0.5 → alignment failed; treat metrics as garbage (check both
  PNGs opened at the same scale, same theme).
* Per-row table: **rows 13 (bold) and 14 (underline) are font-selection /
  decoration axes, not curve axes** — exclude them when judging the curve.
  Rows 6-8 (white-on-black, black-on-white, truecolor extremes) and 3 (thin
  strokes) are the money rows for the luminance curve.
* Expected shape of the answer:
  - **B (identity vs GPUI)** small residuals = the audit's "analytically
    equivalent" claim holds in pixels; residuals concentrated on glyph edges
    at |Δ| ≲ 1-2 quantization steps are placement-model noise (GPUI baked
    ¼-px-X variants + integer-Y vs SwiftTerm fractional bilinear), not
    curve error. Large mid-coverage deltas = real blend/raster divergence —
    gate stays open, escalate.
  - **A (shipping look vs GPUI)** ≈ **D (curve magnitude)** ≫ **B** — i.e. the
    visible gap is the curve and only the curve → the sized patch
    (bg-luminance-patch-plan.md, ~50 LOC) is exactly what closes it, and the
    "no text/atlas-core fork" finding stands.
  - **C** tells us whether upstream main's dilation already narrows A (it is
    fg-only, so dark-on-light rows should barely move — if C ≈ A on light
    theme, the upstream path does NOT substitute for the curve).
* Sub-pixel placement: integer alignment is the tool's limit; a systematic
  ~0.25-0.5 px horizontal smear in the heatmap on EVERY glyph is placement
  quantization, not AA — judge it visually in the aligned crops and note it
  separately in the report.

## 3. Patched re-run — closing the curve (display required, main session only)

Turns spike 1's "the gap IS the curve" into "the curve is closed". The
`gpui-term-main` binary is now the PATCHED build (see the §0 note): pinned
zed rev + `bg-luminance-applied.patch`, plus the two non-curve paint fixes
from `RESULTS-spike1-20260701.md` (inverse-video bar color, block elements
U+2580-259F as procedural pixel-aligned quads). Outputs are kept separate
from the originals: new filenames (`gpui-main-patched-…-{curve}-…`) AND a
separate out dir (`out/patched/`).

The SwiftTerm fixture PNGs in `out/` are REUSED as the reference — do not
re-run the fixture unless the display/scale changed since they were captured
(metas must still say `scale_factor: 2` on the display you use now; if the
machine/display differs, re-run the §1 fixture matrix first).

```sh
cd spikes/phase0-poc
OUT=aa-gamma/out
POUT=aa-gamma/out/patched && mkdir -p "$POUT"
SCENE=aa-gamma/scene/scene.bin
GPUI=aa-gamma/gpui-term-main/target/debug/gpui-term-main

# Patched matrix: theme x curve, smoothing off (= Nice parity) throughout.
"$GPUI" --scene "$SCENE" --out "$POUT" --theme light --smoothing off --curve appleApprox
"$GPUI" --scene "$SCENE" --out "$POUT" --theme dark  --smoothing off --curve appleApprox
"$GPUI" --scene "$SCENE" --out "$POUT" --theme light --smoothing off --curve identity
"$GPUI" --scene "$SCENE" --out "$POUT" --theme dark  --smoothing off --curve identity
```

Expected outputs in `aa-gamma/out/patched/`:

```
gpui-main-patched-{light,dark}-{appleApprox,identity}-smoothing-off.png + .meta.json
```

Sanity: each meta must have `"bg_luminance_patch": true`, the right
`"curve"`, `"scale_factor": 2`, `advance_w_px ≈ 8.036`.

```sh
DIFF=aa-gamma/diff-tool/target/debug/aa-diff
PD=aa-gamma/out/patched/diff && mkdir -p "$PD"

# (A') THE CLOSURE — Nice's shipping look vs PATCHED GPUI, curve on:
"$DIFF" --ref "$OUT"/swiftterm-light-appleApprox.png --img "$POUT"/gpui-main-patched-light-appleApprox-smoothing-off.png --out "$PD"/Ap-light-ship-vs-gpui-patched
"$DIFF" --ref "$OUT"/swiftterm-dark-appleApprox.png  --img "$POUT"/gpui-main-patched-dark-appleApprox-smoothing-off.png  --out "$PD"/Ap-dark-ship-vs-gpui-patched

# (B') residual-fix check — identity vs identity (curve off on both sides;
# isolates the two paint fixes + proves the sentinel keeps identity intact):
"$DIFF" --ref "$OUT"/swiftterm-light-identity.png --img "$POUT"/gpui-main-patched-light-identity-smoothing-off.png --out "$PD"/Bp-light-identity-vs-gpui-patched
"$DIFF" --ref "$OUT"/swiftterm-dark-identity.png  --img "$POUT"/gpui-main-patched-dark-identity-smoothing-off.png  --out "$PD"/Bp-dark-identity-vs-gpui-patched
```

### Reading the patched results (baselines = spike-1 numbers)

* **A′ vs old A (6.97/5.64 mean, 9.9% >thr):** success = A′ collapses to at
  most old-B territory (3.98/3.51 mean, 4.5% >thr) and visibly below it,
  since the paint fixes also remove old B's rows-0/4/12/15 residuals. The
  A-heatmap's "every glyph lights up" signature must be GONE.
* **B′ vs old B:** rows 12 (inverse video, was 36.67% >thr) and 0/15 (full
  blocks) → ≈0; row 4 (was 13.2%) → only the box-drawing LINE glyphs'
  remainder (┌─╔═ etc. stay font-rendered on the GPUI side vs the fork's
  procedural strokes — out of scope of the two fixes, judge separately).
  Everywhere else B′ must match B (±placement noise) — if B′ is WORSE than B
  on any text row, the −1 sentinel regressed the identity path: stop and
  debug before trusting A′.
* **If A′ ≈ A:** the shader curve isn't engaging — check the meta says
  `curve: appleApprox` and that the run went through the patched binary
  (`bg_luminance_patch: true`), then suspect the TextRun.background_color →
  paint_glyph plumbing.
* Row 13 (bold) stays excluded (font-selection axis). Rows 3/6/7/8 remain
  the money rows for curve parity.

## 4. Cleanup / gotchas

* The gpui binary writes the AppleFontSmoothing pref into its own app domain:
  `~/Library/Preferences/gpui-term-main.plist`. Remove with
  `defaults delete gpui-term-main` when done (harmless if left).
* The fixture registers Terminal.app's SF Mono `.otf`s **process-scoped**
  (no system font pollution). Nothing to clean.
* Neither binary touches a pty, `/Applications/Nice.app`, or `Nice Dev.app`.
* Known harness caveats (fold into the spike report):
  - GPUI colors round-trip u8 → Hsla → float RGBA (public-API constraint;
    same path Zed uses). Worst case ±1/255 per channel — below the default
    threshold 8.
  - Fixture uses `NSFont(name: "SFMono-Regular", 13)` after process
    registration = first hit of Nice's shipping chain. On THIS machine bare
    Nice actually falls through to `.AppleSystemUIFontMonospaced` (SF Mono
    isn't installed system-wide) — same glyph outlines and identical cell
    metrics (probed: advW 8.0361, cell 8.5×16 both), but note it.
  - GPUI window includes real titlebar chrome in the capture; the grid is
    inset 10pt from the canvas origin and the diff aligns/crops to the
    fixture drawable (which IS the grid), so chrome never enters the metrics.
* If `gpui-term-main` panics at `capture_screenshot`, re-run once (first-frame
  scheduling with the TestDispatcher can be racy on a cold start); if it
  persists, insert a second `window.refresh()` + `run_until_parked()` round.

## 5. What was deliberately NOT run by the authoring agents

Per the hard constraint, the original authoring agent launched no GUI: builds
of all three targets and the aa-diff self-test (synthetic offset images,
headless CoreGraphics) were its only executed verification. The §1/§2 matrix
was then run live from the main session on 2026-07-01
(`RESULTS-spike1-20260701.md`). The patched-build agent (§3) likewise ran
build-only: the patched binary compiles (metallib validated the new shader at
build time) but the §3 matrix + diffs have NOT been run — that is the main
session's remaining step.
