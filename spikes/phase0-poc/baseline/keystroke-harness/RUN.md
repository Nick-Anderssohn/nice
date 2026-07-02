# RUN.md — keystroke-latency harness (spikes 4b + 5)

Measures **real keyDown → present latency**: `keyinject` posts genuine OS key
events to ONE target pid (`CGEventPostToPid` — never the global HID tap, so a
run can never type into whatever the user has focused elsewhere), a concurrent
`xctrace` Logging recording captures the target's present signposts, and
`reduce-latency.py` joins the two into p50/p95/p99 + a histogram.

Targets:

- **A — Nice Dev** (SwiftTerm fork): present marker = the fork's `Metal.Draw`
  signpost (`org.tirania.SwiftTerm` / `MetalProfile`), needs
  `SWIFTTERM_PROFILE=1` at launch.
- **B — gpui-term** (Rust GPUI): present marker = the os_signpost the gpui-term
  builder is adding — **subsystem/category/name are parameterized below**
  (`$GT_SUB` / `$GT_CAT` / `$GT_NAME`); discover them with `--list` (step B.3).
  Until that signpost lands there is a system-emitted fallback:
  `com.apple.coreanimation` / `CAMetalLayer` / `ClientDrawable` fires once per
  drawable for ANY Metal app and appears in the spike-4 trace (n=370, dur p50
  1.30 ms, tracking `Metal.Draw` 1:1).

Everything here targets **Nice Dev / gpui-term only**. Never prod
`/Applications/Nice.app` (the injector hard-refuses its pid), never `pgrep`
(use `~/.claude/skills/nice-process-check/check.sh`).

## 0. Build + preflight

```sh
cd spikes/phase0-poc/baseline/keystroke-harness
make            # xcrun swiftc -O -o keyinject keyinject.swift
make test       # arg-validation + reducer self-test (posts nothing)
```

**Accessibility re-check (every session, BEFORE recording anything):**

```sh
./keyinject --check --pid <target-pid>
# or the bare probe:
swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'
```

The grant belongs to this session's *responsible process* — prod `Nice.app`
(see `../ACCESSIBILITY-GRANT.md`). If the check fails even though Nice shows
ON in System Settings → Privacy & Security → Accessibility, the grant is
**stale** (prod Nice was rebuilt; its ad-hoc cdhash changed): **remove Nice
with `−` and re-add it**, then re-check. `keyinject` performs this same check
at startup and exits 2 with the same instructions.

## 1. How the clocks line up (read once)

- xctrace exports signpost times as **ns since trace start**.
- `keyinject` emits its own signpost interval around every post — subsystem
  `nice.keyharness`, category `inject`, name `KeyPost`, **signpostID = seq**
  (exported as the `identifier` column, giving an exact join with the CSV).
