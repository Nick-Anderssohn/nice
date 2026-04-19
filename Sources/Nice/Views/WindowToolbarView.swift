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
    @EnvironmentObject private var appState: AppState
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
            InlineTabsView()
                .frame(maxWidth: .infinity, alignment: .leading)
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

// MARK: - Inline tabs strip

/// Scrolls horizontally through the active tab's panes, rendering each as
/// an `InlineTabPill`. The trailing `NewTabBtn` stays pinned; it adds a
/// terminal pane to the active tab.
private struct InlineTabsView: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme

    /// Tracks which pill (if any) the mouse is currently over, keyed by
    /// `Pane.id`. Lives in the container so sibling pills can coordinate
    /// (e.g. only one close "×" ever visible at a time).
    @State private var hoveredPaneId: String? = nil

    private var activeTab: Tab? {
        guard let id = appState.activeTabId else { return nil }
        return appState.tab(for: id)
    }

    var body: some View {
        HStack(spacing: 2) {
            if let tab = activeTab {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 2) {
                        ForEach(tab.panes) { pane in
                            InlineTabPill(
                                pane: pane,
                                isActive: tab.activePaneId == pane.id,
                                canClose: tab.panes.count > 1,
                                isHovered: hoveredPaneId == pane.id,
                                onHoverChange: { hovering in
                                    if hovering {
                                        hoveredPaneId = pane.id
                                    } else if hoveredPaneId == pane.id {
                                        hoveredPaneId = nil
                                    }
                                },
                                onSelect: {
                                    appState.setActivePane(
                                        tabId: tab.id,
                                        paneId: pane.id
                                    )
                                },
                                onClose: {
                                    appState.requestClosePane(
                                        tabId: tab.id,
                                        paneId: pane.id
                                    )
                                }
                            )
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                NewTabBtn {
                    _ = appState.addPane(tabId: tab.id, kind: .terminal)
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
    }
}

// MARK: - Individual pill

private struct InlineTabPill: View {
    @Environment(\.colorScheme) private var scheme

    let pane: Pane
    let isActive: Bool
    let canClose: Bool
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
        canClose && (isHovered || isActive)
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
    WindowToolbarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.light)
}

#Preview("Toolbar — dark") {
    WindowToolbarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.dark)
}
