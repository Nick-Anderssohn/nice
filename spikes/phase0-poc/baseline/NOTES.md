# Baseline capture — current Nice, WITHOUT touching prod

**Method only. Do NOT run any of this as part of scaffolding.** This file
describes how the *user* captures the same three metrics on the current Nice so
the PoC numbers (`src/harness.rs` output) have an apples-to-apples comparator.

## Hard guardrails (CLAUDE.md)

- **Never** target prod `/Applications/Nice.app` (`dev.nickanderssohn.nice`).
- **Never** pass `--prod` to `scripts/install.sh` / `scripts/uninstall.sh`.
- **Never** run a bare `xcodebuild` / `xcodebuild test` against the `Nice`
  scheme (that runs with the prod bundle ID).
- All baseline work targets **`Nice Dev`** (`dev.nickanderssohn.nice-dev`):
  separate UserDefaults, separate Application Support, separate DerivedData.
- Before any process check use the **nice-process-check** skill
  (`~/.claude/skills/nice-process-check/check.sh`). **Never `pgrep`** — on macOS
  a GUI app's `comm` is the exec path truncated to 16 chars, so `pgrep Nice`
  reports a false "not running" for prod.
- Install/run `Nice Dev` under the **worktree-lock** skill (UITests + DerivedData
  contend across worktrees).

## Why the comparison is valid

`Nice Dev` statically links the **same** SwiftTerm fork the PoC links
(`/Users/nick/Projects/SwiftTerm` @ `2f2a0b72…`). The present timing therefore
comes out of the identical renderer in both binaries; the only difference under
test is the *chrome stack* (Nice's AppKit chrome vs the PoC's GPUI dual-Metal
chrome). Both consume the **same `fixture.bin`** (Harness §E.3), so the byte
stream is identical bit-for-bit.

Generate the fixture from the PoC harness (deterministic, seeded):

```sh
# in spikes/phase0-poc
NICE_POC_FIXTURE=/tmp/nice-fixture.bin cargo run   # headless; writes the replay stream
```

## 1. FPS (term present) — shared present hook in the fork

The PoC reads present timing via the bridge's `st_present_now` path. Nice does
NOT go through that bridge, so for the baseline the present hook must live in the
**SwiftTerm fork both binaries share** — env-gated so it is inert normally:

- Add (in a THROWAWAY branch of the fork, gated by `getenv("NICE_HARNESS")`) a
  `commandBuffer.addCompletedHandler` in `MetalTerminalRenderer.draw(in:)`
  (~`:472`) that writes `mach_absolute_time()` to a unix socket / file.
  `present(drawable)` is at `:607`; `draw(in:)` entry at `:321`.
- Build `Nice Dev` with that fork via `scripts/install.sh` (dev default, under
  worktree-lock). NO Nice-app source change is required — the hook is entirely
  in the shared fork.
- Launch ONE `Nice Dev` pane, replay the fixture (next section), collect present
  timestamps, reduce with the SAME percentile code (`harness::percentiles`).

> NOTE: this fork patch is exactly the GPU-complete present hook the PoC could
> NOT add (the fork is read-only for the PoC). For the PoC side, GPU-complete
> timing comes from the fork's existing `SWIFTTERM_PROFILE=1` OSSignposter
> "Metal.Draw" stream instead (see ../README.md §Caveats). Use the SAME source
> (signpost stream) on BOTH sides if you want strict like-for-like, or the
> completion-handler on both.

## 2. Feed the identical workload

Run a tiny replayer **inside a `Nice Dev` pane** so it drives Nice's real PTY →
SwiftTerm feed path with byte-identical input, no Nice-internal hooks:

```sh
# inside a Nice Dev terminal pane
cat /tmp/nice-fixture.bin
# or, to match the PoC's paced rate rather than max throughput, a ~10-line
# program that write(2)s the fixture in bytes_per_sec-sized slices with the
# seeded gap schedule.
```

## 3. Keystroke latency

- Post a **real** key via `CGEventPost` to the focused `Nice Dev` window (a
  genuine OS event, not a selector call), in **loopback** (type into a `cat`
  running in the pane so the shell echoes).
- Timestamp with `mach_absolute_time()` **before** the post; read the shared
  present hook's timestamp for the resulting frame; `latency = present - post`.
- Same single-in-flight correlation + N=500 as Harness §C; same reducer.

## 4. Memory — sample `Nice Dev`'s pid EXTERNALLY (prod-safe, non-privileged)

Do **not** use `task_for_pid` (needs the debugger entitlement). External tools
are sufficient and never contact prod:

```sh
# resolve the dev pid via nice-process-check (NEVER pgrep):
#   ~/.claude/skills/nice-process-check/check.sh   -> prints the Nice Dev pid
PID=<dev pid>
# every 100 ms across idle (>=30 s) then under-load (>=60 s):
footprint "$PID"            # -> "Physical footprint:" == phys_footprint
# or:
vmmap --summary "$PID" | grep -i 'Physical footprint'
ps -o rss=,vsz= -p "$PID"   # RSS comparator
```

The PoC process measures **itself** in-process via `task_info`
(`harness::mem::sample`); the baseline is sampled **externally** — both yield
`phys_footprint`, so they are directly comparable.

## 5. Fill the comparison table

Drop the four baseline numbers (term present p50/p95, keystroke latency p95,
phys_footprint idle, phys_footprint under-load steady/peak) into the `TODO`
cells of the PoC's `Results::markdown()` table and apply the §10 decision tree
(thresholds in ../README.md §Decision tree).
