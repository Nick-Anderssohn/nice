# Bundled Nerd Font symbol fallback (GH issue #6)

**Status:** IMPLEMENTED on `worktree-gh-issue-6`, in PR. Fable-reviewed once
before implementation: 1 blocking, 5 important, and 10 nits, all folded below.
See § As shipped for where the code differs from this plan.

## As shipped

- **Fallback check.** `shape_cell_text` (`element.rs`) asks
  `TextSystem::advance(base_font, ch)`, not a shaped run's `font_id`. GPUI keys
  run font ids by PostScript name, and `load_family` overwrites that map on
  every family load, so a `font_id` comparison could misfire. `advance` uses
  font_kit's `glyph_for_char`, which calls `CTFontGetGlyphsForCharacters` on
  the base font only, never its cascade.
- **Fallback plumbing.** `paint_glyph_run` keeps its own `Font` literal
  (it carries per-run features), so both paint sites share
  `font::cell_fallbacks(symbol)` instead of one `cell_font` construction.
- **Validation done** (Nice Dev, scratch env, lock held):
  - Baseline 0.54.0 with Menlo: every PUA glyph is tofu, plain and bold.
  - Branch with Menlo: all glyphs render, plain and bold, inside their cells,
    on Retina and on the 1× C34J79x. ⚡/♥, box drawing, and ASCII unchanged.
    No registration warning; `$TMPDIR/nice-fonts/` file written.
  - Branch with MesloLGS Nerd Font Mono: patched icons keep their designed
    size (no fallback shrink).
