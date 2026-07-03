You're doing a feature task on a Rust GPUI app. Work in the git worktree at `/Users/nick/Projects/nice/.claude/worktrees/ab-rust` (branch `ab-rust-arm`, already checked out and building). The app is the terminal proof-of-concept in `spikes/phase0-poc` — Rust on the GPUI framework (vendored at `spikes/phase0-poc/vendor/gpui`, v0.2.2).

## Feature request

Add a **bottom status bar** to the main window: full window width, ~28 pt tall, visually matched to the existing chrome (colors, typography, spacing). Left side: a text widget showing the active session's working directory (a sensible static placeholder is acceptable if the codebase does not expose a cwd). Right side: a clock widget (HH:MM, updating each minute). Behavior: (1) clicking the left widget copies its text to the clipboard with a brief visible confirmation; (2) pressing and dragging on any empty (non-widget) area of the bar **moves the window**, exactly like the title bar; (3) double-clicking an empty area performs the same action as double-clicking the title bar (honoring the user's system preference where the platform exposes one); (4) pressing, dragging, or clicking the widgets must **never** move the window. Do not regress existing behavior — in particular existing window-drag surfaces, pill interactions, and terminal input.

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

Report: files changed; how you verified each stated behavior (including the never-moves-the-window and no-regression items); test results; known gaps or caveats. Your final message is a factual report, not a chat reply.
