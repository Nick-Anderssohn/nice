#!/bin/zsh
# cpumeter.sh <pid> <label> -- <workload command...>
# Measures the target process's TOTAL CPU time consumed while the workload
# runs (delta of ps cputime + utime/stime via proc rusage fallback), plus
# wall time. Robust to sampling gaps, comparable across apps.
#
# With no workload command (just <pid> <label> <seconds>), measures an idle
# window of that many seconds.
set -u
PID_T="$1"; LABEL="$2"; shift 2

cputime_s() {
  # ps cputime format: [dd-]hh:mm:ss.cc or mm:ss.cc
  local raw
  raw=$(ps -p "$1" -o cputime= | tr -d ' ')
  [[ -z "$raw" ]] && { echo ""; return; }
  /usr/bin/python3 - "$raw" <<'EOF'
import sys
raw = sys.argv[1]
days = 0
if '-' in raw:
    d, raw = raw.split('-', 1)
    days = int(d)
parts = [float(p) for p in raw.split(':')]
while len(parts) < 3:
    parts.insert(0, 0.0)
h, m, s = parts
print(days*86400 + h*3600 + m*60 + s)
EOF
}

T0_CPU=$(cputime_s $PID_T)
[[ -z "$T0_CPU" ]] && { echo "cpumeter: pid $PID_T not found"; exit 1; }
T0_WALL=$(date +%s.%N)

if [[ "${1:-}" == "--" ]]; then
  shift
  "$@"
  # Workload commands often return before the app finishes rendering the
  # induced output (e.g. typing `seq 1 1000000` returns after the keystrokes,
  # not after the scroll). Wait for the app to go quiescent: %cpu < 5 for 3
  # consecutive 500ms samples (cap 180s).
  QUIET=0; WAITED=0
  while (( QUIET < 3 && WAITED < 360 )); do
    sleep 0.5; WAITED=$((WAITED+1))
    PCT=$(ps -p $PID_T -o %cpu= | tr -d ' ')
    [[ -z "$PCT" ]] && break
    if (( $(echo "$PCT < 5" | bc -l) )); then QUIET=$((QUIET+1)); else QUIET=0; fi
  done
else
  sleep "${1:-60}"
fi

T1_CPU=$(cputime_s $PID_T)
T1_WALL=$(date +%s.%N)
[[ -z "$T1_CPU" ]] && { echo "cpumeter: pid $PID_T exited during workload"; exit 1; }

/usr/bin/python3 -c "
cpu = $T1_CPU - $T0_CPU
wall = $T1_WALL - $T0_WALL
print(f'cpumeter[$LABEL] pid=$PID_T cpu_s={cpu:.2f} wall_s={wall:.1f} avg_cpu_pct={100*cpu/wall:.1f}')
"
