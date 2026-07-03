# t1-swift-1 objective gate — PASS (all items)

O1 Builds clean: PASS — scripts/install.sh BUILD SUCCEEDED, zero errors (build.log; pre-existing CoreSimulator/SwiftTerm-bundle warnings only).
O2 Feature functional: PASS — verifier items 1–6 all PASS.
O3 No regressions + differential invariants: PASS — verifier items 7–10 all PASS; scripts/test.sh -only-testing:NiceUnitTests 1326/0 (implementer-run); UITests TCC-blocked machine-wide (environment note, not gate — see cap-note.md and RESULTS threats).
O4 DNF: no — code final at turn 45; session ran 139 turns (cap analysis in cap-note.md).

## Independent verifier detail (real CGEvents + screenshots; ~10.4 min)

1 PASS — bar at bottom, full width at 700/900/1150/3440, ~27pt (separator y≈423 of 450pt window), same light-gray chrome + hairline as top bar.
2 PASS — left widget "~/Projects/notes/.claude/worktrees/rosy-orbiting-fairy" (live cwd, tilde-abbreviated); clock 16:42@16:42 → 16:43 → 16:44, matching date.
3 PASS — (a) pbpaste = displayed text exactly; (b) "✓ Copied" badge, gone ~3s; (c) frame unchanged (2105,509,900,450); clipboard saved/restored.
4 PASS — empty-bar drag (+120,−80): (2105,509)→(2217,434), delta (+112,−75).
5 PASS — AppleActionOnDoubleClick unset → zoom: (2217,434,900,450)→(1470,30,3440,1410); second restored exactly.
6 PASS — AX-resize 1150×620 and 700×500: bar full-width bottom, layout intact.
7 PASS — press-drag ON cwd widget and ON clock: frame constant; widget double-clicks: frame constant, AXMinimized=false.
8 PASS — top-bar empty drag (−100,+60) → delta (−93,+55); top-bar double-click zoom + restore.
9 PASS — 3 pills: click selects w/o moving window; dragging "Terminal 1" past "Claude" reorders [nick@…, Claude, Terminal 1]→[nick@…, Terminal 1, Claude], frame constant before/mid/after.
10 PASS — `echo ab-verify-ok` typed: command echoed + output rendered.
summary: feature functional: yes; invariants held: yes; regressions: none

Notes: evidence in /tmp/nice-ab-verify-t1s1/; one stray pill click mid-run (helper typo, no movement, reorder re-run cleanly); Nice Dev quit via dialog Quit button; prod Nice (58741) untouched.
