//
//  TerminalContainerViewLayoutTests.swift
//  NiceUnitTests
//
//  Pins the bottom-anchoring behaviour of `TerminalContainerView` — the
//  host wrapper that fixes the wandering bottom-gap on window resize.
//
//  SwiftTerm computes `rows = Int(height / cellHeight)` and renders the
//  grid top-anchored, so any sub-row remainder (0…cellHeight-1 px) lands
//  below the last row. The container sizes its terminal subview to a
//  whole number of rows and pins it to the bottom, moving that remainder
//  to the *top* (under the chrome) so the gap below the prompt is
//  constant. These tests assert the quantization + bottom-pin math and
//  the full-frame fallback that keeps the deferred-spawn gate firing.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class TerminalContainerViewLayoutTests: XCTestCase {

    /// The terminal frame height is always an exact multiple of the cell
    /// height, and the leftover remainder sits at the top (the grid is
    /// pinned to the bottom).
    func test_layout_quantizesToWholeRows_andPinsToBottom() {
        let view = NiceTerminalView(frame: .zero)
        let cell = view.cellHeight
        XCTAssertGreaterThan(cell, 0, "font cell height must be known after init")

        let container = TerminalContainerView(terminal: view)
        // A height deliberately off a row boundary by a few points.
        let remainder: CGFloat = 7
        let height = cell * 10 + remainder
        container.setFrameSize(NSSize(width: 800, height: height))
        container.layout()

        let f = view.frame
        XCTAssertEqual(f.width, 800, accuracy: 0.001, "width fills the container")
        XCTAssertEqual(
            f.height, cell * 10, accuracy: 0.001,
            "height is floored to a whole number of rows"
        )
        XCTAssertEqual(
            f.height.truncatingRemainder(dividingBy: cell), 0, accuracy: 0.001,
            "no sub-row remainder inside the terminal's own frame"
        )
        XCTAssertEqual(f.origin.y, 0, accuracy: 0.001, "grid is pinned to the bottom edge")
        XCTAssertEqual(
            height - f.height, remainder, accuracy: 0.001,
            "the leftover remainder is parked above the grid (under the chrome)"
        )
    }

    /// `bottomInset` lifts the grid off the bottom by a constant amount;
    /// the remainder still lands at the top.
    func test_layout_honoursBottomInset() {
        let view = NiceTerminalView(frame: .zero)
        let cell = view.cellHeight
        let container = TerminalContainerView(terminal: view)
        container.bottomInset = 5
        let height = cell * 8 + 3
        container.setFrameSize(NSSize(width: 600, height: height))
        container.layout()

        let f = view.frame
        XCTAssertEqual(f.origin.y, 5, accuracy: 0.001, "grid sits above the bottom inset")
        XCTAssertEqual(
            f.height, (((height - 5) / cell).rounded(.down)) * cell, accuracy: 0.001,
            "rows are quantized against the inset-adjusted available height"
        )
    }

    /// A window shorter than a single row (or before the cell height is
    /// known) falls back to the full bounds so the terminal still gets a
    /// non-zero frame — that's what fires the deferred shell spawn.
    func test_layout_belowOneRow_fallsBackToFullBounds() {
        let view = NiceTerminalView(frame: .zero)
        let cell = view.cellHeight
        let container = TerminalContainerView(terminal: view)
        let height = cell / 2          // shorter than one row
        container.setFrameSize(NSSize(width: 400, height: height))
        container.layout()

        let f = view.frame
        XCTAssertEqual(f.height, height, accuracy: 0.001, "falls back to full height")
        XCTAssertEqual(f.width, 400, accuracy: 0.001)
        XCTAssertEqual(f.origin.y, 0, accuracy: 0.001)
    }

    /// Re-laying out at a different height re-quantizes cleanly (no stale
    /// frame carried over) — covers resize and font-change relayouts.
    func test_relayout_atNewHeight_requantizes() {
        let view = NiceTerminalView(frame: .zero)
        let cell = view.cellHeight
        let container = TerminalContainerView(terminal: view)

        container.setFrameSize(NSSize(width: 500, height: cell * 20 + 4))
        container.layout()
        XCTAssertEqual(view.frame.height, cell * 20, accuracy: 0.001)

        container.setFrameSize(NSSize(width: 500, height: cell * 12 + 9))
        container.layout()
        XCTAssertEqual(view.frame.height, cell * 12, accuracy: 0.001)
        XCTAssertEqual(view.frame.origin.y, 0, accuracy: 0.001)
    }

    /// An exact row-boundary height keeps every row — `floor` must not
    /// shave off the last row when the remainder is already zero.
    func test_layout_exactRowBoundary_keepsAllRows() {
        let view = NiceTerminalView(frame: .zero)
        let cell = view.cellHeight
        let container = TerminalContainerView(terminal: view)
        container.setFrameSize(NSSize(width: 700, height: cell * 15))
        container.layout()

        XCTAssertEqual(view.frame.height, cell * 15, accuracy: 0.001)
        XCTAssertEqual(view.frame.origin.y, 0, accuracy: 0.001, "no remainder, flush bottom")
    }

    /// A zero-size container (SwiftUI's first mount before measuring) maps
    /// to a zero terminal frame without trapping on the divide.
    func test_layout_zeroSizeFrame_isHandled() {
        let view = NiceTerminalView(frame: .zero)
        let container = TerminalContainerView(terminal: view)
        container.setFrameSize(.zero)
        container.layout()

        XCTAssertEqual(view.frame, NSRect(x: 0, y: 0, width: 0, height: 0))
    }

    /// A font change alters the cell height, and a forced relayout
    /// re-quantizes the grid to the new row size — the regression the
    /// `TabPtySession` font hooks exist to prevent.
    func test_relayout_afterFontChange_requantizesToNewCellHeight() {
        let view = NiceTerminalView(frame: .zero)
        let container = TerminalContainerView(terminal: view)
        container.setFrameSize(NSSize(width: 500, height: 1000))
        container.layout()
        let cellBefore = view.cellHeight

        view.font = NSFont.monospacedSystemFont(
            ofSize: view.font.pointSize * 2, weight: .regular)
        container.needsLayout = true
        container.layout()
        let cellAfter = view.cellHeight

        // The frame value alone can coincide across cell sizes (different
        // cells can floor to the same height), so assert against the
        // *current* cell: the grid is re-quantized to a whole number of
        // the new rows, not left stale at the old quantization.
        XCTAssertGreaterThan(
            cellAfter, cellBefore, "doubling the font must produce taller cells")
        XCTAssertEqual(
            view.frame.height, (1000 / cellAfter).rounded(.down) * cellAfter,
            accuracy: 0.001,
            "the grid re-quantized to the new cell height after the font change")
    }

    /// The container relays a real, non-zero frame down to the terminal,
    /// which is what fires `NiceTerminalView`'s deferred shell spawn. This
    /// pins the integration the unit layout tests don't exercise: the
    /// spawn gate keys off the terminal's own first non-zero frame, and
    /// that frame now originates from the container's layout pass.
    func test_deferredSpawn_firesThroughContainer() {
        let view = NiceTerminalView(frame: .zero)
        let container = TerminalContainerView(terminal: view)
        view.armDeferredSpawn(
            executable: "/usr/bin/true",
            args: [],
            environment: nil,
            execName: nil,
            currentDirectory: nil
        )
        XCTAssertFalse(view.hasFiredPendingSpawn, "no fire before the container is laid out")

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
            styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = container
        container.layoutSubtreeIfNeeded()   // force the layout pass that sizes the terminal

        XCTAssertGreaterThan(view.frame.height, 0, "container relays a non-zero frame down")
        XCTAssertTrue(
            view.hasFiredPendingSpawn,
            "a non-zero terminal frame in a window must fire the deferred spawn through the container")
    }
}
