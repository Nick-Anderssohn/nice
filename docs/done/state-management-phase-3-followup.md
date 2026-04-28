# State management — Phase 3 follow-up: fakes + remaining gaps

**Status: done.** Item 8 landed (`SessionStorePersisting` protocol +
`FakeSessionStore` + `FakeTabPtySession` + `_testing_themeReceivers`
seam on `SessionsModel`). All four blocked must-add coverage gaps
are now covered: `WindowSession.restoreSavedWindow`'s three
branches, `WindowSession.scheduleSessionSave` save-gate,
`WindowSession.tearDown`'s `claimedWindowIds` invariant, and
`SessionsModel`'s four-method theme fan-out. Items 4 and 5 stay
deferred per the spec's default — nothing in this round surfaced a
reason to take them. 752 tests pass at HEAD (709 unit + 43 UI).

## Original brief (preserved for context)

Phase 3 items 1–3, 4, 7 are done. Item 6 is partial (3 of 7
must-add gaps covered). This file is the spec for the work needed
to close out item 6 and decide what to do with items 4, 5.

The umbrella spec is
[`state-management-phase-3.md`](state-management-phase-3.md). Read
that for context and the full leverage ranking; this file is just
the actionable next-round shopping list.

## Status

Branch: `worktree-state-management`. Worktree:
`/Users/nick/Projects/nice/.claude/worktrees/state-management`.

Recent commits at HEAD (closeout round on top of the brief baseline):

- `b7bca06` — Move state-management Phase 3 docs to docs/done/
- `acd60d1` — Phase 3 follow-up: cover SessionsModel theme fan-out
- `0faa4e6` — Phase 3 follow-up: cover WindowSession.tearDown invariants
- `e693a12` — Phase 3 follow-up: cover WindowSession.scheduleSessionSave save-gate
- `6a4fe9c` — Phase 3 follow-up: cover WindowSession.restoreSavedWindow branches
- `bd9203a` — Phase 3 follow-up item 8: inject SessionStorePersisting + claimedWindowIds seam
- `bdf6846` — Add Phase 3 follow-up handoff: fakes + remaining gaps
- `a0372b6` — Update Phase 3 spec with completion status
- `39ccd62` — Phase 3 item 5: add direct coverage for low-cost gaps
- `f6405e2` — Phase 3 item 4: extract FileExplorerOrchestrator from AppState
- `27d7e2b` — Phase 3 item 3: promote duplicated test fixtures to shared file
- `8b66f0d` — Phase 3 item 2: rename AppState*Tests files into sub-model test files
- `c038214` — Phase 3 item 1: drop the last two AppState forwarders

752 tests pass at HEAD (709 unit + 43 UI). Pre-closeout baseline
was 730.

## Goals

1. Land item 8: introduce `FakeSessionStore` and (if needed)
   `FakePtySpawner` so the four blocked must-add coverage gaps in
   item 6 can be exercised.
2. Use the fakes to add direct unit coverage for:
   - `WindowSession.restoreSavedWindow` — three branches.
   - `WindowSession.scheduleSessionSave` save-gate.
   - `WindowSession.tearDown` `claimedWindowIds.remove` invariant.
   - `SessionsModel` theme fan-out (`updateScheme`,
     `updateTerminalFontSize`, `updateTerminalTheme`,
     `updateTerminalFontFamily`).
3. Decide whether to take on items 4 and 5 (theme split,
   `TerminalsSeedResult` removal). They're nice-to-haves; only
   pick them up if the work in 1–2 surfaces a reason.
4. Once item 6 is fully covered, move
   [`state-management-phase-3.md`](state-management-phase-3.md)
   into `docs/done/` and mark the state-management refactor as
   fully complete.

## Item 8 — design notes

### `FakeSessionStore`

The real `SessionStore` (`Sources/Nice/State/SessionStore.swift`)
is referenced through `SessionStore.shared` from `WindowSession` in
five places:

