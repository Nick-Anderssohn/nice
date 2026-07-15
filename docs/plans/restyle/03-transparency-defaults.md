# Restyle 3/3 — Transparency, blur, line-height, new defaults + one-time popup

Part of the 2026-07 restyle (approved mock: `docs/design/restyle-mocks.html`
— its Opacity/Blur/Scheme controls demonstrate exactly the behavior this plan
ships). Depends on plans 1–2 (unified fill-less surface; flat sidebar).

## Goal

Make the whole window optionally translucent with a real window blur, both
tunable per color scheme; add a terminal line-height setting; flip the
first-install default appearance from Catppuccin to the Nice defaults; and
offer existing users the new look once via a one-time popup.

## Decisions (locked — do not re-litigate)

### Settings

New persisted appearance settings, surfaced in the appearance pane.
**Store ownership matters** — the settings file has two co-owners with a
read-merge-write discipline (documented in both files):

- **Opacity and blur (per-scheme) + the popup flag** are appearance-shaped
  data: they live in the `appearance` section owned by `ThemeStore` in
  `crates/nice/src/theme_settings.rs` and live-apply through ITS existing
  theme fanout. Do NOT bolt them (or new fanout wiring) onto `PrefsStore`.
- **Line-height** lives beside the font keys in
  `crates/nice/src/settings/prefs_store.rs` and fans out via `FontState`
  (not the theme fanout).
- The migration's pinning step (below) writes appearance keys
  (palette/accent/theme-ids/…) and therefore goes through `ThemeStore`.

The settings:

- **Window opacity, per scheme** (two stored values, dark + light).
  Slider range 55–100%. New-install defaults: **dark 80%, light 90%**
  (light goes milky at 80 — judged interactively during design; the mock
  ships a single 80% slider default, so the 90-for-light value is recorded
  HERE, not in the mock — don't "correct" it to 80).
- **Background blur radius, per scheme**. Slider 0–60px. Default **30**
  for both schemes.
- **Terminal line-height multiplier** (scheme-independent). Slider
  1.0–1.8, step small (e.g. 0.05). New-install default **1.3**.
  Applied in `nice-term-view::font` where cell metrics derive from the font
  (`FontState` — cell height multiplies; width untouched). Distribute the
  added height **half above / half below the glyph** (re-derive the baseline
  in `cell_metrics`) — otherwise text rides the cell top. Grid geometry
  (rows-per-window, selection rects, scrollback math) must follow the cell
  height, not the raw font metrics.
  **TUI-correctness checklist** (from a survey of how Kitty/Ghostty/
  WezTerm/foot/VS Code handle line height — every known gap bug in the
  wild comes from missing one of these; the single source of truth is the
  final post-multiplier cell height):
  - Round the CELL height once (`round(font_height * multiplier)`) and
    compute every rect below from it — never round per-glyph.
  - Box-drawing/block sprites (`nice-term-view/src/boxdraw.rs`,
    U+2500–259F) are drawn procedurally to the CELL box — verify they draw
    to the new padded cell height, not the raw font height, so TUI borders
    stay continuous at multipliers > 1.0.
  - **Cell BACKGROUND and selection rects must fill the ENTIRE taller
    cell** edge-to-edge with adjacent rows — if the sprite fills the cell
    but the bg doesn't, colored TUI panels show horizontal stripes
    between rows (this exact regression hit iTerm 3.5).
  - Cursor: block/beam cursor stays FONT-height (centered on the glyph),
    not stretched to the tall cell — a full-cell block cursor reads as a
    slab at 1.3+.
  - Underline/strikethrough positions are recomputed from the new cell
    metrics/baseline so they stay inside the cell at every multiplier.
  - Stroke thickness of box glyphs stays decoupled from cell HEIGHT
    (boxdraw derives from font/width metrics today — keep it that way so
    vertical rules don't fatten at 1.3).
  - Do NOT stretch other glyph classes to the cell: pictographic symbols
    (arrows, Nerd Font icons, emoji) keep their natural size, centered.
    Known accepted limitation: powerline (U+E0B0–) and braille (U+2800–)
    glyphs come from the FONT in Nice (boxdraw covers only U+2500–259F),
    so powerline status-line separators may not reach the full cell
    height at multipliers > 1.0 — accept for this plan (do NOT add new
    procedural renderers); note it as a follow-up if visible in
    validation.
  - Unit/golden test: rows of `│` and `█` (and a run of colored-bg
    cells) stacked at multipliers 1.0 / 1.3 / 1.8 → assert zero gap
    pixels between rows and no background stripes.
- The appearance pane sliders edit the ACTIVE scheme's opacity/blur values
  (label them with the scheme, e.g. "Opacity (Dark)"), consistent with how
  scheme-synced settings behave elsewhere.

### Rendering

- Opacity applies to **surface fills only** — the shared window-body /
  terminal background (and any remaining chrome fills) get the alpha; text,
  status dots, hairlines, selection/cursor stay fully opaque on top.
  Within the terminal grid, ONLY the default-background surface gets the
  alpha: cells with an explicit background color (TUI panels, highlights,
  selection) stay opaque. The mock's model: a single
  `background = rgba(theme bg, opacity)` surface over the OS blur.
- `app_shell.rs` backing colors: the remaining opaque body backing
  (`terminal_backing_color`, which paints the theme background at implicit
  alpha 1.0 — `backing_band_color`/`window_backing_band` were already
  deleted by plan 1) is reworked — the window is genuinely non-opaque now;
  gpui's `WindowBackgroundAppearance` handles the NSWindow/Metal-layer
  plumbing.
- Appearance selection: opacity < 100% && blur > 0 → `Blurred`;
  opacity < 100% && blur == 0 → `Transparent`; opacity == 100% → `Opaque`.
  React to slider changes at runtime via
  `Window::set_background_appearance`.
- **Vendored gpui patch — configurable blur radius.** CRITICAL premise:
  in `vendor/zed/crates/gpui_macos/src/window.rs`, the `blur_radius = 80` /
  `CGSSetWindowBackgroundBlurRadius` arm runs ONLY on pre-Monterey macOS
  (`NSAppKitVersionNumber < NSAppKitVersionNumber12_0`) — on every macOS
  Nice targets, `Blurred` instead inserts an `NSVisualEffectView` subclass
  (`BLURRED_VIEW_CLASS`, fixed system material, NO numeric radius). Merely
  parameterizing the 80 changes nothing on modern macOS. The patch must
  give the MODERN path a numeric radius: route `Blurred` through
  `CGSSetWindowBackgroundBlurRadius` on ALL macOS versions (replacing /
  bypassing the effect view), exposed as e.g. a
  `set_background_blur_radius(u32)` platform-window method or a
  radius-carrying appearance variant. Add it as a new patch in the
  `scripts/vendor-zed.sh` pipeline (alongside `zed-bg-luminance.patch`);
  keep it minimal and additive; document it next to the existing patch.

### Default appearance flip (fresh installs)

- Default chrome palette: **Nice** (was Catppuccin) — flip the two fields
  in `Appearance::default()` (`theme_settings.rs`):
  `chrome_light_palette: Palette::CatppuccinLatte` → Nice light,
  `chrome_dark_palette: Palette::CatppuccinMocha` → Nice dark. OS scheme
  sync stays default-ON.
- Accent default: **flip Ocean → Terracotta** in `Appearance::default()`
  (`theme_settings.rs`). The code default TODAY is `AccentPreset::Ocean`
  (Terracotta is only the no-theme-state fallback in
  `active_chrome_accent`); the approved mock renders Terracotta throughout,
  so the shipped default must actually change — "stays Terracotta" would be
  a no-op that ships Ocean.
- Default terminal themes: flip `DEFAULT_TERMINAL_THEME_LIGHT_ID` /
  `DEFAULT_TERMINAL_THEME_DARK_ID` (`theme_settings.rs`) from
  `"catppuccin-latte"` / `"catppuccin-mocha"` to the Nice catalog ids
  **`"nice-default-light"` / `"nice-default-dark"`** (as registered in
  `built_in_terminal_themes.rs`).
- Plus the new-setting defaults above (80/90 opacity, blur 30, line-height
  1.3). "New defaults" below means ALL of: palette Nice + OS-sync on +
  Terracotta + nice-default terminal themes + those slider values.

### Migration + one-time popup (existing users)

- On startup, when a one-time flag (e.g. `restyle_popup_shown`) is absent
  from the store:
  1. **If the user is an EXISTING user** — detected belt-and-braces: a
     prior settings file exists OR the app's Application Support directory
     / session store already exists (the settings file alone can miss a
     user who never touched settings; they must not be silently restyled) —
     first materialize their current look into explicit stored keys
     wherever a key is absent, by writing the **LEGACY defaults as literal
     values**: palettes `CatppuccinLatte`/`CatppuccinMocha`, terminal theme
     ids `"catppuccin-latte"`/`"catppuccin-mocha"`, `sync_with_os = true`,
     accent `Ocean` (the pre-flip default), `opacity=100%` both schemes,
     `blur=0`, `line_height=1.0`. **NEVER derive these from
     `Appearance::default()` or the `DEFAULT_TERMINAL_THEME_*` constants —
     this very plan flips those to the NEW defaults, so resolving through
     them would pin the restyle onto exactly the users this step protects.**
     Keys the user has explicitly set are left untouched. This pins their
     exact current look so declining changes nothing — without this, the
     defaults-flip would silently restyle anyone who was riding defaults,
     and the new line-height default would change their grid.
     Unit test: decode a legacy store with absent keys through
     pin-then-resolve and assert equality with the PRE-flip resolution.
  2. Show the popup (existing confirmation-modal pattern,
     `crates/nice/src/confirmation_modal.rs`): offer the new look.
     Copy sense: "Nice has a new default look — transparent, blurred, and
     restyled. Try it?" [Try the new look] [Keep my setup].
  3. **Yes** → write the new defaults for every appearance setting EXCEPT
     the terminal font family and font size, which are NEVER touched (Nick's
     explicit carve-out). Line-height IS set to 1.3 on Yes.
     **No** → change nothing beyond the pinning in step 1.
  4. Set the flag either way; the popup never shows again.
- Popup shows to everyone without the flag (no "are they on defaults"
  detection — deliberately simple). A genuinely fresh install (neither a
  settings file nor an Application Support presence): nothing to pin, and
  it's already on the new defaults, so either answer is a no-op; showing
  the popup there too is acceptable, or skip it when no prior presence
  existed — implementer's choice, but no more gating logic than that.
- Scope note: "Keep my setup" preserves the SETTINGS axes only (palette /
  themes / accent / opacity / blur / line-height). Plans 1–2's structural
  restyle (28pt bar, flat sidebar) ships for everyone regardless — do not
  attempt (or validate for) a cross-version zero-diff.

## Non-goals

- No per-terminal-theme opacity overrides; the two per-scheme values are
  global.
- No liquid-glass / intra-window backdrop effects (explicitly rejected).
- No font family/size default changes (SF Mono 13px chain stays).
- Settings window, modals, popovers stay opaque; only the main terminal
  window body is translucent.

## Known interactions

- Blur radius via `CGSSetWindowBackgroundBlurRadius` is a PRIVATE API
  (gpui links it today only for its pre-Monterey path — the vendor patch
  extends it to modern macOS); radius 0 with opacity < 100 must degrade to
  plain transparency, matching the mock's blur=0 behavior.
- The `chrome` slot (`CHROME_OPACITY` 0.70 translucent chrome) predates
  this; after plans 1–2 its fills are gone from the main window (the peek
  overlay moved to an opaque theme-background fill in plan 2). Ensure no
  surviving chrome-slot fill double-applies alpha over the translucent
  surface.
- The vendored `zed-bg-luminance.patch` does background-aware glyph
  composition; at 55–80% surface alpha the effective background behind
  glyphs is no longer the theme color — watch for composition artifacts,
  not just alpha (see the legibility check in Validation).
- Screen-recording/screenshot tests: translucency makes pixel assertions
  environment-dependent — scenario/pixel tests should pin opacity 100% (or
  a scratch prefs default) unless specifically testing transparency.

## Validation

- `cargo test --workspace`: prefs round-trip for the new keys, migration
  pinning logic (absent-key materialization writes the LEGACY literals —
  assert pin-then-resolve of a legacy store equals the pre-flip
  resolution), default flips (palette/accent/theme ids), cell-metric math
  under line-height multipliers (including baseline distribution).
- Black-box under the worktree lock (dev install; scratch-env direct bundle
  launch so the LIVE store is never touched; `caffeinate -d`):
  - fresh scratch store → new defaults active (Nice palette, Terracotta
    accent, nice-default theme, 80%/30 dark), window visibly
    translucent+blurred over a bright desktop; text remains fully opaque;
  - sliders live-apply: opacity to 100 → opaque window; blur to 0 →
    transparent-not-blurred; **two distinct nonzero radii (e.g. 15 vs 60)
    are VISIBLY different over high-frequency wallpaper content** (this is
    the only check that exercises the vendor patch — the blur-0 check
    passes via the appearance switch even with a dead radius knob);
    per-scheme values independent (flip OS appearance / scheme and verify
    the other pair applies);
  - text legibility at minimum opacity (55%) over a bright, busy wallpaper
    on both schemes — the bg-luminance glyph composition must not produce
    artifacts (the risk is composition, not alpha);
  - line-height slider changes row pitch live; TUI apps (run `htop` or
    Claude Code) still render their boxes correctly at 1.0 and 1.3 —
    check specifically: no gaps in vertical box lines, no horizontal
    stripes across colored panel backgrounds, cursor not stretched to
    the tall cell, underline still inside the cell;
  - migration: seed a scratch store mimicking an existing user (some keys
    absent), launch → popup shows once; "Keep my setup" → pixel-identical
    look afterwards (compare before/after screenshots), grid unchanged
    (line-height pinned 1.0); relaunch → no popup. Reset scratch, choose
    "Try the new look" with a custom font family/size set → new look
    applied, font family/size preserved; relaunch → no popup.
