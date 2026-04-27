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
import UniformTypeIdentifiers

// MARK: - Section enum

enum SettingsSection: String, CaseIterable, Identifiable {
    case appearance  = "Appearance"
    case editors     = "Editors"
    case shortcuts   = "Shortcuts"
    case font        = "Font"
    case about       = "About"

    var id: String { rawValue }
    var label: String { rawValue }

    /// Short, stable token used as the suffix of the sidebar-row
    /// accessibility identifier (`settings.section.<slug>`). Kept
    /// separate from `rawValue` so renaming the user-visible label
    /// doesn't break the UI-test identifier hook.
    var slug: String {
        switch self {
        case .appearance: return "appearance"
        case .editors:    return "editors"
        case .shortcuts:  return "shortcuts"
        case .font:       return "font"
        case .about:      return "about"
        }
    }
}

// MARK: - Root

struct SettingsView: View {
    @Environment(Tweaks.self) private var tweaks
    @Environment(\.colorScheme) private var scheme

    private var palette: Palette { tweaks.activeChromePalette }

    @State private var active: SettingsSection = .appearance

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            content
        }
        .frame(
            minWidth: 560, idealWidth: 640, maxWidth: .infinity,
            minHeight: 380, idealHeight: 440, maxHeight: .infinity
        )
        .background(Color.nicePanel(scheme, palette))
        .background(
            // SwiftUI's `Settings` scene historically renders its window
            // without `.resizable` in its styleMask, ignoring
            // `.windowResizability(.contentMinSize)` on the scene. Reach
            // into AppKit and OR the bit in once the window is live so
            // the user can drag any edge / corner like a normal window.
            WindowAccessor { window in
                if !window.styleMask.contains(.resizable) {
                    window.styleMask.insert(.resizable)
                }
            }
        )
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
                case .editors:    SettingsEditorsPane()
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
    @Environment(KeyboardShortcuts.self) private var shortcuts

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
    @Environment(Tweaks.self) private var tweaks
    @Environment(TerminalThemeCatalog.self) private var catalog
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    @State private var importError: ImportErrorWrapper?

    var body: some View {
        @Bindable var tweaks = tweaks
        Group {
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
                .fixedSize()
                .disabled(tweaks.syncWithOS)
                .accessibilityIdentifier("settings.appearance.scheme")
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

            SettingSubtitle("Light mode")

            SettingRow(
                label: "Chrome",
                hint: "Palette used for the sidebar, window background, and toolbar."
            ) {
                Picker("", selection: $tweaks.chromeLightPalette) {
                    ForEach(Palette.allCases.filter { $0.matches(scheme: .light) }) { palette in
                        Text(palette.displayName).tag(palette)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
                .accessibilityIdentifier("settings.appearance.chromeLight")
            }

            SettingRow(
                label: "Terminal theme",
                hint: "Palette used inside terminal panes."
            ) {
                ThemePicker(
                    selection: $tweaks.terminalThemeLightId,
                    options: catalog.themes(for: .light),
                    accessibilityIdentifier: "settings.terminal.lightPicker"
                )
            }

            SettingSubtitle("Dark mode")

            SettingRow(
                label: "Chrome",
                hint: "Palette used for the sidebar, window background, and toolbar."
            ) {
                Picker("", selection: $tweaks.chromeDarkPalette) {
                    ForEach(Palette.allCases.filter { $0.matches(scheme: .dark) }) { palette in
                        Text(palette.displayName).tag(palette)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
                .accessibilityIdentifier("settings.appearance.chromeDark")
            }

            SettingRow(
                label: "Terminal theme",
                hint: "Palette used inside terminal panes."
            ) {
                ThemePicker(
                    selection: $tweaks.terminalThemeDarkId,
                    options: catalog.themes(for: .dark),
                    accessibilityIdentifier: "settings.terminal.darkPicker"
                )
            }

            SettingSubtitle("Custom themes")

            SettingRow(
                label: "Import theme",
                hint: "Load a Ghostty-format theme file (.ghostty or .conf). Imported themes appear in both light and dark pickers."
            ) {
                Button("Import…") {
                    runImportPanel()
                }
                .controlSize(.small)
                .accessibilityIdentifier("settings.terminal.import")
            }

            if !catalog.imported.isEmpty {
                SettingRow(
                    label: "Imported themes",
                    hint: "Remove a theme to delete its file from Nice's support directory."
                ) {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(catalog.imported) { theme in
                            ImportedThemeRow(theme: theme, onDelete: { deleteImported(theme) })
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .alert(item: $importError) { wrapper in
            Alert(
                title: Text(wrapper.title),
                message: Text(wrapper.message),
                dismissButton: .default(Text("OK"))
            )
        }
    }

    // MARK: - Theme import

    private func runImportPanel() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if let ghosttyType = UTType(filenameExtension: "ghostty") {
            panel.allowedContentTypes = [ghosttyType, .plainText, .propertyList]
        }
        panel.allowsOtherFileTypes = true
        panel.prompt = "Import"
        panel.message = "Choose a Ghostty-format terminal theme (.ghostty or .conf)."

        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let theme = try catalog.importTheme(from: url)
            // Auto-select in the active-scheme slot so the user sees
            // immediate feedback without having to open the picker.
            switch scheme {
            case .light: tweaks.terminalThemeLightId = theme.id
            case .dark:  tweaks.terminalThemeDarkId = theme.id
            @unknown default: tweaks.terminalThemeLightId = theme.id
            }
        } catch let error as TerminalThemeCatalog.ImportError {
            importError = ImportErrorWrapper(error: error)
        } catch {
            importError = ImportErrorWrapper(
                error: .cannotRead(message: error.localizedDescription)
            )
        }
    }

    private func deleteImported(_ theme: TerminalTheme) {
        try? catalog.remove(theme)
        // If the deleted theme was selected in either slot, fall back
        // to the Nice default for that scheme so the resolver doesn't
        // churn on a dangling id.
        if tweaks.terminalThemeLightId == theme.id {
            tweaks.terminalThemeLightId = Tweaks.defaultTerminalThemeLightId
        }
        if tweaks.terminalThemeDarkId == theme.id {
            tweaks.terminalThemeDarkId = Tweaks.defaultTerminalThemeDarkId
        }
    }
}

/// 2×2 grid of theme choices. Top row is the nice palette (Light / Dark);
/// bottom row is the macOS palette. The cell matching `tweaks.theme` is
/// highlighted with the accent. Tapping a cell calls `tweaks.userPicked`
/// which respects the sync-with-OS flag (if sync is on and the tapped
/// cell's scheme doesn't match the OS, we fall back to its counterpart).
private struct ThemeButtonGrid: View {
    @Environment(Tweaks.self) private var tweaks

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

/// Subgroup header inside a settings pane — between `SettingTitle`
/// (the pane heading) and `SettingRow` (the controls). 14pt bold,
/// primary ink, with extra top breathing room and a hairline rule
/// below so it reads as a clear section break — louder than the
/// 13pt-medium row labels it groups together.
struct SettingSubtitle: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.system(size: 14, weight: .bold))
            .tracking(-0.1)
            .foregroundStyle(Color.niceInk(scheme, palette))
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 24)
            .padding(.bottom, 6)
            .overlay(alignment: .bottom) {
                Rectangle()
                    .fill(Color.niceLine(scheme, palette))
                    .frame(height: 1)
            }
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
        .environment(NiceServices())
        .environment(Tweaks())
        .environment(KeyboardShortcuts())
        .environment(FontSettings())
}
