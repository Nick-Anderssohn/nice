//
//  WindowStatusBarView.swift
//  Nice
//
//  Full-width bottom status bar — the bottom counterpart of
//  `WindowToolbarView`'s top chrome band. Left: the active session's
//  working directory (click to copy, with a transient "Copied"
//  confirmation). Right: a HH:MM clock that ticks once a minute.
//
//  Interaction contract (mirrors the top bar):
//    • empty (non-widget) pixels behave like a native title bar — drag
//      moves the window, double-click runs the user's
//      `AppleActionOnDoubleClick`. Both are owned by `ChromeEventRouter`,
//      whose bottom band is gated on `WindowChrome.bottomBarHeight`; the
//      `WindowDragRegion` in the background vends the `ChromeDragStripView`
//      marker exactly like the toolbar does.
//    • a press on a WIDGET must never move the window. Each widget is
//      wrapped in `StatusBarWidget`, whose `ChromeWidgetHostingView`
//      claims its whole bounds in `hitTest(_:)` and conforms to the
//      `ChromeWidgetHosting` marker — the router classifies the press
//      `.widget` (precedence over `.strip`, same as pills) and passes it
//      through, so the widget's own SwiftUI gestures run and nothing else.
//      This is the same structural veto `PaneDragSource` uses for pills;
//      there is no flag to stick.
//

import AppKit
import SwiftUI

// MARK: - Bar

struct WindowStatusBarView: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @Environment(TabModel.self) private var tabs

    var body: some View {
        HStack(spacing: 10) {
            // Leading: working directory of the active session. Sized to
            // its content (`.fixedSize`) so the widget's press-claiming
            // host never covers empty bar pixels; capped so a deep path
            // can't crowd out the clock.
            StatusBarWidget {
                CwdWidget(path: displayCwd, scheme: scheme, palette: palette)
            }
            .frame(maxWidth: 460, maxHeight: WindowChrome.bottomBarHeight)
            .fixedSize()

            // Everything between the widgets is empty chrome — the router
            // resolves presses here to the drag strip below.
            Spacer(minLength: 10)

            // Trailing: minute clock.
            StatusBarWidget {
                ClockWidget(scheme: scheme, palette: palette)
            }
            .frame(maxHeight: WindowChrome.bottomBarHeight)
            .fixedSize()
        }
        .padding(.leading, 14)
        .padding(.trailing, 20)
        .frame(height: WindowChrome.bottomBarHeight)
        .frame(maxWidth: .infinity)
        .background {
            ZStack {
                Color.niceChrome(scheme, palette)
                // Same marker arrangement as the toolbar: the frontmost
                // view of the chrome background is the drag-strip marker,
                // so `ChromeEventRouter`'s per-press hit-test resolves
                // empty-bar presses to it (or to a transparent SwiftUI
                // wrapper caught by the router's attribute-walk fallback)
                // and owns drag-to-move + the double-click action.
                WindowDragRegion()
            }
        }
        .overlay(alignment: .top) {
            // 1pt top border — mirrors the toolbar's bottom border so the
            // two bands read as the same chrome system.
            Rectangle()
                .fill(Color.niceLine(scheme, palette))
                .frame(height: 1)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("statusBar")
    }

    /// The active session's working directory, tilde-abbreviated for
    /// display. Prefers the active pane's live OSC 7 cwd, falls back to
    /// the tab's cwd, then to the user's home when no tab is active
    /// (transient teardown states).
    private var displayCwd: String {
        StatusBarText.abbreviateHome(activeCwd)
    }

    private var activeCwd: String {
        if let tabId = tabs.activeTabId, let tab = tabs.tab(for: tabId) {
            if let paneId = tab.activePaneId,
               let pane = tab.panes.first(where: { $0.id == paneId }),
               let paneCwd = pane.cwd, !paneCwd.isEmpty {
                return paneCwd
            }
            return tab.cwd
        }
        return NSHomeDirectory()
    }
}

// MARK: - Cwd widget

/// Left status-bar widget: folder glyph + working-directory path. A click
/// copies the displayed text to the clipboard and flashes an accent
/// "Copied" chip for a moment. Receives its palette inputs as plain
/// values (not `@Environment`) because it renders inside
/// `StatusBarWidget`'s own hosting view — the outer bar re-renders on any
/// theme / cwd change and `updateNSView` refreshes this content.
private struct CwdWidget: View {
    let path: String
    let scheme: ColorScheme
    let palette: Palette

    @State private var hovering = false
    @State private var showCopied = false
    /// Monotonic click generation so a rapid re-click extends the
    /// confirmation instead of an older timer hiding the newer flash.
    @State private var copyGeneration = 0

    var body: some View {
        Button(action: copy) {
            HStack(spacing: 5) {
                Image(systemName: "folder")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Color.niceInk3(scheme, palette))
                Text(path)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Color.niceInk2(scheme, palette))
                    .lineLimit(1)
                    .truncationMode(.middle)
                if showCopied {
                    HStack(spacing: 3) {
                        Image(systemName: "checkmark")
                            .font(.system(size: 9, weight: .semibold))
                        Text("Copied")
                            .font(.system(size: 11, weight: .medium))
                    }
                    .foregroundStyle(Color.niceAccent)
                    .transition(.opacity)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 20)
            .background(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(
                        hovering
                            ? Color.niceInk(scheme, palette).opacity(0.08)
                            : .clear
                    )
            )
            .contentShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .animation(.easeInOut(duration: 0.12), value: showCopied)
        .help("Copy working directory")
        .accessibilityIdentifier("statusBar.cwd")
        .accessibilityLabel("Working directory: \(path)")
    }

