//! The rebindable-shortcut binding store (R24, G6) — the mutable, persisted map
//! from every [`ShortcutAction`] to the user's chosen combo (or "unbound"),
//! sharing `ui_settings.json` with R19's sort store, R21's theme store, and R23's
//! font/advanced prefs.
//!
//! ## What lives here (R24 slice 1)
//!
//! * [`ShortcutBindings`] — the gpui `Global` wrapping a
//!   `HashMap<ShortcutAction, Option<OwnedCombo>>` (always every `ShortcutAction::ALL`
//!   key present; a
//!   value of `None` means the action is explicitly unbound) plus the injected file
//!   path. `load(path)` is fail-soft to defaults; `with_defaults(path)` is the
//!   `run_selftest` seam (defaults + a temp path, no disk read). The read accessors
//!   [`binding`](ShortcutBindings::binding) / [`is_at_default`](ShortcutBindings::is_at_default)
//!   and the mutators [`set_binding`](ShortcutBindings::set_binding) /
//!   [`reset`](ShortcutBindings::reset) are the store API R24's recorder pane drives.
//! * The `shortcuts`-section decode/encode over the shared **read-merge-write**
//!   writer ([`write_ui_settings_merged`]) — each mutator persists the FULL
//!   map (chord string or JSON `null`) then rebuilds the keymap, so a rebind survives
//!   relaunch and every co-writer's section (`appearance` / `fonts` / `file_browser_sort`
//!   / any unknown key) rides along untouched.
//!
//! ## The frozen load rules (Swift parity, `KeyboardShortcuts.swift:283-310`)
//!
//! 1. The `shortcuts` section absent entirely ⇒ all defaults.
//! 2. Malformed JSON (whole file or a mistyped section) ⇒ defaults (fail-soft, log).
//! 3. An unknown action key ⇒ dropped silently ([`ShortcutAction::from_id`] rejects it).
//! 4. An action key present with `null` ⇒ that action is UNBOUND.
//! 5. An action key ABSENT from a PRESENT section ⇒ that action loads UNBOUND
//!    (preserves explicit clears across launches; ships a future new action unbound
//!    for upgraders). **One deliberate exception**: `commandCompose` ABSENT from a
//!    present section seeds its DEFAULT (⌘↩) instead — the action shipped after
//!    users already had 13-key sections on disk, and Nick chose "customizers get
//!    the new default too" over rule 5's unbound outcome. An explicit
//!    `"commandCompose": null` still loads unbound (rule 4 wins over the seed).
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

