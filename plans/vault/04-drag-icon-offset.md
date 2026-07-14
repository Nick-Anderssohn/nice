# 04 — File-explorer drag preview offset to the left

## Bug

When dragging a file from the file-explorer sidebar, the floating drag
preview (the little "N items" chip that should ride next to the mouse
pointer) is drawn "way off to the left" of the pointer instead of at it.
The horizontal error grows the further right within the row you grab the
file (e.g. grabbing on the filename lands the chip much further left than
grabbing on the icon).

## Root cause (file:line evidence)

GPUI positions an active drag preview at `mouse_position - cursor_offset`,
where `cursor_offset` is the pointer's position *within the dragged source
element*, captured at drag-arm time.

- `vendor/zed/crates/gpui/src/elements/div.rs:2690` — when the drag arms:
  `let cursor_offset = event.position - hitbox.origin;` (hitbox = the whole
  dragged row element). This offset is stored on the `AnyDrag`
  (`div.rs:2692-2696`).
- `vendor/zed/crates/gpui/src/window.rs:2834` — each frame the preview is
  laid out as a root at
  `let offset = self.mouse_position() - active_drag.cursor_offset;`
  i.e. the preview's **top-left** is placed so that the same point you
  grabbed inside the source row sits under the cursor.

That layout rule assumes the preview element visually mimics the source
element (same width/anchor), so "same relative grab point under cursor"
looks right. Zed relies on exactly that and *compensates* inside its
preview by re-adding the grab offset as padding:

- `vendor/zed/crates/project_panel/src/project_panel.rs:5797-5808` — the
  `on_drag` closure keeps `click_offset` (the `cursor_offset` param).
- `vendor/zed/crates/project_panel/src/project_panel.rs:7393-7396` —
  `DraggedProjectEntryView::render` pads the visible chip by
  `.pl(self.click_offset.x + px(12.))` / `.pt(self.click_offset.y + px(12.))`.
  Net paint position = `(mouse - cursor_offset) + cursor_offset + 12` =
  `mouse + 12`, so the chip sits just below-right of the pointer (Finder-like).

Nice's file-browser drag does **not** compensate. It throws the offset
away and renders a small chip with no padding:

- `crates/nice/src/file_browser/view.rs:2111-2117` — `on_drag` builds
  `DragPreview { count }`, closure signature
  `move |paths: &ExternalPaths, _offset, _window, app|` — the
  `cursor_offset` (`_offset`) is discarded.
- `crates/nice/src/file_browser/view.rs:2244-2267` — `DragPreview::render`
  is a bare `px(8)/py(3)` chip with no leading padding.

Because the sidebar row is full width and the chip is tiny, the chip's
top-left is painted at `mouse.x - cursor_offset.x`. `cursor_offset.x` is
however far into the row you grabbed (icon + indentation + filename can be
100–200px), so the chip lands that many pixels to the left of the pointer.
That is the reported "way off to the left."

## Fix design

Mirror zed's compensation: capture the offset in the `on_drag` closure,
store it on `DragPreview`, and re-add it as leading padding in `render` so
the visible chip nets to a small fixed lead from the pointer.

