# State management refactor: `@Observable` migration + `AppState` decomposition

Two related-but-independent changes. Phase 1 is a near-mechanical
framework migration. Phase 2 is a code-organization refactor that gets
considerably easier *after* Phase 1 lands. They can ship as separate
PR streams, but Phase 2 should not start until Phase 1 is merged and
soaked.

## Goals

1. Move every `ObservableObject` + `@Published` pair in
   `Sources/Nice/State/` and `Sources/Nice/Process/` to the
   `@Observable` macro (Observation framework, macOS 14+).
2. Break `AppState` (currently 2224 lines, 12 `@Published` properties,
   13 `MARK:` sections) into a small set of cohesive observables so
   each view subscribes only to the slice of state it actually reads.

## Non-goals

- No third-party state library (TCA, ReSwift, etc).
- No move off `@MainActor` for state classes — SwiftTerm/AppKit
  interactions are main-thread anyway.
- No SwiftData/Core Data migration. `SessionStore`'s Codable JSON +
  500 ms debounce is the right shape for a tab list.
- No new tests written purely for the refactor; existing UI tests +
  unit tests are the regression net.

---

## Phase 1 — `@Observable` migration

### Scope (every conformer to migrate)

From `grep -rn "ObservableObject" Sources/Nice/`:

- `State/AppState.swift` — `AppState`
- `State/NiceServices.swift` — `NiceServices`
- `State/Tweaks.swift` — `Tweaks`
- `State/KeyboardShortcuts.swift` — `KeyboardShortcuts`
- `State/FontSettings.swift` — `FontSettings`
- `State/FileBrowserSortSettings.swift` — `FileBrowserSortSettings`
- `State/FileBrowserStore.swift` — `FileBrowserStore`
- `State/FileBrowserState.swift` — `FileBrowserState`, `DirectoryWatcher`
- `State/FileBrowserSelection.swift` — `FileBrowserSelection`
- `State/FileBrowserDragState.swift` — `FileBrowserDragState`
- `State/WindowRegistry.swift` — `WindowRegistry`
- `State/ReleaseChecker.swift` — `ReleaseChecker`
- `State/EditorDetector.swift` — `EditorDetector`
- `State/FileOperations/FileOperationHistory.swift` — `FileOperationHistory`
- `State/FileOperations/FilePasteboardAdapter.swift` — `FilePasteboardAdapter`
- `Process/TabPtySession.swift` — `TabPtySession`
- (plus `TerminalThemeCatalog` — confirm path; injected as
  `@EnvironmentObject` from `NiceApp.swift:83`)

Note: `Tweaks` and `KeyboardShortcuts` and `FontSettings` and
`FileBrowserSortSettings` rely on `didSet` observers to write to
`UserDefaults`. These are unaffected by the migration — `@Observable`
keeps Swift property semantics, including `didSet`.

### Mechanical change per file

For each class:

1. Remove `: ObservableObject` from the class declaration.
2. Add `@Observable` macro to the class.
3. Remove every `@Published` attribute. ~45 occurrences total.
4. For any property that should NOT participate in change tracking
   (private caches, debounce work-items, non-observable substructure),
   add `@ObservationIgnored`.
