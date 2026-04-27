# State management refactor — handoff

This file picks up the work documented in `state-and-AppState-refactor.md`.
It captures where Phase 1 landed, what code review surfaced, and what
the next session needs to do **before Phase 2 starts**.

## Status

Branch: `worktree-state-management`. Phase 1 commit is
`6689548` ("Phase 1: Migrate state management to the @Observable macro").
All 707 tests pass (664 unit + 43 UI). Build is clean.

The plan in `state-and-AppState-refactor.md` is otherwise still
authoritative — Phase 2 hasn't started.

## What Phase 1 actually did

- Every `ObservableObject` + `@Published` pair in `Sources/Nice/State/`,
  `Sources/Nice/Process/TabPtySession.swift`, and
  `Sources/Nice/Theme/TerminalThemeCatalog.swift` now uses the
  `@Observable` macro. ~45 `@Published` annotations removed.
- View-local classes `RecorderCoordinator` (KeyRecorderField.swift) and
  `SidebarDragState` (SidebarView.swift), plus
  `FileBrowserDragState`, are also `@Observable`.
- Combine `services.$resolvedClaudePath.sink` replaced with
  re-arming `withObservationTracking` in `AppState.armClaudePathTracking`.
- View sites migrated: `@EnvironmentObject` → `@Environment(Type.self)`,
  `@ObservedObject` → plain stored property, `.environmentObject(_:)` →
  `.environment(_:)`. `@Bindable var foo = foo` added inside
  `AppearancePane.body` and `FontPane.body` for `$foo.bar` bindings.
- `@ObservationIgnored` placement: inspected in the code review,
  considered correct.

## Carve-out that breaks the plan's acceptance criterion

The plan said `grep -r "ObservableObject\|@Published\|@StateObject..." Sources/`
should return zero matches outside comments. **Phase 1 violates this at
four sites by design**, all related to a single SwiftUI lifecycle
problem:

- `Sources/Nice/State/AppState.swift` — `final class AppState: ObservableObject`
- `Sources/Nice/State/NiceServices.swift` — `final class NiceServices: ObservableObject`
- `Sources/Nice/Views/AppShellView.swift:70` — `@StateObject private var appState: AppState`
- `Sources/Nice/NiceApp.swift:68` — `@StateObject private var services = NiceServices()`

`objectWillChange` is never published — observation flows entirely through
the `@Observable` registrar. `ObservableObject` is purely a SwiftUI
lifecycle hook. There are big comments on both classes explaining this.

## Why the carve-out exists

`@State`'s `wrappedValue` is **not** `@autoclosure`. `@StateObject`'s **is**.

`AppState.init` and `NiceServices.init` do heavy side-effectful work:
bind a UNIX-domain control socket, write a per-process ZDOTDIR, spawn
child processes for the Main terminal pane, kick off a `which claude`
probe, install Claude Code's UserPromptSubmit hook.

