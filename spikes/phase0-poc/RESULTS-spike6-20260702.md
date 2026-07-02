# Spike 6 results — release per-frame cost + energy, 2026-07-02

Ran §13 spike 6 live on `gpui-term`: release build, 60 Hz panel, 60 s per
state (30 s for the two variants), single window unless noted. New harness
instrumentation (see README §"2026-07-02 spike prep"): render busy-cost
stamps (snapshot/build/paint), `MetalRenderer::draw` CPU wall, optional
MTLCommandBuffer GPU timestamps (`NICE_POC_GPU_TS=1`), shape-cache hit
counting, and a no-sudo `proc_pid_rusage` CPU/wakeups/energy proxy.

## Verdict: PASS — streaming per-frame total ~2.5 ms ⇒ 120 Hz headroom exists; idle is 0.6% of one core

## The five states (all release, Built-in Retina 60 Hz)

| State | Frame p50/p95 (ms) | paint-closure p50/p95 (ms) | draw CPU p50/p95 (ms) | GPU p50/p95 (ms) | CPU (one core) | wakeups/s | Mem steady (MiB) |
|---|---|---|---|---|---|---|---|
| idle (no feed, no RAF; 60 s) | n/a — 2 RAF frames, 57 draws | 0.839 (n=2) | 0.102 / 0.194 | — | **0.6%** | 40.8 | 47.0 |
| dot (RAF chrome dot; 60 s) | 16.66 / 17.63 | 0.277 / 0.464 | 0.128 / 0.196 | — | 5.6% | 40.9 | 91.4 |
| **streaming + GPU_TS (60 s)** | 16.67 / 17.35 | **1.542 / 2.233** | **0.076 / 0.179** | **0.871 / 0.936** | 14.8% | 95.0 | 136.4 |
| 7w3s + GPU_TS (30 s) | 16.67 / 19.50 | 1.040 / 1.468 | 0.043 / 0.069 (n=5560) | 0.867 / 0.955 | 30.6% | 284.9 | 422.3 |
| styles (bold/italic on; 30 s) | 16.67 / 17.11 | 1.677 / 2.403 | 0.073 / 0.170 | — | 15.6% | 99.9 | 137.1 |

Shape-cache (LineLayoutCache) hit rates: streaming 39.9%, styles 38.1%,
7w3s 41.3%, dot ~100% (static scene).

## The 120 Hz answer (the question §13 posed without ProMotion hardware)

Streaming per-frame total = paint-closure 1.54 + draw CPU 0.08 + GPU 0.87
≈ **2.5 ms p50** (≈3.3 ms at p95; snapshot+build add ~0.1 ms) — well under
the 8.3 ms 120 Hz budget. **120 Hz headroom exists** on this workload, with
~3× margin at p95. (Energy at 120 Hz would roughly double the draw-rate-
proportional part; see the proxy caveat below.)

## Honest comparison against Nice's Metal.Draw 1.19/2.41 ms

gpui's `MetalRenderer::draw` CPU (0.076/0.179) excludes scene build and
glyph shaping — those happen in the element's paint closure during `render()`.
The honest analog to Nice's Metal.Draw (which includes glyph-run assembly)
is the **paint-closure**: 1.54/2.23 vs Nice's 1.19/2.41 — comparable, same
order, slightly higher p50 and lower p95.

## Idle state findings

- **0.6% of one core over 60 s**, 40.8 pkg-idle wakeups/s, 47.0 MiB steady.
- 57 Metal draws over 60 s against only 2 RAF frames — a **~1/s periodic
  present source that remains unexplained** (not the workload, not RAF;
  candidates: AppKit occlusion/backing updates). Worth identifying before
  trusting idle-cost projections for N idle sessions, but the magnitude is
  negligible (draw CPU p50 0.102 ms × 1/s).
- This run initially **hung forever**: with the app fully idle (and napped),
  macOS App Nap defers coalescable dispatch timers indefinitely, so the
  deadline timer never fired. Fixed with `harness::watchdog` — a dedicated
  OS thread that dispatches to the main queue + `CFRunLoopWakeUp` at the
  deadline (headless-proven via a parked-main-thread self-test,
  `NICE_POC_WATCHDOG_SELFTEST=1`). Any deadline/exit logic in a gpui app
  needs this. See the runbook's "Hang fix" section.

## Background-present verification (feeds the spike-8 un-retraction)

The 7w3s + GPU_TS run counts **5560 total Metal draws** vs 3 × 1795
streaming RAF frames = 5385, leaving **~175 non-streaming presents** — the
4 background windows presenting at ≈heartbeat cadence (their scene-rebuild
counters read 31–32 each, plus 7 window-open presents and a handful of
AppKit-driven ones). This is live confirmation that the 2026-07-02
demand-present kick works in the multi-window background path — the claim
retracted in `RESULTS-spike8-20260701.md` is un-retracted there with this
run as evidence.

## Energy-proxy caveat

`ri_billed_energy` is not consistent across states: idle 135.0 mJ = stream
135.0 mJ < dot 157.6 mJ, despite CPU being 0.6% / 14.8% / 5.6% respectively.
**CPU% and wakeups/s are the reliable columns; do not read the mJ line as
absolute power.** Absolute mW needs the optional `sudo powermetrics` pass
(3 states × 55 s) — parked for Nick; the runbook documents the exact
commands.

## Evidence

- CSVs (committed, 386863b): `gpui-term-energy-idle.csv`,
  `gpui-term-energy-dot.csv`, `gpui-term-multi-7w3s-gputs.csv`.
- The streaming+GPU_TS run wrote `gpui-term-gpui-native-single-stack.csv`
  but the styles run (same filename) overwrote it before commit — the
  committed CSV holds the **styles** run (header `mode=none`, 30 s,
  137 MiB); the streaming run's numbers are from its run summary
  (scratchpad `s6-stream-gputs.log`).
