# Audit: custom title bar implementation

## Summary

- The chrome-drag view itself (`WindowDragRegion.ChromeDragView`) is a textbook implementation of the research doc's recommended pattern: `performDrag(with:)` from `mouseDown`, `mouseDownCanMoveWindow = false`, `acceptsFirstMouse = true`, double-click → `performZoom`. Layer 2 of the design is correct.
- The window-level config (`window.isMovable = false` in `AppShellView.swift:205`) is the architectural root cause of the user's "drag region too small" bug. It is exactly the lever the research doc says NOT to pull when you have an explicit drag view: `isMovable = false` not only kills cooperative drag, it also prevents AppKit from computing *any* drag region from the hit-test chain — leaving only the literal pixels of `ChromeDragView` instances as draggable. Every other surface in the toolbar (the `Color.niceChrome` fill, the brand `HStack`'s padding, the SwiftUI hosting wrappers, anything visually empty) is now NOT a drag handle.
- The "title bar" is a hand-rolled `HStack` instead of `NSToolbar` / `NSTitlebarAccessoryViewController`. The research doc explicitly classes hand-rolled fake toolbars as an anti-pattern, but it's a load-bearing one here (the pill strip with overflow + tear-off is genuinely outside `NSToolbar`'s box). The cost is real: zero menu-bar mirroring, no customization sheet, lost unified-window-tab integration, and the entire `WindowDragRegion` plumbing exists only because `NSToolbar` would have given drag-region semantics for free.
- Three layered defences are stacked to suppress drag inheritance from pill descendants. With `isMovable = false`, defences 2 and 3 (`mouseDownCanMoveWindow = false` on `NonDraggableHostingView`, hit-test override) are architecturally dead code from the title-bar side — `isMovable = false` already prevents AppKit's title-bar tracker from firing anywhere in the window. They belong to the pane-drag auditor (the hit-test override is about ensuring the pan recogniser, not the window-drag tracker, wins).
- The toolbar's `WindowDragRegion()` background is laid out *behind* every other child of the HStack but only covers what's not opaque-painted on top. In the current scene, the `Color.niceChrome` fill at the same Z layer covers the full 52pt strip, so empty toolbar pixels DO route to `ChromeDragView` in principle. The shrinkage isn't from layout — it's from `isMovable = false`. See the diagnosis section.

## Findings vs. best practices

### Drag behaviour

**Use `performDrag(with:)` from a custom view's `mouseDown`** — *FOLLOWED.* `WindowDragRegion.swift:49-56` does exactly this in `ChromeDragView.mouseDown(with:)`, with the double-click → `performZoom` short-circuit at line 51-54.

**Leave `isMovable = true` on the window** — *VIOLATED.* `AppShellView.swift:205` explicitly sets `window.isMovable = false`. The research doc lists this as the master switch that disables all user-initiated window movement (item under "APIs to know"); the recommended pattern leaves it at `true` so the explicit drag view still participates in AppKit's drag-region computation. The comment block at `AppShellView.swift:191-204` correctly identifies the *symptom* it's defending against (transparent SwiftUI hosting NSViews inheriting `mouseDownCanMoveWindow == true` and the title-bar tracker firing on stray clicks), but reaches for the most destructive lever rather than the surgical one. Direct cause of the user's "drag region shrunk to almost nothing" bug.

**Do not set `isMovableByWindowBackground = true` when there are widgets** — *FOLLOWED (by omission).* No code sets it. The "macOS 15 SwiftUI equivalent" `windowBackgroundDragBehavior(.enabled)` is also not used (`NiceApp.swift:94` only declares `.windowStyle(.hiddenTitleBar)` and `.windowResizability(.contentSize)`).

