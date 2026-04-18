//
//  VisualEffectView.swift
//  Nice
//
//  `NSViewRepresentable` wrapper around `NSVisualEffectView`, configured
//  for the wallpaper-tinted sidebar treatment Xcode / Finder / Mail use
//  on macOS. Material is `.sidebar`; blending pulls from content behind
//  the window (`.behindWindow`) so the system's Desktop Tinting effect
//  mixes the average wallpaper color into the chrome; state is `.active`
//  so the effect keeps rendering even when the window loses focus.
//
//  Used only when the active `Palette` is `.macOS`. In the `.nice`
//  palette the sidebar falls back to a flat `niceBg2` panel — intentional,
//  because the nice palette's custom oklch values don't blend with
//  vibrancy in a visually coherent way.
//

import AppKit
import SwiftUI

struct VisualEffectView: NSViewRepresentable {
    var material: NSVisualEffectView.Material = .sidebar
    var blendingMode: NSVisualEffectView.BlendingMode = .behindWindow
    var state: NSVisualEffectView.State = .active

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = state
        view.autoresizingMask = [.width, .height]
        return view
    }

    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
        nsView.state = state
    }
}
