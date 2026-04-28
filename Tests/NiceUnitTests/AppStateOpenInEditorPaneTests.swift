//
//  AppStateOpenInEditorPaneTests.swift
//  NiceUnitTests
//
//  Direct coverage for the pure helper that builds the spawn-spec
//  for an editor pane (cwd, title, shell-command-with-quoted-path).
//  Tests path quoting on awkward filenames, the cwd choice (parent
//  directory), and that the editor's own command is left unquoted so
//  args like `nvim -p` survive into the shell.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateOpenInEditorPaneTests: XCTestCase {

    // MARK: - editorPaneSpec

    func test_editorPaneSpec_simpleVimInvocation() {
        let editor = EditorCommand(id: UUID(), name: "Vim", command: "vim")
        let url = URL(fileURLWithPath: "/tmp/notes.md")

        let spec = AppState.editorPaneSpec(editor: editor, url: url)

        XCTAssertEqual(spec.cwd, "/tmp")
        XCTAssertEqual(spec.title, "Vim notes.md")
        XCTAssertEqual(spec.command, "vim '/tmp/notes.md'")
    }

    func test_editorPaneSpec_preservesEditorArgs() {
        // `nvim -p` opens files in tabs. The editor.command must be
        // splatted into the shell verbatim — the path is the only
        // thing that's quoted.
        let editor = EditorCommand(id: UUID(), name: "Neovim", command: "nvim -p")
        let url = URL(fileURLWithPath: "/Users/me/foo.swift")

        let spec = AppState.editorPaneSpec(editor: editor, url: url)

        XCTAssertEqual(spec.command, "nvim -p '/Users/me/foo.swift'")
    }

    func test_editorPaneSpec_quotesPathsWithSpaces() {
        let editor = EditorCommand(id: UUID(), name: "Vim", command: "vim")
        let url = URL(fileURLWithPath: "/Users/me/My Project/README.md")

        let spec = AppState.editorPaneSpec(editor: editor, url: url)

        XCTAssertEqual(spec.cwd, "/Users/me/My Project")
        XCTAssertEqual(spec.command, "vim '/Users/me/My Project/README.md'")
    }

    func test_editorPaneSpec_quotesPathsWithSingleQuotes() {
        // Single-quote inside the path needs the shell-quote dance
        // (`'…'\''…'`) to survive. `shellSingleQuote` already handles
        // it; this test pins the contract end-to-end.
        let editor = EditorCommand(id: UUID(), name: "Vim", command: "vim")
        let url = URL(fileURLWithPath: "/tmp/Has 'weird' name.md")

        let spec = AppState.editorPaneSpec(editor: editor, url: url)

        // Expected form: `'/tmp/Has '\''weird'\'' name.md'`
        XCTAssertEqual(
            spec.command,
            "vim '/tmp/Has '\\''weird'\\'' name.md'"
        )
    }

    func test_editorPaneSpec_titleHasEditorNameAndFilename() {
        let editor = EditorCommand(id: UUID(), name: "Glow", command: "glow")
        let url = URL(fileURLWithPath: "/tmp/some-deep-file.md")

        let spec = AppState.editorPaneSpec(editor: editor, url: url)

        XCTAssertEqual(spec.title, "Glow some-deep-file.md")
    }

    // MARK: - mergeEditorPaneEntries

    func test_mergeEditorPaneEntries_userFirstThenDetected() {
        let user = [
            EditorCommand(id: UUID(), name: "Custom Vim", command: "vim --noplugin"),
        ]
        let detected = [
            EditorCommand(id: UUID(), name: "Helix", command: "hx"),
        ]
        let merged = AppState.mergeEditorPaneEntries(user: user, detected: detected)
        XCTAssertEqual(merged.user.map(\.name), ["Custom Vim"])
        XCTAssertEqual(merged.detected.map(\.name), ["Helix"])
    }

    func test_mergeEditorPaneEntries_dedupByCommandUserWins() {
        // The user manually configured a `vim` editor with custom args
        // before the auto-detector found `vim` on PATH. The user's
        // entry must survive in the user array; the detector's vim
        // disappears so the menu doesn't list "Vim" twice.
        let userId = UUID()
        let user = [
            EditorCommand(id: userId, name: "My Vim", command: "vim"),
        ]
        let detected = [
            EditorCommand(id: UUID(), name: "Vim",     command: "vim"),
            EditorCommand(id: UUID(), name: "Helix",   command: "hx"),
        ]
        let merged = AppState.mergeEditorPaneEntries(user: user, detected: detected)
        XCTAssertEqual(merged.user.first?.id, userId)
        XCTAssertEqual(merged.detected.map(\.command), ["hx"],
                       "Detected `vim` collides with user's custom; only `hx` should remain.")
    }

    func test_mergeEditorPaneEntries_emptyInputsProduceEmptyEntries() {
        let merged = AppState.mergeEditorPaneEntries(user: [], detected: [])
        XCTAssertTrue(merged.isEmpty)
    }

    // MARK: - resolveTargetTab

    func test_resolveTargetTab_prefersValidActiveTab() {
        let resolved = AppState.resolveTargetTab(
            activeTabId: "tab-A",
            hasTab: { _ in true },
            firstAvailable: { "tab-Z" }
        )
        XCTAssertEqual(resolved, "tab-A")
    }

    func test_resolveTargetTab_fallsBackWhenActiveIsStale() {
        // The active tab id is set but no longer corresponds to a
        // live tab (rare, but possible mid-frame during teardown).
        // Falls through to firstAvailable.
        let resolved = AppState.resolveTargetTab(
            activeTabId: "stale",
            hasTab: { _ in false },
            firstAvailable: { "tab-Z" }
        )
        XCTAssertEqual(resolved, "tab-Z")
    }

    func test_resolveTargetTab_fallsBackWhenActiveIsNil() {
        let resolved = AppState.resolveTargetTab(
            activeTabId: nil,
            hasTab: { _ in true },
            firstAvailable: { "tab-Z" }
        )
        XCTAssertEqual(resolved, "tab-Z")
    }

    func test_resolveTargetTab_returnsNilWhenNothingAvailable() {
        let resolved = AppState.resolveTargetTab(
            activeTabId: nil,
            hasTab: { _ in false },
            firstAvailable: { nil }
        )
        XCTAssertNil(resolved)
    }

    // MARK: - openInEditorPane orchestration

    /// Pre-seeded AppState (services-less convenience init) starts
    /// with a Terminals project containing one Main tab + one
    /// terminal pane. Bare invocation of `openInEditorPane` with an
    /// unknown editor id must no-op — no extra pane appears, no
    /// activeTabId churn, no crash. This pins the "missing editor →
    /// silent fallthrough" contract.
    func test_openInEditorPane_unknownEditorId_isNoOp() {
        let appState = AppState()
        let initialActiveTabId = appState.tabs.activeTabId
        let initialPaneCount = countAllPanes(appState)

        appState.openInEditorPane(
            url: URL(fileURLWithPath: "/tmp/x.md"),
            editorId: UUID()
        )

        XCTAssertEqual(appState.tabs.activeTabId, initialActiveTabId)
        XCTAssertEqual(countAllPanes(appState), initialPaneCount,
                       "Unknown editor id must not spawn a pane.")
    }

    // MARK: - openFromDoubleClick

    /// With no Tweaks injected (services-less AppState), the editor
    /// lookup returns nil for every extension, so the call falls
    /// through to NSWorkspace. We can't assert NSWorkspace from a
    /// unit test, but we *can* assert the pane count is unchanged —
    /// which pins "no editor pane is spawned when nothing is mapped".
    func test_openFromDoubleClick_noMapping_noPaneSpawned() {
        let appState = AppState()
        let initial = countAllPanes(appState)

        appState.openFromDoubleClick(url: URL(fileURLWithPath: "/tmp/x.png"))

        XCTAssertEqual(countAllPanes(appState), initial,
                       "Unmapped extension must not spawn an editor pane.")
    }

    // MARK: - helpers

    private func countAllPanes(_ appState: AppState) -> Int {
        appState.tabs.projects.reduce(0) { acc, project in
            acc + project.tabs.reduce(0) { $0 + $1.panes.count }
        }
    }
}
