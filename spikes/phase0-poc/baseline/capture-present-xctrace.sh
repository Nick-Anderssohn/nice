#!/usr/bin/env bash
# capture-present-xctrace.sh — WORKING replacement for capture-present.sh.
#
# WHY: capture-present.sh reduces `log stream --predicate '… category ==
# "MetalProfile"'`, which returns ZERO samples. os_signpost events are kdebug
# signpost records, NOT Unified-Logging messages, so `log stream` never sees
# them — only Instruments/xctrace capture them. (Discovered §13 spike 4,
# 2026-07-01.) This script captures via xctrace and reduces to the same
# percentile format harness.rs uses.
#
# Prereq: the target process must be launched with SWIFTTERM_PROFILE=1 so the
# fork's Metal.Draw / Parser.Parse signposts fire.
#
# USAGE:
#   capture-present-xctrace.sh <pid> <duration_seconds> [trace_out_dir]
# Then, concurrently, drive load (e.g. baseline/replay.sh <tty> paced <dur>).
#
# OUTPUT: prints the reduced cadence line AND the per-draw cost table. Note:
#   - inter-draw CADENCE is meaningless as a frame rate for SwiftTerm — its
#     MTKView is demand-driven (isPaused=true; enableSetNeedsDisplay=true,
#     MacTerminalView.swift:360-361). The COMPARABLE number is per-draw COST.
set -euo pipefail
PID="${1:-}"; DUR="${2:-}"; OUTDIR="${3:-/tmp/nice-xctrace}"
[ -n "$PID" ] && [ -n "$DUR" ] || { echo "usage: $0 <pid> <duration_seconds> [trace_out_dir]" >&2; exit 1; }
HERE="$(cd "$(dirname "$0")" && pwd)"
TRACE="$OUTDIR/nice-dev.trace"
XML="$OUTDIR/signpost-intervals.xml"
rm -rf "$TRACE"; mkdir -p "$OUTDIR"

echo "capture-present-xctrace.sh: recording ${DUR}s attached to pid $PID -> $TRACE" >&2
echo "  (start your paced replay NOW, in another shell, for the same duration)" >&2
xcrun xctrace record --template 'Logging' --attach "$PID" --time-limit "${DUR}s" --output "$TRACE" >&2

xcrun xctrace export --input "$TRACE" \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost-interval"]' > "$XML"

echo "=== inter-draw CADENCE (NOT a frame rate for demand-driven SwiftTerm) ==="
python3 "$HERE/reduce-signposts.py" "$XML" Metal.Draw
echo "=== per-draw COST (this is the comparable number) ==="
python3 "$HERE/draw-durations.py" "$XML"
