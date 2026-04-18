//
//  SidebarBackground.swift
//  Nice
//
//  Paints the sidebar column background appropriate to the active palette.
//  Nice palette: flat `niceBg2`. macOS palette: wallpaper-tinted
//  NSVisualEffectView (`.sidebar` material, `.behindWindow` blending).
//
//  Shared between the main app shell (AppShellView) and the Settings
//  window's left rail (SettingsView) so both surfaces look consistent in
//  both palettes.
//

import SwiftUI

struct SidebarBackground<Content: View>: View {
    let palette: Palette
    let scheme: ColorScheme
    @ViewBuilder let content: Content

    var body: some View {
        content
            .background(
                Group {
                    switch palette {
                    case .nice:
                        Color.niceBg2(scheme, palette)
                    case .macOS:
                        VisualEffectView(
                            material: .sidebar,
                            blendingMode: .behindWindow,
                            state: .active
                        )
                    }
                }
            )
    }
}
