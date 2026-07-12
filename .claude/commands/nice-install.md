---
description: Build and install the Nice Dev development build to /Applications, installing any missing build dependencies first.
---

# /nice-install

Install the **`Nice Dev`** development build on this Mac. This is the safe
default for Claude-run installs — it lands at `/Applications/Nice Dev.app`
with its own bundle ID (`dev.nickanderssohn.nice-dev`), its own Application
Support folder, and its own preferences domain. Rebuilding or killing
`Nice Dev` does **not** touch the user's real `/Applications/Nice.app`
session host.

Nice is a **Rust + GPUI** app (Cargo workspace at the repo root; GPUI/zed is
vendored). Verify each prerequisite first; if anything is missing, guide the
user through installing it before running `scripts/rust-install.sh`. Do not
proceed to the install until every required prerequisite is satisfied.

Run independent checks in parallel.

## Prerequisite checks

1. **macOS 14+** — `sw_vers -productVersion`. If below 14, stop and tell the
   user — Nice's deployment target is macOS 14.

2. **Rust toolchain (`cargo`)** — `command -v cargo`. If missing, guide the
   user to install it:
   - Preferred: `rustup` from https://rustup.rs
     (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`), or
     `brew install rustup` then `rustup-init`.
   - Re-run the check once `cargo` is on PATH.

3. **Vendored GPUI/zed (`vendor/zed/`)** — the workspace path-depends on a
   pinned, patched zed checkout that is gitignored. Check
   `[ -d vendor/zed/crates/gpui ]`. If missing, run `scripts/vendor-zed.sh`
   (idempotent; on a fresh machine it clones zed into a shared mirror at
   `~/.cache/nice/zed-mirror.git` — needs `git` + network the first time,
   then a fast no-op). `scripts/rust-install.sh` hard-fails without it.

**Optional (nicer, not required):** the app icon is compiled by Xcode's
`actool` (`xcrun --find actool`). Without a full Xcode install the bundle
still builds and installs fine — just iconless. Do **not** gate the install
on Xcode; mention it only if the user cares about the icon. (`xcodegen` /
`project.yml` / `xcodebuild` are gone with the Swift build — do not check for
them.)

For any missing *required* prerequisite: explain what's missing, what the
user needs to do, and wait for confirmation before re-checking. Do not
silently skip.

## Install

Once the required prerequisites pass, run `scripts/rust-install.sh` from the
repo root **under the worktree lock** so it doesn't race with another
worktree's install or live tests (see the `worktree-lock` skill for the full
rules):

```
scripts/worktree-lock.sh acquire install \
  && { scripts/rust-install.sh; rc=$?; scripts/worktree-lock.sh release; exit $rc; } \
  || scripts/worktree-lock.sh release
```

Stream the output so the user sees build progress (the first build in a fresh
worktree compiles the full GPUI/zed graph and takes a while; later builds
reuse the per-worktree `target/`). If another worktree is holding the lock,
`acquire` prints the holder and polls every 5 seconds until it's free — let
the user know we're waiting and on whom. If the script fails, surface the last
~20 lines of output and stop (the chain above releases the lock automatically
on failure).

On success, report:
- The installed bundle path (`/Applications/Nice Dev.app`).
- The version (`/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' '/Applications/Nice Dev.app/Contents/Info.plist'`).
- That the user can launch `Nice Dev` from Spotlight, Launchpad, or
  `open -a "Nice Dev"`.

## Promoting a release: installing prod

`scripts/rust-install.sh --prod` installs the production `/Applications/Nice.app`
instead. **Only run this when the user has explicitly asked to upgrade prod**
(e.g. "reinstall Nice", "promote this branch", "update my working Nice").
Unlike the dev install it does **not** force-quit a running prod Nice — it
swaps the bundle in place, and the running instance (which may be hosting a
live Claude Code session, including ours) keeps the old build until it is
relaunched. Still confirm first; installing the wrong branch to prod is a
shared-state hazard.
