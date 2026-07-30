#!/usr/bin/env bash
#
# worktree-link-vendor.sh — give a fresh git worktree the two things it needs
# to build fast: the vendored zed/GPUI checkout the Cargo workspace
# path-depends on (vendor/zed), and a shared cargo target dir so it never
# cold-compiles what the main checkout already built.
#
# 1. vendor symlink. git worktrees don't copy gitignored files, and vendor/
#    (~1 GB) is gitignored and reproduced by scripts/vendor-zed.sh — so a
#    brand-new `claude -w` worktree has no vendor/ and `cargo build
#    --workspace` fails to resolve the `gpui` path dependency
#    (crates/nice/Cargo.toml → ../../vendor/zed/...). Rather than re-vendor
#    1 GB per worktree, symlink to the MAIN checkout's vendor/ — both the
#    main tree and every worktree pin the same zed rev (the committed patches
#    + vendor-zed.sh are the source of truth), so sharing one checkout is
#    safe.
#
# 2. shared target dir (.cargo/config.toml, gitignored). Even with the
#    symlink, a fresh worktree's own target/ cold-compiles vendored gpui +
#    ~700 registry deps (~10+ min) before the first build lands. Cargo hashes
#    compilation units by package id RELATIVE to the workspace root
#    (PackageId::stable_hash), and the symlink makes the vendored sources the
#    same files with the same mtimes — so pointing the worktree's builds at
#    the main checkout's target/ reuses every dep artifact as-is (verified
#    2026-07-30: a fresh worktree's `cargo build -p gpui` against the main
#    target is a 0.14 s no-op). Costs to know about: cargo's own build-dir
#    lock serializes CONCURRENT builds across worktrees (they queue, not
#    fail), and workspace crates whose sources differ between two trees
#    rebuild when alternating builds between them — the heavy deps stay warm
#    either way. Env (CARGO_TARGET_DIR) and CLI flags still override the
#    config, so callers that pin their own build dir (release CI) are
#    unaffected.
#
# Wired as a SessionStart hook (.claude/settings.json) so it runs
# automatically on the first session in a new worktree, and called explicitly
# by orchestrator worktreeSetup scripts. Idempotent + guarded: each step is a
# no-op when its artifact already exists, and the whole script is a no-op in
# the main checkout or outside a repo.
set -euo pipefail

# Repo root of THIS checkout (worktree or main). Bail quietly if not in a repo.
root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
[ -n "$root" ] || exit 0

# The shared .git common dir points at the MAIN working tree's .git; its parent
# is the main checkout root. In the main checkout --git-common-dir is ".git"
# (relative), so main_root resolves back to $root and we skip below.
common="$(git rev-parse --git-common-dir 2>/dev/null)" || exit 0
case "$common" in
  /*) ;;                 # already absolute (typical inside a linked worktree)
  *)  common="$root/$common" ;;
esac
main_root="$(cd "$(dirname "$common")" 2>/dev/null && pwd)" || exit 0

# Main checkout: nothing to link, and it must NOT get a .cargo/config.toml
# (its target/ IS the shared one).
[ "$main_root" != "$root" ] || exit 0

# 1. vendor symlink (skip if this worktree already has one, or main has no
#    vendor/ to point at).
if [ ! -e "$root/vendor" ] && [ -e "$main_root/vendor" ]; then
  ln -s "$main_root/vendor" "$root/vendor"
  echo "[worktree-link-vendor] linked vendor -> $main_root/vendor" >&2
fi

# 2. shared target dir. Never overwrite an existing config (a worktree may
#    have deliberately pinned something else).
if [ ! -e "$root/.cargo/config.toml" ]; then
  mkdir -p "$root/.cargo"
  cat > "$root/.cargo/config.toml" <<EOF
# Written by scripts/worktree-link-vendor.sh (gitignored, worktree-only):
# share the main checkout's cargo target dir so this worktree never
# cold-compiles the vendored gpui / registry deps. See that script's header
# for why this is sound and what it trades away.
[build]
target-dir = "$main_root/target"
EOF
  echo "[worktree-link-vendor] wrote .cargo/config.toml (target-dir -> $main_root/target)" >&2
fi

exit 0
