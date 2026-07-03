//
//  ThroughputMeterTests.swift
//  NiceUnitTests
//
//  Coverage for `ThroughputMeter`, the rolling-window byte-rate meter
//  behind the chrome activity badge. All tests drive an injected clock so
//  the ring/idle logic is exercised deterministically without wall-clock
//  sleeps.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class ThroughputMeterTests: XCTestCase {

    /// Meter with a 1 s window / 0.1 s buckets / 2 s idle threshold and a
    /// caller-controlled clock. The returned setter advances virtual time.
    private func makeMeter(
        start: TimeInterval = 1000
    ) -> (meter: ThroughputMeter, setNow: (TimeInterval) -> Void) {
        var now = start
        let meter = ThroughputMeter(
            windowDuration: 1.0,
            bucketDuration: 0.1,
            idleThreshold: 2.0,
            clock: { now }
        )
        return (meter, { now = $0 })
    }

    // MARK: - Rate

    func test_startsIdleAtZero() {
        let (meter, _) = makeMeter()
        meter.refresh()
        XCTAssertEqual(meter.bytesPerSecond, 0)
        XCTAssertFalse(meter.isActive, "No bytes have ever arrived → idle")
    }

    func test_recordedBytesReportAsRateOverWindow() {
        let (meter, _) = makeMeter()
        // 2048 bytes within the 1 s window → 2048 B/s.
        meter.record(2048)
        meter.refresh()
        XCTAssertEqual(meter.bytesPerSecond, 2048, accuracy: 0.001)
        XCTAssertTrue(meter.isActive, "Bytes just arrived → active")
    }

    func test_bytesAcrossBucketsSumWithinWindow() {
        let (meter, setNow) = makeMeter(start: 1000)
        meter.record(1000)          // bucket at t=1000.0
        setNow(1000.5)
        meter.record(1000)          // bucket at t=1000.5, still in window
        meter.refresh()
        XCTAssertEqual(meter.bytesPerSecond, 2000, accuracy: 0.001,
                       "Two 1000-byte chunks inside the 1 s window sum to 2000 B/s")
    }

    func test_rateDecaysAsBytesLeaveWindow() {
        let (meter, setNow) = makeMeter(start: 1000)
        meter.record(4096)          // at t=1000
        setNow(1000.5)
        meter.refresh()
        XCTAssertEqual(meter.bytesPerSecond, 4096, accuracy: 0.001,
                       "Still inside the window half a second later")

        // Advance past the full window: the 4096 bytes have scrolled out.
        setNow(1001.2)
        meter.refresh()
        XCTAssertEqual(meter.bytesPerSecond, 0, accuracy: 0.001,
                       "After the 1 s window elapses the old bytes fall out")
    }

    // MARK: - Idle / active transition

    func test_staysActiveUntilIdleThreshold() {
        let (meter, setNow) = makeMeter(start: 1000)
        meter.record(1024)
        // 1.9 s later — past the rate window but inside the 2 s idle
        // threshold: dimmed number, but still the active style.
        setNow(1001.9)
        meter.refresh()
        XCTAssertTrue(meter.isActive,
                      "Within the idle threshold the badge stays active")
        XCTAssertEqual(meter.bytesPerSecond, 0, accuracy: 0.001,
                       "…even though the rate has already decayed to 0")
    }

    func test_dimsToIdleAfterThreshold() {
        let (meter, setNow) = makeMeter(start: 1000)
        meter.record(1024)
        setNow(1002.5)              // 2.5 s of silence > 2 s threshold
        meter.refresh()
        XCTAssertFalse(meter.isActive, "Past the idle threshold → idle")
        XCTAssertEqual(meter.bytesPerSecond, 0, accuracy: 0.001)
    }

    func test_reactivatesWhenBytesResume() {
        let (meter, setNow) = makeMeter(start: 1000)
        meter.record(1024)
        setNow(1005)                // long silence → idle
        meter.refresh()
        XCTAssertFalse(meter.isActive)

        meter.record(512)           // new output at t=1005
        meter.refresh()
        XCTAssertTrue(meter.isActive, "Fresh bytes re-activate the badge")
        XCTAssertEqual(meter.bytesPerSecond, 512, accuracy: 0.001,
                       "Ring was cleared on the long jump; only the new chunk counts")
    }

    // MARK: - Hot-path hygiene

    func test_zeroAndNegativeChunksIgnored() {
        let (meter, _) = makeMeter()
        meter.record(0)
        meter.record(-10)
        meter.refresh()
        XCTAssertEqual(meter.bytesPerSecond, 0)
        XCTAssertFalse(meter.isActive, "An empty chunk must not count as activity")
    }

    // MARK: - Label formatting

    func test_labelFormatsWholeKilobytesPerSecond() {
        XCTAssertEqual(ThroughputMeter.label(forBytesPerSecond: 0), "0 KB/s")
        XCTAssertEqual(ThroughputMeter.label(forBytesPerSecond: 1024), "1 KB/s")
        XCTAssertEqual(ThroughputMeter.label(forBytesPerSecond: 1536), "2 KB/s",
                       "1.5 KB/s rounds to nearest whole KB/s")
        XCTAssertEqual(ThroughputMeter.label(forBytesPerSecond: 20480), "20 KB/s")
        XCTAssertEqual(ThroughputMeter.label(forBytesPerSecond: -5), "0 KB/s",
                       "Negative input clamps to 0")
    }
}
