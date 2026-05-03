# Audit: draggable-pane / tab-strip implementation

## Summary

- The implementation is a **hybrid AppKit-source + SwiftUI-destination** model:
  drag is initiated by `NSPanGestureRecognizer` calling
  `beginDraggingSession(...)` (correct per [3]); drops are handled by SwiftUI
  `DropDelegate`s, not `NSDraggingDestination` (deviates from [1] but is
  pragmatically defensible).
- **Live-view migration is sound** — the same `NiceTerminalView` instance is
  reparented via `detachPane`/`attachPane`, preserving the pty + scrollback
  per the research's [5][6][9] recommendation.
- **No drag-image animation / placeholder during reorder.** A static drag
  image follows the cursor and a static 2pt accent line marks the insertion
  slot, but pills never animate aside / leave a placeholder gap. This is the
  Chromium [9] anti-pattern called out in the research doc.
- **The `pendingTearOff` mechanism is the most fragile piece** — global
  process-wide slot, no TTL, overwrite-while-pending leaks panes, source
  mutation committed before destination ever absorbs. The handoff doc already
  flags this; the research doc doesn't address tear-off plumbing in detail
  but Chromium's overlay-window approach [13] avoids the entire class of
  issue by never having an "in transit" state.
- **Coexistence with window-drag uses the BJ-Homer-recommended imperative
  pattern** (`isMovable=false` + `performDrag`) — this is exactly what the
  research [7][8] and the parallel-auditor seam call for. The
  `NonDraggableHostingView.hitTest` override is over-broad but not unsound;
  see "Coexistence" section.

## Findings vs. best practices

### Intra-strip reorder

**VIOLATED — no placeholder gap.** Research [9]: "insert a placeholder for
the given tab at the appropriate mouse location" and animate surrounding
tabs into place. The implementation paints a 2pt accent vertical line
(`WindowToolbarView.swift:382-386`) but the source pill stays in its
original position at 0.4 opacity (`WindowToolbarView.swift:542`) and the
neighbouring pills do not slide aside to make room. Effect: the user sees
the cursor + drag image hovering over an unchanged strip with a thin
indicator line — fine for a v0, but not the Chromium-class behaviour.

**FOLLOWED — slot bisection against tab midpoints.** Research §"Hit
testing": "bisect against tab midpoints to compute the target index — do
not use absolute positions of moving tabs (their frames are mid-animation)."
`PaneStripDropResolver.computeRawSlot` (`PaneStripDropResolver.swift:104-127`)
does exactly this with `cursorX < frame.midX ? idx : idx + 1`. Because pills
don't animate during the drag, the "frames are mid-animation" caveat
doesn't bite us — but the bisection is still the right call.

**FOLLOWED — separate visual slot vs. final array index.** Research
implicitly assumes this; `PaneStripDropResolver.Outcome` carries both
`finalIndex` and `visualSlot` (`PaneStripDropResolver.swift:25-39`). The
same-tab adjustment (`if srcIdx < f { f -= 1 }`,
`PaneStripDropResolver.swift:88`) is the correct post-removal index math.

**PARTIAL — no-op drop suppression.** Research doesn't explicitly cover
this, but `PaneStripDropResolver.swift:91-92` suppresses the indicator on
"drop on current position" — good. However the same-tab path commits
through `SessionsModel.adoptPane` (`WindowToolbarView.swift:819`), which
is the **cross-window** code path, not the intra-tab `TabModel.movePane`
path. This means same-tab reorders take the heavier
detach-attach-to-self route. Functional but wasteful and the orchestration
duplication is called out in the handoff (`docs/draggable-panes-handoff.md:290-303`).

**N/A — autoscroll on drag near strip edges.** Research mentions AppKit's
free autoscroll via `NSDraggingSession`. Not exercised; pills live in a
horizontal `ScrollView` (`WindowToolbarView.swift:235`) and there's no
explicit hook to scroll the strip when the drag image approaches its
edges. The strip *can* scroll, but doesn't auto-scroll under a drag —
worth noting as a UX gap, not a violation per se.

### Cross-window drag

