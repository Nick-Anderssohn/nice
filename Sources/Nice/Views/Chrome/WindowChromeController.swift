//
//  WindowChromeController.swift
//  Nice
//
//  Single owner of one `NSWindow`'s chrome AppKit state. Replaces the
//  scattered band-aid layer (the old `TrafficLightNudger` statics + the
//  inline `window.isMovable = false` write in `AppShellView`) with one
//  object per window whose contract is:
//
//    • chrome state is computed per-event, never remembered — there is no
//      cached "canonical position" or "we already nudged this window"
//      static to drift out of sync (the BUG B class). The traffic-light
//      placer recomputes an absolute target on every frame event; the
//      `isMovable` policy is re-asserted on every focus / KVO event.
//    • one controller per window — the static `NSMapTable` keys on the
//      `NSWindow` itself with WEAK keys, so an entry auto-prunes the
//      instant the window deallocs. That kills the address-reuse hazard
//      that an `ObjectIdentifier`-keyed dictionary had (a freed window's
//      address could be handed to a new window and collide).
//    • registration is UNCONDITIONAL — `adopt()` registers every window
//      it's handed regardless of `styleMask`. `viewDidMoveToWindow` (the
//      bridge that calls `adopt`) can fire before SwiftUI finishes
//      applying `.hiddenTitleBar`'s `.fullSizeContentView` mask, so a
//      registration gated on that bit would leave a tear-off window
//      permanently invisible to Phase D's positive-identity router. The
//      `.fullSizeContentView` check lives ONLY in the per-action paths
//      (`applyPolicy`, `reassertImmovable`, and the placer's `apply`),
//      each of which self-heals on the next focus / frame event once the
//      mask lands. The Settings window (standard chrome, never adopted)
//      is filtered by those guards too.
//
//  Phase D's process-wide `ChromeEventRouter` is installed from `start()`
//  and classifies the window an event landed in via `controller(for:)`
//  (the positive-identity seam below). The router owns title-bar drag,
//  double-click-zoom, and the per-press event-time `isMovable` invariant;
//  this controller's `isMovable` policy is the complementary focus / KVO
//  re-assert that covers the non-`sendEvent` server-side drag-init path
//  the per-press monitor never sees.
//

import AppKit

@MainActor
final class WindowChromeController {

    // MARK: - Registry

    /// One controller per live `NSWindow`. Keys are WEAK so an entry
    /// disappears the moment the window deallocs (no manual prune needed
    /// for the common path; `willClose` also removes eagerly). Values are
    /// STRONG so the table keeps the controller alive for the window's
    /// lifetime — nothing else retains it.
    ///
    /// Weak keys are what makes the "computed, not remembered" contract
    /// safe across window churn: a torn-off window that closes can't leave
    /// a stale controller keyed on a reused address, because the key
    /// zeroes with the window. (The controller holds the window WEAKLY —
    /// see `window` below — so it never keeps the key alive itself.)
    static let controllers = NSMapTable<NSWindow, WindowChromeController>(
        keyOptions: [.weakMemory],
        valueOptions: [.strongMemory]
    )

    /// Idempotent registration. Returns the existing controller for the
    /// window if one is already installed, otherwise creates one, runs its
    /// lifecycle (policy + placer + observers), and registers it.
    ///
    /// Registration is UNCONDITIONAL — see the file header. The styleMask
    /// is NOT consulted here; only the per-action paths gate on it.
    @discardableResult
    static func adopt(_ window: NSWindow) -> WindowChromeController {
        if let existing = controllers.object(forKey: window) {
            return existing
        }
        let controller = WindowChromeController(window: window)
        controllers.setObject(controller, forKey: window)
        controller.start()
        return controller
    }

    /// Positive-identity lookup: "is this a Nice chrome window?" answered
    /// by presence in the registry rather than by inspecting the
    /// `styleMask`. This is the seam Phase D's `ChromeEventRouter` uses to
    /// classify the window an event landed in. Nothing in Phase C calls it
    /// yet — it exists so the seam is in place when the router lands.
    static func controller(for window: NSWindow?) -> WindowChromeController? {
        guard let window else { return nil }
        return controllers.object(forKey: window)
    }

    // MARK: - Instance state

    /// Held WEAKLY so the registry's weak key can actually zero — a strong
    /// back-reference here would form a retain cycle (table → controller →
    /// window) that defeats the auto-prune.
    private weak var window: NSWindow?

