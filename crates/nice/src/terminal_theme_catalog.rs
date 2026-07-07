//! The terminal-theme catalog — the id → [`nice_term_view::TerminalTheme`]
//! resolution seam. R21 shipped a two-entry stub; **R22 fills it**: the full
//! 12-entry bundled table ([`crate::built_in_terminal_themes`]) plus the user's
//! Ghostty-imported theme files enumerated from
//! `<support-root>/Nice RS Dev/terminal-themes/` (the
//! `NICE_APPLICATION_SUPPORT_ROOT` + [`session_store::STORE_FOLDER`]
//! (`crate::session_store::STORE_FOLDER`) convention).
//!
//! The fill is purely additive: **R22 replaces the built-in table and extends
//! the lookup bodies WITHOUT changing [`TerminalThemeCatalog::resolve`] /
//! [`TerminalThemeCatalog::themes`] signatures or their fallback contract**
//! (Exported contracts, R21 plan). R21's Catppuccin default ids
//! (`catppuccin-latte` / `catppuccin-mocha`) are now real built-ins, so they
//! resolve to the Catppuccin themes rather than the Nice-default fallback — the
//! additive upgrade, no default-flip.
//!
//! Import / remove / enumeration mirror `TerminalThemeCatalog.swift`:
//! * [`import_theme`](TerminalThemeCatalog::import_theme) reads a chosen file,
//!   parses it, writes the ORIGINAL source verbatim to `<slug>.ghostty` under the
//!   support dir (atomic temp+rename), and inserts/replaces (dedup by slug) in
//!   the in-memory imported list — it does NOT select or fan out the theme (R23
//!   calls R21's `apply_terminal_theme_id` to make it live).
//! * [`remove_imported`](TerminalThemeCatalog::remove_imported) best-effort
//!   deletes the backing file + drops the entry; a built-in / unknown id is a
//!   no-op.
//! * Enumeration is **boot-only + refresh-on-mutation** (Swift parity; no file
//!   watcher). A malformed file is dropped silently so one bad file never blocks
//!   the rest; a missing/unreadable dir ⇒ empty imported list.
//!
//! The mutating surface is written as `&mut self` methods; a caller holding the
//! process gpui `Global` drives them through `cx.global_mut::<TerminalThemeCatalog>()`
//! (the `import_theme(cx, …)` / `remove_imported(cx, …)` Exported contract) —
//! e.g. `cx.global_mut::<TerminalThemeCatalog>().import_theme(&path)`. Keeping the
//! core `&mut self` also lets the in-crate suites exercise it without a gpui `App`.
//!
//! Boundary note (TRANCHE-2-NOTES §4): the catalog, the parser, the built-in
//! table, the import/remove logic, the imported-file storage, the slug/display
//! helpers, and the [`ThemeImportError`] type are all **app-crate** concerns —
//! they compose catalog metadata (id / display name / scope) over the
//! render-subset [`nice_term_view::TerminalTheme`], which carries no catalog
//! fields. Nothing here leaks into `nice-term-*`.

#![allow(dead_code)] // R23's UI consumes the import/remove/entries surface.

use std::path::{Path, PathBuf};

use gpui::{App, Global};
use nice_term_view::TerminalTheme;
use nice_theme::ColorScheme;

use crate::built_in_terminal_themes::{built_in_terminal_themes, BuiltInTheme};
use crate::ghostty_theme_parser::{parse_ghostty_theme, GhosttyParseError};

/// Whether a theme is designed for one scheme or works in either. Ported from
/// `TerminalTheme.Scope` (`TerminalTheme.swift:99-108`). Imported themes default
/// to [`ThemeScope::Either`] (the file format records no authorial intent) — R22.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeScope {
    /// Designed for light mode; appears only in the light-mode picker.
    Light,
    /// Designed for dark mode; appears only in the dark-mode picker.
    Dark,
    /// Works in either scheme; appears in both pickers.
    Either,
}

