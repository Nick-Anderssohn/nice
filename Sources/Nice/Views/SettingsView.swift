//
//  SettingsView.swift
//  Nice
//
//  Preferences window content, bound to ⌘, via the app's
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
//  `Tweaks` environment object so the rest of the app (Logo, selection
//  tints…) repaints live the moment a new swatch is picked.
//
//  The Appearance pane owns the theme picker (four choices: Nice
//  light/dark + macOS light/dark) and a `Sync with OS theme` toggle that
//  flips between the scheme counterparts within the selected palette
//  family whenever the system appearance changes.
//

import AppKit
import SwiftUI

// MARK: - Section enum

enum SettingsSection: String, CaseIterable, Identifiable {
    case appearance  = "Appearance"
    case terminal    = "Terminal themes"
    case shortcuts   = "Shortcuts"
    case font        = "Font"
    case about       = "About"

    var id: String { rawValue }
    var label: String { rawValue }

    /// Short, stable token used as the suffix of the sidebar-row
    /// accessibility identifier (`settings.section.<slug>`). Kept
    /// separate from `rawValue` so renaming the user-visible label
    /// (e.g. "Terminal themes" → something else) doesn't break the
    /// UI-test identifier hook.
    var slug: String {
        switch self {
        case .appearance: return "appearance"
        case .terminal:   return "terminal"
        case .shortcuts:  return "shortcuts"
        case .font:       return "font"
        case .about:      return "about"
        }
    }
}

// MARK: - Root

struct SettingsView: View {
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    private var palette: Palette { tweaks.activeChromePalette }

    @State private var active: SettingsSection = .appearance

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            content
        }
        .frame(width: 640, height: 440)
        .background(Color.nicePanel(scheme, palette))
        .environment(\.palette, palette)
        // `.accessibilityElement(children: .contain)` groups the whole
        // shell under one container element that carries the id, so
        // child rows / pickers keep their own identifiers instead of
        // inheriting `settings.root` from the parent chain. Without
        // `.contain`, SwiftUI applies the identifier to every
        // descendant element, which silently masks the per-row
        // `settings.section.*` ids the tests need.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("settings.root")
    }

    // MARK: Left rail

    private var sidebar: some View {
        // Floating-card treatment matching AppShellView's sidebar:
        // palette-aware background (VisualEffectView for macOS, flat
        // niceBg2 for nice), rounded corners, subtle stroked border,
        // soft shadow. No traffic-light spacer needed — the Settings
        // window uses standard chrome.
        SidebarBackground(palette: palette, scheme: scheme) {
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
        }
        .frame(width: 160)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(
                    Color.niceLine(scheme, palette).opacity(0.5),
                    lineWidth: 0.5
                )
        )
        .shadow(color: Color.black.opacity(0.15), radius: 4, x: 0, y: 2)
        .padding(.leading, 6)
        .padding(.vertical, 6)
    }

    // MARK: Right panel

    private var content: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                switch active {
                case .shortcuts:  ShortcutsPane()
                case .appearance: AppearancePane()
                case .terminal:   SettingsTerminalPane()
                case .font:       FontPane()
                case .about:      AboutPane()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 24)
            .padding(.vertical, 18)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.nicePanel(scheme, palette))
    }
}

// MARK: - Sidebar row

