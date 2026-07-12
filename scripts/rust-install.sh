#!/usr/bin/env bash
#
# rust-install.sh — install the Rust rewrite into /Applications, in one of two
# variants that mirror the (now-retired) Swift install.sh model.
#
# Builds + assembles via scripts/rust-bundle.sh (pass --no-build to reuse an
# already-assembled bundle), then copies the result into /Applications.
#
# Two variants (choose with --prod; DEFAULT is dev):
#   default (no flag)  → Nice Dev.app (dev.nickanderssohn.nice-dev), built in
#                        ./build-rs. Safe default: rebuilding cannot touch the
#                        user's real prod sessions. A running Nice Dev is
#                        force-quit first so the next launch is the new build.
#   --prod             → Nice.app (dev.nickanderssohn.nice), built in
#                        ./build-rs-prod. The production install. A running
#                        prod Nice may host live Claude Code sessions, so this
#                        script NEVER force-quits it — the bundle is swapped in
#                        place via staged rename and the running process picks
#                        up the new version on next relaunch. Run only when the
#                        user explicitly asked to upgrade prod.
#
# Three distinct name concepts (see rust-bundle.sh header): cargo output binary
# `nice`, per-variant bundle executable name (Contents/MacOS/<name> +
# CFBundleExecutable = "Nice" / "Nice Dev"), and per-variant app name + bundle
# id. Force-quit matching keys on the EXEC_NAME, anchored so Nice.app/.../Nice
# and Nice Dev.app/.../Nice Dev never cross-match.
#
# Force-quit detection uses `ps -Aww -o pid=,args=`, never pgrep/pkill -f —
# on macOS a GUI app's `comm` is the exec path truncated to 16 chars, so
# pgrep/pkill -f can silently miss a running instance. Mirrors the approach
# the Swift install.sh used for its `Nice Dev` build (SIGTERM, poll, SIGKILL —
# no AppleScript quit, which would raise a quit-confirmation dialog and stall
# an unattended install).
#
# Usage: scripts/rust-install.sh [--prod] [--no-build] [--dest PATH]
#
#   --prod       Install the production "Nice" variant. Default: "Nice Dev".
#   --no-build   Skip the scripts/rust-bundle.sh build step; install whatever
#                is already at the variant's build dir.
#   --dest PATH  Directory to install the .app into. Default: /Applications.
#   -h, --help   Show this help.
#
# Environment:
#   NICE_PROD_SIGN_IDENTITY  With --prod, the codesign identity used to re-sign
#                            the bundle for stable TCC permissions (see step 2).
#                            Defaults to the first "Developer ID Application"
#                            identity in the keychain; set to "-" to force
#                            ad-hoc. Ignored for the dev variant.
#
# Exit codes:
#   0  installed
#   1  prereq missing / bad args
#   2  build or install step failed
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

PROD=0
NO_BUILD=0
DEST="/Applications"

usage() {
    cat <<EOF
Usage: scripts/rust-install.sh [--prod] [--no-build] [--dest PATH]

  --prod       Install the production "Nice" variant. Default: "Nice Dev".
  --no-build   Skip the build step; install the existing bundle.
  --dest PATH  Directory to install the .app into. Default: /Applications.
  -h, --help   Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prod)     PROD=1; shift;;
        --no-build) NO_BUILD=1; shift;;
        --dest)     DEST="$2"; shift 2;;
        -h|--help)  usage; exit 0;;
        *) printf '[rust-install] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[rust-install] %s\n' "$*"; }
fail() { printf '[rust-install] FAIL: %s\n' "$*" >&2; exit 2; }

# ── variant identity (mirror rust-bundle.sh) ────────────────────────────
if [[ "$PROD" -eq 1 ]]; then
    APP_NAME="Nice"
    EXEC_NAME="Nice"
    BUILD_DIR="$REPO_ROOT/build-rs-prod"
    BUNDLE_FLAG="--prod"
else
    APP_NAME="Nice Dev"
    EXEC_NAME="Nice Dev"
    BUILD_DIR="$REPO_ROOT/build-rs"
    BUNDLE_FLAG=""
fi

# ── 1. build + assemble ─────────────────────────────────────────────────
if [[ "$NO_BUILD" -eq 0 ]]; then
    log "building + assembling via scripts/rust-bundle.sh $BUNDLE_FLAG"
    # shellcheck disable=SC2086  # empty BUNDLE_FLAG must expand to no arg
    "$SCRIPT_DIR/rust-bundle.sh" $BUNDLE_FLAG --dest "$BUILD_DIR"
fi