impl ThemeScope {
    /// Whether a theme with this scope belongs in the picker for `scheme`.
    /// `.either` matches both sides. Verbatim from `TerminalTheme.matches`
    /// (`TerminalTheme.swift:117-124`).
    pub fn matches(self, scheme: ColorScheme) -> bool {
        match (self, scheme) {
            (ThemeScope::Either, _) => true,
            (ThemeScope::Light, ColorScheme::Light) => true,
            (ThemeScope::Dark, ColorScheme::Dark) => true,
            _ => false,
        }
    }
}

/// One picker row: the metadata half of a catalog theme (the render half is the
/// [`TerminalTheme`] the catalog resolves to). R23's per-scheme picker renders a
/// list of these; the imported ones (all scope [`ThemeScope::Either`]) are its
/// deletable set — see [`TerminalThemeCatalog::imported_entries`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The stable id persisted in the `appearance` section
    /// (`terminal_theme_light_id` / `terminal_theme_dark_id`).
    pub id: String,
    /// The human-facing name for the picker.
    pub display_name: String,
    /// Which scheme(s) this theme is offered in.
    pub scope: ThemeScope,
}

/// The typed import failure R23's Appearance pane maps to a display string. Ports
/// `TerminalThemeCatalog.ImportError` (`TerminalThemeCatalog.swift:83-93`) — the
/// inner [`GhosttyParseError`] preserves the specific parse failure. R22 exports
/// the typed error only; R23 owns the human-readable mapping (`ImportErrorWrapper`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeImportError {
    /// The chosen file could not be read (I/O error, permissions, bad UTF-8). The
    /// message rides along so the UI can surface something actionable.
    CannotRead(String),
    /// The Ghostty parser rejected the file.
    ParseFailed(GhosttyParseError),
    /// Reading succeeded and the file parsed, but the copy into the support
    /// directory failed.
    CannotPersist(String),
}

/// One imported theme: the metadata + render payload PLUS the backing file path
/// (the render [`TerminalTheme`] carries no source, so the catalog tracks the
/// file to delete on [`remove_imported`](TerminalThemeCatalog::remove_imported)).
/// The file may be a `.ghostty` (import-written) or a hand-placed `.conf`.
#[derive(Clone, Debug)]
struct ImportedTheme {
    id: String,
    display_name: String,
    theme: TerminalTheme,
    /// The backing file on disk (deleted by `remove_imported`).
    path: PathBuf,
}

/// The process-wide terminal-theme catalog gpui `Global`. Carries the full
/// built-in table + the imported files enumerated from [`support_dir`](Self::support_dir)
/// at construction, behind R21's unchanged [`resolve`](Self::resolve) /
/// [`themes`](Self::themes) seam.
pub struct TerminalThemeCatalog {
    /// The bundled built-ins in `BuiltInTerminalThemes.all` order (never re-sorted).
    builtins: Vec<BuiltInTheme>,
    /// The user-imported themes, kept sorted by `display_name`. All scope
    /// [`ThemeScope::Either`].
    imported: Vec<ImportedTheme>,
    /// The directory imported files live in (`<support-root>/Nice RS Dev/
    /// terminal-themes/`). `import_theme` writes here; `remove_imported` deletes
    /// from here. Injectable so tests / `run_selftest` point at a temp dir.
    support_dir: PathBuf,
}

impl Global for TerminalThemeCatalog {}

impl TerminalThemeCatalog {
    /// Build the catalog: the full built-in table + the imported themes
    /// enumerated from `support_dir`. Enumeration is **read-only** — it never
    /// creates `support_dir` (a missing dir ⇒ empty imported list), so
    /// `run_selftest` can hand it a throwaway temp path with no launch-time
    /// write (hermeticity). `app::run` creates the real dir on demand before
    /// constructing.
    pub fn new(support_dir: PathBuf) -> Self {
        let imported = Self::enumerate(&support_dir);
        Self {
            builtins: built_in_terminal_themes(),
            imported,
            support_dir,
        }
    }

