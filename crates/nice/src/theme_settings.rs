//! The theme store — the persisted appearance selection + its active-view
//! derivations, sharing `ui_settings.json` with R19's file-browser sort store.
//!
//! ## What lives here (R21 slice 1)
//!
//! * [`Appearance`] — the pure value type: `scheme` (light|dark), `sync_with_os`,
//!   `accent`, and the two per-scheme terminal-theme-id slots. Round-2 restyle
//!   plan 5 merged the chrome selection into the terminal theme, so there are no
//!   separate chrome-palette fields any more — the chrome half is resolved from
//!   the active terminal-theme id in [`ThemeState::from_stores`] (hand-tuned
//!   tables for the nice-default / catppuccin ids, derived from the terminal
//!   colors for every other theme). Its fresh-install defaults, its tolerant
//!   decode (Swift/`SortSettingsStore` fail-soft parity), and `active_terminal_id`
//!   are all pure functions with unit tables below.
//! * [`ThemeSettingsStore`] — the gpui `Global` wrapping an [`Appearance`] + the
//!   injected file path. `load(path)` is fail-soft to defaults; `set` persists
//!   the `appearance` section through the **shared read-merge-write writer**
//!   ([`crate::file_browser::sort_settings_store::write_ui_settings_merged`]) so a
//!   theme write never clobbers `file_browser_sort` (or any other co-writer's
//!   section). [`default_theme_settings_path`] honours
//!   `NICE_APPLICATION_SUPPORT_ROOT`, resolved from `app::run` ONLY.
//!
//! ## What does NOT live here (later R21 slices)
//!
//! The `ThemeState` / `SharedThemeState` resolved-view entity, the `apply_*`
//! mutators (they need the terminal fan-out), OS-appearance sync, and R17-live
//! Claude-sync wiring are slices 2/3. This slice is the store + the pure data +
//! the catalog resolution seam only.
//!
//! ## Persistence
//!
//! One top-level `"appearance"` object inside the shared `ui_settings.json`
//! (alongside R19's `file_browser_sort`; every OTHER top-level key is preserved
//! by read-merge-write). Snake_case keys; values are the `nice-theme` rawValues.
//! Tolerance: an absent section / field / unknown rawValue ⇒ that field's
//! default; malformed JSON ⇒ full defaults; the legacy `chrome_light_palette` /
//! `chrome_dark_palette` keys (including a `"macOS"` value) are ignored on read
//! (the merge dropped them — the terminal theme now drives the chrome).
//! `syncClaudeTheme` is NOT here — it stays the R17 CFPref.

#![allow(dead_code)] // Slice 2/3 (ThemeState + apply_* mutators) consume these.

use std::path::PathBuf;

use gpui::{
    AnyWindowHandle, App, AppContext, Entity, Global, WindowAppearance, WindowBackgroundAppearance,
};
use nice_term_view::{TerminalColor, TerminalTheme};
use nice_theme::color::Srgba;
use nice_theme::palette::{
    ColorScheme, Slots, CATPPUCCIN_LATTE, CATPPUCCIN_MOCHA, NICE_DARK, NICE_LIGHT,
};
use nice_theme::AccentPreset;
use serde::{Deserialize, Serialize};

use crate::terminal_theme_catalog::TerminalThemeCatalog;

/// The fresh-install terminal-theme id for the light slot. Restyle plan 3 flipped
/// this from `"catppuccin-latte"` to the Nice catalog id (registered in
/// `built_in_terminal_themes.rs`); the existing-user migration pins the LEGACY
/// `"catppuccin-latte"` as a literal (never through this constant).
const DEFAULT_TERMINAL_THEME_LIGHT_ID: &str = "nice-default-light";
/// The fresh-install terminal-theme id for the dark slot. Restyle plan 3 flipped
/// this from `"catppuccin-mocha"` to the Nice catalog id.
const DEFAULT_TERMINAL_THEME_DARK_ID: &str = "nice-default-dark";

// ---------------------------------------------------------------------------
// Transparency / blur bounds (restyle plan 3). The window-opacity slider runs
// 55–100% per scheme; the background-blur radius runs 0–60 px per scheme. Both
// are clamped on decode AND on every mutator so a hand-edited settings file or a
// stray slider value can never drive an out-of-range NSWindow state.
// ---------------------------------------------------------------------------

/// Minimum window-opacity percent (the slider floor — milkier than this reads as
/// unusable over busy wallpaper).
const MIN_WINDOW_OPACITY_PCT: u8 = 55;
/// Maximum window-opacity percent (100 ⇒ a fully opaque window).
const MAX_WINDOW_OPACITY_PCT: u8 = 100;
/// Fresh-install dark-scheme window opacity (plan: 80%).
const DEFAULT_WINDOW_OPACITY_DARK_PCT: u8 = 80;
/// Fresh-install light-scheme window opacity (plan: 90% — light goes milky at 80).
const DEFAULT_WINDOW_OPACITY_LIGHT_PCT: u8 = 90;
/// Maximum background-blur radius in px (the slider ceiling).
const MAX_BLUR_RADIUS: u16 = 60;
/// Fresh-install background-blur radius for both schemes (plan: 30 px).
const DEFAULT_BLUR_RADIUS: u16 = 30;

/// Clamp a raw window-opacity percent into the slider's [55, 100] range.
fn clamp_window_opacity_pct(pct: u8) -> u8 {
    pct.clamp(MIN_WINDOW_OPACITY_PCT, MAX_WINDOW_OPACITY_PCT)
}

/// Clamp a raw blur radius into the slider's [0, 60] px range.
fn clamp_blur_radius(radius: u16) -> u16 {
    radius.min(MAX_BLUR_RADIUS)
}

/// Resolve `(opacity_pct, blur_radius)` for the active scheme into the
/// [`WindowBackgroundAppearance`] gpui should paint (plan Rendering §):
/// opacity 100% ⇒ `Opaque`; opacity < 100% && blur > 0 ⇒ `Blurred`;
/// opacity < 100% && blur == 0 ⇒ `Transparent` (blur degrades to plain
/// transparency when the radius is zeroed).
fn window_background_appearance(opacity_pct: u8, blur_radius: u16) -> WindowBackgroundAppearance {
    if opacity_pct >= MAX_WINDOW_OPACITY_PCT {
        WindowBackgroundAppearance::Opaque
    } else if blur_radius > 0 {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Transparent
    }
}

/// The persisted appearance selection — the raw user choice, before any active
/// derivation. A pure value type: no gpui, no I/O. Ported from the `Tweaks`
/// store's persisted axes (`Tweaks.swift:202-402`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    /// The active color scheme. When `sync_with_os`, OS reconciliation
    /// (slice 3) pins this to the system setting.
    pub scheme: ColorScheme,
    /// Whether `scheme` follows the OS appearance.
    pub sync_with_os: bool,
    /// The accent swatch (palette-agnostic).
    pub accent: AccentPreset,
    /// The terminal-theme id active when `scheme == Light` (resolved via the
    /// catalog; unknown ⇒ Nice default).
    pub terminal_theme_light_id: String,
    /// The terminal-theme id active when `scheme == Dark`.
    pub terminal_theme_dark_id: String,
    /// Window opacity percent (55–100) applied to the surface fills when
    /// `scheme == Light` (restyle plan 3).
    pub window_opacity_light: u8,
    /// Window opacity percent (55–100) applied to the surface fills when
    /// `scheme == Dark`.
    pub window_opacity_dark: u8,
    /// Background-blur radius in px (0–60) applied when the window is translucent
    /// and `scheme == Light`.
    pub blur_radius_light: u16,
    /// Background-blur radius in px (0–60) applied when the window is translucent
    /// and `scheme == Dark`.
    pub blur_radius_dark: u16,
    /// The one-time restyle-popup flag (plan 3 migration). Set once the popup has
    /// been shown; this slice only persists/round-trips it — slice 5 owns the
    /// migration + popup logic that reads and writes it.
    pub restyle_popup_shown: bool,
}

impl Default for Appearance {
    /// Fresh-install defaults (dossier §2.1, restyle plan 3 flip). `scheme` is a
    /// placeholder here: with `sync_with_os == true` the store's OS reconcile
    /// (slice 3) pins it to the system appearance at launch, so the fixed value
    /// below is only what a sync-off fresh install would show. `Dark` matches
    /// today's hardcoded look. Restyle plan 3 flipped the chrome palette
    /// (Catppuccin → Nice) and the accent (Ocean → Terracotta) so a fresh install
    /// renders the approved mock; the existing-user migration pins the pre-flip
    /// look as literals (never derived from here).
    fn default() -> Self {
        Self {
            scheme: ColorScheme::Dark,
            sync_with_os: true,
            accent: AccentPreset::Terracotta,
            terminal_theme_light_id: DEFAULT_TERMINAL_THEME_LIGHT_ID.to_string(),
            terminal_theme_dark_id: DEFAULT_TERMINAL_THEME_DARK_ID.to_string(),
            window_opacity_light: DEFAULT_WINDOW_OPACITY_LIGHT_PCT,
            window_opacity_dark: DEFAULT_WINDOW_OPACITY_DARK_PCT,
            blur_radius_light: DEFAULT_BLUR_RADIUS,
            blur_radius_dark: DEFAULT_BLUR_RADIUS,
            restyle_popup_shown: false,
        }
    }
}

impl Appearance {
    /// The active scheme.
    pub fn active_scheme(&self) -> ColorScheme {
        self.scheme
    }

    /// The active accent (palette-agnostic).
    pub fn active_accent(&self) -> AccentPreset {
        self.accent
    }

    /// The terminal-theme id for `scheme` (the RAW persisted id; resolve it via
    /// [`crate::terminal_theme_catalog::TerminalThemeCatalog::resolve`]).
    pub fn terminal_theme_id_for(&self, scheme: ColorScheme) -> &str {
        match scheme {
            ColorScheme::Light => &self.terminal_theme_light_id,
            ColorScheme::Dark => &self.terminal_theme_dark_id,
        }
    }

    /// The terminal-theme id active for the current `scheme`.
    pub fn active_terminal_id(&self) -> &str {
        self.terminal_theme_id_for(self.scheme)
    }