**`mouseDownCanMoveWindow = false` on embedded controls/widgets** — *PARTIAL.* The research doc's nuance: `false` on embedded controls is the right way to mark them as "not a drag handle" so AppKit's drag-region calculation skips them. The codebase has only one explicit `false` (on `ChromeDragView` itself, `WindowDragRegion.swift:43`, which is correct as belt-and-braces against re-enabling `isMovable`); pills get it via `NonDraggableHostingView` (`PaneDragSource.swift:88`). All the OTHER toolbar widgets — `Logo` (`WindowToolbarView.swift:32`), the brand `Text("Nice")` (`:34`), the vertical separator `Rectangle` (`:42`), `UpdateAvailablePill` (`:53`), `OverflowMenuButton`, `NewTabBtn`, the close `×` button — sit in SwiftUI hosting NSViews whose `mouseDownCanMoveWindow` flag is whatever SwiftUI's default is (`true` for transparent ancestors). With `isMovable = true` and no explicit override, AppKit would treat these strips as drag-eligible *or* the buttons as drag handles depending on hit-test depth. The codebase "solved" this by killing `isMovable` window-wide instead of marking individual widgets.

**`acceptsFirstMouse(for:)` on title-bar drag/widget views** — *PARTIAL.* `ChromeDragView.acceptsFirstMouse` returns `true` (`WindowDragRegion.swift:47`) — correct. But the toolbar widgets (logo, pills, +, chevron, gear, the two `SidebarModeIconButton`s, `SidebarToggleButton`) all use SwiftUI `.onTapGesture` / `Button` inside SwiftUI hosting views with no `acceptsFirstMouse` override. Per ref [10] in the research doc, on inactive windows the first click on a SwiftUI custom-button-style widget will activate the window without invoking the action — so first-click-on-inactive-window for any toolbar widget is a "click twice" experience. Not catastrophic; standard SwiftUI macOS limitation pre-15. None of the widgets opt in via the Christian Tietze workaround.

**Don't track `mouseDragged` and call `setFrameOrigin` manually** — *FOLLOWED.* No manual frame-tracking drag exists. `ChromeDragView.mouseDown` hands off to AppKit immediately.

**Don't layer an `NSDraggingSource` on top of the drag region without disambiguation** — *PARTIAL.* `PaneDragSource` does layer an `NSDraggingSource` (the pan recogniser → `beginDraggingSession`) on top of `WindowDragRegion`. The research doc's recommended fix is to disambiguate inside `mouseDown` via movement threshold and decide between `beginDraggingSession` (data drag) and `performDrag` (window drag). The codebase's chosen disambiguation is structural instead: pills sit in their own `NSHostingView` subclass (`NonDraggableHostingView`) that hit-test-claims its bounds, so chrome-drag and pane-drag don't share a `mouseDown`. Functionally workable, but it required `isMovable = false` plus the hit-test override to make stick — the research doc's threshold approach would have avoided both. Most of the architectural force here lives on the pane-drag side; flagged here for the auditor seam.

### Title-bar widget layout

**Use `NSToolbar` with `windowToolbarStyle(.unified)`** — *VIOLATED.* `NiceApp.swift:94` declares only `.windowStyle(.hiddenTitleBar)` (no `.toolbar { }` block, no `.windowToolbarStyle(...)`). The entire toolbar is hand-rolled inside `WindowToolbarView` (`WindowToolbarView.swift:25-75`) as a SwiftUI `HStack` with a `Color.niceChrome` background. This is the "fake toolbar" anti-pattern explicitly called out under the layout section's anti-patterns. Cost of choosing this path: no menu-bar mirroring helper, no overflow-into-`>>` for free (re-implemented in `OverflowMenuButton`/`PaneStripOverflowEstimator`), no customization sheet, and — most relevant to the bug — no built-in drag-region semantics, which is why `WindowDragRegion` exists at all.

**Use `NSTitlebarAccessoryViewController` for non-button accessories** — *VIOLATED.* No accessory view controller anywhere in the codebase. The toolbar content is mounted via SwiftUI's standard view-tree composition, not as an accessory bound to the window's title bar. This means the toolbar lives in *content* coordinates, not title-bar coordinates, and AppKit doesn't know it's a title bar — which interacts badly with the next finding.

**`fullSizeContentView` + `titlebarAppearsTransparent` for full-custom layouts** — *PARTIAL.* `.windowStyle(.hiddenTitleBar)` (`NiceApp.swift:94`) implies `fullSizeContentView` and `titlebarAppearsTransparent` (`TrafficLightNudger.swift:78` even reads `styleMask.contains(.fullSizeContentView)` as the tell for `.hiddenTitleBar`, confirming this). So the SwiftUI shell does occupy title-bar pixels. What's missing is the recommended layering on top: an accessory view controller hosting the custom widgets so AppKit still treats the title strip as title-bar-shaped.

