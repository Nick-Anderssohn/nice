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
    /// Which edge of the header the project-reorder insertion line
    /// is currently painted on, if any. `nil` means no indicator.
    @State private var dropSide: SidebarDropSide?
    /// Captured header height for the midpoint split on project drops
    /// (above midpoint → insert before, below → insert after). Seeded
    /// with a sensible default so the first drop before layout still
    /// behaves sanely.
    @State private var headerHeight: CGFloat = 24

    private var isTerminalsGroup: Bool {
        project.id == AppState.terminalsProjectId
    }

    /// Always show the `+` for the Terminals group (so the user can
    /// re-add after emptying it) but only reveal it on hover for
    /// ordinary project groups — keeps the sidebar clean by default.
    private var showAddButton: Bool {
        isTerminalsGroup || headerHover
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if isOpen {
                ForEach(project.tabs) { tab in
                    TabRow(tab: tab)
                }
            }
        }
        .padding(.bottom, 4)
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
        .overlay(alignment: .top) {
            if dropSide == .before { insertionLine }
        }
        .overlay(alignment: .bottom) {
            if dropSide == .after { insertionLine }
        }
        .background(
            GeometryReader { geo in
                Color.clear.preference(
                    key: ProjectHeaderHeightKey.self,
                    value: geo.size.height
                )
            }
        )
        .onPreferenceChange(ProjectHeaderHeightKey.self) { height in
            Task { @MainActor in headerHeight = height }
        }
        .onHover { headerHover = $0 }
        .modifier(ProjectHeaderDragModifier(
            project: project,
            isTerminalsGroup: isTerminalsGroup,
            headerHeightProvider: { headerHeight },
            appState: appState,
            dropSide: $dropSide
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

    /// 2pt accent-colored insertion line painted at the top or bottom
    /// edge of the project header. Matches the TabRow indicator so
    /// project and tab reorders feel identical.
    private var insertionLine: some View {
        Rectangle()
            .fill(tweaks.accent.color)
            .frame(height: 2)
            .padding(.horizontal, 6)
            .offset(y: dropSide == .after ? 1 : -1)
            .allowsHitTesting(false)
    }
}

/// Attaches `.onDrag` / `.onDrop` to a project header, skipping the
/// pinned Terminals group entirely. A ViewModifier keeps the
/// conditional attachment from churning view identity between the
/// Terminals and non-Terminals branches as `isTerminalsGroup` is
/// resolved once at construction.
private struct ProjectHeaderDragModifier: ViewModifier {
    let project: Project
    let isTerminalsGroup: Bool
    let headerHeightProvider: () -> CGFloat
    let appState: AppState
    @Binding var dropSide: SidebarDropSide?

    func body(content: Content) -> some View {
        if isTerminalsGroup {
            content
        } else {
            content
                .onDrag {
                    let encoded = SidebarDragPayload.project(project.id).encoded
                    appState.draggingSidebarPayload = encoded
                    return NSItemProvider(object: encoded as NSString)
                }
                .onDrop(
                    of: [.text],
                    delegate: ProjectHeaderDropDelegate(
                        targetProjectId: project.id,
                        headerHeightProvider: headerHeightProvider,
                        appState: appState,
                        dropSide: $dropSide
                    )
                )
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
    /// Which edge of this row the insertion indicator is currently
    /// painted on, if any. `nil` means no indicator. Updated live by
    /// `TabRowDropDelegate` as the cursor moves through this row.
    @State private var dropSide: SidebarDropSide?
    /// Row height captured for the drop-destination midpoint split
    /// (above midpoint → insert before target, below → insert after).
    /// Seeded with a reasonable default so the first drop before any
    /// layout tick still behaves sanely.
    @State private var rowHeight: CGFloat = 24

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

    /// 2pt accent-colored insertion line painted at the top or bottom
    /// of the row to preview where a dragged tab will land. Inset by
    /// 6pt horizontally so it lines up with the row's rounded
    /// background, and offset by 1pt past the edge so adjacent rows
    /// draw it on the same visual seam.
    private var insertionLine: some View {
        Rectangle()
            .fill(tweaks.accent.color)
            .frame(height: 2)
            .padding(.horizontal, 6)
            .offset(y: dropSide == .after ? 1 : -1)
            .allowsHitTesting(false)
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
        .overlay(alignment: .top) {
            if dropSide == .before { insertionLine }
        }
        .overlay(alignment: .bottom) {
            if dropSide == .after { insertionLine }
        }
        .background(
            GeometryReader { geo in
                Color.clear.preference(
                    key: TabRowHeightKey.self,
                    value: geo.size.height
                )
            }
        )
        .onPreferenceChange(TabRowHeightKey.self) { height in
            Task { @MainActor in rowHeight = height }
        }
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
        .onDrop(
            of: [.text],
            delegate: TabRowDropDelegate(
                targetTabId: tab.id,
                rowHeightProvider: { rowHeight },
                appState: appState,
                dropSide: $dropSide
            )
        )
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

private struct TabRowHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

private struct ProjectHeaderHeightKey: PreferenceKey {
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

/// Which edge of a drop-target row the insertion-line indicator
/// should paint on. `before` → above the row, `after` → below.
private enum SidebarDropSide {
    case before
    case after
}

/// Drop delegate for a sidebar tab row. Reads `AppState.draggingSidebarPayload`
/// synchronously on every `dropUpdated` so the insertion line can
/// reflect the drop destination live; the SwiftUI Transferable API
/// only exposes the payload once the drop commits. The line is
/// suppressed whenever the drop would be a no-op (same tab, different
/// project, already-adjacent slot), so the indicator only appears
/// when a real reorder is about to happen.
private struct TabRowDropDelegate: DropDelegate {
    let targetTabId: String
    let rowHeightProvider: () -> CGFloat
    let appState: AppState
    @Binding var dropSide: SidebarDropSide?

    func validateDrop(info: DropInfo) -> Bool {
        info.hasItemsConforming(to: [.text])
    }

    func dropEntered(info: DropInfo) {
        updateDropSide(for: info)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        updateDropSide(for: info)
        return DropProposal(operation: dropSide == nil ? .forbidden : .move)
    }

    func dropExited(info: DropInfo) {
        dropSide = nil
    }

    func performDrop(info: DropInfo) -> Bool {
        defer {
            dropSide = nil
            appState.draggingSidebarPayload = nil
        }
        guard let payload = appState.draggingSidebarPayload,
              case let .tab(draggedId) = SidebarDragPayload(encoded: payload)
        else { return false }
        let placeAfter = info.location.y > (rowHeightProvider() / 2)
        guard appState.wouldMoveTab(draggedId, relativeTo: targetTabId, placeAfter: placeAfter) else {
            return false
        }
        appState.moveTab(draggedId, relativeTo: targetTabId, placeAfter: placeAfter)
        return true
    }

    private func updateDropSide(for info: DropInfo) {
        guard let payload = appState.draggingSidebarPayload,
              case let .tab(draggedId) = SidebarDragPayload(encoded: payload)
        else {
            dropSide = nil
            return
        }
        let placeAfter = info.location.y > (rowHeightProvider() / 2)
        guard appState.wouldMoveTab(draggedId, relativeTo: targetTabId, placeAfter: placeAfter) else {
            dropSide = nil
            return
        }
        dropSide = placeAfter ? .after : .before
    }
}

/// Drop delegate for a sidebar project header. Mirrors `TabRowDropDelegate`
/// but routes to `moveProject` / `wouldMoveProject` so Terminals stays
/// pinned and only real project reorders paint the indicator.
private struct ProjectHeaderDropDelegate: DropDelegate {
    let targetProjectId: String
    let headerHeightProvider: () -> CGFloat
    let appState: AppState
    @Binding var dropSide: SidebarDropSide?

    func validateDrop(info: DropInfo) -> Bool {
        info.hasItemsConforming(to: [.text])
    }

    func dropEntered(info: DropInfo) {
        updateDropSide(for: info)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        updateDropSide(for: info)
        return DropProposal(operation: dropSide == nil ? .forbidden : .move)
    }

    func dropExited(info: DropInfo) {
        dropSide = nil
    }

    func performDrop(info: DropInfo) -> Bool {
        defer {
            dropSide = nil
            appState.draggingSidebarPayload = nil
        }
        guard let payload = appState.draggingSidebarPayload,
              case let .project(draggedId) = SidebarDragPayload(encoded: payload)
        else { return false }
        let placeAfter = info.location.y > (headerHeightProvider() / 2)
        guard appState.wouldMoveProject(draggedId, relativeTo: targetProjectId, placeAfter: placeAfter) else {
            return false
        }
        appState.moveProject(draggedId, relativeTo: targetProjectId, placeAfter: placeAfter)
        return true
    }

    private func updateDropSide(for info: DropInfo) {
        guard let payload = appState.draggingSidebarPayload,
              case let .project(draggedId) = SidebarDragPayload(encoded: payload)
        else {
            dropSide = nil
            return
        }
        let placeAfter = info.location.y > (headerHeightProvider() / 2)
        guard appState.wouldMoveProject(draggedId, relativeTo: targetProjectId, placeAfter: placeAfter) else {
            dropSide = nil
            return
        }
        dropSide = placeAfter ? .after : .before
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
