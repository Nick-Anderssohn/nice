# Scrolled drag-selection grows downward while output streams

## Bug

While the terminal is scrolled up in scrollback **and** the process is still
producing output, starting a drag-selection to highlight a line produces a
selection that extends *downward*, below the line the user actually clicked.
Each additional line the process prints while the mouse button is held adds
another highlighted row beneath the intended one.

Verbatim: "if text is being output by the terminal, and you are scrolled up and
try to highlight a line, the highlight ends up extending down below the line
that you tried to highlight with your mouse."

## Root cause

The drag **anchor** is captured once, at mouse-down, as an alacritty grid-line
coordinate (`Line`) that is only valid for the `display_offset` in effect at
that instant. That coordinate is *not* stable as new output streams in, so the
anchor silently drifts downward relative to the content the user clicked.

Evidence:

- `crates/nice-term-view/src/view.rs:1050` — the hit-test converts a viewport
  row to a grid line with the *current* display offset:
  `buffer_line: vrow as i32 - display_offset as i32`. This is alacritty's grid
  coordinate (viewport row `vr` shows `Line(vr - display_offset)`; confirmed by
  the renderer at `crates/nice-term-view/src/element.rs:1554-1555`,
  `let line = Line(vr as i32 - display_offset);`).
- `crates/nice-term-view/src/view.rs:1122` — mouse-down freezes that value into
  the drag state: `self.drag_anchor = Some((hit.buffer_line, hit.col, kind));`
  (field declared `drag_anchor: Option<(i32, usize, SelectionType)>` at
  `view.rs:199`).
- `crates/nice-term-view/src/view.rs:1149-1155` — every mouse-move rebuilds the
  whole selection from that **frozen** anchor line to a **freshly** hit-tested
  end point: `set_selection_typed(kind, (anchor_line, anchor_col), (hit.buffer_line, hit.col))`.
  `set_selection_typed` (`crates/nice-term-view/src/session_handle.rs:532-547`)
  replaces `term.selection` wholesale each call, so it discards any prior state.

Why the grid-line coordinate is not content-stable: when the process prints a
line while the viewport is scrolled up, alacritty's `Grid::scroll_up` bumps the
display offset to keep the same content parked in view
(`alacritty_terminal-0.26.0/src/grid/mod.rs:267-269`,
`self.display_offset = min(self.display_offset + positions, self.max_scroll_limit)`),
and rotates the ring buffer so existing content moves one step further into
history — i.e. the content's grid `Line` *decreases by one per printed line*.
alacritty compensates its **own** committed selection for exactly this by
rotating it on every scroll
(`alacritty_terminal-0.26.0/src/term/mod.rs:778`,
`self.selection = self.selection.take().and_then(|s| s.rotate(self, &region, lines as i32));`).
Nice's `drag_anchor`, however, is a plain tuple the view holds outside the
`Term`; nothing rotates it.

Concrete walk-through (click viewport row 5 at `display_offset = 10`):

- Anchor captured as `Line(5 - 10) = Line(-5)`.
- The clicked content C is at `Line(-5)`, rendered at row `-5 + 10 = 5`. Correct.
- One line streams in → `display_offset` becomes 11; content C rotates to
  `Line(-6)`, still rendered at row `-6 + 11 = 5` (parked, as intended). The
  **end** point, re-hit-tested live, resolves to `Line(-6)` — correctly tracking
  C.
- The **anchor** is still the frozen `Line(-5)`, which now denotes the line
  *below* C and renders at row `-5 + 11 = 6`.
- The rebuilt selection therefore spans `Line(-6)`..`Line(-5)` — the clicked
  line **plus one row below**. Every further streamed line grows the offset,
  pushing the frozen anchor one more row down: after N streamed lines the
  anchor renders at row `5 + N`, so the highlight extends N rows below the
  target. This is the reported symptom exactly.

Note the end point is *not* the culprit — it is re-derived every move against
the live offset and stays glued to the clicked content. Only the frozen anchor
drifts.

## Fix design

Make the anchor content-stable by storing it in a form that survives the offset
changes caused by streaming output, then re-deriving its grid line against the
**current** display offset on every move — the same way the end point is already
derived.

