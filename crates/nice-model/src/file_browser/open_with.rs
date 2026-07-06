//! The pure "Open With ▸" ordering/dedup/synthesized-default function, ported
//! from `Sources/Nice/State/FileOperations/OpenWithProvider.swift`. Gpui-free
//! and Launch-Services-free: it orders a list of candidate apps against an
//! injectable lookup shape ([`OpenWithLookups`]) so it is deterministic under
//! `cargo test`.
//!
//! The R19 `WorkspaceOps` slice wires the **production** lookups (objc2
//! `NSWorkspace` enumeration + `NSBundle` display names, in `platform.rs`)
//! into this same function — it is not re-implemented there. `OpenWithEntry`
//! drops the Swift `icon` field entirely (fetched but never rendered — the
//! YAGNI cut in the R19 DO-NOT-PORT list).
//!
//! Ordering contract (`OpenWithProvider.swift:75-119`):
//! * the user's default app (if any) appears **first**, `is_default = true`;
//! * remaining apps are alphabetized case-insensitively by display name;
//! * duplicates by standardized app path are removed (one bundle reachable via
//!   multiple symlinks doesn't double up);
//! * if the default app isn't in the enumeration (a Launch Services edge case)
//!   its entry is **synthesized** at index 0.

/// One "Open With ▸" menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWithEntry {
    /// Standardized absolute path of the application bundle.
    pub app_path: String,
    /// The display name (CFBundleDisplayName → CFBundleName → filename,
    /// resolved by the lookup).
    pub display_name: String,
    /// Whether this is the user's default app for the target — rendered first,
    /// labeled "<Name> (default)".
    pub is_default: bool,
}

/// Injected lookups the ordering function reads. Production wires these to
/// `NSWorkspace` / `NSBundle` in `platform.rs`; tests pass deterministic
/// fixtures. Paths are already **standardized** by the caller (the production
/// impl standardizes; tests pass canonical strings), so `entries` compares them
/// verbatim.
pub struct OpenWithLookups<'a> {
    /// All apps that can open the target, in Launch Services order.
    pub all_apps: Vec<String>,
    /// The user's default app for the target, if any.
    pub default_app: Option<String>,
    /// Display name for an app path.
    pub display_name: &'a dyn Fn(&str) -> String,
}

/// Build the ordered "Open With ▸" entries for a target from `lookups`, per the
/// ordering contract in the module docs.
pub fn entries(lookups: &OpenWithLookups) -> Vec<OpenWithEntry> {
    let mut seen: Vec<String> = Vec::new();
    let mut default_entry: Option<OpenWithEntry> = None;
    let mut others: Vec<OpenWithEntry> = Vec::new();

    for app in &lookups.all_apps {
        if seen.iter().any(|s| s == app) {
            continue;
        }
        seen.push(app.clone());
        let is_default = lookups.default_app.as_deref() == Some(app.as_str());
        let entry = OpenWithEntry {
            app_path: app.clone(),
            display_name: (lookups.display_name)(app),
            is_default,
        };
        if is_default {
            default_entry = Some(entry);
        } else {
            others.push(entry);
        }
    }

    // The default app sometimes isn't in the enumeration on weird
    // configurations — synthesize its entry.
    if default_entry.is_none() {
        if let Some(default_app) = &lookups.default_app {
            if !seen.iter().any(|s| s == default_app) {
                default_entry = Some(OpenWithEntry {
                    app_path: default_app.clone(),
                    display_name: (lookups.display_name)(default_app),
                    is_default: true,
                });
            }
        }
    }

    others.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });

    match default_entry {
        Some(d) => std::iter::once(d).chain(others).collect(),
        None => others,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XCODE: &str = "/Applications/Xcode.app";
    const TEXTEDIT: &str = "/Applications/TextEdit.app";
    const BBEDIT: &str = "/Applications/BBEdit.app";

    /// Display name = the bundle's base filename without extension, matching the
    /// Swift tests' `url.deletingPathExtension().lastPathComponent` stub.
    fn base_name(path: &str) -> String {
        std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn lookups<'a>(all: &[&str], default: Option<&str>) -> OpenWithLookups<'a> {
        OpenWithLookups {
            all_apps: all.iter().map(|s| s.to_string()).collect(),
            default_app: default.map(|s| s.to_string()),
            display_name: &|p| base_name(p),
        }
    }

    /// `OpenWithProviderTests.test_entries_defaultAppFirst`
    #[test]
    fn entries_default_app_first() {
        let e = entries(&lookups(&[TEXTEDIT, XCODE, BBEDIT], Some(XCODE)));
        assert_eq!(e.first().unwrap().app_path, XCODE);
        assert!(e.first().unwrap().is_default);
    }

    /// `OpenWithProviderTests.test_entries_alphabetisedAfterDefault`
    #[test]
    fn entries_alphabetised_after_default() {
        let e = entries(&lookups(&[TEXTEDIT, XCODE, BBEDIT], Some(XCODE)));
        let rest: Vec<&str> = e.iter().skip(1).map(|x| x.app_path.as_str()).collect();
        assert_eq!(rest, [BBEDIT, TEXTEDIT]);
    }

    /// `OpenWithProviderTests.test_entries_dedupesByBundlePath`
    #[test]
    fn entries_dedupes_by_bundle_path() {
        let e = entries(&lookups(&[XCODE, XCODE, TEXTEDIT], None));
        assert_eq!(e.len(), 2);
    }

    /// `OpenWithProviderTests.test_entries_emptyForUnknownType_returnsEmpty`
    #[test]
    fn entries_empty_for_unknown_type_returns_empty() {
        let e = entries(&lookups(&[], None));
        assert!(e.is_empty());
    }

    /// `OpenWithProviderTests.test_entries_singleAppThatIsDefault_returnsOneEntryWithIsDefaultTrue`
    #[test]
    fn entries_single_app_that_is_default_returns_one_entry() {
        let e = entries(&lookups(&[XCODE], Some(XCODE)));
        assert_eq!(e.len(), 1, "default-and-only app must not duplicate");
        assert_eq!(e[0].app_path, XCODE);
        assert!(e[0].is_default);
    }

    /// `OpenWithProviderTests.test_entries_dedupesByBundlePath_evenWhenDefaultAppDuplicated`
    #[test]
    fn entries_dedupes_by_bundle_path_even_when_default_duplicated() {
        let e = entries(&lookups(&[XCODE, XCODE, TEXTEDIT], Some(XCODE)));
        assert_eq!(e.len(), 2);
        assert!(e[0].is_default);
        assert_eq!(e[1].app_path, TEXTEDIT);
    }

    /// `OpenWithProviderTests.test_entries_defaultAppNotInList_isStillIncluded`
    #[test]
    fn entries_default_app_not_in_list_is_still_included() {
        let e = entries(&lookups(&[TEXTEDIT], Some(XCODE)));
        assert_eq!(e.first().unwrap().app_path, XCODE);
        assert!(e.first().unwrap().is_default);
        let rest: Vec<&str> = e.iter().skip(1).map(|x| x.app_path.as_str()).collect();
        assert_eq!(rest, [TEXTEDIT]);
    }
}
