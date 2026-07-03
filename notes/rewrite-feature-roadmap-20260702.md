# Nice Rewrite — Feature-Level Roadmap (2026-07-02)

Grounded in the Swift sources on branch `worktree-rewrite` (HEAD `cf4de11`),
`Sources/Nice/**` (~26.8k lines). Every feature below cites the file(s) that
implement it today. Target stack per the decision report (`notes/
rewrite-stack-research.md`, Path B): all-Rust, GPUI (pinned zed `main` rev),
`alacritty_terminal` VT core, from-scratch GPUI-native terminal renderer.

**Licensing rule (binding):** Zed's `terminal` / `terminal_view` crates (and
Zed app crates like `title_bar`) are **GPL-3.0 — never open them as
implementation reference and never feed them to codegen**. Allowed reference/
reuse: gpui / gpui_platform / gpui_macos (Apache-2.0), alacritty frontend code
incl. `keyboard.rs` (Apache-2.0), termwiz (MIT), Xuanwo/gpui-ghostty
(Apache-2.0), longbridge/gpui-component (Apache-2.0), sixel-image /
sixel-tokenizer (MIT).

---

## 1. Feature inventory

48 user-facing features, grouped by domain. (Dev-only surfaces —
`--uitest-tearoff-hook`, `NICE_CLAUDE_OVERRIDE`, the XCTest launcher branch in
`NiceApp.swift` — are excluded; they get re-created as needed by the rewrite's
own test harness.)

### A. Terminal pane (the core)

| # | Feature | Current implementation |
|---|---|---|
| T1 | Terminal emulator pane: grid, scrollback, selection, Metal-rendered (SwiftTerm fork, statically linked, pinned in `project.yml`) | `Sources/Nice/Process/NiceTerminalView.swift`, `Sources/Nice/Views/TerminalHost.swift` |
| T2 | Pty spawn semantics: every pane runs a login+interactive zsh; commands run as `zsh -ilc "exec <cmd>"` so user PATH/rc is honored | `Sources/Nice/Process/TabPtySession.swift` (`spawnClaudePane`, `addTerminalPane`, `buildExecArgs`), `Sources/Nice/Process/ShellQuoting.swift` |
| T3 | Deferred spawn: panes are modelled (pill renders) before their pty exists; pty spawns on first focus | `TabPtySession.swift` (`armDeferredSpawn`), `Sources/Nice/State/SessionsModel.swift` (`ensureActivePaneSpawned`) |
| T4 | Row-quantized, bottom-anchored terminal layout (sub-row remainder hidden at top, stable gap under prompt during resize) | `Sources/Nice/Views/TerminalHost.swift` (`TerminalContainerView`) |
| T5 | OSC 0/1/2 window-title tracking → pane titles / tab auto-titles; Claude status parsed from title prefix (braille spinner U+2800–28FF = thinking, `✳` U+2733 = waiting) | `SessionsModel.swift` (`paneTitleChanged`), `Sources/Nice/Process/TerminalDelegateBridge.swift` |
| T6 | OSC 7 cwd tracking per pane (emitted by injected zsh `chpwd_functions` hook), persisted so panes respawn where they were | `SessionsModel.swift` (`paneCwdChanged`), `Sources/Nice/Process/MainTerminalShellInject.swift`, `Models.swift` (`Pane.cwd`) |
| T7 | File/image drag-drop into a terminal: backslash-escaped POSIX path typed at the pty; wrapped in bracketed paste (DEC 2004) when the app enables it; raw image data (browser/Messages) saved to file first | `Sources/Nice/Process/NiceTerminalView.swift` |
| T8 | Keyboard input incl. kitty keyboard protocol (CSI-u; e.g. Cmd-as-super `ESC[99;9u`) — provided by the SwiftTerm fork today | SwiftTerm fork (`/Users/nick/Projects/SwiftTerm`), pinned by revision in `project.yml` |
| T9 | "Launching…" overlay when a fresh pane stays silent past a grace window (slow post-checkout hooks etc.), cleared on first pty output | `Sources/Nice/Views/LaunchingOverlay.swift`, `SessionsModel.swift` (`registerPaneLaunch`/`clearPaneLaunch`), `TabPtySession.swift` (`onPaneLaunched`/`onPaneFirstOutput`) |
| T10 | Held panes: unexpected/non-zero process exit keeps the dead pane's view + scrollback mounted so the user can read final output; explicit dismiss later | `TabPtySession.swift` (`PaneEntry.isHeld`, `handlePaneExit`), `notes/show-exit-output-when-pane-dies.md` |
| T11 | Terminal font: family (default chain SF Mono → JetBrains Mono NL → system mono), pt size, live zoom ⌘+/⌘−/⌘0 (sidebar scales proportionally) | `Sources/Nice/State/FontSettings.swift`, `TabPtySession.swift` (`terminalFont`), `Sources/Nice/Views/SettingsFontPane.swift` |
| T12 | Terminal color themes: bg/fg/cursor/selection + 16 ANSI; separate light/dark selections; caret painted with app accent; smooth-scrolling toggle (opt-in) | `Sources/Nice/Theme/TerminalTheme.swift`, `TabPtySession.swift` (`applyTerminalTheme`), `Tweaks.swift` (`smoothScrolling`) |

### B. Data model: projects / tabs / panes

