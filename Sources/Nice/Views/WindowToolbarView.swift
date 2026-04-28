//
//  WindowToolbarView.swift
//  Nice
//
//  Port of the `WindowToolbar` + `InlineTabs` + `InlineTab` + `NewTabBtn`
//  components from /tmp/nice-design/nice/project/nice/app.jsx (lines
//  ~269–600). The old companion-pane is gone; every Claude/terminal pane
//  now lives as a pill in this toolbar between the brand block and the
//  trailing edge.
//
//  Deliberate omissions (see spec):
//    • no mic button
//    • no "+" dropdown menu ("New Claude session")
//    • no keyboard shortcuts / drag-to-reorder
//
//  The window uses `.hiddenTitleBar` and the sidebar now runs floor-to-
//  ceiling, so the native traffic lights float on top of the sidebar —
//  this toolbar sits to the right of the sidebar and no longer needs to
//  reserve leading space for them.
//

import AppKit
import SwiftUI

struct WindowToolbarView: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    var body: some View {
        HStack(spacing: 10) {
            // Brand block.
            Logo()

            Text("Nice")
                .font(.system(size: 13, weight: .bold))
                .tracking(-0.2)
                .foregroundStyle(Color.niceInk(scheme, palette))
                .layoutPriority(1)

            // Vertical separator — width:1, height:20, margin: 0 6px.
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(width: 1, height: 20)
                .padding(.horizontal, 6)

            // Pill strip fills the remaining width.
            InlinePaneStrip()
                .frame(maxWidth: .infinity, alignment: .leading)

            // Trailing: update-available nudge. Renders nothing when
            // no update is known, so the toolbar is layout-identical
            // to before this feature in the common case.
            UpdateAvailablePill()
        }
        .padding(.leading, 14)
        .padding(.trailing, 20)
        .frame(height: 52)
        .frame(maxWidth: .infinity)
        .background {
            ZStack {
                Color.niceChrome(scheme, palette)
                // Sits on top of the chrome fill but behind the toolbar's
                // interactive children — pills/buttons still receive
                // their own clicks while empty chrome behaves like a
                // title bar (drag to move, double-click to zoom).
                WindowDragRegion()
            }
        }
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
    }
}

// MARK: - Inline pane strip

/// Animation duration shared by all pill / chrome state transitions in
/// this toolbar. The pills established 0.12s as the convention; the
/// overflow chrome (edge fades, chevron badge, menu show/hide) follows
/// suit so timings don't feel staggered.
private let panePillAnimationDuration: Double = 0.12

/// Scrolls horizontally through the active tab's panes, rendering each as
/// an `InlinePanePill`. The trailing `NewTabBtn` stays pinned; it adds a
/// terminal pane to the active tab.
///
/// Overflow handling: pills render at their natural width inside a
/// horizontal `ScrollView`. When the content exceeds the visible width:
///   • edge-fade gradients hint at offscreen content,
///   • an `OverflowMenuButton` appears between the strip and the "+",
///     listing every pane on the active tab,
///   • the active pane auto-scrolls into view,
///   • a status badge on the chevron flags any *fully offscreen* pane
///     that needs attention (`.thinking` or unacknowledged `.waiting`).
///
/// Layout-identical to its predecessor when the strip fits — the chevron
/// only renders when overflowing.
///
/// The decision is split across two pure-Swift helpers so they can be
/// unit-tested without a SwiftUI host:
///   • `PaneStripOverflowEstimator` decides whether the chevron renders,
///     using this view's outer bounds and a per-pill width estimate
///     (independent of the ScrollView, so it doesn't suffer from
///     SwiftUI's off-screen preference virtualization).
///   • `PaneStripGeometry` decides the cosmetic chrome — leading /
///     trailing edge fades and the offscreen pane set used by the
///     attention badge — from the per-pill frames the ScrollView does
///     emit for visible pills.
private struct InlinePaneStrip: View {
    @Environment(TabModel.self) private var tabs
    @Environment(SessionsModel.self) private var sessions
    @Environment(CloseRequestCoordinator.self) private var closer
    @Environment(\.colorScheme) private var scheme

    /// Tracks which pill (if any) the mouse is currently over, keyed by
    /// `Pane.id`. Lives in the container so sibling pills can coordinate
    /// (e.g. only one close "×" ever visible at a time).
    @State private var hoveredPaneId: String? = nil

