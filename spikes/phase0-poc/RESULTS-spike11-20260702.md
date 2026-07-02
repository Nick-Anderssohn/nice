# Spike 11 results — production-pin re-run + fork-rebase cost + tractability A/B staging, 2026-07-02

Ran §13 spike 11: re-run the headline numbers on a pinned production zed
rev, measure the fork-rebase (patch-carry) cost, and stage the #3 Claude
Swift-vs-Rust A/B tractability spike. The stack under test is the actual
production candidate: **pinned zed main `10b0795` + the spike-1
bg-luminance patch** (`aa-gamma/zed-main-patched/`) — not the spike-only
gpui 0.2.2.

## Verdict: parts 1+2 PASS — the headline numbers reproduce on the production pin (keystroke gate re-verified with margin; latency band HALVED vs 0.2.2) and patch-carry risk is LOW. Part 3 is design-staged, execution gated on Nick — the hard-gate premise stays unmeasured until it runs.

## Part 1 — headline re-run on the pin (✅ RUN)

Method: new `headline` bin in `aa-gamma/gpui-term-main/` that reuses
`spikes/phase0-poc/src/harness.rs` **verbatim** via `#[path]` — same mach
clock, same seed-42 synthetic workload, same reducers, so the comparison
against the 0.2.2 numbers is apples-to-apples. One additive zed-side hook
was needed (recorded as `gpui-term-main/zed-main-headline-hook.patch`);
none of the 6 bg-luminance files was touched. Signposts:
`dev.nickanderssohn.gpui-term-main`, category `present`, name `Draw`.

### Streaming FPS (release, 20 s, window frontmost+focused)

| Stack | Frame p50/p95/p99 (ms) | auto-cliffs (1.5×p50) | paint-closure p50/p95 | draw CPU p50/p95 | CPU (one core) | Mem steady (MiB) |
|---|---|---|---|---|---|---|
| **zed-main pin + patches** | **16.67 / 17.95 / 18.60** (max 34.15) | 1 | 1.755 / 2.482 | 0.081 / 0.170 | 17.2% | 136.3 |
| 0.2.2 reference (spike 6, 60 s) | 16.67 / 17.35 / 17.61 | 1 | 1.542 / 2.233 | 0.076 / 0.179 | 14.8% | 136.4 |
| Nice Metal.Draw reference | — | — | (analog) | 1.19 / 2.41 | — | — |

**The pin matches the 0.2.2 headline within noise** — same locked 60 fps
p50, p95 +0.6 ms, draw CPU and memory equal; the small paint/CPU uptick
is within run-to-run spread for a 20 s vs 60 s window. No production-rev
regression.

### Keystroke-to-present latency (identical spike-4/5 harness: 500 keys @100 ms, `CGEventPostToPid`, in-trace xctrace signpost join; clock spread 24.8 µs)

| Target | p50 / p95 / p99 (ms) | min / max | Matched |
|---|---|---|---|
| **zed-main pin + patches** | **6.22 / 10.81 / 11.56** | 2.07 / 11.73 | 500/500, 0 dropped |
| gpui 0.2.2 (spike 5) | 12.35 / 20.12 / 20.78 | ~4 / ~20 | 500/500 |
| Nice Dev (spike 4 baseline) | 1.96 / 6.56 / 8.47 | — | 500/500 |

- **The vsync band HALVED on main: 2–12 ms vs 0.2.2's 4–20 ms** — main's
  `displayLayer` present path is faster than 0.2.2's demand path.
- Deltas vs Nice Dev: **+4.26 / +4.25 / +3.09 ms** — deep inside the
  spike-5 "~1 frame (16.7 ms)" gate. **The keystroke gate is re-verified
  on the production stack, with margin.**
- Demand-present integrity held: 507 Metal draws for 505 key echoes (the
  remainder = window-open presents) — presents 1:1 with echoes, no RAF,
  no cursor/blink timer.
- (Percentile convention note: an independent linear-interp recompute of
  the samples CSV gives 6.22/10.78/11.47 — agrees with the harness
  reducer's nearest-rank figures above within convention.)

### Upstream findings on the pin (worth recording)

- (a) 0.2.2's 1 s post-input keepalive (which frame-phase-corrupted the
  spike-5 measurement until patched out) became a **rate-gated
  `InputRateTracker`** on main — it un-gates only at ≥60 input events/s,
  so **no damage-only patch is needed at human typing rates**.
- (b) main **frame-caps inactive/non-key windows to ~33.3 ms**
  (`gpui/src/window.rs` `min_frame_interval`), plus Serious/Critical
  thermal caps to ~16.7 ms — FPS-methodology note: the measured window
  must be frontmost AND focused.
- (c) `cx.notify()` still never presents while the CVDisplayLink is
  stopped — **the `setNeedsDisplay` kick remains load-bearing on main**
  (ported into the headline bin).

Evidence: `aa-gamma/gpui-term-main/gpui-term-main-headline.csv`;
`baseline/keystroke-harness/out/{keyinject-main.csv,keylat-main-samples.csv}`;
logs `headline-stream.log` / `headline-interactive.log` (session
scratchpad).

## Part 2 — fork-rebase cost (✅ RUN)

Full ledger: `spikes/phase0-poc/rebase-probe-RESULTS-20260702.md`.
Summary: pin `10b0795` → live HEAD `2882636c` (28 commits / ~20 h of
drift): **13/13 patch hunks apply clean, zero conflicts**; `cargo build
-p gpui -p gpui_macos` clean in 41 s with zero fixes; **zero API drift**
across ~30 app-side touchpoints (only the unused `on_system_wake`
moved). Anchor churn over 3 months of history: the monochrome shader
stages changed **0 times in 2,177 commits**; `paint_glyph` once.
Estimate: **0.5–1 h typical monthly rev bump, 2–4 h in a bad month —
patch-carry risk LOW.** Honest caveat (from the ledger): the measured
window was 1 day; the monthly figures are churn-grounded extrapolation,
not a measured month.

## Part 3 — Claude Swift-vs-Rust A/B tractability (◐ DESIGN STAGED, execution gated on Nick)

Protocol: `notes/tractability-ab-design-20260702.md`. Design: T1
stream-activity badge (primary, n=2/arm) + T2 bell-flash replication
(n=1/arm), 3 blind judges per artifact, pre-registered decision rule —
Swift ≥ Rust ⇒ the D1 premium is unsupported and §11 gets re-scored;
weak/unreplicated Rust win ⇒ the gap shrinks to ~4-vs-5; decisive
replicated Rust win ⇒ supported, with one commitment-grade follow-up owed
(a Swift replication inside Xcode 26.3's agent integration, since both
arms run in Claude Code CLI — a known Swift handicap). Budget: ~4–5.5M
tokens + ~1–1.5 h of Nick's GUI verification. **Until this runs, the
founding "Claude weak at Swift, strong at Rust" hard-gate premise remains
UNMEASURED** (§13 gap #3).
