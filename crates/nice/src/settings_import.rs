//! First-launch settings import from the prod Swift `Nice` build.
//!
//! On a GENUINE first launch of the Rust build (its own `ui_settings.json` does
//! not yet exist), copy every prod Swift Nice setting that has a Rust equivalent
//! ONE time, so a fresh install comes up looking + behaving like the user's prod
//! instance instead of shipped defaults (plan: settings-import-from-prod). Two
//! destinations:
//!
//! * **Direct `ui_settings.json` settings** (fonts, smooth-scroll, appearance,
//!   file-browser sort) — written into the shared `ui_settings.json` through the
//!   same read-merge-write writer the R19/R21/R23 stores use, BEFORE those stores
//!   `::load` in `app::run`, so each load observes the imported values. Only two
//!   translations: the file-browser criterion rawValue `dateModified` →
//!   `date_modified`; everything else is verbatim. An absent prod key writes
//!   nothing for it (the Rust default stands).
//! * **The `shortcuts` section** of `ui_settings.json` — prod stores its rebindable
//!   keyboard shortcuts as a JSON `Data` blob under `keyboardShortcuts`
//!   (`{actionId: {keyCode, modifierFlagsRaw}}`, physical Carbon keyCodes +
//!   `NSEvent.ModifierFlags` bits). We translate each prod entry into the chord
//!   token the Rust parser accepts (`OwnedCombo::from_token`) — an explicit
//!   prod-action-id → [`ShortcutAction`] map, a static US-ANSI kVK → gpui key-name
//!   table, and an `NSEvent`-bit → prefix decode — and write a COMPLETE 13-key
//!   `shortcuts` section (each action a token or JSON `null`) so the store's own
//!   load rules reproduce prod exactly. Written into the same `ui_settings.json`
//!   BEFORE `ShortcutBindings::load` in `app::run`. Fail-soft: an unmappable keyCode
//!   keeps the Rust default, an unknown prod action is skipped, malformed/partial
//!   JSON imports what is valid and skips the rest (plan decision #5).
//! * **The three CFPref toggles** (`syncClaudeTheme`, `installHandoffSkill`,
//!   `handoffSkillPromptSeen`) — copied from the prod CFPreferences domain into
//!   this app's OWN domain via [`crate::platform::write_bool_pref`], but ONLY for
//!   a toggle that EXISTS in prod AND is ABSENT in the own domain, so a value the
//!   user already set in the Rust build is never clobbered. `handoffSkillPromptSeen`
//!   is imported too, so a migrated user does not re-see the first-launch prompt.
//!
//! ## Seams (hermetic testing)
//!
//! The prod source is behind [`ProdSettingsReader`] (mirroring
//! `session_store`'s injectable Swift-migration source): production reads the
//! prod CFPreferences domain via `platform.rs`; unit tests inject an in-memory
//! map, touching no CFPreferences at all. The own-domain toggle side is behind
//! [`ToggleSink`] for the same reason. Production resolves the prod domain from
//! [`crate::platform::prod_settings_domain`] (the `NICE_PROD_SETTINGS_DOMAIN`
//! env seam), so even the black-box path never reads the real prod domain.
//!
//! ## Fail-soft (plan decision #6)
//!
//! Prod not installed / domain empty / partial data → import what is valid, skip
//! the rest, never panic, never block startup. The direct-settings write happens
//! EAGERLY on first launch even when prod contributed nothing, so the gate ("own
//! `ui_settings.json` exists") flips after the first launch and the import is
//! genuinely one-shot — a no-prod first launch cannot re-read prod forever.

use std::collections::HashMap;
use std::path::Path;

use nice_model::shortcuts::{default_combo, ShortcutAction};

use crate::file_browser::sort_settings_store::write_ui_settings_merged;

// --- Prod UserDefaults keys (domain `dev.nickanderssohn.nice`) --------------
// Verified against Sources/Nice/State/{FontSettings,Tweaks,FileBrowserSortSettings}.swift.

const PROD_TERMINAL_FONT_SIZE: &str = "terminalFontSize";
const PROD_SIDEBAR_FONT_SIZE: &str = "sidebarFontSize";
const PROD_TERMINAL_FONT_FAMILY: &str = "terminalFontFamily";
const PROD_SMOOTH_SCROLLING: &str = "smoothScrolling";
const PROD_SCHEME: &str = "scheme";
const PROD_SYNC_WITH_OS: &str = "syncWithOS";
const PROD_CHROME_LIGHT_PALETTE: &str = "chromeLightPalette";
const PROD_CHROME_DARK_PALETTE: &str = "chromeDarkPalette";
const PROD_ACCENT: &str = "accent";
const PROD_TERMINAL_THEME_LIGHT_ID: &str = "terminalThemeLightId";
const PROD_TERMINAL_THEME_DARK_ID: &str = "terminalThemeDarkId";
const PROD_FB_SORT_CRITERION: &str = "fileBrowser.sort.criterion";
const PROD_FB_SORT_ASCENDING: &str = "fileBrowser.sort.ascending";
/// Prod's keyboard-shortcuts blob: a JSON-encoded `Data` value under this key,
/// shaped `{actionId: {keyCode: Int, modifierFlagsRaw: UInt}}`
/// (`KeyboardShortcuts.swift`: `defaultsKey = "keyboardShortcuts"`).
const PROD_KEYBOARD_SHORTCUTS: &str = "keyboardShortcuts";

/// The three CFPref toggles. Each uses the SAME key name in the prod domain and
/// this app's own domain, so a copy is a same-key prod→own move.
const TOGGLE_KEYS: [&str; 3] = [
    "syncClaudeTheme",
    "installHandoffSkill",
    "handoffSkillPromptSeen",
];

