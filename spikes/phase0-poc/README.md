# Phase-0 PoC — GPUI chrome over the real SwiftTerm Metal terminal

Throwaway proof-of-concept that hosts the **real** SwiftTerm Metal `NSView`
inside **one GPUI window**, driven through a **live key window**, and
**measures** (not assumes) the numbers that decide the Nice chrome-rewrite
architecture. Implements the three recon specs (SwiftTerm bridge, GPUI
embedding, measurement harness) against the committed spikes in `../`.

> Scope: this is a *measurement rig*, not a product. It deliberately does the
> minimum chrome needed to put a continuously-animating GPUI layer over a live
> terminal and read out FPS / latency / memory / the 7 proofs.

---

## What Phase-0 proves (the §10 decision tree)

The PoC produces measured numbers for the dual-Metal-stack process (GPUI's
renderer + SwiftTerm's Metal in one process) vs the current Nice baseline:

1. **Sustained burst FPS** under a synthetic Claude-streaming workload, counted
   **independently** on both stacks (SwiftTerm present + GPUI composite).
2. **End-to-end keystroke latency** through the GPUI↔AppKit seam (loopback) and
   through a real PTY echo.
3. **Idle + under-load memory** (`phys_footprint`) of the dual-stack process.

…and proves through **real OS routing** (not direct selector calls):

4. **Input through a live responder chain** — keyDown/keyUp/flagsChanged,
   NSTextInputClient/IME, VT mouse, first-responder arbitration — via
   `NSApp.sendEvent` through a genuine key window.
5. **Transparent GPUI region over the terminal** with no z-order/blanking.
6. **Cross-window Metal-layer rebind on tear-off** (move the live terminal to a
   2nd GPUI window, toggle its Metal layer off→on).
7. **One AppKit-deep chrome probe** (process-wide swallow/passthrough NSEvent
   monitor coexisting with GPUI focus).

### Decision tree this resolves
- **All FPS + latency + memory + proofs 4–7 PASS** → **Path A** (reuse the
  SwiftTerm renderer, chrome on GPUI). *Recommended outcome.*
- **Proofs 4/5 FAIL (z-order / responder seam) but FPS/memory PASS** →
  **objc2-hybrid** (keep the renderer, no GPUI seam, imperative chrome).
- **FPS or memory FAIL (dual-stack tax)** → **Path B** (`alacritty_terminal` +
  a GPUI-native `TerminalView`).
- **Broad FAIL** → revert to the in-place AppKit baseline.

---

## File tree

```
phase0-poc/
  README.md                 # this runbook
  Cargo.toml                # gpui 0.2.2, objc2 + app-kit/foundation, raw-window-handle, block2, mach2, libc
  build.rs                  # builds + links the Swift bridge dylib (stub by default; real on a flag)
  src/
    main.rs                 # headless self-test (default) OR display-gated GPUI live run
    bridge.rs               # extern "C" decls matching the @_cdecl bridge + safe `Terminal` wrapper
    embed.rs                # objc2 NSView embedding: gpui handles -> subview z-order -> first responder -> tear-off
    input.rs                # live responder-chain routing (key/flags/mouse), event monitor, marked seams
    harness.rs              # clock + dual FPS counters + latency + mach memory + workload + CSV/markdown
  swift-bridge/             # REAL bridge — SwiftPM pkg depending on the SwiftTerm fork
    Package.swift           #   path-dep on /Users/nick/Projects/SwiftTerm, dynamic library product
    Sources/SwiftTermBridge/Bridge.swift   # @_cdecl over the real MacTerminalView + reverse-FFI delegate + harness hooks
  swift-embed/
    StubBridge.swift        # HEADLESS-COMPILE FALLBACK — identical C ABI, plain NSView, no SwiftTerm/Metal
  baseline/
    NOTES.md                # method (only) for the Nice Dev baseline — never touches prod
```

## What compiles vs what is display-gated

| Piece | Status |
|---|---|
| Rust crate (`cargo check`/`build`) with the **stub** bridge | **Compiles, runs headless** (verified) |
| `swift-embed/StubBridge.swift` (`swiftc -emit-library`) | **Compiles** offline, no display (verified) |
| Headless self-test (`cargo run`, no env) — workload + `task_info` memory + reducer + report | **Runs** with no window/AppKit (verified) |
| `swift-bridge/` **real** bridge (`swift build` against the fork) | Builds on the user's machine (transitive SwiftPM deps cached under `~/Library/Caches/org.swift.swiftpm`); see §Build |
| GPUI window + embed + **live measurement DRIVER** (`NICE_POC_RUN=1`) | **DISPLAY-GATED** — needs a real key window; the scaffolder must not run it. The driver is now fully wired: app activation, SIGINT/auto-exit, keystroke-latency injection, the mouse hit-test swizzle, and a populated report. |

---

## Build

### A. Headless (default — stub bridge, no display, no fork)
```sh
cd spikes/phase0-poc
cargo run                       # builds libswifttermbridge.dylib (stub) via swiftc, runs the self-test
```
`build.rs` compiles `swift-embed/StubBridge.swift` to
`$OUT_DIR/libswifttermbridge.dylib` and links it. The stub exposes the **same C
ABI** as the real bridge, so the whole Rust crate type-checks and the harness
plumbing runs without SwiftTerm or a display.

### B. Real SwiftTerm Metal bridge (for the actual measurement)
```sh
cd spikes/phase0-poc
NICE_POC_REAL_BRIDGE=1 cargo build      # build.rs runs `swift build` in swift-bridge/, links the real dylib
```
`build.rs` runs `swift build -c release --package-path swift-bridge --product
SwiftTermBridge`, producing `swift-bridge/.build/release/libSwiftTermBridge.dylib`
with **SwiftTerm statically linked in** and the Metal-shader resource bundle
`SwiftTerm_SwiftTerm.bundle` next to it. The link step rpaths that directory so
the shaders resolve at runtime (see §Caveats).

> The SwiftTerm fork at `/Users/nick/Projects/SwiftTerm` is read-only here; the
> bridge only *reads* it via a SwiftPM path dependency. Its transitive deps
> (swift-argument-parser, swift-docc-plugin) resolve from the SwiftPM cache.

---

## Run the measurement (USER, on a machine WITH a display)

> The agent that wrote this must NOT run this — there is no display in its
> environment. These are the steps for the human.

```sh
cd spikes/phase0-poc
NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 cargo run
```
This opens the transparent GPUI window, **brings it to the front and makes it
key**, embeds the real terminal `NSView` below the chrome, makes it first
responder, then runs a **self-terminating measurement**: it samples an idle
memory baseline, streams the seeded workload for `NICE_POC_SECS` (default 25),
injects one keystroke per frame through `NSApp.sendEvent`, probes the mouse
hit-test seam, and **prints a populated Results table to stdout, then exits 0**.

You no longer have to kill it: **Ctrl-C exits cleanly** (and still prints the
report), and it **auto-exits** at the deadline. Closing the window / Cmd-Q also
emits the report.

Env knobs:

| Var | Default | Effect |
|---|---|---|
| `NICE_POC_RUN=1` | unset (headless) | enter the display-gated live driver |
| `NICE_POC_REAL_BRIDGE=1` | unset (stub) | link the real SwiftTerm Metal bridge (required for real numbers) |
| `NICE_POC_SECS=<n>` | `25` | measurement window in seconds before auto-exit |
| `NICE_POC_CSV=<path>` | unset | also write the raw per-sample CSV (Harness §H.1) |
| `NICE_POC_PRESENT=<scheme>` | `link` | terminal present scheme (see below) |

### `NICE_POC_PRESENT` — terminal present scheme (the present-loop A/B/C/D)

> Numbers below are the **verified** clean sweep: single stable display **"Built-in Retina Display [1470×956], max 60 Hz"** (recorded per run, zero mid-run-change flags), 18 s continuous load, real bridge; CSV-cross-checked by adversarial review. The harness now prints the window's display + `maximumFramesPerSecond` and flags a mid-run change as CONTAMINATED.

| Scheme | What it does | Measured (60 Hz panel, continuous load) — term / GPUI p50 |
|---|---|---|
| `link` *(default)* | terminal present driven off its own **`CADisplayLink`** (`st_start_present_link`), decoupled from GPUI | **term 16.68 ms (~60 fps)**, **GPUI 700 ms (~1.4 fps — starved)** |
| `sync` | terminal `present_now()` **synchronously inside each GPUI render frame** (the original naive scheme) | 33.4 / 33.4 ms (~30 / 30) |
| `async` | terminal present via coalesced **`DispatchQueue.main.async`** (the SwiftTerm fork's *production* path) + GPUI RAF | 33.3 / 33.4 ms (~30 / 30) |
| `copace` | `sync` clock + terminal layer `displaySyncEnabled=false` (`st_set_display_sync`) — **FAILED**: layer flag does not remove the `nextDrawable` block | 33.4 / 33.4 ms (~30 / 30) |
| `txn` | terminal **presents inside the CA transaction** (`presentsWithTransaction`; `commit→waitUntilScheduled→drawable.present()`) so it co-commits with GPUI — **needs the OPT-IN fork patch on branch `phase0-txn-present`** | **18.3 / 17.9 ms (~54 / ~56)** ✅ |
| `none` | terminal **never** presents (feed only) — isolates GPUI's standalone compositor rate | — / 16.70 ms (~60) |

**Finding (see report §10).** Each stack *alone* reaches the panel's 60 Hz refresh (terminal in `link`, GPUI in `none`); the three **naive** schemes (`sync`/`async`/`copace`) split it 30/30 because two `CAMetalLayer`s in one `NSWindow` each issue an independent vsync-gated present and contend for a ~one-commit-per-vsync main-thread budget. **`txn` breaks that ceiling:** presenting the terminal *inside the CoreAnimation transaction* makes it co-commit with GPUI instead of fighting it — both stacks jump to ~54–56 fps, the main-thread present block drops 16.3→3.6 ms, latency 16.7→3.9 ms. So 30/30 was a **double-present artifact, not an irreducible tax**. `draw-attempt` p50 ≈ 0.02 ms throughout (compositing/scheduling ceiling, never a compute tax); memory native (peaks ≤ 155 MiB, no growth). Still open: `txn` is ~55 not a *locked* 60 (read p95 ≈ 31 ms, not the 120 Hz-calibrated cliff count), 120 Hz ProMotion untested, proof 5 end-to-end intermittent. **`txn` requires building the fork branch `phase0-txn-present`** (the path-dependency picks up whatever the fork worktree is checked out to).

Generate the baseline replay fixture once with:
```sh
NICE_POC_FIXTURE=/tmp/nice-fixture.bin cargo run     # headless; deterministic stream -> file
```

### Reading the harness output
- **Markdown report** (stdout): the reduced comparison table (Harness §H.2) with
  PoC numbers filled and `TODO` cells for the baseline (fill from
  `baseline/NOTES.md`), plus the 4 proof gates and the decision tree.
- **Raw CSV** (`Results::write_csv`): one row per sample (Harness §H.1):
  `metric,stack,phase,run,seed,bytes_per_sec,sample_index,value,unit`. Reduce
  offline if you want different percentiles.
- **Frame interval stats** (`harness::interval_stats`): p50/p95/p99 ms and a
  **cliff count** = intervals > 16.6 ms (2× a 120 Hz ProMotion frame). A clean
  PASS is steady cadence on **both** counters at once with no cliff cluster
  during bursts.

### What each of the 7 checks means / how to read it
1. **Burst FPS (term)** — `FPS_TERM` interval p50/p95 + cliffs. PASS if PoC p95
   ≤ baseline p95 × 1.15 with no cliff cluster.
2. **Latency (seam, loopback)** — `LATENCY` p50/p95/p99 ms; PASS if the seam adds
   < ~1 ProMotion frame (8.3 ms) over baseline render. PTY-echo profile is
   informational.
3. **Memory** — `phys_footprint` idle (≥30 s) and under-load steady/peak (≥60 s);
   PASS if under-load steady ≤ baseline × 1.2 with no monotonic growth.
4. **Live keyboard responder (AUTOMATED)** — the driver injects one
   `NSApp.sendEvent(keyDown/keyUp)` per frame through the key window. The bridge's
   `send` callback fires **only** when the keystroke actually reached the
   TerminalView, so a non-zero echo count is the real PASS signal (vs the latency
   loop, which closes on every present). The seam latency p50/p95/p99 is reported
   from the loopback round-trip. IME **marked-text** stays manual (needs a real
   input source). Mouse is item 5 below.
5. **Mouse hit-test seam (AUTOMATED — THE load-bearing test)** — the driver
   installs `input::install_hittest_shim` (an objc2 `class_addMethod` override of
   `GPUIView.hitTest:`) so terminal-region points resolve to the TerminalView and
   the chrome bar stays with gpui. It then (a) verifies routing **deterministically**
   (`hittest_resolves` for a terminal point vs a chrome point) and (b) synthesizes
   a real `NSApp.sendEvent` drag and checks the terminal formed a **selection**.
   PASS (routing + selection) ⇒ Path A; routing FAIL ⇒ objc2-hybrid; routing OK
   but no selection ⇒ UNPROVEN (drag manually). Transparent-over-terminal
   compositing itself is already **visually confirmed** (gpui's metal layer is
   `opaque=false`, alpha 0); a `CGWindowListCreateImage` pixel assertion remains
   a manual nicety.
6. **Tear-off Metal rebind (PARTLY AUTOMATED)** — the driver runs a **same-window
   proxy**: `set_use_metal(false)` then `(true)` and asserts the terminal present
   counter **resumes** (the load-bearing CAMetalLayer rebind). The full
   cross-window reparent (`embed::reparent_to` into a 2nd gpui window) is wired but
   driven manually.
7. **AppKit-deep monitor (AUTOMATED)** — `input::install_swallow_monitor` installs
   a process-wide local NSEvent monitor (swallows keyCode 53 / Escape, passes the
   rest) alongside gpui focus; the driver reports it installed without a competing
   swallow monitor. End-to-end swallow of a windowserver Escape stays manual
   (local monitors fire on queued events, not synthetic `sendEvent`).

---

## ⚠️ Load-bearing seams (read before trusting a result)

### 1. Z-order vs. hit-test (the single seam that picks Path A vs objc2-hybrid)
Verified from gpui 0.2.2 source: `GPUIView` does **not** override `hitTest:`, and
`GPUIWindow` does **not** override `sendEvent:`. So:
- **Keyboard/IME routing is clean** — one first responder per window; switch with
  `makeFirstResponder`. Expected PASS.
- **Visual compositing is clean** — terminal as a sibling **below** `GPUIView`
  (`embed::embed_below_chrome`), gpui's non-opaque metal layer reveals it.
  Expected PASS for proof 5.
- **MOUSE was the genuine unknown — now WIRED + measured.** With the terminal
  below `GPUIView` (required for "transparent over terminal"), default hit-testing
  gives **every** mouse hit to `GPUIView` — the terminal gets no VT mouse /
  selection. Visual compositing and mouse hit-testing want **opposite** z-orders.
  Resolution (a) is now implemented and measured by the live driver:
  - **(a) IMPLEMENTED** — `input::install_hittest_shim` adds a `hitTest:` override
    to gpui's runtime-declared `GPUIView` class via `objc2::ffi::class_addMethod`
    (verified: `GPUIView` does **not** override `hitTest:`, so we *add* an override
    rather than `method_setImplementation`, which would clobber every `NSView`).
    The override returns the TerminalView for points inside the terminal frame and
    **below** the top chrome bar, and delegates chrome / out-of-bounds points to
    the original NSView IMP. The driver verifies routing deterministically and with
    a synthetic drag (selection). **PASS ⇒ Path A; FAIL ⇒ objc2-hybrid.**
  - **(b)** `embed::embed_above_in_rect` — terminal on top within its sub-rect:
    mouse+keyboard route naturally but gpui can no longer composite *over* it.
    This is the structural objc2-hybrid fallback if (a) ever FAILs.

  Visual-compositing, keyboard, and mouse are reported as **SEPARATE** gates so a
  mouse-only failure routes to **objc2-hybrid** (seam failure), not Path B.

### 2. GPU-complete present timing (the fork is read-only here)
The original harness spec patches `MetalTerminalRenderer.draw(in:)` with a
`commandBuffer.addCompletedHandler` for a true GPU-complete timestamp. **The PoC
does not modify the fork.** Instead:
- `st_present_now` fires the present hook **after** `mtkView.draw()` returns — a
  CPU *frame-submitted* timestamp (`draw(in:)` ran synchronously). Good for
  cadence + the deterministic latency loop, **not** GPU-complete. The same hook
  also fires from `st_start_present_link`'s `CADisplayLink` tick and from
  `st_present_async`, so the cadence stream reflects whichever present scheme
  (`NICE_POC_PRESENT`) is active.
- For true GPU-complete timing, run with `SWIFTTERM_PROFILE=1` and stream the
  fork's existing `OSSignposter` "Metal.Draw" interval (subsystem
  `org.tirania.SwiftTerm`, category `MetalProfile`). Use the **same** source on
  the baseline side for like-for-like (`baseline/NOTES.md`).

### 3. Two-`CAMetalLayer`-in-one-window present contention (the §10 yellow flag — verified)
The decoupling re-measure (2026-06-27, clean single 60 Hz display, CSV-verified)
established that the terminal's `CAMetalLayer` and GPUI's `CAMetalLayer`
**contend for one main-thread, ~one-commit-per-vsync budget** — see the
`NICE_POC_PRESENT` table above and report §10. Proven for the three naive
schemes (`sync`/`async`/`link`); a **co-paced single-clock present** (both layers
off one vsync, or one shared layer) and a **120 Hz ProMotion** re-measure remain
the open experiments before the A-vs-B decision. The renderer's
present is `view.currentDrawable` (vsync-gated) + non-blocking `frameSemaphore` +
async `present(drawable)`, so the cost is the per-present main-thread stall, not
encode (`draw-attempt` p50 ≈ 0.01 ms). Reaching 60/60 needs a **co-paced
single-clock present** (drive both layers from one frame tick) or an
off-main-thread/non-blocking terminal present — not built here; it needs real
GPUI-internals work. Re-measure on a 120 Hz ProMotion panel too (this was 60 Hz).

---

## Live driver: what's AUTOMATED vs still MANUAL

The §C.4 live driver is wired into `gui::run_live` + `ChromeRoot::render` (the
gpui render loop is the main-thread timer; `request_animation_frame` keeps it
ticking). It self-terminates and prints a populated report.

**Automated now (run with `NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1`):**

- **App activation** — `input::activate_front` (`setActivationPolicy(.Regular)` +
  `activateIgnoringOtherApps`) + `cx.activate(true)` + `makeKeyAndOrderFront`, so
  injected events traverse the real responder chain.
- **Clean shutdown** — SIGINT/SIGTERM flip an atomic stop-flag (Ctrl-C works);
  auto-exit after `NICE_POC_SECS`; window-close / Cmd-Q via `cx.on_window_closed`.
  Every path drains the streams, builds the **same** `Results`, prints the
  markdown, (optional CSV), and `exit(0)`.
- **Keystroke-latency injection (proof 4)** — one `key_down`/`key_up` per tick via
  `NSApp.sendEvent`; the bridge `send` callback counts real terminal echoes; the
  loopback present closes the seam-latency loop → `latency_seam` p50/p95/p99.
- **Mouse hit-test seam (proof 5)** — `install_hittest_shim` + deterministic
  routing check + synthetic drag/selection.
- **Metal-layer rebind (proof 6, same-window proxy)** — `set_use_metal(false→true)`
  and assert presents resume.
- **Swallow monitor (proof 7)** — installed at embed; reported installed.
- **Idle + under-load memory** — idle baseline sampled before streaming; under-load
  steady (median) + peak `phys_footprint` tracked during the run.

**Still manual:**

- **IME marked-text:** real `setMarkedText:`/`unmarkText` come from the system
  input source, not a synthesizable event. Committed text already flows through
  `keyDown:` → `NSTextInputClient.insertText:`. See `input::IME_NOTE`.
- **Full cross-window tear-off (proof 6):** `embed::reparent_to` is wired; opening
  a 2nd gpui window and reparenting the live terminal into it is a manual step
  (the same-window metal-rebind proxy covers the load-bearing CAMetalLayer rebind).
- **Screenshot proof (proof 5 visual):** `CGWindowListCreateImage` capture + pixel
  assertion (compositing is already visually confirmed).
- **Real-bridge requirement:** the live run needs `NICE_POC_REAL_BRIDGE=1`; the
  stub has no Metal (`st_set_use_metal` returns 0) and is for headless compile.

---

## Caveats

- **Metal-shaders resource bundle.** `setUseMetal(true)` loads the shader library
  via `Bundle(for: MetalTerminalRenderer.self)`, which resolves next to the
  bridge dylib. `build.rs` rpaths `swift-bridge/.build/release/` where SwiftPM
  places both `libSwiftTermBridge.dylib` and `SwiftTerm_SwiftTerm.bundle`. If you
  move the dylib, move the bundle with it or `st_set_use_metal` returns 0.
- **Main thread only.** Every `st_*` call must run on the AppKit main thread
  (where gpui's platform loop runs). All call sites are inside gpui render /
  main-thread closures.
- **Determinism.** The workload is a fixed-seed xorshift; the same seed yields a
  bit-identical stream across the PoC and the Nice Dev baseline. Dump it with
  `NICE_POC_FIXTURE=…`.

## Provenance
- SwiftTerm fork: `/Users/nick/Projects/SwiftTerm` @ `2f2a0b727feaa0d51659a9aaa21d47d752a16e0b` (read-only).
- gpui `0.2.2` (crates.io — since 2026-07-01 **vendored + patched**, see below), objc2 `0.6`,
  objc2-app-kit/foundation `0.3`, raw-window-handle `0.6`.
- Extends: `../spike-gpui-glass/glassdemo` (window/vibrancy/traffic-lights),
  `../spike-reuse-swiftterm` (objc2 NSView host + @_cdecl C-ABI dylib pattern).

---

# 2026-07-01 spike prep — vendored gpui, spike 3 txn toggle, present signpost, spike 8 multi-session

Build-only prep for §13 spikes 3 and 8 plus the keystroke-latency present
signpost. Everything below compiles headless; every live run stays
display-gated behind `NICE_POC_RUN=1`.

## Vendored gpui (required to build since 2026-07-01)

`Cargo.toml` patches `gpui` to a repo-local copy:

```toml
[patch.crates-io]
gpui = { path = "vendor/gpui-0.2.2" }
```

`vendor/` is **gitignored** (~8 MB / ~185 files). The committed source of truth is:

- `gpui-0.2.2-nice.patch` — the full diff vs the pristine crates.io 0.2.2
  extraction (3 files: `build.rs`, `src/platform/mac/metal_renderer.rs`, new
  `src/platform/mac/nice_signpost.c`);
- `vendor-gpui.sh` — copies the pristine extraction out of
  `~/.cargo/registry/src/*/gpui-0.2.2` (or the `.crate` tarball in
  `registry/cache`) into `vendor/gpui-0.2.2` and applies the patch.

Fresh checkout ⇒ run `./vendor-gpui.sh` once, then `cargo build` as usual.
With the env vars below unset, the patched gpui is **behavior-identical** to
stock 0.2.2 (the txn override reads as `false`; the signpost is gated on
`os_signpost_enabled` and emits nothing unless a recorder is attached).

## Spike 3 — GPUI-side transactional present (`NICE_POC_GPUI_TXN`)

gpui 0.2.2 already ships the transactional-present machinery
(`metal_renderer.rs` draw path; `window.rs` toggles it transiently around
`display_layer`/`windowDidBecomeKey`). The patch adds a runtime override —
env **`NICE_POC_GPUI_TXN=1`** (also accepts `true`/`txn`) — that pins
`presents_with_transaction` ON for the whole run, so the ordinary
CVDisplayLink `step` frames also co-commit inside the CA transaction
(`commit → waitUntilScheduled → drawable.present()`).

> **Deliberate deviation from the tasking:** the GPUI-side switch is a
> **separate** env var, *not* overloading `NICE_POC_PRESENT=txn`. Reason:
> `NICE_POC_PRESENT=txn` already selects the **SwiftTerm-side** transactional
> present in the Path-A bin, and spike 3's A/B needs the two sides
> independently switchable (re-running "terminal txn alone" fresh is arm A).
> Defaults preserve every previously measured mode bit-for-bit.

Live A/B (user, with a display; SwiftTerm fork branch `phase0-txn-present`
checked out for the `txn` terminal mode, as before):

```sh
cd spikes/phase0-poc
# Arm A — terminal txn only (replicates the §10 measurement: p50 18.3 / p95 31 ms)
NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=txn cargo run --bin phase0-poc
# Arm B — terminal txn + GPUI txn (the audit's untested "~1-line GPUI-side fix")
NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=txn NICE_POC_GPUI_TXN=1 cargo run --bin phase0-poc
# Optional Path-B control — does forced txn change the single-stack cadence?
NICE_POC_RUN=1 cargo run --bin gpui-term
NICE_POC_RUN=1 NICE_POC_GPUI_TXN=1 cargo run --bin gpui-term
```

Read: if arm B's p95 tail collapses toward the ~17 ms single-stack line, the
only measured A-vs-B quality differentiator collapses with it (§13 spike 3).

## Present signpost (keystroke-latency harness contract)

The vendored gpui emits an os_signpost **interval** around every GPUI Metal
draw/present (`MetalRenderer::draw`), via a C shim using the real
`<os/signpost.h>` macros. **Exact names** (the injection/reduction harness
keys on these):

| Field | Value |
|---|---|
| subsystem | `dev.nickanderssohn.gpui-term` |
| category | `present` |
| signpost name | `Draw` |
| type | interval (`os_signpost_interval_begin`/`_end`, one per draw) |

Nice/SwiftTerm counterpart (for the symmetric reduction): subsystem
`org.tirania.SwiftTerm`, category `MetalProfile`, name `Metal.Draw`, gated on
`SWIFTTERM_PROFILE=1`.

Placement + semantics:

- Spans `MetalRenderer::draw` entry → return: `next_drawable()` acquire (can
  block on drawable availability/vsync) → instance-buffer acquire → encode →
  `commandBuffer.commit()` (→ `waitUntilScheduled` → `drawable.present()` in
  transactional mode). This is a **CPU-side draw-submission interval, NOT
  GPU-complete** — the same semantics as SwiftTerm's `Metal.Draw` (which spans
  its `draw(in:)` the same way, incl. the `currentDrawable` wait).
- **One semantic difference:** SwiftTerm's `Metal.Draw` includes
  `BuildDrawData` (terminal grid → vertex data) inside the interval; GPUI
  builds its `Scene` earlier (element layout/paint in `render()`), so the GPUI
  `Draw` covers encode+submit of a prebuilt scene. For keystroke→present
  latency use the interval **END** as the present stamp on both sides — those
  anchors are equivalent (post-encode commit, present scheduled/inline).
- Emitted by **both bins** (it is inside gpui): in `gpui-term` (single stack)
  the GPUI draw **is** the terminal present; in `phase0-poc` (Path A) it is
  the GPUI **chrome** composite only — the terminal side keeps SwiftTerm's own
  `Metal.Draw`.
- Multi-window runs interleave `Draw` intervals from all windows on the main
  thread (serialized, never nested); per-interval ids are generated, but the
  stream does not tag which window — single out w0 by cadence or run K=1 for
  per-window latency.
- No env var needed: gated on `os_signpost_enabled()`, zero-cost unless a
  recorder is attached. Capture with
  `xctrace record --template Logging --attach <pid>` (or `--launch`), then
  reduce the `os_signpost` table filtered on the subsystem above.

## Interactive keystroke-latency mode (spikes 4b/5 — the Path-B half)

The `Draw` signpost above is only a usable latency anchor if draws are
**damage-gated** (the harness caveat in `baseline/keystroke-harness/RUN.md`
§4: a per-RAF-frame Draw samples frame phase, not latency). This mode makes
that true by construction:

```sh
cd spikes/phase0-poc
# LIVE (display-gated; release build for real numbers):
NICE_POC_RUN=1 NICE_POC_INTERACTIVE=1 cargo run --release --bin gpui-term
# optional: NICE_POC_SECS=<n> (default 120) — auto-exit + one-line summary
# HEADLESS self-test (safe anywhere; no window):
NICE_POC_INTERACTIVE=1 cargo run --bin gpui-term
```

What it does (all in `src/gpui_term.rs`):

- **One window, NO synthetic workload, NO RAF loop** — `render()` never calls
  `request_animation_frame`. The multi-session flags are ignored in this mode.
- **Real pty:** `alacritty_terminal::tty` spawns **`/bin/cat`** behind an
  `openpty` pair (`PtySession`). Canonical mode + ECHO stay at kernel
  defaults, so every typed byte is echoed by the tty line discipline
  immediately (and cat re-emits the whole line after Return) — the closest
  analog to the Nice Dev baseline's zsh+`cat` loopback. (Fallback if cat ever
  misbehaves: `/bin/zsh -c 'exec cat'`.)