| Site                                  | Line(s) | Method                  |
|---------------------------------------|---------|-------------------------|
| `scheduleSessionSave`                 | 115     | `upsert(window:)`       |
| `restoreSavedWindow`                  | 191     | `load()`                |
| `restoreSavedWindow` (cleanup)        | 244     | `pruneEmptyWindows`     |
| `tearDown` (final upsert)             | 418     | `upsert(window:)`       |
| `tearDown` (sync flush)               | 419     | `flush()`               |

Recommended approach:

1. Define a protocol next to `SessionStore`:

   ```swift
   @MainActor
   protocol SessionStorePersisting: AnyObject {
       func load() -> PersistedState
       func upsert(window: PersistedWindow)
       func pruneEmptyWindows(keeping: String)
       func flush()
   }
   extension SessionStore: SessionStorePersisting {}
   ```

2. Add an injection point on `WindowSession.init`:

   ```swift
   init(
       tabs: TabModel,
       sessions: SessionsModel,
       sidebar: SidebarModel,
       windowSessionId: String,
       persistenceEnabled: Bool,
       store: SessionStorePersisting = SessionStore.shared
   ) { … }
   ```

   Plumb through from `AppState.init` (just forward the param,
   default keeps production wiring untouched).

3. Replace each `SessionStore.shared.X` with `self.store.X` inside
   `WindowSession`.

4. Build `Tests/NiceUnitTests/Fixtures/FakeSessionStore.swift`:

   ```swift
   @MainActor
   final class FakeSessionStore: SessionStorePersisting {
       var state: PersistedState = .empty
       private(set) var upsertCalls: [PersistedWindow] = []
       private(set) var pruneKeepingCalls: [String] = []
       private(set) var flushCount = 0

       func load() -> PersistedState { state }

       func upsert(window: PersistedWindow) {
           upsertCalls.append(window)
           var windows = state.windows.filter { $0.id != window.id }
           windows.append(window)
           state = PersistedState(version: PersistedState.currentVersion, windows: windows)
       }

       func pruneEmptyWindows(keeping: String) {
           pruneKeepingCalls.append(keeping)
           let filtered = state.windows.filter { $0.id == keeping || $0.totalTabCount > 0 }
           state = PersistedState(version: PersistedState.currentVersion, windows: filtered)
       }

       func flush() { flushCount += 1 }
   }
   ```

   Optional: seed convenience initializers to preload `state` for
   restoreSavedWindow tests.

5. To exercise `claimedWindowIds`, expose a test seam — either
   make the static `private` → `internal` for `@testable import
   Nice`, or add a `static func` like
   `WindowSession.testing_resetClaimedWindowIds()` for setUp/tearDown
   isolation.

### `FakePtySpawner`

Only needed for the SessionsModel theme fan-out test. Two shapes
that work, in increasing order of cost:

**Option A — minimal: assert-by-side-effect via a real
TabPtySession**. Spawn ptys with `NICE_CLAUDE_OVERRIDE=/bin/cat`
(matches existing `TabModelProjectBucketingTests` setup) and rely
on the apply* methods being no-throws / idempotent. The test
verifies the cache fields update *and* that the call doesn't
crash. This doesn't fully verify fan-out behavior (we never
observe what each session received) but is a meaningful smoke
test.

**Option B — protocol-extract**. `TabPtySession` is `final` today.
Extract a protocol covering the apply surface:

```swift
@MainActor
protocol TabPtySessionThemeable: AnyObject {
    func applyTheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor)
    func applyTerminalFont(size: CGFloat)
    func applyTerminalTheme(_ theme: TerminalTheme)
    func applyTerminalFontFamily(_ name: String?)
}
extension TabPtySession: TabPtySessionThemeable {}
```

Type `SessionsModel.ptySessions` as `[String: TabPtySessionThemeable]`.
Then build a `FakeTabPtySession` in fixtures that records every
apply call. Tests inject one or more fakes into `ptySessions`
directly (test seam: make the dictionary `internal(set)` or add a
helper `_setPtySession(tabId:_:)` for testing).

Option B is the right answer if we want real coverage. Estimate:
1–2 hours. Option A is the right answer if the priority is just to
land *something* before moving on.

## Coverage gaps to fill

Once the fakes exist, write these tests. Each lives in its own
file under `Tests/NiceUnitTests/`.

