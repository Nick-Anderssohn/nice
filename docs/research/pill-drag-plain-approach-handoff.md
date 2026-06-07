# Handoff: finish draggable pane pills via the `.windowStyle(.plain)` approach

> **⚠️ SUPERSEDED (2026-06-06).** The `.plain` approach was tried and
> rejected — it strips all native window chrome (traffic lights, rounded
> corners, shadow). The window-drag blocker was instead solved with
> `window.isMovable = false` + a SwiftUI-gesture `performDrag` + a pill
> `.onDrag` for selectivity, keeping native chrome intact. **Use
> `docs/research/pill-reorder-continuation-handoff.md` as the current
> brief**, and see `pill-drag-window-move-decision.md` UPDATE 2 / UPDATE 3
> for why `.plain` was dropped. This file is kept for history only.

**Paste this whole file into the new conversation as the brief.**

You are continuing work on **drag-to-reorder for the pane "pills" in the
custom top toolbar** of a SwiftUI macOS app called **Nice**. A prior
conversation built the reorder model + tests and then spent a long time on
the one hard blocker — *dragging a pill also drags the whole window* — and
ruled out many approaches. **The decision has been made: solve it with
`.windowStyle(.plain)` + `windowBackgroundDragBehavior` (macOS 15).** Your
job is to implement that and finish the feature.

---

## Start here (read these first, in the repo)

- `docs/research/pill-drag-window-move-decision.md` — the decision-point
  record: every approach tried and **ruled out**, with results, and *why*
  `.plain` is the chosen path. **Read this so you don't repeat dead ends.**
- `~/.claude/plans/docs-research-draggable-pane-pills-hand-witty-rabin.md`
  — the overall feature plan (scope, resolver, movePane, insertion-line
  visual, test matrix, forward-compat requirements).
- `docs/research/draggable-pane-pills-handoff.md` — the original briefing
  (sidebar pattern to mirror, prior abandoned branch, code facts).

## Where things stand (checkpoint commit)

- Branch: `worktree-refactor-top-bar`. Checkpoint commit: **`85a5d3c`**
  ("checkpoint: pill-drag decision point …"). Revert here if needed.
- Working dir:
  `/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`.

**Already DONE and green (keep — approach-independent):**
- `TabModel.movePane(_:inTab:relativeTo:placeAfter:)` + `wouldMovePane`
  (`Sources/Nice/State/TabModel.swift`) — mirrors `moveTab` index math,
  scoped to one tab's `panes`, fires `onTreeMutation` only on a real move.
- `PaneStripDropResolver` (`Sources/Nice/Views/PaneStripDropResolver.swift`)
  — pure horizontal slot math + forward-compat `PaneDragOrigin`
  (identity + source context) and `PaneDropDestination` enum.
- Unit tests, 34 green: `Tests/NiceUnitTests/PaneStripDropResolverTests.swift`,
  `TabModelMovePaneTests.swift`, `MovePanePersistenceTests.swift`.
- `UITests/PaneReorderUITests.swift` — harness (launch sandboxed app, grow
  to 3 pills via the `tab.add` button, read pill order by `frame.minX`) +
  `testDragOnPillDoesNotMoveWindow`, currently **`XCTSkipIf(true, …)`** (the
  unsolved item — unskip it once `.plain` lands).

**At this checkpoint the app source is otherwise at baseline** — all the
spike experiments were reverted. The pill has **no drag code** yet; the
toolbar still uses `.windowStyle(.hiddenTitleBar)` + `WindowDragRegion`.

---

## The blocker, in one paragraph

