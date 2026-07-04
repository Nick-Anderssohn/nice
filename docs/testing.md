# Testing the Rust rewrite: where a test belongs

This is the placement rulebook for every test written against the `crates/`
workspace, from R9 onward. It names three layers, gives decision rules for
which one a new test belongs in, and records two conventions that keep the
layers honest: the differential-pair rule and the AX decision record. It does
not re-derive the self-test scenario table or the fixture API in full — see
`crates/README.md`'s "Self-test harness" section and `nice-itests`'s own doc
comments for those; this file is the map that tells you which of those docs
to go read.

## The three layers

### 1. Unit — `cargo test`, per crate

Plain Rust tests, no window, no gpui (except where a crate's own unit tests
exercise gpui-free logic it owns). Every gpui-free model crate
(`nice-model`, `nice-theme`, `nice-term-core`, `nice-term-input`) carries its
tests this way — pure functions and value types, `cargo test -p <crate>`,
fully deterministic, no display needed, safe in CI once CI exists. This is
the cheapest layer and should hold the most tests: anything that doesn't
need a mounted view or a real window belongs here, not one layer up.

### 2. In-process integration — `cargo test -p nice-itests`

The `nice-itests` crate (landed in this plan's first slice; see its crate-doc
and `crates/README.md`). It splits into two execution models that must never
be conflated:

- **Behavior tests — mocked `gpui::TestAppContext`.** `TestPlatform` +
  `NoopTextSystem`: no Metal, no pixels, deterministic scheduling. Ordinary
  libtest `#[gpui::test]` cases; they may parallelize under `cargo test`.
  Right for focus, dispatch, entity behavior, and byte-exact input encoding —
  anything that needs a mounted view and gpui's real event-dispatch path but
  never needs to read a pixel back. Boot/mount/input-driver fixtures live in
  `nice_itests::behavior` (gated on `cfg(test)` / the crate's `test-support`
  feature); exemplars in `src/behavior_exemplars.rs`.
- **Visual/pixel tests — real-MacPlatform `VisualTestAppContext`.** The real
  `MacPlatform` wrapped in a `TestDispatcher`: real Metal rendering into an
  off-screen window (placed at −10000,−10000, unfocused — no stolen focus, no
  TCC prompt), `capture_screenshot`, a **simulated clock**. These cannot run
  under libtest — real `NSWindow`s are main-thread-only, libtest runs cases on
  worker threads, and there is no `#[gpui::visual_test]` macro at the pin — so
  they live in one or more `harness = false` integration binaries whose `main`
  owns the platform on the process main thread and runs cases serially,
  exiting nonzero on failure. `cargo test -p nice-itests` still builds and
  runs those binaries and gates on their exit code, exactly like a libtest
  case. Exemplar: `tests/visual_terminal_screenshot.rs`.

Shared, execution-model-agnostic fixtures (`nice_itests::pixels`,
`nice_itests::session`) carry no gpui dependency at all: the `±8/255`
per-channel pixel-tolerance convention and the T4 bottom-anchored
cell-centre geometry (`pixels`), and fixture-session builders + capture-file
pollers reusing the live suite's byte-piped `cat`/`ZDOTDIR` and raw-mode
`tee` patterns (`session`). A new visual harness binary in a downstream
crate reuses these two modules and writes its own short `VisualTestAppContext`
boot inline (the boot helpers in `behavior` are gated and invisible to a
plain integration binary) — see the header comment on
`tests/visual_terminal_screenshot.rs` for why that's the intended shape, not
a gap.

Both execution models run on a **simulated clock**. Neither may ever assert
frames-per-second, frame-pacing percentiles, or wall-clock latency — see
"What in-process tests must never assert" below.

### 3. Live ground truth — `NICE_RS_SELFTEST`

