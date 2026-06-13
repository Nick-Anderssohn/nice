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
import UniformTypeIdentifiers

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
        .frame(height: WindowChrome.topBarHeight)
        .frame(maxWidth: .infinity)
        .background {
            ZStack {
                Color.niceChrome(scheme, palette)
                // The frontmost view of the chrome background. It vends a
                // `ChromeDragStripView` marker that `ChromeEventRouter`
                // hit-tests per-press: empty-chrome presses resolve to it and
                // the router owns drag-to-move + double-click-zoom, while
                // pills/buttons hit-test to themselves and are passed through.
                WindowDragRegion()
            }
        }
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
        // Empty-chrome drag + double-click-zoom are owned by
        // `ChromeEventRouter`. There is no SwiftUI drag gesture and no
        // window-drag veto flag any more: the router's per-press hit-test IS
        // the arbitration. A pill press hit-tests to a `PaneDragHosting`
        // view, so the router passes it through and never arms a window drag
        // — selectivity by construction, not by a flag that can stick.
    }
}

// MARK: - Pane drag state

/// Which strip is currently painting an insertion line, and where. The
/// horizontal analog of `SidebarDropTarget`. `tabId` scopes the line to
/// one strip so a future multi-strip drag can't leave a stale line in
/// another tab's strip.
struct PaneDropTarget: Equatable {
    let tabId: String
    let indicator: PaneDropIndicator
}

/// One active pane-pill drag, start to finish. Bundles the dragged
/// pane's full origin (identity + source context, for forward-compat
/// cross-tab/window drags) with the current insertion-line target, so
/// clearing the session (`dragState.session = nil`) wipes both at once.
/// Mirrors `SidebarDragSession`.
struct PaneDragSession: Equatable {
    let origin: PaneDragOrigin
    var target: PaneDropTarget?
}

/// Ephemeral, view-layer pane-drag state. Owned by `InlinePaneStrip`
/// via `@State` and propagated to its pills via `.environment(_:)` —
/// kept off the persistent model, exactly like `SidebarDragState`. The
/// SwiftUI drop API exposes the cursor location but not the payload
/// until the drop commits, so the pill's `PaneDragSource` coordinator
/// stashes the origin here for synchronous access during hover.
@MainActor
@Observable
final class PaneStripDragState {
    var session: PaneDragSession?
    /// Insertion target painted for a drag that originated in ANOTHER
    /// window (a cross-window move). This window has no local `session`
    /// for such a drag, so the foreign target lives here. `tabId`-scoped
    /// just like `session.target` so only the hovered strip paints.
    var foreignTarget: PaneDropTarget?
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
    @Environment(AppState.self) private var appState
    @Environment(NiceServices.self) private var services
    @Environment(\.openWindow) private var openWindow
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

