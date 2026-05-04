---
name: nice-release
description: Cut a new Nice release. Use whenever the user says "time to release", "ship it", "cut a release", "tag a new version", "release X.Y.Z", or otherwise asks to publish a new Nice version. The skill bumps `project.yml`, commits, pushes, tags `vX.Y.Z`, and pushes the tag — at which point GitHub Actions (`.github/workflows/release.yml`) takes over and handles archive, sign, notarize, staple, the GitHub release, and the homebrew-nice cask bump PR. Default bump is minor (e.g. `0.16.0` → `0.17.0`); the user may override. Reach for this skill instead of reverse-engineering the flow from `git log` each time.
---

# Cutting a Nice release

The release pipeline is split between local and CI:

- **Local (this skill's job):** bump `project.yml`, commit, push to `main`,
  tag `vX.Y.Z`, push the tag.
- **CI (`.github/workflows/release.yml`, fires on `v*.*.*` tag push):** runs
  `scripts/release.sh`, archives + signs + notarizes + staples, attaches
  `Nice-X.Y.Z.zip` to a GitHub release with auto-generated notes, then
  opens a bump PR on the `Nick-Anderssohn/homebrew-nice` tap.

The whole thing keys off the tag, so the only thing the local steps must
get right is "the commit at the tag has the new version in `project.yml`".

## Why the version bump must be committed before the tag

`scripts/release.sh` runs on the tagged commit and asserts that the
`CFBundleShortVersionString` and `MARKETING_VERSION` in `project.yml`
already match the `--version` argument it was passed. If you tag without
bumping, CI fails at the assertion. (v0.13.0 once shipped without the
bump committed; the About menu showed the previous release. The guard
exists to make sure that can't happen again.) So: **bump → commit →
push → tag → push tag**, in that order.

## Pre-checks

Before doing anything, confirm:

1. **Branch is `main`.** Tags should come from main; CI's release
   workflow only runs on tag pushes, but releasing from a feature
   branch will produce a tag that doesn't match the user's mental
   model of "what's on main."
2. **Working tree is clean** (untracked files like `.claude/worktrees/`
   are fine — they're gitignored). Any tracked diff means there's
   uncommitted work that should either land or be stashed first.
3. **Latest `v*` tag matches `project.yml`'s current version.** If
   `project.yml` says `0.16.0` and the latest tag is `v0.16.0`, you're
   in the normal "ready to bump" state. If they don't match, something
   is out of sync — stop and figure out why before bumping. (Common
   cause: a half-finished release where the bump was committed but the
   tag was never pushed. In that case the right move may be to push
   the existing version's tag, not invent a new one.)
4. **Show the user the commits that will be in this release**
   (`git log <latest-tag>..HEAD --oneline`) so they can confirm the
   release is non-empty and that nothing is missing.

```sh
git rev-parse --abbrev-ref HEAD                    # expect: main
git status --porcelain                              # expect: empty (or only untracked)
git tag --sort=-v:refname | head -1                 # latest tag
grep -E '(CFBundleShortVersionString|MARKETING_VERSION):' project.yml
git log "$(git tag --sort=-v:refname | head -1)..HEAD" --oneline
```

## Picking the next version

Default to a **minor** bump (`0.16.0` → `0.17.0`). Nice has stayed on
the `0.x.0` cadence — minor for every release, no patch releases yet —
so deviating without being asked is surprising. If the user says
"release 0.17.1" or "patch release" or "bump major", honor that.

Confirm the chosen version with the user before writing it down,
unless they already named it explicitly.

## The local sequence

1. **Bump the two version fields in `project.yml`.** Only these two —
   `CFBundleVersion` and `CURRENT_PROJECT_VERSION` stay at `"1"` (those
   are the build number, not the marketing version, and `release.sh`
   overrides them in-memory anyway).

   ```
   CFBundleShortVersionString: "X.Y.Z"
   MARKETING_VERSION: "X.Y.Z"
   ```

2. **Commit with the conventional message:**

   ```sh
   git add project.yml
   git commit -m "Bump project.yml version to X.Y.Z for the vX.Y.Z release"
   ```

   The format is consistent across releases (grep `git log --oneline`
   for prior bumps) — keep it the same so the history reads cleanly.

3. **Push to `main`:**

   ```sh
   git push origin main
   ```

   This will print "Bypassed rule violations for refs/heads/main"
   because `main` has branch-protection requiring PRs and a status
   check. **That's expected and not a problem to flag** — the user
   owns the repo and routinely pushes the version-bump commit
   directly. Don't ask for confirmation, don't suggest a PR.

4. **Tag and push the tag:**

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   The tag push is what fires `release.yml`. Up to this point
   everything is locally reversible (delete the tag, revert the
   commit); once CI starts there's a public artifact at stake, but
   even that's recoverable (`gh release delete`, retag).

## Watching CI

After pushing the tag, the release workflow runs on `macos-26` and
takes roughly 3 minutes end-to-end (v0.16.0: 2m48s; v0.15.0: 2m54s).
Surface the run to the user and check back after it should be done:

```sh
gh run list --workflow=release.yml --limit 1
```

Don't poll in a tight loop. A reasonable cadence is to check at
~3-4 minutes after the tag push (use `ScheduleWakeup` if running
inside `/loop` dynamic mode, otherwise just tell the user you'll
look again in a few minutes).

If the run **succeeds**, verify both halves of the post-tag pipeline:

```sh
gh release view vX.Y.Z                              # zip attached, notes generated
gh pr list --repo Nick-Anderssohn/homebrew-nice     # cask bump PR open
```

If the run **fails**, fetch the failing step's logs and surface them:

```sh
gh run view <run-id> --log-failed
```

Common failure modes worth recognizing:

- **`project.yml is at … but --version=…`** — the bump wasn't
  committed on the tagged commit. The fix is to bump + commit on
  main, delete and recreate the tag at the new HEAD, push.
- **Notarization failed** — Apple-side flake or a real signing
  problem. The script already pulls `notarytool log` for the
  submission ID; read it before re-running. Don't retry blindly.
- **Cert import / signing identity** — usually a secret expired or
  rotated. Surface the failing step and stop; the user has to fix
  the GitHub Actions secrets.

## What this skill does NOT do

- It doesn't run `scripts/release.sh` locally. The script *can* run
  locally (it sources `scripts/.env.release`), but the canonical
  release path is via CI on `macos-26` so the build is reproducible
  and signed with the right cert. Run locally only if the user
  explicitly asks (e.g. for a `--skip-notarize` smoke test).
- It doesn't update the homebrew cask. `release.yml` opens that PR
  on the tap repo automatically once notarization succeeds.
- It doesn't write release notes by hand. `softprops/action-gh-release`
  generates them from the merged PRs / commits since the last tag.
  If the user wants curated notes, edit the release after CI finishes
  via `gh release edit vX.Y.Z`.