    /// Each pill's frame in the ScrollView's named coordinate space
    /// `paneStripCoordinateSpace` — populated via `PaneFramePreferenceKey`.
    /// Used for cosmetic chrome (edge fades, attention badge) only;
    /// these can tolerate the one-frame propagation lag SwiftUI's
    /// preference system inflicts during scroll/layout transitions.
    @State private var paneFrames: [String: CGRect] = [:]

    /// The ScrollView's visible viewport width. Used by the cosmetic
    /// chrome only; the chevron's existence is gated on the more
    /// reliable `availableWidth` measured outside the ScrollView.
    @State private var visibleWidth: CGFloat = 0

    /// `InlinePaneStrip`'s own bounds — measured at the body level,
    /// which is *outside* the ScrollView. Drives the chevron's
    /// existence: combined with a per-pill width estimate, we can
    /// decide overflow without depending on any preference that
    /// originates from inside the ScrollView's content (which SwiftUI
    /// silently virtualizes for off-screen pills).
    @State private var availableWidth: CGFloat = 0

    private var activeTab: Tab? {
        guard let id = tabs.activeTabId else { return nil }
        return tabs.tab(for: id)
    }

    private var geometry: PaneStripGeometry {
        PaneStripGeometry(
            paneFrames: paneFrames,
            visibleWidth: visibleWidth
        )
    }

    private var showChevron: Bool {
        guard let tab = activeTab else { return false }
        return PaneStripOverflowEstimator.shouldShowChevron(
            panes: tab.panes,
            availableWidth: availableWidth
        )
    }

    var body: some View {
        HStack(spacing: 2) {
            if let tab = activeTab {
                strip(for: tab)

                if showChevron {
                    OverflowMenuButton(
                        panes: tab.panes,
                        activePaneId: tab.activePaneId,
                        hasAttention: tab.hasOffscreenAttention(
                            offscreenIds: geometry.offscreenPaneIds
                        ),
                        onSelect: { paneId in
                            sessions.setActivePane(
                                tabId: tab.id,
                                paneId: paneId
                            )
                        }
                    )
                    .padding(.leading, 4)
                    .transition(.opacity)
                }

                NewTabBtn {
                    _ = sessions.addPane(tabId: tab.id, kind: .terminal)
                }
                .padding(.leading, 4)
            } else {
                // No active tab — shouldn't happen in practice; render an
                // empty leading region and fall through to nothing. We
                // intentionally omit the "+" here because `addPane`
                // requires a tab id.
                Spacer(minLength: 0)
            }
        }
        .animation(
            .easeInOut(duration: panePillAnimationDuration),
            value: showChevron
        )
        // Measure our own bounds. We write directly to `@State` from
        // the GeometryReader closure rather than going through a
        // PreferenceKey + `.onPreferenceChange` because in this
        // particular view tree (something about the ScrollView
        // ancestry, possibly Swift 6 strict-concurrency interaction)
        // those preference closures simply never fire — paneFrames'
        // do, but the scalar-valued ones don't. `.onAppear` + a
        // value-binding `.onChange` on `geo.size.width` is the
        // boring-but-reliable alternative.
        .background(
            GeometryReader { geo in
                Color.clear
                    .onAppear { availableWidth = geo.size.width }
                    .onChange(of: geo.size.width) { _, newWidth in
                        availableWidth = newWidth
                    }
            }
        )
    }