Chosen approach (minimal, preserves the existing full-cell `selection_sides`
behavior): **store the anchor's click-time viewport row instead of its grid
line.** While the viewport is parked on the clicked content (the streaming
case), that content stays at the same viewport row, so `anchor_line =
anchor_vrow - display_offset_now` re-derives a grid line that tracks the content
across every streamed line — identical algebra to the live end point.

Changes:

1. `crates/nice-term-view/src/view.rs:199` — change the drag state to hold the
   **viewport row** rather than the grid line:
   `drag_anchor: Option<(usize /* anchor_vrow */, usize /* col */, SelectionType)>`.
   (The `i32` line becomes a `usize` row.)

2. `on_mouse_down` (`view.rs:1115-1132`):
   - Store `self.drag_anchor = Some((hit.vrow, hit.col, kind));` instead of
     `hit.buffer_line`.
   - The immediate double/triple-click set (the `else` branch,
     `view.rs:1126-1128`) keeps using `hit.buffer_line` for the anchor point it
     passes to `set_selection_typed`, because at mouse-down
     `display_offset_now == display_offset_at_click`, so `hit.buffer_line ==
     hit.vrow - display_offset`. No behavior change for the initial word/line
     selection.

3. `on_mouse_move` (`view.rs:1142-1156`):
   - Destructure the new tuple: `let Some((anchor_vrow, anchor_col, kind)) = ...`.
   - After a successful `hit_cell`, re-derive the anchor grid line against the
     **current** display offset and pass that as the selection start:
     ```
     let display_offset = hit.vrow as i32 - hit.buffer_line; // == current display_offset
     let anchor_line = anchor_vrow as i32 - display_offset;
     self.handle.read(cx).set_selection_typed(
         kind,
         (anchor_line, anchor_col),
         (hit.buffer_line, hit.col),
     );
     ```
     Deriving `display_offset` from the end hit (`vrow - buffer_line`) avoids a
     second `Term` lock. (Equivalently, read
     `self.handle.read(cx).display_offset()` — `session_handle.rs:601` — if
     clearer; both are fine.)

Everything else (the `selection_sides` full-cell inclusion at
`session_handle.rs:624`, `set_selection_typed`, copy, the mouse-up teardown at
`view.rs:1184-1186,1210`) is unchanged. The end point and rebuild-both-endpoints
strategy stay exactly as they are, so the leftward-drag fix (BUGS.md #11 /
commit `0ae0744`) is preserved.

### Why not switch to alacritty's `update`+`rotate` model instead

The "idiomatic" alternative is to build the `Selection` once at mouse-down, push
it into `term.selection`, and call `Selection::update(end)` on each move — then
alacritty's per-scroll `rotate` keeps the anchor glued to content automatically
(and also handles the `max_scroll_limit` clamp and mid-drag wheel scrolling that
the viewport-row approach approximates). It is rejected here because
`Selection`'s anchor side is frozen at creation and its `region` is private, so
Nice could no longer recompute *both* endpoint sides per move — which is exactly
how `selection_sides` guarantees both the anchor and dragged-to cells are
included on a leftward drag (`session_handle.rs:616-629`). Adopting `update`
would either regress that fixed bug or require threading the sub-cell pixel side
through the hit-test (`cell_from_offset` currently discards sub-cell x). That is
a larger change than the reported bug warrants. The viewport-row fix resolves
the streaming case fully; note it only diverges from true content-tracking in
two rare regimes (scrollback saturated at `max_scroll_limit` during the drag, or
the user spinning the wheel *during* a button-held drag), where the current code
is already wrong too.

## Files touched

- `crates/nice-term-view/src/view.rs` — `drag_anchor` field type
  (line 199), `on_mouse_down` anchor capture (~line 1122), `on_mouse_move`
  anchor re-derivation (~lines 1142-1155). `drag_anchor: None` initializer at
  line 324 and the mouse-up/out teardown at lines 1146/1186/1210 are unaffected
  (they only reset to `None`).

No changes to `session_handle.rs`, `mouse.rs`, `element.rs`, or `nice-term-core`.

