# Restyle 2/3 — Flatten the sidebar into the terminal surface

Part of the 2026-07 restyle (approved mock: `docs/design/restyle-mocks.html`,
Style A / 28pt — open in a browser). Depends on `01-titlebar-restyle.md`
(28pt fill-less titlebar; collapse toggle already relocated to the titlebar).
Transparency lands in plan 3; this plan keeps everything opaque.

## Goal

The sidebar stops being a floating card and becomes part of the terminal:
same background surface, terminal mono typography, accent-text active row,
and a single 1px hairline separating it from the pane area. Its old 52pt top
strip disappears; the mode switcher (claude tabs / file explorer) joins the
gear in the footer.

## Decisions (locked — do not re-litigate)

- **Card chrome removed**: no inset gutter, no rounded corners, no border,
  no drop shadow, no distinct panel fill. The sidebar column shares the
  window-body surface (the terminal background). In
  `crates/nice/src/sidebar_shell.rs`: `build_sidebar_card` loses the
  `CARD_INSET` wrapper padding, `rounded`, `border`, `shadow`, and its
  `sidebar_background` fill.
- `CARD_*` constants in `crates/nice-theme/src/chrome_geometry.rs` retire
  with their provenance tests (or re-cite the mock if any survive for the
  peek overlay — see below).
- **Hairline**: 1px line at the sidebar's right edge, starting at the
  titlebar's bottom (i.e. the sidebar column's full height under the bar),
  using a new "line over glass" treatment — NOT the theme `line` slot:
  - dark scheme: white at 8% alpha; light scheme: `ink` at 10% alpha.
  - Add a helper in `nice-theme` (e.g. `glass_line(scheme) -> Srgba`) so
    plan 3's translucent surfaces reuse the same rule; cite the mock's
    `--hairline` values.
- **Top strip removed** (`build_top_strip`): no drag strip, no in-sidebar
  controls row. Window dragging is the titlebar's job (plan 1). Sidebar
  content (project headers + session rows) starts directly below the
  titlebar.
- **Mode switcher → footer**: the tabs/files toggle buttons move into
  `build_footer` alongside the gear — small icon buttons using the mock's
  EXACT stroke SVGs (chrome icon set from plan 1, verbatim from
  `docs/design/restyle-mocks.html` `.sb-footer`): tabs mode = three
  horizontal lines (14×12, 1.4 stroke, round caps), files mode = outline
  folder (14×12, 1px stroke). Active mode in `ink` with a faint fill using
  the mock's `--fill-active` values — **white 6% alpha dark / `ink` 5%
  alpha light** (NOT the glass-line 8%/10% family; add a companion helper
  next to `glass_line`, e.g. `glass_fill(scheme)`), inactive `ink3`. The
  gear stays at the trailing edge but is redrawn as a NEW thin-stroke gear
  matching the set's style — the mock's gear was a Unicode font glyph
  (`⚙︎`), deliberately not shipped (font-dependent rendering); match its
  visual weight, don't reuse SF_GEAR.
  The footer is the only non-content chrome the sidebar keeps. Its current
  1pt TOP RULE (theme `line` slot, in `build_footer`) is **removed** — the
  mock's footer has no rule; it sits flush on the sidebar surface.
- **Typography**: session rows and project headers render in the terminal's
  resolved mono family (`nice-term-view::font` chain — SF Mono default), not
  the UI sans. Only the FAMILY changes: row text SIZE continues to come from
  the existing user-settable sidebar font size
  (`crates/nice/src/settings/sidebar_font.rs`, default
  `DEFAULT_SIDEBAR_FONT_PX` 12 — which matches the mock's density) with its
  proportional zoom intact. Project headers stay small uppercase `ink3`.
- **Active row**: accent-colored text (`active_chrome_accent`), NO fill, no
  bar. Hover: faint fill only (`--fill-active`: white 6% dark / `ink` 5%
  light — the same `glass_fill` helper as the footer buttons), normal text
  color. The old active-row `selection_tint` fill goes away.
- **Multi-selected non-active rows**: keep a visible marker — the same faint
  `glass_fill` (6%/5%) persists on selected rows (replacing today's fainter
  `selection_tint` fill), normal text color. The ACTIVE row stays fill-less
  (accent text is its marker), so active vs merely-selected remains
  distinguishable.
- **Status dots** in rows: same `StatusDot` component, colors + pulse
  untouched, rendered small (**5pt** in rows; plan 1 set 6pt in tabs).
- **Peek overlay** (collapsed-sidebar hover peek): behavior unchanged. It
  floats OVER terminal content, so unlike the docked sidebar it keeps an
  elevated panel treatment for readability: its fill becomes the **theme
  background at FULL alpha** (replacing the current `s.chrome` 0.70-alpha
  fill — the last chrome-slot consumer after plans 1–2; an opaque fill also
  avoids double-alpha stacking once plan 3 makes the window translucent) +
  shadow. Restyle its contents to match the new flat typography.
- Resize handle, width persistence, min/max widths: unchanged
  (`SIDEBAR_*` constants stay).
- Rename-in-place, context menus, drag/reorder, file-browser mode behavior:
  functionally unchanged; only their visual skin follows the new typography
  and fills.

## Non-goals

- No transparency/alpha yet (plan 3) — the shared surface is still the
  opaque terminal background.
- No changes to sidebar data/model, session grouping, or footer actions
  beyond adding the mode buttons.

## Known interactions

- Thinking dots are fixed brand Terracotta; on the default accent they blend
  with the active row's accent text. Accepted (flagged during design).
- `app_shell.rs`'s body backing bleed (`terminal_backing_color`) previously
  existed partly to color the card gutter; with the card gone, verify no
  seam/mismatch remains between sidebar and pane surfaces.

## Validation

- `cargo test --workspace`; update sidebar scenario self-tests
  (`sidebar_live` and friends) for the flat visuals, footer mode buttons,
  and removed strip.
- Black-box under the worktree lock (install dev build, scratch-env direct
  bundle launch, `caffeinate -d`, real HID-level synthetic input):
  - sidebar surface is pixel-identical to the terminal background on both
    schemes; single 1px hairline at the edge (white-alpha dark / ink-alpha
    light), starting below the titlebar;
  - rows render in the terminal mono; active row is accent text with no
    fill; hover shows faint fill; multi-selected non-active rows show the
    faint persistent fill;
  - mode toggle works from the footer (tabs ↔ files), gear opens settings;
    the footer has no top rule;
  - collapse (titlebar toggle) → peek overlay still appears on hover and is
    readable over busy terminal content;
  - resize handle still drags within min/max.