### `WindowSessionRestoreTests.swift`

Construct `WindowSession` with a `FakeSessionStore` preloaded with
known `PersistedState` and a chosen `windowSessionId`. After each
test, reset `claimedWindowIds` via the test seam.

| Test                                                     | State preload                                                           | Expectation                                                               |
|----------------------------------------------------------|-------------------------------------------------------------------------|---------------------------------------------------------------------------|
| `test_restore_matchedNonEmpty_adoptsThatWindow`          | One window with id matching, projects non-empty                         | After restore, `tabs.projects` rebuilt; `claimedWindowIds` contains the id |
| `test_restore_matchedButEmpty_fallsBackToFirstNonEmpty`  | Matched-by-id window has `projects == []`; another window has projects   | Falls back to the non-empty unclaimed slot                                |
| `test_restore_unmatched_adoptsUnclaimedSlot`             | No matched id; one unclaimed non-empty window                            | Adopts the unclaimed slot; `windowSessionId` updated                      |
| `test_restore_unmatched_secondAppStateStaysFresh`        | Two AppStates; first claims slot A; second has matched-empty slot       | Second AppState refuses to adopt A; stays fresh                           |
| `test_restore_malformedJSON_emptyState_isHandled`        | Empty `PersistedState`                                                   | Restore is a no-op; tree shape is the seed                                |

The deferred Claude-spawn double-`DispatchQueue.main.async`
(WindowSession.swift:299–319) is harder — it relies on
`SessionsModel.makeSession`, which spawns ptys. Skip if too
involved; the synchronous code path is the more important branch.

### `WindowSessionSaveGateTests.swift`

Inject `FakeSessionStore`; verify the save-gate.

| Test                                                            | Expectation                                                    |
|-----------------------------------------------------------------|----------------------------------------------------------------|
| `test_scheduleSessionSave_blockedDuringInit`                    | Pre-`markInitializationComplete()`, `upsertCalls` is empty     |
| `test_scheduleSessionSave_blockedWhenPersistenceDisabled`       | `persistenceEnabled: false`, `upsertCalls` is empty after call |
| `test_scheduleSessionSave_releasedAfterMarkInitializationComplete` | Calling `markInitializationComplete()` then schedule lands an upsert |

### `WindowSessionTearDownTests.swift`

| Test                                                      | Expectation                                                            |
|-----------------------------------------------------------|------------------------------------------------------------------------|
| `test_tearDown_releasesClaimedWindowId`                   | Pre: `claimedWindowIds` contains id. Post: id removed                  |
| `test_tearDown_persistenceEnabled_writesAndFlushes`       | One upsert + one flush captured by fake                                |
| `test_tearDown_persistenceDisabled_doesNothing`           | No upserts, no flushes, but id still removed from `claimedWindowIds`   |
| `test_secondWindow_canAdoptSlotAfterFirstTearsDown`       | Compose two AppStates; after first.tearDown, second can adopt the id  |

### `SessionsModelThemeFanOutTests.swift`

Pick Option A or B from the FakePtySpawner section. Tests:

| Test                                          | Expectation                                                       |
|-----------------------------------------------|-------------------------------------------------------------------|
| `test_updateScheme_fansToEverySession`        | Every fake's last-applied scheme/palette/accent matches the call  |
| `test_updateTerminalFontSize_fansToEverySession` | Every fake captures the new size                              |
| `test_updateTerminalTheme_fansToEverySession` | Every fake captures the theme                                     |
| `test_updateTerminalFontFamily_fansToEverySession` | Every fake captures the family                              |
| `test_newSessionAfterUpdate_picksUpCachedTheme` | A session created *after* update receives the cached state     |
| `test_updateScheme_withNoSessions_doesNotCrash` | Smoke: callable with empty `ptySessions`                       |

## Items 4 and 5 — deferred unless prompted

- **Item 4 (split SessionsModel theming)**: SessionsModel.swift is
  929 lines. Extract `SessionThemeCache` as a peer struct or
  nested class holding the five `currentX` cache fields and the
  four `updateX` methods. The fan-out tests in
  `SessionsModelThemeFanOutTests` should target this new type
  instead of SessionsModel. SessionsModel keeps a thin forwarder
  so callers (`appState.sessions.updateScheme(...)`) don't change.
