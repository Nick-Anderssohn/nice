# Restyle 4/6 — Titlebar polish: inactive underlines, single-tab title, cog gear, tail alignment

Round-2 feel-check fixes (see `docs/plans/restyle/ROUND2-NOTES.md`; design
source of record: `docs/design/restyle-mocks.html` — the "Inactive tabs"
control set to **Grey underline**, the "Tabs → Single" state, and the footer
cog). Independent of plans 05/06; runs first in the round-2 set.

## Goal

Four visual fixes to the shipped restyle titlebar/sidebar chrome: inactive
tabs get a grey underline so they read as clickable; all tab underlines thin
to 1px; a lone tab renders as the window's centered titlebar text instead of
tab chrome; the sidebar footer gear becomes a real cog (the shipped icon
reads as a sun); and the titlebar tail (`+` / overflow chevron) is properly
vertically centered.

## Decisions (locked — do not re-litigate)

- **Inactive-tab underline** (Nick's pick over dividers/chip): every
  inactive tab gets an underline with the SAME geometry as the active one
  (seated on the bar's bottom edge, inset 11px from the tab's outer edges,
  rounded), in a new scheme-scoped grey: **white 16% alpha dark / `ink`
  17% alpha light** (mock token `--tab-underline-idle`). Add a helper in
  `nice-theme` beside `glass_line`/`glass_fill` (e.g.
  `tab_underline_idle(scheme) -> Srgba`). Grammar: underline = tab,
  color = state.
- **Underlines thin to 1px** (was 2px): `TAB_UNDERLINE_HEIGHT` 2.0 → 1.0,
  `TAB_UNDERLINE_RADIUS` 1.0 → 0.5 (`crates/nice/src/toolbar.rs` ~118-120).
  Applies to both the accent (active) and grey (inactive) underline. The
  mock was updated to 1px and supersedes plan 1's "2px".
- **Single-tab mode**: when the strip holds EXACTLY one tab, render no tab
  boxes at all. Instead the tab's title + its status dot render as the
  window's centered titlebar text (macOS window-title convention):
  - centered across the FULL window width (not the residual strip space);
  - mono 12px, `ink`; status dot at the tab size (6pt) leading the text;
  - no underline, no ✕, no hover fill — display-only text (activation is
    meaningless with one tab; close/rename/context stay available in the
    sidebar). It remains part of the titlebar drag region.
  - The overflow chevron is hidden in this state (it cannot be needed);
    the trailing `+` button stays.
  - The moment a 2nd tab opens, the normal strip (with underlines) replaces
    it; no transition animation this plan.
- **Gear icon**: replace `MODE_GEAR_SVG` (`crates/nice/src/chrome_icons.rs`
  ~78 — circle + 6 radial rays, reads as a sun) with the mock's cog,
  verbatim geometry from `docs/design/restyle-mocks.html` `.sb-ico.gear`:
  viewBox `0 0 24 24`, `stroke-width 1.7`, `stroke-linecap round`,
  `stroke-linejoin round`, `<circle cx=12 cy=12 r=3.2>` plus the classic
  toothed-cog outline path (the `M19.4 15a1.65…` path embedded in the
  mock). Rendered size stays 14×14 (stroke lands ≈1px at render size).
  Update the `serves_a_new_thin_stroke_gear_not_a_glyph` provenance test
  to the new geometry.
- **Titlebar tail alignment**: the `+` button (and overflow chevron) must
  be vertically centered in the 28pt bar with the glyph optically centered
  in its hit box, matching the mock's flex-centered `bar-tail` (22px boxes,
  centered). The shipped build renders the `+` visibly off-center — find
  and fix the layout cause in `toolbar.rs` (do not band-aid with a magic
  offset unless the glyph's font metrics genuinely require an optical
  nudge; if a nudge is used, comment the measurement).

## Scope / key files

- `crates/nice/src/toolbar.rs` — underline constants; inactive underline in
  `render_pill`; the single-tab branch (strip-level decision, not per-tab);
  `show_chevron` interaction with single-tab; bar-tail centering.
- `crates/nice-theme/` (glass helpers module) — `tab_underline_idle(scheme)`
  with a provenance test citing the mock values.
- `crates/nice/src/chrome_icons.rs` — new cog SVG + updated tests.
- `crates/nice/src/pane_strip_live.rs`, `chrome_live.rs` — scenario updates
  for underlines/single-tab/tail geometry.

## Non-goals

- No theme/data-model work (plan 05), no settings work (plan 06).
- No Style B / divided-tabs support; no transition animations.
- StatusDot colors/animations untouched (size parameter only, as shipped).

## Known interactions

- Single-tab centered text overlaps the drag region by design; it must not
  capture clicks (drag/double-click-zoom pass through).
- Full-window centering means the text can sit over the strip area used by
  the collapse toggle at very narrow window widths — clamp/ellipsize so it
  never collides with the traffic-light cluster or the tail buttons.
- The tooltip on truncated titles (shipped in plan 1) should also apply to
  the centered single-tab title when ellipsized.

## Validation

- `cargo test --workspace` for toolbar/theme/icon tests (targeted crates
  during fix rounds).
- Update + run `pane_strip_live` / `chrome_live` scenarios.
- Black-box (required) — worktree lock, `scripts/rust-install.sh`, scratch
  env direct bundle launch, `caffeinate -d`, real HID input:
  - with ONE tab: centered title + status dot in the bar, no underline, no
    ✕, no chevron; `+` present and centered; drag + double-click-zoom on
    the bar still work over the title text;
  - open a 2nd tab: strip appears; active tab has a 1px accent underline,
    inactive has the 1px grey underline (verify both schemes — grey must be
    visible on light);
  - gear in the sidebar footer reads as a toothed cog (screenshot compare
    against the mock);
  - `+` and chevron glyphs are vertically centered in the bar (zoomed
    screenshot; compare against the mock's bar-tail).
