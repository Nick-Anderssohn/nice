# State management — test-suite forwarder migration

Phase 2 of the state-management refactor (the AppState decomposition
+ view-side rename pass) is **done**. This file is the working spec
for the one remaining housekeeping item: migrating `Tests/NiceUnitTests/`
off the `appState.X` forwarders that AppState only keeps alive for
the test suite, and then deleting those forwarders.

The previous phase's diary is in
[`state-management-handoff.md`](state-management-handoff.md). Read
that *first* for the architectural shape; this file assumes you
already understand it.

## Status

Branch: `worktree-state-management`. Worktree:
`/Users/nick/Projects/nice/.claude/worktrees/state-management`.
Most recent commits:

- `47c306f` — Phase 2 step 6: View-side rename pass
- `0a88f9b` — Update Phase 2 handoff after step 6

707 tests pass (664 unit + 43 UI) at every commit. After this
follow-up they should still pass — no test-coverage changes here,
just a mechanical call-site rewrite.

## What this PR does

1. Migrates ~425 `appState.X` accessors in `Tests/NiceUnitTests/` to
   `appState.<sub-model>.X` (e.g. `appState.tabs.activeTabId`,
   `appState.closer.requestCloseTab(...)`).
2. Migrates the static `AppState.terminalsProjectId` /
   `AppState.mainTerminalTabId` / `AppState.extractWorktreeName(...)`
   references to `TabModel.X` (the source of truth).
3. Deletes the now-unused forwarders from `AppState.swift`.

After this lands, `AppState.swift` should drop from 721 → ~250 lines
and contain only:
- The five sub-model `let`s (`tabs`, `sessions`, `sidebar`, `closer`,
  `windowSession`) plus `fileBrowserStore`, `fileExplorer`,
  `tweaks`, `editorDetector`, `trackedServices`, `started`.
- `init(...)`, `convenience init()`, `start()`, `tearDown()`,
  `armClaudePathTracking()`, `finalizeDissolvedTab(...)` (private),
  `livePaneCounts` (used by `AppDelegate`), `windowSessionId`
  read-only forwarder (used by `WindowRegistry`),
  `toggleFileBrowserHiddenFiles()` (cross-cutting orchestration that
  spans `sidebar` + `fileBrowserStore`).
- `AppState+FileExplorer.swift` extension stays unchanged — its
  surface (`openFromDoubleClick`, `openInEditorPane`, `cutPaths`,
  `moveOrCopy`, `pasteFromPasteboard`, `trash`, `revealInFinder`,
  `editorPaneEntries`, `openWithEntries`, `openWith`,
  `presentOpenWithPicker`, `canPaste`, `copyToPasteboard`,
  `copyPathsToPasteboard`, `cutToPasteboard`) is genuinely on
  AppState, not forwarders.

## The migration table

The unique forwarder surface tests touch today:

