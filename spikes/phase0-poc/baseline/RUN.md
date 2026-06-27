# RUN.md — baseline capture runbook (current Nice = **Nice Dev**)

Fills **3 of the 4** Baseline cells in the report §10 table / `harness.rs`
`Results::markdown()` `TODO`s for the *current* Swift app, WITHOUT changing any
Nice source:

1. **Term present FPS p50/p95 (ms)** — `capture-present.sh` (fork's `Metal.Draw`
   signpost) while `replay.sh` feeds the fixture.
2. **phys_footprint idle (MiB)** — `sample-mem.sh` over an idle window.
3. **phys_footprint under-load steady/peak (MiB)** — `sample-mem.sh` during replay.

The **4th cell — keystroke-latency pty-echo — is DEFERRED** (see bottom): it
needs `CGEventPost` + Accessibility TCC for *Nice Dev*; do not build it now.

> All commands target **Nice Dev** (`dev.nickanderssohn.nice-dev`). Never prod
> `/Applications/Nice.app`. Never `--prod`. Never bare `xcodebuild`. Never
> `pgrep` (use the **nice-process-check** skill). The scaffolding agent did NOT
> run any of this — it requires a display.

## 0. Make the scripts executable

```sh
chmod +x baseline/replay.sh baseline/sample-mem.sh baseline/capture-present.sh
```

## 1. Ensure the deterministic fixture exists

```sh
ls -l /tmp/nice-fixture.bin   # expect 4,000,000 bytes
# regenerate if missing (headless, no display, deterministic xorshift seed=42):
cd spikes/phase0-poc && NICE_POC_FIXTURE=/tmp/nice-fixture.bin cargo run
```

## 2. Build + install Nice Dev (under the worktree lock)

Use the **worktree-lock** skill so concurrent worktrees don't corrupt shared
DerivedData / the dev bundle:

```sh
scripts/worktree-lock.sh acquire baseline-capture
scripts/install.sh            # DEV default — installs "Nice Dev". NOT --prod.
scripts/worktree-lock.sh release
```

## 3. Launch ONE Nice Dev pane with the signpost enabled

`SWIFTTERM_PROFILE=1` must reach the process or `Metal.Draw` never fires.

**Env-passing caveat:** `open -a 'Nice Dev'` does NOT reliably forward shell env.
Launch the executable directly (this inherits the env you set):

```sh
SWIFTTERM_PROFILE=1 '/Applications/Nice Dev.app/Contents/MacOS/Nice Dev' &
```

Keep it to a **single pane** so only one renderer drives the signpost stream.
Bring the window to the foreground (present timing requires the on-screen key
window — that is why this runbook needs a display).

## 4. Resolve the pane shell tty (pgrep-free)

```sh
# confirm which builds run (prod-safety gate):
~/.claude/skills/nice-process-check/check.sh

# resolve the pane SLAVE pty automatically (Nice Dev pid -> child shell -> tty):
TTY="$(baseline/replay.sh --find-tty)"   # e.g. /dev/ttys003
echo "$TTY"
```

Manual equivalent (documented in `replay.sh`): get the Nice Dev pid from
nice-process-check, find its child shell pid (the child that owns a real tty;
Nice Dev itself shows `??`), then `ps -o tty= -p <shell-pid>` -> `/dev/ttysNNN`.

Also grab the Nice Dev pid for the memory step:

```sh
DEVPID="$(ps -Aww -o pid=,args= | awk '/Nice Dev\.app\/Contents\/MacOS\/Nice Dev( |$)/{print $1; exit}')"
echo "$DEVPID"
```

## 5. Term present FPS (run capture + replay TOGETHER, >=18 s)

Start the capture first (so it's streaming before bytes arrive), then the paced
replay. The fixture is 8 s at 500000 B/s, so paced replay loops it to >=18 s.

```sh
# terminal A — capture 20 s of Metal.Draw signposts:
baseline/capture-present.sh 20 /tmp/nice-present.ndjson
# terminal B (start immediately after) — paced ~500000 B/s for ~20 s into the pane:
baseline/replay.sh "$TTY" paced 20
```

`capture-present.sh` prints, e.g.:

```
PRESENT samples=1180 p50_ms=16.68 p95_ms=18.90 p99_ms=22.10 fps_p50=60.0 cliffs=3
```

-> **Term present FPS p50/p95 (ms)** = `p50_ms / p95_ms`.
(`cliffs` counts intervals > 16.6 ms = 2x a 120 Hz frame; it is **120Hz-calibrated**,
so on a 60 Hz panel report p50/p95, not the cliff count — see README §10.)

## 6. phys_footprint — idle (>=30 s) then under-load (>=60 s)

Idle: pane open, no replay.

```sh
baseline/sample-mem.sh "$DEVPID" 30 idle
# -> MEM idle samples=300 median_mib=NN.N peak_mib=NN.N
```

Under-load: start the memory sampler, then start a paced replay that outlasts it.

```sh
# terminal A:
baseline/sample-mem.sh "$DEVPID" 60 load
# terminal B (start immediately):
baseline/replay.sh "$TTY" paced 65
# -> MEM load samples=600 median_mib=NN.N peak_mib=NN.N
```

-> **phys_footprint idle (MiB)** = idle `median_mib`.
-> **phys_footprint under-load steady/peak (MiB)** = load `median_mib` / `peak_mib`.

Sanity: confirm no monotonic growth across the load window (tail of the raw file
`/tmp/nice-baseline-mem-load.txt` should not be a rising ramp).

## 7. Where the 3 numbers go

Replace the `TODO` Baseline cells in **`spikes/phase0-poc/src/harness.rs`**
`Results::markdown()` (the table emitted to the report §10):

| Metric | Baseline cell to fill | Source |
|---|---|---|
| `Term present FPS p50/p95 (ms)` (harness.rs ~:543) | `p50_ms`/`p95_ms` | step 5 |
| `phys_footprint idle (MiB)` (harness.rs ~:563) | idle `median_mib` | step 6 |
| `phys_footprint under-load steady/peak (MiB)` (harness.rs ~:567) | load `median_mib`/`peak_mib` | step 6 |

(The `Keystroke latency pty echo … TODO` cell at ~:559 stays `TODO` — deferred.)

## 8. §10 decision thresholds (apply once PoC + baseline are both filled)

- **Term present FPS:** PASS if PoC term p95 **<= baseline p95 x 1.15** with no
  cliff cluster.
- **phys_footprint idle:** PoC idle **<= baseline x 1.2**.
- **phys_footprint under-load:** PoC under-load steady **<= baseline x 1.2** with
  **no monotonic growth**.
- All FPS + memory (+ proofs) PASS -> **Path A**; memory/FPS dual-stack tax
  FAIL -> **Path B**; broad FAIL -> revert to in-place AppKit (README §10).

## 9. DEFERRED — keystroke-latency pty echo (4th cell)

Not built. It requires posting a **real** key event via `CGEventPost` to the
focused Nice Dev window and reading the resulting present timestamp
(`latency = present - post`, single-in-flight, N=500; harness §C). `CGEventPost`
into another app needs the **Accessibility TCC grant for Nice Dev**, which is
out of scope for this prod-safe, source-untouched baseline scaffolding. Leave
`harness.rs` `latency_pty` Baseline cell as `TODO`.
