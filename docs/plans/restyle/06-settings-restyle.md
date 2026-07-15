# Restyle 6/6 — Settings window restyle, per-scheme tabs, popup removal

Round-2 (see `docs/plans/restyle/ROUND2-NOTES.md`; mock:
`docs/design/restyle-mocks.html` "Settings · Appearance" section — clickable
Light/Dark tabs, translucent settings window, merged theme grid). DEPENDS ON
plan 05 (the merged theme model + single picker). Runs last in the round-2
set.

## Goal

Three things: (1) the one-time restyle popup is removed — everyone keeps
their theme settings and automatically gets the new opacity/blur/line-height
defaults; (2) the settings window drops its opaque chrome and mirrors the
main window's translucent styling; (3) the appearance pane regroups —
scheme-independent settings on top, then a subsection headed by Light/Dark
mode TABS containing that scheme's theme, opacity, and blur.

## Decisions (locked — do not re-litigate)

### Popup removal + new-defaults migration (supersedes plan 3's migration)

- **Remove the one-time popup entirely**: the startup presentation wiring,
  the "Try the new look" / "Keep my setup" write paths, and the
  `restyle_popup_shown` field in `Appearance`
  (`crates/nice/src/theme_settings.rs` ~139-142, ~335, ~362, ~423). The
  decoder simply ignores the legacy key. The confirmation-modal COMPONENT
  (`confirmation_modal.rs`) stays — only this use of it goes.
- **Existing users keep their theme settings**: the legacy pinning of
  palette/terminal-theme/accent/OS-sync literals for absent keys (plan 3's
  migration step, the LEGACY-literals block in `theme_settings.rs` ~438)
  STAYS — that is what "everyone keeps their current theme settings" means
  for default-riding stores. (After plan 05 the pinned chrome-palette keys
  are inert; the pinned terminal ids are the merged selection.)
- **The new comfort defaults auto-apply to everyone** (Nick's decision,
  incl. line-height): REMOVE the pinning of `opacity=100`, `blur=0`, and
  `line_height=1.0` for existing stores. Absent keys now resolve to the
  shipped defaults — dark 80% / blur 30, light 90% / blur 30, line-height
  1.3 — for fresh AND existing users alike. Users who explicitly set any
  of these keep their explicit values (absent-key semantics only).
- Timing note: the popup shipped only on the unreleased `worktree-restyle`
  branch — no released store ever saw it, so no popup-era store migration
  is needed. Dev/test stores that ran the branch may hold pinned
  100/0/1.0 literals; that is acceptable (they read as explicit values).

### Settings window mirrors the main window

- The settings window becomes translucent to the SAME extent as the main
  window: it reads the same per-scheme opacity/blur settings, uses the
  same `WindowBackgroundAppearance` resolution, and live-applies slider
  changes to itself through the existing theme fanout
  (`crates/nice/src/settings/window.rs` — currently opaque).
- Styling mirrors the main window (mock is the source of record): the
  terminal-resolved mono family throughout; 28pt titlebar with native
  traffic lights and centered "Settings" title text (the single-tab
  convention from plan 04, reuse its centered-title treatment); flat nav
  list (no card) with accent-colored active row and a `glass_line`
  hairline at its right edge; `glass_fill` hover treatment; no opaque
  panel fills anywhere in the window body.
- Modals, popovers, and other auxiliary windows stay opaque (unchanged
  plan-3 non-goal — only the settings window joins the main window).

### Appearance pane regroup (mock-faithful)

