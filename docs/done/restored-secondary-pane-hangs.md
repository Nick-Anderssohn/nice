# Bug: restored secondary terminal panes hang on "Launching terminal…"

## Symptom

After a relaunch where session.json holds at least one tab with two
or more panes, only the pane that was *active at last quit* spawns
correctly. Clicking any other pane pill in the toolbar shows the
"Launching terminal…" overlay and never lifts — first-byte never
arrives, the underlying zsh appears to start but produces no output
the app reads. Affects both terminal-only tabs and Claude tabs (the
companion-terminal pane in a Claude tab hangs the same way).

## Reproduction

1. Open Nice Dev with a tab that has 2+ panes (e.g. open a tab,
   click `+` to add a second terminal pane, optionally use both,
   focus pane 1, quit).
2. Relaunch Nice Dev. Pane 1 (the previously-active one) renders
   normally.
3. Click pane 2's pill in the toolbar.
4. Pane 2 shows the centered "Launching terminal…" overlay
   indefinitely. Cmd-clicking another pane and back doesn't recover.

## What this is NOT

This is **not** a regression introduced by the state-management
refactor (the Phase 3 / Phase 3-followup work that landed on
`worktree-state-management`). Verified by:

- Production code diff vs `main` for the spawn path: the
  `setActivePane` → `ensureActivePaneSpawned` → `addTerminalPane`
  chain is byte-for-byte identical between `main` and the branch.
  Only the persistence injection (`SessionStore.shared` →
  injected `store`), the `SessionThemeCache` extraction, and the
  `TerminalsSeedResult` → `spawnHook` swap touched production code,
  and none touch the secondary-pane spawn path.
- Reproduces on a `main` build (`scripts/install.sh` from the
  main worktree).
- Reproduces on `Nice Dev` v0.14.0 on this machine.

## What's strange

**The bug does not reproduce on every machine.** The same v0.14.0
build:

- Hangs on this machine's `Nice Dev` install
  (`/Applications/Nice Dev.app`,
  `dev.nickanderssohn.nice-dev`).
- Works fine on this machine's *production* `Nice` install
  (`/Applications/Nice.app`, `dev.nickanderssohn.nice`).
- Works fine on the user's other computer entirely.

So the failure is environmental — bundled to this machine's
`Nice Dev` install — even though the code is the same. That points
at one of: machine-local state under
`~/Library/Application Support/Nice Dev/`, UserDefaults domain
`dev.nickanderssohn.nice-dev`, accumulated leaked subprocesses
(see below), TCC/macOS permissions for the dev bundle ID, or
something else machine-specific.

## Diagnostic facts gathered

### Leaked subprocesses

A `ps -axo pid,ppid,user,command | awk '$2==1 && $4=="/bin/zsh"'`
on this machine showed **234 orphan zsh processes** with PPID 1
— i.e. their parent (`Nice Dev`) exited but the children survived
and were re-parented to launchd. They sit in `/bin/zsh -il` waiting
on stdin from a pty whose master end is no longer open.

This count grows across launches. Each click that hangs probably
contributes one new orphan when `Nice Dev` next quits without
terminating the child.

