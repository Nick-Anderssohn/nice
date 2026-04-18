//
//  SidebarView.swift
//  Nice
//
//  Supports expanded (240pt) and collapsed (~52pt) rail modes.
//  The collapsed state is driven by `appState.sidebarCollapsed`.
//

import AppKit
import SwiftUI

struct SidebarView: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if appState.sidebarCollapsed {
            collapsedRail
        } else {
            expandedSidebar
        }
    }

    // MARK: - Collapsed rail

    private var collapsedRail: some View {
        VStack(spacing: 0) {
            RailButton(
                systemImage: "sidebar.left",
                help: "Expand sidebar",
                action: { appState.toggleSidebar() }
            )

            Spacer().frame(height: 6)
            railDivider
            Spacer().frame(height: 6)

            RailButton(
                systemImage: "magnifyingglass",
                help: "Search tabs · ⌘K",
                action: { appState.toggleSidebar() }
            )

            RailButton(
                systemImage: "terminal",
                help: "Terminals",
                isActive: appState.activeTabId == AppState.terminalsTabId,
                action: { appState.selectTab(AppState.terminalsTabId) }
            )

            Spacer().frame(height: 6)
            railDivider
            Spacer().frame(height: 6)

            ScrollView {
                VStack(spacing: 0) {
                    let allTabs = appState.projects.flatMap(\.tabs)
                    ForEach(allTabs) { tab in
                        RailTabDot(tab: tab)
                    }
                }
            }
            .frame(maxHeight: .infinity)

            railDivider
            Spacer().frame(height: 6)

            RailButton(
                systemImage: "gearshape",
                help: "Settings",
                action: { SettingsWindow.open() }
            )
        }
        .padding(.top, 10)
        .padding(.bottom, 8)
        .background(Color.niceBg2(scheme))
    }

    private var railDivider: some View {
        Rectangle()
            .fill(Color.niceLine(scheme))
            .frame(height: 1)
            .padding(.horizontal, 10)
    }

    // MARK: - Expanded sidebar

    private var expandedSidebar: some View {
        VStack(spacing: 0) {
            searchBar
            TerminalsRow()
            tabList
            footer
        }
        .background(Color.niceBg2(scheme))
    }

    // MARK: - Search

    private var searchBar: some View {
        HStack(spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 12, weight: .regular))
                    .foregroundStyle(Color.niceInk3(scheme))
                TextField("Search tabs", text: $appState.sidebarQuery)
                    .textFieldStyle(.plain)
                    .font(.system(size: 12))
                    .foregroundStyle(Color.niceInk(scheme))
                KbdPill(text: "⌘K")
            }
            .padding(.horizontal, 8)
            .frame(height: 26)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.niceBg3(scheme))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(Color.niceLine(scheme), lineWidth: 1)
            )

            SidebarIconButton(systemImage: "sidebar.left", help: "Collapse sidebar") {
                appState.toggleSidebar()
            }
        }
        .padding(.top, 14)
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
    }

    // MARK: - Tab list

    private var tabList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                let projects = appState.filteredProjects
                if projects.isEmpty {
                    Text("No matching tabs")
                        .font(.system(size: 12))
                        .foregroundStyle(Color.niceInk3(scheme))
                        .frame(maxWidth: .infinity)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 20)
                } else {
                    ForEach(projects) { project in
                        ProjectGroup(project: project)
                    }
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
            SidebarIconButton(systemImage: "gearshape", help: "Settings") {
                SettingsWindow.open()
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(Color.niceLine(scheme))
                .frame(height: 1)
        }
    }
}

// MARK: - Rail button (collapsed mode)

private struct RailButton: View {
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    let systemImage: String
    let help: String
    var isActive: Bool = false
    let action: () -> Void

    @State private var hover = false

    private var background: Color {
        if isActive { return Color.niceSel(scheme, accent: tweaks.accent.color) }
        if hover { return Color.niceInk(scheme).opacity(0.07) }
        return .clear
    }

    var body: some View {
        ZStack(alignment: .leading) {
            if isActive {
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .fill(tweaks.accent.color)
                    .frame(width: 2)
                    .padding(.vertical, 6)
                    .offset(x: -6)
            }

            Image(systemName: systemImage)
                .font(.system(size: 14, weight: .regular))
                .foregroundStyle(isActive ? tweaks.accent.color : Color.niceInk2(scheme))
                .frame(width: 36, height: 30)
                .background(
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(background)
                )
        }
        .frame(width: 36, height: 30)
        .frame(maxWidth: .infinity)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { action() }
        .help(help)
    }
}

// MARK: - Rail tab dot (collapsed mode)