| # | Feature | Current implementation |
|---|---|---|
| M1 | Projects → Tabs (sessions) → Panes tree; a pinned, non-removable "Terminals" project at index 0; new Claude tabs auto-bucket into a project by cwd; ≤1 Claude pane per tab invariant | `Sources/Nice/State/TabModel.swift`, `Sources/Nice/State/Models.swift` |
| M2 | Pane/tab status model: thinking / waiting / idle + waiting-acknowledgment ("stop pulsing once the user looked") | `Models.swift` (`TabStatus`, `applyStatusTransition`, `needsAttention`) |
| M3 | Tab/pane titles: OSC auto-titles vs. manual rename lock (`titleManuallySet`); auto-numbered "Terminal N" (monotonic counter, persisted); "Main"/"Main N" for Terminals tabs | `Models.swift`, `TabModel.swift` (`applyAutoTitle`, rename paths), `SessionsModel.swift` (`createTerminalTab`) |
| M4 | Tab lineage (depth-1 tree): `/branch` parents and `[HANDOFF]` children render one indent under a root tab | `Models.swift` (`Tab.parentTabId`), `TabModel.swift` (`insertBranchParent`, `insertHandoffChild`) |
| M5 | Vestigial: `Tab.branch` (git branch field) exists in the model and persistence schema but is never populated or rendered — **do not carry** | `Models.swift:126`, `SessionStore.swift` (`PersistedTab.branch`) |

### C. Claude / shell integration

| # | Feature | Current implementation |
|---|---|---|
| C1 | ZDOTDIR shell injection: synthetic `.zshenv/.zprofile/.zlogin/.zshrc` chain back to the user's real config, then install a `claude()` shadow function + OSC 7 hook; `NICE_PREFILL_COMMAND` pre-types a command at the prompt (`print -z`) | `Sources/Nice/Process/MainTerminalShellInject.swift`, `notes/shell-tool-zdotdir-trap.md` |
| C2 | Control socket: Unix domain socket (0600, self-healing listener), one newline-delimited JSON message per connection; actions `claude`, `session_update`, `handoff`; `NICE_SOCKET`/`NICE_TAB_ID`/`NICE_PANE_ID` env in every pane | `Sources/Nice/Process/NiceControlSocket.swift`, `TabPtySession.swift` (env), `SessionsModel.swift` (`startSocketListener`) |
| C3 | Interactive `claude` interception: typing `claude` in any Nice pane asks the app — reply `newtab` (open a fresh sidebar tab) or `inplace [sid] [settingsPath]` (promote the sending pane to a Claude pane, splice `--session-id`/`--settings`) | `SessionsModel.swift` (`handleClaudeSocketRequest`, `createTabFromMainTerminal`) |
| C4 | Claude session identity: pre-minted UUID via `--session-id` at tab creation; resume across relaunch via `claude --resume <uuid>` | `TabPtySession.swift` (`ClaudeSessionMode`, `buildClaudeExecCommand`), `SessionsModel.swift` |
| C5 | Deferred resume ("prefill"): restored Claude tabs spawn a plain zsh with `claude --resume <uuid>` pre-typed; user hits Enter; the socket handshake promotes the pane in place | `TabPtySession.swift` (`.resumeDeferred`), `MainTerminalShellInject.swift` (`NICE_PREFILL_COMMAND`) |
| C6 | SessionStart hook: installed script (`~/.nice/nice-claude-hook.sh` + `~/.claude/settings.json` entry) relays session-id/cwd rotations (`/clear`, `/branch`, `--fork-session`) back to the socket; refuses to clobber foreign settings | `Sources/Nice/Process/ClaudeHookInstaller.swift` |
| C7 | `/branch` tracking: a `source=resume` rotation with an id change materializes a sibling "parent" tab pinned to the pre-branch session so the original conversation stays resumable | `SessionsModel.swift` (`handleClaudeSessionUpdate`, `materializeBranchParent`), `TabModel.swift` (`insertBranchParent`) |
| C8 | Worktree awareness: `claude -w <name>` → project buckets under the original cwd while `Tab.cwd` follows the worktree (`/`→`+` sanitization); mid-session cwd swaps reported by the hook update `Tab.cwd` | `SessionsModel.swift` (`createTabFromMainTerminal`, `updateTabCwd`), `TabModel.swift` (`extractWorktreeName`, `adoptTabCwd`) |
| C9 | Handoff: `/nice-handoff` skill (+ `~/.nice/nice-handoff.sh`) installed/removed per a Settings toggle with a one-time first-launch prompt; socket `handoff` opens a nested `[HANDOFF] <title>` tab running a fresh Claude seeded with a prompt pointing at the notes file, matching `--model`/`--effort` | `Sources/Nice/Process/SkillInstaller.swift`, `SessionsModel.swift` (`handleHandoffRequest`, `createHandoffTab`, `handoffPrompt`), `Tweaks.swift` (`installHandoffSkill`, `handoffSkillPromptSeen`) |
| C10 | Claude theme sync: mirrors the active terminal theme to `~/.claude/themes/nice.json` (live-reloaded by Claude) and passes `--settings ~/.nice/claude-theme-settings.json` only to Nice-launched Claudes; `_niceManaged` marker prevents clobbering user files; Settings toggle (default ON) | `Sources/Nice/Process/ClaudeThemeSync.swift`, `TabPtySession.swift` (settings flag), `SessionsModel.swift` (in-place reply 3rd field) |
| C11 | `claude` binary resolution via background login-shell `which` at launch (never blocks scene init) | `Sources/Nice/State/NiceServices.swift` (`runWhich`, `bootstrap`) |
| C12 | Orphan shell reaper: on launch, SIGKILL zshes with PPID 1 + `NICE_TAB_ID` in env (crash debris) so the 511-pty cap can't starve `forkpty` | `Sources/Nice/Process/OrphanShellReaper.swift` |

