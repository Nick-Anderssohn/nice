//! Sort preferences for the sidebar's file browser — the pure value type,
//! ported from `Sources/Nice/State/FileBrowserSortSettings.swift` (+ the
//! `FileBrowserSortCriterion` enum it re-exports from
//! `FileBrowserListing.swift`).
//!
//! Swift stores these in `UserDefaults` behind an `@Observable` class; the R19
//! rewrite splits that in two. The **value type** — criterion + direction, the
//! defaulting rules, and the raw-string parse used to hydrate from storage —
//! lives here in `nice-model`, gpui-free and disk-free. The **file store**
//! (`ui_settings.json` under `Nice RS Dev/`, atomic-write, unknown-key
//! preservation, process `Global`) lands in `crates/nice` and reuses this
//! value type as its schema surface.
//!
//! Two rules the Swift init pins and this port keeps:
//!
//! * **Unknown / missing criterion falls back to [`FileBrowserSortCriterion::Name`]**
//!   — a raw string left over from a removed criterion in a future schema must
//!   not crash or carry a null.
//! * **A missing direction defaults to ascending (`true`)**, never to a
//!   bool-decode's implicit `false` (which would silently flip a fresh install
//!   to descending). Direction is **independent** of criterion: switching the
//!   criterion never flips the direction, and vice versa (each was its own
//!   `UserDefaults` key in Swift).
//!
//! Folders-first is deliberately **not** a setting — the browser always groups
//! directories above files regardless of criterion; sort applies within each
//! bucket (see [`crate::file_browser::listing`]).

use serde::{Deserialize, Serialize};

/// Sort key applied within the dirs / files buckets of a directory listing.
///
/// The serde representation is **snake_case** (`"name"` / `"date_modified"`) —
/// the `ui_settings.json` schema the F2 store writes (a documented divergence
/// from the Swift enum's camelCase `rawValue`, which never leaves the Swift
/// build). Ported from `FileBrowserListing.swift:27-35`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileBrowserSortCriterion {
    /// Case-insensitive lexicographic on the entry's last path component. The
    /// default for fresh installs.
    Name,
    /// Filesystem modification time. "What did I touch most recently."
    DateModified,
}

impl FileBrowserSortCriterion {
    /// The stored raw string (matches the serde representation).
    pub fn as_raw(self) -> &'static str {
        match self {
            FileBrowserSortCriterion::Name => "name",
            FileBrowserSortCriterion::DateModified => "date_modified",
        }
    }

    /// Parse a stored raw string, mirroring the Swift `Criterion(rawValue:)`
    /// lookup. Unknown strings return `None` so [`FileBrowserSortSettings`]'s
    /// hydration can apply the name fallback.
    pub fn from_raw(raw: &str) -> Option<Self> {
        match raw {
            "name" => Some(FileBrowserSortCriterion::Name),
            "date_modified" => Some(FileBrowserSortCriterion::DateModified),
            _ => None,
        }
    }
}

impl Default for FileBrowserSortCriterion {
    fn default() -> Self {
        FileBrowserSortCriterion::Name
    }
}

/// Process-wide file-browser sort preference: which criterion and which
/// direction. `ascending == true` means A→Z for names and oldest-first for
/// dates. Ported from `FileBrowserSortSettings.swift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBrowserSortSettings {
    pub criterion: FileBrowserSortCriterion,
    /// `true` = ascending. Independent of `criterion` — the user toggles it
    /// explicitly, so switching criterion does not silently flip it.
    pub ascending: bool,
}

impl Default for FileBrowserSortSettings {
    /// Fresh install: name-ascending (`FileBrowserSortSettings.swift:52-63`
    /// defaults).
    fn default() -> Self {
        Self {
            criterion: FileBrowserSortCriterion::Name,
            ascending: true,
        }
    }
}

