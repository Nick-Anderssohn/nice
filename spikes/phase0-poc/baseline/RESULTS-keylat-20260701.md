# Spikes 4b/5 results (PARTIAL: baseline half) — keystroke→present latency, Nice Dev, 2026-07-01

Measured the **Nice Dev half** of the keystroke-latency gate (§13 spike 4's
deferred keystroke half, which is also spike 5's reference number). The
gpui-term half is **PENDING** — an interactive damage-gated echo mode is being
built now. The gate (Path B within ~1 frame of Nice Dev) **stays OPEN** until
that half runs.

## Headline: Nice Dev keyDown → present-submit p50 1.96 ms / p95 6.56 ms

| p50 | p95 | p99 | min | max |
|---|---|---|---|---|
| **1.96 ms** | **6.56 ms** | 8.47 ms | 0.80 ms | 9.06 ms |

Matched 500/500, dropped 0, clock-join spread 22.2 µs. Histogram: 258 samples
in 0–2 ms, 139 in 2–4, 62 in 4–6, 34 in 6–8, 7 in 8–10.

## Method (new harness: `baseline/keystroke-harness/`)

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

## Status of the gate

- **Nice Dev baseline: DONE.** This is the number Path B must land within
  ~1 frame of.
- **gpui-term half: PENDING** (interactive damage-gated echo mode in
  progress; the harness above is reusable as-is for that side).

## Incident (methodology honesty)

The first injection attempt typed into Nice Dev's restored-session prefill
line (`NICE_PREFILL_COMMAND`) and executed `claude --resume <id>cat` — bogus
id, no data loss, killed. The measurement pane for the real run was a fresh
plain-shell window. Lesson recorded: **keystroke runs must use a fresh window,
never a restored pane.**

## Evidence

`baseline/keystroke-harness/out/keyinject-nicedev.csv`,
`baseline/keystroke-harness/out/keylat-nicedev-samples.csv` (the trace itself
lives in the session scratchpad, not committed).
