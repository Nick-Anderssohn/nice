//
//  PaneDragSourceWindowDragTests.swift
//  NiceUnitTests
//
//  Regression tests for the toolbar's window-drag plumbing. The
//  imperative pattern (`WindowDragRegion.ChromeDragView` overrides
//  `mouseDown(with:)` to call `performDrag(with:)` / `performZoom`)
//  rests on three properties:
//
//  1. `ChromeDragView` reports `mouseDownCanMoveWindow == false`.
//     A transparent `NSView` defaults to `true`, which would let
//     AppKit's title-bar drag tracker engage *before*
//     `mouseDown(with:)` is dispatched — silently re-creating the
//     cooperative path the imperative design avoids.
//
//  2. Hit-tests in the empty chrome region land on `ChromeDragView`,
//     so its `mouseDown` actually fires. If a future layout change
//     puts an opaque view above it, dragging the empty toolbar
//     would stop moving the window.
//
//  3. Hit-tests anywhere inside a pill's frame — including the
//     visually-rounded corner pixels — do NOT fall through to
//     `ChromeDragView`. SwiftUI's `.contentShape(RoundedRectangle)`
//     resolves at the SwiftUI layer, but `NSHostingView`'s default
//     `hitTest:` claims its full bounds when the hosted SwiftUI
//     content returns nil — so corner clicks reach the pill's
//     `NSPanGestureRecognizer` and drag the pane, not the window.
//     This test pins that behaviour: if a future
//     `NSHostingView`-subclass override re-introduces the
//     boundary-nil bug from the previous fix attempt
//     (`NonDraggableHostingView`), corner drags will silently
//     resurrect the window-drag-instead-of-pane bug.
//

