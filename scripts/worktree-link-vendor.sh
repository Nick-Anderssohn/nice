#!/usr/bin/env bash
#
# worktree-link-vendor.sh — give a fresh git worktree the vendored zed/GPUI
# checkout the Cargo workspace path-depends on (vendor/zed).
#
# git worktrees don't copy gitignored files, and vendor/ (~1 GB) is gitignored
# and reproduced by scripts/vendor-zed.sh — so a brand-new `claude -w` worktree
# has no vendor/ and `cargo build --workspace` fails to resolve the `gpui` path
# dependency (crates/nice/Cargo.toml → ../../vendor/zed/...). Rather than
# re-vendor 1 GB per worktree, symlink to the MAIN checkout's vendor/ — both the
# main tree and every worktree pin the same zed rev (the committed patches +
# vendor-zed.sh are the source of truth), so sharing one checkout is safe.
#
# Wired as a SessionStart hook (.claude/settings.json) so it runs automatically
# on the first session in a new worktree. Idempotent + guarded: a no-op when
# vendor/ already exists, when run in the main checkout, or outside a repo.
set -euo pipefail

# Repo root of THIS checkout (worktree or main). Bail quietly if not in a repo.
root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
[ -n "$root" ] || exit 0

# Already have vendor/ (the real dir in the main checkout, or a prior symlink in
# this worktree)? Nothing to do.
[ -e "$root/vendor" ] && exit 0

# The shared .git common dir points at the MAIN working tree's .git; its parent
# is the main checkout root. In the main checkout --git-common-dir is ".git"
# (relative), so main_root resolves back to $root and we skip below.
common="$(git rev-parse --git-common-dir 2>/dev/null)" || exit 0
case "$common" in
  /*) ;;                 # already absolute (typical inside a linked worktree)
  *)  common="$root/$common" ;;
esac
main_root="$(cd "$(dirname "$common")" 2>/dev/null && pwd)" || exit 0

# Only link when this really is a linked worktree (common dir lives in a
# different tree) and the main checkout actually has a vendor/ to point at.
if [ "$main_root" != "$root" ] && [ -e "$main_root/vendor" ]; then
  ln -s "$main_root/vendor" "$root/vendor"
  echo "[worktree-link-vendor] linked vendor -> $main_root/vendor" >&2
fi
exit 0