The `nice-harness`-driven suite (`crates/nice-harness/src/selftest.rs` +
scenarios registered in `crates/nice/src/app.rs`'s `selftest_scenarios()`).
Real windowserver, real `CVDisplayLink`, real CGEvents posted to the app's
own pid, a real Accessibility grant where the scenario needs one. This is
the **only** place cadence, absolute frame-time/memory budgets, real focus
transitions, and CGEvent/IME round-trips are asserted — see
`crates/README.md`'s "Self-test harness" section for the full scenario
table, the env-var contract, and the screenshot-capture feature gating. It
remains the ground-truth floor for the whole test story: in-process tests
complement it, never replace it, and every plan's `## Validation` re-runs
`NICE_RS_SELFTEST=all` as the standing regression gate.

Every windowed live scenario now carries the **self-activation guarantee**
(landed in this plan's second slice): before the driver measures, it drives
the scenario's window frontmost + key and *asserts* that via
`Window::is_window_active`, re-issuing `activate` each 100 ms tick up to a
5 s bound. A scenario that never becomes frontmost within that bound FAILs
with an actionable message ("window could not become frontmost — is another
app fullscreen or occupying the display Space? Free the screen and
re-run.") instead of silently measuring an inactive, frame-capped window and
reporting a mystifying "0 frames" — the failure mode that used to hit
`smoke`/`tokens`/`term-render`/`term-layout`/`term-scroll` whenever the host
session's display Space was occupied. This is a `Scenario::activate: bool`
opt-out seam (`true` for every windowed scenario today; `false` is reserved
for a hypothetical deliberately-background scenario, none of which exist),
not a per-scenario behavior any new scenario has to remember to add — see
`Scenario::activate`'s doc comment in `selftest.rs`.

## Decision rules: where does a new test belong?

Work through these in order; the first one that applies decides the layer.

1. **Does the assertion involve cadence, an absolute frame-time/memory
   budget, wall-clock latency, or anything measured against a real
   `CVDisplayLink`?** → Layer 3 (live). Never layer 2 — see "What in-process
   tests must never assert" below.
2. **Does it need a real CGEvent (keyboard/mouse), the macOS Accessibility
   grant, a real TIS input source (IME), or a real drag/drop session?** →
   Layer 3. `nice-itests`' behavior context drives gpui's *simulated*
   key/mouse dispatch (real dispatch code path, but no real HID event), which
   is the right tool for "does the view react correctly to this input" —
   but "does a real OS-level event reach this process and this pty" is only
   trustworthy live (the A/B program's standing finding: self-reported /
   simulated evidence is untrustworthy on exactly this class of claim).
3. **Does it need to read back a rendered pixel** (a color, a swatch, a
   layout position visible only in a captured frame)? → Layer 2, visual
   context (`VisualTestAppContext`, a `harness = false` binary) — unless it
   also needs (1) or (2), in which case it's layer 3. Don't reach for a live
   scenario just to check a pixel; the visual context's off-screen window is
   cheaper, steals no focus, and prompts no TCC.
4. **Does it need a mounted view + gpui's real focus/dispatch/entity
   machinery, but never touches a pixel or a real OS event?** → Layer 2,
   behavior context (`TestAppContext`). This is the default for new
   chrome/pane-strip interaction tests (R9–R13): mount the view, drive
   simulated input, assert state (which handler fired, which entity
   mutated, what the encoder emitted) — not a screenshot.
5. **Is it a pure function or value type with no window at all** (a
   resolver, a parser, an encoder, a state-machine transition)? → Layer 1,
   in the crate that owns the type. If you're tempted to mount a view just
   to exercise logic that doesn't need one, don't — push the logic down into
   a gpui-free crate (per the Layering rule in `crates/README.md`) and unit
   test it there instead.

A test that seems to need a live scenario's assertion (rule 1 or 2) but is
mostly about UI plumbing (rule 3 or 4) usually means the real bug surface is
smaller than the whole scenario: write the plumbing assertion at layer 2 and
the one genuinely-live claim (the classification outcome, the byte-exact
receipt, the frame budget) at layer 3 — see the differential-pair convention
next, which formalizes exactly this split.

### What in-process tests must never assert

Both `nice-itests` execution models run on a simulated clock (the mocked
context has no display link at all; the visual context's `TestDispatcher`
drives a simulated one). **Neither may assert frames-per-second, frame-pacing
percentiles, or wall-clock latency.** The Path-B tractability A/B proved
self-reported and simulated evidence is untrustworthy on exactly this class
of claim — a scenario that "passes" on a scheduler that never actually
contends for the real render thread tells you nothing about real cadence.
Cadence/perf/latency gates live only in layer 3. **A cadence or perf
assertion inside `nice-itests` is a blocking review finding**, full stop —
this line is repeated in the crate's own doc comments and in the T2 plan's
binding technical decisions; treat any PR that adds one as a bug in the
test, not a new capability.

## The differential-pair convention

From `notes/chrome-pain-catalog-20260702.md`: the recurring pattern behind
the Swift app's worst chrome bugs (window-drag arbitration eating a pill
click, a tear-off gesture also moving the window, a double-click zoom firing
during a drag) was a test suite that only checked "the new interaction
works" and never checked "the thing that must NOT have happened, didn't."
Compiles-and-unit-passes was repeatedly consistent with behaviorally wrong;
the project's own eventual fix was to pair every acceptance test with its
counterfactual.

The convention for this workspace: **for any seam-y interaction — anything
where two handlers could plausibly both claim the same press/drag/event —
write the test as a pair, not a single assertion.**

- The **positive** half asserts the intended outcome happened (e.g. "the
  pill click selected the pane").
- The **negative** half asserts the specific thing that must not have
  happened alongside it (e.g. "the window did not move").

Where the negative half lives depends on what it's actually checking, via
the same decision rules above:

- **In-process tests assert the classification outcome** — e.g. "the
  drag-arm handler never fired" / "the click was classified as a
  pane-select, not a window-drag" — a state/dispatch assertion inside the
  behavior context. This is almost always what an R9–R13 chrome test wants:
  it's cheap, parallel, and pins the *decision logic* the seam bug always
  turned out to be about.
- **Only live scenarios assert real window-frame motion** — e.g. "the
  window's actual on-screen frame is unchanged after the drag" is a claim
  about the real windowserver, not the mocked/visual context's simulated
  platform, and belongs at layer 3 if it's ever needed as a true ground-truth
  check. Don't fake this at layer 2 by asserting on a value the mocked
  platform happens to expose; if the pin doesn't allow trustworthy `NSWindow`
  frame assertions off-screen, that's a signal for a live scenario, not an
  in-process approximation.

Every R9–R13 chrome/pane-strip test plan must follow this convention for its
seam-y interactions — reviewers should treat "asserts the action worked" with
no adjacent "and the counterfactual didn't happen" as an incomplete test for
anything drag/click-arbitration-shaped.

## Environmental preconditions for live runs

The live suite talks to real macOS subsystems, so a live run (a single
scenario or `NICE_RS_SELFTEST=all`) depends on host state a purely in-process
run never needs:

- **Accessibility grant.** Any scenario that posts a real CGEvent
  (`input-live`, `input-shell`, `niceties-zoom`, `niceties-held`) preflights
  `AXIsProcessTrusted()` and FAILs loudly with remediation if the grant is
  missing — never silently skips. Grant it once to whichever build posts the
  events (`nice-rs` under dev, or your terminal if you're driving `cargo run`
  from one) via System Settings → Privacy & Security → Accessibility.
- **Frontmost, focused window.** Every windowed scenario now self-activates
  (see "The self-activation guarantee" above), so a live run no longer
  requires the caller to manually arrange windows — but the machine's
  display Space still needs to be **free** (no other app fullscreen /
  occupying the Space) for activation to succeed within its 5 s bound. If a
  live run fails with the activation-preamble message, that's environmental,
  not a regression: free the screen and re-run.
- **`disable_font_smoothing()` before the first glyph.** `crate::platform`
  writes `AppleFontSmoothing = 0` into the app's own preferences domain
  before `Application::run`, so gpui skips its dilation pass and the
  bg-luminance patch is the sole antialiasing shaping. This is called once
  at startup (both the normal run path and the self-test path), not
  something a test arranges — but it's why `term-render`'s pixel assertions
  and the patch-engagement check are only trustworthy against a process that
  went through this normal startup, not an ad hoc window opened some other
  way.
- **`ZDOTDIR`-blanked shells.** Any fixture or live scenario that spawns a
  real `zsh` (fixture sessions in `nice-itests::session`, `term-render`,
  `input-shell`, `niceties-zoom`, `niceties-held`, …) points `ZDOTDIR` at an
  empty directory so no user `.zshrc`/`.zprofile` output pollutes the grid.
  Never assume a bare login shell starts clean on this or any other
  developer's machine.
- **Poll for readiness, never sleep-and-hope.** A pty child, a `tee`
  capture pipeline, or a real shell prompt all run on OS threads outside
  either gpui's simulated dispatcher or the live suite's frame loop — a
  fixed `sleep` before asserting is exactly the flakiness source the A/B
  program flagged. Every helper that waits on one of these (`nice_itests`'s
  `session::poll_capture_contains` / `poll_capture_after`, the live
  scenarios' grid-readiness polls) polls with a bounded, fail-loud timeout
  instead. Any new fixture that waits on real I/O must follow the same
  shape — a bare `sleep` before an assertion is a review finding.

## The AX decision record

**Finding (re-checked at pin `10b07951838e422722e34641f4a9c0bfec9037ff` on
2026-07-03/04, per `TRANCHE-2-NOTES.md` §1 and §4 of the T2 plan):**
AccessKit is live in gpui (`accesskit` 0.24 / `accesskit_macos` 0.26, a hard
dependency, rebuilding a per-frame tree), and an element is exposed to the
macOS Accessibility tree only when it carries **both** a global `.id()` and
a non-generic `.role()`; its `aria_label` maps to the macOS `AXTitle`. But
**gpui never sets `author_id`**, so the macOS
`accessibilityIdentifier`-based matching a black-box UI test would normally
want (`AXUIElement`'s `AXIdentifier`, matching gpui's own `.id()` string) is
**unreachable without a vendor patch**.

**Consequence:** a full AX-based black-box test suite is **deferred**. What
landed instead is a single live scenario, `ax-probe`
(`crates/nice/src/app.rs`), that stays within the reach that already exists:

- One stable root element (`AxProbeView`) in the app crate is tagged with a
  fixed `.id("ax-probe-root")`, `.role(gpui::Role::Group)`, and
  `.aria_label("nice-rs-ax-probe-root")`.
- `crate::platform::ax_find_titled_role` walks this process's macOS AX tree
  (`AXUIElementCreateApplication` + a depth- and node-budget-bounded
  `AXUIElementCopyAttributeValue` traversal over `AXChildren`/`AXTitle`/
  `AXRole`) and returns the `AXRole` of whichever element's `AXTitle`
  matches the marker string — **role + label matching only**, never
  identifier matching.
- The scenario polls until AccessKit (lazily activated by the first AX
  query) surfaces the node, asserts `role == "AXGroup"`, and prints
  `SELFTEST PASS ax-probe`.

This is a canary that AccessKit stays wired as gpui evolves across future
pin bumps — **not** an a11y test suite, and not a general-purpose black-box
matching mechanism for chrome/pane-strip tests to build on. Don't design a
new live scenario around AX-identifier matching; it doesn't exist yet.

**Threading note (load-bearing for anyone adding a second AX-based
scenario):** a same-process AX query dispatches inline on the calling
thread — it does not marshal to and wait on the main runloop, so it cannot
deadlock — but AccessKit's per-view adapter state is a non-`Sync` `RefCell`
that gpui also borrows every frame while building the tree. A query issued
from a background executor races that per-frame borrow and panics "RefCell
already borrowed" (hit during `ax-probe`'s first live run, fixed by moving
the query onto the gpui main thread). Any future AX query must run on the
gpui main thread, serialized with rendering, exactly like `ax-probe`'s does.

**Forward options (not exercised by this plan — a human decision, per the
plan's binding technical decisions):**

1. A small vendor patch to gpui/AccessKit wiring that sets `author_id` from
   the element's `.id()`, which would make `accessibilityIdentifier`
   matching reachable and unlock real black-box UI-test-style matching
   (closer to what the Swift app's 14-suite XCUITest harness could do).
   Changing the pin or carrying a new patch is a pin/patch decision — see
   `crates/README.md`'s "Vendoring GPUI" section — never something a later
   cycle or a reconciler does silently.
2. Continue with label-based matching conventions (give every
   AX-probe-worthy element a unique, stable `aria_label`) if the patch is
   never taken. This scales worse than identifier matching (labels are
   user-facing strings that can legitimately change) but needs no vendor
   change.

## Where to look next

- `crates/README.md` — the crate map, the full self-test scenario table, the
  env-var contract, and the screenshot-capture feature gating (`nice-harness/
  capture` + `gpui_platform/test-support`) this doc deliberately doesn't
  repeat.
- `crates/nice-itests/src/lib.rs` and its sibling module doc comments
  (`pixels`, `session`, `behavior`) — the fixture API surface, kept
  authoritative there rather than duplicated here so it can't drift.
- `crates/nice-itests/src/behavior_exemplars.rs` and
  `crates/nice-itests/tests/visual_terminal_screenshot.rs` — worked
  examples of both execution models; each doc comment states which future
  test class it templates.
- `notes/chrome-pain-catalog-20260702.md` — the source of the
  differential-pair convention, and the fuller catalog of the seam-y
  failure patterns (press arbitration, drag-session lifecycle, hit-test
  opacity) that R9–R13 tests should specifically be paranoid about.