SRC_APP="$BUILD_DIR/$APP_NAME.app"
[[ -d "$SRC_APP" ]] || fail "$SRC_APP not found — run scripts/rust-bundle.sh $BUNDLE_FLAG first (or drop --no-build)"

# ── 2. Developer ID re-sign (prod only) ─────────────────────────────────
# rust-bundle.sh signs ad-hoc. An ad-hoc signature's designated requirement
# is pinned to the binary's cdhash, which changes on every rebuild — so macOS
# treats each prod reinstall as a new identity and TCC screen-recording /
# accessibility grants go stale (you must re-authorize after every reinstall).
# Re-signing with a STABLE Developer ID identity yields a cdhash-independent
# designated requirement (team id + bundle id), so those grants persist across
# rebuilds. This is a LOCAL-CONVENIENCE re-sign only, NOT the release
# signature — release-rs.sh owns the hardened-runtime + notarize + staple pass
# for shipped builds and does not go through this script.
#
# We sign $SRC_APP (in the user-owned build dir) as the invoking user, before
# the staging copy below: a `sudo ditto` into a non-writable $DEST would sign
# as root, whose keychain does not hold the Developer ID key. `ditto`
# preserves the signature into the installed bundle.
#
# Identity resolution:
#   1. $NICE_PROD_SIGN_IDENTITY if set (set it to "-" to force ad-hoc)
#   2. else the first "Developer ID Application" identity in the keychain
#   3. else keep the existing ad-hoc signature (warn) — a machine without the
#      cert (fresh clone / CI) still installs fine, just without TCC persistence.
if [[ "$PROD" -eq 1 ]]; then
    sign_id="${NICE_PROD_SIGN_IDENTITY:-}"
    if [[ -z "$sign_id" ]]; then
        sign_id=$(security find-identity -v -p codesigning 2>/dev/null \
            | awk -F'"' '/Developer ID Application/ {print $2; exit}')
    fi
    if [[ -z "$sign_id" || "$sign_id" == "-" ]]; then
        log "prod: no Developer ID identity — keeping ad-hoc signature (TCC grants will NOT persist across rebuilds)"
    else
        log "prod: re-signing with stable identity: $sign_id"
        codesign --force --sign "$sign_id" "$SRC_APP" \
            || fail "Developer ID codesign failed (identity: $sign_id)"
        codesign --verify --deep --strict "$SRC_APP" \
            || fail "codesign --verify failed after Developer ID re-sign"
    fi
fi

# ── 3. force-quit a running instance of THIS bundle (dev only) ──────────
# CRITICAL SAFETY: for --prod we NEVER force-quit. A running prod Nice may
# host live Claude Code sessions; the staged-swap below upgrades the bundle
# on disk and the running process picks up the new version on next relaunch.
# The dev variant IS force-quit so its next launch is the new build.
#
# Matching is path-scoped to the installed bundle's own executable path and
# anchored on EXEC_NAME so it only matches a binary launched FROM this
# variant's .app — "Nice Dev.app/Contents/MacOS/Nice Dev" — and never a prod
# "Nice.app/Contents/MacOS/Nice", nor an ad-hoc `cargo run -p nice` /
# `target/release/nice` dev session a developer may be running side-by-side.
nice_pids() {
    ps -Aww -o pid=,args= \
        | grep -E "$APP_NAME"'\.app/Contents/MacOS/'"$EXEC_NAME"'( |$)' \
        | awk '{print $1}' || true
}

if [[ "$PROD" -eq 0 ]]; then
    pids="$(nice_pids)"
    if [[ -n "$pids" ]]; then
        log "$APP_NAME is running (pid(s): $(echo "$pids" | tr '\n' ' ')) — force-quitting"
        # shellcheck disable=SC2086  # word-splitting the pid list is intended
        kill $pids 2>/dev/null || true
        for _ in 1 2 3 4 5 6; do
            [[ -z "$(nice_pids)" ]] && break
            sleep 0.5
        done
        pids="$(nice_pids)"
        if [[ -n "$pids" ]]; then
            log "$APP_NAME survived SIGTERM — sending SIGKILL"
            # shellcheck disable=SC2086
            kill -9 $pids 2>/dev/null || true
            sleep 0.5
        fi
    fi
fi

# ── 4. install via staging path + atomic rename ─────────────────────────
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

# ── 5. report ─────────────────────────────────────────────────────────
VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
    "$DEST_APP/Contents/Info.plist" 2>/dev/null || echo "?")
log "installed $APP_NAME $VERSION at $DEST_APP"
log "launch with:  open -a \"$APP_NAME\""
