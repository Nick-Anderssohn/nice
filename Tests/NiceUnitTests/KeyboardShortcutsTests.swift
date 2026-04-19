//
//  KeyboardShortcutsTests.swift
//  NiceUnitTests
//
//  Unit tests for the binding model in Sources/Nice/State/KeyboardShortcuts.swift.
//
//  Each test creates an isolated `UserDefaults(suiteName:)` so persistence
//  state never leaks between tests. The `KeyboardShortcuts` initializer
//  takes a `defaults` parameter (mirroring `Tweaks`) which is what makes
//  this isolation possible.
//

import AppKit
import Carbon.HIToolbox
import Foundation
import XCTest
@testable import Nice

@MainActor
final class KeyboardShortcutsTests: XCTestCase {

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

    func test_defaults_areAppliedOnFirstRun() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        for action in ShortcutAction.allCases {
            XCTAssertEqual(
                shortcuts.binding(for: action),
                KeyboardShortcuts.defaults[action],
                "first-run binding for \(action) should equal default"
            )
            XCTAssertTrue(shortcuts.isAtDefault(action))
        }
    }

    func test_defaults_arePureWrap_noDualPurposeRightArrow() {
        // Sanity check: ⌘⌥→ is bound to .nextPane (pure wrap), not a
        // create-or-add action. Guards against the older dual-purpose
        // design accidentally creeping back in.
        let combo = KeyboardShortcuts.defaults[.nextPane]
        XCTAssertEqual(combo?.keyCode, UInt16(kVK_RightArrow))
        XCTAssertTrue(combo?.modifierFlags.contains(.command) ?? false)
        XCTAssertTrue(combo?.modifierFlags.contains(.option) ?? false)
    }

    func test_defaults_fontShortcuts_bindCmdPlusMinusZero() {
        let inc = KeyboardShortcuts.defaults[.increaseFontSize]
        XCTAssertEqual(inc?.keyCode, UInt16(kVK_ANSI_Equal))
        XCTAssertEqual(inc?.modifierFlags, [.command])

        let dec = KeyboardShortcuts.defaults[.decreaseFontSize]
        XCTAssertEqual(dec?.keyCode, UInt16(kVK_ANSI_Minus))
        XCTAssertEqual(dec?.modifierFlags, [.command])

        let reset = KeyboardShortcuts.defaults[.resetFontSizes]
        XCTAssertEqual(reset?.keyCode, UInt16(kVK_ANSI_0))
        XCTAssertEqual(reset?.modifierFlags, [.command])
    }

    // MARK: - Persistence

    func test_setBinding_persistsToDefaults() {
        let combo = KeyCombo(keyCode: UInt16(kVK_ANSI_J), modifierFlags: [.command, .shift])

        do {
            let shortcuts = KeyboardShortcuts(defaults: defaults)
            shortcuts.setBinding(combo, for: .nextSidebarTab)
        }

        // Recreate from the same defaults — the binding should survive.
        let reloaded = KeyboardShortcuts(defaults: defaults)
        XCTAssertEqual(reloaded.binding(for: .nextSidebarTab), combo)
    }

    func test_setBinding_nilClearsBinding() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        shortcuts.setBinding(nil, for: .toggleSidebar)
        XCTAssertNil(shortcuts.binding(for: .toggleSidebar))

        let reloaded = KeyboardShortcuts(defaults: defaults)
        XCTAssertNil(reloaded.binding(for: .toggleSidebar))
    }

    func test_resetToDefault_restoresDefault() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        let custom = KeyCombo(keyCode: UInt16(kVK_ANSI_X), modifierFlags: [.control])
        shortcuts.setBinding(custom, for: .toggleSidebar)
        XCTAssertEqual(shortcuts.binding(for: .toggleSidebar), custom)
        XCTAssertFalse(shortcuts.isAtDefault(.toggleSidebar))

        shortcuts.resetToDefault(.toggleSidebar)
        XCTAssertEqual(shortcuts.binding(for: .toggleSidebar), KeyboardShortcuts.defaults[.toggleSidebar])
        XCTAssertTrue(shortcuts.isAtDefault(.toggleSidebar))
    }

    func test_load_missingKey_returnsDefaults() {
        XCTAssertNil(defaults.data(forKey: KeyboardShortcuts.defaultsKey))
        let bindings = KeyboardShortcuts.load(from: defaults)
        XCTAssertEqual(bindings, KeyboardShortcuts.defaults)
    }

    func test_load_corruptBlob_fallsBackToDefaults() {
        defaults.set(Data([0xff, 0x00, 0xab]), forKey: KeyboardShortcuts.defaultsKey)
        let bindings = KeyboardShortcuts.load(from: defaults)
        XCTAssertEqual(bindings, KeyboardShortcuts.defaults)
    }

    func test_load_partialBlob_keepsMissingActionsUnbound() throws {
        // Write a blob containing only one action — others should load
        // as unbound (`nil`). This is what makes explicit
        // `setBinding(nil, ...)` clears survive a relaunch.
        let oneAction: [String: KeyCombo] = [
            ShortcutAction.toggleSidebar.rawValue:
                KeyCombo(keyCode: UInt16(kVK_ANSI_X), modifierFlags: [.command])
        ]
        let data = try JSONEncoder().encode(oneAction)
        defaults.set(data, forKey: KeyboardShortcuts.defaultsKey)

        let bindings = KeyboardShortcuts.load(from: defaults)
        for action in ShortcutAction.allCases where action != .toggleSidebar {
            XCTAssertNil(bindings[action],
                         "missing action \(action) should load as unbound")
        }
        XCTAssertEqual(bindings[.toggleSidebar]?.keyCode, UInt16(kVK_ANSI_X))
    }

    // MARK: - Lookup

    func test_actionMatching_returnsActionForBoundCombo() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        let action = shortcuts.actionMatching(
            keyCode: UInt16(kVK_ANSI_T),
            modifiers: [.command]
        )
        XCTAssertEqual(action, .newTerminalPane)
    }

    func test_actionMatching_returnsNilForUnboundCombo() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        let action = shortcuts.actionMatching(
            keyCode: UInt16(kVK_ANSI_Z),
            modifiers: [.command, .control]
        )
        XCTAssertNil(action)
    }

    func test_actionMatching_ignoresExtraneousFlags() {
        // CapsLock / numeric keypad bits should never affect matching.
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        let action = shortcuts.actionMatching(
            keyCode: UInt16(kVK_ANSI_T),
            modifiers: [.command, .capsLock, .numericPad]
        )
        XCTAssertEqual(action, .newTerminalPane)
    }

    // MARK: - Conflict detection

    func test_conflictingAction_returnsExistingActionWhenComboReused() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        // ⌘T is the default for .newTerminalPane — try to assign it to
        // .toggleSidebar and the conflict check should surface
        // .newTerminalPane.
        let combo = KeyCombo(keyCode: UInt16(kVK_ANSI_T), modifierFlags: [.command])
        let conflict = shortcuts.conflictingAction(for: combo, excluding: .toggleSidebar)
        XCTAssertEqual(conflict, .newTerminalPane)
    }

    func test_conflictingAction_returnsNilWhenComboFree() {
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        let combo = KeyCombo(keyCode: UInt16(kVK_ANSI_Q), modifierFlags: [.command, .shift])
        XCTAssertNil(shortcuts.conflictingAction(for: combo, excluding: .toggleSidebar))
    }

    func test_conflictingAction_excludesGivenAction() {
        // Re-binding an action to its own current combo should not flag
        // a self-conflict.
        let shortcuts = KeyboardShortcuts(defaults: defaults)
        let existing = shortcuts.binding(for: .newTerminalPane)!
        XCTAssertNil(shortcuts.conflictingAction(for: existing, excluding: .newTerminalPane))
    }

    // MARK: - KeyCombo

    func test_keyCombo_displayPills_orderedByHIG() {
        // Apple HIG modifier order is ⌃⌥⇧⌘, key glyph last.
        let combo = KeyCombo(
            keyCode: UInt16(kVK_ANSI_T),
            modifierFlags: [.control, .option, .shift, .command]
        )
        XCTAssertEqual(combo.displayPills, ["⌃", "⌥", "⇧", "⌘", "T"])
    }

    func test_keyCombo_displayPills_arrowKey() {
        let combo = KeyCombo(
            keyCode: UInt16(kVK_RightArrow),
            modifierFlags: [.command, .option]
        )
        XCTAssertEqual(combo.displayPills, ["⌥", "⌘", "→"])
    }

    func test_keyCombo_codable_roundtripsViaJSON() throws {
        let combo = KeyCombo(
            keyCode: UInt16(kVK_ANSI_B),
            modifierFlags: [.command, .shift]
        )
        let data = try JSONEncoder().encode(combo)
        let decoded = try JSONDecoder().decode(KeyCombo.self, from: data)
        XCTAssertEqual(decoded, combo)
    }

    func test_keyCombo_init_masksExtraneousModifierFlags() {
        // CapsLock should be stripped at construction so persisted bindings
        // never accidentally carry it.
        let combo = KeyCombo(
            keyCode: UInt16(kVK_ANSI_T),
            modifierFlags: [.command, .capsLock]
        )
        XCTAssertFalse(combo.modifierFlags.contains(.capsLock))
        XCTAssertTrue(combo.modifierFlags.contains(.command))
    }
}
