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
  cadence + the deterministic latency loop, **not** GPU-complete.
- For true GPU-complete timing, run with `SWIFTTERM_PROFILE=1` and stream the
  fork's existing `OSSignposter` "Metal.Draw" interval (subsystem
  `org.tirania.SwiftTerm`, category `MetalProfile`). Use the **same** source on
  the baseline side for like-for-like (`baseline/NOTES.md`).

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
- gpui `0.2.2` (crates.io), objc2 `0.6`, objc2-app-kit/foundation `0.3`, raw-window-handle `0.6`.
- Extends: `../spike-gpui-glass/glassdemo` (window/vibrancy/traffic-lights),
  `../spike-reuse-swiftterm` (objc2 NSView host + @_cdecl C-ABI dylib pattern).
