# t1-swift-2 implementer final report (claude-opus-4-8, 2026-07-02, 67 turns/73 uses, ~23.3 min, ~193k tokens)
# NOTE: self-report — NOT a judge input; objective gate from independent verifier.

## Files changed
- Sources/Nice/Views/BottomStatusBarView.swift (NEW) — bar (niceChrome bg + top niceLine hairline, statusBarHeight tall), cwd widget (folder icon + home-abbreviated path, click-to-copy + "Copied" flash), clock (TimelineView(.everyMinute), HH:mm 24h), ChromeWidgetGuard/ChromeWidgetHosting marker hosting widgets in NSHostingView with mouseDownCanMoveWindow == false. Widgets take scheme/palette explicitly (robust across nested-hosting boundary).
- Sources/Nice/Views/WindowChrome.swift — shared statusBarHeight = 28 used by both SwiftUI frame and router hit gate (same pattern as topBarHeight).
- Sources/Nice/Views/Chrome/ChromeEventRouter.swift — .widget HitKind classified from ChromeWidgetHosting in ancestor-chain walk; pass-through precedence over .strip in pure decision; hit gate extended with bottom band (y <= statusBarHeight) alongside unchanged top band.
- Sources/Nice/Views/AppShellView.swift — VStack wrapping expanded/collapsed shell with BottomStatusBarView pinned bottom; drift banner lifted by statusBarHeight.
- Tests — ChromeEventRouterTests (4 new .widget precedence cases), WindowChromeTests (statusBarHeight pin), WindowToolbarDragRegionTests (widget-host marker contract).

## Self-verification
- scripts/install.sh → BUILD SUCCEEDED, Nice Dev 0.30.0 installed.
- Unit: full NiceUnitTests — 1318 tests, 0 failures.
- Runtime (real synthesized mouse events): widget click → clipboard = ~/Projects/notes/.claude/worktrees/rosy-orbiting-fairy + "Copied" flash; empty-bar drag → origin moved Δ114pt; clock drag Δ0; cwd drag Δ0; empty double-click zoomed (1100×760 → display), honoring AppleActionOnDoubleClick (mapping unit-covered).
- Rendering: screenshot — full-width bar, matched chrome, live cwd left, clock 17:11 right.
- No regression: top-band router path byte-for-byte unchanged (bottom band is an added ||); pill/strip precedence cases pass; full unit suite green. Cwd is real Tab/Pane.cwd, not placeholder.

## Caveats
- WindowDragUITests XCUITest suite could not run ("Timed out while enabling automation mode" — TCC/automation env issue, same as t1-swift-1). Verified equivalent behaviors via CGEvent injection; routing table fully unit-tested. No new bottom-bar XCUITest added since suite can't initialize here.
- HH:mm fixed 24h per literal spec; double-click action reads live system pref each time.
- Prod Nice untouched; Nice Dev quit; lock released.