    /// Ephemeral pane-reorder drag state, propagated to the pills via
    /// `.environment` so each pill's `PaneDragSource` coordinator can
    /// stash its origin and the drop delegate / insertion line can read
    /// the live target.
    /// Sidebar parity (`SidebarView` owns `SidebarDragState` the same
    /// way).
    @State private var dragState = PaneStripDragState()

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
        // Propagate the drag state to every pill so `PaneDragSource` can
        // stash its origin at drag start (sidebar parity).
        .environment(dragState)
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
                    ForEach(Array(tab.panes.enumerated()), id: \.element.id) { index, pane in
                        pillCell(tab: tab, index: index, pane: pane)
                            .id(pane.id)
                            // Measure the pill's frame in the OUTER SwiftUI
                            // tree. This `.background` sits outside the
                            // `PaneDragSource` host on purpose: SwiftUI
                            // preferences (and the named coordinate space)
                            // do not cross an `NSHostingView` boundary, so
                            // a GeometryReader inside the hosted pill could
                            // not report into `paneStripCoordinateSpace`.
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
            // Insertion line painted at the resolved drop slot. Lives in
            // the named coordinate space so `indicatorX` (derived from
            // `paneFrames`, which are viewport-relative in that same
            // space) lines up with the rendered pills. Horizontal analog
            // of the sidebar's insertion line.
            .overlay(alignment: .leading) {
                if let x = indicatorX(for: tab.id) {
                    insertionLine.offset(x: x - 1)
                }
            }
            // Drop side of the reorder. Attached to the ScrollView (same
            // view that owns `paneStripCoordinateSpace`) so
            // `DropInfo.location` shares coordinates with `paneFrames`.
            .onDrop(
                of: [.text],
                delegate: PaneStripDropDelegate(
                    tabId: tab.id,
                    paneFramesProvider: { paneFrames },
                    paneOrderProvider: { tab.panes.map(\.id) },
                    tabs: tabs,
                    dragState: dragState,
                    appState: appState,
                    services: services
                )
            )
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

    /// One pill, wrapped in its AppKit `PaneDragSource`. The host owns
    /// the press → tap-vs-drag decision and acts as the
    /// `NSDraggingSource` for reorder / cross-window move / tear-off; the
    /// inner `InlinePanePill` renders the pill and keeps all its SwiftUI
    /// interactions (select / rename / close / hover) via forwarded taps.
    /// `.frame(...).fixedSize()` pins the host to the pill's natural size
    /// so the strip lays pills out left-to-right exactly as before.
    @ViewBuilder
    private func pillCell(tab: Tab, index: Int, pane: Pane) -> some View {
        PaneDragSource(
            paneId: pane.id,
            sourceTabId: tab.id,
            sourceIndex: index,
            sourceWindowSessionId: appState.windowSession.windowSessionId,
            services: services,
            sessions: sessions,
            dragState: dragState,
            openWindow: { token in openWindow(id: "main", value: token) }
        ) {
            InlinePanePill(
                tabId: tab.id,
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
                    sessions.setActivePane(tabId: tab.id, paneId: pane.id)
                },
                onClose: {
                    closer.requestClosePane(tabId: tab.id, paneId: pane.id)
                }
            )
        }
        .frame(maxWidth: 220, maxHeight: 28)
        .fixedSize()
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

    /// The drop indicator for `tabId`'s strip, if the live drag session
    /// is currently targeting it. Filtered by `tabId` so a future
    /// multi-strip drag can't paint a line in the wrong strip (sidebar
    /// parity — `myIndicator` there filters by `projectId`).
    private func myIndicator(for tabId: String) -> PaneDropIndicator? {
        // Local reorder target wins; otherwise a cross-window (foreign)
        // drag hovering this strip paints from `foreignTarget`.
        if let target = dragState.session?.target, target.tabId == tabId {
            return target.indicator
        }
        if let foreign = dragState.foreignTarget, foreign.tabId == tabId {
            return foreign.indicator
        }
        return nil
    }

    /// X-position (in the strip's coordinate space) at which to paint the
    /// insertion line: the leading edge of the target pill for a
    /// `.paneBefore`, its trailing edge for a `.paneAfter`.
    private func indicatorX(for tabId: String) -> CGFloat? {
        switch myIndicator(for: tabId) {
        case let .paneBefore(id): return paneFrames[id]?.minX
        case let .paneAfter(id):  return paneFrames[id]?.maxX
        case nil:                 return nil
        }
    }

    /// 2pt vertical accent-colored insertion line — the horizontal
    /// analog of the sidebar's 2pt horizontal line. Never hit-tests, so
    /// it can't interfere with the live drop.
    private var insertionLine: some View {
        Rectangle()
            .fill(Color.niceAccent)
            .frame(width: 2)
            .frame(height: 28)
            .allowsHitTesting(false)
    }
}

// MARK: - Pane strip drop delegate

/// Drop delegate attached to the pane strip's ScrollView. Picks the slot
/// the cursor points at via `PaneStripDropResolver` (kept separate so it
/// stays unit-testable without a live drag) and commits the resulting
/// `movePane` on the next runloop tick — mutating `tab.panes` inline
/// from `performDrop` leaves AppKit's drag tracker stuck on the old
/// view hierarchy, exactly as documented for `ProjectGroupDropDelegate`.
private struct PaneStripDropDelegate: DropDelegate {
    let tabId: String
    let paneFramesProvider: () -> [String: CGRect]
    let paneOrderProvider: () -> [String]
    let tabs: TabModel
    let dragState: PaneStripDragState
    /// This strip's own window state — the migration TARGET for a
    /// cross-window drop, and the source of this window's id.
    let appState: AppState
    let services: NiceServices

