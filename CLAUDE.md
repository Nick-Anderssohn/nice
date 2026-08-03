# Nice — working rules for Claude

Rust + GPUI macOS app (Cargo workspace at repo root; crates `nice`, `nice-term-core`, `nice-term-input`, `nice-term-view`, `nice-model`, `nice-theme`, `nice-harness`, `nice-itests`). No Xcode project — old `scripts/install.sh`/`scripts/test.sh`/`xcodebuild`/`project.yml`/`UITests/` are gone.

GPUI/zed is vendored: `scripts/vendor-zed.sh` produces `vendor/zed/` (pinned zed checkout + `patches/*.patch`). 7 patches, `nice` crates path-depend on this:
- `zed-bg-luminance`
- `zed-configurable-blur` — modern-macOS blur radius
- `zed-display-link-selfheal`
- `zed-force-width-exact`
- `zed-translucent-dst-alpha` — correct dst-alpha blending for translucent windows
- `zed-external-drag-out` — makes the window's GPUIView an `NSDraggingSource` so files can be dragged OUT to other apps (stock gpui is drag-destination-only)
- `zed-1x-crisp-text` — whole-pixel glyph placement, no smoothing dilation, 0.3-strength composition curve on 1x displays (text renders fat/wide there otherwise); `NICE_1X_TEXT_CURVE` tunes it; retina untouched

## Two Nice builds — which one Claude can touch

- **`/Applications/Nice.app`** (`dev.nickanderssohn.nice`) — user's working install, hosts live Claude Code sessions incl. this one. **Claude MUST NOT build, install, test, uninstall, or kill this build** except with explicit per-task authorization ("install prod", "promote a release", "reinstall Nice" — must unambiguously name the production install). `--prod` on `scripts/rust-install.sh`/`scripts/uninstall.sh` is destructive to shared user state — always confirm first.

- **`/Applications/Nice Dev.app`** (`dev.nickanderssohn.nice-dev`) — dev build. Own UserDefaults/CFPreferences domain, own Application Support (`~/Library/Application Support/Nice Dev/`), own build dir (`./build-rs`). Rebuilding/killing is safe by default (can't affect the user's real session host) — still announce before doing it (user may have a demo/manual test running).

## Running builds and tests

