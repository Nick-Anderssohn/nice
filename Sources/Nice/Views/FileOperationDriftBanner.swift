//
//  FileOperationDriftBanner.swift
//  Nice
//
//  Transient bottom-anchored banner that surfaces messages from
//  `FileOperationHistory.lastDriftMessage` — drift on undo/redo, or
//  the heads-up that a cross-window undo applied in a closed window
//  the user can't see directly. Auto-dismisses after a short delay.
//

import SwiftUI

struct FileOperationDriftBanner: View {
    @ObservedObject var history: FileOperationHistory
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    /// How long a message stays on screen before it self-clears.
    /// Long enough to read a one-line sentence, short enough that
    /// the next op's banner doesn't pile on top.
    private static let visibleDuration: TimeInterval = 3.5

    var body: some View {
        if let message = history.lastDriftMessage {
            banner(message)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .task(id: message) {
                    try? await Task.sleep(nanoseconds: UInt64(Self.visibleDuration * 1_000_000_000))
                    if history.lastDriftMessage == message {
                        history.lastDriftMessage = nil
                    }
                }
        }
    }

    private func banner(_ message: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.uturn.backward")
                .font(.system(size: 11, weight: .semibold))
            Text(message)
                .font(.system(size: 12))
                .lineLimit(2)
                .multilineTextAlignment(.leading)
            Spacer(minLength: 0)
            Button(action: { history.lastDriftMessage = nil }) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .padding(4)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Dismiss")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .foregroundStyle(Color.niceInk(scheme, palette))
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Color.niceInk(scheme, palette).opacity(0.08))
                .overlay(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .stroke(Color.niceInk(scheme, palette).opacity(0.18), lineWidth: 1)
                )
        )
        .frame(maxWidth: 480)
        .padding(.horizontal, 14)
        .padding(.bottom, 14)
        .accessibilityIdentifier("fileBrowser.driftBanner")
    }
}
