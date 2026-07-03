//
//  BottomStatusBarView.swift
//  Nice
//
//  The window's bottom status bar — a full-width, `statusBarHeight`-tall
//  chrome band that mirrors the top toolbar's material (`niceChrome` +
//  a `niceLine` hairline) and hosts two widgets:
//
//    • LEFT  — the active session's working directory. Click it to copy
//      the path to the clipboard; a brief "Copied" flash confirms.
//    • RIGHT — a clock (HH:MM, ticking on the minute via `TimelineView`).
//
//  Like the top bar it is a native-title-bar-equivalent surface: empty
//  (non-widget) pixels drag the window and double-click runs the user's
//  `AppleActionOnDoubleClick`, all owned by `ChromeEventRouter`. The bar's
//  `.background` vends a `ChromeDragStripView` marker for that, and the
//  router's hit gate treats the bottom `statusBarHeight` band like the top
//  band (see `ChromeEventRouter.handleMouseDown`).
//
//  The widgets must NEVER move the window on press / drag / click. A
//  SwiftUI view alone can't guarantee that: an empty-status-bar press
//  hit-tests through SwiftUI's transparent hosting wrappers, which report
//  `mouseDownCanMoveWindow == true`, and the router's attribute-walk
//  fallback would classify a widget press as the draggable strip too. So
//  each widget is wrapped in a `ChromeWidgetGuard` — an `NSHostingView`
//  subclass that conforms to the `ChromeWidgetHosting` marker and reports
//  `mouseDownCanMoveWindow == false`. The router finds that marker in the
//  press's ancestor chain and, with `.widget` precedence over `.strip`,
//  passes the press through. Same model as the pane pills' `PaneDragSource`.
//

import AppKit
import SwiftUI

// MARK: - Widget guard

/// Marker protocol: `ChromeEventRouter` treats any `ChromeWidgetHosting`
/// view in a press's ancestor chain as a status-bar WIDGET — a press /
/// drag / click on it must never move the window (precedence over the
/// empty-chrome strip, the same role `PaneDragHosting` plays for pills).
protocol ChromeWidgetHosting: AnyObject {}

/// Hosts a status-bar widget's SwiftUI content in an `NSHostingView`
/// subclass that (1) conforms to `ChromeWidgetHosting` so the chrome event
/// router passes its presses through instead of arming a window drag, and
/// (2) reports `mouseDownCanMoveWindow == false` so the router's
/// attribute-walk fallback never reclassifies it as draggable chrome.
///
/// SwiftUI hit-testing INSIDE the host is left untouched (no `hitTest`
/// override), so the widget's own taps / hovers keep working normally; the
/// router still finds the host in the ancestor chain because it contains
/// the widget's backing views. Callers pin the natural size with
/// `.frame(...).fixedSize()`, exactly as `PaneDragSource` pills do.
struct ChromeWidgetGuard<Content: View>: NSViewRepresentable {
    @ViewBuilder let content: () -> Content

    func makeNSView(context: Context) -> NSView {
        let host = WidgetHostView(rootView: AnyView(content()))
        host.translatesAutoresizingMaskIntoConstraints = false
        return host
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        (nsView as? WidgetHostView)?.rootView = AnyView(content())
    }

    /// The marker host. No mouse-handling overrides — the default
    /// `NSHostingView` event path feeds SwiftUI, so the widget's tap
    /// gesture (copy) still fires; the only customisations are the marker
    /// conformance and the `mouseDownCanMoveWindow` opt-out.
    final class WidgetHostView: NSHostingView<AnyView>, ChromeWidgetHosting {
        override var mouseDownCanMoveWindow: Bool { false }
    }
}

// MARK: - Status bar

