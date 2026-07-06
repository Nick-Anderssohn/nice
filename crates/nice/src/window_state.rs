//! `WindowState` — the per-window composition root, the Rust mirror of Swift's
//! `AppState` (`Sources/Nice/State/AppState.swift:60-75`).
//!
//! Each Nice window owns exactly one `WindowState`, held as a `gpui::Entity`
//! (app-global) and tracked by [`crate::window_registry::WindowRegistry`]. It is
//! handed to the window as a **constructor argument** by the app's window
//! builder (`crate::app::build_window_root`) — the deliberate inversion of
//! Swift's `WindowGroup` token dance (plan DO-NOT-PORT): "which saved slot /
//! which adopted pane does this window own" becomes a plain parameter. R18 will
//! pass restored state and R25 an adopted pane through the same seam.
//!
//! ## Decomposition (mirrors `AppState`)
//!
//! `AppState` holds six sub-models; R12 carries the subset that exists now,
//! per the plan's "Per-window state struct" decision:
//!
//! * [`model`](WindowState::model) — the R8 `TabModel` document (projects / tabs
//!   / panes), the single source of truth for a window's tab tree. Isolation
//!   between windows is exactly that each `WindowState` owns its own `TabModel`.
//! * [`sidebar`](WindowState::sidebar) — the R10 `SidebarModel` (collapse / mode
//!   / peek state).
//! * [`selection`](WindowState::selection) — the R10 `SidebarTabSelection`
//!   (Finder-style multi-select), seeded so the "selection ⊇ {active tab}"
//!   invariant holds from construction.
//! * [`sidebar_actions`](WindowState::sidebar_actions) /
//!   [`pane_strip_actions`](WindowState::pane_strip_actions) — the R10/R11
//!   create/close/select seams. Model-only today; R13 swaps the implementations
//!   for real sessions without touching callers.
//! * [`session`](WindowState::session) — the per-window
//!   [`SessionManager`](crate::session_manager::SessionManager) (R13). Owns the
//!   window's live pane sessions and routes their OSC title/cwd events into
//!   `model`; [`teardown`](WindowState::teardown) is the close hook that tears
//!   them down. R12 carried an empty placeholder here.
//!
//! `AppState`'s remaining sub-models (`sessions`, `closer`,
//! `fileExplorerOrchestrator`, `fileBrowserStore`) are deferred: sessions to R13,
//! the file explorer to R19. They land in later cycles behind the same struct.
//!
//! The fields carry `#![allow(dead_code)]`: R12 slice 1 establishes the state
//! container + window builder + registry; the *keymap* slice (R12 slice 2) is
//! the first live reader of `sidebar` / the action seams (routing ⌘B, ⌘T, the
//! pane-step actions through them), and R13 reads the session slot. The shapes
//! below are exercised by this module's tests.
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::AnyWindowHandle;
use nice_model::{PaneKind, SidebarMode, SidebarModel, SidebarTabSelection, TabModel};
use nice_term_view::TerminalEvent;

use crate::control_socket::{NiceControlSocket, Reply, RecordedSocketMessage, SocketMessage};
use crate::pane_strip_actions::{ModelPaneStripActions, PaneStripActions};
use crate::session_manager::{
    compose_claude_reply, mint_session_uuid, ClaudeReplyDecision, ClaudeTabPlacement,
    DissolveTerminus, SessionManager,
};
use crate::sidebar_actions::{ModelSidebarActions, SidebarActions};

/// Process-wide monotonic source of per-window session ids. Cheap, dependency-
/// free stand-in for Swift's `UUID().uuidString` window-session id — R13 owns
/// the real session identity, but a stable unique id per window exists now so
/// the registry's per-session-id lookup (undo routing, Stage 5) has a real key
/// to match on.
static NEXT_SESSION_SEQ: AtomicU64 = AtomicU64::new(1);

fn mint_session_id() -> String {
    format!("win-{}", NEXT_SESSION_SEQ.fetch_add(1, Ordering::Relaxed))
}

/// A deferred `newtab` spawn request returned by
/// [`WindowState::resolve_claude_request`] — the `newtab` reply has already gone
/// out, and the gpui-context-carrying caller must build + spawn the Claude tab.
struct NewTabSpawn {
    cwd: String,
    args: Vec<String>,
}

