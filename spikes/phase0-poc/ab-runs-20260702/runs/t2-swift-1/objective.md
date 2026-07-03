# t2-swift-1 objective gate — FAIL (traffic-light hover-effect regression, baseline-attributed)

O1 Builds clean: PASS — BUILD SUCCEEDED, zero errors. (A same-day orchestrator
   harness bug aborted install.sh's COPY step mid-session — unrelated to the
   change and to compilation; the implementer diagnosed it correctly and
   completed the install via the script's own staging steps. Do not attribute
   to this session.)
O2 Feature functional: PASS — verifier items 1–3 all PASS with exact-pitch
   measurements (+23 px from green, Δy=0, through resize/zoomed/3× focus
   cycles/two full-screen round-trips, toggle state preserved).
O3 No regressions + differential invariants: FAIL — item 4: traffic-light
   hover glyphs (×/−/zoom) do NOT appear on the artifact build under synthetic
   hover (dwell 1.5–2 s, hidSystemState; Finder positive control shows glyphs).
   Baseline differential probe (no-pin build @ a1c7e43, identical methodology,
   single-jump AND slow-path): glyphs appear on ALL THREE buttons → the
   regression is caused by the change. Brief explicitly required "their normal
   hover effects". Everything else held: zoom/FS/minimize/close work; item 5
   10× focus storm — positions identical, no drift/double-spacing.
   NOTE: implementer self-reported normal hover glyphs; independent
   verification with positive+baseline controls contradicts — second Swift
   run whose self-verified claim failed ground truth.
O4 DNF: no — 77 turns, ~26 min.

## Verifier detail (~14.6 min, real CGEvents + AX + pixel diffs)
1 PASS — pin (AXCheckBox "Pin window") at exact 23px pitch, same 16×16 size/row as lights.
2 PASS — click toggles false→true→false; strip pixel-identical after round-trip (diff bbox None); frame unchanged (no move/zoom from clicks).
3a PASS — 700×500 shrink / 1000×720 regrow / 3440×1410 zoomed / restore: +23/Δy0 in every state.
3b PASS — 3 focus cycles: identical positions all three.
3c PASS — FS via Ctrl+Cmd+F with pin ACTIVE: lights+pin hidden in FS (parity); after exit exact placement AND toggle state preserved; second FS round-trip via green click also exact.
4 PARTIAL→FAIL(differential) — hover glyphs absent (baseline probe: present); Opt+green zoom works; FS works; minimize + AX restore works; close verified on a real second window (Cmd+N → red → app's own Close dialog → closed; original intact; second window's pin at identical +23).
5 PASS — 10× storm ~0.35s: AX positions identical (uniform 23px pitch), strip pixel-identical.
summary: feature functional yes; invariants: hover-effect regression (differential, baseline-attributed); placement invariants held.

Baseline probe incidentals (for the record): on baseline, once triggered,
glyphs persisted after cursor left the window (synthetic hover-exit not
processed) + a notch element below green persisted — quirks of synthetic
hover on this app, but the PRESENCE differential is clean and replicated.
Evidence: /tmp/nice-ab-verify-t2s1/ + /tmp/nice-ab-baseline-probe/.
