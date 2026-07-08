//! The rebindable-shortcut binding store (R24, G6) — the mutable, persisted map
//! from every [`ShortcutAction`] to the user's chosen combo (or "unbound"),
//! sharing `ui_settings.json` with R19's sort store, R21's theme store, and R23's
//! font/advanced prefs.
//!
//! ## What lives here (R24 slice 1)
//!
//! * [`ShortcutBindings`] — the gpui `Global` wrapping a
//!   `HashMap<ShortcutAction, Option<OwnedCombo>>` (always all 13 keys present; a
//!   value of `None` means the action is explicitly unbound) plus the injected file
//!   path. `load(path)` is fail-soft to defaults; `with_defaults(path)` is the
//!   `run_selftest` seam (defaults + a temp path, no disk read). The read accessors
//!   [`binding`](ShortcutBindings::binding) / [`is_at_default`](ShortcutBindings::is_at_default)
//!   and the mutators [`set_binding`](ShortcutBindings::set_binding) /
//!   [`reset`](ShortcutBindings::reset) are the store API R24's recorder pane drives.
//! * The `shortcuts`-section decode/encode over the shared **read-merge-write**
//!   writer ([`write_ui_settings_merged`]) — each mutator persists the FULL 13-entry
//!   map (chord string or JSON `null`) then rebuilds the keymap, so a rebind survives
//!   relaunch and every co-writer's section (`appearance` / `fonts` / `file_browser_sort`
//!   / any unknown key) rides along untouched.
//!
//! ## The frozen load rules (Swift parity, `KeyboardShortcuts.swift:283-310`)
//!
//! 1. The `shortcuts` section absent entirely ⇒ all 13 defaults.
//! 2. Malformed JSON (whole file or a mistyped section) ⇒ defaults (fail-soft, log).
//! 3. An unknown action key ⇒ dropped silently ([`ShortcutAction::from_id`] rejects it).
//! 4. An action key present with `null` ⇒ that action is UNBOUND.
//! 5. An action key ABSENT from a PRESENT section ⇒ that action loads UNBOUND
//!    (preserves explicit clears across launches; ships a future new action unbound
//!    for upgraders).
//!
//! Write rule (a deliberate, load-equivalent divergence from Swift, which omits
//! unbound keys): Rust persists the FULL current map every time — each action a
//! chord string or an explicit JSON `null` — for a self-describing, diffable file.
//! Equivalent under load rule 5.
//!
//! ## What does NOT live here (later R24 slices)
//!
//! `keymap::rebuild_keymap` (slice 2 fills the body the mutators call), the recorder
//! field + Shortcuts pane (slice 3), and the close-out composition (slice 4). This
//! slice is the store + the persisted section + boot seeding only.

// Slice 2 (`rebuild_keymap` + conflict wiring) and slice 3 (the recorder pane)
// consume `set_binding` / `reset` / `bindings`; slice 1 installs + tests the store.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{App, Global};
use nice_model::shortcuts::{default_bindings, default_combo, OwnedCombo, ShortcutAction};
use serde::Deserialize;

use crate::file_browser::sort_settings_store::write_ui_settings_merged;

/// Just the `shortcuts` key of the shared `ui_settings.json` doc, for tolerant
/// decode. The section is a map of action-id → optional chord token (`null` = the
/// action is explicitly unbound). Other top-level keys are ignored on read (serde
/// default) and preserved on write (read-merge-write), so no flatten catch-all is
/// needed. A missing section decodes to `None` (load rule 1); a mistyped section
/// makes the whole `from_slice` fail (load rule 2).
#[derive(Debug, Default, Deserialize)]
struct DocForShortcuts {
    #[serde(default)]
    shortcuts: Option<HashMap<String, Option<String>>>,
}

/// The map every action starts at (all 13 present) with its default combo owned.
fn default_map() -> HashMap<ShortcutAction, Option<OwnedCombo>> {
    default_bindings()
        .into_iter()
        .map(|(action, combo)| (action, Some(OwnedCombo::from(combo))))
        .collect()
}