    @ViewBuilder
    private func strip(for tab: Tab) -> some View {
        // Why `.background(GeometryReader)` instead of wrapping the
        // ScrollView in one: a wrapping GeometryReader inherits its
        // parent's full proposed size and forces the ScrollView to fill
        // the toolbar's 52pt height, which top-anchors the 28pt pills
        // instead of centering them. A background GeometryReader emits
        // a preference value but does not influence layout, so the
        // ScrollView keeps sizing to its content.
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 2) {
                    ForEach(tab.panes) { pane in
                        InlinePanePill(
                            pane: pane,
                            isActive: tab.activePaneId == pane.id,
                            isHovered: hoveredPaneId == pane.id,
                            onHoverChange: { hovering in
                                if hovering {
                                    hoveredPaneId = pane.id
                                } else if hoveredPaneId == pane.id {
                                    hoveredPaneId = nil
                                }
                            },
                            onSelect: {
                                sessions.setActivePane(
                                    tabId: tab.id,
                                    paneId: pane.id
                                )
                            },
                            onClose: {
                                closer.requestClosePane(
                                    tabId: tab.id,
                                    paneId: pane.id
                                )
                            }
                        )
                        .id(pane.id)
                        .background(
                            GeometryReader { geo in
                                Color.clear.preference(
                                    key: PaneFramePreferenceKey.self,
                                    value: [
                                        pane.id: geo.frame(
                                            in: .named(
                                                paneStripCoordinateSpace
                                            )
                                        )
                                    ]
                                )
                            }
                        )
                    }
                }
            }
            .coordinateSpace(name: paneStripCoordinateSpace)
            .background(
                GeometryReader { geo in
                    Color.clear.preference(
                        key: VisibleWidthPreferenceKey.self,
                        value: geo.size.width
                    )
                }
            )
            .onPreferenceChange(VisibleWidthPreferenceKey.self) { width in
                visibleWidth = width
            }
            .onPreferenceChange(PaneFramePreferenceKey.self) { frames in
                // Merge instead of replace: a SwiftUI horizontal
                // ScrollView stops emitting `GeometryReader` preferences
                // for pills that have scrolled off the visible region.
                // If we overwrote `paneFrames` with each new dict, the
                // rightmost pills' entries would silently vanish at
                // scroll-zero — collapsing `canScrollTrailing` to false
                // and hiding the chevron. Keep the last-known frame for
                // every pane id that's still in `tab.panes` and
                // overwrite only the keys we just heard about.
                let liveIds = Set(tab.panes.map(\.id))
                var merged = paneFrames.filter { liveIds.contains($0.key) }
                for (id, frame) in frames where liveIds.contains(id) {
                    merged[id] = frame
                }
                paneFrames = merged
            }
            .onChange(of: tab.activePaneId) { _, newId in
                guard let newId else { return }
                withAnimation(.easeInOut(duration: 0.18)) {
                    proxy.scrollTo(newId, anchor: .center)
                }
            }
            .overlay(alignment: .leading) {
                edgeFade(trailing: false)
                    .opacity(geometry.canScrollLeading ? 1 : 0)
                    .animation(
                        .easeInOut(duration: panePillAnimationDuration),
                        value: geometry.canScrollLeading
                    )
            }
            .overlay(alignment: .trailing) {
                edgeFade(trailing: true)
                    .opacity(geometry.canScrollTrailing ? 1 : 0)
                    .animation(
                        .easeInOut(duration: panePillAnimationDuration),
                        value: geometry.canScrollTrailing
                    )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func edgeFade(trailing: Bool) -> some View {
        LinearGradient(
            colors: [
                Color.niceChrome(scheme),
                Color.niceChrome(scheme).opacity(0)
            ],
            startPoint: trailing ? .trailing : .leading,
            endPoint: trailing ? .leading : .trailing
        )
        .frame(width: 16)
        .allowsHitTesting(false)
    }
}

/// Coordinate-space identifier used by `InlinePaneStrip` so each pill can
/// report its frame relative to the ScrollView's viewport (not the
/// underlying content) — that's what makes "is this pane visible right
/// now" a pure frame-vs-`[0, visibleWidth]` comparison.
private let paneStripCoordinateSpace = "InlinePaneStrip.scrollContent"

/// Collects each pill's frame keyed by `Pane.id`. The reduce merges
/// children's contributions; later writes for the same key win, which is
/// what we want when a pane resizes.
private struct PaneFramePreferenceKey: PreferenceKey {
    static let defaultValue: [String: CGRect] = [:]

    static func reduce(
        value: inout [String: CGRect],
        nextValue: () -> [String: CGRect]
    ) {
        value.merge(nextValue()) { _, new in new }
    }
}

/// The ScrollView's measured viewport width. Read via a `.background`
/// GeometryReader so it doesn't influence layout.
private struct VisibleWidthPreferenceKey: PreferenceKey {
    static let defaultValue: CGFloat = 0

    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

// MARK: - Individual pill

private struct InlinePanePill: View {
    @Environment(\.colorScheme) private var scheme

