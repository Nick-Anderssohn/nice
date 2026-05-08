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
import SwiftUI

@MainActor
@Observable
final class SessionsModel {
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
                    case let .sessionUpdate(paneId, sessionId, source):
                        self.handleClaudeSessionUpdate(
                            paneId: paneId, sessionId: sessionId, source: source
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
                if idx < tab.panes.count {
                    tab.activePaneId = tab.panes[idx].id
                } else if idx > 0 {
                    tab.activePaneId = tab.panes[idx - 1].id
                } else {
                    tab.activePaneId = nil
                }
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

        if parsedId != nil {
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
    func handleClaudeSessionUpdate(
        paneId: String, sessionId: String, source: String?
    ) {
        guard let tabs, let tabId = tabs.tabIdOwning(paneId: paneId) else { return }
        let oldId = tabs.tab(for: tabId)?.claudeSessionId
        updateClaudeSessionId(tabId: tabId, sessionId: sessionId)
        guard source == "resume",
              let oldId,
              oldId != sessionId
        else { return }
        materializeBranchParent(forTabId: tabId, oldSessionId: oldId)
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

        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath,
            extraClaudeArgs: extraClaudeArgs,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: terminalPaneId,
            socketPath: controlSocket?.path,
            zdotdirPath: zdotdirPath,
            userZDotDir: userZDotDir,
            claudeSessionMode: claudeSessionMode,
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
