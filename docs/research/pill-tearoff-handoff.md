# Handoff: pane-pill tear-off + move-between-windows

**Paste this whole file into the new conversation as the brief.**

You are starting the **next** increment of the pane-pill drag work in the
macOS SwiftUI app **Nice**: making a pill draggable **out of its window**.
Two user-facing capabilities:

1. **Tear-off** — drag a pill off the window (drop on empty desktop / no
   drop target) → the pane detaches and opens in a **brand-new window**.
2. **Move between existing windows** — drag a pill from window A's strip
   and drop it onto window B's strip → the pane moves into B (and is
   removed from A).

The pane's **live state must survive the move** — the running pty, the
terminal scrollback, the Claude session. You are moving a live thing, not
re-creating it from persisted JSON.

Branch / worktree: `worktree-refactor-top-bar` at
`/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`. The
intra-strip reorder this builds on is committed (`881d545`) and pushed.

---

## Read these first (in the repo)

- `Sources/Nice/Views/WindowToolbarView.swift` — the pill strip,
  `InlinePanePill`, the **already-built** intra-window reorder:
  `PaneStripDragState` / `PaneDragSession` / `PaneDropTarget`, the pill
  `.onDrag` (stashes a `PaneDragOrigin` + puts the pane id on the
  pasteboard), `PaneStripDropDelegate`, and the insertion line. **This is
  the seam you extend.**
