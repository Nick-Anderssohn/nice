# Selection anchor: content-locked drag across scroll and streaming

## The bug

Drag-select, then wheel-scroll up to extend into scrollback. The highlight stays a
fixed size at a fixed screen position and the text slides underneath it. Expected:
the selection extends from the originally-clicked text to whatever is under the
pointer now.

Root cause: `TerminalView::drag_anchor` (`crates/nice-term-view/src/view.rs`)
stores the anchor as a **viewport row** and re-derives its grid line against the
*current* `display_offset` on every mouse-move (`view.rs:1418-1419`). A viewport
row is a screen coordinate. Re-deriving it each move is a no-op transform — it
pins the anchor to the screen row, not the content. That was deliberate
(commit `63b6080`) to fix a different bug: while parked in scrollback with output
streaming, a frozen grid-line anchor drifted one row per printed line. The
viewport-row scheme fixes streaming (content is parked on screen, so screen row =
content) and breaks user scroll (content moves on screen, screen row ≠ content).

Both cases must work. The streaming fix must not regress.

## What other terminals do (researched 2026-08-03)

Four codebases inspected (alacritty master, wezterm, ghostty, kitty — clones +
issue trackers). All four converge on one invariant:

- **Anchor: content-locked.** Stored in a coordinate space that names content,
  not screen position. Never re-derived after mouse-down.
- **Drag end: screen-locked.** Re-resolved from the pointer position against the
  *current* scroll offset on every mouse-move **and every scroll event**.

Per-terminal mechanism for keeping the anchor content-locked:

| Terminal | Mechanism |
|---|---|
| alacritty | Selection lives inside `Term` in signed grid `Line` coords (0 = top of active area, negative = scrollback). `Term::scroll_up_relative` calls `Selection::rotate` in lockstep with every content rotation. Display scroll touches only `display_offset`, never the selection. |
| wezterm | `StableRowIndex` — a monotonic line id offset by a purge counter. Selection and viewport both stored as stable rows; nothing ever rebases. |
| ghostty | Tracked `Pin`s (page pointer + row) fixed up by every grid mutation. The viewport is itself a pin. |
| kitty | Viewport row + per-endpoint `scrolled_by` snapshot; `index_selection()` rebases every endpoint on every line rotation. |

Scroll-mid-drag extension (the missing half of our bug) is an **explicit call in
the scroll handler**, not a side effect:

- alacritty `event.rs` `scroll()`: with a button held, re-convert the stored mouse
  *pixel* position against the new `display_offset` and `selection.update()`.
  Their issue #1598, fixed in 0.2.2. Not gated on "cell changed" — the pixel
  hasn't moved, only the offset has.
- kitty `mouse.c` wheel handler: `screen_history_scroll(...)` then
  `update_drag(w)`. Their issue #7453 (repro: run `yes`, hold mouse1,
  wheel-scroll without moving the mouse — identical to ours), fixed in 0.35.0.
- wezterm and ghostty *lack* this call; their end only catches up on the next
  mouse-move. Anchor stays correct either way. We take the alacritty/kitty
  behavior (extend live on scroll).

Battle-tested gotchas worth pinning:

- **kitty `a13f815591`:** rebase must not skip zero-length selections — a
  just-pressed anchor with no drag yet is zero-length and must still track
  content. (alacritty's `Term` rotates `Option<Selection>` unconditionally, so
  Term-owned selections get this for free — but pin it with a test.)
- **wezterm:** at scrollback capacity, `display_offset` saturates and the
  viewport itself drifts while the selection correctly tracks its text — tests
  must assert on content coordinates, never viewport rows.
- **alacritty:** clamp at read time (`to_range` + `Boundary::Grid`), not at
  rotate time — a temporarily out-of-history anchor renders as clamped/empty
  instead of corrupting.
- **All four:** faux-scrolling apps (`less`, `man` redraw instead of rotating
  the grid) are deliberately unsolved everywhere. Out of scope.

## The fix: let the Term own the anchor

We already depend on vanilla `alacritty_terminal 0.26`, and `term.selection` is
alacritty's own `Selection`. The library already does the hard part:

- `Term::scroll_up/scroll_down` rotate the stored selection with the grid
  (`term/mod.rs:689,752,778` in the 0.26 crate) — the streaming case, plus
  correct clamp/drop when the anchor falls off the scrollback cap (today's code
  would drift there).
