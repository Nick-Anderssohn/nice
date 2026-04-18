//
//  SettingsView.swift
//  Nice
//
//  Phase 5: preferences window content, bound to ⌘, via the app's
//  `Settings { … }` scene. Port of the React mock in
//  /tmp/nice-design/nice/project/nice/settings.jsx with the Voice
//  section intentionally dropped (not a product surface here).
//
//  Layout:
//    HStack:
//      - 160pt left rail (niceBg2, right border, section list)
//      - content area (18/24 padding, scrollable, panel per selection)
//
//  The accent swatches live in the Appearance tab and write through the
//  `Tweaks` environment object so the rest of the app (Logo, MCP chip,
//  selection tints…) repaints live the moment a new swatch is picked.
//

import AppKit
import ServiceManagement
import SwiftUI

// MARK: - Section enum

enum SettingsSection: String, CaseIterable, Identifiable {
    case general     = "General"
    case shortcuts   = "Shortcuts"
    case mcp         = "MCP"
    case appearance  = "Appearance"
    case about       = "About"

    var id: String { rawValue }
    var label: String { rawValue }
}

// MARK: - Root

struct SettingsView: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    @State private var active: SettingsSection = .general

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            content
        }
        .frame(width: 640, height: 440)
        .background(Color.nicePanel(scheme))
        .accessibilityIdentifier("settings.root")
    }

    // MARK: Left rail

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(SettingsSection.allCases) { section in
                SettingsSectionRow(
                    section: section,
                    active: active == section,
                    accent: tweaks.accent.color
                ) {
                    active = section
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 10)
        .frame(width: 160)
        .background(Color.niceBg2(scheme))
        .overlay(alignment: .trailing) {
            Rectangle()
                .fill(Color.niceLine(scheme))
                .frame(width: 1)
        }
    }

    // MARK: Right panel

    private var content: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                switch active {
                case .general:    GeneralPane()
                case .shortcuts:  ShortcutsPane()
                case .mcp:        MCPPane()
                case .appearance: AppearancePane()
                case .about:      AboutPane()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 24)
            .padding(.vertical, 18)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.nicePanel(scheme))
    }
}

// MARK: - Sidebar row

private struct SettingsSectionRow: View {
    @Environment(\.colorScheme) private var scheme

    let section: SettingsSection
    let active: Bool
    let accent: Color
    let action: () -> Void

    var body: some View {
        Text(section.label)
            .font(.system(size: 12.5, weight: active ? .semibold : .medium))
            .foregroundStyle(active ? Color.niceInk(scheme) : Color.niceInk2(scheme))
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(active
                          ? Color.niceSel(scheme, accent: accent)
                          : Color.clear)
            )
            .contentShape(Rectangle())
            .onTapGesture { action() }
    }
}

// MARK: - General pane

private struct GeneralPane: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme

    @AppStorage("launchAtLogin") private var launchAtLogin: Bool = false
    @AppStorage("mainTerminalCwd") private var mainTerminalCwd: String = NSHomeDirectory()

    private var cwdDisplayName: String {
        let home = NSHomeDirectory()
        if mainTerminalCwd == home { return "~" }
        if mainTerminalCwd.hasPrefix(home + "/") {
            return "~" + mainTerminalCwd.dropFirst(home.count)
        }
        return mainTerminalCwd
    }

    /// Custom binding so flipping the Toggle calls SMAppService.register/
    /// unregister instead of just writing @AppStorage. If the call
    /// throws, we snap the UI back to whatever SMAppService reports —
    /// common in dev builds where the app isn't in /Applications.
    private var launchAtLoginBinding: Binding<Bool> {
        Binding(
            get: { launchAtLogin },
            set: { newValue in
                do {
                    if newValue {
                        try SMAppService.mainApp.register()
                    } else {
                        try SMAppService.mainApp.unregister()
                    }
                    launchAtLogin = newValue
                } catch {
                    NSLog("SMAppService \(newValue ? "register" : "unregister") failed: \(error)")
                    launchAtLogin = SMAppService.mainApp.status == .enabled
                }
            }
        )
    }

    var body: some View {
        SettingTitle("General")
            .onAppear {
                // Sync the stored flag with reality — the user may have
                // toggled the Login Items entry via System Settings, or
                // a previous register() call may have silently failed.
                launchAtLogin = SMAppService.mainApp.status == .enabled
            }

        SettingRow(label: "Launch at login") {
            Toggle("", isOn: launchAtLoginBinding)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
        }

        SettingRow(
            label: "Main terminal directory",
            hint: "Where the shared main terminal boots `zsh`."
        ) {
            HStack(spacing: 8) {
                Text(cwdDisplayName)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Color.niceInk2(scheme))
                    .lineLimit(1)
                    .truncationMode(.head)
                    .frame(maxWidth: 200, alignment: .trailing)
                Button("Choose…") { pickDirectory() }
                    .controlSize(.small)
            }
        }

        SettingRow(label: "Default shell") {
            ReadOnlyValuePill(value: "zsh")
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
            appState.restartTerminalsFirstPane(cwd: url.path)
        }
    }
}