- **Key path:** GPUI `on_key_down` on a focused div → `keystroke_bytes()`
  (plain printables via `Keystroke.key_char`, plus enter→`\r`, tab, space,
  backspace→0x7f; **no kitty/CSI-u encoder**; modified keys ignored) → raw
  `write` to the pty master. The window is activated (`cx.activate(true)`)
  and the div focused at open, so externally-activated typing (harness:
  activate the app, then `CGEventPostToPid` keycode 0 + unicode 'a') lands
  without a click.
- **Echo path (present-kick fix, 2026-07-01, after the first live run):**
  blocking pty reader thread → parse into `FairMutex<Term>` → byte counter +
  dirty flag → unbounded-channel wakeup → foreground task does `cx.notify()`
  **plus a platform present kick**. The kick is load-bearing: in gpui 0.2.2
  `cx.notify()` only rebuilds the scene (app-side `Window::draw`, which sets
  `needs_present`) — the Metal present runs *only* from the platform
  request-frame path (CVDisplayLink `step` / `displayLayer:`), and gpui
  **stops the display link for occluded windows** — so notify alone produced
  505 scene rebuilds and **zero** Metal draws in the first live run. The kick
  (`kick_platform_display`) marks the GPUI NSView **and its backing
  CAMetalLayer** (`setNeedsDisplay`) so the next CA commit fires
  `displayLayer:` → gpui's request-frame → `Window::present()` →
  `MetalRenderer::draw` → **one `Draw` signpost per echo**, independent of
  the display-link state. (That path presents transactionally — gpui's own
  `displayLayer:` behavior.) A channel, not a poller, so the wakeup adds no
  quantization to the measured latency.