### D. Windows & chrome

| # | Feature | Current implementation |
|---|---|---|
| W1 | Custom hidden-titlebar chrome: 52pt top bar, full-size content, native traffic lights repositioned into the sidebar card (y=26 row, default-x+8 preserving OS pitch) | `Sources/Nice/Views/Chrome/TrafficLightPlacer.swift`, `Sources/Nice/Views/WindowChrome.swift`, `Sources/Nice/Views/Chrome/WindowChromeController.swift`, `Sources/Nice/Views/Chrome/WindowBridge.swift` |
| W2 | Empty-chrome drag-to-move + double-click runs the user's `AppleActionOnDoubleClick` (zoom/minimize); pill presses structurally excluded via per-press hit-test | `Sources/Nice/Views/Chrome/ChromeEventRouter.swift`, `Sources/Nice/Views/WindowDragRegion.swift` |
| W3 | Multi-window: ⌘N opens a fully isolated window (own tabs/panes/ptys/socket); focused-window routing for process-wide subsystems | `NiceApp.swift` (`WindowGroup`, `NewWindowButton`), `Sources/Nice/State/WindowRegistry.swift`, `Sources/Nice/Views/AppShellView.swift` |
| W4 | Native full screen + View ▸ Enter/Exit Full Screen menu item (⌃⌘F) | `NiceApp.swift` (`FullScreenTracker`, commands) |
| W5 | Close/quit confirmation when live panes would die (⌘Q counts all windows; red-button/⌘W counts one) | `Sources/Nice/State/AppDelegate.swift`, `Sources/Nice/State/CloseRequestCoordinator.swift`, `WindowRegistry.swift` (`CloseConfirmationDelegate`), `Sources/Nice/State/SessionLifecycleController.swift` |
| W6 | Window frame persistence + restore (constrained to visible screen) | `Sources/Nice/State/WindowSession.swift`, `SessionStore.swift` (`PersistedFrame`) |

### E. Sidebar (sessions mode)

| # | Feature | Current implementation |
|---|---|---|
| S1 | Expanded 240pt sidebar: project groups with header + count pill + per-group "+" (Claude tab in project; terminal tab in Terminals), tab rows with status dot + title | `Sources/Nice/Views/SidebarView.swift`, `Sources/Nice/Views/StatusDot.swift`, `SessionsModel.swift` (`createClaudeTabInProject`, `createTerminalTab`) |
| S2 | Collapsed mode: sidebar column disappears; a small chrome cap hosts the traffic lights + restore button; state per window, persisted | `Sources/Nice/Views/AppShellView.swift` (layout modes), `Sources/Nice/State/SidebarModel.swift` |
| S3 | Status dots: color per status; breathing pulse + expanding ring for attention (thinking always; waiting until acknowledged) | `Sources/Nice/Views/StatusDot.swift`, `Models.swift` |
| S4 | Tab multi-select (click / ⌘-click / ⇧-range with Finder-style anchor), right-click snap-to-clicked policy | `Sources/Nice/State/SidebarTabSelection.swift` |
| S5 | Tab context menu: Rename Tab, Close Tab / Close N Tabs; project context menu: Close Project | `SidebarView.swift` (`TabRow`, `ProjectGroup`) |
| S6 | Inline rename with slow-second-click gate (rename ≠ the selecting click) | `SidebarView.swift`, `Sources/Nice/State/InlineRenameClickGate.swift` |
| S7 | Drag-reorder tabs / move tabs between projects (pure slot-math resolver) | `SidebarView.swift` (drop delegates), `SidebarDropResolver` (tested in `Tests/.../SidebarDropResolverTests.swift`) |
| S8 | Footer icon controls + sidebar-mode toggle (tabs ↔ files) + collapse toggle | `SidebarView.swift` (footer), `AppShellView.swift` (`SidebarModeIconButton`, `SidebarToggleButton`) |
| S9 | Sidebar background: flat panel (nice palette) vs wallpaper-tinted vibrancy — `NSVisualEffectView` `.sidebar`/`.behindWindow` (macOS palette) | `Sources/Nice/Views/SidebarBackground.swift`, `Sources/Nice/Views/VisualEffectView.swift` |

### F. Pane strip (toolbar)

