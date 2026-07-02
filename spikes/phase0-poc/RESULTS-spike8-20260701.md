# Spike 8 results — multi-window / multi-session, 2026-07-01

Ran §13 spike 8 live on `gpui-term` (the single-stack GPUI-native terminal):
7 windows / 7 sessions, same machine, 60 Hz panel. This was one of the audit's
"zero data anywhere" blind spots — tear-off is the flagship feature, and the
original spike parsed the pty inside `render()` on the main thread.

## Verdict: PASS-leaning — 60 fps p50 held at 7 windows, parse-off-main scales

## Precondition fixed first: pump() moved out of render()

- Per-session feeder thread parses into `Arc<FairMutex<Term>>` off-main at 5 ms
  ticks; `render()` snapshots the grid under a short lock.
- Background (non-key) windows are demand-driven: dirty→notify wakes a ~10 Hz
  poller instead of a continuous RAF.

## Numbers (7 windows = 3 streaming + 4 idle-bg at 1 line/s unless stated; 18 s)

| Config | Frame p50/p95/p99 (ms) | Mem steady/peak (MiB) | Notes |
|---|---|---|---|
| Debug 7w3s | 16.63 / 26.72 / 28.59 | 467.5 / 472.5 | |
| **RELEASE 7w3s (headline)** | **16.66 / 19.85 / 21.15** | 428.0 / 429.8 | 1075 frames/18 s; per-window streaming p95 19.85–20.18 |
| RELEASE 7w3s + `NICE_POC_BG_BPS=500000` | 16.67 / 21.42 / 24.55 | 624.5 / 626.7 | all 7 sessions at 500 KB/s ≈ 3.5 MB/s aggregate parse; bg windows 173 demand-frames each, 6.4 MB fed each |

## Reading

- **60 fps p50 held with 3 concurrent streams + 4 full-rate background
  parsers**; p95 ≈ 1.2–1.3 frames even with all 7 sessions parsing at full
  rate. Parse-off-main scales.
- Reference point: Path A's dual stack measured 18.3/31.2 p50/p95 for a
  **single** window (§10 `txn`, re-confirmed by spike 3).

## Caveats

- The harness "cliff" counter is 120 Hz-calibrated — ignore it; read the
  percentiles.
- **Memory scales with scrollback fill** (~60–90 MiB/session at ~6.4 MB fed).
  This is explicitly handed to spike 9 (scrollback/reflow) and spike 10 (atlas
  pressure) as an open question — it is NOT a spike-8 failure, but it is not
  yet a sizing for N long-lived real sessions either.

## Evidence

Raw CSV: `spikes/phase0-poc/gpui-term-multi-7w3s.csv`.

---

## Correction (2026-07-02): bg-window "demand frames" counted scene rebuilds, not presents

A gpui-0.2.2 finding from the spike-4b/5 keystroke run
(`baseline/RESULTS-keylat-20260701.md`) invalidates one counter above:
`cx.notify()` alone never reaches `MetalRenderer::draw` while a window's
CVDisplayLink is stopped — a demand-driven present needs an explicit layer
kick. The background windows' "demand frames" counters (173/19) counted
**scene rebuilds** (element renders), not Metal presents. Given that
finding, the background windows very likely never presented during these
runs (visually frozen; scene rebuilt and dropped).

- **Stands UNCHANGED** (for what it actually measured): the headline
  streaming-window frame pacing is RAF-driven — a real present path — and
  the parse-off-main scaling conclusion (all 7 sessions at 500 KB/s, p95
  21.42) is about main-thread load, which is unaffected.
- **RETRACTED** until re-run with the present kick: "background windows
  redraw on demand at notify cadence." The kick now exists (interactive
  mode, `NICE_POC_INTERACTIVE=1`) but has not been wired into the
  multi-window background path.
