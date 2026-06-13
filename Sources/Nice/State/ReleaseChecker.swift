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

    /// A sandboxed UI-test run sets `NICE_APPLICATION_SUPPORT_ROOT` to
    /// redirect Application Support to a temp dir. Such a run must never
    /// perform a live GitHub release check or surface the "Update
    /// available" pill: the pill renders at the trailing edge of the
    /// toolbar, and its presence shifts the chrome layout the drag /
    /// reorder / tear-off UITests assert against (an empty-chrome drag
    /// point ~120pt from the right edge would land on the pill instead
    /// of a window-drag surface). A sandboxed test also has no business
    /// hitting the network. Gates BOTH the cache-seed in `init` (so a
    /// version persisted by a prior real run can't flip the flag) and
    /// the timer in `start()`, keeping `updateAvailable == false` so the
    /// toolbar is layout-identical to a fresh build.
    nonisolated static var isSandboxedTestRun: Bool {
        ProcessInfo.processInfo.environment["NICE_APPLICATION_SUPPORT_ROOT"] != nil
    }

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
        // after relaunch if we already knew about an update. Skipped
        // under a sandboxed UI-test run so a version persisted by a
        // prior real launch can't surface the pill mid-test (see
        // `isSandboxedTestRun`).
        if !Self.isSandboxedTestRun,
           let cached = defaults.string(forKey: Self.lastKnownLatestVersionKey) {
            applyLatest(cached)
        }
    }

    /// Schedule the first check and the repeating interval. Idempotent —
    /// repeated calls after the first are no-ops, matching the
    /// `NiceServices.bootstrap()` pattern.
    func start() {
        // Never run the live release check under a sandboxed UI-test
        // run — keeps `updateAvailable` false so the toolbar stays
        // layout-stable for the chrome/drag UITests (see
        // `isSandboxedTestRun`).
        guard !Self.isSandboxedTestRun else { return }
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
