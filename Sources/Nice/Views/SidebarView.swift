//
//  SidebarView.swift
//  Nice
//
//  The expanded 240pt sidebar column. The collapsed state is handled
//  upstream in `AppShellView` as a small top-bar cap, so this view
//  is only instantiated when `sidebar.sidebarCollapsed == false`.
//
//  The column background is owned upstream by `AppShellView` via
//  `SidebarBackground` (flat panel for `.nice`, wallpaper-tinted
//  `NSVisualEffectView` for `.macOS`); this view paints no background
//  of its own so vibrancy shows through.
//

import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SidebarView: View {
    @Environment(TabModel.self) private var tabs
    @Environment(SidebarModel.self) private var sidebar
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(\.openSettings) private var openSettings
    @State private var dragState = SidebarDragState()

    var body: some View {
        expandedSidebar
            .environment(dragState)
    }

    // MARK: - Expanded sidebar

    @ViewBuilder
    private var expandedSidebar: some View {
        // Peek state always shows the tabs view: holding the
        // tab-cycling shortcut means the user is picking a tab, so
        // even if `sidebarMode == .files` we surface the project list
        // here. The non-peek expanded sidebar respects sidebarMode.
        if sidebar.sidebarPeeking {
            VStack(spacing: 0) {
                tabList
                footer
            }
        } else {
            VStack(spacing: 0) {
                switch sidebar.sidebarMode {
                case .tabs:  tabList
                case .files: FileBrowserView()
                }
                footer
            }
        }
    }

    // MARK: - Tab list

    private var tabList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(tabs.projects) { project in
                    ProjectGroup(project: project)
                }
            }
            .padding(.vertical, 10)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: 4) {
            Spacer(minLength: 0)
            SidebarIconButton(
                systemImage: "gearshape",
                help: "Settings",
                accessibilityId: "sidebar.settings"
            ) {
                openSettings()
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
    }
}

// MARK: - Project group

private struct ProjectGroup: View {
    @Environment(TabModel.self) private var tabs
    @Environment(SessionsModel.self) private var sessions
    @Environment(CloseRequestCoordinator.self) private var closer
    @Environment(Tweaks.self) private var tweaks
    @Environment(FontSettings.self) private var fontSettings
    @Environment(SidebarDragState.self) private var dragState
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let project: Project
    @State private var isOpen: Bool = true
    @State private var headerHover: Bool = false
    /// Frames of each TabRow in the group's local coordinate space,
    /// collected via preference key. The drop delegate uses these to
    /// pick the tab whose slot the cursor is in and to position the
    /// insertion line on the row's top or bottom edge.
    @State private var tabFrames: [String: CGRect] = [:]

    private var isTerminalsGroup: Bool {
        project.id == TabModel.terminalsProjectId
    }

    /// Always show the `+` for the Terminals group (so the user can
    /// re-add after emptying it) but only reveal it on hover for
    /// ordinary project groups — keeps the sidebar clean by default.
    private var showAddButton: Bool {
        isTerminalsGroup || headerHover
    }

    private var coordSpace: String { "sidebar.group.\(project.id)" }

    /// The indicator for this project group, if any. Reads from the
    /// sidebar-wide drag session and filters to this group's own
    /// project id, so a new delegate writing to the shared state
    /// automatically clears any stale line in another group.
    private var myIndicator: DropIndicator? {
        guard let target = dragState.session?.target,
              target.projectId == project.id
        else { return nil }
        return target.indicator
    }

