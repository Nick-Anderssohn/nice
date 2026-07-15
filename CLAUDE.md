# Nice — working rules for Claude

Nice is a **Rust + GPUI** macOS app (Cargo workspace at the repo root;
crates `nice`, `nice-term-core`, `nice-term-input`, `nice-term-view`,
`nice-model`, `nice-theme`, `nice-harness`, `nice-itests`). GPUI/zed is
vendored — `scripts/vendor-zed.sh` produces `vendor/zed/` (a pinned zed
checkout + the `patches/*.patch` set — currently 4: `zed-bg-luminance`,
`zed-configurable-blur` (restyle plan 3's modern-macOS blur radius),
`zed-display-link-selfheal`, `zed-force-width-exact`), which the `nice`
crates path-depend into. There is **no Xcode project**: the old `scripts/install.sh`
/ `scripts/test.sh` / `xcodebuild` / `project.yml` / `UITests/` are gone.

## Two Nice builds — which one Claude can touch

Nice ships as two parallel installs so you can develop Nice without
killing the app that's hosting your live Claude Code session. Same
two-variant model as before — the difference is which bundle IDs and
what tooling; both are now the Rust build.

- **`/Applications/Nice.app`** — the user's working install
  (`dev.nickanderssohn.nice`). Hosts live Claude Code sessions in
  long-lived ptys, including ours. **Claude MUST NOT build, install,
  test, uninstall, or kill this build** except with explicit per-task
  authorization ("install prod", "promote a release", "reinstall Nice"
  — something that unambiguously names the production install).
  Passing `--prod` to `scripts/rust-install.sh` / `scripts/uninstall.sh`
  counts as a destructive action against shared user state. Always
  confirm first.

- **`/Applications/Nice Dev.app`** — the development build
  (`dev.nickanderssohn.nice-dev`). This is where all Claude-side repo
  activity lives: its own bundle ID, its own UserDefaults/CFPreferences
  domain, its own Application Support folder (`~/Library/Application
  Support/Nice Dev/`), its own build dir (`./build-rs`). Rebuilding or
  killing `Nice Dev` is safe by default — it cannot affect the user's
  real session host. Still announce the action before taking it; the
  user may have a demo or manual test in progress in the dev build.

## Running builds and tests

- **Build:** `cargo build --workspace`. Uses the per-worktree `target/`
  (no shared DerivedData), so a plain build needs no worktree lock.

- **Tests:** `cargo test --workspace` (unit + in-process scenarios) and
  `cargo test -p nice-itests` (integration). Plain `cargo test` touches
  no installed bundle and needs no worktree lock. The live GUI self-test
  scenarios / black-box harnesses that drive an installed `Nice Dev.app`
  DO need the lock (they contend on the shared dev bundle). During fix
  rounds, run only the targeted tests for the touched modules.

- **Install (default = dev):** `scripts/rust-install.sh` — builds
  (`./build-rs`) and installs `Nice Dev` into `/Applications`. A running
  `Nice Dev` is force-quit first so the next launch is the new build.
  Run under the worktree lock (see the `worktree-lock` skill; hold it
  through the whole install+test window). This is the safe default;
  Claude should reach for it without asking.

- **Install (prod):** `scripts/rust-install.sh --prod` — builds
  (`./build-rs-prod`) and installs the user's working `Nice`. It never
  force-quits a running prod Nice (swaps the bundle in place; the
  running process picks up the new version on next relaunch). Only run
  with explicit user authorization.

- **Uninstall:** `scripts/uninstall.sh` defaults to `Nice Dev`. Pass
  `--prod` only with explicit authorization.

## Validating in the real app (never against live state)

To exercise the GUI/behavior, launch the installed **`Nice Dev`** bundle
binary **directly** (not `open`, not `cargo run`) under a scratch
environment so it never touches the live session's state:

```sh
HOME=<scratch> \
NICE_APPLICATION_SUPPORT_ROOT=<scratch>/support \
NICE_PROD_SETTINGS_DOMAIN=<scratch-domain> \
"/Applications/Nice Dev.app/Contents/MacOS/Nice Dev"
```

**Never** run a bare `cargo run -p nice` / plain unbundled launch — the
unbundled fallback resolves state to the user's LIVE prod
`~/Library/Application Support/Nice/` + `~/.claude`. Keep the display
awake for screenshots (`caffeinate -d`).

## Before killing a running Nice

Even with the two-variant split, confirm which variant is running
before killing anything. **Do NOT use `pgrep`** — on macOS a GUI app's
`comm` is the full exec path truncated to 16 chars (`/Applications/Ni`),
so `pgrep`/`pgrep -f` silently MISS a running prod Nice and report a
false "not running". Use the `nice-process-check` skill
(`~/.claude/skills/nice-process-check/check.sh`), or `ps` directly:

```sh
snap="$(ps -Aww -o pid=,args=)"
printf '%s\n' "$snap" | grep -E '/Applications/Nice\.app/Contents/MacOS/Nice( |$)'  # prod
printf '%s\n' "$snap" | grep -E 'Nice Dev\.app/Contents/MacOS/Nice Dev( |$)'        # dev (incl. build-dir)
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

- `scripts/rust-install.sh --prod` and `scripts/uninstall.sh --prod` —
  destructive to the user's working install. Confirm first.
- `scripts/rust-install.sh` / `scripts/uninstall.sh` (no flag) — dev
  only; safe to run after announcing (hold the worktree lock).
- A bare `cargo run -p nice` / plain unbundled launch — resolves state
  to the user's LIVE prod Application Support + `~/.claude`. Never do
  this for validation; use the scratch-env dev-bundle launch above.
- `pkill -x Nice`, `killall Nice`, `kill <pid>` targeting prod Nice —
  confirm first (`Nice Dev` has a different process name, `Nice Dev`).
- `rm`/`mv` against `/Applications/Nice.app` — confirm first.
  `/Applications/Nice Dev.app` is safe to remove.

If the user has already authorized the action in the current turn
(e.g. "reinstall Nice"), you may proceed without re-asking.
Authorization does not carry across unrelated tasks.