/// The map every action starts at (every `ShortcutAction::ALL` key present) with
/// its default combo owned.
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
                // The rule-5 seeding exception (module docs): a section written
                // before `commandCompose` existed has no such key at all — seed
                // the default ⌘↩. A present key (chord or explicit `null`) is
                // decoded by the loop below and overwrites this seed.
                if !section.contains_key(ShortcutAction::CommandCompose.id()) {
                    map.insert(
                        ShortcutAction::CommandCompose,
                        default_combo(ShortcutAction::CommandCompose).map(OwnedCombo::from),
                    );
                }
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

    /// A snapshot of the live map (read hook — the conflict check and the window
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
            eprintln!("nice: shortcut binding persist failed: {e}");
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
/// as R19's sort store / R21's theme store, so `<support-root>/<variant>/ui_settings.json`
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

    /// A missing file loads all defaults (fresh-install path, load rule 1).
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
        assert!(store.set_in_memory(ShortcutAction::NewTerminalWindow, Some(combo("cmd-y"))));
        assert!(store.set_in_memory(ShortcutAction::ToggleSidebar, None)); // explicit unbind

        let reloaded = ShortcutBindings::load(path);
        assert_eq!(
            reloaded.binding(ShortcutAction::NewTerminalWindow),
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
            store.set_in_memory(ShortcutAction::NewTerminalWindow, Some(combo("cmd-y"))),
            "first set changes"
        );
        assert!(
            !store.set_in_memory(ShortcutAction::NewTerminalWindow, Some(combo("cmd-y"))),
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
        assert!(store.set_in_memory(ShortcutAction::NewTerminalWindow, Some(combo("cmd-y"))));

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

    /// The write rule: the persisted section carries EVERY action's key, each a
    /// chord string or explicit `null` (a self-describing, diffable file).
    #[test]
    fn write_persists_full_map_with_explicit_null() {
        let path = temp_path("fullmap");
        let mut store = ShortcutBindings::load(path.clone());
        store.set_in_memory(ShortcutAction::ToggleSidebar, None); // explicit unbind

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let section = raw["shortcuts"].as_object().expect("shortcuts is an object");
        assert_eq!(
            section.len(),
            ShortcutAction::ALL.len(),
            "every action's key is persisted"
        );
        // The unbound action is an explicit JSON null.
        assert!(section["toggleSidebar"].is_null());
        // A bound action is its chord string.
        assert_eq!(section["undoFileOperation"], "cmd-z");
    }

    /// Load rule 1: an absent `shortcuts` section (even with a sibling section
    /// present) loads all defaults.
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
        assert_eq!(store.binding(ShortcutAction::NewTerminalWindow), Some(combo("cmd-y")));
        // Present + null ⇒ unbound (rule 4).
        assert_eq!(store.binding(ShortcutAction::ToggleSidebar), None);
        // Absent from a present section ⇒ unbound, NOT the default (rule 5).
        assert_eq!(store.binding(ShortcutAction::UndoFileOperation), None);
        assert!(!store.is_at_default(ShortcutAction::UndoFileOperation));
    }

    /// The rule-5 seeding exception: `commandCompose` ABSENT from a present
    /// (pre-14th-action) section loads at its DEFAULT ⌘↩ — a customizer's
    /// legacy 13-key file gains the new shortcut instead of shipping it dead.
    #[test]
    fn legacy_section_seeds_command_compose_default() {
        let path = temp_path("seed-compose");
        // A customized 13-key-era section: no commandCompose key at all.
        std::fs::write(
            &path,
            br#"{"version":1,"shortcuts":{"newTerminalPane":"cmd-y","toggleSidebar":null}}"#,
        )
        .unwrap();
        let store = ShortcutBindings::load(path);
        assert_eq!(
            store.binding(ShortcutAction::CommandCompose),
            Some(combo("cmd-enter")),
            "absent commandCompose seeds the default, not rule 5's unbound"
        );
        assert!(store.is_at_default(ShortcutAction::CommandCompose));
        // Rule 5 is UNCHANGED for every pre-existing action.
        assert_eq!(store.binding(ShortcutAction::UndoFileOperation), None);
    }

    /// The seed never overrides an explicit choice: `"commandCompose": null`
    /// (the user unbound it) still loads unbound, and a rebound chord decodes.
    #[test]
    fn explicit_command_compose_choice_beats_the_seed() {
        let unbound = temp_path("compose-null");
        std::fs::write(
            &unbound,
            br#"{"version":1,"shortcuts":{"commandCompose":null}}"#,
        )
        .unwrap();
        let store = ShortcutBindings::load(unbound);
        assert_eq!(
            store.binding(ShortcutAction::CommandCompose),
            None,
            "an explicit null (rule 4) wins over the seeding exception"
        );

        let rebound = temp_path("compose-rebound");
        std::fs::write(
            &rebound,
            br#"{"version":1,"shortcuts":{"commandCompose":"cmd-shift-enter"}}"#,
        )
        .unwrap();
        let store2 = ShortcutBindings::load(rebound);
        assert_eq!(
            store2.binding(ShortcutAction::CommandCompose),
            Some(combo("cmd-shift-enter"))
        );
    }

    /// The seeded default persists on the next write: load a legacy section,
    /// mutate anything, and the written file carries `commandCompose: cmd-enter`.
    #[test]
    fn seeded_default_round_trips_through_the_next_write() {
        let path = temp_path("seed-roundtrip");
        std::fs::write(
            &path,
            br#"{"version":1,"shortcuts":{"newTerminalPane":"cmd-y"}}"#,
        )
        .unwrap();
        let mut store = ShortcutBindings::load(path.clone());
        store.set_in_memory(ShortcutAction::ToggleSidebar, Some(combo("cmd-j")));

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["shortcuts"]["commandCompose"], "cmd-enter");
        // And a fresh load of the written file needs no seeding path at all.
        let reloaded = ShortcutBindings::load(path);
        assert_eq!(
            reloaded.binding(ShortcutAction::CommandCompose),
            Some(combo("cmd-enter"))
        );
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
        assert_eq!(store.binding(ShortcutAction::NewTerminalWindow), Some(combo("cmd-y")));
        // The bogus key produced no entry; every other action is unbound (rule 5).
        assert_eq!(store.binding(ShortcutAction::ToggleSidebar), None);
    }

    /// Load rule 2: malformed JSON is fail-soft ⇒ all defaults, no crash.
    #[test]
    fn malformed_json_falls_back_to_defaults() {
        let path = temp_path("garbage");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = ShortcutBindings::load(path);
        assert!(store.is_at_default(ShortcutAction::NewTerminalWindow));

        // A mistyped section (not a map) also fails soft to defaults.
        let path2 = temp_path("mistyped");
        std::fs::write(&path2, br#"{"version":1,"shortcuts":"not-a-map"}"#).unwrap();
        let store2 = ShortcutBindings::load(path2);
        assert!(store2.is_at_default(ShortcutAction::NewTerminalWindow));
    }

    /// `is_at_default` flips off after a rebind and back on after reset-to-default
    /// (exercised directly through `set_in_memory`, the engine `reset` uses).
    #[test]
    fn is_at_default_tracks_rebind_and_reset() {
        let path = temp_path("at-default");
        let mut store = ShortcutBindings::load(path);
        assert!(store.is_at_default(ShortcutAction::NewTerminalWindow));

        store.set_in_memory(ShortcutAction::NewTerminalWindow, Some(combo("cmd-y")));
        assert!(!store.is_at_default(ShortcutAction::NewTerminalWindow));

        // Restore the default combo (what `reset` sets).
        let default = default_combo(ShortcutAction::NewTerminalWindow)
            .map(OwnedCombo::from);
        store.set_in_memory(ShortcutAction::NewTerminalWindow, default);
        assert!(store.is_at_default(ShortcutAction::NewTerminalWindow));
    }

    /// An unbound action is never "at default" (every action HAS a default).
    #[test]
    fn unbound_is_not_at_default() {
        let path = temp_path("unbound");
        let mut store = ShortcutBindings::load(path);
        store.set_in_memory(ShortcutAction::NewTerminalWindow, None);
        assert!(!store.is_at_default(ShortcutAction::NewTerminalWindow));
        // A masked-modifier default sanity check: the default really is cmd-t.
        assert_eq!(
            default_combo(ShortcutAction::NewTerminalWindow).map(OwnedCombo::from),
            Some(OwnedCombo {
                modifiers: Modifiers::COMMAND,
                key: "t".to_string()
            })
        );
    }
}

