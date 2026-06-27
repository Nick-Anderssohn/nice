#!/usr/bin/env bash
# sample-mem.sh — externally sample a Nice Dev process's phys_footprint every
# 100 ms for a duration, then reduce to MiB stats. External sampling needs no
# debugger entitlement (no task_for_pid) and never contacts prod.
#
# The PoC measures ITSELF in-process via mach task_info phys_footprint
# (harness::mem::sample, MiB = bytes/1024/1024). This samples the SAME metric
# externally so the two are directly comparable. Reduction uses the harness
# percentile convention: median = sorted[ floor(n/2) ] (0-indexed); peak = max.
#
# USAGE:
#   sample-mem.sh <pid> <duration_seconds> [label]
#     <pid>               Nice Dev pid (resolve via nice-process-check skill;
#                         NEVER pgrep). See RUN.md.
#     <duration_seconds>  how long to sample (RUN.md: >=30 idle, >=60 under-load)
#     label               optional tag printed with the result + raw filename
#
# OUTPUT (stdout, last line, easy to scrape):
#   MEM <label> samples=<n> median_mib=<x.x> peak_mib=<x.x>
#   For the IDLE window use median_mib as "idle".
#   For the UNDER-LOAD window: median_mib == steady, peak_mib == peak.
#
# Raw per-sample MiB values are written to:
#   ${NICE_MEM_OUT:-/tmp/nice-baseline-mem-<label>.txt}   (capture raw, reduce late)
#
# SOURCE PREFERENCE (all report phys_footprint, falling back gracefully):
#   1. footprint <pid>            -> "Physical footprint:" value
#   2. vmmap --summary <pid>      -> "Physical footprint:" line
#   3. ps -o rss= -p <pid>        -> RSS comparator (KiB) if neither is available
#
# UNIT NOTE: footprint/vmmap print mebibyte-scaled values labelled K/M/G
# (1024-based, same as the harness MiB). We normalise K->/1024, M->as-is,
# G->*1024, bytes->/1048576, ps RSS (KiB)->/1024. Differences vs the harness are
# sub-MiB rounding only.
#
# GUARDRAILS: pass a Nice Dev pid only. Do not run as scaffolding — RUN.md drives it.
set -euo pipefail

PID="${1:-}"
DURATION="${2:-}"
LABEL="${3:-load}"
[ -n "$PID" ] && [ -n "$DURATION" ] || { echo "usage: sample-mem.sh <pid> <duration_seconds> [label]" >&2; exit 1; }
kill -0 "$PID" 2>/dev/null || { echo "sample-mem.sh: pid $PID not alive" >&2; exit 1; }

RAW="${NICE_MEM_OUT:-/tmp/nice-baseline-mem-$LABEL.txt}"
: > "$RAW"

# Convert a "<number><unit>" footprint/vmmap token to MiB on stdout.
to_mib() { # $1 number, $2 unit (K|M|G|B or empty)
  awk -v n="$1" -v u="$2" 'BEGIN{
    u=toupper(u);
    if(u=="K"||u=="KB"){print n/1024}
    else if(u=="G"||u=="GB"){print n*1024}
    else if(u=="B"||u==""){ if(n>1048576) print n/1048576; else print n }  # bare big number = bytes
    else {print n}   # M/MB already MiB-scaled
  }'
}

sample_mib() {
  local line num unit
  if command -v footprint >/dev/null 2>&1; then
    # First "Physical footprint:" line is the live value (a later line is the peak).
    line="$(footprint "$PID" 2>/dev/null | grep -i 'physical footprint' | head -1 || true)"
    if [ -n "$line" ]; then
      num="$(printf '%s' "$line" | grep -Eo '[0-9]+(\.[0-9]+)?' | head -1)"
      unit="$(printf '%s' "$line" | grep -Eo '[0-9](\.[0-9]+)?[[:space:]]*[KMGB]B?' | grep -Eo '[KMGB]B?' | head -1)"
      [ -n "$num" ] && { to_mib "$num" "$unit"; return; }
    fi
  fi
  if command -v vmmap >/dev/null 2>&1; then
    line="$(vmmap --summary "$PID" 2>/dev/null | grep -i 'physical footprint:' | head -1 || true)"
    if [ -n "$line" ]; then
      num="$(printf '%s' "$line" | grep -Eo '[0-9]+(\.[0-9]+)?' | head -1)"
      unit="$(printf '%s' "$line" | grep -Eo '[0-9](\.[0-9]+)?[[:space:]]*[KMGB]B?' | grep -Eo '[KMGB]B?' | head -1)"
      [ -n "$num" ] && { to_mib "$num" "$unit"; return; }
    fi
  fi
  # Fallback: RSS in KiB.
  num="$(ps -o rss= -p "$PID" 2>/dev/null | tr -d ' ')"
  [ -n "$num" ] && to_mib "$num" "K"
}

# Sample loop: every 100 ms for DURATION seconds.
end=$(awk -v d="$DURATION" 'BEGIN{ srand(); print systime()+d }')
while [ "$(date +%s)" -lt "$end" ]; do
  kill -0 "$PID" 2>/dev/null || { echo "sample-mem.sh: pid $PID exited mid-sample" >&2; break; }
  m="$(sample_mib || true)"
  [ -n "${m:-}" ] && printf '%s\n' "$m" >> "$RAW"
  sleep 0.1
done

# Reduce: median (harness convention sorted[floor(n/2)]) + peak (max).
awk -v label="$LABEL" -v raw="$RAW" '
  { v[NR]=$1+0 }
  END{
    n=NR
    if(n==0){ printf "MEM %s samples=0 median_mib=0 peak_mib=0\n", label; exit }
    # sort ascending
    for(i=1;i<=n;i++) for(j=i+1;j<=n;j++) if(v[j]<v[i]){t=v[i];v[i]=v[j];v[j]=t}
    med=v[int(n/2)+1]          # 0-indexed floor(n/2) -> awk 1-indexed +1
    peak=v[n]
    printf "MEM %s samples=%d median_mib=%.1f peak_mib=%.1f (raw=%s)\n", label, n, med, peak, raw
  }' "$RAW"
