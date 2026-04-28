# State management — Phase 3 follow-ups

Phase 1 (`@Observable`), Phase 2 (sub-model extraction + view rename),
and the test-suite forwarder migration are all **done**. See
[`state-management-handoff.md`](../done/state-management-handoff.md) for the
landed shape and
[`state-management-test-migration.md`](../done/state-management-test-migration.md)
for what the follow-up rewrote.

This file collects the optional follow-ups that two independent
reviewers (code-quality + testability) flagged after the dust
settled. None of these are blockers — the refactor is shippable
as-is. Pick what you want; skip what you don't.

## Status

Branch: `worktree-state-management`. Worktree:
`/Users/nick/Projects/nice/.claude/worktrees/state-management`. The
items below are scoped against the head of that branch (which is
also the merge target if/when this merges to `main`).

730 unit tests pass (664 → 687 → 730 across phase-3 commits;
SessionsModel theme fan-out + WindowSession restore/save-gate/
tearDown gaps still open and need item 8 fakes — see below).

## Recent commits — Phase 3

- `c038214` — Item 1 (spec item 2): drop the last two AppState
  forwarders. AppDelegate / WindowRegistry / one test now read the
  sub-models directly. AppState's surface is composition-root only.
- `8b66f0d` — Item 2 (spec item 1): rename AppState*Tests files into
  sub-model test files. Pure-tabs files renamed; Navigation /
  PaneCwd split by which sub-model API the test exercises.
- `27d7e2b` — Item 3 (spec item 7): promote duplicated test fixtures
  to `Tests/NiceUnitTests/Fixtures/{TabModelFixtures, GitFsFixtures}.swift`.
  Migrates seven test files; existing per-file private helpers stay
  as thin pass-throughs.
- `f6405e2` — Item 4 (spec item 3): extract `FileExplorerOrchestrator`
  from `AppState`. New `@Observable` sub-model holds the file-browser
  action surface; AppState wires it as a peer alongside the other
  five sub-models. AppShellView injects it into the environment.
- `39ccd62` — Item 5 (spec item 6, partial): add direct coverage for
  the three must-add gaps that don't need fakes —
  `TabModel.extractClaudeSessionId`, `SidebarModel` toggle paths,
  `CloseRequestCoordinator.requestClosePane` + `.pane` scope through
  `confirmPendingClose`. The remaining must-add gaps need item 8
  (FakeSessionStore / FakePtySpawner) — see status notes below.

## Items, ranked by leverage

> Status legend: ✅ landed, ◾ partial, ⏭ deferred.

### 1. ✅ Add `TabModelTests.swift` — direct unit tests for the tab tree

**Why this is the single highest-leverage item.** `TabModel` is 656
lines of pure-ish logic (no pty, no socket, no disk). Today every
test that exercises it allocates a full `AppState`, which spawns a
real control socket and a real Main-tab pty. A `TabModel` constructed
with `TabModel(initialMainCwd:)` runs an order of magnitude faster
and exercises the model directly. The whole suite would benefit.

The migration is mechanical: `Tests/NiceUnitTests/AppState*Tests.swift`
files where assertions are exclusively `appState.tabs.X` already
*are* `TabModel` tests; they're just paying for an `AppState` they
don't need. Move them.

Candidates to migrate (or extract chunks from):

- `AppStateProjectBucketingTests.swift` — entirely `tabs.X` after the
  rename pass. Becomes `TabModelProjectBucketingTests.swift`.
- `AppStateProjectRepairTests.swift` — entirely `tabs.X`. Becomes
  `TabModelProjectRepairTests.swift`.
- `AppStateRenameTabTests.swift` — mostly `tabs.X` (one
  `windowSession.snapshotPersistedWindow()` assertion can stay or
  move to `WindowSessionSerializationTests.swift`). Becomes
  `TabModelRenameTests.swift`.
- `AppStateReorderTests.swift` — entirely `tabs.X`.
- `AppStateNavigationTests.swift` — split: sidebar-nav half (`tabs.X`)
  → `TabModelNavigationTests.swift`; pane-nav half (`sessions.X`) →
  `SessionsModelNavigationTests.swift`.
- `AppStatePaneCwdTests.swift` — split: cwd-resolution helpers
  (`tabs.resolvedSpawnCwd`) → `TabModelCwdResolutionTests.swift`;
  `paneCwdChanged` (`sessions.paneCwdChanged`) →
  `SessionsModelTests`.