- `Grid::scroll_up` auto-grows `display_offset` while parked
  (`grid/mod.rs:267-268`) — already what keeps the viewport glued.
- `Selection::update()` rewrites only `region.end`; the anchor is untouchable
  after `Selection::new` — exactly the asymmetry we want.
- User scroll (`scroll_display`) changes only `display_offset`; grid coords and
  the stored selection are untouched.

Our current code defeats all of this by rebuilding the whole `Selection` from
scratch every mouse-move (`set_selection_typed` → `Selection::new` + `update`).
The fix is to **create once, extend in place, and hook the wheel**.

### The sides detail (leftward-drag fix, BUGS.md #11)

`selection_sides` in `session_handle.rs` assigns endpoint sides by drag
direction so both endpoint cells are included. `Selection::update()` alone would
leave the anchor's side stale when the drag direction flips. The library has the
exact replacement: `Selection::include_all()` (`selection.rs:252-268`) —
verified line-for-line identical logic to `selection_sides` for non-Block types
(`start > end → (Right, Left)`, else `(Left, Right)`). Call it after every
`update()`.

## Implementation

### 1. `session_handle.rs` — two new methods

```rust
/// Begin a drag selection anchored at `point` (buffer coords, line 0 = top of
/// active area, negative = scrollback). The Term owns the Selection from here:
/// alacritty rotates it with the grid while output streams, so the anchor
/// stays glued to content with no help from the view.
pub fn start_selection(&self, ty: SelectionType, point: (i32, usize))
// term.selection = Some(Selection::new(ty, pt, Side::Left));
// (initial side is irrelevant — include_all overwrites both sides on the
// first extend; a zero-area Simple selection is_empty → paints nothing,
// so this also replaces the old clear_selection() on single click)

/// Move the drag end to `point`, leaving the anchor alone. Returns false if
/// the Term dropped the selection (clear/erase/reflow/rotated-out) — caller
/// should end the drag.
pub fn extend_selection(&self, point: (i32, usize)) -> bool
// if let Some(sel) = term.selection.as_mut() {
//     sel.update(pt, Side::Right);  // side overwritten next line
//     sel.include_all();            // == selection_sides, library edition
//     true
// } else { false }
```

