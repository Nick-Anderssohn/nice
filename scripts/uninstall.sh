#!/usr/bin/env bash
#
# Uninstall Nice.app. By default removes the bundle but preserves user
# settings (UserDefaults under dev.nickanderssohn.nice). Pass --purge to
# also wipe settings.
#
# macOS owns the SMAppService "open at login" registration in a system
# daemon — there's no clean CLI to flip it from outside the app. We make
# a best-effort attempt; a stale entry may linger in System Settings →
# General → Login Items until next login. Cosmetic only.
#
# Exit codes:
#   0  uninstalled (or already absent)
#   2  uninstall step failed

set -euo pipefail

DEST="/Applications"
PURGE=0

usage() {
    cat <<EOF
Usage: scripts/uninstall.sh [--purge] [--dest PATH]

  --purge    Also delete UserDefaults for dev.nickanderssohn.nice.
  --dest     Directory Nice.app was installed into. Default: /Applications.
  -h, --help Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge)   PURGE=1; shift;;
        --dest)    DEST="$2"; shift 2;;
        -h|--help) usage; exit 0;;
        *) printf '[uninstall] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[uninstall] %s\n' "$*"; }
fail() { printf '[uninstall] FAIL: %s\n' "$*" >&2; exit 2; }

DEST_APP="$DEST/Nice.app"

# ── 1. quit running instance ──────────────────────────────────────────
if pgrep -x Nice >/dev/null 2>&1; then
    log "Nice is running — asking it to quit"
    osascript -e 'tell application "Nice" to quit' >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        pgrep -x Nice >/dev/null 2>&1 || break
        sleep 0.5
    done
    if pgrep -x Nice >/dev/null 2>&1; then
        log "Nice did not quit cleanly — sending SIGTERM"
        pkill -x Nice 2>/dev/null || true
        sleep 1
    fi
fi

# ── 2. best-effort login-item unregister ──────────────────────────────
osascript -e 'tell application "System Events" to delete login item "Nice"' \
    >/dev/null 2>&1 || true

# ── 3. remove the bundle ──────────────────────────────────────────────
if [[ -e "$DEST_APP" ]]; then
    SUDO=""
    if [[ ! -w "$DEST" ]]; then
        SUDO="sudo"
        log "$DEST is not writable as $(id -un) — sudo required"
    fi
    log "removing $DEST_APP"
    $SUDO rm -rf "$DEST_APP"
else
    log "no Nice.app at $DEST_APP — nothing to remove"
fi

# ── 4. optionally purge user settings ─────────────────────────────────
if [[ "$PURGE" -eq 1 ]]; then
    log "purging UserDefaults dev.nickanderssohn.nice"
    defaults delete dev.nickanderssohn.nice >/dev/null 2>&1 || true
fi

log "done"
log "note: macOS may show a stale 'Nice' entry under Login Items until next login"
