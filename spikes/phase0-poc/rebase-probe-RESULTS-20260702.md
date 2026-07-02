# Rebase-probe RESULTS — fork-rebase cost measurement (Phase-0 §13 spike 11, part 2)

**Date:** 2026-07-02 · **Machine:** local (aarch64-apple-darwin, Rust 1.95.0 from zed's `rust-toolchain.toml`)
**Question:** what does a zed rev bump actually cost while carrying the bg-luminance patch
(`aa-gamma/bg-luminance-applied.patch`, 65+/7− across 6 files)?
**Method:** depth-1 clone of zed `main` HEAD → apply patch → classify hunks → `cargo build -p gpui -p gpui_macos`
→ grep-audit gpui-term-main's API touchpoints on HEAD → churn analysis (GitHub API + blobless 3-month deepen).
Probe workspace: scratchpad `rebase-probe/zed-head` (build `target/` deleted after success; clone kept, 125 MB).

## 1. Revs and drift

| | |
|---|---|
| Pin (production) | `10b07951838e422722e34641f4a9c0bfec9037ff` — committed 2026-07-01T22:52Z |
| HEAD (probed) | `2882636c06923e58d83865ecc370bd0d8199d738` — committed 2026-07-02T19:13Z ("Fix hanging updates after system sleep (#60301)") |
| Distance | **28 commits, ~20 hours** (GitHub compare API: ahead 28 / behind 0; pin is an ancestor of HEAD) |
| Files changed upstream in that window | 94 total; under `crates/gpui*` only **`crates/gpui/src/app.rs`** |
| Overlap with our 6 patch-site files | **none** |

⚠️ **Representativeness caveat:** the pin was cut yesterday, so this probe measured a ~1-day bump, not the
~monthly bump the plan budgets. zed main moves at **136 commits/week** (measured, last 7 days), so a monthly
bump ≈ **550–600 commits** — ~20× this window. §5 extrapolates from measured churn at the exact patch
anchors over 3 months (2,177 commits) rather than from this one lucky window.

## 2. Patch application on HEAD

`git apply --check` (strict, zero fuzz tolerance) and BSD `patch -p1 --dry-run` both passed with no messages.

| File | Hunks | Clean | Fuzzy/offset | Conflict |
|---|---|---|---|---|
| crates/gpui/src/scene.rs | 1 | 1 | 0 | 0 |
| crates/gpui/src/window.rs | 3 | 3 | 0 | 0 |
| crates/gpui/src/text_system/line.rs | 3 | 3 | 0 | 0 |
| crates/gpui_macos/src/shaders.metal | 4 | 4 | 0 | 0 |
| crates/gpui_wgpu/src/shaders.wgsl | 1 | 1 | 0 | 0 |
| crates/gpui_windows/src/shaders.hlsl | 1 | 1 | 0 | 0 |
| **Total** | **13** | **13** | **0** | **0** |

After applying, all 6 files are **byte-identical** to the reference patched tree
(`aa-gamma/zed-main-patched/`) — the 28 upstream commits did not touch any patch-site file.
Conflict-resolution effort: **zero**. Wall time for clone+apply+verify: ~3 min (clone dominates).

## 3. Build proof on HEAD (patch applied)

```
cargo build -p gpui -p gpui_macos    # gpui_macos hosts shaders.metal + metal_renderer
Finished `dev` profile in 40.8s wall (207.8s user)  →  exit 0, no warnings at patch sites
```

- `gpui_macos/build.rs` compiles `shaders.metal` at build time (non-`runtime_shaders` path); the produced
  `shaders.air`/`shaders.metallib` artifacts confirm the **patched shader compiles** against HEAD's toolchain.
- **API drift that hit our patch sites: none.** Fixes required: **none.** Specifically re-verified on HEAD:
  - `paint_glyph` still has exactly **one caller** (`text_system/line.rs:538`, the one our patch edits) — the
    signature change (added `background: Option<Hsla>` param) breaks nothing else.
  - `MonochromeSprite` still has exactly the **two construction sites** our patch covers (window.rs:3921, :4070).
- gpui_wgpu/gpui_windows shader edits are text-only mirrors of the struct layout; they don't build on macOS
  and were not build-tested here (same as the original spike).

## 4. App-side API audit — gpui-term-main touchpoints on HEAD

Definitive bound first: `diff -rq` of pristine pin (`~/.cargo/git/checkouts/zed-a70e2ad075855582/10b0795/`)
vs HEAD across every crate gpui-term-main consumes (`gpui`, `gpui_platform`, `gpui_macos`, `gpui_macros`,
`gpui_shared_string`, `gpui_util`): the **only upstream change is `gpui/src/app.rs`** — the
`on_system_wake` refactor (moved from `Application::on_system_wake(&self, F) -> &Self` to
`App::on_system_wake(&self, F) -> Subscription`, observers now a `SubscriberSet`). gpui-term-main does not
use it. `gpui_platform` is byte-identical to the pin.

Spot-verified signatures on HEAD (all unchanged):

| Touchpoint | HEAD status |
|---|---|
| `WindowTextSystem::shape_line(SharedString, Pixels, &[TextRun], Option<Pixels>)` | unchanged (text_system.rs:397) |
| `Window::paint_quad(PaintQuad)` / `fill()` | unchanged (window.rs:3738) |
| `Window::render_to_image() -> Result<RgbaImage>` (test-support) | unchanged (window.rs:2260) |
| `VisualTestAppContext::new(Rc<dyn Platform>)` / `capture_screenshot` | unchanged (app/visual_test_context.rs:45, :384) |
| `Window::handle_input(&FocusHandle, impl InputHandler, &App)` | unchanged (window.rs:4355) |
| platform `InputHandler` trait (platform.rs): all 11 methods incl. `bounds_for_range(Range<usize>, &mut Window, &mut App) -> Option<Bounds<Pixels>>` | unchanged |
| `prefers_ime_for_printable_keys` default-false hook | unchanged (platform.rs:1473) |
| `ElementInputHandler` blanket impl (the reason we implement the trait directly) | still present (input.rs) |
| `TextRun { len, font, color, background_color, underline, strikethrough }` | unchanged (text_system.rs:987) |
| `gpui_platform::application()` / `current_platform(headless: bool)` | unchanged (gpui_platform.rs:13, :36) |
| `UTF16Selection`, `KeyDownEvent`, `WindowBounds`, `black`, `canvas`, `px/rgb/point/size` | all present |

**Moved/renamed: nothing** in the consumed surface. gpui-term-main would build against HEAD with only a
`rev = "2882636c…"` bump (not rewired here per constraints).

## 5. What a *monthly* bump costs — grounded extrapolation

Measured churn at the exact patch anchors (GitHub commits API per path; blobless `--shallow-since=2026-04-01`
deepen, 2,177 commits ≈ 3 months, then `git log -G` at the anchor regions; the shallow-boundary graft commit
excluded):

| Patch-site file | commits last month | last 3 months | hits at our anchor region (3 mo) |
|---|---|---|---|
| gpui/src/window.rs | 6 | 31 | **1** — `paint_glyph` touched only by 7d42f276 "Pixel snapping (#54728)", the very PR our pin was chosen to include |
| gpui/src/text_system/line.rs | 0 | 6 | 2 adjacent (SharedString `split_at` refactor #54583; underline/strikethrough fix #50934) |
| gpui/src/scene.rs | 0 | 2 | 0 at `MonochromeSprite` |
| gpui_macos/src/shaders.metal | 0 | 1 | **0** at the monochrome sprite stages |
| gpui_wgpu/src/shaders.wgsl | 0 | 2 | 0 at the struct mirror |
| gpui_windows/src/shaders.hlsl | 0 | 2 | 0 at the struct mirror |

Reading: 5 of 6 patch sites are near-frozen; the shader hot path we modify hasn't changed in ≥3 months.
`window.rs` churns (~10 commits/mo) but our hunks live inside `paint_glyph`, a leaf paint function that
changed once in 3 months. `line.rs` is the likeliest fuzz source (our 3 small hunks thread `background`
through the paint loop, and that loop does get refactored occasionally).

**Estimated cost per monthly bump** (task breakdown, grounded in what this probe actually did):

| Step | Typical month | Bad month (~1 in 2–3 at current churn) |
|---|---|---|
| Fetch/checkout new rev + `git apply` | 5 min (clean, as measured) | 30–90 min: re-anchor 1–3 hunks in window.rs/line.rs, same semantics |
| `cargo build -p gpui -p gpui_macos` + fix drift | 5 min (41 s build, zero fixes, as measured) | 30–60 min if `paint_glyph`/paint-loop signature moved (single-caller blast radius) |
| Re-run AA parity harness (diff-tool) + eyeball | 30 min | 30–60 min |
| **Total** | **~0.5–1 h** | **~2–4 h** |

The structural reasons this stays cheap: the patch is additive (only 2 context-sensitive line *changes*:
the `paint_glyph` signature and `color.a *= sample.a`), it has exactly one in-tree caller to keep in sync,
and its shader hunks sit in a file that essentially never changes. The main standing risk is a deliberate
upstream rework of glyph paint (like #54728 was) — that is a ~2–4 h re-anchor, not a rewrite, and it happens
on the order of once a quarter, not once a month.

## 6. Verdict

**The ~65-LOC patch survives rev bumps cheaply — patch-carry risk LOW.** This probe's window was
unrepresentatively short (28 commits; everything applied clean and built first try), but the 3-month churn
data says the same thing at 20× the window: budget **~1 h/month typical, ~4 h worst observed-churn case**,
consistent with the plan's ~monthly rev-bump chore. Revisit only if upstream announces a glyph-pipeline or
shader-uniform rework (watch `crates/gpui_macos/src/shaders.metal` and `paint_glyph` in release notes/PRs).

### Unrepresentativeness notes (honest accounting)
- Pin→HEAD drift was ~20 h, not a month; §5's monthly numbers are extrapolation from measured file/anchor
  churn, not a second measured bump.
- Build was dev-profile with a warm cargo registry cache (40.8 s); a cold cache adds one-time download time.
- The wgsl/hlsl mirrors were not compile-checked (non-mac targets); layout drift there would surface only
  on a future cross-platform build.
- `-G` anchor-churn counts exclude the shallow-graft boundary commit (`a212304e`), which `git log -G`
  falsely reports as touching everything.
