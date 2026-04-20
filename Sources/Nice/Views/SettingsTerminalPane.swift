//
//  SettingsTerminalPane.swift
//  Nice
//
//  Terminal-specific preferences: per-scheme theme selection,
//  Ghostty-format theme import, and management of imported themes.
//  Chrome palette lives in `AppearancePane`; font size in `FontPane`.
//
//  Two independently-editable slots — light and dark — so "Sync with
//  OS" can flip between a user-tuned light look and a user-tuned dark
//  look without either being an afterthought.
//

import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SettingsTerminalPane: View {
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var catalog: TerminalThemeCatalog
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    @State private var importError: ImportErrorWrapper?

    var body: some View {
        Group {
            SettingTitle("Terminal themes")

            SettingRow(
                label: "Light mode theme",
                hint: "Palette used when the active scheme is light."
            ) {
                ThemePicker(
                    selection: $tweaks.terminalThemeLightId,
                    options: catalog.themes(for: .light),
                    accessibilityIdentifier: "settings.terminal.lightPicker"
                )
            }

            SettingRow(
                label: "Dark mode theme",
                hint: "Palette used when the active scheme is dark."
            ) {
                ThemePicker(
                    selection: $tweaks.terminalThemeDarkId,
                    options: catalog.themes(for: .dark),
                    accessibilityIdentifier: "settings.terminal.darkPicker"
                )
            }

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

    // MARK: - Actions

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

// MARK: - Picker

private struct ThemePicker: View {
    @Binding var selection: String
    let options: [TerminalTheme]
    let accessibilityIdentifier: String

    var body: some View {
        Picker("", selection: $selection) {
            ForEach(options) { theme in
                ThemePickerRow(theme: theme).tag(theme.id)
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .frame(minWidth: 220)
        .accessibilityIdentifier(accessibilityIdentifier)
    }
}

private struct ThemePickerRow: View {
    let theme: TerminalTheme

    var body: some View {
        HStack(spacing: 8) {
            SwatchStrip(theme: theme)
            Text(theme.displayName)
        }
    }
}

/// 16 tiny swatches so the user can preview the palette without
/// selecting it. Height matches the picker row so the menu doesn't
/// jitter between items.
private struct SwatchStrip: View {
    let theme: TerminalTheme

    var body: some View {
        HStack(spacing: 1) {
            ForEach(Array(theme.ansi.enumerated()), id: \.offset) { _, color in
                Rectangle()
                    .fill(SwiftUI.Color(color.nsColor))
                    .frame(width: 6, height: 12)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 2, style: .continuous))
    }
}

// MARK: - Imported row

private struct ImportedThemeRow: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let theme: TerminalTheme
    let onDelete: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            SwatchStrip(theme: theme)
            Text(theme.displayName)
                .font(.system(size: 12))
                .foregroundStyle(Color.niceInk(scheme, palette))
            Spacer()
            Button(action: onDelete) {
                Image(systemName: "trash")
                    .font(.system(size: 11))
            }
            .buttonStyle(.borderless)
            .help("Remove \(theme.displayName) from Nice's theme library.")
            .accessibilityIdentifier("settings.terminal.remove.\(theme.id)")
        }
    }
}

// MARK: - Import error alert

private struct ImportErrorWrapper: Identifiable {
    let id = UUID()
    let error: TerminalThemeCatalog.ImportError

    var title: String {
        switch error {
        case .cannotRead:    return "Couldn't read the theme file"
        case .parseFailed:   return "The theme file is invalid"
        case .cannotPersist: return "Couldn't save the theme"
        }
    }

    var message: String {
        switch error {
        case .cannotRead(let m):    return m
        case .cannotPersist(let m): return m
        case .parseFailed(let inner):
            switch inner {
            case .missingPalette(let indices):
                let list = indices.map(String.init).joined(separator: ", ")
                return "The file is missing palette entries: \(list). Ghostty themes must define all 16 colors."
            case .missingRequiredKey(let key):
                return "The file is missing the required `\(key)` key."
            case .invalidHex(let value, let line):
                return "Line \(line) contains an invalid color value: `\(value)`."
            case .paletteIndexOutOfRange(let index, let line):
                return "Line \(line) uses palette index \(index); valid indices are 0–15."
            }
        }
    }
}

