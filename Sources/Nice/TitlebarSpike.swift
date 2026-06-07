//
//  TitlebarSpike.swift
//  Nice
//
//  ⚠️ THROWAWAY SPIKE — NOT PRODUCTION CODE ⚠️
//
//  Evidence-gathering prototype for the question: can a native
//  `NSTitlebarAccessoryViewController` reproduce Nice's custom 52pt
//  top-bar look (single unified band, traffic lights inline, arbitrary
//  non-system theme colors), and what chrome behaviors come for free?
//
//  See docs/research/refactor-recommendation.md and the plan at
//  ~/.claude/plans/regarding-this-the-one-atomic-neumann.md.
//
//  Isolation: this spike creates its OWN scratch `NSWindow` and never
//  touches the real chrome (`AppShellView`, `WindowToolbarView`,
//  `WindowDragRegion`, `TrafficLightNudger`, the main window). It is wired
//  in only via a debug `CommandMenu` + an opt-in env var. Remove by
//  deleting this file and the `.commands { TitlebarSpikeMenu() }` /
//  autoOpen lines in `NiceApp.swift`.
//

import AppKit
import SwiftUI

// MARK: - Shared state (so the in-content Picker can drive the in-titlebar band)

/// The band lives in the titlebar accessory's `NSHostingView`; the live-theme
/// Picker lives in the content view's `NSHostingController`. They are two
/// separate SwiftUI trees, so a plain `@State` can't bridge them — this shared
/// observable does.
@MainActor
@Observable
final class SpikeState {
    var palette: Palette
    let themed: Bool
    let useMaterial: Bool
    let layoutLabel: String
    /// The height the SwiftUI band *requests* via `.frame(height:)` — i.e. its
    /// intrinsic content height. We compare this to the measured accessory
    /// height to tell "titlebar grew to fit" (no clipping) from "titlebar
    /// clipped us". This is the Q1 disambiguation.
    let bandHeight: CGFloat
    /// Filled by `open(...)` after layout: the actual rendered accessory height.
    var measuredHeight: CGFloat = 0

    init(palette: Palette, themed: Bool, useMaterial: Bool, layoutLabel: String, bandHeight: CGFloat) {
        self.palette = palette
        self.themed = themed
        self.useMaterial = useMaterial
        self.layoutLabel = layoutLabel
        self.bandHeight = bandHeight
    }
}

// MARK: - Spike launcher

@MainActor
enum TitlebarSpike {
    /// Strong refs so the scratch windows aren't deallocated out from under us.
    private static var windows: [NSWindow] = []

    static func open(
        layout: NSLayoutConstraint.Attribute,
        themed: Bool,
        fauxSidebar: Bool,
        useMaterial: Bool,
        bandHeight: CGFloat = 52
    ) {
        let layoutLabel = (layout == .top) ? "top" : "bottom"
        let state = SpikeState(
            palette: defaultPalette(),
            themed: themed,
            useMaterial: useMaterial,
            layoutLabel: layoutLabel,
            bandHeight: bandHeight
        )

        // 1. Scratch window. We create it ourselves so it provably exists
        //    before we attach the accessory (no SwiftUI scene timing games),
        //    and so we never collide with the real main window's chrome.
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 640),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false   // we retain it in `windows`
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.title = "Spike — \(layoutLabel) \(themed ? "themed" : "plain")"
        window.isMovableByWindowBackground = false  // test native band-drag, not bg-drag

        // 2. The themed band, hosted in the titlebar accessory. The band's
        //    SwiftUI root requests `bandHeight` via `.frame(height:)`, so its
        //    INTRINSIC content height == bandHeight. With
        //    `automaticallyAdjustsSize = true` the controller sizes the view to
        //    that fitting height — so if the titlebar can grow, measuredHeight
        //    will match bandHeight; if it clips, measuredHeight will cap below.
        let bandHost = NSHostingView(rootView: SpikeBand(state: state))
        // Explicit-frame sizing — the recipe real apps use for tall `.bottom`
        // accessories (favorites/filter bars). frame height is authoritative.
        bandHost.translatesAutoresizingMaskIntoConstraints = true
        bandHost.frame = NSRect(x: 0, y: 0, width: window.frame.width, height: bandHeight)
        bandHost.autoresizingMask = [.width]

