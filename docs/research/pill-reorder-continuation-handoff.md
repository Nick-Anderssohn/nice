# Handoff: finish pane-pill drag-to-reorder (window-drag blocker SOLVED)

**Paste this whole file into the new conversation as the brief.**

You are continuing **drag-to-reorder for the pane "pills"** in the custom
top toolbar of a SwiftUI macOS app called **Nice**. The one hard blocker
that sank every prior attempt — *dragging a pill also drags the whole
window* — is now **solved and regression-tested**. What remains is the
(well-templated) reorder drop side and its test matrix.

Branch / worktree: `worktree-refactor-top-bar` at
`/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`.

---

## Read these first (in the repo)

- `docs/research/pill-drag-window-move-decision.md` — full history of
  every approach tried/ruled out, ending in **UPDATE 3 (RESOLVED)** which
  records the final working architecture. Read UPDATE 3 at minimum.
- `~/.claude/plans/docs-research-draggable-pane-pills-hand-witty-rabin.md`
  — the feature plan (resolver, movePane, drag state, drop delegate,
  insertion line, full test matrix). **Its Step 0 / "do NOT reintroduce
  isMovable=false" guidance is SUPERSEDED** — see "Corrections to the
  plan" below. Everything from Step 1 onward (the reorder mechanics) still
  applies and is the template to follow.
- `Sources/Nice/Views/SidebarView.swift` — the proven `.onDrag`/`.onDrop`
  tab-reorder pattern to mirror horizontally (drag state, drop delegate,
  insertion line, deferred mutation).

---

## The window-drag blocker: how it was solved (don't re-litigate)

Under `.hiddenTitleBar` the whole 52pt top band is the native title bar,
so a press-drag anywhere in it (pills included) moved the window. The fix,
now in place and green:

1. **`window.isMovable = false`** — set in `AppShellView`'s
   `WindowAccessor` callback (main window only; Settings untouched).
   Disables the native title-bar drag for the whole band, so a pill drag
   can't move the window. This is the load-bearing fix.
