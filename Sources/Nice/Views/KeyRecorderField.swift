//
//  KeyRecorderField.swift
//  Nice
//
//  Recorder UI for a single `ShortcutAction`. Two visual states:
//
//  • Resting — renders the bound combo as a `KeyPills`, with a "Reset"
//    button that surfaces only when the binding differs from default.
//    Tapping the pill area enters recording.
//
//  • Recording — pills swap for "Press a combo…" capsule. A local
//    `NSEvent.keyDown` monitor is installed; the next non-Escape key
//    press is captured. If the resulting combo collides with another
//    action's binding, a conflict warning surfaces with Replace / Cancel
//    buttons; otherwise the binding is committed and recording exits
//    immediately. Esc cancels.
//
//  The global `KeyboardShortcutMonitor` is told to stand down (via the
//  static `isRecording` flag) for the duration of recording, so the user's
//  keystrokes don't fire actions while we're trying to capture them.
//
//  We keep the recorder's mutable state on a `@State` coordinator
//  so the NSEvent monitor lifecycle (install on `start`, remove on
//  `teardown`) lives on a stable owner — directly storing the monitor
//  token in `@State` would be brittle across redraws.
//

import AppKit
import Carbon.HIToolbox
import SwiftUI

struct KeyRecorderField: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(KeyboardShortcuts.self) private var shortcuts

    let action: ShortcutAction

    @State private var coordinator = RecorderCoordinator()

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            primaryRow
            if coordinator.recording,
               let conflict = coordinator.conflictAction,
               let pending = coordinator.pendingCombo {
                conflictRow(other: conflict, combo: pending)
            }
        }
        .onDisappear { coordinator.teardown() }
    }

    // MARK: - Primary row

    @ViewBuilder
    private var primaryRow: some View {
        if coordinator.recording {
            recordingCapsule
        } else {
            HStack(spacing: 8) {
                pillsButton
                if !shortcuts.isAtDefault(action) {
                    Button("Reset") { shortcuts.resetToDefault(action) }
                        .controlSize(.small)
                }
            }
        }
    }

    private var pillsButton: some View {
        let pills = shortcuts.binding(for: action)?.displayPills
        return Group {
            if let pills {
                KeyPills(keys: pills)
            } else {
                Text("Not bound")
                    .font(.system(size: 11.5))
                    .foregroundStyle(Color.niceInk3(scheme, palette))
            }
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .onTapGesture { coordinator.start(action: action, shortcuts: shortcuts) }
        .help("Click to record a new combo")
    }

    private var recordingCapsule: some View {
        HStack(spacing: 8) {
            if let pending = coordinator.pendingCombo {
                KeyPills(keys: pending.displayPills)
            }
            Text(coordinator.pendingCombo == nil
                 ? "Press a combo… (Esc to cancel)"
                 : "Press another combo, or resolve below")
                .font(.system(size: 11.5))
                .foregroundStyle(Color.niceInk2(scheme, palette))
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(Color.niceBg3(scheme, palette))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(Color.niceLineStrong(scheme, palette), lineWidth: 1)
        )
    }

    // MARK: - Conflict row

    private func conflictRow(other: ShortcutAction, combo: KeyCombo) -> some View {
        HStack(spacing: 8) {
            Text("Already used by \(other.label)")
                .font(.system(size: 11.5))
                .foregroundStyle(Color.niceInk2(scheme, palette))
            Button("Replace") { coordinator.replaceConflict() }
                .controlSize(.small)
            Button("Cancel") { coordinator.teardown() }
                .controlSize(.small)
        }
    }
}

// MARK: - Coordinator

/// Owns the recording state machine and the live `NSEvent` monitor token.
/// Lifted out of the view so the monitor's install/teardown hangs off a
/// stable identity (`@State` retained reference type) rather than
/// transient value-type `@State`.
@MainActor
@Observable
final class RecorderCoordinator {
    var recording: Bool = false
    /// When non-nil during recording, the user pressed a combo that
    /// conflicts with another action — the recorder is awaiting Replace
    /// or Cancel before committing.
    var pendingCombo: KeyCombo?
    var conflictAction: ShortcutAction?

    @ObservationIgnored
    private var monitor: Any?
    @ObservationIgnored
    private weak var shortcuts: KeyboardShortcuts?
    @ObservationIgnored
    private var action: ShortcutAction?

    /// Begin recording for `action`. Suspends the global monitor and
    /// installs a higher-priority local one. Idempotent — calling while
    /// already recording is a no-op (the existing session keeps running).
    func start(action: ShortcutAction, shortcuts: KeyboardShortcuts) {
        guard !recording else { return }
        self.action = action
        self.shortcuts = shortcuts
        recording = true
        pendingCombo = nil
        conflictAction = nil
        KeyboardShortcutMonitor.isRecording = true

        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            // Hop to MainActor and return Bool (Sendable) rather than
            // NSEvent? (non-Sendable) so the assumeIsolated block doesn't
            // try to ferry NSEvent across the isolation boundary.
            let consumed = MainActor.assumeIsolated {
                self?.handle(event: event) ?? false
            }
            return consumed ? nil : event
        }
    }

    /// Resolve a pending conflict by clearing the other action's binding
    /// and committing the new combo to the recording action. No-op if
    /// there's no pending conflict.
    func replaceConflict() {
        guard let action,
              let combo = pendingCombo,
              let conflict = conflictAction,
              let shortcuts
        else { return }
        shortcuts.setBinding(nil, for: conflict)
        shortcuts.setBinding(combo, for: action)
        teardown()
    }

    /// Stop recording without committing pending changes. Safe to call
    /// from `onDisappear`, the Cancel button, or after a successful
    /// commit. Idempotent.
    func teardown() {
        if let monitor {
            NSEvent.removeMonitor(monitor)
        }
        monitor = nil
        recording = false
        pendingCombo = nil
        conflictAction = nil
        action = nil
        KeyboardShortcutMonitor.isRecording = false
    }

    /// Returns `true` if the event was consumed (caller swallows). We
    /// always consume during recording so the captured keystroke doesn't
    /// propagate to the terminal or Settings field.
    private func handle(event: NSEvent) -> Bool {
        guard recording, let action, let shortcuts else { return true }
        if event.isARepeat { return true }

        let modsRaw = event.modifierFlags
            .intersection(.deviceIndependentFlagsMask)
            .rawValue

        // Plain Esc cancels. Esc with modifiers is a legitimate combo
        // (rare, but we won't steal it).
        if event.keyCode == UInt16(kVK_Escape) && modsRaw == 0 {
            teardown()
            return true
        }

        let combo = KeyCombo(keyCode: event.keyCode, modifierFlags: event.modifierFlags)
        if let other = shortcuts.conflictingAction(for: combo, excluding: action) {
            pendingCombo = combo
            conflictAction = other
        } else {
            shortcuts.setBinding(combo, for: action)
            teardown()
        }
        return true
    }
}