Keep `set_selection` / `set_selection_typed` as the programmatic/test seams they
are. `selection_sides` stays for them (or they migrate to `include_all` too —
implementer's choice, tests pin the resolved ranges either way).

### 2. `view.rs` — shrink the drag state, hook the wheel

- `drag_anchor: Option<(usize, usize, SelectionType)>` →
  `drag_selecting: Option<SelectionType>`. The kind is the only view-side drag
  state left; the anchor lives in the Term. Rewrite the field docs (the current
  ones document the viewport-row scheme).
- **mouse-down** (`on_mouse_down`, ~`view.rs:1367`): for all three click kinds,
  `start_selection(kind, (hit.buffer_line, hit.col))`. Single-click Simple no
  longer calls `clear_selection()` — the fresh zero-area selection is invisible
  (`is_empty`) and, unlike a bare clear, its anchor rotates with streaming
  before the first move (kitty's `a13f815591` bug class). Semantic/Lines paint
  the word/line immediately via `to_range` expansion, same as today.
- **mouse-move** (`on_mouse_move`, ~`view.rs:1404`): keep the pressed-button
  guard; then `if !extend_selection((hit.buffer_line, hit.col)) { self.drag_selecting = None; }`
  — drop the display-offset algebra entirely.
- **scroll wheel** (`on_scroll_wheel`, local-scrollback branch only,
  `view.rs:2089-2094`): after `handle.scroll_lines(lines)`, if
  `drag_selecting` is active, re-hit `event.position` (hit_cell reads the *new*
  display_offset) and `extend_selection`. Order is load-bearing: scroll first,
  hit-test second (kitty and ghostty both pin this ordering; reversed it
  compounds one row per event). Not gated on cell-changed. The VT-mouse-report
  branch above is untouched — during app mouse reporting there is no local drag.
- **mouse-up / drag cancel paths** (`view.rs:1499,1527`): rename only; the
  existing pressed-button check on move already covers release-outside-pane.

### 3. Tests (targeted — `nice-term-view` only)

Port/replace the `63b6080` test and re-pin the whole contract through the new
path (real `Term` + `Processor`, the harness the existing tests already use):

1. **Streaming regression (the case that must not break):** park scrolled-up,
   `start_selection`, stream lines through the parser (Term rotates the
   selection), `extend_selection`, assert the resolved range still covers the
   *clicked content* (compare against the text, or the content-relative line —
   never a viewport row).
2. **User-scroll-mid-drag (the bug):** `start_selection`, `scroll_display(Delta(n))`,
   `extend_selection` at a point derived from the same viewport position →
   resolved range spans from the original content to the newly revealed content.
3. **Fresh-click anchor rotates (zero-length):** `start_selection`, stream lines
   *before any extend*, then `extend_selection` — anchor is on the clicked
   content, not n rows below.
4. **Both-endpoint inclusion re-pinned via `include_all`:** leftward, rightward,
   upward drags include both endpoint cells (replaces the `selection_sides`
   pins for the drag path).
5. **Scrollback cap:** fill history to the limit, anchor near the top, keep
   streaming — resolved range clamps/shrinks (library behavior), no drift, no
   panic. Assert content coords only (viewport drifts by design at cap).

### 4. Manual validation (scratch-env Nice Dev, per CLAUDE.md)

- Drag, hold, wheel-scroll up **without moving the mouse** → selection extends
  live into scrollback, anchor glued to the clicked text.
- Same, then keep dragging after the scroll → no jump.
- Park scrolled-up over streaming output (`yes` or a print loop), drag → anchor
  stays on the clicked row (63b6080 regression).
- Leftward drag → leftmost cell selectable.
- `less` / `man` scroll → selection stays on screen rows; expected, all
  terminals behave this way.

## Review addenda (post-implementation)

Findings from the fresh-eyes review of the landed diff, folded in or accepted:

- **`drag_selecting` is a gesture flag, not a selection-liveness flag.** If the
  Term drops the selection mid-drag, extends become no-ops but the flag stays
  set until a real release. Tying the two together leaked VT reports: with app
  mouse reporting on and a Shift-drag in flight, a mid-drag drop let mouse-up
  fall through to the report branch and send the app a Release for a press it
  never saw.
- **Accepted behavior change (alacritty parity):** the Term drops the selection
  on erase/clear sequences intersecting it, alt-screen swap, and column resize
  (`EL`/`ED`, `term/mod.rs:1657,1773,1786,1803`, `:733`, `:682`). A live drag
  over such content now stops extending until re-pressed, where the old
  rebuild-every-move code resurrected it. This is what upstream alacritty does
  (their `update_selection` early-returns on `None`).
- **Accepted behavior change (intended):** parked AT the bottom with output
  streaming, the anchor now follows its text up and off the viewport (grid
  rotation), where the old scheme pinned it to the screen row. Content-locked
  is the correct reading; all four surveyed terminals behave this way.
- **Correction to §Implementation:** "during app mouse reporting there is no
  local drag" is wrong — Shift-drag is the local override and does drag
  locally while reporting is active. The wheel hook is still correct: Shifted
  wheel events route to the local-scrollback branch (same override), unshifted
  ones go to the app without touching the selection.
- **Known narrow divergence (library, alacritty has it too):** a DECSTBM
  scrolling region with a non-zero top that scrolls during a live drag can
  clamp the anchor to the region top with column reset to 0
  (`Selection::rotate`'s `range_top != 0` arm). The old view-held anchor was
  immune. Not worth defending against.
- **Residual test gap:** the scroll-then-extend *ordering* inside
  `on_scroll_wheel` has no automated pin — `nice-term-view` has no gpui test
  harness. The core-level tests pin everything up to that seam; the ordering
  itself is covered by manual validation.

## Out of scope (deliberate, YAGNI)

- **Edge auto-scroll** (drag past the top edge keeps scrolling): a separate
  feature needing a repeat timer (ghostty 15ms tick / alacritty scheduled
  scroll events). The wheel hook covers the core need; add later if wanted.
- **Faux-scroll apps** (`less`, `man`): unsolvable without content diffing;
  accepted by alacritty (#1022), kitty, wezterm, ghostty alike.
- **Stable row ids** (wezterm-style): equivalent to the library's rotation for
  every case we have; strictly more machinery. Revisit only if we ever persist
  selections across reflow.
