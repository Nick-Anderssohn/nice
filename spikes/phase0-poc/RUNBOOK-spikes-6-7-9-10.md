# Live-run runbook — §13 spikes 6, 7, 9, 10 (prepped 2026-07-02, build-only)

Everything here needs a DISPLAY (the prep agent never ran these). All
commands from `spikes/phase0-poc/`. Every run auto-exits and prints its
summary + CSV path; keep the pointer parked off the windows during
measurement. Release builds are already compiled (`cargo build --release`).

Definitions/gates: `notes/rewrite-stack-research.md` §13 items 6/7/9/10.
Mode reference: `README.md` §"2026-07-02 spike prep".

Fill the RESULT blocks below (or write per-spike RESULTS files) as runs land.

---

## Spike 7 FIRST — capture the real trace (feeds spikes 6/9/10 realism)

§13: "record a real Claude session's pty bytes with timestamps; replay
timing-faithfully into both bins; plus a max-rate drain test."

**Step 1 — capture (HUMAN-adjacent: needs a real interactive claude session;
the main session can run it inside its own terminal).** Aim for a few minutes
of real work incl. a long streamed answer; a ~10 MB trace is ideal for the
drain test.

```sh
cargo run --release --bin pty-capture -- -o /tmp/claude-session.nicetrace -- claude
# ...use claude normally (ask for something that streams a lot)... then exit.
```

Sanity + the PARSE-half drain number (headless, no display needed):

```sh
NICE_POC_TRACE=/tmp/claude-session.nicetrace ./target/release/gpui-term
```

**Step 2 — timing-faithful replay, Path B bin** (auto-sizes its deadline to
the trace duration; finalizes ~1 s after quiescent):

```sh
NICE_POC_RUN=1 NICE_POC_TRACE=/tmp/claude-session.nicetrace cargo run --release --bin gpui-term
```

**Step 3 — max-rate drain, Path B bin** (wall-clock to quiescent + max frame
interval in the summary):

```sh
NICE_POC_RUN=1 NICE_POC_TRACE=/tmp/claude-session.nicetrace NICE_POC_TRACE_MODE=drain cargo run --release --bin gpui-term
```

**Step 4 — same two runs, Path A bin** (real bridge; fork branch as usual for
`txn`):

```sh
NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=txn NICE_POC_TRACE=/tmp/claude-session.nicetrace cargo run --release --bin phase0-poc
NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=txn NICE_POC_TRACE=/tmp/claude-session.nicetrace NICE_POC_TRACE_MODE=drain cargo run --release --bin phase0-poc
```

