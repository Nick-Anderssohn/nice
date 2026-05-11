//
//  RelaunchHarness.swift
//  NiceUnitTests
//
//  Drives the full launch → mutate → close-some → quit → reload
//  dance against the lifecycle layer without spinning up a real
//  NiceServices / `start()` / pty subsystem. Lets a single test
//  describe a multi-window scenario in a few lines and assert what
//  the next process would see on relaunch by inspecting the harness's
//  FakeSessionStore.
//
//  Approximation boundary: each `AppState` here skips the parts of
//  production `start()` that require live infrastructure (socket
//  bootstrap, seed-Main pty spawn, async claude-path probe, key-
//  monitor install) and runs only what the session-restore layer
//  actually needs:
//      windowSession.restoreSavedWindow()
//      windowSession.markInitializationComplete()
//      windowSession.scheduleSessionSave()  // mirror start()'s tail
//
//  Close routing reuses the production `SessionLifecycleController`
//  exactly so the harness exercises the same reason-routing the
//  app does — see `userCloseWindow(at:)` and `quit()`.
//

import Foundation
@testable import Nice

@MainActor
final class RelaunchHarness {

    /// Persistent state seen by every `AppState` the harness creates.
    /// Inspectable end-of-scenario via `store.state.windows`.
    let store: FakeSessionStore

    /// AppStates currently "open" in this simulated process. `launch`
    /// appends; `userCloseWindow` removes by index; `quit` empties;
    /// `relaunch` repopulates from the store.
    private(set) var liveStates: [AppState] = []

    /// New ledger per launch generation — fresh process, fresh ledger.
    /// Reassigned in `relaunch` so the post-relaunch AppStates see an
    /// empty claim set (matching what the real next process would).
    private var ledger: WindowClaimLedger

    /// Same controller instance drives close routing and the quit
    /// cascade so harness behavior matches production wiring.
    private let lifecycleController = SessionLifecycleController()

    init(store: FakeSessionStore = FakeSessionStore()) {
        self.store = store
        self.ledger = WindowClaimLedger()
    }

    /// Pre-seed the store with the windows a prior process would have
    /// written to `sessions.json`. Subsequent `launch(windowIds:)`
    /// calls then take the matched-adoption path (deterministic
    /// adoption order), so the test can target a specific window
    /// by index without guessing.
    func preSeedStore(_ windows: [PersistedWindow]) {
        store.state = PersistedState(
            version: PersistedState.currentVersion,
            windows: windows
        )
    }

    /// Spin up one `AppState` per supplied id. Each takes the
    /// matched-adoption path (its id matches a pre-seeded window if
    /// one exists), runs `restoreSavedWindow`, releases the init
    /// gate, then upserts its initial snapshot — mirroring the tail
    /// of production `AppState.start()`. `liveStates[i]` corresponds
    /// to `windowIds[i]`.
    @discardableResult
    func launch(windowIds: [String]) -> [AppState] {
        for id in windowIds {
            let app = AppState(
                services: nil,
                initialSidebarCollapsed: false,
                initialMainCwd: nil,
                windowSessionId: id,
                store: store,
                claimLedger: ledger
            )
            app.windowSession.restoreSavedWindow()
            app.windowSession.markInitializationComplete()
            // Mirror AppState.start()'s tail so the launched window's
            // snapshot is in the store from the very first tick — any
            // following close/quit operates against a populated store.
            app.windowSession.scheduleSessionSave()
            liveStates.append(app)
        }
        return liveStates
    }

    /// Mimic the user clicking the red traffic light on the AppState
    /// at `index`. Sets the intent flag, routes through the lifecycle
    /// controller (which dispatches `.userClosedWindow` → store.remove
    /// + flush), and drops the AppState from `liveStates`.
    func userCloseWindow(at index: Int) {
        let app = liveStates.remove(at: index)
        app.userInitiatedClose = true
        lifecycleController.handleWindowWillClose(appState: app)
    }

    /// Mimic ⌘Q. Every still-live AppState takes the
    /// `.appTerminating` tearDown path (upsert + flush). `liveStates`
    /// is emptied. The detach-observers closure is a no-op because
    /// the harness never installed any `willCloseNotification`
    /// observers — the unit-level `WindowRegistryTerminationOrderingTests`
    /// covers the detach-then-tearDown ordering directly.
    func quit() {
        let states = liveStates
        liveStates = []
        lifecycleController.handleAppWillTerminate(
            allAppStates: states,
            detachObservers: {}
        )
    }

    /// Simulate the next process launch: a fresh ledger and a fresh
    /// `AppState` per saved-restorable window in the store. Each new
    /// AppState's id is a synthetic UUID (no scene-storage match in
    /// the harness), so adoption flows through the unmatched-slot
    /// path — same path the openWindow(id: "main") fan-out drives in
    /// production. Returns the post-relaunch live set.
    ///
    /// Caveat: if no restorable windows remain in the store, the
    /// harness returns an empty list. Production would still seed
    /// one fresh window in that case; tests that need that
    /// "fresh seed" assertion should check `store.state.windows`
    /// directly.
    @discardableResult
    func relaunch() -> [AppState] {
        ledger = WindowClaimLedger()
        liveStates = []
        let restorableCount = store.state.windows.filter { !$0.projects.isEmpty }.count
        for _ in 0..<restorableCount {
            let app = AppState(
                services: nil,
                initialSidebarCollapsed: false,
                initialMainCwd: nil,
                // Synthetic miss-on-match id forces the unmatched-
                // adoption branch — matches what a ⌘N / openWindow-
                // spawned scene sees on launch.
                windowSessionId: UUID().uuidString,
                store: store,
                claimLedger: ledger
            )
            app.windowSession.restoreSavedWindow()
            app.windowSession.markInitializationComplete()
            app.windowSession.scheduleSessionSave()
            liveStates.append(app)
        }
        return liveStates
    }
}
