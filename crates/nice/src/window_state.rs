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

use gpui::{AnyWindowHandle, AppContext, Entity};
use nice_model::file_browser::FileBrowserStore;
use nice_model::{Pane, PaneKind, SidebarMode, SidebarModel, SidebarTabSelection, TabModel, TabStatus};
use nice_term_view::TerminalEvent;

use crate::confirmation_modal::ConfirmationModal;
use crate::restore::WindowSeed;
use crate::control_socket::{NiceControlSocket, Reply, RecordedSocketMessage, SocketMessage};
use crate::pane_strip_actions::{ModelPaneStripActions, PaneStripActions};
use crate::session_manager::{
    compose_claude_reply, mint_session_uuid, ClaudeReplyDecision, ClaudeSessionMode,
    ClaudeTabPlacement, DissolveTerminus, SessionManager,
};
use crate::sidebar_actions::{ModelSidebarActions, SidebarActions};

/// Mint a fresh window-session id — R18 (L2): a real lowercased UUIDv4 (reusing
/// R15's [`mint_session_uuid`], no `uuid` crate), so `WindowState::session_id`
/// **is** the persisted window id in `sessions.json`. Every fresh / ⌘N window
/// mints one here; a restored window reuses its saved id
/// ([`WindowState::with_seed`]). This retires the old `win-<seq>` stand-in —
/// the persisted id must be stable across relaunches and never collide with a
/// saved slot, so a monotonic per-process counter (which restarts at 1 every
/// launch) can't serve.
fn mint_session_id() -> String {
    mint_session_uuid()
}

/// A deferred `newtab` spawn request returned by
/// [`WindowState::resolve_claude_request`] — the `newtab` reply has already gone
/// out, and the gpui-context-carrying caller must build + spawn the Claude tab.
struct NewTabSpawn {
    cwd: String,
    args: Vec<String>,
}

/// The deferred-resume branch-parent spawn returned by the pure model half of a
/// `session_update` ([`WindowState::materialize_branch_parent`]). The
/// `insert_branch_parent` MODEL mutation has already landed (the sibling parent
/// tab + its `[Claude, Terminal 1]` panes are in the tree); the
/// gpui-context-carrying router still owes the parent's Claude-pane pty (a
/// `.resumeDeferred` login shell). Splitting the mutation from the spawn keeps
/// the rotation classification unit-testable without a gpui context — the mirror
/// of [`NewTabSpawn`].
struct BranchParentSpawn {
    /// The minted parent tab id (its Claude pane is `<tab_id>-claude`).
    tab_id: String,
    /// The minted Claude pane id to spawn the deferred-resume pty on.
    claude_pane_id: String,
    /// The parent's cwd — the PRE-rotation cwd (captured by `insert_branch_parent`
    /// before the caller's `update_tab_cwd` moves the originating tab).
    cwd: String,
    /// The pre-rotation session id the parent resumes (`claude --resume <id>`).
    old_session_id: String,
}

/// Outcome of the pure model half of a `session_update`
/// ([`WindowState::apply_session_update`]): whether any tab state actually
/// changed — the R18 save signal, Swift's `onSessionMutation`; nothing persists
/// yet — and, when the rotation classified as a `/branch`, the deferred-resume
/// [`BranchParentSpawn`] the router must fulfil with its gpui context.
#[derive(Default)]
struct SessionUpdateOutcome {
    did_mutate: bool,
    spawn: Option<BranchParentSpawn>,
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
    /// W5: the user explicitly closed this window (red traffic light / ⌘W). Set
    /// ONLY by the confirmed close path
    /// ([`set_user_initiated_close`](Self::set_user_initiated_close)); read by
    /// [`crate::window_registry::WindowRegistry::handle_window_closed`] to route
    /// the disk fate ([`crate::lifecycle::close_disposition`]) — Swift's
    /// `AppState.userInitiatedClose`. Default `false` (preserve is the safe
    /// failure mode).
    user_initiated_close: bool,
    /// W5: the confirmation dialog currently presented over this window, if any.
    /// [`crate::app_shell::AppShellView`] renders it while present; the confirm /
    /// cancel / Esc / click-away paths emit `DismissEvent`, which
    /// [`present_confirmation`](Self::present_confirmation)'s subscription clears.
    pending_modal: Option<gpui::Entity<ConfirmationModal>>,
    /// Holds the `DismissEvent` subscription for [`pending_modal`](Self::pending_modal)
    /// alive; dropped/replaced when a new modal is presented or the window tears
    /// down.
    modal_sub: Option<gpui::Subscription>,
    /// W6: the last on-screen frame captured for this window (Cocoa bottom-left
    /// screen points), read into [`persisted_snapshot`](Self::persisted_snapshot)
    /// so a saved window restores at its geometry. Updated by
    /// [`capture_frame`](Self::capture_frame) from the window's
    /// `observe_window_bounds` (skipped while fullscreen). `None` until the first
    /// bounds observation (⇒ default placement on restore).
    last_frame: Option<crate::session_store::PersistedFrame>,
    /// R19: the per-window file-browser state catalog (`Tab.id → FileBrowserState`),
    /// lazily populated when a tab first renders in files mode. In-memory only
    /// (never persisted). The [`FileBrowserView`](crate::file_browser::view::FileBrowserView)
    /// reads / mutates it through this handle; a dissolved tab's entry is dropped
    /// via [`prune_dissolved_file_browser_states`](Self::prune_dissolved_file_browser_states)
    /// off the session dissolve cascade.
    pub(crate) file_browser: FileBrowserStore,
    /// R21: this window's mounted pane-content host, stashed at
    /// [`crate::app::build_window_root`] so the process-level theme fan-out
    /// ([`crate::theme_settings::apply_theme_fanout`]) can reach every window's
    /// terminal panes: it walks [`crate::window_registry::WindowRegistry::all_states`]
    /// → each `WindowState` → this host → its cached `TerminalView`s, pushing the new
    /// colors through the boundary-legal setters (the `SessionThemeCache` analog).
    /// `None` on a `WindowState` never mounted by the shipped builder (unit tests /
    /// headless scenarios), so the fan-out simply skips it.
    pane_host: Option<gpui::Entity<crate::app_shell::PaneHostView>>,
}

