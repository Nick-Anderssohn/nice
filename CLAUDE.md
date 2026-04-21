# Nice — working rules for Claude

## Two Nice builds — which one Claude can touch

Nice ships as two parallel installs so you can develop Nice without
killing the app that's hosting your live Claude Code session.

- **`/Applications/Nice.app`** — the user's working install
  (`dev.nickanderssohn.nice`). Hosts live Claude Code sessions in
  long-lived ptys, including ours. **Claude MUST NOT build, install,
  test, uninstall, or kill this build** except with explicit per-task
  authorization ("install prod", "promote a release", "reinstall Nice"
  — something that unambiguously names the production install).
  Passing `--prod` to `install.sh` / `uninstall.sh`, or calling bare
  `xcodebuild` / `xcodebuild test` against the `Nice` scheme with no
  overrides, all count as destructive actions against shared user
  state. Always confirm first.

- **`/Applications/Nice Dev.app`** — the development build
  (`dev.nickanderssohn.nice-dev`). This is where all Claude-side repo
  activity lives: its own bundle ID, its own UserDefaults domain, its
  own Application Support folder (`~/Library/Application Support/Nice
  Dev/`), its own DerivedData (`./build-dev`). Rebuilding or killing
  `Nice Dev` is safe by default — it cannot affect the user's real
  session host. Still announce the action before taking it; the user
  may have a demo or manual test in progress in the dev build.

## Running builds and tests

- **Install (default = dev):** `scripts/install.sh` — installs
  `Nice Dev`. Run under the worktree lock (see the `worktree-lock`
  skill). This is the safe default; Claude should reach for it without
  asking.

- **Install (prod):** `scripts/install.sh --prod` — installs the
  user's working `Nice`. Only run with explicit user authorization.

- **Tests:** `scripts/test.sh` — runs the suite with the `Nice Dev`
  bundle ID so tests never touch the user's real UserDefaults. Forward
  any `-only-testing:` args through. Acquire the worktree lock first
  (UITests contend on the shared dev bundle ID). **Do not** call
  `xcodebuild test` directly without the dev overrides — a bare
  `xcodebuild test -scheme Nice` runs against the prod bundle ID.

- **Uninstall:** `scripts/uninstall.sh` defaults to `Nice Dev`. Pass
  `--prod` only with explicit authorization.

## Before killing a running Nice

Even with the two-variant split, `pgrep` the exact variant before
killing anything:

```sh
pgrep -f '/Applications/Nice.app/Contents/MacOS/Nice'           # prod
pgrep -f '/Applications/Nice Dev.app/Contents/MacOS/Nice Dev'   # dev
```

Killing prod `Nice` **requires explicit permission every time** — it
loses the user's live session work. Killing `Nice Dev` is lower-stakes
but still worth announcing. Prefer a graceful quit:

```sh
osascript -e 'tell application "Nice" to quit'
osascript -e 'tell application "Nice Dev" to quit'
```

Only escalate to `pkill`/SIGKILL with explicit user consent.

## Common actions that touch these builds

- `scripts/install.sh --prod` and `scripts/uninstall.sh --prod` —
  destructive to the user's working install. Confirm first.
- `scripts/install.sh` / `scripts/uninstall.sh` (no flag) — dev only;
  safe to run after announcing.
- Bare `xcodebuild` / `xcodebuild test` against the `Nice` scheme with
  no `PRODUCT_BUNDLE_IDENTIFIER` / `PRODUCT_NAME` overrides — runs
  with the prod bundle ID and can read/write the user's real
  UserDefaults. Use `scripts/test.sh` instead.
- UITests in `UITests/` — drive a `Nice.app` bundle. Run via
  `scripts/test.sh` so they drive the dev variant, not prod.
- `pkill -x Nice`, `killall Nice`, `kill <pid>` targeting prod Nice —
  confirm first (`Nice Dev` has a different process name, `Nice Dev`).
- `rm`/`mv` against `/Applications/Nice.app` — confirm first.
  `/Applications/Nice Dev.app` is safe to remove.

If the user has already authorized the action in the current turn
(e.g. "reinstall Nice"), you may proceed without re-asking.
Authorization does not carry across unrelated tasks.