| # | Feature | Current implementation |
|---|---|---|
| P1 | Pane pills between brand block and trailing edge: icon, status dot, title, hover close ✕, active highlight, inline rename | `Sources/Nice/Views/WindowToolbarView.swift` (`InlinePanePill`, `CloseXButton`) |
| P2 | "+" button: add a terminal pane to the active tab (auto-named "Terminal N") | `WindowToolbarView.swift` (`NewTabBtn`), `SessionsModel.swift` (`addTerminalToActiveTab`) |
| P3 | Overflow chevron with attention badge when an attention-worthy pill is scrolled offscreen; edge fades; width-estimation to dodge SwiftUI virtualization | `Sources/Nice/Views/PaneStripOverflowEstimator.swift`, `Sources/Nice/Views/PaneStripGeometry.swift`, `WindowToolbarView.swift` (`OverflowMenuButton`) |
| P4 | Pill drag → reorder within the strip | `Sources/Nice/Views/PaneStripDropResolver.swift`, `WindowToolbarView.swift` (`PaneStripDropDelegate`) |
| P5 | Pill drag → cross-window move: live pty + view handed between windows via an in-process registry (pasteboard carries only the id); Claude panes become a fresh sidebar tab in the target | `Sources/Nice/Views/PaneDragSource.swift`, `Sources/Nice/State/LivePaneRegistry.swift`, `Sources/Nice/Views/PaneMigrationCoordinator.swift`, `SessionsModel.swift` (`adoptLivePane`, `adoptClaudePaneAsNewTab`, `adoptTerminalPaneAsNewTab`, `adoptTerminalPaneAsMainTerminal`) |
| P6 | Pill drag → tear-off: released over empty desktop opens a brand-new window adopting the live pane (token-paired seed so the right window claims it) | `Sources/Nice/Views/PaneTearOffController.swift`, `NiceServices.swift` (tear-off seed pairing), `NiceApp.swift` (value-presenting `WindowGroup`) |
| P7 | Update pill (trailing edge) with popover: brew upgrade instructions when a newer GitHub release exists | `Sources/Nice/Views/UpdateAvailablePill.swift` |

### G. File explorer (sidebar files mode)

| # | Feature | Current implementation |
|---|---|---|
| F1 | Recursive disclosure tree rooted at the active tab's cwd; per-tab state (root, expansion, hidden-files) in memory, re-bound on tab switch; breadcrumb with up-nav + refresh + hidden toggle; auto-refresh via directory watcher | `Sources/Nice/Views/FileBrowserView.swift`, `Sources/Nice/State/FileBrowserState.swift`, `Sources/Nice/State/FileBrowserStore.swift`, `Sources/Nice/State/FileBrowserListing.swift` |
| F2 | Sort preferences: name vs modified-date, asc/desc, dirs always first; process-wide, persisted | `Sources/Nice/State/FileBrowserSortSettings.swift` |
| F3 | Multi-row selection (click/⌘/⇧-range) with right-click snap policy | `Sources/Nice/State/FileBrowserSelection.swift` |
| F4 | Context menu: Open, Open With ▸ (Launch Services enumeration + Other…), ~~Open in Editor Pane ▸~~ (**CUT**, §2), Reveal in Finder, Copy, Copy Path, Cut, Paste, Move to Trash — with per-target visibility rules | `Sources/Nice/Views/FileBrowserContextMenu.swift`, `Sources/Nice/State/FileExplorerOrchestrator.swift`, `Sources/Nice/State/FileOperations/OpenWithProvider.swift` |
| F5 | File operations engine: copy/move/trash with Finder-style collision auto-rename (`foo copy 2.txt`); inverse records | `Sources/Nice/State/FileOperations/FileOperationsService.swift`, `FileOperation.swift` |
| F6 | App-wide undo/redo of file operations across windows (⌘Z in window B undoes window A; focus routed back to the originator); drift detection with transient banner | `Sources/Nice/State/FileOperations/FileOperationHistory.swift`, `Sources/Nice/Views/FileOperationDriftBanner.swift` |
| F7 | Pasteboard interop with Finder both directions (`public.file-url`); in-process "cut" intent keyed to pasteboard changeCount | `Sources/Nice/State/FileOperations/FilePasteboardAdapter.swift` |
| F8 | Inline rename: basename-only pre-selection, Finder illegal-name + sibling-collision validation, extension-change confirmation, rename-would-break-open-pane-cwd warning (scans all windows) | `Sources/Nice/Views/FileBrowserRenameField.swift`, `Sources/Nice/State/FileBrowserRenameValidator.swift`, `Sources/Nice/State/FileBrowserCWDImpactCheck.swift`, `FileExplorerOrchestrator.swift` (rename flow) |
| F9 | Drag & drop inside the tree: move by default, Option = copy, cross-volume = copy, folder-into-self rejection, hover-highlight of the target folder; drags also feed terminals (T7) | `Sources/Nice/State/FileBrowserDropResolver.swift`, `Sources/Nice/State/FileBrowserDragState.swift`, `FileBrowserView.swift` |
| F10 | Double-click a file → open it (today: extension→editor routing first — **CUT**; rewrite: always OS default handler) | `FileExplorerOrchestrator.swift` (`openFromDoubleClick`) |

### H. Settings, theming, shortcuts