// MARK: - Shortcuts pane

private struct ShortcutsPane: View {
    var body: some View {
        SettingTitle("Shortcuts")
        SettingRow(label: "New tab") {
            KeyPills(keys: ["⌘", "T"])
        }
        SettingRow(label: "Command palette") {
            KeyPills(keys: ["⌘", "K"])
        }
        SettingRow(label: "Toggle sidebar") {
            KeyPills(keys: ["⌘", "\\"])
        }
        SettingRow(label: "Settings") {
            KeyPills(keys: ["⌘", ","])
        }
    }
}

// MARK: - MCP pane

private struct MCPPane: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme
    @AppStorage("mcpAutoStart") private var autoStart: Bool = true

    /// oklch(0.65 0.18 145) — the settings.jsx "running" dot.
    private let runningGreen = Color(
        .sRGB, red: 0.31, green: 0.74, blue: 0.43, opacity: 1.0
    )
    /// Muted red for the "stopped" state.
    private let stoppedRed = Color(
        .sRGB, red: 0.76, green: 0.32, blue: 0.32, opacity: 1.0
    )

    var body: some View {
        SettingTitle("MCP Server")

        SettingRow(label: "Status") {
            HStack(spacing: 8) {
                Circle()
                    .fill(appState.mcp.isRunning ? runningGreen : stoppedRed)
                    .frame(width: 8, height: 8)
                Text(
                    appState.mcp.isRunning
                        ? "Running on :\(appState.mcp.port)"
                        : "Stopped"
                )
                    .font(.system(size: 12))
                    .foregroundStyle(Color.niceInk(scheme))
                if !appState.mcp.isRunning {
                    Button("Start") {
                        let state = appState
                        Task { await state.mcp.start(appState: state) }
                    }
                    .controlSize(.small)
                }
            }
        }

        SettingRow(
            label: "Exposed tools",
            hint: "Lets Claude create tabs, switch, list, and run commands."
        ) {
            Text("nice.tab.new, nice.tab.switch, nice.tab.list, nice.run")
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(Color.niceInk2(scheme))
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: 260, alignment: .trailing)
        }

        SettingRow(label: "Auto-start at login") {
            Toggle("", isOn: $autoStart)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
        }
    }
}

// MARK: - Appearance pane

private struct AppearancePane: View {
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        SettingTitle("Appearance")

        SettingRow(
            label: "Theme",
            hint: "Match the OS, or pin light / dark."
        ) {
            Picker("", selection: $tweaks.theme) {
                ForEach(ThemeChoice.allCases) { t in
                    Text(t.label).tag(t)
                }
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            .frame(width: 240)
        }

        SettingRow(
            label: "Accent",
            hint: "Also drives the logo, MCP chip, and selection tint."
        ) {
            HStack(spacing: 8) {
                ForEach(AccentPreset.allCases) { preset in
                    AccentSwatch(
                        preset: preset,
                        selected: tweaks.accent == preset
                    ) {
                        tweaks.accent = preset
                    }
                }
            }
        }
    }
}

