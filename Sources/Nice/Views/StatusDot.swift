//
//  StatusDot.swift
//  Nice
//
//  Port of the `StatusDot` component from
//  /tmp/nice-design/nice/project/nice/sidebar.jsx. An 8pt circle whose
//  colour maps to TabStatus. When `thinking`, the dot breathes (0.5↔1.0
//  opacity at 1.4s) and an outer ring scales 1.0→1.6 while fading out
//  (1.6s). Keyframes come from the `@keyframes pulse-ring` and
//  `@keyframes status-pulse` rules in Nice.html.
//

import SwiftUI

struct StatusDot: View {
    @Environment(\.colorScheme) private var scheme

    let status: TabStatus
    var size: CGFloat = 8
    /// Disables the `thinking` pulse in previews/snapshots.
    var pulsePaused: Bool = false
    /// Suppresses the `waiting` pulse when the user is already looking at
    /// the owning tab — the pulse exists to attract attention, so it's
    /// only needed when the waiting state is out of sight.
    var suppressWaitingPulse: Bool = false

    private var baseColor: Color {
        switch status {
        case .thinking:
            return .niceAccent
        case .waiting:
            // oklch(0.65 0.14 250) -> sRGB approximation per the JSX.
            return Color(.sRGB, red: 0.48, green: 0.58, blue: 0.86, opacity: 1)
        case .idle:
            return .niceInk3(scheme)
        }
    }

    private var accessibilityLabel: String {
        switch status {
        case .thinking: return "Thinking"
        case .waiting:  return "Waiting for input"
        case .idle:     return "Idle"
        }
    }

    private var shouldPulse: Bool {
        switch status {
        case .thinking: return true
        case .waiting:  return !suppressWaitingPulse
        case .idle:     return false
        }
    }

    var body: some View {
        ZStack {
            if shouldPulse {
                PulsingLayer(
                    status: status,
                    baseColor: baseColor,
                    size: size,
                    pulsePaused: pulsePaused
                )
            } else {
                Circle()
                    .fill(baseColor)
                    .frame(width: size, height: size)
            }
        }
        .frame(width: size + 4, height: size + 4)
        .accessibilityLabel(accessibilityLabel)
    }
}

/// The animated ring + breathing dot. Isolated so that each time
/// `shouldPulse` transitions false → true a fresh instance is inserted
/// with `pulsing` starting at `false`; `onAppear` then flips it to `true`,
/// giving SwiftUI the value change it needs to trigger the repeat-forever
/// animation. Without this isolation, a single persistent `@State` for
/// `pulsing` stays `true` forever and the animation never retriggers.
private struct PulsingLayer: View {
    let status: TabStatus
    let baseColor: Color
    let size: CGFloat
    let pulsePaused: Bool

    @State private var pulsing: Bool = false

    var body: some View {
        ZStack {
            Circle()
                .fill(baseColor)
                .frame(width: size + 4, height: size + 4)
                .scaleEffect(pulsing ? (status == .waiting ? 2.0 : 1.6) : 1.0)
                .opacity(pulsing ? 0.0 : (status == .waiting ? 0.7 : 0.6))
                .animation(
                    pulsePaused
                        ? nil
                        : .easeOut(duration: status == .waiting ? 1.2 : 1.6)
                            .repeatForever(autoreverses: false),
                    value: pulsing
                )

            Circle()
                .fill(baseColor)
                .frame(width: size, height: size)
                .opacity(pulsing ? 1.0 : (status == .waiting ? 0.4 : 0.5))
                .animation(
                    pulsePaused
                        ? nil
                        : .easeInOut(duration: status == .waiting ? 0.9 : 0.7)
                            .repeatForever(autoreverses: true),
                    value: pulsing
                )
        }
        .onAppear {
            if !pulsePaused {
                pulsing = true
            }
        }
    }
}

#Preview("States") {
    HStack(spacing: 24) {
        StatusDot(status: .thinking)
        StatusDot(status: .waiting)
        StatusDot(status: .idle)
    }
    .padding(40)
}
