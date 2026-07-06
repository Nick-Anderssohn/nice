//! Nice → Claude Code theme mirror (R17) — ports Swift `ClaudeThemeSync`
//! (`Sources/Nice/Process/ClaudeThemeSync.swift`).
//!
//! Mirrors Nice's active terminal theme into Claude Code so a Claude session
//! launched inside Nice re-themes itself to match — live, with no `/theme` and
//! no restart. The design has two halves, both verified against the shipped
//! Claude Code CLI (`ClaudeThemeSync.swift:9-30`):
//!
//!   1. The COLORS live in a custom-theme file Claude file-watches and
//!      LIVE-RELOADS. We keep one file at the managed slug and rewrite its
//!      contents on every Nice theme change; every Nice-launched Claude
//!      repaints within a moment.
//!   2. The POINTER that selects the theme (`"theme": "custom:<slug>"`) is read
//!      ONCE at Claude startup, so editing it live does nothing. Instead each
//!      Claude we spawn gets a `--settings <file>` flag pointing at the pointer
//!      file, whose `flag` settings source outranks the user's global
//!      `~/.claude/settings.json` — so the override applies ONLY to sessions
//!      Nice launches (R15 threads the flag; [`settings_flag_path`] resolves
//!      it).
//!
//! Why a per-file marker ([`MANAGED_MARKER`]): the slug is short and could
//! collide with a theme a user hand-authored. Before overwriting the theme file
//! we check for our inert `"_niceManaged": true` key and refuse to clobber a
//! file that lacks it (mirrors [`crate::claude_hook_installer`]'s
//! refuse-to-clobber stance). Claude ignores unknown top-level keys, so the
//! marker is invisible to it.
//!
//! Idempotency: every write is atomic (temp + rename) and only-if-changed
//! (byte-stable via sorted keys), so identical themes never touch disk and
//! Claude's watcher isn't woken needlessly. Failures are logged and swallowed —
//! Claude renders fine with its own theme; only the sync degrades. Structurally
//! modeled on [`crate::claude_hook_installer`].
//!
//! **Dev-time identity isolation.** Nick's daily-driver Swift Nice actively
//! syncs HIS theme to `~/.claude/themes/nice.json` via pointer `custom:nice`.
//! The Rust dev app therefore uses ITS OWN slug and pointer file so the two
//! never collide (see [`SLUG`] / [`POINTER_FILENAME`]). Both flip to their
//! Swift-parity names at the parity rename.
//!
//! **Hermeticity (tranche-3).** Both entry points take injectable base paths
//! ([`write_with`], [`settings_flag_path_in`]); production [`write`] /
//! [`settings_flag_path`] resolve them from `$HOME` (+ `$CLAUDE_CONFIG_DIR`).
//! Tests and self-test scenarios drive the injectable forms against sandbox
//! directories so the regression suite never touches the developer's real
//! `~/.claude` / `~/.nice`, and the launch-time writers run in `app::run` ONLY,
//! never `run_selftest`.

// Production entry points ([`write`], [`settings_flag_path`], and the default-
// path resolvers) are wired by later R17 slices (slice 2 = R15 provider fill,
// slice 3 = bootstrap write-on-startup); the pure writer + guards below are
// exercised by the in-crate tests and re-used by R21's live-retheme cache.
// Matches the not-yet-fully-wired-module idiom used by `app_shell` /
// `control_socket` / `shell_inject`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use nice_term_view::{TerminalColor, TerminalTheme};
use nice_theme::color::Srgba;
use nice_theme::ColorScheme;

// MARK: - Constants

/// Slug for the managed theme — drives the filename (`<slug>.json`) and the
/// pointer value (`custom:<slug>`). `ClaudeThemeSync.swift:60`.
///
/// **Dev-time divergence (flips to `"nice"` at the parity rename):** the Swift
/// app owns `nice.json` / `custom:nice` on this same machine; the Rust dev app
/// uses `nice-rs` so it cannot clobber the Swift app's live theme sync.
pub const SLUG: &str = "nice-rs";

/// Filename of the `--settings` pointer file under `~/.nice/`.
///
/// **Dev-time divergence (flips to `"claude-theme-settings.json"` at the parity
/// rename):** distinct from the Swift app's `claude-theme-settings.json` so the
/// two apps' pointer files never collide. Space-free (the R15 reply field and
/// shell splice are whitespace-parsed — frozen grammar).
pub const POINTER_FILENAME: &str = "claude-theme-settings-rs.json";

/// `name` shown in Claude's `/theme` picker. `ClaudeThemeSync.swift:63`.
pub const DISPLAY_NAME: &str = "Nice";

/// Prefix Claude uses to reference a custom theme by slug.
/// `ClaudeThemeSync.swift:66`.
pub const CUSTOM_THEME_PREFIX: &str = "custom:";

/// Inert top-level key marking a theme file as Nice-authored so we only ever
/// overwrite our own file. `ClaudeThemeSync.swift:70`.
pub const MANAGED_MARKER: &str = "_niceManaged";

/// WCAG 2.1 AA contrast floor for normal-size body text (4.5:1). The
/// secondary-text tokens (`subtle`, `inactive`) must clear this.
/// `ClaudeThemeSync.swift:74`.
pub const MIN_TEXT_CONTRAST: f64 = 4.5;

/// WCAG 2.1 AA contrast floor for UI component boundaries (3:1). The prompt
/// border is decorative chrome, so it may stay fainter than body text while
/// remaining perceptible. `ClaudeThemeSync.swift:79`.
pub const MIN_CHROME_CONTRAST: f64 = 3.0;

// MARK: - Colors

/// An 8-bit sRGB color — the `ThemeColor` shape from the Swift pipeline
/// (`ClaudeThemeSync` works entirely in 8-bit sRGB). The app-side color both
/// [`TerminalColor`] (the view crate's theme color) and [`Srgba`] (the accent)
/// fold into at the edge, so neither view-crate nor theme-crate type needs
/// serde.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb8 {
    /// Red channel, 0–255.
    pub r: u8,
    /// Green channel, 0–255.
    pub g: u8,
    /// Blue channel, 0–255.
    pub b: u8,
}

