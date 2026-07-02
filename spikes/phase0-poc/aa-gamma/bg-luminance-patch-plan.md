# bg-luminance curve patch — sizing plan (spike-1 deliverable 6)

Static analysis of the **pinned GPUI main** sources
(`zed-industries/zed @ 10b07951838e422722e34641f4a9c0bfec9037ff`, checkout:
`~/.cargo/git/checkouts/zed-a70e2ad075855582/10b0795/`). This sizes the real,
multi-file patch the audit said the "~14-line shader patch" actually is: making
GPUI's monochrome text path reproduce SwiftTerm's per-cell, bg-luminance-aware
composition curve (`terminal_text_fragment_gray`: Kitty `text_composition_strategy
1.7 30` ≡ `.appleApprox` — `clamp(mix(cov, pow(cov, 1/1.7), mixFactor) * 1.30)`,
`mixFactor = clamp((1 − L_fg + L_bg) · 0.5, 0, 1)`).

A compile-UNTESTED draft of the whole patch is in `bg-luminance-draft.patch`
(drafted from source reading; apply/fix when the fork lane is actually chosen).

## What main already gives us (cheaper than at 0.2.2)

1. **The Metal-side `MonochromeSprite` struct is cbindgen-GENERATED.**
   `crates/gpui_macos/build.rs` runs cbindgen over `crates/gpui/src/scene.rs`
   into `$OUT_DIR/scene.h`, which `shaders.metal` includes. Adding a field to
   the Rust struct auto-propagates to the Metal struct — no hand-mirroring on
   macOS (the audit's cost model assumed a manual mirror).
2. **`TextRun.background_color` already flows to paint time.** `shape_line`
   folds it into `DecorationRun` (text_system.rs:397-435), and `paint_line`
   iterates decoration runs right where it picks the glyph color
   (line.rs:334+, `color = style_run.color`). The per-cell bg is therefore
   *already at the right place* in the public API — a terminal view just sets
   `TextRun.background_color`; only the last hop (into `paint_glyph`) is missing.
3. **Main already ships a text-gamma seam on other backends:**
   `get_gamma_correction_ratios` (platform.rs:981, MIT-licensed from MS
   Terminal) is wired in gpui_wgpu (wgpu_renderer.rs:1888) and gpui_windows'
   DirectX renderer — upstream is already gamma-aware off-mac, which makes an
   upstream-first PR to gpui_macos plausible in shape.
4. **`SubpixelSprite` is dead on macOS** (`recommended_rendering_mode` returns
   `Grayscale`, gpui_macos/text_system.rs:210-216), so only the monochrome
   pipeline needs the curve.
5. Main's **fg-luminance smoothing dilation** (text_system.rs:218-253) is a
   *rasterizer*-side stem-darkening keyed on AppleFontSmoothing; Nice ships the
   SwiftTerm equivalent OFF (`fontSmoothing=false`), so the plan assumes
   dilation stays 0 and the curve does all the work — same division of labor as
   the SwiftTerm fork.

## File-by-file patch plan

| # | File | Change | ~LOC | Churn risk (rebase) |
|---|------|--------|-----:|---------------------|
| 1 | `crates/gpui/src/scene.rs` | `MonochromeSprite`: add `bg_luminance: f32` + `pad2: u32` (keep C layout 8-byte aligned; sentinel `bg_luminance < 0` = curve off). Verify against regenerated `scene.h`. | 3 | low (struct stable) |
| 2 | `crates/gpui/src/window.rs` | `paint_glyph(...)`: new `background: Option<Hsla>` param; compute Rec-709 luminance (display-gamma, matching SwiftTerm `TextCompositionCurve.luminance`); fill the new field at the `MonochromeSprite` insertion (:3910). The SVG mono-sprite site (:4057) passes the −1 sentinel. | 12 | **medium** — actively churned file; signature change touches every internal caller |
| 3 | `crates/gpui/src/text_system/line.rs` | `paint_line`: pass `style_run.background_color` through to `paint_glyph` (the value is already in scope in the decoration-run loop). | 4 | medium |
| 4 | `crates/gpui_macos/src/shaders.metal` | `MonochromeSpriteVertexOutput`/`FragmentInput`: add flat `mix_factor` (computed in the vertex from `sprite.color` luminance + `sprite.bg_luminance`); fragment applies `cov → clamp(mix(cov, pow(cov, GAMMA_INV), mix_factor) * CONTRAST, 0, 1)` when `mix_factor ≥ 0`. Constants baked for `.appleApprox` in the spike; see "uniform lane" below. | 25 | low (audit: zero subpixel churn in gpui_macos shaders; struct def auto-generated) |
| 5 | `crates/gpui_wgpu/src/shaders.wgsl` | passive struct-mirror update (`bg_luminance`, `pad2`) to keep the shared `Scene` layout in sync (field unused in shader). | 2 | low |
| 6 | `crates/gpui_windows/src/shaders.hlsl` | same passive mirror. | 2 | low |
| 7 | (callers) other in-tree `paint_glyph` users | mechanical `None` at each call site (grep shows the text path in line.rs is the only glyph caller; editor/terminal go through it). | ~2 | low |

**Total: ~50 LOC across 6-7 files.** Two files are passive layout mirrors; the
real logic is ~40 LOC in scene.rs / window.rs / line.rs / shaders.metal.
This confirms the audit's correction (multi-file, not "~14 lines in one
shader") while bounding it: it is an **additive** patch — no change to
text_system/open_type/metal_atlas rasterization, atlas keying (the curve is
per-fragment, so the same atlas texel is reusable across cells with different
bg — exactly why SwiftTerm couldn't bake it either), or blend state.

### Curve-parameter lane (spike vs product)

* **Spike/draft:** bake `GAMMA_INV = 1/1.7`, `CONTRAST = 1.30` as shader
  constants (Nice ships exactly `.appleApprox`; zero uniform plumbing).
* **Product:** add a `text_composition: (f32, f32)` uniform on the mac
  renderer (one more `metal_renderer.rs` binding, ~15 LOC) or reuse the
  `get_gamma_correction_ratios` pattern, exposed via a `WindowOption` /
  text-system setting. Needed only if the value must be user-tunable.

### Zed-compat / upstream-first

With the −1 sentinel default, **every existing GPUI caller renders
byte-identically** (curve off) — the patch is upstreamable as an opt-in
(matching audit G12's recommendation: GPUI core already ships gamma machinery
on wgpu/windows backends, and Zed invited external contributions). Upstream
lane: PR this as "per-run background-aware text composition curve (kitty
`text_composition_strategy`)"; fall back to carrying it as a fork patch only
if declined. As a fork patch, the rebase surface is dominated by window.rs
(#2) — the shader and scene.rs edits are in low-churn files.

### Behavior notes / risks

* `paint_glyph` currently derives `dilation` from fg color when
  AppleFontSmoothing ≠ 0 (main's new default-on path). Shipping config should
  set AppleFontSmoothing=0 for the app domain (or add a public toggle —
  a 1-line `WindowOption` is a separate micro-patch) so dilation and the curve
  don't double-thicken. The spike's `--smoothing on` axis measures exactly
  this interaction.
* Underline/strikethrough go through `paint_underline` (Underline primitive),
  not the curve — same as SwiftTerm (curve applies to glyph coverage only). ✓
* Emoji/polychrome sprites unaffected (separate pipeline) — same as SwiftTerm. ✓
* Layout change to `MonochromeSprite` invalidates nothing at runtime (no
  serialization), but all four backends' struct mirrors must move together —
  the two passive mirrors are the whole cross-platform cost.
* cbindgen regeneration is automatic (build.rs) — but `runtime_shaders`
  feature builds stitch shaders differently; verify both build modes.