/// Selftest instrumentation: a process-global count of demand-present kicks fired
/// by the confirmation-modal path ([`WindowState::present_kick_modal`] — one on
/// present, one on dismiss). The `persistence-restore` scenario reads it via
/// [`modal_present_kick_count`] to PIN that `present_confirmation` actually kicks
/// the window: the regression that made quit/close dialogs never paint on an
/// occluded window (a stopped CVDisplayLink where `cx.notify()` alone never
/// presents — `crate::platform` fact 1) was precisely that this kick was absent.
/// The frontmost self-test window can't reproduce the occluded *pixels*, but this
/// counter pins the mechanism deterministically (0 pre-fix → nonzero post-fix).
///
/// The counter is only ever *incremented* under the `selftest` feature (see
/// [`WindowState::present_kick_modal`]), so the shipped bundle pays no runtime
/// cost and it stays a constant 0 there. It is compiled unconditionally only so
/// the always-built scenario module can reference the reader.
static MODAL_PRESENT_KICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Reader for [`MODAL_PRESENT_KICKS`] — the running total of confirmation-modal
/// present-kicks fired this process (a constant 0 outside `selftest`). The
/// `persistence-restore` scenario samples deltas across present / dismiss to pin
/// the present-kick fix.
pub(crate) fn modal_present_kick_count() -> u64 {
    MODAL_PRESENT_KICKS.load(std::sync::atomic::Ordering::SeqCst)
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
            user_initiated_close: false,
            pending_modal: None,
            modal_sub: None,
            last_frame: None,
            // Per-tab file-browser states are created lazily on first files-mode
            // render, defaulting to dotfiles-hidden (the 2026-07-07 deviation from
            // Swift's cwd-aware `show_hidden` heuristic).
            file_browser: FileBrowserStore::new(),
            pane_host: None,
        }
    }

    /// Rebuild a window from a persisted seed — the L2/L3 restore constructor
    /// (Swift `WindowSession.restoreSavedWindow`, `:326-365`). Unlike
    /// [`with_model`](Self::with_model), which seeds a fresh Terminals+Main tree,
    /// this trusts the SAVED grouping: it builds the document from the hydrated
    /// projects via [`TabModel::from_parts`] (no fresh Terminals/Main), runs the
    /// same repair pass restore always does — `repair_project_structure()` then
    /// `prune_dangling_parent_references()` — then re-applies the saved active tab
    /// **iff it survived** the repairs (else the first navigable tab), and adopts
    /// the saved window id + collapsed-sidebar flag. The selection is re-seeded
    /// from the resolved active tab so the "selection ⊇ {active tab}" invariant
    /// holds from construction.
    ///
    /// No save fires here (the model carries no mutation observer yet — the save
    /// gate): the restore fan-out runs restore's single explicit save
    /// ([`save_to_store`](Self::save_to_store)) after the cwd-heal pass, matching
    /// Swift's "suppress saves during restore, then one save".
    pub(crate) fn with_seed(seed: WindowSeed) -> Self {
        let WindowSeed {
            window_id,
            projects,
            active_tab_id,
            sidebar_collapsed,
            sidebar_mode,
            ..
        } = seed;

        let mut model = TabModel::from_parts_std(projects, active_tab_id);
        // Restore repairs (trust the grouping, then fix structural drift):
        // re-pin project/tab shape, then drop parent links to tabs that didn't
        // survive.
        model.repair_project_structure();
        model.prune_dangling_parent_references();
        // Re-apply the saved active tab iff it still exists after the repairs,
        // else fall back to the first navigable tab (Swift re-applies `activeTabId`
        // only when the tab survived).
        let resolved_active = model
            .active_tab_id()
            .filter(|id| model.tab_for(id).is_some())
            .map(str::to_string)
            .or_else(|| model.navigable_sidebar_tab_ids().into_iter().next());
        if let Some(active) = resolved_active {
            model.select_tab(&active);
        }

        let mut state = Self::with_model(model);
        state.session_id = window_id;
        // R19: restore the saved sidebar mode (absent ⇒ Tabs — the pre-R19 / never-
        // toggled default).
        state.sidebar = SidebarModel::new(sidebar_collapsed, sidebar_mode.unwrap_or(SidebarMode::Tabs));
        // Re-seed the selection from the (possibly repair-shifted) active tab.
        state.selection.sync_active_tab_id(state.model.active_tab_id());
        state
    }

    /// Stash this window's handle (the shipped builder calls it at
    /// [`crate::app::build_window_root`]). Read by
    /// [`subscribe_spawned_panes`](Self::subscribe_spawned_panes)'s routed-exit
    /// terminus actuation.
    pub(crate) fn set_window_handle(&mut self, handle: AnyWindowHandle) {
        self.window_handle = Some(handle);
    }

    /// R21: stash this window's mounted pane host (the shipped builder calls it at
    /// [`crate::app::build_window_root`]) so the process theme fan-out can push
    /// recolors into its terminal panes.
    pub(crate) fn set_pane_host(
        &mut self,
        pane_host: gpui::Entity<crate::app_shell::PaneHostView>,
    ) {
        self.pane_host = Some(pane_host);
    }

    /// R21: this window's mounted pane host, if the shipped builder mounted one.
    /// [`crate::theme_settings::apply_theme_fanout`] reads it to reach the panes.
    pub(crate) fn pane_host(&self) -> Option<gpui::Entity<crate::app_shell::PaneHostView>> {
        self.pane_host.clone()
    }

    /// Mirror the model's active tab into the multi-selection — the Rust analog of
    /// Swift's single active-tab observer (`SidebarView.swift:75-77`). Keyboard tab
    /// cycling (`NextSidebarTab` / `PrevSidebarTab`) mutates only the model, so the
    /// selection's active mirror must be re-synced here or the previously-active row
    /// lingers in `selection` as a faint `SELECTED_DIM_FACTOR` highlight; mouse
    /// paths already sync inline via `route_click`. Keeps the "selection ⊇ {active
    /// tab}" invariant (`sync_active_tab_id` collapses when the new active tab is
    /// outside the set).
    pub(crate) fn sync_selection_to_active_tab(&mut self) {
        self.selection.sync_active_tab_id(self.model.active_tab_id());
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
                // R19: a routed pane-exit may have dissolved a tab — drop its
                // file-browser state (the pane-exit dissolve path, not covered by
                // the UI-close methods above).
                ws.prune_dissolved_file_browser_states();
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

    /// Fill R15's Claude theme-sync `--settings` provider (R17 slice 2). The
    /// shipped window builder (`crate::app::open_managed_window`) computes the value
    /// from the process gate (`ClaudeThemeSyncGate` →
    /// [`crate::claude_theme_sync::settings_path_for_gate`]) and sets it here before
    /// the Main pane forks, so a later Claude spawn/reply/prefill sees it. `None` ⇒
    /// Claude spawns get no `--settings` (sync off, or the gate unset under
    /// `run_selftest`). R21 re-sources this on live theme/toggle changes.
    pub(crate) fn set_claude_settings_path(&mut self, path: Option<String>) {
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
    /// a Claude pane, which needs a gpui context. The `handoff` sub-handler (R26)
    /// likewise takes `cx` — like the `claude` arm, it spawns a fresh Claude tab.
    /// `session_update`'s handler is context-free (the pure rotation flow),
    /// returning a deferred-resume [`BranchParentSpawn`] the router fulfils here
    /// with `cx` when the rotation was a `/branch` (R16).
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
            } => {
                if let Some(spawn) = self.handle_session_update(pane_id, session_id, source, cwd) {
                    self.spawn_branch_parent(spawn, cx);
                }
            }
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
                cx,
            ),
        }
        // R18 (post-gate save trigger): a socket-driven mutation (a `claude`
        // newtab / in-place promotion, or a `session_update` rotation) changed the
        // tab tree — schedule the debounced upsert (Swift's `onSessionMutation` →
        // `scheduleSaveCurrentWindow`). A no-op when no store Global is installed,
        // and the store coalesces a burst into one write. The gpui-free model's
        // `FnMut()` mutation observer cannot snapshot the whole window (it has no
        // `cx`), so the live save triggers hang off the mutation SITES — here for
        // the socket path, `observe_window_bounds` for frames, and the UI-close
        // methods for dissolves — each funnelling into `upsert(snapshot)` + debounce.
        self.save_to_store();
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

    /// `session_update` handler — the Rust twin of Swift
    /// `SessionsModel.handleClaudeSessionUpdate` (`SessionsModel.swift:946-963`).
    /// The SessionStart hook relays a pane's rotated session id / cwd; this records
    /// the normalized message, runs the pure rotation flow
    /// ([`apply_session_update`](Self::apply_session_update)), and returns the
    /// deferred-resume [`BranchParentSpawn`] for the router to fulfil with `cx`
    /// when the rotation classified as a `/branch`.
    ///
    /// Context-free itself (fire-and-forget: R14's transport dropped the client fd
    /// BEFORE dispatch, so the handler never replies). The gpui-context spawn lives
    /// in [`spawn_branch_parent`](Self::spawn_branch_parent) — the mirror of the
    /// `claude` handler's [`resolve_claude_request`](Self::resolve_claude_request) /
    /// spawn split.
    fn handle_session_update(
        &mut self,
        pane_id: String,
        session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    ) -> Option<BranchParentSpawn> {
        self.record_socket_message(RecordedSocketMessage::SessionUpdate {
            pane_id: pane_id.clone(),
            session_id: session_id.clone(),
            source: source.clone(),
            cwd: cwd.clone(),
        });
        self.apply_session_update(&pane_id, &session_id, source.as_deref(), cwd.as_deref())
            .spawn
    }

    /// The pure model half of a `session_update` — the rotation flow per the
    /// PROTECTED ordering (`SessionsModel.swift:946-963`), unit-testable without a
    /// gpui context. Resolve the owning tab by pane (stale/unknown pane ⇒ silent
    /// no-op) → capture `old_id` → `update_claude_session_id` (equality
    /// short-circuit: a redundant forward mutates nothing) → **iff
    /// `source == "resume"` && `old_id` exists && `old_id != session_id`:
    /// materialize the branch parent, BEFORE the cwd update** (so the sibling
    /// inherits the pre-rotation cwd) → `update_tab_cwd` (None/empty filtered).
    /// An unknown/absent source with an id change is a plain id update, NEVER a
    /// parent (deliberately miss an occasional `/branch` rather than spawn a
    /// phantom parent from a mis-classified `/clear`).
    ///
    /// Returns whether anything changed (the R18 save signal — `onSessionMutation`;
    /// nothing persists yet) plus the deferred-resume spawn the caller owes.
    fn apply_session_update(
        &mut self,
        pane_id: &str,
        session_id: &str,
        source: Option<&str>,
        cwd: Option<&str>,
    ) -> SessionUpdateOutcome {
        let Some(tab_id) = self.model.tab_id_owning(pane_id) else {
            return SessionUpdateOutcome::default();
        };
        let old_id = self
            .model
            .tab_for(&tab_id)
            .and_then(|t| t.claude_session_id.clone());
        let id_changed = self.update_claude_session_id(&tab_id, session_id);
        // /branch classification: a `resume` source with an ACTUAL id change is the
        // signature of `/branch` and `--fork-session`. Real `/resume` keeps the id
        // stable (absorbed by the short-circuit above), `/clear` reports
        // `source == "clear"`, and a nil/unknown source is treated as a plain id
        // update. Materialize BEFORE the cwd update so the sibling parent inherits
        // the pre-rotation cwd.
        let spawn = if source == Some("resume") {
            match &old_id {
                Some(old) if old != session_id => self.materialize_branch_parent(&tab_id, old),
                _ => None,
            }
        } else {
            None
        };
        // Apply cwd to the ORIGINATING tab only — after branch materialization, so
        // the sibling parent keeps the pre-rotation cwd.
        let cwd_changed = self.update_tab_cwd(&tab_id, cwd);
        SessionUpdateOutcome {
            did_mutate: id_changed || spawn.is_some() || cwd_changed,
            spawn,
        }
    }

    /// Update `tab.claude_session_id` when Claude rotates its session mid-process
    /// (`SessionsModel.swift:972-984`). Equality short-circuit: a redundant forward
    /// (the hook fires on every SessionStart — this cheapness contract keeps a
    /// steady stream of identical ids from churning the save layer) mutates
    /// nothing. Returns whether the id actually changed.
    ///
    /// R18: the real `onSessionMutation` save flush hangs off this `true` return;
    /// nothing persists yet (the outcome's `did_mutate` is the standin the tests
    /// assert on).
    fn update_claude_session_id(&mut self, tab_id: &str, session_id: &str) -> bool {
        let mut changed = false;
        self.model.mutate_tab(tab_id, |tab| {
            if tab.claude_session_id.as_deref() != Some(session_id) {
                tab.claude_session_id = Some(session_id.to_string());
                changed = true;
            }
        });
        changed
    }

    /// Adopt Claude's reported cwd onto the originating tab (`SessionsModel.swift:
    /// 1001-1009`): the None/empty shapes short-circuit (an older hook payload
    /// omitting cwd, or a defensive empty string), else the actual mutation +
    /// per-pane follow policy lives on [`TabModel::adopt_tab_cwd`]. Returns whether
    /// anything changed (the R18 save signal — nothing persists yet).
    fn update_tab_cwd(&mut self, tab_id: &str, cwd: Option<&str>) -> bool {
        match cwd {
            Some(c) if !c.is_empty() => self.model.adopt_tab_cwd(tab_id, c),
            _ => false,
        }
    }

    /// Materialize the pre-`/branch` session as a sibling parent tab pinned to
    /// `old_session_id`, inserted immediately above the originating tab
    /// (`SessionsModel.swift:1031-1065`). Composes landed pieces: mint the tab +
    /// `-claude`/`-t1` pane ids, hand the model the tree mutation
    /// ([`TabModel::insert_branch_parent`], which refuses a Terminals/unknown
    /// originating tab ⇒ `None`, and does the depth-1 root-promotion re-parenting),
    /// then return the deferred-resume spawn the caller owes.
    ///
    /// The parent's cwd is read from the returned-by-value node HERE, before the
    /// caller's [`update_tab_cwd`](Self::update_tab_cwd) moves the originating tab
    /// into the post-rotation worktree: `insert_branch_parent` copied
    /// `originating.cwd` at insertion, so `parent.cwd` is the PRE-rotation cwd —
    /// which is what the sibling's `claude --resume <old id>` needs (the old-id
    /// transcript is bucketed under the pre-rotation path). Rust's by-value return
    /// makes the ordering structural; the ported cwd-move test pins it anyway.
    fn materialize_branch_parent(
        &mut self,
        originating_tab_id: &str,
        old_session_id: &str,
    ) -> Option<BranchParentSpawn> {
        let new_id = self.session.mint_tab_id("t");
        let claude_pane_id = format!("{new_id}-claude");
        let terminal_pane_id = format!("{new_id}-t1");
        let parent = self.model.insert_branch_parent(
            originating_tab_id,
            &new_id,
            &claude_pane_id,
            &terminal_pane_id,
            old_session_id,
        )?;
        Some(BranchParentSpawn {
            tab_id: new_id,
            claude_pane_id,
            cwd: parent.cwd,
            old_session_id: old_session_id.to_string(),
        })
    }

    /// Fulfil a [`BranchParentSpawn`]: register the parent's (empty) session
    /// container so its deferred companion's later
    /// [`ensure_active_pane_spawned`](crate::session_manager::SessionManager::ensure_active_pane_spawned)
    /// precondition holds, then spawn the parent's Claude pane in
    /// [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred) mode — a plain login
    /// shell carrying `claude --resume <old id>` as `NICE_PREFILL_COMMAND` (nothing
    /// resumes, and no tokens are spent, until the user opens the parent tab and
    /// presses Enter). Fire-and-forget: a spawn failure degrades to a model-only
    /// recovery tab (the tree mutation already landed), so it is logged-and-swallowed
    /// like the rest of the rotation feature.
    fn spawn_branch_parent(&mut self, spawn: BranchParentSpawn, cx: &mut gpui::Context<WindowState>) {
        self.session.register_tab_session(&spawn.tab_id);
        let settings = self.claude_settings_path.clone();
        let _ = self.session.spawn_claude_pane(
            &spawn.tab_id,
            &spawn.claude_pane_id,
            &spawn.cwd,
            &ClaudeSessionMode::ResumeDeferred(spawn.old_session_id),
            &[],
            settings.as_deref(),
            cx,
        );
        // Re-render so the sidebar shows the new sibling parent + re-parented child.
        cx.notify();
    }

    /// Handle a `handoff` request from the `/nice-handoff` skill's helper — the
    /// Rust twin of Swift `SessionsModel.handleHandoffRequest`
    /// (`SessionsModel.swift:1108-1156`). Opens a fresh Claude tab pre-loaded with
    /// the handoff notes: nested one indent under the originating tab, or top-level
    /// on a resolution miss, and ALWAYS replies `ok` (D3).
    ///
    /// The originating tab is resolved exactly as the `claude` request does
    /// ([`resolve_claude_request`](Self::resolve_claude_request)): a non-empty id,
    /// NOT in the Terminals group, present in the model, AND owning the sending
    /// pane. A miss is NOT an error — a handoff from the Main Terminal (or a stale
    /// pane id) must still open a tab — so it falls back to a top-level insert
    /// (unlike the `claude` in-place-promotion path, where a miss opens a newtab
    /// too but never nests). Mirrors the `claude` arm's spawn shape (D6): borrow
    /// settings/model/session, build + spawn through
    /// [`create_handoff_tab`](crate::session_manager::SessionManager::create_handoff_tab),
    /// then `sync_active_tab_id` + `cx.notify()`.
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
        cx: &mut gpui::Context<WindowState>,
    ) {
        self.record_socket_message(RecordedSocketMessage::Handoff {
            cwd: cwd.clone(),
            handoff_file: handoff_file.clone(),
            instructions: instructions.clone(),
            model: model.clone(),
            effort: effort.clone(),
            tab_id: tab_id.clone(),
            pane_id: pane_id.clone(),
        });

        // Resolve the originating tab (owned clones so the immutable model borrow
        // ends before the mutable spawn borrow). A miss ⇒ `None` fields, which
        // steer the tab top-level (D3).
        let (originating_id, originating_title, spawn_cwd) = {
            let originating = if !tab_id.is_empty()
                && !self.model.is_terminals_project_tab(&tab_id)
            {
                self.model
                    .tab_for(&tab_id)
                    .filter(|t| t.panes.iter().any(|p| p.id == pane_id))
            } else {
                None
            };
            (
                originating.map(|t| t.id.clone()),
                originating.map(|t| t.title.clone()),
                // Prefer the resolved tab's live cwd (it may have moved into a
                // worktree); else the payload cwd.
                originating.map(|t| t.cwd.clone()).unwrap_or(cwd),
            )
        };

        let title = crate::session_manager::handoff_title(originating_title.as_deref());
        let prompt = crate::session_manager::handoff_prompt(&handoff_file, &instructions);
        // Nest under the RESOLVED originating tab, never the raw payload `tab_id`:
        // on a miss we pass "" so `insert_handoff_child` rejects it and the tab
        // opens top-level, keeping nesting coherent with the title/cwd (which
        // already key off the resolved tab).
        let under = originating_id.unwrap_or_default();

        let settings = self.claude_settings_path.clone();
        let model_doc = &mut self.model;
        let session = &mut self.session;
        let created = session.create_handoff_tab(
            model_doc,
            &under,
            &spawn_cwd,
            title,
            prompt,
            &model,
            &effort,
            settings.as_deref(),
            cx,
        );
        if created.is_some() {
            // Keep the "selection ⊇ {active tab}" invariant: the new tab is now
            // active. Re-render so the nested / top-level tab appears.
            self.selection.sync_active_tab_id(self.model.active_tab_id());
            cx.notify();
        }
        // The tab opened (nested or top-level) — ALWAYS reply `ok`. Swift's only
        // hard error ("no window") cannot occur for a live WindowState.
        reply.send("ok");
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

    // MARK: - W5 quit / window-close (R18)

    /// This window's live-pane counts `(claude, terminal)` — the quit / close
    /// confirmation counting rule ([`nice_model::TabModel::live_pane_counts`]).
    pub(crate) fn live_pane_counts(&self) -> (usize, usize) {
        self.model.live_pane_counts()
    }

    /// Whether the user explicitly closed this window (red button / ⌘W) — read by
    /// [`crate::window_registry::WindowRegistry::handle_window_closed`] to route
    /// the disk fate. Swift's `AppState.userInitiatedClose`.
    pub(crate) fn user_initiated_close(&self) -> bool {
        self.user_initiated_close
    }

    /// Flip the user-initiated-close flag (the confirmed red-button / ⌘W path, or
    /// the no-live-panes unconditional close). Only ever set to `true`; a window
    /// that stays open (Cancel) leaves it `false`.
    pub(crate) fn set_user_initiated_close(&mut self, value: bool) {
        self.user_initiated_close = value;
    }

    /// The persisted snapshot of this window for the session store — id from the
    /// window's [`session_id`](Self::session_id) (the persisted window id; a fresh
    /// / ⌘N window mints a UUID, a restored one keeps its saved id),
    /// `sidebar_collapsed` from the live sidebar, projects via
    /// [`nice_model::snapshot_projects`] (empty non-Terminals projects dropped),
    /// and the W6 [`last_frame`](Self::last_frame) captured from the bounds
    /// observer (`None` until the first observation ⇒ default placement).
    pub(crate) fn persisted_snapshot(&self) -> crate::session_store::PersistedWindow {
        crate::session_store::PersistedWindow {
            id: self.session_id.clone(),
            active_tab_id: self.model.active_tab_id().map(|s| s.to_string()),
            sidebar_collapsed: self.sidebar.collapsed(),
            // R19: persist the live sidebar mode so a restored window reopens in
            // the mode it was last in (absent on decode ⇒ Tabs).
            sidebar_mode: Some(self.sidebar.mode()),
            projects: nice_model::snapshot_projects(&self.model.projects),
            frame: self.last_frame.clone(),
        }
    }

    /// W6: capture `window`'s current on-screen frame (Cocoa points) into
    /// [`last_frame`](Self::last_frame), UNLESS it is fullscreen — Swift saved the
    /// fullscreen frame, a known wart we deliberately fix by skipping capture
    /// while `matches!(window.window_bounds(), WindowBounds::Fullscreen(_))`, so a
    /// window that quit fullscreen restores at its last windowed geometry. Called
    /// from the window's `observe_window_bounds` (move AND resize). Returns whether
    /// the frame changed (so the caller can skip a redundant save).
    pub(crate) fn capture_frame(&mut self, window: &gpui::Window) -> bool {
        if matches!(window.window_bounds(), gpui::WindowBounds::Fullscreen(_)) {
            return false;
        }
        let Some([x, y, width, height]) = crate::platform::window_screen_frame(window) else {
            return false;
        };
        let captured = crate::session_store::PersistedFrame { x, y, width, height };
        if self.last_frame.as_ref() == Some(&captured) {
            return false;
        }
        self.last_frame = Some(captured);
        true
    }

    /// The dissolve save hook (R18): snapshot this window into the session store
    /// (debounced upsert). A no-op when no store Global is installed (every test /
    /// non-restore scenario), so it is safe to call from every UI-close path.
    pub(crate) fn save_to_store(&self) {
        crate::session_store::upsert(self.persisted_snapshot());
    }

    /// Present a confirmation dialog over this window (the generic W5/R18 surface).
    /// Mints the [`ConfirmationModal`], subscribes to its `DismissEvent` (clearing
    /// [`pending_modal`](Self::pending_modal)), stashes it, and notifies so
    /// [`crate::app_shell::AppShellView`] renders it. `completion(confirmed, ..)`
    /// runs once before dismissal.
    ///
    /// Notifying is not enough on an occluded window: `cx.notify()` never PRESENTS
    /// while the CVDisplayLink is stopped (`crate::platform` fact 1), so the modal
    /// would grab focus but paint nothing — the app looks frozen (this is exactly
    /// how every quit/close silently died: an idle shell keeps a pane alive, so all
    /// three controls take the modal path). We therefore fire the same demand-present
    /// kick the terminal drain uses, both on present and on dismiss (so the backdrop
    /// clears too). See [`present_kick_modal`](Self::present_kick_modal).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn present_confirmation(
        &mut self,
        title: impl Into<gpui::SharedString>,
        message: impl Into<gpui::SharedString>,
        confirm_label: impl Into<gpui::SharedString>,
        cancel_label: impl Into<gpui::SharedString>,
        destructive_confirm: bool,
        completion: impl Fn(bool, &mut gpui::Window, &mut gpui::App) + 'static,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<WindowState>,
    ) {
        let modal = cx.new(|mcx| {
            ConfirmationModal::new(
                title,
                message,
                confirm_label,
                cancel_label,
                destructive_confirm,
                completion,
                window,
                mcx,
            )
        });
        // This window's backing NSView, captured now (while we hold `window`) so
        // the dismiss subscription — which has no `&mut Window` — can kick the same
        // view without a re-entrant `window.update`. The content view is stable for
        // the window's lifetime, and a dismiss can only fire while that window is
        // still alive (teardown drops the subscription instead of emitting), so the
        // captured pointer is valid at both present and dismiss time. Null on a
        // headless / not-yet-on-screen window, where `present_kick` is a no-op.
        let ns_view = crate::platform::ns_view_of(window);
        // Clear the pending modal when it dismisses (confirm / cancel / Esc /
        // click-away all emit DismissEvent). The stale subscription is dropped
        // when the next modal replaces it or the window tears down.
        let sub = cx.subscribe(
            &modal,
            move |ws, _modal, _event: &gpui::DismissEvent, cx| {
                ws.pending_modal = None;
                cx.notify();
                // `cx.notify()` alone never PRESENTS while this window's
                // CVDisplayLink is stopped (occluded window — see `crate::platform`),
                // so the backdrop/overlay would linger as a ghost on a
                // non-presenting window. Kick the NSView so the cleared modal paints
                // on the next CA commit regardless of link state.
                Self::present_kick_modal(ns_view);
            },
        );
        self.pending_modal = Some(modal);
        self.modal_sub = Some(sub);
        cx.notify();
        // Same present weakness in the other direction: on an occluded window the
        // freshly-stashed modal would grab keyboard focus but paint zero pixels
        // (the app looks dead — every quit/close funnels here because an idle shell
        // still counts as a live pane). The terminal drain carries this exact kick;
        // the modal has no RAF of its own, so fire it explicitly here.
        Self::present_kick_modal(ns_view);
    }

    /// Fire the demand-present kick on this window's backing `NSView` so a
    /// confirmation modal (and its later dismissal) paints even when the window's
    /// CVDisplayLink is stopped — `cx.notify()` alone never presents on an occluded
    /// window (`crate::platform` fact 1). The terminal drain uses the same kick
    /// (`crate::app::install_present_kick`). A null view (headless / no AppKit
    /// handle yet) is a safe no-op.
    ///
    /// Occlusion-gated inside `platform::present_kick` (r5d): on a VISIBLE
    /// window the `setNeedsDisplay` is skipped — the running display link
    /// presents the notify-dirtied modal on its next tick — so this path never
    /// feeds the `displayLayer:` link stop/recreate cycle behind the 2026-07-10
    /// presentation wedge. Occluded (the case this kick exists for) it fires
    /// exactly as before. [`MODAL_PRESENT_KICKS`] counts *calls into this
    /// path*, before the gate, so the `persistence-restore` pin (present +
    /// dismiss each kick once) is unaffected by the window's occlusion state.
    fn present_kick_modal(ns_view: *mut std::ffi::c_void) {
        // Selftest instrumentation (see `modal_present_kick_count`): count the kick
        // so the `persistence-restore` scenario can pin that this path fires it.
        #[cfg(feature = "selftest")]
        MODAL_PRESENT_KICKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: `ns_view` is this window's live content `NSView` (from
        // `platform::ns_view_of`) or null, which `present_kick` treats as a no-op.
        unsafe { crate::platform::present_kick(ns_view) };
    }

    /// The confirmation dialog currently presented over this window, if any —
    /// [`crate::app_shell::AppShellView`]'s render reads it.
    pub(crate) fn pending_modal(&self) -> Option<gpui::Entity<ConfirmationModal>> {
        self.pending_modal.clone()
    }

    /// R19: drop the file-browser state of every tab dissolved since the last
    /// drain (the session cascade records them; see
    /// [`SessionManager::take_dissolved_tab_ids`]). Called after every cascade so a
    /// long session doesn't accumulate stale per-tab browser states — the single
    /// removal path for [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore).
    fn prune_dissolved_file_browser_states(&mut self) {
        for tab_id in self.session.take_dissolved_tab_ids() {
            self.file_browser.remove_state(&tab_id);
        }
    }

    /// Real close of a tab through the session manager (pty release + dissolve
    /// cascade) — the shipped-path replacement for the model-only
    /// `SidebarActions::close_tab` stub. Returns the terminus the caller actuates
    /// via [`SessionManager::apply_dissolve_terminus`], and schedules the dissolve
    /// save.
    pub(crate) fn close_tab_via_session(&mut self, tab_id: &str) -> DissolveTerminus {
        let terminus = self
            .session
            .close_tab(&mut self.model, &mut self.selection, tab_id);
        self.prune_dissolved_file_browser_states();
        self.save_to_store();
        terminus
    }

    /// Real close of a batch of tabs (the "Close N Tabs" path). Aggregates each
    /// tab's terminus; schedules a single save at the end.
    pub(crate) fn close_tabs_via_session(&mut self, tab_ids: &[String]) -> DissolveTerminus {
        let mut terminus = DissolveTerminus::None;
        for id in tab_ids {
            terminus = terminus.or(self.session.close_tab(&mut self.model, &mut self.selection, id));
        }
        self.prune_dissolved_file_browser_states();
        self.save_to_store();
        terminus
    }

    /// Real close of a whole project (the sidebar "Close Project" path), porting
    /// Swift's `CloseRequestCoordinator.hardKillProject` (`:369-389`): the pinned
    /// Terminals group is never closed; an already-empty non-Terminals project row
    /// drops directly; otherwise the project is marked pending-removal and each of
    /// its tabs is hard-closed — the last dissolve drops the now-empty row
    /// ([`SessionManager::finalize_dissolved_tab`]). Schedules the dissolve save.
    pub(crate) fn close_project_via_session(&mut self, project_id: &str) -> DissolveTerminus {
        if project_id == TabModel::TERMINALS_PROJECT_ID {
            return DissolveTerminus::None;
        }
        let Some(pi) = self.model.projects.iter().position(|p| p.id == project_id) else {
            return DissolveTerminus::None;
        };
        let tab_ids: Vec<String> = self.model.projects[pi]
            .tabs
            .iter()
            .map(|t| t.id.clone())
            .collect();

        let terminus = if tab_ids.is_empty() {
            // Empty non-Terminals project: drop the row directly + reselect.
            self.model.projects.remove(pi);
            let active_gone = self
                .model
                .active_tab_id()
                .is_some_and(|a| self.model.tab_for(a).is_none());
            if active_gone {
                if let Some(first) = self.model.navigable_sidebar_tab_ids().into_iter().next() {
                    self.model.select_tab(&first);
                }
            }
            if self.model.projects.iter().all(|p| p.tabs.is_empty()) {
                DissolveTerminus::WindowEmptied
            } else {
                DissolveTerminus::None
            }
        } else {
            self.session.mark_project_pending_removal(project_id);
            let mut terminus = DissolveTerminus::None;
            for id in &tab_ids {
                terminus =
                    terminus.or(self.session.close_tab(&mut self.model, &mut self.selection, id));
            }
            terminus
        };
        self.prune_dissolved_file_browser_states();
        self.save_to_store();
        terminus
    }

    /// Real close of one pane on `tab_id` (the toolbar pill × path) — the
    /// shipped-path replacement for the model-only `PaneStripActions::close_pane`
    /// stub. A spawned pane routes through [`SessionManager::terminate_pane`]
    /// (SIGHUP→SIGKILL + model removal via `pane_exited`, dissolving the tab when
    /// it was the last pane); a model-only pane (a lazy companion never focused)
    /// is dropped from the model directly and the tab dissolved if it was the last
    /// — so the × is never dead. Returns the terminus; schedules the dissolve save.
    pub(crate) fn close_pane_via_session(&mut self, tab_id: &str, pane_id: &str) -> DissolveTerminus {
        let terminus = if self.session.pane_is_spawned(tab_id, pane_id) {
            self.session
                .terminate_pane(&mut self.model, &mut self.selection, tab_id, pane_id)
                .terminus
        } else {
            self.model.extract_pane(pane_id, tab_id);
            self.session
                .dissolve_tab_if_empty(&mut self.model, &mut self.selection, tab_id)
        };
        self.prune_dissolved_file_browser_states();
        self.save_to_store();
        terminus
    }

    // MARK: - R20.5 busy-close gates (CloseRequestCoordinator port)
    //
    // The three UI close affordances (toolbar pill ✕, sidebar "Close Tab"/"Close
    // N Tabs", sidebar "Close Project") route through these gates instead of
    // calling `close_*_via_session` directly. Each classifies the close scope's
    // panes as BUSY (D-BUSY) — an alive Claude that is thinking/waiting, or an
    // alive terminal whose shell has a foreground child — and then either presents
    // the R18 `ConfirmationModal` (`destructive_confirm = true`, "Force quit") in
    // front of the existing kill route, or, when nothing is busy, runs the kill
    // route immediately (exactly today's unconfirmed behavior, D0). This is a
    // DISTINCT system from R18's alive-pane quit/window-close confirmation (D0);
    // the two counters never chain.

    /// Whether one pane is BUSY (D-BUSY, ported 1:1 from Swift's `isBusy`,
    /// `CloseRequestCoordinator.swift:268-279`). Reads the terminal-foreground
    /// signal from the [`SessionManager`] seam (synthetic-first, else the
    /// `tcgetpgrp` probe) only for a `Terminal` pane, then defers to the pure
    /// [`pane_is_busy_with`](Self::pane_is_busy_with) core.
    fn pane_is_busy(&self, tab_id: &str, pane: &Pane, cx: &gpui::App) -> bool {
        // Short-circuit: only a Terminal pane consults the shell foreground signal
        // (the syscall is skipped entirely for Claude / dead panes).
        let terminal_has_foreground_child = matches!(pane.kind, PaneKind::Terminal)
            && self.session.shell_has_foreground_child(tab_id, &pane.id, cx);
        Self::pane_is_busy_with(pane, terminal_has_foreground_child)
    }

    /// The pure D-BUSY predicate given the terminal-foreground signal — the
    /// gpui-free core of [`pane_is_busy`](Self::pane_is_busy), unit-testable
    /// without a `SessionManager` / gpui `App`:
    /// 1. `!pane.is_alive` ⇒ **not busy** (a held/dead pane is never busy —
    ///    dead-first guard).
    /// 2. `Claude` ⇒ busy iff `status` is `Thinking`/`Waiting` (an idle Claude at
    ///    rest is disposable; read the PER-PANE status, not any tab aggregate).
    /// 3. `Terminal` ⇒ busy iff `terminal_has_foreground_child` (the caller's
    ///    `tcgetpgrp`/synthetic signal; a terminal pane's `status` is meaningless).
    fn pane_is_busy_with(pane: &Pane, terminal_has_foreground_child: bool) -> bool {
        if !pane.is_alive {
            return false;
        }
        match pane.kind {
            PaneKind::Claude => matches!(pane.status, TabStatus::Thinking | TabStatus::Waiting),
            PaneKind::Terminal => terminal_has_foreground_child,
        }
    }

    /// The [`describe`](crate::close_confirm::describe)d busy panes of `tab_id`, in
    /// pane order, honoring the `is_alive && isBusy` pre-filter (Swift
    /// `requestCloseTab` `:126-129`). Empty when the tab is absent or nothing is
    /// busy.
    fn busy_descriptions_in_tab(&self, tab_id: &str, cx: &gpui::App) -> Vec<String> {
        let Some(tab) = self.model.tab_for(tab_id) else {
            return Vec::new();
        };
        tab.panes
            .iter()
            .filter(|p| self.pane_is_busy(tab_id, p, cx))
            .map(crate::close_confirm::describe)
            .collect()
    }

    /// Prune + re-sync the selection against the surviving tabs after a close —
    /// the model/selection half of the sidebar handlers' post-close reconcile
    /// (formerly `SidebarShellView::reconcile_selection_after_close`). Runs in BOTH
    /// the idle-immediate and the confirm-completion paths (D9); a confirmed close
    /// that skipped it would strand a stale selection.
    pub(crate) fn reconcile_selection_after_close(&mut self) {
        let valid: HashSet<String> =
            self.model.navigable_sidebar_tab_ids().into_iter().collect();
        let active = self.model.active_tab_id().map(|s| s.to_string());
        self.selection.prune(&valid);
        self.selection.sync_active_tab_id(active.as_deref());
    }

    /// Gate the toolbar pill ✕ close of one pane (Swift `requestClosePane`
    /// `:104-117`). Busy ⇒ present the `.pane` modal; idle ⇒ close immediately.
    pub(crate) fn request_close_pane(
        &mut self,
        tab_id: &str,
        pane_id: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_pane({tab_id}, {pane_id}) ignored — a confirmation modal \
                 is already up"
            );
            return;
        }
        let busy_desc = self
            .model
            .tab_for(tab_id)
            .and_then(|t| t.panes.iter().find(|p| p.id == pane_id))
            .filter(|p| self.pane_is_busy(tab_id, p, cx))
            .map(crate::close_confirm::describe);
        match busy_desc {
            Some(desc) => {
                let message = crate::close_confirm::pane_message(&[desc]);
                let state = cx.entity();
                let tid = tab_id.to_string();
                let pid = pane_id.to_string();
                self.present_confirmation(
                    crate::close_confirm::TITLE,
                    message,
                    crate::close_confirm::CONFIRM_LABEL,
                    crate::close_confirm::CANCEL_LABEL,
                    true,
                    move |confirmed, window, app| {
                        if confirmed {
                            Self::commit_close_pane(&state, &tid, &pid, window, app);
                        }
                    },
                    window,
                    cx,
                );
            }
            None => {
                let terminus = self.close_pane_via_session(tab_id, pane_id);
                self.reconcile_selection_after_close();
                cx.notify();
                SessionManager::apply_dissolve_terminus(terminus, window, cx);
            }
        }
    }

    /// The confirmed-`.pane` completion: re-resolve by id (never a stale `Pane`,
    /// D2) and run the existing kill route + reconcile + terminus (D9).
    fn commit_close_pane(
        state: &Entity<Self>,
        tab_id: &str,
        pane_id: &str,
        window: &mut gpui::Window,
        app: &mut gpui::App,
    ) {
        let terminus = state.update(app, |ws, cx| {
            let terminus = ws.close_pane_via_session(tab_id, pane_id);
            ws.reconcile_selection_after_close();
            cx.notify();
            terminus
        });
        SessionManager::apply_dissolve_terminus(terminus, window, app);
    }

    /// Gate the sidebar "Close Tab" close of one tab (Swift `requestCloseTab`
    /// `:123-135`). Any alive busy pane ⇒ present the `.tab` modal; else close
    /// immediately.
    pub(crate) fn request_close_tab(
        &mut self,
        tab_id: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_tab({tab_id}) ignored — a confirmation modal is already up"
            );
            return;
        }
        let busy = self.busy_descriptions_in_tab(tab_id, cx);
        if busy.is_empty() {
            let terminus = self.close_tab_via_session(tab_id);
            self.reconcile_selection_after_close();
            cx.notify();
            SessionManager::apply_dissolve_terminus(terminus, window, cx);
            return;
        }
        let message = crate::close_confirm::tab_message(&busy);
        let state = cx.entity();
        let tid = tab_id.to_string();
        self.present_confirmation(
            crate::close_confirm::TITLE,
            message,
            crate::close_confirm::CONFIRM_LABEL,
            crate::close_confirm::CANCEL_LABEL,
            true,
            move |confirmed, window, app| {
                if confirmed {
                    Self::commit_close_tabs(&state, std::slice::from_ref(&tid), window, app);
                }
            },
            window,
            cx,
        );
    }

    /// Gate the sidebar project-context "Close Project" close (Swift
    /// `requestCloseProject` `:219-236`). The pinned Terminals group has no Close
    /// Project affordance and never presents a dialog (its kill route no-ops it,
    /// `close_project_via_session:1125`); otherwise any alive busy pane across the
    /// project's tabs ⇒ present the `.project` modal, else close immediately.
    pub(crate) fn request_close_project(
        &mut self,
        project_id: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_project({project_id}) ignored — a confirmation modal is \
                 already up"
            );
            return;
        }
        // The pinned Terminals group is never a Close-Project scope: don't present
        // a dialog for it (the kill route already guards it to a no-op).
        let busy = if project_id == TabModel::TERMINALS_PROJECT_ID {
            Vec::new()
        } else {
            self.model
                .projects
                .iter()
                .find(|p| p.id == project_id)
                .into_iter()
                .flat_map(|project| {
                    project.tabs.iter().flat_map(|t| {
                        t.panes
                            .iter()
                            .filter(|p| self.pane_is_busy(&t.id, p, cx))
                            .map(crate::close_confirm::describe)
                    })
                })
                .collect::<Vec<_>>()
        };
        if busy.is_empty() {
            let terminus = self.close_project_via_session(project_id);
            self.reconcile_selection_after_close();
            cx.notify();
            SessionManager::apply_dissolve_terminus(terminus, window, cx);
            return;
        }
        let message = crate::close_confirm::project_message(&busy);
        let state = cx.entity();
        let pid = project_id.to_string();
        self.present_confirmation(
            crate::close_confirm::TITLE,
            message,
            crate::close_confirm::CONFIRM_LABEL,
            crate::close_confirm::CANCEL_LABEL,
            true,
            move |confirmed, window, app| {
                if confirmed {
                    Self::commit_close_project(&state, &pid, window, app);
                }
            },
            window,
            cx,
        );
    }

    /// The confirmed-`.project` completion (D2/D9).
    fn commit_close_project(
        state: &Entity<Self>,
        project_id: &str,
        window: &mut gpui::Window,
        app: &mut gpui::App,
    ) {
        let terminus = state.update(app, |ws, cx| {
            let terminus = ws.close_project_via_session(project_id);
            ws.reconcile_selection_after_close();
            cx.notify();
            terminus
        });
        SessionManager::apply_dissolve_terminus(terminus, window, app);
    }

    /// Gate the sidebar "Close N Tabs" multi-select close — the partial-eager flow
    /// (Swift `requestCloseTabs` `:145-191`, D5/§T). A single id degrades to the
    /// `.tab` gate. Otherwise idle tabs are hard-killed NOW (rows vanish before any
    /// dialog); only busy survivors are gated behind ONE `.tabs` modal. On cancel
    /// the busy survivors stay ALIVE while the already-closed idle members stay
    /// CLOSED — a *partial* close, NOT a total no-op.
    pub(crate) fn request_close_tabs(
        &mut self,
        ids: &[String],
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // §T.1 — single id degrades to the identical `.tab` wording.
        if ids.len() == 1 {
            self.request_close_tab(&ids[0], window, cx);
            return;
        }
        // §T.2 — re-entrancy guard (D7).
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_tabs({} tabs) ignored — a confirmation modal is already up",
                ids.len()
            );
            return;
        }
        // §T.3 — classify each EXISTING id into idle vs busy.
        let TabsCloseSplit {
            idle_ids,
            busy_ids,
            busy_summaries,
        } = split_tabs_close_batch(ids, |id| {
            self.model
                .tab_for(id)
                .map(|t| (t.title.clone(), self.busy_descriptions_in_tab(id, cx)))
        });
        // §T.4 — eagerly close the idle tabs NOW (rows vanish immediately). Any
        // terminus is at most `WindowEmptied`, which can only fire when NO busy
        // survivors remain (they keep the window non-empty) — so actuating it is
        // safe in both branches below.
        let idle_terminus = if idle_ids.is_empty() {
            DissolveTerminus::None
        } else {
            let terminus = self.close_tabs_via_session(&idle_ids);
            self.reconcile_selection_after_close();
            cx.notify();
            terminus
        };
        // §T.5 — everything was idle and is gone.
        if busy_ids.is_empty() {
            SessionManager::apply_dissolve_terminus(idle_terminus, window, cx);
            return;
        }
        // Busy survivors remain: actuate the idle terminus (never `WindowEmptied`
        // here) then present ONE `.tabs` modal over the survivors.
        SessionManager::apply_dissolve_terminus(idle_terminus, window, cx);
        let message = crate::close_confirm::tabs_message(&busy_summaries);
        let state = cx.entity();
        self.present_confirmation(
            crate::close_confirm::TITLE,
            message,
            crate::close_confirm::CONFIRM_LABEL,
            crate::close_confirm::CANCEL_LABEL,
            true,
            move |confirmed, window, app| {
                if confirmed {
                    Self::commit_close_tabs(&state, &busy_ids, window, app);
                }
            },
            window,
            cx,
        );
    }

    /// The confirmed-`.tab`/`.tabs` completion: re-resolve by id and run the batch
    /// kill route + reconcile + terminus (D2/D9). Shared by the singular `.tab`
    /// gate (a one-element slice) and the `.tabs` multi-select gate.
    fn commit_close_tabs(
        state: &Entity<Self>,
        tab_ids: &[String],
        window: &mut gpui::Window,
        app: &mut gpui::App,
    ) {
        let terminus = state.update(app, |ws, cx| {
            let terminus = ws.close_tabs_via_session(tab_ids);
            ws.reconcile_selection_after_close();
            cx.notify();
            terminus
        });
        SessionManager::apply_dissolve_terminus(terminus, window, app);
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

/// The idle-vs-busy split of a `.tabs` (multi-select) close batch — the pure
/// result of [`split_tabs_close_batch`], consumed by
/// [`WindowState::request_close_tabs`] (§T.3).
#[derive(Debug, Default, PartialEq, Eq)]
struct TabsCloseSplit {
    /// Tabs with no alive busy pane — hard-killed eagerly, before any dialog.
    idle_ids: Vec<String>,
    /// Tabs with ≥1 alive busy pane — gated behind the one `.tabs` modal.
    busy_ids: Vec<String>,
    /// The busy tabs' `<Title> (<p1>, <p2>)` summaries, parallel to `busy_ids`.
    busy_summaries: Vec<String>,
}

/// Bucket a multi-select close batch into idle vs busy (§T.3), the pure core of
/// [`WindowState::request_close_tabs`] — gpui-free, so the bucketing is
/// unit-testable without a live window. `classify(id)` returns `None` for a
/// vanished id (skipped — Swift iterates the id list, `:177-183`), else
/// `Some((title, busy_descriptions))`: an empty description list means idle,
/// non-empty means busy (its summary is built from the title + descriptions).
fn split_tabs_close_batch(
    ids: &[String],
    mut classify: impl FnMut(&str) -> Option<(String, Vec<String>)>,
) -> TabsCloseSplit {
    let mut split = TabsCloseSplit::default();
    for id in ids {
        let Some((title, busy)) = classify(id) else {
            continue;
        };
        if busy.is_empty() {
            split.idle_ids.push(id.clone());
        } else {
            split.busy_ids.push(id.clone());
            split
                .busy_summaries
                .push(crate::close_confirm::busy_tab_summary(&title, &busy));
        }
    }
    split
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_socket::{Reply, RecordedSocketMessage};
    use nice_model::{Pane, PaneKind, Project, Tab, TabModel};
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

    // ---- W5 (R18) UI-close wiring + snapshot --------------------------------

    /// Seed a window whose model has the pinned Terminals group plus one
    /// non-Terminals project "proj" with two model-only terminal tabs.
    fn window_with_project() -> WindowState {
        let mut model = TabModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        for id in ["t-a", "t-b"] {
            let mut tab = Tab::new(id, id, "/home/u/proj");
            let pane = format!("{id}-p");
            tab.panes = vec![Pane::new(&pane, "Terminal 1", PaneKind::Terminal)];
            tab.active_pane_id = Some(pane);
            model.projects[pi].tabs.push(tab);
        }
        WindowState::with_model(model)
    }

    #[test]
    fn close_project_via_session_drops_the_project_row() {
        let mut ws = window_with_project();
        let terminus = ws.close_project_via_session("proj");
        assert!(
            ws.model.projects.iter().all(|p| p.id != "proj"),
            "Close Project drops the non-Terminals row once its tabs dissolve"
        );
        assert!(ws.model.tab_for("t-a").is_none());
        assert!(ws.model.tab_for("t-b").is_none());
        assert!(
            ws.model.projects.iter().any(|p| p.id == TabModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group is never closed"
        );
        // The Terminals group still has the Main tab, so the window isn't empty.
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn close_project_via_session_empty_project_drops_directly() {
        let mut model = TabModel::new("/home/u");
        model.ensure_project("empty", "Empty", "/home/u/empty");
        let mut ws = WindowState::with_model(model);

        let terminus = ws.close_project_via_session("empty");

        assert!(
            ws.model.projects.iter().all(|p| p.id != "empty"),
            "an already-empty non-Terminals project row drops directly"
        );
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn close_project_via_session_refuses_terminals_group() {
        let mut ws = WindowState::new("/home/u");
        let terminus = ws.close_project_via_session(TabModel::TERMINALS_PROJECT_ID);
        assert!(
            ws.model.projects.iter().any(|p| p.id == TabModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group can never be closed"
        );
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn close_tab_via_session_dissolves_a_model_only_tab() {
        let mut ws = window_with_project();
        ws.close_tab_via_session("t-a");
        assert!(ws.model.tab_for("t-a").is_none(), "the model-only tab dissolves");
        assert!(ws.model.tab_for("t-b").is_some(), "its sibling survives");
        assert!(
            ws.model.projects.iter().any(|p| p.id == "proj"),
            "the project row survives while it still has a tab"
        );
    }

    #[test]
    fn close_pane_via_session_removes_a_model_only_pane() {
        // A tab with two model-only panes; closing one leaves the tab with one.
        let mut model = TabModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        let mut tab = Tab::new("t", "T", "/home/u/proj");
        tab.panes = vec![
            Pane::new("p1", "A", PaneKind::Terminal),
            Pane::new("p2", "B", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some("p1".into());
        model.projects[pi].tabs.push(tab);
        let mut ws = WindowState::with_model(model);

        let terminus = ws.close_pane_via_session("t", "p1");

        let tab = ws.model.tab_for("t").unwrap();
        assert_eq!(tab.panes.len(), 1, "the closed model-only pane is gone");
        assert_eq!(tab.panes[0].id, "p2");
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn persisted_snapshot_carries_id_sidebar_and_projects() {
        let mut ws = window_with_project();
        ws.sidebar.toggle_sidebar(); // expanded → collapsed
        let snap = ws.persisted_snapshot();
        assert_eq!(snap.id, ws.session_id());
        assert!(snap.sidebar_collapsed, "the live collapse state is captured");
        assert_eq!(
            snap.frame, None,
            "frame stays None until the window's bounds observer captures one (no window here)"
        );
        // The non-Terminals project + the pinned Terminals group both persist.
        assert!(snap.projects.iter().any(|p| p.id == "proj"));
        assert!(snap
            .projects
            .iter()
            .any(|p| p.id == TabModel::TERMINALS_PROJECT_ID));
    }

    #[test]
    fn user_initiated_close_flag_defaults_false_and_sets() {
        let mut ws = WindowState::new("/home/u");
        assert!(!ws.user_initiated_close(), "defaults false (preserve is safe)");
        ws.set_user_initiated_close(true);
        assert!(ws.user_initiated_close());
    }

    // ---- L2/L3 restore (with_model selection re-seed + with_seed) -----------

    fn terminal_tab(id: &str, cwd: &str) -> Tab {
        let mut tab = Tab::new(id, id, cwd);
        let pane = format!("{id}-p");
        tab.panes = vec![Pane::new(&pane, "Terminal 1", PaneKind::Terminal)];
        tab.active_pane_id = Some(pane);
        tab
    }

    #[test]
    fn with_model_reseeds_selection_from_non_default_active_tab() {
        // The R13.5 caveat made load-bearing by restore: a `WindowState` built
        // around a model whose active tab ISN'T the default Main must have its
        // multi-selection re-seeded from that active tab (else the sidebar shows
        // no selected row). Build a two-project model active on a non-Main tab.
        let mut model = TabModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        model.projects[pi].tabs.push(terminal_tab("t-x", "/home/u/proj"));
        model.select_tab("t-x");

        let ws = WindowState::with_model(model);
        assert_eq!(ws.model.active_tab_id(), Some("t-x"));
        assert!(
            ws.selection.contains("t-x"),
            "with_model must re-seed the selection from the model's active tab"
        );
        assert!(
            !ws.selection.contains(TabModel::MAIN_TERMINAL_TAB_ID),
            "the default Main tab is not selected when it isn't active"
        );
    }

    /// A hydrated seed: the pinned Terminals group (with Main) + a "proj" project
    /// carrying `t-a` and `t-b`, active on `t-b`, collapsed sidebar, saved id.
    fn restore_seed(window_id: &str, active: Option<&str>, collapsed: bool) -> WindowSeed {
        let terminals = Project {
            id: TabModel::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: "/home/u".into(),
            tabs: vec![terminal_tab(TabModel::MAIN_TERMINAL_TAB_ID, "/home/u")],
        };
        let proj = Project {
            id: "proj".into(),
            name: "Proj".into(),
            path: "/home/u/proj".into(),
            tabs: vec![
                terminal_tab("t-a", "/home/u/proj"),
                terminal_tab("t-b", "/home/u/proj"),
            ],
        };
        WindowSeed {
            window_id: window_id.into(),
            projects: vec![terminals, proj],
            active_tab_id: active.map(str::to_string),
            sidebar_collapsed: collapsed,
            sidebar_mode: None,
            frame: None,
        }
    }

    #[test]
    fn with_seed_adopts_id_collapse_and_rebuilds_saved_tree() {
        let ws = WindowState::with_seed(restore_seed("win-restored", Some("t-b"), true));
        // The saved window id is adopted verbatim (L2 identity), NOT a fresh mint.
        assert_eq!(ws.session_id(), "win-restored");
        assert!(ws.sidebar.collapsed(), "the saved collapse flag restores");
        // The saved grouping is trusted: proj + its two tabs + the Terminals group.
        assert!(ws.model.tab_for("t-a").is_some());
        assert!(ws.model.tab_for("t-b").is_some());
        assert!(ws.model.projects.iter().any(|p| p.id == "proj"));
        // The saved active tab is re-applied and the selection re-seeded from it.
        assert_eq!(ws.model.active_tab_id(), Some("t-b"));
        assert!(ws.selection.contains("t-b"));
    }

    #[test]
    fn with_seed_falls_back_to_first_navigable_when_active_absent() {
        // A saved active id that no longer resolves (e.g. its tab was pruned) ⇒
        // the first navigable tab (the Terminals Main tab) becomes active.
        let ws = WindowState::with_seed(restore_seed("w", Some("ghost-tab"), false));
        assert_eq!(
            ws.model.active_tab_id(),
            Some(TabModel::MAIN_TERMINAL_TAB_ID),
            "an unresolved saved active tab falls back to the first navigable tab"
        );
    }

    #[test]
    fn with_seed_prunes_dangling_parent_reference() {
        // A restored child tab whose parent didn't survive: the repair pass clears
        // the dangling parent link so the tab renders at root instead of orphaned.
        let mut seed = restore_seed("w", Some("t-a"), false);
        let pi = seed.projects.iter().position(|p| p.id == "proj").unwrap();
        let ti = seed.projects[pi].tabs.iter().position(|t| t.id == "t-a").unwrap();
        seed.projects[pi].tabs[ti].parent_tab_id = Some("never-existed".into());

        let ws = WindowState::with_seed(seed);
        let t_a = ws.model.tab_for("t-a").expect("t-a survives");
        assert_eq!(
            t_a.parent_tab_id, None,
            "prune_dangling_parent_references clears a link to a non-existent parent"
        );
    }

    // ---- R19: sidebar-mode persistence + file-browser dissolve cleanup ------

    #[test]
    fn persisted_snapshot_carries_sidebar_mode() {
        // R19: the live sidebar mode round-trips through the snapshot (Swift's
        // per-window SceneStorage mode). Fresh windows default to Tabs.
        let ws = window_with_project();
        assert_eq!(
            ws.persisted_snapshot().sidebar_mode,
            Some(SidebarMode::Tabs),
            "a fresh window snapshots the default Tabs mode"
        );
        let mut ws = window_with_project();
        ws.sidebar.toggle_sidebar_mode(); // Tabs → Files
        assert_eq!(
            ws.persisted_snapshot().sidebar_mode,
            Some(SidebarMode::Files),
            "toggling to files mode is captured in the snapshot"
        );
    }

    #[test]
    fn with_seed_restores_sidebar_mode_absent_defaults_tabs() {
        // R19: a saved Files mode restores; an absent field (pre-R19 save) ⇒ Tabs.
        let mut seed = restore_seed("w-files", Some("t-a"), false);
        seed.sidebar_mode = Some(SidebarMode::Files);
        let ws = WindowState::with_seed(seed);
        assert_eq!(ws.sidebar.mode(), SidebarMode::Files, "the saved mode restores");

        let ws = WindowState::with_seed(restore_seed("w-none", Some("t-a"), false));
        assert_eq!(
            ws.sidebar.mode(),
            SidebarMode::Tabs,
            "an absent sidebar_mode restores to Tabs (the pre-R19 default)"
        );
    }

    #[test]
    fn file_browser_state_dropped_on_tab_dissolve() {
        // R19: the dissolve cascade drops the closed tab's file-browser state (the
        // single removal path) so a long session doesn't leak per-tab states.
        let mut ws = window_with_project();
        ws.file_browser.ensure_state("t-a", "/home/u/proj");
        ws.file_browser.ensure_state("t-b", "/home/u/proj");
        assert!(ws.file_browser.state_for("t-a").is_some());

        ws.close_tab_via_session("t-a");

        assert!(
            ws.file_browser.state_for("t-a").is_none(),
            "the dissolved tab's file-browser state is dropped"
        );
        assert!(
            ws.file_browser.state_for("t-b").is_some(),
            "a surviving sibling keeps its file-browser state"
        );
    }

    #[test]
    fn with_seed_does_not_seed_a_second_terminals_main() {
        // from_parts must NOT inject a fresh Terminals+Main (that's `new`'s job) —
        // restore trusts the saved grouping, so there is exactly ONE Main tab.
        let ws = WindowState::with_seed(restore_seed("w", Some("t-a"), false));
        let mains = ws
            .model
            .projects
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.id == TabModel::MAIN_TERMINAL_TAB_ID)
            .count();
        assert_eq!(mains, 1, "restore rebuilds the saved tree, never re-seeds Main");
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

    // R26 replaced the R14 `handoff` stub (which replied
    // `error: handoff is not supported yet`) with a real handler that opens a
    // nested `[HANDOFF]` Claude tab and replies `ok`. The new body takes a gpui
    // `Context` (it spawns a Claude pane, like the `claude` arm), so it can no
    // longer be driven from a plain `#[test]` in this binary crate (which never
    // links gpui test-support) — its behavior (nested + top-level-fallback open,
    // the locked title, the `--session-id`/`--model`/`--effort`/prompt argv, and
    // the always-`ok` reply) is pinned end-to-end by the `handoff` self-test
    // scenario (`crate::handoff_live`), and its pure title/prompt/arg helpers are
    // unit-tested in `session_manager`.

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
    fn session_update_records_normalized_message_and_unknown_pane_is_no_op() {
        let mut state = WindowState::new("/home/u");
        // session_update is fire-and-forget and context-free — drive the sub-handler
        // directly. It records the parsed, normalized message, and an unknown pane id
        // (no tab owns "P1") classifies as a silent no-op ⇒ no branch-parent spawn.
        let spawn = state.handle_session_update("P1".into(), "S1".into(), Some("resume".into()), None);
        assert!(spawn.is_none(), "an unknown pane must not materialize a branch parent");
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

    // ---- R16 AppStateClaudeSessionUpdateTests + AppStateBranchTrackingTests -----
    //
    // Ported from Swift `AppStateClaudeSessionUpdateTests` (16) and
    // `AppStateBranchTrackingTests` (16). Each drives `apply_session_update` — the
    // pure model half of the `session_update` handler — so the rotation
    // classification + tree composition + cwd adoption are observable without a
    // gpui context (the deferred-resume SPAWN + the shipped-window end-to-end are
    // the `claude-lifecycle` scenario). `SessionUpdateOutcome::did_mutate` stands
    // in for Swift's `onSessionMutation` save signal (nothing persists until R18).
    //
    // R18: the two persistence round-trip cases in the Swift branch suite —
    // `test_persistedTab_parentTabId_roundTrips` and
    // `test_persistedTab_legacyJsonWithoutParentTabId_decodesAsNil` — are
    // PersistedTab JSON encode/decode tests. Their model half (`Tab::parent_tab_id`)
    // is landed and exercised by the branch cases below; the persisted-shape
    // round-trip lands with R18's session store.

    /// Seed a `[Claude, Terminal 1]` tab (Claude focused, NOT running — deferred /
    /// pre-promotion shape) into a fresh non-Terminals project `project_id` at
    /// `path`, with `session_id`. The Rust twin of `TabModelFixtures.seedClaudeTab`.
    /// The tab cwd + project path are both `path`. Claude pane `<tab>-claude`,
    /// terminal `<tab>-t1`.
    fn seed_rotation_tab(
        model: &mut TabModel,
        project_id: &str,
        tab_id: &str,
        session_id: &str,
        path: &str,
    ) {
        let claude_id = format!("{tab_id}-claude");
        let term_id = format!("{tab_id}-t1");
        let mut tab = Tab::new(tab_id, "New tab", path);
        tab.panes = vec![
            Pane::new(&claude_id, "Claude", PaneKind::Claude),
            Pane::new(&term_id, "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some(claude_id);
        tab.claude_session_id = Some(session_id.to_string());
        tab.next_terminal_index = 2;
        model.ensure_project(project_id, &project_id.to_uppercase(), path);
        let pi = model.projects.iter().position(|p| p.id == project_id).unwrap();
        model.projects[pi].tabs.push(tab);
    }

    /// The tabs of project `project_id`, cloned for post-mutation assertions.
    fn project_tabs(state: &WindowState, project_id: &str) -> Vec<Tab> {
        state
            .model
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.tabs.clone())
            .unwrap_or_default()
    }

    fn tab_session_id(state: &WindowState, tab_id: &str) -> Option<String> {
        state
            .model
            .tab_for(tab_id)
            .and_then(|t| t.claude_session_id.clone())
    }

    // === AppStateClaudeSessionUpdateTests =====================================

    #[test]
    fn session_update_unknown_pane_id_is_no_op() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/tmp/p");
        let out = state.apply_session_update("definitely-not-a-real-pane-id", "should-be-ignored", None, None);
        assert!(!out.did_mutate);
        assert_eq!(tab_session_id(&state, "t1").as_deref(), Some("S1"), "unknown pane must not mutate any tab");
    }

    #[test]
    fn session_update_updates_target_tab_when_multiple_projects_exist() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p1", "t1", "S1", "/tmp/p1");
        seed_rotation_tab(&mut state.model, "p2", "t2", "S2", "/tmp/p2");
        seed_rotation_tab(&mut state.model, "p3", "t3", "S3", "/tmp/p3");
        // Update the middle tab — the reverse scan must hit the right project even
        // when it is not first.
        state.apply_session_update("t2-claude", "S2-NEW", None, None);
        assert_eq!(tab_session_id(&state, "t1").as_deref(), Some("S1"));
        assert_eq!(tab_session_id(&state, "t2").as_deref(), Some("S2-NEW"));
        assert_eq!(tab_session_id(&state, "t3").as_deref(), Some("S3"));
    }

    #[test]
    fn session_update_resolves_by_pane_id_not_tab_id() {
        // Pane ids and tab ids are distinct namespaces; passing a tab id must not
        // match a tab (its pane list holds "t1-claude"/"t1-t1", not "t1").
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/tmp/p");
        let out = state.apply_session_update("t1", "should-not-apply", None, None);
        assert!(!out.did_mutate);
        assert_eq!(tab_session_id(&state, "t1").as_deref(), Some("S1"));
    }

    #[test]
    fn session_update_redundant_update_leaves_value_unchanged() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/tmp/p");
        let first = state.apply_session_update("t1-claude", "S1", None, None);
        let second = state.apply_session_update("t1-claude", "S1", None, None);
        assert!(!first.did_mutate, "same id must not mutate");
        assert!(!second.did_mutate, "the second redundant update must not mutate either");
        assert_eq!(tab_session_id(&state, "t1").as_deref(), Some("S1"));
    }

    #[test]
    fn session_update_new_session_id_replaces_old() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        let out = state.apply_session_update("t1-claude", "NEW", None, None);
        assert!(out.did_mutate);
        assert_eq!(tab_session_id(&state, "t1").as_deref(), Some("NEW"));
    }

    #[test]
    fn session_update_is_scoped_to_owning_window() {
        // Window A owns "tA-claude"; window B owns "tB-claude". A cross-window send
        // (A's handler receives B's pane) is a no-op on both — A's tab_id_owning
        // returns None, and nothing dispatched to B.
        let mut a = WindowState::new("/home/u");
        seed_rotation_tab(&mut a.model, "pA", "tA", "A-INIT", "/tmp/pA");
        let mut b = WindowState::new("/home/u");
        seed_rotation_tab(&mut b.model, "pB", "tB", "B-INIT", "/tmp/pB");

        a.apply_session_update("tB-claude", "LEAKED", None, None);
        assert_eq!(tab_session_id(&a, "tA").as_deref(), Some("A-INIT"), "A untouched by a B-shaped pane");
        assert_eq!(tab_session_id(&b, "tB").as_deref(), Some("B-INIT"), "B untouched until its own handler runs");

        b.apply_session_update("tB-claude", "B-NEW", None, None);
        assert_eq!(tab_session_id(&b, "tB").as_deref(), Some("B-NEW"));
        assert_eq!(tab_session_id(&a, "tA").as_deref(), Some("A-INIT"), "B's mutation must not bleed into A");
    }

    #[test]
    fn session_update_stale_pane_after_pane_exited_is_no_op() {
        // The hook fires asynchronously: a session_update can land after its pane
        // exited. The tab still exists (its terminal pane survives), but the pane id
        // no longer maps to it, so the id must not be mutated.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/tmp/p");
        // Baseline: a live update lands.
        state.apply_session_update("t1-claude", "S1-LIVE", None, None);
        assert_eq!(tab_session_id(&state, "t1").as_deref(), Some("S1-LIVE"));
        // The claude pane exits (model-only removal — no live pty needed).
        let (model, selection) = (&mut state.model, &mut state.selection);
        state.session.pane_exited(model, selection, "t1", "t1-claude");
        assert!(
            state.model.tab_for("t1").is_some_and(|t| !t.panes.iter().any(|p| p.id == "t1-claude")),
            "precondition: claude pane is gone after pane_exited"
        );
        // A late update for the now-defunct pane arrives.
        let out = state.apply_session_update("t1-claude", "S1-STALE", None, None);
        assert!(!out.did_mutate);
        assert_eq!(
            tab_session_id(&state, "t1").as_deref(),
            Some("S1-LIVE"),
            "stale pane id must not mutate the surviving tab"
        );
    }

    // -- cwd update path --------------------------------------------------------

    #[test]
    fn session_update_cwd_matching_current_is_no_op() {
        // Steady state: every SessionStart emits cwd even when nothing moved. The
        // matching-cwd + matching-id case must not churn (both branches short-circuit).
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let out = state.apply_session_update("t1-claude", "S1", Some("clear"), Some("/Users/nick/Projects/notes"));
        assert_eq!(state.model.tab_for("t1").map(|t| t.cwd.as_str()), Some("/Users/nick/Projects/notes"));
        assert!(!out.did_mutate, "matching cwd + matching id must not fire the save signal");
    }

    #[test]
    fn session_update_cwd_differing_updates_tab_and_claude_pane() {
        // The shape the feature fixes: bare `claude -w` lands in an auto-named
        // worktree; the hook forwards it and tab.cwd + the (nil-cwd) claude pane follow.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        let out = state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let tab = state.model.tab_for("t1").unwrap();
        assert_eq!(tab.cwd, worktree, "tab.cwd moves to the worktree");
        let claude = tab.panes.iter().find(|p| p.kind == PaneKind::Claude).unwrap();
        assert_eq!(claude.cwd.as_deref(), Some(worktree), "nil-cwd claude pane follows the tab");
        assert!(out.did_mutate, "cwd change must fire the save signal");
    }

    #[test]
    fn session_update_cwd_companion_terminal_follows_when_matching_old_cwd() {
        // A terminal companion still tracking the pre-update tab.cwd (not yet cd'd
        // via OSC 7) is pulled along so a later shell lands inside the worktree.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        state.model.mutate_tab("t1", |tab| {
            for pane in tab.panes.iter_mut().filter(|p| p.kind == PaneKind::Terminal) {
                pane.cwd = Some("/Users/nick/Projects/notes".into());
            }
        });
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let term = state.model.tab_for("t1").unwrap().panes.iter().find(|p| p.kind == PaneKind::Terminal).unwrap().cwd.clone();
        assert_eq!(term.as_deref(), Some(worktree), "companion matching the old cwd follows into the worktree");
    }

    #[test]
    fn session_update_cwd_companion_terminal_diverged_stays_put() {
        // A companion already tracking the user elsewhere via OSC 7 must not snap
        // back into the claude worktree.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let user_cd = "/Users/nick/Projects/notes/some/subdir";
        state.model.mutate_tab("t1", |tab| {
            for pane in tab.panes.iter_mut().filter(|p| p.kind == PaneKind::Terminal) {
                pane.cwd = Some(user_cd.into());
            }
        });
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let term = state.model.tab_for("t1").unwrap().panes.iter().find(|p| p.kind == PaneKind::Terminal).unwrap().cwd.clone();
        assert_eq!(term.as_deref(), Some(user_cd), "diverged OSC-7-tracked companion stays put");
    }

    #[test]
    fn session_update_cwd_nil_pane_cwd_follows_the_tab() {
        // A nil pane.cwd is "still following the tab" and inherits the new tab.cwd
        // (the rule that makes the always-nil-cwd claude pane track the worktree).
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        assert!(
            state.model.tab_for("t1").unwrap().panes.iter().find(|p| p.kind == PaneKind::Terminal).unwrap().cwd.is_none(),
            "precondition: terminal pane cwd starts nil"
        );
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let term = state.model.tab_for("t1").unwrap().panes.iter().find(|p| p.kind == PaneKind::Terminal).unwrap().cwd.clone();
        assert_eq!(term.as_deref(), Some(worktree), "nil pane cwd inherits the new tab.cwd");
    }

    #[test]
    fn session_update_cwd_nil_in_payload_is_no_op() {
        // An older hook script omits cwd; the socket normalizes it to None, and the
        // handler short-circuits without touching tab.cwd.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        state.apply_session_update("t1-claude", "S1", Some("clear"), None);
        assert_eq!(state.model.tab_for("t1").map(|t| t.cwd.as_str()), Some("/Users/nick/Projects/notes"));
    }

    #[test]
    fn session_update_cwd_empty_in_payload_is_no_op() {
        // Defense-in-depth: an empty-string cwd is treated as None even if the socket
        // layer regressed (the cwd field rode in from a user-modifiable hook script).
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        state.apply_session_update("t1-claude", "S1", Some("clear"), Some(""));
        assert_eq!(state.model.tab_for("t1").map(|t| t.cwd.as_str()), Some("/Users/nick/Projects/notes"));
    }

    #[test]
    fn session_update_cwd_identical_updates_mutate_exactly_once() {
        // Two same-cwd updates: only the first mutates; the second is already at the
        // target value and short-circuits in adopt_tab_cwd's change detection.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        let first = state.apply_session_update("t1-claude", "S1", Some("clear"), Some(worktree));
        let second = state.apply_session_update("t1-claude", "S1", Some("clear"), Some(worktree));
        assert!(first.did_mutate, "first update mutates");
        assert!(!second.did_mutate, "redundant identical update must not re-mutate");
    }

    // -- branch + cwd ordering (the pin) ---------------------------------------

    #[test]
    fn session_update_branch_rotation_with_cwd_move_sibling_inherits_old_cwd() {
        // `/branch` (resume + id-change) spawns a sibling parent pinned to the OLD
        // id. The pre-rotation transcript lives in the OLD bucket, so the sibling
        // must inherit the OLD cwd even though the originating tab moves. If the cwd
        // update ran before materialization, the sibling would pick up the
        // post-rotation worktree and its resume would point at the wrong bucket.
        let original_cwd = "/Users/nick/Projects/notes";
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD-ID", original_cwd);
        assert_eq!(state.model.tab_for("t1").map(|t| t.cwd.as_str()), Some(original_cwd));

        let new_cwd = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "NEW-ID", Some("resume"), Some(new_cwd));

        // The originating tab — post-rotation — sits in the worktree with the new id.
        let orig = state.model.tab_for("t1").unwrap();
        assert_eq!(orig.cwd, new_cwd, "originating tab reflects the post-rotation cwd");
        assert_eq!(orig.claude_session_id.as_deref(), Some("NEW-ID"));

        // The sibling parent — pinned to OLD-ID — holds the PRE-rotation cwd.
        let tabs = project_tabs(&state, "p");
        let sibling = tabs.iter().find(|t| t.claude_session_id.as_deref() == Some("OLD-ID"));
        let sibling = sibling.expect("branch rotation must materialize a sibling parent");
        assert_eq!(
            sibling.cwd, original_cwd,
            "sibling parent inherits the OLD cwd — its old-id transcript lives in the pre-rotation bucket"
        );
    }

    // === AppStateBranchTrackingTests ==========================================

    #[test]
    fn branch_resume_with_id_change_creates_parent_tab() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        state.model.mutate_tab("t1", |t| t.title = "wire up the foo".into());

        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);

        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 2, "branch adds exactly one sibling parent tab");
        // Parent inserted immediately above the originating tab: order reads [parent, child].
        let (parent, child) = (&tabs[0], &tabs[1]);
        assert_eq!(child.id, "t1", "originating tab keeps its id");
        assert_eq!(child.claude_session_id.as_deref(), Some("NEW"), "originating tab adopts the post-rotation id");
        assert_eq!(child.parent_tab_id.as_deref(), Some(parent.id.as_str()), "originating tab points at the new parent");
        assert_eq!(parent.claude_session_id.as_deref(), Some("OLD"), "parent pinned to the pre-rotation id");
        assert!(parent.parent_tab_id.is_none(), "parent stays at root");
        assert_eq!(parent.title, "wire up the foo", "parent inherits the title");
        assert_eq!(parent.cwd, child.cwd, "parent inherits the cwd");
        assert_eq!(parent.panes.len(), 2);
        assert!(parent.panes.iter().any(|p| p.kind == PaneKind::Claude), "parent has a claude pane");
        assert!(parent.panes.iter().any(|p| p.kind == PaneKind::Terminal), "parent has a companion terminal");
    }

    #[test]
    fn branch_clear_with_id_change_does_not_create_parent() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", Some("clear"), None);
        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 1, "/clear must not spawn a parent tab");
        assert_eq!(tabs[0].claude_session_id.as_deref(), Some("NEW"), "/clear still updates the id in place");
        assert!(tabs[0].parent_tab_id.is_none());
    }

    #[test]
    fn branch_missing_source_does_not_create_parent() {
        // Older hook payloads (and any future Claude that drops `source`) surface as
        // None; the conservative no-parent path — rather miss a /branch than
        // misclassify a /clear.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", None, None);
        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 1, "missing source must not spawn a parent tab");
        assert_eq!(tabs[0].claude_session_id.as_deref(), Some("NEW"));
    }

    #[test]
    fn branch_resume_with_same_id_does_not_create_parent() {
        // A real `claude --resume <id>` keeps the id stable; the short-circuit
        // absorbs it and the id-change guard blocks the parent.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "SAME", "/tmp/p");
        state.apply_session_update("t1-claude", "SAME", Some("resume"), None);
        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 1, "resume without rotation must not spawn a parent tab");
        assert_eq!(tabs[0].claude_session_id.as_deref(), Some("SAME"));
    }

    #[test]
    fn branch_first_promotes_parent_to_root_and_originating_becomes_child() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S0", "/tmp/p");
        state.apply_session_update("t1-claude", "S1", Some("resume"), None);
        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 2);
        let (parent, originating) = (&tabs[0], &tabs[1]);
        assert!(parent.parent_tab_id.is_none(), "first parent becomes the lineage root");
        assert_eq!(originating.parent_tab_id.as_deref(), Some(parent.id.as_str()), "originating tab is a depth-1 child of the new root");
    }

    #[test]
    fn branch_second_adds_sibling_child_under_same_root() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S0", "/tmp/p");
        state.apply_session_update("t1-claude", "S1", Some("resume"), None);
        let root_id = project_tabs(&state, "p")[0].id.clone();
        state.apply_session_update("t1-claude", "S2", Some("resume"), None);

        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 3);
        let (root, second, originating) = (&tabs[0], &tabs[1], &tabs[2]);
        assert_eq!(root.id, root_id, "root never changes once established");
        assert_eq!(root.claude_session_id.as_deref(), Some("S0"), "root pins the very first pre-/branch session");
        assert!(root.parent_tab_id.is_none(), "root stays at depth 0");
        assert_eq!(originating.id, "t1");
        assert_eq!(originating.claude_session_id.as_deref(), Some("S2"), "originating carries the freshest id");
        assert_eq!(originating.parent_tab_id.as_deref(), Some(root_id.as_str()), "originating keeps pointing at the original root");
        assert_eq!(second.claude_session_id.as_deref(), Some("S1"), "second parent pins the id current right before the second /branch");
        assert_eq!(second.parent_tab_id.as_deref(), Some(root_id.as_str()), "second parent is a sibling under the same root");
    }

    #[test]
    fn branch_third_keeps_adding_siblings_under_same_root() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S0", "/tmp/p");
        for (i, new_session) in ["S1", "S2", "S3"].iter().enumerate() {
            state.apply_session_update("t1-claude", new_session, Some("resume"), None);
            assert_eq!(project_tabs(&state, "p").len(), i + 2, "each /branch adds one parent");
        }
        let tabs = project_tabs(&state, "p");
        let root = &tabs[0];
        assert!(root.parent_tab_id.is_none());
        assert_eq!(root.claude_session_id.as_deref(), Some("S0"));
        for tab in tabs.iter().skip(1) {
            assert_eq!(tab.parent_tab_id.as_deref(), Some(root.id.as_str()), "every non-root tab points at the original root");
        }
        assert_eq!(tabs.last().unwrap().id, "t1", "originating tab stays at the bottom in display order");
        assert_eq!(tabs.last().unwrap().claude_session_id.as_deref(), Some("S3"));
    }

    #[test]
    fn branch_closing_parent_clears_child_parent_tab_id() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);
        let parent = project_tabs(&state, "p")[0].clone();
        assert_eq!(project_tabs(&state, "p")[1].parent_tab_id.as_deref(), Some(parent.id.as_str()), "precondition: child points at parent");

        // Dissolve the parent by exiting all its panes (model-level cascade).
        for pane_id in parent.panes.iter().map(|p| p.id.clone()) {
            let (model, selection) = (&mut state.model, &mut state.selection);
            state.session.pane_exited(model, selection, &parent.id, &pane_id);
        }
        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 1, "parent is gone after its panes all exited");
        assert_eq!(tabs[0].id, "t1");
        assert!(tabs[0].parent_tab_id.is_none(), "child's parent_tab_id is cleared when parent dissolves");
    }

    #[test]
    fn branch_closing_child_does_not_mutate_parent() {
        // The dangling-pointer sweep only mutates tabs that pointed at the removed
        // id; closing a child (which nothing points at) leaves the parent's
        // parent_tab_id (None) exactly as it was.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);
        let parent = project_tabs(&state, "p")[0].clone();
        let child = project_tabs(&state, "p")[1].clone();
        assert!(parent.parent_tab_id.is_none(), "precondition: parent at root");
        assert_eq!(child.parent_tab_id.as_deref(), Some(parent.id.as_str()), "precondition: child under parent");

        for pane_id in child.panes.iter().map(|p| p.id.clone()) {
            let (model, selection) = (&mut state.model, &mut state.selection);
            state.session.pane_exited(model, selection, &child.id, &pane_id);
        }
        let tabs = project_tabs(&state, "p");
        assert_eq!(tabs.len(), 1, "child is gone, parent remains");
        assert_eq!(tabs[0].id, parent.id);
        assert!(tabs[0].parent_tab_id.is_none(), "parent's parent_tab_id must NOT be cleared when an unrelated child closes");
    }

    #[test]
    fn branch_materialization_is_scoped_to_owning_window() {
        // A /branch-shaped signal addressed to B's pane, dispatched into A, is a
        // no-op on both — A's tab_id_owning returns None.
        let mut a = WindowState::new("/home/u");
        seed_rotation_tab(&mut a.model, "pA", "tA", "A0", "/tmp/pA");
        let mut b = WindowState::new("/home/u");
        seed_rotation_tab(&mut b.model, "pB", "tB", "B0", "/tmp/pB");

        a.apply_session_update("tB-claude", "B-LEAKED", Some("resume"), None);
        assert_eq!(project_tabs(&a, "pA").len(), 1, "A must not materialize a parent for a B-shaped pane");
        assert_eq!(tab_session_id(&a, "tA").as_deref(), Some("A0"));
        assert_eq!(project_tabs(&b, "pB").len(), 1, "B untouched — A was the dispatch target");
        assert_eq!(tab_session_id(&b, "tB").as_deref(), Some("B0"));

        // B's own handler DOES materialize a parent (proves scoping wasn't a false negative).
        b.apply_session_update("tB-claude", "B1", Some("resume"), None);
        assert_eq!(project_tabs(&b, "pB").len(), 2, "B's own /branch materializes a parent in B");
        assert_eq!(project_tabs(&a, "pA").len(), 1, "B's /branch must not bleed a parent into A");
        assert_eq!(tab_session_id(&a, "tA").as_deref(), Some("A0"));
    }

    #[test]
    fn branch_on_root_preserves_depth1_by_reparenting_former_children() {
        // /branch on a lineage root: the new parent becomes the new root, the old
        // root slides to a depth-1 child, AND every former child of the old root is
        // re-parented to the new root (otherwise they'd become depth-2).
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "S0", "/tmp/p");
        state.apply_session_update("t1-claude", "S1", Some("resume"), None);
        let old_root = project_tabs(&state, "p")[0].clone();
        assert!(old_root.parent_tab_id.is_none(), "precondition: old root is the root");
        state.apply_session_update("t1-claude", "S2", Some("resume"), None);

        // /branch on the OLD ROOT. Its claude pane id and current session (S0).
        let old_root_claude = old_root.panes.iter().find(|p| p.kind == PaneKind::Claude).unwrap().id.clone();
        state.apply_session_update(&old_root_claude, "S0-PRIME", Some("resume"), None);

        let tabs = project_tabs(&state, "p");
        let roots: Vec<&Tab> = tabs.iter().filter(|t| t.parent_tab_id.is_none()).collect();
        assert_eq!(roots.len(), 1, "exactly one root remains in the lineage");
        let new_root = roots[0];
        assert_ne!(new_root.id, old_root.id, "old root is no longer at depth 0");
        for tab in tabs.iter().filter(|t| t.id != new_root.id) {
            assert_eq!(tab.parent_tab_id.as_deref(), Some(new_root.id.as_str()), "every non-root tab is re-parented to the new root");
        }
        assert_eq!(tabs.iter().find(|t| t.id == "t1").unwrap().claude_session_id.as_deref(), Some("S2"), "t1 untouched by the /branch on the root");
        assert_eq!(new_root.claude_session_id.as_deref(), Some("S0"), "new root pins the id current on old root right before its /branch");
        assert_eq!(tabs.iter().find(|t| t.id == old_root.id).unwrap().claude_session_id.as_deref(), Some("S0-PRIME"), "old root now holds its post-rotation id");
    }

    #[test]
    fn branch_on_nil_claude_session_id_is_no_op() {
        // A claude tab whose session id is None (claude not yet started): the
        // id-change guard requires a non-None old id, so the id is set in place but
        // no parent spawns.
        let mut state = WindowState::new("/home/u");
        state.model.ensure_project("p-nil", "P-NIL", "/tmp/p-nil");
        let mut tab = Tab::new("t-nil", "Pre-claude", "/tmp/p-nil");
        tab.panes = vec![
            Pane::new("t-nil-claude", "Claude", PaneKind::Claude),
            Pane::new("t-nil-t1", "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some("t-nil-claude".into());
        tab.claude_session_id = None;
        let pi = state.model.projects.iter().position(|p| p.id == "p-nil").unwrap();
        state.model.projects[pi].tabs.push(tab);

        state.apply_session_update("t-nil-claude", "FIRST", Some("resume"), None);
        let tabs = project_tabs(&state, "p-nil");
        assert_eq!(tabs.len(), 1, "no parent when the originating tab had no prior session id");
        assert_eq!(tabs[0].claude_session_id.as_deref(), Some("FIRST"), "id still set in place");
        assert!(tabs[0].parent_tab_id.is_none());
    }

    #[test]
    fn branch_signal_on_terminals_tab_is_no_op() {
        // The pinned Terminals group never hosts Claude; a resume+rotation addressed
        // to a Terminals pane must not materialize a parent (insert_branch_parent
        // refuses the Terminals project).
        let mut state = WindowState::new("/home/u");
        let terminals = TabModel::TERMINALS_PROJECT_ID;
        let before = project_tabs(&state, terminals).len();
        let main = TabModel::MAIN_TERMINAL_TAB_ID;
        let main_pane = state.model.tab_for(main).unwrap().panes[0].id.clone();
        // Give the Main tab a session id so the id-change guard would otherwise fire.
        state.model.mutate_tab(main, |t| t.claude_session_id = Some("OLD".into()));

        state.apply_session_update(&main_pane, "FRESH", Some("resume"), None);
        assert_eq!(project_tabs(&state, terminals).len(), before, "Terminals tab count must not change on a spurious branch signal");
    }

    #[test]
    fn branch_parent_pane_is_not_running_ignores_shell_osc_title() {
        // The materialized parent's claude pane is is_claude_running == false
        // (deferred resume). Its pty hosts a plain zsh whose theme OSC titles must
        // NOT clobber the parent's inherited title — the OSC gate drops the whole
        // Claude branch until the socket in-place promotion opens it.
        let mut state = WindowState::new("/home/u");
        seed_rotation_tab(&mut state.model, "p", "t1", "OLD", "/tmp/p");
        state.model.mutate_tab("t1", |t| t.title = "wire up the foo".into());
        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);

        let parent = project_tabs(&state, "p")[0].clone();
        let parent_claude = parent.panes.iter().find(|p| p.kind == PaneKind::Claude).unwrap().clone();
        assert!(!parent_claude.is_claude_running, "sanity: branch parent's claude pane is deferred");

        let model = &mut state.model;
        state.session.pane_title_changed(model, &parent.id, &parent_claude.id, "Nick@Nicks MacBook Air:~/Projects/nice");
        assert_eq!(
            project_tabs(&state, "p")[0].title, "wire up the foo",
            "branch parent's inherited title must survive its deferred-resume zsh's OSC titles"
        );
    }

    // ---- R17 SessionsModelClaudeThemeSyncTests + real-provider socket cases ----
    //
    // The R17 gate fills R15's `--settings` provider from a process-level bool
    // (default ON, read from CFPreferences at bootstrap — see
    // `crate::app::ClaudeThemeSyncGate`). These pin the GATING semantics (the gate's
    // Some/None mapping and its ensure-on-read side effect) and the six byte-level
    // ON/OFF × {exec, reply, prefill} results driven through R15's REAL composers
    // with the REAL provider value (not an arbitrary stub), plus the `-` placeholder
    // and the `--settings`-already-present suppression. Hermetic: the provider
    // resolves against a throwaway home, so no test touches the developer's real
    // `~/.nice`. // R21: live retheme / toggle fan-out re-sources this value.
    use crate::claude_theme_sync::settings_path_for_gate_in;
    use crate::session_manager::{build_claude_exec_command, build_claude_prefill_command};

    /// A throwaway home dir removed on drop (never the real `~/.nice` — hermeticity).
    struct ScratchHome(std::path::PathBuf);
    impl Drop for ScratchHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch_home() -> ScratchHome {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("r17-gate-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch home");
        ScratchHome(dir)
    }

    // ---- gating semantics ---------------------------------------------------

    /// Gate ON ⇒ `Some(pointer path)`, and reading it ENSURES the pointer file
    /// exists with the exact `custom:nice` bytes (Swift's ensure-on-read,
    /// `ClaudeThemeSync.swift:122-131`).
    #[test]
    fn gate_on_provider_is_ensure_on_read_pointer_path() {
        let home = scratch_home();
        let provider = settings_path_for_gate_in(true, &home.0).expect("gate on ⇒ Some");
        assert_eq!(
            std::path::PathBuf::from(&provider),
            crate::claude_theme_sync::theme_settings_path(&home.0)
        );
        let bytes = std::fs::read(&provider).expect("pointer file ensured on read");
        assert_eq!(bytes, b"{\n  \"theme\": \"custom:nice\"\n}");
    }

    /// Gate OFF ⇒ `None`, and nothing is written (no `~/.nice` under the home).
    #[test]
    fn gate_off_provider_is_none_and_writes_nothing() {
        let home = scratch_home();
        assert!(settings_path_for_gate_in(false, &home.0).is_none());
        assert!(
            !home.0.join(".nice").exists(),
            "OFF must not create the pointer dir"
        );
    }

    /// The gate's CFPreferences read falls back to the default when the key is
    /// absent (Swift `syncClaudeTheme` defaults ON). A random unset key is a
    /// side-effect-free read of the app domain.
    #[test]
    fn read_bool_pref_absent_key_returns_default() {
        assert!(crate::platform::read_bool_pref("nice_rs_r17_absent_key_xyz", true));
        assert!(!crate::platform::read_bool_pref("nice_rs_r17_absent_key_xyz", false));
    }

    /// The PRESENT-key branch (`exists != 0`) — the path a user's `defaults write
    /// dev.nickanderssohn.nice syncClaudeTheme -bool false` actually takes, and
    /// the branch `read_bool_pref_absent_key_returns_default` never reaches. A key
    /// SET in the app domain wins over the passed `default` in BOTH directions, so
    /// this pins `exists != 0` AND the `value != 0` mapping: were the FFI miswired
    /// (exists/value swapped, or the boolean inverted) the absent-key test would
    /// still pass while the escape hatch silently did nothing. Uses the own-domain
    /// `CFPreferencesSetAppValue` write side `disable_font_smoothing` relies on
    /// (this test binary's own `kCFPreferencesCurrentApplication` domain, never the
    /// app's), and removes the scratch key afterwards so the domain is left as found.
    #[test]
    fn read_bool_pref_present_key_overrides_default() {
        use core_foundation::base::TCFType;
        use core_foundation::boolean::CFBoolean;
        use core_foundation::string::CFString;
        use core_foundation_sys::preferences::{
            kCFPreferencesCurrentApplication, CFPreferencesAppSynchronize, CFPreferencesSetAppValue,
        };

        let key = "nice_rs_r17_present_key_probe";
        let cf_key = CFString::new(key);

        // Set the scratch key to a CFBoolean and flush it to the in-memory app
        // cache the reader consults (the same set+synchronize handshake
        // `disable_font_smoothing` uses so gpui's later same-process read sees it).
        // SAFETY: `cf_key` / the CFBoolean constant are live for each call;
        // `kCFPreferencesCurrentApplication` is a valid constant domain; the write
        // is in-process only, to this test binary's own domain.
        let set_bool = |v: bool| unsafe {
            let value = if v {
                CFBoolean::true_value()
            } else {
                CFBoolean::false_value()
            };
            CFPreferencesSetAppValue(
                cf_key.as_concrete_TypeRef(),
                value.as_CFTypeRef(),
                kCFPreferencesCurrentApplication,
            );
            CFPreferencesAppSynchronize(kCFPreferencesCurrentApplication);
        };

        // Present TRUE beats default=false (exists != 0 AND value != 0 => true).
        set_bool(true);
        assert!(
            crate::platform::read_bool_pref(key, false),
            "a present true key must override default=false"
        );

        // Present FALSE beats default=true (exists != 0 AND value == 0 => false) —
        // the `defaults write … syncClaudeTheme -bool false` escape-hatch path.
        set_bool(false);
        assert!(
            !crate::platform::read_bool_pref(key, true),
            "a present false key must override default=true"
        );

        // Remove the scratch key (a null value deletes it) so the run leaves the
        // domain as it found it.
        // SAFETY: same domain / key ref as above; a null value is the documented
        // delete sentinel for `CFPreferencesSetAppValue`.
        unsafe {
            CFPreferencesSetAppValue(
                cf_key.as_concrete_TypeRef(),
                std::ptr::null(),
                kCFPreferencesCurrentApplication,
            );
            CFPreferencesAppSynchronize(kCFPreferencesCurrentApplication);
        }
    }

    // ---- six byte-level ON/OFF × {exec, reply, prefill} (real composers) -----

    /// exec ON: the exec command splices `--settings '<real pointer>'` BEFORE
    /// `--session-id` — the flag order that keeps the UUID from being eaten.
    #[test]
    fn gate_on_exec_command_carries_real_settings_pointer() {
        let home = scratch_home();
        let provider = settings_path_for_gate_in(true, &home.0);
        let cmd = build_claude_exec_command(
            "/c",
            &ClaudeSessionMode::New("abc-123".into()),
            &[],
            false,
            provider.as_deref(),
        );
        let ptr = provider.unwrap();
        assert_eq!(cmd, format!("exec '/c' --settings '{ptr}' --session-id 'abc-123'"));
    }

    /// exec OFF: byte-identical to the settings-free exec form.
    #[test]
    fn gate_off_exec_command_is_settings_free() {
        let provider = settings_path_for_gate_in(false, std::path::Path::new("/unused"));
        let cmd = build_claude_exec_command(
            "/c",
            &ClaudeSessionMode::New("abc-123".into()),
            &[],
            false,
            provider.as_deref(),
        );
        assert_eq!(cmd, "exec '/c' --session-id 'abc-123'");
    }

    /// prefill ON: the deferred-resume prefill splices `--settings '<real ptr>'`
    /// before `--resume`.
    #[test]
    fn gate_on_prefill_carries_real_settings_pointer() {
        let home = scratch_home();
        let provider = settings_path_for_gate_in(true, &home.0);
        let line = build_claude_prefill_command(provider.as_deref(), "SID");
        let ptr = provider.unwrap();
        assert_eq!(line, format!("claude --settings '{ptr}' --resume SID"));
    }

    /// prefill OFF: byte-identical to the settings-free prefill form.
    #[test]
    fn gate_off_prefill_is_settings_free() {
        let provider = settings_path_for_gate_in(false, std::path::Path::new("/unused"));
        let line = build_claude_prefill_command(provider.as_deref(), "SID");
        assert_eq!(line, "claude --resume SID");
    }

    /// reply ON: an in-place promotion whose args already carry the session id
    /// replies `inplace - <real ptr>` — the `-` placeholder lets the pointer ride
    /// as the 3rd field. Driven through the REAL socket-request path
    /// (`resolve_claude_request` → `compose_claude_reply`) with the REAL provider.
    #[test]
    fn gate_on_reply_uses_dash_placeholder_and_real_pointer() {
        let home = scratch_home();
        let provider = settings_path_for_gate_in(true, &home.0);
        let ptr = provider.clone().unwrap();
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path(provider);
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            format!("inplace - {ptr}\n")
        );
    }

    /// reply OFF: the same promotion with the gate OFF replies the bare `inplace`
    /// — byte-identical to the pre-theming protocol.
    #[test]
    fn gate_off_reply_is_byte_identical() {
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path(settings_path_for_gate_in(
            false,
            std::path::Path::new("/unused"),
        ));
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            "inplace\n"
        );
    }

    /// suppression: gate ON but the client's args already carry `--settings` ⇒ the
    /// reply must NOT append a second pointer (Swift's
    /// `themeCache.syncClaudeTheme && !args.contains("--settings")`).
    #[test]
    fn gate_on_reply_suppresses_pointer_when_args_already_have_settings() {
        let home = scratch_home();
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path(settings_path_for_gate_in(true, &home.0));
        let (claude, _t) = seed_claude_tab(&mut state.model, "t1", "OLD", false);
        assert_eq!(
            drive_claude(
                &mut state,
                "/tmp/p",
                &["--settings", "/whatever.json", "--resume", "abc-123"],
                "t1",
                &claude
            ),
            "inplace\n"
        );
    }

    // MARK: - R20.5 busy classification (D-BUSY) + `.tabs` split bucketing
    //
    // The full `request_close_*` gates need a gpui `Window` + `Context` (the
    // `nice` binary links no gpui test-support), so these pin the two extracted
    // pure cores: the per-pane busy predicate and the multi-select split. The
    // busy→modal WIRING is covered end-to-end by the `close-confirmation` live
    // scenario; the terminal foreground-child seam by `session_manager` unit tests.

    fn claude_pane(id: &str, status: TabStatus) -> Pane {
        let mut p = Pane::new(id, "auth-refactor", PaneKind::Claude);
        p.status = status;
        p
    }

    #[test]
    fn busy_idle_claude_and_idle_shell_are_not_busy() {
        // The core parity assert (Swift `isBusy` `:268-279`): an idle Claude at
        // rest (the default pre-first-title state) is DISPOSABLE, and an idle shell
        // (no foreground child) is NOT busy — both close with no dialog.
        let idle_claude = claude_pane("c", TabStatus::Idle);
        assert!(
            !WindowState::pane_is_busy_with(&idle_claude, false),
            "an idle Claude is disposable, not busy"
        );
        let shell = Pane::new("t", "npm run dev", PaneKind::Terminal);
        assert!(
            !WindowState::pane_is_busy_with(&shell, false),
            "a shell with no foreground child is idle, not busy"
        );
    }

    #[test]
    fn busy_thinking_or_waiting_claude_is_busy() {
        for status in [TabStatus::Thinking, TabStatus::Waiting] {
            assert!(
                WindowState::pane_is_busy_with(&claude_pane("c", status), false),
                "a {status:?} Claude is busy"
            );
        }
    }

    #[test]
    fn busy_terminal_follows_the_foreground_child_signal() {
        let shell = Pane::new("t", "cat", PaneKind::Terminal);
        assert!(
            WindowState::pane_is_busy_with(&shell, true),
            "a shell WITH a foreground child is busy (the terminal arm follows the syscall/seam)"
        );
        assert!(
            !WindowState::pane_is_busy_with(&shell, false),
            "the same shell WITHOUT a foreground child is not busy"
        );
    }

    #[test]
    fn busy_dead_pane_is_never_busy_even_when_thinking() {
        // The dead-first guard (D-BUSY §1): a held/dead pane is never busy, even a
        // Claude frozen mid-`Thinking` or a terminal reporting a foreground child.
        let mut dead_claude = claude_pane("c", TabStatus::Thinking);
        dead_claude.is_alive = false;
        assert!(!WindowState::pane_is_busy_with(&dead_claude, false));
        let mut dead_shell = Pane::new("t", "cat", PaneKind::Terminal);
        dead_shell.is_alive = false;
        assert!(
            !WindowState::pane_is_busy_with(&dead_shell, true),
            "a dead shell is not busy even if a stale foreground-child signal is passed"
        );
    }

    #[test]
    fn split_tabs_buckets_idle_and_busy_and_builds_summaries() {
        // §T.3: idle tabs (empty busy list) bucket into `idle_ids`; busy tabs into
        // `busy_ids` with a `<Title> (<p1>, <p2>)` summary; a vanished id is skipped.
        let ids = vec![
            "idle-1".to_string(),
            "busy-1".to_string(),
            "gone".to_string(),
            "idle-2".to_string(),
        ];
        let split = split_tabs_close_batch(&ids, |id| match id {
            "idle-1" => Some(("Idle One".to_string(), vec![])),
            "idle-2" => Some(("Idle Two".to_string(), vec![])),
            "busy-1" => Some((
                "My Project".to_string(),
                vec!["Claude (auth-refactor)".to_string(), "npm run dev".to_string()],
            )),
            _ => None, // "gone" — a vanished id
        });
        assert_eq!(split.idle_ids, vec!["idle-1", "idle-2"]);
        assert_eq!(split.busy_ids, vec!["busy-1"]);
        assert_eq!(
            split.busy_summaries,
            vec!["My Project (Claude (auth-refactor), npm run dev)".to_string()],
            "the busy summary is the BusyTabEntry-style paren join"
        );
    }

    #[test]
    fn split_tabs_all_idle_yields_no_busy_survivors() {
        // Every member idle ⇒ the whole batch is eager-closed, nothing gated (§T.5).
        let ids = vec!["a".to_string(), "b".to_string()];
        let split = split_tabs_close_batch(&ids, |id| Some((id.to_string(), vec![])));
        assert_eq!(split.idle_ids, vec!["a", "b"]);
        assert!(split.busy_ids.is_empty());
        assert!(split.busy_summaries.is_empty());
    }
}