    let pane: Pane
    let isActive: Bool
    let isHovered: Bool
    let onHoverChange: (Bool) -> Void
    let onSelect: () -> Void
    let onClose: () -> Void

    private var background: Color {
        if isActive {
            return Color.nicePanel(scheme)
        }
        if isHovered {
            return Color.niceInk(scheme).opacity(0.05)
        }
        return .clear
    }

    private var borderColor: Color {
        isActive ? Color.niceLine(scheme) : .clear
    }

    private var textColor: Color {
        isActive ? Color.niceInk(scheme) : Color.niceInk2(scheme)
    }

    private var textWeight: Font.Weight {
        isActive ? .semibold : .medium
    }

    private var iconColor: Color {
        isActive ? Color.niceInk2(scheme) : Color.niceInk3(scheme)
    }

    private var showClose: Bool {
        isHovered || isActive
    }

    var body: some View {
        HStack(spacing: 7) {
            // Leading icon — status dot for Claude, terminal glyph
            // otherwise.
            leadingIcon

            Text(pane.title)
                .font(.system(size: 12, weight: textWeight))
                .foregroundStyle(textColor)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)

            // Trailing close "×". Own hit target so the pill's tap
            // doesn't fire when you click the X. We keep the frame
            // reserved even when hidden so the pill width doesn't jump
            // on hover.
            closeButton
                .opacity(showClose ? 1 : 0)
                .animation(.easeInOut(duration: 0.12), value: showClose)
                .allowsHitTesting(showClose)
        }
        .padding(.leading, 10)
        .padding(.trailing, 6)
        .frame(height: 28)
        .frame(maxWidth: 220)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(background)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .stroke(borderColor, lineWidth: 1)
        )
        .shadow(
            color: isActive ? Color.black.opacity(0.04) : .clear,
            radius: 1,
            x: 0,
            y: 1
        )
        .contentShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
        .onTapGesture { onSelect() }
        .onHover { onHoverChange($0) }
        .animation(.easeInOut(duration: 0.12), value: isActive)
        .animation(.easeInOut(duration: 0.12), value: isHovered)
        // Expose the pill as a single AXButton that contains the close
        // button as a child (not a merged peer). Without `.contain`,
        // `.onTapGesture` + `.accessibilityAddTraits(.isButton)` emit
        // two AXButton nodes sharing this identifier, which breaks any
        // XCUITest that counts pills; with `.combine` we'd instead
        // swallow the close button's own identity.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("tab.pill.\(pane.id)")
        .accessibilityLabel(pane.title)
        .accessibilityAddTraits(isActive ? [.isSelected, .isButton] : .isButton)
    }

    @ViewBuilder
    private var leadingIcon: some View {
        switch pane.kind {
        case .claude:
            StatusDot(
                status: pane.status,
                suppressWaitingPulse: pane.waitingAcknowledged
            )
        case .terminal:
            Image(systemName: "terminal")
                .font(.system(size: 12))
                .foregroundStyle(iconColor)
                .frame(width: 12, height: 12)
        }
    }

    private var closeButton: some View {
        CloseXButton(paneId: pane.id, onClose: onClose)
    }
}

/// The little "×" square on the trailing edge of a pill. Its own button so
/// taps don't bubble up to the pill's `onTapGesture`. Hover paints a
/// subtle background (10% ink), matching the JSX mock's `onMouseOver`
/// handler.
private struct CloseXButton: View {
    @Environment(\.colorScheme) private var scheme
    @State private var hovering = false

    let paneId: String
    let onClose: () -> Void