/// Injectable source of the prod Swift Nice settings — the hermetic test seam
/// (the `session_store` injectable-source analog). Production
/// ([`PlatformProdReader`]) reads the prod CFPreferences domain via `platform.rs`;
/// tests inject an in-memory map so no test reads the real prod domain.
pub trait ProdSettingsReader {
    /// A string-valued prod key (theme id, palette, accent, font family, scheme),
    /// or `None` when absent / not a string.
    fn string(&self, key: &str) -> Option<String>;
    /// A numeric prod key (font size) as `f64`, or `None` when absent / not a number.
    fn number(&self, key: &str) -> Option<f64>;
    /// A boolean prod key (smooth-scroll, sync-with-os, sort ascending, a toggle),
    /// or `None` when absent / not a boolean.
    fn boolean(&self, key: &str) -> Option<bool>;
    /// A raw-`Data` prod key (the `keyboardShortcuts` JSON blob) as owned bytes,
    /// or `None` when absent / not `Data`.
    fn data(&self, key: &str) -> Option<Vec<u8>>;
}

/// Production [`ProdSettingsReader`] over a resolved prod CFPreferences domain.
struct PlatformProdReader {
    domain: String,
}

impl ProdSettingsReader for PlatformProdReader {
    fn string(&self, key: &str) -> Option<String> {
        crate::platform::read_prod_string(key, &self.domain)
    }
    fn number(&self, key: &str) -> Option<f64> {
        crate::platform::read_prod_f64(key, &self.domain)
    }
    fn boolean(&self, key: &str) -> Option<bool> {
        crate::platform::read_prod_bool(key, &self.domain)
    }
    fn data(&self, key: &str) -> Option<Vec<u8>> {
        crate::platform::read_prod_data(key, &self.domain)
    }
}

/// The own-domain side of the toggle copy, behind a seam so the toggle logic is
/// unit-testable without touching real CFPreferences (production
/// [`PlatformToggleSink`] uses `platform.rs`; tests use an in-memory map).
pub trait ToggleSink {
    /// Whether `key` is already present in this app's own domain (the user set it
    /// in the Rust build).
    fn own_present(&self, key: &str) -> bool;
    /// Write `value` for `key` into this app's own domain.
    fn write_own(&mut self, key: &str, value: bool);
}

/// Production [`ToggleSink`] over this app's own CFPreferences domain.
struct PlatformToggleSink;

impl ToggleSink for PlatformToggleSink {
    fn own_present(&self, key: &str) -> bool {
        crate::platform::read_own_bool_pref(key).is_some()
    }
    fn write_own(&mut self, key: &str, value: bool) {
        crate::platform::write_bool_pref(key, value);
    }
}

/// Translate the prod file-browser criterion rawValue to the Rust
/// `file_browser_sort.criterion` rawValue: `dateModified` → `date_modified`
/// (the Swift enum spelling → the Rust snake_case spelling); `name` and any
/// unknown value pass through verbatim (the Rust store's tolerant decode maps an
/// unknown value back to its own default).
fn translate_criterion(raw: &str) -> String {
    match raw {
        "dateModified" => "date_modified".to_string(),
        other => other.to_string(),
    }
}

/// Build the `ui_settings.json` sections from the prod reader, including ONLY the
/// keys prod actually holds — an absent prod key contributes nothing, so the Rust
/// store's own default stands (plan decision #3). An entirely-empty section is
/// omitted. The returned map is section-name → section-object, ready to merge
/// into `ui_settings.json`.
fn build_direct_sections(
    reader: &dyn ProdSettingsReader,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::{Map, Value};

    /// Insert a prod `f64` as a JSON number, skipping NaN/∞ (which JSON cannot
    /// represent) so a garbage prod value never poisons the whole write.
    fn insert_number(map: &mut Map<String, Value>, key: &str, v: f64) {
        if let Some(n) = serde_json::Number::from_f64(v) {
            map.insert(key.to_string(), Value::Number(n));
        }
    }

    let mut sections = Map::new();

    // fonts
    let mut fonts = Map::new();
    if let Some(v) = reader.number(PROD_TERMINAL_FONT_SIZE) {
        insert_number(&mut fonts, "terminal_font_size", v);
    }
    if let Some(v) = reader.number(PROD_SIDEBAR_FONT_SIZE) {
        insert_number(&mut fonts, "sidebar_font_size", v);
    }
    if let Some(v) = reader.string(PROD_TERMINAL_FONT_FAMILY) {
        fonts.insert("terminal_font_family".to_string(), Value::String(v));
    }
    if !fonts.is_empty() {
        sections.insert("fonts".to_string(), Value::Object(fonts));
    }

    // advanced (smooth-scroll)
    if let Some(v) = reader.boolean(PROD_SMOOTH_SCROLLING) {
        let mut advanced = Map::new();
        advanced.insert("smooth_scroll".to_string(), Value::Bool(v));
        sections.insert("advanced".to_string(), Value::Object(advanced));
    }

    // appearance (scheme / sync / palettes / accent / theme ids — all verbatim)
    let mut appearance = Map::new();
    if let Some(v) = reader.string(PROD_SCHEME) {
        appearance.insert("scheme".to_string(), Value::String(v));
    }
    if let Some(v) = reader.boolean(PROD_SYNC_WITH_OS) {
        appearance.insert("sync_with_os".to_string(), Value::Bool(v));
    }
    if let Some(v) = reader.string(PROD_CHROME_LIGHT_PALETTE) {
        appearance.insert("chrome_light_palette".to_string(), Value::String(v));
    }
    if let Some(v) = reader.string(PROD_CHROME_DARK_PALETTE) {
        appearance.insert("chrome_dark_palette".to_string(), Value::String(v));
    }
    if let Some(v) = reader.string(PROD_ACCENT) {
        appearance.insert("accent".to_string(), Value::String(v));
    }
    if let Some(v) = reader.string(PROD_TERMINAL_THEME_LIGHT_ID) {
        appearance.insert("terminal_theme_light_id".to_string(), Value::String(v));
    }
    if let Some(v) = reader.string(PROD_TERMINAL_THEME_DARK_ID) {
        appearance.insert("terminal_theme_dark_id".to_string(), Value::String(v));
    }
    if !appearance.is_empty() {
        sections.insert("appearance".to_string(), Value::Object(appearance));
    }

    // file_browser_sort (criterion translated; ascending verbatim)
    let mut fb_sort = Map::new();
    if let Some(v) = reader.string(PROD_FB_SORT_CRITERION) {
        fb_sort.insert(
            "criterion".to_string(),
            Value::String(translate_criterion(&v)),
        );
    }
    if let Some(v) = reader.boolean(PROD_FB_SORT_ASCENDING) {
        fb_sort.insert("ascending".to_string(), Value::Bool(v));
    }
    if !fb_sort.is_empty() {
        sections.insert("file_browser_sort".to_string(), Value::Object(fb_sort));
    }

    sections
}