| `appState.X` (before)            | Replace with                                | Notes |
|----------------------------------|---------------------------------------------|-------|
| `activeTabId`                    | `tabs.activeTabId`                          | get + set |
| `addPane(...)`                   | `sessions.addPane(...)`                     | |
| `addRestoredTabModel(...)`       | `windowSession.addRestoredTabModel(...)`    | |
| `addTerminalToActiveTab()`       | `sessions.addTerminalToActiveTab()`         | |
| `applyAutoTitle(...)`            | `tabs.applyAutoTitle(...)`                  | |
| `cancelPendingClose()`           | `closer.cancelPendingClose()`               | |
| `clearPaneLaunch(paneId:)`       | `sessions.clearPaneLaunch(paneId:)`         | |
| `confirmPendingClose()`          | `closer.confirmPendingClose()`              | |
| `createTabFromMainTerminal(...)` | `sessions.createTabFromMainTerminal(...)`   | |
| `fileBrowserHeaderTitle(forTab:)`| `tabs.fileBrowserHeaderTitle(forTab:)`      | |
| `fileBrowserStore`               | **keep** (`appState.fileBrowserStore`)      | lives on AppState |
| `handleClaudeSessionUpdate(...)` | `sessions.handleClaudeSessionUpdate(...)`   | |
| `launchOverlayGraceSeconds`      | `sessions.launchOverlayGraceSeconds`        | get + set |
| `livePaneCounts`                 | `tabs.livePaneCounts`                       | |
| `moveTab(...)`                   | `tabs.moveTab(...)`                         | |
| `navigableSidebarTabIds`         | `tabs.navigableSidebarTabIds`               | |
| `openFromDoubleClick(url:)`      | **keep** (`appState.openFromDoubleClick`)   | AppState+FileExplorer |
| `openInEditorPane(...)`          | **keep** (`appState.openInEditorPane`)      | AppState+FileExplorer |
| `paneCwdChanged(...)`            | `sessions.paneCwdChanged(...)`              | |
| `paneExited(...)`                | `sessions.paneExited(...)`                  | |
| `paneLaunchStates`               | `sessions.paneLaunchStates`                 | |
| `paneTitleChanged(...)`          | `sessions.paneTitleChanged(...)`            | |
| `pendingCloseRequest`            | `closer.pendingCloseRequest`                | get + set |
| `projects`                       | `tabs.projects`                             | get + set |
| `ptySessions`                    | `sessions.ptySessions`                      | |
| `registerPaneLaunch(...)`        | `sessions.registerPaneLaunch(...)`          | |
| `renameTab(id:to:)`              | `tabs.renameTab(id:to:)`                    | |
| `repairProjectStructure()`       | `tabs.repairProjectStructure()`             | |
| `requestCloseProject(...)`       | `closer.requestCloseProject(...)`           | |
| `requestCloseTab(...)`           | `closer.requestCloseTab(...)`               | |
| `resolvedSpawnCwd(for:)`         | `tabs.resolvedSpawnCwd(for:)`               | |
| `resolvedSpawnCwd(for:pane:)`    | `tabs.resolvedSpawnCwd(for:pane:)`          | |
| `selectNextPane()`               | `sessions.selectNextPane()`                 | |
| `selectNextSidebarTab()`         | `tabs.selectNextSidebarTab()`               | |
| `selectPrevPane()`               | `sessions.selectPrevPane()`                 | |
| `selectPrevSidebarTab()`         | `tabs.selectPrevSidebarTab()`               | |
| `setActivePane(...)`             | `sessions.setActivePane(...)`               | |
| `sidebarCollapsed`               | `sidebar.sidebarCollapsed`                  | get + set |
| `sidebarMode`                    | `sidebar.sidebarMode`                       | get + set |
| `snapshotPersistedWindow()`      | `windowSession.snapshotPersistedWindow()`   | |
| `spawnCwdForNewPane(in:callerProvided:)` | `tabs.spawnCwdForNewPane(in:callerProvided:)` | |
| `tab(for:)`                      | `tabs.tab(for:)`                            | |
| `tearDown()`                     | **keep** (`appState.tearDown()`)            | composition root |
| `toggleFileBrowserHiddenFiles()` | **keep** (`appState.toggleFileBrowserHiddenFiles()`) | cross-cutting |
| `toggleSidebarMode()`            | `sidebar.toggleSidebarMode()`               | |
| `wouldMoveTab(...)`              | `tabs.wouldMoveTab(...)`                    | |

Static rewrites (search the test suite for these too):

| `AppState.X` (before)       | Replace with             |
|-----------------------------|--------------------------|
| `AppState.terminalsProjectId` | `TabModel.terminalsProjectId` |
| `AppState.mainTerminalTabId`  | `TabModel.mainTerminalTabId`  |
| `AppState.extractWorktreeName(from:)` | `TabModel.extractWorktreeName(from:)` |

A handful of tests use a different variable name (`state` instead of
`appState`). Search both. As of writing, only
`AppStateFileOperationsTests.swift` uses `state.windowSessionId` /
`state.activeTabId` (3 hits).

## Working plan

1. **Migrate test files** mechanically. Either Edit per file or do a
   sweeping find-replace per accessor. Recommended order: start with
   the smallest test files to flush out any signature surprise
   (`AppStateRenameTabTests.swift` is small), then run tests early.
2. **Drop the forwarders** from `Sources/Nice/State/AppState.swift`.
   Don't drop the ones marked **keep** in the table above. After the
   delete, the keeper list is exactly what's described in the
   "What this PR does" section.
3. **Run the full suite** under the worktree lock once tests are
   migrated and once forwarders are dropped — two checkpoints.

A reasonable commit shape:

- One commit migrating tests to sub-model calls
  ("Phase 2 follow-up: migrate test surface to sub-model calls").
- One commit deleting now-unused AppState forwarders
  ("Phase 2 follow-up: drop test-only AppState forwarders").
- One commit updating the handoff diary.

