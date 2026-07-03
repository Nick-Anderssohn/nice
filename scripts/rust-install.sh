#!/usr/bin/env bash
#
# rust-install.sh — install the Rust rewrite's dev build into /Applications.
#
# Builds + assembles via scripts/rust-bundle.sh (pass --no-build to reuse an
# already-assembled build-rs/Nice RS Dev.app), force-quits any running
# `nice-rs` instance of THIS bundle, then copies the result to
# /Applications/Nice RS Dev.app.
#
# SAFETY: this script knows how to install exactly one thing — the Rust
# rewrite's dev build, "Nice RS Dev.app" / dev.nickanderssohn.nice-rs-dev.
# It never touches, quits, or even names /Applications/Nice.app (Swift prod)
# or /Applications/Nice Dev.app (Swift dev) — there is no flag that points
# this script at either of those paths. Do not add one; the whole point of
# the Rust rewrite's distinct app identity is that its tooling can't collide
# with the Swift builds' tooling.
#
# Force-quit detection uses `ps -Aww -o pid=,args=`, never pgrep/pkill -f —
# on macOS a GUI app's `comm` is the exec path truncated to 16 chars, so
# pgrep/pkill -f can silently miss a running instance. Mirrors the approach
# scripts/install.sh uses for the Swift `Nice Dev` build as of commit
# 2c08c51 (SIGTERM, poll, SIGKILL — no AppleScript quit, which would raise a
# quit-confirmation dialog and stall an unattended install).
#
# Usage: scripts/rust-install.sh [--no-build] [--dest PATH]
#
#   --no-build   Skip the scripts/rust-bundle.sh build step; install whatever
#                is already at build-rs/Nice RS Dev.app.
#   --dest PATH  Directory to install the .app into. Default: /Applications.
#   -h, --help   Show this help.
#
# Exit codes:
#   0  installed
#   1  prereq missing / bad args
#   2  build or install step failed
set -euo pipefail

APP_NAME="Nice RS Dev"
BIN_NAME="nice-rs"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

NO_BUILD=0
DEST="/Applications"

usage() {
    cat <<EOF
Usage: scripts/rust-install.sh [--no-build] [--dest PATH]

  --no-build   Skip the build step; install the existing build-rs bundle.
  --dest PATH  Directory to install the .app into. Default: /Applications.
  -h, --help   Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) NO_BUILD=1; shift;;
        --dest)     DEST="$2"; shift 2;;
        -h|--help)  usage; exit 0;;
        *) printf '[rust-install] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[rust-install] %s\n' "$*"; }
fail() { printf '[rust-install] FAIL: %s\n' "$*" >&2; exit 2; }

# ── 1. build + assemble ─────────────────────────────────────────────────
if [[ "$NO_BUILD" -eq 0 ]]; then
    log "building + assembling via scripts/rust-bundle.sh"
    "$SCRIPT_DIR/rust-bundle.sh"
fi

SRC_APP="$REPO_ROOT/build-rs/$APP_NAME.app"
[[ -d "$SRC_APP" ]] || fail "$SRC_APP not found — run scripts/rust-bundle.sh first (or drop --no-build)"

# ── 2. force-quit a running instance of THIS bundle ─────────────────────
# Path-scoped to the installed bundle's own executable path so this only
# ever matches a `nice-rs` launched FROM "Nice RS Dev.app" (the thing we're
# about to overwrite) — not an ad-hoc `cargo run -p nice` / `target/release/
# nice-rs` dev session elsewhere, which a developer may intentionally be
# running side-by-side while testing an install.
nice_rs_pids() {
    ps -Aww -o pid=,args= \
        | grep -E "$APP_NAME"'\.app/Contents/MacOS/'"$BIN_NAME"'( |$)' \
        | awk '{print $1}' || true
}

pids="$(nice_rs_pids)"
if [[ -n "$pids" ]]; then
    log "$APP_NAME is running (pid(s): $(echo "$pids" | tr '\n' ' ')) — force-quitting"
    # shellcheck disable=SC2086  # word-splitting the pid list is intended
    kill $pids 2>/dev/null || true
    for _ in 1 2 3 4 5 6; do
        [[ -z "$(nice_rs_pids)" ]] && break
        sleep 0.5
    done
    pids="$(nice_rs_pids)"
    if [[ -n "$pids" ]]; then
        log "$APP_NAME survived SIGTERM — sending SIGKILL"
        # shellcheck disable=SC2086
        kill -9 $pids 2>/dev/null || true
        sleep 0.5
    fi
fi

# ── 3. install via staging path + atomic rename ─────────────────────────
# Same staged-swap shape as scripts/install.sh: stage the new bundle next
# to the target, then a single `mv` swap so $DEST_APP is never observed
# half-populated.
SUDO=""
if [[ ! -w "$DEST" ]]; then
    SUDO="sudo"
    log "$DEST is not writable as $(id -un) — sudo required"
fi

DEST_APP="$DEST/$APP_NAME.app"
STAGING_APP="$DEST/.$APP_NAME.new.$$"
TRASH_APP="$DEST/.$APP_NAME.old.$$"

$SUDO rm -rf "$STAGING_APP" "$TRASH_APP"

log "staging $APP_NAME.app -> $STAGING_APP"
$SUDO ditto "$SRC_APP" "$STAGING_APP"

if [[ -e "$DEST_APP" ]]; then
    log "swapping bundle at $DEST_APP"
    $SUDO mv "$DEST_APP" "$TRASH_APP"
    $SUDO mv "$STAGING_APP" "$DEST_APP"
    $SUDO rm -rf "$TRASH_APP"
else
    log "installing bundle at $DEST_APP"
    $SUDO mv "$STAGING_APP" "$DEST_APP"
fi

# ── 4. report ─────────────────────────────────────────────────────────
VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
    "$DEST_APP/Contents/Info.plist" 2>/dev/null || echo "?")
log "installed $APP_NAME $VERSION at $DEST_APP"
log "launch with:  open -a \"$APP_NAME\""
