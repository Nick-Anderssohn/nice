---
name: nice-release
description: Cut a new Nice release. Use whenever the user says "time to release", "ship it", "cut a release", "tag a new version", "release X.Y.Z", or otherwise asks to publish a new Nice version. The skill bumps the version in `crates/nice/Cargo.toml` (+ `Cargo.lock`), commits, pushes, tags `vX.Y.Z`, and pushes the tag — at which point GitHub Actions (`.github/workflows/release.yml`) takes over and handles build, sign, notarize, staple, the GitHub release, and the homebrew-nice cask bump PR. Default bump is minor (e.g. `0.31.0` → `0.32.0`); the user may override. Reach for this skill instead of reverse-engineering the flow from `git log` each time.
---

# Cutting a Nice release

Nice is a **Rust + GPUI** app; the release pipeline is split between local and CI:

- **Local (this skill's job):** bump the version in `crates/nice/Cargo.toml`
  (and sync `Cargo.lock`), commit, push to `main`, tag `vX.Y.Z`, push the tag.
- **CI (`.github/workflows/release.yml`, fires on `v*.*.*` tag push):** runs
  `scripts/release-rs.sh --version X.Y.Z`, which builds via
  `scripts/rust-bundle.sh` → Developer-ID signs (hardened runtime) → notarizes
  → staples → produces `Nice-X.Y.Z.zip`, attaches it to a **non-prerelease**
  GitHub release with auto-generated notes, then opens a bump PR on the
  `Nick-Anderssohn/homebrew-nice` tap (prod `Casks/nice.rb`).

The whole thing keys off the tag, so the only thing the local steps must get
right is "the commit at the tag has the new version in `crates/nice/Cargo.toml`".

## Why the version bump must be committed before the tag

`scripts/release-rs.sh` runs on the tagged commit and asserts that the
`version` in `crates/nice/Cargo.toml` already equals the `--version` argument
CI passes it (derived from the tag). If you tag without bumping, CI fails at
the guard ("`crates/nice/Cargo.toml` is at version=… but --version=…; commit
the version bump before tagging"). So: **bump → commit → push → tag → push
tag**, in that order.

## Pre-checks

Before doing anything, confirm:

1. **Branch is `main`.** Tags come from main; releasing from a feature branch
   produces a tag that doesn't match "what's on main."
2. **Working tree is clean** (untracked files like `.claude/worktrees/`,
   `build-rs/`, `vendor/`, `target/` are fine — they're gitignored). Any
   tracked diff means uncommitted work that should land or be stashed first.
3. **Latest `v*` tag vs `crates/nice/Cargo.toml`'s version.** Normally the
   latest `v*` tag equals the current Cargo version → you're in the "ready to
   bump" state. **Exception (one-time):** the Rust cutover pre-bumped
   `Cargo.toml` to `0.31.0` while the last tag is the Swift `v0.30.0`, so for
   the *first* Rust release the Cargo version is already ahead — that release
   is just `git tag v0.31.0` with **no new bump**. After that, tag == Cargo
   version is the normal invariant; if they diverge unexpectedly, stop and
   find out why before bumping (common cause: a bump committed but the tag
   never pushed — push the existing version's tag rather than inventing one).
4. **Show the user the commits that will be in this release**
   (`git log <latest-tag>..HEAD --oneline`) so they can confirm it's non-empty
   and nothing's missing.

```sh
git rev-parse --abbrev-ref HEAD                       # expect: main
git status --porcelain                                 # expect: empty (or only gitignored/untracked)
git tag --sort=-v:refname | grep -E '^v[0-9]' | head -1  # latest prod tag
awk -F'"' '/^version = /{print $2; exit}' crates/nice/Cargo.toml
git log "$(git tag --sort=-v:refname | grep -E '^v[0-9]' | head -1)..HEAD" --oneline
```

## Run the test suite first

CI runs `cargo test --workspace` + `cargo test -p nice-itests` on every push
to `main`, **headless with no skipped subset** (the Rust suite has no
display-bound CI-skip logic — that was a Swift/XCUITest concern that no longer
exists). So a green CI on the release commit already exercises everything CI
can. Before tagging, just confirm locally:

```sh
cargo build --workspace          # clean (only pre-existing dead-code warnings)
cargo test --workspace
cargo test -p nice-itests
```

If you want extra confidence in the GUI, the live self-test scenarios / the
black-box `quitprobe`-style harnesses drive an installed `Nice Dev` bundle and
need a display CI lacks — run those locally **under the worktree lock**. They
are optional, not a release gate.

## Picking the next version

Default to a **minor** bump (`0.31.0` → `0.32.0`). Nice has stayed on the
`0.x.0` cadence — minor for every release, no patch releases yet — so
deviating without being asked is surprising. If the user says "release 0.32.1"
or "patch release" or "bump major", honor that. Confirm the chosen version
with the user before writing it down, unless they already named it explicitly.

## The local sequence

1. **Bump the version in `crates/nice/Cargo.toml`** (the `version = "X.Y.Z"`
   line under `[package]`), then sync the lockfile so `Cargo.lock`'s `nice`
   entry matches (otherwise the build/cache drifts):

   ```sh
   # edit crates/nice/Cargo.toml → version = "X.Y.Z"
   cargo update -p nice           # rewrites the nice entry in Cargo.lock
   ```

2. **Commit both files with the conventional message:**

   ```sh
   git add crates/nice/Cargo.toml Cargo.lock
   git commit -m "Bump nice to X.Y.Z for the vX.Y.Z release"
   ```

   Keep the format consistent with prior bumps (grep `git log --oneline`).

3. **Push to `main`:**

   ```sh
   git push origin main
   ```

   This may print "Bypassed rule violations for refs/heads/main" because
   `main` has branch-protection requiring PRs / a status check. **That's
   expected and not a problem to flag** — the user owns the repo and routinely
   pushes the version-bump commit directly. Don't ask for confirmation, don't
   suggest a PR.

4. **Tag and push the tag:**

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   The tag push is what fires `release.yml`. Up to here everything is locally
   reversible (delete the tag, revert the commit); once CI starts there's a
   public artifact, but even that's recoverable (`gh release delete`, retag).

## Watching CI

After pushing the tag, the release workflow runs on `macos-26` and takes
roughly 3 minutes end-to-end. Surface the run and check back after it should
be done:

```sh
gh run list --workflow=release.yml --limit 1
```

Don't poll in a tight loop — check ~3–4 minutes after the tag push (use
`ScheduleWakeup` if inside `/loop` dynamic mode, otherwise tell the user
you'll look again in a few minutes).

If the run **succeeds**, verify both halves of the post-tag pipeline:

```sh
gh release view vX.Y.Z                              # Nice-X.Y.Z.zip attached, notes generated
gh pr list --repo Nick-Anderssohn/homebrew-nice     # cask bump PR open
```

If the run **fails**, fetch the failing step's logs and surface them:

```sh
gh run view <run-id> --log-failed
```

Common failure modes worth recognizing:

- **Version guard (`Cargo.toml is at version=… but --version=…`)** — the bump
  wasn't committed on the tagged commit. Fix: bump + commit on main, delete
  and recreate the tag at the new HEAD, push.
- **Notarization failed** — Apple-side flake or a real signing problem. A
  `403` "agreement missing/expired" means the Apple Developer agreement needs
  accepting, then `gh run rerun <id> --failed` (no retag). Otherwise read the
  `notarytool log` the script pulls before re-running. Don't retry blindly.
- **Cert import / signing identity** — usually a GitHub Actions secret expired
  or rotated. Surface the failing step and stop; the user fixes the secrets.

## What this skill does NOT do

- It doesn't run `scripts/release-rs.sh` locally. The script *can* run locally
  (it sources `scripts/.env.release` for `APPLE_ID`/`APPLE_APP_PASSWORD`, and
  takes `--skip-notarize` for a build+sign smoke test), but the canonical
  release path is CI on `macos-26` for a reproducible, correctly-signed build.
  Run locally only if the user explicitly asks.
- It doesn't update the homebrew cask. `release.yml` opens that PR on the tap
  automatically once notarization succeeds.
- It doesn't write release notes by hand. The GitHub release action generates
  them from commits since the last tag. For curated notes, edit the release
  after CI finishes via `gh release edit vX.Y.Z`.
