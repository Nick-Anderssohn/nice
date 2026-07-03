You're doing a feature task on a Rust GPUI app. Work in the git worktree at `/Users/nick/Projects/nice/.claude/worktrees/ab-rust` (branch `ab-rust-arm`, already checked out and building). The app is the terminal proof-of-concept in `spikes/phase0-poc` — Rust on the GPUI framework (vendored at `spikes/phase0-poc/vendor/gpui`, v0.2.2).

## Feature request

Add a small **activity badge** to the window chrome, adjacent to the existing top-bar content: a dot plus a `NN KB/s` label showing terminal output throughput over a rolling ~1 s window. While bytes are arriving the badge renders in the active/accent style; after ~2 s of silence it dims to the idle style. **Clicking** the badge toggles between the full (dot + label) and compact (dot-only) presentation, and the chosen presentation **persists across app relaunch**. Match the surrounding chrome's visual style (colors, typography, spacing). Do not regress existing behavior.

## Repo facts

- Worktree: `/Users/nick/Projects/nice/.claude/worktrees/ab-rust`; the app lives in `spikes/phase0-poc`.
- The interactive window is built in `spikes/phase0-poc/src/gpui_term.rs`, `run_interactive()`.
- Build/run the live window: `NICE_POC_RUN=1 NICE_POC_INTERACTIVE=1 cargo run --bin gpui-term` (both env vars required — `NICE_POC_INTERACTIVE=1` alone is a headless self-test with no window). `spikes/phase0-poc/README.md` documents the run modes.

## Definition of done

- Compiles clean — zero errors.
- The feature works per the request above, including every stated behavior; verify by building and running the app yourself.
- Existing behavior unregressed: the existing `gpui-term` headless modes still build and run.
- Code matches the style and idiom of the files it touches.

## House rules

- Stay inside `spikes/phase0-poc` (reading other repo code is fine if genuinely useful). `notes/` at the repo root is out of scope — do not read or modify anything under it.
- This machine hosts live production apps — don't quit, kill, or otherwise touch processes that aren't yours (in particular anything named `Nice`).
- Work offline: no web search / web fetch.
- Do not commit, branch, or otherwise touch git state; leave all changes uncommitted in the working tree.
- Budget: at most 100 tool-call turns and 3 hours of wall clock. If you're about to run out, stop cleanly, leave the tree in its best state, and report status honestly.

## Final report

Report: files changed; how you verified each stated behavior (throughput tracking, dimming, click toggle, relaunch persistence, no regressions); test results; known gaps or caveats. Your final message is a factual report, not a chat reply.
