#!/usr/bin/env bash
#
# rust-bundle.sh — build the Rust rewrite (crate `nice`, binary `nice-rs`) in
# release mode and assemble it into an installable "Nice RS Dev.app" bundle.
#
# NOTE ON SIGNING SCOPE — READ BEFORE "FIXING" THIS:
#   Signing here is ad-hoc (`codesign -s -`) only. That is deliberate, not an
#   oversight: R1 (this cycle) promises local installability, nothing more.
#   Notarization and release-CI wiring are Stage 8 (R27-adjacent) work — see
#   the R1 plan's "Binding technical decisions" / signing-scope note. Do not
#   add notarytool/staple/Developer ID signing to this script until that
#   stage lands; doing so early is exactly the kind of silent-reconcile this
#   plan calls out as unwanted.
#
# Produces: build-rs/Nice RS Dev.app — a self-contained bundle.
# scripts/rust-install.sh copies it into /Applications.
#
# The app identity here is deliberately distinct from both Swift installs
# (Nice.app / Nice Dev.app) so nothing collides in /Applications,
# UserDefaults, or process-name greps:
#   bundle id       dev.nickanderssohn.nice-rs-dev
#   display name    Nice RS Dev
#   executable      nice-rs
#
# Usage: scripts/rust-bundle.sh [--dest DIR]
#
#   --dest DIR   Directory to assemble the .app bundle into.
#                Default: build-rs (relative to the repo root).
#   -h, --help   Show this help.
#
# Exit codes:
#   0  bundle assembled + verified
#   1  prereq missing / bad args
#   2  build, assembly, or codesign-verify step failed
set -euo pipefail

APP_NAME="Nice RS Dev"
BUNDLE_ID="dev.nickanderssohn.nice-rs-dev"
BIN_NAME="nice-rs"
MIN_OS="14.0"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

BUNDLE_DEST="$REPO_ROOT/build-rs"

usage() {
    cat <<EOF
Usage: scripts/rust-bundle.sh [--dest DIR]

  --dest DIR   Directory to assemble the .app bundle into. Default: build-rs.
  -h, --help   Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dest)    BUNDLE_DEST="$2"; shift 2;;
        -h|--help) usage; exit 0;;
        *) printf '[rust-bundle] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

log()  { printf '[rust-bundle] %s\n' "$*"; }
fail() { printf '[rust-bundle] FAIL: %s\n' "$*" >&2; exit 2; }
need() { command -v "$1" >/dev/null 2>&1 || { printf '[rust-bundle] missing dep: %s\n' "$1" >&2; exit 1; }; }

need cargo
need codesign
need ditto

if [[ ! -d "$REPO_ROOT/vendor/zed/crates/gpui" ]]; then
    fail "vendor/zed not found — run scripts/vendor-zed.sh first"
fi

# ── 1. build release ────────────────────────────────────────────────────
# Deliberately WITHOUT the `selftest` feature: enabling it turns on gpui
# test-support, which flips CAMetalLayer.framebufferOnly = false PROCESS-
# WIDE. The shipped bundle must keep the live layer framebuffer-only. See
# crates/nice/Cargo.toml and crates/nice-harness/src/capture.rs.
log "building nice-rs (release)"
cargo build --release -p nice

SRC_BIN="$REPO_ROOT/target/release/$BIN_NAME"
[[ -x "$SRC_BIN" ]] || fail "release build finished but $SRC_BIN not found"

# ── 2. version (single source of truth: crates/nice/Cargo.toml) ────────
VERSION=$(awk -F'"' '/^version = /{print $2; exit}' "$REPO_ROOT/crates/nice/Cargo.toml")
[[ -n "$VERSION" ]] || fail "could not read version from crates/nice/Cargo.toml"

# ── 3. assemble the bundle ──────────────────────────────────────────────
APP_BUNDLE="$BUNDLE_DEST/$APP_NAME.app"
log "assembling $APP_NAME.app v$VERSION -> ${APP_BUNDLE#$REPO_ROOT/}"

rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
ditto "$SRC_BIN" "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"

cat > "$APP_BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>$BIN_NAME</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$APP_NAME</string>
	<key>CFBundleDisplayName</key>
	<string>$APP_NAME</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>$MIN_OS</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# ── 3b. app icon — the SAME source art as prod (ui-parity r6) ───────────
# Compiles Resources/AppIcon.icon (the Icon Composer package project.yml
# wires into prod Nice via CFBundleIconName) with actool, which emits both
# Assets.car (the macOS 26 glass icon) and AppIcon.icns (the classic
# fallback), and adds the matching Info.plist keys. Fails SOFT: a machine
# without Xcode's actool still assembles a valid — just iconless — bundle.
ICON_SRC="$REPO_ROOT/Resources/AppIcon.icon"
if [[ -d "$ICON_SRC" ]] && xcrun --find actool >/dev/null 2>&1; then
    ICON_TMP=$(mktemp -d)
    if xcrun actool "$ICON_SRC" --compile "$ICON_TMP" \
            --platform macosx --minimum-deployment-target "$MIN_OS" \
            --app-icon AppIcon \
            --output-partial-info-plist "$ICON_TMP/partial.plist" \
            >/dev/null 2>&1 \
        && [[ -f "$ICON_TMP/Assets.car" && -f "$ICON_TMP/AppIcon.icns" ]]; then
        ditto "$ICON_TMP/Assets.car" "$APP_BUNDLE/Contents/Resources/Assets.car"
        ditto "$ICON_TMP/AppIcon.icns" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
        # The plist above is written fresh every run, so Add never collides.
        /usr/libexec/PlistBuddy \
            -c "Add :CFBundleIconName string AppIcon" \
            -c "Add :CFBundleIconFile string AppIcon" \
            "$APP_BUNDLE/Contents/Info.plist" >/dev/null 2>&1 \
            && log "app icon compiled from Resources/AppIcon.icon" \
            || log "WARN: icon compiled but Info.plist keys failed — bundle ships without an icon"
    else
        log "WARN: actool failed on ${ICON_SRC#$REPO_ROOT/} — bundle ships without an icon"
    fi
    rm -rf "$ICON_TMP"
else
    log "WARN: actool or Resources/AppIcon.icon unavailable — bundle ships without an icon"
fi

# ── 4. ad-hoc codesign + verify ──────────────────────────────────────────
# See the header note above: ad-hoc only, deliberately. Do not add
# Developer ID / notarization here.
log "ad-hoc codesigning"
codesign --force --sign - "$APP_BUNDLE"

log "verifying signature"
codesign --verify --deep --strict "$APP_BUNDLE" \
    || fail "codesign --verify failed on $APP_BUNDLE"

log "ok — $APP_BUNDLE"
log "run directly:  \"$APP_BUNDLE/Contents/MacOS/$BIN_NAME\""
log "or install:    scripts/rust-install.sh"
