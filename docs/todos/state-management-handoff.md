# State management refactor — handoff

Phase 1 (the `@Observable` migration) and all of Phase 2 — including
the view-side rename pass — are **done**. The refactor outlined in
[`state-and-AppState-refactor.md`](state-and-AppState-refactor.md) is
complete.

## Status

Branch: `worktree-state-management`. Worktree:
`/Users/nick/Projects/nice/.claude/worktrees/state-management`. All
Phase 2 work landed on this branch.

Recent commits on top of `main`:

- `220e708` — Add state-management refactor plan
- `6689548` — Phase 1: Migrate state management to the @Observable macro
- `1fa3d87` — Phase 1 cleanup: extract side effects into start() / bootstrap()
- `1b4e0fe` — Phase 2 step 1: Extract TabModel
- `c39cf3b` — Phase 2 step 2: Extract SessionsModel
- `c595e88` — Phase 2 step 3: Extract SidebarModel
- `27949c1` — Phase 2 step 5: Extract CloseRequestCoordinator
- `4e2f94b` — Phase 2 step 4: Extract WindowSession
- _step 6_ — Phase 2 step 6: View-side rename pass

707 tests pass (664 unit + 43 UI) at every commit.

The plan in
[`state-and-AppState-refactor.md`](state-and-AppState-refactor.md)
remains the authoritative outline.

## What's extracted

`Sources/Nice/State/`:

| File | Lines | Owns |
|---|---:|---|
| `TabModel.swift` | 656 | `projects`, `activeTabId`, lookup, reordering, project-structure repair, cwd resolution, kebab→sentence title humanization, static path/arg helpers |
| `SessionsModel.swift` | 929 | `ptySessions`, `paneLaunchStates`, control socket, theme caches, launch overlay, pane lifecycle handlers, pane management, tab creation w/ spawn, `focusActiveTerminal` |
| `SidebarModel.swift` | 61 | `sidebarCollapsed`, `sidebarMode`, `sidebarPeeking` + 3 toggle methods |
| `CloseRequestCoordinator.swift` | 286 | `pendingCloseRequest`, `projectsPendingRemoval`, `requestClose×3`/`confirm`/`cancel`, `isBusy`, `hardKill×3` |
| `WindowSession.swift` | 426 | `windowSessionId`, `persistenceEnabled`, `isInitializing`, static `claimedWindowIds`, `scheduleSessionSave`, `snapshotPersistedWindow`, `restoreSavedWindow`, `ensureTerminalsProjectSeededAndSpawn`, `addRestoredTabModel`, persistence-half `tearDown` |
| `AppState.swift` | 721 | Composition root: holds the five sub-models, wires their callbacks, owns `fileBrowserStore` and the `toggleFileBrowserHiddenFiles` orchestration that spans sidebar+store, has the `start()`/`tearDown()` choreography, runs `finalizeDissolvedTab`, retains a thin set of test-only forwarders for the unit suite |

After step 6, views read sub-models directly:
- `AppShellView` injects `tabs`, `sessions`, `sidebar`, `closer`, and
  `windowSession` into the environment alongside `appState` itself
  (kept for the `start()`/`tearDown()` lifecycle hooks and the
  cross-cutting `fileBrowserStore` / `AppState+FileExplorer` surface).
- `SidebarView`, `WindowToolbarView`, and the inner `ProjectGroup` /
  `TabRow` / `InlinePaneStrip` views declare exactly the sub-models
  they observe.
- `FileBrowserView` reads `tabs` for `activeTabId` / `tab(for:)` /
  `fileBrowserHeaderTitle`, and keeps `appState` only for
  `fileBrowserStore` and the `AppState+FileExplorer` action surface
  (`cutPaths`, `openFromDoubleClick`, `moveOrCopy`).
- `KeyboardShortcutMonitor` and `FileOperationHistory` route through
  `appState.<sub-model>` paths instead of the AppState forwarders.

The remaining `AppState` forwarders are documented as test-only and
pass straight through to a sub-model. A future cleanup PR can
migrate the unit suite to call sub-models directly and delete those
forwarders entirely.

## Callback wiring conventions

A pattern emerged across the five extractions:

- A sub-model holds a **weak** reference to the sub-models it reads
  from (`SessionsModel.tabs`, `CloseRequestCoordinator.tabs` and
  `.sessions`, `WindowSession.tabs`/`.sessions`/`.sidebar`). Cycle
  insurance — they're co-owned by AppState and share its lifetime.
- A sub-model exposes **`@ObservationIgnored var on…: (...) -> Void`**
  callbacks that AppState wires in `init` (`[weak self] in self?…`).
  Used when the sub-model needs to fan an event out to a concern it
  doesn't own (persistence, dissolve cascade, file-browser cleanup).
- Persistence saves are routed through `WindowSession.scheduleSessionSave`,
  gated internally on `isInitializing` and `persistenceEnabled`.
  Sub-models that care call `onSessionMutation?()` (or, via TabModel's
  `onTreeMutation`, whatever name the model uses internally); AppState
  forwards into `windowSession.scheduleSessionSave()`.

