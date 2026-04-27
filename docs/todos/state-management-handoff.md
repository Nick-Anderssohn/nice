# State management refactor — handoff

Phase 1 (the `@Observable` migration) and its follow-up cleanup
(extracting side effects out of `init` and dropping the dual
`ObservableObject` conformance) are both **done**. This file points
the next session at Phase 2.

## Status

Branch: `worktree-state-management` (same worktree, same branch — Phase
2 lands on top of Phase 1).

Commits on top of `main`:

- `220e708` — Add state-management refactor plan
- `6689548` — Phase 1: Migrate state management to the @Observable macro
- `1c481eb` — Add Phase 1 handoff doc with code-review feedback (this file's prior incarnation)
- `1fa3d87` — Phase 1 cleanup: extract side effects into start() / bootstrap(), drop ObservableObject carve-out

707 tests pass (664 unit + 43 UI). Acceptance grep
(`ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject`)
returns zero matches in `Sources/`. Manual smoke tests against the
installed `Nice Dev`:

- Typing `claude` in a fresh Main terminal opens a new sidebar tab via
  the socket handshake. ✅
- Rotating `~/.local/bin/claude` (rm + re-symlink to the same target)
  and re-typing `claude` still spawns a new claude tab — the cached
  resolved path survives the rotation. ✅

## Phase 2 — what's next

The authoritative plan lives in
[`state-and-AppState-refactor.md`](state-and-AppState-refactor.md).
Goal: split `AppState` (~2275 lines) into five cohesive sub-models —
`TabModel`, `SessionsModel`, `SidebarModel`, `CloseRequestCoordinator`,
`WindowSession` — with `AppState` shrinking to a thin composition root
(~200 lines, init + lifecycle choreography only).

Suggested PR sequence (per the plan):

1. Extract `TabModel`. Cleanest cut; other sub-models depend on it.
2. Extract `SessionsModel`. Owns the control socket and the
   `TabPtySession` callbacks.
3. Extract `SidebarModel`. Trivial; can ride with #2 or its own PR.
4. Extract `WindowSession`. Touches `tearDown` and `restoreSavedWindow`.
5. Extract `CloseRequestCoordinator`. Touches the alert binding in
   `AppShellView`.
6. View-side rename pass to read from the most specific sub-model.

We're shipping Phase 2 on this same branch (no separate PR stream),
so each step doesn't need to be a standalone PR — but each should
keep `scripts/test.sh` green and the manual smoke from the plan
passing before moving on.

## What changed in Phase 1 cleanup that matters for Phase 2

The plan's "Risks / things to watch" section warns about init-ordering
choreography (socket up → seed Main pty → restore → release save-gate
→ arm claude tracking). **That choreography is no longer in
`AppState.init` — it's in `AppState.start()`** (`Sources/Nice/State/AppState.swift`,
look for `func start()`). Init is now pure data construction:

- Builds the seed Terminals project + Main tab in `projects`.
- Stashes `services` as `trackedServices` (weak).
- Reads pure values from `services` (tweaks/scheme/palette/font caches).
- Does NOT touch the control socket, ZDOTDIR, `resolvedClaudePath`,
  `restoreSavedWindow`, or `armClaudePathTracking`.

`AppState.start()` (called from `AppShellHost.body`'s `.task`, after
`services.bootstrap()`) is where all the side-effectful wiring lives.
It's idempotent (`started` flag).

`NiceServices.bootstrap()` similarly absorbed the side effects that
used to live in its init: `cleanupStaleTempFiles`, ZDOTDIR write,
async `which claude` probe, `editorDetector.scan()`,
`ClaudeHookInstaller.install()`. Already idempotent (`booted` flag).

### Implication for Phase 2

The plan suggests the new `start()` method "may be the natural seam
for Phase 2's composition root" — i.e., construct `TabModel`,
`SessionsModel`, etc. inside `start()` rather than `init`, and wire
them together there. Worth thinking about up front:

- **Construct in init, wire in start()** — sub-models exist as soon
  as AppState is constructed (so views can `@Environment` them
  immediately), but the side-effectful cross-wiring (e.g. passing
  `TabModel` into `SessionsModel`'s socket handler) happens in
  `start()`.
- **Construct in start()** — sub-models don't exist until `start()`
  runs. Cleaner but means views have to handle the "not yet
  constructed" window. Probably worse.

The handoff doesn't prescribe either; pick one based on what shakes
out when you start sketching `TabModel`'s shape.

## Things still worth being careful about (carried from the plan)

- **`isInitializing` save-gate.** Today it's flipped at the end of
  `start()` (was: end of `init`). Whichever sub-model owns
  persistence (`WindowSession` per the plan) inherits this flag.
- **Static `claimedWindowIds` set.** Process-wide; lives on
  `WindowSession` after the split.
- **`@SceneStorage` bridge.** `AppShellView` reads
  `storedSidebarCollapsed`, `storedSidebarMode`, `storedWindowSessionId`
  and threads them in. After the split the bindings still flow through
  `AppShellView` — they just hand to `SidebarModel` / `WindowSession`
  instead of one big `AppState`.
- **UI-test selectors.** The `sidebar.terminals` accessibility alias
  and any other identifiers must be preserved across the rename.
- **`#Preview` blocks and unit tests.** The convenience `AppState()`
  init currently builds a complete preview-safe instance with
  `services: nil`. After the split, an equivalent — likely an
  `AppState.preview()` factory — is needed.
- **Tests construct `AppState` directly via the convenience init**
  (`Tests/NiceUnitTests/AppStateProjectBucketingTests.swift` and ~20
  other files). They pass `services: nil` and never call `start()` —
  they rely on the seed Main tab being built in `init` (so
  `projects[0].tabs[0]` exists). After the split, the same
  contract needs to hold: construct-without-`start()` should leave the
  data model in a usable state for unit tests.

## Useful commands

```sh
# From /Users/nick/Projects/nice/.claude/worktrees/state-management:
scripts/worktree-lock.sh acquire phase2
scripts/test.sh
scripts/worktree-lock.sh release

# Single-test fast loop:
scripts/test.sh -only-testing:NiceUITests/NiceUITests/testTypeClaudeInTerminalsFiresNewtab

# Acceptance grep (must stay clean):
grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/

# Install Nice Dev for manual smoke:
scripts/install.sh    # under the worktree lock — see CLAUDE.md
```

## Phase 2 acceptance criteria (from the plan)

- [ ] `Sources/Nice/State/AppState.swift` is under ~300 lines.
- [ ] `TabModel`, `SessionsModel`, `SidebarModel`,
      `CloseRequestCoordinator`, `WindowSession` each live in their
      own file under `Sources/Nice/State/`.
- [ ] No view reads `appState.<thing>` for a `<thing>` that lives on
      a sub-model — it reads the sub-model directly via
      `@Environment(SubModel.self)`.
- [ ] `scripts/test.sh` green.
- [ ] `#Preview` blocks compile and render.
- [ ] Manual smoke (typing `claude` in Main terminal opens a tab;
      rotating `~/.local/bin/claude` and re-typing still spawns
      claude) still passes.