- **Primary path:** record with `--all-processes` → KeyPost and the target's
  present signposts land in the SAME table and timebase; latency is a plain
  subtraction, no clock conversion. (An `--attach` recording will NOT work for
  this: the spike-4 TOC shows the signpost table is `target-pid="SINGLE"`, so
  the injector's signposts would be absent.)
- **Fallback path** (attach-only recording): keyDown times come from the CSV's
  `wall_epoch_ns` column anchored via the TOC's `<start-date>`; that ISO stamp
  has **millisecond precision**, so results carry ±1–2 ms — the reducer warns.
- **Cross-check:** with CSV + in-trace KeyPost the reducer prints the spread of
  `trace_ns − mach_abs_ns` and `trace_ns − mach_cont_ns` across samples. A
  near-zero spread on one of them identifies the trace's clock domain; a large
  spread on both means the join is broken — investigate before trusting
  numbers. (CSV ns columns are precomputed as `ticks * numer / denom` from
  `mach_timebase_info`; the raw ticks are also logged.)
- CSV stamp, KeyPost begin and the actual post agree to single-digit µs — noise
  at the ms scale being measured (NOTES.md §3: stamp *before* the post).

## 2. Pacing / single-in-flight (NOTES.md §3, Harness §C)

Default `--gap-ms 100` with N=500: one keystroke is in flight at a time because
100 ms is comfortably above the worst present latency ever observed on this
machine (spike-4: inter-draw p95 66 ms *under heavy load*; idle echo draws land
well under 40 ms). The reducer enforces it too: a sample whose matched present
falls after the next keyDown is dropped and counted (`dropped_overrun`). Run
length ≈ (500+5)×100 ms ≈ **51 s**; size `--time-limit` accordingly.

## 3. Target A — Nice Dev (SwiftTerm `Metal.Draw`)

```sh
# 1. prod-safety gate + make sure no stray dev build is running
~/.claude/skills/nice-process-check/check.sh

# 2. launch ONE Nice Dev pane with the fork's signposts enabled
#    (direct exec — `open -a` does not forward env):
SWIFTTERM_PROFILE=1 '/Applications/Nice Dev.app/Contents/MacOS/Nice Dev' &

# 3. inside the pane, run the loopback echo (NOTES.md §3):
cat

# 4. resolve the Nice Dev pid (never pgrep):
DEVPID="$(ps -Aww -o pid=,args= | awk '/Nice Dev\.app\/Contents\/MacOS\/Nice Dev( |$)/{print $1; exit}')"
echo "$DEVPID"

# 5. preflight:
./keyinject --check --pid "$DEVPID"

# 6. start the recording (ALL processes — see §1), 65 s covers 505 posts @100ms:
xcrun xctrace record --template 'Logging' --all-processes \
  --time-limit 65s --output /tmp/keylat-nicedev.trace &
sleep 3   # let the recorder spin up before the first post

# 7. inject (activates Nice Dev, posts 5 warmup + 500 measured, ~51 s):
./keyinject --pid "$DEVPID" --n 500 --gap-ms 100 \
  --out /tmp/keyinject-nicedev.csv
wait      # for xctrace to finish + post-process

# 8. export + reduce:
xcrun xctrace export --input /tmp/keylat-nicedev.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost-interval"]' \
  > /tmp/keylat-nicedev.xml
python3 reduce-latency.py --xml /tmp/keylat-nicedev.xml \
  --csv /tmp/keyinject-nicedev.csv \
  --present-name Metal.Draw --present-subsystem org.tirania.SwiftTerm \
  --present-category MetalProfile --present-pid "$DEVPID" \
  --out-csv /tmp/keylat-nicedev-samples.csv
```

Do not touch the keyboard/mouse during the ~51 s injection window — the target
must stay frontmost with the `cat` pane focused (injected events go only to
`$DEVPID`, but a focus change can make its window drop keys → dropped samples).

**Semantics:** latency = keyDown post → END of the `Metal.Draw` interval
(`present(drawable)` is called inside `draw(in:)`, so interval-end ≈ CPU-side
present submitted; GPU-complete adds sub-ms — same definition on both targets,
so the comparison is apples-to-apples). `--edge start` measures to draw-begin
instead. Includes: event delivery → pty write → `cat` echo → parse →
invalidation wait (demand-driven MTKView coalesces to the display cycle) →
draw. **Cursor-blink caveat:** blink redraws also emit `Metal.Draw`; one can
only corrupt a sample if it fires inside the few-ms window between post and
echo-arrival (~0.4 % of samples at a 2 Hz blink) — visible as a low-side
histogram outlier, not a percentile mover.

## 4. Target B — gpui-term (parameterized signpost)

```sh
# 1. launch (display-gated; adjust per the gpui-term builder's notes —
#    NICE_POC_SECS must outlast the ~51 s injection):
cd spikes/phase0-poc
NICE_POC_RUN=1 NICE_POC_SECS=90 cargo run --release --bin gpui-term &
GTPID=$!   # cargo exec's the binary; verify: ps -o pid,args -p "$GTPID"

# 2. record + inject exactly as in A.6–A.7 (output /tmp/keylat-gpui.*)
cd baseline/keystroke-harness
xcrun xctrace record --template 'Logging' --all-processes \
  --time-limit 65s --output /tmp/keylat-gpui.trace &
sleep 3
./keyinject --pid "$GTPID" --n 500 --gap-ms 100 --out /tmp/keyinject-gpui.csv
wait
xcrun xctrace export --input /tmp/keylat-gpui.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost-interval"]' \
  > /tmp/keylat-gpui.xml

# 3. DISCOVER the present-signpost identity the other builder shipped:
python3 reduce-latency.py --xml /tmp/keylat-gpui.xml --list

# 4. reduce with the discovered names:
GT_SUB=<subsystem>  GT_CAT=<category>  GT_NAME=<name>
python3 reduce-latency.py --xml /tmp/keylat-gpui.xml \
  --csv /tmp/keyinject-gpui.csv \
  --present-name "$GT_NAME" --present-subsystem "$GT_SUB" \
  --present-category "$GT_CAT" --present-pid "$GTPID" \
  --out-csv /tmp/keylat-gpui-samples.csv
```

**⚠️ Continuous-RAF caveat (decide BEFORE trusting B's numbers):** gpui-term
presents every frame whether or not content changed (spike-4 report). If its
present signpost fires unconditionally per frame, "next present after keyDown"
samples the *frame phase* (uniform 0–16.7 ms @60 Hz), not input latency. Two
valid setups:

1. the gpui-term signpost is **damage-gated** (emitted only for frames that
   consumed new terminal bytes) → plain reduction is valid; or
2. gpui-term emits a separate **echo/damage marker** signpost when the echoed
   byte is applied to the grid → pass it as the gate:
   `--gate-name <marker> --gate-subsystem <sub>` (the reducer then takes the
   first present at/after the marker).

If neither exists yet, the run still captures everything (re-reduce later once
the marker lands), and `ClientDrawable` (see header) gives a per-drawable
present stream — but it is also per-frame under RAF, so the same caveat
applies. Coordinate with the gpui-term builder; don't report an
unconditional-RAF "latency" as a result.

Also note gpui-term must actually **route injected keys to its pty/grid** for
an echo to exist — verify a visible `a` appears in its window during the run.

## 5. Reading the output

```
LATENCY method=in-trace edge=end present=Metal.Draw keys=500 matched=496 \
  dropped_overrun=3 dropped_tail=1 dropped_nogate=0 \
  p50_ms=.. p95_ms=.. p99_ms=.. min_ms=.. max_ms=..
histogram (bin=2 ms): ...
```

- `method=in-trace` is required for headline numbers; `wallclock-fallback`
  carries ±1–2 ms and is flagged.
- Check the `clock-check:` stderr lines: one mach clock should show ~0 spread.
- `dropped_overrun` should be ~0–1 %; a large value means the target stopped
  echoing (lost focus, `cat` not running) — rerun.
- Warmup posts (default 5) are excluded via the CSV's `warmup` column.
- p50/p95/p99 use the same index method as `harness.rs` / the sibling reducers.

## 6. Troubleshooting

- `record` fails with a permissions error on `--all-processes`: rerun under
  `sudo` (export/reduce stay unprivileged), or fall back to
  `--attach "$PID"` + `--toc`:
  `xcrun xctrace export --input X.trace --toc > /tmp/toc.xml` then add
  `--toc /tmp/toc.xml` to the reduce call (fallback mode engages
  automatically when no KeyPost rows are found).
- `reduce-latency.py … 0 present signposts matched`: run `--list`; for Nice
  Dev this usually means `SWIFTTERM_PROFILE=1` didn't reach the process
  (launched via `open -a`?).
- `keyinject` exit 2: Accessibility — see §0 (stale grant → remove + re-add).
- `keyinject` exit 3 naming prod Nice: you resolved the wrong pid — re-run
  nice-process-check.