/// The per-window composition root. One per Nice window, owned by a
/// `gpui::Entity` and registered in [`crate::window_registry::WindowRegistry`].
pub(crate) struct WindowState {
    /// The R8 document — this window's projects / tabs / panes tree. Two windows
    /// are isolated precisely because each owns its own `TabModel`.
    pub(crate) model: TabModel,
    /// R10 sidebar collapse / mode / peek state.
    pub(crate) sidebar: SidebarModel,
    /// R10 Finder-style multi-selection (invariant: contains the active tab).
    pub(crate) selection: SidebarTabSelection,
    /// R10 sidebar create/close/select seam (model-only; R13 rewires).
    pub(crate) sidebar_actions: Box<dyn SidebarActions>,
    /// R11 pane-strip select/close/add seam (model-only; R13 rewires).
    pub(crate) pane_strip_actions: Box<dyn PaneStripActions>,
    /// The per-window pty/session manager (R13). Owns this window's live pane
    /// sessions and routes their OSC title/cwd events into `model`. R12 carried
    /// an empty placeholder here; R13 slice 1 fills it with the real
    /// [`SessionManager`] (the action seams that drive it are rewired in a later
    /// R13 slice — this just makes the manager part of the per-window state).
    pub(crate) session: SessionManager,
    /// Stable unique per-window id (the registry's per-session-id lookup key).
    session_id: String,
    /// R14 control-socket routing record: the parsed / normalized messages this
    /// window received through [`route_socket_message`](WindowState::route_socket_message).
    /// Populated only under `cfg(test)` or the `selftest` feature (see
    /// [`record_socket_message`](WindowState::record_socket_message)) — production
    /// leaves it empty. The `shell-socket` scenario's raw-socket headless driver
    /// and the routing unit tests assert against it.
    recorded_socket_messages: Vec<RecordedSocketMessage>,
    /// R14 per-window control socket, owned here so [`teardown`](WindowState::teardown)
    /// can stop it (suppress healing, unlink the socket file) on window close.
    /// Armed by `crate::app::arm_window_control_socket` before the Main pane forks;
    /// `None` on scenarios/itests that never bootstrap one. `NiceControlSocket`'s
    /// own `Drop` also stops it, so a dropped `WindowState` never leaks its thread.
    control_socket: Option<NiceControlSocket>,
    /// The gpui foreground task draining parsed socket messages into
    /// [`route_socket_message`](WindowState::route_socket_message). Held (not
    /// detached) so dropping it — on teardown or when the window entity drops —
    /// cancels the drain rather than leaking a parked task.
    socket_drain: Option<gpui::Task<()>>,
    /// R15: the injectable Claude theme-sync `--settings` pointer provider (Swift
    /// `themeCache.syncClaudeTheme ? ClaudeThemeSync.settingsFlagPath() : nil`).
    /// `None` in R15 — R17 fills it from the live theme; the socket reply and the
    /// Claude spawn both consult it, and the socket reply additionally suppresses
    /// it when the client's `args` already carry `--settings` (no doubled flag).
    /// Unit tests inject a stub via [`set_claude_settings_path_for_test`](WindowState::set_claude_settings_path_for_test).
    claude_settings_path: Option<String>,
    /// R15 subscription lift: this window's handle, stashed at
    /// [`crate::app::build_window_root`]. The pane-event subscription callback
    /// ([`subscribe_spawned_panes`](WindowState::subscribe_spawned_panes)) needs a
    /// `&mut Window` to actuate a [`RoutedExit`](crate::session_manager)'s
    /// every-project-empty terminus (close this window / quit), which an
    /// entity-subscription callback lacks — it re-enters through this handle. `None`
    /// on a `WindowState` never mounted by the shipped builder (unit tests /
    /// headless scenarios that assert the routed model mutation only).
    window_handle: Option<AnyWindowHandle>,
    /// R15 subscription lift: the `<tab>:<pane>` keys already wired to
    /// [`route_terminal_event`](crate::session_manager::SessionManager::route_terminal_event)
    /// via [`subscribe_spawned_panes`](WindowState::subscribe_spawned_panes). The
    /// subscribe-once dedupe: the sweep runs on every `PaneHostView` render, but a
    /// pane is subscribed exactly once (its entity's `Drop` retires the
    /// subscription when the pane leaves the model / teardown).
    subscribed_panes: HashSet<String>,
}

impl WindowState {
    /// A fresh default window: a seeded [`TabModel`] rooted at `initial_cwd`
    /// (pinned Terminals group + Main tab, per `TabModel::new`), an expanded
    /// sidebar in tabs mode, and a selection seeded from the model's active tab —
    /// mirroring `AppState`'s convenience init defaults
    /// (`initialSidebarCollapsed: false`, `initialSidebarMode: .tabs`). Every ⌘N
    /// mints one of these; R18 will add a variant that takes restored state.
    pub(crate) fn new(initial_cwd: impl Into<String>) -> Self {
        Self::with_model(TabModel::new(initial_cwd))
    }

    /// A window seeded around a pre-built [`TabModel`] — the scenario/restore
    /// seam. The isolated `sidebar` / `pane-strip` self-test windows use it to
    /// mount the shipped views (`SidebarShellView` / `WindowToolbarView`) over a
    /// fixture model while still going through the SAME shared-state shape the
    /// managed window uses (R13.5's "seed a `WindowState` around their seed
    /// models" decision); R18's restore path will thread persisted state through
    /// here too. Same defaults as [`new`](WindowState::new) otherwise (expanded
    /// sidebar, tabs mode, model-only action seams, a fresh [`SessionManager`],
    /// a unique session id), and it re-seeds the selection from the model's active
    /// tab so the "selection ⊇ {active tab}" invariant holds from construction.
    pub(crate) fn with_model(model: TabModel) -> Self {
        let mut selection = SidebarTabSelection::new();
        selection.sync_active_tab_id(model.active_tab_id());
        Self {
            model,
            sidebar: SidebarModel::new(false, SidebarMode::Tabs),
            selection,
            sidebar_actions: Box::new(ModelSidebarActions::new()),
            pane_strip_actions: Box::new(ModelPaneStripActions::new()),
            session: SessionManager::new(),
            session_id: mint_session_id(),
            recorded_socket_messages: Vec::new(),
            control_socket: None,
            socket_drain: None,
            claude_settings_path: None,
            window_handle: None,
            subscribed_panes: HashSet::new(),
        }
    }

