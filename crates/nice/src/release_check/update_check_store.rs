//! The R27 `update_check` section of `ui_settings.json` (What-to-build item 4,
//! Binding decision D3). A tiny process-wide store holding the last-seen GitHub
//! release tag, persisted through the SHARED
//! [`write_ui_settings_merged`](crate::file_browser::sort_settings_store::write_ui_settings_merged)
//! read-merge-write writer R21 extracted — so an `update_check` write never
//! clobbers R21's `appearance`, R19's `file_browser_sort`, R23's
//! `fonts`/`advanced`, or R24's `shortcuts`, and every co-owner's section
//! round-trips untouched.
//!
//! The cached tag is READ at [`ReleaseCheckerGlobal`](super::ReleaseCheckerGlobal)
//! construction — BEFORE any fetch fires — so the "Update available" pill can
//! render on the very first frame after relaunch when we already knew about an
//! update, instead of flashing in 3 s after launch (the Swift first-frame latency
//! fix, `ReleaseChecker.swift:10-12,85-93`). It is NOT offline support: it is a
//! render-latency cache. On EVERY successful fetch the returned tag is written
//! here **unconditionally, even if it parses to garbage**, so the same bad
//! response can't re-nag next run (`ReleaseChecker.swift:139`).
//!
//! ## Hermeticity
//! The store path is **injected** (the `sort_settings_store` convention): only
//! `app::run` resolves the default location (`default_ui_settings_path`);
//! `run_selftest` installs a [`with_defaults`](UpdateCheckStore::with_defaults)
//! temp-path store and performs no launch-time read or write.
//!
//! ## Why a NEW section / store, not a field on a shared doc
//! A NEW top-level section is a NEW per-section store module (mirroring
//! `prefs_store.rs` / `shortcuts_store.rs`), never a field bolted onto a shared
//! doc — the §6.1 do-not-simplify seam. A separate JSON file would be a fourth
//! persistence mechanism for one cached string (YAGNI); the shared writer already
//! merges only its own key.

use std::path::PathBuf;

use gpui::Global;
use serde::{Deserialize, Serialize};

use crate::file_browser::sort_settings_store::write_ui_settings_merged;

/// The `update_check` object — R27's cached-tag persistence. The single field is
/// optional so a missing key / section reads as "no cached tag" (fail-soft).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UpdateCheckSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_known_latest: Option<String>,
}

/// The on-disk document, for DECODING R27's own key. Every other top-level key is
/// ignored on read (serde default) and preserved on write by
/// [`write_ui_settings_merged`]'s read-merge-write.
#[derive(Debug, Default, Deserialize)]
struct UiSettingsUpdateCheckDoc {
    #[serde(default)]
    update_check: Option<UpdateCheckSection>,
}

/// The process-wide update-check store: the current cached tag and the injected
/// file path. Co-writers' sections in the shared file ride along untouched (the
/// read-merge-write writer).
pub struct UpdateCheckStore {
    path: PathBuf,
    section: UpdateCheckSection,
}

impl Global for UpdateCheckStore {}

impl UpdateCheckStore {
    /// Load from `path`. A missing or malformed file yields defaults (never an
    /// error — fail-soft, Swift parity: a corrupt pref reads as "no cached tag").
    pub fn load(path: PathBuf) -> Self {
        let section = match std::fs::read(&path) {
            Ok(bytes) => Self::decode(&bytes),
            Err(_) => UpdateCheckSection::default(),
        };
        Self { path, section }
    }

    /// Construct a store with explicit defaults at `path`, WITHOUT touching disk —
    /// the `run_selftest` seam (the `with_defaults` precedent; no launch-time
    /// read / default-path resolution, per hermeticity).
    pub fn with_defaults(path: PathBuf) -> Self {
        Self {
            path,
            section: UpdateCheckSection::default(),
        }
    }

    /// The cached last-seen tag (`None` ⇒ never cached / fresh install). Read once
    /// at [`ReleaseCheckerGlobal`](super::ReleaseCheckerGlobal) construction to
    /// seed the frame-1 pill.
    pub fn last_known_latest(&self) -> Option<String> {
        self.section.last_known_latest.clone()
    }

