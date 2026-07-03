# t1-rust-1 objective gate — PASS (all items)

O1 Builds clean: PASS — cargo build --bins, zero errors, 0.23 s cached (build.log; pre-existing stub-bridge banner + block-crate future-incompat note only).
O2 Feature functional: PASS — verifier items 1–6 all PASS.
O3 No regressions + differential invariants: PASS — verifier items 7–9 all PASS; headless INTERACTIVE self-test PASS exit 0 (orchestrator-run); cargo build --bins clean.
O4 DNF: no — completed in ~28 min, 71 tool-use turns (cap 100/3 h).

## Independent verifier detail (claude session, real CGEvents, frames via CGWindowList; ~8.5 min)

1 PASS — Bar full width at bottom, dark chrome matching terminal, ~56 px @2x ≈ 28 pt; left path chip + right clock coherent (01-initial.png, 02-after-click.png).
2 PASS — Left widget shows actual cwd ".../ab-rust/spikes/phase0-poc"; clock 16:19@16:19 then 16:20/16:23/16:25/16:26 across boundaries, matching wall clock.
3 PASS — (a) pbpaste = exact cwd (sentinel replaced); (b) green "Copied" flash, reverted ≤~4 s; (c) frame identical before/after: (214,84,1042,820).
4 PASS — Empty-bar drag (950,890)→(1030,830): (214,84)→(294,33); Δx=+80 exact, Δy clamped at menu bar (native titlebar-like); reverse restored exactly; later vertical drag (214,136)→(214,33).
5 PASS — AppleActionOnDoubleClick unset → zoom: (214,136,1042,820)→(0,33,1470,923); second double-click restored exact frame.
6 PASS — Right-edge resize 1042→937 wide, then corner 937×820→837×728: bar spans full new width, layout intact.
7 PASS — Press-drag ON cwd widget and ON clock: frame bit-identical (214.0,84.0,1042.0,820.0) before/between/after.
8 PASS — Title-bar band drag still moves window: (214,84)→(294,136), Δx=+80 (partial-y same as bar drags → no differential change); reverse restored x.
9 PASS — Keystrokes h-e-l-l-o-Return: grid shows "hello" twice (kernel echo + /bin/cat), expected double echo.
summary: feature functional: yes; invariants held: yes; regressions: none

Env note: unrelated "XCTest is trying to Enable UI Automation" system auth dialog floated mid-screen all session (left untouched; from another process — likely the concurrent Swift-arm test run). Forced item-9 keystrokes via CGEventPostToPid instead of HID tap; mouse tests unaffected. First app instance auto-exited at 120 s watchdog; relaunched with NICE_POC_SECS=900, all frame-differential tests in one instance.
Verifier artifacts: /tmp/nice-ab-verify-t1r1/. Verifier cleaned up (clipboard restored, app killed by own PID).
