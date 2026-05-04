//
//  InlineRenameClickGate.swift
//  Nice
//
//  Pure helper for the "click an active row's title to enter rename
//  mode" pattern shared by the sidebar `TabRow` and the toolbar pane
//  pill. The rule: the row must be active, AND at least
//  `doubleClickInterval` seconds must have elapsed since it became
//  active — so the same click that selects a row can't also start a
//  rename. Extracted so the boundary (off-by-one risk against
//  `>=` vs `>`) is unit-tested without driving the real UI.
//

import Foundation

enum InlineRenameClickGate {
    static func canBeginEdit(
        activatedAt: Date?,
        now: Date,
        doubleClickInterval: TimeInterval
    ) -> Bool {
        guard let activatedAt else { return false }
        return now.timeIntervalSince(activatedAt) >= doubleClickInterval
    }
}