    /// Resolve a persisted terminal-theme `id` for `scheme` to a concrete
    /// [`TerminalTheme`]. Lookup order (Swift `theme(withId:)`,
    /// `TerminalThemeCatalog.swift:76-79`): a **built-in whose scope matches**
    /// `scheme` ⇒ that theme; else an **imported theme with that id** (all imports
    /// are scope [`ThemeScope::Either`], so they match either scheme) ⇒ that theme;
    /// else the Nice default for `scheme` (`nice_default_light()` /
    /// `nice_default_dark()` — R21's unchanged fallback).
    ///
    /// This is a **deliberate one-level** fallback (R21 plan / dossier §2): a
    /// stray / deleted id renders the Nice default rather than Swift's
    /// two-level `id → default-id → nice_default`. With R22's full table the
    /// normal case is identical to Swift; only a *deleted-imported selected id*
    /// differs — a benign divergence R21 already baked in.
    pub fn resolve(&self, id: &str, scheme: ColorScheme) -> TerminalTheme {
        for b in &self.builtins {
            if b.id == id && b.scope.matches(scheme) {
                return b.theme.clone();
            }
        }
        for imp in &self.imported {
            if imp.id == id {
                return imp.theme.clone();
            }
        }
        match scheme {
            ColorScheme::Light => TerminalTheme::nice_default_light(),
            ColorScheme::Dark => TerminalTheme::nice_default_dark(),
        }
    }

    /// The ordered picker list for one `scheme` (Swift `themes(for:)`,
    /// `TerminalThemeCatalog.swift:67-71`): every built-in whose scope matches, in
    /// bundled `all` order, THEN the imported themes (all scope
    /// [`ThemeScope::Either`], so present in both schemes) sorted by
    /// `display_name`. R23's per-scheme picker.
    pub fn themes(&self, scheme: ColorScheme) -> Vec<CatalogEntry> {
        let mut out: Vec<CatalogEntry> = self
            .builtins
            .iter()
            .filter(|b| b.scope.matches(scheme))
            .map(|b| CatalogEntry {
                id: b.id.to_string(),
                display_name: b.display_name.to_string(),
                scope: b.scope,
            })
            .collect();
        // `imported` is already sorted by display_name (on enumerate + each
        // mutation), so appending in list order preserves the sort.
        out.extend(self.imported.iter().map(imported_entry));
        out
    }

    /// The imported themes only, sorted by `display_name` — R23's deletable-theme
    /// list. `CatalogEntry` carries no source flag, so membership here is how R23
    /// distinguishes removable imports from built-ins (gate the trash affordance
    /// on it).
    pub fn imported_entries(&self) -> Vec<CatalogEntry> {
        self.imported.iter().map(imported_entry).collect()
    }

