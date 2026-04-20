---
description: Build and install Nice.app to /Applications, installing any missing build dependencies first.
---

# /nice-install

Install the Nice app globally on this Mac. Verify each prerequisite first; if
anything is missing, guide the user through installing it before running
`scripts/install.sh`. Do not proceed to `scripts/install.sh` until every
prerequisite is satisfied.

Run independent checks in parallel.

## Prerequisite checks

1. **macOS 14+** — `sw_vers -productVersion`. If below 14, stop and tell the
   user — Nice's deployment target is macOS 14.

2. **Full Xcode (not just Command Line Tools)** — `xcodebuild -version`. This
   command only succeeds when a real Xcode is the active developer dir. If it
   fails (typical message: "tool 'xcodebuild' requires Xcode, but active
   developer directory '/Library/Developer/CommandLineTools' is a command line
   tools instance"), guide the user:
   - Install Xcode from the App Store.
   - After install: `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`
   - Then: `sudo xcodebuild -license accept`
   - Re-run the check.

3. **xcodegen on PATH** — `command -v xcodegen`.
   - If present: done. How it was installed doesn't matter.
   - If missing: check `command -v brew` and offer the easy path:
     - `brew` present → offer to run `brew install xcodegen`.
     - `brew` missing → tell the user xcodegen needs to be installed and list
       the common options without picking one for them: Homebrew
       (https://brew.sh, then `brew install xcodegen`), Mint, or downloading a
       release from https://github.com/yonaskolb/XcodeGen/releases and putting
       the binary on PATH. Do not assume Homebrew.

For any missing prerequisite: explain what's missing, what the user needs to
do, and wait for confirmation before re-checking. Do not silently skip.

## Install

Once all prerequisites pass, run `scripts/install.sh` from the repo root
**under the worktree lock** so it doesn't race with another worktree's
install or UI tests (see the `worktree-lock` skill for the full rules):

```
scripts/worktree-lock.sh acquire install \
  && { scripts/install.sh; rc=$?; scripts/worktree-lock.sh release; exit $rc; } \
  || scripts/worktree-lock.sh release
```

Stream the output so the user sees build progress. If another worktree is
currently holding the lock, `acquire` will print the holder and poll every
5 seconds until it's free — let the user know we're waiting and on whom.
If the script fails, surface the last ~20 lines of output and stop (the
chain above releases the lock automatically on failure).

On success, report:
- The installed bundle path (`/Applications/Nice.app`).
- The version (`/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' /Applications/Nice.app/Contents/Info.plist`).
- That the user can launch Nice from Spotlight, Launchpad, or `open -a Nice`.