    /// Stash this window's handle (the shipped builder calls it at
    /// [`crate::app::build_window_root`]). Read by
    /// [`subscribe_spawned_panes`](Self::subscribe_spawned_panes)'s routed-exit
    /// terminus actuation.
    pub(crate) fn set_window_handle(&mut self, handle: AnyWindowHandle) {
        self.window_handle = Some(handle);
    }

    /// R15 subscription lift — the shipped-window twin of the
    /// `session-lifecycle` scenario's `spawn_and_subscribe` (the tranche's known
    /// integration gap: `route_terminal_event` was wired ONLY in that scenario, so
    /// in the shipped app OSC titles/cwd and exits dead-ended at the view adapter).
    /// Sweeps every live pane session and subscribes any not-yet-wired one's entity
    /// to [`route_terminal_event`](SessionManager::route_terminal_event), so the
    /// SHIPPED window retitles pills, updates pane cwd, and removes exited panes.
    ///
    /// Called from [`crate::app_shell::PaneHostView`]'s render — the single choke
    /// point every spawn flows past (the Main pane is spawned before the first
    /// render; deferred terminals spawn through `activate_pane` on activation; a
    /// Claude tab's spawn + the socket newtab spawn each re-render the shell). It is
    /// idempotent via [`subscribed_panes`](Self::subscribed_panes) (subscribe-once
    /// dedupe), so running it every render is safe and cheap.
    ///
    /// The [`RoutedExit`](crate::session_manager) neighbor-refocus spawn is
    /// **composed by `PaneHostView`'s activation path**, not re-actuated here: the
    /// `cx.notify()` below re-renders the host, whose activation change re-runs
    /// `activate_pane` (deferred-companion spawn + key focus) per the landed
    /// M2 focus-routing. Only the every-project-empty terminus — which needs a
    /// `&mut Window` a subscription callback lacks — is actuated here, via the
    /// stashed [`window_handle`](Self::window_handle).
    pub(crate) fn subscribe_spawned_panes(&mut self, cx: &mut gpui::Context<WindowState>) {
        for (tab_id, pane_id) in self.session.live_pane_keys() {
            let key = format!("{tab_id}:{pane_id}");
            if self.subscribed_panes.contains(&key) {
                continue;
            }
            let Some(handle) = self.session.pane_handle(&tab_id, &pane_id) else {
                continue;
            };
            self.subscribed_panes.insert(key);
            let (t, p) = (tab_id.clone(), pane_id.clone());
            cx.subscribe(&handle, move |ws, _handle, event: &TerminalEvent, cx| {
                let model = &mut ws.model;
                let selection = &mut ws.selection;
                let routed = ws
                    .session
                    .route_terminal_event(model, selection, &t, &p, event);
                // Re-render so `PaneHostView` re-activates: a routed pane removal
                // shifts the active pane, and its activation change re-runs
                // `activate_pane` (neighbor deferred-companion spawn + key focus),
                // and pills / cwd refresh from the mutated model.
                cx.notify();
                // The every-project-empty terminus (close this window / quit) needs
                // a `&mut Window`; actuate it via the stashed handle (the "composed
                // by the live window root" obligation).
                if routed.terminus == DissolveTerminus::WindowEmptied {
                    if let Some(handle) = ws.window_handle {
                        let _ = handle.update(cx, |_root, window, app| {
                            SessionManager::apply_dissolve_terminus(routed.terminus, window, app);
                        });
                    }
                }
            })
            .detach();
        }
    }

    /// Test seam: inject the Claude theme-sync `--settings` pointer provider (R17's
    /// value; `None` by default). Drives the sync-ON socket-reply cases.
    #[cfg(test)]
    pub(crate) fn set_claude_settings_path_for_test(&mut self, path: Option<String>) {
        self.claude_settings_path = path;
    }

    /// The Claude theme-sync `--settings` pointer provider value (R17 fills it;
    /// `None` in R15). Read by the sidebar project-`+` seam when it spawns a fresh
    /// Claude tab. The socket reply consults [`effective_inplace_settings`](Self::effective_inplace_settings)
    /// (the same provider, plus the `--settings`-in-args gate).
    pub(crate) fn claude_settings_path_provider(&self) -> Option<String> {
        self.claude_settings_path.clone()
    }

    /// Take ownership of this window's armed control socket + its foreground drain
    /// task (`crate::app::arm_window_control_socket`, called before the Main pane
    /// forks). Stopping/replacing any prior socket first keeps a re-arm idempotent.
    pub(crate) fn install_control_socket(
        &mut self,
        socket: NiceControlSocket,
        drain: gpui::Task<()>,
    ) {
        if let Some(old) = self.control_socket.take() {
            old.stop();
        }
        self.control_socket = Some(socket);
        self.socket_drain = Some(drain);
    }