private struct RailTabDot: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    let tab: Tab
    @State private var hover = false

    private var isActive: Bool { tab.id == appState.activeTabId }

    private var background: Color {
        if isActive { return Color.niceSel(scheme, accent: tweaks.accent.color) }
        if hover { return Color.niceInk(scheme).opacity(0.06) }
        return .clear
    }

    var body: some View {
        ZStack(alignment: .leading) {
            if isActive {
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .fill(tweaks.accent.color)
                    .frame(width: 2)
                    .padding(.vertical, 5)
                    .offset(x: -6)
            }

            Group {
                if tab.hasClaude {
                    StatusDot(status: tab.status, size: 10)
                } else {
                    Image(systemName: "terminal")
                        .font(.system(size: 10, weight: .regular))
                        .foregroundStyle(Color.niceInk3(scheme))
                        .frame(width: 14, height: 14)
                }
            }
            .frame(width: 36, height: 28)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(background)
            )
        }
        .frame(width: 36, height: 28)
        .frame(maxWidth: .infinity)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { appState.selectTab(tab.id) }
        .help(tab.title)
    }
}

// MARK: - Terminals row (built-in)

private struct TerminalsRow: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme
    @AppStorage("mainTerminalCwd") private var mainTerminalCwd: String = NSHomeDirectory()
    @State private var hover = false

    private var isActive: Bool { appState.activeTabId == AppState.terminalsTabId }

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
        if hover    { return Color.niceInk(scheme).opacity(0.06) }
        return .clear
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "terminal")
                .font(.system(size: 13, weight: .regular))
            Text("Terminals")
                .font(.system(size: 12, weight: isActive ? .semibold : .medium))
            Spacer(minLength: 6)
            Text(cwdDisplayName)
                .font(.system(size: 10))
                .foregroundStyle(Color.niceInk3(scheme))
                .lineLimit(1)
                .truncationMode(.head)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .foregroundStyle(isActive ? Color.niceInk(scheme) : Color.niceInk2(scheme))
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
            mainTerminalCwd = url.path
            appState.restartTerminalsFirstPane(cwd: url.path)
        }
    }
}

// MARK: - Project group

private struct ProjectGroup: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme

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
                .font(.system(size: 10, weight: .semibold))
                .rotationEffect(.degrees(isOpen ? 90 : 0))
                .opacity(0.7)
                .animation(.easeInOut(duration: 0.12), value: isOpen)
            Text(project.name.uppercased())
                .font(.system(size: 11, weight: .semibold))
                .tracking(0.2)
                .foregroundStyle(Color.niceInk2(scheme))
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
    @Environment(\.colorScheme) private var scheme
    let count: Int

    var body: some View {
        Text("\(count)")
            .font(.system(size: 10, weight: .medium))
            .foregroundStyle(Color.niceInk3(scheme))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(
                Capsule().fill(Color.niceInk(scheme).opacity(0.07))
            )
    }
}

// MARK: - Tab row

private struct TabRow: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    let tab: Tab
    @State private var hover = false

    private var isActive: Bool { tab.id == appState.activeTabId }

    private var backgroundColor: Color {
        if isActive { return Color.niceSel(scheme, accent: tweaks.accent.color) }
        if hover    { return Color.niceInk(scheme).opacity(0.06) }
        return .clear
    }

    var body: some View {
        HStack(spacing: 8) {
            if tab.hasClaude {
                StatusDot(status: tab.status)
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).claudeIcon")
            } else {
                Image(systemName: "terminal")
                    .font(.system(size: 10, weight: .regular))
                    .foregroundStyle(Color.niceInk3(scheme))
                    .frame(width: 12, height: 12)
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).terminalIcon")
            }
            Text(tab.title)
                .font(.system(size: 12, weight: isActive ? .semibold : .regular))
                .foregroundStyle(isActive ? Color.niceInk(scheme) : Color.niceInk2(scheme))
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
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sidebar.tab.\(tab.id)")
    }
}

// MARK: - Footer controls

private struct SidebarIconButton: View {
    @Environment(\.colorScheme) private var scheme

    let systemImage: String
    let help: String
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: 14, weight: .regular))
            .foregroundStyle(Color.niceInk2(scheme))
            .frame(width: 24, height: 24)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hover ? Color.niceInk(scheme).opacity(0.08) : Color.clear)
            )
            .contentShape(Rectangle())
            .onHover { hover = $0 }
            .onTapGesture { action() }
            .help(help)
    }
}

// MARK: - Small shared pill

private struct KbdPill: View {
    @Environment(\.colorScheme) private var scheme
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(Color.niceInk3(scheme))
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(
                RoundedRectangle(cornerRadius: 3, style: .continuous)
                    .fill(Color.niceInk(scheme).opacity(0.06))
            )
    }
}

#Preview("Sidebar") {
    SidebarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .frame(width: 240, height: 680)
}