- **Build:** `cargo build --workspace`. Worktrees share the MAIN checkout's `target/` (gitignored `.cargo/config.toml` from `scripts/worktree-link-vendor.sh` redirects `target-dir`), so a fresh worktree reuses built vendored gpui + deps instead of ~10min cold compile. No worktree lock needed — cargo's build-dir lock serializes concurrent builds (blocks with "waiting for file lock", doesn't fail). Trees whose `nice-*` sources differ will rebuild those crates when alternating; heavy deps stay warm.

- **Tests:** `cargo test --workspace` (unit + in-process scenarios), `cargo test -p nice-itests` (integration). Plain `cargo test` needs no worktree lock. Live GUI self-test scenarios / black-box harnesses driving an installed `Nice Dev.app` DO need the lock (contend on shared dev bundle). During fix rounds, run only targeted tests for touched modules.

- **Install (dev, default):** `scripts/rust-install.sh` — builds `./build-rs`, installs `Nice Dev`, force-quits a running `Nice Dev` first. Run under the worktree lock (`worktree-lock` skill; hold through the whole install+test window). Safe default — use without asking.

- **Install (prod):** `scripts/rust-install.sh --prod` — builds `./build-rs-prod`, installs the user's working `Nice`. Never force-quits a running prod Nice (swaps bundle in place; picked up on next relaunch). Only with explicit user authorization.

- **Uninstall:** `scripts/uninstall.sh` defaults to `Nice Dev`. `--prod` only with explicit authorization.

## Validating in the real app (never against live state)

Launch the installed **`Nice Dev`** bundle binary **directly** (not `open`, not `cargo run`) under a scratch environment:

```sh
HOME=<scratch> \
NICE_APPLICATION_SUPPORT_ROOT=<scratch>/support \
NICE_PROD_SETTINGS_DOMAIN=<scratch-domain> \
"/Applications/Nice Dev.app/Contents/MacOS/Nice Dev"
```

**Never** run a bare `cargo run -p nice` / plain unbundled launch — it resolves state to the user's LIVE prod `~/Library/Application Support/Nice/` + `~/.claude`. Keep the display awake for screenshots (`caffeinate -d`).

## Before killing a running Nice

Confirm which variant is running first. **Do NOT use `pgrep`** — on macOS a GUI app's `comm` is the full exec path truncated to 16 chars (`/Applications/Ni`), so `pgrep`/`pgrep -f` silently MISS a running prod Nice and report a false "not running". Use the `nice-process-check` skill (`~/.claude/skills/nice-process-check/check.sh`), or `ps` directly:

```sh
snap="$(ps -Aww -o pid=,args=)"
printf '%s\n' "$snap" | grep -E '/Applications/Nice\.app/Contents/MacOS/Nice( |$)'  # prod
printf '%s\n' "$snap" | grep -E 'Nice Dev\.app/Contents/MacOS/Nice Dev( |$)'        # dev (incl. build-dir)
```

Killing prod `Nice` **requires explicit permission every time** — loses the user's live session work. Killing `Nice Dev` is lower-stakes but still announce it. Prefer a graceful quit:

```sh
osascript -e 'tell application "Nice" to quit'
osascript -e 'tell application "Nice Dev" to quit'
```

Only escalate to `pkill`/SIGKILL with explicit user consent.

## Quick reference: actions on these builds

| Action | Rule |
|---|---|
| `scripts/rust-install.sh --prod`, `scripts/uninstall.sh --prod` | Destructive to working install — confirm first |
| `scripts/rust-install.sh` / `scripts/uninstall.sh` (no flag) | Dev only, safe after announcing, hold worktree lock |
| Bare `cargo run -p nice` / unbundled launch | Resolves to LIVE prod state — never use for validation; use scratch-env dev-bundle launch above |
| `pkill -x Nice`, `killall Nice`, `kill <pid>` on prod Nice | Confirm first (dev's process name is the distinct `Nice Dev`) |
| `rm`/`mv` on `/Applications/Nice.app` | Confirm first. `/Applications/Nice Dev.app` is safe to remove |

If the user already authorized the action this turn (e.g. "reinstall Nice"), proceed without re-asking. Authorization does not carry across unrelated tasks.

## Rust / GPUI gotchas

- **Never call an AppKit API that pumps the run loop before returning while holding a gpui `App`/entity borrow.** Test: does it drain events before returning control? Blocking calls — `runModal` (`NSOpenPanel`, `NSSavePanel`), `NSWorkspace activateFileViewerSelectingURLs:`, `NSWorkspace openURL(s):…`, any Finder/Launch-Services service — spin a nested run loop while waiting, which drains gpui's main dispatch queue, waking a queued foreground task that calls `AppCell::borrow_mut` while your `cx.update` borrow is still live → `"already borrowed"` panic → `SIGABRT`. Caused the Reveal-in-Finder crash (`4342e23`) and Import-theme crash (`21f7249`). Two safe shapes:
  - **Fire-and-forget** (open/reveal/open-with): defer the OS call to its own main-queue turn so no borrow is held when it runs. `NSWorkspace` wrappers in `platform.rs` already do this via `defer_to_main` — call them from anywhere.
  - **Value-returning modal** (`workspace_choose_application`, `choose_theme_file`): can't defer. Present from a borrow-free context — `cx.spawn(...)` / `app.spawn(...)`, re-enter with `acx.update(...)` for the result. See `perform_import` (`appearance_pane.rs`) and the "Other…" handler (`view.rs`).

  **Exception — do NOT "fix":** OS drag-out (`Window::begin_external_paths_drag`, `zed-external-drag-out` patch → `beginDraggingSessionWithItems:event:source:`) is called at `view.rs` INSIDE a live borrow (gpui's `on_drag` closure) and is safe because that call returns immediately and pumps the drag on later run-loop turns, after the borrow releases — never overlaps the borrow. Deferring or spawning it would BREAK it: it needs `[NSApp currentEvent]` to be the live mouse-down/dragged event, which a deferred turn doesn't have (the patch refuses — returns `false` — in that case). Leave it under the borrow. If swapped for a blocking drag API, the rule above applies again.

- **In GPUI tests, use the executor timer, not `smol::Timer`.** For timeouts/delays driven by `run_until_parked()`, use `cx.background_executor().timer(duration).await` (or `cx.background_executor.timer(...)` on `TestAppContext`). `smol::Timer::after(...)` isn't tracked by gpui's dispatcher, so `run_until_parked()` can report "nothing left to run" and the delayed work never fires. (From the vendored zed's own guidelines.)
