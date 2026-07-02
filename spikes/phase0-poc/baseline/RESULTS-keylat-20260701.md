# Spikes 4b/5 results — keystroke→present latency, Nice Dev vs gpui-term, 2026-07-01/02

Both halves of the keystroke-latency gate are now **MEASURED**: the **Nice Dev
half** (§13 spike 4's deferred keystroke half, which is also spike 5's
reference number) ran 2026-07-01; the **gpui-term (Path B) half** ran late
2026-07-01 → early 07-02 with the identical harness on the same machine.
**The spike-5 gate (Path B within ~1 frame of Nice Dev) = PASS as defined**,
with a mandatory distribution-shape caveat (vsync quantization — see below).

## Comparison: both targets, same harness

| Target | p50 | p95 | p99 | min | max | matched |
|---|---|---|---|---|---|---|
| Nice Dev 0.29.0 (baseline) | 1.96 ms | 6.56 ms | 8.47 ms | 0.80 ms | 9.06 ms | 500/500, 0 dropped |
| gpui-term release (Path B) | 12.35 ms | 20.12 ms | 20.78 ms | 3.76 ms | 21.22 ms | 500/500, 0 dropped |
| **Δ (B − baseline)** | **+10.39 ms** | **+13.56 ms** | **+12.31 ms** | | | |

All three percentile deltas sit within one 60 Hz frame (16.7 ms) →
**spike-5 gate PASS as defined.**

## Nice Dev half (2026-07-01): keyDown → present-submit p50 1.96 ms / p95 6.56 ms

| p50 | p95 | p99 | min | max |
|---|---|---|---|---|
| **1.96 ms** | **6.56 ms** | 8.47 ms | 0.80 ms | 9.06 ms |

Matched 500/500, dropped 0, clock-join spread 22.2 µs. Histogram: 258 samples
in 0–2 ms, 139 in 2–4, 62 in 4–6, 34 in 6–8, 7 in 8–10.

### Method (new harness: `baseline/keystroke-harness/`)

- `keyinject` posts 500+5 real keyDown/keyUp pairs via **`CGEventPostToPid`**
  (never a global HID tap) at 100 ms gap (single-in-flight) into installed
  **Nice Dev 0.29.0** launched with `SWIFTTERM_PROFILE=1`; the measurement
  pane is a plain zsh running `cat` (kernel canonical-mode echo).
- Concurrent `xctrace record --template Logging --all-processes`, 65 s. No
  sudo needed for `--all-processes` on this machine; the Accessibility TCC
  grant (the item spike 4 was blocked on) is now in place for this host.
- **In-trace join**: the injector emits a KeyPost signpost (signpostID=seq),
  joined against `Metal.Draw` intervals in the same trace; the latency edge is
  the interval **END** (CPU present-submit).
- Semantics measured: keyDown → pty write → `cat` echo → parse →
  demand-driven MTKView draw → present-submit. Cursor-blink decoy draws are a
  ≈0.4% mis-join risk — not a percentile mover.

## gpui-term half (2026-07-02): p50 12.35 ms / p95 20.12 ms — gate PASS

| p50 | p95 | p99 | min | max |
|---|---|---|---|---|
| **12.35 ms** | **20.12 ms** | 20.78 ms | 3.76 ms | 21.22 ms |

Matched 500/500, dropped 0, clock-join spread 96.3 µs. Histogram: a roughly
**uniform band across 4–20 ms** — each 2-ms bin from 4 to 20 holds 47–67
samples; 3 samples in 2–4 ms.

### Method (identical harness, Path B target)

- Same `keyinject` (500+5 posts @ 100 ms via `CGEventPostToPid`), same
  `xctrace record --template Logging --all-processes` (62 s), same in-trace
  KeyPost↔Draw join, edge = interval end.
- Target: **`gpui-term` release** with `NICE_POC_INTERACTIVE=1` — one window,
  `/bin/cat` behind a real pty (kernel canonical-mode echo), no RAF, no
  cursor timers, damage-gated demand present via notify + `setNeedsDisplay`
  kick (`NICE_POC_DAMAGE_ONLY=1` auto-set).
- App self-audit: 506 Metal draws = 505 keys + the window-open present —
  **presents are 1:1 with echoes** (no decoy draws on this side).

### MANDATORY caveat: the 4–20 ms band is vsync quantization, not fixed overhead

- The uniform 4–20 ms band is **vsync quantization**, not a constant added
  cost: the demand path presents via Core Animation's vsync-aligned display
  phase (notify + `setNeedsDisplay` → `displayLayer:` → transactional
  present), floor ~4 ms. SwiftTerm submits ~2 ms after the echo (immediate
  draw, unquantized).
- Both halves measure **CPU present-submit, NOT glass**. Glass latency adds
  compositor alignment to BOTH targets, so the on-glass gap is likely
  smaller than the submit gap.
- A production Path B implementation has headroom to shave the band (e.g.
  an immediate `displayIfNeeded`-style kick, or an input-active
  display-link present); as measured it passes anyway.

### Two gpui-0.2.2 findings discovered en route (now fixed in the vendored patch)

1. **`cx.notify()` alone NEVER reaches `MetalRenderer::draw` when the
   CVDisplayLink is stopped** — fully-occluded windows freeze; the first
   live attempt produced 505 scene rebuilds and ZERO Metal presents.
   Demand-driven presents need an explicit layer kick.
2. **Stock gpui presents the UNCHANGED scene at 60 Hz for 1 s after every
   input** (anti-underclock keepalive, `window.rs` `last_input_timestamp`) —
   this would have frame-phase-corrupted the measurement; disabled via the
   new `NICE_POC_DAMAGE_ONLY=1` knob in the vendored patch.

Finding 1 also retro-corrects spike 8's background-window "demand frame"
counters — see the dated correction in `../RESULTS-spike8-20260701.md`.

## Status of the gate

- **Nice Dev baseline: DONE** (p50 1.96 / p95 6.56 / p99 8.47 ms).
- **gpui-term half: DONE** (p50 12.35 / p95 20.12 / p99 20.78 ms).
- **Spike-5 gate ("Path B within ~1 frame of Nice Dev"): PASS as defined**,
  all percentile deltas < 16.7 ms — with the vsync-quantization shape caveat
  above recorded as mandatory context.

## Incident (methodology honesty)

The first injection attempt typed into Nice Dev's restored-session prefill
line (`NICE_PREFILL_COMMAND`) and executed `claude --resume <id>cat` — bogus
id, no data loss, killed. The measurement pane for the real run was a fresh
plain-shell window. Lesson recorded: **keystroke runs must use a fresh window,
never a restored pane.**

## Evidence

- Nice Dev half: `baseline/keystroke-harness/out/keyinject-nicedev.csv`,
  `baseline/keystroke-harness/out/keylat-nicedev-samples.csv`.
- gpui-term half: `baseline/keystroke-harness/out/keyinject-gpui.csv`,
  `baseline/keystroke-harness/out/keylat-gpui-samples.csv`; code in commit
  `14ce6a7`.
- Traces themselves live in the session scratchpad, not committed.