**Step 5 — 7w3s realism variant (feeds spike 8's numbers with real bytes):**

```sh
NICE_POC_RUN=1 NICE_POC_WINDOWS=7 NICE_POC_STREAMING=3 NICE_POC_TRACE=/tmp/claude-session.nicetrace NICE_POC_TRACE_LOOP=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
```

RESULT (2026-07-02, RUN; A-arm re-run same day on the REAL bridge): paced
B t2 16.66/17.63/17.68 vs synthetic 16.67/17.35/17.61 — clean 60 fps;
paced A t2 (real bridge, banner-verified) **18.00/30.75** — the ~31 ms
tail is PRESENT at real pacing, identical to synthetic 18.34/31.07 ⇒
byte-rate independent; the differential does not soften. Drain wall ~0 ms
for 27,799 B (t2) and 98,563 B (t1), >100 MB/s both bins. Realism
validated: real bytes ~3 orders of magnitude lighter than synthetic ⇒
synthetic numbers are conservative upper bounds. (First A attempt
silently ran the stub bridge — build-time flag + prebuilt binary; always
check the bridge banner.) Full write-up: `RESULTS-spike7-20260702.md`.

---

## Spike 6 — release per-frame cost + energy (three states × runs)

§13: "render busy-time stamps, shape-cache hit counting, MTLCommandBuffer GPU
timestamps, powermetrics in three states. Answers the 120 Hz question without
ProMotion hardware."

All release; 60 s each; single window unless noted. The summary now contains:
busy-cost percentiles (snapshot/build/paint), MetalRenderer::draw CPU
percentiles (compare Nice 1.19/2.41 ms), optional GPU time, shape-cache hit
rate, and proc_pid_rusage CPU/wakeups/energy deltas (no sudo needed).

```sh
# state 1 — idle (no feed, no RAF; expect ~0 draws, ~0% CPU):
NICE_POC_RUN=1 NICE_POC_ENERGY_STATE=idle NICE_POC_SECS=60 cargo run --release --bin gpui-term
# state 2 — idle + one animating chrome dot (RAF at refresh — the GPUI
# whole-scene-repaint idle-cost risk):
NICE_POC_RUN=1 NICE_POC_ENERGY_STATE=dot NICE_POC_SECS=60 cargo run --release --bin gpui-term
# state 3 — streaming (the audited workload), + GPU timestamps:
NICE_POC_RUN=1 NICE_POC_GPU_TS=1 NICE_POC_SECS=60 cargo run --release --bin gpui-term
# multi-session variant (7w3s):
NICE_POC_RUN=1 NICE_POC_WINDOWS=7 NICE_POC_STREAMING=3 NICE_POC_GPU_TS=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
# production-fidelity paint check (bold/italic variants actually rendered):
NICE_POC_RUN=1 NICE_POC_STYLES=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
```

OPTIONAL sudo half (HUMAN — the only step in this whole runbook that needs
the human): while each state runs, in another terminal:

```sh
sudo powermetrics --samplers cpu_power,gpu_power,tasks -i 1000 -n 55 > /tmp/pm-<state>.txt
```

Read: average CPU/GPU mW + the gpui-term task row. Cross-check against the
summary's rusage energy line (`ri_billed_energy`).

120 Hz answer without ProMotion: per-frame total cost (draw CPU + GPU time +
busy-cost) < 8.3 ms ⇒ 120 Hz headroom exists; scale energy by 2x draw rate.

RESULT (2026-07-02, RUN): idle CPU 0.6%/60s, dot 5.6%, streaming 14.8%;
draw CPU p50 0.076 ms (honest analog to Nice's 1.19 is paint-closure 1.54);
GPU p50 0.871 ms; shape hit 39.9%; per-frame total ~2.5 ms ⇒ 120 Hz
headroom. rusage mJ column is inconsistent — use CPU%/wakeups; the sudo
powermetrics pass stays parked for Nick. Full write-up:
`RESULTS-spike6-20260702.md`.

---

## Spike 9 — scrollback / resize-reflow / selection under streaming

§13: "10k-line history scroll, live-resize reflow, selection held across
eviction; kill-signal: multi-hundred-ms reflow stalls."

Headless first look already measured (2026-07-02, release, this machine):
reflow max **4.0 ms** @10k history; memory at history-full 1k/10k/100k =
**+3.5 / +28.5 / +287 MiB** per session; selection rotates sanely across
eviction. The live runs add frame pacing + real NSWindow resize on top:

```sh
# scroll churn while streaming (history prefilled to the limit first):
NICE_POC_RUN=1 NICE_POC_SCROLL_CHURN=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
# resize storm (Term reflow TIMED + real window resize every 400 ms):
NICE_POC_RUN=1 NICE_POC_RESIZE_STORM=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
# selection churn held across eviction (rendered inverse):
NICE_POC_RUN=1 NICE_POC_SELECTION=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
# worst case — all three at once:
NICE_POC_RUN=1 NICE_POC_SCROLL_CHURN=1 NICE_POC_RESIZE_STORM=1 NICE_POC_SELECTION=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
# spike-8 memory question — repeat the plain run at 3 scrollback limits and
# compare the summary memory block + the CSV mem_phys series:
NICE_POC_RUN=1 NICE_POC_SCROLLBACK=1000  NICE_POC_SCROLL_CHURN=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
NICE_POC_RUN=1 NICE_POC_SCROLLBACK=10000 NICE_POC_SCROLL_CHURN=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
NICE_POC_RUN=1 NICE_POC_SCROLLBACK=100000 NICE_POC_SCROLL_CHURN=1 NICE_POC_SECS=30 cargo run --release --bin gpui-term
```

What live steps remain manual: a real MOUSE-drag selection + a real
live-resize by dragging the window edge (both need real input; the
programmatic versions above cover the VT-core + relayout cost — note the
distinction in the results).

RESULT (2026-07-02, RUN, PASS): reflow stall p50/p95/max 6.25/7.85/9.00 ms
@10k (kill-signal absent by ~20×; all-three worst 11.87 ms); scroll churn
16.67/17.15; selection 68 re-anchors, resolves to None on eviction sanely;
memory steady 119.7/137.2/434.0 MiB at 1k/10k/100k — linear in the limit,
closes the spike-8 memory flag. Full write-up: `RESULTS-spike9-20260702.md`.

---

## Spike 10 — atlas pressure

§13: "animated kitty (30 fps 512×512) + a dozen sixels through paint_image
for 60 s; verify what remove() actually reclaims; kill-signal: unbounded
growth or upload-driven drops."

```sh
# the §13 scenario (drop_image on stale frames — the reclaim question):
NICE_POC_RUN=1 NICE_POC_ATLAS=1 NICE_POC_SECS=60 cargo run --release --bin gpui-term
# failure-mode demo — never drop (expect live poly bytes to grow unbounded):
NICE_POC_RUN=1 NICE_POC_ATLAS=1 NICE_POC_ATLAS_RETAIN=1 NICE_POC_SECS=60 cargo run --release --bin gpui-term
# glyph-atlas pressure (unbounded distinct glyphs + bold/italic variants):
NICE_POC_RUN=1 NICE_POC_GLYPH_SWEEP=1 NICE_POC_SECS=60 cargo run --release --bin gpui-term
```

Read the `-- atlas --` block: poly tex live count/MiB should PLATEAU in run 1
(remove() frees whole textures once all 4 512x512 tiles on a 1024x1024
texture are removed — audit expectation: it CAN reclaim, but only at
whole-texture granularity) and GROW LINEARLY in run 2. `upload MiB` ≈ 1 MiB ×
30 fps × 60 s ≈ 1.8 GiB in both. Watch the frame-interval block for
upload-driven drops (cliffs>1.5×p50).

RESULT (2026-07-02, RUN, PASS): run-1 poly +423/−419 tex, live plateau
4 tex = 16 MiB (remove() reclaims, whole-texture granularity — as
predicted); run-2 growth ~1.7 GiB/min (424 tex = 1696 MiB, process
~3.5 GiB); frame p95 under pressure 18.34 vs plain 17.35, auto-cliffs 1
(no upload-driven drops at 1682 MiB/60s); sweep mono atlas +368 tex =
368 MiB never evicted (0.2.2 mono atlas has NO eviction — hygiene item,
not a blocker), hit rate 4.0%. Full write-up: `RESULTS-spike10-20260702.md`.

---

## Hang fix (2026-07-02 second pass) — every live run now auto-exits

The first `energy-idle` live run hung forever (no draws ⇒ no render-path
deadline; the gpui executor timer got App-Napped in a fully idle app). All
live modes in BOTH bins now arm `harness::watchdog` — a dedicated OS thread
that force-wakes the main runloop at the deadline (+3 s grace in gpui-term,
+5 s in phase0-poc) and hard-exits(3) if the main thread stays wedged 20 s.
Mechanism proven headless (`NICE_POC_WATCHDOG_SELFTEST=1`, fires at ~1.0 s).

Practical notes for the runs above:

- A summary whose reason reads `deadline (watchdog)` means the render path
  had starved (window fully occluded / app napped) — the numbers are valid
  but cadence will be sparse; prefer re-running with the window visible.
- `idle` runs SHOULD exit via the watchdog (that's the design: nothing else
  ever wakes) — expect `deadline (watchdog)` with ~0 composited frames,
  metal draws ≈ 1–2 (window-open presents), and the rusage/energy deltas as
  the payload.
- Keep spike windows at least partially visible and the app frontmost: gpui
  stops the display link for fully occluded windows, so RAF cadence (incl.
  the `dot` state) is only meaningful on-screen.
- In phase0-poc a watchdog exit skips the end-of-run mouse-seam/rebind
  probes (gates print UNPROVEN) — re-run visible if those gates matter.

---

## Notes for whoever folds results

- Special-mode CSVs are named `gpui-term-<tag>[-KwMs].csv`; `#` header
  comments carry display/build/seed/flags; `mem_phys` rows are the ~4 Hz
  memory series (elapsed seconds in the `phase` column).
- Frame-interval reports now print `max` and BOTH cliff counts (legacy 16.6 ms
  + self-calibrated 1.5×p50) — use the self-calibrated one for judgments; the
  display + Hz is recorded per run and re-checked at exit (hot-plug guard).
- The per-draw CPU number to quote against Nice's 1.19/2.41 ms p50/p95 is
  the `MetalRenderer::draw CPU` line (same CPU-side submission semantics as
  SwiftTerm's Metal.Draw; NOT GPU-complete — use NICE_POC_GPU_TS for that).
- Background windows now actually present their demand-driven redraws (the
  2026-07-02 kick fix) — a spike-8 re-run will show bg draws that the
  original RUN did not have; the per-window frame stamps are unchanged.