struct BottomStatusBarView: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(TabModel.self) private var tabs

    var body: some View {
        HStack(spacing: 8) {
            // Widgets take `scheme` / `palette` explicitly: their SwiftUI
            // content is re-hosted in a nested `NSHostingView` by
            // `ChromeWidgetGuard`, so passing the colors in is more robust
            // than relying on environment bridging across that boundary.
            ChromeWidgetGuard {
                StatusBarCwdWidget(path: activeCwd, scheme: scheme, palette: palette)
            }
            .frame(maxWidth: 420, maxHeight: WindowChrome.statusBarHeight)
            .fixedSize()

            Spacer(minLength: 8)

            ChromeWidgetGuard {
                StatusBarClockWidget(scheme: scheme, palette: palette)
            }
            .frame(maxHeight: WindowChrome.statusBarHeight)
            .fixedSize()
        }
        .padding(.horizontal, 12)
        .frame(height: WindowChrome.statusBarHeight)
        .frame(maxWidth: .infinity)
        .background {
            ZStack {
                Color.niceChrome(scheme, palette)
                // Frontmost view of the chrome background. Vends a
                // `ChromeDragStripView` marker that `ChromeEventRouter`
                // hit-tests per-press: empty-status-bar presses resolve to
                // it (drag to move, double-click to act), while the widgets
                // hit-test to their `ChromeWidgetGuard` hosts and are passed
                // through. Same pattern as `WindowToolbarView`.
                WindowDragRegion()
            }
        }
        .overlay(alignment: .top) {
            // Hairline that separates the status bar from the terminal body
            // above it — mirrors the toolbar's bottom separator so the two
            // chrome bands read as a matched pair.
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
        .accessibilityIdentifier("statusBar")
    }

    /// The working directory shown on the left. Prefers the active pane's
    /// live cwd (captured from OSC 7), falling back to the tab's cwd — the
    /// same precedence the rest of the app uses. `~` when nothing is
    /// resolvable, so the widget is never blank.
    private var activeCwd: String {
        guard let tabId = tabs.activeTabId, let tab = tabs.tab(for: tabId) else {
            return "~"
        }
        let paneCwd = tab.activePaneId.flatMap { paneId in
            tab.panes.first(where: { $0.id == paneId })?.cwd
        }
        let raw = paneCwd?.isEmpty == false ? paneCwd! : tab.cwd
        return raw.isEmpty ? "~" : raw
    }
}

// MARK: - CWD widget

/// Left widget: the working directory, abbreviated with `~` for the home
/// directory. Clicking copies the shown path to the clipboard and flashes
/// a "Copied" confirmation for a beat.
private struct StatusBarCwdWidget: View {
    let path: String
    let scheme: ColorScheme
    let palette: Palette
    @State private var copied = false

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: copied ? "checkmark" : "folder")
                .font(.system(size: 10, weight: .semibold))
            Text(copied ? "Copied" : displayPath)
                .font(.niceMonoSmall)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .foregroundStyle(copied
            ? Color.niceInk(scheme, palette)
            : Color.niceInk2(scheme, palette))
        .contentShape(Rectangle())
        .onTapGesture { copy() }
        .help("Copy working directory")
        .accessibilityIdentifier("statusBar.cwd")
    }

    /// Home-abbreviated form of `path` (and the exact string copied).
    private var displayPath: String {
        let home = NSHomeDirectory()
        if path == home { return "~" }
        if !home.isEmpty, path.hasPrefix(home + "/") {
            return "~" + path.dropFirst(home.count)
        }
        return path
    }

    private func copy() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(displayPath, forType: .string)
        withAnimation(.easeInOut(duration: 0.15)) { copied = true }
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            withAnimation(.easeInOut(duration: 0.2)) { copied = false }
        }
    }
}

// MARK: - Clock widget

/// Right widget: the current time as HH:MM. `TimelineView(.everyMinute)`
/// re-renders aligned to minute boundaries, so the displayed minute is
/// never stale and there is no free-running per-second timer.
private struct StatusBarClockWidget: View {
    let scheme: ColorScheme
    let palette: Palette

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "HH:mm"
        return f
    }()

    var body: some View {
        TimelineView(.everyMinute) { context in
            Text(Self.formatter.string(from: context.date))
                .font(.niceMonoSmall)
                .monospacedDigit()
                .foregroundStyle(Color.niceInk2(scheme, palette))
        }
        .help("Current time")
        .accessibilityIdentifier("statusBar.clock")
    }
}