**FOLLOWED — pasteboard carries identifiers, not the live view.**
`PaneDragPayload` (`PaneDragPayload.swift:20-58`) JSON-encodes
`(windowSessionId, tabId, paneId, kind)` onto an `NSPasteboardItem` with a
custom UTI `dev.nickanderssohn.nice.pane`
(`PaneDragSource.swift:124-129`). Matches research [9] design exactly.

**FOLLOWED — destination resolves the live view via in-process registry.**
`WindowRegistry.appState(forSessionId:)` recovers the source `AppState`
from the pasteboard payload (`WindowToolbarView.swift:806`,
`SidebarView.swift:748`). The view is detached from the source's
`TabPtySession` and re-attached to the destination's via `attachPane`
(`SessionsModel.swift:569-575`). Mirrors the Chromium TabContents-transfer
pattern in [9].

**FOLLOWED — `NSDragOperation.move` for both contexts (within-app).**
`PaneDragSource.swift:155-169` returns `.move` for `.withinApplication`
and `[]` for `.outsideApplication`. The `[]` for outside is the *correct*
choice because we want the no-target signal to spawn a new window
(commented at `PaneDragSource.swift:160-163`). Slight nuance: research
[1] suggests returning `.move` for both so the source can clean up, but
the implementation deliberately uses `[]` outside to detect tear-off,
which is its own correct pattern.

**VIOLATED — drop side is SwiftUI `DropDelegate`, not
`NSDraggingDestination`.** Research [1][9] consistently uses
`NSDraggingDestination` with `registerForDraggedTypes`. The
implementation uses SwiftUI `.onDrop(of: [PaneDragPayload.utType], delegate: ...)`
in `PaneStripDropDelegate` (`WindowToolbarView.swift:776-871`) and
`TabRowPaneDropDelegate` (`SidebarView.swift:719-806`). Concrete impacts
of this divergence:

- The delegates use `info.location` (SwiftUI local point in the drop view)
  rather than `convert(_:from: nil)` from a screen point. This is one of
  the things the handoff calls out at `docs/draggable-panes-handoff.md:331-338`
  as "potentially mismatched coordinate spaces".
- `dropEntered`/`dropUpdated`/`dropExited` lifecycle is similar to the
  AppKit equivalent but `concludeDragOperation` has no SwiftUI analog;
  cleanup happens inside `performDrop` and the source's
  `draggingSession(_:endedAt:operation:)`.
- The `didDropOnTarget` flag (`PaneDragState.swift:38-40`) exists because
  the SwiftUI side cannot directly tell the AppKit drag source "I
  accepted, don't tear off". With a pure AppKit destination,
  `NSDragOperation` returned from `performDragOperation(_:)` propagates
  back through the system to the source's `endedAt:operation:` — no shared
  flag needed. The handoff already flags this at
  `docs/draggable-panes-handoff.md:340-347`.

**PARTIAL — `dropExited` cleanup.** Both delegates clear the indicator
only if they own it (`WindowToolbarView.swift:797-800`,
`SidebarView.swift:738-742`). Correct guard against cross-delegate
clobber. But neither clears `dragState.session` itself on exit — that
relies entirely on the AppKit source's `endedAt` callback
(`PaneDragSource.swift:182`) running. Generally fine, but if SwiftUI's
DropDelegate lifecycle ever fires `performDrop` without `endedAt` (or
vice-versa), session leaks become possible.

### Tear-off into new window

**FOLLOWED — lazy spawn timing.** Research §"Tear-off" recommends "Lazy
(recommended for SwiftUI/AppKit apps): wait for `endedAt:operation:`."
Implementation does exactly this (`PaneDragSource.swift:171-183`) — no
overlay window, no eager spawn while dragging.

**FOLLOWED — uses `screenPoint` from `endedAt:`, not
`NSEvent.mouseLocation`.** Research anti-pattern: "Spawning at
`NSEvent.mouseLocation` instead of the `endedAt:` `screenPoint` — they
can disagree by a frame, producing a visible jump." Implementation
captures `screenPoint` directly (`PaneDragSource.swift:178`) and threads
it through `requestPaneTearOff` → `PendingTearOff.cursorScreenPoint` →
`AppShellHost.applyPendingTearOffOriginIfReady` →
`window.setFrameTopLeftPoint(origin)` (`AppShellView.swift:150-159`).
Correct.

