# c0-swift-1 objective gate — PASS (all items; C0 is objective-only, no judges)

O1 Builds clean: PASS — BUILD SUCCEEDED, zero errors; install exit 0.
O2 Feature functional: PASS — verifier items 1–5 all PASS.
O3 No regressions: PASS — item 6 (top-bar drag, pill click, terminal echo, resize all clean); implementer-run targeted suites 16/16 + 61/61.
O4 DNF: no — 78 turns, ~23 min.

## Verifier detail (~20.3 min)
1 PASS — badge adjacent to trailing top-bar content; started compact (persisted from implementer's session); one click → full "● 0 KB/s".
2 PASS — `yes` stream: "● 1154 KB/s" → "● 1311 KB/s", accent/active style.
3 PASS — bounded burst (yes | head -c 400000): 344→224→0 KB/s, dimmed gray at +3 s; same ≤~3 s dim after closing the streaming tab. Caveat (not a badge fault): after Ctrl+C on an UNBOUNDED yes, app kept draining an internal buffered backlog for minutes (48% CPU, ~4 GB RSS) — bytes genuinely still arriving; badge correctly stayed active and reported the drain. Whether unbounded-output buffering is pre-existing couldn't be baseline-compared; flagged as an app observation, outside this task's scope.
4 PASS — full↔compact toggles both ways; window frame identical before/after every toggle click.
5 PASS — compact persisted through graceful quit (quit dialog Quit-button clicked) + relaunch; full persisted through a second quit/relaunch; left at full.
6 PASS — top-bar drag moves window (e.g. (198,138)→(276,196) for 80/60); pill click selects, frame unchanged; `echo c0-verify-ok` echoes; resize round-trip keeps bar + badge anchored.
summary: feature functional yes; regressions none.
Artifacts /tmp/nice-ab-verify-c0s1/.
