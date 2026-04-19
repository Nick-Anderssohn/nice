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

import SwiftUI

struct FontPane: View {
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    var body: some View {
        SettingTitle("Font")

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
            }
            .controlSize(.small)
        }
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