Current callbacks wired in `AppState.init`:

```swift
tabs.onTreeMutation         = { [weak self] in self?.windowSession.scheduleSessionSave() }
sessions.onSessionMutation  = { [weak self] in self?.windowSession.scheduleSessionSave() }
sessions.onTabBecameEmpty   = { [weak self] tabId, pi, ti in
    self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
}
closer.onSyncFinalizeDissolve = { [weak self] tabId, pi, ti in
    self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
}
closer.onScheduleSave       = { [weak self] in self?.windowSession.scheduleSessionSave() }
```

The `isInitializing` save-gate is released by AppState's `start()`
calling `windowSession.markInitializationComplete()` after
`restoreSavedWindow()` has populated the tree.

## Lessons learned

### Split read/clear when intermediate state spans multiple events

Step 5's first cut had `closer.consumeProjectPendingRemoval(_:)` —
read-and-clear in one call. That broke
`test_requestCloseProject_idleProject_removesProjectAndAllTabs`:
when closing a project with multiple tabs, the *first* tab to dissolve
cleared the flag, leaving subsequent dissolves unable to see it. The
fix was to split into `isProjectPendingRemoval` (read) +
`clearProjectPendingRemoval` (clear), and have AppState's
`finalizeDissolvedTab` only clear when the project is actually being
removed. Lesson: when a flag is consulted across multiple async
events, expose read and clear as separate operations on the model.

### Construct sub-models in `init`, not `start()`

Many unit tests construct `AppState()` via the convenience init,
never call `start()`, and rely on the seed Main tab being present.
Every sub-model has been constructed inside AppState.init — TabModel
builds the seed tab, SessionsModel takes a TabModel reference but
spawns nothing until `start()`. Keep that contract: the
construct-without-`start()` instance must be a usable data model.

### Don't fire callbacks from inside the sub-model's init

A few times I tripped over this: sub-models that have `didSet`
observers (e.g. TabModel.activeTabId) can fire those during their
own `init` for optional-typed assignments. AppState wires callbacks
*after* `tabs = TabModel(...)`, so the seed assignment doesn't bounce
through a partially-constructed AppState. `scheduleSessionSave`'s
`isInitializing` gate covers any straggling fires.

### SourceKit diagnostics lag behind file creation

After creating a new file under `Sources/Nice/State/`, SourceKit will
spit out "Cannot find type X" errors for several minutes — even after
`xcodegen generate`. The actual build (`scripts/test.sh`) is
authoritative. Ignore the inline diagnostics; trust the test run.

### Save full test output to a file

UI flakes happen. When `scripts/test.sh` fails and you've grep-piped
the output for summaries, you've thrown away the failure detail. Pipe
to a log file first, then grep:

```sh
scripts/test.sh > /tmp/nice-test.log 2>&1
echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
# If failed, grep the full log for the failing case:
grep -E "Test Case .* failed|XCTAssert|: error:" /tmp/nice-test.log
```

## Follow-up work

The refactor is functionally complete. One housekeeping follow-up is
spec'd separately and ready to start:

- [`state-management-test-migration.md`](state-management-test-migration.md)
  — migrate ~425 `appState.X` accessors in `Tests/NiceUnitTests/` to
  the most specific sub-model and delete the now-unused AppState
  forwarders. AppState would drop from 721 → ~250 lines. Mechanical
  sweep with a precise migration table; expected to land in 1–2
  commits.

## Acceptance criteria — Phase 2 ✅

- [x] `TabModel`, `SessionsModel`, `SidebarModel`,
      `CloseRequestCoordinator`, `WindowSession` each in their own
      file under `Sources/Nice/State/`.
- [x] No view reads `appState.<x>` for a `<x>` that lives on a
      sub-model — views declare `@Environment(<SubModel>.self)` and
      read the sub-model directly. AppState stays in scope only
      where genuinely cross-cutting (lifecycle hooks +
      `fileBrowserStore` + `AppState+FileExplorer` actions).
- [x] `scripts/test.sh` green (664 unit + 43 UI = 707 tests).
- [x] `#Preview` blocks compile and inject every sub-model the
      previewed view observes.

## Useful commands

```sh
# From the worktree:
scripts/worktree-lock.sh acquire phase2-followup
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release

# Single-test fast loop:
scripts/test.sh -only-testing:NiceUnitTests/AppStateSerializationTests > /tmp/x.log 2>&1

# Acceptance grep for Phase 1 (must stay clean):
grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/

# Acceptance grep for Phase 2 step 6 (must stay clean — only
# `FileBrowserView` keeps `@Environment(AppState.self)` for the
# cross-cutting `fileBrowserStore` + `AppState+FileExplorer` surface):
grep -rn "@Environment(AppState.self)" Sources/Nice/Views/

# Find remaining AppState forwarder users (test-suite migration entry):
grep -rE "appState\.[a-zA-Z]+" Tests/NiceUnitTests/ | sed -E 's/.*appState\.([a-zA-Z_]+).*/\1/' | sort -u

# Install Nice Dev for manual smoke:
scripts/install.sh    # under the worktree lock — see CLAUDE.md
```
