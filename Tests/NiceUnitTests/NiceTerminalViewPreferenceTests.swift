//
//  NiceTerminalViewPreferenceTests.swift
//  NiceUnitTests
//
//  Tests for `NiceTerminalView.applySmoothScrollPreference()`, which
//  reads the `smoothScrollPreference` closure and writes the result to
//  `smoothScrollingEnabled`.
//
//  Note: `applyHardwareAccelerationPreference()` is not tested here
//  because it guards on `window != nil` and Metal is unavailable in a
//  headless test host. `applySmoothScrollPreference()` has no such gate
//  and is safe to exercise against a zero-frame, detached view.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class NiceTerminalViewPreferenceTests: XCTestCase {

    // MARK: - applySmoothScrollPreference

    func test_applySmoothScrollPreference_withFalseProvider_setsFalse() {
        let view = NiceTerminalView(frame: .zero)
        view.smoothScrollPreference = { false }

        view.applySmoothScrollPreference()

        XCTAssertFalse(view.smoothScrollingEnabled,
                       "smoothScrollingEnabled must be false when the provider returns false")
    }

    func test_applySmoothScrollPreference_withTrueProvider_setsTrue() {
        let view = NiceTerminalView(frame: .zero)
        // Ensure we're not just reading a pre-set true value:
        view.smoothScrollPreference = { false }
        view.applySmoothScrollPreference()
        XCTAssertFalse(view.smoothScrollingEnabled, "precondition: forced to false")

        view.smoothScrollPreference = { true }
        view.applySmoothScrollPreference()

        XCTAssertTrue(view.smoothScrollingEnabled,
                      "smoothScrollingEnabled must be true when the provider returns true")
    }

    func test_applySmoothScrollPreference_withNilProvider_defaultsToFalse() {
        let view = NiceTerminalView(frame: .zero)
        // Ensure provider is nil (the default).
        view.smoothScrollPreference = nil

        view.applySmoothScrollPreference()

        XCTAssertFalse(view.smoothScrollingEnabled,
                       "nil provider must use the ?? false default (smooth scrolling is opt-in)")
    }
}
