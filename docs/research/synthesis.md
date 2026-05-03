# Synthesis: title bar × draggable panes — refactor plan

Companion to:
- `macos-custom-title-bar.md` (research)
- `draggable-tabs-best-practices.md` (research)
- `audit-title-bar.md` (audit)
- `audit-draggable-tabs.md` (audit)

## The conflict in one sentence

The pane-drag feature reached for `NSWindow.isMovable = false` to stop
pills from triggering window-drag, and that one line collapsed the
window's draggable surface to the literal pixels of three
`ChromeDragView` instances — only one of which (the sidebar's 52pt
strip) is actually unmasked by an opaque sibling. That's why dragging
the empty top bar no longer moves the window.

## Are the two features actually in conflict?

**No.** Both audits independently arrive at the same architecture, and
neither relies on `isMovable = false`. The features collide today
because of *implementation choices*, not anything intrinsic.

What each side actually needs:

| Title bar wants                                                       | Pane drag wants                                                              |
|-----------------------------------------------------------------------|-------------------------------------------------------------------------------|
| `isMovable = true` so AppKit's drag-region tracker can fire on chrome | Pills must NOT contribute to the drag region                                 |
| Cooperative `mouseDownCanMoveWindow` walk over empty chrome           | Source pill `mouseDown` belongs to `NSPanGestureRecognizer` → drag session   |
| `ChromeDragView` only as fallback over `NSVisualEffectView` strips    | Cross-window/tear-off via `NSDraggingSession` (already correct)              |
| `NSTitlebarAccessoryViewController` for the toolbar (long-term)       | Live-view reparenting of `NiceTerminalView` (already correct)                |

Both wishlists are satisfied by the same pattern: **suppress drag at
the widget level** (`mouseDownCanMoveWindow = false` on each
pill/widget hosting view, plus the existing `hitTest` override on
`NonDraggableHostingView`), and **leave the window cooperative**
(`isMovable = true`).

## Why `isMovable = false` was reached for, and why dropping it should be safe

The handoff says "Adding Layer 1 closed the rest of the gap" — i.e.
Layer 2 (`mouseDownCanMoveWindow = false` on `NonDraggableHostingView`)
and Layer 3 (`hitTest` override claiming `self`) were not on their own
sufficient when the pill subtree was integrated into the real toolbar.
Per BJ Homer's gist (research §"Drag behaviour"), the title-bar
tracker walks the hit-test result *upwards* checking
`mouseDownCanMoveWindow`. Layer 3 should make AppKit stop at the
`NonDraggableHostingView` (because it always wins the in-bounds
hit-test) and read its `false` flag. So in theory Layer 2+3 should
have been sufficient.

Plausible reasons it wasn't, ordered by likelihood:

1. **The override is bounds-checked with a half-open interval at
   edges.** Boundary pixels fall through to `super.hitTest`, which
   descends into SwiftUI internals where `true` reigns. The handoff
   itself flags this at line 248-253: "be careful with the half-open
   interval at the edges."
2. **The drag was being initiated from a chrome region, not the pill
   itself.** With `WindowDragRegion` masked behind the opaque
   `Color.niceChrome` fill (audit, "Diagnosis" §), the empty toolbar
   regions inherit `mouseDownCanMoveWindow = true` from SwiftUI's
   default and trigger window-drag. That isn't actually a regression
   from the pill bug — it was the *intended* behaviour, but it became
   indistinguishable from the pill-drag bug because both pill *and*
   chrome were dragging the window.
3. **A SwiftUI hosting wrapper between `InlinePaneStrip` and the pill
   isn't a `NonDraggableHostingView` and reports `true` for some
   subregion the pill exposes.**

In all three cases, the right fix is at the widget level, not the
window level. (1) is solved by tightening the override to only claim
`self` when `super.hitTest(point) == nil` AND the point is strictly
inside `bounds.insetBy(dx: 0.5, dy: 0.5)`. (2) is solved by the
toolbar layout fix (put `WindowDragRegion` *above* the chrome fill, or
better, restore cooperative drag and remove the explicit drag view
from this position). (3) is solved by walking up to find the actual
contributing wrapper and applying `mouseDownCanMoveWindow = false`
there too — there shouldn't be many.

## The recommended refactor (sequenced, smallest first)

### Step 1 — restore cooperative window drag (one-line revert + layout fix)

- Delete `window.isMovable = false` at `AppShellView.swift:205`.
- In `WindowToolbarView.swift:60-67`, swap the ZStack order so the
  `WindowDragRegion()` is layered *above* `Color.niceChrome`. (Or
  drop the explicit drag region entirely from this position, since
  cooperative drag now handles empty chrome.)
- Manual smoke test: drag empty top bar → window moves; drag a pill
  → pane drag starts; double-click empty chrome → window zooms.

If smoke passes, we're done with the user's reported bug. The fixes
below are quality-of-life improvements that the audits surfaced.

### Step 2 — narrow the `hitTest` override

If smoke test step 1 reveals the pill bug returns:

