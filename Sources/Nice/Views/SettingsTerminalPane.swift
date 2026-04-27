//
//  SettingsTerminalPane.swift
//  Nice
//
//  Building blocks shared with `AppearancePane` for picking and
//  managing terminal themes: the swatch-strip preview, the per-scheme
//  picker, the imported-theme row, and the import-error wrapper.
//
//  These used to back a standalone "Terminal themes" settings section,
//  but that section was folded into Appearance — chrome palette and
//  terminal theme are configured side-by-side, grouped by light/dark
//  scheme. The types are kept here (rather than inlined into
//  SettingsView.swift) to keep that file focused on the pane shells.
//

import AppKit
import SwiftUI

// MARK: - Picker

struct ThemePicker: View {
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
        .fixedSize()
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

struct ImportedThemeRow: View {
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

struct ImportErrorWrapper: Identifiable {
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

