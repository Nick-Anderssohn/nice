//! The theme store — the persisted appearance selection + its active-view
//! derivations, sharing `ui_settings.json` with R19's file-browser sort store.
//!
//! ## What lives here (R21 slice 1)
//!
//! * [`Appearance`] — the pure value type: `scheme` (light|dark), `sync_with_os`,
//!   the two per-scheme chrome-palette slots, `accent`, and the two per-scheme
//!   terminal-theme-id slots. Its fresh-install defaults, its tolerant decode
//!   (Swift/`SortSettingsStore` fail-soft parity), and its active-view
//!   derivations (`active_chrome_palette` with the **macOS → Nice substitution**,
//!   `active_terminal_id`) are all pure functions with unit tables below.
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
//! default; malformed JSON ⇒ full defaults; a persisted `"macOS"` palette is
//! tolerated but substituted to Nice at derivation so nothing ever paints black
//! (OQ6). `syncClaudeTheme` is NOT here — it stays the R17 CFPref.

#![allow(dead_code)] // Slice 2/3 (ThemeState + apply_* mutators) consume these.

use std::path::PathBuf;

use gpui::{App, AppContext, Entity, Global, WindowAppearance};
use nice_theme::color::Srgba;
use nice_theme::palette::{slots, ColorScheme, Palette, Slots};
use nice_theme::AccentPreset;
use serde::{Deserialize, Serialize};

use crate::terminal_theme_catalog::TerminalThemeCatalog;

/// The default terminal-theme id for the light slot (Swift parity). Not in the
/// R21 stub catalog — resolves through the Nice-default fallback until R22.
const DEFAULT_TERMINAL_THEME_LIGHT_ID: &str = "catppuccin-latte";
/// The default terminal-theme id for the dark slot (Swift parity).
const DEFAULT_TERMINAL_THEME_DARK_ID: &str = "catppuccin-mocha";

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
    /// The chrome palette active when `scheme == Light`.
    pub chrome_light_palette: Palette,
    /// The chrome palette active when `scheme == Dark`.
    pub chrome_dark_palette: Palette,
    /// The accent swatch (palette-agnostic).
    pub accent: AccentPreset,
    /// The terminal-theme id active when `scheme == Light` (resolved via the
    /// catalog; unknown ⇒ Nice default).
    pub terminal_theme_light_id: String,
    /// The terminal-theme id active when `scheme == Dark`.
    pub terminal_theme_dark_id: String,
}

impl Default for Appearance {
    /// Fresh-install defaults (dossier §2.1). `scheme` is a placeholder here:
    /// with `sync_with_os == true` the store's OS reconcile (slice 3) pins it to
    /// the system appearance at launch, so the fixed value below is only what a
    /// sync-off fresh install would show. `Dark` matches today's hardcoded look.
    fn default() -> Self {
        Self {
            scheme: ColorScheme::Dark,
            sync_with_os: true,
            chrome_light_palette: Palette::CatppuccinLatte,
            chrome_dark_palette: Palette::CatppuccinMocha,
            accent: AccentPreset::Ocean,
            terminal_theme_light_id: DEFAULT_TERMINAL_THEME_LIGHT_ID.to_string(),
            terminal_theme_dark_id: DEFAULT_TERMINAL_THEME_DARK_ID.to_string(),
        }
    }
}

impl Appearance {
    /// The chrome palette the user picked for `scheme` (the RAW slot, before the
    /// macOS→Nice substitution). `active_chrome_palette = scheme==Light ? light :
    /// dark` (`Tweaks.swift:286-288`).
    pub fn chrome_palette_for(&self, scheme: ColorScheme) -> Palette {
        match scheme {
            ColorScheme::Light => self.chrome_light_palette,
            ColorScheme::Dark => self.chrome_dark_palette,
        }
    }

    /// The chrome palette active for the current `scheme`, **macOS-substituted**:
    /// a palette that would resolve to a System-only (None/black) table for the
    /// active scheme — `MacOs` (deferred, OQ6) or an off-scheme Catppuccin — is
    /// replaced by [`Palette::Nice`] so nothing ever paints black. Every other
    /// palette passes through unchanged.
    pub fn active_chrome_palette(&self) -> Palette {
        substitute_unpaintable(self.chrome_palette_for(self.scheme), self.scheme)
    }

    /// The concrete slot table for the active (substituted) chrome palette. Always
    /// resolves (the substitution guarantees a valid, non-System table).
    pub fn active_slots(&self) -> Slots {
        let palette = self.active_chrome_palette();
        slots(palette, self.scheme)
            .expect("the macOS/off-scheme substitution always yields a resolvable palette")
    }

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
}

