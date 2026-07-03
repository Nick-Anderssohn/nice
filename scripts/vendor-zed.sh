#!/usr/bin/env bash
#
# vendor-zed.sh — reproducibly materialize the pinned + bg-luminance-patched
# GPUI checkout the Rust workspace path-depends on.
#
# The checkout itself (vendor/zed, ~1 GB) is gitignored; this script plus
# patches/zed-bg-luminance.patch are the committed source of truth. Run it once
# per fresh worktree before the first `cargo build --workspace`.
#
# Strategy (self-contained, no external fork):
#   1. Maintain a shared BARE MIRROR at ~/.cache/nice/zed-mirror.git — cloned
#      from zed-industries/zed once, `git fetch`ed only when the pin is missing.
#   2. Local-clone (hardlinked objects, cheap) the mirror into vendor/zed.
#   3. Check out the pinned revision (detached).
#   4. Apply patches/zed-bg-luminance.patch, skipping cleanly if already applied.
#
# Idempotent: a second run with the pin already checked out and patched is a
# fast no-op (a handful of git plumbing checks, no network, no re-clone).
#
# Changing the pin or dropping the patch is a human decision, not an automated
# drift — edit ZED_PIN / the patch deliberately.
set -euo pipefail

ZED_URL="https://github.com/zed-industries/zed"
ZED_PIN="10b07951838e422722e34641f4a9c0bfec9037ff"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Overridable for tests / CI; defaults to a shared per-user cache so multiple
# worktrees reuse one mirror.
MIRROR="${NICE_ZED_MIRROR:-$HOME/.cache/nice/zed-mirror.git}"
VENDOR="$REPO_ROOT/vendor/zed"
PATCH="$REPO_ROOT/patches/zed-bg-luminance.patch"
# Marker lives inside the gitignored checkout; its presence means "patch applied".
MARKER="$VENDOR/.nice-bg-luminance-applied"

log() { printf 'vendor-zed: %s\n' "$*"; }

has_commit() {
    # $1 = repo dir, $2 = rev. True iff the commit object is present.
    git -C "$1" cat-file -e "${2}^{commit}" 2>/dev/null
}

if [ ! -f "$PATCH" ]; then
    echo "vendor-zed: error: patch not found at $PATCH" >&2
    exit 1
fi

# --- 1. Shared bare mirror -------------------------------------------------
if [ ! -d "$MIRROR" ]; then
    log "cloning bare mirror (first time; downloads zed, minutes) -> $MIRROR"
    mkdir -p "$(dirname "$MIRROR")"
    git clone --bare "$ZED_URL" "$MIRROR"
fi

if ! has_commit "$MIRROR" "$ZED_PIN"; then
    log "pin not in mirror; fetching origin"
    git -C "$MIRROR" fetch --quiet origin
fi
if ! has_commit "$MIRROR" "$ZED_PIN"; then
    echo "vendor-zed: error: pin $ZED_PIN not found in $ZED_URL after fetch" >&2
    exit 1
fi

# --- 2. Local hardlinked clone into vendor/zed -----------------------------
if [ ! -d "$VENDOR/.git" ] || ! has_commit "$VENDOR" "$ZED_PIN"; then
    log "local-cloning mirror -> vendor/zed (hardlinked objects)"
    rm -rf "$VENDOR"
    mkdir -p "$(dirname "$VENDOR")"
    git clone --local --no-checkout --quiet "$MIRROR" "$VENDOR"
fi

# --- 3. Check out the pin (detached) ---------------------------------------
current="$(git -C "$VENDOR" rev-parse -q --verify HEAD 2>/dev/null || echo none)"
if [ "$current" != "$ZED_PIN" ] || [ ! -f "$MARKER" ]; then
    # Reset to a pristine pinned tree before (re)applying the patch, so the
    # result is reproducible regardless of prior partial state.
    log "checking out pin $ZED_PIN"
    git -C "$VENDOR" checkout --quiet --detach "$ZED_PIN"
    git -C "$VENDOR" reset --quiet --hard "$ZED_PIN"
    git -C "$VENDOR" clean --quiet -fd
    rm -f "$MARKER"
fi

# --- 4. Apply the bg-luminance patch (idempotent) --------------------------
if [ -f "$MARKER" ]; then
    log "patch already applied (marker present) — no-op"
elif git -C "$VENDOR" apply --check "$PATCH" 2>/dev/null; then
    log "applying bg-luminance patch"
    git -C "$VENDOR" apply "$PATCH"
    touch "$MARKER"
elif git -C "$VENDOR" apply --reverse --check "$PATCH" 2>/dev/null; then
    # Patch content is present but the marker was lost — record and move on.
    log "patch already present (marker missing) — recording marker"
    touch "$MARKER"
else
    echo "vendor-zed: error: patch does not apply cleanly to $ZED_PIN" >&2
    echo "  (tree may be dirty; re-run after 'rm -rf $VENDOR')" >&2
    exit 1
fi

log "ok — vendor/zed @ $ZED_PIN, bg-luminance patch applied"
