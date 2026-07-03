# t1-swift-2 objective gate — FAIL (differential invariant violated)

O1 Builds clean: PASS — scripts/install.sh BUILD SUCCEEDED, zero errors.
O2 Feature functional: PASS — verifier items 1–6 all PASS.
O3 No regressions + differential invariants: FAIL — verifier item 7: double-click
   on the cwd widget ZOOMS the window (title-bar action fires from a widget),
   violating brief behavior (4) "pressing, dragging, or clicking the widgets
   must never move the window" and §4 invariant (ii). Reproduced 3×. Mechanism
   probe: after click 1 swaps in the shorter "✓ Copied" label, double-click at
   widget-x=40 (inside label) does NOT zoom while x=300 (on the original path
   extent) DOES — the widget hitbox shrinks during the confirmation and click 2
   is routed as empty bar. All other invariants held (widget drags inert, clock
   double-click inert, top-bar drag/double-click unchanged, pill click/reorder
   unchanged with frame constant, terminal input fine). Unit suite 1318/0 was
   green — the documented "compiles + unit-passes while behaviorally wrong"
   shape. NOTE: the identical bug was caught and fixed in-session by the
   t1-rust-1 implementer (geometry-stable overlay).
O4 DNF: no — 67 turns, ~23 min.

## Independent verifier detail (real CGEvents + screenshots; ~15.6 min)

1 PASS — full width at 3440×1410 / 1470×923 / 2200×1000; ~28 px; chrome-matched.
2 PASS — cwd tilde-abbreviated; clock ticked 17:21→17:22 at the boundary.
3 PASS — sentinel replaced by exact displayed text; "✓ Copied" +0.25 s → gone +3.75 s; frame unchanged. (First attempt contaminated by pre-existing clipboard content; redone with sentinel.)
4 PASS — empty-bar drag: (1470,30)→(1720,141) (posted +250,+120; observed +250,+111).
5 PASS — double-click zoom (1720,141,2200×1000)→(1470,30,3440×1410); second restored exactly.
6 PASS — AX resize 3440×1410→2200×1000: bar full-width, layout intact.
7 FAIL — widget drags inert AND clock double-click inert, BUT cwd-widget double-click zooms (3× repro; mechanism above).
8 PASS — top-bar drag moves (x −233, y edge-pinned); top-bar double-click zooms.
9 PASS — 4 pills; click selects (frame unchanged); drag reorders (frame constant).
10 PASS — `echo ab-verify-ok` rendered.
summary: feature functional: yes; invariants held: partial (widget double-click → zoom); regressions: none

Env notes: concurrent gpui-term stole focus early; every action bracketed with
immediate frame reads; contaminated tests re-run with sentinels/live coords.
Clipboard restored; Nice Dev quit via dialog Quit button; prod untouched.
Artifacts /tmp/nice-ab-verify-t1s2/.
