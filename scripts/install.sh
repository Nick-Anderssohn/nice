#!/usr/bin/env bash
#
# Build Nice from source and install it into /Applications so it behaves
# like any other Mac app — Spotlight, Launchpad, Dock, login items.
#
# Two variants:
#   default (no flag)  → `Nice Dev` (dev.nickanderssohn.nice-dev) into
#                        /Applications/Nice Dev.app, built in ./build-dev.
#                        Fully isolated from the user's real Nice: its
#                        own UserDefaults domain, its own Application
#                        Support folder, its own install bundle. Safe
#                        default — rebuilding does not touch the user's
#                        live `/Applications/Nice.app` sessions.
#
#   --prod             → `Nice` (dev.nickanderssohn.nice) into
#                        /Applications/Nice.app, built in ./build. The
#                        production install, used for real work. This
#                        quits the user's running Nice and replaces the
#                        bundle — run only when the user explicitly
#                        asked to upgrade prod.
#
# Idempotent: re-running upgrades in place. If the variant being
# installed is running it is asked to quit first; user state lives in
# UserDefaults outside the bundle, so settings survive an upgrade.
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
PROD=0

usage() {
    cat <<EOF
Usage: scripts/install.sh [--prod] [--configuration Debug|Release] [--dest PATH]

  --prod           Install the production 'Nice' build instead of 'Nice Dev'.
                   Default: install 'Nice Dev' alongside any existing 'Nice'.
  --configuration  Build configuration. Default: Release.
  --dest           Directory to install the .app into. Default: /Applications.
  -h, --help       Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prod)          PROD=1; shift;;
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

if [[ "$PROD" -eq 1 ]]; then
    APP_NAME="Nice"
    BUNDLE_ID="dev.nickanderssohn.nice"
    BUILD_DIR="$REPO_ROOT/build"
else
    APP_NAME="Nice Dev"
    BUNDLE_ID="dev.nickanderssohn.nice-dev"
    BUILD_DIR="$REPO_ROOT/build-dev"
fi

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

log "variant=\"$APP_NAME\" bundle=$BUNDLE_ID configuration=$CONFIGURATION dest=$DEST"

# ── 1. patch project.yml for dev (scoped to the Nice target only) ─────
# We sed-patch project.yml rather than pass `PRODUCT_NAME=…
# PRODUCT_BUNDLE_IDENTIFIER=…` on the xcodebuild command line because CLI
# overrides apply to every target in the build — including the SwiftTerm
# package dependency, which breaks its resource bundle. The anchored
# patterns below only match the Nice target's lines (the UITests /
# UnitTests targets have `dev.nickanderssohn.nice.uitests` etc. so the
# end-of-line anchor skips them).
PROJECT_YML="$REPO_ROOT/project.yml"
if [[ "$PROD" -ne 1 ]]; then
    PROJECT_YML_BACKUP=$(mktemp -t nice-install-project-yml)
    cp "$PROJECT_YML" "$PROJECT_YML_BACKUP"
    trap 'cp "$PROJECT_YML_BACKUP" "$PROJECT_YML" 2>/dev/null || true; rm -f "$PROJECT_YML_BACKUP"' EXIT
    log "patching project.yml → $APP_NAME / $BUNDLE_ID"
    /usr/bin/sed -i '' -E \
        "s|^( *PRODUCT_BUNDLE_IDENTIFIER: dev\.nickanderssohn\.nice)\$|\\1-dev|" \
        "$PROJECT_YML"
    /usr/bin/sed -i '' -E \
        "s|^( *)PRODUCT_NAME: Nice\$|\\1PRODUCT_NAME: \"Nice Dev\"|" \
        "$PROJECT_YML"
fi

# ── 2. generate Xcode project ─────────────────────────────────────────
log "generating Xcode project via xcodegen"
xcodegen generate

# ── 3. build into a deterministic path ────────────────────────────────
SRC_APP="$BUILD_DIR/Build/Products/$CONFIGURATION/$APP_NAME.app"

log "building $APP_NAME ($CONFIGURATION) — output to ${BUILD_DIR#$REPO_ROOT/}"
xcodebuild \
    -project Nice.xcodeproj \
    -scheme Nice \
    -configuration "$CONFIGURATION" \
    -derivedDataPath "$BUILD_DIR" \
    -destination 'platform=macOS' \
    CODE_SIGN_IDENTITY='-' \
    build

[[ -d "$SRC_APP" ]] || fail "build finished but $SRC_APP not found"

# ── 4. quit running instance of THIS variant, if any ──────────────────
# Path-based match so we only target the variant being installed —
# installing 'Nice Dev' must never quit a running prod 'Nice', and
# vice versa.
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

# ── 5. install (with sudo only if needed) ─────────────────────────────
SUDO=""
if [[ ! -w "$DEST" ]]; then
    SUDO="sudo"
    log "$DEST is not writable as $(id -un) — sudo required"
fi

DEST_APP="$DEST/$APP_NAME.app"
if [[ -e "$DEST_APP" ]]; then
    log "removing existing $DEST_APP"
    $SUDO rm -rf "$DEST_APP"
fi

log "copying $APP_NAME.app → $DEST_APP"
$SUDO ditto "$SRC_APP" "$DEST_APP"

# ── 6. report ─────────────────────────────────────────────────────────
VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
    "$DEST_APP/Contents/Info.plist" 2>/dev/null || echo "?")
log "installed $APP_NAME $VERSION at $DEST_APP"
log "launch with:  open -a \"$APP_NAME\""
