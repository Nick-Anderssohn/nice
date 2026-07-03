//
//  ThroughputMeter.swift
//  Nice
//
//  Rolling-window byte-rate meter for the window-chrome activity badge.
//  Every pane's `NiceTerminalView` reports the size of each pty output
//  chunk here (via the `onPaneOutput` callback chain: NiceTerminalView →
//  TabPtySession → SessionsModel → AppState → this meter). The meter keeps
//  a short ring of per-bucket byte counts spanning `windowDuration`
//  (~1 s) and reports the summed rate as `bytesPerSecond`.
//
//  Two-phase design so a `cat bigfile` burst can't storm SwiftUI with
//  view invalidations:
//    • `record(_:)` is called on every output chunk. It only accumulates
//      into `@ObservationIgnored` state — no observed property changes,
//      so it never invalidates the badge no matter how fast bytes arrive.
//    • `refresh()` recomputes and publishes `bytesPerSecond` / `isActive`.
//      The badge calls it on a fixed ~4 Hz display timer, so the badge
//      repaints at most a handful of times a second and the rate decays
//      (and the badge dims to idle after `idleThreshold`) even when no new
//      bytes arrive. Published setters are equality-gated so a steady
//      value doesn't notify.
//
//  The clock is injectable so the ring/idle logic is unit-testable
//  without wall-clock sleeps; production uses the monotonic
//  `ProcessInfo.systemUptime`.
//

import Foundation
import Observation

@MainActor
@Observable
final class ThroughputMeter {
    /// Terminal output rate over the last `windowDuration`, in bytes per
    /// second. Refreshed by `refresh()`; drives the badge's `NN KB/s`
    /// label.
    private(set) var bytesPerSecond: Double = 0

    /// True while output has arrived within `idleThreshold`. Drives the
    /// badge's active (accent) vs. idle (dim) styling. Flips to false on
    /// the first `refresh()` that observes `idleThreshold` of silence.
    private(set) var isActive: Bool = false

    // MARK: - Configuration

    @ObservationIgnored private let windowDuration: TimeInterval
    @ObservationIgnored private let bucketDuration: TimeInterval
    @ObservationIgnored private let idleThreshold: TimeInterval
    @ObservationIgnored private let clock: () -> TimeInterval

    // MARK: - Rolling-window state

    /// Number of buckets in the ring — `windowDuration / bucketDuration`,
    /// rounded. The ring holds exactly one window's worth of counts once
    /// `advance(to:)` has zeroed anything that scrolled out.
    @ObservationIgnored private let bucketCount: Int
    @ObservationIgnored private var buckets: [Int]
    /// Absolute index (time / bucketDuration) of the most recently
    /// touched bucket, or -1 before the first sample.
    @ObservationIgnored private var latestBucket: Int = -1
    /// Clock time of the most recent non-empty `record`, or nil if no
    /// bytes have ever arrived.
    @ObservationIgnored private var lastActivity: TimeInterval?

    init(
        windowDuration: TimeInterval = 1.0,
        bucketDuration: TimeInterval = 0.1,
        idleThreshold: TimeInterval = 2.0,
        clock: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    ) {
        precondition(
            bucketDuration > 0 && windowDuration >= bucketDuration,
            "windowDuration must be a positive multiple of bucketDuration"
        )
        self.windowDuration = windowDuration
        self.bucketDuration = bucketDuration
        self.idleThreshold = idleThreshold
        self.clock = clock
        self.bucketCount = max(1, Int((windowDuration / bucketDuration).rounded()))
        self.buckets = Array(repeating: 0, count: bucketCount)
    }

    // MARK: - Recording (hot path — no observed mutation)

    /// Add `byteCount` bytes of terminal output arriving now. Cheap and
    /// safe to call on every pty chunk: it only writes to
    /// `@ObservationIgnored` fields, so it never invalidates the badge.
    /// The badge's display timer picks the accumulated bytes up on its
    /// next `refresh()`.
    func record(_ byteCount: Int) {
        guard byteCount > 0 else { return }
        let now = clock()
        advance(to: now)
        buckets[latestBucket % bucketCount] += byteCount
        lastActivity = now
    }

    // MARK: - Publishing (display cadence)

    /// Recompute `bytesPerSecond` / `isActive` against the current clock
    /// and publish any change. Call on a steady display-rate timer so the
    /// rate decays and the badge dims to idle even when no bytes arrive.
    func refresh() {
        let now = clock()
        advance(to: now)

        let sum = buckets.reduce(0, +)
        let bps = Double(sum) / windowDuration
        let active: Bool
        if let last = lastActivity {
            active = (now - last) < idleThreshold
        } else {
            active = false
        }

        // Equality-gate so a steady value (e.g. sitting at 0 while idle)
        // doesn't notify observers every tick.
        if bytesPerSecond != bps { bytesPerSecond = bps }
        if isActive != active { isActive = active }
    }

    // MARK: - Ring maintenance

    /// Roll the ring forward to the bucket containing `now`, zeroing every
    /// bucket we skip over so counts older than `windowDuration` fall out.
    private func advance(to now: TimeInterval) {
        let idx = Int((now / bucketDuration).rounded(.down))
        if latestBucket < 0 {
            latestBucket = idx
            return
        }
        guard idx > latestBucket else { return }
        let steps = idx - latestBucket
        if steps >= bucketCount {
            // The whole window elapsed since the last touch — clear it all.
            for i in 0..<bucketCount { buckets[i] = 0 }
        } else {
            // Clear only the buckets we're scrolling into.
            for s in 1...steps {
                buckets[(latestBucket + s) % bucketCount] = 0
            }
        }
        latestBucket = idx
    }

    // MARK: - Formatting

    /// The badge's `NN KB/s` label for a given rate. Rounds to whole
    /// KB/s; a negative input (never produced here) clamps to 0.
    nonisolated static func label(forBytesPerSecond bps: Double) -> String {
        let kb = max(0, bps) / 1024.0
        return "\(Int(kb.rounded())) KB/s"
    }
}