    /// The window-opacity percent (55–100) for `scheme`.
    pub fn window_opacity_pct_for(&self, scheme: ColorScheme) -> u8 {
        match scheme {
            ColorScheme::Light => self.window_opacity_light,
            ColorScheme::Dark => self.window_opacity_dark,
        }
    }

    /// The window-opacity percent for the active `scheme`.
    pub fn active_window_opacity_pct(&self) -> u8 {
        self.window_opacity_pct_for(self.scheme)
    }

    /// The active window opacity as a 0.55–1.0 alpha fraction (the surface-fill
    /// alpha).
    pub fn active_window_opacity(&self) -> f32 {
        f32::from(self.active_window_opacity_pct()) / 100.0
    }

    /// The background-blur radius (px, 0–60) for `scheme`.
    pub fn blur_radius_for(&self, scheme: ColorScheme) -> u16 {
        match scheme {
            ColorScheme::Light => self.blur_radius_light,
            ColorScheme::Dark => self.blur_radius_dark,
        }
    }

    /// The background-blur radius for the active `scheme`.
    pub fn active_blur_radius(&self) -> u16 {
        self.blur_radius_for(self.scheme)
    }

    /// The [`WindowBackgroundAppearance`] gpui should paint for the active scheme
    /// (Opaque / Transparent / Blurred per the opacity + blur rules).
    pub fn active_window_appearance(&self) -> WindowBackgroundAppearance {
        window_background_appearance(self.active_window_opacity_pct(), self.active_blur_radius())
    }
}

/// Convert a terminal-theme [`TerminalColor`] (8-bit sRGB) to the nice-theme
/// [`Srgba`] the chrome-derivation seam consumes — the app-boundary conversion
/// the crate layering keeps out of `nice-theme` (`nice-theme` must not depend on
/// `nice-term-view`). Alpha is implicitly opaque.
fn terminal_color_to_srgba(c: TerminalColor) -> Srgba {
    Srgba::rgb(
        f32::from(c.r) / 255.0,
        f32::from(c.g) / 255.0,
        f32::from(c.b) / 255.0,
    )
}

/// The chrome [`Slots`] for a merged theme (round-2 restyle plan 5): the chrome
/// half is keyed off the active terminal-theme `id`, resolved against the
/// active `scheme` and the already-resolved [`TerminalTheme`].
///
/// * `nice-default-light` / `nice-default-dark` → the hand-tuned
///   [`NICE_LIGHT`] / [`NICE_DARK`] tables ("Nice pairs with Nice").
/// * `catppuccin-latte` / `catppuccin-mocha` → the hand-tuned
///   [`CATPPUCCIN_LATTE`] / [`CATPPUCCIN_MOCHA`] tables.
/// * every OTHER built-in (solarized/dracula/nord/gruvbox/tokyo-night/one-dark)
///   AND every imported Ghostty theme → chrome DERIVED from the resolved
///   terminal theme's foreground/background via
///   [`nice_theme::derive_chrome`].
///
/// The four hand-tuned ids are matched together with their natural scheme, so a
/// slot holding a scheme-mismatched hand-tuned id (which the terminal catalog
/// resolves via its own scheme fallback) falls through to derivation from the
/// theme that actually resolved — never a hand-tuned table for the wrong scheme.
fn chrome_slots_for_theme(id: &str, scheme: ColorScheme, terminal: &TerminalTheme) -> Slots {
    match (id, scheme) {
        ("nice-default-light", ColorScheme::Light) => NICE_LIGHT,
        ("nice-default-dark", ColorScheme::Dark) => NICE_DARK,
        ("catppuccin-latte", ColorScheme::Light) => CATPPUCCIN_LATTE,
        ("catppuccin-mocha", ColorScheme::Dark) => CATPPUCCIN_MOCHA,
        _ => nice_theme::derive_chrome(
            terminal_color_to_srgba(terminal.foreground),
            terminal_color_to_srgba(terminal.background),
        ),
    }
}

/// Map a persisted accent rawValue to an [`AccentPreset`]. Unknown ⇒ `None`.
fn accent_from_raw(raw: &str) -> Option<AccentPreset> {
    AccentPreset::ALL.into_iter().find(|a| a.raw_value() == raw)
}

/// Map a persisted scheme rawValue (`"light"` / `"dark"`) to a [`ColorScheme`].
/// Unknown ⇒ `None`. Mirrors `Tweaks.encodeScheme` (`Tweaks.swift:641-645`).
fn scheme_from_raw(raw: &str) -> Option<ColorScheme> {
    match raw {
        "light" => Some(ColorScheme::Light),
        "dark" => Some(ColorScheme::Dark),
        _ => None,
    }
}

/// The rawValue for a [`ColorScheme`] (`"light"` / `"dark"`).
fn scheme_raw(scheme: ColorScheme) -> &'static str {
    match scheme {
        ColorScheme::Light => "light",
        ColorScheme::Dark => "dark",
    }
}

/// The on-disk `"appearance"` section shape. Every field is optional so a
/// missing / unknown field falls through to the [`Appearance`] default (tolerant
/// decode). Serialized with the current selection; decoded permissively. The
/// legacy `chrome_light_palette` / `chrome_dark_palette` keys (round-1) are NOT
/// fields here, so serde ignores them on read and never writes them — the
/// round-2 merge folded the chrome selection into the terminal theme.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AppearanceSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sync_with_os: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_theme_light_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_theme_dark_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_opacity_light: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_opacity_dark: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blur_radius_light: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blur_radius_dark: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restyle_popup_shown: Option<bool>,
}

/// Just the `appearance` key of the shared doc, for tolerant decode. Other
/// top-level keys are ignored on read (serde default) and preserved on write
/// (read-merge-write), so this never needs a flatten catch-all.
#[derive(Debug, Default, Deserialize)]
struct DocForAppearance {
    #[serde(default)]
    appearance: Option<AppearanceSection>,
}

impl AppearanceSection {
    /// Serialize the full current selection (every field present).
    fn from_appearance(a: &Appearance) -> Self {
        Self {
            scheme: Some(scheme_raw(a.scheme).to_string()),
            sync_with_os: Some(a.sync_with_os),
            accent: Some(a.accent.raw_value().to_string()),
            terminal_theme_light_id: Some(a.terminal_theme_light_id.clone()),
            terminal_theme_dark_id: Some(a.terminal_theme_dark_id.clone()),
            window_opacity_light: Some(a.window_opacity_light),
            window_opacity_dark: Some(a.window_opacity_dark),
            blur_radius_light: Some(a.blur_radius_light),
            blur_radius_dark: Some(a.blur_radius_dark),
            restyle_popup_shown: Some(a.restyle_popup_shown),
        }
    }

    /// Decode into an [`Appearance`], filling every absent / unknown field with the
    /// fresh-install default (tolerant, Swift fail-soft parity).
    fn into_appearance(self) -> Appearance {
        self.into_appearance_with_defaults(&Appearance::default())
    }

    /// Decode into an [`Appearance`], filling every absent / unknown field from an
    /// explicit `defaults` source. The tolerant decode ([`into_appearance`]) passes
    /// the fresh-install defaults; the existing-user migration
    /// ([`legacy_pinned_appearance`]) passes the LEGACY (pre-flip) literals so an
    /// absent key materializes to the user's pre-restyle look, NOT the flipped
    /// fresh-install default.
    fn into_appearance_with_defaults(self, d: &Appearance) -> Appearance {
        Appearance {
            scheme: self
                .scheme
                .as_deref()
                .and_then(scheme_from_raw)
                .unwrap_or(d.scheme),
            sync_with_os: self.sync_with_os.unwrap_or(d.sync_with_os),
            accent: self
                .accent
                .as_deref()
                .and_then(accent_from_raw)
                .unwrap_or(d.accent),
            terminal_theme_light_id: self
                .terminal_theme_light_id
                .unwrap_or_else(|| d.terminal_theme_light_id.clone()),
            terminal_theme_dark_id: self
                .terminal_theme_dark_id
                .unwrap_or_else(|| d.terminal_theme_dark_id.clone()),
            window_opacity_light: self
                .window_opacity_light
                .map(clamp_window_opacity_pct)
                .unwrap_or(d.window_opacity_light),
            window_opacity_dark: self
                .window_opacity_dark
                .map(clamp_window_opacity_pct)
                .unwrap_or(d.window_opacity_dark),
            blur_radius_light: self
                .blur_radius_light
                .map(clamp_blur_radius)
                .unwrap_or(d.blur_radius_light),
            blur_radius_dark: self
                .blur_radius_dark
                .map(clamp_blur_radius)
                .unwrap_or(d.blur_radius_dark),
            restyle_popup_shown: self.restyle_popup_shown.unwrap_or(d.restyle_popup_shown),
        }
    }
}

// ---------------------------------------------------------------------------
// Restyle plan 3 — existing-user migration (the one-time popup's pinning step).
// The defaults-flip above changes what an ABSENT key resolves to, so an existing
// user riding the old defaults would be silently restyled. Before offering the
// new look we materialize their pre-flip look into explicit keys, writing the
// LEGACY defaults as LITERAL values wherever a key is absent — NEVER derived from
// `Appearance::default()` or the `DEFAULT_TERMINAL_THEME_*` constants (this plan
// flipped those). Keys the user explicitly set are left untouched.
// ---------------------------------------------------------------------------

/// The LEGACY (pre-restyle-flip) appearance defaults, as LITERAL values. These are
/// the exact fresh-install values BEFORE this plan flipped them, so pinning an
/// absent key to one of these reproduces an existing user's pre-flip look. Kept
/// separate from [`Appearance::default`] on purpose: that function now returns the
/// NEW defaults, so deriving the pins from it would restyle the very users this
/// step protects. `scheme` is NOT pinned from here — the caller preserves the live
/// OS-reconciled scheme (the flip does not change the scheme axis).
fn legacy_appearance_defaults() -> Appearance {
    Appearance {
        scheme: ColorScheme::Dark,
        sync_with_os: true,
        accent: AccentPreset::Ocean,
        terminal_theme_light_id: "catppuccin-latte".to_string(),
        terminal_theme_dark_id: "catppuccin-mocha".to_string(),
        window_opacity_light: 100,
        window_opacity_dark: 100,
        blur_radius_light: 0,
        blur_radius_dark: 0,
        restyle_popup_shown: false,
    }
}