/// The map with every action present but UNBOUND (`None`) — the base a PRESENT
/// `shortcuts` section fills in (load rule 5: an absent key stays unbound).
fn all_unbound_map() -> HashMap<ShortcutAction, Option<OwnedCombo>> {
    ShortcutAction::ALL
        .into_iter()
        .map(|action| (action, None))
        .collect()
}

/// Decode the live binding map from a raw `ui_settings.json` byte buffer, applying
/// the frozen load rules. Malformed JSON or an absent section ⇒ all defaults.
fn decode_bindings(bytes: &[u8]) -> HashMap<ShortcutAction, Option<OwnedCombo>> {
    match serde_json::from_slice::<DocForShortcuts>(bytes) {
        Ok(doc) => match doc.shortcuts {
            // Rule 1: section absent ⇒ all defaults.
            None => default_map(),
            // Rule 5: start all-unbound, then fill from the present keys.
            Some(section) => {
                let mut map = all_unbound_map();
                for (id, token) in section {
                    // Rule 3: an unknown action id is dropped silently.
                    if let Some(action) = ShortcutAction::from_id(&id) {
                        // Rule 4: a `null` token ⇒ unbound; a malformed token also
                        // fails soft to unbound. A valid token ⇒ that combo.
                        let combo = token.and_then(|t| OwnedCombo::from_token(&t));
                        map.insert(action, combo);
                    }
                }
                map
            }
        },
        // Rule 2: malformed JSON ⇒ defaults (fail-soft).
        Err(_) => default_map(),
    }
}

/// The process-wide rebindable-shortcut store: the current binding map + the
/// injected file path. A gpui `Global` (mirrors [`crate::theme_settings::ThemeSettingsStore`]
/// and [`crate::file_browser::sort_settings_store::SortSettingsStore`]). Absent
/// Global ⇒ callers fall back to the defaults, exactly like every other store.
pub struct ShortcutBindings {
    path: PathBuf,
    map: HashMap<ShortcutAction, Option<OwnedCombo>>,
}

impl Global for ShortcutBindings {}

impl ShortcutBindings {
    /// Load from `path`, applying the frozen load rules. A missing or malformed
    /// file ⇒ all-defaults, never an error (fail-soft, Swift parity). `app::run`
    /// ONLY resolves the real path (hermeticity).
    pub fn load(path: PathBuf) -> Self {
        let map = match std::fs::read(&path) {
            Ok(bytes) => decode_bindings(&bytes),
            Err(_) => default_map(),
        };
        Self { path, map }
    }

    /// Construct with all-defaults at `path` WITHOUT touching disk — the
    /// `run_selftest` seam (defaults + a temp path; the launch-time read /
    /// default-path resolution stays in `app::run`, per hermeticity).
    pub fn with_defaults(path: PathBuf) -> Self {
        Self {
            path,
            map: default_map(),
        }
    }

    /// The injected file path (test hook).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current combo bound to `action`, or `None` when the action is unbound.
    pub fn binding(&self, action: ShortcutAction) -> Option<OwnedCombo> {
        self.map.get(&action).cloned().flatten()
    }

    /// Whether `action` is currently at its default binding. True iff the current
    /// combo equals the default (every action has a default, so an unbound action
    /// is never at default). Drives the recorder's per-action "Reset" visibility
    /// (`KeyboardShortcuts.swift:277-279`).
    pub fn is_at_default(&self, action: ShortcutAction) -> bool {
        let default = default_combo(action).map(OwnedCombo::from);
        self.binding(action) == default
    }

    /// A snapshot of the live map (read hook — the conflict check and the pane
    /// iterate it).
    pub fn bindings(&self) -> &HashMap<ShortcutAction, Option<OwnedCombo>> {
        &self.map
    }

