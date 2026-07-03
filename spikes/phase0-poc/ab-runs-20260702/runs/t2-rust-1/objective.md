# t2-rust-1 objective gate — PASS (all items)

O1 Builds clean: PASS — cargo build --bins zero errors (pre-existing notes only).
O2 Feature functional: PASS — verifier items 1–2 PASS.
O3 No regressions + differential invariants: PASS — items 3a/3b/3c, 4, 5 all PASS incl. the 10× focus-cycle storm (no drift/double-spacing) and full-screen enter/EXIT round-trip; headless INTERACTIVE self-test PASS (orchestrator-run); cargo build --bins clean.
O4 DNF: no — 74 turns, ~24.5 min.

## Independent verifier detail (real CGEvents + screenshots + AX; ~9.7 min)

1 PASS — pin = fourth disc right of green: centers red(15.75,13.75) yellow(38.75,13.75) green(61.75,13.75) pin(84.75,13.75); all 14×14 pt, identical cy; green→pin pitch 23.0 pt = EXACT traffic-light pitch; AX cross-check matches.
2 PASS — click → filled amber at identical box; second click → gray ring same box; amber persisted across a focus cycle.
3a PASS — 1042×760→700×500→1042×760: pin rel-green (+23.0, 0.0) in all three states, pixel-identical centroids.
3b PASS — 3 Finder↔gpui-term cycles (frontmost confirmed each): rel-green (+23.0, 0.0), absolute center unchanged.
3c PASS — plain green click entered FS (AXFullScreen=true); pin stayed at same window-relative spot, visible while native lights system-hidden (hover reveal shows system-drawn close/zoom — normal FS); EXIT: Esc failed, Ctrl+Cmd+F failed, AXFullScreen=false SUCCEEDED (err=0); window restored to exactly (214,98,1042×760), all four discs at baseline.
4 PASS — hover glyphs render (x/−/zoom; pin correctly glyph-free); green native action verified; Option+click zoom + restore, discs holding (+23.0,0.0); yellow minimize + AXMinimized=false restore, identical centers; red (last) closed cleanly, exit 0.
5 PASS — 10× storm @0.35s: before/after centroids bit-identical (pitches 46/46/46 px @2x), AX centers identical, frame unchanged.
summary: feature functional: yes; invariants held: yes; regressions: none

Rel-green measurements: baseline/post-shrink/post-regrow/post-3-cycles/post-FS-exit/zoomed/post-zoom-restore/post-min-restore/post-storm ALL (+23.0, 0.0) pt.
Artifacts /tmp/nice-ab-verify-t2r1/. Note: implementer flagged FS-exit as unscriptable; verifier found AXFullScreen=false works — exit realignment ground-truthed, not just structural.