    private func copy() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(path, forType: .string)
        copyGeneration += 1
        let generation = copyGeneration
        showCopied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            guard generation == copyGeneration else { return }
            showCopied = false
        }
    }
}

// MARK: - Clock widget

/// Right status-bar widget: HH:MM wall clock. `TimelineView(.everyMinute)`
/// re-renders exactly at each minute boundary — no timer to own or
/// invalidate. Monospaced digits so the bar doesn't shimmy when a narrow
/// digit ticks over to a wide one.
private struct ClockWidget: View {
    let scheme: ColorScheme
    let palette: Palette

    var body: some View {
        TimelineView(.everyMinute) { context in
            Text(StatusBarText.clock(context.date))
                .font(.system(size: 11, weight: .medium).monospacedDigit())
                .foregroundStyle(Color.niceInk2(scheme, palette))
        }
        .padding(.horizontal, 2)
        .accessibilityIdentifier("statusBar.clock")
    }
}

// MARK: - Pure text helpers (unit-tested)

/// Main-actor isolated so the cached `DateFormatter` (non-Sendable) is
/// legal under strict concurrency; every caller — the bar's body and the
/// unit tests — is already on the main actor.
@MainActor
enum StatusBarText {
    /// 24-hour "HH:mm" — the status-bar clock format. POSIX locale so a
    /// user 12-hour preference can't reshape it (the spec is literal
    /// HH:MM); the current time zone so the wall clock reads local.
    static func clock(_ date: Date, timeZone: TimeZone = .current) -> String {
        let formatter = clockFormatter
        formatter.timeZone = timeZone
        return formatter.string(from: date)
    }

    private static let clockFormatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "HH:mm"
        return f
    }()

    /// Home-relative display form of a path: "/Users/x" → "~",
    /// "/Users/x/dir" → "~/dir". Only a whole-component prefix counts —
    /// "/Users/xylophone" stays untouched for home "/Users/x".
    static func abbreviateHome(
        _ path: String,
        home: String = NSHomeDirectory()
    ) -> String {
        guard !home.isEmpty, home != "/" else { return path }
        if path == home { return "~" }
        if path.hasPrefix(home + "/") {
            return "~" + path.dropFirst(home.count)
        }
        return path
    }
}

// MARK: - Widget hosting (the never-moves-the-window veto)

/// Marker protocol the chrome event router uses to classify a status-bar
/// widget press. The router walks a press's ancestor chain and treats any
/// `ChromeWidgetHosting` view as "a widget owns this press" — pass the
/// event through, never arm a window drag, never run the double-click
/// action. The status-bar analog of `PaneDragHosting` (pills).
protocol ChromeWidgetHosting: AnyObject {}

/// Hosts one widget's SwiftUI content inside an AppKit view the router
/// can recognise. Environment (observably including `TabModel` for the
/// pills' equivalent) bridges across the hosting boundary on this SDK,
/// but the widgets take their inputs as plain values anyway so nothing
/// here depends on it.
private struct StatusBarWidget<Content: View>: NSViewRepresentable {
    @ViewBuilder let content: () -> Content

    func makeNSView(context: Context) -> NSView {
        let hosting = ChromeWidgetHostingView(rootView: AnyView(content()))
        hosting.translatesAutoresizingMaskIntoConstraints = false
        return hosting
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        (nsView as? ChromeWidgetHostingView)?.rootView = AnyView(content())
    }
}

/// `NSHostingView` that claims its whole bounds in `hitTest(_:)` so a
/// press anywhere on a widget resolves to a `ChromeWidgetHosting` view —
/// mirrors `PaneDragSource.PaneDragHostingView`, minus the drag
/// machinery. Not overriding `mouseDown` means AppKit dispatches the
/// press to this hosting view and SwiftUI's gesture router runs the
/// widget's own Button / tap handling normally. `mouseDownCanMoveWindow`
/// is `false` so the router's attribute-walk fallback can't classify the
/// widget as empty chrome.
final class ChromeWidgetHostingView: NSHostingView<AnyView>, ChromeWidgetHosting {
    override var mouseDownCanMoveWindow: Bool { false }

    override func hitTest(_ point: NSPoint) -> NSView? {
        let local = convert(point, from: superview)
        return NSPointInRect(local, bounds) ? self : nil
    }
}

// MARK: - Previews

#Preview("Status bar — light") {
    let appState = AppState()
    return WindowStatusBarView()
        .environment(appState.tabs)
        .frame(width: 900)
        .preferredColorScheme(.light)
}

#Preview("Status bar — dark") {
    let appState = AppState()
    return WindowStatusBarView()
        .environment(appState.tabs)
        .frame(width: 900)
        .preferredColorScheme(.dark)
}
