//
//  LaunchingOverlay.swift
//  Nice
//
//  Centered "Launching…" placeholder drawn on top of a freshly-spawned
//  pane whose child process has been silent for more than the
//  `AppState.launchOverlayGraceSeconds` window. Gives the user feedback
//  for slow startups — e.g. `claude -w foo` against a repo whose
//  post-checkout hooks take 30+ s — instead of showing a bare blinking
//  cursor with no context.
//

import SwiftUI

struct LaunchingOverlay: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    /// "Launching claude…" or "Launching terminal…" — picked by the
    /// caller based on the pane kind.
    let title: String
    /// User-friendly command string, e.g. `claude -w some-worktree`.
    /// Rendered dimmed and monospaced under the headline.
    let command: String

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 8) {
                StatusDot(status: .thinking)
                Text(title)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Color.niceInk(scheme, palette))
            }
            Text(command)
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(Color.niceInk3(scheme, palette))
                .lineLimit(1)
                .truncationMode(.middle)
                .padding(.horizontal, 20)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .allowsHitTesting(false)
    }
}

#Preview("Launching claude") {
    LaunchingOverlay(title: "Launching claude…", command: "claude -w some-worktree")
        .frame(width: 600, height: 400)
        .background(Color.black)
}