5. Drop `import Combine` from files that no longer need it (most do
   not after step 3; `AppState` is the exception — see "Combine
   gotcha" below).

### Call-site changes

#### `.environmentObject(...)` → `.environment(...)`

`NiceApp.swift` injects 8 services per scene (lines 78–85, 105–111).
`AppShellView.swift:163` injects the per-window `appState`. The
`#Preview` blocks (`AppShellView.swift` lines ~669–681,
`SidebarView.swift` lines ~781–784) inject preview instances.

All become `.environment(value)`.

#### Property-wrappers in views

| Before | After |
|---|---|
| `@StateObject private var appState: AppState` | `@State private var appState: AppState` |
| `_appState = StateObject(wrappedValue: AppState(...))` | `_appState = State(wrappedValue: AppState(...))` |
| `@EnvironmentObject private var appState: AppState` | `@Environment(AppState.self) private var appState` (non-optional; SwiftUI traps if missing) |
| `@ObservedObject var history: FileOperationHistory` (`FileOperationDriftBanner.swift:14`) | `var history: FileOperationHistory` (plain stored property; `@Observable` views re-render automatically) |
| `Binding($appState.foo)` / `$appState.foo` | `@Bindable var appState: AppState` block, then `$appState.foo`. Or `Bindings(appState).foo` ad-hoc. |

Files needing edits (from `grep -rn "EnvironmentObject\|StateObject\|ObservedObject" Sources/Nice/Views/`):

- `Views/AppShellView.swift` (5 wrappers including the `appState`
  `@StateObject`)
- `Views/SidebarView.swift` (4 `@EnvironmentObject` + 1
  `@StateObject` for `dragState`)
- `Views/WindowToolbarView.swift` (2 `@EnvironmentObject`)
- `Views/SettingsView.swift` (5)
- `Views/SettingsEditorsPane.swift` (3)
- `Views/SettingsFontPane.swift` (2)
- `Views/KeyRecorderField.swift` (1 `@EnvironmentObject` + 1
  `@StateObject` for the recorder coordinator)
- `Views/Logo.swift`, `Views/UpdateAvailablePill.swift`,
  `Views/FileOperationDriftBanner.swift`, `Views/FileBrowserView.swift`

Run a final pass with `grep -rn "EnvironmentObject\|StateObject\|ObservedObject\|@Published\|ObservableObject" Sources/` and expect zero matches in non-test code.

### Combine gotcha — `services.$resolvedClaudePath`

`AppState.swift:462–467` currently does:

```swift
self.claudePathCancellable = services.$resolvedClaudePath
    .dropFirst()
    .receive(on: DispatchQueue.main)
    .sink { [weak self] path in
        self?.resolvedClaudePath = path
    }
```

`@Observable` removes `@Published` and there is **no `$` projected
value**, so this code does not compile after migration. This is the
only such site in the codebase (verified with
`grep -rE "\$[a-z]" Sources/Nice/`).

**Fix:** replace with re-arming `withObservationTracking`:

```swift
@ObservationIgnored
private var claudePathTrackingArmed = false

private func trackClaudePath(from services: NiceServices) {
    withObservationTracking {
        _ = services.resolvedClaudePath
    } onChange: { [weak self] in
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.resolvedClaudePath = services.resolvedClaudePath
            self.trackClaudePath(from: services) // re-arm
        }
    }
}
```

Drop `claudePathCancellable: AnyCancellable?` and the
`import Combine` from `AppState.swift` if nothing else in the file
uses Combine after migration. (Spot-check: `SessionStore.swift` uses
`DispatchWorkItem` for debouncing — that is GCD, not Combine.)

### Testing & validation

- `scripts/test.sh` (under the worktree lock) — full unit + UI suite.
  UI tests are the most valuable signal: they exercise the
  AppState↔view binding paths end-to-end.
- Manually verify the high-traffic invalidation paths:
  - Toggle sidebar (⌘B) — should still flip without lag or extra
    redraw.
  - Open file browser tab (⌘⇧B), drill into a directory — file
    listing updates.
  - Spawn new Claude tab from sidebar `+` — tab appears, terminal
    paints, "Launching…" overlay behaves (≤750 ms grace window).
  - Change accent in Settings — toolbar tint updates live.
  - Close window with running pty — confirmation alert appears.
- Profile briefly with Instruments' SwiftUI template on a window with
  ~5 tabs and one busy Claude pane. Expectation: fewer view-body
  evaluations on each pty byte than before, because invalidation is
  now per-property instead of per-`AppState`.

### Rollout

Single PR. The migration is global and partial migration creates
mismatched expectations (`@StateObject` vs `@State`, `$x` vs no `$x`),
which is more error-prone than landing it whole. Estimated 1–2 days
of focused work plus a soak day.

### Acceptance criteria

- [ ] `grep -r "ObservableObject\|@Published\|@StateObject\|@ObservedObject\|@EnvironmentObject" Sources/` returns no matches outside comments.
- [ ] `scripts/test.sh` green.
- [ ] All five manual smoke paths above pass on `Nice Dev`.
- [ ] `claudePathCancellable` removed; claude-path probe still
      lands in newly-spawned panes (test by killing
      `~/.local/bin/claude`, re-symlinking, opening a new tab).

---

## Phase 2 — Decompose `AppState`

Premise: after Phase 1, splitting `AppState` is a code-organization
play, not a perf play (per-property invalidation already buys most of
the perf benefit). The remaining motivations are:

- Smaller files are easier to read and navigate.
- Cohesive observables are easier to test in isolation.
- The pty session subsystem is the most complex and most likely to
  grow; keeping it on its own object lets it evolve without dragging
  unrelated UI state along.

### Proposed split

Drawing the seams from the existing `MARK:` sections in
`AppState.swift`:

#### `TabModel` (data model — the document)

Owns the projects/tabs/panes tree and selection. ~600 lines.

- Properties: `projects`, `activeTabId`, `terminalsProjectId`
  constant, `mainTerminalTabId` constant.
- Methods: from the existing `// MARK: - Selection`,
  `// MARK: - Tab creation`, `// MARK: - Pane management`,
  `// MARK: - Reordering`, `// MARK: - Keyboard navigation`,
  `// MARK: - Lookup`, `// MARK: - Helpers` sections.
- Has no knowledge of pty sessions, file browser, theming, or close
  confirmation. Pure value-tree mutation.

#### `SessionsModel` (process plumbing)

Owns the long-lived child processes. ~700 lines.

- Properties: `ptySessions`, `paneLaunchStates`, `controlSocket`,
  `zdotdirPath`, `controlSocketExtraEnv`, `resolvedClaudePath`,
  current scheme/palette/accent/font/theme cache.
- Methods: from `// MARK: - Process plumbing`,
  `// MARK: - Control socket`, `// MARK: - Theme`,
  `// MARK: - Launch overlay`, `// MARK: - Lifecycle handlers`,
  `// MARK: - Pty sessions`.
- Holds a reference to `TabModel` so `paneExited`,
  `paneTitleChanged`, `paneCwdChanged` callbacks can mutate the
  document.

#### `SidebarModel`

Owns the per-window sidebar UI state. ~50 lines.

- Properties: `sidebarCollapsed`, `sidebarMode`, `sidebarPeeking`.
- Methods: `toggleSidebar`, `toggleSidebarMode`, `endSidebarPeek`.
- Bridges to `@SceneStorage` exactly as today (the bridging happens
  in `AppShellView`, not on the model itself).

#### `CloseRequestCoordinator`

Owns the "processes still running" alert flow. ~150 lines.

- Property: `pendingCloseRequest`.
- Methods: `requestClosePane`, `requestCloseTab`,
  `requestCloseProject`, `confirmPendingClose`,
  `cancelPendingClose`, plus `projectsPendingRemoval`.
- Holds references to `TabModel` and `SessionsModel` so it can
  enumerate live panes and invoke `terminateAll()` on confirm.

#### `WindowSession` (persistence)

Owns the window's identity and disk state. ~250 lines.

- Properties: `windowSessionId`, `persistenceEnabled`, the
  static `claimedWindowIds` set.
- Methods: from `// MARK: - Session persistence` —
  `restoreSavedWindow`, `snapshotPersistedWindow`,
  `scheduleSessionSave`, `tearDown`'s persistence half.
- Holds a reference to `TabModel` to read the current tree for
  snapshotting.

#### `AppState` (composition root)

Stays as the per-window `@State` object that `AppShellView` owns —
but shrinks to a thin shell that:

- Constructs the five sub-models.
- Wires them together (passes `TabModel` into `SessionsModel`, etc).
- Handles the cross-cutting init dance (control socket up before
  pty spawns; restore after socket; release `isInitializing` save
  gate at the end).
- Exposes `tearDown()` that calls each sub-model's teardown in the
  right order.

Estimated final size: ~200 lines, mostly init and lifecycle
choreography.

### View-side changes

Today views read `appState.projects`, `appState.activeTabId`,
`appState.sidebarCollapsed`, etc. The rename pass:

- `appState.projects` → `tabs.projects` (or keep as
  `appState.tabs.projects` if you want to avoid threading another
  environment value)
- `appState.sidebarCollapsed` → `sidebar.sidebarCollapsed`
- `appState.pendingCloseRequest` → `closer.pendingCloseRequest`

**Recommendation: thread sub-models through `.environment()` so each
view declares exactly what it observes.** This is the whole point of
the split. `WindowToolbarView`, for example, only needs `TabModel`
and `SessionsModel.paneLaunchStates`; it should not see
`CloseRequestCoordinator` or `SidebarModel`.

`AppShellView` becomes the injection point:

```swift
.environment(appState.tabs)
.environment(appState.sessions)
.environment(appState.sidebar)
.environment(appState.closer)
.environment(appState.windowSession)
.environment(appState) // composition root, for lifecycle hooks
```

Views that today take `@Environment(AppState.self) appState` migrate
to whichever sub-model they actually use. Many will need only one or
two.

### Order of operations

1. Extract `TabModel` first — it is the cleanest cut and other
   sub-models depend on it. Land as its own PR.
2. Extract `SessionsModel`. Larger and trickier because it owns the
   control socket and the TabPtySession callbacks. Own PR.
3. Extract `SidebarModel`. Trivial; can ride with #2 or its own PR.
4. Extract `WindowSession`. Own PR — touches `tearDown` and
   `restoreSavedWindow`, both of which have UI-test coverage.
5. Extract `CloseRequestCoordinator`. Own PR — touches the alert
   binding in `AppShellView`.
6. View-side rename pass to read from the most specific sub-model.
   Own PR (or split if it gets large).

Each PR should ship with `scripts/test.sh` green and a manual smoke
of the same five paths from Phase 1.

### Risks / things to watch

- **Init ordering.** The current `AppState.init` is choreographed:
  socket → seed Terminals project → `makeSession` for the main tab →
  socket `start` → `restoreSavedWindow` → release `isInitializing` →
  install `claudePathCancellable`. The split must preserve this.
  Likely shape: `AppState.init` constructs sub-models in order and
  drives the choreography from the composition root.
- **`isInitializing` save-gate.** Today this is checked inside
  `scheduleSessionSave` and toggled at the end of
  `AppState.init`. Lives most naturally on `WindowSession`.
- **Static `claimedWindowIds` set.** Process-wide. Stays on
  `WindowSession` as a static; the registry semantics do not change.
- **`@SceneStorage` bridge.** `AppShellView` reads
  `storedSidebarCollapsed`, `storedSidebarMode`,
  `storedWindowSessionId` and passes them into the AppState init.
  After the split, the bindings still flow through `AppShellView` —
  it just hands them to `SidebarModel` / `WindowSession` instead of
  one big `AppState`.
- **UI-test selectors.** The `sidebar.terminals` accessibility alias
  and any other `accessibilityIdentifier`s on AppState-bound views
  must be preserved across the rename.
- **`#Preview` blocks and unit tests.** Today `AppState()`'s
  convenience init builds a complete preview-safe instance. Need an
  equivalent for the composed shape — likely an
  `AppState.preview()` factory that wires up the five sub-models
  with `services: nil`.

### Acceptance criteria (Phase 2 complete)

- [ ] `Sources/Nice/State/AppState.swift` is under ~300 lines.
- [ ] `TabModel`, `SessionsModel`, `SidebarModel`,
      `CloseRequestCoordinator`, `WindowSession` each live in their
      own file under `Sources/Nice/State/`.
- [ ] No view reads `appState.<thing>` for a `<thing>` that lives on
      a sub-model — it reads the sub-model directly via
      `@Environment(SubModel.self)`.
- [ ] `scripts/test.sh` green.
- [ ] `#Preview` blocks compile and render.
- [ ] Manual smoke of the five Phase-1 paths still passes.

---

## Sequencing summary

| Phase | What | Effort | Risk |
|---|---|---|---|
| 1 | `@Observable` migration (whole codebase, single PR) | ~1–2 days + soak | Low — mechanical, well-trodden, single Combine site identified |
| 2 | `AppState` decomposition (5 incremental PRs + view rename) | ~1 week elapsed | Medium — touches init choreography, persistence, UI-test surface |

Phase 2 should not start until Phase 1 has soaked at least a few days
in `Nice Dev` — the `@Observable` migration changes invalidation
semantics, and any latent bug from that should surface before the
larger structural refactor begins.