    /// Y-position (in the group's coordinate space) at which the
    /// insertion line should be painted, if any.
    private var indicatorY: CGFloat? {
        guard let indicator = myIndicator else { return nil }
        switch indicator {
        case let .tabBefore(id):  return tabFrames[id]?.minY
        case let .tabAfter(id):   return tabFrames[id]?.maxY
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if isOpen {
                ForEach(project.tabs) { tab in
                    TabRow(tab: tab)
                        .background(
                            GeometryReader { geo in
                                Color.clear.preference(
                                    key: TabFramesKey.self,
                                    value: [tab.id: geo.frame(in: .named(coordSpace))]
                                )
                            }
                        )
                }
            }
        }
        .padding(.bottom, 4)
        .coordinateSpace(name: coordSpace)
        .overlay(alignment: .top) {
            if let y = indicatorY {
                insertionLine.offset(y: y - 1)
            }
        }
        .onPreferenceChange(TabFramesKey.self) { frames in
            Task { @MainActor in tabFrames = frames }
        }
        .onDrop(
            of: [.text],
            delegate: ProjectGroupDropDelegate(
                project: project,
                tabFramesProvider: { tabFrames },
                tabOrderProvider: { project.tabs.map(\.id) },
                tabs: tabs,
                dragState: dragState
            )
        )
        // Make the whole project group addressable by UI tests even
        // when its add button is hover-gated (non-Terminals groups
        // hide it at opacity 0, which also strips accessibility).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sidebar.group.\(project.id)")
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.system(size: fontSettings.sidebarSize(10), weight: .semibold))
                .rotationEffect(.degrees(isOpen ? 90 : 0))
                .opacity(0.7)
                .animation(.easeInOut(duration: 0.12), value: isOpen)
                .contentShape(Rectangle())
                .onTapGesture { isOpen.toggle() }
            Text(project.name.uppercased())
                .font(.system(size: fontSettings.sidebarSize(12), weight: .semibold))
                .tracking(0.2)
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .contentShape(Rectangle())
                .onTapGesture { isOpen.toggle() }
            Spacer(minLength: 4)
            CountPill(count: project.tabs.count)
            AddTabButton(
                accessibilityId: "sidebar.group.\(project.id).add",
                help: isTerminalsGroup ? "New terminal tab" : "New Claude tab"
            ) {
                if isTerminalsGroup {
                    _ = sessions.createTerminalTab()
                } else {
                    _ = sessions.createClaudeTabInProject(projectId: project.id)
                }
            }
            .opacity(showAddButton ? 1 : 0)
            .animation(.easeInOut(duration: 0.12), value: showAddButton)
            .allowsHitTesting(showAddButton)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { headerHover = $0 }
        .contextMenu {
            if !isTerminalsGroup {
                Button("Close Project") {
                    closer.requestCloseProject(projectId: project.id)
                }
                .accessibilityIdentifier("sidebar.group.\(project.id).closeProject")
            }
        }
    }

    /// 2pt accent-colored insertion line. Inset 6pt horizontally to
    /// match the tab background's rounded rectangle; centered on its
    /// target y-position so adjacent drop slots paint on the same
    /// visual seam rather than jumping between rows.
    private var insertionLine: some View {
        Rectangle()
            .fill(tweaks.accent.color)
            .frame(height: 2)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 6)
            .allowsHitTesting(false)
    }
}

private struct CountPill: View {
    @Environment(FontSettings.self) private var fontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    let count: Int

    var body: some View {
        Text("\(count)")
            .font(.system(size: fontSettings.sidebarSize(10), weight: .medium))
            .foregroundStyle(Color.niceInk3(scheme, palette))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(
                Capsule().fill(Color.niceInk(scheme, palette).opacity(0.07))
            )
    }
}

// MARK: - Group "+" button

private struct AddTabButton: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let accessibilityId: String
    let help: String
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Button(action: action) {
            Image(systemName: "plus")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Color.niceInk2(scheme, palette))
                .frame(width: 18, height: 18)
                .background(
                    RoundedRectangle(cornerRadius: 4, style: .continuous)
                        .fill(
                            hover
                                ? Color.niceInk(scheme, palette).opacity(0.10)
                                : .clear
                        )
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .help(help)
        .accessibilityIdentifier(accessibilityId)
        .accessibilityLabel(help)
    }
}

// MARK: - Tab row