/// Materialize an existing user's ABSENT appearance keys as the LEGACY (pre-flip)
/// literal defaults, leaving explicitly-set keys untouched, so declining the
/// restyle popup changes nothing. Reads the RAW `appearance` section (absent-ness
/// survives — the loaded store already filled absents with the NEW defaults, so it
/// can't be used here). `current_scheme` fills an absent `scheme` (the flip does
/// not touch the scheme axis, so the live OS-reconciled scheme is preserved rather
/// than pinned to a stale literal).
fn legacy_pinned_appearance(bytes: &[u8], current_scheme: ColorScheme) -> Appearance {
    let mut defaults = legacy_appearance_defaults();
    defaults.scheme = current_scheme;
    let section = serde_json::from_slice::<DocForAppearance>(bytes)
        .ok()
        .and_then(|doc| doc.appearance)
        .unwrap_or_default();
    section.into_appearance_with_defaults(&defaults)
}

/// Decode the `appearance` section from a raw `ui_settings.json` byte buffer.
/// Malformed JSON or an absent section ⇒ full defaults (never an error).
fn decode_appearance(bytes: &[u8]) -> Appearance {
    match serde_json::from_slice::<DocForAppearance>(bytes) {
        Ok(doc) => doc
            .appearance
            .map(AppearanceSection::into_appearance)
            .unwrap_or_default(),
        Err(_) => Appearance::default(),
    }
}

/// The process-wide theme store: the current [`Appearance`] + the injected file
/// path. A gpui `Global` (mirrors [`crate::file_browser::sort_settings_store::SortSettingsStore`]).
pub struct ThemeSettingsStore {
    path: PathBuf,
    appearance: Appearance,
}

impl Global for ThemeSettingsStore {}

impl ThemeSettingsStore {
    /// Load from `path`. A missing or malformed file ⇒ fresh-install defaults,
    /// never an error (fail-soft, Swift parity).
    pub fn load(path: PathBuf) -> Self {
        let appearance = match std::fs::read(&path) {
            Ok(bytes) => decode_appearance(&bytes),
            Err(_) => Appearance::default(),
        };
        Self { path, appearance }
    }

    /// Construct with fresh-install defaults at `path` WITHOUT touching disk —
    /// the `run_selftest` seam (defaults + a temp path; the launch-time read /
    /// default-path resolution stays in `app::run`, per hermeticity).
    pub fn with_defaults(path: PathBuf) -> Self {
        Self {
            path,
            appearance: Appearance::default(),
        }
    }

    /// The current raw persisted selection.
    pub fn appearance(&self) -> &Appearance {
        &self.appearance
    }

    /// The injected file path (test hook).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    // --- Active-view derivations (delegate to `Appearance`) --------------------

    /// The active color scheme.
    pub fn active_scheme(&self) -> ColorScheme {
        self.appearance.active_scheme()
    }

    /// The active accent.
    pub fn active_accent(&self) -> AccentPreset {
        self.appearance.active_accent()
    }

    /// The active terminal-theme id (resolve via the catalog).
    pub fn active_terminal_id(&self) -> &str {
        self.appearance.active_terminal_id()
    }

    /// The active window opacity as a 0.55–1.0 surface-fill alpha.
    pub fn active_window_opacity(&self) -> f32 {
        self.appearance.active_window_opacity()
    }

    /// The active background-blur radius (px) as a `u32` (gpui's setter type).
    pub fn active_blur_radius(&self) -> u32 {
        u32::from(self.appearance.active_blur_radius())
    }

    /// The [`WindowBackgroundAppearance`] to paint for the active scheme.
    pub fn active_window_appearance(&self) -> WindowBackgroundAppearance {
        self.appearance.active_window_appearance()
    }

    /// The one-time restyle-popup flag (slice 5 reads/writes it; stored here).
    pub fn restyle_popup_shown(&self) -> bool {
        self.appearance.restyle_popup_shown
    }

    /// Replace the selection and persist the `appearance` section through the
    /// shared read-merge-write writer **only if it changed**. Returns `Ok(true)`
    /// when a disk write happened, `Ok(false)` when unchanged. The write reads the
    /// current file, overwrites ONLY the `appearance` key, and atomically
    /// rewrites — so `file_browser_sort` and every other co-writer's section
    /// survive (OQ3). Slice 3's `apply_*` mutators call this, then fan out.
    pub fn set(&mut self, appearance: Appearance) -> std::io::Result<bool> {
        if appearance == self.appearance {
            return Ok(false);
        }
        self.appearance = appearance;
        self.persist()?;
        Ok(true)
    }

    /// Reconcile the in-memory `scheme` with the OS appearance WITHOUT persisting —
    /// the boot-time seed (`install_live_theme` mints the first [`ThemeState`] from
    /// the reconciled selection). A no-op unless `sync_with_os` is on and the stored
    /// scheme differs; a later user change persists through [`set`](Self::set).
    /// Returns whether the scheme flipped. Keeping this off-disk avoids a boot write
    /// on every OS-scheme mismatch (the runtime `reconcile_with_os` DOES persist,
    /// since that reflects a live OS change the user expects saved).
    pub fn reconcile_scheme_in_memory(&mut self, os: ColorScheme) -> bool {
        if !self.appearance.sync_with_os || self.appearance.scheme == os {
            return false;
        }
        self.appearance.scheme = os;
        true
    }

    /// Write the current `appearance` section through the shared merged writer.
    fn persist(&self) -> std::io::Result<()> {
        let section = AppearanceSection::from_appearance(&self.appearance);
        crate::file_browser::sort_settings_store::write_ui_settings_merged(&self.path, |map| {
            map.insert(
                "appearance".to_string(),
                serde_json::to_value(section).expect("AppearanceSection serializes"),
            );
        })
    }
}

/// Resolve the theme store's `ui_settings.json` path — the **same** shared file
/// as R19's sort store ([`crate::file_browser::sort_settings_store::default_ui_settings_path`]),
/// so `<support-root>/<variant>/ui_settings.json` with `<support-root>` from
/// `NICE_APPLICATION_SUPPORT_ROOT` when set else `~/Library/Application Support`.
/// Called from `app::run` ONLY — never a test or `run_selftest` (hermeticity).
pub fn default_theme_settings_path() -> PathBuf {
    crate::file_browser::sort_settings_store::default_ui_settings_path()
}

// ---------------------------------------------------------------------------
// The live resolved view state (`ThemeState`) + the process entity that carries
// it (`SharedThemeState`), plus the chrome/terminal read accessors. This is the
// R21 slice-2 fan-out source: chrome views read the active `Slots`/accent from
// the process entity AT RENDER TIME (mirroring the `SharedFontSettings` idiom,
// `keymap.rs:100`), and `build_window_root` seeds each new terminal pane with the
// active `TerminalTheme`/accent. The store's `apply_*` mutators (slice 3) refresh
// this entity (re-derive from the store + catalog) and then fan out.
// ---------------------------------------------------------------------------

/// The resolved active view state — what chrome + panes actually paint this
/// moment, derived from the [`ThemeSettingsStore`]'s selection through the
/// [`TerminalThemeCatalog`]. Held as one process [`Entity`] inside
/// [`SharedThemeState`]. All fields are already macOS-substituted / resolved, so a
/// consumer never touches an unpaintable palette or an unknown terminal id.
#[derive(Clone)]
pub struct ThemeState {
    /// The active chrome slot table — every slot a concrete sRGB literal
    /// (hand-tuned for the nice-default / catppuccin ids, otherwise derived from
    /// the terminal theme's fg/bg via [`nice_theme::derive_chrome`]).
    pub slots: Slots,
    /// The active color scheme.
    pub scheme: ColorScheme,
    /// The active accent as a concrete sRGB color (the caret / selection / logo
    /// tint).
    pub accent: Srgba,
    /// The active terminal render theme (the catalog-resolved
    /// [`nice_term_view::TerminalTheme`]; unknown id ⇒ the Nice default per
    /// scheme).
    pub terminal_theme: nice_term_view::TerminalTheme,
    /// The active-scheme window-opacity surface-fill alpha (0.55–1.0). Applied to
    /// the window-body backing + the terminal grid's default-background surface;
    /// text / explicit-bg cells / selection / cursor stay opaque on top.
    pub background_opacity: f32,
    /// The active-scheme background-blur radius (px) fed to
    /// `Window::set_background_blur_radius`.
    pub blur_radius: u32,
    /// The active-scheme [`WindowBackgroundAppearance`] (Opaque / Transparent /
    /// Blurred) pushed to each window via `Window::set_background_appearance`.
    pub window_appearance: WindowBackgroundAppearance,
}

impl ThemeState {
    /// Derive the resolved view state from a store selection + the catalog. Slice
    /// 3's `apply_*` mutators call this after each store mutation to refresh the
    /// [`SharedThemeState`] entity, then fan out.
    pub fn from_stores(store: &ThemeSettingsStore, catalog: &TerminalThemeCatalog) -> Self {
        let scheme = store.active_scheme();
        let terminal_id = store.active_terminal_id();
        // Resolve the terminal half once, then key the chrome half off the same
        // id + resolved theme (round-2 merge: one theme drives both halves).
        let terminal_theme = catalog.resolve(terminal_id, scheme);
        let slots = chrome_slots_for_theme(terminal_id, scheme, &terminal_theme);
        Self {
            slots,
            scheme,
            accent: store.active_accent().color(),
            terminal_theme,
            background_opacity: store.active_window_opacity(),
            blur_radius: store.active_blur_radius(),
            window_appearance: store.active_window_appearance(),
        }
    }
}

/// The process-wide live theme entity (mirrors `SharedFontSettings`). Present
/// only after [`install_live_theme`] (the shipped `app::run`). When ABSENT — every
/// self-test scenario that does not opt into live theming, and any unit `#[test]`
/// — the chrome accessors fall back to the shipped Nice/Dark + Terracotta look, so
/// nothing changes for those paths (the R18 `Global-absent ⇒ default` discipline).
pub struct SharedThemeState(pub Entity<ThemeState>);