It's plausible (but unverified) that the orphans are *causing* the
hang — e.g. they're holding pty/tty slots, file descriptors, named
semaphores, or some other shared resource the new spawn needs.
That'd explain why this machine fails and others don't (the others
haven't accumulated the orphans).

`pkill -9 'zsh -il'` would clear the orphans but would also kill
the user's real shells if they're running zsh interactively — so
test it carefully (e.g. only kill PIDs whose PPID is 1).

### Overlay state machine

The "Launching terminal…" overlay shows when
`SessionsModel.paneLaunchStates[paneId] == .visible(command:)`.
The state machine:

- `addTerminalPane` calls `installLaunchOverlayHooks`, which fires
  `onPaneLaunched` → `SessionsModel.registerPaneLaunch` → state
  set to `.pending(command:)`.
- After 0.75s grace, promote to `.visible`.
- On first pty byte, `NiceTerminalView.dataReceived` fires
  `onFirstData` → `onPaneFirstOutput` →
  `SessionsModel.clearPaneLaunch` → state set to nil → overlay
  goes away.

The fact that the overlay shows means:

1. `session.panes[paneId]` *is* populated (otherwise
   `AppShellView.mainContent`'s `let view = session.panes[paneId]`
   guard fails and renders only `terminalBackgroundColor` — no
   overlay).
2. `paneLaunchStates[paneId]` was set and promoted to `.visible`.

The fact that the overlay never lifts means: the master fd is
not reading any byte, or `dataReceived` is being called but the
override in `NiceTerminalView` isn't firing `onFirstData`.

## Code paths to navigate

| File | Symbol | What it does |
|---|---|---|
| `Sources/Nice/Views/WindowToolbarView.swift:246` | onTap → `sessions.setActivePane(...)` | Click handler |
| `Sources/Nice/State/SessionsModel.swift` | `setActivePane(tabId:paneId:)` | mutates `tab.activePaneId`, then calls `ensureActivePaneSpawned` |
| `Sources/Nice/State/SessionsModel.swift` | `ensureActivePaneSpawned(tabId:)` | guards on `session.panes[paneId] == nil`, calls `session.addTerminalPane(id:cwd:)` |
| `Sources/Nice/Process/TabPtySession.swift:261` | `addTerminalPane(id:cwd:...)` | creates `NiceTerminalView(frame: .zero)`, sets delegate, calls `installLaunchOverlayHooks`, calls `view.startProcess(...)` |
| `Sources/Nice/Process/NiceTerminalView.swift:122` | `dataReceived(slice:)` override | calls `super.dataReceived`, then fires `onFirstData` once |
| `Sources/Nice/Views/AppShellView.swift:584` | `mainContent` | renders `TerminalHost(view: view)` + overlay if `.visible` |
| `Sources/Nice/Views/LaunchingOverlay.swift` | overlay UI | "Launching terminal…" |

## Hypotheses worth testing (cheapest first)

1. **Clear Application Support state.** Quit `Nice Dev`, kill all
   orphan zsh's whose PPID is 1, `rm
   ~/Library/Application\ Support/Nice\ Dev/sessions.json`,
   relaunch. If the bug stops reproducing, it's something in the
   restore path tied to *specific* persisted state on this machine.
   Repro it again to capture the exact sessions.json that triggers
   it.
2. **Clear UserDefaults.** `defaults delete dev.nickanderssohn.nice-dev`
   (after backing up). If the bug stops, it's a Tweaks/font/theme
   value the dev defaults domain holds that prod doesn't.
3. **Kill all orphan zsh's first, leave persisted state alone.**
   `ps -axo pid,ppid,command | awk '$2==1 && $3=="/bin/zsh"
   {print $1}' | xargs -r kill -9` — then relaunch. If the bug
   stops reproducing, accumulated orphans are blocking new spawns
   (resource leak). That points at a real fix: terminate child
   processes on `tearDown` / `deinit` so we never orphan in the
   first place.
4. **Compare Console.app stderr** between Nice and Nice Dev when
   reproducing. Filter `process == 'Nice Dev'`. Any startProcess
   error, posix_spawn EAGAIN/EMFILE, or SwiftTerm internal log
   would localize this fast.
5. **Add a breakpoint / NSLog in `addTerminalPane`** right after
   `view.startProcess` to confirm the call returns. Add another in
   `NiceTerminalView.dataReceived` to confirm bytes never arrive
   for the failing pane (vs arrive but `onFirstData` doesn't fire).
6. **Send a probe byte manually.** After `view.startProcess`,
   feed `view.feed(text: "\n")` from a temp debug button — does
   that paint? That separates "pty unidirectional broken" from
   "view not in hierarchy" issues.

## Hypotheses ruled out

- **Phase 3 / Phase 3-followup state-management refactor.** Diff
  is unrelated to this path. Verified.
- **The view starts at `.zero` frame.** True, but the *active*
  pane is also created with `frame: .zero` and works. Both panes
  get sized later by SwiftUI layout.

## Where to fix

The fix likely lives in one of:

- `TabPtySession`'s lifecycle (terminate children on session
  deinit / app quit so we don't accumulate orphans).
- `addTerminalPane` (something about secondary spawns specifically
  — though no obvious differentiator vs the init-time spawn).
- The interaction between `view.startProcess` and SwiftUI mounting
  (the click happens before the view is mounted in the view
  hierarchy; maybe SwiftTerm needs the view live to pump bytes).

Whoever picks this up: start by reproducing on the user's machine
(the same instance that has 234+ orphans), kill the orphans, see
if the symptom changes. That's the fastest signal.

## Branch / commit context

This handoff is being filed from the `worktree-state-management`
branch (HEAD = `645e7a5`). The bug pre-exists that branch — it's
on `main` (HEAD = `17c7ab3`) too. There is no need to block the
state-management merge on this fix.