private struct TabRow: View {
    @Environment(TabModel.self) private var tabs
    @Environment(SessionsModel.self) private var sessions
    @Environment(CloseRequestCoordinator.self) private var closer
    @Environment(Tweaks.self) private var tweaks
    @Environment(FontSettings.self) private var fontSettings
    @Environment(SidebarDragState.self) private var dragState
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let tab: Tab
    @State private var hover = false
    @State private var isEditing = false
    @State private var draftTitle = ""
    /// Wall-clock time at which this row most recently became the active
    /// tab. Used to gate click-to-rename by `NSEvent.doubleClickInterval`
    /// so the same click that selects a tab (or arrives within the
    /// double-click window) can't also trigger an edit — matches Finder.
    @State private var activatedAt: Date?
    @FocusState private var titleFocused: Bool
    /// AppKit mouse-down monitor installed while editing. SwiftUI's
    /// `@FocusState` does not deassert when an embedded `NSView`
    /// (the terminal) steals first responder, so `onChange(of:
    /// titleFocused)` alone can't catch click-away. The monitor
    /// commits the draft on any click outside the field's window-
    /// local frame — terminal, window chrome, empty sidebar, etc.
    @State private var mouseMonitor: Any?
    @State private var fieldFrameInWindow: NSRect = .zero
    @State private var fieldWindowNumber: Int = 0

    private var isActive: Bool { tab.id == tabs.activeTabId }

    /// True if this row was activated long enough ago for a subsequent
    /// tap on the title to count as a deliberate rename request.
    private var renameAllowed: Bool {
        guard let activatedAt else { return false }
        return Date().timeIntervalSince(activatedAt) >= NSEvent.doubleClickInterval
    }

    private var backgroundColor: Color {
        if isActive { return Color.niceSel(scheme, accent: tweaks.accent.color) }
        if hover    { return Color.niceInk(scheme, palette).opacity(0.06) }
        return .clear
    }

    private var titleFont: Font {
        .system(size: fontSettings.sidebarSize(13), weight: isActive ? .semibold : .regular)
    }

    private func beginEditing() {
        draftTitle = tab.title
        isEditing = true
        titleFocused = true
        installMouseMonitor()
    }

