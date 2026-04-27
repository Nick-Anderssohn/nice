//
//  SettingsFontPane.swift
//  Nice
//
//  Font size settings — two independent sliders (Terminal, Sidebar)
//  plus a reset button. Values live in `FontSettings` and update live:
//  dragging the terminal slider reflows the SwiftTerm view behind the
//  settings window; dragging the sidebar slider rescales every element
//  in the sidebar via `FontSettings.sidebarSize(_:)`.
//
//  The global Cmd+=/-/0 shortcuts dispatch to the same `FontSettings`,
//  so they stay in sync with what's shown here.
//

import AppKit
import SwiftUI

struct FontPane: View {
    @Environment(FontSettings.self) private var fontSettings
    @Environment(Tweaks.self) private var tweaks
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    var body: some View {
        @Bindable var fontSettings = fontSettings
        SettingTitle("Font")

        SettingRow(
            label: "Terminal font",
            hint: "Monospace typeface for every terminal and Claude pane. Lists only fonts currently installed on this Mac."
        ) {
            TerminalFontPicker(
                selection: Binding(
                    get: { tweaks.terminalFontFamily ?? TerminalFontPicker.defaultSentinel },
                    set: { newValue in
                        tweaks.terminalFontFamily = (newValue == TerminalFontPicker.defaultSentinel)
                            ? nil
                            : newValue
                    }
                )
            )
        }

        SettingRow(
            label: "Terminal size",
            hint: "Monospace font size for every terminal and Claude pane."
        ) {
            FontSizeControl(value: $fontSettings.terminalFontSize)
        }

        SettingRow(
            label: "Sidebar size",
            hint: "Base size for the sidebar. Other sidebar text scales proportionally."
        ) {
            FontSizeControl(value: $fontSettings.sidebarFontSize)
        }

        SettingRow(label: "Reset") {
            Button("Reset to defaults") {
                fontSettings.resetToDefaults()
                tweaks.terminalFontFamily = nil
            }
            .controlSize(.small)
        }
    }
}

/// Curated dropdown of installed monospace fonts. The "Default" row
/// (`defaultSentinel`) maps to `nil` so `TabPtySession.terminalFont`
/// falls through to the SF Mono → JetBrains Mono NL → system
/// monospaced chain. Candidates that aren't installed are dropped so
/// the user never picks a font that would fail to load.
struct TerminalFontPicker: View {
    @Binding var selection: String

    /// Used as the Picker tag for the "use default" row. Can't be `nil`
    /// because Picker requires non-optional tags. Chosen to be a string
    /// no real font name would ever match.
    static let defaultSentinel = "__nice.default__"

    static let candidates: [String] = [
        "SFMono-Regular",
        "JetBrainsMonoNL-Regular",
        "JetBrainsMono-Regular",
        "Menlo-Regular",
        "Monaco",
        "CourierNewPSMT",
        "PTMono-Regular",
        "FiraCode-Regular",
        "SourceCodePro-Regular",
        "IBMPlexMono",
        "Hack-Regular",
        "CascadiaCode-Regular",
    ]

    /// Filter to fonts the system can actually load. `NSFont(name:size:)`
    /// returns nil for uninstalled fonts — a better gate than
    /// `NSFontManager.availableFontFamilies`, which returns family
    /// names (not PostScript names) and can list fonts the concrete
    /// face isn't available in.
    private var installed: [(psName: String, display: String)] {
        Self.candidates.compactMap { name in
            guard let font = NSFont(name: name, size: 12) else { return nil }
            return (name, font.displayName ?? name)
        }
    }

    var body: some View {
        Picker("", selection: $selection) {
            Text("Default (SF Mono)").tag(Self.defaultSentinel)
            Divider()
            ForEach(installed, id: \.psName) { entry in
                Text(entry.display).tag(entry.psName)
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .frame(minWidth: 220)
        .accessibilityIdentifier("settings.font.terminalFamily")
    }
}

/// Slider + numeric readout used for both terminal and sidebar sizes.
/// Step 1pt, range pulled from `FontSettings`.
private struct FontSizeControl: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    @Binding var value: CGFloat

    var body: some View {
        HStack(spacing: 10) {
            Slider(
                value: $value,
                in: FontSettings.minSize...FontSettings.maxSize,
                step: 1
            )
            .frame(width: 180)

            Text("\(Int(value.rounded())) pt")
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .frame(width: 44, alignment: .trailing)
        }
    }
}