| # | Feature | Current implementation |
|---|---|---|
| G1 | Settings window (⌘,): left rail + panes — Appearance, Fonts, Shortcuts, ~~Editors~~ (**CUT**), advanced toggles (smooth scrolling, handoff skill, Claude theme sync) | `Sources/Nice/Views/SettingsView.swift`, `NiceApp.swift` (`Settings` scene) |
| G2 | Chrome theming: scheme (light/dark) × per-scheme chrome palette (Nice oklch literals / macOS system-semantic+vibrancy / Catppuccin Latte / Mocha); "Sync with OS" observing `AppleInterfaceThemeChangedNotification`; UserDefaults persistence + legacy migration | `Sources/Nice/State/Tweaks.swift`, `Sources/Nice/Theme/Palette.swift` |
| G3 | Accent presets (terracotta/ocean/fern/iris/graphite) tinting logo, selection, controls, terminal caret — live | `Tweaks.swift` (`AccentPreset`), `Sources/Nice/Views/Logo.swift` |
| G4 | Terminal theme catalog: bundled built-ins + user-imported Ghostty `.conf` theme files (stored as files in Application Support; per-scheme pickers with swatch previews; import-error surfacing) | `Sources/Nice/Theme/BuiltInTerminalThemes.swift`, `Sources/Nice/Theme/GhosttyThemeParser.swift`, `Sources/Nice/Theme/TerminalThemeCatalog.swift`, `Sources/Nice/Views/SettingsTerminalPane.swift` |
| G5 | Theme fan-out: scheme/palette/accent/terminal-theme/font changes repaint every live pane in every window immediately | `Sources/Nice/State/SessionThemeCache.swift`, `TabPtySession.swift` (`TabPtySessionThemeable`) |
| G6 | Rebindable keyboard shortcuts: 13 actions (next/prev sidebar tab, next/prev pane, new terminal pane, toggle sidebar, toggle sidebar mode, toggle hidden files, font ±/reset, undo/redo file op); layout-independent keyCodes; conflict detection; JSON blob in UserDefaults | `Sources/Nice/State/KeyboardShortcuts.swift` |
| G7 | Global shortcut dispatch: one process-wide keyDown monitor routing to the focused window; stands down while recording or in Settings | `Sources/Nice/Process/KeyboardShortcutMonitor.swift` |
| G8 | Shortcut recorder UI: capture next combo, conflict Replace/Cancel, per-action Reset | `Sources/Nice/Views/KeyRecorderField.swift` |
| G9 | Font settings pane: terminal + sidebar size sliders (live), reset; sidebar elements scale proportionally | `Sources/Nice/Views/SettingsFontPane.swift`, `FontSettings.swift` |

### I. Persistence & lifecycle

