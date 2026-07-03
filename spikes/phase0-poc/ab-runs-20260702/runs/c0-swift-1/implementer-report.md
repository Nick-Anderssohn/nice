# c0-swift-1 implementer final report (claude-opus-4-8, 2026-07-02, 78 turns/88 uses, ~22.7 min, ~268k tokens)
# NOTE: self-report — NOT a judge input (C0 unjudged anyway); objective gate from independent verifier.

## Files changed
- NEW Sources/Nice/State/ThroughputMeter.swift — @MainActor @Observable rolling-window byte-rate meter (~1s ring of 0.1s buckets); record() on pty hot path with zero observed mutation; refresh() publishes bytesPerSecond/isActive (equality-gated), 2s idle threshold; injectable clock; static label(forBytesPerSecond:).
- Sources/Nice/Process/NiceTerminalView.swift — onData: (Int)->Void hook from dataReceived(slice:) on every non-empty chunk (guard restructured; one-shot onFirstData preserved).
- Sources/Nice/Process/TabPtySession.swift — optional onPaneOutput callback; installOutputMeterHook(on:) wired at both spawn sites + re-pointed in adoptPane (migrated pane's throughput follows to destination window's badge).
- Sources/Nice/State/SessionsModel.swift — onPaneOutput forwarded from TabPtySession.
- Sources/Nice/State/AppState.swift — owns throughput = ThroughputMeter(); wires sessions.onPaneOutput → record.
- Sources/Nice/State/Tweaks.swift — persisted activityBadgeCompact Bool (UserDefaults write-through, default full).
- Sources/Nice/Views/WindowToolbarView.swift — ActivityBadge adjacent to trailing top-bar content (before UpdateAvailablePill); accent tint active / niceInk3 idle; 28pt, radius 7, hover tint matching pills; 4Hz Timer.publish drives refresh(); click toggles; import Combine.
- Tests: ThroughputMeterTests.swift (9 cases), TweaksAdvancedTogglesTests (+2 persistence).

## Self-verification
- scripts/test.sh built app+tests zero errors; ThroughputMeter+Tweaks 16/16; regression suites 61/61 (TabPtySession, pane migration/adopt, cross-window move, AppState lifecycle).
- Live: `yes` in a Nice Dev terminal → badge tracked 4372→1809 KB/s in accent style (screenshot); fresh-launch idle dim state observed; click → compact (screenshot); relaunch → stayed compact (screenshot).
- Caveats: live active→idle dimming transition not screenshotted (couldn't force silence mid-session from sandboxed shell; transition unit-tested); badge aggregates all panes in window; compact/full is app-wide persisted preference; left persisted state=compact + one stray terminal pane in dev app.