private struct AccentSwatch: View {
    @Environment(\.colorScheme) private var scheme
    let preset: AccentPreset
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Circle()
            .fill(preset.color)
            .frame(width: 28, height: 28)
            .overlay(
                // Keep the border ring rendered either way so the layout
                // doesn't jump when the selection moves.
                Circle()
                    .strokeBorder(
                        selected ? Color.niceInk(scheme) : Color.clear,
                        lineWidth: 2
                    )
            )
            .overlay(
                // Subtle inner bevel to echo the JSX `boxShadow: inset`.
                Circle()
                    .strokeBorder(Color.black.opacity(0.15), lineWidth: 0.5)
            )
            .contentShape(Circle())
            .onTapGesture { action() }
            .help(preset.label)
            .accessibilityLabel(preset.label)
            .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

// MARK: - About pane

private struct AboutPane: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        SettingTitle("About")
        VStack(alignment: .leading, spacing: 6) {
            Text("Nice v0.1.0")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.niceInk(scheme))
            Text("A companion app for the Claude CLI.")
                .font(.system(size: 12))
                .foregroundStyle(Color.niceInk2(scheme))
        }
        .padding(.top, 2)
    }
}

// MARK: - Shared building blocks

/// Matches the JSX `SettingTitle` — 16pt bold title at the top of each
/// pane with a 14pt bottom margin.
struct SettingTitle: View {
    @Environment(\.colorScheme) private var scheme
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.system(size: 16, weight: .bold))
            .tracking(-0.2)
            .foregroundStyle(Color.niceInk(scheme))
            .padding(.bottom, 14)
    }
}

/// Matches `SettingRow` in settings.jsx: flex label (with optional
/// hint) on the left, right-aligned content on the right, 10pt vertical
/// padding and a 1pt `niceLine` bottom border.
struct SettingRow<Content: View>: View {
    @Environment(\.colorScheme) private var scheme

    let label: String
    let hint: String?
    let content: () -> Content

    init(
        label: String,
        hint: String? = nil,
        @ViewBuilder content: @escaping () -> Content
    ) {
        self.label = label
        self.hint = hint
        self.content = content
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(label)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Color.niceInk(scheme))
                if let hint {
                    Text(hint)
                        .font(.system(size: 11.5))
                        .foregroundStyle(Color.niceInk3(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            content()
        }
        .padding(.vertical, 10)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.niceLine(scheme))
                .frame(height: 1)
        }
    }
}

/// Matches `KeyPills` in settings.jsx — mono beveled kbd boxes.
struct KeyPills: View {
    @Environment(\.colorScheme) private var scheme
    let keys: [String]

    var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(keys.enumerated()), id: \.offset) { _, key in
                Text(key)
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(Color.niceInk(scheme))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .frame(minWidth: 14)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(Color.niceBg3(scheme))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .strokeBorder(Color.niceLineStrong(scheme), lineWidth: 1)
                    )
                    // Extra-light bevel echoing the JSX box-shadow.
                    .overlay(alignment: .bottom) {
                        Rectangle()
                            .fill(Color.niceLineStrong(scheme))
                            .frame(height: 1)
                            .padding(.horizontal, 1)
                    }
            }
        }
    }
}

/// Read-only value pill mirroring the JSX `Select` component when a
/// dropdown is overkill (e.g. "zsh" with no alternatives).
private struct ReadOnlyValuePill: View {
    @Environment(\.colorScheme) private var scheme
    let value: String

    var body: some View {
        Text(value)
            .font(.system(size: 12))
            .foregroundStyle(Color.niceInk(scheme))
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.niceBg3(scheme))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(Color.niceLineStrong(scheme), lineWidth: 1)
            )
    }
}

// MARK: - Previews

#Preview("Settings") {
    SettingsView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
}
