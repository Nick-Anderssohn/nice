//
//  Typography.swift
//  Nice
//
//  System font helpers for Phase 1. JetBrains Mono bundling is deferred
//  to a later phase; for now we use the OS monospaced system font.
//

import SwiftUI

public extension Font {
    static let niceUI = Font.system(.body)
    static let niceMono = Font.system(.body, design: .monospaced)
    static let niceMonoSmall = Font.system(.caption, design: .monospaced)
}