        let accessory = NSTitlebarAccessoryViewController()
        accessory.layoutAttribute = layout       // MUST be set before attaching
        accessory.view = bandHost
        accessory.automaticallyAdjustsSize = false
        accessory.fullScreenMinHeight = bandHeight   // .bottom-only knob; test it holds in FS
        window.addTitlebarAccessoryViewController(accessory)

        // 3. Content (optionally a faux full-height floating sidebar card to
        //    expose any two-tier gap above it — Q5).
        let content = NSHostingController(
            rootView: SpikeContent(state: state, fauxSidebar: fauxSidebar)
        )
        window.contentViewController = content

        window.center()
        window.makeKeyAndOrderFront(nil)
        windows.append(window)

        // 4. Evidence logging — twice (immediately + after layout settles).
        logGeometry(window: window, accessory: accessory, state: state, when: "attach")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            logGeometry(window: window, accessory: accessory, state: state, when: "post-layout")
        }
    }

    private static func logGeometry(
        window: NSWindow,
        accessory: NSTitlebarAccessoryViewController,
        state: SpikeState,
        when: String
    ) {
        let accH = accessory.view.frame.height
        state.measuredHeight = accH
        func frame(_ b: NSWindow.ButtonType) -> String {
            guard let v = window.standardWindowButton(b) else { return "nil" }
            // Report in window-content coords for comparability.
            let f = v.convert(v.bounds, to: nil)
            return "(\(Int(f.origin.x)),\(Int(f.origin.y)) \(Int(f.width))x\(Int(f.height)))"
        }
        NSLog(
            "[TitlebarSpike:%@] layout=%@ themed=%@ material=%@ accessoryHeight=%.1f close=%@ min=%@ zoom=%@",
            when, state.layoutLabel, "\(state.themed)", "\(state.useMaterial)", accH,
            frame(.closeButton), frame(.miniaturizeButton), frame(.zoomButton)
        )
    }

    private static func defaultPalette() -> Palette {
        // Catppuccin Mocha is a literal non-system purple-navy — the strongest
        // signal for the Q3 "can we paint arbitrary non-system colors" test.
        .catppuccinMocha
    }

    /// Opt-in env launcher, e.g. `NICE_TITLEBAR_SPIKE=top-themed`.
    /// Values: `{top|bottom}[-themed][-sidebar][-material]`.
    static func autoOpenIfRequested() {
        guard let raw = ProcessInfo.processInfo.environment["NICE_TITLEBAR_SPIKE"],
              !raw.isEmpty else { return }
        let parts = Set(raw.lowercased().split(separator: "-").map(String.init))
        open(
            layout: parts.contains("bottom") ? .bottom : .top,
            themed: parts.contains("themed"),
            fauxSidebar: parts.contains("sidebar"),
            useMaterial: parts.contains("material"),
            bandHeight: parts.contains("tall") ? 80 : 52
        )
    }
}

// MARK: - The band

