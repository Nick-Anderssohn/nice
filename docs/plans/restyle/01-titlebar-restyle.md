# Restyle 1/3 — Slim unified titlebar with text+underline tabs

Part of the 2026-07 restyle (see `docs/design/restyle-mocks.html` — the
approved mock is **Style A, 28pt bar**; open it in a browser, controls at the
top). Ordered plan set: this plan, then `02-sidebar-flatten.md`, then
`03-transparency-defaults.md`. This plan covers ONLY the titlebar band and the
tab strip; the sidebar stays a floating card until plan 2, and everything
stays fully opaque until plan 3.

## Goal

Replace today's 52pt toolbar band (opaque fill + 1px bottom rule + brand
block + pill tab strip) with a 28pt, **fill-less** titlebar: traffic lights at
their native macOS position, a sidebar-collapse toggle beside them, then tab
titles as plain text with a 2px accent underline marking the active tab.
The window becomes one continuous surface from its top edge — no band color,
no separator rule — and terminal content moves up close to the bar.

## Decisions (locked — do not re-litigate)

- Titlebar height: **28pt** (true macOS standard). `TOP_BAR_HEIGHT` 52 → 28.
- **No titlebar fill and no bottom rule.** The band paints nothing; the
  window-body background simply extends to the top. (`window_backing_band`
  in `crates/nice/src/app_shell.rs` — the band + its 1px `line` child — is
  removed, not restyled.)
- **Brand block: removed entirely.** No logo glyph replaces it.
- Tab style: **text + accent underline** (mock Style A):
  - Inactive tab: title text in `ink3`; active tab: `ink`.
  - Active tab gets a 2px underline in the user's chosen accent
    (`theme_settings::active_chrome_accent`), seated on the bar's bottom
    edge, spanning the tab's content width (inset ~11px from the tab's
    horizontal padding edges per the mock).
  - No pill fill, border, or rounding anywhere in the strip.
- Tab anatomy: `[status dot] [title] [✕ on hover]`, 12px-range mono text
  (the terminal font family — same family the terminal resolves via
  `nice-term-view::font`, NOT the UI sans), ~12px horizontal padding.
