# Accent "From theme" + caret-follows-accent fix

Mock: `docs/design/accent-from-theme-mock.html` (the folded revision Nick
approved 2026-08-02 — split-disc entry, trailing position). Origin bug: with
OS-sync off, the caret used the active theme's cursor color instead of the
configured accent; investigation showed the caret has no coupling to sync at
all — any theme with `cursor: Some(...)` (every non-Nice built-in + most
imports) overrides the accent at `crates/nice-term-view/src/element.rs`
~915. Nick's resolution: presets must always win, and a NEW sixth accent
entry opts back into theme-driven color deliberately.

## Goal

1. **Fix:** when a preset accent (Terracotta…Graphite) is selected, the
   caret uses it — always. A theme's cursor color no longer overrides a
   preset.
2. **Feature:** a sixth Accent-row entry, **"From theme"**, meaning "derive
   the accent from the active terminal theme." Selecting it makes the
   accent (caret, selection tint, chrome, logo) follow whatever theme is
   active per scheme.

## Decisions (locked — do not re-litigate)

- **`TerminalTheme` gains `accent: Option<TerminalColor>`**
  (`crates/nice-term-view/src/theme.rs` ~55-69, alongside
  `cursor`/`selection`). Exactly this one slot — no generalized "roles"
  system (YAGNI). The renderer does NOT read it; only the resolve layer
  (`ThemeState`) does.
- **Accent resolution chain under "From theme"** (Nick, 2026-08-02, after
  comparing the Warp Catppuccin file's `accent: #b4befe` against the
  canonical cursor `#f5e0dc`):
  1. `theme.accent` when `Some` (built-ins: hand-curated below; future
     file-declared accents, e.g. Warp import, land here);
  2. else **`ansi[4]` (normal blue)** — the slot theme authors treat as the
     primary hue, saturated by design.
  **`cursor` and `foreground` are deliberately NOT in this chain** — they
  are near-monochrome by design (must contrast with everything) and make
  washed-out chrome. Evidence: Warp's own curators pointed cursor AT their
  accent, not the reverse.
- **Caret rule:**
  - Preset selected → caret = preset color. Implemented by CLEARING
    `cursor` to `None` on the resolved `TerminalTheme` in
    `ThemeState::from_stores` (`crates/nice/src/theme_settings.rs` ~635)
    before fan-out — `element.rs`'s existing `match theme.cursor` then does
    the right thing with **zero changes to `nice-term-view`'s paint path**.
  - "From theme" → caret = `theme.cursor` when `Some` (the theme author's
    literal caret choice), else the resolved theme accent. Implemented by
    LEAVING `cursor` as-is on fan-out.
- **Selection setting shape:** the persisted accent becomes a two-variant
  selection — `Preset(AccentPreset)` | `FromTheme` — persisted rawValue
  `"from-theme"` next to the five existing preset rawValues
  (`theme_settings.rs` accent key, ~312/342/372). Tolerant decode: unknown
  → default Terracotta (existing discipline). `AccentPreset` itself is
  untouched (it stays the five colors; "From theme" is a selection kind,
  not a preset).