/// Substitute [`Palette::Nice`] for any palette that cannot paint the active
/// `scheme`: `MacOs` (its slots are System-semantic — deferred, would resolve to
/// opaque black, OQ6) or an off-scheme single-scheme Catppuccin (no table for
/// this scheme). Everything resolvable passes through. This is the tripwire that
/// keeps a stray persisted pref from ever shipping a black UI.
fn substitute_unpaintable(palette: Palette, scheme: ColorScheme) -> Palette {
    if palette == Palette::MacOs {
        return Palette::Nice;
    }
    match slots(palette, scheme) {
        Some(_) => palette,
        None => Palette::Nice,
    }
}

/// Map a persisted palette rawValue to a [`Palette`]. Unknown ⇒ `None` (the
/// caller defaults). RawValues: `Tweaks.swift:33-34` / `palette.rs:61-68`.
fn palette_from_raw(raw: &str) -> Option<Palette> {
    Palette::ALL.into_iter().find(|p| p.raw_value() == raw)
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
/// decode). Serialized with the current selection; decoded permissively.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AppearanceSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sync_with_os: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chrome_light_palette: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chrome_dark_palette: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_theme_light_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_theme_dark_id: Option<String>,
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
            chrome_light_palette: Some(a.chrome_light_palette.raw_value().to_string()),
            chrome_dark_palette: Some(a.chrome_dark_palette.raw_value().to_string()),
            accent: Some(a.accent.raw_value().to_string()),
            terminal_theme_light_id: Some(a.terminal_theme_light_id.clone()),
            terminal_theme_dark_id: Some(a.terminal_theme_dark_id.clone()),
        }
    }

    /// Decode into an [`Appearance`], filling every absent / unknown field with
    /// its default (tolerant, Swift fail-soft parity).
    fn into_appearance(self) -> Appearance {
        let d = Appearance::default();
        Appearance {
            scheme: self
                .scheme
                .as_deref()
                .and_then(scheme_from_raw)
                .unwrap_or(d.scheme),
            sync_with_os: self.sync_with_os.unwrap_or(d.sync_with_os),
            chrome_light_palette: self
                .chrome_light_palette
                .as_deref()
                .and_then(palette_from_raw)
                .unwrap_or(d.chrome_light_palette),
            chrome_dark_palette: self
                .chrome_dark_palette
                .as_deref()
                .and_then(palette_from_raw)
                .unwrap_or(d.chrome_dark_palette),
            accent: self
                .accent
                .as_deref()
                .and_then(accent_from_raw)
                .unwrap_or(d.accent),
            terminal_theme_light_id: self
                .terminal_theme_light_id
                .unwrap_or(d.terminal_theme_light_id),
            terminal_theme_dark_id: self
                .terminal_theme_dark_id
                .unwrap_or(d.terminal_theme_dark_id),
        }
    }
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

    /// The active chrome palette (macOS-substituted).
    pub fn active_chrome_palette(&self) -> Palette {
        self.appearance.active_chrome_palette()
    }

    /// The active chrome slot table.
    pub fn active_slots(&self) -> Slots {
        self.appearance.active_slots()
    }

    /// The active accent.
    pub fn active_accent(&self) -> AccentPreset {
        self.appearance.active_accent()
    }

    /// The active terminal-theme id (resolve via the catalog).
    pub fn active_terminal_id(&self) -> &str {
        self.appearance.active_terminal_id()
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
/// so `<support-root>/Nice RS Dev/ui_settings.json` with `<support-root>` from
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
    /// The active chrome slot table (macOS-substituted — never a System/black
    /// table).
    pub slots: Slots,
    /// The active color scheme.
    pub scheme: ColorScheme,
    /// The active chrome palette (macOS-substituted).
    pub palette: Palette,
    /// The active accent as a concrete sRGB color (the caret / selection / logo
    /// tint).
    pub accent: Srgba,
    /// The active terminal render theme (the catalog-resolved
    /// [`nice_term_view::TerminalTheme`]; unknown id ⇒ the Nice default per
    /// scheme).
    pub terminal_theme: nice_term_view::TerminalTheme,
}

