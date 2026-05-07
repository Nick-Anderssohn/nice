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
            hint: "Typeface for every terminal and Claude pane. Lists every font installed on this Mac; monospace works best."
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

/// Dropdown of installed fonts. The "Default" row
/// (`defaultSentinel`) maps to `nil` so `TabPtySession.terminalFont`
/// falls through to the SF Mono → JetBrains Mono NL → system
/// monospaced chain. The curated `candidates` list of popular
/// programming fonts is surfaced at the top of the menu; every other
/// installed font family appears below a divider, alphabetical, one
/// row per family. We deliberately don't filter to monospace — many
/// terminal-oriented fonts (notably the wide-icon Nerd Font variants)
/// don't claim the fixed-pitch trait, and any heuristic to
/// reconstruct it ends up either over- or under-including. Trust the
/// user to pick what works for them; SwiftTerm renders whatever it's
/// handed. Candidates that aren't installed are dropped so the user
/// never picks a font that would fail to load.
struct TerminalFontPicker: View {
    @Binding var selection: String

    /// Used as the Picker tag for the "use default" row. Can't be `nil`
    /// because Picker requires non-optional tags. Chosen to be a string
    /// no real font name would ever match.
    static let defaultSentinel = "__nice.default__"

    /// Popular fonts shown at the top of the picker, in display order.
    /// Anything else the system reports as monospace appears below a
    /// divider — see `installed`.
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

    private struct Row: Hashable {
        let psName: String
        let display: String
    }

    /// Two ordered slices: the curated popular fonts that are actually
    /// installed (preserving `candidates` order), and every other
    /// installed font family — one row per family, regular face,
    /// alphabetised. The two slices are deduplicated against each
    /// other so a curated font never shows twice.
    ///
    /// We pick the regular face by walking
    /// `availableMembers(ofFontFamily:)` (which returns
    /// `[psName, styleName, weight, traitsBitmask]` tuples) and taking
    /// the first non-bold, non-italic member. `NSFont(name:size:)` is
    /// the final gate: anything it can't construct is dropped,
    /// mirroring the curated path and keeping the load contract
    /// identical.
    private var installed: (curated: [Row], others: [Row]) {
        var seen: Set<String> = []
        var curated: [Row] = []

        for psName in Self.candidates {
            guard let font = NSFont(name: psName, size: 12),
                  seen.insert(psName).inserted else { continue }
            curated.append(Row(psName: psName, display: font.displayName ?? psName))
        }

        let manager = NSFontManager.shared
        var others: [Row] = []

        for family in manager.availableFontFamilies {
            guard let members = manager.availableMembers(ofFontFamily: family) else { continue }
            for member in members {
                guard member.count >= 4,
                      let psName = member[0] as? String,
                      let traitsNum = member[3] as? NSNumber else { continue }
                let traits = NSFontTraitMask(rawValue: UInt(traitsNum.intValue))
                if traits.contains(.italicFontMask) || traits.contains(.boldFontMask) { continue }
                guard seen.insert(psName).inserted,
                      let font = NSFont(name: psName, size: 12) else { continue }
                others.append(Row(psName: psName, display: font.displayName ?? psName))
                break
            }
        }

        others.sort { $0.display.localizedCaseInsensitiveCompare($1.display) == .orderedAscending }
        return (curated, others)
    }

    var body: some View {
        let lists = installed
        Picker("", selection: $selection) {
            Text("Default (SF Mono)").tag(Self.defaultSentinel)
            Divider()
            ForEach(lists.curated, id: \.psName) { entry in
                Text(entry.display).tag(entry.psName)
            }
            if !lists.others.isEmpty {
                Divider()
                ForEach(lists.others, id: \.psName) { entry in
                    Text(entry.display).tag(entry.psName)
                }
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