impl Global for SharedThemeState {}

/// The chrome slot table chrome views paint this frame: the live
/// [`SharedThemeState`] when installed, else the shipped Nice/Dark fallback (so
/// scenarios / tests without the global render exactly as before R21).
pub fn active_chrome_slots(cx: &App) -> Slots {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => shared.0.read(cx).slots,
        None => fallback_chrome_slots(),
    }
}

/// The active color scheme chrome views resolve over-glass primitives against
/// this frame (the flat sidebar's [`glass_line`](nice_theme::glass_line) /
/// [`glass_fill`](nice_theme::glass_fill)): the live [`SharedThemeState`] when
/// installed, else the shipped Nice/Dark fallback scheme (so scenarios / tests
/// without the global resolve the dark over-glass values).
pub fn active_chrome_scheme(cx: &App) -> ColorScheme {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => shared.0.read(cx).scheme,
        None => ColorScheme::Dark,
    }
}

/// The accent color chrome views tint with this frame: the live
/// [`SharedThemeState`] when installed, else the shipped Terracotta fallback.
pub fn active_chrome_accent(cx: &App) -> Srgba {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => shared.0.read(cx).accent,
        None => AccentPreset::Terracotta.color(),
    }
}

/// The `(terminal theme, accent)` a freshly-built terminal pane is seeded with —
/// read by `build_window_root` / the pane host. Live [`SharedThemeState`] when
/// installed, else the shipped `nice_default_dark` + Terracotta pair (unchanged
/// pre-R21 look for scenarios without the global).
pub fn active_terminal_theme_and_accent(cx: &App) -> (nice_term_view::TerminalTheme, Srgba) {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => {
            let state = shared.0.read(cx);
            (state.terminal_theme.clone(), state.accent)
        }
        None => (
            nice_term_view::TerminalTheme::nice_default_dark(),
            AccentPreset::Terracotta.color(),
        ),
    }
}

/// The active-scheme window-opacity surface-fill alpha (0.55–1.0) the window-body
/// backing + terminal grid paint their default background at: the live
/// [`SharedThemeState`] when installed, else `1.0` (fully opaque — scenarios /
/// tests without the global render exactly as before, so pixel assertions hold).
pub fn active_window_opacity(cx: &App) -> f32 {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => shared.0.read(cx).background_opacity,
        None => 1.0,
    }
}

/// The active-scheme [`WindowBackgroundAppearance`] each window should paint: the
/// live [`SharedThemeState`] when installed, else `Opaque` (the pre-restyle
/// window).
pub fn active_window_appearance(cx: &App) -> WindowBackgroundAppearance {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => shared.0.read(cx).window_appearance,
        None => WindowBackgroundAppearance::Opaque,
    }
}

/// The active-scheme background-blur radius (px): the live [`SharedThemeState`]
/// when installed, else `0`.
pub fn active_blur_radius(cx: &App) -> u32 {
    match cx.try_global::<SharedThemeState>() {
        Some(shared) => shared.0.read(cx).blur_radius,
        None => 0,
    }
}

/// Push the active window transparency (appearance + numeric blur radius) into a
/// single freshly-built window. Called from `build_window_root` after the window
/// exists so the first paint already reflects the stored per-scheme opacity/blur
/// (no Opaque→translucent flash). The runtime fan-out
/// ([`apply_window_transparency_fanout`]) reaches every window on a slider change.
pub fn apply_window_transparency(cx: &App, window: &mut gpui::Window) {
    // Set the numeric radius first, then the appearance: the macOS patch's
    // `set_background_appearance` reads the stored `blur_radius`, so ordering it
    // first means the very first `Blurred` apply already carries the right radius.
    window.set_background_blur_radius(active_blur_radius(cx));
    window.set_background_appearance(active_window_appearance(cx));
}

/// The shipped Nice/Dark chrome table — the fallback when [`SharedThemeState`] is
/// absent. Kept out of the chrome files so no live render path hardcodes the
/// literal [`NICE_DARK`] table (Validation §3 hardcode grep).
fn fallback_chrome_slots() -> Slots {
    NICE_DARK
}

/// Install the live theme globals for the shipped app (`app::run` ONLY, before
/// the first window): the production [`OsSchemeSource`], the loaded `store` (its
/// scheme reconciled to the OS appearance in memory for a sync-on install), the
/// terminal-theme catalog (the full built-in table + the imported files
/// enumerated from `terminal_themes_dir`, resolved by `app::run` under the
/// `NICE_APPLICATION_SUPPORT_ROOT` convention), and the [`SharedThemeState`]
/// entity minted from them. The per-window `Window::observe_window_appearance`
/// adapter (wired in `build_window_root`) and the R17-live Claude write ride on
/// top of this.
pub fn install_live_theme(
    cx: &mut App,
    mut store: ThemeSettingsStore,
    terminal_themes_dir: PathBuf,
) {
    // Register the production OS-scheme source first, then reconcile the loaded
    // selection to the current OS appearance in memory (a sync-on fresh install
    // adopts the system scheme — the `scheme = OS at first launch` default) BEFORE
    // the first `ThemeState` derives. No boot write (the runtime reconcile persists;
    // this seed does not).
    install_production_os_scheme_source(cx);
    if let Some(os) = current_os_scheme(cx) {
        store.reconcile_scheme_in_memory(os);
    }
    let catalog = TerminalThemeCatalog::new(terminal_themes_dir);
    let entity = cx.new(|_| ThemeState::from_stores(&store, &catalog));
    cx.set_global(store);
    cx.set_global(catalog);
    cx.set_global(SharedThemeState(entity));
}

// ---------------------------------------------------------------------------
// OS-appearance sync (OQ1). An injectable `OsSchemeSource` (the Swift
// `osSchemeProvider` analog) feeds `reconcile_with_os`; production reads gpui's
// on-demand window appearance, tests/scenarios inject a stub they can flip. R21
// does NOT pin `NSWindow.appearance` (macOS palette deferred, OQ6), so gpui's
// reported appearance equals the system setting — the exact input `sync_with_os`
// needs.
// ---------------------------------------------------------------------------

/// Injectable source of the current OS color scheme (Swift `osSchemeProvider`). A
/// gpui `Global`: production installs a closure reading gpui's window appearance;
/// tests / the `theme-fanout` scenario inject a stub over a flippable cell so the
/// sync LOGIC is exercised without reading the real system appearance.
pub struct OsSchemeSource(Box<dyn Fn(&App) -> ColorScheme>);

impl Global for OsSchemeSource {}

impl OsSchemeSource {
    /// Build a source from a scheme-reading closure.
    pub fn new(read: impl Fn(&App) -> ColorScheme + 'static) -> Self {
        Self(Box::new(read))
    }

    /// Read the current OS scheme.
    pub fn read(&self, cx: &App) -> ColorScheme {
        (self.0)(cx)
    }
}

/// Map gpui's [`WindowAppearance`] onto the two-way scheme:
/// `{Light, VibrantLight} → Light`, `{Dark, VibrantDark} → Dark` (OQ1 / dossier
/// §3.2).
pub fn map_window_appearance(appearance: WindowAppearance) -> ColorScheme {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ColorScheme::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ColorScheme::Dark,
    }
}

/// The current OS scheme from the injected [`OsSchemeSource`], or `None` when no
/// source is installed (a unit test driving the pure store).
pub fn current_os_scheme(cx: &App) -> Option<ColorScheme> {
    cx.try_global::<OsSchemeSource>().map(|s| s.read(cx))
}

/// Install the production OS-scheme source — reads gpui's on-demand
/// [`App::window_appearance`], objc2-free. `app::run` ONLY (via
/// [`install_live_theme`]); tests/scenarios inject their own stub.
fn install_production_os_scheme_source(cx: &mut App) {
    cx.set_global(OsSchemeSource::new(|cx| {
        map_window_appearance(cx.window_appearance())
    }));
}

/// Reconcile the live selection with `os`: a no-op unless `sync_with_os`; when on
/// and the scheme differs, flip `scheme` to `os` (cascading chrome + the active
/// per-scheme terminal slot), persist, and fan out (chrome + panes + Claude). The
/// production `Window::observe_window_appearance` adapter and
/// [`apply_sync_with_os`]`(cx, true)` both call it. No-op when the store Global is
/// absent.
pub fn reconcile_with_os(cx: &mut App, os: ColorScheme) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    if !appearance.sync_with_os || appearance.scheme == os {
        return;
    }
    appearance.scheme = os;
    commit_appearance(cx, appearance);
}

// ---------------------------------------------------------------------------
// The live `apply_*` mutators (Exported contracts). Each reads the current
// selection, mutates it, then routes through `commit_appearance`: persist
// (only-if-changed) → refresh the resolved `ThemeState` → fan chrome + panes out →
// mirror to Claude when the sync gate is on. Free functions (not `&mut self`
// methods) because the store lives inside `App` as a `Global` — the mutation needs
// `&mut App` to also touch `SharedThemeState`, `WindowRegistry`, and the gate.
// ---------------------------------------------------------------------------

/// The current raw selection, or `None` when the store Global is absent.
fn current_appearance(cx: &App) -> Option<Appearance> {
    cx.try_global::<ThemeSettingsStore>()
        .map(|s| s.appearance().clone())
}

/// Set the active color scheme. A manual pick that CONTRADICTS the OS turns
/// `sync_with_os` off (the `userPicked` analog, `Tweaks.swift:495-500`): once the
/// user overrides the system appearance, Nice stops following it.
pub fn apply_scheme(cx: &mut App, scheme: ColorScheme) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    if appearance.sync_with_os {
        if let Some(os) = current_os_scheme(cx) {
            if scheme != os {
                appearance.sync_with_os = false;
            }
        }
    }
    appearance.scheme = scheme;
    commit_appearance(cx, appearance);
}

