//
//  SidebarView.swift
//  Nice
//
//  The expanded 240pt sidebar column. The collapsed state is handled
//  upstream in `AppShellView` as a small top-bar cap, so this view
//  is only instantiated when `appState.sidebarCollapsed == false`.
//
//  The column background is owned upstream by `AppShellView` via
//  `SidebarBackground` (flat panel for `.nice`, wallpaper-tinted
//  `NSVisualEffectView` for `.macOS`); this view paints no background
//  of its own so vibrancy shows through.
//

import AppKit
import SwiftUI

struct SidebarView: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        expandedSidebar
    }

    // MARK: - Expanded sidebar

    private var expandedSidebar: some View {
        VStack(spacing: 0) {
            searchBar
            TerminalsRow()
            tabList
            footer
        }
    }

    // MARK: - Search

    private var searchBar: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: fontSettings.sidebarSize(12), weight: .regular))
                .foregroundStyle(Color.niceInk3(scheme, palette))
            TextField("Search tabs", text: $appState.sidebarQuery)
                .textFieldStyle(.plain)
                .font(.system(size: fontSettings.sidebarSize(12)))
                .foregroundStyle(Color.niceInk(scheme, palette))
            KbdPill(text: "⌘K")
        }
        .padding(.horizontal, 8)
        .frame(height: 26)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(Color.niceBg3(scheme, palette))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(Color.niceLine(scheme, palette), lineWidth: 1)
        )
        .padding(.top, 8)
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
    }

    // MARK: - Tab list

    private var tabList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(appState.filteredProjects) { project in
                    ProjectGroup(project: project)
                }
            }
            .padding(.vertical, 10)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: 4) {
            Spacer(minLength: 0)
            SidebarIconButton(
                systemImage: "gearshape",
                help: "Settings",
                accessibilityId: "sidebar.settings"
            ) {
                openSettings()
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
    }
}

// MARK: - Terminals row (built-in)

private struct TerminalsRow: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @State private var hover = false

    private var isActive: Bool { appState.activeTabId == AppState.terminalsTabId }

    /// The Main Terminal cwd is per-window state now, read directly
    /// from the built-in tab so two windows with different cwds don't
    /// share a label.
    private var mainTerminalCwd: String { appState.terminalsTab.cwd }

    /// Collapse `$HOME` to `~` for the right-side cwd label.
    private var cwdDisplayName: String {
        let home = NSHomeDirectory()
        if mainTerminalCwd == home { return "~" }
        if mainTerminalCwd.hasPrefix(home + "/") {
            return "~" + mainTerminalCwd.dropFirst(home.count)
        }
        return mainTerminalCwd
    }

    private var backgroundColor: Color {
        if isActive { return Color.niceSel(scheme, accent: tweaks.accent.color) }
        if hover    { return Color.niceInk(scheme, palette).opacity(0.06) }
        return .clear
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "terminal")
                .font(.system(size: fontSettings.sidebarSize(13), weight: .regular))
            Text("Terminals")
                .font(.system(size: fontSettings.sidebarSize(12), weight: isActive ? .semibold : .medium))
            Spacer(minLength: 6)
            Text(cwdDisplayName)
                .font(.system(size: fontSettings.sidebarSize(10)))
                .foregroundStyle(Color.niceInk3(scheme, palette))
                .lineLimit(1)
                .truncationMode(.head)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .foregroundStyle(isActive ? Color.niceInk(scheme, palette) : Color.niceInk2(scheme, palette))
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(backgroundColor)
        )
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture {
            appState.selectTab(AppState.terminalsTabId)
        }
        .contextMenu {
            Button("Change directory…") {
                pickDirectory()
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("sidebar.terminals")
    }

    private func pickDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        if panel.runModal() == .OK, let url = panel.url {
            // `restartTerminalsFirstPane` updates `terminalsTab.cwd`
            // which the AppShellView bridges back to `@SceneStorage`.
            appState.restartTerminalsFirstPane(cwd: url.path)
        }
    }
}

