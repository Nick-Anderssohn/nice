# t1-swift-1 implementer final report (claude-opus-4-8, 2026-07-02, 127 tool uses, ~51 min, ~243k tokens)
# NOTE: self-report — NOT a judge input; objective gate comes from the independent verifier.

## Files changed
- Sources/Nice/Views/WindowStatusBarView.swift (NEW) — 28pt full-width bar: CwdWidget (folder glyph + tilde-abbreviated path, click-to-copy with transient accent "✓ Copied" chip), ClockWidget (TimelineView(.everyMinute), POSIX HH:mm, monospaced digits), StatusBarWidget/ChromeWidgetHostingView (AppKit host claiming bounds in hitTest, mouseDownCanMoveWindow == false, conforms to new ChromeWidgetHosting marker), StatusBarText pure helpers.
- Sources/Nice/Views/WindowChrome.swift — added bottomBarHeight: CGFloat = 28 (single source of truth for bar height + router band).
- Sources/Nice/Views/Chrome/ChromeEventRouter.swift — added HitKind.widget (pass-through precedence over .strip, same as pills), ChromeWidgetHosting classification in hit-chain walk, bottom band (locationInWindow.y <= bottomBarHeight) alongside existing 52pt top band. Empty-bar drag/double-click reuse the exact performDrag / DoubleClickTitleBarAction machinery of the title bar.
- Sources/Nice/Views/AppShellView.swift — VStack(spacing:0){ shell; WindowStatusBarView() } for expanded+collapsed layouts; UITest tear-off hooks + drift banner lifted 28pt clear.
- Tests/NiceUnitTests/ChromeEventRouterTests.swift — 4 new decision-table cases (widget single/double click, widget-over-strip precedence).
- Tests/NiceUnitTests/WindowStatusBarTests.swift (NEW) — widget-host contract, clock format, home-abbreviation edge cases.

## Self-verification (installed dev build, synthetic HID events + screenshots)
- Bar: full width, 28pt, niceChrome bg, 1pt niceLine top border, toolbar-matched padding/typography.
- Copy: click replaced pasteboard sentinel with displayed path; "✓ Copied" chip auto-cleared. Cwd is LIVE: showed ~, then /tmp after cd /tmp (OSC 7 → pane.cwd).
- Drag-to-move: empty-bar drag moved window (141,242)→(214,187), size unchanged.
- Double-click: empty-bar zoomed (900×450→full visible) + second restored; runs DoubleClickTitleBarAction.current reading AppleActionOnDoubleClick fresh (unset → zoom).
- Widgets never move window: press-drag on cwd + clock → frame byte-identical; clock double-click → no zoom. Enforced structurally (router classifies ChromeWidgetHosting as pass-through).
- Clock: 16:09 → 16:15 → 16:28 across session.
- No regressions (live): top-bar empty drag moves window; top-bar double-click zooms/restores; sidebar top-strip drag moves; pill click passes through without moving window; pill drag-reorder still commits (window stationary); terminal input works; relocated tear-off hook still migrates a pane.

## Tests
- scripts/test.sh -only-testing:NiceUnitTests: 1326 tests, 0 failures (incl. new tests).
- scripts/install.sh: BUILD SUCCEEDED, zero errors.
- UITests COULD NOT RUN: NiceUITests-Runner fails at init "Timed out while enabling automation mode" — macOS Touch ID/password prompt ("XCTest is trying to Enable UI Automation"; Developer Mode disabled per DevToolsSecurity -status). 3 attempts, 0 tests executed, pre-launch, unrelated to the change. Compensated by manually reproducing every behavior WindowDragUITests / TearOffHookUITests / PaneReorderUITests assert. To run later: approve prompt or sudo DevToolsSecurity -enable.

## Caveats
- Full screen: bar's empty pixels don't drag/zoom — parity with existing top-bar (router full-screen pass-through applies to both bands).
- Cwd widget copies displayed (tilde-abbreviated) text per "copies its text" wording.
- Verification churned Nice Dev dev-side persisted session state; prod untouched. gpui-term (other session) contended for display mid-session; affected checks re-run with Nice Dev verified frontmost.
- Changes left uncommitted; lock released; Nice Dev quit; project.yml restored by test.sh trap.