    private var myWindowId: String { appState.windowSession.windowSessionId }

    func validateDrop(info: DropInfo) -> Bool {
        info.hasItemsConforming(to: [.text])
    }

    func dropEntered(info: DropInfo) {
        updateIndicator(for: info)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        updateIndicator(for: info)
        return DropProposal(operation: ownsCurrentIndicator ? .move : .forbidden)
    }

    func dropExited(info: DropInfo) {
        if dragState.session?.target?.tabId == tabId {
            dragState.session?.target = nil
        }
        if dragState.foreignTarget?.tabId == tabId {
            dragState.foreignTarget = nil
        }
    }

    func performDrop(info: DropInfo) -> Bool {
        // Local drag (this window is the source): the existing intra-
        // window reorder path. Read the slot, clear the local session,
        // and drop the published live-pane handle (an intra-window
        // reorder never migrates the live pty).
        if let session = dragState.session {
            let outcome = resolve(draggedPaneId: session.origin.paneId, info: info)
            dragState.session = nil
            services.livePaneRegistry.withdraw(paneId: session.origin.paneId)
            guard let outcome, case let .slot(targetId, placeAfter) = outcome.destination
            else { return false }
            // Defer — an inline mutation here wedges AppKit's drag tracker.
            DispatchQueue.main.async { [tabs, tabId] in
                tabs.movePane(outcome.draggedId, inTab: tabId,
                              relativeTo: targetId, placeAfter: placeAfter)
            }
            return true
        }

        // Foreign drag (originated in another window): commit a cross-
        // window move into this strip's tab. Terminal panes resolve a
        // slot; Claude panes ignore it (they become a new tab). Deferred
        // for the same drag-tracker reason — and because re-hosting the
        // migrated NSView mid-drop is exactly what wedges it.
        guard foreignDrag != nil else { return false }
        let slot = PaneStripDropResolver.paneTarget(
            x: info.location.x,
            paneOrder: paneOrderProvider(),
            paneFrames: paneFramesProvider()
        )
        dragState.foreignTarget = nil
        let targetTabId = tabId
        // The coordinator re-reads the in-flight handle from the registry
        // (which still holds it until claimed), so nothing to capture here
        // beyond the slot.
        DispatchQueue.main.async { [services, appState] in
            PaneMigrationCoordinator(services: services).commitCrossWindowMove(
                into: appState,
                targetTabId: targetTabId,
                relativeToPaneId: slot?.targetId,
                placeAfter: slot?.placeAfter ?? false
            )
        }
        return true
    }

    private func updateIndicator(for info: DropInfo) {
        // Local reorder indicator.
        if let session = dragState.session {
            if let outcome = resolve(draggedPaneId: session.origin.paneId, info: info),
               let indicator = outcome.indicator {
                dragState.session?.target = PaneDropTarget(tabId: tabId, indicator: indicator)
            } else if dragState.session?.target?.tabId == tabId {
                dragState.session?.target = nil
            }
            return
        }
        // Foreign (cross-window) indicator: only terminal panes land in
        // the strip, so only they paint an insertion line. A Claude pane
        // becomes a new tab — accept the drop but show no slot line.
        guard let drag = foreignDrag, drag.kind == .terminal,
              let slot = PaneStripDropResolver.paneTarget(
                x: info.location.x,
                paneOrder: paneOrderProvider(),
                paneFrames: paneFramesProvider()
              )
        else {
            if dragState.foreignTarget?.tabId == tabId { dragState.foreignTarget = nil }
            return
        }
        let indicator: PaneDropIndicator = slot.placeAfter
            ? .paneAfter(slot.targetId) : .paneBefore(slot.targetId)
        dragState.foreignTarget = PaneDropTarget(tabId: tabId, indicator: indicator)
    }

    private var ownsCurrentIndicator: Bool {
        dragState.session?.target?.tabId == tabId
            || dragState.foreignTarget?.tabId == tabId
            || (dragState.session == nil && foreignDrag != nil)
    }

