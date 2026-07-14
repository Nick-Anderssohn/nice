# 02 — Block cursor hides the character underneath it

## Bug

Verbatim from the user:

> "The cursor is solid, so you can't see the character that is behind it when
> it is on top of one. Any suggestions on how to fix that? Shade it differently?
> Auto-change the color of the character behind it and draw it on top of the
> character? Research how popular terminals typically handle this."

The focused block cursor is painted as a fully-opaque accent-colored quad over
the cell, and the glyph in that cell is **deliberately not painted at all**, so
whatever character the cursor sits on top of is invisible. Every mainstream
terminal instead keeps the character readable by drawing it back on top of the
block in a contrasting color (reverse-video). Nice's own code comments show this
was always intended as a follow-up ("inverse-video caret text is a later slice").

## Current rendering (file:line evidence)

All paths in `crates/nice-term-view/src/element.rs`.

1. **The glyph under a solid cursor is skipped during row planning.**
   `plan_row` takes `solid_cursor_col: Option<usize>` and, for the matching
   column, flushes the batched run and `continue`s without emitting any glyph
   item — element.rs:428-435:

   ```rust
   // A solid cursor covers its cell; skip the glyph so it does not paint
   // over the block (inverse-video caret text is a later slice). ...
   if solid_cursor_col == Some(c) {
       flush(&mut batch, &mut items);
       flush_proc(&mut proc_batch, &mut items);
       continue;
   }
   ```

2. **The cursor is drawn as one solid accent quad, focused.** In `paint`, after
   the background quads and before the foreground glyph runs, element.rs:1105-1121:

   ```rust
   if let Some(cur) = &cursor {
       if !composing {
           let x = ox + px(cur.col as f32 * cw);
           let y = oy + px(cur.row as f32 * ch);
           if cur.solid {
               window.paint_quad(fill(Bounds { origin: point(x, y),
                   size: size(px(cw), px(ch)) }, accent));   // opaque fill, no glyph after
           } else {
               paint_hollow_cursor(window, x, y, cw, ch, accent);
           }
       }
   }
   ```

   Because the glyph was skipped in step 1 and the quad is opaque, the cell reads
   as a blank accent block. The foreground loop (element.rs:1155-1208) then paints
   every *other* cell's glyph but never this one.

3. **`CursorPaint` carries no glyph data** — only placement + focus state
   (element.rs:172-179): `row`, `col`, `solid`. The focused caret is `solid`;
   the unfocused caret is a hollow outline.

4. **Cursor color** is the theme's `cursor` override, else the accent token
   (element.rs:849-864) — a deliberate identity color, **not** the cell's own
   foreground.

5. **Hollow (unfocused) cursor already shows the glyph.** `solid_cursor_col` is
   only set when the caret is solid, so for a hollow caret the glyph is *not*
   skipped — it paints normally in its own fg and `paint_hollow_cursor`
   (element.rs:1451-1484) draws a 1px accent outline around it. This already
   matches the standard unfocused convention; **no change needed there.**

6. **Data needed for the fix is already retained.** `GridCache.rows:
   Vec<Vec<PaintCell>>` (element.rs:635) keeps each visible row's fully-resolved
   `PaintCell { ch, fg, bg, bold, italic, underline, strikethrough, wide_spacer }`
   (element.rs:155-170). The cursor row is damaged on every frame, so
   `cache.rows[cur.row][cur.col]` is always current at paint time — no new plumbing
   is required to know the glyph and its colors under the cursor.

Existing color helpers to reuse/extend: `invert_rgb` (element.rs:1508),
`dim_rgb` (element.rs:1525), and the pure color module
`crates/nice-term-view/src/color.rs`.

## How other terminals handle it (survey)

The universal pattern is **reverse-video**: keep drawing the glyph, but recolor
it so it reads against the cursor block. The variations are (a) whether the block
follows the cell's own fg or a fixed cursor color, and (b) whether there's a
minimum-contrast/"smart" fallback for when block color ≈ glyph color.

