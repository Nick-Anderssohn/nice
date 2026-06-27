#!/usr/bin/env bash
# replay.sh — feed the deterministic fixture into a running Nice Dev terminal
# pane WITHOUT keystroke injection.
#
# HOW IT WORKS (no CGEvent / no key injection):
#   A pseudo-terminal has a master (held by the terminal emulator, Nice Dev) and
#   a slave (the pane shell's stdin/stdout, /dev/ttysNNN). Bytes WRITTEN to the
#   slave device travel up to the master read side exactly as if the shell had
#   printed them — i.e. they drive Nice's real PTY -> SwiftTerm feed/render path.
#   So we simply write the fixture bytes to /dev/ttysNNN. This is output-direction
#   traffic, not input, so it is NOT subject to ECHO/line-discipline and needs no
#   key events and no Accessibility TCC.
#
# USAGE:
#   replay.sh <tty> [mode] [target_seconds]
#     <tty>            the pane shell SLAVE pty, e.g. /dev/ttys003 (or "ttys003")
#     mode             "paced" (default, ~500000 bytes/sec to match the PoC
#                      WorkloadProfile.bytes_per_sec) or "max" (cat, max throughput)
#     target_seconds   paced mode only: keep replaying (looping the fixture) until
#                      at least this many seconds of stream have been written.
#                      Default 20 (the fixture is 4,000,000 B = 8 s at 500000 B/s,
#                      so >=18 s requires looping it ~3x). Ignored in "max" mode.
#
# HOW TO FIND <tty> (NEVER pgrep — macOS truncates a GUI app's comm to 16 chars):
#   1. Launch exactly one Nice Dev pane (see RUN.md).
#   2. Resolve the Nice Dev pid via the nice-process-check skill:
#        ~/.claude/skills/nice-process-check/check.sh   # prints the dev pid line
#      (or reuse the skill's exact, pgrep-free matcher — see find_pane_tty() below).
#   3. The pane shell is a child of the Nice Dev process and owns a real tty;
#      Nice Dev itself shows tty "??". Read it with:
#        ps -o tty= -p <shell-pid>     ->  ttys003   ->  /dev/ttys003
#   This script can do steps 2-3 for you: run `replay.sh --find-tty`.
#
# GUARDRAILS: targets Nice Dev only. Does not touch prod Nice. Read-only w.r.t.
# Nice source. Do not run as part of scaffolding — RUN.md drives it on a display.
set -euo pipefail

FIXTURE="${NICE_POC_FIXTURE:-/tmp/nice-fixture.bin}"
BYTES_PER_SEC=500000          # matches harness.rs WorkloadProfile::default()
CHUNK=25000                   # 25000 B every 0.05 s == 500000 B/s
INTERVAL=0.05

die() { printf 'replay.sh: %s\n' "$*" >&2; exit 1; }

# --- pgrep-free Nice Dev pid + pane-tty resolver (mirrors nice-process-check) ---
find_dev_pid() {
  # Snapshot argv via ps (KERN_PROCARGS2); the dev matcher never cross-matches
  # prod because "Nice.app" is not a substring of "Nice Dev.app".
  ps -Aww -o pid=,args= \
    | grep -E 'Nice Dev\.app/Contents/MacOS/Nice Dev( |$)' \
    | awk '{print $1; exit}'
}

find_pane_tty() {
  local devpid shellpid tty
  devpid="$(find_dev_pid)" || true
  [ -n "${devpid:-}" ] || die "Nice Dev not running (resolve via nice-process-check)"
  # Pane shell = child of Nice Dev that owns a real tty (Nice Dev itself = "??").
  shellpid="$(ps -Aww -o pid=,ppid=,tty= \
    | awk -v p="$devpid" '$2==p && $3!="??" && $3!="?" {print $1; exit}')"
  [ -n "${shellpid:-}" ] || die "no child shell with a tty under Nice Dev pid $devpid"
  tty="$(ps -o tty= -p "$shellpid" | tr -d ' ')"
  [ -n "$tty" ] || die "could not read tty for shell pid $shellpid"
  printf '/dev/%s\n' "$tty"
}

if [ "${1:-}" = "--find-tty" ]; then
  find_pane_tty
  exit 0
fi

TTY="${1:-}"
MODE="${2:-paced}"
TARGET_SECONDS="${3:-20}"
[ -n "$TTY" ] || die "usage: replay.sh <tty> [paced|max] [target_seconds]  (or --find-tty)"
case "$TTY" in /dev/*) : ;; *) TTY="/dev/$TTY" ;; esac
[ -w "$TTY" ] || die "tty $TTY is not writable (is it the pane SLAVE pty? is the pane alive?)"
[ -r "$FIXTURE" ] || die "fixture $FIXTURE missing (regenerate: cd ../ && NICE_POC_FIXTURE=$FIXTURE cargo run)"

FSIZE="$(/usr/bin/stat -f%z "$FIXTURE")"
printf 'replay.sh: tty=%s mode=%s fixture=%s (%s B)\n' "$TTY" "$MODE" "$FIXTURE" "$FSIZE" >&2

if [ "$MODE" = "max" ]; then
  # Max-throughput: single pass, no pacing.
  cat "$FIXTURE" > "$TTY"
  exit 0
fi

[ "$MODE" = "paced" ] || die "unknown mode '$MODE' (use paced|max)"

# Paced ~500000 B/s. Loop the fixture enough times to cover target_seconds.
target_bytes=$(( TARGET_SECONDS * BYTES_PER_SEC ))
loops=$(( (target_bytes + FSIZE - 1) / FSIZE ))   # ceil
[ "$loops" -lt 1 ] && loops=1
printf 'replay.sh: paced %d B/s, %d loop(s) -> ~%d s\n' \
  "$BYTES_PER_SEC" "$loops" "$(( loops * FSIZE / BYTES_PER_SEC ))" >&2

# Block-addressed dd (bs=CHUNK): slice index `blk` covers bytes
# [blk*CHUNK, (blk+1)*CHUNK). The final slice is naturally short.
nblk=$(( (FSIZE + CHUNK - 1) / CHUNK ))
i=0
while [ "$i" -lt "$loops" ]; do
  blk=0
  while [ "$blk" -lt "$nblk" ]; do
    dd if="$FIXTURE" bs="$CHUNK" skip="$blk" count=1 2>/dev/null > "$TTY" || true
    blk=$(( blk + 1 ))
    sleep "$INTERVAL"
  done
  i=$(( i + 1 ))
done