**Estimated payoff:** ~half the unit suite no longer pays the
pty-spawn cost. Test latency drops materially. The test files name
what they're actually testing.

**Risk:** low. Mechanical move; same assertions against the same
underlying behavior.

### 2. ✅ Drop the last two AppState forwarders

`Sources/Nice/State/AppState.swift:74-79` still exposes
`windowSessionId` and `livePaneCounts` as read-only forwarders. Two
production callers depend on them:

- `Sources/Nice/State/WindowRegistry.swift` — `appState.windowSessionId`
  (used to key the per-window registry entry).
- `Sources/Nice/State/AppDelegate.swift` — `state.livePaneCounts`
  (used by quit/window-close confirmation alerts).

Migrate the two callers to `appState.windowSession.windowSessionId`
and `state.tabs.livePaneCounts`, delete the forwarders. Closes the
loop on the "no test-only forwarders" goal: AppState's surface
becomes purely composition-root concerns.

**Estimated payoff:** small but symbolically complete. AppState's
surface stops admitting that it's still pretending to expose a
sub-model's value.

**Risk:** trivial. Two call sites, both checked in by the compiler.

### 3. ✅ Extract `AppState+FileExplorer` into its own sub-model

`Sources/Nice/State/AppState+FileExplorer.swift` is still 355 lines
on AppState. Its actual dependencies are `fileExplorer`, `tweaks`,
`editorDetector`, `tabs.activeTabId`, `tabs.firstAvailableTabId`,
`windowSession.windowSessionId`, and `sessions.addPane` — all
available outside AppState. An extraction (`FileExplorerOrchestrator`?
`OpenWithCoordinator`?) holding the same weak refs would slot in
identically.

Surface it owns:

- File operations: `openFromDoubleClick`, `cutPaths`, `moveOrCopy`,
  `pasteFromPasteboard`, `trash`, `revealInFinder`, `canPaste`,
  `copyToPasteboard`, `copyPathsToPasteboard`, `cutToPasteboard`.
- Open-with: `openInEditorPane`, `editorPaneEntries`,
  `openWithEntries`, `openWith`, `presentOpenWithPicker`.
- Static helpers: `editorPaneSpec`, `mergeEditorPaneEntries`,
  `resolveTargetTab`.

After the extraction, `AppState.swift` keeps only:

- The five sub-model `let`s + `fileBrowserStore` + the new
  `fileExplorerOrchestrator`.
- `init` / `start` / `tearDown`.
- `finalizeDissolvedTab` (private).
- `toggleFileBrowserHiddenFiles` (still legitimately cross-cutting).
- `windowSessionId` / `livePaneCounts` forwarders (or, after item 2,
  nothing).

**Estimated payoff:** AppState drops to ~150 lines. The
`@Environment(AppState.self)` injection on `FileBrowserView.swift`
collapses to `@Environment(FileExplorerOrchestrator.self)` — the
last view-side AppState dependency goes away (Phase-2 step-6's grep
becomes empty everywhere).

**Risk:** moderate. ~15 view callers + a handful of test files. The
work pattern is well-rehearsed (this is exactly Phase 2 step *N*+1).

### 4. ⏭ Split `SessionsModel` theming from pty plumbing

`Sources/Nice/State/SessionsModel.swift` is 929 lines. Two loosely
related concerns:

- **Theme cache + fan-out** (`Sources/Nice/State/SessionsModel.swift:75-185`).
  Five `currentX` cache fields, four `updateX` methods that walk
  `ptySessions` and call into each `TabPtySession`.
- **Pty/socket plumbing** (the remaining 700+ lines).

A nested struct or peer class (`SessionThemeCache`) holding the five
fields and the four `updateX` methods would isolate them. Same
public surface (`appState.sessions.updateScheme(...)` continues to
work via a thin forwarder), but the file walks better and the theme
fan-out is independently testable.

**Estimated payoff:** modest. Better cohesion within the largest
remaining file; unblocks direct unit tests for the fan-out behavior
(see item 6 below).

**Risk:** low. No call-site changes if forwarders remain on
SessionsModel.

### 5. ⏭ Replace `TerminalsSeedResult` with a `spawnHook:` callback

`Sources/Nice/State/TabModel.swift:340-387` returns a
`TerminalsSeedResult` enum so `Sources/Nice/State/WindowSession.swift:326-334`
can switch on it and call `sessions.makeSession`. The indirection
exists only because TabModel wants to stay process-free. An explicit
callback parameter on `TabModel.ensureTerminalsProjectSeeded(spawnHook:)`
is more direct.

