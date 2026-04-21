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
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        expandedSidebar
    }

    // MARK: - Expanded sidebar

    private var expandedSidebar: some View {
        VStack(spacing: 0) {
            tabList
            footer
        }
    }

    // MARK: - Tab list

    private var tabList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(appState.projects) { project in
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

// MARK: - Project group

private struct ProjectGroup: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let project: Project
    @State private var isOpen: Bool = true
    @State private var headerHover: Bool = false

    private var isTerminalsGroup: Bool {
        project.id == AppState.terminalsProjectId
    }

    /// Always show the `+` for the Terminals group (so the user can
    /// re-add after emptying it) but only reveal it on hover for
    /// ordinary project groups — keeps the sidebar clean by default.
    private var showAddButton: Bool {
        isTerminalsGroup || headerHover
    }

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
        // Make the whole project group addressable by UI tests even
        // when its add button is hover-gated (non-Terminals groups
        // hide it at opacity 0, which also strips accessibility).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sidebar.group.\(project.id)")
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.system(size: fontSettings.sidebarSize(10), weight: .semibold))
                .rotationEffect(.degrees(isOpen ? 90 : 0))
                .opacity(0.7)
                .animation(.easeInOut(duration: 0.12), value: isOpen)
                .contentShape(Rectangle())
                .onTapGesture { isOpen.toggle() }
            Text(project.name.uppercased())
                .font(.system(size: fontSettings.sidebarSize(11), weight: .semibold))
                .tracking(0.2)
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .contentShape(Rectangle())
                .onTapGesture { isOpen.toggle() }
            Spacer(minLength: 4)
            CountPill(count: project.tabs.count)
            AddTabButton(
                accessibilityId: "sidebar.group.\(project.id).add",
                help: isTerminalsGroup ? "New terminal tab" : "New Claude tab"
            ) {
                if isTerminalsGroup {
                    _ = appState.createTerminalTab()
                } else {
                    _ = appState.createClaudeTabInProject(projectId: project.id)
                }
            }
            .opacity(showAddButton ? 1 : 0)
            .animation(.easeInOut(duration: 0.12), value: showAddButton)
            .allowsHitTesting(showAddButton)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { headerHover = $0 }
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

// MARK: - Group "+" button

private struct AddTabButton: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let accessibilityId: String
    let help: String
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Button(action: action) {
            Image(systemName: "plus")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .frame(width: 18, height: 18)
                .background(
                    RoundedRectangle(cornerRadius: 4, style: .continuous)
                        .fill(
                            hover
                                ? Color.niceInk(scheme, palette).opacity(0.10)
                                : .clear
                        )
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .help(help)
        .accessibilityIdentifier(accessibilityId)
        .accessibilityLabel(help)
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
        .accessibilityIdentifier(accessibilityIdentifier)
    }

    /// The Main terminal tab keeps the legacy `sidebar.terminals`
    /// identifier so UI tests that targeted the old single top-level
    /// terminals row continue to locate it. All other tabs use the
    /// standard `sidebar.tab.<id>` form.
    private var accessibilityIdentifier: String {
        if tab.id == AppState.mainTerminalTabId {
            return "sidebar.terminals"
        }
        return "sidebar.tab.\(tab.id)"
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

#Preview("Sidebar") {
    SidebarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .environmentObject(FontSettings())
        .frame(width: 240, height: 680)
}
