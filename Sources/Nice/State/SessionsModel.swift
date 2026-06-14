//
//  SessionsModel.swift
//  Nice
//
//  Per-window pty / control-socket / theme-fan-out subsystem. Carved
//  out of `AppState` so the long-lived process plumbing has its own
//  cohesive object — separate from the data model (`TabModel`) and
//  from window-level orchestration concerns (close-confirmation,
//  persistence, sidebar UI) that remain on `AppState`.
//
//  Callbacks bridge the cross-cutting parts that aren't ours to own:
//    - `onSessionMutation` lets the owning `AppState` schedule a
//      debounced session save when a session-driven mutation
//      (in-place Claude promotion, claude-session id rotation, new
//      tab spawn, pane cwd update) lands. The save-gate flags
//      (`isInitializing`, `persistenceEnabled`) live on AppState.
//    - `onTabBecameEmpty` fires from `paneExited` when the last pane
//      on a tab is gone. AppState's handler then runs the dissolve
//      cascade (project removal, file-browser cleanup, save,
//      potential `NSApp.terminate`) using `removePtySession` on us
//      to drop the tab-level pty cache.
//
//  This model intentionally does not know about persistence,
//  `NiceServices`, the file-browser store, or the `pendingCloseRequest`
//  alert. It does know about `TabModel` because it has to mutate the
//  document in response to pty events (pane exits, OSC titles, OSC 7
//  cwd updates) and because tab creation paths spawn ptys for tabs it
//  just appended.
//

import AppKit
import Foundation
import Observation
import os
import SwiftUI

@MainActor
@Observable
final class SessionsModel {
    /// Logger for the cross-window pane-transfer adopt paths. Shares the
    /// "tearoff" category with `PaneTearOffController` so a tear-off /
    /// migration that hits an unexpected branch (e.g. a deferred Claude
    /// pane with no session id) is greppable as one stream.
    static let tearOffLog = Logger(
        subsystem: "dev.nickanderssohn.nice", category: "tearoff"
    )
    /// Tab-keyed pty-session cache. Each entry owns its tab's
    /// `TabPtySession`, which in turn owns one or more pane subprocesses.
    private(set) var ptySessions: [String: TabPtySession] = [:]

    /// Launch state per pane, used to overlay a "Launching…" placeholder
    /// while a freshly-spawned child is still silent. Entries are created
    /// by `registerPaneLaunch` at spawn time (`.pending`), flip to
    /// `.visible` if the child stays quiet for more than 0.75 s, and are
    /// cleared on first pty byte or pane exit. The 0.75 s grace window
    /// exists so fast-starting processes (regular `claude`, a plain
    /// shell) never flash the overlay — the common case is uninterrupted.
    private(set) var paneLaunchStates: [String: PaneLaunchStatus] = [:]

    /// Seam for the pending → visible grace window. Unit tests set this
    /// to 0 so promotion is synchronous.
    var launchOverlayGraceSeconds: Double = 0.75

    @ObservationIgnored
    private var controlSocket: NiceControlSocket?

    /// Process-wide ZDOTDIR path owned by `NiceServices`. Stored here
    /// so terminal-pane spawns can inject it as an env var without
    /// reaching back through the services reference. Never deleted by
    /// this model — the owning `NiceServices` cleans it up at app
    /// terminate.
    @ObservationIgnored
    private var zdotdirPath: String?

    /// `ZDOTDIR` Nice inherited from its launch env (or nil). Plumbed
    /// through to ptys as `NICE_USER_ZDOTDIR` so the synthetic .zshenv
    /// can restore it after our injection runs. See `NiceServices`
    /// for the why; see `MainTerminalShellInject` for how the shell
    /// stubs use it.
    @ObservationIgnored
    private var userZDotDir: String?

    /// Extra environment variables threaded into every pty spawn:
    /// `NICE_SOCKET` so the zsh `claude()` shadow can reach this
    /// window's socket, and `ZDOTDIR` so shell rcs are sourced from
    /// the shared per-process directory.
    @ObservationIgnored
    private(set) var controlSocketExtraEnv: [String: String] = [:]

    /// Theme/font cache + fan-out target. Holds the chrome
    /// scheme/palette/accent triple, the terminal theme, and the
    /// terminal font family/size — the values every live pty session
    /// is painted with. `updateScheme` / `updateTerminalFontSize` /
    /// `updateTerminalTheme` / `updateTerminalFontFamily` on
    /// `SessionsModel` are thin forwarders to this cache; the cache
    /// walks `ptySessions.values` (via the closure passed at init)
    /// to fan out. `makeSession` calls `themeCache.applyAll(to:)` to
    /// seed a freshly-spawned session with the current cache state.
    let themeCache: SessionThemeCache

    /// Absolute path to the `claude` binary if we've resolved it; nil
    /// falls back to zsh inside claude panes. Mirrors
    /// `services.resolvedClaudePath`; AppState writes this via
    /// `setResolvedClaudePath` when the async probe completes.
    @ObservationIgnored
    private var resolvedClaudePath: String?

    /// Document the model mutates from pty / socket callbacks. Held
    /// weakly because TabModel and SessionsModel are co-owned by the
    /// per-window `AppState` and have the same lifetime — the weak
    /// reference is cycle insurance, not a lifetime divergence.
    @ObservationIgnored
    private weak var tabs: TabModel?

    /// Fired when a session-driven mutation should bounce out to the
    /// owning AppState's `scheduleSessionSave`. Examples: in-place
    /// Claude promotion, claude-session id rotation, new tab spawn,
    /// per-pane cwd update.
    @ObservationIgnored
    var onSessionMutation: (() -> Void)?

    /// Fired from `paneExited` when the tab's panes array goes to zero.
    /// AppState's handler runs the dissolve cascade: tree-row removal,
    /// file-browser state cleanup, project pending-removal check,
    /// save, and the all-projects-empty terminate check. The handler
    /// is responsible for calling `removePtySession(tabId:)` on us
    /// during that cascade.
    @ObservationIgnored
    var onTabBecameEmpty: ((_ tabId: String, _ projectIndex: Int, _ tabIndex: Int) -> Void)?

    /// Mint a unique id for a freshly-created tab (or pane). The
    /// production default is `<prefix><ms>-<uuid4>` — millisecond
    /// timestamp keeps ids roughly time-sortable (useful for log
    /// triage), the four-char UUID suffix keeps two creations within
    /// the same millisecond from colliding. Two `/branch`es fired in
    /// quick succession by a script (`--fork-session` in a loop) used
    /// to land in the same ms bucket and produce duplicate tab ids;
    /// the suffix closes that hole. Injectable so unit tests can pass
    /// a deterministic counter and assert by id when they need to.
    @ObservationIgnored
    private let mintTabId: @MainActor (String) -> String

    init(
        tabs: TabModel,
        mintTabId: @escaping @MainActor (String) -> String = SessionsModel.defaultMintTabId
    ) {
        self.tabs = tabs
        self.mintTabId = mintTabId
        // Build the cache with a placeholder receivers closure
        // first — `self` isn't usable until every stored property is
        // assigned. Once init is complete we rebind the closure to
        // `[weak self]` querying `ptySessions.values`, so newly-
        // spawned sessions auto-join the receiver list each fan-out
        // call without any add/remove notification.
        self.themeCache = SessionThemeCache()
        self.themeCache.receivers = { [weak self] in
            guard let self else { return [] }
            return Array(self.ptySessions.values)
        }
        // Production wiring for the Claude theme mirror. Hermeticity holds
        // two ways: SessionThemeCacheTests construct the cache directly and
        // keep the no-op default; SessionsModel/AppState tests run under
        // TestHomeSandbox, whose $HOME redirect ClaudeThemeSync.homeBase()
        // honors, so any write lands in the sandbox, never the real
        // ~/.claude. See `SessionThemeCache.claudeThemeWriter` / `ClaudeThemeSync`.
        self.themeCache.claudeThemeWriter = { theme, scheme, accent in
            ClaudeThemeSync.write(theme: theme, scheme: scheme, accent: accent)
        }
    }

    /// Production minter used by `init`'s default. Format:
    /// `<prefix><ms>-<uuid4>` (e.g. `t1751234567890-a3f2`). Lives as a
    /// static so the default-argument expression at the init site
    /// can reference it without capturing `self`.
    static func defaultMintTabId(prefix: String) -> String {
        let ms = Int(Date().timeIntervalSince1970 * 1000)
        let suffix = UUID().uuidString.prefix(4).lowercased()
        return "\(prefix)\(ms)-\(suffix)"
    }

    // MARK: - Theme cache forwarders