// ---- Phase R: pre-rename `shortcuts` section compatibility ---------------

#[cfg(test)]
mod pre_rename_compat_tests {
    use super::*;

    /// A real-shaped `ui_settings.json` written BEFORE the Phase R rename of the
    /// `ShortcutAction` variants (`NextSidebarSession`/`NextWindow`/`NewTerminalWindow` →
    /// `NextSidebarSession`/`NextWindow`/`NewTerminalWindow`). The serialized
    /// action ids are a frozen surface and are unchanged by the rename.
    const PRE_RENAME_UI_SETTINGS: &str = include_str!("fixtures/pre_rename_ui_settings.json");

    /// The exact id strings the fixture uses, in [`ShortcutAction::ALL`] order.
    /// Written out longhand so a variant rename that forgot to pin its id fails
    /// here rather than silently orphaning every user's binding.
    const FROZEN_IDS: [(ShortcutAction, &str); 14] = [
        (ShortcutAction::NextSidebarSession, "nextSidebarTab"),
        (ShortcutAction::PrevSidebarSession, "prevSidebarTab"),
        (ShortcutAction::NextWindow, "nextPane"),
        (ShortcutAction::PrevWindow, "prevPane"),
        (ShortcutAction::NewTerminalWindow, "newTerminalPane"),
        (ShortcutAction::ToggleSidebar, "toggleSidebar"),
        (ShortcutAction::ToggleSidebarMode, "toggleSidebarMode"),
        (ShortcutAction::ToggleHiddenFiles, "toggleHiddenFiles"),
        (ShortcutAction::IncreaseFontSize, "increaseFontSize"),
        (ShortcutAction::DecreaseFontSize, "decreaseFontSize"),
        (ShortcutAction::ResetFontSizes, "resetFontSizes"),
        (ShortcutAction::UndoFileOperation, "undoFileOperation"),
        (ShortcutAction::RedoFileOperation, "redoFileOperation"),
        (ShortcutAction::CommandCompose, "commandCompose"),
    ];

    fn scratch_ui_settings(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-shortcuts-compat-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ui_settings.json");
        std::fs::write(&path, PRE_RENAME_UI_SETTINGS).unwrap();
        path
    }

    /// Every action id serializes to its pre-rename spelling. The variants moved;
    /// the on-disk keys did not.
    ///
    /// The frozen fourteen are pinned as the LEADING slice of
    /// [`ShortcutAction::ALL`] — later phases append actions (tmux Phase 1 added
    /// eight) — but nothing may reorder, rename, or drop the original ids, because
    /// they are the persistence keys in every existing `ui_settings.json`.
    #[test]
    fn shortcut_action_ids_are_frozen_across_the_rename() {
        assert_eq!(
            ShortcutAction::ALL[..FROZEN_IDS.len()],
            FROZEN_IDS.map(|(a, _)| a),
            "the original fourteen must stay first, in order"
        );
        for (action, id) in FROZEN_IDS {
            assert_eq!(action.id(), id, "{action:?} id must stay frozen");
            assert_eq!(ShortcutAction::from_id(id), Some(action));
        }
    }

