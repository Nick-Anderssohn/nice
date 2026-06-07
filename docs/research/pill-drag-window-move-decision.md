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

---

## UPDATE (2026-06-06): `.plain` built and measured — viable but expensive

We implemented option A end-to-end (deploy target → macOS 15,
`.windowStyle(.plain)` + `.windowBackgroundDragBehavior(.enabled)`, kept
`WindowDragRegion` so the zoom monitor still had its marker) and measured
it. **`.plain` is not a dead end — it works and is XCUITest-verifiable —
but it strips the entire native window chrome, so adopting it means
re-implementing all of it by hand.** Findings:

**What `.plain` removes (visually confirmed against the real build):**
- **Traffic lights** — gone. The AX dump shows no close/minimize/zoom
  buttons anywhere in the window subtree. (`TrafficLightNudger` also keys
  off `styleMask.contains(.fullSizeContentView)`, which `.plain` may not
  set — so even the nudger would skip the window.)
- **Rounded corners** — gone; the window is a square borderless rect.
- **Drop shadow** — gone; the window sits flat on the desktop.
- **Double-click-to-zoom** — broken.
- **Window drag** — broken *even with* `windowBackgroundDragBehavior`.
  Root cause: the modifier makes the *window background* draggable, but
  our toolbar paints an opaque `Color.niceChrome(...)` over the whole
  window, so the background is fully occluded — there's no exposed
  background region for the modifier to grab. Empty chrome would only
  become draggable if the draggable area actually exposed window
  background (or we add our own drag mechanism).

So option A's true cost is "reconstruct traffic lights + rounded corners
+ shadow + drag + zoom on a borderless window," which is more than the
original estimate (it had only flagged traffic lights + zoom). High
effort and fidelity risk, but **doable** — `.plain` remains the fallback.

**Verifiability — the surprising good news (approach-independent):**
`.plain` is NOT an XCUITest blind spot. The window and every pill ARE in
the accessibility tree:
- `app.windows.count == 1`, window has identifier `main-AppWindow-1`.
- `app.buttons["tab.add"].exists == true`; pills resolve by their
  `tab.pill.<id>` identifiers.
- BUT `app.windows.firstMatch.waitForExistence(timeout:)` returns
  **false** — even after `app.activate()`, with app state
  `runningForeground`. The window/app AX nodes are marked **`Disabled`**,
  which is what makes that particular call miss.
- **Fix for any future `.plain` work:** gate test readiness on
  `app.buttons["tab.add"]` / `app.windows.count > 0` / the
  `main-AppWindow-1` identifier — NOT on `windows.firstMatch
  .waitForExistence`. The window `.frame` is readable via the resolvable
  query (the snapshot reports it), so the no-move assertion still works.

This weakens the *original* tie-breaker (verifiability was supposed to be
unique to `.plain`): if `isMovable=false`-style fixes turn out to be
XCUITest-testable too, then native-chrome preservation becomes the
deciding factor, favoring the next experiment.

**Next experiment (chosen):** the "not yet tried" candidate below —
`window.isMovable = false` (kills the native title-bar drag so pills
can't move the window) + restore empty-chrome drag via
`performDrag(with:)`, keeping `.hiddenTitleBar` so all native chrome
stays free. Open question to resolve empirically: the `performDrag`
↔ double-click-zoom interaction, and whether XCUITest's synthesized drag
drives the view's `mouseDown`/`mouseDragged` (the positive
"empty-chrome-drags" control) — the negative "pill-doesn't-move"
invariant should hold regardless, since `isMovable=false` is a window
property, not an interceptable event.

---

## UPDATE 2 (2026-06-06): `isMovable = false` + `performDrag` — measured

Implemented and instrumented the chosen experiment. **Verdict: it solves
the blocker cleanly and keeps all native chrome, and the window-drag is
restorable — but only at the SwiftUI gesture layer, coupled with the pill
reorder gesture.** Ground truth (a temporary `NSEvent`-monitor diagnostic
logging `isMovable`, the `hitTest` chain, and `accessibilityHitTest` for
each top-band click; now removed):