    /// Test seam: install just the control socket (no gpui drain task, which needs
    /// a `Context`) so a plain `#[test]` can pin that `teardown` stops + unlinks it.
    #[cfg(test)]
    pub(crate) fn set_control_socket_for_test(&mut self, socket: NiceControlSocket) {
        self.control_socket = Some(socket);
    }

    /// This window's stable session id — the registry's per-session-id lookup
    /// key (undo routing, Stage 5). R13 reconciles it with the real session
    /// identity.
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// This window's armed control-socket path, if one is bound (`None` on a
    /// window that never bootstrapped a socket). The `claude-lifecycle` scenario
    /// reads it to drive raw-`UnixStream` `claude` requests against the SHIPPED
    /// window (which arms its socket inside `open_managed_window` and discards the
    /// path).
    pub(crate) fn control_socket_path(&self) -> Option<String> {
        self.control_socket.as_ref().map(|s| s.path().to_string())
    }

    /// The R14 control-socket routing point (the Rust mirror of Swift
    /// `SessionsModel.startSocketListener`'s handler dispatch,
    /// `SessionsModel.swift:257-309`): each [`SocketMessage`] variant is routed
    /// to a named window-local handler. The message enum + parser are finished
    /// business after R14 — R15/R16/R26 replace only the handler BODIES below,
    /// never this routing shape. Called on the gpui foreground by the socket
    /// drain task (wired by the R14 env-injection slice's `open_managed_window`).
    ///
    /// Takes the window's `&mut Context` (R15): the `claude` newtab decision spawns
    /// a Claude pane, which needs a gpui context. The `session_update` / `handoff`
    /// sub-handlers stay context-free.
    pub(crate) fn route_socket_message(
        &mut self,
        msg: SocketMessage,
        cx: &mut gpui::Context<WindowState>,
    ) {
        match msg {
            SocketMessage::Claude {
                cwd,
                args,
                tab_id,
                pane_id,
                reply,
            } => self.handle_claude_socket_request(cwd, args, tab_id, pane_id, reply, cx),
            SocketMessage::SessionUpdate {
                pane_id,
                session_id,
                source,
                cwd,
            } => self.handle_session_update(pane_id, session_id, source, cwd),
            SocketMessage::Handoff {
                cwd,
                handoff_file,
                instructions,
                model,
                effort,
                tab_id,
                pane_id,
                reply,
            } => self.handle_handoff(
                cwd,
                handoff_file,
                instructions,
                model,
                effort,
                tab_id,
                pane_id,
                reply,
            ),
        }
    }

    /// Handle a `claude` invocation from a pane's zsh wrapper — the Rust twin of
    /// Swift `SessionsModel.handleClaudeSocketRequest` (`SessionsModel.swift:827-911`).
    /// The wrapper is blocked reading a single-line reply, so
    /// [`resolve_claude_request`](Self::resolve_claude_request) replies exactly once
    /// on every path. On the newtab decision it returns the spawn request, which
    /// this (gpui-context-carrying) handler fulfils by building + spawning a fresh
    /// Claude tab through the ONE shared constructor.
    fn handle_claude_socket_request(
        &mut self,
        cwd: String,
        args: Vec<String>,
        tab_id: String,
        pane_id: String,
        reply: Reply,
        cx: &mut gpui::Context<WindowState>,
    ) {
        self.record_socket_message(RecordedSocketMessage::Claude {
            cwd: cwd.clone(),
            args: args.clone(),
            tab_id: tab_id.clone(),
            pane_id: pane_id.clone(),
        });
        if let Some(spawn) = self.resolve_claude_request(&cwd, &args, &tab_id, &pane_id, reply) {
            // newtab: build + spawn the tab (the "newtab" reply already went out).
            // The spawn's settings pointer is the provider value (no args gate here —
            // Swift's `makeSession` uses the raw provider; only the reply gates on
            // `--settings`, done inside `resolve_claude_request`).
            let settings = self.claude_settings_path.clone();
            let model = &mut self.model;
            let session = &mut self.session;
            let created = session.create_claude_tab(
                model,
                ClaudeTabPlacement::Bucket { cwd: spawn.cwd },
                &spawn.args,
                settings.as_deref(),
                cx,
            );
            if created.is_some() {
                // Keep the "selection ⊇ {active tab}" invariant: the new tab is now
                // active (Swift sets `activeTabId`).
                self.selection.sync_active_tab_id(self.model.active_tab_id());
            }
        }
        // Re-render: the newtab appears / the in-place promotion retitles the pill.
        cx.notify();
    }

