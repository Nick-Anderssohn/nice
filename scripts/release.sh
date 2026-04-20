#!/usr/bin/env bash
#
# Build a signed, notarized, stapled Nice-X.Y.Z.zip for distribution via
# GitHub Releases + Homebrew cask. End-to-end: bump version → xcodegen →
# xcodebuild archive (Developer ID, manual signing) → export → codesign
# verify → ditto zip → notarize → staple → final zip → SHA256.
#
# Required env vars (read from scripts/.env.release if present locally):
#   APPLE_ID                Apple ID email (not needed with --skip-notarize)
#   APPLE_APP_PASSWORD      app-specific password from appleid.apple.com
#                           (not needed with --skip-notarize)
#   APPLE_TEAM_ID           10-char Team ID
#   APPLE_SIGNING_IDENTITY  full certificate common name, e.g.
#                             "Developer ID Application: Jane Doe (ABCDE12345)"
#
# Exit codes:
#   0  artifact produced
#   1  bad arguments / missing prerequisite
#   2  build, signing, or notarization failed

set -euo pipefail

VERSION=""
SKIP_NOTARIZE=0

usage() {
    cat <<EOF
Usage: scripts/release.sh --version X.Y.Z [--skip-notarize]

  --version         Version number (e.g. 0.1.0). Required.
  --skip-notarize   Archive + export + sign, but skip notarize/staple.
                    Useful for iterating on the pre-notarize pipeline.
  -h, --help        Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)       VERSION="$2"; shift 2;;
        --skip-notarize) SKIP_NOTARIZE=1; shift;;
        -h|--help)       usage; exit 0;;
        *) printf '[release] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[release] %s\n' "$*"; }
fail() { printf '[release] FAIL: %s\n' "$*" >&2; exit 2; }

[[ -n "$VERSION" ]] || { usage >&2; printf '[release] FAIL: --version is required\n' >&2; exit 1; }
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { printf '[release] FAIL: --version must be X.Y.Z (got: %s)\n' "$VERSION" >&2; exit 1; }

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

# Source optional local env file (gitignored) for APPLE_* vars.
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

command -v xcodegen   >/dev/null 2>&1 || fail "xcodegen not on PATH (brew install xcodegen)"
command -v xcodebuild >/dev/null 2>&1 || fail "xcodebuild not on PATH (install full Xcode)"

BUILD_DIR="$REPO_ROOT/build"
ARCHIVE_PATH="$BUILD_DIR/Nice.xcarchive"
EXPORT_DIR="$BUILD_DIR/export"
APP_PATH="$EXPORT_DIR/Nice.app"
EXPORT_OPTIONS="$BUILD_DIR/ExportOptions.plist"
ZIP_PRE="$BUILD_DIR/Nice-$VERSION.pre.zip"
ZIP_FINAL="$BUILD_DIR/Nice-$VERSION.zip"

log "version=$VERSION skip_notarize=$SKIP_NOTARIZE"

# ── 1. bump project.yml (restored on exit — the tag is the source of truth) ──
PROJECT_YML="$REPO_ROOT/project.yml"
PROJECT_YML_BACKUP="$BUILD_DIR/project.yml.orig"
mkdir -p "$BUILD_DIR"
cp "$PROJECT_YML" "$PROJECT_YML_BACKUP"
trap 'cp "$PROJECT_YML_BACKUP" "$PROJECT_YML" 2>/dev/null || true' EXIT

log "patching project.yml versions → $VERSION"
/usr/bin/sed -i '' -E "s|CFBundleShortVersionString: \".*\"|CFBundleShortVersionString: \"$VERSION\"|" "$PROJECT_YML"
/usr/bin/sed -i '' -E "s|CFBundleVersion: \".*\"|CFBundleVersion: \"$VERSION\"|"                       "$PROJECT_YML"
/usr/bin/sed -i '' -E "s|MARKETING_VERSION: \".*\"|MARKETING_VERSION: \"$VERSION\"|"                   "$PROJECT_YML"
/usr/bin/sed -i '' -E "s|CURRENT_PROJECT_VERSION: \".*\"|CURRENT_PROJECT_VERSION: \"$VERSION\"|"       "$PROJECT_YML"

# ── 2. regenerate the Xcode project from the patched YAML ────────────
log "xcodegen generate"
xcodegen generate

# ── 3. clean previous outputs in this build dir ──────────────────────
rm -rf "$ARCHIVE_PATH" "$EXPORT_DIR" "$EXPORT_OPTIONS" "$ZIP_PRE" "$ZIP_FINAL"

# ── 4. archive (Developer ID Application, manual signing) ────────────
log "xcodebuild archive"
xcodebuild \
    -project Nice.xcodeproj \
    -scheme Nice \
    -configuration Release \
    -archivePath "$ARCHIVE_PATH" \
    -derivedDataPath "$BUILD_DIR/DerivedData" \
    -destination 'generic/platform=macOS' \
    CODE_SIGN_STYLE=Manual \
    CODE_SIGN_IDENTITY="$APPLE_SIGNING_IDENTITY" \
    DEVELOPMENT_TEAM="$APPLE_TEAM_ID" \
    archive

# ── 5. export .app from the archive ──────────────────────────────────
log "generating $EXPORT_OPTIONS from template"
/usr/bin/sed "s|__TEAM_ID__|$APPLE_TEAM_ID|g" "$SCRIPT_DIR/ExportOptions.plist" > "$EXPORT_OPTIONS"

log "xcodebuild -exportArchive"
xcodebuild \
    -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$EXPORT_OPTIONS"

[[ -d "$APP_PATH" ]] || fail "exportArchive produced no bundle at $APP_PATH"

# ── 6. signing sanity check (fast; catches problems before notarize) ──
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

# ── 7. pre-notarize zip ──────────────────────────────────────────────
log "ditto zip (pre-notarize) → $ZIP_PRE"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_PRE"

# ── 8. notarize (blocking, may take minutes) ─────────────────────────
SUBMIT_LOG=$(mktemp)
trap 'cp "$PROJECT_YML_BACKUP" "$PROJECT_YML" 2>/dev/null || true; rm -f "$SUBMIT_LOG"' EXIT

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

if ! grep -qE "^ *status: Accepted" "$SUBMIT_LOG"; then
    fail "notarytool exited 0 but status was not Accepted — see output above"
fi

# ── 9. staple the ticket onto the .app (offline Gatekeeper verify) ──
log "xcrun stapler staple"
xcrun stapler staple "$APP_PATH"
xcrun stapler validate "$APP_PATH"

# ── 10. final zip (with staple baked into the bundle) ───────────────
log "ditto zip (stapled, final) → $ZIP_FINAL"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_FINAL"

# ── 11. Gatekeeper assessment (informational; authoritative test is on a clean Mac) ──
log "spctl --assess --type execute"
spctl --assess --type execute --verbose=4 "$APP_PATH" || \
    log "note: spctl assessment non-zero — verify on a Mac that hasn't run this signing identity"

# ── 12. report ───────────────────────────────────────────────────────
SHA=$(shasum -a 256 "$ZIP_FINAL" | awk '{print $1}')
log "release artifact ready:"
printf '  zip:    %s\n' "$ZIP_FINAL"
printf '  sha256: %s\n' "$SHA"

# Emit machine-readable outputs for CI.
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
        printf 'zip=%s\n'     "$ZIP_FINAL"
        printf 'sha256=%s\n'  "$SHA"
        printf 'version=%s\n' "$VERSION"
    } >> "$GITHUB_OUTPUT"
fi