**Mark widget subviews `mouseDownCanMoveWindow = false`, spacers `= true`** — *N/A under current design.* Because `isMovable = false`, this hint is irrelevant — AppKit's title-bar tracker is gated off entirely. Under a best-practice implementation (see "Recommended direction") this finding would re-engage as VIOLATED: only the chrome view explicitly sets `false`; spacers (`Spacer`, padding, `Color` fills) get whatever SwiftUI's NSHostingView default is, with no explicit `true` annotation.

**Mirror toolbar actions in the menu bar (HIG)** — *N/A for this audit.* Out of scope (it's a HIG concern about the toolbar's content surface, not drag/title-bar mechanics).

**Don't replace traffic lights with custom buttons** — *FOLLOWED.* `TrafficLightNudger.swift` only repositions the standard `standardWindowButton(_:)` instances; it never replaces them. The 8/-8 nudge (`AppShellView.swift:190`) is purely cosmetic.

**Accessory views need explicit Auto Layout / proper sizing** — *N/A.* No accessory views are used.

## Anti-patterns present

- **Hand-rolled "fake toolbar" instead of `NSToolbar`** — `WindowToolbarView.swift:25-75`. Anti-pattern per the layout section. The fake toolbar is the *reason* the rest of this complexity exists: no native drag-region inference → must build `WindowDragRegion`; no native customization → must build `OverflowMenuButton`; no accessory-view contract → toolbar lives in content coordinates and SwiftUI hosting wrappers leak `mouseDownCanMoveWindow` defaults that have to be defended against. This anti-pattern is the upstream cause of nearly every other finding in this audit.

- **Setting `isMovable = false` to suppress unwanted drag inheritance** — `AppShellView.swift:205`. Not in the research doc's enumerated anti-patterns, but a logical extension of one: the research's anti-pattern is "rely on `mouseDownCanMoveWindow` alone for a custom title bar" because hit-test propagation breaks. The codebase's response — turn off the entire mechanism the title bar depends on — is the equally-bad inverse. A best-practice implementation suppresses unwanted drag inheritance at the *widget* level (`mouseDownCanMoveWindow = false` on the offending NSHostingView) while keeping `isMovable = true` so empty chrome remains draggable cooperatively *and* the explicit `performDrag` view still works on its own pixels.