    /// The `claude` newtab/inplace decision + reply — the pure, spawn-free half of
    /// [`handle_claude_socket_request`](Self::handle_claude_socket_request), so the
    /// dispatch's model side effects are unit-testable without a gpui context
    /// (Swift `handleClaudeSocketRequest:834-910`). Replies exactly once. Returns
    /// `Some(NewTabSpawn)` when the caller must build + spawn a fresh Claude tab
    /// (the `newtab` reply already went out); `None` when it promoted in place (the
    /// model mutation is applied and the `inplace…` reply already went out).
    fn resolve_claude_request(
        &mut self,
        cwd: &str,
        args: &[String],
        tab_id: &str,
        pane_id: &str,
        reply: Reply,
    ) -> Option<NewTabSpawn> {
        // Decision: promote in place ONLY when the request names a real pane in a
        // known, non-Terminals tab that has NO running Claude; else open a new tab
        // (empty/unknown tabId, a Terminals-group tab, a stale paneId, or the
        // ≤1-Claude-per-tab guard).
        let known = !tab_id.is_empty() && self.model.tab_for(tab_id).is_some();
        let is_terminals = self.model.is_terminals_project_tab(tab_id);
        let (pane_in_tab, has_running) = match self.model.tab_for(tab_id) {
            Some(tab) => (
                tab.panes.iter().any(|p| p.id == pane_id),
                tab.panes.iter().any(|p| p.is_claude_running),
            ),
            None => (false, false),
        };
        if !(known && !is_terminals && pane_in_tab && !has_running) {
            reply.send("newtab");
            return Some(NewTabSpawn {
                cwd: cwd.to_string(),
                args: args.to_vec(),
            });
        }

        // Promotion in place. Extract `--resume`/`--session-id` from args if present
        // (a restored deferred pane's pre-typed `claude --resume <uuid>`); else mint
        // a fresh session id to persist for the next relaunch.
        let parsed = TabModel::extract_claude_session_id(args);
        let parsed_from_args = parsed.is_some();
        let session_id = parsed.unwrap_or_else(mint_session_uuid);
        self.model.mutate_tab(tab_id, |tab| {
            if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                pane.kind = PaneKind::Claude;
                // The ONLY production false→true flip of `is_claude_running` (the
                // signal `pane_title_changed`'s OSC gate releases on).
                pane.is_claude_running = true;
                // Seed "Claude" so the pill isn't stale until the OSC arrives —
                // unless the user hand-renamed the pane (the OSC gate would block
                // the next title anyway).
                if !pane.title_manually_set {
                    pane.title = "Claude".to_string();
                }
            }
            tab.active_pane_id = Some(pane_id.to_string());
            tab.claude_session_id = Some(session_id.clone());
        });
        // onSessionMutation → R18's did-mutate save; nothing to persist yet.

