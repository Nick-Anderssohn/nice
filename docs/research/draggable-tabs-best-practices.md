# Draggable tabs in a top bar — best practices

## TL;DR
- Use `NSDraggingSession` (started via `NSView.beginDraggingSession(with:event:source:)`) for cross-window/tear-off; SwiftUI's `.draggable`/`.dropDestination` cannot move *live* views and is fine only for value/identifier payloads. [1][3][12]
- Detect tear-off in `draggingSession(_:endedAt:operation:)` when `operation == []` (i.e. `NSDragOperation.none`) — **no** destination accepted the drop. The `screenPoint` argument is where you spawn the new window. [2][4]
- For intra-strip reorder use a placeholder (gap) model and animate `frame`s; Chromium calls this "insert a placeholder for the given tab at the appropriate mouse location." [9]
- Migrate live state by reparenting the *same* `NSView` instance via `removeFromSuperview()` → `addSubview(_:positioned:relativeTo:)`. The pty/process is owned by your model, not the view, so it survives. Use `viewWillMove(toWindow:)` to react. [5][6]
- In a custom titlebar, **every** subview that should not start a window-drag must override `mouseDownCanMoveWindow` to return `false`. [7][8]

---

## Intra-strip reorder

### Recommended pattern
Maintain a tab-model array (source of truth). On drag, insert a *placeholder* (a transparent gap of the dragged tab's width) at the index nearest the cursor's x. Animate the surrounding tabs' frames into place; the dragged tab's image follows the cursor via the dragging session. Chromium's TabView "instructs the window's TabWindowController to insert a placeholder for the given tab at the appropriate mouse location," forcing layout to recompute. [9]

Hit-testing: convert the drag's screen point to the strip's local coords (`convert(_:from:nil)` on the strip view) and bisect against tab midpoints to compute the target index — do **not** use absolute positions of moving tabs (their frames are mid-animation).

### APIs to know
- `NSView.beginDraggingSession(with:event:source:)` — returns `NSDraggingSession`. [3]
- `NSDraggingItem` + `setDraggingFrame(_:contents:)` for the drag preview; or `imageComponentsProvider` for richer multi-layer previews. [10][11]
- `NSDraggingSource.draggingSession(_:movedTo:)` — fires on every cursor move; use it to update placeholder index. [1]
- For pure SwiftUI prototypes: `.draggable { Payload(id:…) } preview: { TabPill() }` plus `.dropDestination(for: Payload.self) { items, location in … }`. Sufficient for reorder *within* one window. [12]

### Anti-patterns
- Driving reorder off `NSEvent` `mouseDragged` only — you re-implement what `NSDraggingSession` gives you (drag image, autoscroll, system feedback) and lose cross-app drag-image continuity.
- Mutating the model during the drag instead of using a placeholder — produces visible "jump" frames whenever the cursor crosses a tab boundary.

---

## Cross-window drag

### Recommended pattern
The drag-source window publishes a custom pasteboard type (e.g. `"app.tab.handle"`) carrying a tab identifier (UUID), **not** the live view. Each tab strip is an `NSDraggingDestination`; on `draggingEntered(_:)` it shows an insertion indicator and returns `.move`. On `performDragOperation(_:)` it asks a shared registry for the live view by ID, reparents it, and returns `true`. This mirrors Chromium's design where "TabContents transfers to destination window's model." [9]

Use `NSDragOperation.move` (not `.copy`) so the source knows to remove the tab on success. Because the source and destination are in the same process, the pasteboard payload is just a key into an in-process dictionary; no real serialization is needed.

### APIs to know
- `NSDraggingDestination` methods: `draggingEntered(_:)`, `draggingUpdated(_:)`, `draggingExited(_:)`, `prepareForDragOperation(_:)`, `performDragOperation(_:)`, `concludeDragOperation(_:)`. [1]
- `NSView.registerForDraggedTypes([.init("app.tab.handle")])` on each strip.
- `NSPasteboard.PasteboardType` (custom UTI) on the dragged item.
- `NSDraggingSource.draggingSession(_:sourceOperationMaskFor:)` — return `.move` for `.outsideApplication` *and* `.withinApplication`. [1]

### Live-view migration (preserving pty/process state)
- The pty / WKWebView / AVPlayer must be owned by a *model object* (e.g. `TabSession`) that the **view** holds, not the other way round. Then reparenting is just view shuffling.
- Move the existing `NSView` instance: `oldStrip.removeFromSuperview()` → `newWindow.contentView!.addSubview(view)`. AppKit explicitly supports this: "View instances can be moved from window to window and installed as a subview first of one superview, then of another." [5]
- Override `viewWillMove(toWindow:)` on the content view if you need to refresh tracking areas, observers, or `NSView.window`-tied resources. **Do not** call `removeFromSuperview` then re-create the view — you'll tear down `NSWindow`-tied state inside (e.g. layer-backed contexts, IOSurface bindings). [6]
- For `WKWebView` specifically, reparenting the same instance preserves the web process; recreating one starts a new web process. Same shape for pty: keep the file descriptor and `Process` in the model layer.
- Chromium does the equivalent: a tear-off "creates a new Browser, complete with its own TabStripModel, containing the TabContents associated with the original tab, allowing it to maintain all its existing state and render processes (complete with animations!)" — the *contents* object is moved, not rebuilt. [9]

### Anti-patterns
- Encoding the live `NSView` itself onto the pasteboard (you can't, and shouldn't try via `NSCoding` — the pty fd doesn't survive).
- Re-instantiating the SwiftUI view tree on the destination — any `@State`, scroll position, `WKWebView` process, etc. is gone.
- Forgetting to call `NSWindow.makeFirstResponder(_:)` on the migrated view in the new window.

---

## Tear-off into new window

### Recommended pattern
In `draggingSession(_:endedAt:operation:)`, if `operation` is `[]` (no destination accepted), spawn a new `NSWindow`, position it so the tab strip sits under the cursor (subtract the tab's offset within the strip from `screenPoint`), and reparent the live view as in cross-window. Order it front and `makeKey()`. Chromium's same-process equivalent: "If the mouse leaves a certain boundary above or below the tab strip, the dragging code assumes the user wants to 'tear' the tab out into its own window." [9]

Two valid spawn timings:
1. **Lazy (recommended for SwiftUI/AppKit apps):** Wait for `endedAt:operation:`. Simpler, no overlay-window plumbing.
2. **Eager (Chromium model):** As soon as the cursor leaves the strip's tear distance (`kTearDistance = 36 px` in `CTTabStripDragController`), create a translucent "overlay window" that follows the cursor and re-targets on hover. Higher fidelity, much more code. [13]

### APIs to know
- `NSDraggingSource.draggingSession(_:endedAt:operation:)` — `screenPoint: NSPoint` is in screen coords (Y-up, bottom-left origin). [2]
- `NSDragOperation` — empty option set means "rejected/nowhere." [4]
- `NSWindow.setFrameOrigin(_:)` / `setFrame(_:display:)` to place the new window at the release point.
- `NSWindow.orderFrontRegardless()` + `makeKeyAndOrderFront(_:)`.

### Anti-patterns
- Tearing off based on `draggingSession(_:movedTo:)` checking distance from the strip — racy with the system's own drag handling, and you'll fight your own destination's `draggingExited`.
- Creating the new window *before* the user releases the mouse without the overlay-window technique — you end up with a window glued to the cursor, an unfinished drag session, and weird key-window state.
- Spawning at `NSEvent.mouseLocation` instead of the `endedAt:` `screenPoint` — they can disagree by a frame, producing a visible jump.

---

## Coexistence with window-drag

This is the conflict zone for any app whose tab strip lives in (or overlaps) the titlebar area. macOS's window manager intercepts `mouseDown` in titlebar regions for window movement *before* your view sees a drag.

### Required moves
1. Override `mouseDownCanMoveWindow` to return `false` on the tab pill view, the strip background, and any container that visually sits in the titlebar. The default for opaque titlebar-area views is `true`; you must explicitly opt out. [7][8]
2. If you use `NSWindow.titlebarAppearsTransparent = true` + `NSWindow.styleMask` with `.fullSizeContentView`, your custom strip *is* in the titlebar's hit area — every interactive subview needs the override.
3. Implement an explicit "drag empty strip background → move window" gesture using `NSWindow.performDrag(with:)` on `mouseDown` in the gap area (since OS X 10.11). Don't rely on the default titlebar behavior in that region; it's now your strip.
4. Threshold the tab-drag start (Chromium uses ~3 px, iTerm2 defaults to 10 px) so a tiny twitch on click doesn't start a drag. Compare cursor delta from `mouseDown` location before calling `beginDraggingSession`. [14]

### Anti-patterns
- Setting `mouseDownCanMoveWindow = false` on the strip but forgetting child views — child views inherit `true`, the system reads *those*, and the window starts moving when the user clicks a tab.
- Calling `NSWindow.performDrag(with:)` and `beginDraggingSession(with:event:source:)` from the same `mouseDown` — the two event loops compete and one silently loses.
- Using SwiftUI's `WindowGroup` + `.draggable` on a tab and expecting it to move the window when the drag misses; SwiftUI window-drag is implicit and not composable with `.draggable`.

---

## Sources
1. [NSDraggingSource | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsdraggingsource) — protocol surface for sources, including `endedAt:operation:` and `movedTo:`.
2. [draggingSession(\_:endedAt:operation:) | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsdraggingsource/draggingsession(_:endedat:operation:)) — `screenPoint` and the meaning of `NSDragOperation.none`.
3. [NSDraggingSession | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsdraggingsession) — entry point and lifecycle.
4. [Dragging Sources (Apple Archive)](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/DragandDrop/Concepts/dragsource.html) — semantics of `NSDragOperationNone` as "drop failed/rejected."
5. [Working with the View Hierarchy (Apple Archive)](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/CocoaViewsGuide/WorkingWithAViewHierarchy/WorkingWithAViewHierarchy.html) — explicit support for moving NSViews between windows.
6. [viewWillMove(toWindow:) | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsview/1483415-viewwillmove) — lifecycle hook for window changes; central to safe reparenting.
7. [mouseDownCanMoveWindow | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsview/mousedowncanmovewindow) — default and override semantics.
8. [Why not -[NSView mouseDownCanMoveWindow]? (Brent Simmons-adjacent gist)](https://gist.github.com/bjhomer/2a0035fa516dd8672fe7) — practical notes on why every titlebar subview needs the override.
9. [Tab Strip Design (Mac) — Chromium](https://www.chromium.org/developers/design-documents/tab-strip-mac/) — the canonical public design doc: placeholder reorder, tear-off threshold, cross-window TabContents transfer, application-agnostic split.
10. [NSDraggingItem | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsdraggingitem) — drag-image construction.
11. [setDraggingFrame(\_:contents:) | Apple Developer Documentation](https://developer.apple.com/documentation/appkit/nsdraggingitem/1528746-setdraggingframe) — quick path for a single-image drag preview.
12. [Adopting drag and drop using SwiftUI | Apple Developer Documentation](https://developer.apple.com/documentation/SwiftUI/Adopting-drag-and-drop-using-SwiftUI) — SwiftUI's `.draggable`/`.dropDestination` with `Transferable`.
13. [chromium-tabs / CTTabStripDragController.m (rsms mirror)](https://github.com/rsms/chromium-tabs/blob/master/src/Tab%20Strip/CTTabStripDragController.m) — a publicly readable reimplementation showing `kTearDistance = 36`, overlay window, `detachTabToNewWindow:`, `moveTabView:fromController:`.
14. [iTerm2 — iTermAdvancedSettingsModel (minimum tab drag distance)](https://github.com/gnachman/iTerm2/blob/master/sources/iTermAdvancedSettingsModel.m) — the default 10 px drag threshold used by a real shipping app.
15. [PSMTabBarControl (jcouture fork)](https://github.com/jcouture/PSMTabBarControl) — long-running open-source tab bar with cross-window drag built in; the `allowsBackgroundTabClosing` / cross-window properties are a useful API surface to compare against.
16. [SwiftUI on macOS: Drag and drop, and more — Eclectic Light](https://eclecticlight.co/2024/05/21/swiftui-on-macos-drag-and-drop-and-more/) — concrete limitations of SwiftUI drag-and-drop on macOS that justify falling back to AppKit for tear-off.