    /// A pre-rename `shortcuts` section loads with every binding intact: the
    /// custom chords land on the renamed variants, the explicit `null` loads
    /// unbound, and an id no longer in the set is dropped silently.
    #[test]
    fn loads_pre_rename_shortcuts_section_with_every_binding_intact() {
        let store = ShortcutBindings::load(scratch_ui_settings("load"));

        let expected = [
            (ShortcutAction::NextSidebarSession, Some("cmd-alt-down")),
            (ShortcutAction::PrevSidebarSession, Some("cmd-alt-up")),
            (ShortcutAction::NextWindow, Some("cmd-alt-right")),
            (ShortcutAction::PrevWindow, Some("cmd-alt-left")),
            // A user-customized chord (the default is plain ⌘T) — proves the
            // stored VALUE is read, not silently re-defaulted.
            (ShortcutAction::NewTerminalWindow, Some("cmd-shift-t")),
            (ShortcutAction::ToggleSidebar, None), // explicit null ⇒ unbound
            (ShortcutAction::ToggleSidebarMode, Some("cmd-shift-b")),
            (ShortcutAction::ToggleHiddenFiles, Some("cmd-shift-.")),
            (ShortcutAction::IncreaseFontSize, Some("cmd-=")),
            (ShortcutAction::DecreaseFontSize, Some("cmd--")),
            (ShortcutAction::ResetFontSizes, Some("cmd-0")),
            (ShortcutAction::UndoFileOperation, Some("cmd-z")),
            (ShortcutAction::RedoFileOperation, Some("cmd-shift-z")),
            (ShortcutAction::CommandCompose, Some("cmd-enter")),
        ];
        for (action, token) in expected {
            assert_eq!(
                store.binding(action),
                token.map(|t| OwnedCombo::from_token(t).unwrap()),
                "{action:?} must load its pre-rename binding"
            );
        }
        // The unknown `somethingRemovedLongAgo` key was dropped, not mapped.
        assert_eq!(store.bindings().len(), ShortcutAction::ALL.len());

        // Frozen load rule 5 for the tmux Phase 1 additions (the plan's accepted
        // store-migration consequence, explicitly NOT seeded): ids absent from a
        // PRESENT section load UNBOUND, so a user who ever rebound anything keeps
        // their old board and picks the new chords up by hand.
        for action in [
            ShortcutAction::FocusPaneLeft,
            ShortcutAction::LastActiveWindow,
            ShortcutAction::ScrollHalfPageUp,
            ShortcutAction::WindowByIndex,
        ] {
            assert_eq!(
                store.binding(action),
                None,
                "{action:?} is absent from the pre-Phase-1 file, so it loads unbound"
            );
        }
    }

    /// Round-trip: persisting the freshly-loaded map rewrites every frozen id with
    /// the SAME value, drops the unknown key, and leaves every co-writer's section
    /// alone. (The write rule persists the FULL map, so ids added after the fixture
    /// was written appear too — as explicit `null`, per load rule 5.)
    #[test]
    fn pre_rename_shortcuts_section_round_trips_with_identical_ids() {
        let path = scratch_ui_settings("roundtrip");
        let before: serde_json::Value =
            serde_json::from_str(PRE_RENAME_UI_SETTINGS).unwrap();

        let mut store = ShortcutBindings::load(path.clone());
        // Flip one binding away and back so `persist` actually runs (the store
        // only writes on a real change), leaving the map exactly as loaded.
        let original = store.binding(ShortcutAction::NewTerminalWindow);
        assert!(store.set_in_memory(
            ShortcutAction::NewTerminalWindow,
            Some(OwnedCombo::from_token("cmd-y").unwrap())
        ));
        assert!(store.set_in_memory(ShortcutAction::NewTerminalWindow, original));

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let written = after["shortcuts"].as_object().expect("shortcuts section");

        // Exactly the current action id set — the unknown key is not re-emitted,
        // and every frozen id is still there.
        let mut keys: Vec<&str> = written.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut want: Vec<&str> = ShortcutAction::ALL.iter().map(|a| a.id()).collect();
        want.sort_unstable();
        assert_eq!(keys, want, "the persisted key set is the full action id set");
        for (_, id) in FROZEN_IDS {
            assert!(keys.contains(&id), "the frozen id {id} must survive the rewrite");
        }
        assert!(
            !keys.contains(&"somethingRemovedLongAgo"),
            "an unknown key must not be re-emitted"
        );
        // The ids added after this fixture was written persist as explicit nulls
        // (load rule 5: absent from a present section ⇒ unbound).
        assert!(written["windowByIndex"].is_null());

        // Every value that was in the pre-rename file round-trips unchanged.
        let read_section = before["shortcuts"].as_object().unwrap();
        for (id, value) in read_section {
            if ShortcutAction::from_id(id).is_none() {
                continue; // the dropped unknown key
            }
            assert_eq!(&written[id], value, "{id} must round-trip unchanged");
        }

        // A co-writer's section survives the read-merge-write.
        assert_eq!(after["appearance"], before["appearance"]);
    }
}