**What works:**
- **`window.isMovable = false`** (set in `AppShellView`'s `WindowAccessor`)
  stops a pill drag from moving the window. `testDragOnPillDoesNotMoveWindow`
  **passes**. This is the headline blocker — solved, and XCUITest-verified.
- Native chrome is fully intact (traffic lights, rounded corners, shadow) —
  visually confirmed. Strictly better than `.plain` on that axis.
- **Double-click zoom still works** (`testEmptyToolbarDoubleClickZoomsWindow`
  passes when run from a non-zoomed start; keep `WindowDragRegion` as the
  zoom monitor's `mouseDownCanMoveWindow` marker).

**What `isMovable = false` breaks, and why:**
- It disables **all** native window dragging, not just the pills'. The
  diagnostic proved this: with `isMovable == false`, every view in the
  top-band hit chain still reports `mouseDownCanMoveWindow == true` (the
  sidebar's `NSVisualEffectView` included) **yet nothing drags**. So the
  earlier hunch that the sidebar's `mdcmw=true` "survives" `isMovable=false`
  was wrong — `isMovable=false` gates the `mdcmw` drag path too.
- Empty-chrome drag must therefore be driven **explicitly**.
  `window.performDrag(with:)` **does** move the window despite
  `isMovable == false`, and — **corrected from an earlier wrong claim
  here** — XCUITest **can** drive it when it's triggered from a **SwiftUI
  `DragGesture`** (not from a view `mouseDown` or an `NSEvent` monitor,
  which the synthesized drag bypasses). A toolbar-wide
  `DragGesture(minimumDistance: 2).onChanged { window.performDrag(with:
  NSApp.currentEvent!) }` makes `testEmptyToolbarDragMovesWindow` **pass**.
  So the empty-chrome positive control IS automatable. The earlier failures
  were because the drag handler sat on the background `DragView`, which the
  pane strip's `NSClipView` occludes at the test's click point — the events
  went to the scroll view, not our handler. Lesson: drive window drag from
  a SwiftUI gesture, not an embedded NSView's mouse handlers.

**Why selectivity can't live in an `NSEvent` monitor:**
- A monitor can't tell "pill" from "empty chrome." `hitTest` returns
  SwiftUI-internal classes that vary inconsistently (`NSClipView`,
  `PlatformGroupContainer`, `DragView`, …); much of the "empty" toolbar is
  actually the pane strip's `NSClipView` (the `ScrollView`), which occludes
  the background `DragView`. And `contentView.accessibilityHitTest(...)`
  returns the top-level hosting view as `AXGroup` for *every* point — it
  does not descend into SwiftUI to surface a pill's `AXButton`. No reliable
  monitor-level signal exists.

**Conclusion / plan:** Selectivity has to come from SwiftUI, which *does*
know what's a pill. So window-drag becomes a SwiftUI gesture on the empty
chrome (`DragGesture` → `window.performDrag(with: NSApp.currentEvent!)`),
and the pill reorder is a SwiftUI `DragGesture` on the pill — the two are
halves of one gesture layer, where a drag that starts on a pill is claimed
by reorder and a drag on empty chrome is claimed by window-move. Net state
kept on the branch: `isMovable = false` (+ `WindowDragRegion` retained for
the zoom marker). Next: build the unified gesture layer (Step 3).

---

## UPDATE 3 (2026-06-06): RESOLVED — final architecture, both guards green

The window-drag conflict that sank the first attempt is **solved**, with
both differential UITests passing. The working configuration:

1. **`window.isMovable = false`** (`AppShellView`'s `WindowAccessor`,
   main window only) — disables AppKit's native title-bar drag for the
   whole 52pt band, so a pane pill can no longer ride it to move the
   window. This is the load-bearing fix.
2. **Empty-chrome window drag** — a SwiftUI `DragGesture(minimumDistance:
   2)` on `WindowToolbarView`, attached as a plain `.gesture` (so it
   yields to higher-priority child gestures), whose `onChanged` calls
   `NSApp.keyWindow?.performDrag(with: NSApp.currentEvent!)`. `performDrag`
   moves the window even though `isMovable == false`, and — unlike a view
   `mouseDown` or an `NSEvent` monitor — XCUITest's synthesized drag DOES
   drive it.
3. **Selectivity** — the pane pill carries `.onDrag { NSItemProvider(...) }`
   (the reorder drag source). Because `.onDrag` claims the gesture, the
   toolbar's lower-priority window-drag `.gesture` yields: dragging a pill
   reorders it (once drop is wired) and does NOT move the window.
4. **Double-click zoom** unchanged — `TitleBarZoomMonitor` still works;
   `WindowDragRegion`/`DragView` is retained solely as its
   `mouseDownCanMoveWindow == true` marker (it no longer drives any drag).

Native chrome (traffic lights, rounded corners, shadow) is fully intact —
strictly better than the `.plain` path, which discarded all of it.

**Test status:** `testDragOnPillDoesNotMoveWindow` (pill → no move) and
`testEmptyToolbarDragMovesWindow` (empty chrome → moves) both **pass** —
a meaningful differential pair. `testEmptyToolbarDoubleClickZoomsWindow`
passes from a non-zoomed start but is environmentally flaky (it leaves the
window zoomed/full-screen for the next test); harden before relying on it.

**Correction to earlier notes in this doc:** an intermediate claim that the
empty-chrome positive control "is not XCUITest-automatable" was **wrong**.
It only held for drag handlers on the background `DragView`, which the pane
strip's `NSClipView` occludes at the test's click point. Driving the drag
from a SwiftUI gesture instead makes it fully automatable. Likewise the
hunch that `mouseDownCanMoveWindow == true` "survives" `isMovable = false`
was disproved by instrumentation — `isMovable = false` gates that path too.

## How to revert to this point

This checkpoint is committed on `worktree-refactor-top-bar`. To return here
after trying something else:

```sh
git log --oneline        # find the "checkpoint: pill-drag decision point" commit
git reset --hard <sha>   # or: git checkout <sha> -- <paths>
```

At this checkpoint the app builds, all unit tests pass, and
`testDragOnPillDoesNotMoveWindow` is `XCTSkip`-ped (the unsolved item).