/// Set the accent (palette-agnostic): recolors the caret on cursor-None terminal
/// themes plus the chrome / selection / logo tint.
pub fn apply_accent(cx: &mut App, accent: AccentPreset) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    appearance.accent = accent;
    commit_appearance(cx, appearance);
}

/// Set the terminal-theme id for one scheme's slot. A change to the INACTIVE
/// scheme's slot is LATENT — persisted now, applied on the next scheme flip
/// (`AppShellView.swift:557-571`); the active slot recolors every pane immediately.
pub fn apply_terminal_theme_id(cx: &mut App, scheme: ColorScheme, id: &str) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    match scheme {
        ColorScheme::Light => appearance.terminal_theme_light_id = id.to_string(),
        ColorScheme::Dark => appearance.terminal_theme_dark_id = id.to_string(),
    }
    commit_appearance(cx, appearance);
}

/// Set the window opacity (percent, clamped 55–100) for one scheme's slot. The
/// active slot re-composites every surface fill immediately (and re-applies the
/// window `WindowBackgroundAppearance`, since crossing 100% flips Opaque↔
/// translucent); a change to the INACTIVE scheme's slot is latent until a scheme
/// flip. The slice-4 opacity slider drives this.
pub fn apply_window_opacity(cx: &mut App, scheme: ColorScheme, pct: u8) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    let pct = clamp_window_opacity_pct(pct);
    match scheme {
        ColorScheme::Light => appearance.window_opacity_light = pct,
        ColorScheme::Dark => appearance.window_opacity_dark = pct,
    }
    commit_appearance(cx, appearance);
}

/// Set the background-blur radius (px, clamped 0–60) for one scheme's slot. When
/// the active scheme is translucent this re-applies the window appearance (a
/// radius of 0 degrades `Blurred`→`Transparent`) and pushes the numeric radius to
/// the WindowServer; a change to the INACTIVE scheme's slot is latent. The
/// slice-4 blur slider drives this.
pub fn apply_blur_radius(cx: &mut App, scheme: ColorScheme, radius: u16) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    let radius = clamp_blur_radius(radius);
    match scheme {
        ColorScheme::Light => appearance.blur_radius_light = radius,
        ColorScheme::Dark => appearance.blur_radius_dark = radius,
    }
    commit_appearance(cx, appearance);
}

/// Persist the one-time restyle-popup flag (slice 5's migration owns the WHEN;
/// this slice owns the stored key + the setter). Routes through the shared commit
/// so it round-trips like any other appearance key; the redundant fan-out it
/// triggers is a cheap repaint with no visual change.
pub fn apply_restyle_popup_shown(cx: &mut App, shown: bool) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    appearance.restyle_popup_shown = shown;
    commit_appearance(cx, appearance);
}

/// Pin an existing user's pre-flip appearance look into explicit keys (restyle
/// plan 3 migration step 1). Reads the store's on-disk `appearance` section RAW,
/// materializes every ABSENT key to the LEGACY literal default (via
/// [`legacy_pinned_appearance`]), and commits it — so the defaults-flip does not
/// silently restyle a user riding the old defaults, and declining the popup
/// changes nothing. Keys the user explicitly set survive untouched; the live
/// OS-reconciled scheme is preserved. No-op when the store Global is absent.
pub fn pin_legacy_appearance(cx: &mut App) {
    let Some(store) = cx.try_global::<ThemeSettingsStore>() else {
        return;
    };
    let current_scheme = store.active_scheme();
    // The RAW file bytes carry absent-ness; the loaded store already filled
    // absents with the NEW defaults, so it cannot report which keys were absent.
    let bytes = std::fs::read(store.path()).unwrap_or_default();
    let pinned = legacy_pinned_appearance(&bytes, current_scheme);
    commit_appearance(cx, pinned);
}

/// Apply the restyle NEW-look defaults to every appearance axis (restyle plan 3
/// migration, the popup's "Try the new look" answer): Terracotta accent, the
/// nice-default terminal themes (which now drive the chrome too), the 80/90
/// opacity + 30px blur slider defaults, and OS-sync ON (reconciling the scheme
/// to the OS now).
/// Terminal FONT family/size and terminal LINE-HEIGHT live in the prefs store —
/// the caller sets line-height to 1.3 and never touches font family/size (Nick's
/// carve-out). No-op when the store Global is absent.
pub fn apply_restyle_new_look(cx: &mut App) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    let nu = Appearance::default();
    appearance.accent = nu.accent;
    appearance.terminal_theme_light_id = nu.terminal_theme_light_id;
    appearance.terminal_theme_dark_id = nu.terminal_theme_dark_id;
    appearance.window_opacity_light = nu.window_opacity_light;
    appearance.window_opacity_dark = nu.window_opacity_dark;
    appearance.blur_radius_light = nu.blur_radius_light;
    appearance.blur_radius_dark = nu.blur_radius_dark;
    // OS-sync ON is a new default; reconcile the scheme to the OS now so the flip
    // is visible immediately (not only after the next launch's boot reconcile).
    appearance.sync_with_os = true;
    if let Some(os) = current_os_scheme(cx) {
        appearance.scheme = os;
    }
    commit_appearance(cx, appearance);
}

/// Turn OS-appearance sync on or off. Turning it ON reconciles the scheme to the
/// current OS appearance immediately (collapsed into ONE commit, so the flip fans
/// out and writes Claude at most once).
pub fn apply_sync_with_os(cx: &mut App, on: bool) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    appearance.sync_with_os = on;
    if on {
        if let Some(os) = current_os_scheme(cx) {
            appearance.scheme = os;
        }
    }
    commit_appearance(cx, appearance);
}

/// Flip the `ClaudeThemeSyncGate` and re-source every window's `--settings`
/// provider (the provider path depends ONLY on the gate, so it is re-sourced only
/// here, not on every theme change). On `off→on` the colors file is rewritten
/// immediately to the active triple; `on→off` leaves the file in place and stops
/// handing NEW panes the flag (Swift semantics). R23's Settings toggle drives this.
pub fn apply_sync_claude_theme(cx: &mut App, on: bool) {
    let was_on = crate::app::claude_theme_sync_gate_on(cx);
    crate::app::set_claude_theme_sync_gate(cx, on);
    // Re-source every live window's provider (ensure-on-read pointer path when on,
    // `None` when off). Gate-only, per the R17 contract (`window_state.rs:434`).
    let provider = crate::claude_theme_sync::settings_path_for_gate(on);
    for state in crate::window_registry::WindowRegistry::all_states(cx) {
        state.update(cx, |ws, _cx| ws.set_claude_settings_path(provider.clone()));
    }
    // off→on: rewrite the colors file now so an already-open Claude repaints.
    if on && !was_on {
        claude_sync_if_gated(cx);
    }
}

/// Set the store selection (persist only-if-changed) and, when it changed, refresh
/// the live [`ThemeState`] + fan out to chrome and panes + mirror to Claude. The
/// shared engine behind every `apply_*` mutator and [`reconcile_with_os`].
fn commit_appearance(cx: &mut App, appearance: Appearance) {
    let changed = match cx.global_mut::<ThemeSettingsStore>().set(appearance) {
        Ok(changed) => changed,
        Err(e) => {
            eprintln!("nice: theme store persist failed: {e}");
            // Persist failed but the in-memory selection changed — still fan out so
            // the UI reflects the pick (matches the fail-soft store discipline).
            true
        }
    };
    if !changed {
        return;
    }
    refresh_theme_state(cx);
    apply_theme_fanout(cx);
    claude_sync_if_gated(cx);
}

/// Re-derive the resolved [`ThemeState`] from the store + catalog into the
/// [`SharedThemeState`] entity (chrome reads it at render time). No-op when the
/// live entity is absent (a scenario/test that did not opt into live theming).
fn refresh_theme_state(cx: &mut App) {
    let Some(entity) = cx.try_global::<SharedThemeState>().map(|s| s.0.clone()) else {
        return;
    };
    // Derive under the immutable global borrows, then drop them before updating the
    // entity (which needs `&mut App`).
    let new_state = {
        let store = cx.global::<ThemeSettingsStore>();
        let catalog = cx.global::<TerminalThemeCatalog>();
        ThemeState::from_stores(store, catalog)
    };
    entity.update(cx, |state, cx| {
        *state = new_state;
        cx.notify();
    });
}

/// Fan the live theme out across every window: repaint chrome
/// ([`App::refresh_windows`]) and push the resolved terminal theme + accent into
/// every window's [`PaneHostView`](crate::app_shell::PaneHostView) — walking
/// [`WindowRegistry::all_states`](crate::window_registry::WindowRegistry) — through
/// the boundary-legal `TerminalView::set_theme` setters (the `SessionThemeCache`
/// analog). Later-built panes seed with the new colors because
/// `PaneHostView::set_theme` updates the host's own seed fields first (the
/// load-bearing theme-before-per-pane order, dossier §0.7/§4.1). No-op when the
/// live entity is absent.
pub fn apply_theme_fanout(cx: &mut App) {
    let Some(state) = cx
        .try_global::<SharedThemeState>()
        .map(|s| s.0.read(cx).clone())
    else {
        return;
    };
    // Chrome: every window re-renders and re-reads the active `Slots`/accent (and
    // the window-body backing re-reads the active window opacity via
    // [`active_window_opacity`]).
    cx.refresh_windows();
    // Panes: collect the hosts first (each read borrows `cx`), then push into each
    // — both the terminal theme + accent AND the active surface-fill opacity (the
    // grid's default-background surface tracks the same alpha as the backing).
    let hosts: Vec<_> = crate::window_registry::WindowRegistry::all_states(cx)
        .into_iter()
        .filter_map(|ws| ws.read(cx).pane_host())
        .collect();
    for host in hosts {
        host.update(cx, |h, cx| {
            h.set_theme(state.terminal_theme.clone(), state.accent, cx);
            h.set_background_opacity(state.background_opacity, cx);
        });
    }
    // Windows: push the resolved `WindowBackgroundAppearance` + numeric blur radius
    // into each live NSWindow (a slider change or a scheme flip re-applies both).
    apply_window_transparency_fanout(cx);
}

