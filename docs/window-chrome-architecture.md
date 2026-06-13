# Window chrome & pane tear-off: the single-owner contract

Nice runs under `.hiddenTitleBar` + `.fullSizeContentView`, so AppKit's
native title bar is gone and Nice has to re-synthesize everything a title
bar normally gives you: traffic-light placement, drag-to-move,
double-click-to-zoom, and the rule that none of those fire when you grab a
pane pill. For years three bugs kept recurring because each of those
behaviours was implemented by a separate band-aid that *remembered* state
and could drift out of sync.

This redesign replaces the band-aids with a **single-owner architecture**:
one `WindowChromeController` per window, chrome state **computed per event
(never remembered)**, and **one arbitration point per press**. This doc is
the maintainer-facing contract.

## The three recurring bugs and their structural fixes

### BUG A — tearing off an unspawned pane silently no-op'd

A restored-but-never-focused pane has its pty spawn **deferred**
(`WindowSession` brings up only the active pane on restore; "other panes
stay lazy until first focus"). The old tear-off/migration path force-read a
*live* pty entry, got `nil` for a deferred pane, and bailed — so dragging
that pane onto the desktop did nothing, and dropping it on another window
did nothing.

**Fix:** pane liveness is now a **closed type**, `LivePaneRegistry.PaneClaim`:

```swift
enum PaneClaim { case live(PaneEntry); case notSpawned(cwd: String); case gone }
```

The tear-off and migration consumers switch over it exhaustively. `.live`
detaches and re-adopts the running pty; `.notSpawned(cwd:)` **spawns the
pane fresh in the destination** (terminals via the session-*creating*
`ensurePaneSpawned`, Claude via `.resumeDeferred`); `.gone` aborts loudly.
`SessionsModel.claimPaneForTransfer(tabId:paneId:)` produces the claim
without ever force-unwrapping. An unspawned pane therefore migrates by
spawning in the destination instead of no-op'ing.

### BUG B — traffic lights intermittently doubled-spaced

The old `TrafficLightNudger` **captured** a button's origin once and
**pinned** it. When AppKit relaid the cluster (a torn-off window that opened
already-key and was never resized was the classic trigger), the nudger
re-applied a *stale captured* origin — leaving the trio with doubled
spacing.

**Fix:** `TrafficLightPlacer` computes an **absolute target per frame
event**, nothing captured-then-pinned. Geometry:

- **y is absolute and OS-independent:** every Nice top-bar element centers
  on window-y **26** (`WindowChrome.trafficLightCenterFromTop`
  `= topBarHeight / 2 = 52 / 2`). The placer targets that row directly:
  `originY = windowHeight − centerFromTop − buttonHeight/2`. This does *not*
  depend on macOS's default button y, so it is robust across macOS 14/15/26.
- **x is default-relative, not hardcoded:** the placer takes each button's
  *own* native default leading x and adds a uniform
  `WindowChrome.trafficLightNudgeX` (8pt) inward. A uniform translation
  **preserves the OS-native inter-button pitch** (23pt on macOS 26, 20pt on
  macOS ≤ 15). Hardcoding `28/48/68` — or a fixed 20pt pitch — is the
  stale-macOS regression: on the current host (measured macOS 26 native
  defaults are **9 / 32 / 55 @ 23pt pitch**, *not* 20/40/60) it would shove
  the lights ~11pt right and compress the spacing.

The only thing cached is each button **instance's** native default x, keyed
by `ObjectIdentifier`, recorded *before* the first move; because the target
is absolute (default + offset), re-applying never compounds, and a swapped
button instance is captured fresh.

### BUG C — dragging a pill moved the whole window

The old machinery cooperated badly: a process-wide double-click monitor, a
SwiftUI `DragGesture` that fished `NSApp.keyWindow`, and a one-bit
`WindowDragGate` flag the pill press flipped to "yield." When the flag stuck
(or in a torn-off window where the veto failed), a pill drag dragged the
window.

**Fix:** `ChromeEventRouter` is **one arbitration point per press** — a
single process-wide local `NSEvent` monitor that classifies each
`.leftMouseDown` exactly once and drives all three title-bar behaviours. The
pill veto is no longer a flag: a pill press **hit-tests to a
`PaneDragHosting` view**, the router gives `.pill` precedence and passes the
event through, so a pill press can never arm a window drag — selectivity by
construction.

## The contract / invariants

- **Chrome state is computed per event, never remembered.** No cached
  "canonical position," no "we already nudged this window" static. The
  placer recomputes an absolute target on every frame event; the
  `isMovable = false` policy is re-asserted on every focus/KVO event and at
  event time per press.
- **One `WindowChromeController` owns each window's AppKit chrome state.**
  Controllers live in a `static NSMapTable` with **weak keys**, so an entry
  auto-prunes the instant the window deallocs (no `ObjectIdentifier`
  address-reuse hazard). `adopt()` registers **unconditionally** — it does
  *not* gate on `.fullSizeContentView`, because `viewDidMoveToWindow` can
  fire before SwiftUI applies the hidden-title-bar mask; an unregistered
  window would be invisible to the router. The styleMask check lives only in
  the per-action paths, each of which self-heals on the next focus/frame
  event. (The Settings window keeps standard chrome and is filtered by those
  guards.)
- **One arbitration point per press** — the router. Each `.leftMouseDown` is
  classified once: pill → pass through, empty strip → arm drag / run
  double-click zoom, else pass through. Its only state is a single
  `pendingDrag`, **overwritten on every mouseDown and cleared on every
  mouseDown AND mouseUp** — there is no stuck-bit failure mode.
- **Pane liveness is a closed type** (`PaneClaim`), switched exhaustively, so
  a deferred-spawn pane is a first-class case, not a `nil` that silently
  bails.
- **Traffic-light geometry is OS-version-robust:** default-relative x (each
  button's own native default + 8pt, preserving native pitch), absolute
  y = 26 = `topBarHeight / 2` (Nice's top-bar row, **not** macOS defaults).

## The components

All under `Sources/Nice/Views/Chrome/`:

- **`WindowChromeController.swift`** — single owner of one window's chrome
  state; weak-keyed registry with auto-prune; the `isMovable = false` policy
  (focus/KVO re-assert covering the non-`sendEvent` server-side drag-init
  path); owns the `TrafficLightPlacer`; installs the `ChromeEventRouter`
  once, process-wide, from `start()`. `willClose` → `tearDown()`.
- **`TrafficLightPlacer.swift`** — the per-frame-event absolute placement
  math (BUG B). Observes each button's own `frameDidChange`, lazily
  re-resolves `standardWindowButton` + superview inside every `apply()`, and
  re-applies on window focus/resize/move; suspends across full-screen
  transitions.
- **`ChromeEventRouter.swift`** — the single per-press arbitration point
  (BUG C). Owns empty-chrome drag, double-click-zoom
  (`DoubleClickTitleBarAction`, moved here), and the per-press event-time
  `isMovable = false` invariant.
- **`WindowBridge.swift`** — synchronous `viewDidMoveToWindow`
  `NSViewRepresentable` that hands SwiftUI's `NSWindow` to the controller at
  attach (replaces the old one-runloop-deferred `WindowAccessor`). The two
  ordering-sensitive writes (`NICE_UITEST_WINDOW_FRAME` pin and
  `WindowRegistry.register`'s `CloseConfirmationDelegate` wrap) stay
  **deferred** one runloop in `AppShellView` because they only stick after
  SwiftUI finalizes the window.

Two marker types feed the router's hit-test:

- **`ChromeDragStripView`** (`WindowDragRegion.swift`) — a behaviourless
  AppKit marker laid into the chrome `.background`. `mouseDownCanMoveWindow`
  is `false`; its only job is to be a recognisable class in the ancestor
  chain so the router classifies a press on empty chrome as `.strip`.
- **`PaneDragHosting`** (`PaneDragSource.swift`) — a marker protocol the
  pill's hosting view conforms to; the router gives it precedence so a pill
  press is passed through.

**Router class-walk + attribute-walk fallback.** The router classifies the
hit view's ancestor chain by class (`PaneDragHosting` → `.pill`,
`ChromeDragStripView` → `.strip`). The **sidebar** strip resolves directly
to its marker, but the **toolbar** strip does *not*: it sits in a
`.background` `ZStack` behind the toolbar's `HStack`, and SwiftUI resolves an
empty-toolbar press to a transparent hosting wrapper that is a sibling
*above* the strip — never a descendant — so a pure class-walk dead-spotted
empty-toolbar drag + double-click zoom. The router therefore *also*
classifies `.strip` via an attribute-walk: any non-`NSVisualEffectView`
ancestor with `mouseDownCanMoveWindow == true`. This only **widens**
drag/zoom and can never let a pill ride, because the `PaneDragHosting`
branch matches first and `.pill` takes precedence.

## What was deleted

- `TrafficLightNudger.swift` (the capture-then-pin nudger) **and**
  `WindowAccessor` (the one-runloop-deferred bridge it carried), plus
  `TrafficLightNudgerTests.swift`.
- `WindowDragGate` (the one-bit pill-yield flag) and all its write sites /
  `.environment` plumbing.
- `windowDraggable` / `WindowDraggableModifier` (the SwiftUI `DragGesture`
  that fished `NSApp.keyWindow`).
- `TitleBarZoomMonitor` (the old double-click monitor; its empty-chrome
  predicate survives as the router's attribute-walk fallback,
  `DoubleClickTitleBarAction` moved into the router).
- The `isMovable` re-assert **timer loop** (`0 / 0.05 / 0.2s`) in
  `AppShellView` — replaced by the router's per-press assert + the
  controller's focus/KVO re-assert.
- The temporal **FIFO tear-off pairing** — replaced by token-keyed window
  birth (`WindowGroup(id:"main", for: String.self)` + per-window UUID token;
  the new window consumes only the seed deposited under its own token).

## The test nets

Unit (`Tests/NiceUnitTests/`):

- **`PaneClaimTransferTests`** — BUG A: `.live` / `.notSpawned(cwd)` / `.gone`
  resolution, unspawned tear-off, session-less migration, resume-deferred
  Claude.
- **`TrafficLightPlacerTests`** — BUG B: the pure absolute-placement math
  (default-relative x, absolute y = 26), preserved monotonic order + equal
  pitch, and idempotence (re-applying converges and never compounds).
- **`ChromeEventRouterTests`** — BUG C: the pure
  `decision(hitChain:clickCount:inBand:isFullScreen:)` (pill precedence,
  in-band gate, full-screen skip).
- **`WindowToolbarDragRegionTests`** — `ChromeDragStripView`'s
  `mouseDownCanMoveWindow == false` marker contract.

UITest gates (`UITests/`):

- **`WindowDragUITests`** — empty-toolbar drag, sidebar-strip drag,
  double-click zoom (the router's three drag behaviours).
- **`PaneReorderUITests`** — a pill drag reorders and never moves the window.
- **`TearOffHookUITests`** — driven by the `--uitest-tearoff-hook`
  launch-arg hooks (`test.tearOffActivePane`, `test.tearOffInactivePane`):
  the BUG A unspawned-pane tear-off (seeded-`sessions.json` relaunch →
  second window opens, non-blank), the BUG B relative + monotonic +
  equal-pitch traffic-light assertions on fresh and torn-off windows, and
  the BUG C pill-drag-doesn't-move-window net in the torn-off context (the
  window-agnostic reorder-and-doesn't-move net lives in `PaneReorderUITests`
  — the router fix is identical in any window, and a two-window fixture
  isn't reliably controllable from XCUITest).
- **`CrossWindowMoveUITests`** — cross-window pane migration.
- **`MultiWindowRestoreUITests`** — the seeded-`sessions.json` quit/relaunch
  round-trip (the fixture pattern the BUG A net reuses).