    private func commitEdit() {
        guard isEditing else { return }
        isEditing = false
        titleFocused = false
        removeMouseMonitor()
        tabs.renameTab(id: tab.id, to: draftTitle)
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
                // destination view (e.g. the terminal grabbing focus,
                // or a different tab row receiving its tap) before we
                // tear down the field.
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

    var body: some View {
        // Sized for parity with Xcode's Project Navigator and the
        // file-browser rows: 13pt title, 6pt HStack spacing, 4pt
        // vertical padding, 4pt corner radius. The row reads as one
        // consistent sidebar regardless of mode.
        HStack(spacing: 6) {
            if tab.hasClaude {
                StatusDot(
                    status: tab.status,
                    suppressWaitingPulse: tab.waitingAcknowledged
                )
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).claudeIcon")
            } else {
                Image(systemName: "terminal")
                    .font(.system(size: fontSettings.sidebarSize(12), weight: .regular))
                    .foregroundStyle(Color.niceInk3(scheme, palette))
                    .frame(width: 16, height: 16)
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).terminalIcon")
            }
            titleView
        }
        // 22pt is the default sidebar inset (matches the project-group
        // header chevron + label gutter). Tabs spawned by /branch
        // tracking carry a `parentTabId` pointing at their sibling
        // pre-rotation tab; those render one indent level deeper so
        // the parent/child relationship reads at a glance. The
        // additional 16pt is roughly the visual width of a sidebar
        // status dot — enough to register as nested without crowding
        // the row text.
        .padding(.leading, tab.parentTabId == nil ? 22 : 38)
        .padding(.trailing, 10)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(backgroundColor)
        )
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture {
            if !isEditing {
                tabs.selectTab(tab.id)
            }
        }
        .contextMenu {
            Button("Rename Tab") {
                tabs.selectTab(tab.id)
                beginEditing()
            }
            .accessibilityIdentifier("sidebar.tab.\(tab.id).renameTab")
            Button("Close Tab") {
                closer.requestCloseTab(tabId: tab.id)
            }
            .accessibilityIdentifier("sidebar.tab.\(tab.id).closeTab")
        }
        .onDrag {
            dragState.session = SidebarDragSession(draggedTabId: tab.id, target: nil)
            return NSItemProvider(object: tab.id as NSString)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(accessibilityIdentifier)
        // Lineage marker for UITests: a hidden, zero-size sibling
        // element that exists iff this row is indented under a
        // /branch parent. The identifier carries the root tab's id so
        // tests can assert depth-1 layout without pixel-comparing the
        // padding constant. Lives in its own `branch.lineageChild.*`
        // namespace so existing `sidebar.tab.<id>`-prefix queries in
        // the UITest suite are unaffected.
        .background(lineageMarker)
        .onAppear {
            if isActive && activatedAt == nil { activatedAt = Date() }
        }
        .onChange(of: isActive) { _, nowActive in
            if nowActive {
                activatedAt = Date()
            } else {
                activatedAt = nil
                // Keyboard tab switches (⌘1…⌘9, ⌘⇧[ / ⌘⇧]) don't go
                // through the mouse monitor, so commit the rename
                // when the tab is deactivated while editing.
                if isEditing { commitEdit() }
            }
        }
    }

    /// The Main terminal tab keeps the legacy `sidebar.terminals`
    /// identifier so UI tests that targeted the old single top-level
    /// terminals row continue to locate it. All other tabs use the
    /// standard `sidebar.tab.<id>` form.
    private var accessibilityIdentifier: String {
        if tab.id == TabModel.mainTerminalTabId {
            return "sidebar.terminals"
        }
        return "sidebar.tab.\(tab.id)"
    }

    /// Hidden zero-size element whose accessibility identifier carries
    /// the /branch lineage relationship — present iff this row is
    /// indented under a parent tab. Lives in its own `branch.*`
    /// namespace so existing `sidebar.tab.<id>`-prefix UITest queries
    /// keep matching only the row itself, not this marker. UITests
    /// that want to assert "row X is depth-1 child of root Y" look up
    /// `branch.lineageChild.<X>.under.<Y>`.
    @ViewBuilder
    private var lineageMarker: some View {
        if let parent = tab.parentTabId {
            Color.clear
                .frame(width: 0, height: 0)
                .accessibilityElement()
                .accessibilityIdentifier(
                    "branch.lineageChild.\(tab.id).under.\(parent)"
                )
                .allowsHitTesting(false)
        }
    }

    @ViewBuilder
    private var titleView: some View {
        if isEditing {
            TextField("", text: $draftTitle)
                .textFieldStyle(.plain)
                .font(titleFont)
                .foregroundStyle(Color.niceInk(scheme, palette))
                .focused($titleFocused)
                .lineLimit(1)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Color.niceBg3(scheme, palette))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Color.niceLineStrong(scheme, palette), lineWidth: 1)
                )
                .background(WindowFrameReporter { frame, windowNumber in
                    fieldFrameInWindow = frame
                    fieldWindowNumber = windowNumber
                })
                .onSubmit { commitEdit() }
                .onExitCommand { cancelEdit() }
                .onChange(of: titleFocused) { _, focused in
                    if !focused && isEditing { commitEdit() }
                }
                .onDisappear { removeMouseMonitor() }
                .accessibilityIdentifier("sidebar.tab.\(tab.id).titleField")
        } else {
            Text(tab.title)
                .font(titleFont)
                .foregroundStyle(isActive ? Color.niceInk(scheme, palette) : Color.niceInk2(scheme, palette))
                .lineLimit(1)
                .truncationMode(.tail)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
                .onTapGesture {
                    if isActive {
                        if renameAllowed { beginEditing() }
                    } else {
                        tabs.selectTab(tab.id)
                    }
                }
                .accessibilityIdentifier("sidebar.tab.\(tab.id).title")
        }
    }
}

/// Measured frames of each TabRow inside a project group, keyed by
/// tab id and expressed in the group's own coordinate space. Merged
/// across `ForEach` children so `ProjectGroupDropDelegate` can map a
/// cursor y to the specific tab whose slot it lands in.
private struct TabFramesKey: PreferenceKey {
    static let defaultValue: [String: CGRect] = [:]
    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue(), uniquingKeysWith: { _, new in new })
    }
}

// MARK: - Drag session

/// Where a project group's insertion-line indicator is currently
/// painted. Carries the target tab id so the overlay can look up its
/// frame. Internal so the drop resolver tests can assert the
/// expected indicator for a drop outcome.
enum DropIndicator: Equatable {
    case tabBefore(String)
    case tabAfter(String)
}

/// Scoped drop-indicator state: which project's group is currently
/// painting an indicator, and which slot within it. Single-valued so
/// only one project group ever paints an insertion line at a time —
/// a new delegate writing this field implicitly clears any stale
/// indicator from another group.
struct SidebarDropTarget: Equatable {
    let projectId: String
    let indicator: DropIndicator
}

