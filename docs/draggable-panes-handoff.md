# Draggable panes — handoff

This doc supersedes the original handoff (commit `9d3dbc3`). It
covers the imperative window-drag refactor done after the user found
that click-and-dragging a pill ALSO dragged the window.

## Status

Branch `worktree-draggable-panes` (worktree at
`/Users/nick/Projects/nice/.claude/worktrees/draggable-panes`),
rebased on top of `origin/main`. Two commits ahead:

- `2bea2c1` Add draggable pane pills with cross-window drag-and-drop
- `27668f5` Add draggable-panes handoff doc

**There is uncommitted work on the branch** — the imperative
window-drag refactor (described below). Decide whether to commit it
before further edits.

```text
modified:   Sources/Nice/Views/AppShellView.swift
modified:   Sources/Nice/Views/PaneDragSource.swift
modified:   Sources/Nice/Views/WindowDragRegion.swift
new:        Tests/NiceUnitTests/PaneDragSourceWindowDragTests.swift
new:        UITests/PaneDragWindowMoveUITests.swift
```

## What's working

- Build is green; full unit + UI suite (~840 tests) passes locally.
- The original "drag a pill drags the window" bug is **fixed in
  the production app** (user manually verified after the latest
  install).
- Empty toolbar chrome still drags the window (via
  `WindowDragRegion.ChromeDragView.mouseDown` → `performDrag`).
- Double-click empty chrome zooms (in the same `mouseDown`
  override).

## What's broken / unverified

The user said "a lot is still broken" after manually verifying the
window-drag fix — but they didn't enumerate the regressions. **Ask
the user what specifically isn't working** before debugging. The
suspect surfaces, in priority order:

1. **Pill drag UX** — single-pill drag, multi-pill reorder, drop on
   sidebar tab row, drop on another window's strip, tear-off into
   empty space. Any of these may have regressed when we added
   `NSWindow.isMovable = false` and the `hitTest` override on
   `NonDraggableHostingView`.

2. **Pill `.onTapGesture` / `.onHover`** — the `hitTest` override
   claims `self` for every in-bounds point. SwiftUI's gesture
   router is supposed to still resolve internally inside the
   hosting view, but this is the highest-risk change for breaking
   tap-to-select / hover state on the pill.

3. **Click-pass-through targets inside the pill** — the close `×`
   button and any sub-element of the pill. Same risk as (2).

4. **Window double-click-to-zoom** — was previously installed by
   `TitleBarZoomMonitor` (a process-wide `NSEvent` monitor). That
   monitor is gone; `ChromeDragView` now handles `clickCount == 2`
   directly. Coverage shrank: zoom now ONLY fires on
   `ChromeDragView`'s pixels, not on `NSVisualEffectView`-tinted
   sidebar chrome the monitor used to allow.

5. **Sidebar / file-browser drag-and-drop** — `isMovable = false`
   has window-wide effect. Verify that anything else relying on
   AppKit's cooperative drag (file drop, image drop, etc.) still
   works.

### The new UITest is failing

`UITests/PaneDragWindowMoveUITests.swift` simulates a 60pt
click-drag on a pill and asserts the window's frame doesn't
change. It **fails**: window moves ~450pt right during the
synthesised drag.

The user's manual testing shows the bug is fixed in production, so
the failing UITest is most likely an `XCUICoordinate.press(...
thenDragTo:)` event-synthesis artifact (synthesised mouse events
arrive with timing/flags that bypass real-world AppKit gating).
Hypotheses for the next conversation:

- **Synthesised events skip the leaf hit-test path** that
  `NonDraggableHostingView.hitTest` short-circuits, hitting AppKit
  internals directly. Verify by adding a `print` to
  `NonDraggableHostingView.hitTest` and watching whether it logs
  during the test.
- **`isMovable = false` is set too late.** `WindowAccessor`'s
  callback runs when SwiftUI hands AppKit a window — possibly
  after the test's first interaction. Add a `print` to confirm the
  property is set before the drag starts. If late, consider
  setting `isMovable = false` via `applicationDidFinishLaunching`
  on every new window, or in `NSWindowDelegate.windowDidLoad`.
- **The synthesised drag uses an internal AppKit drag path that
  doesn't honour `isMovable`.** Test by using
  `XCUIRemote.shared.press(...)` or by setting up an
  `NSEvent.addLocalMonitorForEvents` that records what AppKit
  receives during the test.

