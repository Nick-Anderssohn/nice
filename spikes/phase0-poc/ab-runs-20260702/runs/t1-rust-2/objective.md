# t1-rust-2 objective gate — PASS (all items)

O1 Builds clean: PASS — cargo build --bins, zero errors (build.log; pre-existing block-crate note only).
O2 Feature functional: PASS — verifier items 1–6 all PASS.
O3 No regressions + differential invariants: PASS — verifier items 7–9 all PASS; headless INTERACTIVE self-test PASS exit 0 (orchestrator-run); cargo build --bins clean.
O4 DNF: no — 86 tool-calling turns (≤100 ✓), ~22 min.

## Independent verifier detail (real CGEvents + screenshots; ~8.8 min)

1 PASS — bar full width at bottom, ~28.5 pt (57 px @2x), dark-gray chrome consistent; path left, clock right.
2 PASS — left widget "~/Projects/nice/.claude/worktrees/ab-rust/spikes/phase0-poc" (tilde-abbreviated real cwd); clock 16:52→16:53→16:57→16:59 matching system time at each capture.
3 PASS — (a) pbpaste = displayed text exactly (sentinel replaced); (b) green "Copied" badge, gone ~3 s; (c) frame unchanged (214,84,1042,820).
4 PASS — empty-bar drag: (214,84)→(294,33); Δx=+80 exact, Δy clamped at menu bar like title-bar drag.
5 PASS — pref unset → zoom: (294,33,1042,820)→(0,33,1470,923); second double-click restored exactly.
6 PASS — corner resize → (234,93,917,708): bar full-width bottom, layout intact.
7 PASS — drag ON path widget and ON clock: frame bit-identical after both; double-click ON each widget: no zoom/minimize, frame constant.
8 PASS — title-bar drag: exactly Δ(−60,+60) — pre-existing surface unchanged.
9 PASS — keystrokes echo twice on grid (kernel + cat): "aaaaa", "hello".
summary: feature functional: yes; invariants held: yes; regressions: none

Verifier artifacts /tmp/nice-ab-verify-t1r2/; app killed by own PID (exit 143 expected); clipboard restored.