    /// The in-flight cross-window drag targeting this window, with the
    /// dragged pane's kind resolved from its source window — or nil when
    /// there's no foreign drag (no in-flight handle, or it originated in
    /// this same window, i.e. an intra-window reorder).
    private var foreignDrag: (handle: LivePaneRegistry.Handle, kind: PaneKind)? {
        guard let handle = services.livePaneRegistry.currentDrag,
              handle.sourceWindowSessionId != myWindowId,
              let source = services.registry.appState(forSessionId: handle.sourceWindowSessionId),
              let kind = source.tabs.tab(for: handle.sourceTabId)?
                .panes.first(where: { $0.id == handle.paneId })?.kind
        else { return nil }
        return (handle, kind)
    }

    private func resolve(draggedPaneId: String, info: DropInfo) -> PaneStripDropResolver.Outcome? {
        PaneStripDropResolver.resolve(
            draggedPaneId: draggedPaneId,
            location: info.location,
            paneOrder: paneOrderProvider(),
            paneFrames: paneFramesProvider(),
            wouldMovePane: { dragged, target, after in
                tabs.wouldMovePane(dragged, inTab: tabId, relativeTo: target, placeAfter: after)
            }
        )
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
    @Environment(TabModel.self) private var tabs
    @Environment(SessionsModel.self) private var sessions

    let tabId: String
    let pane: Pane
    let isActive: Bool
    let isHovered: Bool
    let onHoverChange: (Bool) -> Void
    let onSelect: () -> Void
    let onClose: () -> Void

    @State private var isEditing = false
    @State private var draftTitle = ""
    /// Wall-clock time at which this pill most recently became active.
    /// Used to gate click-to-rename by `NSEvent.doubleClickInterval` so
    /// the same click that selects a pill (or arrives within the
    /// double-click window) can't also trigger an edit — mirrors the
    /// sidebar `TabRow` behavior.
    @State private var activatedAt: Date?
    @FocusState private var titleFocused: Bool
    /// AppKit mouse-down monitor installed while editing. SwiftUI's
    /// `@FocusState` does not deassert when an embedded `NSView`
    /// (the terminal) steals first responder, so `onChange(of:
    /// titleFocused)` alone can't catch click-away. The monitor
    /// commits the draft on any click outside the field's window-
    /// local frame.
    @State private var mouseMonitor: Any?
    @State private var fieldFrameInWindow: NSRect = .zero
    @State private var fieldWindowNumber: Int = 0

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

    /// True if this pill was activated long enough ago for a subsequent
    /// tap on the title to count as a deliberate rename request.
    private var renameAllowed: Bool {
        InlineRenameClickGate.canBeginEdit(
            activatedAt: activatedAt,
            now: Date(),
            doubleClickInterval: NSEvent.doubleClickInterval
        )
    }

    private var renameMenuLabel: String {
        pane.kind == .terminal ? "Rename Terminal" : "Rename Pane"
    }

    private var closeMenuLabel: String {
        pane.kind == .terminal ? "Close Terminal" : "Close Pane"
    }

    var body: some View {
        HStack(spacing: 7) {
            // Leading icon — status dot for Claude, terminal glyph
            // otherwise.
            leadingIcon

            titleView

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
        // NOTE: the pill's drag is owned by the AppKit `PaneDragSource`
        // host that wraps this view (see `InlinePaneStrip.pillCell`), not
        // a SwiftUI `.onDrag`. The host needs an `NSDraggingSource`'s
        // drag-ended-outside callback to drive desktop tear-off, which
        // pure SwiftUI cannot deliver. The host sets `dragState.session`
        // and publishes the live-pane handle at drag start. A pill press
        // never moves the window because the host hit-tests to itself (a
        // `PaneDragHosting` view): `ChromeEventRouter` sees the pill in the
        // ancestor chain and passes the press through. See `PaneDragSource`.
        .onTapGesture {
            // Title taps are handled by `titleView`'s own gesture; this
            // catches taps on the icon, padding, or empty pill area.
            // Skipped while editing so a stray tap doesn't spuriously
            // re-select the pane mid-edit.
            if !isEditing { onSelect() }
        }
        .onHover { onHoverChange($0) }
        .animation(.easeInOut(duration: 0.12), value: isActive)
        .animation(.easeInOut(duration: 0.12), value: isHovered)
        .contextMenu {
            Button(renameMenuLabel) {
                onSelect()
                beginEditing()
            }
            .accessibilityIdentifier("tab.pill.\(pane.id).renamePane")
            Button(closeMenuLabel) {
                if isEditing { commitEdit() }
                onClose()
            }
            .accessibilityIdentifier("tab.pill.\(pane.id).closePane")
        }
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
        .onAppear {
            if isActive && activatedAt == nil { activatedAt = Date() }
        }
        .onChange(of: isActive) { _, nowActive in
            if nowActive {
                activatedAt = Date()
            } else {
                activatedAt = nil
                // Keyboard pane switches and parent-driven activation
                // changes don't go through the mouse monitor, so commit
                // the rename when the pill is deactivated while editing.
                if isEditing { commitEdit() }
            }
        }
        .onDisappear {
            if isEditing { commitEdit() }
            removeMouseMonitor()
        }
    }

    @ViewBuilder
    private var titleView: some View {
        if isEditing {
            TextField("", text: $draftTitle)
                .textFieldStyle(.plain)
                .font(.system(size: 12, weight: textWeight))
                .foregroundStyle(textColor)
                .focused($titleFocused)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(WindowFrameReporter { frame, windowNumber in
                    fieldFrameInWindow = frame
                    fieldWindowNumber = windowNumber
                })
                .onSubmit { commitEdit() }
                .onExitCommand { cancelEdit() }
                .onChange(of: titleFocused) { _, focused in
                    if !focused && isEditing { commitEdit() }
                }
                .accessibilityIdentifier("tab.pill.\(pane.id).titleField")
        } else {
            Text(pane.title)
                .font(.system(size: 12, weight: textWeight))
                .foregroundStyle(textColor)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
                .onTapGesture {
                    if isActive {
                        if renameAllowed { beginEditing() }
                    } else {
                        onSelect()
                    }
                }
                .accessibilityIdentifier("tab.pill.\(pane.id).title")
        }
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
        // Pre-commit any in-flight edit so a click on X doesn't drop the
        // user's draft before the pane is destroyed.
        CloseXButton(paneId: pane.id) {
            if isEditing { commitEdit() }
            onClose()
        }
    }

    private func beginEditing() {
        draftTitle = pane.title
        isEditing = true
        titleFocused = true
        installMouseMonitor()
    }

    private func commitEdit() {
        guard isEditing else { return }
        isEditing = false
        titleFocused = false
        removeMouseMonitor()
        // `renamePane` trims internally; an empty submit resets the
        // pane to its per-kind auto-default and clears the manual-set
        // lock (releasing OSC titles to drive the pill again). A
        // non-empty submit flips the lock so subsequent OSC titles
        // can't clobber the user's choice.
        tabs.renamePane(tabId: tabId, paneId: pane.id, to: draftTitle)
        sessions.focusActiveTerminal()
    }

    private func cancelEdit() {
        guard isEditing else { return }
        isEditing = false
        titleFocused = false
        removeMouseMonitor()
        sessions.focusActiveTerminal()
    }

    private func installMouseMonitor() {
        removeMouseMonitor()
        mouseMonitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { event in
            guard isEditing else { return event }
            if event.window?.windowNumber == fieldWindowNumber,
               !fieldFrameInWindow.insetBy(dx: -2, dy: -2).contains(event.locationInWindow) {
                // Defer so the mouse-down finishes dispatching to its
                // destination view (the close X, a sibling pill, the
                // terminal grabbing focus) before we tear down the field.
                DispatchQueue.main.async { commitEdit() }
            }
            return event
        }
    }

    private func removeMouseMonitor() {
        if let monitor = mouseMonitor {
            NSEvent.removeMonitor(monitor)
            mouseMonitor = nil
        }
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
        .environment(appState.windowSession)
        .environment(NiceServices())
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
        .environment(appState.windowSession)
        .environment(NiceServices())
        .environment(Tweaks())
        .frame(width: 1180)
        .preferredColorScheme(.dark)
}
