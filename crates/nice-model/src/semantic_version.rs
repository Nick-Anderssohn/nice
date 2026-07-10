//! `SemanticVersion` — ported from `Sources/Nice/State/SemanticVersion.swift`
//! (`SemanticVersion.swift:1-55`). The dotted-integer version parser
//! `ReleaseChecker` (crates/nice, a later R27 slice) uses to decide whether a
//! GitHub release tag is newer than the running app.
//!
//! Strips a single leading `v`/`V` so a tag like `v0.1.5` compares equal to
//! the app's `CFBundleShortVersionString` `0.1.5`. This is **not** full
//! semver: no prerelease/build-metadata handling, dotted non-negative
//! integers only. Anything else — a non-numeric component, an empty piece
//! from `1..3`, a negative number — makes the whole parse fail. The caller
//! treats "unparseable" the same as "no info" and simply doesn't show the
//! update pill (`ReleaseChecker.swift:147-158`, ported in a later slice).

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

/// A dotted-integer version (`"0.1.5"` → `[0, 1, 5]`). Trailing components
/// missing from either side of a comparison compare as `0`, so `0.1` and
/// `0.1.0` are equal (`SemanticVersion.swift:20-21,36-54`). `PartialEq`/`Eq`/
/// `Hash` all honor that padded equality (not raw-`Vec` equality) — mirroring
/// Swift's overridden `==` (`SemanticVersion.swift:46-54`), which compares the
/// same way `<` does rather than deriving structural equality.
#[derive(Debug, Clone)]
pub struct SemanticVersion {
    components: Vec<u64>,
}

impl SemanticVersion {
    /// Parse `"0.1.5"` or `"v0.1.5"`. Trims surrounding whitespace, strips
    /// ONE leading `v`/`V`, then splits on `.`: every piece must be a
    /// non-negative integer with no empty pieces, else the whole parse
    /// returns `None` (`SemanticVersion.swift:22-34`). So `1.a.3`, `1..3`,
    /// `beta`, `-1.0.0`, `""`, and `"v"` all parse to `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut s = raw.trim();
        if let Some(rest) = s.strip_prefix('v').or_else(|| s.strip_prefix('V')) {
            s = rest;
        }
        if s.is_empty() {
            return None;
        }
        let mut components = Vec::new();
        for piece in s.split('.') {
            if piece.is_empty() {
                return None;
            }
            let n: u64 = piece.parse().ok()?;
            components.push(n);
        }
        Some(SemanticVersion { components })
    }

    /// Component-wise integer compare, NOT lexicographic — `0.1.9 < 0.1.10`
    /// holds because component 3 compares `9 < 10` as integers, not as
    /// strings (`SemanticVersion.swift:36-44`). Missing trailing components
    /// on the shorter side pad with `0`.
    fn compare(&self, other: &Self) -> Ordering {
        let count = self.components.len().max(other.components.len());
        for i in 0..count {
            let a = self.components.get(i).copied().unwrap_or(0);
            let b = other.components.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }
        Ordering::Equal
    }

    /// Whether `latest_raw` is a strictly newer version than `current_raw`.
    /// Parses both; if EITHER fails to parse, returns `false` (no pill) —
    /// mirroring `ReleaseChecker.applyLatest`'s parse-failure-means-no-pill
    /// discipline (`ReleaseChecker.swift:147-158`).
    pub fn is_newer(latest_raw: &str, current_raw: &str) -> bool {
        match (Self::parse(latest_raw), Self::parse(current_raw)) {
            (Some(latest), Some(current)) => latest.compare(&current) == Ordering::Greater,
            _ => false,
        }
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.compare(other))
    }
}

impl Ord for SemanticVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare(other)
    }
}

impl PartialEq for SemanticVersion {
    fn eq(&self, other: &Self) -> bool {
        self.compare(other) == Ordering::Equal
    }
}

impl Eq for SemanticVersion {}