// --- Keyboard-shortcut translation (plan decision #5) -----------------------
//
// Prod persists its rebindable shortcuts as physical Carbon keyCodes +
// `NSEvent.ModifierFlags` bits; the Rust store wants a `shortcuts` section of
// chord tokens (`OwnedCombo::from_token`). Three pure translations bridge them:
// the action-id map, the kVK→key-name table, and the modifier-bit→prefix decode.

/// The four `NSEvent.ModifierFlags` bits a shortcut can carry (the same set Swift
/// masks to `relevantModifierMask`). Caps Lock / function / numeric-pad bits are
/// ignored — a `&`-test against these four naturally strips them.
const NS_SHIFT: u64 = 1 << 17;
const NS_CONTROL: u64 = 1 << 18;
const NS_OPTION: u64 = 1 << 19;
const NS_COMMAND: u64 = 1 << 20;

/// The explicit prod-action-id (Swift `ShortcutAction` rawValue, camelCase) →
/// Rust [`ShortcutAction`] pairing. Spelled out rather than assumed: even though
/// the Rust ids were ported verbatim from these rawValues, this table is the
/// authoritative, test-verified statement of each pairing, and a prod action with
/// no entry here (a future prod-only action) is simply skipped on import.
const PROD_ACTION_MAP: [(&str, ShortcutAction); 13] = [
    ("nextSidebarTab", ShortcutAction::NextSidebarTab),
    ("prevSidebarTab", ShortcutAction::PrevSidebarTab),
    ("nextPane", ShortcutAction::NextPane),
    ("prevPane", ShortcutAction::PrevPane),
    ("newTerminalPane", ShortcutAction::NewTerminalPane),
    ("toggleSidebar", ShortcutAction::ToggleSidebar),
    ("toggleSidebarMode", ShortcutAction::ToggleSidebarMode),
    ("toggleHiddenFiles", ShortcutAction::ToggleHiddenFiles),
    ("increaseFontSize", ShortcutAction::IncreaseFontSize),
    ("decreaseFontSize", ShortcutAction::DecreaseFontSize),
    ("resetFontSizes", ShortcutAction::ResetFontSizes),
    ("undoFileOperation", ShortcutAction::UndoFileOperation),
    ("redoFileOperation", ShortcutAction::RedoFileOperation),
];

/// The Rust action a prod action-id maps to, or `None` for a prod-only action with
/// no Rust counterpart (skipped on import).
fn rust_action_for_prod_id(id: &str) -> Option<ShortcutAction> {
    PROD_ACTION_MAP
        .iter()
        .find(|(pid, _)| *pid == id)
        .map(|(_, action)| *action)
}

/// One decoded prod shortcut entry (`{keyCode, modifierFlagsRaw}`).
struct ProdShortcut {
    key_code: u16,
    modifier_flags_raw: u64,
}

/// Static US-ANSI Carbon `kVK_*` → gpui key-name table (the exact tokens the Rust
/// parser + gpui keymap use). Ref: `scripts/freeze-harness/ktype.swift` (letters,
/// digits, punctuation) and `<Carbon/HIToolbox/Events.h>` (arrows, F-keys, the
/// specials in `KeyboardShortcuts.keyCodeGlyphs`). US-ANSI only, matching the rest
/// of the app's key handling (plan non-goal: non-US layouts). An unlisted keyCode
/// → `None` (the caller skips that action + warns, keeping the Rust default).
fn key_name_for_keycode(code: u16) -> Option<&'static str> {
    Some(match code {
        // Letters (kVK_ANSI_*)
        0 => "a", 1 => "s", 2 => "d", 3 => "f", 4 => "h", 5 => "g",
        6 => "z", 7 => "x", 8 => "c", 9 => "v", 11 => "b", 12 => "q",
        13 => "w", 14 => "e", 15 => "r", 16 => "y", 17 => "t",
        31 => "o", 32 => "u", 34 => "i", 35 => "p", 37 => "l",
        38 => "j", 40 => "k", 45 => "n", 46 => "m",
        // Digits
        18 => "1", 19 => "2", 20 => "3", 21 => "4", 23 => "5",
        22 => "6", 26 => "7", 28 => "8", 25 => "9", 29 => "0",
        // Punctuation (bind-able per prod's glyph table)
        24 => "=", 27 => "-", 30 => "]", 33 => "[", 39 => "'",
        41 => ";", 42 => "\\", 43 => ",", 44 => "/", 47 => ".",
        50 => "`",
        // Arrows
        123 => "left", 124 => "right", 125 => "down", 126 => "up",
        // Specials (gpui token spelling: kVK_Delete ⌫ → "backspace",
        // kVK_ForwardDelete ⌦ → "delete", kVK_Return → "enter")
        36 => "enter", 48 => "tab", 49 => "space", 51 => "backspace",
        53 => "escape", 117 => "delete", 115 => "home", 119 => "end",
        116 => "pageup", 121 => "pagedown",
        // Function keys F1–F20
        122 => "f1", 120 => "f2", 99 => "f3", 118 => "f4", 96 => "f5",
        97 => "f6", 98 => "f7", 100 => "f8", 101 => "f9", 109 => "f10",
        103 => "f11", 111 => "f12", 105 => "f13", 107 => "f14",
        113 => "f15", 106 => "f16", 64 => "f17", 79 => "f18",
        80 => "f19", 90 => "f20",
        _ => return None,
    })
}