private struct SettingsSectionRow: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let section: SettingsSection
    let active: Bool
    let accent: Color
    let action: () -> Void

    var body: some View {
        // Wrap in a Button so XCUITest sees a proper focusable button
        // element (with the identifier reliably attached) rather than
        // a bare Text — we'd get static-text label collisions with the
        // pane's `SettingTitle` on the right otherwise, and identifier
        // bindings don't always land on a plain Text reliably. The
        // visuals are preserved via `.buttonStyle(.plain)`.
        Button(action: action) {
            Text(section.label)
                .font(.system(size: 12.5, weight: active ? .semibold : .medium))
                .foregroundStyle(active ? Color.niceInk(scheme, palette) : Color.niceInk2(scheme, palette))
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
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("settings.section.\(section.slug)")
        .accessibilityLabel(section.label)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}

// MARK: - Shortcuts pane

private struct ShortcutsPane: View {
    @EnvironmentObject private var shortcuts: KeyboardShortcuts

    var body: some View {
        SettingTitle("Shortcuts")
        ForEach(ShortcutAction.allCases, id: \.self) { action in
            SettingRow(label: action.label) {
                KeyRecorderField(action: action)
            }
        }
    }
}

// MARK: - Appearance pane

private struct AppearancePane: View {
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    var body: some View {
        SettingTitle("Appearance")

        SettingRow(
            label: "Sync with OS theme",
            hint: "Flip between light and dark as the system does."
        ) {
            Toggle("", isOn: $tweaks.syncWithOS)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
                .accessibilityIdentifier("settings.theme.sync")
        }

        SettingRow(
            label: "Scheme",
            hint: "Manual light/dark override. Disabled when Sync with OS is on."
        ) {
            Picker("", selection: $tweaks.scheme) {
                Text("Light").tag(ColorScheme.light)
                Text("Dark").tag(ColorScheme.dark)
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            .frame(width: 160)
            .disabled(tweaks.syncWithOS)
            .accessibilityIdentifier("settings.appearance.scheme")
        }

        SettingRow(
            label: "Light mode chrome",
            hint: "Palette used for the sidebar, window background, and toolbar when the scheme is light."
        ) {
            Picker("", selection: $tweaks.chromeLightPalette) {
                ForEach(Palette.allCases.filter { $0.matches(scheme: .light) }) { palette in
                    Text(palette.displayName).tag(palette)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(minWidth: 120)
            .accessibilityIdentifier("settings.appearance.chromeLight")
        }

        SettingRow(
            label: "Dark mode chrome",
            hint: "Palette used for the sidebar, window background, and toolbar when the scheme is dark."
        ) {
            Picker("", selection: $tweaks.chromeDarkPalette) {
                ForEach(Palette.allCases.filter { $0.matches(scheme: .dark) }) { palette in
                    Text(palette.displayName).tag(palette)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(minWidth: 120)
            .accessibilityIdentifier("settings.appearance.chromeDark")
        }

        SettingRow(
            label: "Accent",
            hint: "Also drives the logo and selection tint."
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

        SettingRow(
            label: "GPU rendering",
            hint: "Use Metal for terminal drawing. Faster on most Macs; falls back to CPU automatically if Metal is unavailable."
        ) {
            Toggle("", isOn: $tweaks.gpuRendering)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
                .accessibilityIdentifier("settings.appearance.gpuRendering")
        }

        SettingRow(
            label: "Smooth scrolling",
            hint: "Pixel-precise trackpad scrolling. Requires GPU rendering; mouse wheels keep using line-based scroll."
        ) {
            Toggle("", isOn: $tweaks.smoothScrolling)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
                .disabled(!tweaks.gpuRendering)
                .help(tweaks.gpuRendering ? "" : "Turn on GPU rendering to enable smooth scrolling.")
                .accessibilityIdentifier("settings.appearance.smoothScrolling")
        }
    }
}

/// 2×2 grid of theme choices. Top row is the nice palette (Light / Dark);
/// bottom row is the macOS palette. The cell matching `tweaks.theme` is
/// highlighted with the accent. Tapping a cell calls `tweaks.userPicked`
/// which respects the sync-with-OS flag (if sync is on and the tapped
/// cell's scheme doesn't match the OS, we fall back to its counterpart).
private struct ThemeButtonGrid: View {
    @EnvironmentObject private var tweaks: Tweaks

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                cell(.macLight)
                cell(.macDark)
            }
            HStack(spacing: 8) {
                cell(.niceLight)
                cell(.niceDark)
            }
        }
        .frame(width: 260)
    }

    private func cell(_ choice: ThemeChoice) -> some View {
        ThemeCell(
            choice: choice,
            selected: tweaks.theme == choice,
            accent: tweaks.accent.color
        ) {
            tweaks.userPicked(choice)
        }
    }
}

private struct ThemeCell: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let choice: ThemeChoice
    let selected: Bool
    let accent: Color
    let action: () -> Void

    @State private var hover = false

    private var background: Color {
        if selected { return Color.niceSel(scheme, accent: accent) }
        if hover    { return Color.niceInk(scheme, palette).opacity(0.06) }
        return Color.niceBg3(scheme, palette)
    }

    private var border: Color {
        selected ? accent : Color.niceLine(scheme, palette)
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 12, weight: .regular))
                .foregroundStyle(selected ? accent : Color.niceInk3(scheme, palette))

            Text(choice.label)
                .font(.system(size: 12, weight: selected ? .semibold : .medium))
                .foregroundStyle(Color.niceInk(scheme, palette))
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(background)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(border, lineWidth: selected ? 1.5 : 1)
        )
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { action() }
        .help(choice.label)
        // Combine the inner Image + Text into a single a11y element so
        // XCUITest queries for the cell resolve to one node, not two.
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("settings.theme.cell.\(choice.rawValue)")
        .accessibilityLabel(choice.label)
        // `.isSelected` trait doesn't reliably surface to XCUIElement.isSelected
        // on macOS; expose the bit as the element's value instead so
        // UI tests can read it via `.value`.
        .accessibilityValue(selected ? "selected" : "unselected")
        .accessibilityAddTraits(.isButton)
    }
}