/// Re-apply the active-scheme window transparency (appearance + numeric blur
/// radius) to every live window. Collects each window's stashed
/// [`AnyWindowHandle`] first (the reads borrow `cx`), then updates each window out
/// of band. No-op for a window with no stashed handle (never happens post
/// `build_window_root`) and when the live theme entity is absent.
///
/// The per-window pushes are `cx.defer`-ed so they run OUTSIDE any in-flight
/// window update. The restyle popup's "Try the new look" answer runs its
/// completion — and thus `commit_appearance` → this fanout — from inside the
/// modal's mouse handler, i.e. while the host window is mid-update (gpui `take`s
/// the window out of its slot for the duration). A reentrant `cx.update_window`
/// for that same window would hit the "window already taken" guard and get
/// silently dropped (`let _ =`), so the pre-existing opaque window only flipped
/// to Blurred after a relaunch. The settings-stepper path never hit this because
/// its fanout targets terminal windows other than the settings window it runs
/// inside. Deferring makes both paths apply the NSWindow-level appearance
/// uniformly, on the next effect flush, once the update stack has unwound.
fn apply_window_transparency_fanout(cx: &mut App) {
    let (appearance, radius) = match cx.try_global::<SharedThemeState>() {
        Some(shared) => {
            let s = shared.0.read(cx);
            (s.window_appearance, s.blur_radius)
        }
        None => return,
    };
    let handles: Vec<AnyWindowHandle> = crate::window_registry::WindowRegistry::all_states(cx)
        .into_iter()
        .filter_map(|ws| ws.read(cx).window_handle())
        .collect();
    cx.defer(move |cx| {
        for handle in handles {
            let _ = cx.update_window(handle, |_root, window, _cx| {
                window.set_background_blur_radius(radius);
                window.set_background_appearance(appearance);
                record_transparency_applied(appearance, radius);
            });
        }
    });
}

/// Selftest/test instrumentation (restyle-popup reentrancy pin): the
/// `(appearance, radius, generation)` most recently pushed to a live window by
/// [`apply_window_transparency_fanout`]'s deferred per-window closure —
/// `generation` is a monotonic counter bumped on each push. The `theme-fanout`
/// scenario commits a `Blurred` appearance FROM INSIDE a window update
/// (reproducing the migration "Try the new look" answer, whose confirm callback
/// runs `commit_appearance` during the host window's update) and asserts the
/// generation advanced afterwards — i.e. the DEFERRED fanout actually reached the
/// window instead of being silently dropped by gpui's reentrant-`update_window`
/// guard (the bug left the pre-existing opaque window opaque until relaunch).
#[cfg(any(test, feature = "selftest"))]
static TRANSPARENCY_FANOUT_APPLIED: std::sync::Mutex<
    Option<(WindowBackgroundAppearance, u32, u64)>,
> = std::sync::Mutex::new(None);

/// Record a per-window transparency push (bumping the generation). Instrumented
/// only for tests / the selftest bundle; a no-op in the shipped app.
#[cfg(any(test, feature = "selftest"))]
fn record_transparency_applied(appearance: WindowBackgroundAppearance, radius: u32) {
    let mut slot = TRANSPARENCY_FANOUT_APPLIED.lock().unwrap();
    let generation = slot.map(|(_, _, g)| g).unwrap_or(0) + 1;
    *slot = Some((appearance, radius, generation));
}

#[cfg(not(any(test, feature = "selftest")))]
#[inline]
fn record_transparency_applied(_appearance: WindowBackgroundAppearance, _radius: u32) {}

/// Reader for [`TRANSPARENCY_FANOUT_APPLIED`]: the last-applied
/// `(appearance, radius, generation)`, or `None` if the deferred fanout has not
/// pushed to a window this process. Always compiled so the always-built
/// `theme_fanout_live` scenario can name it; a constant `None` in the shipped
/// bundle (the backing store is instrumented only under test / `selftest`).
pub(crate) fn transparency_fanout_applied() -> Option<(WindowBackgroundAppearance, u32, u64)> {
    #[cfg(any(test, feature = "selftest"))]
    {
        *TRANSPARENCY_FANOUT_APPLIED.lock().unwrap()
    }
    #[cfg(not(any(test, feature = "selftest")))]
    {
        None
    }
}

/// Mirror the active resolved triple (terminal theme × scheme × accent) into
/// Claude's live-reload custom-theme file when the `ClaudeThemeSyncGate` is ON
/// (R17-live, dossier §6). Reuses the landed atomic only-if-changed writer, so a
/// scheme flip's identical re-derivations collapse to ≤1 disk write and Claude's
/// watcher isn't woken needlessly. No-op when the gate is off or the live entity is
/// absent. Hermetic under a scenario's temp `$HOME`.
pub(crate) fn claude_sync_if_gated(cx: &App) {
    if !crate::app::claude_theme_sync_gate_on(cx) {
        return;
    }
    let Some(state) = cx
        .try_global::<SharedThemeState>()
        .map(|s| s.0.read(cx).clone())
    else {
        return;
    };
    crate::claude_theme_sync::write(&state.terminal_theme, state.scheme, state.accent);
}

