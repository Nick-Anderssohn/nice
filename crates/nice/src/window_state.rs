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

use std::sync::atomic::{AtomicU64, Ordering};

use nice_model::{SidebarMode, SidebarModel, SidebarTabSelection, TabModel};

use crate::control_socket::{NiceControlSocket, Reply, RecordedSocketMessage, SocketMessage};
use crate::pane_strip_actions::{ModelPaneStripActions, PaneStripActions};
use crate::session_manager::SessionManager;
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
        }
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

    /// The R14 control-socket routing point (the Rust mirror of Swift
    /// `SessionsModel.startSocketListener`'s handler dispatch,
    /// `SessionsModel.swift:257-309`): each [`SocketMessage`] variant is routed
    /// to a named window-local handler. The message enum + parser are finished
    /// business after R14 — R15/R16/R26 replace only the handler BODIES below,
    /// never this routing shape. Called on the gpui foreground by the socket
    /// drain task (wired by the R14 env-injection slice's `open_managed_window`).
    pub(crate) fn route_socket_message(&mut self, msg: SocketMessage) {
        match msg {
            SocketMessage::Claude {
                cwd,
                args,
                tab_id,
                pane_id,
                reply,
            } => self.handle_claude_socket_request(cwd, args, tab_id, pane_id, reply),
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

    /// `claude` action stub. R14 replies the bare `inplace` line: with `sid=""`
    /// and `settings=""` the frozen wrapper runs `exec command claude "$@"` —
    /// claude launches in-pane with the user's args, no error, no model mutation.
    /// (A `newtab` stub would swallow the invocation; a decision requires the
    /// promotion logic.)
    ///
    /// R15 replaces this body with the newtab/inplace promotion + session-id
    /// minting decision (`SessionsModel.handleClaudeSocketRequest`).
    fn handle_claude_socket_request(
        &mut self,
        cwd: String,
        args: Vec<String>,
        tab_id: String,
        pane_id: String,
        reply: Reply,
    ) {
        self.record_socket_message(RecordedSocketMessage::Claude {
            cwd,
            args,
            tab_id,
            pane_id,
        });
        reply.send("inplace");
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
    use crate::control_socket::{Reply, RecordedSocketMessage, SocketMessage};
    use nice_model::TabModel;
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
    fn claude_stub_replies_bare_inplace_and_records_message() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut state = WindowState::new("/home/u");
        state.route_socket_message(SocketMessage::Claude {
            cwd: "/tmp/x".into(),
            args: vec!["--resume".into(), "abc-123".into()],
            tab_id: "t1".into(),
            pane_id: "p1".into(),
            reply: Reply::for_test(server),
        });
        // Frozen reply grammar: exactly the bare `inplace` line. The wrapper reads
        // `read -r mode sid settings` → mode=inplace, sid="", settings="" ⇒
        // `exec command claude "$@"`. Never a trailing field, never diagnostics.
        assert_eq!(read_reply(client), "inplace\n");

        let recorded = state.recorded_socket_messages();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0],
            RecordedSocketMessage::Claude {
                cwd: "/tmp/x".into(),
                args: vec!["--resume".into(), "abc-123".into()],
                tab_id: "t1".into(),
                pane_id: "p1".into(),
            }
        );
    }

    #[test]
    fn handoff_stub_replies_error_and_records_message() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut state = WindowState::new("/home/u");
        state.route_socket_message(SocketMessage::Handoff {
            cwd: "/tmp/work".into(),
            handoff_file: "/tmp/work/.claude/handoff/h.md".into(),
            instructions: String::new(),
            model: String::new(),
            effort: String::new(),
            tab_id: "t1".into(),
            pane_id: "p1".into(),
            reply: Reply::for_test(server),
        });
        // R14 stub: the installed helper's `error*` branch degrades gracefully.
        // R26 replaces the body with a nested-tab open + `ok` reply.
        assert_eq!(read_reply(client), "error: handoff is not supported yet\n");
        assert_eq!(state.recorded_socket_messages().len(), 1);
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
        // session_update is fire-and-forget — no Reply variant, so the routing
        // point just records the parsed, normalized message (R16 fills the body).
        state.route_socket_message(SocketMessage::SessionUpdate {
            pane_id: "P1".into(),
            session_id: "S1".into(),
            source: Some("resume".into()),
            cwd: None,
        });
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