- Change `NonDraggableHostingView.hitTest` to:
  ```
  if let leaf = super.hitTest(point), leaf !== self { return leaf }
  return bounds.contains(point) ? self : nil
  ```
  This preserves SwiftUI's internal hit-testing for things like the
  close-X and tap-to-select while still claiming `self` for the
  "AppKit drag-region computation" walk.
- Walk the pill subtree once with a debug print to find any
  `NSHostingView` descendants that aren't `NonDraggableHostingView`
  and tag them similarly.

### Step 3 — disambiguate pill-drag vs window-drag in `mouseDown` (research §"Coexistence" #4)

If step 2 still leaves bugs (or as a cleaner long-term replacement
for the hit-test override):

- In `PaneDragSource.NonDraggableHostingView`, override
  `mouseDown(with:)` to capture, watch `mouseDragged` for movement
  direction:
  - Predominantly horizontal motion past ~10pt → start
    `beginDraggingSession` (pane drag).
  - Predominantly vertical motion or large delta → call
    `self.window?.performDrag(with: event)` (window drag).
  - Movement under threshold → tap/click (don't consume).
- This makes the pill self-disambiguating and removes the dependency
  on AppKit's drag-region computation entirely. It's the pattern the
  draggable-tabs research [13] cites Chromium and iTerm2 using.

Steps 2 and 3 are alternatives — pick one.

### Step 4 — add `acceptsFirstMouse(for:)` on widget hosts

Title-bar audit "Anti-patterns" §4. Wrap each toolbar widget's
`Button` / `.onTapGesture` in a small `NSViewRepresentable` whose
hosted NSView returns `true` from `acceptsFirstMouse(for:)`. Removes
the "click-twice on inactive window" papercut. Low priority.

### Step 5 — fix the `pendingTearOff` fragility (handoff §304-329)

Independent of the title-bar work. Pane-drag audit Phase 1:

- Tag `PendingTearOff` with a destination-window-session-id minted
  before `openWindow(id: "main")`; filter `consumeTearOff` strictly
  on that match.
- Add a 2s TTL.
- Defer source persistence (`onSessionMutation`) until after the
  destination has absorbed.

This is the single highest-impact change for cross-window stability
and doesn't touch the title bar.

### Step 6 (long-term, optional) — move toolbar into `NSTitlebarAccessoryViewController`

Title-bar audit "Recommended direction" §1. Real fix for the
"hand-rolled fake toolbar" anti-pattern. Big change — touches
`AppShellView`, `WindowToolbarView`, `TrafficLightNudger`, and the
overflow logic. Defer until everything else is stable; capture as a
follow-up.

### Step 7 (long-term, optional) — placeholder reorder animation

Pane-drag audit Phase 2. UX polish: animate pills aside as the drag
crosses boundaries. Out of scope for the conflict-resolution work.

## What the audits agree on but I'm explicitly NOT recommending yet

- **Removing the three-layer defence in depth.** The title-bar audit
  says Layers 2+3 are "architecturally dead code" once `isMovable =
  false` exists. The pane-drag audit says Layer 2+3 is the *correct*
  pattern and Layer 1 is the over-aggressive belt. Once we restore
  `isMovable = true` (step 1), Layer 1 goes away naturally and Layers
  2+3 become the load-bearing defence — which is what the pane-drag
  research endorses. Don't pre-emptively remove Layers 2+3.
- **Switching the SwiftUI `DropDelegate`s to `NSDraggingDestination`.**
  Pane-drag audit calls this a violation of best practice but
  acknowledges it's "pragmatically defensible." Real cost is the
  `didDropOnTarget` flag and coordinate-space ambiguity; both are
  livable. Defer.

## Risk register for step 1 (the one-line fix)

| Risk                                                                                  | Mitigation                                       |
|---------------------------------------------------------------------------------------|--------------------------------------------------|
| Pill bug returns (drag-pill drags window)                                             | Step 2 or step 3 immediately                     |
| Other toolbar widgets (logo, +, gear) start dragging window when clicked              | Wrap their hosts in a `NonDraggableHostingView`  |
| Sidebar's `NSVisualEffectView` chrome stops responding to drag                        | Keep its `WindowDragRegion()` (already there)    |
| Double-click-to-zoom regresses on `NSVisualEffectView` strips outside `ChromeDragView`| Pre-existing (handoff §"What's broken" §4); tackle separately if user notices |
| The unit test `PaneDragSourceWindowDragTests` fails                                   | Expected — that test fixture asserts `false`-everywhere; rewrite to assert the cooperative-drag invariant instead |

## Bottom line

The two features can coexist cleanly. The bug is one line + one
layout swap away. The pane-drag implementation's source side is
mostly right; the only thing it got wrong was reaching for a
window-level switch (`isMovable = false`) when widget-level
suppression was sufficient. The title-bar implementation has deeper
architectural issues (hand-rolled toolbar, no accessory view
controller, manual traffic-light nudging) that would be worth
addressing eventually but are not blocking the user's reported bug.