With `@State private var appState: AppState` and
`_appState = State(wrappedValue: AppState(...))` in `init`, the
`AppState(...)` expression evaluates eagerly **on every parent body
re-render**. Each call constructs a new socket. SwiftUI's `@State`
identity rule keeps the *first* instance for the View, but the socket
whose bind succeeded ended up on a *later* instance that got discarded.
Mutations from socket messages (the `claude` shadow's handshake) then
updated an `AppState` the views never saw. The UI symptom: typing
`claude` in the Main terminal stopped opening a new sidebar tab.
Diagnosis logged the addresses: socket handler `self` ≠ View-side
`appState`.

`@StateObject(wrappedValue: AppState(...))` evaluates the closure
exactly once, so the lifecycle is stable. Hence the dual conformance.

## Code-review verdict

A general-purpose code-review subagent inspected the diff. Verdict: **net
improvement, with one architectural debt to pay off**.

The reviewer's specific finding on the carve-out:

> The cleaner fix is to make the init side-effect-free and add an
> explicit `start()` (or move the side effects into `.task` /
> `.onAppear` on the owning view). That collapses to a normal `@State`
> + `@Observable` model, drops one of two registrars from each class,
> and removes a permanent footgun where someone reaches for
> `objectWillChange.send()` because the type "is" an ObservableObject.

Other findings (no action needed, captured for context):
- Per-property invalidation is real — storage mechanism (`@StateObject`
  vs `@State`) is independent of tracking mechanism (`@Environment`
  routes through the `@Observable` registrar regardless).
- `withObservationTracking` re-arming is idiomatic; `trackedServices`
  weak property is load-bearing (handles the deinit race + lets the
  re-arm be re-entrant without a strong arg).
- `@ObservationIgnored` placement is sound across all categories
  (lifecycle bookkeeping; cached settings driven by external onChange;
  plumbing). Tracked vs ignored matches view-side reads.
- The Combine gotcha was handled cleanly per the plan.
- Phase 1 acceptance criterion at line 187 of the plan ("kill
  `~/.local/bin/claude`, re-symlink, open a new tab") was **not**
  smoked manually. It exercises the `withObservationTracking`
  re-arming under real binary rotation. Worth running before declaring
  Phase 1 fully soaked.

## Decision: expand Phase 1 scope to address the review

The user has decided to **expand Phase 1 to drop the dual conformance
before starting Phase 2**. The cleanup is a prerequisite for Phase 2,
not a follow-up.

## Work for the next session

1. **Extract side effects out of `AppState.init` and `NiceServices.init`**
   into a `start()` (or equivalent — pick a name) called from
   `.onAppear` or `.task` on the owning view.

   For `AppState`: socket creation/bind, `controlSocketExtraEnv`
   assembly, the seed `makeSession` call for the Main terminal,
   `restoreSavedWindow`, the `isInitializing` save-gate flip, and
   `armClaudePathTracking`. The init should leave the object in a
   well-defined "constructed but not started" state — empty `projects`
   is acceptable; views are guarded on `activeTabId != nil` etc. so an
   empty interim state should not crash.

   For `NiceServices`: ZDOTDIR write, the async `which claude` probe,
   `editorDetector.scan()`, `ClaudeHookInstaller.install()`, the
   `cleanupStaleTempFiles` call. All currently in `init`. Move to a
   `start()` called from the App's body (`.task` is cleanest).

2. **Drop `: ObservableObject` from both classes.** Drop the big
   carve-out comments (or shrink them to a one-liner pointing at the
   commit history).

3. **Switch the view sites back to `@State`:**
   - `Sources/Nice/Views/AppShellView.swift:70` — `@State private var appState: AppState`
   - `Sources/Nice/NiceApp.swift:68` — `@State private var services = NiceServices()`

4. **Verify the eager-init issue does not recur.** The two empirical
   symptoms to watch for:
   - Typing `claude` in the Main terminal must open a new sidebar tab
     (`testTypeClaudeInTerminalsFiresNewtab` and the other 10 socket-
     touching UI tests in `UITests/NiceUITests.swift`).
   - The Main terminal pane must actually start spawning. In the
     diagnostic session, the test app's terminal worked because the
     first AppState's pty session was stored in `ptySessions`, but
     this is fragile.

5. **Run the manual smoke from the plan** (line 187 in
   `state-and-AppState-refactor.md`). Specifically: kill
   `~/.local/bin/claude` if present, re-symlink it, open a new tab, and
   verify the new pane spawns claude rather than a bare zsh fallback.
   This is the single test that exercises `armClaudePathTracking` in
   anger.

6. **Re-run the full suite.** `scripts/test.sh` under the worktree lock
   (see CLAUDE.md and the `worktree-lock` skill). All 707 tests must
   stay green.

7. **Verify the acceptance criterion is now satisfied:**
   `grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/`
   should return only comment lines or nothing.

## Things to be careful about

- **Init ordering choreography in `AppState.init` is load-bearing.**
  Read the existing comments around lines 333–469 carefully. The
  socket must be up before pty spawns inject `NICE_SOCKET` env;
  `restoreSavedWindow` must run after the socket is up; the
  `isInitializing` save-gate must release after restore; the
  `armClaudePathTracking` install must happen at the end so
  `[weak self]` doesn't trip Swift's "self before fully initialized"
  check. Whatever shape `start()` takes, this ordering must survive.
- **`AppShellHost` currently passes `services: NiceServices` into
  `AppState.init` which immediately reads `services.tweaks`,
  `services.fontSettings.terminalFontSize`, etc.** That's fine — those
  reads are pure. The side effects to move out are below them in
  init: socket bind, makeSession, restore.
- **Tests that construct `AppState` directly** (in
  `Tests/NiceUnitTests/AppStateProjectBucketingTests.swift` and
  similar) pass `services: nil` and currently work because the
  `controlSocket` is created either way but `restoreSavedWindow` is
  guarded on `persistenceEnabled`. After the refactor, those unit
  tests will need to call `start()` themselves if they want the side
  effects, or skip it if they don't. Decide which tests need which.
- **Two windows can be open simultaneously** (⌘N spawns a new
  WindowGroup scene). Each gets its own `AppState`; they share
  `NiceServices`. The lifecycle change must preserve this.
- **The `WindowAccessor` block in `AppShellHost.body`
  (AppShellView.swift:149)** registers the AppState with
  `services.registry`. That registration currently happens after
  AppState init has already done all its side-effect work. After the
  refactor, you may want to move `appState.start()` into this same
  block, or into `.task { }`. `.task { }` is preferred because it's
  scoped to view lifetime and runs before the first render frame.

## Useful commands

```sh
# From /Users/nick/Projects/nice/.claude/worktrees/state-management:
scripts/worktree-lock.sh acquire phase1-cleanup
scripts/test.sh
scripts/worktree-lock.sh release

# A single-test fast-iteration loop:
scripts/test.sh -only-testing:NiceUITests/NiceUITests/testTypeClaudeInTerminalsFiresNewtab

# Check the acceptance grep:
grep -rE "ObservableObject|@Published|@StateObject|@ObservedObject|@EnvironmentObject" Sources/
```

## Once this cleanup is done

Phase 2 from `state-and-AppState-refactor.md` becomes safe to start.
The plan there describes splitting `AppState` into `TabModel`,
`SessionsModel`, `SidebarModel`, `CloseRequestCoordinator`, and
`WindowSession`. The init choreography you'll have just untangled is
exactly the choreography Phase 2's "Risks / things to watch" section
calls out as the hardest part of Phase 2.

If `start()` ends up being the natural seam for "construct sub-models
and wire them together", consider whether to land Phase 2's
composition root in `start()` directly rather than re-doing the work.
But that's a judgment call to make in the moment, not something this
handoff prescribes.
