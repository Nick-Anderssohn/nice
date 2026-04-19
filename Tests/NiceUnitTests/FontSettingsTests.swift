//
//  FontSettingsTests.swift
//  NiceUnitTests
//
//  Unit tests for the font size state model in Sources/Nice/State/FontSettings.swift.
//
//  Each test uses an isolated `UserDefaults(suiteName:)` so persistence
//  state never leaks between tests.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FontSettingsTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "test-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        defaults = nil
        suiteName = nil
        super.tearDown()
    }

    // MARK: - Defaults

    func test_freshInstall_usesDefaults() {
        let fs = FontSettings(defaults: defaults)
        XCTAssertEqual(fs.terminalFontSize, FontSettings.defaultSize)
        XCTAssertEqual(fs.sidebarFontSize,  FontSettings.defaultSize)
    }

    // MARK: - Persistence

    func test_persistence_roundTrips() {
        do {
            let fs = FontSettings(defaults: defaults)
            fs.terminalFontSize = 18
            fs.sidebarFontSize  = 14
        }
        let reloaded = FontSettings(defaults: defaults)
        XCTAssertEqual(reloaded.terminalFontSize, 18)
        XCTAssertEqual(reloaded.sidebarFontSize,  14)
    }

    func test_loadClamps_outOfRangeValuesStored() {
        defaults.set(4.0,  forKey: FontSettings.terminalKey)
        defaults.set(99.0, forKey: FontSettings.sidebarKey)

        let fs = FontSettings(defaults: defaults)
        XCTAssertEqual(fs.terminalFontSize, FontSettings.minSize)
        XCTAssertEqual(fs.sidebarFontSize,  FontSettings.maxSize)
    }

    // MARK: - sidebarSize proportional scaling

    func test_sidebarSize_atDefault_isIdentityAt12() {
        let fs = FontSettings(defaults: defaults)
        XCTAssertEqual(fs.sidebarSize(12), 12)
        XCTAssertEqual(fs.sidebarSize(11), 11)
        XCTAssertEqual(fs.sidebarSize(10), 10)
        XCTAssertEqual(fs.sidebarSize(14), 14)
    }

    func test_sidebarSize_scalesProportionally() {
        let fs = FontSettings(defaults: defaults)

        fs.sidebarFontSize = 24
        XCTAssertEqual(fs.sidebarSize(12), 24) // 24*12/12 = 24
        XCTAssertEqual(fs.sidebarSize(11), 22) // round(22.0) = 22
        XCTAssertEqual(fs.sidebarSize(10), 20)

        fs.sidebarFontSize = 8
        XCTAssertEqual(fs.sidebarSize(12), 8)
        // round(8*11/12) = round(7.33) = 7
        XCTAssertEqual(fs.sidebarSize(11), 7)
        // round(8*10/12) = round(6.67) = 7
        XCTAssertEqual(fs.sidebarSize(10), 7)
    }

    func test_sidebarSize_neverReturnsZero() {
        let fs = FontSettings(defaults: defaults)
        fs.sidebarFontSize = 8
        // Even for a tiny defaultPt, the floor is 1pt.
        XCTAssertGreaterThanOrEqual(fs.sidebarSize(0.1), 1)
    }

    // MARK: - zoom(by:) preserves ratio

    func test_zoom_withEqualSizes_movesBothTogether() {
        let fs = FontSettings(defaults: defaults)
        // Defaults are 12/12 — ratio 1:1.
        fs.zoom(by: +1)
        XCTAssertEqual(fs.terminalFontSize, 13)
        XCTAssertEqual(fs.sidebarFontSize,  13)

        fs.zoom(by: -1)
        XCTAssertEqual(fs.terminalFontSize, 12)
        XCTAssertEqual(fs.sidebarFontSize,  12)
    }

    func test_zoom_preservesRatioWithinRounding() {
        let fs = FontSettings(defaults: defaults)
        fs.terminalFontSize = 18
        fs.sidebarFontSize  = 12
        // Ratio 18:12 = 1.5. Zoom up by 1.
        fs.zoom(by: +1)
        XCTAssertEqual(fs.terminalFontSize, 19)
        // round(12 * 19 / 18) = round(12.667) = 13
        XCTAssertEqual(fs.sidebarFontSize, 13)
    }

    func test_zoom_clampsAtMax_butLeavesSidebarAtExisting() {
        let fs = FontSettings(defaults: defaults)
        fs.terminalFontSize = FontSettings.maxSize
        fs.sidebarFontSize  = 12
        fs.zoom(by: +1)
        // Terminal already at max; zoom is a no-op (no new terminal value
        // so sidebar is untouched too).
        XCTAssertEqual(fs.terminalFontSize, FontSettings.maxSize)
        XCTAssertEqual(fs.sidebarFontSize, 12)
    }

    func test_zoom_clampsAtMin() {
        let fs = FontSettings(defaults: defaults)
        fs.terminalFontSize = FontSettings.minSize
        fs.sidebarFontSize  = FontSettings.minSize
        fs.zoom(by: -1)
        XCTAssertEqual(fs.terminalFontSize, FontSettings.minSize)
        XCTAssertEqual(fs.sidebarFontSize,  FontSettings.minSize)
    }

    // MARK: - resetToDefaults

    func test_resetToDefaults_snapsBothTo12() {
        let fs = FontSettings(defaults: defaults)
        fs.terminalFontSize = 20
        fs.sidebarFontSize  = 8
        fs.resetToDefaults()
        XCTAssertEqual(fs.terminalFontSize, FontSettings.defaultSize)
        XCTAssertEqual(fs.sidebarFontSize,  FontSettings.defaultSize)
    }

    func test_resetToDefaults_persists() {
        do {
            let fs = FontSettings(defaults: defaults)
            fs.terminalFontSize = 20
            fs.resetToDefaults()
        }
        let reloaded = FontSettings(defaults: defaults)
        XCTAssertEqual(reloaded.terminalFontSize, FontSettings.defaultSize)
        XCTAssertEqual(reloaded.sidebarFontSize,  FontSettings.defaultSize)
    }
}