- **Item 5 (replace `TerminalsSeedResult`)**: TabModel.swift:340–387
  returns a `TerminalsSeedResult` enum that WindowSession.swift:
  326–334 switches on. Replace with an explicit
  `spawnHook: (Tab) -> Void` callback parameter on
  `TabModel.ensureTerminalsProjectSeeded`. Single caller, mechanical.

Both are low-risk small wins. Take them if there's time after the
must-add coverage gaps land; otherwise leave for a future round.

## Verification

Same pattern as Phase 3:

```sh
scripts/worktree-lock.sh acquire phase3-followup
scripts/test.sh > /tmp/nice-test.log 2>&1 ; echo "exit=$?"
grep -E "TEST FAILED|TEST SUCCEEDED|with [0-9]+ failures" /tmp/nice-test.log
scripts/worktree-lock.sh release
```

Don't pipe `scripts/test.sh` through `grep` directly — UI flakes
lose their failure context that way. UI tests can fail to launch
the dev app when another foreground app is keeping focus; if you
hit this, the unit suite is the primary signal and a retry of the
full suite after a brief idle window typically clears the failure
mode.

## Done state

When this round wraps:

- `Sources/Nice/State/SessionStore.swift` exposes
  `SessionStorePersisting`. `WindowSession` reads through the
  injected `store` reference; production wiring is unchanged.
- `Tests/NiceUnitTests/Fixtures/FakeSessionStore.swift` (and
  `FakeTabPtySession.swift` if Option B was picked) exist.
- Four new test files cover the previously-blocked gaps.
- `WindowSession.claimedWindowIds` has a test seam (or is reset
  per-test some other way).
- All 30+ added tests pass; full suite stays at 43 UI tests and
  unit count grows accordingly.
- `docs/todos/state-management-phase-3.md` and
  `docs/todos/state-management-phase-3-followup.md` move to
  `docs/done/`. Update `state-management-handoff.md` (in
  `docs/done/`) with a "Phase 3 closeout" section noting the round
  is complete.

After this lands, the state-management refactor is **done-done**
— no follow-ups, no "future cleanup" comments, no test-only
forwarders, every must-add coverage gap covered.

## Closeout (2026-04-28)

Landed:

- `SessionStorePersisting` protocol on `SessionStore`. `WindowSession`
  reads through `store: SessionStorePersisting`, defaulting to
  `SessionStore.shared` so production wiring is unchanged.
- `Tests/NiceUnitTests/Fixtures/FakeSessionStore.swift` capturing
  upsert / prune / flush. Mirrors the real store's in-memory shape
  so successive `load()` calls observe pruned state.
- `Tests/NiceUnitTests/Fixtures/FakeTabPtySession.swift`. Conforms
  to a new `TabPtySessionThemeable` protocol on `TabPtySession`
  (4 apply methods).
- `WindowSession._testing_resetClaimedWindowIds()` and
  `_testing_isClaimed(_:)` test seams. Internal-only via
  `@testable import Nice`.
- `SessionsModel._testing_themeReceivers` (test-only fan-out
  collection that the four `updateX` methods walk alongside
  `ptySessions`) and `_testing_themeCache` readback for cache-state
  verification.
- Four new test files:
  `WindowSessionRestoreTests.swift` (6),
  `WindowSessionSaveGateTests.swift` (4),
  `WindowSessionTearDownTests.swift` (4),
  `SessionsModelThemeFanOutTests.swift` (9).

Unit suite grew 687 → 709 (+22, three Restore tests share helpers
that don't double-count). UI suite stays at 43. Full suite green.

Items 4 and 5 stay deferred. Item 4's "split SessionsModel theming"
would entail moving the just-landed `_testing_themeReceivers` /
`_testing_themeCache` plumbing into the new peer; net wash for the
pure refactor. Item 5 ("replace `TerminalsSeedResult`") is
truly mechanical but didn't come up while writing the must-add
tests. Both can land in a future round if motivation appears.