    /// Persist a new last-seen tag, write-through only-if-changed. Called on EVERY
    /// successful fetch, UNCONDITIONALLY — even for a tag that parses to garbage
    /// (`ReleaseChecker.swift:139`) — so a bad response can't re-nag; the
    /// only-if-changed guard merely skips a redundant identical rewrite.
    pub fn set_last_known_latest(&mut self, tag: &str) -> std::io::Result<bool> {
        if self.section.last_known_latest.as_deref() == Some(tag) {
            return Ok(false);
        }
        self.section.last_known_latest = Some(tag.to_string());
        self.write()?;
        Ok(true)
    }

    /// Write the `update_check` section through the shared read-merge-write writer,
    /// preserving every other top-level key (`appearance`, `fonts`, `shortcuts`,
    /// `file_browser_sort`, …).
    fn write(&self) -> std::io::Result<()> {
        let section =
            serde_json::to_value(&self.section).expect("UpdateCheckSection serializes");
        write_ui_settings_merged(&self.path, |map| {
            map.insert("update_check".to_string(), section);
        })
    }

    /// Decode bytes into the section, applying tolerant defaulting. Malformed JSON
    /// falls back to defaults (fail-soft).
    fn decode(bytes: &[u8]) -> UpdateCheckSection {
        match serde_json::from_slice::<UiSettingsUpdateCheckDoc>(bytes) {
            Ok(doc) => doc.update_check.unwrap_or_default(),
            Err(_) => UpdateCheckSection::default(),
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
            "nice-update-check-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// A missing file loads defaults (no cached tag) — the fresh-install path.
    #[test]
    fn missing_file_loads_none() {
        let path = temp_path("missing");
        assert!(!path.exists());
        let store = UpdateCheckStore::load(path);
        assert_eq!(store.last_known_latest(), None);
    }

    /// Round-trip: a cached tag persists and reloads identically.
    #[test]
    fn tag_round_trips() {
        let path = temp_path("roundtrip");
        let mut store = UpdateCheckStore::load(path.clone());
        assert!(store.set_last_known_latest("v0.1.5").unwrap());

        let reloaded = UpdateCheckStore::load(path);
        assert_eq!(reloaded.last_known_latest(), Some("v0.1.5".to_string()));
    }

    /// only-if-changed: setting the same tag twice performs no second write.
    #[test]
    fn set_same_tag_does_not_rewrite() {
        let path = temp_path("noop");
        let mut store = UpdateCheckStore::load(path);
        assert!(store.set_last_known_latest("v0.1.5").unwrap(), "first set writes");
        assert!(
            !store.set_last_known_latest("v0.1.5").unwrap(),
            "re-setting the identical tag must not rewrite"
        );
    }

    /// § Cache store co-owner preservation — writing `update_check` through the
    /// shared writer PRESERVES a planted `appearance` (R21), `fonts` (R23), and
    /// `shortcuts` (R24) key (the shared-writer non-clobber contract; the
    /// `prefs_store.rs` `co_owner_sections_survive_a_fonts_write` precedent).
    #[test]
    fn co_owner_sections_survive_an_update_check_write() {
        let path = temp_path("cowriter");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{"scheme":"dark","accent":"ocean"},"fonts":{"terminal_font_size":18},"shortcuts":{"newTab":"cmd-t"}}"#,
        )
        .unwrap();

        let mut store = UpdateCheckStore::load(path.clone());
        store.set_last_known_latest("v9.9.9").unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The update_check write landed.
        assert_eq!(raw["update_check"]["last_known_latest"], "v9.9.9");
        // Every co-owner section is untouched.
        assert_eq!(raw["appearance"]["scheme"], "dark");
        assert_eq!(raw["appearance"]["accent"], "ocean");
        assert_eq!(raw["fonts"]["terminal_font_size"], 18);
        assert_eq!(raw["shortcuts"]["newTab"], "cmd-t");
        assert_eq!(raw["version"], 1);
    }

    /// An absent `update_check` section ⇒ `None` cached tag (fail-soft), while any
    /// co-owner keys present in the file are left for the writer to preserve.
    #[test]
    fn absent_section_reads_none() {
        let path = temp_path("absent");
        std::fs::write(&path, br#"{"version":1,"appearance":{"scheme":"light"}}"#).unwrap();
        let store = UpdateCheckStore::load(path);
        assert_eq!(store.last_known_latest(), None);
    }

    /// Malformed JSON is fail-soft: `None`, no crash.
    #[test]
    fn malformed_json_falls_back_to_none() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = UpdateCheckStore::load(path);
        assert_eq!(store.last_known_latest(), None);
    }
}
