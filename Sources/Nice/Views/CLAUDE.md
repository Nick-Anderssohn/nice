# Sources/Nice/Views — load-bearing invariants

## ⚠️ Window-drag vs. pill-drag selectivity (DO NOT BREAK)

`WindowToolbarView.swift` owns a fragile, **behavioral** invariant that the
type system and unit tests cannot catch — only UITests do. It has been
broken and re-fixed more than once. Read this before touching pane-pill
drag, the toolbar's window-drag gesture, or anything in `InlinePanePill`.

### The invariant

- **Dragging a pane pill must reorder/move the pill — it must NOT move the
  window.**
- **Dragging empty toolbar chrome (not on a pill) must move the window.**

### How it works (and why it's delicate)

- The window sets `window.isMovable = false` (in `AppShellView`), which
  kills AppKit's native title-bar drag. Empty-chrome window dragging is
  then restored by a SwiftUI `DragGesture` → `window.performDrag(with:)`,
  attached as a **plain** `.gesture(windowDragGesture)` on the toolbar
  HStack.
- A plain `.gesture` yields to any **higher-priority child gesture**. The
  pill's SwiftUI **`.onDrag`** is that higher-priority gesture: a press-drag
  that starts on a pill is claimed by `.onDrag`, so `windowDragGesture`
  never fires there. A press on empty chrome has no such child gesture, so
  the window moves.
- **This only works because the pill uses SwiftUI `.onDrag`.** An AppKit
  `NSDraggingSource` driven from a background `NSView` whose `hitTest`
  returns `nil` (with `NSEvent` monitors) does **NOT** participate in
  SwiftUI's gesture-priority arbitration. Swapping `.onDrag` for that
  silently re-introduces "dragging a pill moves the window" — the code
  compiles and unit tests pass while the behavior is wrong.

### Rules for changing the pill drag mechanism

1. Prefer keeping the pill's `.onDrag`. The cross-window-move wiring lives
   **inside** that `.onDrag` (it stamps `sourceWindowSessionId` and
   publishes a `LivePaneRegistry.Handle`) and is additive — it preserves
   the gesture claim.
2. If you MUST own the drag at the AppKit layer (e.g. to get an
   `NSDraggingSource` drag-END callback for desktop **tear-off**), you must
   **also re-solve the yield**: gate `windowDragGesture` so it never calls
   `performDrag` for a press that began over a pill (the AppKit source
   knows the `mouseDown` landed on a pill — surface that and check it in
   the gesture). Removing `.onDrag` without this is the exact regression.
3. **Any** change to the pill drag mechanism or `windowDragGesture` MUST
   keep these UITests green — run them before considering the change done:
   - `scripts/test.sh -only-testing:NiceUITests/WindowDragUITests`
     (`testEmptyToolbarDragMovesWindow`, `testEmptyToolbarDoubleClickZoomsWindow`)
   - `scripts/test.sh -only-testing:NiceUITests/PaneReorderUITests`
     (esp. `testDragOnPillDoesNotMoveWindow`,
     `testDragOnPillReordersAndDoesNotMoveWindow`)
4. This is **gesture-critical** work: do it in the main loop on the most
   capable model, not a delegated cheaper-model subagent. It compiles and
   unit-passes while behaviorally wrong, so the UITest gate above is the
   only real safety net.

If you're an automated agent and you find yourself about to delete the
pill's `.onDrag` or add a background drag-source NSView here, STOP and
re-read rule 2.