    /// Owns the three standard window buttons' positions for this window.
    private let trafficLights: TrafficLightPlacer

    /// Notification observer tokens (willClose / didBecomeKey /
    /// didBecomeMain). Removed in `tearDown`.
    private var observerTokens: [NSObjectProtocol] = []

    /// KVO on `isMovable`. Catches the server-side / non-`sendEvent`
    /// window-move-init path (a busy-main-thread drag the WindowServer can
    /// start without dispatching through `NSApp`), which the event-time
    /// monitor in `ChromeEventRouter` never sees. Event-driven, no timer.
    private var isMovableObservation: NSKeyValueObservation?

    private init(window: NSWindow) {
        self.window = window
        self.trafficLights = TrafficLightPlacer(window: window)
    }

    // MARK: - Lifecycle

    /// Runs once from `adopt()` on a freshly-created controller: apply the
    /// movable policy, start the traffic-light placer, and install the
    /// self-healing observers.
    private func start() {
        applyPolicy()
        trafficLights.start()
        // Install the process-wide chrome event router once. Owned here so
        // the single arbitration point goes live the first time any Nice
        // window is adopted; idempotent on every later adopt.
        ChromeEventRouter.installIfNeeded()

        guard let window else { return }
        let center = NotificationCenter.default

        // willClose: tear everything down and drop the registry entry so a
        // reused address can't resolve to this dead controller.
        let closeToken = center.addObserver(
            forName: NSWindow.willCloseNotification,
            object: window,
            queue: .main
        ) { [weak self, weak window] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.tearDown()
                if let window {
                    WindowChromeController.controllers.removeObject(forKey: window)
                }
            }
        }
        observerTokens.append(closeToken)

        // didBecomeKey / didBecomeMain: re-assert the movable policy. This
        // both keeps `isMovable` sticky across AppKit's focus relayout and
        // self-heals the case where the `.fullSizeContentView` mask wasn't
        // ready when `adopt` first ran (the guard in `applyPolicy` no-ops
        // at attach, then this fires once the mask lands).
        for name in [NSWindow.didBecomeKeyNotification,
                     NSWindow.didBecomeMainNotification] {
            let token = center.addObserver(
                forName: name,
                object: window,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.applyPolicy()
                }
            }
            observerTokens.append(token)
        }

        // KVO on isMovable: if anything (AppKit re-finalization, the
        // WindowServer) flips it back to true, snap it false again. Shrinks
        // the unguarded window from "until the next app-dispatched press"
        // to ~KVO latency. The `if isMovable` guard inside `reassertImmovable`
        // keeps this from looping on its own false→false writes.
        isMovableObservation = window.observe(\.isMovable, options: [.new]) { [weak self] win, _ in
            MainActor.assumeIsolated {
                self?.reassertImmovable(win)
            }
        }
    }

    // MARK: - Policy

    /// Adopt-time / focus-time movable policy: under `.hiddenTitleBar` the
    /// whole 52pt top band is the native title bar, so a press-drag
    /// anywhere in it would move the window — the blocker for
    /// drag-to-reorder. `isMovable = false` stops that below the
    /// synthesized-event layer. The `.fullSizeContentView` guard is the
    /// same tell the old `TrafficLightNudger` used to skip the Settings
    /// window; the `if isMovable` guard avoids a redundant write (and a
    /// redundant KVO fire).
    func applyPolicy() {
        guard let window, window.styleMask.contains(.fullSizeContentView) else { return }
        if window.isMovable {
            window.isMovable = false
        }
    }

    /// KVO re-assert. Same policy as `applyPolicy`, but takes the window
    /// the observation handed us (which is the same window) and guards on
    /// `isMovable` so a false→false write can't re-fire the observer and
    /// spin.
    private func reassertImmovable(_ window: NSWindow) {
        if window.styleMask.contains(.fullSizeContentView), window.isMovable {
            window.isMovable = false
        }
    }

    // MARK: - Teardown

    /// Removes every observer and stops the placer. Called from the
    /// willClose observer; also safe to call directly. No timers exist, so
    /// there is nothing else to cancel.
    func tearDown() {
        observerTokens.forEach { NotificationCenter.default.removeObserver($0) }
        observerTokens = []
        isMovableObservation?.invalidate()
        isMovableObservation = nil
        trafficLights.stop()
    }
}
