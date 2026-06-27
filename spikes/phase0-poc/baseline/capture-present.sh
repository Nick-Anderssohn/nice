#!/usr/bin/env bash
# capture-present.sh — capture the SwiftTerm fork's "Metal.Draw" OSSignposter
# stream (one signpost per MetalTerminalRenderer.draw(in:) under SWIFTTERM_PROFILE=1)
# while the fixture replays, and reduce inter-draw intervals to p50/p95 (ms) plus
# a dropped-frame cliff count, using the SAME nearest-rank logic as
# harness::interval_stats / harness::percentiles.
#
# This is the BASELINE side of the "Term present FPS" metric. It uses the fork's
# existing signpost stream (no fork patch), exactly the source the PoC uses for
# its GPU-side present timing — so both sides are like-for-like.
#
# Signpost provenance (from the prompt + fork):
#   subsystem == "org.tirania.SwiftTerm"  category == "MetalProfile"  name "Metal.Draw"
# Nice Dev MUST be launched with env SWIFTTERM_PROFILE=1 for the signpost to fire
# (see RUN.md for the env-passing caveat).
#
# REDUCTION (mirrors src/harness.rs):
#   * intervals_ms[i] = ms_between(ts[i], ts[i+1])   (windows(2), harness:137-139)
#       ms = (ticks_delta / hw.tbfrequency) * 1000   (same mach domain as
#       mach_absolute_time; harness uses mach_timebase, we use the tick frequency
#       so machTimestamp ticks convert identically)
#   * cliffs = count(interval > 16.6)                (harness:141; 16.6 ms == 2x a
#       120 Hz ProMotion frame -> the cliff count is 120Hz-CALIBRATED. On a 60 Hz
#       panel a clean ~60 fps cadence (16.7 ms) trips ~every interval; read p50/p95
#       there, not the cliff count. See README §10.)
#   * sort ascending; p50 = sorted[ floor(m/2) ], p95 = sorted[ min(floor(m*95/100), m-1) ],
#       p99 = sorted[ min(floor(m*99/100), m-1) ]    (harness::percentiles, :165-175)
#
# USAGE:
#   capture-present.sh <duration_seconds> [raw_ndjson_out]
#     <duration_seconds>   how long to stream the log (RUN.md: >=18, run CONCURRENTLY
#                          with replay.sh; start this first, then start replay).
#     raw_ndjson_out       raw capture file (default /tmp/nice-present.ndjson),
#                          kept for audit (capture raw, reduce late — harness §A).
#
# OUTPUT (stdout, last line, scrape-friendly):
#   PRESENT samples=<n_draws> p50_ms=<x.xx> p95_ms=<x.xx> p99_ms=<x.xx> \
#           fps_p50=<x.x> cliffs=<k>
#
# GUARDRAILS: read-only observation of Nice Dev's logs. Does not touch prod or
# Nice source. Do not run as scaffolding — RUN.md drives it on a display.
set -euo pipefail

DURATION="${1:-}"
RAW="${2:-/tmp/nice-present.ndjson}"
[ -n "$DURATION" ] || { echo "usage: capture-present.sh <duration_seconds> [raw_ndjson_out]" >&2; exit 1; }

TBFREQ="$(sysctl -n hw.tbfrequency)"   # mach ticks per second (machTimestamp domain)
: > "$RAW"

echo "capture-present.sh: streaming ${DURATION}s of Metal.Draw signposts -> $RAW" >&2

# Stream only MetalProfile signposts. Capture raw ndjson; reduce after.
log stream --style ndjson \
  --predicate 'subsystem == "org.tirania.SwiftTerm" && category == "MetalProfile"' \
  >> "$RAW" 2>/dev/null &
LPID=$!
# Ensure the log stream is torn down even if interrupted.
trap 'kill "$LPID" 2>/dev/null || true' EXIT INT TERM
sleep "$DURATION"
kill "$LPID" 2>/dev/null || true
wait "$LPID" 2>/dev/null || true
trap - EXIT INT TERM

# Extract machTimestamp of each Metal.Draw BEGIN record (one per draw(in:)).
# Each ndjson record is one line. We keep only begin events so we count one
# timestamp per draw (begin+end would double-count).
grep 'Metal.Draw' "$RAW" \
  | grep -i 'begin' \
  | grep -Eo '"machTimestamp":[0-9]+' \
  | grep -Eo '[0-9]+' \
  | awk -v tb="$TBFREQ" '
    { ts[NR]=$1 }
    END{
      n=NR
      if(n<2){ printf "PRESENT samples=%d p50_ms=0 p95_ms=0 p99_ms=0 fps_p50=0 cliffs=0\n", n; exit }
      # intervals (ms) from consecutive timestamps (harness windows(2))
      m=0
      for(i=2;i<=n;i++){
        d=(ts[i]-ts[i-1])/tb*1000.0
        if(d<0) continue            # guard against any out-of-order log delivery
        m++; iv[m]=d
        if(d>16.6) cliffs++         # harness cliff_ms (120Hz-calibrated)
      }
      if(m==0){ printf "PRESENT samples=%d p50_ms=0 p95_ms=0 p99_ms=0 fps_p50=0 cliffs=0\n", n; exit }
      # sort ascending
      for(i=1;i<=m;i++) for(j=i+1;j<=m;j++) if(iv[j]<iv[i]){t=iv[i];iv[i]=iv[j];iv[j]=t}
      # harness::percentiles indices (0-indexed -> awk 1-indexed +1; clamp to m)
      p50=iv[int(m/2)+1]
      i95=int(m*95/100); if(i95>m-1)i95=m-1; p95=iv[i95+1]
      i99=int(m*99/100); if(i99>m-1)i99=m-1; p99=iv[i99+1]
      fps = (p50>0)?1000.0/p50:0
      printf "PRESENT samples=%d p50_ms=%.2f p95_ms=%.2f p99_ms=%.2f fps_p50=%.1f cliffs=%d\n", \
             n, p50, p95, p99, fps, cliffs+0
    }'