/// Emit the chord token for a translated shortcut — the modifier prefixes in the
/// canonical `cmd`,`ctrl`,`alt`,`shift` order (exactly [`OwnedCombo::to_token`]'s
/// grammar, so the token round-trips through [`OwnedCombo::from_token`]) followed
/// by the gpui `key` name. `modifier_flags_raw` is `&`-tested against the four
/// relevant `NSEvent` bits, so any stray bit (Caps Lock, function) is stripped.
fn emit_token(modifier_flags_raw: u64, key: &str) -> String {
    let mut s = String::new();
    if modifier_flags_raw & NS_COMMAND != 0 {
        s.push_str("cmd-");
    }
    if modifier_flags_raw & NS_CONTROL != 0 {
        s.push_str("ctrl-");
    }
    if modifier_flags_raw & NS_OPTION != 0 {
        s.push_str("alt-");
    }
    if modifier_flags_raw & NS_SHIFT != 0 {
        s.push_str("shift-");
    }
    s.push_str(key);
    s
}

/// `action`'s default chord token — what a shortcut whose prod keyCode is
/// unmappable falls back to (keeping the Rust default, plan decision #5).
fn default_token(action: ShortcutAction) -> String {
    default_combo(action)
        .map(|c| c.chord_str())
        .unwrap_or_default()
}

/// Decode the prod `keyboardShortcuts` JSON `Data` blob into a per-action map,
/// fully fail-soft: malformed top-level JSON → an empty map (import nothing for
/// shortcuts); a single entry missing `keyCode`/`modifierFlagsRaw` or with an
/// out-of-range keyCode is skipped, the rest kept (plan decision #6 — partial data
/// imports what is valid).
fn decode_prod_shortcuts(bytes: &[u8]) -> HashMap<String, ProdShortcut> {
    let mut out = HashMap::new();
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return out;
    };
    let Some(obj) = root.as_object() else {
        return out;
    };
    for (id, entry) in obj {
        let (Some(kc), Some(mf)) = (
            entry.get("keyCode").and_then(serde_json::Value::as_u64),
            entry.get("modifierFlagsRaw").and_then(serde_json::Value::as_u64),
        ) else {
            continue;
        };
        if kc > u16::MAX as u64 {
            continue;
        }
        out.insert(
            id.clone(),
            ProdShortcut {
                key_code: kc as u16,
                modifier_flags_raw: mf,
            },
        );
    }
    out
}

/// Build the COMPLETE 13-key `shortcuts` section from the decoded prod map, or
/// `None` when prod carries no binding for ANY Rust action (prod never customized
/// our shortcuts) — in which case no section is written and the store's defaults
/// stand (its load rule 1). When at least one of our actions is present, every
/// action is emitted explicitly so the store reproduces prod exactly under its own
/// load rules:
///
/// * a prod entry that translates → its chord token;
/// * a prod entry whose keyCode is unmappable → the Rust DEFAULT token (keep
///   default) + a warning;
/// * an action absent from a present prod map → JSON `null` (prod had it explicitly
///   unbound; the store's load rule 4/5 reads `null`/absent-from-present as unbound).
///
/// A prod action-id with no Rust counterpart is skipped + warned.
fn build_shortcuts_section(
    prod: &HashMap<String, ProdShortcut>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    use serde_json::{Map, Value};

    // Fold prod entries onto Rust actions, warning about any prod-only id.
    let mut by_action: HashMap<ShortcutAction, &ProdShortcut> = HashMap::new();
    for (id, combo) in prod {
        match rust_action_for_prod_id(id) {
            Some(action) => {
                by_action.insert(action, combo);
            }
            None => eprintln!(
                "nice: settings import: skipping prod shortcut action {id:?} (no Rust counterpart)"
            ),
        }
    }

    // No overlap with our action set ⇒ leave the Rust defaults untouched.
    if by_action.is_empty() {
        return None;
    }

    let mut section = Map::new();
    for action in ShortcutAction::ALL {
        let value = match by_action.get(&action) {
            Some(combo) => match key_name_for_keycode(combo.key_code) {
                Some(key) => Value::String(emit_token(combo.modifier_flags_raw, key)),
                None => {
                    eprintln!(
                        "nice: settings import: shortcut {} has unmappable keyCode {}; keeping the Rust default",
                        action.id(),
                        combo.key_code
                    );
                    Value::String(default_token(action))
                }
            },
            // Present prod map omits this action ⇒ prod had it explicitly unbound.
            None => Value::Null,
        };
        section.insert(action.id().to_string(), value);
    }
    Some(section)
}