The 52pt top toolbar band is the **native title bar** of a
`.hiddenTitleBar` window, so a press-drag anywhere in it (including on a
pill) moves the window. This cannot be vetoed by SwiftUI gestures
(`.onDrag`, `DragGesture`, `highPriorityGesture` — all tried, all failed)
nor by SwiftUI-embedded `mouseDownCanMoveWindow=false` NSViews (AppKit's
title-bar hit-test doesn't descend into them), and XCUITest's synthesized
drag can't be intercepted by an `NSEvent` monitor. The only lever that
operates below all of that **and** can selectively exclude the pills is to
stop using the native title bar: `.windowStyle(.plain)` + SwiftUI's
`windowBackgroundDragBehavior`, where SwiftUI owns the whole window
background and automatically treats interactive controls as no-drag holes.

---

## Your task: the `.plain` migration

### Step 1 — Make the change (small diff)
1. **Bump deployment target to macOS 15** in `project.yml`. There are 6
   occurrences of `"14.0"` to change: `options.deploymentTarget.macOS`,
   `settings.base.MACOSX_DEPLOYMENT_TARGET`, `LSMinimumSystemVersion`, and
   three per-target `deploymentTarget:` lines.
2. In `Sources/Nice/NiceApp.swift` (scene `body`, ~line 140): change
   `.windowStyle(.hiddenTitleBar)` → `.windowStyle(.plain)` and add
   `.windowBackgroundDragBehavior(.enabled)`.
3. In `Sources/Nice/Views/WindowToolbarView.swift` (~lines 59-68): the
   chrome `.background` is `ZStack { Color.niceChrome(...); WindowDragRegion() }`.
   With the scene modifier owning drag, `WindowDragRegion` is no longer the
   drag mechanism. Decide whether to keep it (harmless) or remove it; if
   removed, also remove the now-unused `WindowDragRegion` type and re-check
   `TitleBarZoomMonitor` (see Step 2c).

### Step 2 — Verify the critical unknowns EARLY (these decide feasibility)
Each is cheap relative to building on a wrong assumption. Verify before
investing in the reorder UI.

- **2a. Does `.plain` still show the traffic-light buttons (close/minimize/
  zoom)?** This is the biggest risk. `.plain` may hide them. If so, you must
  re-add window controls. Check whether `TrafficLightNudger`
  (`Sources/Nice/Views/TrafficLightNudger.swift`, which positions the
  standard buttons) still finds them. **Ask the user for a visual check** —
  screenshots can't be captured from the agent pty.
- **2b. Does `windowBackgroundDragBehavior(.enabled)` under `.plain`
  actually (i) keep empty chrome draggable and (ii) auto-exclude the pills?**
  Unskip `testDragOnPillDoesNotMoveWindow` and run it together with
  `testEmptyToolbarDragMovesWindow`. Note: pills currently use
  `.onTapGesture` (not a `Button`); confirm SwiftUI treats them as no-drag
  holes — if not, you may need to make the pill an actual control or add an
  explicit exclusion. (Under `.hiddenTitleBar` the modifier did NOT carve
  holes; `.plain` is expected to differ because SwiftUI owns the
  background.)
- **2c. Double-click-to-zoom.** `TitleBarZoomMonitor`
  (`Sources/Nice/Views/WindowDragRegion.swift`) walks for a
  `mouseDownCanMoveWindow=true` view; removing `WindowDragRegion` and/or
  switching to `.plain` may break it. The existing
  `testEmptyToolbarDoubleClickZoomsWindow` is **environmentally flaky** (the
  UITest window launches maximized, so a zoom toggles to the same size and
  the assertion never fires) — don't trust it as-is; harden it (e.g.
  un-maximize first, or assert via the zoom action firing) or verify zoom
  manually with the user.
- **2d. Sanity-check** window resize, full-screen (green button + ⌃⌘F
  menu), rounded corners, shadow, and the chrome look under `.plain`.

If 2a/2b show `.plain` is unworkable (e.g. traffic lights can't be
restored acceptably), **stop and report to the user** rather than forcing
it — the fallback options are in the decision doc.

### Step 3 — Once the window-drag conflict is solved
Resume the feature per the plan:
- Wire the reorder gesture on the pill. With the native title bar gone, the
  original SwiftUI `.onDrag`/`.onDrop` pattern (mirroring the sidebar) is
  back in play; the pure `PaneStripDropResolver` + `TabModel.movePane`
  already exist to drive it. (A `DragGesture` is an alternative.)
- Add the horizontal **insertion-line** overlay (2pt accent line; sidebar
  parity — the chosen visual), positioned from the existing `paneFrames`.
- Disambiguate from the pill's existing `.onTapGesture` (select), the
  title click-to-rename (gated by `InlineRenameClickGate`), and the
  close-X.
- Finish the UITest matrix in `PaneReorderUITests.swift`: reorder
  right/left/to-end, the no-window-move guard (unskipped), tap-select,
  click-to-rename, close-X, and relaunch persistence. Keep
  `WindowDragUITests` green.

---

## Build / test / environment rules (important — follow exactly)

- **Acquire the worktree lock around every build/install/test:**
  `scripts/worktree-lock.sh acquire <op>` … `scripts/worktree-lock.sh
  release` (release even on failure). There are `worktree-lock` and
  `nice-install` skills.
- **Only ever touch the `Nice Dev` build**, never the user's prod
  `/Applications/Nice.app` (it hosts the live Claude session). Build/install
  with `scripts/install.sh` (defaults to dev). Tests: `scripts/test.sh`
  (runs `xcodegen` first; forwards `-only-testing:` args). **Never** run
  bare `xcodebuild`/`xcodebuild test` against the `Nice` scheme.
- **Test target names:** unit tests are `NiceUnitTests/<Class>/<test>`; UI
  tests are `NiceUITests/<Class>/<test>` (the folder is `UITests/` but the
  target is `NiceUITests`).
- **Only run the RELEVANT UITests** — the full UI suite is slow. For the
  window-drag work, the relevant pair is
  `NiceUITests/WindowDragUITests/testEmptyToolbarDragMovesWindow` and
  `NiceUITests/PaneReorderUITests/testDragOnPillDoesNotMoveWindow`.
- **Swift 6 + `@MainActor` XCTestCase gotcha:** calling an actor-isolated
  helper from `setUp()` trips "Sending 'self' risks causing data races".
  Seed fixtures **inline in `setUp`** (see the existing movePane tests).
- **SourceKit phantom diagnostics:** after adding a file you'll see
  spurious "No such module 'XCTest'" / "Cannot find type X" until
  `xcodegen` regenerates (which `test.sh`/`install.sh` do). The build is the
  source of truth — don't chase these.
- **Screenshots can't be captured from the agent pty.** For anything
  visual (traffic lights, the drag image, the insertion line), ask the user
  to look — they have been responsive to that.

## Suggested first moves in the new conversation
1. Read the three docs listed at the top.
2. Make the Step 1 change.
3. Acquire the lock; run the two-test drag pair + ask the user for a visual
   check of traffic lights (Step 2a/2b). That single build answers whether
   `.plain` is viable.
4. Only then proceed to the reorder UI + full test matrix (Step 3).
