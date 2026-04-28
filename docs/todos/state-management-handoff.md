# State management refactor — handoff

Phase 1 (the `@Observable` migration) and all five of Phase 2's
sub-model extractions are **done**. This file points the next session
at what's left: the view-side rename pass (step 6 of the plan).

## Status

Branch: `worktree-state-management`. Worktree:
`/Users/nick/Projects/nice/.claude/worktrees/state-management`. All
Phase 2 work lands on this branch.

Recent commits on top of `main`:

- `220e708` — Add state-management refactor plan
- `6689548` — Phase 1: Migrate state management to the @Observable macro
- `1fa3d87` — Phase 1 cleanup: extract side effects into start() / bootstrap()
- `1b4e0fe` — Phase 2 step 1: Extract TabModel
- `c39cf3b` — Phase 2 step 2: Extract SessionsModel
- `c595e88` — Phase 2 step 3: Extract SidebarModel
- `27949c1` — Phase 2 step 5: Extract CloseRequestCoordinator
- _step 4_ — Phase 2 step 4: Extract WindowSession

707 tests pass (664 unit + 43 UI) at every commit.

The plan in
[`state-and-AppState-refactor.md`](state-and-AppState-refactor.md) is
still the authoritative outline. This handoff is the running diary.

## What's already extracted

`Sources/Nice/State/`:

| File | Lines | Owns |
|---|---:|---|
| `TabModel.swift` | 656 | `projects`, `activeTabId`, lookup, reordering, project-structure repair, cwd resolution, kebab→sentence title humanization, static path/arg helpers |
| `SessionsModel.swift` | 929 | `ptySessions`, `paneLaunchStates`, control socket, theme caches, launch overlay, pane lifecycle handlers, pane management, tab creation w/ spawn, `focusActiveTerminal` |
| `SidebarModel.swift` | 61 | `sidebarCollapsed`, `sidebarMode`, `sidebarPeeking` + 3 toggle methods |
| `CloseRequestCoordinator.swift` | 286 | `pendingCloseRequest`, `projectsPendingRemoval`, `requestClose×3`/`confirm`/`cancel`, `isBusy`, `hardKill×3` |
| `WindowSession.swift` | 426 | `windowSessionId`, `persistenceEnabled`, `isInitializing`, static `claimedWindowIds`, `scheduleSessionSave`, `snapshotPersistedWindow`, `restoreSavedWindow`, `ensureTerminalsProjectSeededAndSpawn`, `addRestoredTabModel`, persistence-half `tearDown` |
| `AppState.swift` | 796 | Composition root: holds the five sub-models, wires their callbacks, owns `fileBrowserStore`, has the `start()`/`tearDown()` choreography, runs `finalizeDissolvedTab`, exposes the public-surface forwarders |

Public API is preserved everywhere via forwarders on `AppState`. Views
and unit tests still call `appState.tab(for:)`,
`appState.paneLaunchStates[...]`, `appState.requestCloseTab(...)`,
etc. The view-side rename pass (step 6 of the plan) is not done yet.

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

## Phase 2 — what's left

### Step 6: View-side rename pass

The plan's "View-side changes" section in
[`state-and-AppState-refactor.md`](state-and-AppState-refactor.md).
Pre-conditions: all five sub-models exist (✅). The pass updates
views to read from the most specific sub-model — e.g.
`WindowToolbarView` becomes `@Environment(SessionsModel.self)`
instead of going through `AppState`.

Today the AppState forwarders carry the public surface — `AppState`
is still 796 lines, mostly forwarders. Step 6 retires those by
threading the sub-models through `.environment(...)`:

```swift
// In AppShellView, replacing the single `.environment(appState)`:
.environment(appState.tabs)
.environment(appState.sessions)
.environment(appState.sidebar)
.environment(appState.closer)
.environment(appState.windowSession)
.environment(appState) // composition root, for lifecycle hooks
```

Each view migrates to `@Environment(<SubModel>.self)` for the
slice it actually reads. Once the rename pass lands, AppState should
shrink to ~200 lines (init + lifecycle choreography +
`finalizeDissolvedTab` + the few pieces that genuinely cross
sub-models like `armClaudePathTracking`).

Pieces of work:

1. Update `AppShellView.swift` to inject all five sub-models via
   `.environment(...)`.
2. Walk `Sources/Nice/Views/`: replace `@Environment(AppState.self)`
   with the most specific sub-model. Many views need only one or two.
3. Rename call sites: `appState.projects` → `tabs.projects`,
   `appState.sidebarCollapsed` → `sidebar.sidebarCollapsed`,
   `appState.pendingCloseRequest` → `closer.pendingCloseRequest`,
   `appState.paneLaunchStates` → `sessions.paneLaunchStates`, etc.
4. Delete the now-unused forwarders from `AppState`. Keep only
   surface that's genuinely cross-cutting (e.g. `livePaneCounts`,
   `tearDown()`, the `start()`/`init` plumbing).
5. UI-test selectors must survive: the `sidebar.terminals`
   accessibility alias and any other `accessibilityIdentifier`s
   on AppState-bound views.
6. Tests: many unit tests still call `appState.tab(for:)`,
   `appState.addRestoredTabModel(...)`, etc. Decide whether to
   migrate them to the sub-model directly (purer) or keep test-only
   forwarders. The simpler call would be to migrate.

### Subtleties to watch in step 6

- **`@Bindable` for two-way bindings.** `AppShellView`'s alert binds
  to `appState.pendingCloseRequest` via a `Binding(get:set:)`. After
  the rename, the `closer` is what to bind. Use `@Bindable var
  closer: CloseRequestCoordinator` if you want `$closer.pendingCloseRequest`,
  or keep the manual `Binding(get:set:)` form.

- **Preview safety.** `#Preview` blocks construct `AppState()` via
  the convenience init and do `.environment(appState)`. After step
  6 they must also inject every sub-model the previewed view needs.
  Either add a `.previewEnvironment()` helper on AppState that fans
  them all out, or update each preview block by hand.

- **Test conveniences.** Several tests do
  `appState.requestCloseTab(...)`. Keeping a thin pass-through
  layer just for tests is fine — the goal of step 6 is to clean up
  the *view* surface, not the test surface.

- **Avoid a giant single PR.** The plan suggests splitting if it
  gets large. A reasonable cut: (a) inject sub-models +
  migrate views by file, (b) follow up with a smaller PR removing
  AppState forwarders that have no remaining callers.

## Acceptance criteria for step 6

- [ ] No view declares `@Environment(AppState.self)` for state that
      lives on a sub-model — it reads the sub-model directly.
- [ ] AppState's forwarder count drops materially (target: composition
      root + cross-cutting only).
- [ ] `scripts/test.sh` green: 707/707.
- [ ] `#Preview` blocks compile and render.
- [ ] Manual smoke (typing `claude` in Main terminal opens a tab;
      rotating `~/.local/bin/claude` and re-typing still spawns
      claude; close window with running pty triggers confirmation
      alert) still passes.

## Useful commands

```sh
# From the worktree:
scripts/worktree-lock.sh acquire phase2-step6
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release

# Single-test fast loop:
scripts/test.sh -only-testing:NiceUnitTests/AppStateSerializationTests > /tmp/x.log 2>&1

# Acceptance grep for Phase 1 (must stay clean):
grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/

# Find AppState environment readers (step 6 entry point):
grep -rn "@Environment(AppState.self)" Sources/Nice/Views/

# Install Nice Dev for manual smoke:
scripts/install.sh    # under the worktree lock — see CLAUDE.md
```
