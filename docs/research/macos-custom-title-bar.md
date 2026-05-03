# macOS custom title bars — best practices

## TL;DR
- Prefer **`NSWindow.performDrag(with:)`** (called from a custom view's `mouseDown(_:)`) over the older **`NSView.mouseDownCanMoveWindow`** approach. This is what an Apple Frameworks Engineer recommends in forum thread 81149 [2].
- Use **`fullSizeContentView` + `titlebarAppearsTransparent`** to extend SwiftUI/AppKit content into the title-bar area, then carve out an explicit drag region (a custom `NSView` subclass) on top.
- For widget layout, prefer the framework-supplied containers — **`NSToolbar`/`NSToolbarItem`** and **`NSTitlebarAccessoryViewController`** — over reinventing layout, because they preserve standard click/drag/menu-bar mirroring behavior.
- Buttons and other interactive subviews inside a custom title bar do **not** auto-receive clicks on a non-key window. Override **`acceptsFirstMouse(for:)`** to return `true` on those subviews [10].
- **`isMovableByWindowBackground = true`** is the cheapest, dumbest way to make the whole window draggable, but it is not selective; it conflicts with embedded interactive controls and is gated by `isMovable`.

## Drag behaviour

### Recommended pattern
The modern, Apple-blessed pattern for a custom title bar with drag is:

1. Build a dedicated `NSView` subclass (`TitleBarDragView` or similar) that occupies the draggable strip.
2. Override `mouseDown(_:)` to call `self.window?.performDrag(with: event)` [1][2].
3. Leave `isMovable = true` (the default) on the window. Do **not** set `isMovableByWindowBackground = true` if other parts of the window contain non-trivial interactive content; you'll get unintentional window moves on stray background clicks [4][5].
4. For interactive widgets layered on top of the drag view (toolbar buttons, tab pills), let them be normal `NSControl`s. They naturally win the hit test and consume `mouseDown`, so they will not accidentally drag the window.
5. Override `acceptsFirstMouse(for:)` on those interactive widgets to return `true` so a click on an inactive window both activates **and** triggers the control in one click [10][11].

`performDrag(with:)` "explicitly start[s] a window drag, and it'll transfer the event tracking to the windowing system" — meaning AppKit takes over the mouse session and the originating view does not have to chase mouse-dragged events itself [2].

### APIs to know
- **`NSWindow.performDrag(with:)`** (10.11+) — explicit, imperative window drag. Call from `mouseDown`. The recommended modern API [1][2].
- **`NSView.mouseDownCanMoveWindow`** — declarative opt-in/opt-out. Returning `true` says "events that propagate through me may initiate a window drag." Returning `false` blocks drag on this view (commonly used on toolbar buttons inside a title-bar container) [3][8].
- **`NSWindow.isMovable`** — master switch. If `false`, the window can't be moved by the user at all (programmatic frame changes still work). Disabling `isMovable` also disables `isMovableByWindowBackground` [5][12].
- **`NSWindow.isMovableByWindowBackground`** — drag from any background hit. Useful for toolless utility windows; dangerous in apps with custom content because every empty pixel becomes a drag handle [4][5][9].
- **`NSView.acceptsFirstMouse(for:)`** — return `true` so an inactive-window click is delivered to the view (and handled), instead of being swallowed as a window-activation click [10][11].
- **SwiftUI `Scene.windowBackgroundDragBehavior(.enabled)`** (macOS 15+) — declarative equivalent of `isMovableByWindowBackground` [6][7].

### Anti-patterns
- **Relying on `mouseDownCanMoveWindow` alone for a custom title bar.** BJ Homer's canonical gist explains the failure mode: `mouseDownCanMoveWindow` only controls whether mouse events propagate up the hit-test chain. Any view in the way that handles `mouseDown` itself — including most `NSVisualEffectView`-backed materials and most SwiftUI content — silently breaks the drag, with no way for a view to assert "I want this event to start a window drag" [1].
- **Enabling `isMovableByWindowBackground` and *also* embedding clickable widgets in the same area.** The whole-window-drag heuristic fights the widget's `mouseDown` and produces accidental window moves on miss-clicks [4][5].
- **Implementing drag by tracking `mouseDragged` and calling `setFrameOrigin` manually.** This bypasses AppKit's window-drag pipeline (snap-to-edge, Mission Control, multi-display geometry) and feels off. Use `performDrag(with:)` instead [2].
- **Forgetting `acceptsFirstMouse(for:)` on title-bar buttons.** Result: the first click on an inactive window only activates the window; users have to click twice to invoke the button. The standard system title-bar buttons do this for you; custom ones do not [10][11].
- **Layering an `NSDraggingSource` (file/pill drag) view directly on top of the drag region without disambiguation.** Both want `mouseDown`. Conventional fix: in `mouseDown`, capture the event and decide in `mouseDragged` based on movement threshold/direction whether to call `beginDraggingSession(with:event:source:)` (data drag) or `performDrag(with:)` (window drag). Do not start both [13].

### Open questions / disagreements in sources
- **`mouseDownCanMoveWindow` — useful or obsolete?** Apple still documents it as the way to opt views in/out of drag-region inheritance, and Apple sample code uses it on container views [3]. Independent experts (BJ Homer, the engineer in thread 81149) recommend the explicit `performDrag(with:)` pattern instead [1][2]. **Apple's currently endorsed answer:** `performDrag(with:)` for any custom view that *originates* a drag; `mouseDownCanMoveWindow` returning `false` is still the right way to mark embedded controls as "not a drag handle" so AppKit's drag-region calculation skips them.
- **`windowBackgroundDragBehavior(.enabled)` vs. an explicit drag view.** Apple's SwiftUI modifier (macOS 15+) is the cleanest answer when there is genuinely no widget-bearing area. For apps with title-bar widgets, the explicit-drag-view pattern still wins because it's selective [6][7].

## Title-bar widget layout

### Recommended pattern
Use the system containers; don't lay widgets out by hand inside a hand-rolled title bar.

1. **`NSToolbar` with `windowToolbarStyle(.unified)` / `.unifiedCompact`.** This is the standard "buttons live in the title bar" container. AppKit handles drag (the toolbar background is a drag region by default), overflow into the `>>` menu, and customization sheets [14][15].
2. **`NSTitlebarAccessoryViewController`** for non-button accessories (tab strip, breadcrumbs, status indicator). Set its `view` (often an `NSHostingView` for SwiftUI), set `layoutAttribute` (`.leading`, `.trailing`, or `.bottom`), and add via `NSWindow.addTitlebarAccessoryViewController(_:)`. Apple's own example is the Safari Favorites bar [8][16].
3. For full custom layouts: enable `styleMask.insert(.fullSizeContentView)` and `titlebarAppearsTransparent = true`, then add an `NSTitlebarAccessoryViewController` whose hosted view contains your custom widgets [9][16].
4. Mark widget subviews with `mouseDownCanMoveWindow = false`; mark spacer/background subviews with `mouseDownCanMoveWindow = true`. AppKit's hit-test walk uses these flags to compute the draggable region [3].
5. Mirror every toolbar action in the menu bar — required by HIG because toolbars are user-customizable and may be hidden [17].

### APIs to know
- **`NSWindow.styleMask` flags:** `.fullSizeContentView` (content extends behind title bar), `.titled` (drop for fully custom chrome) [9][12].
- **`NSWindow.titlebarAppearsTransparent`**, **`NSWindow.titleVisibility`** — combine with `.fullSizeContentView` to reclaim title-bar pixels [9].
- **`NSToolbar` / `NSToolbarItem`** plus `NSToolbarDelegate` (`toolbarDefaultItemIdentifiers`, `toolbarAllowedItemIdentifiers`, `toolbar(_:itemForItemIdentifier:willBeInsertedIntoToolbar:)`) [14].
- **`NSTitlebarAccessoryViewController.layoutAttribute`** — `.leading`, `.trailing`, `.bottom` [16].
- **SwiftUI `.toolbar { }`** with `ToolbarItem(placement:)` and `.windowToolbarStyle(_:)` for the modern declarative path [6][14].
- **`NSWindow.standardWindowButton(_:)`** to fetch/hide traffic lights in fully custom chrome [9].

### Anti-patterns
- **Building a custom NSView "fake toolbar" that re-implements layout, overflow, and customization** instead of using `NSToolbar`. You lose customization sheets, overflow handling, menu-bar mirroring help, and unified-window-tab integration [14][17].
- **Putting interactive controls inside a title-bar accessory and forgetting to set `mouseDownCanMoveWindow = false` on them.** AppKit's hit-test walk may treat the accessory as one big drag region and your buttons start dragging the window [3][8].
- **Replacing the standard traffic lights with custom buttons.** HIG explicitly tells you not to — users expect them to behave (and look) like the system [17].
- **Not setting `automaticallyAdjustsSize` / proper layout on the accessory view controller** — accessory views need explicit Auto Layout constraints; otherwise they collapse to zero height under the title bar.

## Sources
1. [BJ Homer — "Why not -[NSView mouseDownCanMoveWindow]?" gist](https://gist.github.com/bjhomer/2a0035fa516dd8672fe7) — canonical critique of `mouseDownCanMoveWindow`; explains hit-test propagation failure.
2. [Apple Developer Forums thread 81149 — "Move NSWindow by dragging NSView"](https://developer.apple.com/forums/thread/81149) — Apple Frameworks Engineer recommends `performWindowDragWithEvent:` (Swift: `performDrag(with:)`).
3. [Apple — `NSView.mouseDownCanMoveWindow`](https://developer.apple.com/documentation/appkit/nsview/1483666-mousedowncanmovewindow) — official property reference; default behavior.
4. [Apple — `NSWindow.isMovableByWindowBackground`](https://developer.apple.com/documentation/appkit/nswindow/ismovablebywindowbackground) — official property reference.
5. [usagimaru — Key Points for Controlling NSWindow Movement and Event Handling](https://zenn.dev/usagimaru/articles/c2d6aa5bbab0a5?locale=en) — synthesis of `isMovable`, `isMovableByWindowBackground`, `mouseDownCanMoveWindow`, `acceptsFirstMouse` and their interactions.
6. [Apple — `Scene.windowBackgroundDragBehavior(_:)`](https://developer.apple.com/documentation/swiftui/scene/windowbackgrounddragbehavior(_:)) — SwiftUI modifier (macOS 15+).
7. [nilcoalescing — Customizing macOS window background in SwiftUI](https://nilcoalescing.com/blog/CustomizingMacOSWindowBackgroundInSwiftUI/) — practical SwiftUI pairing of `.hiddenTitleBar` + `windowBackgroundDragBehavior(.enabled)`.
8. [Apple — `NSTitlebarAccessoryViewController`](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller) — class reference for title-bar accessory views and `layoutAttribute`.
9. [Luka Kerr — NSWindow Styles showcase](https://lukakerr.github.io/swift/nswindow-styles) and [GitHub repo](https://github.com/lukakerr/NSWindowStyles) — pattern catalog: `fullSizeContentView`, `titlebarAppearsTransparent`, `isMovableByWindowBackground`.
10. [Christian Tietze — Enable SwiftUI Button Click-Through for Inactive Windows on macOS](https://christiantietze.de/posts/2024/04/enable-swiftui-button-click-through-inactive-windows/) — `acceptsFirstMouse(for:)` workaround for SwiftUI custom button styles; macOS 15 fixed natively.
11. [Apple — Handling Mouse Events (Cocoa archive)](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/HandlingMouseEvents/HandlingMouseEvents.html) — original explanation of `acceptsFirstMouse:` and inactive-window click-through.
12. [Apple — `NSWindow`](https://developer.apple.com/documentation/appkit/nswindow) — class reference for `isMovable`, `styleMask`, `standardWindowButton(_:)`.
13. [Apple — `NSDraggingSource`](https://developer.apple.com/documentation/appkit/nsdraggingsource) and [Dragging Sources guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/DragandDrop/Concepts/dragsource.html) — pattern for `beginDraggingSession(with:event:source:)` from `mouseDown`/`mouseDragged`.
14. [Hugh Jeremy — Adding a Unified `NSToolbar` to a SwiftUI window](https://dev.to/hugh_jeremy/adding-a-unified-nstoolbar-to-a-swiftui-window-4ppp) and [robin/TitlebarAndToolbar](https://github.com/robin/TitlebarAndToolbar) — pragmatic reference for `NSToolbar` + transparent title bar + `.unified` style.
15. [Gavin Wiggins — Window and toolbar style (Swift macOS)](https://gavinw.me/swift-macos/swiftui/window-toolbar-style.html) — SwiftUI `windowToolbarStyle` options (`.automatic`, `.expanded`, `.unified`, `.unifiedCompact`).
16. [Apple — `NSTitlebarAccessoryViewController.layoutAttribute`](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller/layoutattribute) — placement options for accessory views.
17. [Apple HIG — Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars) — mirror toolbar items in the menu bar; do not replace traffic lights.