- **Three-layer defence in depth** — `PaneDragSource.swift:65-102` (the comment enumerates all three layers). The research doc nowhere recommends layered redundancy of suppression mechanisms; the recommended approach is one disambiguation strategy done correctly. Layered defences make every future change risky (any one layer flipping introduces silent regressions; you can't tell which layer was actually load-bearing). The handoff doc itself notes "Adding Layer 1 closed the rest of the gap" — i.e. only Layer 1 was actually needed for the symptom, but Layers 2 + 3 were kept "because each catches a different failure mode and the cost of redundancy is trivial." From a title-bar perspective the cost is not trivial: Layer 1 (`isMovable = false`) is the cause of the user's regression.

- **No `acceptsFirstMouse(for:)` on title-bar widgets** — toolbar widgets (logo, pills, `+`, gear, etc.) lack the override. Anti-pattern per the research doc. Mild; only matters when window is inactive.

## Novel choices

- **Two `WindowDragRegion()` instances inside `collapsedCap`** (`AppShellView.swift:566` and `:573`) flanking the `SidebarToggleButton`. Architecturally fine (each is its own NSView and each correctly opts out via `mouseDownCanMoveWindow = false`), but novel relative to the research doc — the recommended pattern has *one* drag view per logical drag region. Multiple instances per region is a reasonable response to needing to gap around a button without reaching for an `NSTitlebarAccessoryViewController`, which would be the cleaner answer.

- **`WindowDragRegion()` placed in the sidebar's top 52pt** (`AppShellView.swift:406`) so the floating sidebar card behaves as a draggable title bar in its own right. Not covered by the research doc, which assumes title-bar pixels live above the content area, not flanking it. Functionally this is what salvages window-drag in the current build — the sidebar's drag strip is the only place in the window where empty chrome reliably routes to `ChromeDragView`, and it's why the user reports "we can still drag where the sidebar and the top bar overlap." Smart workaround, but it's a workaround for the bug, not a deliberate design.

- **Manually nudging traffic lights via `setFrameOrigin` and re-applying on `didBecomeKey`/`didResize` notifications** (`TrafficLightNudger.swift:74-129`). The research doc says you can fetch traffic lights via `standardWindowButton(_:)` (ref [12]) but doesn't bless re-positioning them. AppKit's repeated re-laying-out of these buttons on focus/resize (which the code explicitly defends against, `:88`) is exactly the warning sign that this is unsupported territory. Works, but brittle — and it's a symptom of not using `NSTitlebarAccessoryViewController.layoutAttribute = .leading`, which would let you reserve traffic-light space declaratively without touching the buttons themselves.

- **`NonDraggableHostingView.hitTest` claims `self` for every in-bounds point** (`PaneDragSource.swift:90-93`). Not covered by the research doc; the doc trusts AppKit's default hit-test. This is a pane-drag-side concern (it ensures the pan recogniser, not the window-drag tracker, owns drags from inside the pill). From the title-bar side: with `isMovable = false`, this override is dead code as far as window drag is concerned — the title-bar tracker isn't going to fire anywhere regardless. Pass to the pane-drag auditor for evaluation as a drag-suppression mechanism. From a *title-bar architectural* view: the right response wouldn't need this override at all, because pills would either (a) be `NSToolbarItem`s (which AppKit naturally treats as non-drag-handles) or (b) live in an accessory view whose subtree had `mouseDownCanMoveWindow = false` set explicitly on the hosting view.

- **`TitleBarZoomMonitor` was deleted** in this branch (handoff doc, "What changed" §). The previous global `NSEvent` monitor for double-click-to-zoom on visual-effect chrome is gone; zoom now only fires on `ChromeDragView` pixels (handoff doc, "What's broken" §4). Not specifically called out by the research doc but worth noting: with `isMovable = false`, neither the system title-bar tracker NOR the deleted monitor are observing double-clicks on, e.g., the sidebar's NSVisualEffectView background — so double-click-to-zoom shrank coincident with single-click-to-drag.

## Diagnosis of the "drag region too small" bug

User's report: "we can no longer drag the window via the top bar at all — only where the sidebar and top bar overlap."

Root cause: **`window.isMovable = false` at `AppShellView.swift:205`**, combined with the hand-rolled-toolbar layout choice.

Mechanism: with `isMovable = false`, AppKit's title-bar drag tracker is disabled window-wide. The ONLY surfaces that can drag the window are NSViews whose `mouseDown(with:)` explicitly calls `performDrag(with:)` — i.e. literal `ChromeDragView` instances, of which there are exactly three in the current layout:

1. `WindowDragRegion()` inside the toolbar's `.background { ZStack { Color.niceChrome; WindowDragRegion() } }` (`WindowToolbarView.swift:66`).
2. `WindowDragRegion()` reserving the 52pt strip at the top of the floating sidebar card (`AppShellView.swift:406`).
3. The two `WindowDragRegion()`s inside `collapsedCap` (`AppShellView.swift:566`, `:573`).

The toolbar instance (#1) is laid out *behind* every other child in the toolbar HStack — including the `Color.niceChrome` fill at the same ZStack layer (`WindowToolbarView.swift:60-67`). Because `Color` in SwiftUI renders as an opaque NSView wrapper that wins hit-testing, the `WindowDragRegion()` *behind* the chrome fill receives no hits in the empty regions. So only places that bypass the fill are draggable: namely, the sidebar's own 52pt drag strip (#2 — the only `WindowDragRegion` not buried under an opaque sibling), which is exactly the surface the user identified as "still works."

What a best-practice implementation would do instead:

- Keep `isMovable = true`. AppKit's title-bar tracker walks the hit chain and treats every NSView reporting `mouseDownCanMoveWindow == true` as drag-eligible — and SwiftUI's hosting wrappers default to `true`, so empty chrome regions are naturally drag-eligible without any explicit drag view at all.
- For interactive widgets that should NOT inherit drag, set `mouseDownCanMoveWindow = false` on the specific NSHostingView wrapping them — the same trick used in `NonDraggableHostingView`, applied to each toolbar control's host. AppKit's drag-region computation then skips those pixels and the drag remains anchored to actually-empty chrome.
- Keep `ChromeDragView` for the explicit/`performDrag` path on top of the chrome fill (so it isn't masked by the opaque background), as the research doc recommends, for the cases where the cooperative path can't decide (e.g. chrome strips inside `NSVisualEffectView` that consume `mouseDown` themselves).

Two layout fixes would also help independently of the `isMovable` decision:

- The toolbar `.background { ZStack { Color.niceChrome; WindowDragRegion() } }` should put `WindowDragRegion` *above* the `Color.niceChrome` fill, not behind it. As written, `WindowDragRegion` is masked everywhere the chrome fill paints (i.e. everywhere — it's a full-bleed `Color`). The comment at `WindowToolbarView.swift:62-65` claims the drag region "sits on top of the chrome fill but behind the toolbar's interactive children," but ZStack layering is bottom-to-top declaration order, so `WindowDragRegion()` declared *after* `Color.niceChrome` is on top — the comment matches the code, but the AppKit reality is that the SwiftUI view masking semantics treat the opaque `Color` as a hit-test winner, so functionally the drag region is occluded. (This becomes irrelevant once `isMovable = true` is restored, because the cooperative path doesn't need ChromeDragView to win the hit test — but it's a latent bug under any design.)
- The custom toolbar isn't an `NSTitlebarAccessoryViewController`, so AppKit doesn't know it's a title bar at all. With native containment, AppKit would carve out drag regions automatically based on toolbar-item positions.

## Recommended direction

A best-practice implementation, sketched (no code):

1. **Move the toolbar into a real `NSTitlebarAccessoryViewController`** with `layoutAttribute = .bottom`, hosting an `NSHostingView` that contains the existing `WindowToolbarView`. This makes the strip a first-class title-bar accessory; AppKit treats it as title-bar geometry, computes drag regions, hosts traffic lights without nudging, and handles inactive-window click-through coordination.

2. **Restore `window.isMovable = true`** (delete `AppShellView.swift:205`). AppKit's title-bar drag tracker is the right primary mechanism for "drag empty chrome to move the window." It already does the right thing for SwiftUI-hosted content as long as widgets explicitly opt out.

3. **Mark each interactive widget's hosting NSView with `mouseDownCanMoveWindow = false`** — pills, `+`, `>>`, gear, sidebar toggle, mode toggles, the `UpdateAvailablePill`, the close `×`. The `NonDraggableHostingView` pattern from `PaneDragSource.swift:87-102` is reusable for each (minus the hit-test override; see point 6).

4. **Keep `ChromeDragView` only as a fallback** for surfaces where the cooperative path is unreliable — specifically where `NSVisualEffectView` consumes `mouseDown` (the sidebar card's top 52pt is the obvious one). It should be layered *above* opaque siblings in the SwiftUI tree so it wins hit-testing where it sits.

5. **Disambiguate pill vs window drag inside `mouseDown`** (the research doc's recommended approach for layered drag sources) instead of via hit-test surgery: inside the pill's `mouseDown`, capture; in `mouseDragged`, decide based on movement direction/threshold whether to call `beginDraggingSession` (pill drag) or fall through to AppKit's title-bar tracker (window drag). This eliminates the need for the hit-test override and removes the "is this load-bearing?" puzzle from `NonDraggableHostingView`.

6. **Add `acceptsFirstMouse(for:)` returning `true`** on each interactive widget's hosting NSView so first-click on an inactive window both activates and invokes the control (per ref [10]).

7. **Stop manually nudging traffic lights.** Once the toolbar is an accessory view with `.leading` reservation, the sidebar card's 52pt header is no longer competing with the buttons for the same coordinate space — the system's default positioning is fine, or any reservation can be done declaratively via the accessory's preferred geometry.

Net effect: the entire `WindowDragRegion` + `NonDraggableHostingView` + `isMovable = false` + `TrafficLightNudger` constellation collapses to "use AppKit's containers as designed, with one fallback drag view for the visual-effect strip." The user's bug disappears because the toolbar's empty pixels are draggable by default.