/// One active sidebar drag, start to finish. Bundles the dragged-tab
/// id with the current insertion-line target so clearing the session
/// (`dragState.session = nil`) wipes both in one assignment — a
/// caller physically cannot end the drag while leaving stale state
/// half-set.
struct SidebarDragSession: Equatable {
    let draggedTabId: String
    var target: SidebarDropTarget?
}

/// Ephemeral, view-layer sidebar-drag state. Owned by `SidebarView`
/// via `@State` and propagated to its subtree via `.environment(_:)`;
/// deliberately kept off `AppState` so the persistent model doesn't
/// accumulate transient UI scratchpads. The SwiftUI Transferable drop
/// API exposes the cursor location but not the payload until the drop
/// commits, so `TabRow`'s `onDrag` stashes the dragged id here for
/// synchronous access during hover.
@MainActor
@Observable
final class SidebarDragState {
    var session: SidebarDragSession?
}

/// Drop delegate attached to each project group's VStack. The
/// group-level drop region spans the header, every tab row, and the
/// 4pt trailing padding, so a tab can be dropped "above the first
/// tab" (cursor in the header area) or "below the last tab" (cursor
/// in the trailing gap) — not just onto another tab.
///
/// Slot-picking lives in `SidebarDropResolver` so it stays
/// unit-testable without a live drag session. Commits the resulting
/// `moveTab` on the next runloop tick: rearranging `tabs.projects`
/// inline from `performDrop` has been observed to leave AppKit's
/// drag tracking stuck on the old view hierarchy, which manifests
/// as subsequent drags not registering.
private struct ProjectGroupDropDelegate: DropDelegate {
    let project: Project
    let tabFramesProvider: () -> [String: CGRect]
    let tabOrderProvider: () -> [String]
    let tabs: TabModel
    let dragState: SidebarDragState

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
        // Only clear if this group owns the current indicator —
        // another group's `dropEntered` may already have overwritten
        // it.
        if ownsCurrentIndicator {
            dragState.session?.target = nil
        }
    }

    func performDrop(info: DropInfo) -> Bool {
        let outcome = dragState.session.flatMap {
            resolve(draggedTabId: $0.draggedTabId, info: info)
        }
        dragState.session = nil
        guard let outcome else { return false }
        // Defer the model mutation to the next runloop tick — see the
        // type-level doc for `ProjectGroupDropDelegate` on why
        // rearranging `tabs.projects` inline here leaves AppKit's
        // drag tracker stuck on a subsequent drag.
        DispatchQueue.main.async { [tabs] in
            tabs.moveTab(
                outcome.draggedId,
                relativeTo: outcome.targetId,
                placeAfter: outcome.placeAfter
            )
        }
        return true
    }

    private func updateIndicator(for info: DropInfo) {
        let outcome = dragState.session.flatMap {
            resolve(draggedTabId: $0.draggedTabId, info: info)
        }
        guard let outcome else {
            if ownsCurrentIndicator {
                dragState.session?.target = nil
            }
            return
        }
        dragState.session?.target = SidebarDropTarget(
            projectId: project.id,
            indicator: outcome.indicator
        )
    }

    private var ownsCurrentIndicator: Bool {
        dragState.session?.target?.projectId == project.id
    }

    private func resolve(draggedTabId: String, info: DropInfo) -> SidebarDropResolver.Outcome? {
        SidebarDropResolver.resolve(
            draggedTabId: draggedTabId,
            location: info.location,
            tabOrder: tabOrderProvider(),
            tabFrames: tabFramesProvider(),
            wouldMoveTab: tabs.wouldMoveTab
        )
    }
}

/// Pure, side-effect-free drop-slot picker for a tab drag. Takes a
/// snapshot of the target group's tab frames plus the dragged tab id
/// and the cursor location, and returns what the drop would do — or
/// `nil` for a no-op. Separate from `ProjectGroupDropDelegate` so
/// unit tests can exercise the slot-picking rules without a live
/// SwiftUI `DropInfo`.
enum SidebarDropResolver {
    struct Outcome: Equatable {
        let draggedId: String
        let targetId: String
        let placeAfter: Bool

