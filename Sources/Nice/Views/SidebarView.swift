//
//  SidebarView.swift
//  Nice
//
//  Phase 2 sidebar. Ported from
//  /tmp/nice-design/nice/project/nice/sidebar.jsx. Structure:
//
//      VStack(spacing: 0) {
//          Search bar
//          MainTerminalRow
//          ScrollView { ProjectGroup*  }
//          Footer (New tab, Settings)
//      }
//
//  Mock data only — real processes / keyboard shortcuts come later.
//

import AppKit
import SwiftUI

struct SidebarView: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            searchBar
            MainTerminalRow()
            tabList
            footer
        }
        .background(Color.niceBg2(scheme))
    }

    // MARK: - Search

    private var searchBar: some View {
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
            FooterBtn(
                systemImage: "plus",
                label: "New tab",
                shortcut: "⌘T"
            ) {
                appState.newTab()
            }
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

// MARK: - Main terminal row

private struct MainTerminalRow: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme
    @AppStorage("mainTerminalCwd") private var mainTerminalCwd: String = NSHomeDirectory()
    @State private var hover = false

    private var isActive: Bool { appState.activeTabId == nil }

    /// Collapse `$HOME` to `~` per the JSX's right-side label shape.
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
            Text("Main terminal")
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
            appState.selectMainTerminal()
        }
        .contextMenu {
            Button("Change directory…") {
                pickDirectory()
            }
        }
    }

    private func pickDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        if panel.runModal() == .OK, let url = panel.url {
            mainTerminalCwd = url.path
            // Phase 4: re-root the main terminal's zsh at the new path.
            appState.restartMainTerminal(cwd: url.path)
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
        .padding(.horizontal, 6) // wraps the inner horizontal 10, mirrors `margin: '0 6px'`
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
            StatusDot(status: tab.status)
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
    }
}

// MARK: - Footer controls

private struct FooterBtn: View {
    @Environment(\.colorScheme) private var scheme

    let systemImage: String
    let label: String
    let shortcut: String
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: systemImage)
                .font(.system(size: 14, weight: .regular))
            Text(label)
                .font(.system(size: 12, weight: .medium))
            Text(shortcut)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(Color.niceInk3(scheme))
                .padding(.leading, 2)
        }
        .foregroundStyle(Color.niceInk2(scheme))
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(hover ? Color.niceInk(scheme).opacity(0.08) : Color.clear)
        )
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { action() }
    }
}

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