**Estimated payoff:** tiny. Removes one indirection that exists
solely to thread an action through a result.

**Risk:** trivial. Single caller.

### 6. ◾ Cover the missing test paths the testability review flagged

These are real coverage gaps the decomposition exposed but didn't
fill. Three of the must-add items landed in `39ccd62`; the other
four need fakes from item 8 to exercise.

**Must-add:**

- ⏭ `WindowSession.restoreSavedWindow`
  (`Sources/Nice/State/WindowSession.swift:188-320`). Three branches:
  matched-non-empty, matched-empty fallback, unmatched-adopt-or-fresh.
  The `claimedWindowIds` collision-prevention path (lines 213/221)
  and the deferred Claude-spawn double-`DispatchQueue.main.async`
  (lines 299-319) are unverified.
- ⏭ `WindowSession.scheduleSessionSave` save-gate
  (`Sources/Nice/State/WindowSession.swift:112-116`). No test
  verifies that `isInitializing == true` blocks the upsert. The
  whole point of `markInitializationComplete` (handoff-documented as
  load-bearing) is untested.
- ⏭ `WindowSession.tearDown`
  (`Sources/Nice/State/WindowSession.swift:416-425`). The
  `claimedWindowIds.remove` invariant (line 424) — "second window
  can adopt the slot after first closes" — is uncovered.
- ✅ `CloseRequestCoordinator.requestClosePane`
  (`Sources/Nice/State/CloseRequestCoordinator.swift:102-115`) and
  the `.pane` scope through `confirmPendingClose`. Landed as
  `Tests/NiceUnitTests/CloseRequestCoordinatorPaneTests.swift` —
  idle Claude / busy Claude (thinking + waiting) / idle terminal /
  unknown tab+pane / confirm clears request / cancel leaves pane.
- ⏭ `SessionsModel.updateScheme` /
  `updateTerminalFontSize` / `updateTerminalTheme` /
  `updateTerminalFontFamily` fan-out
  (`Sources/Nice/State/SessionsModel.swift:148-185`). No tests for
  the live-session walk.
- ✅ `TabModel.extractClaudeSessionId`
  (`Sources/Nice/State/TabModel.swift:639`). Landed in
  `TabModelProjectBucketingTests.swift` — 7 tests covering both
  `--resume` / `--session-id`, both space-delimited and equals
  forms, scan-past-other-args, trailing flag, and absent.
- ✅ `SidebarModel.toggleSidebar()`, `endSidebarPeek()`, the
  `sidebarPeeking` toggle path. Landed as
  `Tests/NiceUnitTests/SidebarModelTests.swift`.

**Nice-to-have:**

- `TabModel.firstAvailableTabId`, `tabIdOwning(paneId:)`,
  `isTerminalsProjectTab`, `mutateTab` — only tested transitively.
- `TabModel.stripNiceWorktreeSuffix` (covered transitively in
  `AppStateProjectRepairTests.swift:527`; worth a direct unit).
- `SessionsModel.focusActiveTerminal`,
  `SessionsModel.createTerminalTab`, `createClaudeTabInProject`.
- `TabModel.ensureTerminalsProjectSeeded` directly (only hit via
  `AppState.init` today).

**Edge cases that the new wiring made worth pinning down:**

- Late `paneExited` callback after `AppState.tearDown`. The
  `[weak self]` in `SessionsModel.makeSession`
  (`Sources/Nice/State/SessionsModel.swift:838-851`) protects
  SessionsModel; the silent no-op via `tabs?.mutateTab` in
  `paneExited` (line 269) when AppState is gone is desired but
  unverified.
- `launchOverlayGraceSeconds` async timer firing after AppState
  release. `registerPaneLaunch` schedules `asyncAfter` with
  `[weak self]` (`Sources/Nice/State/SessionsModel.swift:400`).
- `claimedWindowIds` cross-AppState contention: two AppStates in the
  same process with overlapping saved snapshots. Collision-prevention
  at `Sources/Nice/State/WindowSession.swift:213-222` is unverified.
- `restoreSavedWindow` malformed/missing JSON.
- Multi-tab project pending-removal interleave with **real async**
  paneExited cascades. `AppStateCloseProjectTests.test_requestCloseProject_idleProject_removesProjectAndAllTabs`
  (`Tests/NiceUnitTests/AppStateCloseProjectTests.swift:45`) goes
  through the synchronous all-unspawned `hardKillTab` branch — the
  async-interleave path isn't exercised.

