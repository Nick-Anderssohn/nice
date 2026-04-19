//
//  NiceTerminalView.swift
//  Nice
//
//  Subclass of `LocalProcessTerminalView` that opts the terminal into
//  SwiftTerm's Metal renderer (PR #484, March 2026) once it's attached
//  to a window. The Metal path requires window attachment because the
//  `MTKView` it installs needs a live `CAMetalLayer`; calling
//  `setUseMetal(true)` from `init` would crash.
//
//  The current GPU preference is read through a closure rather than
//  captured by value so a Settings toggle can flip it live without
//  rebuilding the view. `TabPtySession` installs the closure (it points
//  at AppState's cached value) and calls `applyGpuPreference()` on every
//  pane when the user flips the setting.
//

import AppKit
import SwiftTerm

@MainActor
final class NiceTerminalView: LocalProcessTerminalView {
    /// Reads the live "GPU rendering" preference. `nil` means "no
    /// session has wired this up yet" — treated as on, matching the
    /// `Tweaks.gpuRendering` default.
    var gpuPreferenceProvider: (() -> Bool)?

    /// Re-evaluates the GPU preference and toggles the Metal renderer
    /// to match. No-op when the view isn't yet in a window — the
    /// `viewDidMoveToWindow` override applies the current preference
    /// once attachment happens. Idempotent: SwiftTerm's `setUseMetal`
    /// short-circuits when the renderer is already in the requested state.
    func applyGpuPreference() {
        guard window != nil else { return }
        let desired = gpuPreferenceProvider?() ?? true
        do {
            try setUseMetal(desired)
        } catch {
            // Metal unavailable (deviceUnavailable on VMs / CI).
            // Stay on the CG path silently — `setUseMetal(false)`
            // is also a no-op when Metal was never enabled.
            NSLog("NiceTerminalView: Metal renderer unavailable, falling back to CoreGraphics: \(error)")
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        applyGpuPreference()
    }
}