    /// Import `path` into the catalog (Swift `importTheme`,
    /// `TerminalThemeCatalog.swift:100-140`): read the file as UTF-8
    /// ([`ThemeImportError::CannotRead`] on failure); derive `id = slug(stem)` /
    /// `display_name = display_name(stem)`; parse the source
    /// ([`ThemeImportError::ParseFailed`] on failure); write the ORIGINAL source
    /// verbatim to `<support-dir>/<id>.ghostty` atomically
    /// ([`ThemeImportError::CannotPersist`] on failure); replace any same-`id`
    /// entry in the imported list (dedup by slug, last wins) and re-sort by
    /// `display_name`. Returns the new [`CatalogEntry`]. Does NOT select or fan
    /// out the theme.
    ///
    /// A caller holding the gpui `Global` drives this through
    /// `cx.global_mut::<TerminalThemeCatalog>().import_theme(&path)`.
    pub fn import_theme(&mut self, path: &Path) -> Result<CatalogEntry, ThemeImportError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| ThemeImportError::CannotRead(e.to_string()))?;

        let stem = file_stem(path);
        let id = slug(&stem);
        let display = display_name(&stem);

        // Parse BEFORE any write (Swift order): a bad file never leaves a
        // half-written destination.
        let theme = parse_ghostty_theme(&source).map_err(ThemeImportError::ParseFailed)?;

        let destination = self.support_dir.join(format!("{id}.ghostty"));
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ThemeImportError::CannotPersist(e.to_string()))?;
        }
        // The original source text is written verbatim (not re-serialized); the
        // atomic temp+rename handles a same-slug replace.
        crate::atomic_file::write_atomic(&destination, source.as_bytes(), None)
            .map_err(|e| ThemeImportError::CannotPersist(e.to_string()))?;

        self.imported.retain(|i| i.id != id);
        self.imported.push(ImportedTheme {
            id: id.clone(),
            display_name: display.clone(),
            theme,
            path: destination,
        });
        self.imported.sort_by(|a, b| a.display_name.cmp(&b.display_name));

        Ok(CatalogEntry {
            id,
            display_name: display,
            scope: ThemeScope::Either,
        })
    }

    /// Remove an imported theme by `id` (Swift `remove`,
    /// `TerminalThemeCatalog.swift:145-149`): best-effort delete the backing file
    /// (an fs error is ignored, Swift's `try?`) and drop the entry. Returns `true`
    /// iff an imported theme was removed; a built-in or unknown id is a no-op ⇒
    /// `false` (built-ins can never be removed).
    ///
    /// A caller holding the gpui `Global` drives this through
    /// `cx.global_mut::<TerminalThemeCatalog>().remove_imported(id)`.
    pub fn remove_imported(&mut self, id: &str) -> bool {
        let mut removed = false;
        self.imported.retain(|i| {
            if i.id == id {
                let _ = std::fs::remove_file(&i.path);
                removed = true;
                false
            } else {
                true
            }
        });
        removed
    }

    /// The imported-theme storage dir this catalog reads/writes (test / callsite
    /// introspection).
    pub fn support_dir(&self) -> &Path {
        &self.support_dir
    }

    /// Re-read every `.ghostty` / `.conf` file in `support_dir` (Swift
    /// `reloadImported`, `TerminalThemeCatalog.swift:156-184`): skip hidden files;
    /// accept a lowercased extension of `ghostty` or `conf`; parse each; **drop a
    /// file that fails to parse silently** (one bad file never blocks the rest);
    /// sort by `display_name`. A missing/unreadable dir ⇒ empty list, never an
    /// error.
    fn enumerate(support_dir: &Path) -> Vec<ImportedTheme> {
        let Ok(entries) = std::fs::read_dir(support_dir) else {
            return Vec::new();
        };
        let mut parsed: Vec<ImportedTheme> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // skipsHiddenFiles.
            if name.starts_with('.') {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if !matches!(ext.as_deref(), Some("ghostty") | Some("conf")) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let stem = file_stem(&path);
            if let Ok(theme) = parse_ghostty_theme(&source) {
                parsed.push(ImportedTheme {
                    id: slug(&stem),
                    display_name: display_name(&stem),
                    theme,
                    path,
                });
            }
        }
        parsed.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        parsed
    }
}

/// The metadata view of an imported theme (always scope [`ThemeScope::Either`]).
fn imported_entry(imp: &ImportedTheme) -> CatalogEntry {
    CatalogEntry {
        id: imp.id.clone(),
        display_name: imp.display_name.clone(),
        scope: ThemeScope::Either,
    }
}

/// The filename without its extension (the slug/display-name source). Empty when
/// the path has no file name.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Slug an imported theme's filename into a stable id (Swift `slug(from:)`,
/// `TerminalThemeCatalog.swift:191-206`): lowercase; collapse each run of
/// non-alphanumerics to a single `-`; trim trailing `-`; empty ⇒ `"imported"`.
/// "Catppuccin Frappe" / "catppuccin_frappe" / "catppuccin-frappe" all →
/// `"catppuccin-frappe"`. Reusable by any future importer.
pub fn slug(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut out = String::new();
    let mut last_was_hyphen = false;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_was_hyphen = false;
        } else if !last_was_hyphen && !out.is_empty() {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "imported".to_string()
    } else {
        out
    }
}