2. **Empty-chrome window drag restored** via a SwiftUI
   `DragGesture(minimumDistance: 2)` on `WindowToolbarView`, attached as a
   plain `.gesture` (yields to child gestures), whose `onChanged` calls
   `NSApp.keyWindow?.performDrag(with: NSApp.currentEvent!)`. `performDrag`
   moves the window even with `isMovable == false`, and XCUITest's
   synthesized drag DOES drive it (so it's testable).
3. **Selectivity** — the pill's `.onDrag { NSItemProvider(object: pane.id
   as NSString) }` claims pill drags, so the lower-priority window-drag
   `.gesture` yields. Dragging a pill ⇒ no window move.
4. **Zoom / chrome** — `TitleBarZoomMonitor` and native chrome (traffic
   lights, rounded corners, shadow) are fully intact. `WindowDragRegion`/
   `DragView` is kept ONLY as the zoom monitor's `mouseDownCanMoveWindow`
   marker; it no longer drives any drag (see its code comment).

**Both differential UITests pass:**
`NiceUITests/PaneReorderUITests/testDragOnPillDoesNotMoveWindow` (pill →
window does NOT move) and
`NiceUITests/WindowDragUITests/testEmptyToolbarDragMovesWindow` (empty
chrome → window moves).

**Do NOT** revert to `.plain` (it strips all native chrome) or to an
`NSEvent`-monitor / view-`mouseDown` drag (XCUITest can't drive those).
The SwiftUI-gesture `performDrag` is the verified path.

---

## What's DONE (keep)

- **`TabModel.movePane(_:inTab:relativeTo:placeAfter:)` + `wouldMovePane`**
  (`Sources/Nice/State/TabModel.swift`) — index math mirrors `moveTab`,
  scoped to one tab's `panes`, fires `onTreeMutation` only on a real move.
- **`PaneStripDropResolver`** (`Sources/Nice/Views/PaneStripDropResolver.swift`)
  — pure horizontal slot math + forward-compat `PaneDragOrigin` /
  `PaneDropDestination`.
- **34 unit tests green**: `PaneStripDropResolverTests`,
  `TabModelMovePaneTests`, `MovePanePersistenceTests`.
- **Window-drag blocker solved** (4 points above), both drag UITests green.
- **Pill `.onDrag` source wired** in `InlinePanePill`
  (`WindowToolbarView.swift`, just after `.contentShape`). It currently
  only returns the provider — there is **no `.onDrop` yet**, so a pill
  drag claims the gesture (no window move) but does **not reorder**.

### Exact current diff from checkpoint `bf801c5`
- `Sources/Nice/Views/AppShellView.swift` — `window.isMovable = false`.
- `Sources/Nice/Views/WindowToolbarView.swift` — `.gesture(windowDragGesture)`
  + the `windowDragGesture` property; pill `.onDrag`.
- `Sources/Nice/Views/WindowDragRegion.swift` — comment-only (DragView
  role clarified; no behaviour change).
- `UITests/PaneReorderUITests.swift` — `testDragOnPillDoesNotMoveWindow`
  unskipped + comment updated; the throwaway diagnostic test removed.

---

## What REMAINS (the work to do)

Follow the plan's Steps 3–8 (the reorder is now unblocked):

1. **Drag-state holder** `PaneStripDragState` (mirror `SidebarDragState`):
   `@MainActor @Observable`, holds `PaneDragSession?` (origin + current
   target). Inject via `.environment`.
2. **`.onDrop` + `PaneStripDropDelegate`** (mirror `ProjectGroupDropDelegate`):
   attach inside the named coordinate space
   `"InlinePaneStrip.scrollContent"` (so `DropInfo.location` shares
   coordinates with `paneFrames`); drive `PaneStripDropResolver` then
   `TabModel.movePane`.
3. **Deferred mutation (non-negotiable)**: in `performDrop`, resolve
   synchronously → `dragState.session = nil` → `DispatchQueue.main.async {
   tabs.movePane(...) }`. Mutating `tab.panes` inline wedges AppKit's drag
   tracker (the sidebar does the same deferral).
4. **Insertion-line overlay**: 2pt vertical `Color.niceAccent` line at the
   target slot, positioned from `paneFrames[id].minX/.maxX`, in the named
   coordinate space. Sidebar parity. `.allowsHitTesting(false)`.
5. **Disambiguation** — confirm the pill `.onDrag` coexists with:
   `.onTapGesture` select, title click-to-rename (gated by
   `InlineRenameClickGate` / `doubleClickInterval`), and the close-X
   (`CloseXButton`, its own Button). Leave the accessibility ids
   (`tab.pill.<id>`, `tab.close.<id>`, `tab.add`, `.title`/`.titleField`)
   untouched — UITests depend on them.
6. **UITest matrix** in `UITests/PaneReorderUITests.swift` (harness +
   `orderedPaneIds` already exist): reorder right / left / to-end,
   tap-select, click-rename, close-X, relaunch persistence. Keep the two
   drag guards green.
7. **Harden the flaky zoom test**
   `WindowDragUITests/testEmptyToolbarDoubleClickZoomsWindow`: it leaves
   the window zoomed/full-screen for the next test (order-dependent).
   Un-zoom / un-full-screen in `setUp`/`tearDown`, or assert via the zoom
   action firing rather than a size delta.

Auto-scroll-during-drag (plan Step 7) is a deferred stretch — skip unless
strips routinely overflow.

---

## Corrections to the plan (important)

The plan predates the window-drag decision and is wrong on these points —
the resolved architecture above takes precedence:

- The plan's Step 0 says **"do NOT reintroduce `isMovable=false` /
  `performDrag`."** That guidance is **reversed**: `window.isMovable =
  false` + SwiftUI-gesture `performDrag` IS the chosen, working, tested
  solution. The plan's earlier `.onDrag`-moves-the-window failure was
  under `isMovable == true` (native drag live); with it off, `.onDrag`
  works.
- The plan's "fully automated, no manual smoke testing" framing holds for
  the *reorder* and the *pill-no-move* and *empty-chrome-drag* guards (all
  XCUITest-automatable). The only thing that needed a human this round was
  one-off visual confirmation of native chrome.

---

## Build / test / environment rules (follow exactly)

- **Worktree lock around every build/install/test:**
  `scripts/worktree-lock.sh acquire <op>` … `scripts/worktree-lock.sh
  release` (release even on failure). Skills: `worktree-lock`,
  `nice-install`.
- **Only ever touch the `Nice Dev` build**, never prod
  `/Applications/Nice.app` (it hosts the live Claude session). Build/
  install: `scripts/install.sh` (defaults to dev). Tests:
  `scripts/test.sh` (runs `xcodegen`; forwards `-only-testing:` args).
  **Never** run bare `xcodebuild`/`xcodebuild test` against the `Nice`
  scheme.
- **Test target names:** unit `NiceUnitTests/<Class>/<test>`; UI
  `NiceUITests/<Class>/<test>` (folder `UITests/`, target `NiceUITests`).
- **Run only the RELEVANT UITests** — the full UI suite is slow. The
  window-drag pair is
  `NiceUITests/WindowDragUITests/testEmptyToolbarDragMovesWindow` +
  `NiceUITests/PaneReorderUITests/testDragOnPillDoesNotMoveWindow`.
- **SourceKit phantom diagnostics** ("No such module 'XCTest'", "Cannot
  find type X", "Cannot find 'WindowChrome'") appear until `xcodegen`
  regenerates — the build is the source of truth; don't chase them.
- **Screenshots can't be captured from the agent pty** — ask the user for
  any visual check (they have been responsive).

## Suggested first moves in the new conversation
1. Read UPDATE 3 of the decision doc + the plan's Steps 3–8.
2. Build the drag-state holder + `.onDrop`/`PaneStripDropDelegate` +
   deferred `movePane`, then the insertion line.
3. Under the lock, run the unit suite + the reorder UITests + the two drag
   guards. Iterate to green.