/// Install the self-test theme globals (`run_selftest`): the defaults+temp store
/// and the catalog over a throwaway temp `terminal_themes_dir`, but NOT
/// [`SharedThemeState`] — scenarios paint the Nice/Dark fallback unless a scenario
/// opts into live theming by minting the entity itself (slice 3's `theme-fanout`).
/// `TerminalThemeCatalog::new` enumerates the (nonexistent) temp dir read-only, so
/// there is no launch-time write (hermeticity).
pub fn install_selftest_theme_defaults(
    cx: &mut App,
    store: ThemeSettingsStore,
    terminal_themes_dir: PathBuf,
) {
    cx.set_global(store);
    cx.set_global(TerminalThemeCatalog::new(terminal_themes_dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-theme-settings-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// Fresh-install defaults (restyle plan 3 flip): Terracotta accent, the
    /// nice-default terminal themes (which drive the chrome after the round-2
    /// merge).
    #[test]
    fn fresh_install_defaults() {
        let a = Appearance::default();
        assert_eq!(a.scheme, ColorScheme::Dark);
        assert!(a.sync_with_os);
        assert_eq!(a.accent, AccentPreset::Terracotta);
        assert_eq!(a.terminal_theme_light_id, "nice-default-light");
        assert_eq!(a.terminal_theme_dark_id, "nice-default-dark");
        // Restyle plan 3 new-install defaults: dark 80% / light 90% opacity, blur
        // 30 px both schemes, popup not yet shown.
        assert_eq!(a.window_opacity_dark, 80);
        assert_eq!(a.window_opacity_light, 90);
        assert_eq!(a.blur_radius_dark, 30);
        assert_eq!(a.blur_radius_light, 30);
        assert!(!a.restyle_popup_shown);
    }

    /// Appearance-selection rules (plan Rendering §): opacity 100% ⇒ Opaque;
    /// opacity < 100% with blur > 0 ⇒ Blurred; opacity < 100% with blur == 0 ⇒
    /// Transparent (a zeroed radius degrades to plain transparency).
    #[test]
    fn window_appearance_selection_rules() {
        let base = Appearance {
            scheme: ColorScheme::Dark,
            ..Appearance::default()
        };
        // 100% opacity ⇒ Opaque regardless of blur.
        let opaque = Appearance {
            window_opacity_dark: 100,
            blur_radius_dark: 30,
            ..base.clone()
        };
        assert_eq!(
            opaque.active_window_appearance(),
            WindowBackgroundAppearance::Opaque
        );
        // Translucent + blur ⇒ Blurred.
        let blurred = Appearance {
            window_opacity_dark: 80,
            blur_radius_dark: 30,
            ..base.clone()
        };
        assert_eq!(
            blurred.active_window_appearance(),
            WindowBackgroundAppearance::Blurred
        );
        assert_eq!(blurred.active_window_opacity(), 0.8);
        // Translucent + zero blur ⇒ Transparent.
        let transparent = Appearance {
            window_opacity_dark: 80,
            blur_radius_dark: 0,
            ..base
        };
        assert_eq!(
            transparent.active_window_appearance(),
            WindowBackgroundAppearance::Transparent
        );
    }

    /// Per-scheme opacity/blur select the active slot; a scheme flip swaps them.
    #[test]
    fn opacity_and_blur_are_per_scheme() {
        let a = Appearance {
            window_opacity_light: 90,
            window_opacity_dark: 70,
            blur_radius_light: 10,
            blur_radius_dark: 55,
            ..Appearance::default()
        };
        let light = Appearance {
            scheme: ColorScheme::Light,
            ..a.clone()
        };
        assert_eq!(light.active_window_opacity_pct(), 90);
        assert_eq!(light.active_blur_radius(), 10);
        let dark = Appearance {
            scheme: ColorScheme::Dark,
            ..a
        };
        assert_eq!(dark.active_window_opacity_pct(), 70);
        assert_eq!(dark.active_blur_radius(), 55);
    }

    /// Out-of-range opacity/blur in the stored file are clamped on decode (a
    /// hand-edited settings file can never drive an out-of-range NSWindow state).
    #[test]
    fn out_of_range_opacity_and_blur_clamp_on_decode() {
        let path = temp_path("clamp");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{
                "window_opacity_dark":10,
                "window_opacity_light":200,
                "blur_radius_dark":999,
                "blur_radius_light":0
            }}"#,
        )
        .unwrap();
        let store = ThemeSettingsStore::load(path);
        let a = store.appearance();
        assert_eq!(a.window_opacity_dark, 55, "below the floor clamps to 55");
        assert_eq!(a.window_opacity_light, 100, "above the ceiling clamps to 100");
        assert_eq!(a.blur_radius_dark, 60, "above the ceiling clamps to 60");
        assert_eq!(a.blur_radius_light, 0, "0 is in range");
    }

    /// The transparency keys + the one-time popup flag round-trip through the store.
    #[test]
    fn transparency_and_popup_flag_round_trip() {
        let path = temp_path("transparency-roundtrip");
        let mut store = ThemeSettingsStore::load(path.clone());
        let target = Appearance {
            window_opacity_light: 88,
            window_opacity_dark: 66,
            blur_radius_light: 12,
            blur_radius_dark: 44,
            restyle_popup_shown: true,
            ..Appearance::default()
        };
        assert!(store.set(target.clone()).unwrap());
        let reloaded = ThemeSettingsStore::load(path);
        assert_eq!(*reloaded.appearance(), target);
        assert!(reloaded.restyle_popup_shown());
    }

    /// Restyle plan 3 migration: pinning an existing user's ABSENT appearance keys
    /// materializes the LEGACY (pre-flip) literals, so pin-then-resolve reproduces
    /// the PRE-flip resolution exactly — the defaults-flip cannot silently restyle
    /// a user who was riding the old defaults. Explicitly-set keys survive.
    #[test]
    fn legacy_pin_reproduces_pre_flip_resolution() {
        // An existing user with the WHOLE appearance section absent (rode every old
        // default). Pin with the live OS-reconciled scheme (Dark here — no OS source
        // in a unit test).
        let empty = br#"{"version":1,"file_browser_sort":{"criterion":"name","ascending":true}}"#;
        let pinned = legacy_pinned_appearance(empty, ColorScheme::Dark);

        // Every field pins to the LEGACY literal — NOT the flipped fresh-install
        // default (Terracotta / nice-default-* / 80-90 / 30).
        assert_eq!(pinned, legacy_appearance_defaults());
        assert_eq!(pinned.accent, AccentPreset::Ocean);
        assert_eq!(pinned.terminal_theme_light_id, "catppuccin-latte");
        assert_eq!(pinned.terminal_theme_dark_id, "catppuccin-mocha");
        assert_eq!(pinned.window_opacity_light, 100);
        assert_eq!(pinned.window_opacity_dark, 100);
        assert_eq!(pinned.blur_radius_light, 0);
        assert_eq!(pinned.blur_radius_dark, 0);

        // The RESOLVED active look equals the PRE-flip resolution: opaque, unblurred,
        // the Catppuccin-Mocha terminal id on the Dark scheme (which now also drives
        // the chrome — see `legacy_mismatched_pair_resolves_to_terminal_derived_chrome`
        // for the chrome-derivation half over a catalog).
        assert_eq!(pinned.active_terminal_id(), "catppuccin-mocha");
        assert_eq!(pinned.active_window_opacity(), 1.0);
        assert_eq!(pinned.active_blur_radius(), 0);
        assert_eq!(
            pinned.active_window_appearance(),
            WindowBackgroundAppearance::Opaque
        );

        // A user who EXPLICITLY set a couple of keys keeps them; the rest still pin
        // to the legacy literals.
        let partial = br#"{"appearance":{"accent":"iris","window_opacity_dark":70}}"#;
        let pinned = legacy_pinned_appearance(partial, ColorScheme::Light);
        assert_eq!(pinned.accent, AccentPreset::Iris, "explicit accent survives");
        assert_eq!(pinned.window_opacity_dark, 70, "explicit opacity survives");
        // Absent keys still legacy — never the flipped fresh-install default.
        assert_eq!(pinned.terminal_theme_light_id, "catppuccin-latte");
        assert_eq!(pinned.window_opacity_light, 100);
        assert_eq!(pinned.blur_radius_dark, 0);
        // The absent scheme is preserved from the caller (the live OS-reconciled
        // scheme), not pinned to the legacy literal.
        assert_eq!(pinned.scheme, ColorScheme::Light);
    }

    /// A missing file loads defaults (fresh-install path).
    #[test]
    fn missing_file_loads_defaults() {
        let path = temp_path("missing");
        assert!(!path.exists());
        let store = ThemeSettingsStore::load(path);
        assert_eq!(*store.appearance(), Appearance::default());
    }

    /// Full round-trip: a non-default selection persists and reloads identically.
    #[test]
    fn set_persists_and_reloads() {
        let path = temp_path("roundtrip");
        let mut store = ThemeSettingsStore::load(path.clone());
        let target = Appearance {
            scheme: ColorScheme::Light,
            sync_with_os: false,
            accent: AccentPreset::Iris,
            terminal_theme_light_id: "solarized-light".to_string(),
            terminal_theme_dark_id: "dracula".to_string(),
            window_opacity_light: 85,
            window_opacity_dark: 75,
            blur_radius_light: 20,
            blur_radius_dark: 40,
            restyle_popup_shown: true,
        };
        assert!(store.set(target.clone()).unwrap(), "changing the value writes");

        let reloaded = ThemeSettingsStore::load(path);
        assert_eq!(*reloaded.appearance(), target);
    }

    /// only-if-changed: re-setting the identical selection performs no write.
    #[test]
    fn set_same_value_does_not_rewrite() {
        let path = temp_path("noop");
        let mut store = ThemeSettingsStore::load(path);
        let target = Appearance {
            accent: AccentPreset::Fern,
            ..Appearance::default()
        };
        assert!(store.set(target.clone()).unwrap(), "first set writes");
        assert!(
            !store.set(target).unwrap(),
            "re-setting the identical value must not rewrite"
        );
    }

    /// Tolerance: an absent `appearance` section ⇒ all fresh-install defaults,
    /// even when a sibling section is present.
    #[test]
    fn absent_section_loads_defaults() {
        let path = temp_path("absent");
        std::fs::write(
            &path,
            br#"{"version":1,"file_browser_sort":{"criterion":"name","ascending":true}}"#,
        )
        .unwrap();
        let store = ThemeSettingsStore::load(path);
        assert_eq!(*store.appearance(), Appearance::default());
    }

    /// Tolerance: an unknown accent / scheme rawValue and a missing field all fall
    /// back to that field's default; a legacy `chrome_light_palette` key is
    /// ignored (round-2 merge dropped it); known fields still decode.
    #[test]
    fn unknown_and_missing_fields_default() {
        let path = temp_path("tolerant");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{
                "scheme":"chartreuse",
                "chrome_light_palette":"bogus",
                "accent":"neon",
                "terminal_theme_dark_id":"gruvbox-dark"
            }}"#,
        )
        .unwrap();
        let store = ThemeSettingsStore::load(path);
        let a = store.appearance();
        let d = Appearance::default();
        // Unknown rawValues → defaults.
        assert_eq!(a.scheme, d.scheme);
        assert_eq!(a.accent, d.accent);
        // Missing fields → defaults.
        assert!(a.sync_with_os);
        assert_eq!(a.terminal_theme_light_id, d.terminal_theme_light_id);
        // A known field still decodes (and the ignored legacy chrome key did not
        // break the decode).
        assert_eq!(a.terminal_theme_dark_id, "gruvbox-dark");
    }

    /// Round-2 migration: the legacy `chrome_dark_palette` key (including a
    /// `"macOS"` value) is ignored on read — it never breaks the decode, and the
    /// terminal-theme selection alone survives (the terminal drives the chrome).
    #[test]
    fn legacy_chrome_keys_including_macos_are_ignored_on_read() {
        let path = temp_path("legacy-chrome");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{
                "scheme":"dark",
                "chrome_light_palette":"catppuccinLatte",
                "chrome_dark_palette":"macOS",
                "terminal_theme_dark_id":"dracula"
            }}"#,
        )
        .unwrap();
        // Decodes without error; the terminal id is honored, the chrome keys gone.
        let store = ThemeSettingsStore::load(path);
        let a = store.appearance();
        assert_eq!(a.scheme, ColorScheme::Dark);
        assert_eq!(a.terminal_theme_dark_id, "dracula");
        assert_eq!(a.active_terminal_id(), "dracula");
    }

    /// Malformed JSON is fail-soft: full defaults, no crash.
    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = ThemeSettingsStore::load(path);
        assert_eq!(*store.appearance(), Appearance::default());
    }

    /// A hermetic builtins-only catalog over a throwaway (nonexistent) temp dir.
    fn hermetic_catalog(tag: &str) -> TerminalThemeCatalog {
        TerminalThemeCatalog::new(std::env::temp_dir().join(format!(
            "nice-theme-settings-{tag}-{}",
            std::process::id()
        )))
    }

    /// The merged chrome resolution ([`chrome_slots_for_theme`]): the four
    /// hand-tuned ids map to their hand-tuned tables; every OTHER built-in derives
    /// chrome from the resolved terminal theme's fg/bg (never a hand-tuned table).
    #[test]
    fn chrome_slots_key_off_the_merged_theme_id() {
        let catalog = hermetic_catalog("chrome-slots");

        // The four hand-tuned ids (each with its natural scheme) → their tables.
        for (id, scheme, expected) in [
            ("nice-default-light", ColorScheme::Light, NICE_LIGHT),
            ("nice-default-dark", ColorScheme::Dark, NICE_DARK),
            ("catppuccin-latte", ColorScheme::Light, CATPPUCCIN_LATTE),
            ("catppuccin-mocha", ColorScheme::Dark, CATPPUCCIN_MOCHA),
        ] {
            let theme = catalog.resolve(id, scheme);
            assert_eq!(chrome_slots_for_theme(id, scheme, &theme), expected, "{id}");
        }

        // A derived built-in (Dracula): chrome == derive_chrome(fg, bg), the
        // background slot is the terminal background verbatim, and NOT a hand-tuned
        // table.
        let dracula = catalog.resolve("dracula", ColorScheme::Dark);
        let derived = chrome_slots_for_theme("dracula", ColorScheme::Dark, &dracula);
        assert_eq!(
            derived,
            nice_theme::derive_chrome(
                terminal_color_to_srgba(dracula.foreground),
                terminal_color_to_srgba(dracula.background),
            )
        );
        assert_eq!(
            derived.background,
            nice_theme::palette::SlotColor::Srgb(terminal_color_to_srgba(dracula.background))
        );
        assert_ne!(derived, NICE_DARK);
        assert_ne!(derived, CATPPUCCIN_MOCHA);
    }

    /// Legibility floor over the ACTUAL built-in catalog (plan 5 Validation +
    /// the `derive.rs` deferral at `ink_ramp_clears_the_nice_contrast_floor`,
    /// which pins only the Nice reference and defers "the per-catalog contrast
    /// checks over the actual built-ins" to the consumer wiring — this test).
    ///
    /// For every built-in id the catalog exposes, resolve its chrome via
    /// [`chrome_slots_for_theme`] (the 8 muted themes derive, the 4 hand-tuned
    /// ids use their tables) and assert the whole ink ramp (`ink`/`ink2`/`ink3`)
    /// clears a readable contrast floor against the resolved background. The
    /// floors are DELIBERATELY below what `NICE_DARK`/`NICE_LIGHT` achieve: real
    /// terminal themes carry lower intrinsic fg/bg contrast than Nice (Solarized
    /// is intentionally low-contrast), so the derivation can only preserve the
    /// theme's own legibility, not manufacture Nice's. Grounded floors —
    /// `ink >= 4.0` (~WCAG AA body), `ink2 >= 2.5`, `ink3 >= 1.8` (muted but
    /// distinguishable) — with margin under the observed minima (ink 4.13 /
    /// ink2 2.93 / ink3 1.95, both from Solarized), so a real collapse (a slot
    /// routing to the wrong color, or a factor regressing the muted ramp toward
    /// invisibility) fails here while the intended derivation passes.
    #[test]
    fn every_built_in_ink_ramp_is_legible_over_its_derived_surface() {
        use crate::built_in_terminal_themes::built_in_terminal_themes;
        use crate::terminal_theme_catalog::ThemeScope;
        use nice_theme::palette::SlotColor;

        // WCAG 2.1 relative luminance + contrast ratio over `Srgba`, matching the
        // formula `nice-theme::derive` uses for scheme detection.
        fn relative_luminance(c: Srgba) -> f32 {
            fn linearize(ch: f32) -> f32 {
                if ch <= 0.039_28 {
                    ch / 12.92
                } else {
                    ((ch + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * linearize(c.r) + 0.7152 * linearize(c.g) + 0.0722 * linearize(c.b)
        }
        fn contrast(a: Srgba, b: Srgba) -> f32 {
            let (la, lb) = (relative_luminance(a), relative_luminance(b));
            let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
            (hi + 0.05) / (lo + 0.05)
        }
        fn slot(s: SlotColor) -> Srgba {
            let SlotColor::Srgb(c) = s;
            c
        }

        let catalog = hermetic_catalog("ink-legibility");
        for built_in in built_in_terminal_themes() {
            // Built-ins are single-scheme (never `Either`); resolve in that scheme.
            let scheme = match built_in.scope {
                ThemeScope::Light => ColorScheme::Light,
                ThemeScope::Dark => ColorScheme::Dark,
                ThemeScope::Either => ColorScheme::Dark,
            };
            let terminal = catalog.resolve(built_in.id, scheme);
            let chrome = chrome_slots_for_theme(built_in.id, scheme, &terminal);
            let bg = slot(chrome.background);
            for (name, ink, floor) in [
                ("ink", slot(chrome.ink), 4.0_f32),
                ("ink2", slot(chrome.ink2), 2.5),
                ("ink3", slot(chrome.ink3), 1.8),
            ] {
                let ratio = contrast(ink, bg);
                assert!(
                    ratio >= floor,
                    "{} {name} contrast {ratio:.3} below legibility floor {floor:.3}",
                    built_in.id
                );
            }
        }
    }

    /// Migration (plan Validation): a legacy store with a MISMATCHED pair
    /// (chrome = Catppuccin Mocha, terminal = Dracula) resolves — after the merge
    /// drops the chrome key — to Dracula everywhere. The terminal colors are
    /// byte-identical to the catalog's Dracula, and the chrome is Dracula-derived
    /// (NOT the Catppuccin Mocha table). "The terminal theme wins."
    #[test]
    fn legacy_mismatched_pair_resolves_to_terminal_derived_chrome() {
        let path = temp_path("legacy-mismatch");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{
                "scheme":"dark",
                "chrome_dark_palette":"catppuccinMocha",
                "terminal_theme_dark_id":"dracula"
            }}"#,
        )
        .unwrap();
        let store = ThemeSettingsStore::load(path);
        assert_eq!(store.active_terminal_id(), "dracula");

        let catalog = hermetic_catalog("legacy-mismatch");
        let state = ThemeState::from_stores(&store, &catalog);

        // Terminal half: byte-identical to the catalog's Dracula (unchanged look).
        let dracula = catalog.resolve("dracula", ColorScheme::Dark);
        assert_eq!(state.terminal_theme, dracula);

        // Chrome half: Dracula-DERIVED — the Catppuccin Mocha table is NOT used.
        assert_eq!(
            state.slots,
            nice_theme::derive_chrome(
                terminal_color_to_srgba(dracula.foreground),
                terminal_color_to_srgba(dracula.background),
            )
        );
        assert_ne!(state.slots, CATPPUCCIN_MOCHA);
        assert_eq!(
            state.slots.background,
            nice_theme::palette::SlotColor::Srgb(terminal_color_to_srgba(dracula.background))
        );
    }

    /// `active_terminal_id` selects the per-scheme slot; the (explicitly-set)
    /// Catppuccin ids resolve to the REAL Catppuccin built-ins, while a genuinely
    /// unknown id still falls back to the Nice default per scheme.
    #[test]
    fn active_terminal_id_and_unknown_id_nice_default_fallback() {
        use crate::terminal_theme_catalog::TerminalThemeCatalog;
        // A hermetic catalog over a throwaway temp dir (no imports).
        let catalog = TerminalThemeCatalog::new(std::env::temp_dir().join(format!(
            "nice-theme-settings-catalog-{}",
            std::process::id()
        )));

        // Per-scheme id selection (the fresh-install default ids are now the
        // nice-default catalog ids after the restyle plan 3 flip — see
        // `fresh_install_defaults`; assert selection with explicit Catppuccin ids).
        let light = Appearance {
            scheme: ColorScheme::Light,
            terminal_theme_light_id: "catppuccin-latte".to_string(),
            ..Appearance::default()
        };
        assert_eq!(light.active_terminal_id(), "catppuccin-latte");
        let dark = Appearance {
            scheme: ColorScheme::Dark,
            terminal_theme_dark_id: "catppuccin-mocha".to_string(),
            ..Appearance::default()
        };
        assert_eq!(dark.active_terminal_id(), "catppuccin-mocha");

        // R22: those ids are real built-ins — NOT the Nice-default fallback.
        assert_ne!(
            catalog.resolve(light.active_terminal_id(), ColorScheme::Light),
            nice_term_view::TerminalTheme::nice_default_light()
        );
        assert_ne!(
            catalog.resolve(dark.active_terminal_id(), ColorScheme::Dark),
            nice_term_view::TerminalTheme::nice_default_dark()
        );
        // A genuinely unknown id still falls back per scheme.
        assert_eq!(
            catalog.resolve("does-not-exist", ColorScheme::Light),
            nice_term_view::TerminalTheme::nice_default_light()
        );
    }

    /// `map_window_appearance` folds gpui's four appearance variants onto the two
    /// schemes ({Light,VibrantLight}→Light, {Dark,VibrantDark}→Dark, OQ1).
    #[test]
    fn window_appearance_maps_to_scheme() {
        assert_eq!(map_window_appearance(WindowAppearance::Light), ColorScheme::Light);
        assert_eq!(
            map_window_appearance(WindowAppearance::VibrantLight),
            ColorScheme::Light
        );
        assert_eq!(map_window_appearance(WindowAppearance::Dark), ColorScheme::Dark);
        assert_eq!(
            map_window_appearance(WindowAppearance::VibrantDark),
            ColorScheme::Dark
        );
    }

    /// The boot-seed OS reconcile: a no-op unless `sync_with_os` AND the scheme
    /// differs; when it flips it mutates in memory WITHOUT persisting (the boot seed;
    /// the runtime `reconcile_with_os` persists).
    #[test]
    fn reconcile_scheme_in_memory_gates_on_sync_and_diff() {
        // sync on, scheme differs ⇒ flips.
        let mut store = ThemeSettingsStore::with_defaults(temp_path("reconcile-flip"));
        assert_eq!(store.appearance().scheme, ColorScheme::Dark);
        assert!(store.appearance().sync_with_os);
        assert!(store.reconcile_scheme_in_memory(ColorScheme::Light));
        assert_eq!(store.appearance().scheme, ColorScheme::Light);
        // No file written (the seed is in-memory only).
        assert!(!store.path().exists());

        // sync on, scheme already matches ⇒ no-op.
        let mut same = ThemeSettingsStore::with_defaults(temp_path("reconcile-same"));
        assert!(!same.reconcile_scheme_in_memory(ColorScheme::Dark));
        assert_eq!(same.appearance().scheme, ColorScheme::Dark);

        // sync OFF ⇒ no-op even when the OS differs.
        let mut off = ThemeSettingsStore::load(temp_path("reconcile-off"));
        off.set(Appearance {
            sync_with_os: false,
            scheme: ColorScheme::Dark,
            ..Appearance::default()
        })
        .unwrap();
        assert!(!off.reconcile_scheme_in_memory(ColorScheme::Light));
        assert_eq!(off.appearance().scheme, ColorScheme::Dark);
    }

    /// Read-merge-write preserves a planted `file_browser_sort` key when the
    /// theme store writes its `appearance` section (OQ3 — no co-writer clobber).
    #[test]
    fn write_preserves_file_browser_sort_section() {
        let path = temp_path("merge");
        std::fs::write(
            &path,
            br#"{"version":1,"file_browser_sort":{"criterion":"date_modified","ascending":false},"future_section":{"hello":42}}"#,
        )
        .unwrap();

        let mut store = ThemeSettingsStore::load(path.clone());
        store
            .set(Appearance {
                accent: AccentPreset::Graphite,
                ..Appearance::default()
            })
            .unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The theme section landed.
        assert_eq!(raw["appearance"]["accent"], "graphite");
        // The sibling sort section and an unknown key both survive.
        assert_eq!(raw["file_browser_sort"]["criterion"], "date_modified");
        assert_eq!(raw["file_browser_sort"]["ascending"], false);
        assert_eq!(raw["future_section"]["hello"], 42);
        assert_eq!(raw["version"], 1);
    }
}
