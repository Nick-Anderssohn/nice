# Handoff: drag-to-reorder pane pills in the top toolbar

**Purpose of the new conversation:** add drag-to-rearrange for the pane
pills in the custom top toolbar (`WindowToolbarView`'s `InlinePaneStrip`).
The new conversation will **build the implementation plan** — this file is
the briefing of what's already known so that planning starts from facts,
not a cold read.

**Paste this whole file into the fresh conversation as the brief.**

---

## Scope (confirm with the user before planning)

The user's ask, verbatim intent: *"drag and rearrange the pane pills in
the upper toolbar."* That is **intra-strip reorder within the active
tab** — moving a pill left/right among its siblings.

**In scope for THIS step: intra-strip reorder only.**

**Explicitly out of scope for this step, but planned for the future** —
the user is building this incrementally, one step at a time:

- Cross-window pane drag (drag a pill from one window's strip into
  another window's strip).
- Tear-off into a new window (drag a pill out of the strip → new window).
- (Possibly) dragging a pill into / out of the sidebar.

### ⚠️ Forward-compatibility is a hard requirement, not a nice-to-have

The user has stated these later features **will** be added. So the
intra-strip design **must not lock them out.** Don't over-fit to
"reorder within one array" in a way that would force a rewrite to add
cross-window / tear-off later. Concretely, when planning, favor choices
that leave the door open:

- Model the dragged thing as a **pane identity + its source context**
  (window / tab / index), not just an array index — a cross-window or
  tear-off drop will need to know *where it came from*.
- Keep the move logic as a **pure resolver** that returns a target
  (slot/index today; later possibly "new window" or "other window's
  strip") rather than mutating arrays inline — so new destination types
  are added cases, not a redesign.
- Don't assume the drop destination is always the same strip. Today it
  is; design the drop handling so another destination (sidebar, another
  window, empty space → tear-off) can be added as a sibling handler.
- Be deliberate about the **drag payload / transfer mechanism**: pick one
  that *can* later carry a pane across process-internal window boundaries
  (the live `NiceTerminalView` + pty can't ride the pasteboard literally —
  the old branch used a side channel for this; note the constraint now so
  the payload design anticipates it, even if this step only moves indices).

This does **not** mean building the future features now — it means not
painting into a corner. Call out in the plan any decision that would be
expensive to reverse when cross-window / tear-off lands.

Confirm scope and this forward-compat intent with the user as step 1 of
planning.

---

## ⚠️ The single most important finding: a prior attempt exists and was deliberately abandoned

There is an earlier draggable-pane-pills attempt in git history, on the
branch **`worktree-draggable-panes`** (present both locally and as
`origin/worktree-draggable-panes`). It is **NOT an ancestor of `main`**.

**The user deliberately did not merge it because it wasn't working well —
possibly not at all.** Treat it as a cautionary reference, **not** a
codebase to resurrect or cherry-pick from. The default plan should be to
**build fresh** on `main`, mirroring the working sidebar-reorder pattern
(see "The pattern to mirror" below). Do not propose merging, rebasing, or
lifting files from that branch unless the user explicitly changes course.

Its value is purely as a record of *what was tried and where it went
wrong* — the audit doc (`docs/research/audit-draggable-tabs.md`,
especially "Anti-patterns present") catalogs the failure modes so you can
avoid repeating them. That audit, plus the architectural divergence below,
is the main thing to learn from the old branch; you don't need to read its
source to plan this work.

Key commits on that branch:
- `2bea2c1` — "Add draggable pane pills with cross-window drag-and-drop"
  (the core, ~2232 insertions across 17 files)
- `0abc4c1` — "Phase B view wiring: PaneDragSource + drop delegates +
  tear-off plumbing"
- `27668f5` — "Add draggable-panes handoff doc" (`docs/draggable-panes-handoff.md`)
- `c05bff6` — "WIP: imperative window-drag attempt + research/audit docs"

Inspect without checking out:
```sh
git show 2bea2c1 --stat
git show worktree-draggable-panes:Sources/Nice/Views/PaneStripDropResolver.swift
git show worktree-draggable-panes:docs/draggable-panes-handoff.md
# or grab a single file into the tree to study:
git checkout worktree-draggable-panes -- Sources/Nice/Views/PaneStripDropResolver.swift
```

Files that branch added (none exist on `main`):
`Sources/Nice/State/PaneDragPayload.swift`, `…/PaneDragState.swift`,
`Sources/Nice/Views/PaneDragSource.swift`,
`Sources/Nice/Views/PaneStripDropResolver.swift`, plus `TabModel.movePane`,
`SessionsModel.adoptPane`, the `NiceServices.pendingTearOff` tear-off slot,
and tests `PaneStripDropResolverTests.swift`, `TabModelMovePaneTests.swift`,
`SessionsModelAdoptPaneTests.swift`.

**Do not reuse this branch.** It was abandoned for not working; carrying
its code forward risks inheriting whatever made the user shelve it. The
file list above is provided so you can *recognize and avoid* its shape —
in particular the heavy cross-window / tear-off plumbing
(`pendingTearOff`, `adoptPane`) and the rejected window-drag strategy.
Even a tempting-looking "pure" piece like `PaneStripDropResolver` should
be re-derived from scratch (and re-tested) against `main` if you want
similar slot math, not copied. Build the feature fresh.

The relevant *research* (as opposed to the abandoned code) lives in two
docs that ARE in this tree — read both before planning; they are the
useful inheritance from the prior attempt:
- `docs/research/draggable-tabs-best-practices.md` — the best-practices
  research (intra-strip reorder, drag-image/placeholder, coexistence with
  window-drag, autoscroll).
- `docs/research/audit-draggable-tabs.md` — an audit of the
  `worktree-draggable-panes` implementation against those practices. The
  "Anti-patterns present" and "Recommended direction" sections are the
  cliff-notes of what went wrong and what to keep.

---

## ⚠️ Architectural divergence: window-drag model changed since that branch

This is the reason you can't just merge the old branch. The two lines
solved "let empty chrome drag the window" in **incompatible** ways:

**`worktree-draggable-panes` branch (old):**
- `isMovable = false` on the whole window + `mouseDownCanMoveWindow =
  false` on each pill view + a `NonDraggableHostingView.hitTest` override.
- Empty-chrome window drag via `performDrag(with:)` called from a
  `ChromeDragView.mouseDown`.

**`main` (current — what you must build against):**
- `WindowDragRegion.DragView` sets `mouseDownCanMoveWindow = true`
  (cooperative AppKit title-bar drag tracker). See
  `Sources/Nice/Views/WindowDragRegion.swift:56-58`.
- Double-click-to-zoom via a process-wide `NSEvent` monitor,
  `TitleBarZoomMonitor`, gated to the top band
  (`WindowDragRegion.swift:64-130`).
- The file header comment (`WindowDragRegion.swift:5-43`) **explicitly
  documents why the `mouseDown`-override / `performDrag` approach (the old
  branch's approach) was abandoned as unviable** under SwiftUI hosting.
  Treat re-introducing `isMovable=false`/`performDrag` as **reopening a
  settled decision** — only do it with explicit user buy-in.

**Implication for pill drag:** a pill drag must be disambiguated from a
window drag *within the cooperative `mouseDownCanMoveWindow=true` model*.
Pills are opaque SwiftUI views laid in front of the `WindowDragRegion`
background, but the audit notes SwiftUI's internal `NSHostingView`
descendants can inherit `mouseDownCanMoveWindow == true`, so a press that
begins on a pill can still be grabbed by AppKit's drag tracker. Validate
empirically early — this is the highest-risk unknown.

---

## The pattern to mirror: sidebar tab reorder (already on `main`, already coexists with window-drag)

`main` already ships click-drag reorder for **sidebar tabs**, and it works
alongside the current window-drag model. Use it as the template rather
than the abandoned branch's heavier AppKit-source hybrid.

In `Sources/Nice/Views/SidebarView.swift`:
- `TabRow` `.onDrag { … }` stashes the dragged tab id (≈ line 654).
- `.onDrop(…)` with a `DropDelegate` on the list (≈ line 270), using a
  named `.coordinateSpace` (≈ line 261).
- Slot math is a **pure, side-effect-free enum** `SidebarDropResolver`
  (defined inline ≈ line 954; tests in
  `Tests/NiceUnitTests/SidebarDropResolverTests.swift`).
- Commits the move via `tabs.moveTab(_:relativeTo:placeAfter:)`
  (`TabModel.swift:434`) on the next runloop tick (≈ line 908) — note the
  "resolve before clearing drag id" ordering fix the code comments call
  out.

This is SwiftUI `.onDrag`/`.onDrop` (NSItemProvider-based), not
`NSPanGestureRecognizer`. It's the lighter, proven-on-`main` approach and
the natural fit for intra-strip-only scope.

---

## Current code facts (the surfaces you'll touch)

**Data model** (`Sources/Nice/State/Models.swift`):
- `struct Pane: Identifiable, Hashable, Sendable, Codable` (line 34).
- `var panes: [Pane]` lives on `Tab` (line 129) — reorder = permuting this
  array. There is **no `movePane` on `main`'s `TabModel`** yet (the old
  branch added one; reuse/adapt it). Persistence: panes are `Codable` and
  saved via `SessionStore` (`PersistedPane`), so order changes should
  flow through whatever already persists tab/pane state — verify a reorder
  survives relaunch.

**The strip** (`Sources/Nice/Views/WindowToolbarView.swift`, ~871 lines,
currently has **zero** drag code):
- `InlinePaneStrip` (≈ line 111) renders pills inside a horizontal
  `ScrollView` (≈ line 231).
- `InlinePanePill` (≈ line 378): each pill already has an `.onTapGesture`
  to select (≈ line 494), a title `.onTapGesture` for click-to-rename
  gated by `InlineRenameClickGate` / `NSEvent.doubleClickInterval`
  (≈ line 574), an `.onHover`, a close-X subview, and a context menu. A
  reorder gesture must **not** swallow select, the rename double-click, or
  the close-X. (The old branch used a ~4pt pan threshold for this.)
- Each pill already publishes its frame in a named coordinate space via
  `PaneFramePreferenceKey` → `paneFrames` (≈ lines 127, 260-305). **Reuse
  these frames for midpoint-bisection slot math** instead of re-measuring.
- Overflow chrome: `OverflowMenuButton` chevron, edge-fade gradients, and
  auto-scroll-to-active (`proxy.scrollTo`, ≈ line 306). Reorder near the
  strip edges ideally auto-scrolls; today nothing auto-scrolls under a
  drag (the audit flags this as a UX gap, not a blocker).
- Accessibility ids `tab.pill.<id>` (≈ line 523) — UITests count/locate
  pills by these. Preserve them.

**Shared geometry:** `Sources/Nice/Views/WindowChrome.swift` —
`WindowChrome.topBarHeight` (52) if a drag indicator needs the band
height. (Added in the just-completed top-bar correctness refactor;
see `docs/done/in-place-refactor-handoff.md`.)

---

## Existing tests / guards to keep green (and where to add new ones)

- `UITests/WindowDragUITests.swift`:
  - `testEmptyToolbarDragMovesWindow` and
    `testEmptyToolbarDoubleClickZoomsWindow` — dragging / double-clicking
    **empty** chrome must STILL move / zoom the window. Your pill-drag
    must not break these.
  - The file's closing comment (≈ line 164) explicitly defers pill
    behavior to "Phase B (draggable-panes-v2)" — this work. Add the
    inverse assertion there: a press-drag that **starts on a pill**
    reorders and does **not** move the window.
- `Tests/NiceUnitTests/WindowToolbarDragRegionTests.swift` — asserts
  `DragView.mouseDownCanMoveWindow == true`; don't regress it.
- Put the new slot-resolution logic in a **pure enum** (à la
  `SidebarDropResolver` / the old `PaneStripDropResolver`) so it's
  unit-tested without a SwiftUI host. The old branch's
  `PaneStripDropResolverTests.swift` (250 lines) and
  `TabModelMovePaneTests.swift` (291 lines) are recoverable references.

---

## Build / test / environment rules (same as the last task)

- Work happens in the worktree at
  `/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar` (branch
  `worktree-refactor-top-bar`) **or** a fresh worktree/branch — confirm
  with the user where this should land. Do not `cd` to the main checkout.
- **Only ever touch the `Nice Dev` build**, never the user's prod
  `/Applications/Nice.app` (it hosts the live Claude session). Install with
  `scripts/install.sh` (defaults to dev — safe). Never run bare
  `xcodebuild`/`xcodebuild test` against the `Nice` scheme.
- **Acquire the worktree lock** around any install/test/xcodebuild:
  `scripts/worktree-lock.sh acquire <op>` … `scripts/worktree-lock.sh
  release`. Build is slow; run install in the foreground with a long
  timeout; release the lock even on failure. (`worktree-lock` and
  `nice-install` skills exist.)
- Tests: `scripts/test.sh` (forwards `-only-testing:` args; runs
  `xcodegen generate` first, so new files are picked up). UITests drive
  the dev bundle.
- **SourceKit caveat:** after adding a new file, the editor spuriously
  reports "cannot find type X in scope" for same-module types until
  `xcodegen` regenerates (which `install.sh` / `test.sh` do). The build is
  the source of truth — don't chase those phantom diagnostics.
- **Screenshots can't be captured from inside the agent's pty** (no Screen
  Recording permission). Drag-reorder is highly visual — plan to ask the
  user for visual confirmation, as was done for the top-bar refactor.

---

## Suggested first moves for the planning conversation

1. **Confirm scope + forward-compat intent** with the user: intra-strip
   reorder only for this step, but designed so cross-window drag and
   tear-off-into-new-window can be added later without a rewrite (the
   user has said those are coming — see the Scope section's
   forward-compatibility requirement).
2. Read `docs/research/draggable-tabs-best-practices.md` and
   `docs/research/audit-draggable-tabs.md` (the latter for its
   "Anti-patterns present" — what to avoid). You do **not** need to read
   the abandoned branch's source; build fresh.
3. Study the live sidebar-reorder pattern in `SidebarView.swift` +
   `SidebarDropResolver` (the template that already coexists with
   `main`'s window-drag).
4. **De-risk the unknown early:** prototype that a press-drag starting on
   a pill is captured by SwiftUI (not stolen by AppKit's
   `mouseDownCanMoveWindow=true` tracker) before committing to a full
   design. This single question drives the whole approach.
5. Then write the plan: pure resolver + `TabModel.movePane` (+ tests),
   gesture wiring on the pill, drop indicator / placeholder, disambiguation
   from tap-select / double-click-rename / close-X, and the
   keep-empty-chrome-draggable guarantee.