        var indicator: DropIndicator {
            placeAfter ? .tabAfter(targetId) : .tabBefore(targetId)
        }
    }

    /// Resolve a tab drag hovering inside a project group.
    ///
    /// - Parameters:
    ///   - draggedTabId: id of the tab being dragged.
    ///   - location: cursor point in the project group's coordinate
    ///     space.
    ///   - tabOrder: ids of the target group's tabs in display order.
    ///   - tabFrames: per-tab frames in the group's coordinate space.
    ///   - wouldMoveTab: no-op predicate. Injected so the resolver
    ///     stays pure — callers pass `AppState.wouldMoveTab`.
    static func resolve(
        draggedTabId: String,
        location: CGPoint,
        tabOrder: [String],
        tabFrames: [String: CGRect],
        wouldMoveTab: (String, String, Bool) -> Bool
    ) -> Outcome? {
        guard let (targetId, placeAfter) = tabTarget(
            y: location.y,
            tabOrder: tabOrder,
            tabFrames: tabFrames
        ) else { return nil }
        guard wouldMoveTab(draggedTabId, targetId, placeAfter) else { return nil }
        return Outcome(draggedId: draggedTabId, targetId: targetId, placeAfter: placeAfter)
    }

    /// Pick the tab slot a cursor y-coordinate points at within a
    /// project group: above the first tab → before it; below the
    /// last tab → after it; over a tab → midpoint split.
    static func tabTarget(
        y: CGFloat,
        tabOrder: [String],
        tabFrames: [String: CGRect]
    ) -> (targetId: String, placeAfter: Bool)? {
        guard !tabOrder.isEmpty else { return nil }
        if let firstId = tabOrder.first, let firstFrame = tabFrames[firstId], y < firstFrame.minY {
            return (firstId, false)
        }
        if let lastId = tabOrder.last, let lastFrame = tabFrames[lastId], y > lastFrame.maxY {
            return (lastId, true)
        }
        for id in tabOrder {
            guard let frame = tabFrames[id] else { continue }
            if y >= frame.minY, y <= frame.maxY {
                return (id, y > frame.midY)
            }
        }
        return nil
    }
}

// MARK: - Footer controls

private struct SidebarIconButton: View {
    @Environment(FontSettings.self) private var fontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let systemImage: String
    let help: String
    var accessibilityId: String? = nil
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: fontSettings.sidebarSize(14), weight: .regular))
            .foregroundStyle(Color.niceInk2(scheme, palette))
            .frame(width: 24, height: 24)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hover ? Color.niceInk(scheme, palette).opacity(0.08) : Color.clear)
            )
            .contentShape(Rectangle())
            .onHover { hover = $0 }
            .onTapGesture { action() }
            .help(help)
            .accessibilityIdentifier(accessibilityId ?? "")
    }
}

#Preview("Sidebar") {
    let appState = AppState()
    return SidebarView()
        .environment(appState)
        .environment(appState.tabs)
        .environment(appState.sessions)
        .environment(appState.sidebar)
        .environment(appState.closer)
        .environment(appState.windowSession)
        .environment(Tweaks())
        .environment(FontSettings())
        .environment(FileBrowserSortSettings())
        .frame(width: 240, height: 680)
}

// MARK: - Window frame reporter

/// Transparent `NSView` that reports its window-local frame and the
/// enclosing window's `windowNumber` back to SwiftUI. Used by the tab
/// rename field to know where to check for click-outside events —
/// `NSEvent.locationInWindow` is in window coordinates, and
/// `NSWindow.windowNumber` identifies the window across the process.
struct WindowFrameReporter: NSViewRepresentable {
    var onReport: (NSRect, Int) -> Void

    func makeNSView(context: Context) -> Reporter {
        let view = Reporter()
        view.onReport = onReport
        return view
    }

    func updateNSView(_ nsView: Reporter, context: Context) {
        nsView.onReport = onReport
        nsView.report()
    }

    final class Reporter: NSView {
        var onReport: ((NSRect, Int) -> Void)?

        override func layout() {
            super.layout()
            report()
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            report()
        }

        func report() {
            guard let window else { return }
            let frame = convert(bounds, to: nil)
            onReport?(frame, window.windowNumber)
        }
    }
}
