---
name: worktree-lock
description: Serialize operations that can't run concurrently across this repo's git worktrees. Use BEFORE running `scripts/rust-install.sh` (dev or `--prod`, or the /nice-install command) and BEFORE running any live GUI self-test scenario / black-box harness that drives the installed `Nice Dev.app` bundle. Plain `cargo build` / `cargo test` do NOT need it (per-worktree `target/`). Acquire a single global lock via `scripts/worktree-lock.sh acquire <op>`, run the operation, then always release with `scripts/worktree-lock.sh release`. Also use it when an install/validation fails with a concurrent-access smell (e.g. "/Applications/Nice Dev.app is busy", or the app won't launch because another copy already holds the `dev.nickanderssohn.nice-dev` bundle ID) — that usually means another worktree holds the resource.
---

# Nice worktree lock

This repo uses git worktrees for parallel feature development (see
`.claude/worktrees/`). A few operations can't run concurrently across worktrees
because they touch shared, non-worktree-local state — the installed
`/Applications` bundles, running Nice processes, and the single shared dev
bundle ID. This skill keeps them mutually exclusive via a single file-based
lock at `~/.claude/locks/nice.lock`.

Nice is a **Rust + GPUI** app: each worktree builds into its own `target/`, so
parallel `cargo build` / `cargo test` are fine and need no lock. What must be
serialized is anything touching the *installed* app.

## When to acquire the lock

**Always acquire before these operations:**

1. **Install** — `scripts/rust-install.sh` (with or without `--prod`), or the
   `/nice-install` command. The dev default writes `/Applications/Nice Dev.app`
   and **force-quits a running `Nice Dev`** first; `--prod` writes
   `/Applications/Nice.app` (swapped in place, no force-quit). Two worktrees
   racing on this interleave the `/Applications` writes and the force-quit.

2. **Live GUI validation** — any self-test scenario / black-box harness (the
   `quitprobe`-style pixel + CGEvent checks, the live `NICE_*SELFTEST`
   scenarios) that drives the **installed** `Nice Dev.app`. They share the
   `dev.nickanderssohn.nice-dev` bundle ID and the one `/Applications/Nice
   Dev.app`, so two at once fight over the app window / macOS refuses to launch
   a second copy, and an in-flight install quits the app mid-run.

**Hold the lock across the WHOLE install+validate window** — acquire before
the install and release only when validation is done, not right after
installing. Otherwise another worktree can reinstall/force-quit the shared
`Nice Dev.app` out from under your running validation.

**You do NOT need the lock for:**

- `cargo build` / `cargo build --workspace` — per-worktree `target/`.
- `cargo test --workspace` / `cargo test -p nice-itests` — headless, no
  installed bundle, per-worktree `target/`.
- `scripts/vendor-zed.sh` — writes into this worktree's `vendor/` (plus a
  shared read-only zed mirror it manages).
- Reading source files, editing code, etc.

(A hermetic dev-bundle launch under a scratch `HOME` still runs the one
installed `Nice Dev.app` binary, so if you're validating that way while
another worktree might install, do it under the same lock window as above.)

## How to use it

From anywhere inside the repo or a worktree:

```
scripts/worktree-lock.sh acquire <op-name>
# ... run the operation ...
scripts/worktree-lock.sh release
```

Pick an `<op-name>` that describes what you're doing: `install`,
`install-prod`, `validate`. It's stored in the lock metadata so other
worktrees see what you're up to while they wait.

**Always chain acquire + op + release in one shell invocation**, using `&&`
so a failed acquire aborts, plus a trap or `||` to make release fire even on
op failure:

```
scripts/worktree-lock.sh acquire install \
  && { scripts/rust-install.sh; rc=$?; scripts/worktree-lock.sh release; exit $rc; } \
  || scripts/worktree-lock.sh release
```

Or, more simply, with a trap:

```
scripts/worktree-lock.sh acquire validate
trap 'scripts/worktree-lock.sh release' EXIT
scripts/rust-install.sh && ./scripts/quitprobe/quitprobe.sh
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

Git worktrees give each branch its own working tree and its own `target/`
build cache, so parallel *code edits*, *builds*, and *headless `cargo test`*
all work fine. But anything that touches:

- `/Applications/Nice.app` or `/Applications/Nice Dev.app` (the installed
  bundles),
- a running `Nice` / `Nice Dev` process (the dev install force-quits `Nice
  Dev`), or
- the shared `dev.nickanderssohn.nice-dev` bundle ID (macOS won't launch two
  copies; live UI harnesses fight over the one app),

is inherently shared across worktrees, and two of them hitting it at once
cause corrupted installs or flaky/failed live validation. The lock serializes
access without requiring the user to coordinate manually.
