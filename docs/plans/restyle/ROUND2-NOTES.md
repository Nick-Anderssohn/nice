# Restyle feel-check round 2 — pending changes (NOT yet planned/implemented)

Collected from Nick's feel-check of the merged restyle (worktree-restyle,
2026-07-15). Mock updates for these are committed in
`docs/design/restyle-mocks.html` (source of record); implementation plans are
NOT yet authored. Keep this list until the round-2 plans exist.

## 1. Kill the one-time restyle popup (replaces plan 3's migration design)

- Remove the popup entirely (`restyle_popup_shown` flag machinery can go or
  stay as a no-op migration marker — implementer's call).
- Everyone (existing users included) KEEPS their current theme settings —
  no defaults-flip is applied to existing stores, no pinning prompt.
- Everyone AUTOMATICALLY gets the new opacity/blur defaults applied:
  dark 80% / blur 30, light 90% / blur 30 (confirmed by Nick).
- OPEN QUESTION (asked, awaiting answer): does line-height 1.3 also
  auto-apply to existing users, or do they keep 1.0 (grid-affecting)?
- Fresh installs: unchanged from plan 3 (Nice defaults, Terracotta, etc.).

## 2. Merge chrome palette + terminal theme into ONE theme setting

- One theme entry drives BOTH chrome colors and terminal colors; the two
  separate pickers collapse into one.
- Terminal catalog has 12 themes; chrome palettes only 4 (Nice, MacOs,
  Catppuccin Latte/Mocha). Missing chrome palettes must be AUTHORED for:
  solarized-light, solarized-dark, dracula, nord, gruvbox-light,
  gruvbox-dark, tokyo-night, one-dark — colors are well-known, look up
  canonical values on the web.
- OPEN QUESTION (asked): fate of the MacOs chrome palette (no terminal
  counterpart) — retire, or keep as a special case?
- Scheme scoping: each scheme's picker lists themes valid for that scheme
  (dark themes usable in the light tab per the mock caption).

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

- Inactive-tab affordance: candidates in the mock (grey underline /
  dividers / faint chip). Nick's pick: PENDING (leaning grey underline —
  the mock default).
- Tab underlines (active accent + inactive grey) are now **1px** (plan 1
  said 2px — mock supersedes).
- Single-tab mode: with one tab, render NO tab chrome — the title (with
  status dot) is the window's centered titlebar text; overflow chevron
  hidden. Underline grammar only appears at ≥2 tabs.
- Gear icon: replaced with the stroke cog now in the mock (Feather-style
  geometry, stroke ~1px at 14px) — shipped sun-like gear is superseded.
- `+` button vertical alignment in the titlebar is off in the shipped
  build — implementation bug; the mock (flex-centered bar-tail) is correct.