| Terminal        | Block color               | Glyph on block                              | Low-contrast fallback |
|-----------------|---------------------------|---------------------------------------------|-----------------------|
| xterm           | cursor color              | cell drawn reverse-video                     | none (double-reverses reverse-video cells) |
| Alacritty       | cell **foreground**       | cell **background** (true fg/bg swap)        | none by default |
| kitty           | cursor color              | `cursor_text_color` (defaults to background) | user-set color only |
| Ghostty         | cursor color              | `cursor-text` / `cursor-invert-fg-bg`        | `minimum-contrast` (WCAG ratio 1–21) |
| iTerm2          | adaptive ("smart cursor color") | reverse-video                          | "Smart cursor color" + "Minimum contrast" (brightness-diff guarantee) |
| Terminal.app    | cursor color              | cell drawn in cursor-inverse                 | none exposed |
| **zed (GPUI)**  | player cursor color       | `terminal_ansi_background` (fixed bg color)  | none |

zed is the closest analog because it's GPUI-native and uses a fixed *identity*
cursor color (the "player" color), exactly like Nice's accent. Its terminal
element shapes the cursor char in `terminal_ansi_background` and draws it over the
block for a focused `Block` cursor, and switches to a `Hollow` outline (no text)
when unfocused — vendor/zed `crates/terminal_view/src/terminal_element.rs:1214-1268`:

```rust
let cursor_text = window.text_system().shape_line(
    str_trxt.into(), font_size,
    &[TextRun { color: theme.colors().terminal_ansi_background, .. }], None);
...
CursorShape::Block if !focused => (EditorCursorShape::Hollow, None),
CursorShape::Block            => (EditorCursorShape::Block,  Some(cursor_text)),
```

Note zed has **no** contrast fallback — if the player color were ever close to
the terminal background, its cursor glyph would wash out. iTerm2 and Ghostty are
the terminals that solved that edge case; Ghostty's is the cleanest to copy
(WCAG contrast ratio threshold).