private struct SpikeBand: View {
    @Bindable var state: SpikeState
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        ZStack(alignment: .bottom) {
            // Background under test: themed solid fill, optionally over the
            // system `.sidebar` material so we can tell a system vibrancy
            // seam (Q3) from our own layer.
            ZStack {
                if state.useMaterial {
                    VisualEffectView(material: .sidebar, blendingMode: .behindWindow, state: .active)
                }
                if state.themed {
                    Color.niceChrome(scheme, state.palette)
                } else {
                    Color.gray.opacity(0.4)
                }
            }

            // Bottom hairline, mirroring the real band (AppShellView:603-606).
            if state.themed {
                Color.niceLine(scheme, state.palette).frame(height: 1)
            }

            // Self-labeling overlay + faux "pills" so the band height and
            // inline-vs-two-tier traffic-light relationship are obvious in a
            // screenshot.
            HStack(spacing: 8) {
                Text("accessory: \(state.layoutLabel)  h=\(Int(state.measuredHeight))pt")
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(state.themed ? Color.white : Color.primary)
                ForEach(0..<3, id: \.self) { i in
                    Text("Pane \(i + 1)")
                        .font(.system(size: 12))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(
                            RoundedRectangle(cornerRadius: 6)
                                .fill(.white.opacity(0.15))
                        )
                        .foregroundStyle(state.themed ? Color.white : Color.primary)
                }
                Spacer()
            }
            .padding(.horizontal, 14)
            .frame(maxHeight: .infinity)   // vertically center within the band
        }
        // Request a fixed intrinsic height — this is what we compare the
        // measured accessory height against (Q1 disambiguation).
        .frame(maxWidth: .infinity)
        .frame(height: state.bandHeight)
    }
}

// MARK: - The content body (+ optional faux sidebar + live-theme Picker)

private struct SpikeContent: View {
    @Bindable var state: SpikeState
    let fauxSidebar: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(spacing: 0) {
            if fauxSidebar {
                SpikeSidebarCard(state: state)
            }
            VStack(alignment: .leading, spacing: 16) {
                Text("Spike content area")
                    .font(.title2)
                Text("Drag the band to move • double-click to zoom • ⌃⌘F for full screen")
                    .foregroundStyle(.secondary)
                Picker("Live palette", selection: $state.palette) {
                    ForEach(Palette.allCases) { p in
                        Text(p.rawValue).tag(p)
                    }
                }
                .pickerStyle(.segmented)
                .frame(maxWidth: 420)
                Spacer()
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .windowBackgroundColor))
    }
}

private struct SpikeSidebarCard: View {
    @Bindable var state: SpikeState
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(state.themed ? Color.niceBg2(scheme, state.palette) : Color.gray.opacity(0.25))
            .overlay(
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(["Search", "Home", "Library", "Playlists"], id: \.self) { item in
                        Text(item)
                            .font(.system(size: 13))
                            .foregroundStyle(state.themed ? Color.white.opacity(0.85) : Color.primary)
                    }
                    Spacer()
                }
                .padding(14),
                alignment: .topLeading
            )
            .frame(width: 220)
            .padding(6)
            .shadow(radius: 8, y: 2)
    }
}

// MARK: - Debug menu hook

struct TitlebarSpikeMenu: Commands {
    var body: some Commands {
        CommandMenu("Titlebar Spike") {
            Button("Top · plain") { TitlebarSpike.open(layout: .top, themed: false, fauxSidebar: false, useMaterial: false) }
            Button("Top · themed") { TitlebarSpike.open(layout: .top, themed: true, fauxSidebar: false, useMaterial: false) }
            Button("Top · themed + material") { TitlebarSpike.open(layout: .top, themed: true, fauxSidebar: false, useMaterial: true) }
            Button("Top · themed + sidebar") { TitlebarSpike.open(layout: .top, themed: true, fauxSidebar: true, useMaterial: false) }
            Divider()
            Button("Bottom · plain") { TitlebarSpike.open(layout: .bottom, themed: false, fauxSidebar: false, useMaterial: false) }
            Button("Bottom · themed") { TitlebarSpike.open(layout: .bottom, themed: true, fauxSidebar: false, useMaterial: false) }
            Button("Bottom · themed + material") { TitlebarSpike.open(layout: .bottom, themed: true, fauxSidebar: false, useMaterial: true) }
            Button("Bottom · themed + sidebar") { TitlebarSpike.open(layout: .bottom, themed: true, fauxSidebar: true, useMaterial: false) }
        }
    }
}