| # | Feature | Current implementation |
|---|---|---|
| L1 | Session persistence: `sessions.json` (versioned schema v3) — windows × projects × tabs × panes, active ids, sidebar collapsed, frames; debounced 500ms writes off-main; synchronous flush on close/quit | `Sources/Nice/State/SessionStore.swift`, `Sources/Nice/State/WindowSession.swift` |
| L2 | Multi-window restore: per-window identity (`windowSessionId` via SceneStorage), claim ledger prevents double-adoption, launch fan-out opens one window per unclaimed saved slot (AppKit's own restoration disabled) | `WindowSession.swift`, `Sources/Nice/State/WindowClaimLedger.swift`, `AppShellView.swift` (`AppShellHost.task`), `NiceServices.swift` (`bootstrap`, `consumeMultiWindowRestoreSlot`) |
| L3 | Restore behavior: Claude tabs come back as deferred-resume prefill (C5); terminal tabs respawn fresh shells in their saved cwd; heal passes for stale cwds | `WindowSession.swift` (`restoreSavedWindow`, heal helpers), `TabPtySession.swift` (`.resumeDeferred`) |
| L4 | Launch bootstrap: temp-file sweep, orphan reap, ZDOTDIR write, claude probe, hook install, theme-sync write, skill sync, editor scan (**editor scan is CUT**) | `NiceServices.swift` (`bootstrap`) |
| L5 | Tab dissolve cascade: last pane exits → tab dissolves → empty project row removed → possibly app terminates; file-browser state cleanup rides it | `Sources/Nice/State/AppState.swift` (`finalizeDissolvedTab`, `dissolveTabIfEmpty`), `CloseRequestCoordinator.swift` |

### J. Updates

| # | Feature | Current implementation |
|---|---|---|
| U1 | Update check: GitHub `releases/latest` every 6h, semantic-version compare vs `CFBundleShortVersionString`, cached last-seen tag, silent failures; surfaces as P7's pill (no auto-update — brew instructions only) | `Sources/Nice/State/ReleaseChecker.swift`, `ReleaseFetcher.swift`, `SemanticVersion.swift` |

**Notable absences (confirmed, not features):** no system notifications
(no UserNotifications anywhere — attention is in-app dots only); no split
panes inside one tab view (panes are pills, one visible at a time); no
search-in-scrollback UI; no profiles/multiple shells (zsh is assumed
throughout the injection layer).

---

## 2. Cut features (documented, NOT scheduled)

### Editors (editor-per-file-type overrides) — CUT

Today Nice lets the user configure terminal editors and route file
extensions to them; double-clicking a matching file in the file explorer
opens it in that editor **in a new terminal pane** (`zsh -ilc "exec <editor>
<file>"`), and a context-menu submenu offers the same. The rewrite drops all
of it: **double-click (and "Open") simply opens the file with the OS default
handler** (NSWorkspace-equivalent — same as Finder). That trivial default-open
behavior is part of the file-explorer roadmap item (R19); no other work is
scheduled.

Where the feature lives today (for the record):

- `Sources/Nice/Views/SettingsEditorsPane.swift` — the whole Settings ▸
  Editors pane (user editor rows, detected-editor promotion, extension
  routing table).
- `Sources/Nice/State/EditorDetector.swift` — launch-time PATH probe for
  vim/nvim/hx/… via a login-interactive zsh.
- `Sources/Nice/State/Tweaks.swift` — `EditorCommand` (line ~179),
  `editorCommands` / `extensionEditorMap` storage + mutators
  (`addEditor`/`updateEditor`/`removeEditor`/`setMapping`/`removeMapping`,
  `editor(for:)`, `editor(forExtension:)`, persistence, ~lines 343–800).
- `Sources/Nice/State/FileExplorerOrchestrator.swift` —
  `openFromDoubleClick` (extension→editor routing), `editorPaneEntries`,
  `openInEditorPane`, `editorPaneSpec` (spawns the editor as a pane).
- `Sources/Nice/Views/FileBrowserContextMenu.swift` — the
  "Open in Editor Pane ▸" submenu.
- Wiring: `NiceServices.editorDetector` + `bootstrap()` scan;
  `NiceApp.swift` environment injection; `AppState.init` threading.
- Tests that die with it: `TweaksEditorsTests`, `EditorDetectorTests`,
  `AppStateOpenInEditorPaneTests`, `FileBrowserOpenInEditorUITests`.

Kept nearby (NOT cut): "Open With ▸" (Launch Services app enumeration,
`OpenWithProvider.swift`) and "Reveal in Finder" — those are OS-level, not
editor overrides. Also kept: `TabPtySession.addTerminalPane(command:)`'s
generic run-a-command-in-a-pane capability is not scheduled for the rewrite
(its only user was the editor pane); re-add it later only if something needs
it.

Also not carried: `Tab.branch` (M5, vestigial dead field).

---

## 3. Roadmap

Nine stages, 27 items (R1–R27). Each stage ends runnable/testable. Sizes are
relative: **S** ≈ a day or two, **M** ≈ up to a week, **L** ≈ multi-week.
"Deps" reference earlier items. Rust/GPUI mappings name crates only where the
decision report or the code makes them obvious.

### Stage 0 — Foundations (runnable empty app)

- **R1. Cargo workspace + app skeleton + packaging.** GPUI app on a pinned
  zed `main` rev (per §13 — 0.2.2 is stale for input work), one window,
  `.app` bundling + codesign/notarize hook-up so every later stage is
  installable (reuse `scripts/` + `.github/workflows/release.yml` shape).
  Carry forward the `spikes/phase0-poc` harness learnings (present pacing,
  signposts). Maps to: `gpui`, `gpui_platform`, cargo-bundle or a bespoke
  bundling script. Size **M**. Deps: none.
- **R2. Design-token port.** Nice palette (oklch→sRGB literals), accent
  presets, typography scale, chrome geometry constants as a Rust module —
  everything downstream paints with these. Sources: `Palette.swift`,
  `Tweaks.swift` (AccentPreset), `WindowChrome.swift`, `Typography.swift`.
  Size **S**. Deps: R1.

**Milestone 0:** signed `Nice Dev`-style .app opens a blank GPUI window.

### Stage 1 — Terminal core (the load-bearing subsystem)

- **R3. VT core + pty.** `alacritty_terminal` 0.26 (grid, scrollback,
  damage, selection incl. eviction-safe anchors — verified fork-free);
  pty spawn replicating T2 semantics (`zsh -ilc "exec …"`, login shell, env
  injection, tilde expansion) and T3 deferred-spawn arming. Sources:
  `TabPtySession.swift`, `ShellQuoting.swift`. Size **L**. Deps: R1.
- **R4. GPUI terminal renderer.** From-scratch cell painter on public GPUI
  primitives (`shape_line().paint()`, `paint_quad`, `paint_glyph`,
  `paint_emoji`): text runs, 16-ANSI theme (T12), cursor (accent caret),
  selection, underline/strikethrough, box-drawing, row-quantized
  bottom-anchored layout (T4), scrollback scrolling (smooth-scroll knob
  behind a setting — GPUI main pixel-snaps; line-stepped first, sub-line as
  the §12 additive follow-up). **GPL rule applies hard here** — reference
  alacritty's Apache frontend and gpui-ghostty only. Size **L**. Deps: R3.
- **R5. Input: keyboard + IME.** GPUI platform `InputHandler` implemented
  **directly** (~400–500 LOC per §13 scoping; NOT
  `EntityInputHandler`/`ElementInputHandler` — spike 2 proved that path
  unusable for terminals: the blanket impl ties
  `prefers_ime_for_printable_keys` to `accepts_text_input`): marked text
  rendered inline, `bounds_for_range` anchored to the cursor cell (never
  `None` while composing), Enter-commit swallow; kitty keyboard CSI-u
  encoding (T8) — `termwiz` `KeyboardEncoding::Kitty` or alacritty
  `keyboard.rs` (Apache) as the encoder reference, keyCode recovery via
  objc2 side-channel (GPUI's `Keystroke` carries no keyCode on the pin).
  *(Update 2026-07-02: the §13 G1 gate CLOSED fork-free — live checklist
  FULL PASS on the pin; the gpui_macos contingency patch retired unused.
  See `spikes/phase0-poc/ime-spike/RESULTS-spike2-20260702.md`.)*
  Size **L**. Deps: R4.
- **R6. OSC plumbing.** OSC 0/1/2 titles surfaced to the app layer (T5's
  transport half) and OSC 7 cwd (T6) — vte 0.15 has no OSC 7 arm, so either
  the pty-stream tee or a ~10-line vte ansi patch (per §12); bracketed-paste
  state exposure for T7. Size **M**. Deps: R3.
- **R7. Terminal niceties.** Font chain + size + live zoom (T11, monospace
  via GPUI text system), file/image drop → escaped path + bracketed paste
  (T7, GPUI `ExternalPaths` drop), "Launching…" overlay (T9), held panes
  (T10). Size **M**. Deps: R4, R6.

**Milestone 1:** a single-pane terminal window: 60fps, themed, IME-clean,
kitty-keyboard, correct zsh env — measurable against SwiftTerm side-by-side
(the §12 AA/gamma pixel comparison closes here).

### Stage 2 — App shell: model, chrome, sidebar, pane strip

- **R8. Tabs/panes/projects model.** Direct port of the pure value tree +
  status model (M1–M4): Terminals project, cwd bucketing, ≤1-Claude
  invariant, title locks, auto-numbering, dissolve cascade (L5's model
  half). This code is SwiftUI-free today — highest-fidelity port in the
  plan; keep the unit-test suite's semantics
  (`Tests/NiceUnitTests/TabModel*`, `Models`). Size **M**. Deps: R1.
- **R9. Window chrome.** Hidden titlebar + traffic lights (W1): GPUI
  `TitlebarOptions { appears_transparent, traffic_light_position }` replaces
  the entire `TrafficLightPlacer`/`WindowChromeController` AppKit dance —
  verify the y=26/x+8 geometry is reachable; empty-chrome drag-to-move +
  double-click `AppleActionOnDoubleClick` (W2 — GPUI has no equivalent of
  the ChromeEventRouter; implement on GPUI mouse listeners + objc2 for the
  user-default read and zoom/miniaturize calls); full-screen menu item (W4).
  Size **M**. Deps: R1, R2.
- **R10. Sidebar, sessions mode.** S1–S9 minus drag-reorder: groups, rows,
  status dots (pulse animations via GPUI animation), count pills, "+"
  buttons, context menus, multi-select, inline rename with click gate,
  collapsed cap mode, footer + mode/collapse toggles. Vibrancy (S9):
  GPUI `WindowBackgroundAppearance::Blurred` approximates the
  NSVisualEffectView sidebar material — accept approximation or objc2
  layer-under work; decide when seen. `gpui-component` (Apache-2.0) is a
  legitimate accelerant for menus/list rows. Size **L**. Deps: R8, R9.
- **R11. Pane strip.** P1–P3: pills with dots/close/rename, "+" button,
  overflow chevron + attention badge + edge fades (the width-estimator
  workaround likely dies — GPUI gives real layout access). Size **M**.
  Deps: R8, R9.
- **R12. Multi-window + registry + shortcut dispatch.** W3 (⌘N isolated
  windows — per-window state struct mirroring `AppState`'s decomposition),
  window registry / focused-window routing, fixed default keybindings for
  the 13 actions (G6's defaults) through GPUI's action/keymap system (the
  rebinding UI comes in Stage 6). Size **M**. Deps: R10, R11.
- **R13. Terminal session manager.** Port of `SessionsModel`'s pane
  lifecycle half: per-tab session objects owning terminal instances,
  active-pane focus, deferred spawn on focus, exit→dissolve wiring,
  next/prev pane/tab actions. Size **M**. Deps: R8, R3–R7, R12.

**Milestone 2:** a multi-window, multi-tab, multi-pane terminal app with the
Nice sidebar and pane strip — no Claude, no persistence. Daily-drivable for
plain shells.

### Stage 3 — Claude integration

- **R14. Shell injection + control socket.** C1 (ZDOTDIR synthetic rc
  chain, `claude()` shadow, OSC 7 hook, `NICE_PREFILL_COMMAND` — the shell
  scripts port almost verbatim) + C2 (Unix socket listener; Rust std
  `UnixListener` on a background thread replaces the DispatchSource dance;
  same NDJSON protocol so existing shell helpers keep working). Sources:
  `MainTerminalShellInject.swift`, `NiceControlSocket.swift`. Size **M**.
  Deps: R13.
- **R15. Claude tab lifecycle.** C3 (newtab/inplace promotion protocol),
  C4 (session-id minting, `--session-id`/`--resume` exec building), T5's
  status parsing (braille/✳ → thinking/waiting + acknowledgment), C8
  (worktree cwd following), C11 (claude path probe), C12 (orphan reaper —
  straight libproc/sysctl port). Size **L**. Deps: R14.
- **R16. Session-rotation tracking.** C6 (SessionStart hook installer —
  file-writing logic ports directly, protocol unchanged) + C7 (`/branch`
  parent materialization + lineage rendering M4). Size **M**. Deps: R15.
- **R17. Claude theme sync.** C10: theme file writer + `--settings` pointer
  + in-place reply field. Gate on the same default-ON toggle. Size **S**.
  Deps: R15 (and R21 for live-theme fan-out, wire-up then).

**Milestone 3:** typing `claude` anywhere opens/promotes tabs; statuses
pulse; `/clear`/`/branch` tracked — Claude parity with today minus restore.

### Stage 4 — Persistence & restore

- **R18. Session store + window restore + lifecycle guards.** L1 (serde
  JSON, same schema shape — consider reading today's v3 `sessions.json` for
  seamless migration; drop the `branch` field), L2 (window identity + claim
  ledger + launch fan-out; no SceneStorage in GPUI — persist window ids in
  the store itself), L3 (deferred-resume prefill restore via C5, cwd heal),
  W5 (close/quit confirmation counting live panes), W6 (frames), L4 (the
  bootstrap sequence order). Size **L**. Deps: R13, R15 (prefill), R12.

**Milestone 4:** quit and relaunch restores windows/tabs/panes with
`claude --resume` pre-typed — the app is now a credible daily driver.

### Stage 5 — File explorer

- **R19. Tree + listing + open.** F1 (tree, breadcrumb, per-tab state,
  directory watcher — `notify` crate or FSEvents via objc2), F2 (sort
  settings), F3 (selection), F10 as **plain OS-default open** (the cut's
  replacement: `NSWorkspace.open`-equivalent via objc2-app-kit or the
  `open` crate), Reveal in Finder, Open With ▸ enumeration (F4's kept
  entries; Launch Services via objc2). Size **L**. Deps: R10, R18 (per-tab
  cwd), R2.
- **R20. File ops + undo + rename + DnD.** F5 (ops service w/ collision
  rename; trash via objc2 NSWorkspace recycle or `trash` crate), F6
  (cross-window undo history + drift banner), F7 (NSPasteboard file-URL
  interop — objc2; GPUI clipboard API doesn't carry file URLs), F8 (inline
  rename + validators + cwd-impact scan), F9 (in-tree drag/drop rules +
  drop-into-terminal already covered by R7). Size **L**. Deps: R19.

**Milestone 5:** files mode at parity (minus editors, by design).

### Stage 6 — Settings, theming, shortcuts UI

- **R21. Theme system + fan-out.** G2 (scheme × per-scheme chrome palette,
  OS sync via the distributed-notification equivalent — objc2 observer or
  gpui `window_appearance` observation), G3 (accents), G5 (live fan-out to
  all windows/panes), S9 finalization, wire R17 to live changes. Size
  **M**. Deps: R12, R2.
- **R22. Terminal theme catalog + Ghostty import.** G4: built-in table port
  (`BuiltInTerminalThemes.swift` is data — transcribe), `key = value`
  parser port, imported files in Application Support. Size **S**. Deps:
  R21.
- **R23. Settings window.** G1 shell (GPUI has no Settings scene — plain
  window bound to ⌘,), Appearance pane, Fonts pane (G9, live sliders),
  advanced toggles (smooth scroll, handoff skill, Claude theme sync).
  Size **M**. Deps: R21, R22.
- **R24. Rebindable shortcuts.** G6 (persisted binding map + conflict
  detection), G7 (dispatch — in GPUI this is keymap rebinding rather than
  an NSEvent monitor), G8 (recorder field with capture mode). Size **M**.
  Deps: R23, R12.

**Milestone 6:** full preferences parity (minus Editors pane).

### Stage 7 — Advanced pane management

- **R25. Pill drag: reorder, cross-window move, tear-off.** P4 (strip
  reorder — GPUI `on_drag`/drop), P5 (live-pane registry + migration
  coordinator: in-process handoff of a running terminal entity between
  windows — GPUI entities are app-global, which should make this *simpler*
  than the AppKit version), P6 (tear-off: detect drag ended outside any
  Nice window and open a new window adopting the pane). **Open question:
  GPUI's drag sessions are in-app; "released over empty desktop" detection
  needs verification — fallback design: a drag-out-of-window-bounds
  threshold instead of NSDraggingSource end-operation semantics.** Sources:
  `PaneDragSource.swift`, `LivePaneRegistry.swift`,
  `PaneMigrationCoordinator.swift`, `PaneTearOffController.swift`. Size
  **L**. Deps: R11, R12, R13, R18.

**Milestone 7:** full pane-drag parity (the current app's flagship trick).

### Stage 8 — Ecosystem & polish

- **R26. Handoff.** C9: skill installer + first-launch prompt + socket
  handler + nested `[HANDOFF]` tab with model/effort flags. Size **M**.
  Deps: R16, R18, R23 (toggle).
- **R27. Update checker + pill.** U1 + P7: `reqwest`/`ureq` + serde against
  GitHub releases, version compare, cached tag, popover with brew commands.
  Size **S**. Deps: R11.

**Milestone 8 (parity):** feature-complete vs. the inventory minus the
documented cut; retire the Swift app.

---

## 4. Open design questions (flagged, not blocking the order)

1. **IME arbitration (§13 G1)** — the one gate that could force a small
   `gpui_macos` patch; resolved inside R5 by the live spike
   (`spikes/phase0-poc/ime-spike/SCOPE.md`).
2. **Tear-off end-of-drag detection** (R25) — no NSDraggingSource
   `endedAt:operation:` equivalent confirmed in GPUI; verify or redesign
   the gesture.
3. **Vibrancy parity** (R10/R21) — `WindowBackgroundAppearance::Blurred`
   vs. real `.sidebar` material Desktop Tinting; accept approximation or
   objc2 effect-view layering.
4. **Smooth scrolling** (R4) — line-stepped ships first; sub-line scroll is
   the known one-constant text-core patch decision from §13 spike 11.
5. **sessions.json migration** (R18) — read the Swift app's v3 file so
   users' tabs survive the app swap, or version-bump and start clean.
   Recommendation: read v3 (schema is small and stable).
6. **objc2 surface area** — NSWorkspace (open/reveal/trash), NSPasteboard
   file URLs, AppleActionOnDoubleClick, Launch Services: all-Rust via
   objc2-app-kit bindings is consistent with Path B (bindings ≠ second
   language runtime); keep them behind one `platform` module.
