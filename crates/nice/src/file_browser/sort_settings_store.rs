//! The F2 sort-preferences file store — `ui_settings.json` under
//! `~/Library/Application Support/Nice RS Dev/`.
//!
//! Swift persisted the two sort knobs (`fileBrowser.sort.criterion` /
//! `fileBrowser.sort.ascending`) in `UserDefaults`. The rewrite deliberately does
//! NOT reuse `UserDefaults`/`CFPreferences`, and does NOT fold them into R18's
//! sessions store — they're a small process-wide "view preference" (Finder-like),
//! so they get their own tiny JSON file that R21/R23 will later share.
//!
//! ## Schema
//!
//! ```json
//! {"version":1,"file_browser_sort":{"criterion":"name","ascending":true}}
//! ```
//!
//! The `file_browser_sort` object serializes the pure
//! [`FileBrowserSortSettings`] value type (its own snake_case serde is the
//! schema surface). **Unknown top-level keys are preserved on rewrite** (a
//! `#[serde(flatten)]` catch-all): when R21/R23 add their own top-level sections,
//! an R19 write must not clobber them.
//!
//! ## Write discipline
//!
//! Writes go through [`crate::atomic_file::write_atomic`] BY NAME (the R18-hoisted
//! shared helper) — a temp sibling + rename, so a concurrent reader never sees a
//! half-written file — and are **only-if-changed**: setting the same value twice
//! touches no disk. The store path is **injected** (the R16 convention); only
//! `app::run`'s bootstrap resolves the default location.
//!
//! ## Process Global
//!
//! [`app::run`] loads the file once into a [`SortSettingsStore`] gpui `Global`
//! (write-through on change); `run_selftest` installs one with defaults + a temp
//! path (never resolving or writing the real user file — hermeticity). Absent
//! Global ⇒ the file-browser view falls back to in-memory defaults, exactly like
//! every other R18/R19 store.

use std::path::{Path, PathBuf};

use gpui::Global;
use nice_model::file_browser::{FileBrowserSortCriterion, FileBrowserSortSettings};
use serde::{Deserialize, Serialize};

/// The on-disk `ui_settings.json` document. `version` + `file_browser_sort` are
/// R19's own keys; `extra` captures every OTHER top-level key so a rewrite
/// preserves sections future stages (R21/R23) add.
#[derive(Debug, Serialize, Deserialize)]
struct UiSettingsDoc {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_browser_sort: Option<SortSection>,
    /// Unknown top-level keys, preserved verbatim across rewrites.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// The `file_browser_sort` object, decoded tolerantly: a missing / unknown
/// `criterion` and a missing `ascending` both fall back through
/// [`FileBrowserSortSettings::from_stored`] (name / ascending), never crashing or
/// silently flipping a fresh install to descending.
#[derive(Debug, Serialize, Deserialize)]
struct SortSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    criterion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ascending: Option<bool>,
}

const SCHEMA_VERSION: u32 = 1;

/// The process-wide file-browser sort store: the current settings, the injected
/// file path, and any unknown top-level keys held for round-tripping.
pub struct SortSettingsStore {
    path: PathBuf,
    settings: FileBrowserSortSettings,
    extra: serde_json::Map<String, serde_json::Value>,
}

impl Global for SortSettingsStore {}

impl SortSettingsStore {
    /// Load the store from `path`. A missing or malformed file yields defaults
    /// (name / ascending) with no extra keys — never an error (fail-soft, Swift
    /// parity: a corrupt pref reads as the default, it doesn't crash the app).
    pub fn load(path: PathBuf) -> Self {
        let (settings, extra) = match std::fs::read(&path) {
            Ok(bytes) => Self::decode(&bytes),
            Err(_) => (FileBrowserSortSettings::default(), Default::default()),
        };
        Self {
            path,
            settings,
            extra,
        }
    }

    /// Construct a store with explicit defaults at `path`, WITHOUT touching disk
    /// — the `run_selftest` seam (defaults + a temp path; the launch-time read /
    /// default-path resolution stays in `app::run` only, per hermeticity).
    pub fn with_defaults(path: PathBuf) -> Self {
        Self {
            path,
            settings: FileBrowserSortSettings::default(),
            extra: Default::default(),
        }
    }

    /// The current settings.
    pub fn settings(&self) -> FileBrowserSortSettings {
        self.settings
    }

    /// The injected file path (test hook).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply `new` and write through to disk **only if it changed**. Returns
    /// `Ok(true)` when a disk write happened, `Ok(false)` when the value was
    /// unchanged (no write), or an I/O error from the atomic write. Preserved
    /// unknown top-level keys ride along on every write.
    pub fn set(&mut self, new: FileBrowserSortSettings) -> std::io::Result<bool> {
        if new == self.settings {
            return Ok(false);
        }
        self.settings = new;
        self.write()?;
        Ok(true)
    }