Sources: [Alacritty smooth-cursor fork discussion](https://github.com/GregTheMadMonk/alacritty-smooth-cursor),
[kitty inverted-cursor issue #234](https://github.com/kovidgoyal/kitty/issues/234),
[iTerm2 smart cursor color docs](https://iterm2.com/documentation-preferences-profiles-colors.html) /
[algorithm issue #776](https://gitlab.com/gnachman/iterm2/-/work_items/776),
[Ghostty minimum-contrast reference](https://ghostty.org/docs/config/reference),
[foot reverse-video cursor issue #1347](https://codeberg.org/dnkl/foot/issues/1347).

## Recommended fix

**Reverse-video-over-accent with a WCAG minimum-contrast fallback.** Keep Nice's
accent-colored block (its deliberate identity color — don't recolor the block),
and draw the character back on top of it in a color chosen to be readable against
the accent. This is the zed approach plus the Ghostty/iTerm2 contrast guard that
zed lacks — it is the minimal change that both fixes the bug and never regresses
into an invisible glyph.

### Color logic

Let `accent` = the cursor block color (element.rs:773 / :849-864, as a
`0xRRGGBB`), and let `cell = cache.rows[cur.row][cur.col]` be the resolved cursor
cell. Compute the glyph color `text_color`:

1. **Ideal reverse-video color** = the cell's own resolved background:
   `base = cell.bg.unwrap_or(default_bg)`. This makes the glyph read as a
   punched "hole" the color of the page behind the block — the same intuition as
   a true fg/bg swap, and identical to zed using the terminal background.

2. **Minimum-contrast guard (the low-contrast edge case).** Compute the WCAG 2.0
   contrast ratio between `base` and `accent`. If
   `contrast_ratio(base, accent) < MIN_CURSOR_CONTRAST` (default **3.0**, WCAG AA
   for large text / UI), replace `base` with **black or white**, whichever has the
   higher contrast against `accent`:

   ```
   text_color =
     if contrast_ratio(base, accent) >= MIN_CURSOR_CONTRAST { base }
     else if contrast_ratio(0xFFFFFF, accent) >= contrast_ratio(0x000000, accent)
          { 0xFFFFFF } else { 0x000000 }
   ```

   This covers the real failure mode: a theme whose accent/cursor color is close
   to the page background (`base ≈ accent`), where a plain reverse-video glyph
   would vanish into the block. Black/white is guaranteed to clear the threshold
   against any mid-tone accent, and picking by max-contrast handles both light
   and dark accents.

   WCAG helpers (put in `color.rs`, pure + unit-testable):
   - `relative_luminance(rgb) -> f32`: linearize each channel
     (`c/255`; `c<=0.03928 ? c/12.92 : ((c+0.055)/1.055).powf(2.4)`), then
     `0.2126*R + 0.7152*G + 0.0722*B`.
   - `contrast_ratio(a, b) -> f32`: `(Lmax + 0.05) / (Lmin + 0.05)`.

3. **Paint the glyph over the block.** After the solid-block `paint_quad`
   (element.rs:1116), if the cursor is solid and not composing and
   `cell.ch` is inkable (`!= ' '` and not `wide_spacer`), shape the single char
   and paint it at the cursor cell in `text_color`, honoring `cell.bold` /
   `cell.italic`. **Pass `background_color: Some(rgb(accent))` on the TextRun** so
   the zed-bg-luminance patch composites the glyph's antialiasing against the
   block (every other glyph carries its cell bg for exactly this reason — see the
   `paint_glyph_run` doc at element.rs:1384-1389). Use the existing procedural
   path for box-drawing glyphs if `cell.ch` is a box/block element, so those still
   tile — but recolor them to `text_color` with `bg = accent`.

Keep the `plan_row` skip (element.rs:431) exactly as is: the normal-fg glyph must
still be suppressed from the batched runs, or it would paint under the block in
the wrong color. The new step paints the *recolored* glyph explicitly, after the
block, in correct z-order.

### Unfocused / hollow state

No change. The hollow caret already leaves `solid_cursor_col == None`, so the
glyph paints normally in its own fg and `paint_hollow_cursor` outlines it —
matching the standard "unfocused shows the character normally inside a hollow
box" convention. Just confirm the new cursor-glyph step is gated on
`cur.solid` so it never double-paints in the hollow case.

### Why this one

- Preserves Nice's accent-colored cursor identity (unlike a true fg/bg swap,
  which would make the cursor color follow the text and produce multi-colored
  cursors on colored text).
- Single, local, well-understood change; the data (`cache.rows`) and the paint
  seam (right after the block quad) already exist.
- The contrast guard fixes the one case zed gets wrong, at the cost of two tiny
  pure functions that are trivial to unit-test.

## Alternatives

The user explicitly asked for suggestions, so here are the two credible other
choices and why they weren't picked:

1. **True fg/bg swap (Alacritty-style).** Paint the block in the cell's *own*
   foreground color and the glyph in the cell's background. Pro: zero extra
   contrast logic — the swap is self-contrasting by construction, and it's what
   xterm/Alacritty do. Con: the cursor color stops being the theme accent and
   instead follows whatever text it's over (green over `ls` output, blue over a
   path, etc.), which throws away Nice's deliberate accent-cursor identity and
   makes the caret harder to track. Choose this only if the user prefers a cursor
   with no fixed color.

2. **iTerm2 "smart cursor color."** Adapt the *block* color (not the glyph) to be
   maximally distant from the glyph and its neighboring cells' backgrounds. Pro:
   best-in-class legibility in pathological themes. Con: substantially more code
   (sample neighbor cells, build the color-distance search) for a case the WCAG
   fallback already covers; violates YAGNI here. Good future enhancement, not the
   first fix.

A third, even-simpler option is the **pure-zed** version of the recommendation:
draw the glyph in `default_bg` over the block with *no* contrast guard. It's
fewer lines, but it silently fails for any theme whose accent ≈ background — so
the recommendation keeps the guard.

## Files touched

- `crates/nice-term-view/src/color.rs` — add pure `relative_luminance(u32)` and
  `contrast_ratio(u32, u32)` (+ a `MIN_CURSOR_CONTRAST` const and a small
  `cursor_text_color(cell_bg, accent)` helper), with unit tests.
- `crates/nice-term-view/src/element.rs` — extend the cursor paint block
  (element.rs:1105-1121) to draw the recolored glyph over the solid block; add a
  small `paint_cursor_glyph` helper (shape one char, `background_color =
  Some(accent)`), routing box-drawing chars through the procedural path. The
  `plan_row` skip stays. Extend `CursorPaint` only if it's cleaner to capture the
  cursor cell there instead of re-reading `cache.rows` at paint time (either
  works; `cache.rows` avoids new fields).

No cache-invalidation changes: the cursor glyph is painted fresh each frame from
`cache.rows` (like the block itself), and `accent` is already a per-frame paint
input, not a plan input (element.rs:629-631).

## Tests

**Unit (pure color logic, in `color.rs`):**
- `contrast_ratio(0x000000, 0xFFFFFF) == 21.0` (± epsilon); `contrast_ratio(x, x) == 1.0`.
- `relative_luminance` monotonic: `L(black) == 0.0`, `L(white) == 1.0`,
  `L(0x808080)` between.
- `cursor_text_color`: when `contrast_ratio(cell_bg, accent) >= 3.0` returns
  `cell_bg` unchanged; when `accent == cell_bg` (or near), returns black/white by
  max contrast — e.g. accent = dark default bg `0x090705` → returns `0xFFFFFF`;
  accent = a light color → returns `0x000000`.
- Edge: accent exactly mid-gray `0x777777` → falls back and picks whichever of
  black/white wins (assert it clears the threshold).

**Manual validation (dev bundle, scratch env per CLAUDE.md — never prod):**
- Move the cursor onto a dense line of text; every character under it stays
  readable as you arrow across.
- Type at a prompt: the character just typed is visible under the caret.
- Load/construct a theme whose `cursor`/accent color ≈ the background; confirm the
  glyph flips to black/white and stays legible (the contrast-guard path).
- Unfocus the window (click another app): caret becomes the hollow outline and
  the glyph shows in its normal color — unchanged.
- Cursor over a box-drawing char (e.g. in a TUI border) and over an emoji /
  wide glyph; over a reverse-video cell (`ESC[7m`); during IME composition
  (should still show the preedit overlay, no cursor glyph).

## Risks & interactions

- **bg-luminance AA composition (the zed patch).** The glyph's antialiasing is
  gamma-composited against the run's `background_color`. The cursor glyph must
  pass `background_color = Some(accent)` (not `None`/`default_bg`) or its edges
  will fringe against the wrong luminance on the block. This is the single most
  important implementation detail.
- **Wide glyphs / emoji.** Nice's cursor block is one cell wide; a wide glyph
  (2 cells) drawn over it overhangs the block on the right. Existing behavior
  already has a single-cell block, so this is not a regression — but consider
  widening the block to the glyph width for wide cells (zed does:
  `cursor_text.width.max(cell_width)`). Track as a small follow-up, not a blocker.
  Color emoji ignore the fg recolor (they carry their own color), matching
  Ghostty's "min-contrast excludes emoji" note — acceptable.
- **Box-drawing under the cursor.** Must route through the procedural glyph path
  (recolored) so line-joins still tile; otherwise a box char under the cursor
  would shape as a normal font glyph and could misalign.
- **Performance.** One extra `shape_line` per frame for the single cursor cell,
  memoized by GPUI's `LineLayoutCache` — negligible next to the batched-run work
  (fix round r5).
- **Don't remove the `plan_row` skip.** If both the skip and the new explicit
  paint were dropped/duplicated, the glyph would paint twice (once in normal fg
  under the block, once recolored over it) — keep exactly one suppressed +
  one recolored.
- **Reverse-video cells (`ESC[7m`).** The cell's `bg`/`fg` are already swapped by
  `fill_row` before caching, so `cell.bg.unwrap_or(default_bg)` is the
  *post-inverse* background — the reverse-video glyph gets the right base color
  for free. (xterm's double-reverse quirk is intentionally not replicated.)
