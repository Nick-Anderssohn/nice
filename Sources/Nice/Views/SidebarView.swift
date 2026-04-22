//
//  SidebarView.swift
//  Nice
//
//  The expanded 240pt sidebar column. The collapsed state is handled
//  upstream in `AppShellView` as a small top-bar cap, so this view
//  is only instantiated when `appState.sidebarCollapsed == false`.
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
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        expandedSidebar
    }

    // MARK: - Expanded sidebar

    private var expandedSidebar: some View {
        VStack(spacing: 0) {
            tabList
            footer
        }
    }

    // MARK: - Tab list

    private var tabList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(appState.projects) { project in
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
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let project: Project
    @State private var isOpen: Bool = true
    @State private var headerHover: Bool = false
    /// Where the drop-indicator line is currently painted for this
    /// project group. `nil` means no indicator. The drop delegate on
    /// the group's VStack sets this live as the cursor moves, and the
    /// overlay at the VStack level draws the single accent line at
    /// the computed y-position.
    @State private var dropIndicator: DropIndicator?
    /// Frames of each TabRow in the group's local coordinate space,
    /// collected via preference key. The drop delegate uses these to
    /// pick the tab whose slot the cursor is in and to position the
    /// insertion line on the row's top or bottom edge.
    @State private var tabFrames: [String: CGRect] = [:]
    /// Total height of the project group in its own coordinate space.
    /// Used for project-drop before/after midpoint split and for
    /// painting the "after this project" line at the group's bottom.
    @State private var groupHeight: CGFloat = 0

    private var isTerminalsGroup: Bool {
        project.id == AppState.terminalsProjectId
    }

    /// Always show the `+` for the Terminals group (so the user can
    /// re-add after emptying it) but only reveal it on hover for
    /// ordinary project groups — keeps the sidebar clean by default.
    private var showAddButton: Bool {
        isTerminalsGroup || headerHover
    }

    private var coordSpace: String { "sidebar.group.\(project.id)" }

    /// Y-position (in the group's coordinate space) at which the
    /// insertion line should be painted, if any.
    private var indicatorY: CGFloat? {
        guard let indicator = dropIndicator else { return nil }
        switch indicator {
        case let .tabBefore(id):  return tabFrames[id]?.minY
        case let .tabAfter(id):   return tabFrames[id]?.maxY
        case .projectBefore:      return 0
        case .projectAfter:       return groupHeight
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
        .background(
            GeometryReader { geo in
                Color.clear.preference(
                    key: GroupHeightKey.self,
                    value: geo.size.height
                )
            }
        )
        .overlay(alignment: .top) {
            if let y = indicatorY {
                insertionLine.offset(y: y - 1)
            }
        }
        .onPreferenceChange(TabFramesKey.self) { frames in
            Task { @MainActor in tabFrames = frames }
        }
        .onPreferenceChange(GroupHeightKey.self) { h in
            Task { @MainActor in groupHeight = h }
        }
        .onDrop(
            of: [.text],
            delegate: ProjectGroupDropDelegate(
                project: project,
                isTerminalsGroup: isTerminalsGroup,
                tabFramesProvider: { tabFrames },
                groupHeightProvider: { groupHeight },
                tabOrderProvider: { project.tabs.map(\.id) },
                appState: appState,
                indicator: $dropIndicator
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
                .font(.system(size: fontSettings.sidebarSize(11), weight: .semibold))
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
                    _ = appState.createTerminalTab()
                } else {
                    _ = appState.createClaudeTabInProject(projectId: project.id)
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
        .modifier(ProjectHeaderDragModifier(
            project: project,
            isTerminalsGroup: isTerminalsGroup,
            appState: appState
        ))
        .contextMenu {
            if !isTerminalsGroup {
                Button("Close Project") {
                    appState.requestCloseProject(projectId: project.id)
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

/// Attaches `.onDrag` to a project header so the project can be
/// reordered by dragging its header. Terminals skips the modifier
/// entirely since that project is pinned. The drop handling lives at
/// the project group VStack, not here — this modifier is now only
/// the drag source.
private struct ProjectHeaderDragModifier: ViewModifier {
    let project: Project
    let isTerminalsGroup: Bool
    let appState: AppState

    func body(content: Content) -> some View {
        if isTerminalsGroup {
            content
        } else {
            content.onDrag {
                let encoded = SidebarDragPayload.project(project.id).encoded
                appState.draggingSidebarPayload = encoded
                return NSItemProvider(object: encoded as NSString)
            }
        }
    }
}

private struct CountPill: View {
    @EnvironmentObject private var fontSettings: FontSettings
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
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var fontSettings: FontSettings
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

    private var isActive: Bool { tab.id == appState.activeTabId }

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
        .system(size: fontSettings.sidebarSize(12), weight: isActive ? .semibold : .regular)
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
        appState.renameTab(id: tab.id, to: draftTitle)
        appState.focusActiveTerminal()
    }

    private func cancelEdit() {
        guard isEditing else { return }
        isEditing = false
        titleFocused = false
        removeMouseMonitor()
        appState.focusActiveTerminal()
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
        HStack(spacing: 8) {
            if tab.hasClaude {
                StatusDot(
                    status: tab.status,
                    suppressWaitingPulse: tab.waitingAcknowledged
                )
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).claudeIcon")
            } else {
                Image(systemName: "terminal")
                    .font(.system(size: fontSettings.sidebarSize(10), weight: .regular))
                    .foregroundStyle(Color.niceInk3(scheme, palette))
                    .frame(width: 12, height: 12)
                    .accessibilityElement()
                    .accessibilityIdentifier("sidebar.tab.\(tab.id).terminalIcon")
            }
            titleView
        }
        .padding(.leading, 22)
        .padding(.trailing, 10)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(backgroundColor)
        )
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture {
            if !isEditing {
                appState.selectTab(tab.id)
            }
        }
        .contextMenu {
            Button("Rename Tab") {
                appState.selectTab(tab.id)
                beginEditing()
            }
            .accessibilityIdentifier("sidebar.tab.\(tab.id).renameTab")
            Button("Close Tab") {
                appState.requestCloseTab(tabId: tab.id)
            }
            .accessibilityIdentifier("sidebar.tab.\(tab.id).closeTab")
        }
        .onDrag {
            let encoded = SidebarDragPayload.tab(tab.id).encoded
            appState.draggingSidebarPayload = encoded
            return NSItemProvider(object: encoded as NSString)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(accessibilityIdentifier)
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
        if tab.id == AppState.mainTerminalTabId {
            return "sidebar.terminals"
        }
        return "sidebar.tab.\(tab.id)"
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
                        appState.selectTab(tab.id)
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

/// Total height of a project group's VStack in its own coordinate
/// space. Used for project-drop midpoint and for painting the
/// "after this project" line at the group's bottom edge.
private struct GroupHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

/// Sidebar drag payload discriminator. Tab rows and project headers
/// both present themselves as drop destinations, and we need each to
/// reject the wrong kind of drop (a project header shouldn't consume
/// a tab drag, and a tab row shouldn't consume a project drag). The
/// `Transferable` payload is a single `String` so SwiftUI's
/// auto-generated drag preview works; the prefix namespaces the id.
private enum SidebarDragPayload {
    case tab(String)
    case project(String)

    var encoded: String {
        switch self {
        case let .tab(id):     return "tab:\(id)"
        case let .project(id): return "project:\(id)"
        }
    }

    init(encoded: String) {
        if let id = encoded.dropPrefix("tab:") {
            self = .tab(id)
        } else if let id = encoded.dropPrefix("project:") {
            self = .project(id)
        } else {
            // Unknown payload — treat as a tab with a no-match id so
            // the drop destinations naturally reject it.
            self = .tab("")
        }
    }
}

private extension String {
    func dropPrefix(_ prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        return String(dropFirst(prefix.count))
    }
}

// MARK: - Drop indicator

/// Where a project group's insertion-line indicator is currently
/// painted. Tab cases carry the target tab id so the overlay can
/// look up its frame; project cases resolve to the group's top and
/// bottom edges without needing a target id. `nil` (absent from the
/// group's state) means no indicator.
private enum DropIndicator: Equatable {
    case tabBefore(String)
    case tabAfter(String)
    case projectBefore
    case projectAfter
}

/// Unified drop delegate for a project group. Attached to the group's
/// VStack so the drop region covers the header, every tab row, and
/// the 4pt trailing padding — which means a tab can be dropped
/// "above the first tab" (cursor in the header area) or "below the
/// last tab" (cursor in the trailing gap), and a project can be
/// dropped anywhere in another group to land before or after it.
///
/// Tab and project payloads are routed internally by the payload
/// prefix. Tab drops consult the per-tab frames to pick a target and
/// before/after split; project drops use the group midpoint. All
/// drops defer to `wouldMoveTab` / `wouldMoveProject` so the
/// indicator is hidden (and the drop rejected) whenever the move
/// would be a no-op — same id, adjacent slot, cross-project tab,
/// Terminals as source or target.
private struct ProjectGroupDropDelegate: DropDelegate {
    let project: Project
    let isTerminalsGroup: Bool
    let tabFramesProvider: () -> [String: CGRect]
    let groupHeightProvider: () -> CGFloat
    let tabOrderProvider: () -> [String]
    let appState: AppState
    @Binding var indicator: DropIndicator?

    func validateDrop(info: DropInfo) -> Bool {
        info.hasItemsConforming(to: [.text])
    }

    func dropEntered(info: DropInfo) {
        updateIndicator(for: info)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        updateIndicator(for: info)
        return DropProposal(operation: indicator == nil ? .forbidden : .move)
    }

    func dropExited(info: DropInfo) {
        indicator = nil
    }

    func performDrop(info: DropInfo) -> Bool {
        defer {
            indicator = nil
            appState.draggingSidebarPayload = nil
        }
        guard let payloadString = appState.draggingSidebarPayload else { return false }
        switch SidebarDragPayload(encoded: payloadString) {
        case let .tab(draggedId):
            guard let (targetId, placeAfter) = tabTarget(at: info.location) else { return false }
            guard appState.wouldMoveTab(draggedId, relativeTo: targetId, placeAfter: placeAfter) else {
                return false
            }
            appState.moveTab(draggedId, relativeTo: targetId, placeAfter: placeAfter)
            return true
        case let .project(draggedId):
            guard !isTerminalsGroup else { return false }
            let placeAfter = info.location.y > groupHeightProvider() / 2
            guard appState.wouldMoveProject(draggedId, relativeTo: project.id, placeAfter: placeAfter) else {
                return false
            }
            appState.moveProject(draggedId, relativeTo: project.id, placeAfter: placeAfter)
            return true
        }
    }

    private func updateIndicator(for info: DropInfo) {
        guard let payloadString = appState.draggingSidebarPayload else {
            indicator = nil
            return
        }
        switch SidebarDragPayload(encoded: payloadString) {
        case let .tab(draggedId):
            guard let (targetId, placeAfter) = tabTarget(at: info.location),
                  appState.wouldMoveTab(draggedId, relativeTo: targetId, placeAfter: placeAfter)
            else {
                indicator = nil
                return
            }
            indicator = placeAfter ? .tabAfter(targetId) : .tabBefore(targetId)
        case let .project(draggedId):
            guard !isTerminalsGroup else {
                indicator = nil
                return
            }
            let placeAfter = info.location.y > groupHeightProvider() / 2
            guard appState.wouldMoveProject(draggedId, relativeTo: project.id, placeAfter: placeAfter) else {
                indicator = nil
                return
            }
            indicator = placeAfter ? .projectAfter : .projectBefore
        }
    }

    /// Pick which tab slot the cursor is currently in. Cursor above
    /// the first tab (header area) → before the first tab. Cursor
    /// below the last tab (trailing gap) → after the last tab.
    /// Otherwise, the tab whose frame contains the cursor, split on
    /// its vertical midpoint. Returns nil for empty projects.
    private func tabTarget(at location: CGPoint) -> (tabId: String, placeAfter: Bool)? {
        let ids = tabOrderProvider()
        guard !ids.isEmpty else { return nil }
        let frames = tabFramesProvider()
        let y = location.y
        if let firstId = ids.first, let firstFrame = frames[firstId], y < firstFrame.minY {
            return (firstId, false)
        }
        if let lastId = ids.last, let lastFrame = frames[lastId], y > lastFrame.maxY {
            return (lastId, true)
        }
        for id in ids {
            guard let frame = frames[id] else { continue }
            if y >= frame.minY, y <= frame.maxY {
                return (id, y > frame.midY)
            }
        }
        return nil
    }
}

// MARK: - Footer controls

private struct SidebarIconButton: View {
    @EnvironmentObject private var fontSettings: FontSettings
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
    SidebarView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .environmentObject(FontSettings())
        .frame(width: 240, height: 680)
}

// MARK: - Window frame reporter

/// Transparent `NSView` that reports its window-local frame and the
/// enclosing window's `windowNumber` back to SwiftUI. Used by the tab
/// rename field to know where to check for click-outside events —
/// `NSEvent.locationInWindow` is in window coordinates, and
/// `NSWindow.windowNumber` identifies the window across the process.
private struct WindowFrameReporter: NSViewRepresentable {
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
