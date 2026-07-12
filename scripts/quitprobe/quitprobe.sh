#!/bin/bash
# quitprobe.sh — black-box acceptance test for Nice Dev quit/close confirmations,
# driven with REAL CGEvents/AX against the INSTALLED app. Keys post via
# CGEventPostToPid to the app's own pid (the standing rule): globally-posted
# synthetic keys can be consumed by third-party ACTIVE event taps (Wispr Flow
# ate synthetic Escape system-wide, 2026-07-07 — physical keys unaffected),
# which false-fails the Escape-cancel asserts against a healthy app. A quit-bug fix does
# not count until this prints OVERALL: PASS.
#
# Usage: scripts/quitprobe/quitprobe.sh   (helpers auto-compiled to ./.build)
# Requires: Accessibility trust (AXIsProcessTrusted), display awake.
#
# A: cmd-q  -> dialog VISIBLY painted; Escape cancels; pixels restore.
# C: red button -> dialog visibly painted; Escape cancels; window survives.
# D: menu Quit  -> dialog visibly painted; Escape cancels.
# B: cmd-q + Enter -> process actually exits. (last; app relaunched after)
set -u
# Helpers: compiled from the .swift sources next to this script on first run.
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
H="${1:-$SRC_DIR/.build}"; mkdir -p "$H"
for tool in keypost winids imgdiff; do
  if [ ! -x "$H/$tool" ] || [ "$SRC_DIR/$tool.swift" -nt "$H/$tool" ]; then
    swiftc -O "$SRC_DIR/$tool.swift" -o "$H/$tool" || exit 2
  fi
done
OUT="${TMPDIR:-/tmp}/quitprobe-$$"; mkdir -p "$OUT"
fail=0
say() { echo "[quitprobe] $*"; }
assert() { if [ "$2" -eq 0 ]; then say "PASS: $1"; else say "FAIL: $1"; fail=1; fi; }
getpid() { ps -Aww -o pid=,args= | grep -E 'Nice Dev\.app/Contents/MacOS/Nice Dev' \
  | grep -v grep | awk '{print $1}' | head -1; }
launch_and_pid() { open -a "Nice Dev"; sleep 2.5; getpid; }
diffpct() { "$H/imgdiff" "$1" "$2"; }
shoot() { screencapture -x -o -l "$WIN" "$1"; }

# Always test a FRESH instance of the installed binary — a running instance may
# predate the install (stale binary) or carry invisible modal state from
# earlier attempts (that exact confound produced a false diagnosis once).
OLD=$(getpid); [ -n "${OLD}" ] && { say "killing stale instance $OLD"; kill "$OLD"; sleep 1; }
PID=$(launch_and_pid)
[ -z "${PID}" ] && { say "FATAL: cannot launch Nice Dev"; exit 2; }
WIN=$("$H/winids" "$PID" | head -1 | awk '{print $1}')
say "pid=$PID win=$WIN"
osascript -e 'tell application "Nice Dev" to activate'; sleep 0.6

probe_visible() { # probe_visible <name> <trigger-cmd...>
  local name="$1"; shift
  shoot "$OUT/$name-before.png"
  "$@"; sleep 1.3
  kill -0 "$PID" 2>/dev/null; assert "$name: app alive after trigger (dialog, not instant quit)" $?
  shoot "$OUT/$name-after.png"
  local d; d=$(diffpct "$OUT/$name-before.png" "$OUT/$name-after.png")
  say "$name center diff: $d (need > 0.008)"
  awk "BEGIN{exit !($d > 0.008)}"; assert "$name: dialog visibly painted" $?
  "$H/keypost" 53 "$PID"; sleep 1.0
  kill -0 "$PID" 2>/dev/null; assert "$name: app alive after Escape" $?
  shoot "$OUT/$name-dismissed.png"
  d=$(diffpct "$OUT/$name-before.png" "$OUT/$name-dismissed.png")
  say "$name dismissed diff: $d (need < 0.008)"
  awk "BEGIN{exit !($d < 0.008)}"; assert "$name: dialog visibly dismissed" $?
}

probe_visible "A-cmdq"  "$H/keypost" 12 cmd "$PID"
probe_visible "C-redbtn" osascript -e 'tell application "System Events" to tell process "Nice Dev" to click button 1 of window 1'
probe_visible "D-menu"  osascript -e 'tell application "System Events" to tell process "Nice Dev" to click (first menu item of menu 1 of menu bar item 2 of menu bar 1 whose name begins with "Quit")'

# B: confirm actually quits
"$H/keypost" 12 cmd "$PID"; sleep 1.3
"$H/keypost" 36 "$PID"; sleep 2.0
if kill -0 "$PID" 2>/dev/null; then assert "B: Enter confirms quit (process exits)" 1
else assert "B: Enter confirms quit (process exits)" 0; fi

NEWPID=$(launch_and_pid)
say "relaunched pid=${NEWPID:-NONE}; artifacts in $OUT"
[ $fail -eq 0 ] && say "OVERALL: PASS" || say "OVERALL: FAIL"
exit $fail
