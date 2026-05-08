# Show pty output when a pane exits before the user can read

## Trigger / repro

In a Nice terminal pane, run `claude -w <name>` outside a git repo.
Claude prints "fatal: not a git repository" (or whatever) to stderr,
exits non-zero. The Nice Claude tab opens, the error scrolls in, the
pane exits, and Nice immediately closes the tab — the user sees a tab
pop in and pop out and reasonably concludes Nice crashed.

Same shape applies to any quick-exit:
- `claude --bad-flag`
- `claude` when the binary isn't on PATH from a login shell
- claude failing to reach anthropic
- vim/less/editor pane that errors out before showing any UI

## Why

`SessionsModel.paneExited(tabId:, paneId:, exitCode:)` at
`Sources/Nice/State/SessionsModel.swift:274` receives the exit code
but ignores it. It just:

1. Removes the pane from the tab model.
2. Removes the pane's pty session.
3. If the tab has no panes left, fires `onTabBecameEmpty` → tab is
   dissolved (model + UI gone).

The SwiftTerm view's scrollback (which still holds Claude's actual
error output) is destroyed alongside the pane. The user never gets
a chance to read it.

## What we want

Stay on the dead pane long enough for the user to read what claude
(or whatever was running) said. The pattern modern terminal apps
converged on:

- **iTerm2** — prints `[Process completed]` on a new line, leaves the
  pane open, closes only on user-initiated close.
- **Ghostty** — similar; "Process exited (1)" footer, pane stays.
- **VS Code's terminal** — prints "The terminal process terminated
  with exit code: X" and waits.

## Approach options

1. **Pane-level overlay** — keep the pane object alive after exit,
   show an overlay similar to `installLaunchOverlayHooks` at
   `Sources/Nice/Process/TabPtySession.swift:364` saying "claude
   exited with status X — press any key (or click) to dismiss".
   Pane teardown defers to user action. Symmetric with the existing
   "Launching <command>..." overlay.
2. **Footer line written into the pty buffer** — write a final
   `[claude exited (1) — close this tab to dismiss]` line directly
   into the SwiftTerm view, then leave the pane in the model so the
   scrollback stays intact. Simpler — no new state machine, just a
   teardown delay + a `view.feed(...)` of the footer string.
3. **Conditional hold** — if the pane exited fast (< some threshold,
   ~2s) OR with non-zero status, hold; if it exited cleanly after a
   longer session, close immediately as today. Matches user intent —
   `vim` exiting cleanly after 30 minutes should still drop the pane.

Combining 2 and 3 is probably the answer: write a footer line, hold
the pane open, and only auto-dissolve under specific success
conditions.

## Code touchpoints

- `Sources/Nice/State/SessionsModel.swift:274` — `paneExited`. The
  `exitCode` param exists but is unused; this is where the hold-vs-
  dissolve decision should live.
- `Sources/Nice/Process/TabPtySession.swift:53,151,159` — the
  `onPaneExit` callback already plumbs the exit code through; no
  signature changes required upstream of `paneExited`.
- `Sources/Nice/Process/TabPtySession.swift:364`
  (`installLaunchOverlayHooks`) — pattern to mirror if we go with
  Option 1. The lifecycle ("show until X, then hide") is the same;
  trigger swaps from `onFirstData` to `onProcessExit`.
- `Sources/Nice/Process/TabPtySession.swift:417` (`terminatePane`) —
  user-initiated close of a held-open pane should route here so the
  existing SIGHUP→SIGKILL ladder still applies if there's somehow a
  resurrected child (there shouldn't be, but be defensive).

## Open questions for the next conversation

1. What's the right threshold for "exited too fast"? A pane that ran
   for an hour and then `exit 1`'d probably shouldn't be held; one
   that ran for 200ms and `exit 1`'d should be. Or do we always
   hold non-zero exits regardless of duration?
2. Do Claude panes and Terminal panes deserve different defaults?
   Claude exits are almost always notable (the user wanted an
   interactive session and didn't get one); Terminal panes
   running ad-hoc commands may legitimately exit fast and silently.
3. The failed-spawn case (claude binary not found, zsh -ilc itself
   failed, pty couldn't be allocated) — should it get its own
   treatment? Today these all surface as "pane exited with some
   code" at the same site; we could differentiate via `view.process`
   not even reaching `onFirstData`.
4. Where should the dismiss affordance live? Click-the-pane,
   any-keypress, a small "[ Dismiss ]" pill, or just rely on the
   user closing the tab?
5. Multi-pane tabs (Claude pane + companion Terminal pane): if only
   the Claude pane dies and the Terminal pane is alive, do we still
   surface the Claude exit somehow? Currently the tab survives
   (Terminal pane keeps it alive); the Claude-pane death is
   silently dropped.

## Repro context

Found while testing the ZDOTDIR fix (commit `d92332a Restore user's
intended ZDOTDIR before sourcing their .zshrc`). `claude -w foo` in
a non-repo cwd was the trigger. The ZDOTDIR work is unrelated; this
gap has been latent in the Claude-pane lifecycle since at least the
two-pane tab refactor.
