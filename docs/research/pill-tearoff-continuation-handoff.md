# Handoff: finish pane tear-off (cross-window move is DONE)

**Paste this into the new conversation as the brief.** Branch/worktree:
`worktree-refactor-top-bar` at
`/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`. Nothing is
committed yet — all work is in the working tree. The full design lives in
`/Users/nick/.claude/plans/read-docs-research-pill-tearoff-handoff-cozy-axolotl.md`.

## Current state: GREEN and known-good

The tree builds and all relevant tests pass. The window-drag-vs-pill-drag
fix is intact and **UITest-verified** (see below). Run this to confirm
before changing anything:

```
scripts/worktree-lock.sh acquire verify; \
scripts/test.sh -only-testing:NiceUITests/WindowDragUITests \
  -only-testing:NiceUITests/PaneReorderUITests; \
scripts/worktree-lock.sh release
```

### DONE + verified

- **Live-pane migration foundation** (Spike B): `LivePaneRegistry`
  (`Sources/Nice/State/LivePaneRegistry.swift`, on `NiceServices`),
  `TabPtySession.detachPane/adoptPane`,
  `SessionsModel.detachLivePane/adoptLivePane/adoptClaudePaneAsNewTab/adoptTerminalPaneAsNewTab`,
  `TabModel.extractPane/insertPane/ensureProjectByPath` + shared
  `neighborActivePaneId`, `ProcessTerminationDelegate.routedPane` test seam.
  Tests: `LivePaneRegistryTests`, `LivePaneMigrationTests`,
  `ClaudePaneMigrationTests`, `TabModelInsertExtractPaneTests`.
- **Cross-window MOVE** (the easier user feature, fully working):
  `PaneMigrationCoordinator.commitCrossWindowMove`
  (`Sources/Nice/Views/PaneMigrationCoordinator.swift`); the pill `.onDrag`
  publishes a registry handle; `PaneStripDropDelegate` handles foreign
  drops (terminal → insert into strip; Claude → new tab under matching
  project) with a foreign-drag insertion indicator. Multi-window-safe
  source-tab dissolve via `AppState.dissolveTabIfEmpty` (guards
  `NSApp.terminate` when another window is live). Test:
  `CrossWindowMoveTests` (terminal move, Claude new-tab, same-window no-op,
  last-pane dissolve).
- **Tear-off LOGIC** (controller + seed): `PaneTearOffController.tearOff`
  (`Sources/Nice/Views/PaneTearOffController.swift`) +
  `NiceServices.PendingTearOff`/`enqueueTearOff`/`consumeTearOffSeed` +
  the consume step in `AppShellHost.task` (seeds the new window by kind).
  Test: `PaneTearOffControllerTests`. **This is fully built and tested —
  only the UI TRIGGER is missing.**
- **Reparent guards**: `NiceTerminalView` (no-respawn after window change,
  focus re-arm, Metal layer rebind) + `TabPtySession.adoptPane` sets
  `wantsFocusOnAttach`. Test: `NiceTerminalViewReparentTests` (+
  `NiceTerminalViewDeferredSpawnTests` regression, green).

## OPEN work

### 1. Tear-off UI trigger (the only missing feature) — GESTURE-CRITICAL

Decision (from the user): **finish the AppKit drag source correctly.** Pure
SwiftUI `.onDrag` cannot detect "dropped off the window" (SwiftUI owns the
drag session and exposes no end callback), so drag-to-desktop tear-off
requires owning an AppKit `NSDraggingSource`. A prior attempt did this but
**re-introduced the window-drag bug** by removing the pill's `.onDrag`
(which is what made `windowDragGesture` yield) without re-solving the
yield. That attempt has been reverted; `WindowToolbarView.swift` is back to
the verified `.onDrag` + cross-window-move wiring.

To finish correctly:
- Own the drag at the AppKit layer to get the `draggingSession(_:endedAt:
  operation:)` callback; on `operation == []` AND release point outside
  every app window frame, call
  `PaneTearOffController(services:).tearOff(paneId:sourceWindowSessionId:at:
  openWindow: { openWindow(id:"main") })`. Keep the SAME pane-id string on
  the pasteboard so the existing `.onDrop` reorder + cross-window move keep
  working. Move the `.onDrag` side effects (set `dragState.session`,
  `livePaneRegistry.publish(...)`) into the drag-begin callback.
- **RE-SOLVE THE YIELD** — this is the part the prior attempt skipped: gate
  `windowDragGesture` so it never calls `performDrag` for a press that
  began over a pill (the AppKit source sees the `mouseDown` location — have
  it set a flag the gesture checks). See `Sources/Nice/Views/CLAUDE.md`
  (rule 2). **This is auto-verifiable**: `WindowDragUITests` /
  `PaneReorderUITests` assert "pill drag does not move the window" — they
  must stay green.
- Add a UITest that drives the REAL gesture: pin the window with
  `NICE_UITEST_WINDOW_FRAME`, press-drag a pill to a coordinate OUTSIDE
  that frame, assert `app.windows.count` increased + the pane id is in the
  new window. This measures Spike A's real open question (can XCUITest's
  synthesized drag drive `NSDraggingSource`'s ended-outside path?). If — and
  only if — that proves undrivable, add a `tab.pill.<id>.tearOffForTest`
  accessibility action gated on `NICE_UITEST` that injects the end-point
  and assert window-count/pane-migration through that instead (still
  automated; the entry-point is already unit-tested).
- **Do this in the MAIN LOOP on Opus, not a delegated cheaper-model
  subagent.** It compiles + unit-passes while behaviorally wrong; the
  UITest gate is the only real check.

### 2. Remaining UITests (step 9)

- `CrossWindowMoveUITests`: two windows (pin non-overlapping frames via
  seeded `sessions.json`), drag a terminal pill from A into B's strip →
  appears in B, gone from A, `app.windows.count` unchanged; and a Claude
  pill from A into B → new sidebar tab in B under the matching project,
  source tab survives. Build on `MultiWindowRestoreUITests` helpers.
- The tear-off UITest from item 1.

### 3. Review pass (step 10)

Per the plan: run three reviewers in parallel (code-quality on Opus,
testability on Opus, docs-sync on Sonnet), synthesize, address the
important findings, re-test until green, then stop. The user authorized a
Workflow for this.

## Guardrails added this session (keep them)

- `Sources/Nice/Views/CLAUDE.md` — documents the window-drag invariant.
- `.claude/settings.json` + `.claude/hooks/guard-window-drag.sh` —
  PreToolUse hook that injects the invariant + required UITests whenever an
  agent edits `WindowToolbarView.swift`. **Commit these to the repo** so
  they protect the main checkout and every worktree (hooks load at session
  start, so they activate in the next session).
- A loud `⚠️ INVARIANT` comment on the pill's `.onDrag` in
  `WindowToolbarView.swift`.

## Build/test rules

- Worktree lock around every build/test. The lock script records its holder
  as the **main-repo path** even from a worktree, so a trap-based release
  from a subshell can fail and leave a stale lock. Run acquire / test /
  release as **separate sequential commands in one invocation, no subshell
  trap**. If a stale lock is held by a dead pid (`ps -p <pid>`),
  `scripts/worktree-lock.sh break` first.
- Only the `Nice Dev` build; `scripts/install.sh` (dev) / `scripts/test.sh`
  (forwards `-only-testing:`); never bare `xcodebuild` against `Nice`.
- SourceKit "No such module"/"Cannot find type" diagnostics are phantom
  until xcodegen regenerates (`scripts/test.sh` runs it) — the build is the
  source of truth.