- `Sources/Nice/Views/PaneStripDropResolver.swift` — `PaneDragOrigin`
  (has a commented `sourceWindowId` extension point) and
  `PaneDropDestination` (an enum *designed* so "new window" / "other
  window's strip" are **added cases**, not signature changes). The
  forward-compat seams for THIS feature were deliberately planted here.
- `Sources/Nice/State/WindowSession.swift` — per-window identity, the
  claim ledger, `restoreSavedWindow()`, `snapshotPersistedWindow()`,
  frame save/restore, `scheduleSessionSave()`.
- `Sources/Nice/State/NiceServices.swift` + `WindowRegistry.swift` — the
  app-global services and the **registry of all live windows'
  AppStates**. This is the bridge between windows.
- `Sources/Nice/Process/TabPtySession.swift` — where a pane's live
  `NiceTerminalView` + pty live (`entries: [paneId: PaneEntry]`). The
  thing that has to migrate.
- `Sources/Nice/Views/AppShellView.swift` — per-window `AppState`
  construction, the `WindowAccessor`, the launch-time multi-window
  fan-out (`openWindow(id: "main")`), and the `NICE_UITEST_WINDOW_FRAME`
  test hook added for the zoom test.
- `UITests/MultiWindowRestoreUITests.swift` — the only existing
  multi-window UITest patterns (seed `sessions.json`, count windows,
  read per-window tab ids).
- `docs/research/pill-reorder-continuation-handoff.md` and
  `pill-drag-window-move-decision.md` — history of the reorder + the
  solved window-drag blocker (context, not required reading).

---

## The architecture you're working against (load-bearing facts)

These determine the whole design — confirm them yourself before building.

1. **`NiceServices` is app-global**: one instance, `@State` in
   `NiceApp` (`NiceApp.swift:107`), injected via `.environment` to every
   window. Holds `Tweaks`, `WindowRegistry`, `claimLedger`,
   `fileExplorer`, etc. **A new process-wide registry for live pane
   handoff belongs here.**

2. **Each window owns its own `AppState`** (`AppShellView` `@State`),
   which owns its own `TabModel`, `SessionsModel`, `WindowSession`, and
   **its own pty sessions**. There is **no shared tab/pane model**.
   `TabModel.movePane` only moves a pane *within one `TabModel`* — it
   cannot reach another window.

3. **`WindowRegistry` (shared, on `NiceServices`) tracks every live
   window's `AppState`** (`registry.allAppStates`). This is how window A's
   drop code reaches window B's `TabModel` / `SessionsModel`. Cross-window
   move = "find the target window's AppState in the registry, then operate
   on its models."

4. **A pane's live state lives in the *source* window's
   `TabPtySession.entries[paneId]`** (`TabPtySession.swift` —
   `PaneEntry` bundles the `NiceTerminalView` NSView, the pty, the
   delegate, held state). Moving a pane between windows means
   **detaching that `PaneEntry` from the source `TabPtySession` and
   re-attaching it to the target window's `TabPtySession` without
   tearing down the pty or recreating the view.** Killing + respawning
   would lose the running process and scrollback — not acceptable.

5. **The pasteboard only carries the pane id** (the pill's `.onDrag`
   returns `NSItemProvider(object: pane.id as NSString)`). A live
   `NiceTerminalView` + pty **cannot ride the pasteboard.** So the live
   `PaneEntry` must be handed off through an in-process side channel
   keyed by that id — exactly the "id → resolve to live origin" design
   the reorder plan called out as the cross-window migration path.

6. **Persistence is per-window snapshots**: `SessionStore` holds
   `windows → projects → tabs → panes`, keyed by `windowSessionId`. After
   a move, BOTH affected windows must re-snapshot (`scheduleSessionSave`
   already fires on tree mutation via `onTreeMutation`). A torn-off new
   window needs its own `windowSessionId` + claim-ledger entry.

---

## The two hard problems (the spikes — do these FIRST)

The reorder feature was de-risked by one spike (window-drag). This feature
has **two** genuinely unknown pieces. Resolve each with an automated test
that you've proven can fail, before building on it.

### Spike A — Detecting "dropped outside any window" (tear-off trigger)

SwiftUI's `.onDrop` only fires when the drag ends **over a drop target**.
There is **no SwiftUI callback for "dropped on empty desktop."** Tear-off
needs exactly that signal. Likely resolutions, in rough order of
preference — find which actually works AND is XCUITest-drivable:

- **AppKit `NSDraggingSource`**: replace/augment the pill's SwiftUI
  `.onDrag` with an AppKit-level drag (`beginDraggingSession(with:…)` from
  the pill's hosting NSView). The source's
  `draggingSession(_:endedAt:operation:)` fires with `operation == []`
  when the drop hit no target → that's the tear-off trigger, and
  `endedAt` gives the screen point to place the new window. This is how
  Safari/Terminal tab tear-off works. Cost: the pill drag stops being a
  pure SwiftUI `.onDrag`, which interacts with the **window-drag
  selectivity** the reorder relies on (the pill `.onDrag` currently claims
  the gesture so the toolbar's window-drag `.gesture` yields — see
  `WindowToolbarView.swift` `windowDragGesture` + the
  `pill-drag-window-move-decision.md` UPDATE 3). Re-verify
  `testDragOnPillDoesNotMoveWindow` and `testEmptyToolbarDragMovesWindow`
  stay green under whatever drag mechanism you choose.
- **`NSWindow`-level drag tracking / `draggingEnded`**: a window-level
  drag-destination or a monitor that notices the drag terminated with no
  in-app target.
- **Spring-loaded / `dropExited` heuristic**: weakest — infer tear-off
  when the drag leaves the strip and never enters another. Fragile; prefer
  a real source-ended callback.

**Critical compatibility check:** whatever you pick must still let the
existing **intra-window reorder** and **cross-window move** receive normal
`.onDrop`s. The cleanest end state is probably: AppKit drag source (for
the ended-outside signal) feeding the **same** pasteboard id that the
SwiftUI `.onDrop` strips already consume — so move/reorder keep working via
`.onDrop` and only tear-off uses the source-ended callback.

**Can XCUITest even drive this?** XCUITest's synthesized drag *did* drive
SwiftUI `.onDrop` for reorder (proven). Whether it drives an AppKit
`NSDraggingSource` ended-outside-window path is **unknown** — that's the
first thing to measure. If XCUITest can't synthesize a drop on the empty
desktop, fall back to: (a) a unit/integration test that calls the
tear-off entry point directly with a synthetic end-point, plus (b) the
user eyeballing the real Nice Dev build (they have been responsive to
manual-check requests). Don't let an untestable UI path block the
model-level work.

### Spike B — Migrating a live `PaneEntry` between windows without killing it

Prove you can move the running pty + `NiceTerminalView` from window A's
`TabPtySession` to window B's and have the terminal keep running with
intact scrollback (and the Claude session keep its conversation). Steps to
validate:

- Add a **process-wide live-pane registry** (on `NiceServices`) keyed by
  pane id: at drag start the source stashes a handle to its live
  `PaneEntry` (or a closure that detaches + returns it); the drop side
  (in any window) resolves the pasteboard id against this registry to claim
  the live entry.
- Detach from source: remove the `PaneEntry` from
  source `SessionsModel`/`TabPtySession` **without** calling the
  process-exit / `removePane` teardown that kills the pty.
- Re-parent the `NiceTerminalView` NSView into the target window's view
  hierarchy (SwiftUI re-hosts it when the `Pane` appears in the target
  tab; verify the NSView survives re-hosting — SwiftTerm views can be
  finicky about `setFrameSize`/first-responder on reparent; see
  `NiceTerminalView.swift` deferred-spawn logic so you don't trip the
  "spawn on first non-zero frame" path again).
- Insert the `Pane` into target `TabModel` + re-register the entry in
  target `TabPtySession.entries[paneId]`; update `activePaneId` on both
  sides; fire `onTreeMutation` on both so both windows persist.

If full live migration proves too deep for the first cut, a **documented
fallback** is: persist the pane, close it in A, and **re-open it from its
persisted state** in B/new-window (loses live pty + scrollback for
terminals; a Claude pane could `--resume` its `claudeSessionId`). Decide
with the user whether live-migration is in scope for v1 or a follow-up —
it's the single biggest cost driver. **Ask early.**

---

## What's already DONE (the foundation you extend)

- **Intra-window reorder** is complete, tested, committed (`881d545`):
  drag state, `.onDrag` source, `PaneStripDropDelegate`, deferred
  `movePane`, insertion line; 9 `PaneReorderUITests` + 34 unit tests
  green; the window-drag differential guards green.
- **Forward-compat seams already planted** (use them, don't re-cut):
  - `PaneDragOrigin` has the commented `sourceWindowId` slot — fill it in.
  - `PaneDropDestination` is an enum ready for `.newWindow` /
    `.otherWindowStrip(windowId:tabId:…)` cases.
  - `TabModel.movePane(_:inTab:…)` takes an explicit `tabId` so it can
    target a non-active tab (a building block, but note it's still
    single-`TabModel`; cross-window needs registry-mediated calls).
- **Window machinery exists**: `openWindow(id: "main")`, the claim
  ledger, per-window `windowSessionId`, frame save/restore. A new window
  already knows how to adopt/seed itself.
- **`NICE_UITEST_WINDOW_FRAME`** test hook (`AppShellView` `WindowAccessor`)
  pins a deterministic launch frame — reuse it to place/position test
  windows deterministically.

---

## Suggested build order

1. **Spike A** (tear-off detection) and **Spike B** (live migration) —
   independently, each with a failing-first test. These are the risk.
2. **Decide live-migration scope** with the user (full live vs.
   persist-and-reopen for v1).
3. **Process-wide live-pane registry** on `NiceServices` (the id → live
   `PaneEntry` side channel).
4. **Cross-window MOVE** (the easier of the two user features, since it
   reuses `.onDrop`): extend `PaneStripDropDelegate` so a drop whose
   pasteboard id belongs to *another* window resolves the source via the
   registry, detaches there, inserts here. Add a `PaneDropDestination`
   case. Insertion line already works per-strip.
5. **TEAR-OFF**: on the source-ended-outside signal, `openWindow`, give
   the new window an empty/seeded tab, then migrate the pane into it at
   the drop screen-point (reuse `NICE_UITEST_WINDOW_FRAME`-style framing
   to place it under the cursor).
6. **Edge cases**: tearing off the *last* pane of a tab (does the tab/
   window dissolve?); moving the *active* pane (re-focus a neighbor in the
   source); moving between a Claude tab and a terminal tab; dropping back
   onto the origin strip (no-op); the source window becoming empty
   (close it? per existing `paneExited` tab-dissolve rules).
7. **Tests** (see below).

---

## Test strategy

- **Unit/integration first** (fast, deterministic, where the real logic
  lives): the live-pane registry, the detach/re-attach migration, the
  model mutations on both windows, persistence round-trips for both
  affected windows. Mirror `MovePanePersistenceTests` /
  `TabModelMovePaneTests`.
- **UITests** (`UITests/`): build on `MultiWindowRestoreUITests` helpers
  (seed `sessions.json`, count `app.windows`, read per-window pane ids).
  - Cross-window move: two windows, drag a pill from one strip to the
    other, assert it appears in the target and is gone from the source,
    both window origins unchanged.
  - Tear-off: drag a pill to empty desktop, assert
    `app.windows.count` increased and the pane is in the new window.
    **Gated on Spike A's XCUITest verdict** — if synthesized drops on the
    desktop don't work, drive the tear-off entry point directly + a manual
    eyeball (user has offered before).
  - Keep green: all 9 `PaneReorderUITests`, both `WindowDragUITests`
    guards (the drag mechanism may change in Spike A — these are your
    regression net).

---

## Build / test / environment rules (follow exactly)

- **Worktree lock around every build/install/test**:
  `scripts/worktree-lock.sh acquire <op>` … `scripts/worktree-lock.sh
  release` (release even on failure — use a `trap`). Skill:
  `worktree-lock`. If a build/test is interrupted, the lock can be left
  held by a dead pid — `scripts/worktree-lock.sh status`, confirm the pid
  is dead, then `break`.
- **Only ever touch the `Nice Dev` build**, never prod
  `/Applications/Nice.app` (it hosts the live Claude session). Install:
  `scripts/install.sh` (defaults to dev). Tests: `scripts/test.sh` (runs
  `xcodegen`; forwards `-only-testing:` args). **Never** run bare
  `xcodebuild`/`xcodebuild test` against the `Nice` scheme.
- **Test target names**: unit `NiceUnitTests/<Class>/<test>`; UI
  `NiceUITests/<Class>/<test>` (folder `UITests/`, target `NiceUITests`).
- **Run only the RELEVANT tests** — the full UI suite is slow.
- **SourceKit phantom diagnostics** ("No such module 'XCTest'", "Cannot
  find type X / WindowChrome") appear until `xcodegen` regenerates — the
  build is the source of truth; don't chase them.
- **Screenshots can't be captured from the agent pty** — ask the user for
  any visual check.
- **Multi-window UITests are flakier than single-window** — window focus,
  z-order, and which window is `keyWindow` all matter. Prefer asserting on
  `sessions.json` contents / accessibility-tree counts over pixel
  geometry, and pin frames via the `NICE_UITEST_WINDOW_FRAME` hook so the
  two windows don't overlap at the drop point.

## First moves in the new conversation

1. Confirm the architecture facts above by reading the listed files.
2. **Ask the user**: is full live-pane migration (keep the running pty +
   scrollback across the move) in scope for v1, or is persist-and-reopen
   acceptable to start? This single answer reshapes the whole plan.
3. Run Spike A and Spike B with failing-first tests. Report what's
   XCUITest-drivable before committing to a UI-test-only verification
   story.
