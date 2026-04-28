# State management refactor — handoff

Phase 1 (the `@Observable` migration) and four of Phase 2's five
sub-model extractions are **done**. This file points the next session
at what's left: extract `WindowSession` (step 4 of the plan), and the
view-side rename pass that follows (step 6).

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
| `AppState.swift` | 1094 | Composition root: holds the four sub-models, owns `windowSessionId` + persistence, owns sidebar UI flags' `@SceneStorage` bridge, owns `fileBrowserStore`, has the `start()`/`tearDown()` choreography, runs `finalizeDissolvedTab` |

Public API is preserved everywhere via forwarders on `AppState`. Views
and unit tests still call `appState.tab(for:)`,
`appState.paneLaunchStates[...]`, `appState.requestCloseTab(...)`,
etc. The view-side rename pass (step 6 of the plan) is not done yet.

## Callback wiring conventions

A pattern emerged across the four extractions that step 4 should
follow:

- A sub-model holds a **weak** reference to the sub-models it reads
  from (`SessionsModel.tabs`, `CloseRequestCoordinator.tabs` and
  `.sessions`). Cycle insurance — they're co-owned by AppState and
  share its lifetime.
- A sub-model exposes **`@ObservationIgnored var on…: (...) -> Void`**
  callbacks that AppState wires in `init` (`[weak self] in self?…`).
  Used when the sub-model needs to fan an event out to a concern it
  doesn't own (persistence, dissolve cascade, file-browser cleanup).