impl Rgb8 {
    /// A color from its three 8-bit channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `#rrggbb`, six lowercase hex digits — the form Claude's parser accepts
    /// (`ThemeColor.hexString`; anything else is silently dropped by Claude).
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// A view-crate [`TerminalColor`] (already 8-bit sRGB) folded into the
    /// pipeline's color type.
    pub const fn from_terminal(c: TerminalColor) -> Self {
        Self { r: c.r, g: c.g, b: c.b }
    }

    /// A [`Srgba`] accent (f32 `0.0..=1.0`) quantized to 8-bit sRGB — mirrors
    /// Swift's `themeColor(NSColor)` (`ClaudeThemeSync.swift:336-340`):
    /// `byte(v) = clamp(round(v * 255), 0, 255)`. Alpha is dropped (the accent
    /// is opaque).
    pub fn from_srgba(c: Srgba) -> Self {
        fn byte(v: f32) -> u8 {
            (v * 255.0).round().clamp(0.0, 255.0) as u8
        }
        Self { r: byte(c.r), g: byte(c.g), b: byte(c.b) }
    }
}

/// The 8-bit theme inputs [`make_claude_theme`] consumes, converted once at the
/// edge from a view-crate [`TerminalTheme`]. `ansi` is a slice (not a fixed
/// array) so a future malformed import (R22) whose palette isn't 16 entries can
/// degrade to a clean base flip rather than trap — the Swift
/// `guard theme.ansi.count == 16` (`ClaudeThemeSync.swift:165`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeColors {
    /// Default background.
    pub background: Rgb8,
    /// Default foreground (default text color).
    pub foreground: Rgb8,
    /// Selection color; `None` ⇒ `selectionBg` falls through to `base`.
    pub selection: Option<Rgb8>,
    /// The ANSI palette entries — contractually 16 (normal 0–7, bright 8–15).
    pub ansi: Vec<Rgb8>,
}

impl ThemeColors {
    /// Fold a view-crate [`TerminalTheme`] into the 8-bit pipeline inputs (the
    /// "convert at the edge" step; keeps serde off `nice-term-view`).
    pub fn from_terminal(t: &TerminalTheme) -> Self {
        Self {
            background: Rgb8::from_terminal(t.background),
            foreground: Rgb8::from_terminal(t.foreground),
            selection: t.selection.map(Rgb8::from_terminal),
            ansi: t.ansi.iter().copied().map(Rgb8::from_terminal).collect(),
        }
    }
}

// MARK: - Theme schema

/// Claude's custom-theme document, app-side and serializable (no serde on the
/// view-crate types it derives from). Shape
/// `{ "name", "base": "dark"|"light", "_niceManaged": true, "overrides": {…} }`
/// (`ClaudeThemeSync.swift:156-160,272`). `overrides` is `None` when the input
/// palette was malformed (the bare base flip).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeTheme {
    /// `name` shown in Claude's `/theme` picker (always [`DISPLAY_NAME`]).
    pub name: String,
    /// `"dark"` or `"light"` — flips with the scheme so unmapped tokens stay
    /// legible against the matching preset.
    pub base: &'static str,
    /// Our marker key's value (always `true`).
    pub managed: bool,
    /// `token -> "#rrggbb"`; `None` for a malformed palette (base flip only).
    pub overrides: Option<BTreeMap<String, String>>,
}

impl ClaudeTheme {
    /// The `serde_json` document, keys sorted recursively so the serialization
    /// is byte-stable regardless of serde_json's `preserve_order` feature (the
    /// same discipline as [`crate::claude_hook_installer`]).
    fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(MANAGED_MARKER.to_string(), Value::Bool(self.managed));
        map.insert("base".to_string(), Value::String(self.base.to_string()));
        map.insert("name".to_string(), Value::String(self.name.clone()));
        if let Some(overrides) = &self.overrides {
            let mut om = Map::new();
            for (k, v) in overrides {
                om.insert(k.clone(), Value::String(v.clone()));
            }
            map.insert("overrides".to_string(), Value::Object(om));
        }
        sort_value(&Value::Object(map))
    }

    /// Pretty, stable-sorted JSON bytes (what lands on disk).
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&self.to_value())
            .expect("serialize claude theme (in-memory Value never fails)")
    }
}