- **Status dots keep their exact colors and pulse animations** (the existing
  `StatusDot` component and `nice-theme/src/status.rs` tokens are reused
  untouched), but render at a smaller size in the strip: **6pt** (the
  component's size is already a parameter; 8pt stays the default elsewhere).
- Truncation: max tab width **200px**, middle-truncate or tail-ellipsize per
  existing pill behavior; full title available as a hover tooltip.
- Overflow: **keep the existing machinery** — `ScrollHandle` scroll, overflow
  chevron with attention badge + edge fades, auto-center-on-activate, and the
  trailing `+` button (all in `crates/nice/src/toolbar.rs`). Only the pill
  visuals change.
- Traffic lights: **native default placement**. At a 28pt bar the custom
  placer targeting (`TRAFFIC_LIGHT_CENTER_FROM_TOP` = 26 = 52/2, nudges) is
  obsolete — remove the repositioning so the OS default (which centers in a
  standard titlebar) applies. Delete/retire the placer constants rather than
  retuning them.
- **Sidebar-collapse toggle moves into the titlebar**, immediately right of
  the traffic lights (Finder/Safari position), BEFORE the tab strip. It
  renders the mock's EXACT stroke icon — NOT an SF Symbol — via gpui's
  `svg()` element (the vendored gpui ships one; add a minimal embedded
  `AssetSource` for a new chrome icon set). The icon (verbatim from
  `docs/design/restyle-mocks.html`, `.tb-btn`): 15×12 viewBox, 1px stroke,
  `<rect x=.5 y=.5 w=14 h=11 rx=2.5>` + vertical line at x=5.5; tinted
  `ink3`, hover `ink` + faint fill. It toggles the existing collapsed state
  in `WindowState`.
  It is present in BOTH collapsed and expanded states — this replaces the
  collapsed-mode restore button that lives in the old 52pt band
  (`build_collapsed_shell` in `crates/nice/src/sidebar_shell.rs`).
- Terminal content moves up: terminal pane content begins ~8px below the
  bar, with ~16px left/right padding (per the mock's breathing room).
- Titlebar remains the window drag region (drag-to-move + double-click-zoom,
  the current R9 band behaviors) across its full width at the new height,
  with tab/button hitboxes consuming their own presses as today.

## Scope / key files

- `crates/nice-theme/src/chrome_geometry.rs` — `TOP_BAR_HEIGHT` 28.0; retire
  traffic-light placer constants (keep `TRAFFIC_LIGHT_CLUSTER_WIDTH` /
  reserved-width helper if the collapse button layout needs the reservation);
  update the provenance tests: these constants no longer cite Swift parity —
  cite `docs/design/restyle-mocks.html` + this plan instead.
- `crates/nice/src/app_shell.rs` — remove `window_backing_band` (band +
  rule); the body backing (`terminal_backing_color`) now covers the full
  window height.
- `crates/nice/src/toolbar.rs` — brand block out; pill → text+underline tab
  rendering; collapse toggle at leading edge; overflow/scroll logic intact.
- `crates/nice/src/pane_strip_live.rs` — self-test scenario geometry follows
  `TOP_BAR_HEIGHT` (band_y etc.); update fixtures/assertions to the new
  height and visuals.
- `crates/nice/src/sidebar_shell.rs` — collapsed shell: drop the in-band
  restore button (titlebar toggle covers it). The sidebar's own top strip
  otherwise remains as-is this plan (it shrinks with `TOP_BAR_HEIGHT`; its
  mode buttons stay until plan 2 relocates them; its collapse button is
  removed now so the control isn't duplicated).
- Wherever the traffic-light placer runs (window setup — `window_frame.rs` /
  platform hooks): stop repositioning.
- New module for the chrome icon set (e.g. `crates/nice/src/chrome_icons.rs`
  + embedded SVG assets and the gpui `AssetSource` registration in app
  setup): the restyle's stroke icons, replacing SF Symbols for the chrome
  controls this plan and plan 2 touch (the mock SVGs are the source of
  record). SF-symbol rendering (`sf_symbols.rs`) stays for any surfaces the
  restyle doesn't cover.

## Non-goals (later plans)

- Sidebar card removal / flat sidebar styling / footer mode switcher → plan 2.
- Any transparency, blur, opacity settings, or theme-default changes → plan 3.
- No changes to StatusDot colors/animations, terminal rendering, or fonts.

## Known interactions

- The thinking dot is hard-coded brand Terracotta by design; on the default
  Terracotta accent it matches the active tab's underline color. Accepted.
- Tab hit targets shrink with the bar; keep the full 28pt height clickable
  per tab.

## Validation

- `cargo test --workspace` for the geometry/token/provenance tests and
  toolbar pure-logic tests (overflow, truncation, center-offset unchanged).
- Update + run the pane-strip and chrome scenario self-tests
  (`pane_strip_live`, related `*_live` scenarios) for the new height/visuals.
- Black-box (required — scenario-green alone is not sufficient, per repo
  practice): under the worktree lock, `scripts/rust-install.sh`, launch the
  installed `Nice Dev` bundle binary directly with scratch `HOME` /
  `NICE_APPLICATION_SUPPORT_ROOT` / `NICE_PROD_SETTINGS_DOMAIN`, keep the
  display awake (`caffeinate -d`), and screenshot-verify:
  - bar is 28pt, no fill/rule, no brand block;
  - traffic lights at native position; collapse toggle beside them works in
    both collapsed and expanded states (real synthetic clicks — global HID,
    app activated, never pid-posted);
  - active-tab underline in the accent; inactive tabs `ink3`; long titles
    ellipsize at 200px; overflow chevron + scroll still work with 8+ tabs;
  - status dots pulse (thinking/waiting) at the smaller size;
  - drag-to-move and double-click-zoom on the bar still work.