A single commit is fine too — fewer than 1k lines of test diff and
~400 lines of AppState delete is well within review tolerance for
this codebase.

## Verification

Use the existing pattern (don't pipe through grep — UI flakes lose
their failure context):

```sh
scripts/worktree-lock.sh acquire phase2-followup
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release
```

Acceptance:

- [ ] No `appState.X` accessor in `Tests/NiceUnitTests/` resolves to
      a forwarder marked **rewrite to sub-model** in the table above.
      Spot-check: `grep -rE "appState\.(activeTabId|projects|sidebarMode|sidebarCollapsed|pendingCloseRequest|ptySessions|paneLaunchStates|tab\()" Tests/` should be empty.
- [ ] No `AppState.terminalsProjectId` / `AppState.mainTerminalTabId`
      / `AppState.extractWorktreeName` in tests.
- [ ] `AppState.swift` ≤ 300 lines.
- [ ] `scripts/test.sh` green (664 unit + 43 UI = 707).
- [ ] No view file regressed onto `appState.X` for any of the
      removed forwarders (compile would catch this — but eyeball
      `Sources/Nice/Views/` once anyway).

## Subtleties to watch

- **`activeTabId` is mutable in tests.** Several tests do
  `appState.activeTabId = AppState.mainTerminalTabId` to seed
  selection. Setter forwards to `tabs.activeTabId`'s `didSet`,
  which fires `acknowledgeWaitingOnActivePane` + `onTreeMutation`.
  `appState.tabs.activeTabId = ...` keeps the same observer chain
  — no behavior change.

- **`projects` mutation in tests.** A few tests build the projects
  tree by hand (`appState.projects = [...]`) before exercising
  behavior. After migration, that's `appState.tabs.projects = [...]`.
  TabModel's `projects` is a plain `var` so the assignment shape is
  identical.

- **`sidebarCollapsed` / `sidebarMode` mutation.** Same as above —
  plain `var`s on `SidebarModel`.

- **`pendingCloseRequest = nil`** in the alert test. `closer.pendingCloseRequest`
  is a plain `var` on `CloseRequestCoordinator`; assignment works
  the same way.

- **`launchOverlayGraceSeconds` is a test seam.** Tests set it to a
  small number to make the launch-overlay grace window deterministic.
  `sessions.launchOverlayGraceSeconds` keeps the same semantics.

- **`AppStateRenameTabTests.swift`** uses
  `appState.snapshotPersistedWindow()` to verify a rename round-trips.
  That migrates to `appState.windowSession.snapshotPersistedWindow()`.

- **`addRestoredTabModel` is `@discardableResult` on AppState.**
  `WindowSession.addRestoredTabModel` is *not* discardable. If a test
  drops the result, it'll need to add `_ = ...` or
  `XCTUnwrap(...)`. Check `AppStateProjectBucketingTests.swift`
  around line 441 — there's one such call.

- **Static `terminalsProjectId` / `mainTerminalTabId` are
  `static var`s on TabModel** (with custom getters) and `static let`s
  on the model proper — both forms are exposed. Tests just need the
  reads, so `TabModel.terminalsProjectId` works identically.

- **The `AppShellView.swift` doc comment** still references
  `appState.sidebarPeeking`. That's a comment, not a call site —
  doesn't affect compilation. Optional cleanup; not required for
  this PR.

## After this PR

The state-management refactor is fully done. The Phase-1 acceptance
grep (`grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/`)
should still be empty, and the Phase-2 step-6 grep
(`grep -rn "@Environment(AppState.self)" Sources/Nice/Views/`)
should still show only `FileBrowserView.swift` (which legitimately
needs AppState for `fileBrowserStore` + the FileExplorer action
surface).

## Useful commands

```sh
# From the worktree:
scripts/worktree-lock.sh acquire phase2-followup
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release

# Single test class (faster iteration on a small file):
scripts/test.sh -only-testing:NiceUnitTests/AppStateRenameTabTests > /tmp/x.log 2>&1
echo "exit=$?" ; grep -E "TEST FAILED|TEST SUCCEEDED|: error:" /tmp/x.log

# After-migration acceptance grep:
grep -rE "appState\.(activeTabId|projects|sidebarMode|sidebarCollapsed|pendingCloseRequest|ptySessions|paneLaunchStates)" Tests/NiceUnitTests/

# AppState size check:
wc -l Sources/Nice/State/AppState.swift
```
