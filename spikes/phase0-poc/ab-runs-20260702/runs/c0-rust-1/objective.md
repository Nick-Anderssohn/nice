# c0-rust-1 objective gate — PASS (all items; C0 is objective-only, no judges)

O1 Builds clean: PASS — cargo build --bins zero errors.
O2 Feature functional: PASS — verifier items 1–5 all PASS.
O3 No regressions: PASS — item 6 (resize + terminal echo); all pre-existing headless modes PASS (orchestrator-run: workload, INTERACTIVE pty-echo, badge selftest added by the run itself).
O4 DNF: no — 65 turns, ~20 min.

## Verifier detail (~8.1 min)
1 PASS — chrome bar: "pty: /bin/cat" left, badge (dot + "0 KB/s") right.
2 PASS — 1552 'a' keys/s (~3.1 KB/s echoed incl. kernel+cat): green accent dot + "2 KB/s" during burst — within 2×.
3 PASS — timed dim series: green at stop, green "0 KB/s" +1 s, dimmed gray by +2 s.
4 PASS — click full→compact (dot-only); second click restored. (First attempt was a coordinate miss, not a bug.)
5 PASS — persistence: file read "compact", killed PID, relaunch → dot-only; toggled back, file "full".
6 PASS — AX resize 1042×820→760×520: bar spans, badge pinned right; "hello" echoed twice.
summary: feature functional yes; regressions none.
Artifacts /tmp/nice-ab-verify-c0r1/.
