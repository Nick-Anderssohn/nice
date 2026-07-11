# freeze-harness — black-box input-flood / presentation-liveness probes

Regression harness for the input-flood freeze and presentation wedge fixed in
r5–r5d (see `nice-rewrite-plans/freeze-input-flood/BRIEF.md` for the root
cause and `VALIDATION.md` for the gates and measured numbers). Everything here
observes the app from OUTSIDE the process — AX round-trips, synthetic
CGEvents, pixel captures, ps sampling — so it works against any installed
build with no cooperation from the app.

## Building

The Swift tools are single files; build on demand (binaries are not checked
in):

```sh
swiftc -O axping.swift  -o axping
swiftc -O kflood.swift  -o kflood
swiftc -O ktype.swift   -o ktype
swiftc -O winmove.swift -o winmove
```

`axping` and `winmove` need Accessibility trust for the terminal running them.

## Tools

- `axping.swift` — main-thread responsiveness probe. 250ms AX attribute
  round-trips against a pid; CSV `t_rel_s,latency_ms,status` on stdout,
  `HANG-DETECTED` / `RECOVERED` events on stderr (wrappers key off these).
- `kflood.swift` — keystroke flood: `kflood <pid> [--cps N] [--duration-s N]
  [--enter-every N]`. Posts keydown/up pairs via `CGEventPostToPid`.
- `ktype.swift` — string typer: `ktype <pid> [--cps N] [--enter]
  [--pre-ctrl-c] -- <text>`. US-ANSI layout.
- `winmove.swift` — `winmove displays` (id/bounds/refresh per display),
  `winmove <pid>` (list windows), `winmove <pid> x y [w h]` (move/size
  window 0 — used to pin window size, the freeze's controlling variable).
- `cpumeter.sh` — total CPU-seconds a pid consumes across a workload
  (`cpumeter.sh <pid> <label> -- <cmd...>`) or an idle window
  (`cpumeter.sh <pid> <label> <seconds>`). Comparable across apps.
- `runleg.sh` — one measurement leg: wraps an action with axping + a 500ms
  %cpu sampler + an automatic `sample <pid>` on the first hang.
- `wedgewatch.sh` — presentation liveness: `wedgewatch.sh <pid> <winid>
  <active_window_s> <outdir>`. Captures the window every 500ms; an
  identical-frame streak ≥2s DURING the flood window = presents wedged.
- `render-test.sh` — 10-section rendering exerciser (rules, block/shade
  glyphs, box drawing, ANSI/256/truecolor, styles, run boundaries, CJK) for
  eyeballing parity between builds.

## Gotchas (hard-won; do not relearn)

- Post keys with `CGEventPostToPid`, never the global HID tap (Wispr Flow
  eats globally-posted synthetic keys), and keep the poster alive ~250ms
  after the last event or trailing events are dropped (both key tools do
  this).
- AX-only probing is BLIND to the presentation wedge — the app answers AX
  while the screen is frozen. Any CPU numbers measured during a wedged run
  are invalid (flattered by skipped renders). Always run `wedgewatch`
  alongside CPU/latency legs.
- AX calls against a loaded target need `AXUIElementSetMessagingTimeout`
  (already set in axping/winmove) or they die with -25205.
- Keep the display awake during captures: `caffeinate -d`.
- Window IDs for `screencapture -l`: `scripts/quitprobe/winids.swift <pid>`.

## Reference numbers (r5d, 2026-07-10)

400cps × 12s flood into a 3350×1360 window: pre-fix 51s whole-app freeze;
post-fix AX max 8ms. Full tables in
`nice-rewrite-plans/freeze-input-flood/VALIDATION.md`.
