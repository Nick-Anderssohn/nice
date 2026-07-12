#!/usr/bin/env bash
#
# rust-bundle.sh — build the Rust rewrite (crate `nice`, cargo binary `nice`)
# in release mode and assemble it into an installable .app bundle, in one of
# two variants that mirror the (now-retired) Swift install.sh model.
#
# NOTE ON SIGNING SCOPE — READ BEFORE "FIXING" THIS:
#   Signing here is ad-hoc (`codesign -s -`) only. That is deliberate, not an
#   oversight: R1 (this cycle) promises local installability, nothing more.
#   Notarization and release-CI wiring are release-rs.sh's job (Developer ID
#   re-sign + notarytool + staple, overriding this ad-hoc signature). Do not
#   add notarytool/staple/Developer ID signing to this script.
#
# Two variants (choose with --prod; DEFAULT is dev):
#   default (no flag)  → Nice Dev.app, built in ./build-rs
#                          bundle id   dev.nickanderssohn.nice-dev
#                          app name    Nice Dev
#                          executable  Nice Dev   (Contents/MacOS/Nice Dev)
#   --prod             → Nice.app, built in ./build-rs-prod
#                          bundle id   dev.nickanderssohn.nice
#                          app name    Nice
#                          executable  Nice       (Contents/MacOS/Nice)
#
# THREE distinct name concepts, kept separate on purpose:
#   * cargo output binary — always `nice` (target/release/nice); the crate's
#     [[bin]] name. `cargo build --release -p nice` builds it for BOTH variants.
#   * bundle executable name (Contents/MacOS/<name> filename AND
#     CFBundleExecutable) — per-variant "Nice" / "Nice Dev". The single cargo
#     binary is copied to this per-variant name; they must match each other.
#   * app/bundle name (.app dir + CFBundleName/CFBundleDisplayName) and bundle
#     id — per-variant as above.
#
# scripts/rust-install.sh copies the assembled bundle into /Applications.
#
# Usage: scripts/rust-bundle.sh [--prod] [--dest DIR] [--universal]
#
#   --prod       Build the production "Nice" variant. Default: "Nice Dev".
#   --dest DIR   Directory to assemble the .app bundle into.
#                Default: build-rs (dev) / build-rs-prod (--prod).
#   --universal  Build a universal (arm64 + x86_64) binary via two per-target
#                cargo builds + `lipo`. Default OFF: a plain host-arch build,
#                which keeps dev installs fast. release-rs.sh passes this so
#                shipped releases run on both Apple Silicon and Intel.
#   -h, --help   Show this help.
#
# Exit codes:
#   0  bundle assembled + verified
#   1  prereq missing / bad args
#   2  build, assembly, or codesign-verify step failed
set -euo pipefail

MIN_OS="14.0"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

PROD=0
BUNDLE_DEST=""

usage() {
    cat <<EOF
Usage: scripts/rust-bundle.sh [--prod] [--dest DIR] [--universal]

  --prod       Build the production "Nice" variant. Default: "Nice Dev".
  --dest DIR   Directory to assemble the .app bundle into.
               Default: build-rs (dev) / build-rs-prod (--prod).
  --universal  Build a universal (arm64 + x86_64) binary via lipo.
               Default OFF (host-arch build; keeps dev installs fast).
  -h, --help   Show this help.
EOF
}

UNIVERSAL=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prod)      PROD=1; shift;;
        --dest)      BUNDLE_DEST="$2"; shift 2;;
        --universal) UNIVERSAL=1; shift;;
        -h|--help)   usage; exit 0;;
        *) printf '[rust-bundle] unknown arg: %s\n' "$1" >&2; usage >&2; exit 1;;
    esac
done

# ── variant identity ────────────────────────────────────────────────────
# APP_NAME  = .app dir + CFBundleName/CFBundleDisplayName
# EXEC_NAME = Contents/MacOS/<name> filename + CFBundleExecutable (may have a
#             space — quote everywhere)
# BUNDLE_ID = CFBundleIdentifier
if [[ "$PROD" -eq 1 ]]; then
    APP_NAME="Nice"
    EXEC_NAME="Nice"
    BUNDLE_ID="dev.nickanderssohn.nice"
    DEFAULT_DEST="$REPO_ROOT/build-rs-prod"
else
    APP_NAME="Nice Dev"
    EXEC_NAME="Nice Dev"
    BUNDLE_ID="dev.nickanderssohn.nice-dev"
    DEFAULT_DEST="$REPO_ROOT/build-rs"