- **Under "From theme" the accent is scheme-dependent** (it re-derives from
  the active scheme's theme on every scheme flip / theme change) — this
  falls out of `ThemeState::from_stores` re-running on each commit; no new
  fan-out machinery.
- **Curated built-in accents** (subject to Nick's feel-check; the point is
  each theme's identity hue, not its cursor):

  | id | accent |
  |---|---|
  | nice-default-light / nice-default-dark | `#c96442` (terracotta) |
  | solarized-light / solarized-dark | `#268bd2` |
  | dracula | `#bd93f9` |
  | nord | `#88c0d0` |
  | gruvbox-light | `#d65d0e` |
  | gruvbox-dark | `#fe8019` |
  | catppuccin-latte | `#7287fd` |
  | catppuccin-mocha | `#b4befe` (matches Warp's curated pick) |
  | tokyo-night | `#7aa2f7` |
  | one-dark | `#61afef` |

- **Ghostty imports:** parser unchanged; imported themes get
  `accent: None` → resolve falls to `ansi[4]`. New import formats
  (iTerm2, Warp) are a SEPARATE follow-up plan — do not start them here.
- **Settings UI** (match the mock exactly; both schemes):
  - The entry TRAILS the five presets, separated by a 1px
    `hairline-strong` divider (16px tall).
  - It renders as a **16px disc split at 135°**: upper-left half = the
    accent the LIGHT-scheme slot's theme derives, lower-right = the
    DARK-scheme slot's (resolve `terminal_theme_light_id`/`_dark_id`
    through the catalog + the chain above). 1px seam in the window surface
    color between the halves so it reads as two chips, not a gradient.
  - Selection grammar unchanged: same 2px-padding cell + 1px ink ring,
    footprint reserved (no layout shift). A11y id
    `settings.appearance.accent.from-theme`, label "From theme".
  - **Hint line** under the Accent row, only while "From theme" is
    selected: 11px `ink3`, right-aligned — "Accent follows the theme — now
    <display name>" with a 9px swatch of the effective accent. Names the
    active scheme's source theme.
- The status-dot accent (`nice-term-view/src/view.rs` ~1884) and every
  other `active_chrome_accent` consumer keep reading the resolved accent —
  they follow "From theme" automatically; no per-consumer work.
- Claude theme-sync mirror (`--settings` provider) is scheme-driven; verify
  it does not persist/echo the accent rawValue anywhere it would choke on
  `"from-theme"`, but do not extend it.

## Scope / key files

- `crates/nice-term-view/src/theme.rs` — the `accent` field (+ the two Nice
  defaults set it to terracotta).
- `crates/nice/src/built_in_terminal_themes.rs` — curated accent per
  built-in (table above) + fixture tests.
- `crates/nice/src/ghostty_theme_parser.rs` — construct with
  `accent: None` (no key parsing).
- `crates/nice/src/theme_settings.rs` — the selection enum, persistence
  rawValue, `apply_accent` signature, `ThemeState::from_stores` effective
  (theme, accent) resolution incl. the cursor-clearing rule, a
  `derived_accent_for(scheme)` helper for the pane's split disc.
- `crates/nice/src/settings/appearance_pane.rs` — `accent_control` grows
  the divider + split-disc entry + hint line; control-contract tests (the
  `SchemeSegment` precedent: testable without a window).

## Natural seams (suggested slices)

1. **Model + resolution** — theme field, curated accents, selection enum,
   persistence, `ThemeState` resolution (cursor-clearing + chain), unit
   tests. This slice alone fixes the origin bug and is shippable.
2. **Settings UI** — split-disc entry, divider, hint line, contract tests.

## Validation

- `cargo test --workspace` (targeted during fix rounds — the touched
  modules only, per the standing rule):
  - resolution tests: preset × cursor-bearing theme → fanned-out theme has
    `cursor: None` and accent = preset; from-theme × Dracula → caret color
    = `#f8f8f2`, accent = `#bd93f9`; from-theme × an imported theme
    (accent None) → accent = its `ansi[4]`; from-theme × Nice dark → caret
    = terracotta (cursor None falls to accent).
  - persistence: `"from-theme"` round-trips; unknown rawValue → Terracotta;
    legacy stores decode unchanged.
  - built-in fixture tests: every catalog theme's `accent` matches the
    curated table.
  - pane contract tests: six entries render, from-theme trails behind the
    divider, click → `apply_accent(FromTheme)`, hint only when selected.
- Black-box (worktree lock held through install+test; scratch-env
  dev-bundle launch; `caffeinate -d`):
  - **Origin bug:** sync OFF, accent = Ocean, theme = Dracula → pixel-
    sample the caret cell: Ocean `#3b82f6`, NOT `#f8f8f2`.
  - "From theme" + Dracula → caret `#f8f8f2` (theme cursor), chrome accent
    purple `#bd93f9` (nav/toggle/underline).
  - "From theme" + Nice dark → caret + chrome terracotta.
  - Scheme flip under "From theme" → accent follows the other scheme's
    theme without relaunch.
  - Relaunch → "From theme" selection persists.
  - Accent row shows the split disc + divider on BOTH schemes (screenshot
    each).