    /// Set (or clear, with `None`) `action`'s combo: mutate the live [`Global`],
    /// persist the full `shortcuts` section (only when the value actually changed),
    /// then rebuild the keymap so the rebind is live. A no-op when the store Global
    /// is absent (a scenario/test that did not install it). A free-standing
    /// associated fn (not `&mut self`) because the store lives inside `App` and the
    /// rebuild needs `&mut App` after the mutation.
    pub fn set_binding(cx: &mut App, action: ShortcutAction, combo: Option<OwnedCombo>) {
        if cx.try_global::<ShortcutBindings>().is_none() {
            return;
        }
        // `global_mut` borrows the store mutably; the borrow ends before the
        // `rebuild_keymap(cx)` call (the `crate::theme_settings` mutator pattern).
        let changed = cx
            .global_mut::<ShortcutBindings>()
            .set_in_memory(action, combo);
        if changed {
            crate::keymap::rebuild_keymap(cx);
        }
    }

    /// Restore `action` to its default combo (persist + rebuild). Swift's per-action
    /// Reset (`isAtDefault` drives the button; there is no global "reset all").
    pub fn reset(cx: &mut App, action: ShortcutAction) {
        let default = default_combo(action).map(OwnedCombo::from);
        Self::set_binding(cx, action, default);
    }

    /// Mutate the in-memory map and persist the full section only if the value
    /// changed. Returns whether it changed (so the caller skips a redundant keymap
    /// rebuild). A persist error is logged and swallowed (fail-soft store
    /// discipline): the in-memory change still stands and still rebuilds.
    fn set_in_memory(&mut self, action: ShortcutAction, combo: Option<OwnedCombo>) -> bool {
        if self.binding(action) == combo {
            return false;
        }
        self.map.insert(action, combo);
        if let Err(e) = self.persist() {
            eprintln!("nice-rs: shortcut binding persist failed: {e}");
        }
        true
    }

    /// Write the FULL current map as the `shortcuts` section through the shared
    /// read-merge-write writer, preserving every other top-level key. Each action
    /// is a chord token string or an explicit JSON `null` (the write rule).
    fn persist(&self) -> std::io::Result<()> {
        let mut section = serde_json::Map::new();
        for action in ShortcutAction::ALL {
            let value = match self.binding(action) {
                Some(combo) => serde_json::Value::String(combo.to_token()),
                None => serde_json::Value::Null,
            };
            section.insert(action.id().to_string(), value);
        }
        write_ui_settings_merged(&self.path, |map| {
            map.insert(
                "shortcuts".to_string(),
                serde_json::Value::Object(section),
            );
        })
    }
}

