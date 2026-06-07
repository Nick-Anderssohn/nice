# Toolbar Gap Analysis — Nice custom top bar vs. macOS best practices

Compares the **as-built** custom top bar against
`docs/research/custom-macos-toolbar-best-practices.md`, focused on the
developer's goal: making the bar easy to extend with **drag-to-reorder pane
pills** without introducing bugs. Analysis only — no code changed.

All file:line references are to the worktree at
`/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`.

---

## Executive summary

The current bar is a **fully custom `.hiddenTitleBar` implementation**
(`NiceApp.swift:100`) that hand-rolls every responsibility the report says you
inherit by hiding the title bar: traffic-light placement
(`TrafficLightNudger.swift`), window drag (`WindowDragRegion`), and
double-click-to-zoom (`TitleBarZoomMonitor`, a *process-wide* event monitor).
This is the exact architecture the report's top recommendation steers away from
(`NSTitlebarAccessoryViewController`, report §3 and Recommendation #1). The
window-drag/zoom layer in particular has real correctness gaps: zoom ignores the
user's `AppleActionOnDoubleClick` preference, and the zoom hook is a global
`NSEvent` monitor gated by a hard-coded 52pt geometry constant
(`WindowDragRegion.swift:87`).

The **good news for reorder specifically**: the pane data model is already
reorder-ready (`Tab.panes` is an ordered `[Pane]` with stable `Pane.id`,
`Models.swift:129`), and the repo already contains a complete, battle-tested
drag-reorder pattern for sidebar tabs (`SidebarView.swift` —
`ProjectGroupDropDelegate` + `SidebarDropResolver` + deferred `moveTab`) that can
be mirrored almost verbatim for panes. So reorder does **not** require the
foundational title-bar refactor — the two are largely independent. The
foundational refactor is worth doing for the move/zoom/full-screen bug surface,
but it is **not a prerequisite** for shipping reorder, and coupling them would
inflate the risk of the reorder work rather than reduce it.

**Recommendation in one line:** build reorder incrementally on top of the
existing `Tab.panes` model using the in-repo sidebar `DropDelegate` pattern;
treat the `NSTitlebarAccessoryViewController` migration as a separate, later
foundational cleanup, not a gate on reorder.

---

## Current architecture (as-built)

**Window chrome.** The single `WindowGroup(id: "main")` applies
`.windowStyle(.hiddenTitleBar)` (`NiceApp.swift:100`) with
`.windowResizability(.contentSize)`. There is no `NSTitlebarAccessoryViewController`,
no `NSToolbar`, and no native window tabbing anywhere in `Sources/` (confirmed by
search). The bar is an ordinary SwiftUI view stacked above the main content
(`AppShellView.swift:355-358`: `VStack { WindowToolbarView(); mainContent }`).

**The bar view.** `WindowToolbarView` (`WindowToolbarView.swift:25-75`) is a
52pt-tall `HStack`: brand block (`Logo` + "Nice" + separator), then
`InlinePaneStrip` filling remaining width, then `UpdateAvailablePill`. The
background `ZStack` layers `Color.niceChrome` under a `WindowDragRegion`
(`WindowToolbarView.swift:59-68`) so empty chrome is draggable while interactive
children take their own clicks.

**Window drag + zoom.** `WindowDragRegion` (`WindowDragRegion.swift`) is two
pieces:
- `DragView`, an `NSView` subclass overriding `mouseDownCanMoveWindow` to return
  `true` (`WindowDragRegion.swift:56-58`) — this is the report's recommended hook
  (report §6, `[MouseDown]`).
- `TitleBarZoomMonitor` (`WindowDragRegion.swift:64-107`), a **single
  process-wide local `NSEvent` monitor** for `.leftMouseDown`. On a
  `clickCount == 2`, it gates on a hard-coded `yFromTop <= 52`
  (`WindowDragRegion.swift:87-88`), hit-tests, walks the ancestor chain looking
  for any view with `mouseDownCanMoveWindow == true` that is not an
  `NSVisualEffectView`, and calls `window.performZoom(nil)`
  (`WindowDragRegion.swift:90-104`). Installed once from
  `AppShellView.swift:190`. The header comment (lines 25-43) documents a prior
  failed attempt to fold zoom into a `mouseDown` override.

**Traffic lights.** Because the title bar is hidden, `TrafficLightNudger`
(`TrafficLightNudger.swift`) reaches into the host `NSWindow` and manually
re-offsets the close/minimize/zoom buttons, re-applying on
`didBecomeKeyNotification` and `didResizeNotification` (lines 91-101). This is a
hand-rolled replacement for layout AppKit would otherwise own.

**Pane pill state.** `InlinePaneStrip` (`WindowToolbarView.swift:111-344`) reads
the active `Tab` from `TabModel` and renders `ForEach(tab.panes)`
(`WindowToolbarView.swift:233`) inside a horizontal `ScrollView`, each pill keyed
`.id(pane.id)` (line 259). Order is purely the array order of `Tab.panes`.
Overflow chrome (edge fades, chevron, attention badge) is computed from two pure
helpers, `PaneStripOverflowEstimator` and `PaneStripGeometry`, fed by
`PaneFramePreferenceKey` frames and a separately-measured `availableWidth`
(lines 99-160, 260-305). The comment at lines 201-218 notes scalar
`PreferenceKey` closures "simply never fire" in this ScrollView ancestry, so
`availableWidth` is written directly from a `GeometryReader` `onAppear`/`onChange`
— a sign the measurement layer is already fragile.

**Pane data model.** `Pane` (`Models.swift:34-79`) is an `Identifiable, Hashable,
Sendable, Codable` value with a stable `let id: String`. `Tab.panes: [Pane]` is
"Ordered panes shown as pills in the toolbar" (`Models.swift:127-129`), plus
`activePaneId`. Pane mutations funnel through `TabModel.mutateTab(id:)`
(`TabModel.swift:120-128`), which copies the tab, applies a transform, writes back
only on change, and persistence is triggered via `onTreeMutation`. `addPane`
appends to `tab.panes` (`SessionsModel.swift:578-586`); pane removal does
`tab.panes.remove(at:)` (`SessionsModel.swift:284`). **There is no `movePane` /
`reorderPane` method today** (confirmed by search).

**Existing reorder precedent (sidebar tabs).** `SidebarView.swift` already
implements live drag-reorder for sidebar tabs: `TabRow.onDrag` stashes a drag
session and returns an `NSItemProvider(object: tab.id as NSString)` — a built-in
`String` payload, not a custom UTI (`SidebarView.swift:654-657`);
`ProjectGroupDropDelegate` (lines 868-946) drives a drop indicator from a frame
snapshot via the pure `SidebarDropResolver`, validates on `.text`, and in
`performDrop` **defers the model mutation to the next runloop tick**
(`DispatchQueue.main.async`, lines 907-913) with an explicit comment that
mutating the array inline "leaves AppKit's drag tracker stuck on a subsequent
drag." The model mutation is `TabModel.moveTab` (lines 434-449), a clean
remove-then-insert with index adjustment, paired with a no-op-predicting
`wouldMoveTab` (lines 454-463).

---

## Gap table

| Area | Current approach | Best practice (report) | Severity | Impact on adding reorder |
|---|---|---|---|---|
| Title-bar architecture | `.hiddenTitleBar` + fully custom bar (`NiceApp.swift:100`) | Host custom strip in `NSTitlebarAccessoryViewController`; let AppKit own the title bar (§3, Rec #1) | **High** (foundational) | Indirect. Custom chrome doesn't block reorder, but its bug surface raises the cost of *any* change in this file. |
| Traffic-light layout | Hand-offset via `TrafficLightNudger` on key/resize notifications | AppKit lays out for free under native title bar (§1, §7) | **Med** | Low — orthogonal to reorder, but more hand-rolled state to keep coherent during edits. |
| Double-click-to-zoom | Always `performZoom`; ignores user preference (`WindowDragRegion.swift:99`) | Read `AppleActionOnDoubleClick` from `NSGlobalDomain`; branch Maximize/Minimize/None, live (§6, `[DblClick30166]`) | **High** (correctness bug) | Low for reorder mechanics, but it's in the file you'll be editing. |
| Window-drag region | `mouseDownCanMoveWindow = true` on a `DragView` — matches report | `mouseDownCanMoveWindow` per region (§6) | **Low** (correct) | Helps. Pills already sit *above* the drag region; a drag gesture on a pill won't fight window-move if the pill consumes the event. |
| Zoom event delivery | Process-wide `NSEvent` monitor gated by hard-coded 52pt + class sniffing (`WindowDragRegion.swift:72-104`) | Native title-bar region handles double-click; no global hook needed (§3, §6) | **Med** | Med — adding a drag handler inside the 52pt strip means another consumer of the same `leftMouseDown` events the monitor inspects; interaction must be verified. |
| Pane ordering model | `Tab.panes: [Pane]` ordered array, stable `Pane.id` (`Models.swift:129`) | Pure `array.move(fromOffsets:toOffset:)` on source-of-truth (§5) | **Low** (ready) | Strongly helps — model is already shaped for reorder. |
| Reorder mutation API | None (`addPane`/remove exist; no `movePane`) | Single move method on the model | **Low** (easy add) | Small, well-scoped addition mirroring `moveTab`. |
| DnD infrastructure | None in toolbar; mature pattern exists in sidebar | Custom `ForEach` + `DropDelegate` moving on `dropEntered`, built-in payload, deferred mutation (§5.2, Rec #3) | **Low** (precedent exists) | Strongly helps — `SidebarView` is a copy-ready template incl. the deferred-mutation foot-gun fix. |
| Accessibility for reorder | Pills are AXButtons with selected state; **no non-drag reorder path** | Provide context-menu/keyboard "Move Left/Right"; gate animation on Reduce Motion (§7, Rec #6) | **Med** | Net-new work reorder must add; not blocked by anything. |
| Overflow/measurement layer | Frame-preference + direct-`@State` measurement; documented fragility (`WindowToolbarView.swift:201-218, 286-305`) | n/a (custom-strip-specific) | **Med** | Med — reorder animates pill frames, which feed the same preference plumbing; reorder + overflow interaction needs testing. |

---

## Detailed gaps

### 1. Fully custom title bar instead of `NSTitlebarAccessoryViewController` (High, foundational)

`NiceApp.swift:100` commits to `.windowStyle(.hiddenTitleBar)`, and the codebase
then reimplements by hand the three things the report (§1) says you take over by
doing so:

- **Window move** — `WindowDragRegion.DragView` (correct, uses
  `mouseDownCanMoveWindow`).
- **Double-click zoom** — `TitleBarZoomMonitor` (buggy; see gap 2).
- **Traffic-light layout** — `TrafficLightNudger`, re-applied on key/resize
  notifications.

The report's Recommendation #1 is explicit: keep the pill visuals custom but stop
fully replacing the title bar — host the SwiftUI strip in an
`NSTitlebarAccessoryViewController` so "AppKit owns move/zoom/full-screen/traffic-
lights again." None of that is present.

**Why it matters for reorder:** it doesn't block reorder, but it's the reason the
bar is "risky to extend." Every change to `WindowToolbarView` shares a file
neighborhood with hand-rolled chrome state (the zoom monitor, the nudger) that has
no test coverage for the interaction with new gestures. The report's full-screen
caveat (§1) is also unverified here — I found no full-screen transition handling
for the custom bar, so its behavior entering/exiting full screen is an open
question I could not confirm from the code.

### 2. Double-click-to-zoom ignores the user preference (High, correctness)

`TitleBarZoomMonitor` unconditionally calls `window.performZoom(nil)` on any
in-region double-click (`WindowDragRegion.swift:99`). The report (§6,
`[DblClick30166]`) is emphatic that this is a **user preference**, not a constant:
read `AppleActionOnDoubleClick` from `NSGlobalDomain` and branch — `performZoom`
for `Maximize`, `performMiniaturize(nil)` for `Minimize`, **nothing** for `None`
— and read it *live* because it can change at runtime. The current code is exactly
the "(a) ignoring `AppleActionOnDoubleClick`" regression the report calls out as
the most common custom-bar bug. A user who set "Do Nothing" (or "Minimize") will
see the wrong behavior.

This is a pre-existing bug independent of reorder, but it lives in the file the
reorder work touches and illustrates the cost of hand-rolling title-bar behavior.

### 3. Process-wide event monitor gated by a magic 52pt constant (Med)

The zoom monitor is a single app-wide `NSEvent.addLocalMonitorForEvents`
(`WindowDragRegion.swift:72`) that inspects *every* left mouse-down in the
process, then geometrically gates on `yFromTop <= 52` (lines 87-88) and excludes
`NSVisualEffectView` by class (line 98) to avoid zooming on sidebar/terminal
double-clicks. This is fragile on two axes the report's native approach avoids
entirely:

- The `52` is duplicated from the bar's `.frame(height: 52)`
  (`WindowToolbarView.swift:57`) with no shared constant — a future bar-height
  change silently desyncs the zoom region.
- It depends on AppKit class sniffing (`!(v is NSVisualEffectView)`) and ancestor
  `mouseDownCanMoveWindow` walking, which the file's own header (lines 25-43)
  documents as the *third* approach after two failures.

**Why it matters for reorder:** a drag-to-reorder gesture installed inside the
52pt strip becomes another consumer/competitor for the same `leftMouseDown`
stream the monitor watches. Whether the monitor's `clickCount == 2` gate and a
pill drag handler coexist cleanly is exactly the kind of interaction that "makes
the bar risky to extend." It should be explicitly tested if reorder is built on
top of the current chrome.

### 4. No reorder mutation on the model, but the model is ready (Low)

`Tab.panes` is already an ordered array of `Identifiable` value types
(`Models.swift:129`, `Pane.id` at line 35). The report (§5, Rec #3) wants the move
expressed as a pure `array.move(fromOffsets:toOffset:)` on the source of truth —
which `mutateTab` (`TabModel.swift:120`) supports directly. There is simply no
`movePane` method yet. Adding one is a small, well-scoped mirror of the existing
`moveTab` (`TabModel.swift:434-449`), including a `wouldMovePane` no-op predictor
for indicator suppression (mirroring `wouldMoveTab`, lines 454-463). No model
restructuring is required.

### 5. Accessibility: no non-drag reorder path, no Reduce Motion gating (Med)

Pills are already decent AX citizens: each is an AXButton with selected state
(`WindowToolbarView.swift:522-525`), and the overflow menu has rich labels
(lines 731-732, 789-809). But the report (§7, Rec #6) requires a **non-drag
alternative** for reorder (context-menu "Move Left/Right" or keyboard), since
pointer-drag reorder is not VoiceOver/Switch-Control friendly, and wants reorder
animations gated on Reduce Motion. The current pill context menu
(`WindowToolbarView.swift:504-515`) has only Rename/Close. These are net-new
items the reorder work must add; nothing blocks them.

### 6. Measurement/overflow plumbing is already fragile (Med)

The strip's overflow chrome leans on `PaneFramePreferenceKey` frames merged with
care to survive ScrollView virtualization (`WindowToolbarView.swift:289-305`) and
on `availableWidth` written directly from a `GeometryReader` because "those
preference closures simply never fire" in this ancestry
(`WindowToolbarView.swift:201-218`). Reorder animates pill frames as they move,
feeding the same preference system that already misbehaves here. The risk is not
correctness of the move itself but visual coherence (edge fades, chevron, active-
pane auto-scroll at `WindowToolbarView.swift:306-311`) during a drag. This needs
in-app testing — and the report notes (§5) that Xcode Previews don't run
`DropDelegate` actions, so the toolbar previews (lines 849-871) won't catch it.

---

## What specifically makes drag-to-reorder hard today

Honest assessment: **less than the framing suggests.** The hard parts are mostly
already solved in-repo.

What helps (already in place):
- **Ordered model with stable ids.** `Tab.panes` + `Pane.id` is exactly the
  source-of-truth shape the report wants for `array.move(...)`.
- **A complete, proven DnD template.** `SidebarView`'s tab reorder is the report's
  recommended pattern already implemented for this codebase: built-in `String`
  payload (`SidebarView.swift:656`, avoiding the macOS custom-UTI pitfall the
  report flags in §5), a pure resolver (`SidebarDropResolver`), and — critically —
  the **deferred-mutation fix** (`DispatchQueue.main.async` in `performDrop`,
  `SidebarView.swift:907-913`) for the "AppKit drag tracker gets stuck" foot-gun.
  Porting this to panes is mostly mechanical.
- **The window-drag region sits *behind* the pills** (`WindowToolbarView.swift:59-68`),
  so a drag started on a pill is consumed by the pill, not the window-move tracker
  — provided the pill keeps a non-transparent `contentShape` (it does:
  `WindowToolbarView.swift:493`), which also satisfies the report's "transparent
  areas don't initiate drags" warning (§5).

What genuinely adds friction:
- **The zoom monitor shares the `leftMouseDown` stream** with any new in-strip
  gesture (gap 3). Coexistence must be verified, not assumed.
- **The fragile measurement/overflow plumbing** (gap 6) will interact with the
  animated frame changes a reorder produces.
- **No non-drag/AX path or Reduce-Motion gating** yet (gap 5) — net-new, but
  unblocked.
- **No previews/coverage for live DnD** — must be exercised in the running app.

None of these require the title-bar foundational refactor to resolve.

---

## Recommended path forward

### Can add incrementally (the smallest change that unblocks reorder)

Build reorder on the existing foundation, mirroring the sidebar pattern:

1. **Add `TabModel.movePane(tabId:paneId:relativeTo:placeAfter:)`** plus
   `wouldMovePane(...)`, mirroring `moveTab`/`wouldMoveTab`
   (`TabModel.swift:434-463`) and going through `mutateTab` so persistence
   (`onTreeMutation`) fires for free.
2. **Add `.onDrag` to `InlinePanePill`** returning
   `NSItemProvider(object: pane.id as NSString)` (built-in `String` payload, per
   report §5 / matching `SidebarView.swift:656`).
3. **Add a `PanePillDropDelegate` + pure `PaneStripDropResolver`** modeled on
   `ProjectGroupDropDelegate` / `SidebarDropResolver`, driving an insertion
   indicator from the `paneFrames` the strip already collects
   (`WindowToolbarView.swift:127`), and **deferring the `movePane` call** to the
   next runloop tick exactly as the sidebar does (`SidebarView.swift:907-913`).
   Add a container-level drop delegate so end/empty drops work (report §5).
4. **Add the AX/keyboard path and Reduce-Motion gating** from the start (gap 5):
   context-menu "Move Left / Move Right" on the pill, animation gated on
   `accessibilityReduceMotion`.
5. **Verify two interactions in the running app, not previews:** (a) drag vs. the
   `TitleBarZoomMonitor` `leftMouseDown` stream (gap 3), and (b) reorder animation
   vs. the overflow edge-fade/chevron/auto-scroll plumbing (gap 6).

Trade-off: this keeps the buggy chrome (gaps 1-3) in place. That's acceptable
because those bugs are independent of reorder and the move work is small and
isolated.

### Must refactor first? No — but do it as a separate follow-up

The `NSTitlebarAccessoryViewController` migration (report Rec #1) is the right
*foundational* cleanup: it deletes `TitleBarZoomMonitor`, the
`AppleActionOnDoubleClick` bug (gap 2), the magic-52 monitor (gap 3), and most of
`TrafficLightNudger`, and recovers full-screen handling. But it is a **larger,
higher-risk change** touching window setup, full-screen, and the 52pt-vs-fixed-
title-height clipping caveat the report itself flags as unverified (§3, Uncertainties).
Coupling it to reorder would make a small feature ride on a risky rewrite.

Sequencing: ship reorder on the current foundation first; then do the title-bar
accessory migration as its own change, after which the reorder code is unchanged
(it lives entirely in the SwiftUI strip the accessory would host). If you only fix
*one* chrome bug opportunistically while in these files, fix the
`AppleActionOnDoubleClick` handling (gap 2) — it's a small, clearly-correct
change.

**Bottom line:** do **not** refactor the foundation before attempting reorder. The
model and a proven DnD pattern are already in place; reorder is an incremental,
low-risk addition. Schedule the title-bar accessory migration separately for the
move/zoom/full-screen bug surface.

### Uncertainties

- **Full-screen behavior of the custom bar** is not handled anywhere I found;
  I could not confirm from code whether it transitions correctly. Worth checking
  manually.
- **52pt strip in a title-bar accessory** may clip (report §3 caveat) — relevant
  only if/when the foundational refactor happens.
- **Persistence of pane order**: `mutateTab` writes back and `addPane` already
  persists pane lists, so a `movePane` through `mutateTab` should persist via the
  same `onTreeMutation` path — but I did not trace the full serialization of
  `Tab.panes` order end-to-end, so confirm reordered panes survive relaunch.
