# Restyle 5/6 — Merge chrome palette + terminal theme into one theme

Round-2 (see `docs/plans/restyle/ROUND2-NOTES.md`; mock:
`docs/design/restyle-mocks.html` "Settings · Appearance" section — the single
merged theme grid). Depends on nothing in 04; plan 06 depends on THIS plan's
merged model. Runs second in the round-2 set.

## Goal

With the restyle there is barely any chrome left (inks, hairlines, one
translucent surface), so separate "Chrome" and "Terminal theme" settings no
longer make sense. Merge them: ONE theme per scheme drives both the terminal
colors and the chrome colors. The terminal catalog's 12 built-ins (plus
imported Ghostty themes) become the single theme list; chrome halves that
don't exist yet are derived procedurally from the terminal colors.

## Decisions (locked — do not re-litigate)

- **The merged theme id IS the terminal theme id.** The stored per-scheme
  selection keys stay `terminal_theme_light_id` / `terminal_theme_dark_id`
  (`crates/nice/src/theme_settings.rs` ~124-126) — no key rename, no store
  migration for the selection itself. The `chrome_light_palette` /
  `chrome_dark_palette` fields (~117-119) are REMOVED from `Appearance`;
  the decoder ignores the legacy keys on read and stops writing them.
- **Chrome-half resolution** for a merged theme id:
  - `nice-default-light` / `nice-default-dark` → the existing hand-tuned
    `NICE_LIGHT` / `NICE_DARK` palettes ("Nice pairs with Nice" — Nick).
  - `catppuccin-latte` / `catppuccin-mocha` → the existing hand-tuned
    `CatppuccinLatte` / `CatppuccinMocha` palettes.
  - EVERY other theme — the 8 built-ins without chrome halves
    (solarized-light, solarized-dark, dracula, nord, gruvbox-light,
    gruvbox-dark, tokyo-night, one-dark) AND all imported Ghostty custom
    themes (Nick's decision: imports derive) — gets chrome DERIVED from
    its terminal colors by a new derivation function in `nice-theme`:
    - terminal background → the chrome background/surface slots
      (`background2` = background nudged toward the fg by a small fixed
      ratio, matching the Nice palettes' bg↔bg2 relationship);
    - terminal foreground → `ink`; `ink2`/`ink3` = fg blended toward bg at
      fixed ratios REPLICATING the Nice ramp's contrast relationships
      (measure NICE_DARK/NICE_LIGHT's ink:ink2:ink3 blend factors and reuse
      them — do not invent new ratios);
    - hairlines/fills keep the existing scheme-scoped glass treatment
      (they are alpha-over-surface, not palette slots);
    - the accent slot is NOT derived — the user's accent setting stays
      independent and unchanged.
    Enumerate and cover every slot the `Palette`/chrome-theme API exposes
    (`crates/nice-theme/src/palette.rs`); no slot may fall back to a stale
    Catppuccin/Nice constant for a derived theme.
  - One derivation function serves built-ins and imports alike; per-theme
    hand-tuned overrides MAY be added later if a derived palette looks off
    (not this plan — note it in the code).
- **`Palette::MacOs` retires.** It is already excluded from the settings
  picker (`chrome_palettes_for` filters it, `appearance_pane.rs` ~216-222),
  so no user-visible option disappears. Remove the variant and its system-
  semantic-colors plumbing; a stored legacy `"macOS"` chrome key decodes as
  Nice (moot anyway — chrome keys are ignored after the merge).
- **Migration rule: the terminal theme wins.** An existing store's merged
  selection = its stored terminal theme ids, so every user's TERMINAL looks
  exactly as before. Users who had a mismatched chrome (e.g. Catppuccin
  chrome + Dracula terminal) get Dracula-derived chrome — accepted; the
  terminal is the dominant surface.
- **Scheme scoping**: each scheme's picker lists the themes valid for that
  scheme exactly as the terminal catalog scopes them today (universal
  themes appear in both; light-only in Light; dark-only… usable per the
  existing scope rules — the mock's caption reflects this).
- **Settings UI in THIS plan**: collapse the two dropdowns ("Chrome",
  "Terminal theme") in each scheme section of the appearance pane into ONE
  "Theme" dropdown, in the pane's EXISTING layout. The full pane regroup
  (Light/Dark tabs, theme grid) is plan 06 — do not start it here.
- Fresh-install defaults unchanged: `nice-default-light` / `-dark`.

## Scope / key files

- `crates/nice-theme/src/palette.rs` + a new derivation module — derived
  chrome palettes; MacOs removal.
- `crates/nice/src/theme_settings.rs` — `Appearance` field removal, decode
  compat, chrome resolution now keyed off the merged theme id, fanout
  unchanged in shape.
- `crates/nice/src/settings/appearance_pane.rs` — single Theme dropdown per
  scheme; `chrome_palettes_for` and its tests retire.
- `crates/nice/src/terminal_theme_catalog.rs` /
  `built_in_terminal_themes.rs` — the catalog is now the theme registry for
  both halves (imports included).
- Chrome-slot consumers (toolbar, sidebar, app_shell, settings) —
  unchanged: the slot API stays stable; only where slot VALUES come from
  changes.

## Non-goals

- No settings-window restyle or pane regrouping (plan 06).
- No popup/migration-of-defaults changes (plan 06).
- No per-theme hand-tuned chrome for the 8 derived built-ins (derivation
  only, this plan).
- No accent changes; no new themes beyond the existing catalog.

## Known interactions

- The theme fanout co-owner discipline (`ThemeStore` owns the appearance
  section) — field removal must respect the read-merge-write rules
  documented in `theme_settings.rs` / `prefs_store.rs`.
- The vendored bg-luminance patch composites glyphs against the effective
  background — derived backgrounds flow through the same path as today
  (same code path as any terminal theme change; no special handling).
- Scenario/self-tests that assert specific palette values (chrome_live,
  theme_fanout_live, appearance-pane tests) need updating where they
  reference the removed chrome-palette selection.

## Validation

- `cargo test --workspace`:
  - derivation unit tests: for each of the 8 derived built-ins, the ink
    ramp is legible over the derived surface (contrast-ratio assertions
    ink/ink2/ink3 vs background — pin the minimum ratios from what
    NICE_DARK/NICE_LIGHT achieve) and no slot equals an unrelated
    palette's constant;
  - migration tests: legacy store with (chrome=CatppuccinMocha,
    terminal=dracula) resolves to Dracula-everything with the terminal
    colors byte-identical to before; legacy `"macOS"` chrome key decodes
    without error; selection keys round-trip unchanged.
- Black-box (worktree lock, dev install, scratch env, `caffeinate -d`):
  - switch the merged theme to Dracula (dark tab): terminal colors AND
    chrome (sidebar inks, window surface tint) change together; repeat
    for a light theme (Solarized Light) on the light scheme;
  - import a Ghostty theme file: it appears in the theme list; selecting
    it re-skins the WHOLE window (chrome derived), not just the grid;
  - seed a scratch store mimicking an existing user with a mismatched
    pair: after launch the terminal renders pixel-identical to before the
    update (screenshot compare of the grid region); chrome follows the
    terminal theme.
