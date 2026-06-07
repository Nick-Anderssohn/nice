# Decision point: pill drag also drags the window

**Status:** blocked on an approach decision. This doc is the checkpoint
record so we can revert here and try a different approach.

**Branch:** `worktree-refactor-top-bar`. The commit that adds this file is
the intended revert anchor.

---

## What we're building

Drag-to-reorder for the pane "pills" in the custom top toolbar
(`WindowToolbarView` → `InlinePaneStrip` → `InlinePanePill`). Plan lives at
`~/.claude/plans/docs-research-draggable-pane-pills-hand-witty-rabin.md`.
Scope: intra-strip reorder within the active tab, designed so cross-window
drag / tear-off can be added later. Visual: a 2pt insertion line (sidebar
parity). Verification was meant to be fully automated except a one-time
spike.

## The blocking problem

When you click-drag a pill, the pill drag is recognized **but the whole
window also moves**, because the 52pt toolbar band is a window-drag region.
The window must stay put when dragging a pill. This is the exact issue that
sank an earlier attempt (`worktree-draggable-panes`).

---

## What is DONE and solid (keep across any approach)

These are committed at this checkpoint and are approach-independent:

- **`TabModel.movePane(_:inTab:relativeTo:placeAfter:)` + `wouldMovePane`**
  (`Sources/Nice/State/TabModel.swift`) — mirrors `moveTab` index math,
  scoped to one tab's `panes`, fires `onTreeMutation` only on a real move.
- **`PaneStripDropResolver`** (`Sources/Nice/Views/PaneStripDropResolver.swift`)
  — pure horizontal slot-math enum, with forward-compat `PaneDragOrigin`
  (identity + source context) and `PaneDropDestination` enum.
- **Unit tests, all green (34):**
  `Tests/NiceUnitTests/PaneStripDropResolverTests.swift`,
  `TabModelMovePaneTests.swift`, `MovePanePersistenceTests.swift`.
  - Gotcha learned: in a `@MainActor` XCTestCase, calling an actor-isolated
    helper from `setUp()` trips Swift 6 "Sending 'self' risks data races" —
    seed **inline** in `setUp` (see those files).
- **UITest harness** `UITests/PaneReorderUITests.swift` — launches the
  sandboxed app, grows to 3 pills via `tab.add`, reads pill order by
  `frame.minX`. `testDragOnPillDoesNotMoveWindow` is written but currently
  `XCTSkip`-ped (the unsolved bit).

Still TODO (after the drag conflict is resolved): the gesture/drop wiring,
insertion-line overlay, and the rest of the UITest matrix (reorder
right/left/to-end, tap-select, rename, close, relaunch persistence).

## Window/chrome facts

- Window: `.windowStyle(.hiddenTitleBar)`, min deploy macOS 14, dev machine
  macOS 26.
- `WindowDragRegion` (`mouseDownCanMoveWindow=true` NSView) is laid as the
  toolbar's `.background` for empty-chrome drag; `TitleBarZoomMonitor`
  (process-wide `NSEvent` monitor) handles double-click-to-zoom.

---

## Approaches TRIED and their results (so we don't repeat them)

Verified via `testDragOnPillDoesNotMoveWindow` (drag a pill, assert window
origin unchanged) paired with `testEmptyToolbarDragMovesWindow` (empty
chrome must still drag — the differential control).

1. **Baseline (no fix):** pill drag moves window ~170pt. ✗ (confirms bug +
   that the test catches it.)
2. **SwiftUI `.onDrag` on the pill:** window still moves. ✗
3. **`.highPriorityGesture(DragGesture())` on the pill** (another agent's
   suggestion): window still moves. ✗ — SwiftUI gestures lose to the
   AppKit title-bar tracker, which sits below them.
4. **`NonDraggableRegion` (`mouseDownCanMoveWindow=false` NSView) as the
   pill's `.background`:** window still moves. ✗ — a sibling/behind view,
   not in the event-propagation chain.