    /// Serialize the current state and atomically replace the file.
    fn write(&self) -> std::io::Result<()> {
        let doc = UiSettingsDoc {
            version: SCHEMA_VERSION,
            file_browser_sort: Some(SortSection {
                criterion: Some(self.settings.criterion.as_raw().to_string()),
                ascending: Some(self.settings.ascending),
            }),
            extra: self.extra.clone(),
        };
        // `to_vec_pretty` for a human-diffable small config file.
        let bytes = serde_json::to_vec_pretty(&doc)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::atomic_file::write_atomic(&self.path, &bytes, None)
    }

    /// Decode bytes into `(settings, extra)`, applying the tolerant defaulting.
    /// Malformed JSON falls back to defaults + no extra.
    fn decode(
        bytes: &[u8],
    ) -> (
        FileBrowserSortSettings,
        serde_json::Map<String, serde_json::Value>,
    ) {
        match serde_json::from_slice::<UiSettingsDoc>(bytes) {
            Ok(doc) => {
                let section = doc.file_browser_sort.unwrap_or(SortSection {
                    criterion: None,
                    ascending: None,
                });
                let settings = FileBrowserSortSettings::from_stored(
                    section.criterion.as_deref(),
                    section.ascending,
                );
                (settings, doc.extra)
            }
            Err(_) => (FileBrowserSortSettings::default(), Default::default()),
        }
    }
}

/// Resolve the default `ui_settings.json` path:
/// `<support-root>/Nice RS Dev/ui_settings.json`, where `<support-root>` is
/// `NICE_APPLICATION_SUPPORT_ROOT` when set (tests / scenarios redirect state
/// into a sandbox) else `~/Library/Application Support`. Called from `app::run`
/// ONLY — never a test or `run_selftest` (the `session_store` convention).
pub fn default_ui_settings_path() -> PathBuf {
    let root = match std::env::var("NICE_APPLICATION_SUPPORT_ROOT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            PathBuf::from(home).join("Library/Application Support")
        }
    };
    // Same folder convention as the session store (`Nice RS Dev`).
    root.join(crate::session_store::STORE_FOLDER)
        .join("ui_settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-uisettings-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// A missing file loads defaults (name / ascending) — the fresh-install path.
    #[test]
    fn missing_file_loads_defaults() {
        let path = temp_path("missing");
        assert!(!path.exists());
        let store = SortSettingsStore::load(path);
        assert_eq!(store.settings(), FileBrowserSortSettings::default());
    }

    /// Round-trip: a set persists and reloads identically.
    #[test]
    fn set_persists_and_reloads() {
        let path = temp_path("roundtrip");
        let mut store = SortSettingsStore::load(path.clone());
        let wrote = store
            .set(FileBrowserSortSettings {
                criterion: FileBrowserSortCriterion::DateModified,
                ascending: false,
            })
            .unwrap();
        assert!(wrote, "changing the value writes");

        let reloaded = SortSettingsStore::load(path);
        assert_eq!(reloaded.settings().criterion, FileBrowserSortCriterion::DateModified);
        assert!(!reloaded.settings().ascending);
    }

    /// only-if-changed: setting the same value a second time performs no write.
    #[test]
    fn set_same_value_does_not_rewrite() {
        let path = temp_path("noop");
        let mut store = SortSettingsStore::load(path.clone());
        let target = FileBrowserSortSettings {
            criterion: FileBrowserSortCriterion::DateModified,
            ascending: true,
        };
        assert!(store.set(target).unwrap(), "first set writes");
        assert!(
            !store.set(target).unwrap(),
            "re-setting the identical value must not rewrite the file"
        );
    }

    /// Unknown top-level keys survive a rewrite (R21/R23 share the file).
    #[test]
    fn unknown_top_level_keys_preserved() {
        let path = temp_path("extra");
        std::fs::write(
            &path,
            br#"{"version":1,"file_browser_sort":{"criterion":"name","ascending":true},"future_section":{"hello":42}}"#,
        )
        .unwrap();

        let mut store = SortSettingsStore::load(path.clone());
        store
            .set(FileBrowserSortSettings {
                criterion: FileBrowserSortCriterion::DateModified,
                ascending: false,
            })
            .unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["future_section"]["hello"], 42, "unknown key must survive");
        assert_eq!(raw["file_browser_sort"]["criterion"], "date_modified");
        assert_eq!(raw["file_browser_sort"]["ascending"], false);
        assert_eq!(raw["version"], 1);
    }

    /// Unset fields default: a file with only a criterion still reads ascending;
    /// an unknown criterion string falls back to name.
    #[test]
    fn unset_fields_and_unknown_criterion_default() {
        let path = temp_path("defaults");
        std::fs::write(
            &path,
            br#"{"version":1,"file_browser_sort":{"criterion":"size"}}"#,
        )
        .unwrap();
        let store = SortSettingsStore::load(path);
        // "size" is unknown → name; missing ascending → true.
        assert_eq!(store.settings().criterion, FileBrowserSortCriterion::Name);
        assert!(store.settings().ascending);
    }

    /// Malformed JSON is fail-soft: defaults, no crash.
    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = SortSettingsStore::load(path);
        assert_eq!(store.settings(), FileBrowserSortSettings::default());
    }
}