## Tests

The touched handlers (`on_mouse_down` / `on_mouse_move`) need a live gpui window
(`paint_bounds`, real `MouseMoveEvent`s), so they are not directly unit-testable
without the GUI harness. Split the coverage:

1. **Unit test the coordinate math (the actual fix), against a real
   `Term`.** Add to the `session_handle.rs` test module (which already builds a
   `Term` via `TermSize`/`VoidListener` and resolves selections — see
   `session_handle.rs:1313-1333`). The test reproduces the streaming-scroll
   sequence end-to-end:
   - Build an 80xN `Term`, print enough lines to create scrollback, then
     `scroll_display(Scroll::Delta(k))` to scroll up so `display_offset == D0`.
   - Pick an anchor viewport row `vr`. Compute the **buggy** frozen grid line
     `vr - D0` and the **fixed** re-derived line `vr - D_now`.
   - Feed one more line of output (drives `Term::scroll_up_relative`, bumping
     the offset to `D_now = D0 + 1`).
   - Assert that a `set_selection_typed` built with the frozen anchor spans two
     rows (anchor + one below) — capturing the regression — while the anchor
     re-derived from `vr` and `D_now` spans exactly the intended single row.
     Resolve via `Selection::to_range(&term)` / `selection_to_string`, mirroring
     the existing `resolved_range` helper.
   - Optionally factor the one-line re-derivation into a tiny pure helper
     (`fn anchor_line(anchor_vrow: usize, display_offset: i32) -> i32`) and
     assert it directly; keep it colocated with the handler or in `mouse.rs`
     alongside `cell_from_offset` so it is unit-testable and self-documenting.

2. **Manual repro / validation** (for the end-to-end GUI path). Launch the
   installed `Nice Dev` bundle under a scratch env per CLAUDE.md, then:
   - In a shell, start a slow continuous printer, e.g.
     `while true; do date; sleep 0.2; done`.
   - Scroll up several pages into the backlog with the trackpad/wheel while it
     keeps printing.
   - Press-drag across a single visible line to highlight just that line and
     hold the button for a couple of seconds while output continues.
   - **Before fix:** the highlight grows downward, one extra row per printed
     line. **After fix:** the highlight stays on the line under the pointer;
     streaming output does not extend it.
   - Regression checks with output *idle*: leftward drag still includes both
     endpoint cells (BUGS.md #11); double-click word and triple-click line
     selection unchanged; parked-at-bottom drag-select unchanged; ⌘C copies the
     highlighted text.

## Risks & interactions

- **Shared selection/scroll/grid surface.** The fix touches drag-selection input
  in `view.rs` only; it does **not** touch the renderer's selection painting
  (`element.rs`), the cursor rendering, `set_selection_typed`/`selection_sides`
  (`session_handle.rs`), or the scroll path. Cursor rendering and selection
  rendering both read `renderable_content()` independently and are unaffected.
- **Preserves the leftward-drag fix.** Because we keep rebuilding both endpoints
  with `selection_sides` each move (rather than switching to alacritty's
  `update`), the BUGS.md #11 / `0ae0744` behavior and its tests
  (`session_handle.rs` `leftward_selection_includes_both_endpoints`) are
  untouched.
- **In-flight collisions.** Recent bug-round commits touched persistence,
  shortcut recorder, and quit/close lifecycle — none touch `view.rs` mouse
  handling or selection, so no expected conflict. Anyone else editing
  `on_mouse_down`/`on_mouse_move` or the `drag_anchor` field would collide; this
  is a small, localized diff (one field type + two handler edits).
- **Known residual (accepted, not a regression):** the viewport-row anchor only
  perfectly tracks content while the viewport stays parked. It diverges from
  true content-tracking if the scrollback saturates `max_scroll_limit` mid-drag
  (content actually falls off history — alacritty would drop the selection
  anyway) or if the user scrolls the wheel *during* a held drag. Both are rare
  and the pre-fix code is already wrong there; call it out if broader correctness
  is later required (then adopt the `update`+`rotate` model with proper sub-cell
  sides).