/// Human-friendly display name from a filename (Swift `displayName(from:)`,
/// `TerminalThemeCatalog.swift:210-218`): replace `_` and `-` with spaces;
/// capitalize the first letter of each space-split word (the remainder keeps its
/// case). Reusable by any future importer.
pub fn display_name(name: &str) -> String {
    let spaced: String = name
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    spaced
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve the imported-theme storage dir: `<support-root>/Nice RS Dev/
/// terminal-themes`, where `<support-root>` = `NICE_APPLICATION_SUPPORT_ROOT`
/// when set (tests / scenarios redirect state into a sandbox) else
/// `~/Library/Application Support`. Reuses [`session_store::STORE_FOLDER`]
/// (`crate::session_store::STORE_FOLDER`). Called from `app::run` ONLY (the
/// `sort_settings_store::default_ui_settings_path` convention) — never a test or
/// `run_selftest`.
pub fn default_terminal_themes_dir() -> PathBuf {
    let root = match std::env::var("NICE_APPLICATION_SUPPORT_ROOT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            PathBuf::from(home).join("Library/Application Support")
        }
    };
    root.join(crate::session_store::STORE_FOLDER)
        .join("terminal-themes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique temp dir for a hermetic catalog (never the real support root).
    /// NOT created on disk — `TerminalThemeCatalog::new` enumerates read-only, so
    /// a nonexistent dir yields an empty imported list with no write.
    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "nice-rs-catalog-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A catalog over an empty (nonexistent) temp support dir — built-ins only.
    fn catalog_builtins_only(tag: &str) -> TerminalThemeCatalog {
        TerminalThemeCatalog::new(temp_dir(tag))
    }

    /// A minimal, well-formed Ghostty theme source with a given background hex and
    /// a full 16-entry palette (so it parses). `bg` is `rrggbb` (no `#`).
    fn ghostty_source(bg: &str) -> String {
        let mut s = format!("background = #{bg}\nforeground = #ffffff\n");
        for i in 0..16u8 {
            s.push_str(&format!("palette = {i}=#0000{i:02x}\n"));
        }
        s
    }

    // ---- ThemeScope ----------------------------------------------------------

    #[test]
    fn scope_matches_mirrors_swift() {
        // TerminalTheme.swift:117-124.
        assert!(ThemeScope::Light.matches(ColorScheme::Light));
        assert!(!ThemeScope::Light.matches(ColorScheme::Dark));
        assert!(ThemeScope::Dark.matches(ColorScheme::Dark));
        assert!(!ThemeScope::Dark.matches(ColorScheme::Light));
        assert!(ThemeScope::Either.matches(ColorScheme::Light));
        assert!(ThemeScope::Either.matches(ColorScheme::Dark));
    }

    // ---- slug / display_name (Validation §1) ---------------------------------

    #[test]
    fn slug_collapses_common_filename_conventions() {
        // The three conventions user theme packs ship with all collapse alike.
        assert_eq!(slug("Catppuccin Frappe"), "catppuccin-frappe");
        assert_eq!(slug("catppuccin_frappe"), "catppuccin-frappe");
        assert_eq!(slug("catppuccin-frappe"), "catppuccin-frappe");
    }

    #[test]
    fn slug_collapses_runs_and_trims_trailing() {
        // A run of non-alphanumerics collapses to ONE hyphen; trailing trimmed.
        assert_eq!(slug("Tokyo   Night!!"), "tokyo-night");
        assert_eq!(slug("--leading and trailing--"), "leading-and-trailing");
    }

    #[test]
    fn slug_empty_becomes_imported() {
        // No alphanumerics at all ⇒ the "imported" fallback.
        assert_eq!(slug(""), "imported");
        assert_eq!(slug("   "), "imported");
        assert_eq!(slug("---"), "imported");
    }

    #[test]
    fn display_name_spaces_and_capitalizes() {
        assert_eq!(display_name("catppuccin-frappe"), "Catppuccin Frappe");
        assert_eq!(display_name("tokyo_night"), "Tokyo Night");
        // Multiple separators collapse (empty words dropped, Swift split parity).
        assert_eq!(display_name("one--dark"), "One Dark");
        // The remainder of each word keeps its original case.
        assert_eq!(display_name("myTheme-v2"), "MyTheme V2");
    }

    // ---- resolve (Validation §1) ---------------------------------------------

    #[test]
    fn resolve_known_builtin_returns_that_theme() {
        let cat = catalog_builtins_only("resolve-builtin");
        assert_eq!(
            cat.resolve("nice-default-light", ColorScheme::Light),
            TerminalTheme::nice_default_light()
        );
        assert_eq!(
            cat.resolve("solarized-dark", ColorScheme::Dark),
            crate::built_in_terminal_themes::built_in_terminal_themes()
                .into_iter()
                .find(|t| t.id == "solarized-dark")
                .unwrap()
                .theme
        );
    }

    /// The R22-additive guarantee: the Catppuccin default ids R21 persists are now
    /// REAL built-ins, so they resolve to the Catppuccin themes (NOT the Nice
    /// default) — the additive upgrade, no default-flip. Named per Validation §1.
    #[test]
    fn resolve_catppuccin_default_ids_are_now_real_builtins() {
        let cat = catalog_builtins_only("resolve-catppuccin");
        let table = crate::built_in_terminal_themes::built_in_terminal_themes();
        let latte = table.iter().find(|t| t.id == "catppuccin-latte").unwrap();
        let mocha = table.iter().find(|t| t.id == "catppuccin-mocha").unwrap();
        assert_eq!(cat.resolve("catppuccin-latte", ColorScheme::Light), latte.theme);
        assert_eq!(cat.resolve("catppuccin-mocha", ColorScheme::Dark), mocha.theme);
        // And they are NOT the Nice default (proving the fallback is not taken).
        assert_ne!(
            cat.resolve("catppuccin-mocha", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
    }

    #[test]
    fn resolve_unknown_id_falls_back_to_nice_default_per_scheme() {
        let cat = catalog_builtins_only("resolve-unknown");
        assert_eq!(
            cat.resolve("does-not-exist", ColorScheme::Light),
            TerminalTheme::nice_default_light()
        );
        assert_eq!(
            cat.resolve("", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
    }

    #[test]
    fn resolve_known_builtin_off_scheme_falls_back() {
        let cat = catalog_builtins_only("resolve-offscheme");
        // solarized-light is scope Light; resolving it for Dark falls back.
        assert_eq!(
            cat.resolve("solarized-light", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );
    }

    // ---- themes(for:) (Validation §1) ----------------------------------------

    #[test]
    fn themes_for_returns_builtins_in_bundled_order_then_sorted_imports() {
        let mut cat = catalog_builtins_only("themes-order");

        // The Light picker: exactly the scope-Light built-ins in `all` order.
        let light = cat.themes(ColorScheme::Light);
        let light_ids: Vec<&str> = light.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            light_ids,
            vec![
                "nice-default-light",
                "solarized-light",
                "gruvbox-light",
                "catppuccin-latte",
            ]
        );

        // Import two themes whose display names sort AFTER the built-ins but out
        // of insertion order — they must appear appended, sorted by display_name.
        let dir = cat.support_dir().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let zebra = dir.join("zebra.ghostty");
        let alpha = dir.join("alpha.ghostty");
        std::fs::write(&zebra, ghostty_source("111111")).unwrap();
        std::fs::write(&alpha, ghostty_source("222222")).unwrap();
        cat.import_theme(&zebra).unwrap();
        cat.import_theme(&alpha).unwrap();

        let dark = cat.themes(ColorScheme::Dark);
        let dark_ids: Vec<&str> = dark.iter().map(|e| e.id.as_str()).collect();
        // Built-in Dark block (bundled order) THEN imports sorted by display name.
        assert_eq!(
            dark_ids,
            vec![
                "nice-default-dark",
                "solarized-dark",
                "dracula",
                "nord",
                "gruvbox-dark",
                "catppuccin-mocha",
                "tokyo-night",
                "one-dark",
                "alpha",
                "zebra",
            ]
        );
        // Imports carry scope Either and appear in BOTH pickers.
        let light2 = cat.themes(ColorScheme::Light);
        let light2_ids: Vec<&str> = light2.iter().map(|e| e.id.as_str()).collect();
        assert!(light2_ids.ends_with(&["alpha", "zebra"]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- import round-trip / dedup / enumerate / remove (Validation §1) ------

    #[test]
    fn import_round_trip_persists_and_resolves() {
        let mut cat = catalog_builtins_only("import-roundtrip");
        let dir = cat.support_dir().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        // A source file named with mixed case / separators — id slugs it.
        let src = dir.join("My Cool Theme.ghostty");
        std::fs::write(&src, ghostty_source("abcdef")).unwrap();

        let entry = cat.import_theme(&src).expect("import parses + persists");
        assert_eq!(entry.id, "my-cool-theme");
        assert_eq!(entry.display_name, "My Cool Theme");
        assert_eq!(entry.scope, ThemeScope::Either);

        // Written under the temp support dir as `<slug>.ghostty`.
        let persisted = dir.join("my-cool-theme.ghostty");
        assert!(persisted.exists(), "the slug-named file was written");

        // In the imported set + resolvable by id for either scheme.
        assert!(cat.imported_entries().iter().any(|e| e.id == "my-cool-theme"));
        let resolved = cat.resolve("my-cool-theme", ColorScheme::Dark);
        assert_eq!(resolved.background, nice_term_view::TerminalColor::new(0xab, 0xcd, 0xef));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_slug_reimport_replaces_last_wins() {
        let mut cat = catalog_builtins_only("import-dedup");
        let dir = cat.support_dir().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();

        // Two differently-named files that slug identically.
        let a = dir.join("Neon Wave.ghostty");
        let b = dir.join("neon_wave.conf");
        std::fs::write(&a, ghostty_source("010101")).unwrap();
        std::fs::write(&b, ghostty_source("020202")).unwrap();

        let e1 = cat.import_theme(&a).unwrap();
        let e2 = cat.import_theme(&b).unwrap();
        assert_eq!(e1.id, "neon-wave");
        assert_eq!(e2.id, "neon-wave");

        // One entry, last import wins (its background).
        let matches: Vec<_> = cat
            .imported_entries()
            .into_iter()
            .filter(|e| e.id == "neon-wave")
            .collect();
        assert_eq!(matches.len(), 1, "same-slug re-import ⇒ one entry");
        assert_eq!(
            cat.resolve("neon-wave", ColorScheme::Light).background,
            nice_term_view::TerminalColor::new(0x02, 0x02, 0x02)
        );
        // Exactly one on-disk file (both imports target `<slug>.ghostty`).
        assert!(dir.join("neon-wave.ghostty").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enumeration_accepts_conf_and_drops_malformed_silently() {
        let dir = temp_dir("enumerate");
        std::fs::create_dir_all(&dir).unwrap();
        // A valid `.conf` file, a valid `.ghostty` file, and a malformed one.
        std::fs::write(dir.join("good-conf.conf"), ghostty_source("111111")).unwrap();
        std::fs::write(dir.join("good-ghostty.ghostty"), ghostty_source("222222")).unwrap();
        std::fs::write(dir.join("broken.ghostty"), "background = nothex\n").unwrap();
        // A hidden file + an unrelated extension are ignored.
        std::fs::write(dir.join(".hidden.ghostty"), ghostty_source("333333")).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();

        let cat = TerminalThemeCatalog::new(dir.clone());
        let ids: Vec<String> = cat.imported_entries().into_iter().map(|e| e.id).collect();
        // Both valid files enumerated (sorted by display name), malformed dropped.
        assert_eq!(ids, vec!["good-conf".to_string(), "good-ghostty".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_imported_deletes_file_and_entry_and_noops_for_builtin() {
        let mut cat = catalog_builtins_only("remove");
        let dir = cat.support_dir().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("throwaway.ghostty");
        std::fs::write(&src, ghostty_source("0a0b0c")).unwrap();
        cat.import_theme(&src).unwrap();
        let persisted = dir.join("throwaway.ghostty");
        assert!(persisted.exists());

        // Removing a built-in id is a no-op (built-ins are never removable).
        assert!(!cat.remove_imported("nice-default-dark"));
        // Removing an unknown id is a no-op.
        assert!(!cat.remove_imported("does-not-exist"));

        // Removing the import deletes the file + the entry.
        assert!(cat.remove_imported("throwaway"));
        assert!(!persisted.exists(), "the backing file was deleted");
        assert!(cat.imported_entries().is_empty());
        // resolve now falls back (deleted-imported selected id ⇒ Nice default).
        assert_eq!(
            cat.resolve("throwaway", ColorScheme::Dark),
            TerminalTheme::nice_default_dark()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_support_dir_yields_empty_imports_no_write() {
        let dir = temp_dir("missing");
        // Never created on disk.
        let cat = TerminalThemeCatalog::new(dir.clone());
        assert!(cat.imported_entries().is_empty());
        assert!(!dir.exists(), "new() must not create the support dir");
    }
}