- Pane order:
  1. **Sync with OS appearance** toggle (scheme-independent);
  2. **Accent** swatch row (scheme-independent);
  3. **Per-scheme subsection** headed by two text tabs — "Light mode" /
     "Dark mode" — using the SAME grammar as titlebar tabs: mono text,
     `ink3` inactive / `ink` active, 1px accent underline under the active
     tab (inset per the mock's `.scheme-tab`), seated on a `--hairline`
     rule that underlines the whole tab row. No grey underline on the
     inactive scheme tab (mock-faithful; the pair is self-evidently
     tabs). The tab defaults to the currently ACTIVE scheme. Content:
     - the merged **Theme** picker (from plan 05) scoped to that scheme —
       grid of cards with name + color chips per the mock (a dropdown is
       acceptable if the grid is disproportionate effort, but the grid is
       the mock's rendering; prefer it);
     - **Opacity** slider for that scheme (55–100%);
     - **Blur** slider for that scheme (0–60px).
     Editing a tab's controls edits THAT scheme's stored values regardless
     of which scheme is live (replaces the old "sliders edit the active
     scheme" behavior — the tab, not the OS, now picks the target).
  4. **Custom themes** import section (scheme-independent), below the
     tabbed subsection.
- Other panes (General/Fonts/Shortcuts/Claude/Advanced/About) get the
  visual re-skin only (typography, hairlines, fills) — no regrouping.

## Scope / key files

- `crates/nice/src/theme_settings.rs` — popup field/paths removal; pinning
  trimmed to theme/accent/sync only.
- Startup popup wiring (grep `restyle_popup_shown` consumers — app startup
  / app_shell) — removed.
- `crates/nice/src/settings/window.rs`, `root.rs` — translucency +
  titlebar/nav restyle.
- `crates/nice/src/settings/appearance_pane.rs` — regroup; scheme tabs;
  per-tab theme/opacity/blur; custom-themes section placement.
- `crates/nice/src/settings/controls.rs` — restyled controls (toggle,
  slider, swatches) per the mock's flat treatment.
- Scenario/self-tests: settings scenarios, `theme_fanout_live`, migration
  unit tests.

## Non-goals

- No theme-model changes (plan 05 owns the merge).
- No new settings; no font-pane changes beyond the re-skin (line-height
  slider stays in the font pane).
- No transparency for modals/popovers/other windows.

## Known interactions

- Plan 04's centered-title treatment is reused for the "Settings" titlebar
  — if 04 landed first (it does in this set), share the component rather
  than re-implementing.
- The settings window previously being opaque may be load-bearing for
  readability of dense panes over busy wallpapers — the shipped opacity
  floor (55%) applies here too; validation checks legibility at the floor.
- Editing the INACTIVE scheme's values means the live window doesn't
  change — the tab content must make the target scheme unambiguous (the
  tab labels carry it; no extra badging needed per the mock).
- Scenario pixel tests: settings-window translucency is environment-
  dependent — pin opacity 100% in scenario fixtures except where
  transparency itself is under test (same rule as plan 3).

## Validation

- `cargo test --workspace`:
  - migration: a seeded legacy store (absent appearance keys) resolves to
    pinned theme/accent/sync literals AND the NEW opacity/blur/line-height
    defaults (80/30, 90/30, 1.3); explicit stored values are untouched;
    no `restyle_popup_shown` is ever written;
  - appearance-pane logic: tab targeting writes the selected scheme's keys
    (edit Dark values while Light is live and vice versa).
- Black-box (worktree lock, dev install, scratch env, `caffeinate -d`):
  - NO popup on any launch: fresh store, and a seeded pre-restyle store;
    the seeded store keeps its Catppuccin theme + accent while the window
    launches translucent at 80/30 (dark) with line-height 1.3;
  - the settings window renders translucent + blurred over a busy
    wallpaper, visually matching the main window's surface (side-by-side
    screenshot); dense panes stay legible at 55% opacity;
  - appearance pane matches the mock: OS-sync + accent on top; Light/Dark
    tabs with 1px accent underline; per-tab theme grid + opacity + blur;
    custom-themes section below; switching the Dark tab's theme while the
    app is in dark mode re-skins the whole app live; moving the Light
    tab's opacity slider while dark is live changes nothing visibly, then
    flipping the OS scheme shows the new light value applied;
  - "Settings" renders as centered titlebar text in the settings window's
    28pt bar.