    /// Forward to `themeCache.updateScheme`. Production callers
    /// (`AppState.init`, `AppShellHost`) drive the theme through
    /// `appState.sessions.updateScheme(...)` — the forwarder keeps
    /// that surface stable now that the cache lives on a peer type.
    func updateScheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        themeCache.updateScheme(scheme, palette: palette, accent: accent)
    }

    /// Forward to `themeCache.updateTerminalFontSize`.
    func updateTerminalFontSize(_ size: CGFloat) {
        themeCache.updateTerminalFontSize(size)
    }

    /// Forward to `themeCache.updateTerminalTheme`.
    func updateTerminalTheme(_ theme: TerminalTheme) {
        themeCache.updateTerminalTheme(theme)
    }

    /// Forward to `themeCache.updateTerminalFontFamily`.
    func updateTerminalFontFamily(_ name: String?) {
        themeCache.updateTerminalFontFamily(name)
    }

    /// Forward to `themeCache.updateSmoothScrolling`.
    func updateSmoothScrolling(_ enabled: Bool) {
        themeCache.updateSmoothScrolling(enabled)
    }

    /// Forward to `themeCache.updateSyncClaudeTheme`. Called when the user
    /// toggles "Sync Claude Code theme" so this window's cache stops/starts
    /// handing the `--settings` pointer to newly spawned panes (and writes
    /// the theme file on enable).
    func updateSyncClaudeTheme(_ enabled: Bool) {
        themeCache.updateSyncClaudeTheme(enabled)
    }

    /// Update the resolved `claude` binary path. Called by AppState
    /// from its `armClaudePathTracking` handler when `services.resolvedClaudePath`
    /// flips, and at init/start time to seed the cache.
    func setResolvedClaudePath(_ path: String?) {
        resolvedClaudePath = path
    }

    // MARK: - Lifecycle

    /// Allocate the control socket and prepare its env-injection table.
    /// Does NOT start the socket listener yet — pty spawns must read
    /// `NICE_SOCKET` from `controlSocketExtraEnv` before the listener is
    /// armed, so AppState's choreography is bootstrap → seed-main-pty
    /// → start-listener.
    func bootstrapSocket(zdotdirPath: String?, userZDotDir: String?) {
        self.zdotdirPath = zdotdirPath
        self.userZDotDir = userZDotDir

        // Allocate the control socket *before* spawning any ptys —
        // the shells need NICE_SOCKET in their environment at startup
        // or the `claude()` shadow can't reach us. Each window owns
        // its own socket so a `claude` invocation in one window's
        // Main Terminal only opens a tab in that window.
        let socket = NiceControlSocket()
        self.controlSocket = socket

        var extraEnv: [String: String] = [:]
        extraEnv["NICE_SOCKET"] = socket.path
        if let zdotdirPath {
            extraEnv["ZDOTDIR"] = zdotdirPath
        }
        // Always set NICE_USER_ZDOTDIR (empty if Nice didn't inherit
        // one) so the synthetic .zshenv's `[[ -n "$NICE_USER_ZDOTDIR" ]]`
        // check distinguishes "we have a launch-env value" from "fall
        // back to sourcing ~/.zshenv ourselves" cleanly.
        extraEnv["NICE_USER_ZDOTDIR"] = userZDotDir ?? ""
        self.controlSocketExtraEnv = extraEnv
    }

    /// Arm the socket message handler. Pre-condition: `bootstrapSocket`
    /// already ran. Idempotent against repeat calls because the socket
    /// itself is a one-shot.
    func startSocketListener() {
        guard let socket = controlSocket else { return }
        do {
            try socket.start { [weak self] message in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    switch message {
                    case let .claude(cwd, args, tabId, paneId, reply):
                        self.handleClaudeSocketRequest(
                            cwd: cwd, args: args,
                            tabId: tabId, paneId: paneId,
                            reply: reply
                        )
                    case let .sessionUpdate(paneId, sessionId, source, cwd):
                        self.handleClaudeSessionUpdate(
                            paneId: paneId,
                            sessionId: sessionId,
                            source: source,
                            cwd: cwd
                        )
                    case let .handoff(cwd, handoffFile, instructions, tabId, paneId, reply):
                        self.handleHandoffRequest(
                            cwd: cwd,
                            handoffFile: handoffFile,
                            instructions: instructions,
                            tabId: tabId,
                            paneId: paneId,
                            reply: reply
                        )
                    }
                }
            }
        } catch {
            NSLog("SessionsModel: control socket failed to bind: \(error)")
        }
    }

    /// Tear down every pty and stop the control socket. Called by
    /// AppState.tearDown after persisting. Safe to call more than once.
    func tearDown() {
        for session in ptySessions.values {
            session.terminateAll()
        }
        ptySessions.removeAll()
        controlSocket?.stop()
        controlSocket = nil
    }

    // MARK: - Pane lifecycle handlers (called by TabPtySession callbacks)

    /// A pane exited. Remove it from its tab, pick a neighbor to focus,
    /// and dissolve the tab if nothing remains. Dissolve cleanup is
    /// fanned out to AppState via `onTabBecameEmpty` because it touches
    /// concerns this model doesn't own (file browser, project pending-
    /// removal, persistence, app-quit).
    func paneExited(tabId: String, paneId: String, exitCode: Int32?) {
        guard let tabs else { return }
        clearPaneLaunch(paneId: paneId)
        tabs.mutateTab(id: tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.panes.remove(at: idx)
            if tab.activePaneId == paneId {
                // Same neighbor rule a cross-window move uses
                // (`TabModel.extractPane`), shared so a moved pane and
                // an exited pane re-focus identically.
                tab.activePaneId = TabModel.neighborActivePaneId(
                    afterRemovingIndex: idx, from: tab.panes
                )
            }
        }

        ptySessions[tabId]?.removePane(id: paneId)
        // If focus auto-switched onto the lazily-spawned companion
        // terminal as a result of this exit, start its shell now.
        ensureActivePaneSpawned(tabId: tabId)

        guard let (pi, ti) = tabs.projectTabIndex(for: tabId),
              tabs.projects[pi].tabs[ti].panes.isEmpty
        else { return }

        onTabBecameEmpty?(tabId, pi, ti)
    }

    /// A pane's process exited but `TabPtySession` decided to keep its
    /// view mounted so the user can read whatever the process printed
    /// before dying — typical for `claude -w foo` outside a git repo,
    /// `claude --bad-flag`, or any non-zero exit. Flip `Pane.isAlive`
    /// to false so the rest of the model treats it as dead (sidebar
    /// status dot, `livePaneCounts`, `Tab.hasClaude`,
    /// `CloseRequestCoordinator.isBusy`) while leaving the pane in the
    /// tab's `panes` array so the toolbar pill still renders and the
    /// SwiftTerm view stays on screen with its scrollback intact. Also
    /// dismisses the launch overlay; without this an exit-before-first-
    /// byte would leave the "Launching…" placeholder on screen forever.
    /// The actual model removal happens later when the user closes the
    /// tab and `TabPtySession.terminatePane` synthesizes the deferred
    /// `onPaneExit`.
    func paneHeld(tabId: String, paneId: String, exitCode: Int32?) {
        guard let tabs else { return }
        clearPaneLaunch(paneId: paneId)
        tabs.mutateTab(id: tabId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.panes[pi].isAlive = false
            // Idle-out any pulsing status — a held-dead Claude pane is
            // not thinking or waiting for input regardless of what its
            // last OSC title said.
            tab.panes[pi].status = .idle
            tab.panes[pi].waitingAcknowledged = false
            tab.panes[pi].isClaudeRunning = false
        }
    }

    /// A pane emitted a window-title update via OSC 0/1/2. Claude panes
    /// encode thinking/waiting as a leading braille-spinner or asterisk;
    /// the trailing text is the session label (e.g. "fix-top-bar-height")
    /// which becomes the sidebar tab title. The claude-pane pill itself
    /// stays pinned to "Claude". Terminal panes take the emitted title
    /// verbatim as their toolbar pill label.
    func paneTitleChanged(tabId: String, paneId: String, title: String) {
        guard let tabs,
              let tab = tabs.tab(for: tabId),
              let pane = tab.panes.first(where: { $0.id == paneId })
        else { return }

        if pane.kind == .terminal {
            let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return }
            // Once the user has manually renamed this pane via the
            // pane-pill editor, OSC titles from the running program
            // (vim/zsh themes/etc.) must not overwrite their choice.
            // The lock clears via `renamePane(... to: "")`. Mirrors the
            // `Tab.titleManuallySet` gate in `applyAutoTitle`.
            if pane.titleManuallySet { return }
            let clipped: String = {
                guard trimmed.count > 40 else { return trimmed }
                let idx = trimmed.index(trimmed.startIndex, offsetBy: 40)
                return String(trimmed[..<idx]).trimmingCharacters(in: .whitespaces)
            }()
            tabs.mutateTab(id: tabId) { tab in
                guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                    return
                }
                if tab.panes[pi].title != clipped {
                    tab.panes[pi].title = clipped
                }
            }
            return
        }

        // Claude pane but claude isn't actually running: the underlying
        // pty is a plain zsh in `.resumeDeferred` mode — either a
        // restored tab waiting for the user to hit Enter on the pre-
        // typed `claude --resume <uuid>`, or a freshly-materialized
        // /branch parent (which uses the same mode). zsh themes
        // (oh-my-zsh, p10k, starship, …) emit OSC window titles like
        // "user@host:cwd" on every prompt; those would otherwise flow
        // into `applyAutoTitle` and clobber the persisted Claude
        // session label. Skip the entire Claude branch — no status
        // transition (zsh has no thinking/waiting semantics) and no
        // tab-title application — until `handleClaudeSocketRequest`
        // flips `isClaudeRunning` and the real Claude takes over the
        // OSC stream. `false` appears at pane creation, when a held
        // pane is recorded via `paneHeld` (Claude exited; the pty is
        // a corpse, not a live shell), and never spontaneously
        // otherwise — every other false→true transition is driven by
        // a deliberate Claude-startup site (`paneHeld` is the inverse
        // direction; held panes can't go back to running without a
        // fresh spawn).
        guard pane.isClaudeRunning else { return }

        // Claude pane: split off the status prefix, update pane/tab
        // status, and feed the trailing label into the tab title.
        guard let first = title.unicodeScalars.first else { return }
        let newStatus: TabStatus?
        let labelStart: String.Index
        if first.value >= 0x2800 && first.value <= 0x28FF {
            // Braille-spinner prefix: Claude is thinking.
            newStatus = .thinking
            labelStart = title.index(after: title.startIndex)
        } else if first == "\u{2733}" {
            // Sparkle: Claude is waiting for input.
            newStatus = .waiting
            labelStart = title.index(after: title.startIndex)
        } else {
            newStatus = nil
            labelStart = title.startIndex
        }

        if let newStatus {
            let viewing = (tabs.activeTabId == tabId)
            tabs.mutateTab(id: tabId) { tab in
                guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                    return
                }
                let isActivePane = (tab.activePaneId == paneId)
                tab.panes[pi].applyStatusTransition(
                    to: newStatus,
                    isCurrentlyBeingViewed: viewing && isActivePane
                )
            }
        }

        let rawLabel = title[labelStart...]
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !rawLabel.isEmpty else { return }
        // Ignore Claude's generic placeholder before a session is named.
        if rawLabel == "Claude Code" { return }
        tabs.applyAutoTitle(tabId: tabId, rawTitle: rawLabel)
    }

    /// A pane's shell emitted OSC 7 with a new working directory. Stash
    /// it on `Pane.cwd` so a relaunch respawns the pane in the same
    /// place. We deliberately don't touch `Tab.cwd` — that field is
    /// load-bearing for `claude --resume`'s working dir on Claude tabs,
    /// and overwriting it from a companion terminal's cwd would silently
    /// relocate the session on restore.
    func paneCwdChanged(tabId: String, paneId: String, cwd: String) {
        guard let tabs else { return }
        var changed = false
        tabs.mutateTab(id: tabId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId })
            else { return }
            if tab.panes[pi].cwd != cwd {
                tab.panes[pi].cwd = cwd
                changed = true
            }
        }
        if changed {
            onSessionMutation?()
        }
    }

    // MARK: - Launch overlay

    /// Record that a pane was just spawned and start the grace timer. If
    /// `clearPaneLaunch` is called before the timer fires (first byte
    /// arrived, or the pane exited) the overlay never appears. If the
    /// timer fires first the entry is promoted to `.visible` and
    /// `AppShellView` starts rendering the "Launching…" overlay.
    func registerPaneLaunch(paneId: String, command: String) {
        paneLaunchStates[paneId] = .pending(command: command)
        let grace = launchOverlayGraceSeconds
        let promote: @MainActor () -> Void = { [weak self] in
            guard let self,
                  case .pending(let cmd)? = self.paneLaunchStates[paneId]
            else { return }
            self.paneLaunchStates[paneId] = .visible(command: cmd)
        }
        if grace <= 0 {
            promote()
        } else {
            DispatchQueue.main.asyncAfter(deadline: .now() + grace, execute: promote)
        }
    }

    /// Remove any pending or visible overlay for this pane. Called from
    /// `NiceTerminalView.onFirstData` on first pty byte and from
    /// `paneExited` so a process that dies before emitting anything
    /// doesn't leave an orphan entry.
    func clearPaneLaunch(paneId: String) {
        paneLaunchStates[paneId] = nil
    }

    // MARK: - Selection / pane management

    /// Pick which pane is focused in `tabId`. No-op if `paneId` isn't a
    /// pane on the tab.
    func setActivePane(tabId: String, paneId: String) {
        guard let tabs else { return }
        let viewing = tabs.activeTabId == tabId
        tabs.mutateTab(id: tabId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.activePaneId = paneId
            if viewing {
                tab.panes[pi].markAcknowledgedIfWaiting()
            }
        }
        ensureActivePaneSpawned(tabId: tabId)
    }

    /// Spawn the active pane's PTY if it was deferred at tab creation.
    /// The companion terminal in Claude tabs is modelled up front but its
    /// shell isn't started until the user first switches to it (via click,
    /// keyboard shortcut, or auto-focus after the Claude pane exits).
    func ensureActivePaneSpawned(tabId: String) {
        guard let tabs,
              let tab = tabs.tab(for: tabId),
              let paneId = tab.activePaneId,
              let pane = tab.panes.first(where: { $0.id == paneId }),
              pane.kind == .terminal,
              let session = ptySessions[tabId],
              !session.hasPane(paneId)
        else { return }
        _ = session.addTerminalPane(
            id: paneId, cwd: tabs.resolvedSpawnCwd(for: tab, pane: pane)
        )
    }

    /// Move focus to the next pane within the active tab, wrapping. No-op
    /// when the active tab has fewer than two panes.
    func selectNextPane() { stepActivePane(by: +1) }

    /// Move focus to the previous pane within the active tab, wrapping.
    func selectPrevPane() { stepActivePane(by: -1) }

    private func stepActivePane(by offset: Int) {
        guard let tabs,
              let tabId = tabs.activeTabId,
              let tab = tabs.tab(for: tabId)
        else { return }
        guard tab.panes.count > 1, let activeId = tab.activePaneId,
              let currentIdx = tab.panes.firstIndex(where: { $0.id == activeId })
        else { return }
        let nextIdx = ((currentIdx + offset) % tab.panes.count + tab.panes.count) % tab.panes.count
        setActivePane(tabId: tabId, paneId: tab.panes[nextIdx].id)
    }

    /// Append a new terminal pane to `tabId`, spawn its pty, and focus
    /// it. Returns the new pane id, or nil if the tab doesn't exist.
    ///
    /// `command`, when set, runs that command instead of a plain login
    /// shell (used by the File Explorer's "Open in Editor Pane" path).
    /// On exit the pane drops via the existing `paneExited` flow.
    @discardableResult
    func addPane(
        tabId: String,
        kind: PaneKind = .terminal,
        cwd: String? = nil,
        title: String? = nil,
        command: String? = nil
    ) -> String? {
        // Only terminal kind is exposed to callers. Claude panes are
        // created exclusively by `createTabFromMainTerminal` — this
        // preserves the "at most one Claude pane per tab" invariant.
        guard kind == .terminal, let tabs else { return nil }
        guard let tab = tabs.tab(for: tabId) else { return nil }
        let newId = mintTabId("\(tabId)-p")

        // Resolve the spawn cwd before mutating the tab — once we
        // re-point `activePaneId` at the new pane below, the "spawning"
        // pane is no longer recoverable.
        let spawnCwd = tabs.spawnCwdForNewPane(in: tab, callerProvided: cwd)

        // Read the counter, compute the title, and increment all
        // inside the same mutateTab closure so the inputs and outputs
        // are one atomic unit. The counter increments unconditionally:
        // an explicit `title` consumes the slot just like an auto-named
        // one (callers pass titles for editor panes; reusing the slot
        // would leak names across tab restarts).
        tabs.mutateTab(id: tabId) { tab in
            let n = tab.nextTerminalIndex
            let resolvedTitle = title ?? "Terminal \(n)"
            tab.panes.append(
                Pane(id: newId, title: resolvedTitle, kind: .terminal)
            )
            tab.activePaneId = newId
            tab.nextTerminalIndex = n + 1
        }

        let session: TabPtySession
        if let existing = ptySessions[tabId] {
            session = existing
        } else {
            session = makeSession(for: tabId, cwd: spawnCwd)
        }
        _ = session.addTerminalPane(id: newId, cwd: spawnCwd, command: command)
        return newId
    }

    /// Append a new terminal pane to the active tab and focus it. No-op
    /// when there is no active tab.
    func addTerminalToActiveTab() {
        guard let id = tabs?.activeTabId else { return }
        _ = addPane(tabId: id, kind: .terminal)
    }

    // MARK: - Tab creation (with pty spawn)

    /// Open a new tab rooted at `cwd`, running `claude` with any `args`
    /// forwarded through. Called from the control socket's `newtab`
    /// handler when a zsh shadow's `claude` fires.
    func createTabFromMainTerminal(cwd: String, args: [String]) {
        guard let tabs else { return }
        let newId = mintTabId("t")
        let title: String = {
            guard !args.isEmpty else { return "New tab" }
            let joined = args.joined(separator: " ")
            let trimmed = String(joined.prefix(40))
                .trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? "New tab" : trimmed
        }()
        let claudePaneId = "\(newId)-claude"
        let terminalPaneId = "\(newId)-t1"
        // Pre-mint the session UUID so we can pass --session-id to
        // claude and persist the same id for later --resume.
        let sessionId = UUID().uuidString.lowercased()
        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true

        // If the user ran `claude -w <name>`, the Claude CLI creates
        // (and runs inside) a worktree at
        // `<cwd>/.claude/worktrees/<name>`. Keep `projectPath` pointing
        // at the original $PWD so sidebar bucketing still lands under
        // the parent project, and store the worktree path in `Tab.cwd`
        // so the companion terminal follows the session in.
        let projectPath = cwd
        let sessionCwd: String = {
            guard let name = TabModel.extractWorktreeName(from: args) else { return cwd }
            // Claude sanitizes `/` to `+` when deriving the on-disk
            // directory name from the `-w` value (so `foo/bar` becomes
            // `foo+bar`). Mirror that here so the companion terminal
            // lands in the same directory Claude actually created.
            let sanitized = name.replacingOccurrences(of: "/", with: "+")
            return (cwd as NSString).appendingPathComponent(".claude/worktrees/\(sanitized)")
        }()

        var tab = Tab(
            id: newId,
            title: title,
            cwd: sessionCwd,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: sessionId
        )
        tab.nextTerminalIndex = 2

        tabs.addTabToProjects(tab, cwd: projectPath)
        tabs.activeTabId = newId
        // The companion terminal pane is modelled up front so its pill
        // renders in the toolbar, but its PTY is deferred until the user
        // first focuses it — see `ensureActivePaneSpawned`.
        // Claude pane still launches from `projectPath` so `exec claude
        // -w <name>` continues to resolve/create the worktree itself.
        _ = makeSession(
            for: newId, cwd: projectPath,
            extraClaudeArgs: args,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: nil,
            claudeSessionMode: .new(id: sessionId)
        )
        onSessionMutation?()
    }

    /// Append a new terminal-only tab to the pinned Terminals group,
    /// focus it, and spawn its pty. Used by the sidebar's group-level
    /// `+` button. First tab added to an empty group is titled "Main";
    /// subsequent tabs are auto-numbered "Main 2", "Main 3", etc.
    /// Cwd inherits the Terminals project's path.
    @discardableResult
    func createTerminalTab() -> String? {
        guard let tabs,
              let pi = tabs.projects.firstIndex(where: { $0.id == TabModel.terminalsProjectId })
        else { return nil }
        let project = tabs.projects[pi]
        let title: String
        if project.tabs.isEmpty {
            title = "Main"
        } else {
            title = "Main \(project.tabs.count + 1)"
        }
        let newId = mintTabId("tt")
        let paneId = "\(newId)-p0"
        let cwd = project.path
        var tab = Tab(
            id: newId,
            title: title,
            cwd: cwd,
            branch: nil,
            panes: [Pane(id: paneId, title: "Terminal 1", kind: .terminal)],
            activePaneId: paneId
        )
        tab.nextTerminalIndex = 2
        tabs.projects[pi].tabs.append(tab)
        tabs.activeTabId = newId
        _ = makeSession(for: newId, cwd: cwd)
        onSessionMutation?()
        return newId
    }

    /// Create a fresh Claude tab in an existing project group. Mirrors
    /// `createTabFromMainTerminal` but targets `projectId` directly so
    /// the sidebar's per-project `+` button can add into that project
    /// instead of bucketing by cwd. No-op for the pinned Terminals
    /// group (which only holds terminal tabs).
    @discardableResult
    func createClaudeTabInProject(projectId: String) -> String? {
        guard let tabs,
              projectId != TabModel.terminalsProjectId,
              let pi = tabs.projects.firstIndex(where: { $0.id == projectId })
        else { return nil }
        let project = tabs.projects[pi]
        let newId = mintTabId("t")
        let claudePaneId = "\(newId)-claude"
        let terminalPaneId = "\(newId)-t1"
        let sessionId = UUID().uuidString.lowercased()
        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true
        var tab = Tab(
            id: newId,
            title: "New tab",
            cwd: project.path,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: sessionId
        )
        tab.nextTerminalIndex = 2
        tabs.projects[pi].tabs.append(tab)
        tabs.activeTabId = newId
        _ = makeSession(
            for: newId, cwd: project.path,
            extraClaudeArgs: [],
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: nil,
            claudeSessionMode: .new(id: sessionId)
        )
        onSessionMutation?()
        return newId
    }

    // MARK: - Control socket handlers

    /// Handle a `claude` invocation from a pane's zsh wrapper. The
    /// wrapper is blocked reading a single-line reply from the socket;
    /// we must call `reply` exactly once. Three outcomes:
    ///
    /// - "newtab": no promotion candidate (the sending tab lives in
    ///   the pinned Terminals group, unknown tabId, or the target
    ///   sidebar tab already has a live Claude). Open a fresh sidebar
    ///   tab via `createTabFromMainTerminal`.
    /// - "inplace": promote the sending pane — flip its kind to
    ///   `.claude` and mark it running. The wrapper `exec`s claude
    ///   with the user's args as-is (they already contain `--resume`
    ///   or `--session-id`).
    /// - "inplace <uuid>": same promotion, but mint a new session id
    ///   so we can later resume it. The wrapper prepends
    ///   `--session-id <uuid>`.
    /// - A trailing pointer path ("inplace <uuid|-> <settingsPath>") is
    ///   appended when theme sync is on; the wrapper splices it as
    ///   `--settings <path>` so the in-place Claude matches the Nice
    ///   theme. A "-" sid placeholder lets the path follow when the
    ///   user's own args already carry the session. Sync off omits the
    ///   field, leaving the two replies above byte-identical.
    ///
    /// `internal` so unit tests can drive the dispatch path directly
    /// without standing up a real socket — matches `paneExited` and
    /// `handleClaudeSessionUpdate`'s access level for the same reason.
    /// The promotion path here is also the only writer that flips a
    /// pane's `isClaudeRunning` from `false` to `true`, which is the
    /// signal `paneTitleChanged`'s OSC-title gate releases on; testing
    /// this dispatch in isolation pins that load-bearing transition.
    func handleClaudeSocketRequest(
        cwd: String,
        args: [String],
        tabId: String,
        paneId: String,
        reply: @Sendable (String) -> Void
    ) {
        guard let tabs else {
            reply("newtab")
            return
        }

        // No/unknown tabId, or the request came from a tab in the
        // pinned Terminals group: always open a new sidebar tab.
        guard !tabId.isEmpty,
              !tabs.isTerminalsProjectTab(tabId),
              let existingTab = tabs.tab(for: tabId),
              existingTab.panes.contains(where: { $0.id == paneId })
        else {
            reply("newtab")
            self.createTabFromMainTerminal(cwd: cwd, args: args)
            return
        }

        // Sidebar tab already has a running Claude: spawn-in-place
        // would create a second Claude pane in this tab, violating
        // the "at most one Claude pane per tab" invariant. Open a
        // new tab instead.
        if existingTab.panes.contains(where: { $0.isClaudeRunning }) {
            reply("newtab")
            self.createTabFromMainTerminal(cwd: cwd, args: args)
            return
        }

        // Promotion path. Extract --resume/--session-id from args if
        // present (e.g. the pre-typed `claude --resume <uuid>` on a
        // restored tab); otherwise mint a fresh session id so we can
        // persist it for next relaunch.
        let parsedId = TabModel.extractClaudeSessionId(from: args)
        let sessionId = parsedId ?? UUID().uuidString.lowercased()

        tabs.mutateTab(id: tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.panes[idx].kind = .claude
            tab.panes[idx].isClaudeRunning = true
            // Let the upcoming OSC title from claude set the real label;
            // seed with "Claude" so the pill doesn't render stale text.
            // Skip the seed when the user has manually renamed this pane
            // — promoting a user-named pane to Claude shouldn't blow
            // their custom label away (and the OSC gate in
            // `paneTitleChanged` would block the next OSC anyway).
            if !tab.panes[idx].titleManuallySet {
                tab.panes[idx].title = "Claude"
            }
            tab.activePaneId = paneId
            tab.claudeSessionId = sessionId
        }
        onSessionMutation?()

        // Hand the wrapper the theme pointer when sync is on, so an
        // in-place promotion matches the Nice theme exactly like a
        // from-scratch Nice Claude pane. The wrapper splices it as the
        // `--settings <path>` 3rd reply field; sync off omits it so the
        // reply stays byte-identical to the pre-theming protocol.
        // Skip it when the args already carry `--settings` (e.g. a
        // restored deferred pane re-dispatching its pre-typed `claude
        // --settings … --resume …` through this socket) so we never emit
        // a doubled flag. Mirrors `makeSession`'s gate — see `ClaudeThemeSync`.
        let settingsPath = themeCache.syncClaudeTheme && !args.contains("--settings")
            ? ClaudeThemeSync.settingsFlagPath()
            : nil
        if let settingsPath {
            // "-" sid placeholder when the user's args already carry the
            // session, so the pointer can follow as the 3rd field; else
            // the freshly minted id.
            let sidField = parsedId != nil ? "-" : sessionId
            reply("inplace \(sidField) \(settingsPath)")
        } else if parsedId != nil {
            reply("inplace")
        } else {
            reply("inplace \(sessionId)")
        }
    }

    /// Handle a `session_update` socket message from Claude Code's
    /// SessionStart hook. Looks up the tab whose pane set contains
    /// `paneId`, captures the pre-rotation session id (so
    /// `materializeBranchParent` can resume from it), then updates the
    /// tab's stored id and — when this rotation is a `/branch` (or
    /// `--fork-session`) — spawns a sibling parent tab pinned to the
    /// old id. Silent no-op if the pane is stale (exited while the
    /// hook's `nc` was in flight) or isn't owned by any tab.
    /// `internal` so unit tests can drive the dispatch path directly
    /// without standing up a real socket — matches `paneExited`'s
    /// access level for the same reason.
    ///
    /// Branch detection: `source == "resume"` plus an actual id-change
    /// is the signature of `/branch` and `--fork-session`. Real
    /// `/resume` keeps the id stable (absorbed by
    /// `updateClaudeSessionId`'s short-circuit), `/clear` reports
    /// `source == "clear"`, and `/compact` typically doesn't rotate at
    /// all in current Claude Code. A nil/unknown source (older hook
    /// payload still in flight during upgrade, or a future Claude
    /// version that drops the field) is treated as a plain id update
    /// — we'd rather miss a /branch occasionally than spawn a phantom
    /// parent tab from a /clear we mis-classified.
    ///
    /// `cwd` carries the absolute path Claude is currently running in
    /// (from the SessionStart payload). Used to keep `tab.cwd` in sync
    /// when Claude moves into a worktree mid-session — bare `claude
    /// -w` (auto-named worktree the parser can't predict), `/worktree`
    /// slash command, or anything else that swaps Claude's working
    /// directory without restarting the process. Ordering matters: the
    /// branch-parent materialization step runs **before** the cwd
    /// update so the newly-spawned sibling parent inherits the
    /// pre-rotation cwd (its old-session-id transcript lives in the
    /// pre-rotation bucket).
    func handleClaudeSessionUpdate(
        paneId: String,
        sessionId: String,
        source: String?,
        cwd: String?
    ) {
        guard let tabs, let tabId = tabs.tabIdOwning(paneId: paneId) else { return }
        let oldId = tabs.tab(for: tabId)?.claudeSessionId
        updateClaudeSessionId(tabId: tabId, sessionId: sessionId)
        if source == "resume", let oldId, oldId != sessionId {
            materializeBranchParent(forTabId: tabId, oldSessionId: oldId)
        }
        // Apply cwd to the *originating* tab only — runs after branch
        // materialization so the sibling parent keeps the pre-rotation
        // cwd by virtue of having forked from `originating.cwd` while
        // it still held the old value.
        updateTabCwd(tabId: tabId, newCwd: cwd)
    }

    /// Update `tab.claudeSessionId` when claude rotates its session
    /// mid-process — `/clear`, `/compact`, and `/branch` all swap the
    /// UUID without restarting the process, so the pre-minted id we
    /// stored at tab creation goes stale. Persist the new id immediately
    /// so an unexpected Nice shutdown still resumes the correct
    /// conversation. No-op if the tab already has this id or no longer
    /// exists.
    private func updateClaudeSessionId(tabId: String, sessionId: String) {
        guard let tabs else { return }
        var changed = false
        tabs.mutateTab(id: tabId) { tab in
            if tab.claudeSessionId != sessionId {
                tab.claudeSessionId = sessionId
                changed = true
            }
        }
        if changed {
            onSessionMutation?()
        }
    }

    /// Update `tab.cwd` to reflect Claude's actual working directory,
    /// reported via the SessionStart hook. The hook fires whenever
    /// Claude swaps directories mid-process (bare `claude -w` lands
    /// in an auto-named worktree the arg parser can't predict;
    /// `/worktree` slash command does the same after the fact). The
    /// recorded cwd is what `claude --resume` keys off on the next
    /// restart, so keeping `tab.cwd` aligned with it is what makes
    /// resume work across quits.
    ///
    /// Pane policy and the actual mutation live on
    /// `TabModel.adoptTabCwd` so the rotation handler and the
    /// restore-time heal pass share one definition of "follow the
    /// tab." This shim filters out the no-op shapes (nil / empty)
    /// before calling through and fires the persistence hook on real
    /// change.
    private func updateTabCwd(tabId: String, newCwd: String?) {
        guard let tabs,
              let newCwd,
              !newCwd.isEmpty
        else { return }
        if tabs.adoptTabCwd(forTabId: tabId, newCwd: newCwd) {
            onSessionMutation?()
        }
    }

    /// Materialize the pre-/branch session as a sibling sidebar tab
    /// pinned to `oldSessionId`, inserted immediately above the
    /// originating tab in the same project. Called from
    /// `handleClaudeSessionUpdate` once the rotation has been
    /// classified as a `/branch` (or `--fork-session`).
    ///
    /// The new tab's Claude pane is wired up with
    /// `ClaudeSessionMode.resumeDeferred(id:)` — same pattern as the
    /// restored tabs in `WindowSession.restoreSavedWindow` — so a
    /// plain shell starts in the companion terminal with `claude
    /// --resume <oldId>` pre-typed via `print -z`. Nothing actually
    /// resumes (and no tokens are spent) until the user opens the
    /// new tab and presses Enter.
    ///
    /// The tree-mutation half (insert at slot, depth-1 lineage rule,
    /// same-project precondition) lives on `TabModel.insertBranchParent`
    /// so the model invariant is owned by the model. This method
    /// does the per-window glue: mint ids, hand the model the tree
    /// mutation, then spawn the deferred-resume pty against the
    /// returned parent tab and notify the save subsystem.
    private func materializeBranchParent(
        forTabId originatingTabId: String,
        oldSessionId: String
    ) {
        guard let tabs else { return }
        let newId = mintTabId("t")
        let claudePaneId = "\(newId)-claude"
        let terminalPaneId = "\(newId)-t1"
        guard let parent = tabs.insertBranchParent(
            forTabId: originatingTabId,
            newTabId: newId,
            claudePaneId: claudePaneId,
            terminalPaneId: terminalPaneId,
            oldSessionId: oldSessionId
        ) else { return }

        // Read `parent.cwd` here, before the caller's `updateTabCwd`
        // moves the originating tab into the post-rotation worktree.
        // `insertBranchParent` copies `originating.cwd` at the moment
        // of insertion, so `parent.cwd` is the pre-rotation cwd —
        // which is what the sibling's `claude --resume <oldId>` needs
        // because the old-id transcript was bucketed under the
        // pre-rotation path. A future refactor that delays this read
        // (e.g. fetches `parent` again from `tabs` later) would
        // silently break the branch-cwd ordering contract.
        _ = makeSession(
            for: newId,
            cwd: parent.cwd,
            extraClaudeArgs: [],
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: nil,
            claudeSessionMode: .resumeDeferred(id: oldSessionId)
        )
        onSessionMutation?()
    }

    // MARK: - Handoff

    /// Handle a `handoff` socket message from the `/nice-handoff` skill.
    /// Opens a fresh Claude tab nested under the originating tab in the
    /// sidebar, titled "[HANDOFF] <originating title>", running a brand-
    /// new Claude session seeded with a prompt that points Claude at the
    /// notes file the skill wrote.
    ///
    /// `internal` (not `private`) so unit tests can drive the dispatch
    /// path directly without standing up a real socket — matches the
    /// access level of `handleClaudeSocketRequest` and `paneExited` for
    /// the same reason.
    ///
    /// Originating-tab resolution mirrors the "claude" socket request:
    /// the request must name a non-empty `tabId` that isn't in the
    /// pinned Terminals group, resolves to a real tab, and owns the
    /// sending `paneId`. Unlike the promote-in-place path, a failed
    /// resolution is NOT an error — a handoff fired from the Main
    /// Terminal (or any unowned/stale pane) still opens a *top-level*
    /// handoff tab. Resolution only controls (a) which tab the new tab
    /// nests under and (b) where it spawns; it never blocks the handoff.
    ///
    /// Spawn cwd prefers the originating tab's own `cwd` so the child
    /// lands wherever its parent is (possibly a worktree Claude moved
    /// into mid-session), falling back to the payload's `cwd` when there
    /// is no originating tab.
    ///
    /// Title: the originating tab's title with any leading "[HANDOFF] "
    /// stripped first (so a handoff *from* a handoff tab doesn't stack
    /// the prefix), defaulting to "Session" when there's no usable
    /// title, then re-prefixed with "[HANDOFF] ".
    ///
    /// Prompt: always instructs Claude to read the notes file, then
    /// appends either the skill's custom `instructions` (when non-blank)
    /// or a default "continue the work described there" directive.
    func handleHandoffRequest(
        cwd: String,
        handoffFile: String,
        instructions: String,
        tabId: String,
        paneId: String,
        reply: @Sendable (String) -> Void
    ) {
        guard let tabs else {
            reply("error: no window")
            return
        }

        // Resolve the originating tab the same way the "claude" request
        // does — non-empty id, not in the Terminals group, present in
        // the model, and actually owning the sending pane. A miss here
        // is a top-level fallback, not an error: a handoff from the
        // Main Terminal should still open a tab.
        let originating: Tab? = {
            guard !tabId.isEmpty,
                  !tabs.isTerminalsProjectTab(tabId),
                  let tab = tabs.tab(for: tabId),
                  tab.panes.contains(where: { $0.id == paneId })
            else { return nil }
            return tab
        }()

        let spawnCwd = originating?.cwd ?? cwd

        // Nest under the *resolved* originating tab, not the raw payload
        // `tabId`. When resolution failed (empty/Terminals/unknown id, or
        // a stale `paneId` the tab no longer owns) we pass "" so
        // `insertHandoffChild` rejects it and the tab opens top-level —
        // keeping nesting coherent with the title/cwd, which already key
        // off `originating`. (In production `originating?.id == tabId`
        // whenever resolution succeeds, since the helper sends the pane's
        // own NICE_TAB_ID/NICE_PANE_ID.)
        createHandoffTab(
            underTabId: originating?.id ?? "",
            cwd: spawnCwd,
            title: Self.handoffTitle(forOriginatingTitle: originating?.title),
            prompt: Self.handoffPrompt(handoffFile: handoffFile, instructions: instructions)
        )
        reply("ok")
    }

    /// Prefix used on handoff-tab titles. Exposed so tests and the
    /// title builder share one literal. `nonisolated` so the pure
    /// `handoffTitle` helper (also nonisolated) can reference it.
    nonisolated static let handoffTitlePrefix = "[HANDOFF] "

    /// Build the locked "[HANDOFF] …" title for a handoff tab from the
    /// originating tab's current title. Pure (no actor state) so it can
    /// be unit-tested directly — mirrors `TabPtySession.buildClaudeExecCommand`.
    ///
    /// Strips a single existing "[HANDOFF] " prefix first so a handoff
    /// fired *from* a handoff tab reads "[HANDOFF] Foo" rather than
    /// stacking into "[HANDOFF] [HANDOFF] Foo". Falls back to "Session"
    /// when the originating title is nil or blank — including
    /// whitespace-only titles, which would otherwise yield a ragged
    /// "[HANDOFF]    ".
    nonisolated static func handoffTitle(forOriginatingTitle originatingTitle: String?) -> String {
        let raw = originatingTitle ?? ""
        let stripped = raw.hasPrefix(handoffTitlePrefix)
            ? String(raw.dropFirst(handoffTitlePrefix.count))
            : raw
        let trimmed = stripped.trimmingCharacters(in: .whitespacesAndNewlines)
        let base = trimmed.isEmpty ? "Session" : trimmed
        return handoffTitlePrefix + base
    }

    /// Build the initial prompt seeded into a handoff session. Pure (no
    /// actor state) so it can be unit-tested directly.
    ///
    /// Always points Claude at the notes file; the continuation is the
    /// skill's custom `instructions` when non-blank (direct `/nice-handoff
    /// <args>` invocations), otherwise a default "wait for the user"
    /// directive (model-triggered / no-arg invocations). The default
    /// deliberately does NOT auto-resume the work — a no-arg handoff lands
    /// the fresh session in a read-and-await state so the user stays in
    /// control of when (and how) it picks up. Passing custom instructions
    /// (e.g. `/nice-handoff keep going`) overrides this to continue.
    nonisolated static func handoffPrompt(handoffFile: String, instructions: String) -> String {
        let trimmed = instructions.trimmingCharacters(in: .whitespacesAndNewlines)
        let directive = trimmed.isEmpty
            ? "Do not start working yet — once you have read it, wait for the user to tell you how to proceed."
            : trimmed
        return "Read the handoff notes at \(handoffFile). " + directive
    }

    /// Mint and spawn the handoff tab. Modelled closely on
    /// `createTabFromMainTerminal`: a Claude pane (marked running) plus
    /// a deferred-spawn companion terminal, a pre-minted session UUID
    /// passed as `--session-id` so the transcript lands under a known
    /// id, and `activeTabId` flipped to the new tab.
    ///
    /// Differences from `createTabFromMainTerminal`:
    ///   • The title is fixed up front and `titleManuallySet` is flipped
    ///     so Claude's OSC-driven auto-title can't overwrite the
    ///     "[HANDOFF] …" label.
    ///   • Insertion goes through `tabs.insertHandoffChild` so the new
    ///     tab nests one indent deep under the originating tab (depth-1
    ///     lineage, same invariant `/branch` uses). When there's no
    ///     valid parent (top-level handoff from the Main Terminal, or a
    ///     stale originating id) it falls back to `addTabToProjects` so
    ///     the tab still opens at top level — same bucketing
    ///     `createTabFromMainTerminal` uses.
    ///   • The seeded `prompt` is passed as a single positional Claude
    ///     arg, which becomes `claude --session-id <id> "<prompt>"` and
    ///     auto-runs the prompt on launch.
    private func createHandoffTab(
        underTabId: String,
        cwd: String,
        title: String,
        prompt: String
    ) {
        guard let tabs else { return }
        let newId = mintTabId("t")
        let claudePaneId = "\(newId)-claude"
        let terminalPaneId = "\(newId)-t1"
        // Pre-mint the session UUID so we can pass --session-id to
        // claude and persist the same id for later --resume.
        let sessionId = UUID().uuidString.lowercased()
        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true

        var tab = Tab(
            id: newId,
            title: title,
            cwd: cwd,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: sessionId
        )
        // Lock the title: the handoff label is meaningful and Claude's
        // OSC auto-title would otherwise replace it once the new session
        // names itself.
        tab.titleManuallySet = true
        tab.nextTerminalIndex = 2

        // Nest under the originating tab when possible; otherwise bucket
        // by cwd at top level so a Main-Terminal handoff still opens.
        if !tabs.insertHandoffChild(tab, underTabId: underTabId) {
            tabs.addTabToProjects(tab, cwd: cwd)
        }
        tabs.activeTabId = newId

        // Passing the prompt as a single positional arg makes the
        // launch line `claude --session-id <id> "<prompt>"`, which
        // auto-runs the prompt. The companion terminal's pty is deferred
        // until first focus, same as createTabFromMainTerminal.
        _ = makeSession(
            for: newId,
            cwd: cwd,
            extraClaudeArgs: [prompt],
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: nil,
            claudeSessionMode: .new(id: sessionId)
        )
        onSessionMutation?()
    }

    // MARK: - Pty session creation

    /// Return the pty session for `tabId`, creating and caching one if
    /// it doesn't exist yet. Spawns initial panes based on the tab's
    /// model state.
    @discardableResult
    func makeSession(
        for tabId: String,
        cwd: String,
        extraClaudeArgs: [String] = [],
        initialClaudePaneId: String? = nil,
        initialTerminalPaneId: String? = nil,
        claudeSessionMode: TabPtySession.ClaudeSessionMode = .none
    ) -> TabPtySession {
        if let existing = ptySessions[tabId] {
            return existing
        }
        let resolvedCwd = TabModel.expandTilde(cwd)

        // Work out which panes to spawn. Callers can pass ids explicitly
        // (e.g. createTabFromMainTerminal) or we infer them from the
        // model.
        var claudePaneId = initialClaudePaneId
        var terminalPaneId = initialTerminalPaneId
        if claudePaneId == nil && terminalPaneId == nil {
            if let tab = tabs?.tab(for: tabId) {
                for pane in tab.panes {
                    switch pane.kind {
                    case .claude where claudePaneId == nil:
                        claudePaneId = pane.id
                    case .terminal where terminalPaneId == nil:
                        terminalPaneId = pane.id
                    default:
                        break
                    }
                }
            }
        }

        // Hand Claude the Nice-managed theme pointer only when sync is on.
        // `settingsFlagPath()` ensures the file exists and returns its path;
        // nil (sync off, or write failed) omits `--settings` so Claude uses
        // the user's own theme. Computed here — not in `instantiateSession`
        // — so the migration/adopt callers (which re-home an existing pane
        // rather than spawn a fresh Claude) neither trigger the write nor
        // bake `--settings` into a migrated pane's prefill. Those panes
        // still get themed on resume via the socket reply. See `ClaudeThemeSync`.
        let claudeSettingsPath = themeCache.syncClaudeTheme
            ? ClaudeThemeSync.settingsFlagPath()
            : nil
        return instantiateSession(
            for: tabId,
            cwd: resolvedCwd,
            extraClaudeArgs: extraClaudeArgs,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: terminalPaneId,
            claudeSessionMode: claudeSessionMode,
            claudeSettingsPath: claudeSettingsPath
        )
    }

    /// Construct, theme, cache, and return a `TabPtySession` spawning
    /// exactly the panes named by `initialClaudePaneId` /
    /// `initialTerminalPaneId` — no model inference. Shared by
    /// `makeSession` (which infers the ids first) and the live-migration
    /// adopt paths (which spawn no Claude pane because the migrated one
    /// is already running). `cwd` is expected pre-expanded.
    private func instantiateSession(
        for tabId: String,
        cwd: String,
        extraClaudeArgs: [String],
        initialClaudePaneId: String?,
        initialTerminalPaneId: String?,
        claudeSessionMode: TabPtySession.ClaudeSessionMode,
        claudeSettingsPath: String? = nil
    ) -> TabPtySession {
        let resolvedCwd = cwd

        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath,
            extraClaudeArgs: extraClaudeArgs,
            initialClaudePaneId: initialClaudePaneId,
            initialTerminalPaneId: initialTerminalPaneId,
            socketPath: controlSocket?.path,
            zdotdirPath: zdotdirPath,
            userZDotDir: userZDotDir,
            claudeSessionMode: claudeSessionMode,
            claudeSettingsPath: claudeSettingsPath,
            onPaneExit: { [weak self] paneId, code in
                self?.paneExited(tabId: tabId, paneId: paneId, exitCode: code)
            },
            onPaneTitleChange: { [weak self] paneId, title in
                self?.paneTitleChanged(tabId: tabId, paneId: paneId, title: title)
            },
            onPaneCwdChange: { [weak self] paneId, cwd in
                self?.paneCwdChanged(tabId: tabId, paneId: paneId, cwd: cwd)
            },
            onPaneLaunched: { [weak self] paneId, command in
                self?.registerPaneLaunch(paneId: paneId, command: command)
            },
            onPaneFirstOutput: { [weak self] paneId in
                self?.clearPaneLaunch(paneId: paneId)
            },
            onPaneHeld: { [weak self] paneId, code in
                self?.paneHeld(tabId: tabId, paneId: paneId, exitCode: code)
            }
        )
        themeCache.applyAll(to: session)
        ptySessions[tabId] = session
        return session
    }

    // MARK: - Live pane migration (move a pane between windows)

    /// Detach the live entry for `paneId` from `tabId`'s pty session
    /// WITHOUT killing its pty, returning it so another window's
    /// `SessionsModel` can `adoptLivePane` it. The running process and
    /// scrollback survive (owned by the entry's view). Clears any
    /// pending launch-overlay state for the pane so a migrated-but-
    /// still-launching pane doesn't leave an orphan "Launching…" entry
    /// behind in the source window. Returns nil when the tab has no
    /// session or the pane isn't hosted.
    ///
    /// Model-only removal of the `Pane` from the source tab is the
    /// caller's job (`TabModel.extractPane`); this touches the pty
    /// layer only.
    func detachLivePane(tabId: String, paneId: String) -> TabPtySession.PaneEntry? {
        clearPaneLaunch(paneId: paneId)
        return ptySessions[tabId]?.detachPane(id: paneId)
    }

    /// Resolve `paneId` on `tabId` to a `PaneClaim` for a cross-window
    /// transfer (tear-off or migration), one-shot. This is the model-
    /// layer half of the tri-state that replaces the old silent-nil
    /// claim (BUG A):
    ///
    ///   - The pane has a live pty (`hasPane`) → detach it and return
    ///     `.live(entry)`. The detach is a SAFE `if let`, never a force-
    ///     unwrap: a force-unwrap at the exact site whose silent-nil
    ///     history motivated the tri-state would be the inverse failure
    ///     mode. If `detachLivePane` unexpectedly returns nil, fall
    ///     through to the model check rather than trapping.
    ///   - Otherwise the pane is MODELLED in the tab but its spawn was
    ///     deferred → return `.notSpawned(cwd:)` with the cwd resolved
    ///     from the SOURCE model (so the destination spawns it in the
    ///     right directory).
    ///   - Neither holds → `.gone`.
    func claimPaneForTransfer(tabId: String, paneId: String) -> PaneClaim {
        if ptySessions[tabId]?.hasPane(paneId) == true {
            if let entry = detachLivePane(tabId: tabId, paneId: paneId) {
                return .live(entry)
            }
            // Unexpected: `hasPane` said yes but detach returned nil.
            // Don't trap — fall through to the model resolution below.
        }
        if let tabs,
           let tab = tabs.tab(for: tabId),
           let pane = tab.panes.first(where: { $0.id == paneId }) {
            return .notSpawned(cwd: tabs.resolvedSpawnCwd(for: tab, pane: pane))
        }
        return .gone
    }

    /// Spawn `paneId` on `tabId` in the DESTINATION window when a torn-
    /// off / migrated pane arrived `.notSpawned` (no live entry to
    /// adopt). Unlike `ensureActivePaneSpawned` — which hard-guards on an
    /// already-existing session and only spawns the ACTIVE pane — this is
    /// SESSION-CREATING: it stands up an empty session shell for the tab
    /// when one doesn't exist yet (mirroring `adoptLivePane`'s create-if-
    /// missing branch), then spawns `paneId` as a fresh terminal in
    /// `cwd`. That makes a drop into a session-less target tab spawn the
    /// pane instead of silently no-op'ing.
    ///
    /// `cwd` is the resolved spawn cwd carried in the claim; the pane
    /// spawns there with this destination window's own socket / ZDOTDIR /
    /// tab env. Claude panes are spawned by their adopt path
    /// (`.resumeDeferred`), not here; this handles `.terminal` only.
    func ensurePaneSpawned(tabId: String, paneId: String, cwd: String) {
        let session: TabPtySession
        if let existing = ptySessions[tabId] {
            session = existing
        } else {
            // Mirror adoptLivePane's create-if-missing branch: an empty
            // session shell in the resolved cwd (claim cwd preferred,
            // else the tab's own cwd) that the spawn below populates.
            let sessionCwd = TabModel.expandTilde(cwd)
            session = instantiateSession(
                for: tabId, cwd: sessionCwd, extraClaudeArgs: [],
                initialClaudePaneId: nil, initialTerminalPaneId: nil,
                claudeSessionMode: .none
            )
        }
        // Only terminal panes spawn here, and only when not already live.
        guard let tabs,
              let tab = tabs.tab(for: tabId),
              let pane = tab.panes.first(where: { $0.id == paneId }),
              pane.kind == .terminal,
              !session.hasPane(paneId)
        else { return }
        _ = session.addTerminalPane(id: paneId, cwd: cwd)
    }

    /// Adopt a pane previously detached from another window into
    /// `tabId`'s session (creating an empty session for the tab when one
    /// doesn't exist yet, e.g. a target tab whose pty was never spawned).
    ///
    /// `entry` is OPTIONAL so this also serves the `.notSpawned` claim
    /// path (BUG A): when non-nil, the live entry's delegate is re-pointed
    /// at this window via `TabPtySession.adoptPane`; when nil, the pane
    /// was modelled-but-deferred in the source and is spawned FRESH in
    /// the destination via `ensurePaneSpawned` (resolving the cwd from
    /// the tab so it opens in the right directory). Inserting the `Pane`
    /// model into the target tab is the caller's job
    /// (`TabModel.insertPane`).
    func adoptLivePane(
        tabId: String, paneId: String, entry: TabPtySession.PaneEntry?
    ) {
        guard let entry else {
            // No live entry: spawn the deferred pane fresh in the
            // destination. `ensurePaneSpawned` creates the session shell
            // if absent (it mirrors this method's create-if-missing
            // branch) and spawns in the tab's resolved cwd.
            let cwd: String
            if let tabs, let tab = tabs.tab(for: tabId) {
                cwd = tabs.resolvedSpawnCwd(for: tab)
            } else {
                cwd = NSHomeDirectory()
            }
            ensurePaneSpawned(tabId: tabId, paneId: paneId, cwd: cwd)
            return
        }
        let session: TabPtySession
        if let existing = ptySessions[tabId] {
            session = existing
        } else {
            let cwd = tabs?.tab(for: tabId).map { TabModel.expandTilde($0.cwd) }
                ?? NSHomeDirectory()
            session = instantiateSession(
                for: tabId, cwd: cwd, extraClaudeArgs: [],
                initialClaudePaneId: nil, initialTerminalPaneId: nil,
                claudeSessionMode: .none
            )
        }
        session.adoptPane(id: paneId, entry: entry)
    }

    /// Adopt a Claude pane (migrated from another window) as a brand-new
    /// sidebar tab. A Claude pane can't join an existing tab's pane set —
    /// a tab holds at most one alive Claude pane — so a cross-window move
    /// / tear-off of a Claude pane lands as its own tab under the project
    /// matching `projectPath` (recreated from the supplied identity when
    /// the destination window lacks it). The new tab takes the canonical
    /// `[Claude, companion terminal]` shape with the Claude pane focused
    /// and `claudeSessionId` carried across; the companion terminal is a
    /// fresh deferred spawn (started on first focus, exactly like
    /// `createTabFromMainTerminal`).
    ///
    /// `entry` is OPTIONAL (BUG A):
    ///   - non-nil → the migrated pane was a LIVE Claude; instantiate a
    ///     `.none`-mode session and adopt the entry without re-spawning.
    ///   - nil → the Claude pane was modelled-but-deferred in the source
    ///     (a restored tab never focused). Instantiate with the Claude
    ///     pane id as `initialClaudePaneId` AND `.resumeDeferred(id:)`
    ///     mode and DO NOT adopt — the deferred claude spawns on first
    ///     focus, mirroring restore (`WindowSession.restoreSavedWindow`)
    ///     and `createTabFromMainTerminal`. A nil `claudeSessionId` on
    ///     this branch should be near-impossible (an unspawned Claude
    ///     pane always carries the session it would resume); we log a
    ///     loud error and still create the tab as a best-effort fresh
    ///     `.resumeDeferred` with an empty id rather than dropping it.
    ///
    /// Returns the new tab id, or nil when `tabs` is gone.
    @discardableResult
    func adoptClaudePaneAsNewTab(
        entry: TabPtySession.PaneEntry?,
        paneId: String,
        title: String,
        claudeSessionId: String?,
        projectId: String,
        projectName: String,
        projectPath: String
    ) -> String? {
        guard let tabs else { return nil }
        let newTabId = mintTabId("t")
        let companionId = "\(newTabId)-t1"
        var claudePane = Pane(id: paneId, title: title, kind: .claude)
        // The migrated pane was a live Claude; keep the runtime flag so
        // `paneTitleChanged`'s OSC gate stays open and status flows. For
        // the deferred (nil-entry) path the flag is harmless — the OSC
        // gate simply stays armed until the resume actually starts.
        claudePane.isClaudeRunning = true
        var tab = Tab(
            id: newTabId,
            title: title,
            cwd: projectPath,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: companionId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: paneId,
            claudeSessionId: claudeSessionId
        )
        tab.nextTerminalIndex = 2
        let pi = tabs.ensureProjectByPath(
            id: projectId, name: projectName, path: projectPath
        )
        tabs.projects[pi].tabs.append(tab)
        tabs.activeTabId = newTabId

        if let entry {
            // Live Claude: spawn nothing (companion stays deferred), then
            // adopt the live entry rather than re-spawning it.
            let session = instantiateSession(
                for: newTabId, cwd: TabModel.expandTilde(projectPath),
                extraClaudeArgs: [],
                initialClaudePaneId: nil, initialTerminalPaneId: nil,
                claudeSessionMode: .none
            )
            session.adoptPane(id: paneId, entry: entry)
        } else {
            // Deferred Claude: instantiate in resume-deferred mode with
            // the Claude pane as the initial pane and DO NOT adopt — the
            // resume spawns on first focus, exactly like restore.
            if claudeSessionId == nil {
                Self.tearOffLog.error(
                    "adoptClaudePaneAsNewTab: nil entry AND nil claudeSessionId for pane \(paneId, privacy: .public) — creating a best-effort resumeDeferred tab with an empty session id"
                )
            }
            _ = instantiateSession(
                for: newTabId, cwd: TabModel.expandTilde(projectPath),
                extraClaudeArgs: [],
                initialClaudePaneId: paneId, initialTerminalPaneId: nil,
                claudeSessionMode: .resumeDeferred(id: claudeSessionId ?? "")
            )
        }
        onSessionMutation?()
        return newTabId
    }

    /// Adopt a live terminal pane (migrated from another window) as a
    /// brand-new sidebar tab. This is the terminal-only sibling to
    /// `adoptClaudePaneAsNewTab` — used exclusively by the tear-off
    /// path when a lone terminal pane is dragged off a window and must
    /// land in a fresh tab rather than an existing strip.
    ///
    /// A new tab is created under the project matching `projectPath`
    /// (recreated from the supplied identity when the destination window
    /// lacks it, via `tabs.ensureProjectByPath`), with a single pane of
    /// kind `.terminal` whose id is `paneId` and whose title is `title`.
    /// `activePaneId` is set to `paneId`. Sets `tabs.activeTabId` to the
    /// new tab. Fires `onSessionMutation`. Returns the new tab id, or nil
    /// when `tabs` is gone.
    ///
    /// `entry` is OPTIONAL (BUG A): non-nil → an empty session shell is
    /// instantiated and the live entry adopted into it via
    /// `session.adoptPane`; nil → the pane was modelled-but-deferred in
    /// the source and is spawned FRESH via `ensurePaneSpawned` in
    /// `spawnCwd` (the cwd carried in the claim) — falling back to
    /// `projectPath` when no spawn cwd was supplied.
    @discardableResult
    func adoptTerminalPaneAsNewTab(
        entry: TabPtySession.PaneEntry?,
        paneId: String,
        title: String,
        projectId: String,
        projectName: String,
        projectPath: String,
        spawnCwd: String? = nil
    ) -> String? {
        guard let tabs else { return nil }
        let newTabId = mintTabId("t")
        let terminalPane = Pane(id: paneId, title: title, kind: .terminal)
        var tab = Tab(
            id: newTabId,
            title: title,
            cwd: projectPath,
            branch: nil,
            panes: [terminalPane],
            activePaneId: paneId,
            claudeSessionId: nil
        )
        tab.nextTerminalIndex = 2
        let pi = tabs.ensureProjectByPath(
            id: projectId, name: projectName, path: projectPath
        )
        tabs.projects[pi].tabs.append(tab)
        tabs.activeTabId = newTabId

        if let entry {
            // Live entry: instantiate an empty session shell and adopt
            // into it directly (no fresh spawn).
            let session = instantiateSession(
                for: newTabId, cwd: TabModel.expandTilde(projectPath),
                extraClaudeArgs: [],
                initialClaudePaneId: nil, initialTerminalPaneId: nil,
                claudeSessionMode: .none
            )
            session.adoptPane(id: paneId, entry: entry)
        } else {
            // Deferred pane: spawn it fresh in the destination.
            // `ensurePaneSpawned` creates the session shell if absent.
            ensurePaneSpawned(
                tabId: newTabId, paneId: paneId, cwd: spawnCwd ?? projectPath
            )
        }
        onSessionMutation?()
        return newTabId
    }

    /// Adopt a live terminal pane (torn off from another window's
    /// TERMINALS section) as THIS window's Main terminal — REPLACING the
    /// pristine auto-seeded Main pane rather than adding a second
    /// TERMINALS section. This is the terminal-from-Terminals-section
    /// sibling to `adoptTerminalPaneAsNewTab` (which is used for a
    /// companion terminal torn off a Claude project).
    ///
    /// The new window seeds a fresh "Main" terminal tab in `start()`
    /// (`TabModel.init` / `ensureTerminalsProjectSeeded`); its single pane
    /// is deferred-armed (no real child yet). We:
    ///   1. Tear that seeded pane down (`terminatePane` handles the
    ///      armed-deferred / spawned / unspawned cases without leaking).
    ///   2. Replace the Main tab's pane list with the single torn-off
    ///      `Pane(id: paneId, …)` and focus it.
    ///   3. Select the Main tab.
    ///   4. Adopt the pane into the Main tab's existing session — via
    ///      `session.adoptPane` for a live entry (same mechanism
    ///      `adoptTerminalPaneAsNewTab` uses), or a fresh
    ///      `ensurePaneSpawned` for a deferred (nil-entry) pane. The Main
    ///      tab's session already exists because `AppState.start()`
    ///      called `makeSession` for it.
    /// Keeps the Main tab's id and title ("Main"). Fires
    /// `onSessionMutation`. Returns the Main tab id, or nil when `tabs`
    /// is gone or the Main tab can't be found.
    ///
    /// `entry` is OPTIONAL (BUG A): when nil the torn-off pane was
    /// modelled-but-deferred in the source, so the Main tab spawns it
    /// FRESH — in `spawnCwd` when provided (the cwd carried in the claim)
    /// so it opens in the right directory rather than the new window's
    /// pristine Main cwd.
    @discardableResult
    func adoptTerminalPaneAsMainTerminal(
        entry: TabPtySession.PaneEntry?,
        paneId: String,
        title: String,
        spawnCwd: String? = nil
    ) -> String? {
        guard let tabs else { return nil }
        let mainTabId = TabModel.mainTerminalTabId
        guard let mainTab = tabs.tab(for: mainTabId) else { return nil }

        // Snapshot the pristine seeded pane ids before mutating the tree.
        let seededPaneIds = mainTab.panes.map(\.id)

        // 1. Replace the Main tab's panes with the single torn-off pane
        //    and focus it. Keep id/title "Main".
        tabs.mutateTab(id: mainTabId) { tab in
            tab.panes = [Pane(id: paneId, title: title, kind: .terminal)]
            tab.activePaneId = paneId
        }
        tabs.activeTabId = mainTabId

        // 2. Adopt the pane into the Main tab's session BEFORE retiring
        //    the seeded pty. The session exists (start() made it). For a
        //    live entry, pass it straight through to `adoptLivePane`. For
        //    a deferred pane, spawn it fresh in the claim's `spawnCwd`
        //    (falling back to `adoptLivePane`'s tab-resolved cwd when no
        //    spawn cwd was carried). Either way the session ends up
        //    hosting `paneId`, so the `ensureActivePaneSpawned` that
        //    `paneExited` runs in step 3 sees the active pane as spawned
        //    and won't double-spawn it.
        if entry == nil, let spawnCwd {
            ensurePaneSpawned(tabId: mainTabId, paneId: paneId, cwd: spawnCwd)
        } else {
            adoptLivePane(tabId: mainTabId, paneId: paneId, entry: entry)
        }

        // 3. Retire the seeded pty entries. The model no longer references
        //    them, so the `paneExited` they fire is a no-op on the
        //    (already-replaced) tab — it only removes the stale pty entry.
        //    Skip the torn-off id defensively.
        for seededId in seededPaneIds where seededId != paneId {
            terminatePane(tabId: mainTabId, paneId: seededId)
        }

        onSessionMutation?()
        return mainTabId
    }

    // MARK: - Helpers exposed to the close-request coordinator on AppState

    /// Ask the pty session whether the named terminal pane currently
    /// has a foreground child (something running under the shell).
    /// Used by `AppState.isBusy` to decide whether closing the pane
    /// needs a confirmation prompt.
    func shellHasForegroundChild(tabId: String, paneId: String) -> Bool {
        ptySessions[tabId]?.shellHasForegroundChild(id: paneId) ?? false
    }

    /// SIGTERM the named pane and tear down its pty. The usual
    /// `paneExited` delegate fires and removes the pane from the model,
    /// dissolving the tab if it was the last pane. No-op if the pane
    /// is unspawned or the tab has no session.
    func terminatePane(tabId: String, paneId: String) {
        let key = Self.syntheticPaneKey(tabId: tabId, paneId: paneId)
        if syntheticHeldPanes.remove(key) != nil {
            // Test-only synthetic-held path: mirror the production
            // held-pane fast path on `TabPtySession.terminatePane`,
            // which fires `onPaneExit` synchronously. The exit code
            // here only feeds `paneExited`, which ignores it; tests
            // that need a specific code can inspect the model
            // directly after this call returns.
            syntheticSpawnedPanes.remove(key)
            paneExited(tabId: tabId, paneId: paneId, exitCode: 1)
            return
        }
        if syntheticArmedDeferredPanes.remove(key) != nil {
            // Test-only synthetic-armed-deferred path: mirror the
            // production armed-but-not-fired fast path on
            // `TabPtySession.terminatePane`, which cancels the
            // captured spawn and fires `onPaneExit(id, nil)`. There
            // is no real `pendingSpawn` to cancel here — the seam
            // simulates the post-cancel state directly. nil exit
            // code matches the production synthesis (no real child
            // ever ran).
            syntheticSpawnedPanes.remove(key)
            paneExited(tabId: tabId, paneId: paneId, exitCode: nil)
            return
        }
        ptySessions[tabId]?.terminatePane(id: paneId)
    }

    /// Tear down every pane on `tabId`'s session. Used by
    /// `restoreSavedWindow` when discarding the in-init main pty before
    /// rebuilding from the snapshot.
    func terminateAll(tabId: String) {
        ptySessions[tabId]?.terminateAll()
    }

    /// True iff `tabId`'s session has a live pty for `paneId`. Used by
    /// `AppState.hardKillTab` to split panes into spawned vs unspawned
    /// before deciding which to terminate vs synchronously remove.
    func paneIsSpawned(tabId: String, paneId: String) -> Bool {
        let key = Self.syntheticPaneKey(tabId: tabId, paneId: paneId)
        if syntheticSpawnedPanes.contains(key) { return true }
        return ptySessions[tabId]?.hasPane(paneId) ?? false
    }

    // MARK: - Test-only seams

    /// Set of `<tabId>:<paneId>` keys that `paneIsSpawned` should
    /// report as spawned without needing a real `TabPtySession` entry.
    /// Populated by `markSyntheticHeldPaneForTesting`; read by
    /// `paneIsSpawned`.
    @ObservationIgnored
    private var syntheticSpawnedPanes: Set<String> = []
    /// Subset of `syntheticSpawnedPanes` whose `terminatePane` should
    /// fire `paneExited` synchronously, mirroring the held-pane fast
    /// path in `TabPtySession.terminatePane`. Removed once consumed,
    /// matching the one-shot semantics of the production held entry.
    @ObservationIgnored
    private var syntheticHeldPanes: Set<String> = []
    /// Subset of `syntheticSpawnedPanes` whose `terminatePane` should
    /// fire `paneExited(tabId, paneId, nil)` synchronously, mirroring
    /// the armed-but-not-fired fast path in
    /// `TabPtySession.terminatePane`. Removed once consumed, matching
    /// the one-shot semantics of the production cancel — once you've
    /// declared the pane gone you can't re-cancel.
    @ObservationIgnored
    private var syntheticArmedDeferredPanes: Set<String> = []

    private static func syntheticPaneKey(tabId: String, paneId: String) -> String {
        "\(tabId):\(paneId)"
    }

    /// Test-only: mark `paneId` on `tabId` as if its child process had
    /// exited and Nice had decided to keep the view mounted (the held
    /// pane state). After this call, `paneIsSpawned` returns true for
    /// the pane (so close-flow code routes it through the spawned
    /// branch) and `terminatePane` fires `paneExited` synchronously
    /// (the production held-pane fast path). Lets close-flow tests
    /// repro the held + unspawned-companion scenario without standing
    /// up a real pty + SwiftTerm view. No-op for any other call site.
    func markSyntheticHeldPaneForTesting(tabId: String, paneId: String) {
        let key = Self.syntheticPaneKey(tabId: tabId, paneId: paneId)
        syntheticSpawnedPanes.insert(key)
        syntheticHeldPanes.insert(key)
    }

    /// Test-only: mark `paneId` on `tabId` as if its
    /// `NiceTerminalView` had captured a deferred spawn but never
    /// fired (the view never got a non-zero frame in a window). After
    /// this call, `paneIsSpawned` returns true (so `hardKillTab`
    /// routes the pane through the spawned branch) and
    /// `terminatePane` fires `paneExited(tabId, paneId, nil)`
    /// synchronously, matching the production armed-but-not-fired
    /// fast path on `TabPtySession.terminatePane`. Lets close-flow
    /// tests repro the right-click → Close on a never-focused
    /// resume-deferred Claude tab without standing up a real
    /// SwiftTerm view that AppKit would resize away from .zero.
    func markSyntheticArmedDeferredPaneForTesting(tabId: String, paneId: String) {
        let key = Self.syntheticPaneKey(tabId: tabId, paneId: paneId)
        syntheticSpawnedPanes.insert(key)
        syntheticArmedDeferredPanes.insert(key)
    }

    /// Drop the pty-session cache entry for `tabId`. Called by
    /// `AppState.finalizeDissolvedTab` during the dissolve cascade —
    /// the tab is already gone from the tree by then; this just
    /// releases the per-tab session record.
    func removePtySession(tabId: String) {
        ptySessions.removeValue(forKey: tabId)
    }

    // MARK: - Focus

    /// Hand AppKit first-responder status back to the active pane's
    /// terminal view. Call after any SwiftUI control (e.g. the sidebar
    /// rename field) finishes editing — SwiftUI does not restore focus
    /// to an embedded `NSView` when a TextField is torn down, so keys
    /// fall off the responder chain until the user clicks the terminal.
    /// The async hop lets SwiftUI finish its current update before the
    /// responder change, matching the pattern in `TerminalHost`.
    func focusActiveTerminal() {
        guard let tabs,
              let tabId = tabs.activeTabId,
              let tab = tabs.tab(for: tabId),
              let paneId = tab.activePaneId,
              let session = ptySessions[tabId],
              let view = session.view(forPane: paneId)
        else { return }
        view.wantsFocusOnAttach = true
        DispatchQueue.main.async {
            view.window?.makeFirstResponder(view)
        }
    }
}
