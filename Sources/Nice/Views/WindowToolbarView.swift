//
//  WindowToolbarView.swift
//  Nice
//
//  Port of the `WindowToolbar` function in
//  /tmp/nice-design/nice/project/nice/app.jsx (lines ~150–207).
//
//  The mic button from the original is intentionally omitted — this bar
//  is brand + active-tab-info only for Phase 3. The window uses the
//  `.hiddenTitleBar` style, so the native traffic lights float over the
//  leading Spacer reserved in the HStack (72pt).
//

import SwiftUI

struct WindowToolbarView: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme
    @AppStorage("mainTerminalCwd") private var mainTerminalCwd: String = NSHomeDirectory()

    // MARK: - Derived info

    /// The active tab, or nil if the Main terminal row is selected.
    private var activeTab: Tab? {
        guard let id = appState.activeTabId else { return nil }
        for project in appState.projects {
            if let hit = project.tabs.first(where: { $0.id == id }) {
                return hit
            }
        }
        return nil
    }

    private var isMainTerminal: Bool {
        appState.activeTabId == nil
    }

    /// Collapses `$HOME` (and `$HOME/...`) to `~` in the same shape the
    /// sidebar uses for its right-side path label.
    private var cwdDisplayName: String {
        let home = NSHomeDirectory()
        if mainTerminalCwd == home { return "~" }
        if mainTerminalCwd.hasPrefix(home + "/") {
            return "~" + mainTerminalCwd.dropFirst(home.count)
        }
        return mainTerminalCwd
    }

    private var title: String {
        if isMainTerminal { return "Main terminal" }
        return activeTab?.title ?? "Nice"
    }

    private var subtitle: String {
        if isMainTerminal {
            return "zsh · \(cwdDisplayName)"
        }
        guard let tab = activeTab else { return "" }
        if let branch = tab.branch {
            return "\(tab.cwd) · \(branch)"
        }
        return tab.cwd
    }

    // MARK: - Body

    var body: some View {
        HStack(spacing: 10) {
            // Reserve space for the native traffic lights (top-left of the
            // hidden-title-bar window). 72pt matches the close/min/zoom
            // triad + its own leading inset under `.hiddenTitleBar`.
            Spacer().frame(width: 72)

            Logo()

            Text("Nice")
                .font(.system(size: 13, weight: .bold))
                .tracking(-0.2)
                .foregroundStyle(Color.niceInk(scheme))

            MCPChip()

            // Vertical separator — equivalent to the CSS div with
            // width:1, height:20, margin: 0 6px.
            Rectangle()
                .fill(Color.niceLine(scheme))
                .frame(width: 1, height: 20)
                .padding(.horizontal, 6)

            // Active-tab block. StatusDot only when a real tab is selected;
            // the Main terminal row is represented by just the text.
            if let tab = activeTab, !isMainTerminal {
                StatusDot(status: tab.status)
            }

            VStack(alignment: .leading, spacing: 0) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.niceInk(scheme))
                    .lineLimit(1)
                    .truncationMode(.tail)
                if !subtitle.isEmpty {
                    Text(subtitle)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.niceInk3(scheme))
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .frame(maxWidth: 360, alignment: .leading)

            Spacer(minLength: 0)
        }
        .padding(.leading, 14)
        .padding(.trailing, 10)
        .frame(height: 52)
        .frame(maxWidth: .infinity)
        .background(Color.niceChrome(scheme))
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.niceLine(scheme))
                .frame(height: 1)
        }
    }
}

// MARK: - MCP chip

/// Small accent-coloured "MCP" pill that sits next to the brand mark.
/// Mirrors the inline span in app.jsx (lines ~173–178) with its
/// `--accent-soft` background (accent at 18% alpha). Reads the accent
/// from `Tweaks` so it repaints when the user picks a new swatch.
private struct MCPChip: View {
    @EnvironmentObject private var tweaks: Tweaks

    var body: some View {
        let accent = tweaks.accent.color
        Text("MCP")
            .font(.system(size: 9.5, weight: .bold))
            .tracking(0.3)
            .foregroundStyle(accent)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(accent.opacity(0.18))
            )
    }
}

// MARK: - Previews

#Preview("Toolbar — tab selected (light)") {
    WindowToolbarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.light)
}

#Preview("Toolbar — main terminal (dark)") {
    let state = AppState()
    state.selectMainTerminal()
    return WindowToolbarView()
        .environmentObject(state)
        .environmentObject(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.dark)
}
