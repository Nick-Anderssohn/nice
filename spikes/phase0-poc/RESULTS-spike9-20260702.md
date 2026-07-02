# Spike 9 results — scrollback / resize-reflow / selection under streaming, 2026-07-02

Ran §13 spike 9 live on `gpui-term`: release build, 60 Hz panel, 30 s runs,
all while streaming the synthetic 500 KB/s workload (itself ~3 orders of
magnitude heavier than real Claude traffic — spike 7). The §13 kill-signal
was **multi-hundred-ms reflow stalls**.

## Verdict: PASS — kill-signal absent by ~20×; memory is linear in the scrollback limit (closes the spike-8 memory flag)

## Numbers (10k scrollback unless stated; history prefilled to the limit)

| Run | Frame p50/p95/p99 (ms) | Spike-9 payload | Mem steady/peak (MiB) |
|---|---|---|---|
| scroll churn (±3 lines/frame over 10k history) | 16.67 / 17.15 / 17.62 | 1799 scroll_display ops; paint-closure p50 1.359 ms | 137.0 |
| **resize storm** (real NSWindow resize every 400 ms + timed `Term::resize`) | 16.67 / **18.61** / 23.70 | **reflow stall n=72: p50 6.248 / p95 7.851 / max 8.996 ms** | 148.7 / 172.0 |
| selection churn (held across eviction, rendered inverse) | 16.67 / 18.34 / 18.63 | 68 re-anchors; held selection resolves to `None` on eviction sanely (68 None-frames, no panic/garbage) | 135.9 |
| all three at once | 16.67 / 18.60 / 22.18 | reflow p50 6.923 / max **11.865** ms; 258 re-anchors | 185.7 / 194.2 |

- **The kill-signal is absent by a wide margin:** worst measured reflow is
  9.0 ms alone, 11.9 ms with everything at once — the ~200 ms
  "multi-hundred-ms" bar sits 17–25× above anything observed at 10k
  history. Frame p99 23.70 in the resize storm is the reflow landing
  *inside* a frame (render-body p99 8.35 ms), costing ~1.4 frames at
  worst, never a visible stall.
- Selection semantics across eviction confirm the §12 audit finding
  (vanilla alacritty rotates selections gracefully) **live under
  streaming**, not just in source.

## Scrollback memory sweep (closes the spike-8 open question)

Same scroll-churn run at three `scrolling_history` limits, history
prefilled to the limit (prefill itself: 1 / 7 / 74 ms):

| Limit | Idle after prefill (MiB) | Steady under churn (MiB) | Frame p95 (ms) |
|---|---|---|---|
| 1,000 | 16.8 | 119.7 | 18.44 |
| 10,000 | 45.3 | 137.2 | 18.40 |
| 100,000 | 254.6 | 434.0 | 18.29 |

**Memory is linear in the scrollback limit** (~2–3 KB per 120-col line,
consistent with alacritty's row storage), and frame pacing is flat across
two orders of magnitude of history. This closes the flag spike 8 handed
over: the ~60–90 MiB/session growth was **scrollback fill — a
configurable product knob, not a leak or a Path B liability.**

## Manual residue (programmatic vs real input)

The programmatic drivers cover the VT-core + relayout + paint cost — the
decision-relevant half. Still untested by hand: a real mouse-drag
selection and a human window-edge live-resize (continuous AppKit
live-resize delivery vs the 400 ms stepped storm). Note the distinction;
neither touches the reflow/selection machinery just measured.

## Evidence

CSVs (committed, 386863b): `gpui-term-resize.csv`, `gpui-term-select.csv`,
`gpui-term-scroll+resize+select.csv`. The three sweep legs and the plain
scroll run share the filename `gpui-term-scroll.csv` — the committed copy
holds the **last** leg (header `scrollback=100000`); the 1k/10k legs and
the plain 10k churn run are preserved in the tables above (scratchpad
logs `s9-scroll`, `s9-sb1k`, `s9-sb10k`, `s9-sb100k`).
