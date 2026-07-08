//! The R23 `fonts` + `advanced` sections of `ui_settings.json` (What-to-build
//! item 9). A tiny process-wide store — the terminal font size / family + sidebar
//! font size (the Font pane, G9) and the inert smooth-scroll toggle (the Advanced
//! pane, D2) — persisted through the SHARED
//! [`write_ui_settings_merged`](crate::file_browser::sort_settings_store::write_ui_settings_merged)
//! read-merge-write writer R21 extracted, so an R23 write never clobbers R21's
//! `appearance`, R19's `file_browser_sort`, or any future co-owner's section.
//!
//! ## Hermeticity
//! The store path is **injected** (the `sort_settings_store` convention): only
//! `app::run` resolves the default location (`default_ui_settings_path`);
//! `run_selftest` installs a defaults + temp-path store and performs no
//! launch-time write. Boot seeding of the terminal/sidebar font entities from the
//! loaded `fonts` section happens in `app::run` (see `keymap::install_shortcuts`).

use std::path::{Path, PathBuf};

use gpui::Global;
use serde::{Deserialize, Serialize};

use crate::file_browser::sort_settings_store::write_ui_settings_merged;

/// The `fonts` object — R23's font persistence. Every field is optional so a
/// missing key / section reads as "use the default" (fail-soft), and an absent
/// family stays the shipped default chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FontsSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_font_size: Option<f32>,
}

/// The `advanced` object — the persisted-inert smooth-scroll toggle (D2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AdvancedSection {
    #[serde(default)]
    smooth_scroll: bool,
}

/// The on-disk document, for DECODING R23's own keys. Every other top-level key is
/// ignored on read and preserved on write by `write_ui_settings_merged`.
#[derive(Debug, Default, Deserialize)]
struct UiSettingsExtrasDoc {
    #[serde(default)]
    fonts: Option<FontsSection>,
    #[serde(default)]
    advanced: Option<AdvancedSection>,
}

/// The process-wide settings-prefs store: the current `fonts` + `advanced` values
/// and the injected file path. Co-writers' sections in the shared file ride along
/// untouched (the read-merge-write writer).
pub struct SettingsPrefsStore {
    path: PathBuf,
    fonts: FontsSection,
    advanced: AdvancedSection,
}

impl Global for SettingsPrefsStore {}

impl SettingsPrefsStore {
    /// Load from `path`. A missing or malformed file yields defaults (never an
    /// error — fail-soft, Swift parity).
    pub fn load(path: PathBuf) -> Self {
        let (fonts, advanced) = match std::fs::read(&path) {
            Ok(bytes) => Self::decode(&bytes),
            Err(_) => (FontsSection::default(), AdvancedSection::default()),
        };
        Self {
            path,
            fonts,
            advanced,
        }
    }

    /// Construct a store with explicit defaults at `path`, WITHOUT touching disk —
    /// the `run_selftest` seam (the `with_defaults` precedent; no launch-time
    /// read / default-path resolution, per hermeticity).
    pub fn with_defaults(path: PathBuf) -> Self {
        Self {
            path,
            fonts: FontsSection::default(),
            advanced: AdvancedSection::default(),
        }
    }

    /// The persisted terminal font size (`None` ⇒ the default 13pt).
    pub fn terminal_font_px(&self) -> Option<f32> {
        self.fonts.terminal_font_size
    }

    /// The persisted terminal font family override (`None` ⇒ the default chain).
    pub fn terminal_font_family(&self) -> Option<String> {
        self.fonts.terminal_font_family.clone()
    }

    /// The persisted sidebar font size (`None` ⇒ the default 12pt).
    pub fn sidebar_font_px(&self) -> Option<f32> {
        self.fonts.sidebar_font_size
    }

    /// The persisted smooth-scroll toggle (default OFF).
    pub fn smooth_scroll(&self) -> bool {
        self.advanced.smooth_scroll
    }

    /// The injected file path (test hook).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist a new terminal font size, write-through only-if-changed.
    pub fn set_terminal_font_px(&mut self, px: f32) -> std::io::Result<bool> {
        if self.fonts.terminal_font_size == Some(px) {
            return Ok(false);
        }
        self.fonts.terminal_font_size = Some(px);
        self.write()?;
        Ok(true)
    }

    /// Persist a new terminal font family (`None` ⇒ the default chain),
    /// write-through only-if-changed.
    pub fn set_terminal_font_family(&mut self, family: Option<String>) -> std::io::Result<bool> {
        if self.fonts.terminal_font_family == family {
            return Ok(false);
        }
        self.fonts.terminal_font_family = family;
        self.write()?;
        Ok(true)
    }

    /// Persist a new sidebar font size, write-through only-if-changed.
    pub fn set_sidebar_font_px(&mut self, px: f32) -> std::io::Result<bool> {
        if self.fonts.sidebar_font_size == Some(px) {
            return Ok(false);
        }
        self.fonts.sidebar_font_size = Some(px);
        self.write()?;
        Ok(true)
    }