        // Reply. Hand the wrapper the theme pointer when the provider has one AND
        // the args don't already carry `--settings` (no doubled flag). Sync off /
        // gated → the reply stays byte-identical to the pre-theming protocol.
        let settings = self.effective_inplace_settings(args);
        let line = compose_claude_reply(
            &ClaudeReplyDecision::InPlace {
                parsed_from_args,
                session_id,
            },
            settings.as_deref(),
        );
        reply.send(&line);
        None
    }

    /// The `--settings` pointer to splice into the in-place promotion reply: the
    /// provider's value, suppressed when the client's `args` already carry
    /// `--settings` (Swift `themeCache.syncClaudeTheme && !args.contains("--settings")`).
    fn effective_inplace_settings(&self, args: &[String]) -> Option<String> {
        if args.iter().any(|a| a == "--settings") {
            return None;
        }
        self.claude_settings_path.clone()
    }

    /// `session_update` action stub — fully parsed, no-op body. R16 fills it with
    /// the session-id / cwd rotation routing (`SessionsModel.applySessionUpdate`).
    fn handle_session_update(
        &mut self,
        pane_id: String,
        session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    ) {
        self.record_socket_message(RecordedSocketMessage::SessionUpdate {
            pane_id,
            session_id,
            source,
            cwd,
        });
        // No-op: the client fd was already closed before dispatch (fire-and-forget).
    }

    /// `handoff` action stub. R14 replies `error: handoff is not supported yet`;
    /// the installed helper's `error*` branch handles that gracefully (the user
    /// gets a clear message instead of a silent hang). R26 fills this body with
    /// the nested-handoff-tab open + `ok` reply (`SkillInstaller` / handoff
    /// receiver).
    #[allow(clippy::too_many_arguments)]
    fn handle_handoff(
        &mut self,
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        tab_id: String,
        pane_id: String,
        reply: Reply,
    ) {
        self.record_socket_message(RecordedSocketMessage::Handoff {
            cwd,
            handoff_file,
            instructions,
            model,
            effort,
            tab_id,
            pane_id,
        });
        reply.send("error: handoff is not supported yet");
    }

    /// Record a routed message for the scenario / routing tests. Compiled to a
    /// no-op in a production build (no `selftest` feature) so a long-lived
    /// window never accumulates messages — the accessor is test-only, and R15
    /// replaces these handler bodies wholesale.
    fn record_socket_message(&mut self, msg: RecordedSocketMessage) {
        #[cfg(any(test, feature = "selftest"))]
        self.recorded_socket_messages.push(msg);
        #[cfg(not(any(test, feature = "selftest")))]
        let _ = msg;
    }

    /// The parsed / normalized messages this window has routed, in arrival order.
    /// Populated only under `cfg(test)` / the `selftest` feature (see
    /// [`record_socket_message`](WindowState::record_socket_message)); returns an
    /// EMPTY slice in a production build (recording is a no-op there, so a
    /// long-lived window never accumulates). Always compiled — the `shell-socket`
    /// scenario module is always built (meaningful only under `--features
    /// selftest`), so it must be able to name this accessor even in a plain
    /// `cargo run -p nice` build. The scenario asserts a routed `claude` carried
    /// the pane's exact tabId/paneId/cwd and a raw-`UnixStream` `session_update`
    /// surfaced normalized.
    pub(crate) fn recorded_socket_messages(&self) -> &[RecordedSocketMessage] {
        &self.recorded_socket_messages
    }

    /// Tear the window's owned resources down on close. R12 has nothing to stop
    /// (the shipped live terminal is owned by the view and dies with the window's
    /// entity, exactly as before this cycle); this is the hook
    /// [`crate::window_registry::WindowRegistry`] calls on window close, which
    /// R13 extends to terminate the window's sessions / ptys. Idempotent.
    pub(crate) fn teardown(&mut self) {
        // R14: stop this window's control socket first — suppress healing, unblock
        // the accept loop, and unlink the socket file (Swift `SessionsModel.tearDown`'s
        // `controlSocket?.stop()`). Dropping the held drain task cancels the
        // foreground drain so no parked task lingers past the window.
        self.socket_drain = None;
        if let Some(socket) = self.control_socket.take() {
            socket.stop();
        }
        // Terminate this window's ptys: dropping each cached session handle tears
        // its child process group down (SIGHUP→SIGKILL), so no orphan zsh
        // survives. R18 flushes the session snapshot before this runs. Idempotent.
        self.session.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_socket::{Reply, RecordedSocketMessage};
    use nice_model::{Pane, PaneKind, Tab, TabModel};
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    #[test]
    fn new_seeds_default_window_shape() {
        let state = WindowState::new("/home/u");
        // Seeded TabModel: the pinned Terminals group + Main tab, Main active.
        assert_eq!(
            state.model.active_tab_id(),
            Some(TabModel::MAIN_TERMINAL_TAB_ID),
            "the Main terminal tab is active on a fresh window"
        );
        assert!(
            state
                .model
                .projects
                .iter()
                .any(|p| p.id == TabModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group is present"
        );
        // Sidebar defaults: expanded, tabs mode (AppState convenience-init parity).
        assert!(!state.sidebar.collapsed(), "sidebar starts expanded");
        assert_eq!(state.sidebar.mode(), SidebarMode::Tabs);
        // Selection invariant: the active tab is selected from construction.
        assert!(
            state.selection.contains(TabModel::MAIN_TERMINAL_TAB_ID),
            "selection is seeded with the active tab"
        );
    }

    #[test]
    fn each_window_has_a_unique_session_id() {
        let a = WindowState::new("/home/u");
        let b = WindowState::new("/home/u");
        assert!(!a.session_id().is_empty());
        assert_ne!(
            a.session_id(),
            b.session_id(),
            "session ids must be unique per window (the undo-routing lookup key)"
        );
    }

    #[test]
    fn windows_are_isolated_mutating_one_model_leaves_the_other_untouched() {
        // The isolation guarantee at the state level: two windows own
        // independent TabModels, so a mutation to one's tree is invisible to the
        // other. (The live two-window itest — mutate A, B byte-identical — is the
        // scenario slice; this pins the underlying state ownership.)
        let mut a = WindowState::new("/home/u");
        let b = WindowState::new("/home/u");

        let before_b: Vec<usize> = b.model.projects.iter().map(|p| p.tabs.len()).collect();

        // Mutate A's tree through its own seam (the same surface the keymap slice
        // will drive): add a terminal tab.
        let new_id = a
            .sidebar_actions
            .create_terminal_tab(&mut a.model)
            .expect("Terminals group exists");
        assert!(a.model.tab_for(&new_id).is_some(), "A gained the new tab");

        let after_b: Vec<usize> = b.model.projects.iter().map(|p| p.tabs.len()).collect();
        assert_eq!(before_b, after_b, "B's tree is unchanged by A's mutation");
        assert!(
            b.model.tab_for(&new_id).is_none(),
            "A's new tab never appears in B"
        );
    }

    #[test]
    fn teardown_is_idempotent() {
        // R12's teardown is a no-op hook; calling it more than once is safe (R13
        // extends it to real session teardown, which must also be idempotent —
        // the registry calls it exactly once on close, but app-terminate paths
        // may double up).
        let mut state = WindowState::new("/home/u");
        state.teardown();
        state.teardown();
    }

    // ---- R14 control-socket routing point + stub handlers -------------------

    /// The handler writes its line then drops the server end (EOF); read to EOF.
    fn read_reply(mut client: UnixStream) -> String {
        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();
        buf
    }

    #[test]
    fn handoff_stub_replies_error_and_records_message() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut state = WindowState::new("/home/u");
        // The `handoff` sub-handler is context-free (only `claude` needs the gpui
        // context for its newtab spawn), so drive it directly.
        state.handle_handoff(
            "/tmp/work".into(),
            "/tmp/work/.claude/handoff/h.md".into(),
            String::new(),
            String::new(),
            String::new(),
            "t1".into(),
            "p1".into(),
            Reply::for_test(server),
        );
        // R14 stub: the installed helper's `error*` branch degrades gracefully.
        // R26 replaces the body with a nested-tab open + `ok` reply.
        assert_eq!(read_reply(client), "error: handoff is not supported yet\n");
        assert_eq!(state.recorded_socket_messages().len(), 1);
    }

    // ---- R15 SessionsModelClaudeSocketRequestTests (decision + reply) --------
    //
    // Ported from Swift `SessionsModelClaudeSocketRequestTests`. Each drives
    // `resolve_claude_request` — the spawn-free half of the `claude` handler — so
    // the decision + reply + model mutation are observable without a gpui context
    // (the newtab SPAWN + the claude-lifecycle end-to-end are the slice-3 scenario).

    /// Seed a `[Claude, Terminal 1]` tab (Claude focused) into a fresh non-Terminals
    /// project `p` — the Rust twin of `TabModelFixtures.seedClaudeTab`. Returns
    /// `(claude_pane_id, terminal_pane_id)`.
    fn seed_claude_tab(
        model: &mut TabModel,
        tab_id: &str,
        session_id: &str,
        is_running: bool,
    ) -> (String, String) {
        let claude_id = format!("{tab_id}-claude");
        let term_id = format!("{tab_id}-t1");
        let mut claude = Pane::new(&claude_id, "Claude", PaneKind::Claude);
        claude.is_claude_running = is_running;
        let mut tab = Tab::new(tab_id, "New tab", "/tmp/p");
        tab.panes = vec![
            claude,
            Pane::new(&term_id, "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some(claude_id.clone());
        tab.claude_session_id = Some(session_id.to_string());
        tab.next_terminal_index = 2;
        model.ensure_project("p", "P", "/tmp/p");
        let pi = model.projects.iter().position(|p| p.id == "p").unwrap();
        model.projects[pi].tabs.push(tab);
        (claude_id, term_id)
    }

    /// Drive `resolve_claude_request` and return the single reply line it wrote
    /// (with its trailing `\n`). Ignores the returned newtab-spawn request (the
    /// spawn is the scenario's concern; these tests pin the decision + reply).
    fn drive_claude(
        state: &mut WindowState,
        cwd: &str,
        args: &[&str],
        tab_id: &str,
        pane_id: &str,
    ) -> String {
        let (client, server) = UnixStream::pair().unwrap();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let _ = state.resolve_claude_request(cwd, &args, tab_id, pane_id, Reply::for_test(server));
        read_reply(client)
    }

    #[test]
    fn claude_empty_tab_id_replies_newtab() {
        let mut state = WindowState::new("/home/u");
        assert_eq!(drive_claude(&mut state, "/tmp/x", &[], "", "p"), "newtab\n");
    }

    #[test]
    fn claude_terminals_project_tab_replies_newtab() {
        // The pinned Terminals group never hosts Claude — a bare `claude` from the
        // Main pane opens a fresh tab, never promotes the Main pane in place.
        let mut state = WindowState::new("/home/u");
        let main = TabModel::MAIN_TERMINAL_TAB_ID;
        let main_pane = state.model.tab_for(main).unwrap().panes[0].id.clone();

        assert_eq!(drive_claude(&mut state, "/tmp/x", &[], main, &main_pane), "newtab\n");

        let pane = &state.model.tab_for(main).unwrap().panes[0];
        assert_eq!(pane.kind, PaneKind::Terminal, "Main pane must NOT promote");
        assert!(!pane.is_claude_running, "Main pane must NOT flip claude-running");
    }

    #[test]
    fn claude_pane_id_not_in_tab_replies_newtab() {
        // A stale paneId (pane exited while the wrapper's nc was in flight) falls
        // through to a new tab.
        let mut state = WindowState::new("/home/u");
        seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &[], "t1", "does-not-exist"),
            "newtab\n"
        );
    }

    #[test]
    fn claude_existing_running_claude_replies_newtab() {
        // The ≤1-Claude-per-tab invariant: a tab that already has a live Claude
        // pane opens a fresh tab rather than promoting a second one.
        let mut state = WindowState::new("/home/u");
        let (_c, term) = seed_claude_tab(&mut state.model, "t1", "OLD", true);
        assert_eq!(drive_claude(&mut state, "/tmp/p", &[], "t1", &term), "newtab\n");
        let pane = state
            .model
            .tab_for("t1")
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == term)
            .unwrap();
        assert_eq!(pane.kind, PaneKind::Terminal, "terminal pane must NOT promote");
        assert!(!pane.is_claude_running);
    }

    #[test]
    fn claude_inplace_with_session_id_flips_running_and_replies_inplace() {
        // Deferred-resume promotion: args already carry `--resume <uuid>`, so the
        // reply is the bare `inplace` (wrapper passes args through) and the pane's
        // is_claude_running flips false→true (the gate-release signal T5 keys on).
        let mut state = WindowState::new("/home/u");
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            "inplace\n"
        );

        let tab = state.model.tab_for("t1").unwrap();
        let pane = tab.panes.iter().find(|p| p.id == claude).unwrap();
        assert!(pane.is_claude_running, "deferred-resume promotion flips running");
        assert_eq!(pane.kind, PaneKind::Claude);
        assert_eq!(pane.title, "Claude", "pill reset to Claude until the OSC arrives");
        assert_eq!(tab.active_pane_id.as_deref(), Some(claude.as_str()));
        assert_eq!(
            tab.claude_session_id.as_deref(),
            Some("abc-123"),
            "the id parsed from --resume overwrites the seeded session id"
        );
    }

    #[test]
    fn claude_inplace_without_session_id_mints_and_replies_with_it() {
        // Plain `claude` in a terminal pane inside a Claude tab: mint a fresh id and
        // ship it back so the wrapper can prepend `--session-id <uuid>`.
        let mut state = WindowState::new("/home/u");
        let (_c, term) = seed_claude_tab(&mut state.model, "t1", "OLD", false);

        let reply = drive_claude(&mut state, "/tmp/p", &[], "t1", &term);
        let reply = reply.trim_end();
        assert!(reply.starts_with("inplace "), "reply is 'inplace <uuid>': {reply:?}");
        let minted = reply.strip_prefix("inplace ").unwrap();
        assert!(!minted.is_empty(), "reply carries the freshly minted uuid");

        let tab = state.model.tab_for("t1").unwrap();
        assert_eq!(
            tab.claude_session_id.as_deref(),
            Some(minted),
            "wrapper + model must agree on the persisted session id"
        );
        let pane = tab.panes.iter().find(|p| p.id == term).unwrap();
        assert_eq!(pane.kind, PaneKind::Claude, "terminal pane promotes to claude");
        assert!(pane.is_claude_running);
        assert_eq!(pane.title, "Claude");
    }

    #[test]
    fn claude_inplace_with_session_id_sync_on_appends_settings_pointer() {
        // Sync on + user-supplied session id → 'inplace - <pointer>' (the `-` sid
        // placeholder lets the --settings path follow as the 3rd field).
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            "inplace - /ptr.json\n"
        );
    }

    #[test]
    fn claude_inplace_without_session_id_sync_on_appends_pointer_after_minted_id() {
        // Sync on, mint-new path → 'inplace <uuid> <pointer>' (wrapper prepends both
        // --settings and --session-id).
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (_c, term) = seed_claude_tab(&mut state.model, "t1", "OLD", false);

        let reply = drive_claude(&mut state, "/tmp/p", &[], "t1", &term);
        let parts: Vec<&str> = reply.trim_end().split(' ').collect();
        assert_eq!(parts.len(), 3, "reply is 'inplace <uuid> <pointer>': {reply:?}");
        assert_eq!(parts[0], "inplace");
        assert_ne!(parts[1], "-", "mint-new path uses the real minted id");
        assert_eq!(parts[2], "/ptr.json", "third field is the --settings pointer");
        assert_eq!(
            state.model.tab_for("t1").unwrap().claude_session_id.as_deref(),
            Some(parts[1]),
            "minted id in the reply matches the persisted tab session id"
        );
    }

    #[test]
    fn claude_inplace_sync_off_replies_byte_identical() {
        // Sync off (the default): reply is byte-identical to the pre-theming protocol.
        let mut state = WindowState::new("/home/u");
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            "inplace\n"
        );
    }

    #[test]
    fn claude_inplace_sync_on_args_already_have_settings_does_not_double() {
        // A restored deferred pane re-dispatches its pre-typed `claude --settings
        // <path> --resume <id>`; the reply must NOT append a second pointer.
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(
                &mut state,
                "/tmp/p",
                &["--settings", "/ptr.json", "--resume", "abc-123"],
                "t1",
                &claude
            ),
            "inplace\n"
        );
    }

    #[test]
    fn teardown_stops_and_unlinks_the_control_socket() {
        use crate::control_socket::NiceControlSocket;
        use std::path::Path;

        let mut state = WindowState::new("/home/u");
        let socket = NiceControlSocket::new();
        // Bind + start so the socket file exists on disk (a no-op handler is fine —
        // this test never connects a client; it asserts the unlink-on-teardown).
        socket.start(|_msg| {}).expect("control socket should bind");
        let path = socket.path().to_string();
        assert!(Path::new(&path).exists(), "precondition: the socket file is bound");

        state.set_control_socket_for_test(socket);
        state.teardown();
        assert!(
            !Path::new(&path).exists(),
            "teardown must stop the control socket and unlink its file"
        );
        // Idempotent — a second teardown (app-terminate double-up) must not panic.
        state.teardown();
    }

    #[test]
    fn session_update_stub_records_normalized_message_and_sends_no_reply() {
        let mut state = WindowState::new("/home/u");
        // session_update is fire-and-forget and context-free — drive the sub-handler
        // directly. It just records the parsed, normalized message (R16 fills the body).
        state.handle_session_update("P1".into(), "S1".into(), Some("resume".into()), None);
        let recorded = state.recorded_socket_messages();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0],
            RecordedSocketMessage::SessionUpdate {
                pane_id: "P1".into(),
                session_id: "S1".into(),
                source: Some("resume".into()),
                cwd: None,
            }
        );
    }
}