impl FileBrowserSortSettings {
    /// Hydrate from stored raw values, applying the Swift init's defaulting:
    /// an unknown / absent criterion falls back to `Name`; an absent direction
    /// defaults to `true` (ascending). This is the pure kernel the F2 file
    /// store calls after decoding `ui_settings.json`
    /// (`FileBrowserSortSettings.swift:52-63`).
    pub fn from_stored(criterion_raw: Option<&str>, ascending: Option<bool>) -> Self {
        Self {
            criterion: criterion_raw
                .and_then(FileBrowserSortCriterion::from_raw)
                .unwrap_or(FileBrowserSortCriterion::Name),
            ascending: ascending.unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - Defaults
    // `FileBrowserSortSettingsTests.test_freshInstall_defaultsToNameAscending`

    #[test]
    fn fresh_install_defaults_to_name_ascending() {
        // Both the value-type `Default` and the empty-storage hydration must
        // land on name / ascending so an upgrade from a pre-sort version reads
        // identically on first launch.
        let d = FileBrowserSortSettings::default();
        assert_eq!(d.criterion, FileBrowserSortCriterion::Name);
        assert!(d.ascending);

        let hydrated = FileBrowserSortSettings::from_stored(None, None);
        assert_eq!(hydrated.criterion, FileBrowserSortCriterion::Name);
        assert!(hydrated.ascending);
    }

    // MARK: - Persistence (round-trip through the stored raw representation)
    // `FileBrowserSortSettingsTests.test_criterion_persists`

    #[test]
    fn criterion_persists() {
        let s = FileBrowserSortSettings {
            criterion: FileBrowserSortCriterion::DateModified,
            ..Default::default()
        };
        let reloaded = FileBrowserSortSettings::from_stored(Some(s.criterion.as_raw()), Some(s.ascending));
        assert_eq!(reloaded.criterion, FileBrowserSortCriterion::DateModified);
    }

    // `FileBrowserSortSettingsTests.test_ascending_persists`
    #[test]
    fn ascending_persists() {
        let s = FileBrowserSortSettings {
            ascending: false,
            ..Default::default()
        };
        let reloaded = FileBrowserSortSettings::from_stored(Some(s.criterion.as_raw()), Some(s.ascending));
        assert!(!reloaded.ascending);
    }

    // MARK: - Independence
    // `FileBrowserSortSettingsTests.test_directionFlip_doesNotResetCriterion`

    #[test]
    fn direction_flip_does_not_reset_criterion() {
        // Each knob is its own stored field; flipping direction must not
        // clobber the criterion choice, on the live value or a reload.
        let s = FileBrowserSortSettings {
            criterion: FileBrowserSortCriterion::DateModified,
            ascending: false,
        };
        assert_eq!(s.criterion, FileBrowserSortCriterion::DateModified);
        assert!(!s.ascending);

        let reloaded = FileBrowserSortSettings::from_stored(Some(s.criterion.as_raw()), Some(s.ascending));
        assert_eq!(reloaded.criterion, FileBrowserSortCriterion::DateModified);
        assert!(!reloaded.ascending);
    }

    // MARK: - Fallbacks
    // `FileBrowserSortSettingsTests.test_unknownStoredCriterion_fallsBackToName`

    #[test]
    fn unknown_stored_criterion_falls_back_to_name() {
        // A raw string left over from a removed criterion in a future version
        // must fall back to Name rather than crash or carry a null.
        let s = FileBrowserSortSettings::from_stored(Some("size"), Some(true));
        assert_eq!(s.criterion, FileBrowserSortCriterion::Name);
    }

    // `FileBrowserSortSettingsTests.test_missingAscendingKey_defaultsToTrue`
    #[test]
    fn missing_ascending_key_defaults_to_true() {
        // An unset direction must default to true, not to a bool-decode's
        // implicit false — otherwise a criterion-only store silently flips a
        // fresh install to descending.
        let s = FileBrowserSortSettings::from_stored(Some("date_modified"), None);
        assert!(s.ascending);
        assert_eq!(s.criterion, FileBrowserSortCriterion::DateModified);
    }

    /// Guards the snake_case JSON contract the F2 store shares with R21/R23.
    #[test]
    fn criterion_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&FileBrowserSortCriterion::DateModified).unwrap(),
            "\"date_modified\""
        );
        assert_eq!(
            serde_json::from_str::<FileBrowserSortCriterion>("\"name\"").unwrap(),
            FileBrowserSortCriterion::Name
        );
    }
}