/// Build Claude's custom-theme document from the 8-bit theme inputs + accent.
///
/// The mapping is transcribed verbatim as data from `makeThemeJSON`
/// (`ClaudeThemeSync.swift:150-273`): normal tokens ← matching normal ANSI /
/// accent; brighter "emphasis" tokens ← the bright ANSI variant (a[8..16]) or a
/// lightened accent; background-tinted tokens ← a flat [`blend`] over the
/// background (Claude tokens take no alpha). `cursor` is terminal-drawn, so it
/// has no Claude token. A malformed palette (`ansi.len() != 16`) degrades to a
/// clean base flip (`ClaudeThemeSync.swift:165`).
pub fn make_claude_theme(colors: &ThemeColors, scheme: ColorScheme, accent: Rgb8) -> ClaudeTheme {
    let base = match scheme {
        ColorScheme::Dark => "dark",
        ColorScheme::Light => "light",
    };
    let mut theme = ClaudeTheme {
        name: DISPLAY_NAME.to_string(),
        base,
        managed: true,
        overrides: None,
    };

    // ANSI palette is contractually 16 entries; guard so a malformed imported
    // theme degrades to a clean light/dark flip rather than indexing OOB.
    if colors.ansi.len() != 16 {
        return theme;
    }

    let fg = colors.foreground;
    let bg = colors.background;
    let a = &colors.ansi; // 0 blk 1 red 2 grn 3 yel 4 blu 5 mag 6 cyn 7 wht; 8–15 bright
    let acc = accent;
    let acc_light = lighten(accent, 0.25);

    let mut o: BTreeMap<String, String> = BTreeMap::new();
    let mut put = |k: &str, c: Rgb8| {
        o.insert(k.to_string(), c.hex());
    };

    // Text & surfaces (ClaudeThemeSync.swift:176-183)
    put("text", fg);
    put("inverseText", bg);
    put("background", bg);
    if let Some(sel) = colors.selection {
        put("selectionBg", sel);
    }
    put("userMessageBackground", blend(fg, bg, 0.06));
    put("userMessageBackgroundHover", blend(fg, bg, 0.10));
    put("bashMessageBackgroundColor", blend(a[5], bg, 0.08));
    put("memoryBackgroundColor", blend(a[4], bg, 0.08));

    // Status (ClaudeThemeSync.swift:186-189)
    put("error", a[1]);
    put("success", a[2]);
    put("warning", a[3]);
    put("warningShimmer", a[11]);

    // Accent family (ClaudeThemeSync.swift:192-203)
    put("claude", acc);
    put("autoAccept", acc);
    put("skill", a[6]);
    put("fastMode", acc);
    put("fastModeShimmer", acc_light);
    put("effortUltra", acc);
    put("merged", a[5]);
    put("claudeShimmer", acc_light);
    put("clawd_body", acc);
    put("clawd_background", bg);
    put("briefLabelClaude", acc);

    // Info / links / modes (ClaudeThemeSync.swift:206-216)
    put("permission", a[4]);
    put("permissionShimmer", a[12]);
    put("suggestion", a[4]);
    put("remember", a[4]);
    put("ide", a[4]);
    put("planMode", a[6]);
    put("bashBorder", a[5]);
    put("professionalBlue", a[4]);
    put("chromeYellow", a[3]);
    put("briefLabelYou", a[4]);
    put("claudeBlue_FOR_SYSTEM_SPINNER", a[4]);
    put("claudeBlueShimmer_FOR_SYSTEM_SPINNER", a[12]);

    // Muted / chrome. `subtle`/`inactive` carry secondary TEXT (model line, cwd
    // path, tool summaries, status row), so they ride the 4.5:1 body floor;
    // `promptBorder` is a UI boundary, so it rides the lower 3:1 chrome floor.
    // ANSI bright-black (a[8]) is kept where it already clears the floor and
    // only lifted toward fg when it doesn't (ClaudeThemeSync.swift:229-235).
    put("promptBorder", legible_mute(fg, bg, a[8], MIN_CHROME_CONTRAST));
    put("promptBorderShimmer", a[15]);
    put("inactive", legible_mute(fg, bg, a[8], MIN_TEXT_CONTRAST));
    put("inactiveShimmer", a[15]);
    put("subtle", legible_mute(fg, bg, a[8], MIN_TEXT_CONTRAST));
    put("rate_limit_fill", a[4]);
    put("rate_limit_empty", blend(a[8], bg, 0.5));

    // Diffs — block tints over the background, word-level on bright
    // (ClaudeThemeSync.swift:238-243)
    put("diffAdded", blend(a[2], bg, 0.30));
    put("diffRemoved", blend(a[1], bg, 0.30));
    put("diffAddedDimmed", blend(a[2], bg, 0.15));
    put("diffRemovedDimmed", blend(a[1], bg, 0.15));
    put("diffAddedWord", blend(a[10], bg, 0.55));
    put("diffRemovedWord", blend(a[9], bg, 0.55));

    // Per-agent palette (ClaudeThemeSync.swift:246-253)
    put("red_FOR_SUBAGENTS_ONLY", a[1]);
    put("green_FOR_SUBAGENTS_ONLY", a[2]);
    put("yellow_FOR_SUBAGENTS_ONLY", a[3]);
    put("blue_FOR_SUBAGENTS_ONLY", a[4]);
    put("cyan_FOR_SUBAGENTS_ONLY", a[6]);
    put("purple_FOR_SUBAGENTS_ONLY", a[5]);
    put("pink_FOR_SUBAGENTS_ONLY", a[13]);
    put("orange_FOR_SUBAGENTS_ONLY", blend(a[1], a[3], 0.5));

    // Rainbow gradient — best-fit onto the ~6 ANSI hues; shimmer stops use the
    // bright variants (ClaudeThemeSync.swift:257-270)
    put("rainbow_red", a[1]);
    put("rainbow_orange", blend(a[1], a[3], 0.5));
    put("rainbow_yellow", a[3]);
    put("rainbow_green", a[2]);
    put("rainbow_blue", a[4]);
    put("rainbow_indigo", a[5]);
    put("rainbow_violet", a[13]);
    put("rainbow_red_shimmer", a[9]);
    put("rainbow_orange_shimmer", blend(a[9], a[11], 0.5));
    put("rainbow_yellow_shimmer", a[11]);
    put("rainbow_green_shimmer", a[10]);
    put("rainbow_blue_shimmer", a[12]);
    put("rainbow_indigo_shimmer", a[13]);
    put("rainbow_violet_shimmer", a[13]);

    theme.overrides = Some(o);
    theme
}

/// Convenience edge: build the document straight from the view-crate
/// [`TerminalTheme`] + [`Srgba`] accent (converting to 8-bit once).
pub fn theme_json(theme: &TerminalTheme, scheme: ColorScheme, accent: Srgba) -> ClaudeTheme {
    make_claude_theme(&ThemeColors::from_terminal(theme), scheme, Rgb8::from_srgba(accent))
}

// MARK: - Color helpers

/// `fg` composited over `bg` at `alpha` (0…1), opaque result. Claude tokens take
/// no alpha, so background-tinted tokens are pre-flattened against the terminal
/// background. `ClaudeThemeSync.swift:281-287`.
pub fn blend(fg: Rgb8, bg: Rgb8, alpha: f64) -> Rgb8 {
    fn mix(f: u8, b: u8, alpha: f64) -> u8 {
        let v = f as f64 * alpha + b as f64 * (1.0 - alpha);
        v.round().clamp(0.0, 255.0) as u8
    }
    Rgb8::new(mix(fg.r, bg.r, alpha), mix(fg.g, bg.g, alpha), mix(fg.b, bg.b, alpha))
}

