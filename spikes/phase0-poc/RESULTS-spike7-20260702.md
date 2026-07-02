# Spike 7 results — real-trace workload, 2026-07-02

Ran §13 spike 7: captured two REAL claude TUI sessions' pty bytes with
timestamps (`pty-capture`, nicetrace v1), replayed them timing-faithfully
into both bins, plus max-rate drain tests. This closes the audit's
workload-realism blind spot ("never validated against a byte of real
Claude output"). Release builds, 60 Hz panel.

## Verdict: workload-realism blind spot CLOSED — real loads are ~3 orders of magnitude lighter than the synthetic stream; Path B paces a clean 60 fps under real traffic while **Path A (real bridge) keeps its ~31 ms p95 tail even at real-trace pacing**. The A-vs-B FPS differential does NOT soften at realistic loads.

## The two captured traces (committed: `traces/`)

| Trace | Session | Records | Bytes | Native duration | Character |
|---|---|---|---|---|---|
| t1 `claude-session.nicetrace` | Fable-effort tetris prompt | 1091 | 98,563 | 299.6 s | mostly thinking-phase spinner — realistic "waiting on Claude" |
| t2 `claude-stream.nicetrace` | Haiku, fizzbuzz in 20 languages | 139 | 27,799 | 192.0 s | incl. ~40 s of dense streaming |

Headline realism fact: a ~5-minute real session emits **under 100 KB**.
The synthetic workload feeds 500 KB/s — per streaming window over 30 s
that is 12.19 MB vs t1's 3,971 B (the 7w3s loop run below), **~3,000×
heavier**. (The earlier hand-computed "~1300× vs 6.3 MB" compared against
the 18-s spike-8 fill; the same-duration, log-verified ratio is even
stronger.) **Every synthetic number in this program is a conservative
upper bound on real Claude traffic.**

## Timing-faithful paced replay (t2, the streaming-heavy trace)

| Bin | Frame p50/p95/p99 or p50/p95 (ms) | Notes | Mem steady (MiB) |
|---|---|---|---|
| **Path B `gpui-term`** | **16.66 / 17.63 / 17.68** | paint-closure p50 0.407 ms; shape-cache hit ~100% | 109.7 |
| **Path A `phase0-poc` (txn, REAL bridge)** | term **18.00 / 30.75** (cliffs 8156); composite 18.41 / 30.75 | `present_now()` wall p50 **2.16** / max 19.42 ms (n=9263) — real `currentDrawable` stalls | 153.9 / 161.2 peak |
| Path A, first attempt (stub bridge — methodology lesson, see below) | term 16.66 / 17.86; composite 16.67 / 17.86 | `present_now()` p50 0.00 / max 0.06 ms — no real Metal layer | 87.5 |

- **Path B: clean 60 fps under real pacing, measured on the real single
  Metal stack.** Memory 109.7 MiB ≈ Nice's ~111–114 MiB under-load
  baseline — at realistic loads the §11 "Path B inherits ~baseline
  memory" claim now holds on a release build.
- **Path A: the ~31 ms p95 tail is PRESENT under real-trace pacing,
  numerically identical to its synthetic tail** (30.75 vs spike 3's
  31.07; p50 18.00 vs 18.34 — A misses refresh pacing at p50 too, vs B's
  16.66). The tail is **structural to the dual-Metal co-present path and
  independent of byte rate**: this trace averages ~145 B/s, ~3,400×
  lighter than the synthetic stream, and the tail did not move —
  consistent with spike 3's txn refutation, because the RAF-driven
  co-present pays the coordination cost **per frame, not per byte**
  (`present_now` p50 2.16 ms × 9,263 presents). Even a mostly-idle real
  claude session pays it.

**The A-vs-B FPS differential does NOT soften at realistic loads.** The
2026-07-02 first-pass inference ("the tail should vanish at light loads")
is **reversed by measurement**: B 16.66/17.63 vs A 18.00/30.75 under the
identical real trace. Path B's structural advantage holds at realistic
single-window loads, not just under stress/multi-window.

### Methodology lesson — the stub-bridge first attempt

The first Path A replay silently ran the **stub** bridge: the real-bridge
selection is a *build-time* flag (`NICE_POC_REAL_BRIDGE=1` consumed by
`build.rs`), and a prebuilt binary bypassed it — the run "worked" and
printed plausible numbers (`metal_renderer=stub/unavailable`,
`present_now` max 0.06 ms, 87.5 MiB). Caught by log audit during results
folding; the re-run's banner confirms the fix ("linked REAL SwiftTerm
Metal bridge", `metal=true`, `metal_renderer=REAL`). Lesson: **always
verify the bridge banner in the log header before trusting a Path A
number.** Both runs' numbers are kept in the table above for contrast —
the stub row is, incidentally, a clean measurement of A's *non-Metal*
overhead (parse + chrome + composite), which is not where the tail lives.

## Max-rate drain (parse throughput)

| Run | Bytes | Feeder wall to quiescent | Throughput |
|---|---|---|---|
| B drain t1 (live) | 98,563 | 0 ms | 250.4 MB/s |
| B drain t2 (live) | 27,799 | 0 ms | 138.9 MB/s |
| headless parse-half (step-1 sanity) | t1 / t2 | — | 132.7 / 124.3 MB/s |

Both traces drain **instantly** (sub-ms; the MB/s figures are noise-level
at these sizes — read them as ">100 MB/s, far above any realistic pty
rate"). The headless step-1 numbers are from the runner's sanity pass and
were not preserved in the log set; the live feeder numbers above are
log-verified. Path A's real-bridge drain also completed instantly (feed
complete → quiescent within a 1.5 s window) and shows the same tail
signature in its few frames: term 17.28/32.49, composite 18.38/30.93,
`present_now` p50 6.21 / max 15.75 ms (n=49), 137.3 MiB.

## 7w3s real-trace loop (t1 looped, 3 streaming + 4 bg heartbeat, 30 s)

Frame p50/p95/p99 **16.67 / 17.94 / 18.71** ms (per-window streaming p95
17.94–18.09) vs the synthetic 7w3s headline 16.66 / 19.85 / 21.15 —
real bytes are lighter (3,971 B fed per streaming window vs 12.19 MB
synthetic) and the tail shrinks accordingly. Memory 356 MiB steady.
Total Metal draws 5566 vs 3×~1796 streaming frames — background windows
presenting on demand, consistent with the spike-6/spike-8 un-retraction.

## Method notes

- Replays are RAF-driven — identical semantics (and reducer/CSV) to the
  synthetic FPS harness, so numbers are directly comparable.
- Drain mode starves the render-side early-exit detector after the burst:
  the t2 B-drain sat 43.7 s with 2 composited frames before the quiescent
  exit fired (the deadline watchdog guarantees exit regardless). Drain-run
  frame percentiles are meaningless; the payload is the feeder-side wall
  number.
- Capture lessons (for the next trace): exit the captured claude session
  with `/exit\r` or raw 0x04 — sending ESC at an idle claude prompt opens
  the previous-message UI and swallows subsequently typed input;
  Fable-effort prompts think for minutes before streaming a byte (that IS
  the realistic profile, but budget capture time for it).

## Evidence

- Traces committed: `traces/claude-session.nicetrace`,
  `traces/claude-stream.nicetrace` (386863b).
- CSVs committed: `gpui-term-trace.csv` (paced t2),
  `gpui-term-trace-7w3s.csv` (loop), `gpui-term-trace-drain.csv` — holds
  the **t1** drain (the t2 drain wrote the same filename first and was
  overwritten; its numbers are from the run summary, scratchpad
  `s7-B-drain-t2.log`).
- Path A runs: log summaries only — real-bridge runs
  `s7-A-paced-t2-real.log`, `s7-A-drain-t2-real.log` (banner:
  `metal_renderer=REAL`); superseded stub-bridge first attempts
  `s7-A-paced-t2.log`, `s7-A-drain-t2.log` (kept for the methodology
  lesson). All in the session scratchpad.