impl Hash for SemanticVersion {
    /// Hashes the trailing-zero-trimmed components so values that compare
    /// equal (`0.1` vs `0.1.0`) also hash equal, preserving the
    /// `Eq`/`Hash` contract under the padded-equality override above.
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut trimmed: &[u64] = &self.components;
        while trimmed.last() == Some(&0) {
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        trimmed.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported case-for-case from `SemanticVersionTests`
    // (`Tests/NiceUnitTests/ReleaseCheckerTests.swift:27-68`). Provenance
    // spot-check: Swift's `test_componentWiseCompare_notLexicographic`
    // (`:48-53`) asserts `0.1.9 < 0.1.10`, which only holds under an integer
    // compare — a naive string/lexicographic compare would put `0.1.10`
    // before `0.1.9` (`'1' < '9'`). This is the exact bug the component-wise
    // `Ord` impl above exists to avoid.

    #[test]
    fn parses_plain_dotted() {
        assert_eq!(
            SemanticVersion::parse("0.1.5").unwrap().components,
            vec![0, 1, 5]
        );
        assert_eq!(
            SemanticVersion::parse("10.20.30").unwrap().components,
            vec![10, 20, 30]
        );
    }

    #[test]
    fn strips_leading_v() {
        assert_eq!(
            SemanticVersion::parse("v0.1.5"),
            SemanticVersion::parse("0.1.5")
        );
        assert_eq!(
            SemanticVersion::parse("V2.0"),
            SemanticVersion::parse("2.0")
        );
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            SemanticVersion::parse("  v1.2.3  "),
            SemanticVersion::parse("1.2.3")
        );
    }

    #[test]
    fn missing_components_are_zero() {
        assert_eq!(SemanticVersion::parse("1"), SemanticVersion::parse("1.0.0"));
        assert_eq!(
            SemanticVersion::parse("1.2"),
            SemanticVersion::parse("1.2.0")
        );
    }

    /// Provenance: `SemanticVersionTests.test_componentWiseCompare_notLexicographic`
    /// (`ReleaseCheckerTests.swift:48-53`) — the case a lexicographic compare
    /// would get backwards.
    #[test]
    fn component_wise_compare_not_lexicographic_0_1_9_lt_0_1_10() {
        let nine = SemanticVersion::parse("0.1.9").unwrap();
        let ten = SemanticVersion::parse("0.1.10").unwrap();
        assert!(nine < ten, "0.1.9 must be less than 0.1.10");
        assert!(!(ten < nine));
    }

    #[test]
    fn equality_with_differing_lengths() {
        let a = SemanticVersion::parse("1.0").unwrap();
        let b = SemanticVersion::parse("1.0.0").unwrap();
        assert_eq!(a, b);
        assert!(!(a < b));
    }

    /// Provenance: `SemanticVersionTests.test_rejectsGarbage`
    /// (`ReleaseCheckerTests.swift:60-67`) — every case must parse to `None`.
    #[test]
    fn rejects_garbage() {
        assert!(SemanticVersion::parse("").is_none());
        assert!(SemanticVersion::parse("v").is_none());
        assert!(SemanticVersion::parse("1.a.3").is_none());
        assert!(SemanticVersion::parse("1..3").is_none());
        assert!(SemanticVersion::parse("beta").is_none());
        assert!(SemanticVersion::parse("-1.0.0").is_none());
    }

    // Not in the Swift suite, but directly exercises `is_newer` (the frozen
    // "Version compare" block, `.dev-cycle-orchestrator-plan.md`) which
    // `ReleaseChecker.applyLatest` will drive in a later slice.
    #[test]
    fn is_newer_true_when_latest_strictly_greater() {
        assert!(SemanticVersion::is_newer("v0.1.5", "0.1.4"));
        assert!(!SemanticVersion::is_newer("v0.1.5", "0.1.5"));
        assert!(!SemanticVersion::is_newer("v0.1.4", "0.1.5"));
    }

    #[test]
    fn is_newer_false_when_either_side_unparseable() {
        assert!(!SemanticVersion::is_newer("not-a-version", "0.1.4"));
        assert!(!SemanticVersion::is_newer("v0.1.5", "not-a-version"));
        assert!(!SemanticVersion::is_newer("", ""));
    }
}