fi
[[ -n "$BUNDLE_DEST" ]] || BUNDLE_DEST="$DEFAULT_DEST"

log()  { printf '[rust-bundle] %s\n' "$*"; }
fail() { printf '[rust-bundle] FAIL: %s\n' "$*" >&2; exit 2; }
need() { command -v "$1" >/dev/null 2>&1 || { printf '[rust-bundle] missing dep: %s\n' "$1" >&2; exit 1; }; }

need cargo
need codesign
need ditto
need git

# Ensure the pinned + patched GPUI/zed checkout the workspace path-depends
# on is present and at the correct revision. vendor-zed.sh is idempotent: a
# fast no-op (a few git plumbing checks, no network) when vendor/zed is
# already at the pin — so it materializes a fresh worktree AND re-syncs a
# stale pin, rather than failing the build and making the caller run it.
log "ensuring vendored zed is present + at the pin (scripts/vendor-zed.sh)"
"$SCRIPT_DIR/vendor-zed.sh" || fail "vendor-zed.sh failed — see its output above"
[[ -d "$REPO_ROOT/vendor/zed/crates/gpui" ]] \
    || fail "vendor/zed still missing after vendor-zed.sh"

# ── 1. build release ────────────────────────────────────────────────────
# Deliberately WITHOUT the `selftest` feature: enabling it turns on gpui
# test-support, which flips CAMetalLayer.framebufferOnly = false PROCESS-
# WIDE. The shipped bundle must keep the live layer framebuffer-only. See
# crates/nice/Cargo.toml and crates/nice-harness/src/capture.rs.
#
# The resulting SRC_BIN is copied to a per-variant exec name in step 3.
if [[ "$UNIVERSAL" -eq 1 ]]; then
    # Universal build: one cargo build per arch (cross-compiling the non-host
    # slice — no emulation needed to BUILD), then lipo them into one fat binary.
    # Both slices are Developer-ID re-signable as a unit downstream.
    need lipo
    need rustup
    ARM_TARGET="aarch64-apple-darwin"
    X86_TARGET="x86_64-apple-darwin"
    for t in "$ARM_TARGET" "$X86_TARGET"; do
        if ! rustup target list --installed | grep -qx "$t"; then
            log "installing rust target $t"
            rustup target add "$t" || fail "could not add rust target $t"
        fi
    done
    log "building nice (release, universal: $ARM_TARGET + $X86_TARGET)"
    cargo build --release -p nice --target "$ARM_TARGET"
    cargo build --release -p nice --target "$X86_TARGET"
    ARM_BIN="$REPO_ROOT/target/$ARM_TARGET/release/nice"
    X86_BIN="$REPO_ROOT/target/$X86_TARGET/release/nice"
    [[ -x "$ARM_BIN" ]] || fail "arm64 build finished but $ARM_BIN not found"
    [[ -x "$X86_BIN" ]] || fail "x86_64 build finished but $X86_BIN not found"
    SRC_BIN="$REPO_ROOT/target/nice-universal"
    log "lipo -create → $SRC_BIN"
    lipo -create -output "$SRC_BIN" "$ARM_BIN" "$X86_BIN" || fail "lipo failed"
    log "lipo -archs: $(lipo -archs "$SRC_BIN")"
else
    log "building nice (release, host arch)"
    cargo build --release -p nice
    # Single cargo binary; copied to a per-variant exec name below.
    SRC_BIN="$REPO_ROOT/target/release/nice"
fi
[[ -x "$SRC_BIN" ]] || fail "release build finished but $SRC_BIN not found"

# ── 2. version (single source of truth: crates/nice/Cargo.toml) ────────
VERSION=$(awk -F'"' '/^version = /{print $2; exit}' "$REPO_ROOT/crates/nice/Cargo.toml")
[[ -n "$VERSION" ]] || fail "could not read version from crates/nice/Cargo.toml"

# ── 3. assemble the bundle ──────────────────────────────────────────────
APP_BUNDLE="$BUNDLE_DEST/$APP_NAME.app"
log "assembling $APP_NAME.app v$VERSION -> ${APP_BUNDLE#$REPO_ROOT/}"

rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
ditto "$SRC_BIN" "$APP_BUNDLE/Contents/MacOS/$EXEC_NAME"

cat > "$APP_BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>$EXEC_NAME</string>
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
log "run directly:  \"$APP_BUNDLE/Contents/MacOS/$EXEC_NAME\""
log "or install:    scripts/rust-install.sh$([[ "$PROD" -eq 1 ]] && echo ' --prod')"