// MARK: - Project group

private struct ProjectGroup: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let project: Project
    @State private var isOpen: Bool = true

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if isOpen {
                ForEach(project.tabs) { tab in
                    TabRow(tab: tab)
                }
            }
        }
        .padding(.bottom, 4)
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.system(size: fontSettings.sidebarSize(10), weight: .semibold))
                .rotationEffect(.degrees(isOpen ? 90 : 0))
                .opacity(0.7)
                .animation(.easeInOut(duration: 0.12), value: isOpen)
            Text(project.name.uppercased())
                .font(.system(size: fontSettings.sidebarSize(11), weight: .semibold))
                .tracking(0.2)
                .foregroundStyle(Color.niceInk2(scheme, palette))
            Spacer(minLength: 4)
            CountPill(count: project.tabs.count)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onTapGesture {
            isOpen.toggle()
        }
    }
}

private struct CountPill: View {
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    let count: Int

    var body: some View {
        Text("\(count)")
            .font(.system(size: fontSettings.sidebarSize(10), weight: .medium))
            .foregroundStyle(Color.niceInk3(scheme, palette))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(
                Capsule().fill(Color.niceInk(scheme, palette).opacity(0.07))
            )
    }
}

// MARK: - Tab row

private struct TabRow: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let tab: Tab
    @State private var hover = false

    private var isActive: Bool { tab.id == appState.activeTabId }

    private var backgroundColor: Color {
        if isActive { return Color.niceSel(scheme, accent: tweaks.accent.color) }
        if hover    { return Color.niceInk(scheme, palette).opacity(0.06) }
        return .clear
    }

    var body: some View {
        HStack(spacing: 8) {
            if tab.hasClaude {
                StatusDot(
                    status: tab.status,
                    suppressWaitingPulse: tab.waitingAcknowledged
                )
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).claudeIcon")
            } else {
                Image(systemName: "terminal")
                    .font(.system(size: fontSettings.sidebarSize(10), weight: .regular))
                    .foregroundStyle(Color.niceInk3(scheme, palette))
                    .frame(width: 12, height: 12)
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).terminalIcon")
            }
            Text(tab.title)
                .font(.system(size: fontSettings.sidebarSize(12), weight: isActive ? .semibold : .regular))
                .foregroundStyle(isActive ? Color.niceInk(scheme, palette) : Color.niceInk2(scheme, palette))
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.leading, 22)
        .padding(.trailing, 10)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(backgroundColor)
        )
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture {
            appState.selectTab(tab.id)
        }
        .contextMenu {
            Button("Close Tab") {
                appState.requestCloseTab(tabId: tab.id)
            }
            .accessibilityIdentifier("sidebar.tab.\(tab.id).closeTab")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sidebar.tab.\(tab.id)")
    }
}

// MARK: - Footer controls

private struct SidebarIconButton: View {
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let systemImage: String
    let help: String
    var accessibilityId: String? = nil
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: fontSettings.sidebarSize(14), weight: .regular))
            .foregroundStyle(Color.niceInk2(scheme, palette))
            .frame(width: 24, height: 24)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hover ? Color.niceInk(scheme, palette).opacity(0.08) : Color.clear)
            )
            .contentShape(Rectangle())
            .onHover { hover = $0 }
            .onTapGesture { action() }
            .help(help)
            .accessibilityIdentifier(accessibilityId ?? "")
    }
}

// MARK: - Small shared pill

private struct KbdPill: View {
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: fontSettings.sidebarSize(10), design: .monospaced))
            .foregroundStyle(Color.niceInk3(scheme, palette))
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(
                RoundedRectangle(cornerRadius: 3, style: .continuous)
                    .fill(Color.niceInk(scheme, palette).opacity(0.06))
            )
    }
}

#Preview("Sidebar") {
    SidebarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .environmentObject(FontSettings())
        .frame(width: 240, height: 680)
}
