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
    /// The terminal line-height multiplier (restyle 3/3). `None` ⇒ the shipped
    /// default (see `nice_term_view::DEFAULT_TERMINAL_LINE_HEIGHT`); the
    /// existing-user migration pins the legacy `1.0` here explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_line_height: Option<f32>,
}

/// The `advanced` object — the persisted-inert smooth-scroll toggle (D2) and
/// the shell-abstraction migration's `shell` key (design §4 step 2). `shell`
/// absent, or an empty string, means
/// [`crate::shell::resolve::ShellSetting::Automatic`]; a non-empty string is a
/// `Path` override. Written by the Settings ▸ Advanced ▸ Shell picker
/// ([`SettingsPrefsStore::set_shell`], migration step 6), and still tolerant of
/// a hand-edited `ui_settings.json`. tmux-port Phase 4 adds
/// `close_window_detaches` on the same terms.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AdvancedSection {
    #[serde(default)]
    smooth_scroll: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shell: Option<String>,
    /// tmux-port Phase 4 (D1): whether ⌘W / the red button DETACHES the
    /// window's running sessions into the app-global pool instead of killing
    /// them. `Option` on purpose — `None` is "never chosen", which reads as the
    /// shipped default **ON** ([`SettingsPrefsStore::close_window_detaches`]) —
    /// so the default can move later without mistaking an explicit OFF for an
    /// absent key. The checkbox always writes `Some(..)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    close_window_detaches: Option<bool>,
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

    /// The persisted terminal line-height multiplier (`None` ⇒ the shipped
    /// default). Fans out via `FontSettings` (not the theme fanout).
    pub fn terminal_line_height(&self) -> Option<f32> {
        self.fonts.terminal_line_height
    }

    /// The persisted smooth-scroll toggle (default OFF).
    pub fn smooth_scroll(&self) -> bool {
        self.advanced.smooth_scroll
    }

    /// Whether closing a window detaches its running sessions into the
    /// app-global pool (tmux-port Phase 4, D1). **Default ON** — an absent key
    /// (and an absent store) reads as detach-on-close; turning it OFF restores
    /// the pre-Phase-4 confirm-then-kill flow.
    pub fn close_window_detaches(&self) -> bool {
        self.advanced.close_window_detaches.unwrap_or(true)
    }

    /// The persisted `advanced.shell` choice, as a
    /// [`crate::shell::resolve::ShellSetting`]. Absent, or an empty string
    /// (defensive — an empty value should never win a resolution hop), reads
    /// as `Automatic`. This is what the shell bootstrap consults.
    pub fn shell_setting(&self) -> crate::shell::resolve::ShellSetting {
        match &self.advanced.shell {
            Some(s) if !s.is_empty() => crate::shell::resolve::ShellSetting::Path(s.clone()),
            _ => crate::shell::resolve::ShellSetting::Automatic,
        }
    }

    /// The persisted `advanced.shell` value as a RAW path (`None` ⇒ Automatic),
    /// applying [`Self::shell_setting`]'s empty-string rule. The picker needs
    /// the raw string rather than the `ShellSetting`: a path Nice cannot offer
    /// as a row is shown verbatim as its own trailing choice, never silently
    /// dropped.
    pub fn shell(&self) -> Option<String> {
        self.advanced
            .shell
            .clone()
            .filter(|s: &String| !s.is_empty())
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

    /// Persist a new terminal line-height multiplier, write-through
    /// only-if-changed.
    pub fn set_terminal_line_height(&mut self, mult: f32) -> std::io::Result<bool> {
        if self.fonts.terminal_line_height == Some(mult) {
            return Ok(false);
        }
        self.fonts.terminal_line_height = Some(mult);
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

    /// Persist the detach-on-close toggle, write-through only-if-changed.
    /// Always writes an EXPLICIT `Some(on)`, so re-picking the default value
    /// still records the user's choice rather than leaving the key absent.
    pub fn set_close_window_detaches(&mut self, on: bool) -> std::io::Result<bool> {
        if self.advanced.close_window_detaches == Some(on) {
            return Ok(false);
        }
        self.advanced.close_window_detaches = Some(on);
        self.write()?;
        Ok(true)
    }

    /// Persist the `advanced.shell` choice, write-through only-if-changed.
    /// `None` ⇒ Automatic, and the key is then omitted from the JSON entirely
    /// rather than written as `null` (the section's `skip_serializing_if`), so a
    /// user who goes back to Automatic leaves no residue behind.
    pub fn set_shell(&mut self, path: Option<String>) -> std::io::Result<bool> {
        if self.advanced.shell == path {
            return Ok(false);
        }
        self.advanced.shell = path;
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
        assert_eq!(store.terminal_line_height(), None);
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
        assert!(store.set_terminal_line_height(1.3).unwrap());
        assert!(store.set_smooth_scroll(true).unwrap());

        let reloaded = SettingsPrefsStore::load(path);
        assert_eq!(reloaded.terminal_font_px(), Some(16.0));
        assert_eq!(
            reloaded.terminal_font_family(),
            Some("JetBrains Mono".to_string())
        );
        assert_eq!(reloaded.sidebar_font_px(), Some(14.0));
        assert_eq!(reloaded.terminal_line_height(), Some(1.3));
        assert!(reloaded.smooth_scroll());
    }

    /// only-if-changed for line-height: setting the same value twice performs no
    /// second write.
    #[test]
    fn set_same_line_height_does_not_rewrite() {
        let path = temp_path("lh-noop");
        let mut store = SettingsPrefsStore::load(path);
        assert!(store.set_terminal_line_height(1.3).unwrap(), "first set writes");
        assert!(
            !store.set_terminal_line_height(1.3).unwrap(),
            "re-setting the identical line-height must not rewrite"
        );
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
        assert_eq!(store.terminal_line_height(), None, "absent line-height ⇒ None");
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

    /// Absent `advanced.shell` ⇒ `Automatic` — the default before any UI ever
    /// writes the key.
    #[test]
    fn shell_setting_absent_is_automatic() {
        let path = temp_path("shell-absent");
        let store = SettingsPrefsStore::load(path);
        assert_eq!(
            store.shell_setting(),
            crate::shell::resolve::ShellSetting::Automatic
        );
    }

    /// A non-empty `advanced.shell` string ⇒ `Path`.
    #[test]
    fn shell_setting_present_is_path() {
        let path = temp_path("shell-present");
        std::fs::write(
            &path,
            br#"{"version":1,"advanced":{"shell":"/opt/homebrew/bin/fish"}}"#,
        )
        .unwrap();
        let store = SettingsPrefsStore::load(path);
        assert_eq!(
            store.shell_setting(),
            crate::shell::resolve::ShellSetting::Path("/opt/homebrew/bin/fish".to_string())
        );
    }

    /// An empty `advanced.shell` string is defensively treated as absent.
    #[test]
    fn shell_setting_empty_string_is_automatic() {
        let path = temp_path("shell-empty");
        std::fs::write(&path, br#"{"version":1,"advanced":{"shell":""}}"#).unwrap();
        let store = SettingsPrefsStore::load(path);
        assert_eq!(
            store.shell_setting(),
            crate::shell::resolve::ShellSetting::Automatic
        );
    }

    /// The picker's round-trip: Automatic → a path → Automatic, through the
    /// file each time, with `shell()` and `shell_setting()` agreeing.
    #[test]
    fn set_shell_round_trips_through_the_file() {
        let path = temp_path("shell-set");
        let mut store = SettingsPrefsStore::load(path.clone());
        assert_eq!(store.shell(), None, "absent key ⇒ Automatic");

        assert!(store.set_shell(Some("/bin/bash".to_string())).unwrap());
        let reloaded = SettingsPrefsStore::load(path.clone());
        assert_eq!(reloaded.shell(), Some("/bin/bash".to_string()));
        assert_eq!(
            reloaded.shell_setting(),
            crate::shell::resolve::ShellSetting::Path("/bin/bash".to_string())
        );

        let mut store = reloaded;
        assert!(store.set_shell(None).unwrap(), "back to Automatic writes");
        let reloaded = SettingsPrefsStore::load(path);
        assert_eq!(reloaded.shell(), None);
        assert_eq!(
            reloaded.shell_setting(),
            crate::shell::resolve::ShellSetting::Automatic
        );
    }

    /// Automatic omits the key entirely — never `"shell": null`, which a
    /// hand-editing user would read as a broken setting.
    #[test]
    fn set_shell_none_omits_the_key() {
        let path = temp_path("shell-omit");
        let mut store = SettingsPrefsStore::load(path.clone());
        store.set_shell(Some("/bin/bash".to_string())).unwrap();
        store.set_shell(None).unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            raw["advanced"].get("shell").is_none(),
            "Automatic must leave no `shell` key: {raw}"
        );
    }

    /// only-if-changed: re-picking the same shell performs no second write.
    #[test]
    fn set_same_shell_does_not_rewrite() {
        let path = temp_path("shell-noop");
        let mut store = SettingsPrefsStore::load(path);
        assert!(
            store.set_shell(Some("/bin/bash".to_string())).unwrap(),
            "first pick writes"
        );
        assert!(
            !store.set_shell(Some("/bin/bash".to_string())).unwrap(),
            "re-picking the identical shell must not rewrite"
        );
        assert!(
            store.set_shell(None).unwrap(),
            "Automatic is a different value, so it does write"
        );
    }

    /// A shell write goes through the shared read-merge-write writer, so R21's
    /// `appearance` and R19's `file_browser_sort` survive it.
    #[test]
    fn co_owner_sections_survive_a_shell_write() {
        let path = temp_path("shell-cowriter");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{"scheme":"dark","accent":"ocean"},"file_browser_sort":{"criterion":"name","ascending":true}}"#,
        )
        .unwrap();

        let mut store = SettingsPrefsStore::load(path.clone());
        store.set_shell(Some("/bin/bash".to_string())).unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["advanced"]["shell"], "/bin/bash");
        assert_eq!(raw["appearance"]["scheme"], "dark");
        assert_eq!(raw["appearance"]["accent"], "ocean");
        assert_eq!(raw["file_browser_sort"]["criterion"], "name");
        assert_eq!(raw["version"], 1);
    }

    /// A hand-edited empty string reads as Automatic through the raw accessor
    /// too, so the picker never renders an empty passthrough row.
    #[test]
    fn shell_empty_string_reads_as_absent() {
        let path = temp_path("shell-raw-empty");
        std::fs::write(&path, br#"{"version":1,"advanced":{"shell":""}}"#).unwrap();
        let store = SettingsPrefsStore::load(path);
        assert_eq!(store.shell(), None);
    }

    /// tmux-port Phase 4 (D1): the detach-on-close toggle defaults ON when the
    /// key is absent, round-trips both values through the file, writes an
    /// EXPLICIT `Some(..)` (so a user who ticks the default back on still
    /// records it), and only reports real changes.
    #[test]
    fn close_window_detaches_defaults_on_and_round_trips() {
        let path = temp_path("detach-on-close");
        let mut store = SettingsPrefsStore::load(path.clone());
        assert!(
            store.close_window_detaches(),
            "absent key ⇒ the shipped default ON"
        );

        assert!(store.set_close_window_detaches(false).unwrap());
        assert!(
            !store.set_close_window_detaches(false).unwrap(),
            "re-setting the identical value must not rewrite"
        );
        let reloaded = SettingsPrefsStore::load(path.clone());
        assert!(!reloaded.close_window_detaches(), "OFF survived the file");

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["advanced"]["close_window_detaches"], false);

        let mut store = reloaded;
        assert!(store.set_close_window_detaches(true).unwrap());
        let reloaded = SettingsPrefsStore::load(path.clone());
        assert!(reloaded.close_window_detaches());
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            raw["advanced"]["close_window_detaches"], true,
            "ticking the default back on writes an explicit true, not an absent key"
        );
    }

    /// The new advanced key co-exists with `shell` — one section, two
    /// independent slots through the read-merge-write writer.
    #[test]
    fn close_window_detaches_and_shell_co_exist() {
        let path = temp_path("detach-plus-shell");
        let mut store = SettingsPrefsStore::load(path.clone());
        store.set_shell(Some("/bin/bash".to_string())).unwrap();
        store.set_close_window_detaches(false).unwrap();

        let reloaded = SettingsPrefsStore::load(path);
        assert_eq!(reloaded.shell(), Some("/bin/bash".to_string()));
        assert!(!reloaded.close_window_detaches());
    }

    /// `advanced.shell` survives a write of an unrelated advanced field
    /// (`set_smooth_scroll`) — the read-merge-write writer round-trips it even
    /// though this slice adds no setter for it.
    #[test]
    fn shell_setting_survives_a_smooth_scroll_write_round_trip() {
        let path = temp_path("shell-round-trip");
        std::fs::write(
            &path,
            br#"{"version":1,"advanced":{"shell":"/opt/homebrew/bin/fish"}}"#,
        )
        .unwrap();

        let mut store = SettingsPrefsStore::load(path.clone());
        assert_eq!(
            store.shell_setting(),
            crate::shell::resolve::ShellSetting::Path("/opt/homebrew/bin/fish".to_string())
        );
        store.set_smooth_scroll(true).unwrap();

        let reloaded = SettingsPrefsStore::load(path);
        assert!(reloaded.smooth_scroll());
        assert_eq!(
            reloaded.shell_setting(),
            crate::shell::resolve::ShellSetting::Path("/opt/homebrew/bin/fish".to_string())
        );
    }
}