private struct AccentSwatch: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
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
                        selected ? Color.niceInk(scheme, palette) : Color.clear,
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
    @Environment(\.palette) private var palette

    var body: some View {
        SettingTitle("About")
        VStack(alignment: .leading, spacing: 6) {
            Text("Nice v\(Self.shortVersion)")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.niceInk(scheme, palette))
            Text("A terminal emulator that auto-organizes claude instances.")
                .font(.system(size: 12))
                .foregroundStyle(Color.niceInk2(scheme, palette))
        }
        .padding(.top, 2)
    }

    private static let shortVersion: String =
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
}

// MARK: - Shared building blocks

/// Matches the JSX `SettingTitle` — 16pt bold title at the top of each
/// pane with a 14pt bottom margin.
struct SettingTitle: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.system(size: 16, weight: .bold))
            .tracking(-0.2)
            .foregroundStyle(Color.niceInk(scheme, palette))
            .padding(.bottom, 14)
    }
}

/// Matches `SettingRow` in settings.jsx: flex label (with optional
/// hint) on the left, right-aligned content on the right, 10pt vertical
/// padding and a 1pt `niceLine` bottom border.
struct SettingRow<Content: View>: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

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
                    .foregroundStyle(Color.niceInk(scheme, palette))
                if let hint {
                    Text(hint)
                        .font(.system(size: 11.5))
                        .foregroundStyle(Color.niceInk3(scheme, palette))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            content()
        }
        .padding(.vertical, 10)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
    }
}

/// Matches `KeyPills` in settings.jsx — mono beveled kbd boxes.
struct KeyPills: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    let keys: [String]

    var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(keys.enumerated()), id: \.offset) { _, key in
                Text(key)
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(Color.niceInk(scheme, palette))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .frame(minWidth: 14)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(Color.niceBg3(scheme, palette))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .strokeBorder(Color.niceLineStrong(scheme, palette), lineWidth: 1)
                    )
                    // Extra-light bevel echoing the JSX box-shadow.
                    .overlay(alignment: .bottom) {
                        Rectangle()
                            .fill(Color.niceLineStrong(scheme, palette))
                            .frame(height: 1)
                            .padding(.horizontal, 1)
                    }
            }
        }
    }
}

// MARK: - Previews

#Preview("Settings") {
    SettingsView()
        .environmentObject(NiceServices())
        .environmentObject(Tweaks())
        .environmentObject(KeyboardShortcuts())
        .environmentObject(FontSettings())
}