### 7. ✅ Promote duplicated test fixtures to a shared file

`seedProjectWithClaudeTab` is duplicated nearly verbatim in
`AppStateCloseProjectTests.swift:182`,
`AppStatePaneLifecycleTests.swift:279`, and
`AppStateClaudeSessionUpdateTests.swift:121`. `injectClaudeTab`
(`AppStateFileBrowserTests.swift:200`) and `injectTab`
(`AppStateRenameTabTests.swift:139`) are the same shape.

`seedProject` (`AppStateProjectBucketingTests.swift:594`) and the
per-test git-repo builders (`makeGitRepo`, `makeWorktreeMarker` at
lines 580+; same names but different bodies in
`AppStateProjectRepairTests.swift:355-370`) are similarly reusable.

Promote to:

- `Tests/NiceUnitTests/Fixtures/TabModelFixtures.swift` —
  `seedClaudeTab(into:projectId:tabId:claudeStatus:)`,
  `seedTerminalTab(into:projectId:tabId:)`. Take a `TabModel`
  directly so post-item-1 tests don't need an `AppState`.
- `Tests/NiceUnitTests/Fixtures/GitFsFixtures.swift` — `makeGitRepo`,
  `makeWorktreeMarker`, `makeDir`, `makeFile`. Keep
  `TestHomeSandbox.swift` next door.

### 8. ⏭ Optional: introduce `FakeSessionStore` / fake pty spawner

Today the test trick to skip persistence is "construct AppState with
`services: nil`" — which disables `persistenceEnabled`. That gates
out save-side tests. A `FakeSessionStore` (mirroring
`SessionStore.shared`'s shape, capturing upserts in-memory) plus a
`FakePtySpawner` would unlock direct tests for the
`markInitializationComplete` save-gate, the theme fan-out, and the
`claimedWindowIds` collision logic — none of which are testable
today without standing up real disk + a real pty.

**Estimated payoff:** unblocks several of the "must-add" gaps in
item 6.

**Risk:** moderate. Designing the fake interfaces is the work; once
the fakes exist, individual tests are straightforward.

## Suggested ordering

If you want to tackle this in a single sitting:

1. **Item 2** (drop the last two forwarders) — closes the loop and
   warms up the mental model. ~1 hr.
2. **Item 1** (rename/split test files into `<SubModel>Tests.swift`)
   — biggest test-quality win. ~2 hr.
3. **Item 7** (promote fixtures) — naturally falls out of item 1.
   ~1 hr.
4. **Item 3** (extract `AppState+FileExplorer`) — biggest remaining
   structural win. ~3 hr.
5. **Item 6** (must-add coverage) — half a day, parallelizable
   across the four areas (WindowSession, CloseRequestCoordinator,
   SessionsModel theming, TabModel extractor).

Items 4, 5, 8 are nice-to-haves; defer until something prompts them.

## Verification

Same pattern as the previous phases:

```sh
scripts/worktree-lock.sh acquire phase3
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release
```

Don't pipe `scripts/test.sh` through `grep` directly — UI flakes
lose their failure context that way.

## After this

`AppState.swift` is now 288 lines (down from 721 pre-Phase-2 and
300 pre-Phase-3): six sub-model `let`s + `fileBrowserStore`, the
`init` / `start` / `tearDown` / `finalizeDissolvedTab` /
`toggleFileBrowserHiddenFiles` surface, plus the theme-seed and
claude-path-tracking choreography that genuinely lives at the
composition root. The test suite organizes by sub-model. The
remaining `@Environment(AppState.self)` injection in
`FileBrowserView` is for `fileBrowserStore.ensureState(...)`; the
file-operation surface is now `FileExplorerOrchestrator`.

Open work (deferred per the spec's "skip unless prompted"
guidance):

- Item 4: split `SessionsModel` theming from pty plumbing.
- Item 5: replace `TerminalsSeedResult` with a `spawnHook:`
  callback.
- Item 8: introduce `FakeSessionStore` / `FakePtySpawner`.
- The four must-add coverage gaps in item 6 that depend on item 8
  (WindowSession restore-flow / save-gate / tearDown invariant +
  SessionsModel theme fan-out walk).

A future round picks these up if and when the failure modes they'd
cover start mattering.