- Persistence saves are routed through a single AppState method,
  `scheduleSessionSave`, gated on `isInitializing` and
  `persistenceEnabled`. Sub-models that care call
  `onSessionMutation?()` (or, via TabModel's `onTreeMutation`,
  whatever name the model uses internally — pick whichever name fits
  the model's vocabulary).

Current callbacks wired in `AppState.init`:

```swift
tabs.onTreeMutation         = { [weak self] in self?.scheduleSessionSave() }
sessions.onSessionMutation  = { [weak self] in self?.scheduleSessionSave() }
sessions.onTabBecameEmpty   = { [weak self] tabId, pi, ti in
    self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
}
closer.onSyncFinalizeDissolve = { [weak self] tabId, pi, ti in
    self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
}
closer.onScheduleSave       = { [weak self] in self?.scheduleSessionSave() }
```

`WindowSession` will absorb `scheduleSessionSave` itself, so this
section will get rewired during step 4.

## Lessons learned (read these before step 4)

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

### Step 4: Extract `WindowSession` (~250 lines)

The plan's authoritative section is in
[`state-and-AppState-refactor.md`](state-and-AppState-refactor.md)
under "WindowSession (persistence)". Owns this window's identity and
disk state.

Properties to move from AppState onto a new `WindowSession`:

- `var windowSessionId: String` (currently `private(set)`; observed
  so `AppShellView`'s `onChange(of: appState.windowSessionId)` can
  mirror adoption back into `@SceneStorage`)
- `let persistenceEnabled: Bool`
- `var isInitializing: Bool`
- `static var claimedWindowIds: Set<String>` (process-wide; stays a
  `static` on `WindowSession`)

Methods to move:

- `private func scheduleSessionSave()`
- `func snapshotPersistedWindow() -> PersistedWindow` (internal — unit
  tests call this)
- `private func restoreSavedWindow()`
- `private func ensureTerminalsProjectSeededAndSpawn()` — straddles
  WindowSession + SessionsModel. The pure tree half is already on
  TabModel; the pty side-effect lives on SessionsModel; the orchestration
  belongs on WindowSession (it's only called from `restoreSavedWindow`).
- `func addRestoredTabModel(...)` — internal; tests call this. Mixed
  concern: builds a Tab, appends to `tabs.projects`, optionally calls
  `sessions.makeSession`. Lives on WindowSession with weak references
  to both.
- `tearDown`'s persistence half: the
  `if persistenceEnabled { SessionStore.shared.upsert/flush }` block.
  AppState's `tearDown` would call `windowSession.tearDown()` plus
  `sessions.tearDown()` plus the `claimedWindowIds.remove(...)` that
  also moves with this carve.

What stays on AppState after step 4 lands:

- The composition root: `let tabs / sessions / sidebar / closer /
  windowSession`, init, callback wiring, `start()`, `tearDown()`
  orchestration, `livePaneCounts` forwarder.
- `fileBrowserStore` (it's already its own observable; lives on
  AppState because dissolve cascade owns its cleanup).
- `finalizeDissolvedTab` — the dissolve cascade orchestrator.
- `armClaudePathTracking` (writes through to sessions).
- All the public surface forwarders.
- `let fileExplorer / tweaks / editorDetector` (NiceServices pointers
  threaded down for `AppState+FileExplorer`).
- `weak var trackedServices` (used by `armClaudePathTracking`).

### Subtleties to watch in step 4

- **`isInitializing` save-gate timing.** Today it's set true in init,
  flipped false at the end of `start()` after `restoreSavedWindow`
  runs. Multiple sub-model didSets fire *during* init (especially
  TabModel.activeTabId's didSet for the seed assignment) and bounce
  through `scheduleSessionSave` — which short-circuits because
  `isInitializing` is true. Step 4 must preserve that ordering even
  as `scheduleSessionSave` moves to WindowSession. The simplest shape:
  `windowSession.scheduleSessionSave()` checks the gate; AppState's
  callbacks call `windowSession.scheduleSessionSave()` instead of a
  private method.

- **`windowSessionId` adoption mid-restore.** `restoreSavedWindow` may
  switch the id to an adopted slot's id. The `@SceneStorage` mirror
  in `AppShellView` reads `appState.windowSessionId` via observation
  to write the new value back. After step 4, `windowSessionId` lives
  on `WindowSession`; expose it as `appState.windowSessionId` via a
  forwarder so `AppShellView` doesn't need a rename.

- **`claimedWindowIds` is process-static.** Shared across every live
  window. Stays as `static var` on `WindowSession`.

- **`restoreSavedWindow` reaches into three sub-models.** It uses
  TabModel (mutate `projects`, ensureProject, repairProjectStructure,
  ensureTerminalsProjectSeeded), SessionsModel (terminateAll,
  removePtySession, makeSession), and itself (claimedWindowIds,
  windowSessionId, scheduleSessionSave). Pass weak references to tabs
  and sessions in `WindowSession.init`.

- **`addRestoredTabModel` is called from tests via `appState.…`.**
  Three test files call it. After moving to WindowSession, keep an
  `appState.addRestoredTabModel(...)` forwarder.

- **`snapshotPersistedWindow` reads sidebar state too.** The
  `PersistedWindow` includes `sidebarCollapsed`. After moving to
  WindowSession, that read is `sidebar.sidebarCollapsed` (via the
  forwarder or a direct reference held by WindowSession). I'd just
  read `appState.sidebarCollapsed` via a back-reference, but cleaner:
  WindowSession holds weak `sidebar: SidebarModel`.

- **`addRestoredTabModel`'s pty fallback** for terminal-only tabs
  spawns the active pane's shell at its last-observed cwd. That's
  `sessions.makeSession(...)`. Keep the fallback exact — there's a
  comment explaining why we honour `activePaneId` over inferring the
  first terminal.

- **The deferred Claude spawn** in `restoreSavedWindow` (two nested
  `DispatchQueue.main.async` hops) calls `sessions.makeSession` and
  `sessions.ensureActivePaneSpawned`. Both stay on SessionsModel; the
  outer call site moves to WindowSession.

- **Test contract: convenience init seeds projects[0].tabs[0].** Same
  as steps 1–3. WindowSession constructed without start() must be a
  usable dummy; tests don't call WindowSession methods directly other
  than `addRestoredTabModel` (currently via AppState forwarder).

### Step 6: View-side rename pass

The plan's "View-side changes" section. Pre-conditions: all five
sub-models exist. The pass updates views to read from the most
specific sub-model — e.g. `WindowToolbarView` becomes
`@Environment(SessionsModel.self)` instead of going through
`AppState`. Likely a separate session.

Until step 6, the AppState forwarders carry the public surface. None
of the sub-models are exposed via `.environment(...)` yet —
`AppShellView.swift:163` still does `.environment(appState)` only.

## Acceptance criteria for step 4

- [ ] `WindowSession.swift` exists under `Sources/Nice/State/`.
- [ ] `Sources/Nice/State/AppState.swift` is under ~600 lines (mostly
      composition root + forwarders).
- [ ] `scripts/test.sh` green: 707/707.
- [ ] `#Preview` blocks compile and render.
- [ ] Manual smoke (typing `claude` in Main terminal opens a tab;
      rotating `~/.local/bin/claude` and re-typing still spawns
      claude; close window with running pty triggers confirmation
      alert) still passes.

## Useful commands

```sh
# From the worktree:
scripts/worktree-lock.sh acquire phase2-step4
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release

# Single-test fast loop:
scripts/test.sh -only-testing:NiceUnitTests/AppStateSerializationTests > /tmp/x.log 2>&1

# Acceptance grep for Phase 1 (must stay clean):
grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/

# Install Nice Dev for manual smoke:
scripts/install.sh    # under the worktree lock — see CLAUDE.md
```