    var body: some View {
        Button(action: onClose) {
            Image(systemName: "xmark")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(Color.niceInk3(scheme))
                .frame(width: 16, height: 16)
                .background(
                    RoundedRectangle(cornerRadius: 4, style: .continuous)
                        .fill(
                            hovering
                                ? Color.niceInk(scheme).opacity(0.10)
                                : .clear
                        )
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityIdentifier("tab.close.\(paneId)")
        .accessibilityLabel("Close \(paneId)")
    }
}

// MARK: - Overflow menu

/// 22×22 square chevron button shown only when the pill strip overflows.
/// Tapping pops a SwiftUI `Menu` listing every pane on the active tab so
/// every pane is reachable in one click even when scrolled off-screen.
///
/// The menu doubles as an attention surface: when an offscreen pane is
/// `.thinking` or unacknowledged-`.waiting`, a small accent dot overlays
/// the chevron so the user notices without having to scan the strip.
private struct OverflowMenuButton: View {
    @Environment(\.colorScheme) private var scheme
    @State private var hovering = false

    let panes: [Pane]
    let activePaneId: String?
    let hasAttention: Bool
    let onSelect: (String) -> Void

    var body: some View {
        Menu {
            ForEach(panes) { pane in
                Button {
                    onSelect(pane.id)
                } label: {
                    rowLabel(for: pane)
                }
                .accessibilityIdentifier("tab.overflow.row.\(pane.id)")
            }
        } label: {
            chevron
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Show all panes")
        .accessibilityIdentifier("tab.overflow")
        .accessibilityLabel("Show all panes")
    }

    private var chevron: some View {
        Image(systemName: "chevron.down")
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(Color.niceInk2(scheme))
            .frame(width: 22, height: 22)
            .background(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(
                        hovering
                            ? Color.niceInk(scheme).opacity(0.08)
                            : .clear
                    )
            )
            .overlay(alignment: .topTrailing) {
                if hasAttention {
                    Circle()
                        .fill(Color.niceAccent)
                        .frame(width: 6, height: 6)
                        .offset(x: -3, y: 3)
                        .transition(.scale.combined(with: .opacity))
                }
            }
            .animation(
                .easeInOut(duration: panePillAnimationDuration),
                value: hasAttention
            )
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
    }

    @ViewBuilder
    private func rowLabel(for pane: Pane) -> some View {
        // Use Label so the system menu lays out icon + text the standard
        // way; `Image(systemName: "checkmark")` on the active row mirrors
        // AppKit menu conventions.
        Label {
            HStack(spacing: 6) {
                Text(pane.title)
                if pane.id == activePaneId {
                    Image(systemName: "checkmark")
                        .font(.system(size: 11, weight: .semibold))
                }
            }
        } icon: {
            switch pane.kind {
            case .claude:
                StatusDot(
                    status: pane.status,
                    suppressWaitingPulse: pane.waitingAcknowledged
                )
            case .terminal:
                Image(systemName: "terminal")
            }
        }
        .accessibilityLabel(rowAccessibilityLabel(for: pane))
    }

    /// VoiceOver text for a menu row. Surfaces the same "thinking" /
    /// "waiting" status that the inline pill's `StatusDot` exposes,
    /// since the dot in the menu row is decorative and merged into the
    /// Label's combined element.
    private func rowAccessibilityLabel(for pane: Pane) -> String {
        let suffix: String
        switch pane.kind {
        case .claude:
            switch pane.status {
            case .thinking: suffix = ", thinking"
            case .waiting:  suffix = ", waiting for input"
            case .idle:     suffix = ""
            }
        case .terminal:
            suffix = ", terminal"
        }
        let active = pane.id == activePaneId ? ", selected" : ""
        return pane.title + suffix + active
    }
}

// MARK: - New tab button

/// 22×22 square "+" button at the trailing edge of the pill strip. Taps
/// add a terminal pane to the active tab.
private struct NewTabBtn: View {
    @Environment(\.colorScheme) private var scheme
    @State private var hovering = false

    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "plus")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Color.niceInk2(scheme))
                .frame(width: 22, height: 22)
                .background(
                    RoundedRectangle(cornerRadius: 5, style: .continuous)
                        .fill(
                            hovering
                                ? Color.niceInk(scheme).opacity(0.08)
                                : .clear
                        )
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help("New tab")
        .accessibilityIdentifier("tab.add")
        .accessibilityLabel("New tab")
    }
}

// MARK: - Previews

#Preview("Toolbar — light") {
    let appState = AppState()
    return WindowToolbarView()
        .environment(appState)
        .environment(appState.tabs)
        .environment(appState.sessions)
        .environment(appState.closer)
        .environment(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.light)
}

#Preview("Toolbar — dark") {
    let appState = AppState()
    return WindowToolbarView()
        .environment(appState)
        .environment(appState.tabs)
        .environment(appState.sessions)
        .environment(appState.closer)
        .environment(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.dark)
}