/// The toggle-copy decision (the pure, exhaustively-testable core): copy prod's
/// value into the own domain only when the toggle is PRESENT in prod AND ABSENT
/// in the own domain (plan decision #4); otherwise leave the own domain alone.
fn toggle_import_action(prod: Option<bool>, own_present: bool) -> Option<bool> {
    match (prod, own_present) {
        (Some(value), false) => Some(value),
        _ => None,
    }
}

/// Run the one-shot import when `own_path` (the Rust `ui_settings.json`) is
/// absent — the gated core, with both foreign sides injected so the whole thing
/// is unit-testable. Not first launch (the file exists) ⇒ a clean no-op. Fully
/// fail-soft: a direct-settings write error is logged and swallowed, and every
/// per-key read tolerates an absent / wrong-typed prod value.
fn import_if_first_launch(
    own_path: &Path,
    reader: &dyn ProdSettingsReader,
    toggle_sink: &mut dyn ToggleSink,
) {
    // Gate: only on a genuine first launch (own store absent). Mirrors
    // `session_store`'s "own store present ⇒ never re-migrate" discipline.
    if own_path.exists() {
        return;
    }

    // Direct settings → write EAGERLY (even an empty set stamps `{"version":1}`),
    // so the gate flips to "present" after this launch and the import never
    // re-runs — the load-bearing one-shot guarantee (plan decision #6).
    let mut sections = build_direct_sections(reader);

    // Keyboard shortcuts → the `shortcuts` section (plan decision #5). Decoded from
    // prod's `keyboardShortcuts` JSON `Data`; fail-soft (absent blob / malformed
    // JSON / no overlapping action ⇒ no section, so the store's defaults stand).
    if let Some(bytes) = reader.data(PROD_KEYBOARD_SHORTCUTS) {
        let prod = decode_prod_shortcuts(&bytes);
        if let Some(section) = build_shortcuts_section(&prod) {
            sections.insert(
                "shortcuts".to_string(),
                serde_json::Value::Object(section),
            );
        }
    }

    if let Err(e) = write_ui_settings_merged(own_path, |map| {
        for (key, value) in sections {
            map.insert(key, value);
        }
    }) {
        eprintln!("nice: settings import (direct ui_settings.json) failed: {e}");
    }

    // CFPref toggles → own domain, only-when-present-in-prod-AND-absent-in-own.
    for key in TOGGLE_KEYS {
        if let Some(value) = toggle_import_action(reader.boolean(key), toggle_sink.own_present(key))
        {
            toggle_sink.write_own(key, value);
        }
    }
}

