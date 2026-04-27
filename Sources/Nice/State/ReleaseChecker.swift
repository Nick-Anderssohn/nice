//
//  ReleaseChecker.swift
//  Nice
//
//  Polls GitHub Releases every 6h for a newer version of Nice and
//  exposes an `updateAvailable` flag the toolbar binds to. No auto-
//  update or downloading — we just nudge the user to run `brew upgrade
//  --cask nice` themselves.
//
//  Caches the last-seen latest tag in `UserDefaults` so the pill shows
//  immediately on relaunch when we already knew about an update,
//  instead of flashing in 3s after launch.
//
//  Failures are silent: any error from the fetcher (network down,
//  GitHub rate limit, JSON decode) leaves `updateAvailable` unchanged
//  and the pill either stays hidden or keeps showing a previously
//  cached newer version.
//

import Foundation

@MainActor
@Observable
final class ReleaseChecker {
    nonisolated static let lastKnownLatestVersionKey = "releaseChecker.lastKnownLatestVersion"

    /// Default delay before the first check after `start()`. Keeps the
    /// launch quiet — the toolbar has other work in the first few
    /// seconds and network latency shouldn't compete with it.
    nonisolated static let defaultInitialDelay: TimeInterval = 3

    /// Default interval between subsequent checks. 6h keeps long-
    /// running sessions fresh without hitting the 60 req/hr/IP
    /// unauthenticated GitHub limit (a single instance at 6h = 4
    /// req/day).
    nonisolated static let defaultInterval: TimeInterval = 6 * 60 * 60

    private(set) var latestVersion: String?
    private(set) var updateAvailable: Bool = false

    let currentVersion: String

    @ObservationIgnored
    private let fetcher: ReleaseFetcher
    @ObservationIgnored
    private let defaults: UserDefaults
    @ObservationIgnored
    private let initialDelay: TimeInterval
    @ObservationIgnored
    private let interval: TimeInterval

    @ObservationIgnored
    private var timer: DispatchSourceTimer?
    @ObservationIgnored
    private var started = false

    init(
        currentVersion: String = ReleaseChecker.bundleVersion(),
        fetcher: ReleaseFetcher = GitHubReleaseFetcher(),
        defaults: UserDefaults = .standard,
        initialDelay: TimeInterval = ReleaseChecker.defaultInitialDelay,
        interval: TimeInterval = ReleaseChecker.defaultInterval
    ) {
        self.currentVersion = currentVersion
        self.fetcher = fetcher
        self.defaults = defaults
        self.initialDelay = initialDelay
        self.interval = interval
        // Seed from cache so the pill can appear on the first frame
        // after relaunch if we already knew about an update.
        if let cached = defaults.string(forKey: Self.lastKnownLatestVersionKey) {
            applyLatest(cached)
        }
    }

    /// Schedule the first check and the repeating interval. Idempotent —
    /// repeated calls after the first are no-ops, matching the
    /// `NiceServices.bootstrap()` pattern.
    func start() {
        guard !started else { return }
        started = true
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(
            deadline: .now() + initialDelay,
            repeating: interval
        )
        timer.setEventHandler { [weak self] in
            // The timer fires on the main queue, but we need
            // MainActor isolation explicitly so we can mutate
            // observed properties.
            Task { @MainActor [weak self] in
                await self?.checkNow()
            }
        }
        timer.resume()
        self.timer = timer
    }

    /// Cancel the repeating timer. Safe to call from the
    /// `willTerminate` hook or repeatedly.
    func stop() {
        timer?.cancel()
        timer = nil
        started = false
    }

    /// Run one check immediately. Exposed for tests and for an
    /// optional future "check now" menu item. Silently ignores fetch
    /// failures.
    func checkNow() async {
        do {
            let tag = try await fetcher.fetchLatestTag()
            applyLatest(tag)
            defaults.set(tag, forKey: Self.lastKnownLatestVersionKey)
        } catch {
            // Intentional: don't surface network errors to the UI.
        }
    }

    // MARK: - Helpers

    private func applyLatest(_ raw: String) {
        latestVersion = raw
        updateAvailable = isNewer(raw, than: currentVersion)
    }

    private func isNewer(_ latest: String, than current: String) -> Bool {
        guard
            let l = SemanticVersion(latest),
            let c = SemanticVersion(current)
        else { return false }
        return l > c
    }

    nonisolated static func bundleVersion() -> String {
        (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "0.0.0"
    }
}
