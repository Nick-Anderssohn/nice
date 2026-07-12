#!/usr/bin/env bash
#
# release-rs.sh — build a signed, notarized, stapled Nice-X.Y.Z.zip of the
# Rust rewrite's PRODUCTION build ("Nice.app", dev.nickanderssohn.nice).
#
# Pipeline: version guard vs crates/nice/Cargo.toml → scripts/rust-bundle.sh
# --prod --universal (per-arch release cargo builds + lipo into a universal
# arm64+x86_64 binary + prod bundle assembly; its ad-hoc signature is
# deliberately overwritten here) → Developer ID codesign with hardened runtime
# → verify → ditto zip → notarize → staple → final zip → SHA256.
#
# Releases ship UNIVERSAL (Apple Silicon + Intel). codesign/notarize/staple all
# operate on the fat binary as a unit, so nothing downstream of the build needs
# to know about the second arch.
#
# This builds the prod "Nice.app" identity via rust-bundle.sh --prod (built in
# ./build-rs-prod so it never collides with the dev ./build-rs bundle).
#
# Required env vars (read from scripts/.env.release if present locally):
#   APPLE_ID                Apple ID email (not needed with --skip-notarize)
#   APPLE_APP_PASSWORD      app-specific password (not needed with --skip-notarize)
#   APPLE_TEAM_ID           10-char Team ID
#   APPLE_SIGNING_IDENTITY  full certificate common name
#
# Exit codes: 0 artifact produced / 1 bad args or prereq / 2 build, signing,
# or notarization failed
set -euo pipefail

VERSION=""
SKIP_NOTARIZE=0

usage() {
    cat <<EOF
Usage: scripts/release-rs.sh --version X.Y.Z [--skip-notarize]

  --version         Version number (e.g. 0.1.0). Required; must equal the
                    version in crates/nice/Cargo.toml.
  --skip-notarize   Build + sign, but skip notarize/staple (pipeline iteration).
  -h, --help        Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)       VERSION="$2"; shift 2;;
        --skip-notarize) SKIP_NOTARIZE=1; shift;;
        -h|--help)       usage; exit 0;;
        *) printf '[release-rs] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[release-rs] %s\n' "$*"; }
fail() { printf '[release-rs] FAIL: %s\n' "$*" >&2; exit 2; }

[[ -n "$VERSION" ]] || { usage >&2; printf '[release-rs] FAIL: --version is required\n' >&2; exit 1; }
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { printf '[release-rs] FAIL: --version must be X.Y.Z (got: %s)\n' "$VERSION" >&2; exit 1; }

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

if [[ -f "$SCRIPT_DIR/.env.release" ]]; then
    log "sourcing scripts/.env.release"
    set -a; . "$SCRIPT_DIR/.env.release"; set +a
fi

: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"
if [[ "$SKIP_NOTARIZE" -eq 0 ]]; then
    : "${APPLE_ID:?APPLE_ID is required (or pass --skip-notarize)}"
    : "${APPLE_APP_PASSWORD:?APPLE_APP_PASSWORD is required (or pass --skip-notarize)}"
fi

# ── 1. version guard: the tag is cut AFTER the Cargo.toml bump lands ────
CARGO_VERSION=$(awk -F'"' '/^version = /{print $2; exit}' "$REPO_ROOT/crates/nice/Cargo.toml")
if [[ "$CARGO_VERSION" != "$VERSION" ]]; then
    fail "crates/nice/Cargo.toml is at version=\"$CARGO_VERSION\" but --version=$VERSION; commit the version bump before tagging"
fi

BUILD_DIR="$REPO_ROOT/build-rs-prod"
APP_PATH="$BUILD_DIR/Nice.app"
ZIP_PRE="$BUILD_DIR/Nice-$VERSION.pre.zip"
ZIP_FINAL="$BUILD_DIR/Nice-$VERSION.zip"

