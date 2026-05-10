//
//  FileBrowserContextMenuModelTests.swift
//  NiceUnitTests
//
//  Coverage for the visibility rules that decide which entries
//  appear in the file-browser right-click menu. Lives behind
//  `FileBrowserContextMenuModel` so the rules are testable without
//  spinning up a SwiftUI environment.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserContextMenuModelTests: XCTestCase {

    func test_menuItems_onFile_includesOpenAndOpenWith() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: false, isRoot: false, canPaste: false
        )
        XCTAssertTrue(model.items.contains(.open))
        XCTAssertTrue(model.items.contains(.openWith))
        XCTAssertTrue(model.items.contains(.openInEditorPane))
    }

    func test_menuItems_onDirectory_omitsOpenAndOpenWith() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: false, canPaste: false
        )
        XCTAssertFalse(model.items.contains(.open))
        XCTAssertFalse(model.items.contains(.openWith))
        XCTAssertFalse(model.items.contains(.openInEditorPane))
    }

    func test_menuItems_onRoot_omitsCutCopyTrash() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: true, canPaste: false
        )
        XCTAssertFalse(model.items.contains(.copy))
        XCTAssertFalse(model.items.contains(.cut))
        XCTAssertFalse(model.items.contains(.trash))
    }

    func test_menuItems_onRoot_keepsRevealAndCopyPath() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: true, canPaste: false
        )
        XCTAssertTrue(model.items.contains(.revealInFinder))
        XCTAssertTrue(model.items.contains(.copyPath))
    }

    func test_menuItems_pasteHidden_whenPasteboardEmpty() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: false, canPaste: false
        )
        XCTAssertFalse(model.items.contains(.paste))
    }

    func test_menuItems_pasteVisible_whenPasteboardHasFileURLs() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: false, canPaste: true
        )
        XCTAssertTrue(model.items.contains(.paste))
    }

    /// File rows show the menu in the documented order:
    /// Open / Open With / Open in Editor Pane / Reveal in Finder /
    /// divider / Rename / Copy / Copy Path / Cut / Paste (when
    /// canPaste) / Trash. The trailing divider is intentionally
    /// absent — the user asked for Copy Path to sit directly under
    /// Copy.
    func test_menuItems_orderMatchesSpec_fileRow_canPaste() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: false, isRoot: false, canPaste: true
        )
        XCTAssertEqual(model.items, [
            .open, .openWith, .openInEditorPane, .revealInFinder,
            .dividerOpen,
            .rename, .copy, .copyPath, .cut, .paste, .trash
        ])
    }

    func test_menuItems_orderMatchesSpec_dirRow_noPaste() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: false, canPaste: false
        )
        XCTAssertEqual(model.items, [
            .revealInFinder,
            .dividerOpen,
            .rename, .copy, .copyPath, .cut, .trash
        ])
    }

    func test_menuItems_orderMatchesSpec_rootRow_canPaste() {
        // On root: no Open (it's a directory), no Cut/Copy/Trash.
        // Project root IS renameable — only the filesystem root `/`
        // is special-cased (handled by canRename: false in the
        // separate test below).
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: true, canPaste: true
        )
        XCTAssertEqual(model.items, [
            .revealInFinder,
            .dividerOpen,
            .rename, .copyPath, .paste
        ])
    }

    // MARK: - Rename visibility

    func test_menuItems_rename_visible_whenCanRenameTrue() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: false, isRoot: false, canPaste: false,
            canRename: true
        )
        XCTAssertTrue(model.items.contains(.rename))
    }

    func test_menuItems_rename_hidden_whenCanRenameFalse() {
        // Caller passes `canRename: false` for multi-select rows or
        // for the filesystem root `/`.
        let model = FileBrowserContextMenuModel.build(
            isDirectory: false, isRoot: false, canPaste: false,
            canRename: false
        )
        XCTAssertFalse(model.items.contains(.rename))
    }

    func test_menuItems_rename_visible_onProjectRoot() {
        // The file-browser root row (project CWD) is renameable.
        let model = FileBrowserContextMenuModel.build(
            isDirectory: true, isRoot: true, canPaste: false,
            canRename: true
        )
        XCTAssertTrue(model.items.contains(.rename))
    }

    func test_menuItems_rename_positionedBetweenDividerAndCopy() {
        let model = FileBrowserContextMenuModel.build(
            isDirectory: false, isRoot: false, canPaste: false,
            canRename: true
        )
        guard let dividerIdx = model.items.firstIndex(of: .dividerOpen),
              let renameIdx = model.items.firstIndex(of: .rename),
              let copyIdx = model.items.firstIndex(of: .copy) else {
            XCTFail("expected divider, rename, copy all present"); return
        }
        XCTAssertLessThan(dividerIdx, renameIdx)
        XCTAssertLessThan(renameIdx, copyIdx)
    }
}