/// First-launch settings-import entry — call from `app::run` ONLY, immediately
/// BEFORE `SettingsPrefsStore::load` (so every subsequent `::load` observes the
/// imported values) and BEFORE the `read_bool_pref` toggle reads (so the copied
/// toggles are live). Resolves the prod CFPreferences domain (honoring the
/// `NICE_PROD_SETTINGS_DOMAIN` seam) and the own `ui_settings.json` path, then
/// runs the gated one-shot import over the production seams. Never called from
/// `run_selftest` (hermeticity: the suite writes no real user state).
pub fn import_prod_settings_on_first_launch() {
    let own_path = crate::file_browser::sort_settings_store::default_ui_settings_path();
    let reader = PlatformProdReader {
        domain: crate::platform::prod_settings_domain(),
    };
    let mut sink = PlatformToggleSink;
    import_if_first_launch(&own_path, &reader, &mut sink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use nice_model::file_browser::FileBrowserSortCriterion;
    use nice_model::shortcuts::{default_bindings, Modifiers, OwnedCombo};
    use nice_theme::palette::{ColorScheme, Palette};
    use nice_theme::AccentPreset;

    use crate::file_browser::sort_settings_store::SortSettingsStore;
    use crate::settings::prefs_store::SettingsPrefsStore;
    use crate::shortcuts_store::ShortcutBindings;
    use crate::theme_settings::ThemeSettingsStore;

    /// An absent temp `ui_settings.json` path in its own throwaway dir (never the
    /// real support root — the import core takes the path directly, so
    /// `default_ui_settings_path` is not exercised here).
    fn temp_ui_settings(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-settings-import-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    #[derive(Default)]
    struct FakeReader {
        strings: HashMap<String, String>,
        numbers: HashMap<String, f64>,
        bools: HashMap<String, bool>,
        datas: HashMap<String, Vec<u8>>,
    }

    impl FakeReader {
        fn with_string(mut self, key: &str, v: &str) -> Self {
            self.strings.insert(key.to_string(), v.to_string());
            self
        }
        fn with_number(mut self, key: &str, v: f64) -> Self {
            self.numbers.insert(key.to_string(), v);
            self
        }
        fn with_bool(mut self, key: &str, v: bool) -> Self {
            self.bools.insert(key.to_string(), v);
            self
        }
        fn with_data(mut self, key: &str, v: &[u8]) -> Self {
            self.datas.insert(key.to_string(), v.to_vec());
            self
        }
    }

    impl ProdSettingsReader for FakeReader {
        fn string(&self, key: &str) -> Option<String> {
            self.strings.get(key).cloned()
        }
        fn number(&self, key: &str) -> Option<f64> {
            self.numbers.get(key).copied()
        }
        fn boolean(&self, key: &str) -> Option<bool> {
            self.bools.get(key).copied()
        }
        fn data(&self, key: &str) -> Option<Vec<u8>> {
            self.datas.get(key).cloned()
        }
    }

    #[derive(Default)]
    struct FakeToggleSink {
        own: HashMap<String, bool>,
        writes: Vec<(String, bool)>,
    }

    impl FakeToggleSink {
        fn with_own(mut self, key: &str, v: bool) -> Self {
            self.own.insert(key.to_string(), v);
            self
        }
    }

    impl ToggleSink for FakeToggleSink {
        fn own_present(&self, key: &str) -> bool {
            self.own.contains_key(key)
        }
        fn write_own(&mut self, key: &str, value: bool) {
            self.own.insert(key.to_string(), value);
            self.writes.push((key.to_string(), value));
        }
    }

    /// A fully-populated prod domain lands every mapped direct key end-to-end:
    /// the three stores that own `ui_settings.json` decode the imported values.
    #[test]
    fn every_direct_key_lands_in_the_stores() {
        let path = temp_ui_settings("direct");
        let reader = FakeReader::default()
            .with_number(PROD_TERMINAL_FONT_SIZE, 15.0)
            .with_number(PROD_SIDEBAR_FONT_SIZE, 11.0)
            .with_string(PROD_TERMINAL_FONT_FAMILY, "Menlo")
            .with_bool(PROD_SMOOTH_SCROLLING, true)
            .with_string(PROD_SCHEME, "light")
            .with_bool(PROD_SYNC_WITH_OS, false)
            .with_string(PROD_CHROME_LIGHT_PALETTE, "nice")
            .with_string(PROD_CHROME_DARK_PALETTE, "nice")
            .with_string(PROD_ACCENT, "iris")
            .with_string(PROD_TERMINAL_THEME_LIGHT_ID, "solarized-light")
            .with_string(PROD_TERMINAL_THEME_DARK_ID, "dracula")
            .with_string(PROD_FB_SORT_CRITERION, "dateModified")
            .with_bool(PROD_FB_SORT_ASCENDING, false);
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        // fonts + advanced (SettingsPrefsStore).
        let prefs = SettingsPrefsStore::load(path.clone());
        assert_eq!(prefs.terminal_font_px(), Some(15.0));
        assert_eq!(prefs.sidebar_font_px(), Some(11.0));
        assert_eq!(prefs.terminal_font_family(), Some("Menlo".to_string()));
        assert!(prefs.smooth_scroll());

        // appearance (ThemeSettingsStore).
        let theme = ThemeSettingsStore::load(path.clone());
        let a = theme.appearance();
        assert_eq!(a.scheme, ColorScheme::Light);
        assert!(!a.sync_with_os);
        assert_eq!(a.chrome_light_palette, Palette::Nice);
        assert_eq!(a.chrome_dark_palette, Palette::Nice);
        assert_eq!(a.accent, AccentPreset::Iris);
        assert_eq!(a.terminal_theme_light_id, "solarized-light");
        assert_eq!(a.terminal_theme_dark_id, "dracula");

        // file_browser_sort (SortSettingsStore) — criterion translated.
        let sort = SortSettingsStore::load(path).settings();
        assert_eq!(sort.criterion, FileBrowserSortCriterion::DateModified);
        assert!(!sort.ascending);
    }

    /// The sole criterion translation: prod `dateModified` → `date_modified`;
    /// `name` passes through verbatim.
    #[test]
    fn criterion_date_modified_is_translated() {
        assert_eq!(translate_criterion("dateModified"), "date_modified");
        assert_eq!(translate_criterion("name"), "name");

        let path = temp_ui_settings("criterion");
        let reader = FakeReader::default().with_string(PROD_FB_SORT_CRITERION, "dateModified");
        let mut sink = FakeToggleSink::default();
        import_if_first_launch(&path, &reader, &mut sink);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["file_browser_sort"]["criterion"], "date_modified");
    }

    /// An absent prod key writes nothing for it: an empty prod domain still
    /// creates the file (flipping the gate) but leaves every store at its default.
    #[test]
    fn absent_prod_keys_leave_defaults() {
        let path = temp_ui_settings("empty");
        let reader = FakeReader::default();
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        // The file was written eagerly (one-shot gate flips).
        assert!(path.exists());
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // No section objects — only the stamped version.
        assert!(raw.get("fonts").is_none());
        assert!(raw.get("advanced").is_none());
        assert!(raw.get("appearance").is_none());
        assert!(raw.get("file_browser_sort").is_none());

        // Stores read defaults.
        let prefs = SettingsPrefsStore::load(path.clone());
        assert_eq!(prefs.terminal_font_px(), None);
        assert_eq!(prefs.terminal_font_family(), None);
        assert!(!prefs.smooth_scroll());
        assert_eq!(*ThemeSettingsStore::load(path.clone()).appearance(), Default::default());
        let sort = SortSettingsStore::load(path).settings();
        assert_eq!(sort.criterion, FileBrowserSortCriterion::Name);
        assert!(sort.ascending);
    }

    /// A partially-populated prod domain imports only the present keys: a lone
    /// `terminalFontSize` writes a `fonts` object with just that field, and no
    /// other section is written at all.
    #[test]
    fn partial_prod_imports_only_present_keys() {
        let path = temp_ui_settings("partial");
        let reader = FakeReader::default().with_number(PROD_TERMINAL_FONT_SIZE, 17.0);
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["fonts"]["terminal_font_size"], 17.0);
        assert!(raw["fonts"].get("terminal_font_family").is_none());
        assert!(raw["fonts"].get("sidebar_font_size").is_none());
        assert!(raw.get("appearance").is_none());
        assert!(raw.get("advanced").is_none());
        assert!(raw.get("file_browser_sort").is_none());
    }

    /// The pure toggle decision: copy iff present-in-prod AND absent-in-own.
    #[test]
    fn toggle_action_only_when_present_in_prod_and_absent_in_own() {
        // Present in prod, absent in own → copy prod's value (both polarities).
        assert_eq!(toggle_import_action(Some(true), false), Some(true));
        assert_eq!(toggle_import_action(Some(false), false), Some(false));
        // Present in own → never overwrite.
        assert_eq!(toggle_import_action(Some(true), true), None);
        assert_eq!(toggle_import_action(Some(false), true), None);
        // Absent in prod → nothing to copy.
        assert_eq!(toggle_import_action(None, false), None);
        assert_eq!(toggle_import_action(None, true), None);
    }

    /// End-to-end toggle copy: prod-present + own-absent toggles are written; a
    /// toggle already set in the own domain is left untouched (no clobber).
    #[test]
    fn toggles_copied_only_when_absent_in_own() {
        let path = temp_ui_settings("toggles");
        let reader = FakeReader::default()
            .with_bool("syncClaudeTheme", false)
            .with_bool("installHandoffSkill", true)
            .with_bool("handoffSkillPromptSeen", true);
        // The user already turned installHandoffSkill OFF in the Rust build.
        let mut sink = FakeToggleSink::default().with_own("installHandoffSkill", false);

        import_if_first_launch(&path, &reader, &mut sink);

        // syncClaudeTheme + handoffSkillPromptSeen adopted from prod.
        assert_eq!(sink.own.get("syncClaudeTheme"), Some(&false));
        assert_eq!(sink.own.get("handoffSkillPromptSeen"), Some(&true));
        // installHandoffSkill NOT overwritten (own wins).
        assert_eq!(sink.own.get("installHandoffSkill"), Some(&false));
        // Exactly the two absent-in-own keys were written.
        assert_eq!(sink.writes.len(), 2);
        assert!(sink
            .writes
            .iter()
            .all(|(k, _)| k == "syncClaudeTheme" || k == "handoffSkillPromptSeen"));
    }

    /// A second launch (own `ui_settings.json` already present) imports nothing:
    /// a user edit survives and no toggle is copied.
    #[test]
    fn second_run_with_own_store_present_imports_nothing() {
        let path = temp_ui_settings("second-run");
        // First launch already produced a file; the user then edited a value.
        std::fs::write(
            &path,
            br#"{"version":1,"fonts":{"terminal_font_size":99}}"#,
        )
        .unwrap();

        let reader = FakeReader::default()
            .with_number(PROD_TERMINAL_FONT_SIZE, 15.0)
            .with_bool("syncClaudeTheme", true);
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        // The user's edit is untouched (no re-import / clobber).
        let prefs = SettingsPrefsStore::load(path);
        assert_eq!(prefs.terminal_font_px(), Some(99.0));
        // No toggle was copied on the second run.
        assert!(sink.writes.is_empty());
        assert!(sink.own.is_empty());
    }

    // --- Keyboard-shortcut import (plan decision #5) ------------------------

    /// A `Modifiers` with exactly the given flags (round-trip expectation helper).
    fn m(command: bool, control: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers {
            command,
            control,
            alt,
            shift,
        }
    }

    /// Encode a prod `keyboardShortcuts` JSON `Data` blob from `(actionId, keyCode,
    /// modifierFlagsRaw)` triples — the exact shape prod's `KeyCombo` Codable emits.
    fn prod_shortcuts_blob(entries: &[(&str, u16, u64)]) -> Vec<u8> {
        let mut map = serde_json::Map::new();
        for (id, kc, mf) in entries {
            map.insert(
                id.to_string(),
                serde_json::json!({ "keyCode": kc, "modifierFlagsRaw": mf }),
            );
        }
        serde_json::to_vec(&serde_json::Value::Object(map)).unwrap()
    }

    /// The prod-action-id → Rust action map is an explicit, verified pairing: every
    /// listed prod id resolves to its action AND agrees with the store's shared JSON
    /// key (`ShortcutAction::id`); an unknown id has no counterpart.
    #[test]
    fn prod_action_map_pairs_every_action_by_verified_id() {
        for (prod_id, action) in PROD_ACTION_MAP {
            assert_eq!(rust_action_for_prod_id(prod_id), Some(action));
            assert_eq!(action.id(), prod_id, "shared JSON key agrees for {action:?}");
        }
        assert_eq!(PROD_ACTION_MAP.len(), ShortcutAction::ALL.len());
        assert_eq!(rust_action_for_prod_id("notAnAction"), None);
    }

    /// The kVK → key-name table uses gpui's exact token spellings for the named
    /// specials the plan enumerates (esc/tab/return/space/delete + an arrow); an
    /// unmapped keyCode is `None`.
    #[test]
    fn special_key_names_match_gpui_tokens() {
        assert_eq!(key_name_for_keycode(53), Some("escape"));
        assert_eq!(key_name_for_keycode(48), Some("tab"));
        assert_eq!(key_name_for_keycode(36), Some("enter")); // kVK_Return
        assert_eq!(key_name_for_keycode(49), Some("space"));
        assert_eq!(key_name_for_keycode(51), Some("backspace")); // kVK_Delete ⌫
        assert_eq!(key_name_for_keycode(126), Some("up"));
        assert_eq!(key_name_for_keycode(200), None); // unmapped
    }

    /// Every emitted token feeds back through the REAL Rust chord parser
    /// (`OwnedCombo::from_token`) and recovers the intended key + modifiers, across
    /// the representative matrix: letter / digit / F-key / arrow, each modifier
    /// alone and all four combined, plus an irrelevant (Caps Lock) bit stripped.
    #[test]
    fn emitted_tokens_round_trip_through_owned_combo_parser() {
        let cases: &[(u16, u64, Modifiers, &str)] = &[
            // letter, digit, F-key (no modifiers), arrow.
            (17, NS_COMMAND, m(true, false, false, false), "t"),
            (29, NS_COMMAND, m(true, false, false, false), "0"),
            (96, 0, m(false, false, false, false), "f5"),
            (125, NS_COMMAND | NS_OPTION, m(true, false, true, false), "down"),
            // each modifier alone (key "a").
            (0, NS_SHIFT, m(false, false, false, true), "a"),
            (0, NS_CONTROL, m(false, true, false, false), "a"),
            (0, NS_OPTION, m(false, false, true, false), "a"),
            (0, NS_COMMAND, m(true, false, false, false), "a"),
            // all four combined.
            (
                0,
                NS_COMMAND | NS_CONTROL | NS_OPTION | NS_SHIFT,
                m(true, true, true, true),
                "a",
            ),
            // an irrelevant bit (Caps Lock, 1<<16) is stripped.
            (0, NS_COMMAND | (1 << 16), m(true, false, false, false), "a"),
        ];
        for (kc, mf, want_mods, want_key) in cases {
            let key = key_name_for_keycode(*kc).expect("keyCode maps");
            let token = emit_token(*mf, key);
            let parsed = OwnedCombo::from_token(&token).expect("emitted token parses");
            assert_eq!(parsed.modifiers, *want_mods, "modifiers for keyCode {kc} ({token:?})");
            assert_eq!(parsed.key, *want_key, "key for keyCode {kc} ({token:?})");
        }
    }

    /// End-to-end: a prod blob imported into `ui_settings.json` is read back by the
    /// REAL `ShortcutBindings` store, exercising the whole translate → write → parse
    /// path. Covers a custom rebind, a default-valued entry, an unmappable keyCode
    /// (keep Rust default), a present-map-omits action (explicitly unbound), and a
    /// prod-only action-id (skipped).
    #[test]
    fn shortcuts_import_round_trips_through_the_store() {
        let path = temp_ui_settings("shortcuts-e2e");
        let blob = prod_shortcuts_blob(&[
            ("newTerminalPane", 16, NS_COMMAND),             // ⌘Y (custom rebind)
            ("nextSidebarTab", 125, NS_COMMAND | NS_OPTION), // ⌘⌥↓ (== default)
            ("undoFileOperation", 200, NS_COMMAND),          // unmappable → keep ⌘Z
            ("someFutureAction", 17, NS_COMMAND),            // no Rust counterpart
        ]);
        let reader = FakeReader::default().with_data(PROD_KEYBOARD_SHORTCUTS, &blob);
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        // A prod-only id never leaks a bogus key into the section.
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(raw["shortcuts"].get("someFutureAction").is_none());
        assert_eq!(raw["shortcuts"].as_object().unwrap().len(), 13);

        let store = ShortcutBindings::load(path);
        // Custom rebind imported and re-parsed by the real store.
        assert_eq!(
            store.binding(ShortcutAction::NewTerminalPane),
            OwnedCombo::from_token("cmd-y")
        );
        // A prod entry equal to the default lands as the default.
        assert_eq!(
            store.binding(ShortcutAction::NextSidebarTab),
            OwnedCombo::from_token("cmd-alt-down")
        );
        // Unmappable keyCode kept the Rust default (⌘Z).
        assert_eq!(
            store.binding(ShortcutAction::UndoFileOperation),
            OwnedCombo::from_token("cmd-z")
        );
        // Present-map-omits ⇒ explicitly unbound (not the default).
        assert_eq!(store.binding(ShortcutAction::ToggleSidebar), None);
        assert!(!store.is_at_default(ShortcutAction::ToggleSidebar));
    }

    /// An absent prod `keyboardShortcuts` blob writes NO `shortcuts` section, so the
    /// store loads all 13 defaults (load rule 1) — a fresh prod user is not
    /// clobbered into all-unbound.
    #[test]
    fn absent_prod_shortcuts_leaves_store_defaults() {
        let path = temp_ui_settings("shortcuts-absent");
        let reader = FakeReader::default().with_number(PROD_TERMINAL_FONT_SIZE, 15.0);
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(raw.get("shortcuts").is_none());

        let store = ShortcutBindings::load(path);
        for (action, def) in default_bindings() {
            assert_eq!(store.binding(action), Some(OwnedCombo::from(def)));
        }
    }

    /// Malformed shortcut JSON is fail-soft: `decode_prod_shortcuts` yields an empty
    /// map, no section is written, and the store loads defaults.
    #[test]
    fn malformed_prod_shortcuts_falls_back_to_defaults() {
        assert!(decode_prod_shortcuts(b"{ not json").is_empty());

        let path = temp_ui_settings("shortcuts-garbage");
        let reader = FakeReader::default().with_data(PROD_KEYBOARD_SHORTCUTS, b"{ not json");
        let mut sink = FakeToggleSink::default();

        import_if_first_launch(&path, &reader, &mut sink);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(raw.get("shortcuts").is_none());
        assert!(ShortcutBindings::load(path).is_at_default(ShortcutAction::NewTerminalPane));
    }

    /// Partial data: an entry missing `modifierFlagsRaw` is skipped while the valid
    /// entry is kept (plan decision #6 — import what is valid).
    #[test]
    fn partial_entry_skipped_others_kept() {
        let decoded = decode_prod_shortcuts(
            br#"{"newTerminalPane":{"keyCode":16,"modifierFlagsRaw":1048576},"toggleSidebar":{"keyCode":11}}"#,
        );
        assert!(decoded.contains_key("newTerminalPane"));
        assert!(!decoded.contains_key("toggleSidebar"));
    }

    /// A prod map that overlaps NONE of our actions (only unknown ids) writes no
    /// section, so the store's defaults stand rather than collapsing to all-unbound.
    #[test]
    fn only_unknown_prod_actions_writes_no_section() {
        let prod = decode_prod_shortcuts(
            br#"{"someFutureAction":{"keyCode":16,"modifierFlagsRaw":1048576}}"#,
        );
        assert!(build_shortcuts_section(&prod).is_none());
    }
}