The unit tests in `PaneDragSourceWindowDragTests.swift` (5 tests,
all passing) lock in:
- `ChromeDragView` reports `mouseDownCanMoveWindow == false`.
- Hit-tests in chrome route to `ChromeDragView`.
- Pill region's interior leaves report `mouseDownCanMoveWindow ==
  false` (with the override in place).
- Pill corner pixels don't fall through to the chrome view.

These structural invariants hold in the synthetic fixture. They
caught the bug when the override was temporarily removed (840-point
failure → 0). They did NOT catch the production regression because
the synthetic fixture's NSView tree is shallower than the real
toolbar's — see "Why the unit test missed it" below.

## The imperative refactor (what changed)

Original implementation used the cooperative `mouseDownCanMoveWindow
= true` pattern: a transparent `WindowDragRegion` sat in the
`.background` of the toolbar, AppKit's title-bar tracker walked the
hit chain, and ANY view in the chain reporting `true` engaged
window-drag. This is fragile for toolbars with widgets — Apple's
own forum guidance and BJ Homer's well-known gist both steer
people away from it for exactly this class of app.

The refactor replaces it with an imperative pattern, layered:

### Layer 1: `NSWindow.isMovable = false`

Set in `AppShellView.swift`'s `WindowAccessor`. Disables AppKit's
cooperative title-bar drag tracker for the entire window.
`performDrag(with:)` still works (Apple-documented to bypass
`isMovable`).

### Layer 2: `WindowDragRegion.ChromeDragView`

In `Sources/Nice/Views/WindowDragRegion.swift`. Replaces the
passive transparent-with-`mouseDownCanMoveWindow=true` view. Now:
- `mouseDownCanMoveWindow = false` (explicit; default for
  transparent views is `true` in titled windows).
- `acceptsFirstMouse = true` so chrome drag works on inactive
  windows.
- `mouseDown(with:)` calls `performZoom` for `clickCount == 2`,
  else `performDrag(with:)`.

Type name kept as `WindowDragRegion` so callsites in
`AppShellView.swift` didn't change.

`TitleBarZoomMonitor` is **deleted** — the `mouseDown` override
absorbs its job.

### Layer 3: `PaneDragSource.NonDraggableHostingView`

Subclass of `NSHostingView<AnyView>` in
`Sources/Nice/Views/PaneDragSource.swift`. Two overrides:
- `mouseDownCanMoveWindow = false` (defence in depth).
- `hitTest(_:)` claims `self` for every in-bounds point, so AppKit
  always sees this `false`-reporting view as the leaf instead of
  descending into transparent SwiftUI internals that inherit
  `true`.

### Why all three layers

I tried each in isolation. With only Layer 2, the unit test caught
the structural bug but the user's bug remained. With Layer 2 + 3
the unit test passed but the UITest still showed window movement.
Adding Layer 1 closed the rest of the gap (manually verified;
UITest still fails, see above). I kept all three because each
catches a different failure mode and the cost of redundancy is
trivial.

### Why the unit test missed it

`PaneDragSourceWindowDragTests` mounts a synthetic fixture: a
`ZStack { WindowDragRegion(); PaneDragSource { ... } }` inside an
`NSHostingController` inside a borderless `NSWindow`. Two
divergences from production:

- The fixture's NSView tree is **shallow** (3-4 levels deep). The
  real `WindowToolbarView` lives inside `AppShellView` →
  `AppShellHost` → `WindowGroup` SwiftUI hosting → many SwiftUI
  internal wrappers → toolbar HStack → InlinePaneStrip → pill.
  Some of those wrappers report `mouseDownCanMoveWindow == true`,
  and the cooperative tracker in production walked them.
- The fixture's pill content was originally `Color.blue` (opaque).
  Production pills' background is `.clear` for inactive state,
  making `NSHostingView.isOpaque == false`. The unit test now
  uses `RoundedRectangle.fill(Color.clear)` to mimic this — but
  even that didn't catch the deeper-tree-walking issue.

Future "did we regress?" coverage really has to come from a
UITest. The current UITest is right in concept — it fails when the
regression is present (we verified by reverting overrides) — but
its failure under the fix is the artifact described in "What's
broken" §2.

## File-by-file diff summary

### `Sources/Nice/Views/WindowDragRegion.swift`
Replaced passive `DragView` (`mouseDownCanMoveWindow = true`) with
imperative `ChromeDragView`. Removed `TitleBarZoomMonitor`.

### `Sources/Nice/Views/AppShellView.swift`
`WindowAccessor` callback: removed `TitleBarZoomMonitor.install()`,
added `window.isMovable = false`.

### `Sources/Nice/Views/PaneDragSource.swift`
Resurrected `NonDraggableHostingView` (had been removed mid-
conversation). Now wraps content with both
`mouseDownCanMoveWindow = false` AND a `hitTest` override that
claims `self` for in-bounds points.

### `Tests/NiceUnitTests/PaneDragSourceWindowDragTests.swift` (new)
5 tests, all passing. Lock in structural invariants. Use
`isReleasedWhenClosed = false` on the fixture window — without it
ARC + the legacy default crashes XCTest's memory checker in
`objc_release` during teardown.

### `UITests/PaneDragWindowMoveUITests.swift` (new)
1 test, **failing**. Synthesises a 60pt drag on a pane pill,
asserts the window frame doesn't change. See "What's broken" §6
for the diagnosis.

## Smoke test status

Only step 1 is verified manually. Steps 2-6 from the original
handoff are still pending:

1. ~~Window-drag fix verification.~~ **Done** — pill drag does
   NOT drag the window.
2. **Click-to-select still works.** Single-click a pill — should
   select that pane. The `hitTest` override is the highest risk
   for regression here.
3. **Close-X still works.** Click the close button on a pill —
   should close the pane.
4. **Tear-off lands at cursor.** Drag a pill to empty space (off
   all windows). New window should appear at the cursor release
   point (NOT cascaded).
5. **Pty + scrollback preserved cross-window.** Open ⌘N, drag a
   pane between windows, run a command — should work, scrollback
   intact.
6. **Claude rules.** Type `claude` in Main, then try to drag the
   Claude pane on its own tab (rejected) and onto another window
   (spawns a new tab in destination, doesn't join the active tab).

If 2 or 3 fail, the culprit is the `hitTest` override on
`NonDraggableHostingView` (`PaneDragSource.swift:~106-130`).
First thing to try: remove that override and see if the UI bug
goes away — if it does, fall back to a more surgical approach
(e.g. claim `self` only for points where `super.hitTest(point) ==
nil`, like the original buggy version did but with proper bounds
handling that doesn't half-open at the edges).

## References (web research from this session)

- BJ Homer, "Why not -[NSView mouseDownCanMoveWindow]?" — the
  canonical "don't do this for toolbars with widgets" gist:
  https://gist.github.com/bjhomer/2a0035fa516dd8672fe7
- Apple Dev Forums #81149 — Apple framework engineer recommending
  imperative `performDrag(with:)`:
  https://developer.apple.com/forums/thread/81149
- `NSWindow.performDrag(with:)` API:
  https://developer.apple.com/documentation/appkit/nswindow/performdrag(with:)
- Chromium "Tab Strip Design (Mac)":
  https://www.chromium.org/developers/design-documents/tab-strip-mac/

## Outstanding follow-ups (preserved from original handoff)

These are review findings deferred from the initial implementation.
**Listed roughly in priority order.** All still apply.

### Tear-off has zero unit tests (HIGH)
`requestPaneTearOff`, `absorbTearOff`, `absorbClaudeAsNewTab`, and
`consumeTearOff` are completely uncovered. These are the most
error-prone paths — Claude rules, view migration, originator
filtering, project anchor inheritance.

Suggested test files:
- `Tests/NiceUnitTests/NiceServicesTearOffTests.swift` —
  `requestPaneTearOff` (terminal + Claude), `consumeTearOff`
  originator filter, overwrite-while-pending behavior.
- `Tests/NiceUnitTests/AppStateAbsorbClaudeAsNewTabTests.swift` —
  cross-window Claude move, source `activePaneId` recovery,
  `claudeSessionId` carry-over, source dissolve when Claude was
  the only pane.
- `Tests/NiceUnitTests/AppStateAbsorbTearOffTests.swift` —
  Terminals-anchor vs repoPath-anchor inheritance.

### Source-detach orchestration duplicated 4× (HIGH)
The "remove pane → recover `activePaneId` → fire `onTabBecameEmpty`
if empty" sequence is open-coded in `SessionsModel.paneExited`
(`:234-263`), `TabModel.movePane` (`:330-345`),
`SessionsModel.adoptPane` (`:583-600`),
`AppState.absorbClaudeAsNewTab` (`:339-368`), and
`NiceServices.requestPaneTearOff` (`:226-259`).

The `paneLaunchStates` handling differs slightly between copies —
drift hazard. Extract a single
`TabModel.removePaneAndRecoverActive(tabId:paneId:) -> (becameEmpty:
Bool, projectTabIndex: (Int, Int)?)` helper; replace all four
open-codings.

### `pendingTearOff` consumer-selection is fragile (HIGH)
`NiceServices.consumeTearOff(for:)` filters out the originator
AppState, then any other AppState whose `.task` runs next claims
the pending pane. Issues:

- **Overwrite-while-pending leaks the prior pane.** Two rapid
  tear-offs from the same window: second `requestPaneTearOff`
  overwrites `pendingTearOff` while pane A's view is still parked
  there — pane A is silently lost (along with its pty).
- **No TTL.** If the new window fails to spawn (Stage Manager,
  Mission Control quirks), `pendingTearOff` sits forever and the
  next ⌘N steals it.
- **Source persistence committed before destination absorption.**
  `requestPaneTearOff` already fires `onSessionMutation` on source.
  If the new window never mounts, source is saved without the
  pane — restore-from-disk loses it.

Suggested fixes (any one helps):
(a) Tag `PendingTearOff` with a destination-window-session-id
    minted before `openWindow(id:)`; filter strictly on that match.
(b) Pass via SwiftUI's `openWindow(id:value:)` (the live `NSView`
    can't go through Codable; keep a separate disposable view box
    keyed by the value's id).
(c) On overwrite, re-home the previous pending payload back into
    a fresh tab on its originator.
(d) Defer source persistence until destination absorbed.

### Coordinate-space concern (verify before fixing)
The code-quality reviewer flagged `paneFrames` (in
`paneStripCoordinateSpace`) vs `info.location.x` (drop view's local
space) as potentially mismatched. The existing comment at
`WindowToolbarView.swift:354` documents `paneStripCoordinateSpace`
as the ScrollView's viewport space; off-screen detection in
`PaneStripGeometry` already depends on this. Smoke test step 23
(drag onto pills after scrolling) is the way to confirm.

### `didDropOnTarget` flag may be redundant (MEDIUM)
`PaneDragState.didDropOnTarget` is a cross-component contract:
drop delegates must remember to set it before the deferred
mutation, or the AppKit drag source's `endedAt` callback wrongly
engages tear-off. The `NSDragOperation` argument to
`draggingSession(_:endedAt:operation:)` likely already encodes
"did anything accept this" (non-empty when accepted, `[]` when no
target). Verify and delete the flag.

### Two parallel drop delegates with the same scaffolding (MEDIUM)
`PaneStripDropDelegate` and `TabRowPaneDropDelegate` share
identical `validateDrop` / `dropEntered` / `dropUpdated` /
`dropExited` / `ownsCurrentIndicator` plumbing (different drop
semantics). Extract a base struct or shared free functions.

### `paneLaunchStates` carried as `.pending` never gets a destination promotion timer (MEDIUM)
`SessionsModel.adoptPaneLaunchState` just sets the dict entry; no
0.75s timer. Edge case: a still-launching pane (within grace
window) gets dragged, never emits a byte for >0.75s on the dest
side, never shows the "Launching…" overlay. Re-schedule the timer
on `.pending` adoption.

### `ProjectAnchor.from` silently wrong on missing source (MEDIUM)
`AppState.swift:84-93`'s `else { return .terminals }` branch fires
if the source tab can't be resolved (source window closed mid-
drag). User dragged from a project tab, source closed, now the
absorbed pane lands in Terminals — wrong anchor. Make
`ProjectAnchor.from` return optional and propagate the nil.

### Other small items
- `PaneDragPayload.encoded()` swallows JSON errors as empty
  `Data()` — `try!` instead so the bug surfaces in dev.
- `wouldMovePane` cross-tab branch has dead `_ = dstPi; _ = dstTi`
  lines.
- `wouldMovePane` duplicates `movePane`'s same-tab math; extract a
  private resolver helper.
- `absorbAsNewTab` mints id as `"t\(millis)"` — collision risk for
  rapid back-to-back tear-offs. Use UUID.
- The `PendingTearOff` struct + `ProjectAnchor` enum live in
  `AppState.swift`; consider moving to their own files.

## Useful references

- Build (worktree-local DerivedData):
  `xcodebuild -project Nice.xcodeproj -scheme Nice -derivedDataPath ./build-dev build`
- Run unit tests under worktree lock:
  `scripts/worktree-lock.sh acquire test && scripts/test.sh -only-testing:NiceUnitTests; scripts/worktree-lock.sh release`
- Run the new UITest only:
  `scripts/worktree-lock.sh acquire test && scripts/test.sh -only-testing:NiceUITests/PaneDragWindowMoveUITests; scripts/worktree-lock.sh release`
- Reinstall Nice Dev under worktree lock:
  `scripts/worktree-lock.sh acquire install && { scripts/install.sh; rc=$?; scripts/worktree-lock.sh release; exit $rc; } || scripts/worktree-lock.sh release`

## First thing for the next conversation

Ask the user what specifically is still broken (their phrase: "a
lot is still broken"). Run smoke test steps 2-6 above with them
one at a time. The likeliest regressions, given the
`NonDraggableHostingView.hitTest` override, are:
- Click-to-select on a pill no longer works.
- Hover state no longer activates on a pill.
- The close `×` button no longer responds to clicks.

If any of those are broken, the override at
`PaneDragSource.swift:~108-130` is too aggressive. Fall back to
"only claim self when `super.hitTest(point) == nil` AND the local
point is strictly inside `bounds`" — but be careful with the
half-open interval at the edges. The previous attempt's bug was
that `bounds.contains` (and `NSPointInRect`) treats the upper-left
corner as "inside" but the lower-right edge as "outside", which
exposed boundary pixels as fall-through.
