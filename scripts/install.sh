#!/usr/bin/env bash
#
# Build Nice from source and install Nice.app into /Applications so it
# behaves like any other Mac app — Spotlight, Launchpad, Dock, login items.
#
# Idempotent: re-running upgrades the install in place. If Nice is running
# it is asked to quit first; user state lives in UserDefaults outside the
# bundle, so settings survive an upgrade.
#
# Requires: Xcode (full IDE, not just Command Line Tools), xcodegen,
# macOS 14+. xcodegen can be installed via `brew install xcodegen` or any
# other route that puts it on PATH.
#
# Exit codes:
#   0  installed
#   1  prereq missing
#   2  build or install step failed

set -euo pipefail

CONFIGURATION="Release"
DEST="/Applications"

usage() {
    cat <<EOF
Usage: scripts/install.sh [--configuration Debug|Release] [--dest PATH]

  --configuration  Build configuration. Default: Release.
  --dest           Directory to install Nice.app into. Default: /Applications.
  -h, --help       Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --configuration) CONFIGURATION="$2"; shift 2;;
        --dest)          DEST="$2"; shift 2;;
        -h|--help)       usage; exit 0;;
        *) printf '[install] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[install] %s\n' "$*"; }
fail() { printf '[install] FAIL: %s\n' "$*" >&2; exit 2; }
need() { command -v "$1" >/dev/null 2>&1 || { printf '[install] missing dep: %s\n' "$1" >&2; exit 1; }; }

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

# ── 0. prereqs ────────────────────────────────────────────────────────
need xcodegen
# `xcodebuild -version` only succeeds when a full Xcode (not just CLT) is
# the active developer dir — perfect proxy for both checks.
if ! xcodebuild -version >/dev/null 2>&1; then
    printf '[install] FAIL: xcodebuild not available — install full Xcode from\n' >&2
    printf '         the App Store, then:\n' >&2
    printf '           sudo xcode-select -s /Applications/Xcode.app/Contents/Developer\n' >&2
    printf '           sudo xcodebuild -license accept\n' >&2
    exit 1
fi
os_major=$(sw_vers -productVersion | cut -d. -f1)
[[ "$os_major" -ge 14 ]] || fail "macOS 14+ required (have $(sw_vers -productVersion))"

case "$CONFIGURATION" in
    Debug|Release) ;;
    *) fail "--configuration must be Debug or Release (got: $CONFIGURATION)";;
esac

log "configuration=$CONFIGURATION dest=$DEST"

# ── 1. generate Xcode project ─────────────────────────────────────────
log "generating Xcode project via xcodegen"
xcodegen generate

# ── 2. build into a deterministic path ────────────────────────────────
BUILD_DIR="$REPO_ROOT/build"
SRC_APP="$BUILD_DIR/Build/Products/$CONFIGURATION/Nice.app"

log "building Nice ($CONFIGURATION) — output to ./build"
xcodebuild \
    -project Nice.xcodeproj \
    -scheme Nice \
    -configuration "$CONFIGURATION" \
    -derivedDataPath "$BUILD_DIR" \
    -destination 'platform=macOS' \
    CODE_SIGN_IDENTITY='-' \
    build

[[ -d "$SRC_APP" ]] || fail "build finished but $SRC_APP not found"

# ── 3. quit running instance, if any ──────────────────────────────────
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

# ── 4. install (with sudo only if needed) ─────────────────────────────
SUDO=""
if [[ ! -w "$DEST" ]]; then
    SUDO="sudo"
    log "$DEST is not writable as $(id -un) — sudo required"
fi

DEST_APP="$DEST/Nice.app"
if [[ -e "$DEST_APP" ]]; then
    log "removing existing $DEST_APP"
    $SUDO rm -rf "$DEST_APP"
fi

log "copying Nice.app → $DEST_APP"
$SUDO ditto "$SRC_APP" "$DEST_APP"
$SUDO xattr -dr com.apple.quarantine "$DEST_APP" 2>/dev/null || true

# ── 5. report ─────────────────────────────────────────────────────────
VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
    "$DEST_APP/Contents/Info.plist" 2>/dev/null || echo "?")
log "installed Nice $VERSION at $DEST_APP"
log "launch with:  open -a Nice"