**PARTIAL — does not subtract tab-offset-within-strip from screenPoint.**
Research [9]: "position it so the tab strip sits under the cursor
(subtract the tab's offset within the strip from `screenPoint`)." The
implementation positions the window's *top-left corner* at the cursor
release point, so the released pane jumps relative to the cursor by
whatever offset within the strip it had. A user dragging a pill at index
3 will see the new window's traffic lights pop under their cursor, not
the pill itself. Minor UX polish issue, not a correctness one.

**VIOLATED — no overlay-window option / no eager option.** Research lists
both lazy and eager (Chromium-style) as "valid" with eager being "higher
fidelity, much more code." Implementation only does lazy. Acceptable for
v0 but worth noting; the eager path would also obviate `pendingTearOff`
plumbing entirely (the new window already exists when the drag releases
into it).

**VIOLATED — `pendingTearOff` is a process-wide slot.** This is the
research doc's "Anti-patterns" §3 from the cross-window section
generalised — encoding the live `NSView` itself onto the pasteboard
isn't quite what's happening, but the workaround (a side-channel
`var pendingTearOff: PendingTearOff?` on `NiceServices.swift:70`) has
analogous failure modes:
- Overwrite-while-pending leaks the prior pane + pty
  (`NiceServices.swift:249` blindly assigns).
- `consumeTearOff(for:)` (`NiceServices.swift:265-271`) just gives the
  payload to "any AppState that isn't the originator" — there's no
  matching of intent. If the user opens a new window via ⌘N at exactly
  the wrong moment, that window steals the torn-off pane.
- No TTL — if `openWindow(id: "main")` (`WindowToolbarView.swift:333`)
  fails to spawn (Stage Manager / Mission Control quirks), the slot
  stays full forever.
- Source mutation (model + persistence schedule via `onSessionMutation`,
  `NiceServices.swift:247`) commits *before* destination absorption.
  If destination never mounts, the source has already saved without the
  pane — restore loses it.
The handoff doc enumerates all four of these at
`docs/draggable-panes-handoff.md:304-329`.

**PARTIAL — origin-application choreography is ordering-robust.** The
`pendingTearOffOriginToApply` + `hostedWindow` `@State` pair triggers
`applyPendingTearOffOriginIfReady` on either change
(`AppShellView.swift:326-327`), so it doesn't matter whether
`WindowAccessor` or `.task` wins. Good defensive coding — but it's
defending against a fragility that better tear-off architecture wouldn't
have in the first place.

### Drag initiation & drag-image

**FOLLOWED — uses `NSDraggingSession` not raw `mouseDragged`.**
`PaneDragSource.handlePan` (`PaneDragSource.swift:111-140`) calls
`view.beginDraggingSession(with:event:source:)` from
`NSPanGestureRecognizer.began`. Avoids the research anti-pattern "Driving
reorder off `NSEvent` `mouseDragged` only".

**FOLLOWED — drag-image via `setDraggingFrame(_:contents:)`.** Uses the
quick-path single-image API per [11]
(`PaneDragSource.swift:131`). Snapshot taken via
`bitmapImageRepForCachingDisplay` + `cacheDisplay`
(`PaneDragSource.swift:142-151`) — correct AppKit pattern for capturing a
view to an `NSImage`.

**FOLLOWED — custom UTType pasteboard payload.**
`dev.nickanderssohn.nice.pane` (`PaneDragPayload.swift:28`) — distinct
from the sidebar tab-reorder drag's `.text`-based `NSItemProvider`
(`SidebarView.swift:469-470`). Research §"Cross-window" requires this so
SwiftUI/AppKit routes each drag to the right delegate.

**PARTIAL — drag-distance threshold.** Research [14] / iTerm2 use a 10pt
default; Chromium ~3pt. Implementation relies on `NSPanGestureRecognizer`
defaults (commented at `PaneDragSource.swift:44-45` as "4pt is AppKit's
default drag-recognition slop"). 4pt is reasonable but isn't tunable, and
on a trackpad with palm contact a stationary press could plausibly start
a drag. Worth verifying empirically.

**PARTIAL — drag-session bookkeeping is set EARLY.**
`dragState.session = PaneDragSession(payload: payload)` runs *before*
`beginDraggingSession` (`PaneDragSource.swift:135-139`). Comment at
`:133-134` claims this is so "source-pill fade + drop indicators are
live from the first hover frame." Plausibly correct, but if
`beginDraggingSession` ever fails (e.g. event no longer valid), the
session gets stuck set with no `endedAt` to clear it. No guard.

### Drop indicators

**FOLLOWED — pane-strip indicator paints a 2pt vertical accent line
between pills.** `WindowToolbarView.swift:378-407` resolves the visual
slot to an x position via `indicatorX(for:in:)` and overlays a
`Rectangle().fill(Color.niceAccent)`. Standard Chromium-style insertion
indicator.

**FOLLOWED — sidebar-row indicator (separate target type).**
`PaneDropTarget.sidebarTabRow(tabId:)` (`PaneDragState.swift:29-30`); the
sidebar paints its own highlight (not visible in the snippets I read but
referenced from `SidebarView.swift:797`). Different visual treatment from
the pane strip is appropriate — different semantic (drop here = join this
tab / spawn new tab in this window).

**PARTIAL — no animation as cursor crosses pill boundaries.** Research
recommends animating sliding pills out of the way as the placeholder
inserts. Implementation's indicator just snaps to the new x position. The
2pt line jumps; pills don't move. See "Intra-strip reorder" violation
above for the full picture.

### Live-view migration

**FOLLOWED — `NiceTerminalView` is reparented, not recreated.**
`TabPtySession.detachPane` (`TabPtySession.swift:352-355`) removes the
view from its dict without terminating the process; `attachPane`
(`:366-381`) wires a fresh delegate, re-applies cached theme/font, and
re-arms `onFirstData`. The pty `Process` lives on the model side
(SwiftTerm's `LocalProcessTerminalView`), so reparenting preserves it
exactly as research [5][6] calls for.

**FOLLOWED — view lifecycle hooks via SwiftTerm subclass overrides.**
`NiceTerminalView.viewDidMoveToWindow` (`NiceTerminalView.swift:90`) and
`viewDidMoveToSuperview` (`NiceTerminalView.swift:97`) re-engage Metal
renderer + first-responder-on-attach. Research [6] recommends
`viewWillMove(toWindow:)`; the implementation uses the *did* variants,
which fire after the move completes — for the resources here (Metal
context, focus latch) that's fine because both want the new window
state, not the pre-move state.

**PARTIAL — `makeFirstResponder` after migration.** Research anti-pattern:
"Forgetting to call `NSWindow.makeFirstResponder(_:)` on the migrated
view in the new window." The implementation does call this via the
`wantsFocusOnAttach` latch (`NiceTerminalView.swift:39`,
`:106-111`) but the latch is set by `TerminalHost.makeNSView` for the
*active* pane only (`TerminalHost.swift:41-48` referenced). For a torn-
off pane, `attachPane` doesn't set `wantsFocusOnAttach = true` at
`SessionsModel.swift:569-575` — focus depends on the destination tab
becoming `activeTabId`, which `adoptPane` does at `:583`, then the next
`TerminalHost.updateNSView` sets the latch. Multi-hop, but it works.

**FOLLOWED — order of attach-then-detach.** `SessionsModel.adoptPane`
explicitly comments at `:566-571`: "NSView's single-parent rule means
doing this BEFORE source-detach guarantees the view never has a
window-less moment." Good awareness; correct order.

**PARTIAL — `adoptPaneLaunchState` doesn't restart the launch grace
timer.** Handoff doc flags this at
`docs/draggable-panes-handoff.md:355-360`. A `.pending`-state pane
dragged mid-launch never gets a destination-side promotion to `.visible`,
so the "Launching…" overlay never shows on the new window even if the
process stays silent past the 0.75s window. Research doesn't cover this
specifically; it's a Nice-internal detail.

### Coexistence with window-drag

**FOLLOWED — `mouseDownCanMoveWindow = false` on the source pill view.**
`NonDraggableHostingView.mouseDownCanMoveWindow` returns `false`
(`PaneDragSource.swift:88`). Research [7][8] and Brent Simmons-adjacent
gist [8] explicitly require this for every titlebar subview.

**FOLLOWED — `isMovable = false` on the entire window.**
`AppShellView.swift:205`. Belt-and-braces with the per-view override.
Research doesn't specifically endorse the `isMovable=false` approach
because it's more aggressive than needed for the standard case, but it's
a defensible defence-in-depth choice for a custom-titlebar app and the
imperative `performDrag(with:)` path in `WindowDragRegion` keeps chrome
drag working.

**FOLLOWED — `performDrag(with:)` for the empty chrome region.**
`WindowDragRegion.ChromeDragView.mouseDown` (`WindowDragRegion.swift:49-56`).
Research §"Required moves" #3: "Implement an explicit drag empty strip
background → move window gesture using `NSWindow.performDrag(with:)`."

**PARTIAL — `hitTest` override is over-broad.** This is the grey-area
question the prompt asks me to evaluate.

`NonDraggableHostingView.hitTest` (`PaneDragSource.swift:90-93`) returns
`self` for every in-bounds point, short-circuiting AppKit's descent into
SwiftUI's internal NSView tree. The comment at `:65-86` and the unit
tests in `Tests/NiceUnitTests/PaneDragSourceWindowDragTests.swift` argue
that this is necessary because SwiftUI internals (transparent
`NSHostingView` descendants) inherit `mouseDownCanMoveWindow == true`.

**Assessment as a drag-source pattern:**
- *Soundness for click/hover/drag-init on the pill itself:* the comment
  is right that SwiftUI's gesture router runs internally inside
  `NSHostingView`'s event-handling path, *after* the AppKit hit-test has
  already chosen the host. So `onTapGesture`, `onHover`, and the close-X
  inside the pill should still work — the SwiftUI router descends the
  SwiftUI tree without consulting AppKit's hit-test decision again. The
  handoff confirms this for the production app at
  `docs/draggable-panes-handoff.md:30-36` ("user manually verified after
  the latest install").
- *Tab-strip best-practice equivalent:* the canonical AppKit pattern (per
  [8]) is to override `mouseDownCanMoveWindow = false` on every relevant
  subview. The hit-test override is a *workaround* for not being able to
  reach inside SwiftUI's hosting machinery to apply the override
  individually — SwiftUI's internals are private NSViews you can't
  subclass. So the override is the right pragmatic answer for a
  SwiftUI-hosted custom titlebar; an all-AppKit tab strip would not need
  it.
- *Risks:* the override returns `self` for every in-bounds point, which
  means *anything* SwiftUI relies on `super.hitTest` for in this view is
  bypassed. The handoff specifically warns at `:248-253`: "fall back to
  'only claim self when `super.hitTest(point) == nil`'" — a more
  surgical version that wouldn't disturb SwiftUI's own hit-test results.
  The current version works because the `NSPanGestureRecognizer` is
  attached to `hosting` (the view returning `self`), so AppKit's drag
  recognition still works; SwiftUI's internal router handles the rest.
  But it's coarse.
- *Alternative pattern:* register the `NSPanGestureRecognizer` on the
  hosting view as today, override only `mouseDownCanMoveWindow=false`,
  and rely on `isMovable=false` to handle the rest. Drop the `hitTest`
  override entirely. This would only re-introduce the bug if AppKit's
  drag tracker reads `mouseDownCanMoveWindow` from a *descendant* the
  override on `self` doesn't catch — which `isMovable=false` should
  already preclude. Worth empirically validating.

**FOLLOWED — drag threshold prevents micro-drags from initiating.**
Research §"Required moves" #4: "Threshold the tab-drag start." Pan
recogniser default is 4pt (`PaneDragSource.swift:44-45`); on the higher
side of "click vs drag" but defensible.

## Anti-patterns present

1. **Mutating the model during the drag instead of using a placeholder.**
   Research §"Intra-strip reorder anti-patterns": "Mutating the model
   during the drag … produces visible 'jump' frames whenever the cursor
   crosses a tab boundary." Mitigated here by *not* mutating until
   `performDrop` — but also not animating pills aside. Net effect: the
   strip looks static under the drag image, which is the opposite UX
   problem (less feedback rather than too-jumpy feedback). The research
   doc's recommendation of an animated placeholder gap is unimplemented.
   File:line: `WindowToolbarView.swift:378-407` (indicator-only),
   absence of pill-frame animation in `pillCell`
   (`WindowToolbarView.swift:314-371`).

2. **Mid-flight drag-source state cleared *only* by the AppKit `endedAt`
   callback.** `dragState.session = nil` clearing in
   `PaneDragSource.swift:182` is the only cleanup path. SwiftUI
   `DropDelegate.dropExited` deliberately doesn't clear it
   (`WindowToolbarView.swift:797-800` — only the indicator/target).
   Combined with the early-set at `PaneDragSource.swift:135`, a
   `beginDraggingSession` failure leaves `session` set indefinitely.
   Lower-likelihood but non-zero.

3. **Process-wide mutable slot for in-flight live-view transfer**
   (`NiceServices.pendingTearOff`, `NiceServices.swift:70`). Research
   doesn't enumerate this exact anti-pattern but the cross-window
   anti-pattern §"Encoding the live `NSView` itself onto the pasteboard"
   is the same shape — a side-channel for the live view because the
   pasteboard can't carry it. Chromium [13] avoids the entire class via
   eager overlay window.

4. **Source persistence committed before destination absorbs**
   (`NiceServices.requestPaneTearOff` calls `onSessionMutation?()` at
   `NiceServices.swift:247`, before the new window mounts). Research
   anti-pattern §"Cross-window": doesn't directly enumerate this but
   it's the moral equivalent — committing source-side work before the
   destination has accepted means a failure mid-flight loses data.

5. **Same pasteboard payload used to drive the model resolve via a
   side channel** — `PaneStripDropDelegate.performDrop` reads the
   payload from `dragState.session?.payload`
   (`WindowToolbarView.swift:803`) rather than from the dropped
   `NSItemProvider`. Works because both windows share one process, but
   bypasses the pasteboard's role as the canonical source of truth. If
   `dragState.session` ever races with `performDrop` (e.g. another drag
   starts before the previous's `endedAt` runs), the wrong payload
   could be acted on. Not enumerated by the research doc but is
   architecturally suspect.

## Novel choices

1. **Hybrid `NSDraggingSource` + SwiftUI `DropDelegate`.** The research
   doc doesn't cover this combination explicitly — it presents AppKit
   and SwiftUI as alternatives. The implementation uses AppKit purely
   for the source side (`PaneDragSource.swift`) and SwiftUI
   `.onDrop`/`DropDelegate` for the destination side
   (`WindowToolbarView.swift:296-305`, `SidebarView.swift:472-480`).
   Pragmatic: AppKit gives the tear-off-detection signal that SwiftUI
   doesn't expose, but SwiftUI's drop delegates compose more easily
   into a SwiftUI view tree. The cost is the `didDropOnTarget` flag and
   the coordinate-space ambiguity called out above.

2. **Separate `PaneDropTarget` enum vs. drop-delegate-side state.**
   `PaneDragState.session.target` (`PaneDragState.swift:21-30`) is a
   single enum that holds either a pane-strip slot or a sidebar-row
   target. Allows the source-side drag to render UI consistently
   regardless of which destination type is hovered. Reasonable design.

3. **Per-window `PaneDragState` rather than process-wide.**
   `PaneDragState` is `@State` in `AppShellHost`
   (`AppShellView.swift:96`) and propagated via `.environment`. Each
   window has its own; cross-window drags use *both* — source's drives
   the source-pill fade, destination's drives the destination
   indicator. Sound separation of concerns, but means cross-window
   coordination flows through the registry-resolved `AppState`
   reference rather than shared state. Works, somewhat clever.

4. **Three layers of window-drag suppression.** `isMovable=false` +
   `mouseDownCanMoveWindow=false` + `hitTest` override
   (`PaneDragSource.swift:64-93`). Research only requires layer 2.
   The handoff doc justifies the redundancy at `:163-171`. Defence in
   depth is reasonable when the alternative is intermittent
   `mouseDown`-stealing bugs that are notoriously hard to reproduce.

5. **`PaneStripDropResolver` and `SidebarDropResolver` are pure
   side-effect-free enums.** The drag-and-drop logic is split into a
   pure resolver (testable without a SwiftUI host) and a thin delegate
   that calls into it. Research doesn't address this but it's a clean
   separation pattern that makes the indicator decisions
   independently verifiable. Good practice.

## Recommended direction

**Phase 1 — fix the fragile bits without redesign:**

- **Eliminate `pendingTearOff` failure modes by pre-tagging the
  destination.** Mint a destination-window-session-id at
  `requestPaneTearOff` time before calling `openWindow(id: "main")`;
  filter `consumeTearOff` strictly on that match (handoff option (a)).
  Add a TTL (e.g. 2s) so a failed window-spawn can recover. Defer
  source persistence until destination has absorbed (handoff option
  (d)).
- **Drop the `didDropOnTarget` side-channel.** Switch the destinations
  to `NSDraggingDestination` (or have the SwiftUI `DropDelegate`
  return a real `NSDragOperation` that the AppKit source can read in
  `endedAt:operation:`). Simpler invariant: "tear off iff
  `operation == []`."
- **Make the resolver same-tab path use `TabModel.movePane`** instead
  of round-tripping through `adoptPane`. Removes the heavy
  detach-and-attach-to-self.

**Phase 2 — close the UX gap with placeholder reorder:**

- Add a "drag-in-flight" placeholder model: when a pane is the source
  of an active drag, replace its pill with a transparent
  `Color.clear`-filled spacer the same width, and animate slot changes
  via SwiftUI's implicit animation on the `panes` array's positional
  state. Or use a lightweight `paneDragState`-driven layout offset
  that shifts pills aside as `visualSlot` changes.
- Animate the indicator x to its new position rather than snapping —
  small touch but reads as polished.

**Phase 3 — consider eager / overlay-window tear-off (Chromium-style):**

- The research [13] eager-overlay path eliminates the
  `pendingTearOff` plumbing entirely. The trade-off is more code in
  `PaneDragSource` (track distance from strip; create
  `NSWindow.borderless` overlay; reparent live view to it; on release
  promote overlay to full window or reabsorb if hovering a
  destination). Probably out of scope for v1; capture as a long-term
  direction.

**Phase 4 — coexistence cleanup:**

- Once the rest is stable, empirically test removing the
  `NonDraggableHostingView.hitTest` override and relying solely on
  `isMovable=false` + `mouseDownCanMoveWindow=false`. The override is
  doing real work today (the unit tests prove the structural
  invariant) but it's also the suspect-#1 surface for the hover/click
  regressions the handoff anticipates at `:393-409`. If `isMovable`
  alone holds, drop the override. If not, fall back to the surgical
  variant suggested in the handoff: `super.hitTest(point) == nil ?
  self : super.hitTest(point)`.

**Sketched architecture (no code):**

```
PaneDragSource (NSPanGestureRecognizer → beginDraggingSession)
   │
   ├── pasteboard: PaneDragPayload (custom UTI)
   │
   ├── source bookkeeping: PaneDragState.session (per-window)
   │
   └── on endedAt:
         operation == [] → tear-off via destination-tagged
                           PendingTearOff with TTL
         operation == .move → no-op (destination committed)

Destination (NSDraggingDestination, ideally — or shared SwiftUI
DropDelegate today):
   │
   ├── PaneStripDropDelegate — intra-strip slot picker
   │     (placeholder model + animated pill positions)
   │
   ├── TabRowPaneDropDelegate — sidebar row absorber
   │
   └── on performDragOperation:
         resolve payload → in-process WindowRegistry lookup
         → reparent NiceTerminalView via attach-then-detach
         → return .move (source clears via endedAt)
```

The fundamentals are right. The fixes are mostly making the existing
design more robust (TTL, tagged tear-off, real `NSDragOperation`
return) and adding the placeholder-reorder polish.
