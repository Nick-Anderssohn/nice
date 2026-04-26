//
//  FilePasteboardAdapterTests.swift
//  NiceUnitTests
//
//  Coverage for `FilePasteboardAdapter` — the wrapper around
//  `NSPasteboard` that backs the file-browser context menu's
//  Copy / Cut / Paste flow. Tests use a private `NSPasteboard
//  (name:)` so the suite never reads or scribbles over the user's
//  real clipboard.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class FilePasteboardAdapterTests: XCTestCase {

    private var pasteboardName: NSPasteboard.Name!
    private var pasteboard: NSPasteboard!

    override func setUp() {
        super.setUp()
        pasteboardName = NSPasteboard.Name("nice-test-\(UUID().uuidString)")
        pasteboard = NSPasteboard(name: pasteboardName)
    }

    override func tearDown() {
        pasteboard.releaseGlobally()
        pasteboard = nil
        pasteboardName = nil
        super.tearDown()
    }

    // MARK: - Round-trip

    func test_writeCopy_thenRead_returnsCopyIntent() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let url = URL(fileURLWithPath: "/tmp/file.txt")

        adapter.write(urls: [url], intent: .copy)
        let read = adapter.read()

        XCTAssertEqual(read?.urls, [url])
        XCTAssertEqual(read?.intent, .copy)
    }

    func test_writeCut_thenRead_returnsCutIntent() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let url = URL(fileURLWithPath: "/tmp/file.txt")

        adapter.write(urls: [url], intent: .cut)
        let read = adapter.read()

        XCTAssertEqual(read?.urls, [url])
        XCTAssertEqual(read?.intent, .cut)
    }

    func test_externalChangeCount_invalidatesCutIntent() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let url = URL(fileURLWithPath: "/tmp/file.txt")
        adapter.write(urls: [url], intent: .cut)

        // Simulate a different app bumping the pasteboard. We must
        // overwrite via the same pasteboard handle so changeCount
        // increments — clearContents + writeObjects is the standard
        // way to do that.
        pasteboard.clearContents()
        let other = URL(fileURLWithPath: "/tmp/other.txt") as NSURL
        pasteboard.writeObjects([other])

        let read = adapter.read()
        XCTAssertEqual(read?.intent, .copy,
                       "Cut intent must be invalidated when the change count moves under us.")
    }

    func test_writeMultipleURLs_roundtrips() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let urls = [
            URL(fileURLWithPath: "/tmp/a.txt"),
            URL(fileURLWithPath: "/tmp/b.txt")
        ]

        adapter.write(urls: urls, intent: .copy)
        let read = adapter.read()

        XCTAssertEqual(read?.urls, urls)
    }

    func test_read_emptyPasteboard_returnsNil() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        XCTAssertNil(adapter.read())
    }

    func test_read_nonFileURLContent_returnsNil() {
        // Plain string only — no file URLs.
        pasteboard.clearContents()
        pasteboard.setString("hello", forType: .string)

        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        XCTAssertNil(adapter.read())
    }

    // MARK: - Cut companion

    func test_clearCutIntent_removesCutHighlight() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let url = URL(fileURLWithPath: "/tmp/file.txt")
        adapter.write(urls: [url], intent: .cut)
        XCTAssertTrue(adapter.isCut(url))

        adapter.clearCutIntent()

        XCTAssertFalse(adapter.isCut(url))
    }

    func test_isCut_reflectsCurrentCompanion() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let cutURL = URL(fileURLWithPath: "/tmp/cut.txt")
        let other = URL(fileURLWithPath: "/tmp/other.txt")

        adapter.write(urls: [cutURL], intent: .cut)

        XCTAssertTrue(adapter.isCut(cutURL))
        XCTAssertFalse(adapter.isCut(other))
    }

    func test_overwriteWithCopy_clearsCutCompanion() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let url = URL(fileURLWithPath: "/tmp/file.txt")
        adapter.write(urls: [url], intent: .cut)
        XCTAssertEqual(adapter.read()?.intent, .cut)

        adapter.write(urls: [url], intent: .copy)

        XCTAssertEqual(adapter.read()?.intent, .copy)
        XCTAssertFalse(adapter.isCut(url))
    }

    // MARK: - Mixed and edge content

    func test_read_mixedFileAndHTTPURLs_returnsOnlyFileURLs() {
        pasteboard.clearContents()
        let file = URL(fileURLWithPath: "/tmp/file.txt") as NSURL
        let web = URL(string: "https://example.com")! as NSURL
        pasteboard.writeObjects([file, web])

        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let read = adapter.read()

        XCTAssertEqual(read?.urls.count, 1)
        XCTAssertEqual(read?.urls.first?.lastPathComponent, "file.txt")
    }

    func test_externalClear_afterCutWrite_readReturnsNil() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        adapter.write(urls: [URL(fileURLWithPath: "/tmp/cut.txt")], intent: .cut)

        // Simulate another app clearing the pasteboard with no
        // file URLs left.
        pasteboard.clearContents()
        pasteboard.setString("hello", forType: .string)

        XCTAssertNil(adapter.read(),
                     "Plain-text content with no file URLs must read as nil regardless of stale cut companion.")
    }

    // MARK: - writeText (Copy Path)

    func test_writeText_writesNewlineSeparatedString_andClearsCutIntent() {
        let adapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let url = URL(fileURLWithPath: "/tmp/cut.txt")
        adapter.write(urls: [url], intent: .cut)
        XCTAssertTrue(adapter.isCut(url))

        adapter.writeText("/tmp/a.txt\n/tmp/b.txt")

        XCTAssertEqual(pasteboard.string(forType: .string), "/tmp/a.txt\n/tmp/b.txt")
        XCTAssertNil(adapter.read(), "writeText replaces file URLs with text.")
        XCTAssertFalse(adapter.isCut(url),
                       "writeText must clear the cut companion — different content is on the pasteboard now.")
    }
}