5. **Same veto as an `.overlay` (frontmost):** window still moves. ✗ —
   AppKit's title-bar hit-test doesn't reliably descend into SwiftUI-
   embedded NSViews (this codebase's `WindowDragRegion.swift` header
   documents the same limitation for double-click).
6. **macOS 15 `windowBackgroundDragBehavior(.enabled)` + remove
   `WindowDragRegion`, keeping `.hiddenTitleBar`:** pill still drags; also
   broke zoom. ✗ — the modifier governs the SwiftUI *window background*,
   not the native title-bar drag; it needs `.windowStyle(.plain)`.
7. **Diagnostic — remove `WindowDragRegion` entirely:** empty chrome STILL
   drags. → The drag is the **native title-bar drag** of the hidden-title-
   bar window, not `WindowDragRegion`.
8. **Process-wide `NSEvent` monitor toggling `window.isMovable=false` on a
   top-band press over a control:** window still moves. ✗
9. **Diagnostic — monitor consumes (`return nil`) EVERY left-mouse-down:**
   empty chrome STILL drags. → **App-level `NSEvent` monitors cannot
   intercept XCUITest's synthesized title-bar drag at all.** (Implies
   `TitleBarZoomMonitor` likely doesn't fire under XCUITest either — a
   second reason the zoom test is red, alongside the next point.)

## Other findings

- **Zoom UITest (`testEmptyToolbarDoubleClickZoomsWindow`) is
  environmentally red**, not a regression: the UITest window launches at
  the maximized/zoom size, so double-click-zoom toggles to the same size
  and the size-change assertion never fires. (Worth hardening separately.)

---

## The core conclusion

The pill's window-drag is the **native title-bar drag** of a
`.hiddenTitleBar` window. It cannot be vetoed by SwiftUI gestures or by
SwiftUI-embedded `mouseDownCanMoveWindow=false` views, and XCUITest's
synthesized drag can't be intercepted by an app-level `NSEvent` monitor.

Consequence: the only approach that operates *below* the synthesized-event
path **and** can selectively exclude the pills is
`.windowStyle(.plain)` + `windowBackgroundDragBehavior` — SwiftUI owns the
whole background, no native title bar, controls auto-excluded. That is the
only path we believe is both **effective and XCUITest-verifiable**, but it
removes the native title-bar chrome (traffic-light handling + double-click
zoom would need rework).

A monitor/`isMovable`-style fix might work for **real** users but **cannot
be verified by XCUITest**, which conflicts with the "fully automated, no
manual smoke tests" requirement.

## The open options (the decision)

- **A. Commit to `.windowStyle(.plain)` + `windowBackgroundDragBehavior`**
  (macOS 15). Effective + auto-testable. Cost: re-implement traffic-light/
  window-control handling + double-click zoom.
- **B. Keep current chrome (macOS 14); real-event-level fix + manual
  verification.** Likely works for real users; NOT XCUITest-verifiable.
- **C. Rethink** — different UX, different window architecture, or whether
  title-bar-band reorder is worth this cost.

## Not yet tried (candidate "different approaches")

- Reintroducing the deliberately-abandoned `isMovable=false` +
  `WindowDragRegion.performDrag(with:)` for empty chrome (selective:
  empty chrome drags via `performDrag`, pills don't). Abandoned because
  `performDrag` interfered with double-click zoom; the header in
  `WindowDragRegion.swift` documents this. Would need to confirm it
  coexists with `TitleBarZoomMonitor`.
- Verifying whether a real-event monitor fix works via a non-XCUITest
  harness (e.g. a `CGEvent`-posting helper) instead of XCUITest.
- Confirming whether `.windowStyle(.plain)` keeps the traffic-light
  buttons before committing to the rework.

---

## How to revert to this point

This checkpoint is committed on `worktree-refactor-top-bar`. To return here
after trying something else:

```sh
git log --oneline        # find the "checkpoint: pill-drag decision point" commit
git reset --hard <sha>   # or: git checkout <sha> -- <paths>
```

At this checkpoint the app builds, all unit tests pass, and
`testDragOnPillDoesNotMoveWindow` is `XCTSkip`-ped (the unsolved item).
