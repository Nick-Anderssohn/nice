#!/usr/bin/env bash
#
# vendor-zed.sh — reproducibly materialize the pinned + patched GPUI checkout
# the Rust workspace path-depends on.
#
# The checkout itself (vendor/zed, ~1 GB) is gitignored; this script plus the
# patches/*.patch files listed below are the committed source of truth. Run it
# once per fresh worktree before the first `cargo build --workspace`.
#
# Strategy (self-contained, no external fork):
#   1. Maintain a shared BARE MIRROR at ~/.cache/nice/zed-mirror.git — cloned
#      from zed-industries/zed once, `git fetch`ed only when the pin is missing.
#   2. Local-clone (hardlinked objects, cheap) the mirror into vendor/zed.
#   3. Check out the pinned revision (detached).
#   4. Apply each patch, skipping cleanly if already applied:
#        - zed-bg-luminance.patch: the kitty-style bg-aware glyph composition
#          curve (SwiftTerm parity).
#        - zed-display-link-selfheal.patch: fix round r5d — a delayed retry when
#          gpui_macos's start_display_link cannot (re)start the CVDisplayLink.
#          On the pin, displayLayer: stops + RECREATES the link around every
#          demand-present draw, and both of start_display_link's failure paths
#          (stale occlusion read; DisplayLink::new error) silently leave the
#          link permanently stopped: the 2026-07-10 presentation wedge (screen
#          frozen on a stale frame for minutes, app responsive, CVDisplayLink
#          thread parked with zero callbacks). The patch schedules a ~50 ms
#          single-flight retry that restarts the link iff the window is still
#          occlusion-visible, and logs the self-heal.
#        - zed-force-width-exact.patch: make apply_force_width_to_layout snap
#          every base glyph to its exact cell slot instead of only past a 1px
#          tolerance. Nice's cell width is ceil-snapped above the font's
#          natural advance, so sub-tolerance drift accumulated across a run and
#          glyph positions depended on where the run started — selecting text
#          (which re-splits runs on background) visibly shifted characters.
#
# Idempotent: a second run with the pin already checked out and patched is a
# fast no-op (a handful of git plumbing checks, no network, no re-clone).
#
# Changing the pin or dropping a patch is a human decision, not an automated
# drift — edit ZED_PIN / the patch list deliberately.
set -euo pipefail

ZED_URL="https://github.com/zed-industries/zed"
ZED_PIN="10b07951838e422722e34641f4a9c0bfec9037ff"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Overridable for tests / CI; defaults to a shared per-user cache so multiple
# worktrees reuse one mirror.
MIRROR="${NICE_ZED_MIRROR:-$HOME/.cache/nice/zed-mirror.git}"
VENDOR="$REPO_ROOT/vendor/zed"
# The committed patch set, applied in order. Each patch pairs with a marker
# file inside the gitignored checkout; the marker's presence means "applied".
PATCHES=(
    "zed-bg-luminance"
    "zed-display-link-selfheal"
    "zed-force-width-exact"
)
patch_file() { printf '%s/patches/%s.patch' "$REPO_ROOT" "$1"; }
marker_file() { printf '%s/.nice-%s-applied' "$VENDOR" "${1#zed-}"; }

log() { printf 'vendor-zed: %s\n' "$*"; }

has_commit() {
    # $1 = repo dir, $2 = rev. True iff the commit object is present.
    git -C "$1" cat-file -e "${2}^{commit}" 2>/dev/null
}

for name in "${PATCHES[@]}"; do
    if [ ! -f "$(patch_file "$name")" ]; then
        echo "vendor-zed: error: patch not found at $(patch_file "$name")" >&2
        exit 1
    fi
done

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
all_markers_present() {
    local name
    for name in "${PATCHES[@]}"; do
        [ -f "$(marker_file "$name")" ] || return 1
    done
}

current="$(git -C "$VENDOR" rev-parse -q --verify HEAD 2>/dev/null || echo none)"
if [ "$current" != "$ZED_PIN" ] || ! all_markers_present; then
    # Reset to a pristine pinned tree before (re)applying the patches, so the
    # result is reproducible regardless of prior partial state. A checkout that
    # predates a newly-added patch lands here too (its marker is missing): it
    # pays one reset + full reapply + gpui rebuild — the designed migration.
    log "checking out pin $ZED_PIN"
    git -C "$VENDOR" checkout --quiet --detach "$ZED_PIN"
    git -C "$VENDOR" reset --quiet --hard "$ZED_PIN"
    git -C "$VENDOR" clean --quiet -fd
    for name in "${PATCHES[@]}"; do
        rm -f "$(marker_file "$name")"
    done
fi

# --- 4. Apply the patch set (idempotent, in order) --------------------------
for name in "${PATCHES[@]}"; do
    patch="$(patch_file "$name")"
    marker="$(marker_file "$name")"
    if [ -f "$marker" ]; then
        log "$name: patch already applied (marker present) — no-op"
    elif git -C "$VENDOR" apply --check "$patch" 2>/dev/null; then
        log "$name: applying patch"
        git -C "$VENDOR" apply "$patch"
        touch "$marker"
    elif git -C "$VENDOR" apply --reverse --check "$patch" 2>/dev/null; then
        # Patch content is present but the marker was lost — record and move on.
        log "$name: patch already present (marker missing) — recording marker"
        touch "$marker"
    else
        echo "vendor-zed: error: $name.patch does not apply cleanly to $ZED_PIN" >&2
        echo "  (tree may be dirty; re-run after 'rm -rf $VENDOR')" >&2
        exit 1
    fi
done

log "ok — vendor/zed @ $ZED_PIN, patches applied: ${PATCHES[*]}"
