//
//  TweaksEditorsTests.swift
//  NiceUnitTests
//
//  Coverage for the editor-command storage on `Tweaks`:
//    • round-trip persistence of `editorCommands` and
//      `extensionEditorMap` through `UserDefaults`,
//    • `normalizeExtension` collapses leading-dot / case variants,
//    • `removeEditor` cascades to drop orphan extension mappings,
//    • `editor(forExtension:)` self-heals when a mapping points at a
//      missing UUID (corrupted Defaults case).
//

import Foundation
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class TweaksEditorsTests: XCTestCase {

    // MARK: - normalizeExtension

    func test_normalizeExtension_dropsLeadingDot() {
        XCTAssertEqual(Tweaks.normalizeExtension(".md"), "md")
    }

    func test_normalizeExtension_lowercases() {
        XCTAssertEqual(Tweaks.normalizeExtension("MD"), "md")
        XCTAssertEqual(Tweaks.normalizeExtension(".SWIFT"), "swift")
    }

    func test_normalizeExtension_emptyStringStaysEmpty() {
        XCTAssertEqual(Tweaks.normalizeExtension(""), "")
    }

    // MARK: - mutators

    func test_addEditor_appendsToList() {
        let tweaks = makeTweaks()
        let id = UUID()
        tweaks.addEditor(EditorCommand(id: id, name: "Vim", command: "vim"))
        XCTAssertEqual(tweaks.editorCommands.count, 1)
        XCTAssertEqual(tweaks.editor(for: id)?.name, "Vim")
    }

    func test_updateEditor_changesNameAndCommand() {
        let tweaks = makeTweaks()
        let id = UUID()
        tweaks.addEditor(EditorCommand(id: id, name: "Vim", command: "vim"))
        tweaks.updateEditor(id: id, name: "Neovim", command: "nvim")
        XCTAssertEqual(tweaks.editor(for: id)?.name, "Neovim")
        XCTAssertEqual(tweaks.editor(for: id)?.command, "nvim")
    }

    func test_removeEditor_dropsMatchingMappings() {
        // Critical invariant: removing an editor must clean up any
        // extension that pointed at it. Otherwise double-click on
        // those extensions silently does nothing — the menu/pane path
        // can't recover from a dangling UUID.
        let tweaks = makeTweaks()
        let vimId = UUID()
        let glowId = UUID()
        tweaks.addEditor(EditorCommand(id: vimId, name: "Vim", command: "vim"))
        tweaks.addEditor(EditorCommand(id: glowId, name: "Glow", command: "glow"))
        tweaks.setMapping(extension: "swift", editorId: vimId)
        tweaks.setMapping(extension: ".md",   editorId: glowId)
        tweaks.setMapping(extension: "rs",    editorId: vimId)

        tweaks.removeEditor(id: vimId)

        XCTAssertNil(tweaks.editor(for: vimId))
        XCTAssertNil(tweaks.extensionEditorMap["swift"])
        XCTAssertNil(tweaks.extensionEditorMap["rs"])
        // Glow's mapping is unaffected.
        XCTAssertEqual(tweaks.extensionEditorMap["md"], glowId)
    }

    func test_setMapping_normalisesExtension() {
        let tweaks = makeTweaks()
        let id = UUID()
        tweaks.addEditor(EditorCommand(id: id, name: "Vim", command: "vim"))
        tweaks.setMapping(extension: ".SWIFT", editorId: id)
        // Stored under the normalised key — last-write-wins collapses
        // case variants so the user can't accidentally have ".SWIFT"
        // and "swift" both routing to different editors.
        XCTAssertEqual(tweaks.extensionEditorMap["swift"], id)
    }

    func test_editorForExtension_normalisesLookup() {
        let tweaks = makeTweaks()
        let id = UUID()
        tweaks.addEditor(EditorCommand(id: id, name: "Vim", command: "vim"))
        tweaks.setMapping(extension: "md", editorId: id)
        XCTAssertEqual(tweaks.editor(forExtension: ".MD")?.id, id)
        XCTAssertEqual(tweaks.editor(forExtension: "MD")?.id, id)
        XCTAssertEqual(tweaks.editor(forExtension: ".md")?.id, id)
    }

    func test_editorForExtension_returnsNilForUnmapped() {
        let tweaks = makeTweaks()
        XCTAssertNil(tweaks.editor(forExtension: "txt"))
    }

    func test_editorForExtension_selfHealsOnMissingUUID() {
        // Simulate corrupted persistence: a mapping points at a UUID
        // that no longer exists in `editorCommands`. Lookup should
        // return nil rather than crash so double-click falls back to
        // NSWorkspace and the user can fix the mapping in Settings.
        let tweaks = makeTweaks()
        tweaks.extensionEditorMap = ["md": UUID()]
        XCTAssertNil(tweaks.editor(forExtension: "md"))
    }

    // MARK: - Persistence round-trip

    func test_persistence_editorsRoundTripThroughInjectedDefaults() {
        // Persisters now route writes to the injected `defaults`
        // domain, so the round-trip stays inside an isolated suite —
        // no leak into the user's `.standard` plist if the test crashes.
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let tweaks = Tweaks(
            defaults: suite,
            osSchemeProvider: { .light },
            installOSObserver: false
        )
        let id = UUID()
        tweaks.addEditor(EditorCommand(id: id, name: "Vim", command: "vim"))
        tweaks.setMapping(extension: "md", editorId: id)

        let reloaded = Tweaks.loadEditorCommands(defaults: suite)
        XCTAssertEqual(reloaded.count, 1)
        XCTAssertEqual(reloaded.first?.id, id)
        XCTAssertEqual(reloaded.first?.command, "vim")

        let reloadedMap = Tweaks.loadExtensionEditorMap(defaults: suite)
        XCTAssertEqual(reloadedMap["md"], id)
    }

    func test_persistence_doesNotLeakIntoStandardDefaults() {
        // Regression guard: the persisters used to hard-code
        // `.standard`, which masked test pollution. Construct a tweaks
        // bound to a private suite, mutate it, and assert `.standard`
        // is untouched for our keys.
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        // Snapshot whatever the user's real `.standard` already had
        // for our keys so we restore exactly that on tear-down.
        let standard = UserDefaults.standard
        let beforeEditors = standard.data(forKey: Tweaks.editorCommandsKey)
        let beforeMap = standard.data(forKey: Tweaks.extensionEditorMapKey)
        defer {
            if let before = beforeEditors {
                standard.set(before, forKey: Tweaks.editorCommandsKey)
            } else {
                standard.removeObject(forKey: Tweaks.editorCommandsKey)
            }
            if let before = beforeMap {
                standard.set(before, forKey: Tweaks.extensionEditorMapKey)
            } else {
                standard.removeObject(forKey: Tweaks.extensionEditorMapKey)
            }
        }

        let tweaks = Tweaks(
            defaults: suite,
            osSchemeProvider: { .light },
            installOSObserver: false
        )
        tweaks.addEditor(EditorCommand(id: UUID(), name: "Vim", command: "vim"))

        XCTAssertEqual(
            standard.data(forKey: Tweaks.editorCommandsKey),
            beforeEditors,
            "Tweaks persisters must write to the injected `defaults`, not `.standard`."
        )
    }

    // MARK: - Test-seed env-var override

    /// `NICE_TEST_EDITOR_SEED` short-circuits the loaders so UITests
    /// can deterministically seed the running app's editor config
    /// without going through cfprefsd-mediated UserDefaults. Pin the
    /// override for both loaders so a future refactor doesn't
    /// silently drop one branch.
    func test_loadersHonourTestSeedEnvVar() {
        let id = UUID()
        let payload = """
        {"editorCommands":[{"id":"\(id.uuidString)","name":"Vim","command":"vim"}],"extensionEditorMap":{"md":"\(id.uuidString)"}}
        """
        setenv("NICE_TEST_EDITOR_SEED", payload, 1)
        defer { unsetenv("NICE_TEST_EDITOR_SEED") }

        let suite = freshSuite()  // empty — seed must override
        defer { wipeSuite(suite) }

        let editors = Tweaks.loadEditorCommands(defaults: suite)
        XCTAssertEqual(editors.count, 1)
        XCTAssertEqual(editors.first?.id, id)
        XCTAssertEqual(editors.first?.command, "vim")

        let map = Tweaks.loadExtensionEditorMap(defaults: suite)
        XCTAssertEqual(map["md"], id)
    }

    func test_loadersIgnoreEmptyTestSeed() {
        // Empty string is treated as "not set" so production builds
        // (where the env var is either unset or empty) flow through
        // to UserDefaults as normal.
        setenv("NICE_TEST_EDITOR_SEED", "", 1)
        defer { unsetenv("NICE_TEST_EDITOR_SEED") }

        let suite = freshSuite()
        defer { wipeSuite(suite) }

        XCTAssertEqual(Tweaks.loadEditorCommands(defaults: suite), [])
        XCTAssertEqual(Tweaks.loadExtensionEditorMap(defaults: suite), [:])
    }

    func test_loadersIgnoreMalformedTestSeed() {
        // A garbled seed must fall through to UserDefaults — better a
        // fresh-install state than a crash if the env var ever ends
        // up populated by an unrelated tool.
        setenv("NICE_TEST_EDITOR_SEED", "{not valid json", 1)
        defer { unsetenv("NICE_TEST_EDITOR_SEED") }

        let suite = freshSuite()
        defer { wipeSuite(suite) }

        XCTAssertEqual(Tweaks.loadEditorCommands(defaults: suite), [])
        XCTAssertEqual(Tweaks.loadExtensionEditorMap(defaults: suite), [:])
    }

    func test_persistence_emptyOnFreshInstall() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        XCTAssertEqual(Tweaks.loadEditorCommands(defaults: suite), [])
        XCTAssertEqual(Tweaks.loadExtensionEditorMap(defaults: suite), [:])
    }

    // MARK: - helpers

    private func makeTweaks() -> Tweaks {
        Tweaks(
            defaults: freshSuite(),
            osSchemeProvider: { .light },
            installOSObserver: false
        )
    }

    private func freshSuite() -> UserDefaults {
        UserDefaults(suiteName: "tweaks-editors-\(UUID().uuidString)")!
    }

    private func wipeSuite(_ suite: UserDefaults) {
        suite.dictionaryRepresentation().keys.forEach {
            suite.removeObject(forKey: $0)
        }
    }
}