Target anchoring: chip top-left ≈ `pointer + (12, 12)` (a few px down-right
of the cursor, matching Finder / zed's `px(12.)`), independent of where in
the row the drag began.

1. `crates/nice/src/file_browser/view.rs:2111-2117` — change the closure to
   bind the offset and pass it through:
   ```rust
   el = el.on_drag(
       ExternalPaths(drag_paths.iter().map(PathBuf::from).collect()),
       move |paths: &ExternalPaths, offset, _window, app| {
           let count = paths.paths().len();
           app.new(|_| DragPreview { count, offset })
       },
   );
   ```
   (`offset` is `gpui::Point<gpui::Pixels>` — the `on_drag` constructor's
   second param, `div.rs:578`.)

2. `crates/nice/src/file_browser/view.rs:2244-2246` — add the field:
   ```rust
   struct DragPreview {
       count: usize,
       offset: gpui::Point<gpui::Pixels>,
   }
   ```

3. `crates/nice/src/file_browser/view.rs:2256-2265` — pad the chip by the
   offset plus a small constant so it nets to pointer+const. Because GPUI
   subtracts `offset` before layout, wrap the chip in an outer container
   that carries the padding (padding on the chip itself would also inflate
   the chip's background box). Add e.g.:
   ```rust
   div()
       .pl(self.offset.x + px(12.0))
       .pt(self.offset.y + px(12.0))
       .child( /* existing px(8)/py(3) chip div */ )
   ```
   Keep the existing chip styling as the inner child.

Constant: use `px(12.0)` to match zed. Optional refinement — since the chip
has no pointer "hotspot" of its own, a smaller lead like `px(8.0)` is also
fine; 12 is the safe, proven default.

## Files touched

- `crates/nice/src/file_browser/view.rs` — only:
  - the `on_drag` closure at ~2111-2117 (bind `offset`, pass to `DragPreview`),
  - `struct DragPreview` at ~2244 (add `offset` field),
  - `impl Render for DragPreview` at ~2248-2267 (wrap chip in an offset-padded
    outer div).

No other files. No GPUI/vendor changes (the vendored layout rule is correct
and shared; the fix is purely in Nice's preview element, exactly as zed does
it in its own panels).

## Tests

Manual-only. GPUI drag-preview positioning is painted by the windowing layer
against a live `mouse_position`; there is no headless assertion path for the
floating preview's screen coordinates in this codebase (the existing
file-browser drag tests cover payload/`can_drop`/`on_drop` logic, not preview
geometry). Do not add a brittle unit test asserting pixel math against
private GPUI internals.

Manual validation (Nice Dev, scratch env per CLAUDE.md — do not touch prod):

1. Open a project in the file-explorer sidebar with a moderately deep tree so
   rows are visibly wide (long filenames / indentation).
2. Press-and-drag a file grabbing it **on the far right of the row** (on the
   filename text, not the icon). Before fix: the "1 item" chip floats far to
   the left of the pointer. After fix: the chip rides just below-right of the
   pointer (~12px), same as grabbing on the icon.
3. Repeat grabbing on the icon / left edge — chip position relative to the
   pointer should be the *same* as step 2 (offset no longer depends on grab
   point).
4. Multi-select several files, drag — the "N items" chip tracks the pointer
   identically.
5. Drag over a valid directory (accent hover highlight) and drop — the drop
   still lands (verifies the offset change didn't disturb `on_drop`).

## Risks & interactions

- **Scope is isolated.** `DragPreview` is used only by the file browser
  (`view.rs`). The change is additive (one field + padding wrapper) and does
  not touch `on_drop` / `can_drop` / `drag_over` payload handling, so
  drag-and-drop *behavior* is unaffected — only where the ghost paints.
- **Other drag sources share the same GPUI rule but NOT this preview type,
  so they are not changed by this fix — and each has the same latent bug:**
  - Toolbar pane pills — `crates/nice/src/toolbar.rs:1392` builds
    `PaneDragGhost`, closure also discards `_offset`
    (`toolbar.rs:1392-1394`; ghost struct/render separate). The inline
    comment at `toolbar.rs:1387-1390` incorrectly claims "gpui positions it,
    so it ignores the constructor's Point offset" — per `window.rs:2834`
    gpui subtracts the offset, so pills have the same leftward error; it is
    just smaller because a pill is narrow (small `cursor_offset.x`).
  - Sidebar tab rows — `crates/nice/src/sidebar_shell.rs:2063` builds
    `TabRowDragGhost`, likewise ignores `_offset`. Tab rows are wide, so
    this ghost can drift noticeably too.
  Fixing those is optional and out of scope for this specific bug report
  (file explorer only). If desired, apply the identical
  capture-offset-then-pad pattern to `PaneDragGhost` and `TabRowDragGhost`
  and correct/remove the misleading toolbar comment. Recommend tracking as a
  follow-up rather than expanding this fix, to keep the change surface tight
  and match the reported symptom.
- **No regression to drag arming / threshold.** The `on_drag` value payload
  (`ExternalPaths`) and the mouse-down select handler
  (`view.rs:2152-2168`) are untouched.
