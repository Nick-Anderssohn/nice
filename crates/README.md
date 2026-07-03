# Nice rewrite — Rust + GPUI workspace

This is the permanent home of the Nice rewrite (decision report:
`../notes/rewrite-stack-research.md`, Path B — all-Rust, single Metal stack
via [GPUI](https://www.gpui.rs/), zed's UI framework). It coexists with the
Swift app at the repo root; nothing Swift moves, and this workspace never
builds, installs, or touches `/Applications/Nice.app` or
`/Applications/Nice Dev.app`.

The roadmap for the rewrite lives at
`../notes/rewrite-feature-roadmap-20260702.md`; this file documents the
workspace as it exists, and every later cycle that adds a crate or a
self-test scenario should extend it in place rather than leaving the map
stale.

## Crate map

```
crates/
  nice           — the app binary (GPUI). Process name `nice-rs`.
  nice-harness   — measurement + self-test library. No app logic lives here.
```

### `crates/nice` (bin `nice-rs`)

The GPUI application. Structure (grows over later cycles):

- `app` — owns window creation and the root view: the shipped window (one
  static "Nice RS Dev" window, solid background + version line) and the
  self-test scenario windows (animated root view, registered in
  `selftest_scenarios()`).
- `platform` — the single home for foreign AppKit / `objc2` access (see
  "All-Rust rule" below). R1 holds exactly one thing here: the demand-present
  kick (`present_kick`) plus the two present-timing facts that motivate it
  (see the doc comment on that module — every later cycle adding
  demand-driven repaint needs to know them).
- `main.rs` — dispatches on `NICE_RS_SELFTEST`: unset runs the normal app,
  set runs the self-test driver.

### `crates/nice-harness` (lib)

The measurement + self-test library every later cycle reuses. Modules:

- `clock` — monotonic mach clock (`mach_absolute_time` + timebase), the
  single time source for every frame stamp and measurement.
- `mem` — `task_info(TASK_VM_INFO)` `phys_footprint` + `resident_size`
  sampler (hand-declared `struct task_vm_info`; `mach2` 0.4 doesn't ship it).
- `signpost` — `os_signpost` emission on subsystem
  `dev.nickanderssohn.nice-rs` (category `selftest`, name `Frame`). The
  actual emission is a C shim (`src/signpost.c`, compiled + linked by
  `build.rs`) because the `os_signpost` macros must run from C to place
  their strings in the `__TEXT` sections Instruments reads.
- `frame` — the frame-stamp stream, the percentile reducer (p50/p95/p99 over
  frame intervals), and the cadence gate (`assess_cadence`): passes when a
  scenario sustains enough frames and p95 interval `< 2×` the median.
- `watchdog` — an App-Nap-immune OS-thread deadline. macOS App Nap
  indefinitely defers coalescable timers in an idle, occluded gpui app (a 60s
  libdispatch deadline was observed not firing within 8 minutes — phase-0
  spike-6 finding), so self-test exit logic cannot rely on a coalescable
  timer or the gpui render path. The watchdog sleeps on a dedicated OS thread
  in drift-corrected slices, then forces the deadline callback onto the main
  thread via `dispatch_async_f` + `CFRunLoopWakeUp` (both immune to App Nap),
  and hard-exits(3) if the main thread still hasn't serviced it ~20s later.
  One watchdog per process; `arm()` must be called on the main thread.
- `capture` — screenshot capture via `Window::render_to_image()`, behind the
  `capture` cargo feature (see "Screenshot capture" below).
- `selftest` — the `NICE_RS_SELFTEST` driver + `all` suite runner, and the
  `Scenario` registry later cycles extend (see "Self-test scenarios" below).

## Layering rule

**Crates that mirror today's pure-Swift model code must not depend on
`gpui`.** That purity is what made the Swift model layer painless to test and
reason about (`../notes/chrome-pain-catalog-20260702.md` §2), and the rewrite
means to keep it. `nice-harness` is not one of those crates — it is
inherently a gpui/measurement library (it drives and inspects real gpui
windows) — so it depends on `gpui` directly. When a later cycle adds a model
crate (parsing, session state, config, anything that doesn't paint pixels),
it must NOT gain a `gpui` dependency; if it needs to talk to the UI layer,
that's a sign the boundary belongs in `crates/nice` instead.

## All-Rust rule

Path B means no Swift sources and no second language toolchain in this
workspace. Foreign AppKit access, when unavoidable, goes through `objc2` /
`objc2-app-kit` and lives behind exactly one platform module per binary
crate (`crates/nice/src/platform.rs` today). Don't scatter `objc2` calls
across view/business logic — add to `platform.rs`, or add a sibling
`platform` module in a new binary crate if one appears later.

## Vendoring GPUI: pin, patch, and provenance

GPUI is **not** a workspace member — it's vendored via a pinned git checkout
under `vendor/zed/` (gitignored, not committed; ~1 GB). The crates below
path-depend into it:

```toml
gpui          = { path = "../../vendor/zed/crates/gpui" }
gpui_platform = { path = "../../vendor/zed/crates/gpui_platform", features = ["font-kit"] }
gpui_macos    = { path = "../../vendor/zed/crates/gpui_macos" }
```

**Pin:** zed main revision `10b07951838e422722e34641f4a9c0bfec9037ff`, plus
the bg-luminance patch (`../patches/zed-bg-luminance.patch` — the phase-0
spike's closure patch that makes GPUI text anti-aliasing match SwiftTerm on
pixels; 65+/7− across 6 zed files). The patch file was copied byte-identical
(sha256-verified) from
`../spikes/phase0-poc/aa-gamma/bg-luminance-applied.patch` and must never be
hand-edited — regenerate and re-copy it from the spike if it ever needs to
change.

`crates.io` publishes `gpui 0.2.2`; that crate is **spike-only** and must
never be used for production code in this workspace — the pin above is the
only source of truth. **Changing the pin or dropping the patch is a human
decision, not something a later cycle or the reconciler should do silently.**

**Reproducing the checkout:** run `../scripts/vendor-zed.sh` (idempotent —
safe to re-run; a second run with the pin already checked out and patched is
a fast no-op). It:

1. Maintains a shared bare mirror at `~/.cache/nice/zed-mirror.git` (cloned
   from `zed-industries/zed` once; `git fetch`ed only when the pin is
   missing — override the mirror path with `NICE_ZED_MIRROR`).
2. Local-clones (hardlinked objects, cheap) the mirror into `vendor/zed`.
3. Checks out the pinned revision (detached).
4. Applies `patches/zed-bg-luminance.patch`, using a marker file
   (`vendor/zed/.nice-bg-luminance-applied`) plus `git apply --check` so a
   second run doesn't try to re-apply an already-patched tree.

Add `exclude = ["vendor"]` to the root `Cargo.toml` is **load-bearing**:
`vendor/zed` is itself a cargo workspace, and without the exclude, cargo
would try to auto-attach it as a member of *this* workspace.

**Licensing — binding, read before touching anything under `vendor/zed`:**
Zed's `crates/terminal`, `crates/terminal_view`, and the Zed app-layer crates
(`crates/title_bar`, `crates/workspace`, `crates/editor`, …) are
**GPL-3.0-or-later**. Never open, read, copy, or feed them to code
generation — not even "for reference." The allowed reference/reuse surface
is `vendor/zed/crates/gpui`, `gpui_platform`, `gpui_macos`, `gpui_macros`
(Apache-2.0 — verify a crate's license file before reading anything else in
the zed tree). Nice is MIT and publicly distributed; GPL taint is
unshippable. See the R1 plan's "Ground rules" section for the full allowed
list (alacritty frontend code, termwiz, gpui-ghostty, gpui-component,
sixel-image/sixel-tokenizer).

## Self-test harness

### Env contract

| Env var | Effect |
|---|---|
| `NICE_RS_SELFTEST=<scenario>` | Run one named scenario. Prints exactly `SELFTEST PASS <scenario>` and exits 0 on success, or `SELFTEST FAIL <scenario>` (+ a detail line on stderr) and exits nonzero on failure. |
| `NICE_RS_SELFTEST=all` | Run every registered scenario sequentially. Prints a PASS/FAIL table, exits nonzero if any scenario failed. This is the standing UI regression gate — every later plan's validation re-runs it, so a later cycle cannot silently break an earlier scenario. |
| `NICE_RS_SELFTEST_SECS=<f64>` | Override the per-scenario measurement window (default 2.5s). Applies after a fixed 0.5s warm-up that's always discarded. |
| `NICE_RS_CAPTURE=<path>` | Additionally write a PNG of each scenario's window to `<path>`. Requires building `crates/nice` with `--features selftest` (see "Screenshot capture" below) — without it, capture is a hard error, not a silent no-op. |

The whole self-test run — every scenario, in sequence — happens inside a
single `Application::run` call (`nice_harness::selftest::drive`, invoked from
`crates/nice`'s `run_selftest`). The driver activates the app so scenario
windows are frontmost and focused (see "Why frontmost & focused" below),
arms the watchdog, then spawns one async orchestrator that opens each
scenario's window in turn, warms up, measures, optionally captures, closes
the window, and moves to the next scenario.

### Registered scenarios

| Name | What it exercises |
|---|---|
| `smoke` | Opens the window, drives continuous animated repaint via `request_animation_frame`, and asserts frame-cadence sanity (`p95 < 2× median` interval, at least 30 sampled frames). The minimal "the window opens and paints at a sane cadence" gate. |

Later cycles add scenarios by pushing onto the `Vec<Scenario>` returned from
`crates/nice/src/app.rs`'s `selftest_scenarios()` — the driver, reducer,
watchdog, and table-printing in `nice-harness` never need to change. Each
scenario supplies an `open: fn(&mut AsyncApp) -> anyhow::Result<AnyWindowHandle>`
whose view stamps a frame (`nice_harness::frame::stamp()`) and requests the
next animation frame on every render, so the harness can measure cadence.
**Keep this table in sync** — it's the map a future cycle (or a reconciler)
reads to know what regression coverage already exists before adding more.

### Why frontmost & focused

Two present-timing facts about the pinned zed-main revision govern every
scenario (documented in code at `crates/nice-harness/src/frame.rs` and
`crates/nice/src/platform.rs`):

1. `cx.notify()` alone never **presents** while a window's CVDisplayLink is
   stopped (gpui stops it on occlusion). A demand-driven repaint on an
   occluded window needs an explicit `setNeedsDisplay` kick to the `NSView`
   + its `CAMetalLayer` — that's `platform::present_kick`. The `smoke`
   scenario sidesteps this by driving continuous RAF repaints on a visible
   window; later demand-driven scenarios must issue the kick themselves.
2. zed-main frame-caps **inactive** windows at ~33ms (`min_frame_interval`),
   so a backgrounded window animates at ~30fps regardless of the panel
   refresh rate. Frame-cadence assertions must therefore run on a
   frontmost, focused window — which is why `selftest::drive` calls
   `cx.activate(true)` and why any manual self-test run needs the app in the
   foreground.

### Screenshot capture

`Window::render_to_image()` is public but gated
`#[cfg(any(test, feature = "test-support"))]` in gpui; the macOS renderer
implements it by reading the drawable texture back, which requires
`CAMetalLayer.framebufferOnly = false` — a flag `gpui_macos` only clears
under that same cfg, **process-wide**. Turning it on for the shipped app
would leave the live window's Metal layer non-framebuffer-only forever, so
capture is entirely opt-in via a cargo feature:

- `crates/nice`'s `selftest` feature is what you build with to get capture:
  `cargo build -p nice --features selftest` (or
  `cargo run -p nice --features selftest`).
- It forwards to **two** features that are both load-bearing:
  - `nice-harness/capture` → `gpui/test-support` — compiles the outer
    `Window::render_to_image()` method + the PNG encoder (`image` crate).
  - `gpui_platform/test-support` → `gpui_macos/test-support` — compiles the
    macOS `MacWindow::render_to_image` **override** (the one that actually
    reads the drawable texture). Without this half, the default trait impl
    bails with "render_to_image not implemented for this platform" even
    though the outer method compiled.
- The shipped bundle (`scripts/rust-bundle.sh`, no `--features`) omits both,
  so the live app's Metal layer stays framebuffer-only.
- We deliberately do **not** use `VisualTestAppContext::capture_screenshot`
  for this — that's a `TestDispatcher` context (off-screen windows,
  deterministic scheduling) and would invalidate the live cadence
  assertions the same scenarios make. Capture always runs against the real,
  on-screen window.

Perf thresholds (the cadence gate) were measured with `test-support` on in
the phase-0 spike, so they stay comparable whether or not `--features
selftest` is set.

## Running the self-tests

From the repo root, on a Mac with a GUI session (the app window must become
frontmost — see above):

```sh
# one scenario
NICE_RS_SELFTEST=smoke cargo run -p nice

# the full regression suite
NICE_RS_SELFTEST=all cargo run -p nice

# with a screenshot capture (needs the selftest feature)
NICE_RS_SELFTEST=smoke NICE_RS_CAPTURE=/tmp/nice-rs-smoke.png \
    cargo run -p nice --features selftest
```

Ordinary build/test commands:

```sh
cargo build --workspace          # debug build, all crates
cargo test --workspace           # unit tests
cargo build --workspace --release  # perf-gated validations should use this
```

The first build in a fresh worktree is a cold build of the whole gpui
dependency stack (after `scripts/vendor-zed.sh` has produced `vendor/zed/`)
— several minutes is normal, not a hang. `[profile.dev.package."*"]` in the
root `Cargo.toml` builds dependencies at opt-level 2 even in dev builds so
this cost is paid once per dependency version, not on every iteration of
your own code (which stays opt-level 0 for fast rebuilds).

## Bundling + installing

```sh
scripts/rust-bundle.sh    # cargo build --release -p nice, assemble + ad-hoc
                           # codesign build-rs/Nice RS Dev.app, verify
scripts/rust-install.sh   # (re)builds via rust-bundle.sh, force-quits a
                           # running nice-rs, installs to
                           # /Applications/Nice RS Dev.app
```

App identity (deliberately distinct from both Swift installs so nothing
collides in `/Applications`, UserDefaults, or process-name greps — renaming
to `Nice.app` happens at parity, Stage 8, not now):

| | |
|---|---|
| Bundle | `Nice RS Dev.app` |
| Bundle id | `dev.nickanderssohn.nice-rs-dev` |
| Display name | `Nice RS Dev` |
| Executable / process name | `nice-rs` |

Signing is **ad-hoc only** (`codesign -s -`), verified with
`codesign --verify --deep --strict`. This is deliberate and recorded, not an
oversight: R1 promises local installability, nothing more. Notarization and
release-CI wiring are Stage 8 (R27-adjacent) work — see the header comment
in `scripts/rust-bundle.sh` and the R1 plan's "Binding technical decisions."
**Do not** add Developer ID signing / notarytool / stapling to these scripts
before Stage 8.

`scripts/rust-install.sh` only ever touches
`/Applications/Nice RS Dev.app` — it has no flag that points it at
`/Applications/Nice.app` or `/Applications/Nice Dev.app` (the Swift builds).
Its running-instance detection uses `ps -Aww -o pid=,args=` + a path-scoped
grep, never `pgrep`/`pkill -f` (macOS truncates a GUI app's `comm` to 16
chars, which makes `pgrep`/`pkill -f` silently miss a running instance), and
it force-quits with SIGTERM → poll → SIGKILL rather than an AppleScript
`quit` (which would raise a confirmation dialog and stall an unattended
install) — mirroring `../scripts/install.sh`'s approach for the Swift `Nice
Dev` build as of commit `2c08c51`.
