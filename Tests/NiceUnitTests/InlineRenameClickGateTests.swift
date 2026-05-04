//
//  InlineRenameClickGateTests.swift
//  NiceUnitTests
//
//  Boundary tests for the click-to-rename time gate shared by the
//  sidebar TabRow and the pane pill. Pure-Swift seam carved out
//  precisely so the `>=` comparator boundary is unit-tested rather
//  than relying on UITest timing (which is inherently flaky for the
//  ~0.5 s double-click interval).
//

import Foundation
import XCTest
@testable import Nice

final class InlineRenameClickGateTests: XCTestCase {

    private let interval: TimeInterval = 0.5

    func test_nilActivatedAt_disallowsEdit() {
        XCTAssertFalse(
            InlineRenameClickGate.canBeginEdit(
                activatedAt: nil,
                now: Date(),
                doubleClickInterval: interval
            ),
            "A row that has not been activated must not allow rename."
        )
    }

    func test_freshActivation_disallowsEdit() {
        let now = Date()
        XCTAssertFalse(
            InlineRenameClickGate.canBeginEdit(
                activatedAt: now,
                now: now,
                doubleClickInterval: interval
            ),
            "Same-instant activation must not enter edit (catches the 'click that selects also renames' bug)."
        )
    }

    func test_justUnderInterval_disallowsEdit() {
        let activatedAt = Date()
        let now = activatedAt.addingTimeInterval(interval - 0.001)
        XCTAssertFalse(
            InlineRenameClickGate.canBeginEdit(
                activatedAt: activatedAt,
                now: now,
                doubleClickInterval: interval
            ),
            "Less than the double-click interval must not enter edit."
        )
    }

    func test_exactlyAtInterval_allowsEdit() {
        let activatedAt = Date()
        let now = activatedAt.addingTimeInterval(interval)
        XCTAssertTrue(
            InlineRenameClickGate.canBeginEdit(
                activatedAt: activatedAt,
                now: now,
                doubleClickInterval: interval
            ),
            "The boundary uses `>=`, so exactly the interval must allow edit."
        )
    }

    func test_pastInterval_allowsEdit() {
        let activatedAt = Date()
        let now = activatedAt.addingTimeInterval(2 * interval)
        XCTAssertTrue(
            InlineRenameClickGate.canBeginEdit(
                activatedAt: activatedAt,
                now: now,
                doubleClickInterval: interval
            )
        )
    }
}