import AppKit
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class PaneDragSourceWindowDragTests: XCTestCase {

    /// Toolbar fixture: `WindowDragRegion` in the back, a pill (a
    /// `PaneDragSource`-wrapped colour rectangle with the same
    /// `.contentShape(RoundedRectangle)` the production
    /// `InlinePanePill` uses) on top.
    /// Mimics the structure of the production `InlinePanePill`
    /// closely enough to expose AppKit-level pitfalls — in
    /// particular, a `RoundedRectangle.fill` background instead of a
    /// flat `Color.frame()`. The latter makes `NSHostingView` report
    /// `isOpaque == true`, which silently sidesteps the default-true
    /// `mouseDownCanMoveWindow` for transparent NSViews. Production
    /// pills are not opaque (rounded corners are transparent), so we
    /// have to test against that reality.
    private struct Fixture: View {
        let payload: PaneDragPayload
        @State private var dragState = PaneDragState()

        var body: some View {
            ZStack {
                WindowDragRegion()
                PaneDragSource(
                    payload: payload,
                    dragState: dragState,
                    onTearOff: { _ in }
                ) {
                    // Match the production inactive-pill state:
                    // `InlinePanePill.background == .clear` when the
                    // pill is neither active nor hovered. A
                    // transparent fill here exposes the AppKit-level
                    // pitfall an opaque `Color.blue` would mask —
                    // namely that `NSHostingView` reports
                    // `isOpaque == false`, which defaults the
                    // hosted views' `mouseDownCanMoveWindow` to
                    // `true`. Most clicks on a real pill land on
                    // an inactive-state pixel, so this is the
                    // common case.
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(Color.clear)
                        .frame(width: 120, height: 28)
                        .contentShape(
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                        )
                }
                .frame(width: 120, height: 28)
                .fixedSize()
            }
            .frame(width: 200, height: 52)
        }
    }

    private func mountFixture() -> (NSWindow, NSView) {
        let payload = PaneDragPayload(
            windowSessionId: "test-window",
            tabId: "t1",
            paneId: "p1",
            kind: .terminal
        )
        let controller = NSHostingController(
            rootView: Fixture(payload: payload)
        )
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 52),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        // ARC + NSWindow's legacy `isReleasedWhenClosed = true`
        // default is a known double-release source: closing the
        // window releases it, then ARC releases again at scope
        // end and `XCTMemoryChecker` crashes the test process in
        // `objc_release` during autorelease-pool drain. Opt out of
        // the legacy behaviour and let ARC own the lifetime.
        window.isReleasedWhenClosed = false
        window.contentViewController = controller
        window.layoutIfNeeded()
        // Pump the runloop briefly so SwiftUI commits its NSView
        // descendants — the hosting tree isn't always fully
        // materialised after the first synchronous layout.
        RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        window.layoutIfNeeded()
        return (window, window.contentView!)
    }

    // MARK: - Property 1 — chrome view doesn't engage cooperative drag

    /// `ChromeDragView` must explicitly opt out of AppKit's
    /// cooperative `mouseDownCanMoveWindow` mechanism. If it
    /// inherits the default `true` for transparent `NSView`s, the
    /// title-bar tracker engages before our `mouseDown(with:)` runs
    /// — bypassing the imperative path that distinguishes
    /// single-click drag from double-click zoom.
    func test_chromeDragView_doesNotEngageCooperativeDrag() {
        let (window, parent) = mountFixture()
        defer { window.close() }

        // Hit-test the empty chrome region (well outside the centred
        // pill). The leaf must be `ChromeDragView` AND must report
        // `mouseDownCanMoveWindow == false`.
        let chromePoint = NSPoint(x: 10, y: 6)
        guard let hit = parent.hitTest(chromePoint) else {
            XCTFail("Chrome point \(chromePoint) had no hit-test result.")
            return
        }
        let name = String(describing: type(of: hit))
        XCTAssertTrue(
            name.contains("ChromeDragView"),
            "Expected chrome point hit to be `ChromeDragView`, got \(name)."
        )
        XCTAssertFalse(
            hit.mouseDownCanMoveWindow,
            """
            `ChromeDragView` reports `mouseDownCanMoveWindow == \
            true` — AppKit's title-bar tracker will engage \
            cooperatively, bypassing the imperative \
            `mouseDown(with:)` → `performDrag` path.
            """
        )
    }

    // MARK: - Property 2 — chrome region routes to ChromeDragView

    /// Sanity: chrome clicks must reach `ChromeDragView` so its
    /// `mouseDown(with:)` can fire. Future layout changes that put
    /// an opaque view above the chrome drag region would break
    /// drag-empty-chrome-to-move-window.
    func test_chromeRegion_routesToChromeDragView() {
        let (window, parent) = mountFixture()
        defer { window.close() }

        let chromePoint = NSPoint(x: 10, y: 6)
        let hit = parent.hitTest(chromePoint)
        let name = hit.map { String(describing: type(of: $0)) } ?? "(nil)"
        XCTAssertTrue(
            name.contains("ChromeDragView"),
            """
            Expected hit-test at chrome point \(chromePoint) to \
            return `ChromeDragView` (the imperative window-drag \
            handler). Got \(name) — empty-chrome drags will no \
            longer move the window.
            """
        )
    }

    // MARK: - Property 3 — pill claims its full bounds (incl. corners)

    /// The pill must claim every interior point of its bounding
    /// rectangle at the AppKit hit-test level — even the rounded-
    /// corner pixels SwiftUI's `.contentShape` rejects. If a corner
    /// pixel falls through to `ChromeDragView`, dragging the pill
    /// from that corner drags the window instead of the pane.
    /// `NSHostingView`'s default `hitTest:` provides this for free
    /// (it claims its bounds when its content returns nil); the
    /// previous fix attempt (`NonDraggableHostingView`) added an
    /// override that broke this at boundary pixels and re-introduced
    /// the bug. This test pins that behaviour so a future override
    /// can't quietly re-introduce the fall-through.
    /// The actual property AppKit's title-bar drag tracker reads is
    /// the hit-tested leaf's `mouseDownCanMoveWindow`. This is the
    /// strict version of the fall-through test: not just "is the
    /// leaf the chrome drag view", but "does the leaf return
    /// `false`" — covering the case where some other transparent
    /// SwiftUI internal NSView ends up as the leaf. Default
    /// `mouseDownCanMoveWindow` for a transparent `NSView` is
    /// `true`, and `NSHostingView` is non-opaque whenever the
    /// hosted SwiftUI tree has any transparent region (rounded
    /// corners), so without the explicit override on
    /// `PaneDragSource.NonDraggableHostingView` the leaf can
    /// silently return `true` and AppKit's tracker engages on
    /// `mouseDown` — dragging the pill drags the window.
    func test_pillRegion_leafReportsMouseDownCanMoveWindowFalse() {
        let (window, parent) = mountFixture()
        defer { window.close() }

        let interior = NSRect(x: 41, y: 13, width: 118, height: 26)

        var offenders: [(NSPoint, String)] = []
        for x in stride(from: interior.minX, through: interior.maxX, by: 2) {
            for y in stride(from: interior.minY, through: interior.maxY, by: 2) {
                let p = NSPoint(x: x, y: y)
                guard let hit = parent.hitTest(p) else { continue }
                if hit.mouseDownCanMoveWindow {
                    offenders.append((p, String(describing: type(of: hit))))
                }
            }
        }

        if !offenders.isEmpty {
            let preview = offenders.prefix(5).map { p, name in
                "  at (\(p.x), \(p.y)) → \(name)"
            }.joined(separator: "\n")
            XCTFail(
                """
                \(offenders.count) interior pill point(s) hit-test \
                to a leaf with `mouseDownCanMoveWindow == true`. \
                AppKit's title-bar tracker engages at `mouseDown` \
                on these views, pre-empting the pan recogniser — \
                dragging the pill drags the window. First offenders:
                \(preview)
                """
            )
        }
    }

    func test_pillRegion_neverFallsThroughToChromeDragView() {
        let (window, parent) = mountFixture()
        defer { window.close() }

        // Probe the pill's interior. Pill is centred 120×28 inside
        // the 200×52 fixture. Stay 1pt inside the frame on every
        // side — SwiftUI's hit-test treats `Rectangle` as half-open
        // at the exact frame edge, and a real cursor never lands
        // on a sub-pixel boundary anyway.
        let interior = NSRect(x: 41, y: 13, width: 118, height: 26)

        var fellThrough: [(NSPoint, String)] = []
        for x in stride(from: interior.minX, through: interior.maxX, by: 2) {
            for y in stride(from: interior.minY, through: interior.maxY, by: 2) {
                let p = NSPoint(x: x, y: y)
                guard let hit = parent.hitTest(p) else { continue }
                let name = String(describing: type(of: hit))
                if name.contains("ChromeDragView") {
                    fellThrough.append((p, name))
                }
            }
        }

        if !fellThrough.isEmpty {
            let preview = fellThrough.prefix(5).map { p, name in
                "  at (\(p.x), \(p.y)) → \(name)"
            }.joined(separator: "\n")
            XCTFail(
                """
                \(fellThrough.count) interior pill point(s) fell \
                through to `ChromeDragView`. Drag-from-pill at \
                those points will move the window instead of the \
                pane. First offenders:
                \(preview)
                """
            )
        }
    }

    /// Specifically the four corner pixels — these are the original
    /// failure mode `NonDraggableHostingView`'s buggy override
    /// produced. Tightly assert on each.
    func test_pillCorners_routeToPillNotChrome() {
        let (window, parent) = mountFixture()
        defer { window.close() }

        // 1pt inside each corner of the pill's frame.
        let corners: [(String, NSPoint)] = [
            ("top-left",     NSPoint(x: 41,  y: 13)),
            ("top-right",    NSPoint(x: 159, y: 13)),
            ("bottom-left",  NSPoint(x: 41,  y: 39)),
            ("bottom-right", NSPoint(x: 159, y: 39)),
        ]

        for (label, p) in corners {
            guard let hit = parent.hitTest(p) else {
                XCTFail("\(label) corner \(p) had no hit-test result.")
                continue
            }
            let name = String(describing: type(of: hit))
            XCTAssertFalse(
                name.contains("ChromeDragView"),
                """
                \(label) corner pixel at \(p) fell through to \
                `ChromeDragView`. Got hit: \(name).
                """
            )
        }
    }
}
