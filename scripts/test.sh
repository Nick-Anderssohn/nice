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
# Acquire the worktree lock before calling this script. The lock is
# load-bearing for two reasons:
#   1. Shared dev.nickanderssohn.nice-dev bundle ID across worktrees
#      means UITests can't run concurrently.
#   2. The crash-recovery dotfile .scripts-project-yml.bak is a single
#      shared path within a worktree (also read by install.sh). Two
#      uncoordinated runs could overwrite each other's backup with an
#      already-patched file and bake the patch in permanently.
# See the `worktree-lock` skill / CLAUDE.md.
#
# project.yml is restored on exit. If the script is killed before the
# EXIT trap fires (kill -9, parent shell killed mid-script, power
# loss), the next invocation finds the stale backup and restores from
# it before doing anything else.
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
# Stable-named backup. Shared across scripts: install.sh writes/reads
# the same path so install→test cross-script crashes self-heal.
PROJECT_YML_BACKUP="$REPO_ROOT/.scripts-project-yml.bak"

# Recover from a previous run that was killed before its EXIT trap
# fired (kill -9, parent shell killed mid-script, power loss). The
# backup file's existence is the signal: a clean run always deletes
# it on exit. The contents are the pre-patch state captured by that
# prior run, so restoring from it returns project.yml to whatever the
# developer had before that run started. Recovers from in-script
# crashes regardless of which script crashed.
#
# Guards:
#   - require non-empty backup. A truncated/zero-byte backup (left by
#     a hypothetical mid-`cp` interruption, or external truncation)
#     would otherwise replace project.yml with garbage.
#   - skip the restore entirely when backup contents already match
#     project.yml. Catches the case where the developer noticed the
#     bad state and `git restore`d the file themselves but didn't
#     know to delete the dotfile too.
if [[ -f "$PROJECT_YML_BACKUP" ]]; then
    if [[ ! -s "$PROJECT_YML_BACKUP" ]]; then
        log "WARNING: stale backup at .scripts-project-yml.bak is empty; deleting without restore"
        rm -f "$PROJECT_YML_BACKUP"
    elif cmp -s "$PROJECT_YML_BACKUP" "$PROJECT_YML"; then
        log "found stale backup at .scripts-project-yml.bak; project.yml already matches it, deleting backup"
        rm -f "$PROJECT_YML_BACKUP"
    else
        log "found stale backup at .scripts-project-yml.bak — treating as crashed prior run; restoring project.yml"
        cp "$PROJECT_YML_BACKUP" "$PROJECT_YML"
        rm -f "$PROJECT_YML_BACKUP"
    fi
fi

# Capture the pre-patch state BEFORE applying any modifications, so
# the EXIT trap (or the recovery block above on the next run) can
# restore to it. Write to a tmp path then atomic-rename so a partial
# write (signal during cp, ENOSPC) never appears as a complete
# backup that the next run's recovery would trust.
cp "$PROJECT_YML" "${PROJECT_YML_BACKUP}.tmp"
mv "${PROJECT_YML_BACKUP}.tmp" "$PROJECT_YML_BACKUP"
trap 'cp "$PROJECT_YML_BACKUP" "$PROJECT_YML" 2>/dev/null || true; rm -f "$PROJECT_YML_BACKUP" "${PROJECT_YML_BACKUP}.tmp"' EXIT

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
