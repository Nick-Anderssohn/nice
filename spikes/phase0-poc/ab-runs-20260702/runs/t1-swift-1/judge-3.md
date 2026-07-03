# t1-swift-1 — judge 3 (claude-fable-5, blind, packet-s1)
edit_locality: 5 — expected files only; only out-of-feature touches (tear-off hooks + drift banner padding) required by the new bottom band.
api_hallucination: 5 — count 0; no build/run failure citing nonexistent symbol; all diff APIs real (NSHostingView, mouseDownCanMoveWindow, hitTest, TimelineView(.everyMinute), NSPasteboard, AppleActionOnDoubleClick); seq 57 grep proactive.
iterations_to_green: 5 — count 1 build cycle; code final seq 58; first unit-test run green (1326/0); UITest attempts environmental, not iteration.
human_fixup_minutes: 5 — <5 min essentially zero; verifier 10/10 incl. multi-size checks; cosmetic nits only (1.5s coded vs ~3s observed Copied timer; frame/fixedSize ordering subtle but verified).
style_conformance: 5 — contract-style headers matching ChromeEventRouter; palette helpers; marker protocol modeled on PaneDragHosting; shared-constant gating like topBarHeight; MARKs, #Preview, a11y ids; XCTest style matches neighbors.
composite: 5.0
