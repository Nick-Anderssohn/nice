# Restyle feel-check round 2 — decisions (plans 04/05/06 implemented)

Collected from Nick's feel-check of the merged restyle (worktree-restyle,
2026-07-15). Mock updates for these are committed in
`docs/design/restyle-mocks.html` (source of record). Implemented as plans
04/05/06; §5 records the round-2.5 revisions from Nick's feel-check OF that
implementation.

## 1. Kill the one-time restyle popup (replaces plan 3's migration design)

- Remove the popup entirely (`restyle_popup_shown` flag machinery can go or
  stay as a no-op migration marker — implementer's call).
- Everyone (existing users included) KEEPS their current theme settings —
  no defaults-flip is applied to existing stores, no pinning prompt.
- Everyone AUTOMATICALLY gets the new opacity/blur defaults applied:
  dark 80% / blur 30, light 90% / blur 30 (confirmed by Nick).
- DECIDED (Nick 2026-07-15): line-height 1.3 ALSO auto-applies to existing
  users (they can slide back to 1.0).
- Fresh installs: unchanged from plan 3 (Nice defaults, Terracotta, etc.).

## 2. Merge chrome palette + terminal theme into ONE theme setting

- One theme entry drives BOTH chrome colors and terminal colors; the two
  separate pickers collapse into one.
- Terminal catalog has 12 themes; chrome palettes only 4 (Nice, MacOs,
  Catppuccin Latte/Mocha). Missing chrome palettes must be AUTHORED for:
  solarized-light, solarized-dark, dracula, nord, gruvbox-light,
  gruvbox-dark, tokyo-night, one-dark — colors are well-known, look up
  canonical values on the web.
- DECIDED (Nick 2026-07-15): "Nice" chrome pairs with the "Nice" terminal
  theme (nice-default-light/dark per scheme). The MacOs chrome palette is
  already excluded from the settings picker (`chrome_palettes_for` filters
  it out, appearance_pane.rs) — it retires with the merge.
- Scheme scoping: each scheme's picker lists themes valid for that scheme
  (dark themes usable in the light tab per the mock caption).
- DECIDED (Nick 2026-07-15): imported Ghostty "Custom themes" DERIVE their
  chrome half from the imported theme's own colors (background → window
  surface, foreground → ink family) so the whole window matches the
  imported theme.

## 3. Settings window restyle + appearance regrouping

- Settings window becomes translucent to the same extent as the main
  window; mirrors main-window styling (mono type, hairlines, flat nav with
  accent active row, 28pt titlebar with centered title text).
- Appearance pane regrouped: scheme-independent settings (OS sync, accent)
  on top; then a subsection headed by **Light mode / Dark mode tabs** (same
  text + 1px accent underline grammar as titlebar tabs) containing that
  scheme's THEME, OPACITY, and BLUR.
- Mock: "Settings · Appearance" section in restyle-mocks.html.

## 4. Round-1 items (mocked, awaiting implementation round)

- Inactive-tab affordance: DECIDED (Nick 2026-07-15) — **grey underline**
  (the mock default): same 1px geometry as the active underline, in
  --tab-underline-idle (white 16% dark / ink 17% light).
- Tab underlines (active accent + inactive grey) are now **1px** (plan 1
  said 2px — mock supersedes).
- Single-tab mode: with one tab, render NO tab chrome — the title (with
  status dot) is the window's centered titlebar text; overflow chevron
  hidden. Underline grammar only appears at ≥2 tabs.
- Gear icon: replaced with the stroke cog now in the mock (Feather-style
  geometry, stroke ~1px at 14px) — shipped sun-like gear is superseded.
- `+` button vertical alignment in the titlebar is off in the shipped
  build — implementation bug; the mock (flex-centered bar-tail) is correct.
  *(Superseded by §5: the drift measured HORIZONTAL, not vertical — fixed
  via `TOOLBAR_TRAILING_PAD` 20→10.)*

## 5. Round-2.5 revisions (Nick's feel-check of the 06 build, 2026-07-15)

- **Manual Scheme control RESTORED** — dropping it in the plan-06 regroup was
  a mock miss (Nick's own words): with OS-sync off there was no light/dark
  flip left in the UI. It returns as a flat segmented Light|Dark control
  (mock `.scheme-seg`: hairline border, radius 7, selected cell over-glass
  `--fill-x` + ink), placed directly BELOW the OS-sync toggle and ABOVE the
  Accent row (Nick flipped Scheme/Accent at the round-2.5 check; both sit
  above the Light/Dark edit-target tabs); still disabled (0.4 opacity, no
  handlers) while OS-sync is on. It flips the LIVE scheme — distinct from
  the tabs, which only pick the editing target.
- **Theme picker back to a DROPDOWN, no color chips** — the 3-column card
  grid truncated nearly every theme name at real pane widths. One "Theme"
  row per scheme tab with the in-house dropdown (full display names); the
  `settings.terminal.{light,dark}Picker` a11y ids and the
  `apply_terminal_theme_id` selection contract are unchanged.
- **`+` alignment root-caused as HORIZONTAL, not vertical** — pixel
  measurement of the shipped build: the glyph's ink center is level with the
  title text; the drift was `TOOLBAR_TRAILING_PAD` still at the Swift-era
  20pt, vs the mock's 10px `.bar-tail` inset. Now 10pt (the `+` right edge
  sits 10pt off the window corner, matching the mock).