impl ThemeState {
    /// Derive the resolved view state from a store selection + the catalog. Slice
    /// 3's `apply_*` mutators call this after each store mutation to refresh the
    /// [`SharedThemeState`] entity, then fan out.
    pub fn from_stores(store: &ThemeSettingsStore, catalog: &TerminalThemeCatalog) -> Self {
        let scheme = store.active_scheme();
        Self {
            slots: store.active_slots(),
            scheme,
            palette: store.active_chrome_palette(),
            accent: store.active_accent().color(),
            terminal_theme: catalog.resolve(store.active_terminal_id(), scheme),
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

/// The shipped Nice/Dark chrome table — the fallback when [`SharedThemeState`] is
/// absent. Kept out of the chrome files so no live render path hardcodes
/// `slots(Palette::Nice, ColorScheme::Dark)` (Validation §3 hardcode grep).
fn fallback_chrome_slots() -> Slots {
    slots(Palette::Nice, ColorScheme::Dark)
        .expect("Nice + Dark is a valid palette/scheme combo")
}

/// Install the live theme globals for the shipped app (`app::run` ONLY, before
/// the first window): the production [`OsSchemeSource`], the loaded `store` (its
/// scheme reconciled to the OS appearance in memory for a sync-on install), the
/// built-in catalog stub, and the [`SharedThemeState`] entity minted from them.
/// The per-window `Window::observe_window_appearance` adapter (wired in
/// `build_window_root`) and the R17-live Claude write ride on top of this.
pub fn install_live_theme(cx: &mut App, mut store: ThemeSettingsStore) {
    // Register the production OS-scheme source first, then reconcile the loaded
    // selection to the current OS appearance in memory (a sync-on fresh install
    // adopts the system scheme — the `scheme = OS at first launch` default) BEFORE
    // the first `ThemeState` derives. No boot write (the runtime reconcile persists;
    // this seed does not).
    install_production_os_scheme_source(cx);
    if let Some(os) = current_os_scheme(cx) {
        store.reconcile_scheme_in_memory(os);
    }
    let catalog = TerminalThemeCatalog::with_builtins();
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

/// Set the chrome palette for one scheme's slot (the picker is per-scheme). A
/// change to the INACTIVE scheme's slot is latent (no active repaint) until a
/// scheme flip makes it active.
pub fn apply_chrome_palette(cx: &mut App, scheme: ColorScheme, palette: Palette) {
    let Some(mut appearance) = current_appearance(cx) else {
        return;
    };
    match scheme {
        ColorScheme::Light => appearance.chrome_light_palette = palette,
        ColorScheme::Dark => appearance.chrome_dark_palette = palette,
    }
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
            eprintln!("nice-rs: theme store persist failed: {e}");
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
    // Chrome: every window re-renders and re-reads the active `Slots`/accent.
    cx.refresh_windows();
    // Panes: collect the hosts first (each read borrows `cx`), then push into each.
    let hosts: Vec<_> = crate::window_registry::WindowRegistry::all_states(cx)
        .into_iter()
        .filter_map(|ws| ws.read(cx).pane_host())
        .collect();
    for host in hosts {
        host.update(cx, |h, cx| {
            h.set_theme(state.terminal_theme.clone(), state.accent, cx)
        });
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
/// and the catalog stub, but NOT [`SharedThemeState`] — scenarios paint the
/// Nice/Dark fallback unless a scenario opts into live theming by minting the
/// entity itself (slice 3's `theme-fanout`). No launch-time write (hermeticity).
pub fn install_selftest_theme_defaults(cx: &mut App, store: ThemeSettingsStore) {
    cx.set_global(store);
    cx.set_global(TerminalThemeCatalog::with_builtins());
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

    /// Fresh-install defaults (dossier §2.1): the exact contract-table values.
    #[test]
    fn fresh_install_defaults() {
        let a = Appearance::default();
        assert_eq!(a.scheme, ColorScheme::Dark);
        assert!(a.sync_with_os);
        assert_eq!(a.chrome_light_palette, Palette::CatppuccinLatte);
        assert_eq!(a.chrome_dark_palette, Palette::CatppuccinMocha);
        assert_eq!(a.accent, AccentPreset::Ocean);
        assert_eq!(a.terminal_theme_light_id, "catppuccin-latte");
        assert_eq!(a.terminal_theme_dark_id, "catppuccin-mocha");
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
            chrome_light_palette: Palette::Nice,
            chrome_dark_palette: Palette::Nice,
            accent: AccentPreset::Iris,
            terminal_theme_light_id: "solarized-light".to_string(),
            terminal_theme_dark_id: "dracula".to_string(),
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

    /// Tolerance: an unknown palette / accent / scheme rawValue and a missing
    /// field all fall back to that field's default; known fields still decode.
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
        assert_eq!(a.chrome_light_palette, d.chrome_light_palette);
        assert_eq!(a.accent, d.accent);
        // Missing fields → defaults.
        assert!(a.sync_with_os);
        assert_eq!(a.chrome_dark_palette, d.chrome_dark_palette);
        assert_eq!(a.terminal_theme_light_id, d.terminal_theme_light_id);
        // A known field still decodes.
        assert_eq!(a.terminal_theme_dark_id, "gruvbox-dark");
    }

    /// Tolerance: a persisted `"macOS"` palette rawValue decodes as `MacOs` (shape
    /// tolerance) but the ACTIVE derivation substitutes Nice — never black (OQ6).
    #[test]
    fn macos_palette_tolerated_but_substituted() {
        let path = temp_path("macos");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{"scheme":"dark","chrome_dark_palette":"macOS"}}"#,
        )
        .unwrap();
        let store = ThemeSettingsStore::load(path);
        // Shape-tolerated: the raw slot really is MacOs.
        assert_eq!(store.appearance().chrome_dark_palette, Palette::MacOs);
        // Derived: substituted to Nice, and the slot table is the Nice dark table
        // (a concrete literal, NOT a System/black slot).
        assert_eq!(store.active_chrome_palette(), Palette::Nice);
        assert_eq!(store.active_slots(), nice_theme::palette::NICE_DARK);
    }

    /// Malformed JSON is fail-soft: full defaults, no crash.
    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = ThemeSettingsStore::load(path);
        assert_eq!(*store.appearance(), Appearance::default());
    }

    /// Active-palette derivation across schemes, including the macOS→Nice and
    /// off-scheme-Catppuccin→Nice substitutions.
    #[test]
    fn active_chrome_palette_derivation() {
        // Nice both schemes: passes through, resolves to the Nice tables.
        let nice = Appearance {
            chrome_light_palette: Palette::Nice,
            chrome_dark_palette: Palette::Nice,
            ..Appearance::default()
        };
        let light = Appearance {
            scheme: ColorScheme::Light,
            ..nice.clone()
        };
        assert_eq!(light.active_chrome_palette(), Palette::Nice);
        assert_eq!(light.active_slots(), nice_theme::palette::NICE_LIGHT);
        let dark = Appearance {
            scheme: ColorScheme::Dark,
            ..nice
        };
        assert_eq!(dark.active_chrome_palette(), Palette::Nice);
        assert_eq!(dark.active_slots(), nice_theme::palette::NICE_DARK);

        // Default Catppuccin slots pass through for their own scheme.
        let latte = Appearance {
            scheme: ColorScheme::Light,
            ..Appearance::default()
        };
        assert_eq!(latte.active_chrome_palette(), Palette::CatppuccinLatte);
        assert_eq!(latte.active_slots(), nice_theme::palette::CATPPUCCIN_LATTE);
        let mocha = Appearance {
            scheme: ColorScheme::Dark,
            ..Appearance::default()
        };
        assert_eq!(mocha.active_chrome_palette(), Palette::CatppuccinMocha);
        assert_eq!(mocha.active_slots(), nice_theme::palette::CATPPUCCIN_MOCHA);

        // macOS → Nice for the active scheme (both schemes).
        let macos = Appearance {
            chrome_light_palette: Palette::MacOs,
            chrome_dark_palette: Palette::MacOs,
            ..Appearance::default()
        };
        let ml = Appearance {
            scheme: ColorScheme::Light,
            ..macos.clone()
        };
        assert_eq!(ml.active_chrome_palette(), Palette::Nice);
        let md = Appearance {
            scheme: ColorScheme::Dark,
            ..macos
        };
        assert_eq!(md.active_chrome_palette(), Palette::Nice);

        // An off-scheme single-scheme Catppuccin (Latte pinned to the dark slot)
        // has no table for Dark → substituted Nice, never black.
        let off = Appearance {
            scheme: ColorScheme::Dark,
            chrome_dark_palette: Palette::CatppuccinLatte,
            ..Appearance::default()
        };
        assert_eq!(off.active_chrome_palette(), Palette::Nice);
        assert_eq!(off.active_slots(), nice_theme::palette::NICE_DARK);
    }

    /// `active_terminal_id` selects the per-scheme slot; combined with the stub
    /// catalog the default Catppuccin ids resolve to the Nice default (the
    /// R22-additive fallback). Named per Validation §1.
    #[test]
    fn active_terminal_id_and_unknown_id_nice_default_fallback() {
        use crate::terminal_theme_catalog::TerminalThemeCatalog;
        let catalog = TerminalThemeCatalog::with_builtins();

        let a = Appearance::default();

        // Per-scheme id selection.
        let light = Appearance {
            scheme: ColorScheme::Light,
            ..a.clone()
        };
        assert_eq!(light.active_terminal_id(), "catppuccin-latte");
        let dark = Appearance {
            scheme: ColorScheme::Dark,
            ..a
        };
        assert_eq!(dark.active_terminal_id(), "catppuccin-mocha");

        // The stub catalog lacks those ids → Nice default per scheme.
        assert_eq!(
            catalog.resolve(light.active_terminal_id(), ColorScheme::Light),
            nice_term_view::TerminalTheme::nice_default_light()
        );
        assert_eq!(
            catalog.resolve(dark.active_terminal_id(), ColorScheme::Dark),
            nice_term_view::TerminalTheme::nice_default_dark()
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