/// Resolve the shortcut store's `ui_settings.json` path — the **same** shared file
/// as R19's sort store / R21's theme store, so `<support-root>/Nice RS Dev/ui_settings.json`
/// with `<support-root>` from `NICE_APPLICATION_SUPPORT_ROOT` when set else
/// `~/Library/Application Support`. Called from `app::run` ONLY — never a test or
/// `run_selftest` (hermeticity).
pub fn default_shortcut_bindings_path() -> PathBuf {
    crate::file_browser::sort_settings_store::default_ui_settings_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::shortcuts::Modifiers;

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-shortcuts-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    fn combo(token: &str) -> OwnedCombo {
        OwnedCombo::from_token(token).unwrap()
    }

    /// A missing file loads all 13 defaults (fresh-install path, load rule 1).
    #[test]
    fn missing_file_loads_defaults() {
        let path = temp_path("missing");
        assert!(!path.exists());
        let store = ShortcutBindings::load(path);
        for (action, def) in default_bindings() {
            assert_eq!(store.binding(action), Some(OwnedCombo::from(def)));
            assert!(store.is_at_default(action));
        }
    }

    /// Full-map round-trip: persist a mutated map (a rebind + an explicit unbind)
    /// and reload it identically. `persist` runs directly (no gpui `App`).
    #[test]
    fn full_map_round_trip() {
        let path = temp_path("roundtrip");
        let mut store = ShortcutBindings::load(path.clone());
        assert!(store.set_in_memory(ShortcutAction::NewTerminalPane, Some(combo("cmd-y"))));
        assert!(store.set_in_memory(ShortcutAction::ToggleSidebar, None)); // explicit unbind

        let reloaded = ShortcutBindings::load(path);
        assert_eq!(
            reloaded.binding(ShortcutAction::NewTerminalPane),
            Some(combo("cmd-y"))
        );
        assert_eq!(reloaded.binding(ShortcutAction::ToggleSidebar), None);
        // An untouched action keeps its default across the round-trip.
        assert_eq!(
            reloaded.binding(ShortcutAction::UndoFileOperation),
            Some(combo("cmd-z"))
        );
    }

    /// only-if-changed: re-setting the identical binding reports no change.
    #[test]
    fn set_same_value_reports_unchanged() {
        let path = temp_path("noop");
        let mut store = ShortcutBindings::load(path);
        assert!(
            store.set_in_memory(ShortcutAction::NewTerminalPane, Some(combo("cmd-y"))),
            "first set changes"
        );
        assert!(
            !store.set_in_memory(ShortcutAction::NewTerminalPane, Some(combo("cmd-y"))),
            "re-setting the identical combo reports no change"
        );
        // Re-setting an action to its existing default is likewise a no-op.
        assert!(!store.set_in_memory(ShortcutAction::UndoFileOperation, Some(combo("cmd-z"))));
    }

    /// Read-merge-write preserves a planted `appearance` / `fonts` /
    /// `file_browser_sort` section when the shortcut store writes its section
    /// (co-owner non-clobber, D5).
    #[test]
    fn write_preserves_co_owner_sections() {
        let path = temp_path("cowriter");
        std::fs::write(
            &path,
            br#"{"version":1,"appearance":{"scheme":"dark","accent":"ocean"},"fonts":{"size":14},"file_browser_sort":{"criterion":"name","ascending":true},"future_section":{"hello":42}}"#,
        )
        .unwrap();

        let mut store = ShortcutBindings::load(path.clone());
        assert!(store.set_in_memory(ShortcutAction::NewTerminalPane, Some(combo("cmd-y"))));

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The shortcuts section landed (full map, one rebound entry).
        assert_eq!(raw["shortcuts"]["newTerminalPane"], "cmd-y");
        // Every co-owner's section (and an unknown key) survives untouched.
        assert_eq!(raw["appearance"]["scheme"], "dark");
        assert_eq!(raw["appearance"]["accent"], "ocean");
        assert_eq!(raw["fonts"]["size"], 14);
        assert_eq!(raw["file_browser_sort"]["criterion"], "name");
        assert_eq!(raw["future_section"]["hello"], 42);
        assert_eq!(raw["version"], 1);
    }

    /// The write rule: the persisted section carries ALL 13 keys, each a chord
    /// string or explicit `null` (a self-describing, diffable file).
    #[test]
    fn write_persists_full_map_with_explicit_null() {
        let path = temp_path("fullmap");
        let mut store = ShortcutBindings::load(path.clone());
        store.set_in_memory(ShortcutAction::ToggleSidebar, None); // explicit unbind

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let section = raw["shortcuts"].as_object().expect("shortcuts is an object");
        assert_eq!(section.len(), 13, "all 13 keys persisted");
        // The unbound action is an explicit JSON null.
        assert!(section["toggleSidebar"].is_null());
        // A bound action is its chord string.
        assert_eq!(section["undoFileOperation"], "cmd-z");
    }

    /// Load rule 1: an absent `shortcuts` section (even with a sibling section
    /// present) loads all 13 defaults.
    #[test]
    fn absent_section_loads_all_defaults() {
        let path = temp_path("absent");
        std::fs::write(
            &path,
            br#"{"version":1,"file_browser_sort":{"criterion":"name","ascending":true}}"#,
        )
        .unwrap();
        let store = ShortcutBindings::load(path);
        for (action, def) in default_bindings() {
            assert_eq!(store.binding(action), Some(OwnedCombo::from(def)));
        }
    }

    /// Load rules 4 + 5: within a PRESENT section, a key with `null` ⇒ unbound; a
    /// key ABSENT ⇒ also unbound (NOT its default). A present bound key decodes.
    #[test]
    fn null_and_absent_keys_load_unbound() {
        let path = temp_path("null-vs-absent");
        // Section is present, with: newTerminalPane rebound, toggleSidebar null,
        // and every other key (e.g. undoFileOperation) simply absent.
        std::fs::write(
            &path,
            br#"{"version":1,"shortcuts":{"newTerminalPane":"cmd-y","toggleSidebar":null}}"#,
        )
        .unwrap();
        let store = ShortcutBindings::load(path);
        // Present + bound.
        assert_eq!(store.binding(ShortcutAction::NewTerminalPane), Some(combo("cmd-y")));
        // Present + null ⇒ unbound (rule 4).
        assert_eq!(store.binding(ShortcutAction::ToggleSidebar), None);
        // Absent from a present section ⇒ unbound, NOT the default (rule 5).
        assert_eq!(store.binding(ShortcutAction::UndoFileOperation), None);
        assert!(!store.is_at_default(ShortcutAction::UndoFileOperation));
    }

    /// Load rule 3: an unknown action key is dropped silently (no crash, no bogus
    /// entry); the known keys still decode.
    #[test]
    fn unknown_action_key_dropped() {
        let path = temp_path("unknown-key");
        std::fs::write(
            &path,
            br#"{"version":1,"shortcuts":{"newTerminalPane":"cmd-y","notARealAction":"cmd-j"}}"#,
        )
        .unwrap();
        let store = ShortcutBindings::load(path);
        assert_eq!(store.binding(ShortcutAction::NewTerminalPane), Some(combo("cmd-y")));
        // The bogus key produced no entry; every other action is unbound (rule 5).
        assert_eq!(store.binding(ShortcutAction::ToggleSidebar), None);
    }

    /// Load rule 2: malformed JSON is fail-soft ⇒ all defaults, no crash.
    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = ShortcutBindings::load(path);
        assert!(store.is_at_default(ShortcutAction::NewTerminalPane));

        // A mistyped section (not a map) also fails soft to defaults.
        let path2 = temp_path("mistyped");
        std::fs::write(&path2, br#"{"version":1,"shortcuts":"not-a-map"}"#).unwrap();
        let store2 = ShortcutBindings::load(path2);
        assert!(store2.is_at_default(ShortcutAction::NewTerminalPane));
    }

    /// `is_at_default` flips off after a rebind and back on after reset-to-default
    /// (exercised directly through `set_in_memory`, the engine `reset` uses).
    #[test]
    fn is_at_default_tracks_rebind_and_reset() {
        let path = temp_path("at-default");
        let mut store = ShortcutBindings::load(path);
        assert!(store.is_at_default(ShortcutAction::NewTerminalPane));

        store.set_in_memory(ShortcutAction::NewTerminalPane, Some(combo("cmd-y")));
        assert!(!store.is_at_default(ShortcutAction::NewTerminalPane));

        // Restore the default combo (what `reset` sets).
        let default = default_combo(ShortcutAction::NewTerminalPane)
            .map(OwnedCombo::from);
        store.set_in_memory(ShortcutAction::NewTerminalPane, default);
        assert!(store.is_at_default(ShortcutAction::NewTerminalPane));
    }

    /// An unbound action is never "at default" (every action HAS a default).
    #[test]
    fn unbound_is_not_at_default() {
        let path = temp_path("unbound");
        let mut store = ShortcutBindings::load(path);
        store.set_in_memory(ShortcutAction::NewTerminalPane, None);
        assert!(!store.is_at_default(ShortcutAction::NewTerminalPane));
        // A masked-modifier default sanity check: the default really is cmd-t.
        assert_eq!(
            default_combo(ShortcutAction::NewTerminalPane).map(OwnedCombo::from),
            Some(OwnedCombo {
                modifiers: Modifiers::COMMAND,
                key: "t".to_string()
            })
        );
    }
}
