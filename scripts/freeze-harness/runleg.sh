#!/bin/zsh
# runleg.sh <legname> <pid> <duration_s> -- <action command...>
#
# Wraps an input action with:
#  - axping (250ms AX round-trips, CSV) — freeze detector + duration meter
#  - CPU sampler (ps %cpu every 500ms)  — saturation (~100%) vs blocked (~0%)
#  - auto `sample` on the FIRST HANG-DETECTED (2s threshold), 3s capture
# Results land in $RESULTS/<legname>/.
set -u
HERE="${0:A:h}"
RESULTS="$HERE/../results"

LEG="$1"; PID_T="$2"; DUR="$3"
shift 3
[[ "$1" == "--" ]] && shift

OUT="$RESULTS/$LEG"
mkdir -p "$OUT"
echo "=== leg $LEG pid=$PID_T dur=${DUR}s action: $*" | tee "$OUT/leg.log"

# CPU sampler
(
  while kill -0 $PID_T 2>/dev/null; do
    echo "$(date +%s.%N),$(ps -p $PID_T -o %cpu= | tr -d ' ')"
    sleep 0.5
  done
) > "$OUT/cpu.csv" &
CPU_PID=$!

# axping with hang-triggered sample (only the first hang gets sampled)
SAMPLED="$OUT/.sampled"
rm -f "$SAMPLED"
"$HERE/axping" $PID_T --interval-ms 250 --hang-ms 2000 --duration-s $((DUR + 90)) \
  > "$OUT/axping.csv" \
  2> >(while IFS= read -r line; do
         echo "$(date +%T) $line" >> "$OUT/events.log"
         if [[ "$line" == HANG-DETECTED* && ! -e "$SAMPLED" ]]; then
           touch "$SAMPLED"
           sample $PID_T 3 -mayDie -file "$OUT/hang-sample.txt" >> "$OUT/events.log" 2>&1 &
         fi
       done) &
AX_PID=$!

sleep 1
LEG_T0=$(date +%s)
# Run the action (the flood in the pane may run long past the action command)
"$@" >> "$OUT/leg.log" 2>&1
ACTION_RC=$?
echo "action rc=$ACTION_RC" >> "$OUT/leg.log"

# Watch for the FULL leg duration from action start (the flood keeps running
# after the action command returns), plus a recovery tail: only stop once the
# last 4 probes were 'ok' (or a hard cap of DUR+60s passes).
while :; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - LEG_T0))
  if (( ELAPSED >= DUR )); then
    TAIL_OK=$(tail -4 "$OUT/axping.csv" | grep -c ",ok" || true)
    (( TAIL_OK == 4 )) && break
    (( ELAPSED >= DUR + 60 )) && { echo "recovery-tail cap hit" >> "$OUT/leg.log"; break; }
  fi
  kill -0 $AX_PID 2>/dev/null || break   # axping hit its own duration cap
  sleep 2
done
kill $AX_PID $CPU_PID 2>/dev/null
wait 2>/dev/null

# Summarize
/usr/bin/python3 - "$OUT" <<'EOF'
import sys, os, csv
out = sys.argv[1]
rows = list(csv.DictReader(open(os.path.join(out, 'axping.csv'))))
lat = [float(r['latency_ms']) for r in rows]
if lat:
    lat_sorted = sorted(lat)
    n = len(lat)
    print(f"axping: n={n} max={max(lat):.0f}ms p95={lat_sorted[int(n*0.95)]:.1f}ms hangs={sum(1 for r in rows if r['status']=='hang')} slow={sum(1 for r in rows if r['status']=='slow')}")
cpu_path = os.path.join(out, 'cpu.csv')
if os.path.exists(cpu_path):
    cpus = [float(l.split(',')[1]) for l in open(cpu_path) if ',' in l and l.strip().split(',')[1]]
    if cpus:
        print(f"cpu: max={max(cpus):.0f}% mean={sum(cpus)/len(cpus):.0f}%")
ev = os.path.join(out, 'events.log')
if os.path.exists(ev):
    print("events:")
    print(open(ev).read())
smp = os.path.join(out, 'hang-sample.txt')
print(f"hang-sample: {'CAPTURED ' + str(os.path.getsize(smp)) + ' bytes' if os.path.exists(smp) else 'none'}")
EOF
