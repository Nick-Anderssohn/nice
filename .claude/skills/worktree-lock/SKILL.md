---
name: worktree-lock
description: Serialize operations that can't run concurrently across this repo's git worktrees. Use BEFORE running `scripts/install.sh` (or the /nice-install command), BEFORE running UI tests in `UITests/` (any `xcodebuild test` that drives `Nice.app`), and BEFORE any `xcodebuild` invocation that uses shared DerivedData (i.e. no `-derivedDataPath` override). Acquire a single global lock via `scripts/worktree-lock.sh acquire <op>`, run the operation, then always release with `scripts/worktree-lock.sh release`. Also use this skill when a build/install/test fails with errors that smell like a concurrent-access race (e.g. "/Applications/Nice.app is busy", Xcode module-cache corruption, XCUITest "application is not running") — those often mean another worktree is holding the resource.
---

# Nice worktree lock

This repo uses git worktrees for parallel feature development (see
`.claude/worktrees/`). A few operations can't run concurrently across worktrees
because they touch shared, non-worktree-local state. This skill keeps them
mutually exclusive via a single file-based lock at `~/.claude/locks/nice.lock`.

## When to acquire the lock

**Always acquire before these operations:**

1. **Global install** — `scripts/install.sh`, or the `/nice-install` command.
   Writes `/Applications/Nice.app` and kills any running Nice process. Two
   worktrees racing on this interleave file writes and kill each other's
   running app.

2. **UI tests** — anything that runs `NiceUITests` (the XCUITest suite in
   `UITests/`). These launch and drive `Nice.app` via XCUITest; two suites
   running at once fight over the app window. Typical command:
   `xcodebuild test -scheme Nice -destination 'platform=macOS' -only-testing:NiceUITests`.
   Also conflicts with an in-flight install (install kills Nice mid-test).

3. **`xcodebuild` against shared DerivedData** — any `xcodebuild` invocation
   that does **not** pass `-derivedDataPath` to a worktree-local path. The
   default DerivedData lives at `~/Library/Developer/Xcode/DerivedData/` and
   is shared across worktrees. Two builds into it will corrupt each other's
   module cache.

   **Note:** `scripts/install.sh` already uses `-derivedDataPath
   "$REPO_ROOT/build"`, so the *build step* of install doesn't need the lock
   on DerivedData grounds — but install itself is still gated on the
   `/Applications` write, so the whole script runs under the lock.

**You do NOT need the lock for:**

- `xcodegen generate` (writes inside the worktree).
- `xcodebuild` with `-derivedDataPath` pointing into the worktree (e.g.
  `./build`).
- Reading source files, running unit tests that don't launch `Nice.app`,
  editing code, etc.

## How to use it

From anywhere inside the repo or a worktree:

```
scripts/worktree-lock.sh acquire <op-name>
# ... run the operation ...
scripts/worktree-lock.sh release
```

Pick an `<op-name>` that describes what you're doing: `install`, `ui-tests`,
`xcodebuild`. It's stored in the lock metadata so other worktrees see what
you're up to while they wait.

**Always chain acquire + op + release in one shell invocation**, using `&&`
so a failed acquire aborts, plus a trap or `||` to make release fire even on
op failure:

```
scripts/worktree-lock.sh acquire install \
  && { scripts/install.sh; rc=$?; scripts/worktree-lock.sh release; exit $rc; } \
  || scripts/worktree-lock.sh release
```

Or, more simply, with a trap:

```
scripts/worktree-lock.sh acquire ui-tests
trap 'scripts/worktree-lock.sh release' EXIT
xcodebuild test -scheme Nice -destination 'platform=macOS' -only-testing:NiceUITests
```

If you're running the acquire + op as separate Bash tool calls, remember:
**you must run release even if the op fails or times out.** If you forget,
the lock sits there until the 30-minute TTL expires and other worktrees are
blocked in the meantime.

## Contention behavior

`acquire` blocks with a 5-second poll until it can take the lock. While
waiting it prints the current holder (worktree path, operation, age). It
also auto-breaks any lock older than 30 minutes (TTL) on the assumption the
holder died.

If the operation you're about to run could genuinely take longer than the
Bash tool's 10-minute ceiling to even *acquire* (i.e. another worktree is
running a long legitimate job), run the `acquire + op + release` chain via
`run_in_background: true` instead of a foreground Bash call. Report the
background task ID to the user so they can monitor it.

You can tune wait behavior per-invocation:

```
NICE_LOCK_MAX_WAIT=120 scripts/worktree-lock.sh acquire install
```

will give up after 120 seconds instead of waiting indefinitely (exits 3).
Useful if you'd rather fail fast than block.

## Inspecting and breaking the lock

- `scripts/worktree-lock.sh status` — show who holds it and for how long.
- `scripts/worktree-lock.sh break` — force-release. Use this **only** when
  the user has confirmed the current holder is truly dead (e.g. they
  cancelled the other Claude session) and they don't want to wait for the
  TTL. Do not break a lock you don't own without asking the user first.

If `release` prints "refusing to release — held by X, not Y", that means
someone broke your lock (likely via TTL) and re-acquired it. Don't force a
break; just report the situation. Your own operation is already done; the
new holder is now responsibly using the lock.

## Why this exists

Git worktrees give each feature branch its own working tree and its own
`./build/` DerivedData directory, so parallel *code edits* and *builds* work
fine. But anything that touches:

- `/Applications/Nice.app` (the installed bundle),
- the running `Nice.app` process, or
- `~/Library/Developer/Xcode/DerivedData/` (shared default DerivedData)

is inherently shared across worktrees, and two of them hitting it at once
causes corrupted installs, flaky UI tests, or broken module caches. The
lock serializes access without requiring the user to coordinate manually.
