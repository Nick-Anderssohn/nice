#!/bin/zsh
# wedgewatch.sh <pid> <winid> <active_window_s> <outdir>
# Captures the window every 500ms for active_window_s+3s and reports the
# longest identical-frame streak that lands ENTIRELY within the active window
# (the flood must be running for the full active_window_s). Streaks that
# include the trailing idle after the flood are excluded — a settled screen
# after input stops is correct, not a wedge. A streak >= 4 (2s) DURING the
# active window = presents wedged.
set -u
PID_T="$1"; WID="$2"; ACTIVE="$3"; OUT="$4"
mkdir -p "$OUT"
N=$(( (ACTIVE + 3) * 2 ))
ACTIVE_FRAMES=$((ACTIVE * 2))
for i in $(seq -w 1 $N); do
  screencapture -x -o -l "$WID" "$OUT/f$i.png" 2>/dev/null
  sleep 0.5
done
/usr/bin/python3 - "$OUT" "$ACTIVE_FRAMES" <<'EOF'
import sys, glob, hashlib
out = sys.argv[1]; active = int(sys.argv[2])
frames = sorted(glob.glob(f"{out}/f*.png"))
hashes = [hashlib.md5(open(f, 'rb').read()).hexdigest() for f in frames]
# Only judge frames [0, active): the flood is running through then.
h = hashes[:active]
streak, max_streak, max_at = 1, 1, 0
for i in range(1, len(h)):
    if h[i] == h[i-1]:
        streak += 1
        if streak > max_streak: max_streak, max_at = streak, i
    else:
        streak = 1
uniq = len(set(h))
print(f"active_frames={len(h)} unique={uniq} max_identical_streak_during_flood={max_streak} (ending at frame {max_at})")
print(f"(trailing idle frames {active}-{len(hashes)} excluded)")
print("WEDGED" if max_streak >= 4 else "LIVE")
EOF