- **Keepalive present disabled** (`NICE_POC_DAMAGE_ONLY=1`, auto-set by this
  mode; a vendored-gpui knob in core `window.rs`): stock gpui keeps
  presenting the *unchanged* scene at refresh rate for **1 s after every
  input** ("prevent the display from underclocking") whenever the display
  link runs — during continuous typing that is a 60 Hz `Draw` spray that
  samples frame phase, not latency. With the knob set, presents happen only
  on damage (`needs_present`/dirty). Env unset ⇒ stock behavior (all other
  modes unaffected).
- **Occlusion (for arranging the live run):** stock gpui stops its
  CVDisplayLink when the window is **fully occluded**
  (`windowDidChangeOcclusionState`) — consistent with the first run's zero
  presents if the window sat behind others. The kick does **not** depend on
  the display link and fires even for an occluded window (CA commits still
  run app-side). Residual caveat: a fully-occluded `CAMetalLayer` *may*
  recycle drawables lazily (pool of 3, `allowsNextDrawableTimeout=NO` ⇒
  `next_drawable` blocks rather than drops) — safest is the window at least
  partially visible; the summary's `metal draws` count will immediately show
  if occlusion still gated anything.
- **Cursor/blink:** this bin renders **no cursor at all** (grid text + cell
  backgrounds only) and owns **no timers** — zero timer-driven draws exist.
  Remaining non-echo draw sources are OS-initiated only (window open, resize,
  occlusion/appearance change, activation) — and **mouse movement over the
  window** can trigger extra renders via GPUI's hitbox tracking, so keep the
  pointer parked outside the window during a measurement run.
