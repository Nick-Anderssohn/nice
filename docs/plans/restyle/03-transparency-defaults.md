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

### Settings (all live-apply through the existing theme fanout)

New persisted appearance settings in the prefs store
(`crates/nice/src/settings/prefs_store.rs`), surfaced in the appearance pane:

- **Window opacity, per scheme** (two stored values, dark + light).
  Slider range 55–100%. New-install defaults: **dark 80%, light 90%**
  (light goes milky at 80 — judged in the mock).
- **Background blur radius, per scheme**. Slider 0–60px. Default **30**
  for both schemes.
- **Terminal line-height multiplier** (scheme-independent). Slider
  1.0–1.8, step small (e.g. 0.05). New-install default **1.3**.
  Applied in `nice-term-view::font` where cell metrics derive from the font
  (`FontState` — cell height multiplies; width untouched). Grid geometry
  (rows-per-window, selection rects, scrollback math) must follow the cell
  height, not the raw font metrics.
- The appearance pane sliders edit the ACTIVE scheme's opacity/blur values
  (label them with the scheme, e.g. "Opacity (Dark)"), consistent with how
  scheme-synced settings behave elsewhere.

### Rendering

- Opacity applies to **surface fills only** — the shared window-body /
  terminal background (and any remaining chrome fills) get the alpha; text,
  status dots, hairlines, selection/cursor stay fully opaque on top.
  The mock's model: `background = rgba(theme bg, opacity)` over the OS blur.
- `app_shell.rs` backing colors: the forced-opaque backing
  (`backing_band_color`'s alpha-force-1.0 rationale, `terminal_backing_color`)
  is reworked — the window is genuinely non-opaque now; gpui's
  `WindowBackgroundAppearance` handles the NSWindow/Metal-layer plumbing.
- Appearance selection: opacity < 100% && blur > 0 → `Blurred`;
  opacity < 100% && blur == 0 → `Transparent`; opacity == 100% → `Opaque`.
  React to slider changes at runtime via
  `Window::set_background_appearance`.
- **Vendored gpui patch — configurable blur radius**: `gpui_macos`
  hard-codes radius 80 when `Blurred` (see `vendor/zed/crates/gpui_macos/
  src/window.rs`, the `blur_radius = 80` arm). Add a new patch in the
  `scripts/vendor-zed.sh` pipeline (alongside `zed-bg-luminance.patch`)
  exposing a per-window blur radius (e.g. a
  `set_background_blur_radius(u32)` platform-window method or a
  radius-carrying appearance variant). Keep the patch minimal and additive;
  document it next to the existing patch.

### Default appearance flip (fresh installs)

- Default chrome palette: **Nice** (was Catppuccin); OS scheme sync stays
  default-ON; accent default stays Terracotta.
- Default terminal themes: **niceDefaultDark / niceDefaultLight** (the
  R21 Catppuccin default ids in `crates/nice/src/terminal_theme_catalog.rs`
  / `theme_settings.rs` flip to the Nice defaults).
- Plus the new-setting defaults above (80/90 opacity, blur 30, line-height
  1.3). "New defaults" below means ALL of: palette Nice + OS-sync on +
  Terracotta + niceDefault terminal themes + those slider values.

### Migration + one-time popup (existing users)

- On startup, when a one-time flag (e.g. `restyle_popup_shown`) is absent
  from the prefs store:
  1. **If a prior prefs store exists** (updating user): first materialize
     the CURRENT effective appearance into explicit stored keys wherever a
     key is absent — palette/scheme-sync/accent/terminal-theme ids as they
     resolve today, `opacity=100%` both schemes, `blur=0`, `line_height=1.0`.
     This pins their exact current look so declining changes nothing —
     without this, the defaults-flip would silently restyle anyone who was
     riding defaults, and the new line-height default would change their
     grid.
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
  detection — deliberately simple). A genuinely fresh install has no prior
  store: nothing to pin, and it's already on the new defaults, so either
  answer is a no-op; showing the popup there too is acceptable, or skip it
  when no prior store existed — implementer's choice, but no more gating
  logic than that.

## Non-goals

- No per-terminal-theme opacity overrides; the two per-scheme values are
  global.
- No liquid-glass / intra-window backdrop effects (explicitly rejected).
- No font family/size default changes (SF Mono 13px chain stays).
- Settings window, modals, popovers stay opaque; only the main terminal
  window body is translucent.

## Known interactions

- Blur radius is a private-API window property (gpui already uses it);
  radius 0 with opacity < 100 must degrade to plain transparency, matching
  the mock's blur=0 behavior.
- The `chrome` slot (`CHROME_OPACITY` 0.70 translucent chrome) predates
  this; ensure the new uniform surface doesn't double-apply alpha where
  that slot was used.
- Screen-recording/screenshot tests: translucency makes pixel assertions
  environment-dependent — scenario/pixel tests should pin opacity 100% (or
  a scratch prefs default) unless specifically testing transparency.

## Validation

- `cargo test --workspace`: prefs round-trip for the new keys, migration
  pinning logic (absent-key materialization), default-id flips, cell-metric
  math under line-height multipliers.
- Black-box under the worktree lock (dev install; scratch-env direct bundle
  launch so the LIVE store is never touched; `caffeinate -d`):
  - fresh scratch store → new defaults active (Nice palette, niceDefault
    theme, 80%/30 dark), window visibly translucent+blurred over a bright
    desktop; text remains fully opaque;
  - sliders live-apply: opacity to 100 → opaque window; blur to 0 →
    transparent-not-blurred; per-scheme values independent (flip OS
    appearance / scheme and verify the other pair applies);
  - line-height slider changes row pitch live; TUI apps (run `htop` or
    Claude Code) still render their boxes correctly at 1.0 and 1.3;
  - migration: seed a scratch store mimicking an existing user (some keys
    absent), launch → popup shows once; "Keep my setup" → pixel-identical
    look afterwards (compare before/after screenshots), grid unchanged
    (line-height pinned 1.0); relaunch → no popup. Reset scratch, choose
    "Try the new look" with a custom font family/size set → new look
    applied, font family/size preserved; relaunch → no popup.