- **Skipped** (Nick's call): step 5's Claude Code status line, and step 8's
  registration-failure run.
- **Not addressed:** powerline separators are shorter than their segments at
  1.3× line height (font glyphs draw at natural height). Pre-existing for
  patched fonts too. Follow-up: Fleet backlog item
  `nice/procedural-powerline-separators` (draw them procedurally for every
  font, like kitty and Ghostty).
**Issue:** https://github.com/Nick-Anderssohn/nice/issues/6 (reported by Jesse Rathbun).
**Lands via:** a PR from `worktree-gh-issue-6` into `main` whose body says
`Closes #6`. This is a one-off for this task; the usual flow pushes straight
to main.

**Goal:** powerline and Nerd Font icon glyphs (Private Use Area, e.g.
U+E0A0–U+E0D7, U+F000+) render in the terminal with ANY terminal font, sized
to the cell, with nothing for the user to install.

**Approach:**
- Ship Symbols Nerd Font Mono inside the Nice binary.
- Register it with CoreText for this process at launch.
- For PUA cells only, shape with that family as a fallback.
- When the glyph actually comes from the fallback font, re-shape it at a size
  that fits one cell.

## Current-code facts the plan builds on

- Terminal text is shaped by GPUI's CoreText path (`shape_line`). Every
  terminal `Font` passes `fallbacks: None`:
  - `crates/nice-term-view/src/element.rs:1598` — `paint_glyph_run` (`:1568`)
  - `crates/nice-term-view/src/element.rs:1736` — `cell_font` (callers
    `:1406` preedit, `:1662` cursor glyph)
  - `crates/nice-term-view/src/view.rs:1947` — `term_font` (caller `:1917`)
  - `crates/nice-term-view/src/font.rs:362` — `cell_metrics`
- With `fallbacks: None`, `apply_features_and_fallbacks`
  (`vendor/zed/crates/gpui_macos/src/open_type.rs:42`) sets no cascade list.
  CoreText then uses its default language cascade, which never includes a
  symbols font for PUA codepoints. Jesse confirmed installing Symbols Nerd
  Font system-wide did not help.
- With fallbacks set, GPUI builds a `kCTFontCascadeListAttribute` of
  descriptors by family name **plus the base face's weight/slant traits**
  (`open_type.rs:102-151`). User entries come first, then the system cascade.
  Fallbacks are part of the font-id cache key (`gpui_macos/src/text_system.rs:60-64`, `:142-146`).
- GPUI's `add_fonts` (`gpui_macos/src/text_system.rs:256`) only feeds
  font_kit's in-memory source. CoreText cannot see those fonts, so
  registration must go through `CTFontManager`.
- **Single-cell runs already exist.** Every non-ASCII cell gets its own
  `GlyphRun::single` (`element.rs:524-531`; test
  `non_ascii_is_isolated_per_cell`). So `paint_glyph_run` sees a PUA
  character alone, and `paint_cursor_glyph` (`:1633`, shaping `:1668-1670`)
  shapes one character.
- `shape_line` returns a `ShapedLine`, which derefs to `LineLayout` with public
  `runs[].font_id` (`vendor/zed/crates/gpui/src/text_system/line_layout.rs:16-37`,
  `line.rs:43-46`). `window.text_system()` derefs to `TextSystem::resolve_font`
  (`text_system.rs:148`, `:363-367`). Together these tell us whether a glyph
  came from the base font or from a fallback.
- The force-width patch only moves glyph positions onto the grid
  (`line_layout.rs:787-815`). It does not scale ink. `paint_line` centres the
  base font's ascent+descent box in the cell height (`line.rs:353-354`).
- The renderer draws U+2500–U+259F procedurally (`boxdraw.rs:75-76`). There is
  no overlap with PUA.
- Raw CoreText / CoreGraphics `extern "C"` FFI lives in
  `crates/nice/src/platform.rs` (e.g. `:347-360`). `nice-term-view` stays
  objc2-free and FFI-light (`crates/nice-term-view/Cargo.toml:13-19`).
- Startup: `app::run()` (`app.rs:1086`) and `app::run_selftest()` (`:4424`)
  both call `platform::disable_font_smoothing()` before
  `gpui_platform::application()`. The GPUI text system is created inside
  `application()`, so registering before it precedes all font resolution.
  `session_store::support_root()` is resolved in `app::run` only, never in
  selftests (`session_store.rs:487-500`).
- Log convention is `eprintln!("nice: ...")` (e.g. `app.rs:471`).

### Font facts (from parsing the TTF tables)

- `SymbolsNerdFontMono-Regular.ttf` from Nerd Fonts v3.5.1
  `NerdFontsSymbolsOnly.zip` is 2,610,012 bytes. The zip also holds the
  non-Mono TTF and a fontconfig `.conf`; only the Mono TTF is vendored.
- Family (name IDs 1/4) is `Symbols Nerd Font Mono`. PostScript name is
  `SymbolsNFM`. There is one Regular face (weight class 400).
- **Every glyph advances exactly 1.000 em** (upem 2048, `hmtx` 2048). The "Mono"
  in the name means uniform advance, not cell-fitted. Menlo's `M` is 0.602 em
  and SF Mono's is 0.618 em. So a cascade fallback at the terminal's point size
  draws icons about 1.6× the cell width.
- The font has no `m`, `M`, or space glyph. GPUI refuses to load such a family
  directly as a `Font` (`text_system.rs:301-317`), so the symbol font can only
  be reached as a cascade fallback.
- Its cmap also maps six non-PUA ranges: U+23FB–23FE, U+2630, U+2665, U+26A1,
  U+276C–2771, U+2B58.
- Licenses (from the zip's README): the font is MIT (Ryan L McIntyre). The icon
  sets are MIT, CC BY 4.0 (Codicons, Font Awesome), Apache 2.0 (Material
  Design), OFL 1.1 (Weather Icons, Pomicons), and "unlicensed" (Font Logos;
  upstream appears to use The Unlicense, a public-domain dedication; confirm
  while vendoring).

## Changes

### 1. Vendor the font file

- Add `crates/nice/assets/fonts/SymbolsNerdFontMono-Regular.ttf` (v3.5.1,
  unmodified).
- Add `crates/nice/assets/fonts/LICENSE` (the zip's MIT text).
- Add `crates/nice/assets/fonts/README.md` (the zip's README). Prepend a short
  header covering: the source release, that only the Mono TTF is vendored, why
  it is here (GH #6), and the confirmed Font Logos license wording.

### 2. Register the font with CoreText at launch (`crates/nice/src/platform.rs`)

New `pub fn register_bundled_fonts()`:

1. **Embed the bytes.** `const SYMBOLS_FONT: &[u8] = include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");`
2. **Materialize a file.** Target is
   `std::env::temp_dir()/nice-fonts/SymbolsNerdFontMono-Regular-v3.5.1.ttf`.
   - If the file exists with the same length, reuse it.
   - Otherwise write to a sibling temp name and `rename` it into place. The
     rename is atomic, so concurrent Nice / Nice Dev launches never read a
     partial file.
   - Use `temp_dir`, not `support_root()`: selftests must not touch the
     support root, and the file is a cache that can be rebuilt. macOS purging
     `$TMPDIR` is harmless because the file is rewritten on the next launch.
3. **Register it.**
   `CTFontManagerRegisterFontsForURL(url, kCTFontManagerScopeProcess, &mut error)`.
   This API is not deprecated. The in-memory `CTFontManagerRegisterGraphicsFont`
   is deprecated since macOS 15 (`CTFontManager.h:216-218`), and its suggested
   replacement's descriptors "are not available through font descriptor
   matching", which is exactly what the cascade needs.
4. **Classify the result.**
   - Success, or error code 105 (`kCTFontManagerErrorAlreadyRegistered`) or
     305 (`kCTFontManagerErrorDuplicatedName`): the family is available. This
     covers users who installed the Homebrew cask, such as Jesse. Stay silent.
   - Any other failure, including the file write failing:
     `eprintln!("nice: bundled symbol font unavailable: …")` once, then
     continue. PUA cells then render as they do today.
5. Build the `CFURL` with the existing CF FFI style (`CFURLCreateFromFileSystemRepresentation`)
   and `CFRelease` everything created. Link against the CoreText framework
   (already linked at `:347`).

Call it from `app::run()` and `app::run_selftest()` right after
`disable_font_smoothing()`.

### 3. Symbol cells: fallback + cell-fitted size (`crates/nice-term-view`)

In `font.rs`:

- `pub const SYMBOL_FALLBACK_FAMILY: &str = "Symbols Nerd Font Mono";`
- `pub(crate) fn is_symbol_char(ch: char) -> bool` returns true for PUA:
  U+E000–U+F8FF, U+F0000–U+FFFFD, and U+100000–U+10FFFD. The six non-PUA ranges
  the font also maps are deliberately **excluded** (see Decisions).
- A unit test for `is_symbol_char`: range boundaries, plus ASCII,
  box-drawing, U+26A1, and U+2665 all return false.

In `element.rs`:

- `cell_font` gains a `symbol: bool` parameter. When true, it sets
  `fallbacks: Some(FontFallbacks::from_fonts(vec![SYMBOL_FALLBACK_FAMILY.into()]))`.
  When false, fallbacks stay `None`, so non-PUA text is untouched.
  `paint_glyph_run` builds its `Font` through `cell_font` too, instead of its
  inline literal, so both paint sites share one construction.
- New helper `shape_cell_glyph(window, text, font, font_px, cw) -> ShapedLine`,
  used by `paint_glyph_run` when the run is a single symbol cell and by
  `paint_cursor_glyph` when `is_symbol_char(cell.ch)`:
  1. Shape at `px(font_px)` with the symbol-fallback font.
  2. If every shaped run's `font_id` equals `resolve_font(&font)`, the base
     font carries the glyph, as with a patched Nerd Font such as MesloLGS NF.
     Return the step-1 result. **Icons in patched fonts keep their designed
     size.**
  3. Otherwise the glyph came from a fallback. Re-shape the same text at
     `px(cw)` so the 1-em icon fits the cell width, keeping `Some(px(cw))` as
     force width. Painting stays at the same `(x, y)` and `ch`, and
     `paint_line` centres it vertically.
- Only single-cell runs qualify (`run.cells == 1 && is_symbol_char(ch)`).
  Multi-cell ASCII runs never contain PUA (`element.rs:524-531`).
- The `LineLayoutCache` keys on text + size + runs, so the second size adds one
  cache entry per distinct icon. It is not a per-frame cost.

Unchanged: `term_font` (preedit anchor), the preedit `cell_font` call
(`:1406`, passes `symbol: false`), and `cell_metrics`.

### 4. Attribution

- `README.md`: add a short credits line. It says Nice bundles Symbols Nerd Font
  Mono from Nerd Fonts (MIT) and points to `crates/nice/assets/fonts/` for the
  icon-set licenses. The DMG carries no in-app notice; kitty and Ghostty take
  the same position.

No settings, no UI, no `ui_settings.json` key (YAGNI).

## Decisions

- **Embed with `include_bytes!`, materialize to `$TMPDIR`, register by URL.**
  - Embedding costs +2.6 MB per architecture slice, about +5.2 MB for the
    universal binary. Nick accepted this (2026-09-12).
  - Embedding keeps `scripts/rust-bundle.sh` and the release untouched, and
    works for unbundled selftests.
  - The file step exists only because the in-memory registration API is
    deprecated.
- **PUA only; the six non-PUA ranges are excluded.** Including them would turn
  ⚡ (U+26A1) from color emoji into a monochrome glyph, and would restyle ♥ and
  the others whenever the base font lacks them. No reported prompt theme
  depends on them.
- **Fit to `cw`, not a tuned ratio.** The em square equals the cell width,
  which is the scale the Nerd patcher applies to Mono variants. Tweak only if
  validation shows icons look off.
- **Shrink only when the fallback supplied the glyph** (the `font_id` check).
  Blanket PUA shrinking would shrink icons for users whose terminal font is
  already patched.
- **Bundle instead of relying on a user-installed font.** "Symbols Nerd Font
  Mono" is not a macOS system font. Ghostty, WezTerm, and Kitty bundle Nerd
  Font symbols too.
- **Nice implements it; Jesse's PR offer is declined with thanks** (Nick's
  call).

### Pre-decided contingency: bold/italic

GPUI stamps each fallback descriptor with the base face's weight and slant
(`open_type.rs:104-118`), and the symbol font has only a Regular face. If
validation shows bold or italic PUA cells still render as tofu:

- Try first, with no vendor patch: pass `symbol: true` with
  `FontWeight::NORMAL` / `FontStyle::Normal` for the shaping `Font` of symbol
  cells. Icons have no bold form anyway.
- Only if that fails, add an 8th vendor patch (`zed-fallback-no-traits`) that
  omits the traits dictionary for user fallback entries in
  `generate_fallback_array`. Also add it to the CLAUDE.md patch list.

## Tests

- New unit test for `is_symbol_char` (above).
- The CoreText cascade cannot run under GPUI's test platform. It is covered by
  black-box validation.
- Targeted: `cargo test -p nice-term-view`, `cargo test -p nice`,
  `cargo build --workspace`.

## Validation (Nice Dev, scratch env, under the worktree lock)

Use the CLAUDE.md scratch-env recipe: keychain symlink + `.claude.json` seed,
full env, never `env -i`, never prod. Hold the lock through all steps.
Use `caffeinate -d` for screenshots. Reuse one scratch dir throughout so
settings carry over.

Test string:
`printf '     ⚡\n'`.

1. **Baseline.** Install `main` bits into Nice Dev (an extra install, done on
   purpose). Set `fonts.terminal_font_family` to `"Menlo"` in the scratch
   `ui_settings.json`. Run the test string and screenshot.
   - Expect tofu for the PUA glyphs and a color-emoji ⚡.
   - If PUA glyphs already render, the test is invalid: find which installed
     font catches them before going on.
2. **Branch.** Install the branch (`scripts/rust-install.sh`), relaunch the
   same scratch env, run the test string, and screenshot. Expect:
   - Every PUA glyph renders within its own cell, with no ink in the
     neighbouring column.
   - U+E0B2's ink sits inside its own cell.
   - Icons are vertically centred, not sitting on the baseline.
   - ⚡ is still color emoji.
3. **Bold.** Run `printf '\e[1m \e[0m\n'`. If it shows tofu, apply
   the bold contingency above, then re-run.
4. **Patched font unchanged.** Set the family to `"MesloLGS NF"` (installed on
   this Mac). Icons must keep their pre-change size, so compare against the
   same string on the baseline install.
5. **Real prompt.** Open a Claude Code session and a powerline prompt in the
   scratch env. Screenshot the status line and prompt with Menlo.
6. **1× display.** Repeat step 2 with the window on the external 1× display.
   Check icon edges and cell alignment.
7. **Grid regression.** ASCII column alignment and box-drawing match the
   baseline screenshot.
8. **Registration failure path.** Do one run with the registration call
   temporarily disabled. Expect ASCII fine, PUA tofu, and no crash. Revert
   before committing.

## Landing

1. Commit on `worktree-gh-issue-6`, push, and run
   `gh pr create --base main`. The body includes `Closes #6`, a summary, the
   validation screenshots' findings, and the session footer
   (`🤖 Generated with [Claude Code](https://claude.com/claude-code)` + the
   session link).
2. Nick feel-checks. The PR merges only on Nick's explicit go.
3. Nick posts the issue comment (draft already given to him).

## Known gaps (deliberate)

- Icons drawn from the bundled font are sized to one cell. Nerd Font's
  "wide" icons render smaller than in a non-Mono setup, the same as in other
  terminals that bundle the Mono variant.
- The six non-PUA symbols the font maps are not covered (see Decisions).
- A future Nerd Fonts codepoint change needs a manual font refresh. There is
  no update automation (YAGNI).