- **Exit:** after `NICE_POC_SECS` (a timer, not a render-path check — a
  demand-driven window may not render for long stretches), prints one line:
  **metal draws** (real `MetalRenderer::draw` count from the C shim — the
  honest present number), scene rebuilds (Element renders), pty bytes echoed,
  keys sent. Healthy run: `metal draws ≈ keys + a few` (window-open/
  activation presents). `scene rebuilds` high while `metal draws` ~0 is the
  failed pattern.

Headless verification (`NICE_POC_INTERACTIVE=1` without `NICE_POC_RUN`):
spawns the same pty, writes `hello\r`, asserts ≥12 bytes came back (kernel
echo + cat's line), the dirty flag was set, and the parsed grid contains
"hello" twice. Verified PASS in debug and release (bytes_echoed=14). The
present kick itself is display-gated — verify live via the summary counts
and xctrace (`Draw` rows ≈ key count).

## Spike 8 — multi-session gpui-term (pump off the render path)

Restructure (in `src/gpui_term.rs`): the old `render()`-inline
`pump()` (workload generate + `Processor::advance` **on the main thread**,
old lines 286–289) is gone. Each session now owns a **feeder thread**
(`Session::spawn`) that parses the deterministic workload into a shared
`FairMutex<Term>` paced by wall clock (5 ms ticks at the profile byte rate —
same ~500 KB/s aggregate as before); `render()` only takes a short lock to
snapshot. Idle windows redraw on demand via a dirty-flag → `cx.notify()`
poller (~10 Hz) instead of RAF; streaming windows keep the RAF loop as the
measurement clock, unchanged.

Live-run flags (all optional; defaults = the original single-window run):

| Var | Default | Effect |
|---|---|---|
| `NICE_POC_WINDOWS=K` | `1` | open K windows (clamped 1..=16), cascaded |
| `NICE_POC_STREAMING=M` | `K` | first M windows stream the full workload + RAF-render; w0 is always streaming (it is the coordinator: deadline, memory, report) |
| `NICE_POC_BG_BPS=N` | `0` | byte rate of the K−M background sessions; `0` = idle-with-live-session (1 heartbeat line/s, demand-driven redraw) |

Per-window seeds are `seed + window_index` (deterministic, distinct streams;
w0 keeps the original stream). Every session = its own feeder thread, so
K−M background sessions are literally "background parsers off the main
thread" (§13 spike 8).

```sh
cd spikes/phase0-poc
# Spike 8 target scenario: 3 streaming + 4 idle-with-live-session = 7 windows
NICE_POC_RUN=1 NICE_POC_WINDOWS=7 NICE_POC_STREAMING=3 cargo run --bin gpui-term
# Harder variant: the 4 background parsers churn at FULL workload rate off-main
NICE_POC_RUN=1 NICE_POC_WINDOWS=7 NICE_POC_STREAMING=3 NICE_POC_BG_BPS=500000 cargo run --bin gpui-term
# Spike 6 pairing — release build (already built): add --release to any of the above
NICE_POC_RUN=1 NICE_POC_WINDOWS=7 NICE_POC_STREAMING=3 cargo run --release --bin gpui-term
```

Output: the single-window summary/CSV are unchanged when `K=1`
(`gpui-term-gpui-native-single-stack.csv`). With `K>1` the report adds a
`-- per-window cadence (spike 8) --` block (per-window p50/p95/p99/cliffs +
bytes fed) and writes `gpui-term-multi-<K>w<M>s.csv` with the window index in
the `stack` column (`gpui-native-w<N>-stream` / `gpui-native-w<N>-bg`).
Memory numbers are process-wide (all sessions live in one process).

Known limits (spike-grade, by design): the measurement start is w0's first
frame (other windows' warm-up frames are cleared then, but their streams may
start a frame or two apart); background-window frame intervals track the
heartbeat/notify cadence, not fps; a background window's dirty flag can be
consumed by its own render racing the poller (at most one delayed redraw).