log "version=$VERSION skip_notarize=$SKIP_NOTARIZE"
rm -f "$ZIP_PRE" "$ZIP_FINAL"

# ── 2. vendor + build + assemble (rust-bundle.sh --prod ad-hoc signs; step 3 re-signs) ──
# --universal: ship a fat arm64+x86_64 binary so releases run on both Apple
# Silicon and Intel (rust-bundle.sh cross-compiles the non-host slice + lipos).
[[ -d "$REPO_ROOT/vendor/zed/crates/gpui" ]] || scripts/vendor-zed.sh
scripts/rust-bundle.sh --prod --universal --dest "$BUILD_DIR"
[[ -d "$APP_PATH" ]] || fail "rust-bundle.sh --prod produced no bundle at $APP_PATH"

# ── 3. Developer ID re-sign with hardened runtime (required by notarization) ──
log "codesigning with Developer ID (hardened runtime + timestamp)"
codesign --force --options runtime --timestamp \
    --sign "$APPLE_SIGNING_IDENTITY" "$APP_PATH"

log "codesign --verify --deep --strict"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

if [[ "$SKIP_NOTARIZE" -eq 1 ]]; then
    log "ditto zip (unstapled) → $ZIP_FINAL"
    ditto -c -k --keepParent "$APP_PATH" "$ZIP_FINAL"
    SHA=$(shasum -a 256 "$ZIP_FINAL" | awk '{print $1}')
    log "done (notarization skipped)"
    printf '  zip:    %s\n  sha256: %s\n' "$ZIP_FINAL" "$SHA"
    exit 0
fi

# ── 4. pre-notarize zip ──────────────────────────────────────────────
log "ditto zip (pre-notarize) → $ZIP_PRE"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_PRE"

# ── 5. notarize (blocking) ───────────────────────────────────────────
SUBMIT_LOG=$(mktemp)
trap 'rm -f "$SUBMIT_LOG"' EXIT

log "xcrun notarytool submit --wait"
if ! xcrun notarytool submit "$ZIP_PRE" \
        --apple-id "$APPLE_ID" \
        --password "$APPLE_APP_PASSWORD" \
        --team-id "$APPLE_TEAM_ID" \
        --wait 2>&1 | tee "$SUBMIT_LOG"; then
    SUBMIT_ID=$(awk -F': *' '/^  *id:/{print $2; exit}' "$SUBMIT_LOG")
    if [[ -n "$SUBMIT_ID" ]]; then
        log "fetching notarization log for $SUBMIT_ID"
        xcrun notarytool log "$SUBMIT_ID" \
            --apple-id "$APPLE_ID" \
            --password "$APPLE_APP_PASSWORD" \
            --team-id "$APPLE_TEAM_ID" || true
    fi
    fail "notarization failed"
fi

grep -qE "^ *status: Accepted" "$SUBMIT_LOG" \
    || fail "notarytool exited 0 but status was not Accepted — see output above"

# ── 6. staple + final zip ────────────────────────────────────────────
log "xcrun stapler staple"
xcrun stapler staple "$APP_PATH"
xcrun stapler validate "$APP_PATH"

log "ditto zip (stapled, final) → $ZIP_FINAL"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_FINAL"

log "spctl --assess --type execute"
spctl --assess --type execute --verbose=4 "$APP_PATH" || \
    log "note: spctl assessment non-zero — verify on a Mac that hasn't run this signing identity"

# ── 7. report ────────────────────────────────────────────────────────
SHA=$(shasum -a 256 "$ZIP_FINAL" | awk '{print $1}')
log "release artifact ready:"
printf '  zip:    %s\n' "$ZIP_FINAL"
printf '  sha256: %s\n' "$SHA"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
        printf 'zip=%s\n'     "$ZIP_FINAL"
        printf 'sha256=%s\n'  "$SHA"
        printf 'version=%s\n' "$VERSION"
    } >> "$GITHUB_OUTPUT"
fi
