//
//  SemanticVersion.swift
//  Nice
//
//  Dotted-integer version parser used by `ReleaseChecker` to decide
//  whether a GitHub release tag is newer than the running app. Strips a
//  leading "v" so a tag like "v0.1.5" compares equal to the app's
//  `CFBundleShortVersionString` "0.1.5".
//
//  Returns `nil` on anything that isn't purely dotted non-negative
//  integers — the caller treats "unparseable" the same as "no info" and
//  simply doesn't show the update pill.
//

import Foundation

struct SemanticVersion: Comparable, Hashable {
    let components: [Int]

    /// Parse `"0.1.5"` or `"v0.1.5"`. Trailing components missing from
    /// either side compare as 0, so `"0.1" == "0.1.0"`.
    init?(_ raw: String) {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.first == "v" || s.first == "V" {
            s.removeFirst()
        }
        guard !s.isEmpty else { return nil }
        var parts: [Int] = []
        for piece in s.split(separator: ".", omittingEmptySubsequences: false) {
            guard !piece.isEmpty, let n = Int(piece), n >= 0 else { return nil }
            parts.append(n)
        }
        self.components = parts
    }

    static func < (lhs: SemanticVersion, rhs: SemanticVersion) -> Bool {
        let count = max(lhs.components.count, rhs.components.count)
        for i in 0..<count {
            let a = i < lhs.components.count ? lhs.components[i] : 0
            let b = i < rhs.components.count ? rhs.components[i] : 0
            if a != b { return a < b }
        }
        return false
    }

    static func == (lhs: SemanticVersion, rhs: SemanticVersion) -> Bool {
        let count = max(lhs.components.count, rhs.components.count)
        for i in 0..<count {
            let a = i < lhs.components.count ? lhs.components[i] : 0
            let b = i < rhs.components.count ? rhs.components[i] : 0
            if a != b { return false }
        }
        return true
    }
}
