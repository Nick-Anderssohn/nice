# Spike 4 results — Direct Nice (SwiftTerm) baseline, 2026-07-01

Ran the §13 spike-4 present-timing baseline against **installed Nice Dev 0.29.0**
(SwiftTerm fork rev on disk `583551f`, signpost code present since `3c45fdc`,
confirmed ancestor of the shipped `cceae86`). Fixture: `/tmp/nice-fixture.bin`
(4 MB, deterministic seed=42), paced ~500 000 B/s for ~20 s into one Nice Dev
pane. Machine: MacBook Air, macOS 26.5.1, **60 Hz panel** (no ProMotion).

## Headline: two blocking corrections to the baseline method + a reframed gate

### 1. `capture-present.sh` is BROKEN — `log stream` never sees the signposts
`baseline/capture-present.sh` reduces `log stream --predicate 'subsystem ==
"org.tirania.SwiftTerm" && category == "MetalProfile"'`. That returns **0
samples**. `os_signpost` events emitted via `OSLog`/`OSSignposter` are NOT
delivered as Unified-Logging *messages* — they are kdebug signpost records that
only Instruments/`xctrace` (or the signpost Instrument) capture. The whole
RUN.md §5 present-FPS path yields zeros as written.

**Fix (used here):** capture with xctrace, then reduce the exported interval table.
```sh
xcrun xctrace record --template 'Logging' --attach <pid> --time-limit 24s \
  --output nice-dev.trace           # 'Logging' template captures os-signpost tables
xcrun xctrace export --input nice-dev.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost-interval"]' \
  > signpost-intervals.xml
python3 reduce-signposts.py signpost-intervals.xml Metal.Draw   # cadence
python3 draw-durations.py  signpost-intervals.xml               # per-draw cost
```
(both reducers live in scratchpad; port into `baseline/` when folding.)
The `--attach <pid>` needs the running Nice Dev pid; env `SWIFTTERM_PROFILE=1`
must be set on launch (it was; confirmed in the trace's captured environment).

### 2. SwiftTerm is DEMAND-DRIVEN, so inter-draw cadence is NOT a frame rate
`MacTerminalView.swift:360-361`: `mtkView.isPaused = true;
mtkView.enableSetNeedsDisplay = true`. The Metal view draws only when content is
invalidated, coalesced to the AppKit display cycle — it does **not** run a
continuous 60/120 Hz display link. Therefore the inter-`Metal.Draw` interval
measures *how often the terminal chose to redraw*, not display cadence.

## Numbers (paced 500 KB/s, ~12.5 s of active draw, N=370 draws)

| Signpost | count | dur p50 | dur p95 | dur p99 | dur max | sum(ms) |
|---|---|---|---|---|---|---|
| **Metal.Draw** (whole frame) | 370 | **1.19 ms** | **2.41 ms** | 2.74 ms | 3.83 ms | 367 |
| Metal.BuildDrawData (atlas/vertex) | 370 | 1.10 | 2.23 | 2.53 | 3.70 | 329 |
| Metal.Encode | 370 | 0.053 | 0.110 | 0.130 | 0.138 | 21.7 |
| Metal.Commit | 370 | 0.013 | 0.024 | 0.027 | 0.036 | 5.4 |
| Metal.CurrentDrawable | 370 | 0.019 | 0.040 | 0.058 | 0.068 | 8.0 |
| Parser.Parse | 4727 | 0.036 | 0.091 | 0.118 | 0.159 | 200 |

- **Inter-draw cadence** (reduce-signposts): p50 55.8 ms, p95 66.3 ms, ~18 draws/s,
  187 "cliffs" — all meaningless as a frame rate (demand-driven; see above).
- **Per-draw cost is the real number: p50 1.19 ms, p95 2.41 ms.** ~2% duty cycle
  (1.2 ms of work in a 55 ms window). Nice's SwiftTerm renderer is FAST and has
  enormous headroom under this load; it simply redraws less often on purpose.
- `BuildDrawData` is ~92% of the draw cost. Encode+Commit+drawable acquisition
  are together <0.1 ms. Parser is fully decoupled (fires ~13×/draw) and negligible.

## Why this reframes the FPS gate (feeds the audit's measurement concern)

The §12 "FPS gate PASS / Path B locks 60 fps, p50 16.666" number is **B's RAF
cadence**, i.e. B pumps+renders every display refresh (60 Hz here) whether or not
content changed. That was never validly compared to a SwiftTerm baseline because
no working SwiftTerm baseline existed (capture-present.sh = 0 samples). This is
the first real SwiftTerm present measurement, and it shows the two sides measure
different things:

- **Path B (gpui_term):** continuous RAF → 60 *presents*/s; per-frame *cost*
  never isolated in the CSV.
- **Nice / SwiftTerm:** demand-driven → ~18 *draws*/s, each costing **1.2 ms**.

So "B locks 60 fps" is not a quality win over Nice — it means B redraws every
refresh. If anything B's continuous-RAF model does ~3× the draws for the same
visible result, which is an **energy mark against B** (→ spike 6) and moves the
real comparison to: (a) per-draw GPU/CPU **cost** — Nice = 1.2 ms p50, B TBD;
(b) **keystroke→glyph latency** (spike 5, the responsiveness metric); (c) energy
(spike 6). Cadence-vs-cadence is a category error.

## Status of spike 4's cells
- Term-present **cost** baseline: **DONE** — Nice draw p50 1.19 ms / p95 2.41 ms.
- Term-present **cadence**: captured (55.8/66.3 ms) but flagged non-comparable.
- Memory idle / under-load: NOT captured this run (sample-mem.sh untouched) — cheap
  follow-up, no blocker.
- Keystroke-latency pty-echo: **still deferred** — needs `CGEventPost` +
  Accessibility TCC grant for this session's host (AXIsProcessTrusted() == false).
  This is the one item that needs Nick.

## Cleanup / prod-safety
- Launched Nice Dev myself; the graceful quit hit Nice Dev's "Quit NICE?" 4-session
  confirmation dialog, which I could not click (no click tool; AX not granted), so
  I terminated the pid I launched. Prod Nice (58741) untouched throughout.