    /// Persist the smooth-scroll toggle, write-through only-if-changed.
    pub fn set_smooth_scroll(&mut self, on: bool) -> std::io::Result<bool> {
        if self.advanced.smooth_scroll == on {
            return Ok(false);
        }
        self.advanced.smooth_scroll = on;
        self.write()?;
        Ok(true)
    }

    /// Write the `fonts` + `advanced` sections through the shared read-merge-write
    /// writer, preserving every other top-level key (`appearance`,
    /// `file_browser_sort`, …).
    fn write(&self) -> std::io::Result<()> {
        let fonts = serde_json::to_value(&self.fonts).expect("FontsSection serializes");
        let advanced = serde_json::to_value(&self.advanced).expect("AdvancedSection serializes");
        write_ui_settings_merged(&self.path, |map| {
            map.insert("fonts".to_string(), fonts);
            map.insert("advanced".to_string(), advanced);
        })
    }

    /// Decode bytes into the two sections, applying tolerant defaulting. Malformed
    /// JSON falls back to defaults.
    fn decode(bytes: &[u8]) -> (FontsSection, AdvancedSection) {
        match serde_json::from_slice::<UiSettingsExtrasDoc>(bytes) {
            Ok(doc) => (
                doc.fonts.unwrap_or_default(),
                doc.advanced.unwrap_or_default(),
            ),
            Err(_) => (FontsSection::default(), AdvancedSection::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-settings-prefs-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// A missing file loads defaults (no family, default sizes, smooth-scroll OFF).
    #[test]
    fn missing_file_loads_defaults() {
        let path = temp_path("missing");
        assert!(!path.exists());
        let store = SettingsPrefsStore::load(path);
        assert_eq!(store.terminal_font_px(), None);
        assert_eq!(store.terminal_font_family(), None);
        assert_eq!(store.sidebar_font_px(), None);
        assert!(!store.smooth_scroll());
    }

    /// Round-trip: fonts + advanced persist and reload identically.
    #[test]
    fn fonts_and_advanced_round_trip() {
        let path = temp_path("roundtrip");
        let mut store = SettingsPrefsStore::load(path.clone());
        assert!(store.set_terminal_font_px(16.0).unwrap());
        assert!(store
            .set_terminal_font_family(Some("JetBrains Mono".to_string()))
            .unwrap());
        assert!(store.set_sidebar_font_px(14.0).unwrap());
        assert!(store.set_smooth_scroll(true).unwrap());

        let reloaded = SettingsPrefsStore::load(path);
        assert_eq!(reloaded.terminal_font_px(), Some(16.0));
        assert_eq!(
            reloaded.terminal_font_family(),
            Some("JetBrains Mono".to_string())
        );
        assert_eq!(reloaded.sidebar_font_px(), Some(14.0));
        assert!(reloaded.smooth_scroll());
    }

    /// only-if-changed: setting the same value twice performs no write.
    #[test]
    fn set_same_value_does_not_rewrite() {
        let path = temp_path("noop");
        let mut store = SettingsPrefsStore::load(path);
        assert!(store.set_terminal_font_px(16.0).unwrap(), "first set writes");
        assert!(
            !store.set_terminal_font_px(16.0).unwrap(),
            "re-setting the identical value must not rewrite"
        );
    }

    /// Read-merge-write PRESERVES a planted `appearance` (R21) and
    /// `file_browser_sort` (R19) key — the co-owner non-clobber discipline.
    #[test]
    fn co_owner_sections_survive_a_fonts_write() {
        let path = temp_path("cowriter");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{"scheme":"dark","accent":"ocean"},"file_browser_sort":{"criterion":"name","ascending":true}}"#,
        )
        .unwrap();

        let mut store = SettingsPrefsStore::load(path.clone());
        store.set_terminal_font_px(18.0).unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The fonts write landed.
        assert_eq!(raw["fonts"]["terminal_font_size"], 18.0);
        // The co-owners are untouched.
        assert_eq!(raw["appearance"]["scheme"], "dark");
        assert_eq!(raw["appearance"]["accent"], "ocean");
        assert_eq!(raw["file_browser_sort"]["criterion"], "name");
        assert_eq!(raw["version"], 1);
    }

    /// Absent section / field falls back to defaults (fail-soft).
    #[test]
    fn absent_section_and_field_default() {
        let path = temp_path("partial");
        // A `fonts` object with only a size — family + sidebar absent; no `advanced`.
        std::fs::write(
            &path,
            br#"{"version":1,"fonts":{"terminal_font_size":20}}"#,
        )
        .unwrap();
        let store = SettingsPrefsStore::load(path);
        assert_eq!(store.terminal_font_px(), Some(20.0));
        assert_eq!(store.terminal_font_family(), None);
        assert_eq!(store.sidebar_font_px(), None);
        assert!(!store.smooth_scroll(), "absent advanced ⇒ smooth-scroll OFF");
    }

    /// Malformed JSON is fail-soft: defaults, no crash.
    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = SettingsPrefsStore::load(path);
        assert_eq!(store.terminal_font_px(), None);
        assert!(!store.smooth_scroll());
    }
}