/// Blend `c` toward white by `amount` (0…1) — the "brighter ramp" for shimmer
/// tokens whose base is the accent (no ANSI-bright sibling).
/// `ClaudeThemeSync.swift:292-294`.
pub fn lighten(c: Rgb8, amount: f64) -> Rgb8 {
    blend(Rgb8::new(255, 255, 255), c, amount)
}

/// WCAG 2.1 relative luminance of an 8-bit sRGB color (0…1).
/// `ClaudeThemeSync.swift:297-303`.
pub fn luminance(c: Rgb8) -> f64 {
    fn channel(v: u8) -> f64 {
        let s = v as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// WCAG 2.1 contrast ratio between two colors (1…21, order-independent).
/// `ClaudeThemeSync.swift:306-309`.
pub fn contrast_ratio(x: Rgb8, y: Rgb8) -> f64 {
    let a = luminance(x);
    let b = luminance(y);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// A muted text/chrome color guaranteed to clear `min_contrast` against `bg`.
/// Prefers the theme's own dim color (`dim`, normally ANSI bright-black) so each
/// theme keeps its character; when `dim` is too faint it is composited toward
/// `fg` — the smallest lift that clears the floor — preserving the dim hue while
/// rescuing legibility. Falls back to `fg` if even that can't (a pathologically
/// low-contrast theme). Compositing toward `fg` moves monotonically away from
/// `bg` in luminance, so the first clearing step is also the most muted one.
/// `ClaudeThemeSync.swift:320-331` — the Solarized-class fix.
pub fn legible_mute(fg: Rgb8, bg: Rgb8, dim: Rgb8, min_contrast: f64) -> Rgb8 {
    if contrast_ratio(dim, bg) >= min_contrast {
        return dim;
    }
    let mut alpha = 0.1;
    while alpha < 1.0 {
        let lifted = blend(fg, dim, alpha);
        if contrast_ratio(lifted, bg) >= min_contrast {
            return lifted;
        }
        alpha += 0.05;
    }
    fg
}

// MARK: - File writers

/// Write/refresh the theme file and ensure the pointer file exists, against
/// injected sandbox paths. Failures are logged and swallowed (Claude renders
/// fine with its own theme; only the sync degrades) — mirrors Swift's
/// `write(theme:scheme:accent:themesDir:settingsURL:)`
/// (`ClaudeThemeSync.swift:103-116`).
pub fn write_with(
    theme: &TerminalTheme,
    scheme: ColorScheme,
    accent: Srgba,
    themes_dir: &Path,
    settings_path: &Path,
) {
    let document = theme_json(theme, scheme, accent);
    if let Err(e) = ensure_theme_file(&document, themes_dir) {
        eprintln!("nice-rs: ClaudeThemeSync: write failed: {e}");
        return;
    }
    if let Err(e) = ensure_settings_file(settings_path) {
        eprintln!("nice-rs: ClaudeThemeSync: write failed: {e}");
    }
}

/// Write/refresh the theme file + pointer file against the real `$HOME`
/// (+ `$CLAUDE_CONFIG_DIR`). Call once on startup from `app::run` (NEVER
/// `run_selftest`). Wired by R17 slice 3 (bootstrap).
pub fn write(theme: &TerminalTheme, scheme: ColorScheme, accent: Srgba) {
    write_with(theme, scheme, accent, &default_themes_dir(), &default_theme_settings_path());
}

/// Ensure the pointer file exists and return its absolute path for
/// `claude --settings <path>`, resolving against `home`. Returns `None` on
/// failure so the caller can omit the flag and let Claude use its own theme.
/// `ClaudeThemeSync.swift:122-131` (ensure-on-read: a `--settings` pointing at a
/// missing file makes claude error, so the read has the write side effect).
pub fn settings_flag_path_in(home: &Path) -> Option<String> {
    let path = theme_settings_path(home);
    match ensure_settings_file(&path) {
        Ok(()) => Some(path.to_string_lossy().into_owned()),
        Err(e) => {
            eprintln!("nice-rs: ClaudeThemeSync: settings file failed: {e}");
            None
        }
    }
}

/// Ensure-on-read of the pointer file against the real `$HOME`. Wired by R17
/// slice 2 (the R15 settings-path provider fill).
pub fn settings_flag_path() -> Option<String> {
    settings_flag_path_in(&home_dir())
}

/// R15's Claude theme-sync `--settings` provider value for a given gate state,
/// resolved against `home` (the hermetic form). `sync_on` ⇒ the ensure-on-read
/// pointer path ([`settings_flag_path_in`]); off ⇒ `None`. The injectable twin of
/// [`settings_path_for_gate`] — the gating tests exercise this exact mapping
/// against a throwaway home so they never touch the real `~/.nice`.
pub fn settings_path_for_gate_in(sync_on: bool, home: &Path) -> Option<String> {
    if sync_on {
        settings_flag_path_in(home)
    } else {
        None
    }
}

/// Production fill for R15's `--settings` provider: `sync_on` ⇒
/// `Some(settings_flag_path())` (ensure-on-read against the real `$HOME`), off ⇒
/// `None` — the Rust mirror of Swift's
/// `themeCache.syncClaudeTheme ? ClaudeThemeSync.settingsFlagPath() : nil`
/// (`SessionThemeCache.swift`). The shipped window builder
/// (`crate::app::open_managed_window`) calls this with the process gate's bool to
/// fill each window's provider before the Main pane forks.
pub fn settings_path_for_gate(sync_on: bool) -> Option<String> {
    if sync_on {
        settings_flag_path()
    } else {
        None
    }
}

/// Write the theme file only when its bytes change, and only when an existing
/// file is ours (carries [`MANAGED_MARKER`]). A foreign or unparseable file is
/// left intact (logged), never destroyed. `ClaudeThemeSync.swift:347-374`.
fn ensure_theme_file(document: &ClaudeTheme, dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{SLUG}.json"));

    if let Ok(existing) = fs::read(&path) {
        if !existing.is_empty() {
            match serde_json::from_slice::<Value>(&existing) {
                Ok(Value::Object(map)) => {
                    if map.get(MANAGED_MARKER).and_then(Value::as_bool) != Some(true) {
                        eprintln!(
                            "nice-rs: ClaudeThemeSync: refusing to overwrite foreign {}",
                            path.display()
                        );
                        return Ok(());
                    }
                }
                // Valid JSON but not an object, or non-JSON bytes: a user may
                // have hand-authored a file at our slug. Leave it intact.
                _ => {
                    eprintln!(
                        "nice-rs: ClaudeThemeSync: refusing to overwrite non-JSON {}",
                        path.display()
                    );
                    return Ok(());
                }
            }
        }
    }

    let bytes = document.to_json_bytes();
    if fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    write_atomic(&path, &bytes)
}

/// Write the pointer file (`{"theme":"custom:<slug>"}`) once; rewritten only if
/// the contents drift. The directory is the no-space `~/.nice/` so the path
/// survives Claude's shell-based flag handling. `ClaudeThemeSync.swift:381-391`.
fn ensure_settings_file(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut map = Map::new();
    map.insert(
        "theme".to_string(),
        Value::String(format!("{CUSTOM_THEME_PREFIX}{SLUG}")),
    );
    let bytes = serde_json::to_vec_pretty(&sort_value(&Value::Object(map)))
        .expect("serialize pointer file (in-memory Value never fails)");
    if fs::read(path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    write_atomic(path, &bytes)
}

/// Atomically replace `path` with `contents`: write a pid-suffixed sibling in
/// the same directory, then rename over the target (atomic within a filesystem
/// — Claude's watcher never sees a half-written file). Mirrors
/// [`crate::claude_hook_installer`]'s `write_atomic` (mode-less variant).
fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

/// Recursively rebuild `v` with every object's keys sorted (arrays keep their
/// order). Serializing the result is byte-stable even if a workspace dependency
/// flips serde_json's `preserve_order` on. Same discipline as
/// [`crate::claude_hook_installer`].
fn sort_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), sort_value(&map[k]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

// MARK: - Path resolution

/// `<config>/themes` where `config` is `$CLAUDE_CONFIG_DIR` (if set non-empty)
/// else `<home>/.claude` — the directory Claude watches for custom themes.
/// `ClaudeThemeSync.swift:399-407`. `home` and the override are injected so
/// tests never touch the real environment.
pub fn themes_dir(home: &Path, config_dir_override: Option<&str>) -> PathBuf {
    match config_dir_override {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("themes"),
        _ => home.join(".claude").join("themes"),
    }
}

/// `<home>/.nice/<pointer filename>` — the per-session `{"theme":"custom:…"}`
/// pointer handed to `claude --settings`. Shares the no-space `~/.nice` dotdir
/// with the hook script. `ClaudeThemeSync.swift:414-416`.
pub fn theme_settings_path(home: &Path) -> PathBuf {
    home.join(".nice").join(POINTER_FILENAME)
}

/// Production themes dir: reads `$CLAUDE_CONFIG_DIR` + `$HOME`.
fn default_themes_dir() -> PathBuf {
    let over = std::env::var("CLAUDE_CONFIG_DIR").ok();
    themes_dir(&home_dir(), over.as_deref())
}

/// Production pointer-file path: resolves against `$HOME`.
fn default_theme_settings_path() -> PathBuf {
    theme_settings_path(&home_dir())
}

/// The process `$HOME`, falling back to `/` (production-only; the app always has
/// a real home). Tests drive the injectable forms directly.
fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()))
}

#[cfg(test)]
mod tests {
    //! Ports `Tests/NiceUnitTests/ClaudeThemeSyncTests.swift`. The mocha /
    //! every-bundled-dark-theme cases depend on the R22 theme catalog (not in
    //! this crate yet); the Solarized-like constructed case below covers the
    //! same bright-black-collapse lift. R21 owns live-retheme fan-out.
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use nice_theme::AccentPreset;

    // ---- temp-dir plumbing (mirrors claude_hook_installer.rs) --------------

    /// A throwaway directory removed on drop (never the developer's real
    /// `~/.claude` / `~/.nice` — hermeticity).
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(prefix: &str) -> Scratch {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// A sandbox `(root, themes_dir, settings_path)` triple under a fresh
    /// auto-removed scratch root.
    fn sandbox(prefix: &str) -> (Scratch, PathBuf, PathBuf) {
        let root = scratch(prefix);
        let themes = root.0.join(".claude").join("themes");
        let settings = root.0.join(".nice").join(POINTER_FILENAME);
        (root, themes, settings)
    }

    // ---- test fixtures -----------------------------------------------------

    /// A theme whose ANSI entries encode their index in the red channel
    /// (`a[i] == (i,0,0)`), so assertions can name the index they expect
    /// without hard-coding hex. Mirrors the Swift `makeTheme` helper.
    fn make_theme(selection: Option<Rgb8>) -> ThemeColors {
        ThemeColors {
            background: Rgb8::new(0, 0, 0),
            foreground: Rgb8::new(255, 255, 255),
            selection,
            ansi: (0..16).map(|i| Rgb8::new(i as u8, 0, 0)).collect(),
        }
    }

    /// A theme with explicit bg/fg/bright-black so muted-token contrast can be
    /// exercised directly. Mirrors the Swift `makeTheme(bg:fg:brightBlack:)`.
    fn make_muted_theme(bg: Rgb8, fg: Rgb8, bright_black: Rgb8) -> ThemeColors {
        let mut ansi: Vec<Rgb8> = (0..16).map(|i| Rgb8::new(i as u8, 0, 0)).collect();
        ansi[8] = bright_black;
        ThemeColors { background: bg, foreground: fg, selection: None, ansi }
    }

    fn overrides(theme: &ClaudeTheme) -> BTreeMap<String, String> {
        theme.overrides.clone().unwrap_or_default()
    }

    fn dark(colors: &ThemeColors, accent: Rgb8) -> ClaudeTheme {
        make_claude_theme(colors, ColorScheme::Dark, accent)
    }

    // ---- base / scheme -----------------------------------------------------

    #[test]
    fn base_flips_with_scheme() {
        let t = make_theme(Some(Rgb8::new(10, 20, 30)));
        assert_eq!(make_claude_theme(&t, ColorScheme::Dark, Rgb8::new(1, 2, 3)).base, "dark");
        assert_eq!(make_claude_theme(&t, ColorScheme::Light, Rgb8::new(1, 2, 3)).base, "light");
    }

    #[test]
    fn carries_name_and_managed_marker() {
        let j = dark(&make_theme(None), Rgb8::new(1, 2, 3));
        assert_eq!(j.name, "Nice");
        assert!(j.managed, "managed marker lets us tell our own file apart from a user's");
        // The marker surfaces in the serialized JSON under `_niceManaged`.
        let v: Value = serde_json::from_slice(&j.to_json_bytes()).unwrap();
        assert_eq!(v[MANAGED_MARKER], Value::Bool(true));
    }

    // ---- golden theme JSON (current default theme + accent) ----------------

    /// The shipped default (`TerminalTheme::nice_default_dark` + Terracotta —
    /// app.rs:867-868). Each expected value is an independent transcription of
    /// the cited source, per the fixture-provenance convention.
    #[test]
    fn golden_default_dark_theme() {
        let colors = ThemeColors::from_terminal(&TerminalTheme::nice_default_dark());
        let accent = Rgb8::from_srgba(AccentPreset::Terracotta.color());
        let j = dark(&colors, accent);
        assert_eq!(j.base, "dark");
        assert!(j.managed);
        let o = overrides(&j);

        // theme.rs nice_default_dark fg/bg/selection + ansi 1/2/3/11.
        assert_eq!(o["text"], "#f4f0ef"); // fg = (244,240,239)
        assert_eq!(o["background"], "#090705"); // bg = (9,7,5)
        assert_eq!(o["inverseText"], "#090705"); // bg
        assert_eq!(o["selectionBg"], "#3a3430"); // selection = (58,52,48)
        assert_eq!(o["error"], "#c23621"); // ansi[1] red
        assert_eq!(o["success"], "#25bc24"); // ansi[2] green
        assert_eq!(o["warning"], "#adad27"); // ansi[3] yellow
        assert_eq!(o["warningShimmer"], "#ead423"); // ansi[11] bright yellow
        // Accent (Terracotta #c96442 → 8-bit (201,100,66)).
        assert_eq!(o["claude"], "#c96442");
        // A blend result: userMessageBackground = blend(fg, bg, 0.06),
        // independently computed: (23,21,19) = #171513.
        assert_eq!(o["userMessageBackground"], "#171513");
        // nice_default_dark bright-black (#818383) clears 4.5:1 on the near-
        // black bg, so `subtle` passes through untouched (no lift).
        assert_eq!(o["subtle"], "#818383");
    }

    // ---- token mapping (wiring) --------------------------------------------

    #[test]
    fn maps_core_tokens() {
        let t = make_theme(Some(Rgb8::new(10, 20, 30)));
        let accent = Rgb8::new(7, 8, 9);
        let o = overrides(&dark(&t, accent));

        assert_eq!(o["text"], t.foreground.hex());
        assert_eq!(o["inverseText"], t.background.hex());
        assert_eq!(o["background"], t.background.hex());
        assert_eq!(o["selectionBg"], t.selection.unwrap().hex());
        assert_eq!(o["error"], t.ansi[1].hex());
        assert_eq!(o["success"], t.ansi[2].hex());
        assert_eq!(o["warning"], t.ansi[3].hex());
        assert_eq!(o["claude"], accent.hex());
        assert_eq!(o["autoAccept"], accent.hex());
        assert_eq!(o["fastMode"], accent.hex());
        assert_eq!(o["permission"], t.ansi[4].hex());
        assert_eq!(o["planMode"], t.ansi[6].hex());
        assert_eq!(o["bashBorder"], t.ansi[5].hex());
    }

    #[test]
    fn uses_bright_ansi_variants() {
        let t = make_theme(None);
        let o = overrides(&dark(&t, Rgb8::new(7, 8, 9)));
        assert_eq!(o["warningShimmer"], t.ansi[11].hex());
        assert_eq!(o["permissionShimmer"], t.ansi[12].hex());
        assert_eq!(o["inactiveShimmer"], t.ansi[15].hex());
        assert_eq!(o["diffAddedWord"], blend(t.ansi[10], t.background, 0.55).hex());
        assert_eq!(o["diffRemovedWord"], blend(t.ansi[9], t.background, 0.55).hex());
    }

    #[test]
    fn block_diffs_blend_normal_ansi_over_background() {
        let t = make_theme(None);
        let o = overrides(&dark(&t, Rgb8::new(7, 8, 9)));
        assert_eq!(o["diffAdded"], blend(t.ansi[2], t.background, 0.30).hex());
        assert_eq!(o["diffRemoved"], blend(t.ansi[1], t.background, 0.30).hex());
        assert_eq!(o["diffAddedDimmed"], blend(t.ansi[2], t.background, 0.15).hex());
    }

    #[test]
    fn shimmer_accent_tokens_are_lightened_accent() {
        let accent = Rgb8::new(100, 0, 0);
        let o = overrides(&dark(&make_theme(None), accent));
        assert_eq!(o["claudeShimmer"], lighten(accent, 0.25).hex());
        assert_eq!(o["fastModeShimmer"], lighten(accent, 0.25).hex());
    }

    #[test]
    fn omits_selection_when_none() {
        let o = overrides(&dark(&make_theme(None), Rgb8::new(1, 2, 3)));
        assert!(
            !o.contains_key("selectionBg"),
            "no selection color → fall through to base rather than emit garbage"
        );
    }

    #[test]
    fn malformed_ansi_returns_base_only_without_trapping() {
        // A palette with the wrong ANSI count degrades to a clean flip.
        let bad = ThemeColors {
            background: Rgb8::new(0, 0, 0),
            foreground: Rgb8::new(255, 255, 255),
            selection: None,
            ansi: vec![Rgb8::new(1, 1, 1)], // only 1 entry
        };
        let j = make_claude_theme(&bad, ColorScheme::Light, Rgb8::new(1, 2, 3));
        assert_eq!(j.base, "light");
        assert!(j.overrides.is_none(), "malformed palette emits no overrides");
        // And it serializes without an `overrides` key.
        let v: Value = serde_json::from_slice(&j.to_json_bytes()).unwrap();
        assert!(v.get("overrides").is_none());
    }

    #[test]
    fn emits_colors_in_claude_accepted_hex_form() {
        // Every override value must be `#rrggbb` (6 lowercase hex digits).
        let o = overrides(&dark(&make_theme(None), Rgb8::new(7, 8, 9)));
        for (token, value) in &o {
            assert!(is_hex6(value), "token {token} emitted non-hex value {value}");
        }
    }

    fn is_hex6(s: &str) -> bool {
        let bytes = s.as_bytes();
        bytes.len() == 7
            && bytes[0] == b'#'
            && bytes[1..].iter().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }

    // ---- muted tokens (contrast) -------------------------------------------

    #[test]
    fn muted_text_keeps_legible_dim_hue_untouched() {
        // a[8] that already clears 4.5:1 against the bg is passed through.
        let bg = Rgb8::new(0, 0, 0);
        let dim = Rgb8::new(0x88, 0x88, 0x88); // ~5.9:1 on black
        let o = overrides(&dark(&make_muted_theme(bg, Rgb8::new(255, 255, 255), dim), Rgb8::new(1, 2, 3)));
        assert_eq!(o["subtle"], dim.hex());
        assert_eq!(o["inactive"], dim.hex());
        assert_eq!(o["promptBorder"], dim.hex());
    }

    #[test]
    fn muted_text_rescued_when_dim_collapses_into_background() {
        // Solarized Dark's failure mode: bright-black == background. The
        // secondary-text tokens must be lifted to a readable color.
        let bg = Rgb8::new(0x00, 0x2b, 0x36);
        let t = make_muted_theme(bg, Rgb8::new(0x83, 0x94, 0x96), bg);
        let o = overrides(&dark(&t, Rgb8::new(1, 2, 3)));
        let subtle = parse_hex(&o["subtle"]);
        assert_ne!(o["subtle"], bg.hex(), "subtle must not equal the background");
        assert!(
            contrast_ratio(subtle, bg) >= MIN_TEXT_CONTRAST,
            "subtle text must clear the AA body floor"
        );
    }

    #[test]
    fn prompt_border_rides_lower_chrome_floor() {
        // On a theme where a[8] sits between the two floors, the border stays
        // at a[8] (3:1) while the text tokens get lifted higher (4.5:1).
        let bg = Rgb8::new(0, 0, 0);
        let dim = Rgb8::new(0x66, 0x66, 0x66); // ~3.7:1 on black: clears 3.0, not 4.5
        let o = overrides(&dark(&make_muted_theme(bg, Rgb8::new(255, 255, 255), dim), Rgb8::new(1, 2, 3)));
        assert_eq!(o["promptBorder"], dim.hex(), "border keeps the faint dim at the 3:1 floor");
        assert_ne!(o["subtle"], dim.hex(), "text is lifted past the 4.5:1 floor");
    }

    fn parse_hex(s: &str) -> Rgb8 {
        let n = u32::from_str_radix(s.trim_start_matches('#'), 16).expect("parse hex");
        Rgb8::new(((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8)
    }

    // ---- contrast helpers (WCAG spot values) -------------------------------

    #[test]
    fn luminance_endpoints() {
        assert!((luminance(Rgb8::new(0, 0, 0)) - 0.0).abs() < 1e-9);
        assert!((luminance(Rgb8::new(255, 255, 255)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn contrast_ratio_is_symmetric_and_bounded() {
        let black = Rgb8::new(0, 0, 0);
        let white = Rgb8::new(255, 255, 255);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 1e-6);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 1e-6, "order-independent");
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn legible_mute_returns_dim_when_already_legible() {
        let bg = Rgb8::new(0, 0, 0);
        let dim = Rgb8::new(0xcc, 0xcc, 0xcc);
        assert_eq!(legible_mute(Rgb8::new(255, 255, 255), bg, dim, 4.5), dim);
    }

    #[test]
    fn legible_mute_lifts_when_dim_too_faint() {
        let bg = Rgb8::new(0, 0, 0);
        let dim = Rgb8::new(0x10, 0x10, 0x10); // ~1.1:1 on black
        let result = legible_mute(Rgb8::new(255, 255, 255), bg, dim, 4.5);
        assert_ne!(result, dim);
        assert!(contrast_ratio(result, bg) >= 4.5);
    }

    // ---- color math --------------------------------------------------------

    #[test]
    fn blend_endpoints_and_midpoint() {
        let black = Rgb8::new(0, 0, 0);
        let white = Rgb8::new(255, 255, 255);
        assert_eq!(blend(white, black, 0.0).hex(), "#000000");
        assert_eq!(blend(white, black, 1.0).hex(), "#ffffff");
        assert_eq!(blend(white, black, 0.5).hex(), "#808080");
    }

    #[test]
    fn lighten_moves_toward_white() {
        assert_eq!(lighten(Rgb8::new(0, 0, 0), 0.25).hex(), "#404040");
    }

    #[test]
    fn from_srgba_quantizes_like_swift_theme_color() {
        assert_eq!(Rgb8::from_srgba(Srgba::rgb(0.0, 0.0, 0.0)), Rgb8::new(0, 0, 0));
        assert_eq!(Rgb8::from_srgba(Srgba::rgb(1.0, 0.0, 0.0)), Rgb8::new(255, 0, 0));
        // Terracotta round-trips (201/255 → 201).
        assert_eq!(Rgb8::from_srgba(AccentPreset::Terracotta.color()), Rgb8::new(201, 100, 66));
    }

    #[test]
    fn hex_formatting() {
        assert_eq!(Rgb8::new(0, 0, 0).hex(), "#000000");
        assert_eq!(Rgb8::new(255, 255, 255).hex(), "#ffffff");
        assert_eq!(Rgb8::new(15, 16, 255).hex(), "#0f10ff");
    }

    // ---- file writing ------------------------------------------------------

    fn write_once(themes: &Path, settings: &Path, scheme: ColorScheme) {
        write_with(
            &TerminalTheme::nice_default_dark(),
            scheme,
            AccentPreset::Terracotta.color(),
            themes,
            settings,
        );
    }

    fn theme_file(themes: &Path) -> PathBuf {
        themes.join(format!("{SLUG}.json"))
    }

    #[test]
    fn write_creates_both_files() {
        let (_root, themes, settings) = sandbox("theme-both");
        write_once(&themes, &settings, ColorScheme::Dark);
        assert!(theme_file(&themes).exists());
        assert!(settings.exists());
    }

    #[test]
    fn settings_file_carries_custom_pointer() {
        let (_root, themes, settings) = sandbox("theme-pointer");
        write_once(&themes, &settings, ColorScheme::Dark);
        let v: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(v["theme"], "custom:nice-rs");
    }

    /// The exact pointer-file bytes (frozen contract + byte-stability).
    #[test]
    fn settings_file_exact_bytes() {
        let (_root, themes, settings) = sandbox("theme-pointer-bytes");
        write_once(&themes, &settings, ColorScheme::Dark);
        let bytes = fs::read(&settings).unwrap();
        assert_eq!(bytes, b"{\n  \"theme\": \"custom:nice-rs\"\n}");
    }

    #[test]
    fn write_is_only_if_changed() {
        let (_root, themes, settings) = sandbox("theme-noop");
        write_once(&themes, &settings, ColorScheme::Dark);
        let m1 = fs::metadata(theme_file(&themes)).unwrap().modified().unwrap();
        write_once(&themes, &settings, ColorScheme::Dark); // identical inputs
        let m2 = fs::metadata(theme_file(&themes)).unwrap().modified().unwrap();
        assert_eq!(m1, m2, "an unchanged theme must not rewrite the file (watcher-safe)");
    }

    /// `to_json_bytes` is deterministic across calls (sorted keys).
    #[test]
    fn serialization_is_byte_stable() {
        let j = dark(&make_theme(Some(Rgb8::new(10, 20, 30))), Rgb8::new(7, 8, 9));
        assert_eq!(j.to_json_bytes(), j.to_json_bytes());
    }

    #[test]
    fn write_updates_on_real_change() {
        let (_root, themes, settings) = sandbox("theme-change");
        write_once(&themes, &settings, ColorScheme::Dark);
        write_once(&themes, &settings, ColorScheme::Light);
        let v: Value = serde_json::from_slice(&fs::read(theme_file(&themes)).unwrap()).unwrap();
        assert_eq!(v["base"], "light", "a real change must land on disk");
    }

    #[test]
    fn write_refuses_to_clobber_foreign_theme_file() {
        let (_root, themes, settings) = sandbox("theme-foreign");
        fs::create_dir_all(&themes).unwrap();
        let foreign = br#"{"name":"My Theme","base":"dark"}"#;
        fs::write(theme_file(&themes), foreign).unwrap();

        write_once(&themes, &settings, ColorScheme::Dark);

        assert_eq!(
            fs::read(theme_file(&themes)).unwrap(),
            foreign,
            "a foreign theme file must not be overwritten"
        );
    }

    #[test]
    fn write_overwrites_own_managed_file() {
        let (_root, themes, settings) = sandbox("theme-managed");
        write_once(&themes, &settings, ColorScheme::Dark); // ours (marker present)
        write_once(&themes, &settings, ColorScheme::Light);
        let v: Value = serde_json::from_slice(&fs::read(theme_file(&themes)).unwrap()).unwrap();
        assert_eq!(v["base"], "light", "our own managed file is fair game to update");
    }

    #[test]
    fn write_refuses_to_clobber_non_json_theme_file() {
        let (_root, themes, settings) = sandbox("theme-garbage");
        fs::create_dir_all(&themes).unwrap();
        let garbage = b"not json {{{";
        fs::write(theme_file(&themes), garbage).unwrap();

        write_once(&themes, &settings, ColorScheme::Dark);

        assert_eq!(
            fs::read(theme_file(&themes)).unwrap(),
            garbage,
            "an unparseable file must be left intact, never destroyed"
        );
    }

    // ---- path resolution ---------------------------------------------------

    #[test]
    fn themes_dir_ends_in_themes() {
        let d = themes_dir(Path::new("/home/u"), None);
        assert_eq!(d.file_name().unwrap(), "themes");
        assert_eq!(d, Path::new("/home/u/.claude/themes"));
    }

    #[test]
    fn themes_dir_honors_claude_config_dir() {
        // $CLAUDE_CONFIG_DIR (injected, not read from the real env) wins over
        // <home>/.claude.
        let d = themes_dir(Path::new("/home/u"), Some("/custom/cfg"));
        assert_eq!(d, Path::new("/custom/cfg/themes"));
        // An empty override is ignored (falls back to <home>/.claude).
        assert_eq!(themes_dir(Path::new("/home/u"), Some("")), Path::new("/home/u/.claude/themes"));
    }

    #[test]
    fn theme_settings_path_under_nice_dir() {
        let p = theme_settings_path(Path::new("/home/u"));
        assert_eq!(p.file_name().unwrap(), "claude-theme-settings-rs.json");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), ".nice");
    }

    #[test]
    fn settings_flag_path_in_ensures_pointer_and_returns_path() {
        let (_root, _themes, settings) = sandbox("theme-flag");
        let home = settings.parent().unwrap().parent().unwrap().to_path_buf();
        let path = settings_flag_path_in(&home).expect("ensure pointer file");
        assert_eq!(PathBuf::from(&path), theme_settings_path(&home));
        // Ensure-on-read: the pointer file now exists with the right bytes.
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "custom:nice-rs");
    }
}
