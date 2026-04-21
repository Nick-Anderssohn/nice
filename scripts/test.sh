#!/usr/bin/env bash
#
# Run Nice's test suite against the `Nice Dev` bundle identity
# (dev.nickanderssohn.nice-dev) with ./build-dev as DerivedData.
#
# Why this wrapper exists: a bare `xcodebuild test -scheme Nice` runs
# tests with the production bundle ID, which means the test process
# reads and writes the user's real UserDefaults domain and contends for
# the production DerivedData. This wrapper patches project.yml so only
# the Nice target's PRODUCT_BUNDLE_IDENTIFIER changes, then xcodegen +
# xcodebuild test run without CLI overrides. (CLI overrides leak into
# the SwiftTerm package dependency and break its resource bundle.)
# project.yml is restored on exit.
#
# PRODUCT_NAME is deliberately NOT patched: changing it to "Nice Dev"
# would rename the Swift module (PRODUCT_MODULE_NAME defaults to
# PRODUCT_NAME.c99ExtIdentifier → "Nice_Dev") and every `@testable
# import Nice` in the unit tests would stop resolving. The Nice Dev
# install.sh variant renames the .app on disk to avoid the Applications
# collision, but tests build into ./build-dev and never touch
# /Applications, so a name collision can't happen here.
#
# Forwarded args: anything after the script name is passed through to
# xcodebuild, e.g.
#   scripts/test.sh -only-testing:NiceUnitTests/FooTests/testBar
#
# Acquire the worktree lock before calling this script (shared
# dev.nickanderssohn.nice-dev bundle ID across worktrees means UITests
# can't run concurrently). See the `worktree-lock` skill / CLAUDE.md.
#
# Exit codes:
#   0  tests passed
#   1  prereq missing
#   2  tests failed

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

log() { printf '[test] %s\n' "$*"; }

command -v xcodegen   >/dev/null 2>&1 || { printf '[test] missing dep: xcodegen\n' >&2; exit 1; }
command -v xcodebuild >/dev/null 2>&1 || { printf '[test] missing dep: xcodebuild\n' >&2; exit 1; }

# ── patch project.yml for dev (scoped to the Nice target only) ───────
PROJECT_YML="$REPO_ROOT/project.yml"
PROJECT_YML_BACKUP=$(mktemp -t nice-test-project-yml)
cp "$PROJECT_YML" "$PROJECT_YML_BACKUP"
trap 'cp "$PROJECT_YML_BACKUP" "$PROJECT_YML" 2>/dev/null || true; rm -f "$PROJECT_YML_BACKUP"' EXIT

log "patching project.yml → dev.nickanderssohn.nice-dev"
/usr/bin/sed -i '' -E \
    "s|^( *PRODUCT_BUNDLE_IDENTIFIER: dev\.nickanderssohn\.nice)\$|\\1-dev|" \
    "$PROJECT_YML"

log "generating Xcode project via xcodegen"
xcodegen generate >/dev/null

log "running tests against dev.nickanderssohn.nice-dev"
xcodebuild test \
    -project Nice.xcodeproj \
    -scheme Nice \
    -configuration Debug \
    -destination 'platform=macOS' \
    -derivedDataPath "$REPO_ROOT/build-dev" \
    CODE_SIGN_IDENTITY='-' \
    "$@"
