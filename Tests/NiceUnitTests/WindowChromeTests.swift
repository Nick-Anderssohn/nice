//
//  WindowChromeTests.swift
//  NiceUnitTests
//
//  Pins the shared top-bar geometry so the values several views depend
//  on can't silently drift. In particular `trafficLightReservedWidth`
//  (the collapsed cap's leading reserve) is derived from the
//  traffic-light nudge — the whole point of folding those magic numbers
//  into one source — so this asserts the derivation still lands on the
//  82pt the cap historically hard-coded.
//

import XCTest
@testable import Nice

final class WindowChromeTests: XCTestCase {

    func test_topBarHeight_is52() {
        XCTAssertEqual(WindowChrome.topBarHeight, 52)
    }

    func test_trafficLightReservedWidth_matchesHistoricalValue() {
        XCTAssertEqual(WindowChrome.trafficLightReservedWidth, 82)
    }

    func test_trafficLightReservedWidth_derivesFromNudgeGeometry() {
        // If a future change to the nudge breaks this identity, the
        // collapsed cap's reserve has drifted from where the traffic
        // lights actually end up.
        XCTAssertEqual(
            WindowChrome.trafficLightReservedWidth,
            WindowChrome.trafficLightDefaultLeading
                + WindowChrome.trafficLightNudgeX
                + WindowChrome.trafficLightClusterWidth
        )
    }
}
