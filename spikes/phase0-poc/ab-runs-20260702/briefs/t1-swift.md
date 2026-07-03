You're doing a feature task in the Nice repo — a macOS terminal app written in Swift (SwiftUI + AppKit). Work in the git worktree at `/Users/nick/Projects/nice/.claude/worktrees/ab-swift` (branch `ab-swift-arm`, already checked out and building).

## Feature request

Add a **bottom status bar** to the main window: full window width, ~28 pt tall, visually matched to the existing chrome (colors, typography, spacing). Left side: a text widget showing the active session's working directory (a sensible static placeholder is acceptable if the codebase does not expose a cwd). Right side: a clock widget (HH:MM, updating each minute). Behavior: (1) clicking the left widget copies its text to the clipboard with a brief visible confirmation; (2) pressing and dragging on any empty (non-widget) area of the bar **moves the window**, exactly like the title bar; (3) double-clicking an empty area performs the same action as double-clicking the title bar (honoring the user's system preference where the platform exposes one); (4) pressing, dragging, or clicking the widgets must **never** move the window. Do not regress existing behavior — in particular existing window-drag surfaces, pill interactions, and terminal input.

## Repo facts

- Worktree: `/Users/nick/Projects/nice/.claude/worktrees/ab-swift`
- The top bar is `Sources/Nice/Views/WindowToolbarView.swift`; the window container is built under `Sources/Nice/`.
- Build + install the dev app with `scripts/install.sh` (no flags), run under the repo's worktree lock (`scripts/worktree-lock.sh acquire install` … `scripts/worktree-lock.sh release`); then launch with `open "/Applications/Nice Dev.app"`. Tests: `scripts/test.sh` (also under the lock; forwards `-only-testing:` args).

## Definition of done

- Compiles clean — zero errors.
- The feature works per the request above, including every stated behavior; verify by building, installing, and running the app yourself.
- Existing behavior unregressed: targeted `scripts/test.sh` suites relevant to your changes are green; the app launches; existing chrome interactions still behave normally.
- Code matches the style and idiom of the files it touches.

## House rules

- The production `/Applications/Nice.app` is installed AND RUNNING on this machine hosting live sessions — never build, install, quit, kill, or otherwise touch it or its processes. Dev work uses `scripts/install.sh` (dev is the default target) and `/Applications/Nice Dev.app` only; never pass `--prod` to any script; never run bare `xcodebuild`/`xcodebuild test` against the `Nice` scheme. Never use `pgrep`/`pkill` for Nice processes (they give false results on macOS); if you need to check what's running, use `ps -Aww -o pid=,args=` and grep. Quit Nice Dev gracefully when you finish: `osascript -e 'tell application "Nice Dev" to quit'`.
- `notes/` at the repo root is out of scope — do not read or modify anything under it. `docs/` (including `docs/research/`) is fair game and worth consulting.
- Work offline: no web search / web fetch.
- Do not commit, branch, or otherwise touch git state; leave all changes uncommitted in the working tree. Leave the worktree lock released when done.
- Budget: at most 100 tool-call turns and 3 hours of wall clock. If you're about to run out, stop cleanly, leave the tree in its best state, and report status honestly.

## Final report

Report: files changed; how you verified each stated behavior (including the never-moves-the-window and no-regression items); test results; known gaps or caveats. Your final message is a factual report, not a chat reply.
