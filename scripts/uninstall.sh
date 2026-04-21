#!/usr/bin/env bash
#
# Uninstall Nice. By default removes the dev variant ('Nice Dev',
# dev.nickanderssohn.nice-dev), which is the safe target for Claude-run
# uninstalls. Pass --prod to uninstall the production 'Nice' (that's the
# user's real working install — only run with explicit authorization).
#
# By default the bundle is removed but UserDefaults are preserved. Pass
# --purge to also wipe settings.
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
PROD=0

usage() {
    cat <<EOF
Usage: scripts/uninstall.sh [--prod] [--purge] [--dest PATH]

  --prod     Uninstall the production 'Nice' build instead of 'Nice Dev'.
             Default: uninstall 'Nice Dev'.
  --purge    Also delete UserDefaults for the variant being uninstalled.
  --dest     Directory the .app was installed into. Default: /Applications.
  -h, --help Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prod)    PROD=1; shift;;
        --purge)   PURGE=1; shift;;
        --dest)    DEST="$2"; shift 2;;
        -h|--help) usage; exit 0;;
        *) printf '[uninstall] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[uninstall] %s\n' "$*"; }
fail() { printf '[uninstall] FAIL: %s\n' "$*" >&2; exit 2; }

if [[ "$PROD" -eq 1 ]]; then
    APP_NAME="Nice"
    BUNDLE_ID="dev.nickanderssohn.nice"
else
    APP_NAME="Nice Dev"
    BUNDLE_ID="dev.nickanderssohn.nice-dev"
fi

DEST_APP="$DEST/$APP_NAME.app"

# ── 1. quit running instance of THIS variant ──────────────────────────
RUNNING_PATH="/Applications/$APP_NAME.app/Contents/MacOS/$APP_NAME"
if pgrep -f "$RUNNING_PATH" >/dev/null 2>&1; then
    log "$APP_NAME is running — asking it to quit"
    osascript -e "tell application \"$APP_NAME\" to quit" >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        pgrep -f "$RUNNING_PATH" >/dev/null 2>&1 || break
        sleep 0.5
    done
    if pgrep -f "$RUNNING_PATH" >/dev/null 2>&1; then
        log "$APP_NAME did not quit cleanly — sending SIGTERM"
        pkill -f "$RUNNING_PATH" 2>/dev/null || true
        sleep 1
    fi
fi

# ── 2. best-effort login-item unregister ──────────────────────────────
osascript -e "tell application \"System Events\" to delete login item \"$APP_NAME\"" \
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
    log "no $APP_NAME.app at $DEST_APP — nothing to remove"
fi

# ── 4. optionally purge user settings ─────────────────────────────────
if [[ "$PURGE" -eq 1 ]]; then
    log "purging UserDefaults $BUNDLE_ID"
    defaults delete "$BUNDLE_ID" >/dev/null 2>&1 || true
fi

log "done"
log "note: macOS may show a stale '$APP_NAME' entry under Login Items until next login"
