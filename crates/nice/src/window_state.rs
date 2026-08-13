//! `WindowState` — the per-window composition root, the Rust mirror of Swift's
//! `AppState` (`Sources/Nice/State/AppState.swift:60-75`).
//!
//! Each Nice window owns exactly one `WindowState`, held as a `gpui::Entity`
//! (app-global) and tracked by [`crate::window_registry::WindowRegistry`]. It is
//! handed to the window as a **constructor argument** by the app's window
//! builder (`crate::app::build_window_root`) — the deliberate inversion of
//! Swift's `WindowGroup` token dance (plan DO-NOT-PORT): "which saved slot /
//! which adopted window does this window own" becomes a plain parameter. R18 will
//! pass restored state and R25 an adopted window through the same seam.
//!
//! ## Decomposition (mirrors `AppState`)
//!
//! `AppState` holds six sub-models; R12 carries the subset that exists now,
//! per the plan's "Per-window state struct" decision:
//!
//! * [`workspace`](WindowState::workspace) — the R8 `WorkspaceModel` document (projects / sessions
//!   / windows), the single source of truth for a window's session tree. Isolation
//!   between windows is exactly that each `WindowState` owns its own `WorkspaceModel`.
//! * [`sidebar`](WindowState::sidebar) — the R10 `SidebarModel` (collapse / mode
//!   / peek state).
//! * [`selection`](WindowState::selection) — the R10 `SidebarSessionSelection`
//!   (Finder-style multi-select), seeded so the "selection ⊇ {active session}"
//!   invariant holds from construction.
//! * [`sidebar_actions`](WindowState::sidebar_actions) /
//!   [`window_strip_actions`](WindowState::window_strip_actions) — the R10/R11
//!   create/close/select seams. Model-only today; R13 swaps the implementations
//!   for real sessions without touching callers.
//! * [`ptys`](WindowState::ptys) — the per-window
//!   [`PtyManager`](crate::pty_manager::PtyManager) (R13). Owns the
//!   window's live term-window ptys and routes their OSC title/cwd events into
//!   `workspace`; [`teardown`](WindowState::teardown) is the close hook that tears
//!   them down. R12 carried an empty placeholder here.
//!
//! `AppState`'s remaining sub-models (`sessions`, `closer`,
//! `fileExplorerOrchestrator`, `fileBrowserStore`) are deferred: sessions to R13,
//! the file explorer to R19. They land in later cycles behind the same struct.
//!
//! The fields carry `#![allow(dead_code)]`: R12 slice 1 establishes the state
//! container + window builder + registry; the *keymap* slice (R12 slice 2) is
//! the first live reader of `sidebar` / the action seams (routing ⌘B, ⌘T, the
//! window-step actions through them), and R13 reads the session slot. The shapes
//! below are exercised by this module's tests.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

/// The kitty CSI-u encoding of ⌘↩ (`Enter` = codepoint 13, modifier field
/// 1+super(8) = 9) — the exact bytes the terminal view forwarded for an unbound
/// ⌘↩ under `kitty_forwards_super` before `commandCompose` claimed the chord.
/// [`WindowState::dispatch_command_compose`] replays it on the gated-out branch
/// so kitty TUIs (Claude Code, vim with kitty protocol) observe no change.
const KITTY_CMD_ENTER: &[u8] = b"\x1b[13;9u";

/// Where a Command Compose dispatch routes — see
/// [`WindowState::compose_route`] for the truth table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeRoute {
    /// Idle interactive shell: write the compose trigger; the ZLE widget runs.
    Trigger,
    /// Busy window whose child forwards super chords: replay ⌘↩ verbatim.
    ForwardCmdEnter,
    /// No pty bytes at all (dead window / busy legacy-mode shell / Claude window).
    Noop,
}

/// One pane's inputs to the D-BUSY close gate — see
/// [`WindowState::window_is_busy`].
#[derive(Debug, Clone, Copy)]
struct PaneBusySignal {
    kind: TermWindowKind,
    /// The pane's pty AND its pill are both alive.
    alive: bool,
    /// The pane's own status (the Claude arm's signal).
    status: SessionStatus,
    /// The pane's own `tcgetpgrp`/synthetic answer (the shell arm's signal).
    has_foreground_child: bool,
}

use gpui::{AnyWindowHandle, AppContext, Entity, Subscription};
use nice_model::file_browser::FileBrowserStore;
use nice_model::{Session, TermWindow, TermWindowKind, KeyHintModel, SidebarMode, SidebarModel, SidebarSessionSelection, WorkspaceModel, SessionStatus};
use nice_term_view::TerminalEvent;

use crate::confirmation_modal::ConfirmationModal;
use crate::restore::WindowSeed;
use crate::control_socket::{NiceControlSocket, Reply, RecordedSocketMessage, SocketMessage};
use crate::window_strip_actions::{ModelWindowStripActions, WindowStripActions};
use crate::pty_manager::{
    compose_claude_reply, mint_session_uuid, ClaudeReplyDecision, ClaudeSessionMode,
    ClaudeSessionPlacement, ClaudeSessionSpec, DissolveTerminus, PtyManager,
};
use crate::sidebar_actions::{ModelSidebarActions, SidebarActions};

/// Mint a fresh window-session id — R18 (L2): a real lowercased UUIDv4 (reusing
/// R15's [`mint_session_uuid`], no `uuid` crate), so `WindowState::window_session_id`
/// **is** the persisted window id in `sessions.json`. Every fresh / ⌘N window
/// mints one here; a restored window reuses its saved id
/// ([`WindowState::with_seed`]). This retires the old `win-<seq>` stand-in —
/// the persisted id must be stable across relaunches and never collide with a
/// saved slot, so a monotonic per-process counter (which restarts at 1 every
/// launch) can't serve.
fn mint_window_session_id() -> String {
    mint_session_uuid()
}

/// A deferred `newtab` spawn request returned by
/// [`WindowState::resolve_claude_request`] — the `newtab` reply has already gone
/// out, and the gpui-context-carrying caller must build + spawn the Claude session.
struct NewSessionSpawn {
    cwd: String,
    /// The argv the new session's `claude` runs. Normally the client's args
    /// verbatim; Fix D rewrites them when the request names a background
    /// claude session (`--resume <hosted uuid>` ⇒ `attach <short id>` and back).
    args: Vec<String>,
    /// Which claude session that session runs — the newtab twin of the in-place
    /// reply's exec-time decision (see [`WindowState::plan_newtab_claude_exec`]).
    spec: ClaudeSessionSpec,
}

/// The deferred-resume branch-parent spawn returned by the pure model half of a
/// `session_update` ([`WindowState::materialize_branch_parent`]). The
/// `insert_branch_parent` MODEL mutation has already landed (the sibling parent
/// session + its `[Claude, Terminal 1]` windows are in the tree); the
/// gpui-context-carrying router still owes the parent's Claude-window pty (a
/// `.resumeDeferred` login shell). Splitting the mutation from the spawn keeps
/// the rotation classification unit-testable without a gpui context — the mirror
/// of [`NewSessionSpawn`].
struct BranchParentSpawn {
    /// The minted parent session id (its Claude window is `<session_id>-claude`).
    session_id: String,
    /// The minted Claude window id to spawn the deferred-resume pty on.
    claude_window_id: String,
    /// The parent's cwd — the PRE-rotation cwd (captured by `insert_branch_parent`
    /// before the caller's `update_session_cwd` moves the originating session).
    cwd: String,
    /// The pre-rotation session id the parent resumes (`claude --resume <id>`).
    old_session_id: String,
}

/// A `~/.claude/jobs/<first8>/` entry — the discriminator that tells a
/// daemon-hosted BACKGROUND fork apart from an in-window rotation. Both relay
/// `source: "fork"` since Claude Code 2.1.214, so the source alone cannot
/// separate them:
///
/// * `/fork` (since 2.1.212) copies the conversation into a detached background
///   session run by the Claude Code daemon. The daemon creates
///   `~/.claude/jobs/<first8(fork id)>/` (and copies the parent transcript into
///   `…/tmp/`) **before** spawning the fork child, so the directory is already
///   on disk when that child's SessionStart hook fires.
/// * `/branch` and `--fork-session` resumes rotate the FOREGROUND window's session
///   and never create a jobs entry.
///
/// Hence the directory's mere existence is the classification signal (a `Some`
/// at all), while every parsed field is optional: `state.json` may not have
/// landed yet when the hook fires, and an aborted fork never writes one. A
/// `Some` with all-`None` fields means "background fork, details not on disk
/// yet" — the deferred retry that fills them in is Fix B's job (next slice).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ForkJobInfo {
    /// `state.json`'s `sessionId` — the fork's own full uuid. Lets a caller
    /// guard against first-8 collisions by comparing it with the id it probed
    /// with (the id in hand is the truth; a mismatch means someone else's job).
    pub(crate) claude_session_id: Option<String>,
    /// `state.json`'s `forkParentSessionId` — the conversation this fork
    /// branched from. The key the parent sidebar session is resolved by (Fix B).
    pub(crate) fork_parent_session_id: Option<String>,
    /// `state.json`'s `name` — the job's human label (it carries the `⑂`
    /// marker), the fork session's title when present.
    pub(crate) name: Option<String>,
}

/// How long [`WindowState::materialize_background_fork`] waits between re-probes
/// while a background fork's `state.json` has not landed yet.
const FORK_STATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// How many times that materialization re-probes before giving up SILENTLY.
/// 20 × 500 ms ≈ 10 s — the top of the plan's 5–10 s window, and comfortably more
/// than the daemon needs between creating `jobs/<first8>/` and writing its
/// `state.json`. Past it the job is treated as aborted: an aborted `/fork` (the
/// live `298689bf` evidence: a jobs dir that only ever got `tmp/`) must leave
/// nothing behind in the sidebar.
const FORK_STATE_POLL_ATTEMPTS: usize = 20;

/// The injectable `~/.claude/jobs/<first8>/` probe behind
/// [`WindowState::fork_job_probe`]: maps a claude session id to its jobs entry, or
/// `None` when no such entry exists (⇒ not a daemon job).
type ForkJobProbe = Box<dyn Fn(&str) -> Option<ForkJobInfo>>;

/// The production probe: read `~/.claude/jobs/<first8(session_id)>/`. Hardcodes
/// `~/.claude` exactly as [`crate::claude_hook_installer`] does — Nice honors no
/// `$CLAUDE_CONFIG_DIR`-style override anywhere today, and inventing one here
/// would make the hook and the probe disagree about where Claude's state lives.
fn probe_fork_job(claude_session_id: &str) -> Option<ForkJobInfo> {
    let home = std::env::var("HOME").ok()?;
    probe_fork_job_in(std::path::Path::new(&home).join(".claude").join("jobs"), claude_session_id)
}

/// [`probe_fork_job`] against an explicit jobs directory — the hermetic entry
/// point (tests / fixtures point it at a scratch dir instead of the developer's
/// real `~/.claude`). `None` when the id is too short to have a first-8 or the
/// per-job directory is absent; a present directory with an unreadable or
/// malformed `state.json` still yields `Some(ForkJobInfo::default())`, because
/// the DIRECTORY is the classification signal and its contents are only detail.
fn probe_fork_job_in(jobs_dir: impl AsRef<std::path::Path>, claude_session_id: &str) -> Option<ForkJobInfo> {
    let short = claude_session_id.get(..8)?;
    let dir = jobs_dir.as_ref().join(short);
    if !dir.is_dir() {
        return None;
    }
    let mut info = ForkJobInfo::default();
    if let Ok(bytes) = std::fs::read(dir.join("state.json")) {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let field = |key: &str| {
                map.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            info.claude_session_id = field("sessionId");
            info.fork_parent_session_id = field("forkParentSessionId");
            info.name = field("name");
        }
    }
    Some(info)
}

/// The session-identifying shape of an intercepted `claude` invocation — the
/// input to Fix D's exec-time normalization
/// ([`WindowState::plan_claude_exec`]). Only the two shapes that name ONE
/// specific session are modelled; everything else (a bare `claude`, a
/// `--session-id`, an argument-less `-r` picker) is [`Neither`](Self::Neither)
/// and takes the untouched pre-Fix-D path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClaudeArgSession {
    /// `--resume <uuid>`, `-r <uuid>`, or `--resume=<uuid>`.
    Resume(String),
    /// The `attach <id>` subcommand (`id` is normally the 8-hex short job id).
    Attach(String),
    /// Nothing in the args names a session.
    Neither,
}

/// Classify `args` for Fix D. Deliberately NOT
/// [`WorkspaceModel::extract_claude_session_id`]: that one folds `--session-id` in
/// with `--resume` (both "the claude session this window will run"), while Fix D must
/// tell a RESUME of an existing conversation apart from a fresh id, and must
/// also see `-r` and the `attach` subcommand, neither of which the shared
/// parser knows.
fn classify_claude_session_args(args: &[String]) -> ClaudeArgSession {
    // `attach <id>`: a subcommand, so only ever in first position.
    if args.first().map(String::as_str) == Some("attach") {
        return match args.get(1) {
            Some(id) if !id.is_empty() && !id.starts_with('-') => {
                ClaudeArgSession::Attach(id.clone())
            }
            _ => ClaudeArgSession::Neither,
        };
    }
    for (i, a) in args.iter().enumerate() {
        let value = match a.as_str() {
            "--resume" | "-r" => args.get(i + 1).map(String::as_str),
            other => match other.strip_prefix("--resume=") {
                Some(v) => Some(v),
                None => continue,
            },
        };
        // A value-less `--resume` / `-r` (or one followed by another flag)
        // opens Claude's interactive picker — it names no session, so there is
        // nothing to normalize.
        return match value {
            Some(v) if !v.is_empty() && !v.starts_with('-') => {
                ClaudeArgSession::Resume(v.to_string())
            }
            _ => ClaudeArgSession::Neither,
        };
    }
    ClaudeArgSession::Neither
}

/// What a `claude` invocation resolves to once Fix D's jobs probe has run —
/// the shared core of the two consumers that must agree: the in-place reply
/// verb ([`WindowState::plan_claude_exec`]) and the newtab spawn
/// ([`WindowState::plan_newtab_claude_exec`]). Both used to derive this
/// independently, and the newtab half silently didn't (it spliced
/// `--session-id` beside the user's `--resume`, an argv Claude Code refuses).
enum ResolvedClaudeSession {
    /// The named session is a background job the daemon still hosts: open it
    /// with `attach`. `short_id` is what `attach` resolves (it prefix-matches
    /// `~/.claude/jobs` directory names); `uuid` is the full id — the wire verb
    /// carries it for the wrapper's `--resume` fallback, and the session pins it.
    Attach { short_id: String, uuid: String },
    /// The user said `attach <full uuid>` for a session no daemon hosts:
    /// `attach` could only ever fail on it, so resume `uuid` instead.
    Resume { uuid: String },
    /// The args already name their session — run them as typed, splicing no
    /// `--session-id`. `pin` is the id the session should remember (`None` when
    /// the named session resolves to no full uuid).
    AsTyped { pin: Option<String> },
    /// Nothing in the args names a session.
    Unnamed,
}

/// The 8-hex short job id `claude attach` resolves, derived from a full
/// session uuid (`~/.claude/jobs/<first8>/`). Falls back to the whole string
/// for an id too short to slice — the probe that produced it already agreed
/// it keys a jobs entry.
fn short_job_id(uuid: &str) -> String {
    uuid.get(..8).unwrap_or(uuid).to_string()
}

/// Whether `id` has the shape of a full Claude session uuid (8-4-4-4-12 hex),
/// as opposed to the 8-hex short job id `claude attach` takes. The two are
/// interchangeable to a human and NOT to the CLI: `attach` resolves its
/// argument by prefix-matching `~/.claude/jobs/` DIRECTORY names, so a full
/// uuid matches nothing there, while `--resume` accepts only the full uuid.
fn looks_like_session_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Fix D's decision for one intercepted `claude` invocation: what the wrapper
/// should exec ([`decision`](Self::decision)) and which claude session id the
/// promoted session should remember
/// ([`pin_claude_session_id`](Self::pin_claude_session_id)).
struct ClaudeExecPlan {
    /// The id to pin on the promoted session, or `None` when the request names a
    /// claude session we cannot resolve to a full uuid (a short `attach <id>`
    /// with no readable jobs entry). Pinning the short id there would leave the
    /// session holding an id no later `claude --resume` could ever use, so the
    /// session keeps whatever it already had.
    pin_claude_session_id: Option<String>,
    /// The reply the wrapper acts on.
    decision: ClaudeReplyDecision,
}

/// A `session_update` that classified as a NEWLY BORN daemon-hosted background
/// fork: `source == "fork"` AND a `~/.claude/jobs/<first8>/` entry for the
/// incoming id. A jobs entry under any other source is the same daemon relaying
/// a later life-cycle event for a job that already has its session — distrusted the
/// same way, but handed off nowhere.
///
/// The classification itself closes bug 3 by rotating NOTHING — the window id
/// such a relay carries belongs to whichever window first spawned the daemon, so
/// it is untrustworthy. This type is the hand-off from that context-free
/// classification ([`WindowState::apply_session_update`]) to the
/// gpui-context-carrying router, which passes it to
/// [`materialize_background_fork`](WindowState::materialize_background_fork):
/// a nested child session under the session whose `claude_session_id` is
/// [`ForkJobInfo::fork_parent_session_id`], after the deferred `state.json`
/// retry that fills that field in (`job` may still be all-`None` at hook time).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BackgroundFork {
    /// The fork's own claude session id (the id the hook relayed).
    fork_claude_session_id: String,
    /// The relayed cwd, empty-normalized to `None` — the fork may live in its
    /// own worktree since 2.1.220, so it is not necessarily the parent's cwd.
    cwd: Option<String>,
    /// The jobs entry that classified this as a background fork.
    job: ForkJobInfo,
}

/// Outcome of the pure model half of a `session_update`
/// ([`WindowState::apply_session_update`]): whether any session state actually
/// changed — the R18 save signal, Swift's `onSessionMutation`; nothing persists
/// yet — plus, when the rotation classified as a `/branch`, the deferred-resume
/// [`BranchParentSpawn`] the router must fulfil with its gpui context, or, when
/// it classified as a background `/fork`, the [`BackgroundFork`] hand-off. The
/// two are mutually exclusive: a background fork touches no session at all.
#[derive(Default)]
struct SessionUpdateOutcome {
    did_mutate: bool,
    spawn: Option<BranchParentSpawn>,
    background_fork: Option<BackgroundFork>,
}

/// The per-window composition root. One per Nice window, owned by a
/// `gpui::Entity` and registered in [`crate::window_registry::WindowRegistry`].
pub(crate) struct WindowState {
    /// The R8 document — this window's projects / sessions / windows tree. Two windows
    /// are isolated precisely because each owns its own `WorkspaceModel`.
    pub(crate) workspace: WorkspaceModel,
    /// R10 sidebar collapse / mode / peek state.
    pub(crate) sidebar: SidebarModel,
    /// Phase 1 (D5): whether the hold-to-hint overlay is showing (the window-index
    /// badges on the toolbar pills). Set/cleared by the keymap's modifier observer
    /// ([`crate::keymap::on_window_modifiers_changed`]) through
    /// [`arm_key_hint`](Self::arm_key_hint) / [`cancel_key_hint`](Self::cancel_key_hint);
    /// read by [`crate::toolbar::WindowToolbarView`]'s render. Never persisted.
    pub(crate) key_hint: KeyHintModel,
    /// Phase 1 (D5): the pending ~200 ms hold debounce, held (not detached) so
    /// dropping it — on a cancel, or when the window entity drops — cancels the
    /// timer instead of leaking a task that would flash the overlay after the keys
    /// lifted. Lives here rather than in `nice-model` because that crate is
    /// gpui-free.
    hint_task: Option<gpui::Task<()>>,
    /// Phase 1 (D5): bumped on every arm and every cancel, and captured by the
    /// pending task. A timer that already fired its `timer(..).await` cannot be
    /// stopped by dropping its `Task`, so the generation is what makes a cancel
    /// that lands in that window still win: the task compares and returns.
    hint_generation: u64,
    /// R10 Finder-style multi-selection (invariant: contains the active session).
    pub(crate) selection: SidebarSessionSelection,
    /// R10 sidebar create/close/select seam (model-only; R13 rewires).
    pub(crate) sidebar_actions: Box<dyn SidebarActions>,
    /// R11 window-strip select/close/add seam (model-only; R13 rewires).
    pub(crate) window_strip_actions: Box<dyn WindowStripActions>,
    /// The per-window pty/session manager (R13). Owns this window's live window
    /// sessions and routes their OSC title/cwd events into `model`. R12 carried
    /// an empty placeholder here; R13 slice 1 fills it with the real
    /// [`PtyManager`] (the action seams that drive it are rewired in a later
    /// R13 slice — this just makes the manager part of the per-window state).
    pub(crate) ptys: PtyManager,
    /// Stable unique per-window id (the registry's per-session-id lookup key).
    window_session_id: String,
    /// R14 control-socket routing record: the parsed / normalized messages this
    /// window received through [`route_socket_message`](WindowState::route_socket_message).
    /// Populated only under `cfg(test)` or the `selftest` feature (see
    /// [`record_socket_message`](WindowState::record_socket_message)) — production
    /// leaves it empty. The `shell-socket` scenario's raw-socket headless driver
    /// and the routing unit tests assert against it.
    recorded_socket_messages: Vec<RecordedSocketMessage>,
    /// R14 per-window control socket, owned here so [`teardown`](WindowState::teardown)
    /// can stop it (suppress healing, unlink the socket file) on window close.
    /// Armed by `crate::app::arm_window_control_socket` before the Main window forks;
    /// `None` on scenarios/itests that never bootstrap one. `NiceControlSocket`'s
    /// own `Drop` also stops it, so a dropped `WindowState` never leaks its thread.
    control_socket: Option<NiceControlSocket>,
    /// The gpui foreground task draining parsed socket messages into
    /// [`route_socket_message`](WindowState::route_socket_message). Held (not
    /// detached) so dropping it — on teardown or when the window entity drops —
    /// cancels the drain rather than leaking a parked task.
    socket_drain: Option<gpui::Task<()>>,
    /// BUGHUNT1-D: the gpui foreground task draining the model's did-mutate signal
    /// into the debounced session save. The `WorkspaceModel` mutation observer
    /// ([`WorkspaceModel::set_on_tree_mutation`](nice_model::WorkspaceModel::set_on_tree_mutation),
    /// wired once per window at [`crate::app::build_window_root`]) fires
    /// SYNCHRONOUSLY inside this entity's lease, so it may only signal (an unbounded
    /// send); this task drains those signals and runs
    /// [`save_to_store`](Self::save_to_store) OUTSIDE the lease — the mandatory
    /// deferral that keeps the observer clear of the gpui double-lease SIGABRT
    /// class (D1). Held (not detached) so dropping it on teardown / entity drop
    /// cancels the drain.
    save_drain: Option<gpui::Task<()>>,
    /// R15: the injectable Claude theme-sync `--settings` pointer provider (Swift
    /// `themeCache.syncClaudeTheme ? ClaudeThemeSync.settingsFlagPath() : nil`).
    /// `None` in R15 — R17 fills it from the live theme; the socket reply and the
    /// Claude spawn both consult it, and the socket reply additionally suppresses
    /// it when the client's `args` already carry `--settings` (no doubled flag).
    /// Unit tests inject a stub via [`set_claude_settings_path_for_test`](WindowState::set_claude_settings_path_for_test).
    claude_settings_path: Option<String>,
    /// R15 subscription lift: this window's handle, stashed at
    /// [`crate::app::build_window_root`]. The window-event subscription callback
    /// ([`subscribe_spawned_windows`](WindowState::subscribe_spawned_windows)) needs a
    /// `&mut Window` to actuate a [`RoutedExit`](crate::pty_manager)'s
    /// every-project-empty terminus (close this window / quit), which an
    /// entity-subscription callback lacks — it re-enters through this handle. `None`
    /// on a `WindowState` never mounted by the shipped builder (unit tests /
    /// headless scenarios that assert the routed model mutation only).
    window_handle: Option<AnyWindowHandle>,
    /// R15 subscription lift, pane-keyed since Phase 2: the live
    /// `<session>:<window>:<pane>` subscriptions wired to
    /// [`route_terminal_event`](crate::pty_manager::PtyManager::route_terminal_event)
    /// via [`subscribe_spawned_windows`](WindowState::subscribe_spawned_windows).
    /// The sweep runs on every `WindowHostView` render; a pane is subscribed
    /// exactly once, and unsubscribed by dropping its [`Subscription`] when the
    /// pane's pty is gone from the manager.
    ///
    /// **Retained, not `.detach()`ed** — that is the load-bearing part. Break-pane
    /// re-homes a live pane under a different pill, which changes its key; a
    /// detached subscription could never be re-keyed, so the sweep would either
    /// leak a subscription per move or (with a window-level key) never subscribe
    /// a background pane at all.
    pane_subscriptions: HashMap<String, Subscription>,
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
    /// R19: the per-window file-browser state catalog (`Session.id → FileBrowserState`),
    /// lazily populated when a session first renders in files mode. In-memory only
    /// (never persisted). The [`FileBrowserView`](crate::file_browser::view::FileBrowserView)
    /// reads / mutates it through this handle; a dissolved session's entry is dropped
    /// via [`prune_dissolved_file_browser_states`](Self::prune_dissolved_file_browser_states)
    /// off the session dissolve cascade.
    pub(crate) file_browser: FileBrowserStore,
    /// The `~/.claude/jobs/<first8>/` probe that classifies a `source: "fork"`
    /// `session_update` as a daemon-hosted BACKGROUND fork (entry present) or an
    /// in-window `/branch`-shaped rotation (entry absent) — see [`ForkJobInfo`].
    /// Defaults to [`probe_fork_job`] (a real read under `$HOME/.claude`); unit
    /// tests swap in a fixture via
    /// [`set_fork_job_probe_for_test`](WindowState::set_fork_job_probe_for_test)
    /// so the classification is testable without touching the developer's real
    /// `~/.claude` — and so no test can be perturbed by whatever forks happen to
    /// be running on the machine.
    fork_job_probe: ForkJobProbe,
    /// R21: this window's mounted window-content host, stashed at
    /// [`crate::app::build_window_root`] so the process-level theme fan-out
    /// ([`crate::theme_settings::apply_theme_fanout`]) can reach every window's
    /// terminal windows: it walks [`crate::window_registry::WindowRegistry::all_states`]
    /// → each `WindowState` → this host → its cached `TerminalView`s, pushing the new
    /// colors through the boundary-legal setters (the `SessionThemeCache` analog).
    /// `None` on a `WindowState` never mounted by the shipped builder (unit tests /
    /// headless scenarios), so the fan-out simply skips it.
    window_host: Option<gpui::Entity<crate::app_shell::WindowHostView>>,
    /// Phase 2: the pane area's last painted size in px (width, height) — the
    /// region [`crate::app_shell::WindowHostView`] lays the active pill's split
    /// tree out in, recorded by that host on every render.
    ///
    /// It lives here because the pane ACTIONS need px context and cannot
    /// measure anything: an App-level action handler has no `&mut Window`, and
    /// the split-refusal (P6 minimum pane size) and the resize step's
    /// px → ratio conversion both need the painted extent. `None` until the
    /// window has painted once, which the readers treat as "no px context"
    /// (split at an even ratio, resize no-op) rather than guessing.
    pane_content_size: Option<(f32, f32)>,
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
    /// A fresh default window: a seeded [`WorkspaceModel`] rooted at `initial_cwd`
    /// (pinned Terminals group + Main session, per `WorkspaceModel::new`), an expanded
    /// sidebar in sessions mode, and a selection seeded from the model's active session —
    /// mirroring `AppState`'s convenience init defaults
    /// (`initialSidebarCollapsed: false`, `initialSidebarMode: .sessions`). Every ⌘N
    /// mints one of these; R18 will add a variant that takes restored state.
    pub(crate) fn new(initial_cwd: impl Into<String>) -> Self {
        Self::with_model(WorkspaceModel::new(initial_cwd))
    }

    /// A window seeded around a pre-built [`WorkspaceModel`] — the scenario/restore
    /// seam. The isolated `sidebar` / `pane-strip` self-test windows use it to
    /// mount the shipped views (`SidebarShellView` / `WindowToolbarView`) over a
    /// fixture model while still going through the SAME shared-state shape the
    /// managed window uses (R13.5's "seed a `WindowState` around their seed
    /// models" decision); R18's restore path will thread persisted state through
    /// here too. Same defaults as [`new`](WindowState::new) otherwise (expanded
    /// sidebar, sessions mode, model-only action seams, a fresh [`PtyManager`],
    /// a unique session id), and it re-seeds the selection from the model's active
    /// session so the "selection ⊇ {active session}" invariant holds from construction.
    pub(crate) fn with_model(workspace: WorkspaceModel) -> Self {
        let mut selection = SidebarSessionSelection::new();
        selection.sync_active_session_id(workspace.active_session_id());
        Self {
            workspace,
            sidebar: SidebarModel::new(false, SidebarMode::Sessions),
            key_hint: KeyHintModel::new(),
            hint_task: None,
            hint_generation: 0,
            selection,
            sidebar_actions: Box::new(ModelSidebarActions::new()),
            window_strip_actions: Box::new(ModelWindowStripActions::new()),
            ptys: PtyManager::new(),
            window_session_id: mint_window_session_id(),
            recorded_socket_messages: Vec::new(),
            control_socket: None,
            socket_drain: None,
            save_drain: None,
            claude_settings_path: None,
            window_handle: None,
            pane_subscriptions: HashMap::new(),
            user_initiated_close: false,
            pending_modal: None,
            modal_sub: None,
            last_frame: None,
            // Per-session file-browser states are created lazily on first files-mode
            // render, defaulting to dotfiles-hidden (the 2026-07-07 deviation from
            // Swift's cwd-aware `show_hidden` heuristic).
            file_browser: FileBrowserStore::new(),
            fork_job_probe: Box::new(probe_fork_job),
            window_host: None,
            pane_content_size: None,
        }
    }

    /// Rebuild a window from a persisted seed — the L2/L3 restore constructor
    /// (Swift `WindowSession.restoreSavedWindow`, `:326-365`). Unlike
    /// [`with_model`](Self::with_model), which seeds a fresh Terminals+Main tree,
    /// this trusts the SAVED grouping: it builds the document from the hydrated
    /// projects via [`WorkspaceModel::from_parts`] (no fresh Terminals/Main), runs the
    /// same repair pass restore always does — `repair_project_structure()` then
    /// `prune_dangling_parent_references()` — then re-applies the saved active session
    /// **iff it survived** the repairs (else the first navigable session), and adopts
    /// the saved window id + collapsed-sidebar flag. The selection is re-seeded
    /// from the resolved active session so the "selection ⊇ {active session}" invariant
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
            active_session_id,
            sidebar_collapsed,
            sidebar_mode,
            sidebar_width,
            ..
        } = seed;

        let mut model = WorkspaceModel::from_parts_std(projects, active_session_id);
        // Restore repairs (trust the grouping, then fix structural drift):
        // re-pin project/session shape, then drop parent links to sessions that didn't
        // survive.
        model.repair_project_structure();
        model.prune_dangling_parent_references();
        // Re-mint any duplicate window ids a pre-fix save left behind (the old
        // reset-at-launch minter could persist two windows sharing one id, which
        // double-lights the strip and makes rename edit both).
        model.dedupe_window_ids();
        // Re-apply the saved active session iff it still exists after the repairs,
        // else fall back to the first navigable session (Swift re-applies `activeTabId`
        // only when the session survived).
        let resolved_active = model
            .active_session_id()
            .filter(|id| model.session_for(id).is_some())
            .map(str::to_string)
            .or_else(|| model.navigable_sidebar_session_ids().into_iter().next());
        if let Some(active) = resolved_active {
            model.select_session(&active);
        }

        let mut state = Self::with_model(model);
        state.window_session_id = window_id;
        // R19: restore the saved sidebar mode (absent ⇒ Sessions — the pre-R19 / never-
        // toggled default).
        state.sidebar = SidebarModel::new(sidebar_collapsed, sidebar_mode.unwrap_or(SidebarMode::Sessions));
        // Phase 0: restore the saved per-window sidebar width (absent ⇒ default).
        state.sidebar.set_width(sidebar_width.map(|w| w as f32));
        // Re-seed the selection from the (possibly repair-shifted) active session.
        state.selection.sync_active_session_id(state.workspace.active_session_id());
        state
    }

    /// Stash this window's handle (the shipped builder calls it at
    /// [`crate::app::build_window_root`]). Read by
    /// [`subscribe_spawned_windows`](Self::subscribe_spawned_windows)'s routed-exit
    /// terminus actuation.
    pub(crate) fn set_window_handle(&mut self, handle: AnyWindowHandle) {
        self.window_handle = Some(handle);
    }

    /// This window's stashed handle (present after
    /// [`crate::app::build_window_root`]). Read by the R21 theme fan-out's
    /// window-transparency pass so it can reach each live `Window` to set its
    /// `WindowBackgroundAppearance` + blur radius.
    pub(crate) fn window_handle(&self) -> Option<AnyWindowHandle> {
        self.window_handle
    }

    /// R21: stash this window's mounted window host (the shipped builder calls it at
    /// [`crate::app::build_window_root`]) so the process theme fan-out can push
    /// recolors into its terminal windows.
    pub(crate) fn set_window_host(
        &mut self,
        window_host: gpui::Entity<crate::app_shell::WindowHostView>,
    ) {
        self.window_host = Some(window_host);
    }

    /// R21: this window's mounted window host, if the shipped builder mounted one.
    /// [`crate::theme_settings::apply_theme_fanout`] reads it to reach the windows.
    pub(crate) fn window_host(&self) -> Option<gpui::Entity<crate::app_shell::WindowHostView>> {
        self.window_host.clone()
    }

    /// Record the pane area's painted size — see
    /// [`pane_content_size`](Self::pane_content_size). Written by
    /// [`crate::app_shell::WindowHostView`]'s render from the size its bounds
    /// probe captured on the previous paint. Deliberately silent (no
    /// `cx.notify()`): a size write must never itself schedule a render, or a
    /// window resize would loop.
    pub(crate) fn set_pane_content_size(&mut self, size: Option<(f32, f32)>) {
        self.pane_content_size = size;
    }

    /// The pane area's last painted size in px, `None` before the first paint.
    /// The px context the pane actions and the divider drag measure against.
    pub(crate) fn pane_content_size(&self) -> Option<(f32, f32)> {
        self.pane_content_size
    }

    /// Mirror the model's active session into the multi-selection — the Rust analog of
    /// Swift's single active-session observer (`SidebarView.swift:75-77`). Keyboard session
    /// cycling (`NextSidebarSession` / `PrevSidebarSession`) mutates only the model, so the
    /// selection's active mirror must be re-synced here or the previously-active row
    /// lingers in `selection` as a faint `SELECTED_DIM_FACTOR` highlight; mouse
    /// paths already sync inline via `route_click`. Keeps the "selection ⊇ {active
    /// session}" invariant (`sync_active_session_id` collapses when the new active session is
    /// outside the set).
    pub(crate) fn sync_selection_to_active_session(&mut self) {
        self.selection.sync_active_session_id(self.workspace.active_session_id());
    }

    /// The single seam for "toggle the sidebar collapsed flag": flips it,
    /// clears any peek when expanding (`AppShellView`: expand clears peek), and
    /// notifies the `WindowState` entity so every `cx.observe(&state)` seam fires
    /// — the ⌘B keymap action, the titlebar collapse toggle
    /// ([`crate::toolbar::WindowToolbarView`]), and the sidebar view
    /// ([`crate::sidebar_shell::SidebarShellView`]) all route through here so the
    /// state mutation + peek cleanup stay identical across entry points. (The
    /// sidebar view additionally drops its view-local hover pin off the resulting
    /// state notification; see its state observer.)
    pub(crate) fn toggle_sidebar_collapsed(&mut self, cx: &mut gpui::Context<WindowState>) {
        self.sidebar.toggle_sidebar();
        if !self.sidebar.collapsed() {
            self.sidebar.end_sidebar_peek();
        }
        cx.notify();
    }

    /// Phase 1 (D5): start the hold debounce — show the hint overlay if the
    /// scheme's modifier pair is STILL held `delay` from now. Driven by
    /// [`crate::keymap::on_window_modifiers_changed`] the moment the held set
    /// becomes exactly that pair; its counterpart is
    /// [`cancel_key_hint`](Self::cancel_key_hint).
    ///
    /// The delay is what keeps a fast `⌃⌘L` from flashing the badges: a chord that
    /// commits within it releases the modifiers, which cancels before the timer
    /// fires.
    ///
    /// Idempotent per hold — a second arm while one is pending (or while the
    /// overlay already shows) is ignored, so the repeated modifier events one
    /// physical hold can produce never restart the countdown.
    ///
    /// The timer is the gpui executor's (`background_executor().timer`), NEVER
    /// `smol::Timer` — an untracked timer is invisible to `run_until_parked`, so
    /// the tests below could never observe it fire.
    pub(crate) fn arm_key_hint(
        &mut self,
        delay: std::time::Duration,
        cx: &mut gpui::Context<WindowState>,
    ) {
        if self.key_hint.visible() || self.hint_task.is_some() {
            return;
        }
        self.hint_generation = self.hint_generation.wrapping_add(1);
        let generation = self.hint_generation;
        self.hint_task = Some(cx.spawn(
            async move |this: gpui::WeakEntity<WindowState>, acx: &mut gpui::AsyncApp| {
                acx.background_executor().timer(delay).await;
                // The window this fires on, iff the hold survived — `None` means
                // some cancel won, or the flag was already set, so there is
                // nothing to paint.
                let kick = this
                    .update(acx, |ws, cx| {
                        // A cancel (or a re-arm) since this task was spawned wins:
                        // the modifiers changed inside the debounce window.
                        if ws.hint_generation != generation {
                            return None;
                        }
                        if !ws.key_hint.set_visible(true) {
                            return None;
                        }
                        cx.notify();
                        ws.window_handle
                    })
                    .ok()
                    .flatten();
                // `cx.notify()` alone never PRESENTS while the window's
                // CVDisplayLink is stopped (`crate::platform` fact 1), and unlike
                // every other shortcut path this one paints on a TIMER, with no
                // user event behind it — so fire the same demand-present kick the
                // modal path uses. A no-op on a visible window (the kick is
                // occlusion-gated inside `platform::present_kick`) and on any
                // `WindowState` the shipped builder never mounted (`window_handle`
                // is `None` in unit tests / headless scenarios).
                if let Some(handle) = kick {
                    let _ = handle.update(acx, |_root, window, _app| {
                        let view_ptr = crate::platform::ns_view_of(window);
                        // SAFETY: `view_ptr` is this window's live NSView (or null,
                        // which `present_kick` treats as a no-op).
                        unsafe { crate::platform::present_kick(view_ptr) };
                    });
                }
            },
        ));
    }

    /// Phase 1 (D5): hide the hint overlay and drop any pending debounce — the
    /// instant-clear half, driven by any modifier change that leaves the scheme's
    /// pair (including the release that ends the hold).
    ///
    /// Both halves matter: dropping the [`Task`](gpui::Task) cancels a timer that
    /// has not fired, and the generation bump neutralizes one that already fired
    /// and is waiting its turn on the foreground queue.
    pub(crate) fn cancel_key_hint(&mut self, cx: &mut gpui::Context<WindowState>) {
        self.hint_task = None;
        self.hint_generation = self.hint_generation.wrapping_add(1);
        if self.key_hint.set_visible(false) {
            cx.notify();
        }
    }

    /// R15 subscription lift — the shipped-window twin of the
    /// `session-lifecycle` scenario's `spawn_and_subscribe` (the tranche's known
    /// integration gap: `route_terminal_event` was wired ONLY in that scenario, so
    /// in the shipped app OSC titles/cwd and exits dead-ended at the view adapter).
    /// Sweeps every live PANE and subscribes any not-yet-wired one's entity
    /// to [`route_terminal_event`](PtyManager::route_terminal_event), so the
    /// SHIPPED window retitles pills, updates window cwd, and removes exited windows.
    ///
    /// Called from [`crate::app_shell::WindowHostView`]'s render — the single choke
    /// point every spawn flows past (the Main window is spawned before the first
    /// render; deferred terminals spawn through `activate_term_window` on activation; a
    /// Claude session's spawn + the socket newtab spawn each re-render the shell). It is
    /// idempotent via [`pane_subscriptions`](Self::pane_subscriptions) (subscribe-once
    /// dedupe), so running it every render is safe and cheap.
    ///
    /// Pane-level since Phase 2, in both directions:
    ///
    /// * a subscription is created per `(session, window, pane)`, so a
    ///   BACKGROUND pane's exit routes — a window-keyed sweep would wire
    ///   whichever pane spawned first and dedupe every later one away forever;
    /// * subscriptions are RETAINED and dropped when their pane's key stops
    ///   appearing in the sweep, which both frees a dead pane's subscription and
    ///   re-keys a pane that break-pane re-homed under another pill.
    ///
    /// Each closure captures only `(session, pane)` and resolves the owning
    /// WINDOW from the model at event time, so a re-homed pane's events land in
    /// its current pill even before the next sweep re-keys it.
    ///
    /// The [`RoutedExit`](crate::pty_manager) neighbor-refocus spawn is
    /// **composed by `WindowHostView`'s activation path**, not re-actuated here: the
    /// `cx.notify()` below re-renders the host, whose activation change re-runs
    /// the activation path (`activate_term_window`'s deferred-companion spawn, then the
    /// host's own key-focus move) per the landed M2 focus-routing. Only the
    /// every-project-empty terminus — which needs a
    /// `&mut Window` a subscription callback lacks — is actuated here, via the
    /// stashed [`window_handle`](Self::window_handle).
    pub(crate) fn subscribe_spawned_windows(&mut self, cx: &mut gpui::Context<WindowState>) {
        let live = self.ptys.live_pane_keys();
        // Retire subscriptions whose pane no longer has a pty under that key —
        // the pane died, or break-pane moved it to another pill and it is about
        // to be re-subscribed under the new key below.
        let live_keys: HashSet<String> = live
            .iter()
            .map(|(t, w, pane)| format!("{t}:{w}:{pane}"))
            .collect();
        self.pane_subscriptions.retain(|key, _| live_keys.contains(key));

        for (session_id, term_window_id, pane_id) in live {
            let key = format!("{session_id}:{term_window_id}:{pane_id}");
            if self.pane_subscriptions.contains_key(&key) {
                continue;
            }
            let Some(handle) = self
                .ptys
                .pane_handle(&session_id, &term_window_id, &pane_id)
            else {
                continue;
            };
            let (t, pane) = (session_id.clone(), pane_id.clone());
            let subscription = cx.subscribe(&handle, move |ws, _handle, event: &TerminalEvent, cx| {
                // Quit freeze: once quit has begun the model is read-only.
                // `quit_cascade` has already flushed every window's snapshot
                // (step 2) when its teardown (step 3) kills the windows — and an
                // intentional kill classifies `held: false` (nice-term-core
                // `should_hold_on_exit`), the clean-exit value whose routing
                // dissolves the session. gpui can still deliver those `Exited`
                // events between teardown and the `on_app_quit` re-flush, which
                // would re-snapshot the shrunken model and overwrite the good
                // step-2 write — losing whichever sessions' events won the race
                // (and, via the `WindowEmptied` mint below, wiping the whole
                // slot when every window's event landed). The `AppQuitting`
                // latch already makes window closes inert; window events freeze
                // the same way.
                if cx.has_global::<crate::lifecycle::AppQuitting>() {
                    return;
                }
                // Which pill owns this pane RIGHT NOW — resolved from the model,
                // not captured, so a pane break-pane re-homed since this
                // subscription was made still routes to the pill it now lives in.
                let Some(term_window_id) = ws
                    .workspace
                    .session_for(&t)
                    .and_then(|s| s.window_for_pane(&pane))
                    .map(|w| w.id.clone())
                else {
                    return;
                };
                let workspace = &mut ws.workspace;
                let selection = &mut ws.selection;
                let routed = ws.ptys.route_terminal_event(
                    workspace,
                    selection,
                    &t,
                    &term_window_id,
                    &pane,
                    event,
                );
                // R19: a routed window-exit may have dissolved a session — drop its
                // file-browser state (the window-exit dissolve path, not covered by
                // the UI-close methods above).
                ws.prune_dissolved_file_browser_states();
                // Re-render so `WindowHostView` re-activates: a routed window removal
                // shifts the active window, and its activation change re-runs the
                // activation path (neighbor deferred-companion spawn via
                // `activate_term_window`, then the host's own key-focus move), and pills
                // / cwd refresh from the mutated model.
                cx.notify();
                // The every-project-empty terminus (close this window / quit) needs
                // a `&mut Window`; actuate it via the stashed handle (the "composed
                // by the live window root" obligation).
                //
                // It MUST be deferred out of this subscription callback: we are
                // still inside the `WindowState` entity's lease (`ws: &mut Self`)
                // while delivering the window's `Exited` event. When another window
                // is live, `apply_dissolve_terminus` → `window.remove_window()`
                // drives gpui's synchronous window-removal trail, which fires the
                // `on_window_closed` observer → `WindowRegistry::route_close_disk_fate`
                // → `state.update(.., teardown)` on THIS same leased entity — a
                // re-entrant update that aborts the process. (The single-window
                // case took `cx.quit()` and never re-entered, which is why the crash
                // only showed with a second window open.) `cx.defer` (App-level; the
                // `Context` derefs to `App`) runs the actuation at the end of the
                // current effect cycle, once this lease is released.
                if routed.terminus == DissolveTerminus::WindowEmptied {
                    // Drop this emptied window's disk slot (else it restores as a
                    // broken empty window next launch). Set on the leased `ws` HERE,
                    // before the defer — never inside `apply_dissolve_terminus`,
                    // which would re-lease this entity on the UI-close paths.
                    ws.mark_removed_if_window_emptied(routed.terminus);
                    if let Some(handle) = ws.window_handle {
                        let terminus = routed.terminus;
                        cx.defer(move |app| {
                            let _ = handle.update(app, |_root, window, app| {
                                PtyManager::apply_dissolve_terminus(terminus, window, app);
                            });
                        });
                    }
                }
            });
            self.pane_subscriptions.insert(key, subscription);
        }
    }

    /// Test seam: inject the Claude theme-sync `--settings` pointer provider (R17's
    /// value; `None` by default). Drives the sync-ON socket-reply cases.
    #[cfg(test)]
    pub(crate) fn set_claude_settings_path_for_test(&mut self, path: Option<String>) {
        self.claude_settings_path = path;
    }

    /// Test seam: inject the `~/.claude/jobs/<first8>/` probe (production reads
    /// the real directory — see [`probe_fork_job`]). Every `source: "fork"` case
    /// in the suite installs one, so the classification never depends on the
    /// developer's machine state.
    #[cfg(test)]
    pub(crate) fn set_fork_job_probe_for_test(
        &mut self,
        probe: impl Fn(&str) -> Option<ForkJobInfo> + 'static,
    ) {
        self.fork_job_probe = Box::new(probe);
    }

    /// Scenario seam: re-root the fork-job probe at `jobs_dir` instead of
    /// `$HOME/.claude/jobs`. Unlike
    /// [`set_fork_job_probe_for_test`](Self::set_fork_job_probe_for_test) this
    /// keeps the REAL probe ([`probe_fork_job_in`]) — only its directory moves —
    /// so a live scenario exercises the shipped filesystem read against a scratch
    /// fixture instead of the developer's `~/.claude` (which the live suites can't
    /// use: they restore the real `HOME` after the window opens, and the machine's
    /// actual background forks would make the assertions non-deterministic).
    pub(crate) fn set_fork_jobs_dir_for_scenario(&mut self, jobs_dir: std::path::PathBuf) {
        self.fork_job_probe = Box::new(move |id| probe_fork_job_in(&jobs_dir, id));
    }

    /// Fill R15's Claude theme-sync `--settings` provider (R17 slice 2). The
    /// shipped window builder (`crate::app::open_managed_window`) computes the value
    /// from the process gate (`ClaudeThemeSyncGate` →
    /// [`crate::claude_theme_sync::settings_path_for_gate`]) and sets it here before
    /// the Main window forks, so a later Claude spawn/reply/prefill sees it. `None` ⇒
    /// Claude spawns get no `--settings` (sync off, or the gate unset under
    /// `run_selftest`). R21 re-sources this on live theme/toggle changes.
    pub(crate) fn set_claude_settings_path(&mut self, path: Option<String>) {
        self.claude_settings_path = path;
    }

    /// The Claude theme-sync `--settings` pointer provider value (R17 fills it;
    /// `None` in R15). Read by the sidebar project-`+` seam when it spawns a fresh
    /// Claude session. The socket reply consults [`effective_inplace_settings`](Self::effective_inplace_settings)
    /// (the same provider, plus the `--settings`-in-args gate).
    pub(crate) fn claude_settings_path_provider(&self) -> Option<String> {
        self.claude_settings_path.clone()
    }

    /// Take ownership of this window's armed control socket + its foreground drain
    /// task (`crate::app::arm_window_control_socket`, called before the Main window
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

    /// BUGHUNT1-D: take ownership of this window's did-mutate save-drain task
    /// (spawned by [`crate::app::wire_tree_mutation_save`] when the observer is
    /// wired). Held so dropping the window cancels it; replacing any prior task
    /// keeps a re-wire idempotent.
    pub(crate) fn set_save_drain(&mut self, drain: gpui::Task<()>) {
        self.save_drain = Some(drain);
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
    pub(crate) fn window_session_id(&self) -> &str {
        &self.window_session_id
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
    /// to a named window-local handler. The three FROZEN actions' shapes are
    /// finished business after R14 — R15/R16/R26 replaced only the handler BODIES;
    /// the later `dispatch` action added a fourth arm without reshaping the rest.
    /// Called on the gpui foreground by the socket drain task (wired by the R14
    /// env-injection slice's `open_managed_window`).
    ///
    /// Takes the window's `&mut Context` (R15): the `claude` newtab decision spawns
    /// a Claude window, which needs a gpui context. The `handoff` (R26) and
    /// `dispatch` sub-handlers likewise take `cx` — like the `claude` arm, each
    /// spawns a fresh Claude session.
    /// `session_update`'s handler is context-free (the pure rotation flow),
    /// returning a deferred-resume [`BranchParentSpawn`] the router fulfils here
    /// with `cx` when the rotation was a `/branch` (R16), or a [`BackgroundFork`]
    /// the router materializes here (Fix B) when it was a daemon-hosted `/fork`.
    pub(crate) fn route_socket_message(
        &mut self,
        msg: SocketMessage,
        cx: &mut gpui::Context<WindowState>,
    ) {
        match msg {
            SocketMessage::Claude {
                cwd,
                args,
                session_id,
                term_window_id,
                reply,
            } => self.handle_claude_socket_request(cwd, args, session_id, term_window_id, reply, cx),
            SocketMessage::SessionUpdate {
                term_window_id,
                claude_session_id,
                source,
                cwd,
            } => {
                let outcome = self.handle_session_update(term_window_id, claude_session_id, source, cwd);
                if let Some(spawn) = outcome.spawn {
                    self.spawn_branch_parent(spawn, cx);
                }
                // A daemon-hosted background `/fork` rotated nothing (that is bug
                // 3's fix) and is instead materialized as its own sidebar entry.
                if let Some(fork) = outcome.background_fork {
                    self.materialize_background_fork(fork, cx);
                }
            }
            SocketMessage::ClaudeExited { term_window_id } => {
                self.handle_claude_exited(term_window_id);
                // Re-render: the window's status dot / waiting pulse and its pill
                // must leave the running state now, not at the next unrelated
                // event.
                cx.notify();
            }
            SocketMessage::Handoff {
                cwd,
                handoff_file,
                instructions,
                model,
                effort,
                session_id,
                term_window_id,
                reply,
            } => self.handle_handoff(
                cwd,
                handoff_file,
                instructions,
                model,
                effort,
                session_id,
                term_window_id,
                reply,
                cx,
            ),
            SocketMessage::Dispatch {
                cwd,
                worktree_name,
                task_file,
                instructions,
                model,
                effort,
                session_id,
                term_window_id,
                reply,
            } => self.handle_dispatch(
                cwd,
                worktree_name,
                task_file,
                instructions,
                model,
                effort,
                session_id,
                term_window_id,
                reply,
                cx,
            ),
        }
        // R18 (post-gate save trigger): a socket-driven mutation (a `claude`
        // newtab / in-place promotion, or a `session_update` rotation) changed the
        // session tree — schedule the debounced upsert (Swift's `onSessionMutation` →
        // `scheduleSaveCurrentWindow`). A no-op when no store Global is installed,
        // and the store coalesces a burst into one write. The gpui-free model's
        // `FnMut()` mutation observer cannot snapshot the whole window (it has no
        // `cx`), so the live save triggers hang off the mutation SITES — here for
        // the socket path, `observe_window_bounds` for frames, and the UI-close
        // methods for dissolves — each funnelling into `upsert(snapshot)` + debounce.
        self.save_to_store();
    }

    /// Handle a `claude` invocation from a window's zsh wrapper — the Rust twin of
    /// Swift `SessionsModel.handleClaudeSocketRequest` (`SessionsModel.swift:827-911`).
    /// The wrapper is blocked reading a single-line reply, so
    /// [`resolve_claude_request`](Self::resolve_claude_request) replies exactly once
    /// on every path. On the newtab decision it returns the spawn request, which
    /// this (gpui-context-carrying) handler fulfils by building + spawning a fresh
    /// Claude session through the ONE shared constructor.
    fn handle_claude_socket_request(
        &mut self,
        cwd: String,
        args: Vec<String>,
        session_id: String,
        term_window_id: String,
        reply: Reply,
        cx: &mut gpui::Context<WindowState>,
    ) {
        self.record_socket_message(RecordedSocketMessage::Claude {
            cwd: cwd.clone(),
            args: args.clone(),
            session_id: session_id.clone(),
            term_window_id: term_window_id.clone(),
        });
        if let Some(spawn) = self.resolve_claude_request(&cwd, &args, &session_id, &term_window_id, reply) {
            // newtab: build + spawn the session (the "newtab" reply already went out).
            // The spawn's settings pointer is the provider value (no args gate here —
            // Swift's `makeSession` uses the raw provider; only the reply gates on
            // `--settings`, done inside `resolve_claude_request`).
            let settings = self.claude_settings_path.clone();
            let workspace = &mut self.workspace;
            let ptys = &mut self.ptys;
            let created = ptys.create_claude_session(
                workspace,
                ClaudeSessionPlacement::Bucket { cwd: spawn.cwd },
                &spawn.args,
                spawn.spec,
                settings.as_deref(),
                cx,
            );
            if created.is_some() {
                // Keep the "selection ⊇ {active session}" invariant: the new session is now
                // active (Swift sets `activeTabId`).
                self.selection.sync_active_session_id(self.workspace.active_session_id());
            }
        }
        // Re-render: the newtab appears / the in-place promotion retitles the pill.
        cx.notify();
    }

    /// `claude_exited` handler — the wrapper telling us the Claude it ran as a
    /// CHILD has returned and the window is a shell prompt again.
    ///
    /// Only Fix D's `attach` verb runs Claude as a child (so a jobs entry the
    /// daemon left behind can degrade to `--resume` instead of stranding the
    /// user). Every other verb `exec`s, which ties Claude's lifetime to the
    /// pty's: when it ends the window dies and [`PtyManager::window_held`]
    /// clears the promotion flag. A child leaves the pty alive, so without this
    /// message `is_claude_running` would stay `true` forever — and that flag is
    /// the ≤1-Claude-per-session guard, so every later `claude` in the session
    /// would open a NEW session instead of promoting in place (observed in
    /// validation: after detaching from an attached fork, `claude --resume
    /// <fork id>` at the window's own prompt opened a stray root session).
    ///
    /// Clears only what the promotion set. The window stays Claude-kind: it is
    /// still the session's Claude window, exactly as a deferred-resume window is
    /// before its first run.
    fn handle_claude_exited(&mut self, term_window_id: String) {
        self.record_socket_message(RecordedSocketMessage::ClaudeExited {
            term_window_id: term_window_id.clone(),
        });
        let Some(session_id) = self.workspace.session_id_owning(&term_window_id) else {
            return; // stale / unknown window — silent no-op, like session_update
        };
        self.workspace.mutate_session(&session_id, |session| {
            if let Some(window) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                window.is_claude_running = false;
                // A prompt is neither thinking nor waiting; clear the ack too so
                // a future run can pulse again (`window_held`'s rule).
                window.status = SessionStatus::Idle;
                window.waiting_acknowledged = false;
            }
        });
    }

    /// The `claude` newtab/inplace decision + reply — the pure, spawn-free half of
    /// [`handle_claude_socket_request`](Self::handle_claude_socket_request), so the
    /// dispatch's model side effects are unit-testable without a gpui context
    /// (Swift `handleClaudeSocketRequest:834-910`). Replies exactly once. Returns
    /// `Some(NewSessionSpawn)` when the caller must build + spawn a fresh Claude session
    /// (the `newtab` reply already went out); `None` when it promoted in place (the
    /// model mutation is applied and the `inplace…` reply already went out).
    fn resolve_claude_request(
        &mut self,
        cwd: &str,
        args: &[String],
        session_id: &str,
        term_window_id: &str,
        reply: Reply,
    ) -> Option<NewSessionSpawn> {
        // Decision: promote in place ONLY when the request names a real window in a
        // known, non-Terminals session that has NO running Claude; else open a new session
        // (empty/unknown tabId, a Terminals-group session, a stale paneId, or the
        // ≤1-Claude-per-session guard).
        let known = !session_id.is_empty() && self.workspace.session_for(session_id).is_some();
        let is_terminals = self.workspace.is_terminals_project_session(session_id);
        let (window_in_session, has_running) = match self.workspace.session_for(session_id) {
            Some(session) => (
                session.windows.iter().any(|w| w.id == term_window_id),
                session.windows.iter().any(|w| w.is_claude_running),
            ),
            None => (false, false),
        };
        if !(known && !is_terminals && window_in_session && !has_running) {
            reply.send("newtab");
            // The fresh session still owes Fix D's decision: a request that NAMES
            // a claude session must keep that id (and never get a `--session-id`
            // spliced in beside it — an argv the CLI refuses).
            let (args, spec) = self.plan_newtab_claude_exec(args);
            return Some(NewSessionSpawn {
                cwd: cwd.to_string(),
                args,
                spec,
            });
        }

        // Promotion in place. Resolve the claude session the invocation names —
        // the id parsed from `--resume`/`--session-id`/`attach` (a restored
        // deferred window's pre-typed `claude --resume <uuid>`), else a freshly
        // minted one to persist for the next relaunch — and, with it, Fix D's
        // exec-time normalization between `--resume` and `attach`.
        let plan = self.plan_claude_exec(args);
        self.workspace.mutate_session(session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|w| w.id == term_window_id) {
                term_window.kind = TermWindowKind::Claude;
                // The tree's half of the same flip: mark which LEAF is the Claude
                // pane, so "kind == Claude iff exactly one Claude leaf" survives
                // promotion. The socket names the pill, not the pane (P2), so the
                // focused pane takes the mark.
                term_window.mark_claude_pane();
                // The ONLY production false→true flip of `is_claude_running` (the
                // signal `window_title_changed`'s OSC gate releases on).
                term_window.is_claude_running = true;
                // Seed "Claude" so the pill isn't stale until the OSC arrives —
                // unless the user hand-renamed the window (the OSC gate would block
                // the next title anyway).
                if !term_window.title_manually_set {
                    term_window.title = "Claude".to_string();
                }
            }
            session.active_window_id = Some(term_window_id.to_string());
            if let Some(id) = &plan.pin_claude_session_id {
                session.claude_session_id = Some(id.clone());
            }
        });
        // onSessionMutation → R18's did-mutate save; nothing to persist yet.

        // Reply. Hand the wrapper the theme pointer when the provider has one AND
        // the args don't already carry `--settings` (no doubled flag). Sync off /
        // gated → the reply stays byte-identical to the pre-theming protocol.
        let settings = self.effective_inplace_settings(args);
        let line = compose_claude_reply(&plan.decision, settings.as_deref());
        reply.send(&line);
        None
    }

    /// Fix D — the exec-time `--resume` ⇄ `attach` normalization, plus the
    /// pre-existing "parse the id out of the args, else mint one" resolution
    /// they share.
    ///
    /// Which of the two verbs opens a background session correctly can only be
    /// decided HERE, at exec time: a deferred window's `claude --resume <uuid>`
    /// sits pre-typed in the shell until the user presses Enter, by which point
    /// the Claude daemon may have picked the session up or dropped it. The
    /// discriminator is the same `~/.claude/jobs/<first8>/` probe the
    /// `session_update` classification uses (see [`ForkJobInfo`]), read through
    /// the same injectable seam.
    ///
    /// * `--resume <uuid>` naming a session the daemon still hosts ⇒ `attach`.
    ///   A `--resume` would spawn a SECOND process against a live conversation.
    /// * `attach <full uuid>` ⇒ normalized either way: `attach` matches jobs by
    ///   directory-name prefix, so a full uuid can only ever fail there.
    /// * `attach <short id>` ⇒ passed through untouched (it is already the
    ///   right verb when the job exists, and when the job is gone there is no
    ///   `state.json` left to recover the uuid from — `attach` reports the miss
    ///   itself, exiting 1). Still session-IDENTIFYING, so no `--session-id`
    ///   gets spliced in beside it.
    /// * everything else ⇒ exactly the pre-Fix-D behavior.
    fn plan_claude_exec(&self, args: &[String]) -> ClaudeExecPlan {
        match self.resolve_claude_session(args) {
            ResolvedClaudeSession::Attach { uuid, .. } => ClaudeExecPlan {
                pin_claude_session_id: Some(uuid.clone()),
                // The FULL uuid rides the wire: the wrapper slices attach's
                // short id off it and needs the uuid for its fallback leg.
                decision: ClaudeReplyDecision::Attach { claude_session_id: uuid },
            },
            ResolvedClaudeSession::Resume { uuid } => ClaudeExecPlan {
                pin_claude_session_id: Some(uuid.clone()),
                decision: ClaudeReplyDecision::Resume { claude_session_id: uuid },
            },
            ResolvedClaudeSession::AsTyped { pin } => ClaudeExecPlan {
                decision: ClaudeReplyDecision::InPlace {
                    parsed_from_args: true,
                    // Never rendered under `parsed_from_args` (the reply is the
                    // bare `inplace` / the `-` placeholder form).
                    claude_session_id: pin.clone().unwrap_or_default(),
                },
                pin_claude_session_id: pin,
            },
            ResolvedClaudeSession::Unnamed => {
                let claude_session_id = mint_session_uuid();
                ClaudeExecPlan {
                    pin_claude_session_id: Some(claude_session_id.clone()),
                    decision: ClaudeReplyDecision::InPlace {
                        parsed_from_args: false,
                        claude_session_id,
                    },
                }
            }
        }
    }

    /// Fix D on the **newtab** branch: the same decision as
    /// [`plan_claude_exec`](Self::plan_claude_exec), expressed as the argv +
    /// [`ClaudeSessionSpec`] the fresh session's spawn needs.
    ///
    /// The branch exists because the in-place guard refused (an unknown /
    /// Terminals / stale-window request, or a session that already runs a Claude),
    /// and it used to hand the args to the session constructor untouched — which then
    /// prepended its own freshly minted `--session-id`. Beside a `--resume` or
    /// an `attach` that is an argv the CLI rejects outright (`--session-id can
    /// only be used with --continue or --resume if --fork-session is also
    /// specified`), so the window printed one line and died. A request that names
    /// a claude session therefore keeps ITS id and splices none.
    ///
    /// The attach rewrite carries no wrapper-style `|| --resume <uuid>`
    /// fallback: the session constructor execs a single command line, and the probe
    /// it rests on ran milliseconds earlier in this same handler (not hours ago
    /// at prefill time, which is what makes the in-place fallback worth its
    /// complexity).
    fn plan_newtab_claude_exec(&self, args: &[String]) -> (Vec<String>, ClaudeSessionSpec) {
        match self.resolve_claude_session(args) {
            ResolvedClaudeSession::Attach { short_id, uuid } => (
                vec!["attach".to_string(), short_id.clone()],
                ClaudeSessionSpec {
                    mode: ClaudeSessionMode::Attach(short_id),
                    pin: Some(uuid),
                },
            ),
            ResolvedClaudeSession::Resume { uuid } => (
                vec!["--resume".to_string(), uuid.clone()],
                ClaudeSessionSpec {
                    mode: ClaudeSessionMode::Resume(uuid.clone()),
                    pin: Some(uuid),
                },
            ),
            ResolvedClaudeSession::AsTyped { pin } => {
                // `attach <short id>` passes through as the SUBCOMMAND mode, not
                // as trailing args: the exec builder would otherwise emit the
                // theme `--settings` ahead of it, which stops the CLI from
                // seeing a subcommand at all.
                let mode = match args.first().map(String::as_str) {
                    Some("attach") => ClaudeSessionMode::Attach(
                        args.get(1).cloned().unwrap_or_default(),
                    ),
                    _ => ClaudeSessionMode::None,
                };
                (args.to_vec(), ClaudeSessionSpec { mode, pin })
            }
            ResolvedClaudeSession::Unnamed => (args.to_vec(), ClaudeSessionSpec::mint()),
        }
    }

    /// Resolve what session an intercepted `claude` argv names, probing the
    /// daemon's jobs directory for the two shapes that name one. The single
    /// source of truth behind [`plan_claude_exec`](Self::plan_claude_exec) and
    /// [`plan_newtab_claude_exec`](Self::plan_newtab_claude_exec).
    fn resolve_claude_session(&self, args: &[String]) -> ResolvedClaudeSession {
        match classify_claude_session_args(args) {
            // `--resume <uuid>` for a session the daemon still hosts: resuming
            // would spawn a SECOND process against a live conversation.
            ClaudeArgSession::Resume(id) if self.daemon_hosts_claude_session(&id) => {
                ResolvedClaudeSession::Attach {
                    short_id: short_job_id(&id),
                    uuid: id,
                }
            }
            ClaudeArgSession::Resume(id) => ResolvedClaudeSession::AsTyped { pin: Some(id) },
            ClaudeArgSession::Attach(id) if looks_like_session_uuid(&id) => {
                if self.daemon_hosts_claude_session(&id) {
                    ResolvedClaudeSession::Attach {
                        short_id: short_job_id(&id),
                        uuid: id,
                    }
                } else {
                    ResolvedClaudeSession::Resume { uuid: id }
                }
            }
            ClaudeArgSession::Attach(id) => ResolvedClaudeSession::AsTyped {
                // Pin the job's full uuid when `state.json` still maps this
                // short id (so the session resumes durably after a relaunch), and
                // nothing at all when it does not. The `starts_with` guard is
                // the short-id twin of `daemon_hosts_claude_session`'s equality check:
                // a jobs entry naming some other uuid is not this session.
                pin: (self.fork_job_probe)(&id)
                    .and_then(|job| job.claude_session_id)
                    .filter(|uuid| uuid.starts_with(&id)),
            },
            ClaudeArgSession::Neither => match WorkspaceModel::extract_claude_session_id(args) {
                Some(id) => ResolvedClaudeSession::AsTyped { pin: Some(id) },
                None => ResolvedClaudeSession::Unnamed,
            },
        }
    }

    /// Whether the Claude daemon is hosting `claude_session_id` as a background job:
    /// a `~/.claude/jobs/<first8>/state.json` whose `sessionId` is EXACTLY this
    /// uuid. The equality is the first-8 collision guard — a jobs entry is
    /// keyed by 8 hex characters, so a foreign job can share the prefix, and
    /// attaching to it would drop the user into someone else's conversation.
    /// A jobs directory whose `state.json` has not landed (or does not parse)
    /// therefore reads as "not hosted" and keeps the durable `--resume`.
    fn daemon_hosts_claude_session(&self, claude_session_id: &str) -> bool {
        matches!(
            (self.fork_job_probe)(claude_session_id),
            Some(job) if job.claude_session_id.as_deref() == Some(claude_session_id)
        )
    }

    /// The `~/.claude/jobs/<first8>/` entry `claude_session_id` belongs to, or `None`
    /// when the id names no background job. A `Some` is the "this SessionStart
    /// came from the daemon, not from the window it names" signal
    /// ([`apply_session_update`](Self::apply_session_update)).
    ///
    /// Deliberately looser than
    /// [`daemon_hosts_claude_session`](Self::daemon_hosts_claude_session), which answers "is
    /// the daemon hosting it RIGHT NOW" for the exec-time attach/resume choice: a
    /// jobs directory whose `state.json` has not landed yet still counts here,
    /// because a background fork's SessionStart can beat the daemon's write (the
    /// `tmp/`-only `298689bf` evidence) and that relay is exactly the one that
    /// must not rotate a session. Where `state.json` IS readable its `sessionId` must
    /// match — the same first-8 collision guard, so a foreign job sharing the
    /// prefix can never silence a genuine in-window rotation.
    fn daemon_job_for(&self, claude_session_id: &str) -> Option<ForkJobInfo> {
        (self.fork_job_probe)(claude_session_id)
            .filter(|job| job.claude_session_id.as_deref().is_none_or(|id| id == claude_session_id))
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
    /// The SessionStart hook relays a window's rotated session id / cwd; this records
    /// the normalized message, runs the pure rotation flow
    /// ([`apply_session_update`](Self::apply_session_update)), and returns its
    /// outcome — the deferred-resume [`BranchParentSpawn`] for the router to
    /// fulfil with `cx` when the rotation classified as a `/branch`, or the
    /// [`BackgroundFork`] hand-off when it classified as a daemon-hosted `/fork`.
    ///
    /// Context-free itself (fire-and-forget: R14's transport dropped the client fd
    /// BEFORE dispatch, so the handler never replies). The gpui-context spawn lives
    /// in [`spawn_branch_parent`](Self::spawn_branch_parent) — the mirror of the
    /// `claude` handler's [`resolve_claude_request`](Self::resolve_claude_request) /
    /// spawn split.
    fn handle_session_update(
        &mut self,
        term_window_id: String,
        claude_session_id: String,
        source: Option<String>,
        cwd: Option<String>,
    ) -> SessionUpdateOutcome {
        self.record_socket_message(RecordedSocketMessage::SessionUpdate {
            term_window_id: term_window_id.clone(),
            claude_session_id: claude_session_id.clone(),
            source: source.clone(),
            cwd: cwd.clone(),
        });
        self.apply_session_update(&term_window_id, &claude_session_id, source.as_deref(), cwd.as_deref())
    }

    /// The pure model half of a `session_update` — the rotation flow per the
    /// PROTECTED ordering (`SessionsModel.swift:946-963`), unit-testable without a
    /// gpui context. **Background-fork gate first** (below) → resolve the owning
    /// session by window (stale/unknown window ⇒ silent no-op) → capture `old_id` →
    /// `update_claude_session_id` (equality short-circuit: a redundant forward
    /// mutates nothing) → **iff `source ∈ {"resume", "fork"}` && `old_id` exists
    /// && `old_id != claude_session_id`: materialize the branch parent, BEFORE the
    /// cwd update** (so the sibling inherits the pre-rotation cwd) →
    /// `update_session_cwd` (None/empty filtered). An unknown/absent source with an
    /// id change is a plain id update, NEVER a parent (deliberately miss an
    /// occasional `/branch` rather than spawn a phantom parent from a
    /// mis-classified `/clear`).
    ///
    /// `"fork"` joined `"resume"` in the branch arm because Claude Code 2.1.214
    /// changed what an in-window rotation reports: `/branch` and `--fork-session`
    /// resumes now relay `source: "fork"`, so a `resume`-only gate silently drops
    /// the pre-branch conversation from the sidebar (bug 2). Older CLIs still say
    /// `"resume"`, so both stay in.
    ///
    /// The same `"fork"` source ALSO reaches us from the Claude daemon's detached
    /// background `/fork` child, whose relayed window id belongs to whichever
    /// window first spawned the daemon — rotating that window's session is what
    /// corrupted an unrelated session's claude session id (bug 3). The jobs-entry
    /// probe ([`daemon_job_for`](Self::daemon_job_for)) separates the two, and it
    /// runs on EVERY source before anything is read or written: the daemon relays a
    /// stale window id for a background job's whole life, not just at its birth (a
    /// cold `claude attach` wakes the job and its SessionStart says `"resume"`).
    /// A daemon-owned id must touch no session at all, so this cannot sit
    /// downstream of the id update.
    ///
    /// Returns whether anything changed (the R18 save signal — `onSessionMutation`;
    /// nothing persists yet) plus the deferred-resume spawn the caller owes, or
    /// the [`BackgroundFork`] hand-off.
    fn apply_session_update(
        &mut self,
        term_window_id: &str,
        claude_session_id: &str,
        source: Option<&str>,
        cwd: Option<&str>,
    ) -> SessionUpdateOutcome {
        // Daemon-relayed SessionStart: a jobs entry for the incoming id means the
        // Claude daemon runs this claude session, so the relayed window id is
        // whichever window happened to spawn the daemon and NOTHING here may be
        // acted on. Probed for EVERY source, not just `"fork"` — a cold-woken
        // background job fires SessionStart with `source: "resume"`, and a
        // `"fork"`-only gate let that relay fall straight through to the rotation
        // path below and rewrite an unrelated session (bug 3, via `claude
        // attach`). Probed before the window is even resolved — a daemon relay
        // whose stale window has since closed is still a daemon relay, and
        // resolving that window would only tempt a later edit into writing to the
        // wrong session.
        if let Some(job) = self.daemon_job_for(claude_session_id) {
            return SessionUpdateOutcome {
                did_mutate: false,
                spawn: None,
                // `"fork"` is the job's BIRTH — the one relay that owes the
                // sidebar a new entry (Fix B). Every other source is a later
                // life-cycle event (a wake, a resume, the daemon's own restart)
                // for a job that already has its session, so it changes nothing at
                // all: re-materializing would just duplicate that session.
                background_fork: (source == Some("fork")).then(|| BackgroundFork {
                    fork_claude_session_id: claude_session_id.to_string(),
                    cwd: cwd.filter(|c| !c.is_empty()).map(str::to_string),
                    job,
                }),
            };
        }
        let Some(session_id) = self.workspace.session_id_owning(term_window_id) else {
            return SessionUpdateOutcome::default();
        };
        let old_id = self
            .workspace
            .session_for(&session_id)
            .and_then(|s| s.claude_session_id.clone());
        let id_changed = self.update_claude_session_id(&session_id, claude_session_id);
        // /branch classification: a `resume` / `fork` source with an ACTUAL id
        // change is the signature of `/branch` and `--fork-session`. Real
        // `/resume` keeps the id stable (absorbed by the short-circuit above),
        // `/clear` reports `source == "clear"`, and a nil/unknown source is
        // treated as a plain id update. Materialize BEFORE the cwd update so the
        // sibling parent inherits the pre-rotation cwd.
        let spawn = if matches!(source, Some("resume") | Some("fork")) {
            match &old_id {
                Some(old) if old != claude_session_id => self.materialize_branch_parent(&session_id, old),
                _ => None,
            }
        } else {
            None
        };
        // Apply cwd to the ORIGINATING session only — after branch materialization, so
        // the sibling parent keeps the pre-rotation cwd.
        let cwd_changed = self.update_session_cwd(&session_id, cwd);
        SessionUpdateOutcome {
            did_mutate: id_changed || spawn.is_some() || cwd_changed,
            spawn,
            background_fork: None,
        }
    }

    /// Update `session.claude_session_id` when Claude rotates its session mid-process
    /// (`SessionsModel.swift:972-984`). Equality short-circuit: a redundant forward
    /// (the hook fires on every SessionStart — this cheapness contract keeps a
    /// steady stream of identical ids from churning the save layer) mutates
    /// nothing. Returns whether the id actually changed.
    ///
    /// R18: the real `onSessionMutation` save flush hangs off this `true` return;
    /// nothing persists yet (the outcome's `did_mutate` is the standin the tests
    /// assert on).
    fn update_claude_session_id(&mut self, session_id: &str, claude_session_id: &str) -> bool {
        let mut changed = false;
        self.workspace.mutate_session(session_id, |session| {
            if session.claude_session_id.as_deref() != Some(claude_session_id) {
                session.claude_session_id = Some(claude_session_id.to_string());
                changed = true;
            }
        });
        changed
    }

    /// Adopt Claude's reported cwd onto the originating session (`SessionsModel.swift:
    /// 1001-1009`): the None/empty shapes short-circuit (an older hook payload
    /// omitting cwd, or a defensive empty string), else the actual mutation +
    /// per-window follow policy lives on [`WorkspaceModel::adopt_session_cwd`]. Returns whether
    /// anything changed (the R18 save signal — nothing persists yet).
    fn update_session_cwd(&mut self, session_id: &str, cwd: Option<&str>) -> bool {
        match cwd {
            Some(c) if !c.is_empty() => self.workspace.adopt_session_cwd(session_id, c),
            _ => false,
        }
    }

    /// Materialize the pre-`/branch` session as a sibling parent session pinned to
    /// `old_session_id`, inserted immediately above the originating session
    /// (`SessionsModel.swift:1031-1065`). Composes landed pieces: mint the session +
    /// `-claude`/`-t1` window ids, hand the model the tree mutation
    /// ([`WorkspaceModel::insert_branch_parent`], which refuses a Terminals/unknown
    /// originating session ⇒ `None`, and does the depth-1 root-promotion re-parenting),
    /// then return the deferred-resume spawn the caller owes.
    ///
    /// The parent's cwd is read from the returned-by-value node HERE, before the
    /// caller's [`update_session_cwd`](Self::update_session_cwd) moves the originating session
    /// into the post-rotation worktree: `insert_branch_parent` copied
    /// `originating.cwd` at insertion, so `parent.cwd` is the PRE-rotation cwd —
    /// which is what the sibling's `claude --resume <old id>` needs (the old-id
    /// transcript is bucketed under the pre-rotation path). Rust's by-value return
    /// makes the ordering structural; the ported cwd-move test pins it anyway.
    fn materialize_branch_parent(
        &mut self,
        originating_session_id: &str,
        old_session_id: &str,
    ) -> Option<BranchParentSpawn> {
        let new_id = self.ptys.mint_session_id("t");
        let claude_window_id = format!("{new_id}-claude");
        let terminal_window_id = format!("{new_id}-t1");
        let parent = self.workspace.insert_branch_parent(
            originating_session_id,
            &new_id,
            &claude_window_id,
            &terminal_window_id,
            old_session_id,
        )?;
        Some(BranchParentSpawn {
            session_id: new_id,
            claude_window_id,
            cwd: parent.cwd,
            old_session_id: old_session_id.to_string(),
        })
    }

    /// Fulfil a [`BranchParentSpawn`]: register the parent's (empty) session
    /// container so its deferred companion's later
    /// [`ensure_active_window_spawned`](crate::pty_manager::PtyManager::ensure_active_window_spawned)
    /// precondition holds, then spawn the parent's Claude window in
    /// [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred) mode — a plain login
    /// shell carrying `claude --resume <old id>` as `NICE_PREFILL_COMMAND` (nothing
    /// resumes, and no tokens are spent, until the user opens the parent session and
    /// presses Enter). Fire-and-forget: a spawn failure degrades to a model-only
    /// recovery session (the tree mutation already landed), so it is logged-and-swallowed
    /// like the rest of the rotation feature.
    fn spawn_branch_parent(&mut self, spawn: BranchParentSpawn, cx: &mut gpui::Context<WindowState>) {
        self.spawn_deferred_resume_window(
            &spawn.session_id,
            &spawn.claude_window_id,
            &spawn.cwd,
            spawn.old_session_id,
            cx,
        );
        // Re-render so the sidebar shows the new sibling parent + re-parented child.
        cx.notify();
    }

    /// Give a just-inserted session its deferred-resume Claude window: register
    /// the (empty) pty container so the session's later
    /// [`ensure_active_window_spawned`](crate::pty_manager::PtyManager::ensure_active_window_spawned)
    /// precondition holds, then spawn the window in
    /// [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred) mode — a plain login
    /// shell carrying `claude --resume <claude session id>` as `NICE_PREFILL_COMMAND`
    /// (nothing resumes, and no tokens are spent, until the user opens the session
    /// and presses Enter). Fire-and-forget: a spawn failure degrades to a model-only
    /// recovery session (the tree mutation already landed), so it is
    /// logged-and-swallowed like the rest of the rotation feature.
    ///
    /// Shared by the `/branch` parent ([`spawn_branch_parent`](Self::spawn_branch_parent))
    /// and the background-`/fork` child
    /// ([`insert_background_fork_child`](Self::insert_background_fork_child)) — the
    /// two differ only in which session they hang off and which id they pin, never
    /// in how the deferred window is brought up.
    fn spawn_deferred_resume_window(
        &mut self,
        session_id: &str,
        claude_window_id: &str,
        cwd: &str,
        claude_session_id: String,
        cx: &mut gpui::Context<WindowState>,
    ) {
        self.ptys.register_session_pty(session_id);
        let settings = self.claude_settings_path.clone();
        let _ = self.ptys.spawn_claude_window(
            session_id,
            claude_window_id,
            cwd,
            &ClaudeSessionMode::ResumeDeferred(claude_session_id),
            &[],
            settings.as_deref(),
            cx,
        );
    }

    /// **Fix B.** Materialize a daemon-hosted background `/fork`
    /// ([`BackgroundFork`]) as its own sidebar entry: a nested, UNSELECTED child
    /// session under the session whose conversation was forked, pinned to the
    /// fork's claude session id and carrying a deferred `claude --resume <fork id>`
    /// prefill.
    ///
    /// Everything happens on a spawned foreground task, for two reasons that both
    /// forbid doing it inline:
    ///
    /// * **`state.json` may not exist yet.** The daemon creates
    ///   `~/.claude/jobs/<first8>/` BEFORE spawning the fork child, so the child's
    ///   SessionStart hook can beat the file that names
    ///   [`fork_parent_session_id`](ForkJobInfo::fork_parent_session_id) — the only
    ///   key we can resolve the parent session by. The task re-probes on a short poll
    ///   ([`FORK_STATE_POLL_ATTEMPTS`] × [`FORK_STATE_POLL_INTERVAL`]) and then
    ///   gives up SILENTLY: an aborted fork (the daemon writes `tmp/` and dies)
    ///   must leave no trace in the sidebar.
    /// * **The parent session may live in another OS window.** `session_update` is
    ///   routed to the window owning the relayed window id, and for a background fork
    ///   that id is whichever window first spawned the daemon — it says NOTHING about
    ///   where the forked conversation is open. Reading another window's
    ///   [`WindowState`] means reading another entity, which is illegal while this
    ///   one is leased by the routing `update`; off the task no lease is held.
    fn materialize_background_fork(
        &mut self,
        fork: BackgroundFork,
        cx: &mut gpui::Context<WindowState>,
    ) {
        cx.spawn(async move |this: gpui::WeakEntity<WindowState>, acx: &mut gpui::AsyncApp| {
            let mut fork = fork;
            // 1. The parent session id, re-probing while `state.json` is missing.
            let mut attempts = 0usize;
            let parent_claude_session_id = loop {
                if let Some(parent) = fork.job.fork_parent_session_id.clone() {
                    break parent;
                }
                if attempts >= FORK_STATE_POLL_ATTEMPTS {
                    return; // never landed — an aborted fork, drop it silently
                }
                attempts += 1;
                acx.background_executor().timer(FORK_STATE_POLL_INTERVAL).await;
                match this.update(acx, |ws, _cx| (ws.fork_job_probe)(&fork.fork_claude_session_id)) {
                    // The jobs entry itself is gone: the daemon cleaned up an
                    // aborted job, so there is no fork left to materialize.
                    Ok(None) => return,
                    Ok(Some(job)) => fork.job = job,
                    Err(_) => return, // this window is gone
                }
            };

            // 2. The OS window holding the forked-from session: THIS one first (the
            //    common case — the daemon usually inherited a window id from the same
            //    OS window), then every other live window.
            let owner = match this
                .update(acx, |ws, _cx| {
                    ws.workspace.session_id_for_claude_session(&parent_claude_session_id).is_some()
                }) {
                Ok(true) => this.upgrade(),
                Ok(false) => acx.update(|app| {
                    crate::window_registry::WindowRegistry::state_for_claude_session(
                        app,
                        &parent_claude_session_id,
                    )
                }),
                Err(_) => return, // this window is gone
            };
            // No session anywhere is pinned to the forked-from conversation (it was
            // never open in Nice, or its session has since closed) ⇒ drop silently.
            let Some(owner) = owner else { return };

            owner.update(acx, |ws, cx| {
                if ws.insert_background_fork_child(&fork, &parent_claude_session_id, cx) {
                    // Re-render so the sidebar shows the new nested child.
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Insert `fork` as a nested child of the session in THIS window pinned to
    /// `parent_claude_session_id`, and bring up its deferred-resume Claude window. Returns
    /// whether the child actually landed.
    ///
    /// Shape (the handoff shape, not the `/branch` one): built with
    /// [`WorkspaceModel::insert_handoff_child`], which nests one indent under the parent
    /// WITHOUT re-parenting the parent's existing children, and — decisively — is
    /// never selected. The foreground conversation the user forked from keeps
    /// selection and key focus; the fork is a background offshoot, exactly like a
    /// `/nice-handoff` session.
    ///
    /// Field sources:
    /// * **claude session id** — the fork's own id, pinned before insertion (the ONE
    ///   field `insert_handoff_child` leaves alone is `parent_session_id`; everything
    ///   else is inserted verbatim), so the deferred prefill opens that exact
    ///   conversation.
    /// * **cwd** — the relayed cwd when non-empty, else the parent session's. Since
    ///   Claude Code 2.1.220 a fork can relocate into its own worktree, so the
    ///   relayed value is NOT redundant with the parent's.
    /// * **title** — the job's `name` when present (it carries the `⑂` fork
    ///   marker), locked against Claude's OSC auto-title so resuming the fork can't
    ///   erase the marker; else the parent's title with the parent's title flags,
    ///   the same inheritance [`WorkspaceModel::insert_branch_parent`] does.
    ///
    /// `false` when a session here is already pinned to the fork's own id (the
    /// entry exists — a repeat relay must not double it), when this window has no
    /// session pinned to `parent_claude_session_id`, or when the anchor refuses the child
    /// (an unknown session, or one in the pinned Terminals group) — each drops the
    /// fork silently, per Fix B step 3.
    fn insert_background_fork_child(
        &mut self,
        fork: &BackgroundFork,
        parent_claude_session_id: &str,
        cx: &mut gpui::Context<WindowState>,
    ) -> bool {
        // Idempotence: a session already pinned to this fork IS the fork's sidebar
        // entry. A second `"fork"` relay for the same id (the daemon respawning a
        // woken job with its `respawnFlags`) must not mint a rival session — two
        // sessions claiming one conversation is the shape bug 3 produced.
        if self.workspace.session_id_for_claude_session(&fork.fork_claude_session_id).is_some() {
            return false;
        }
        let Some(parent_session_id) = self.workspace.session_id_for_claude_session(parent_claude_session_id)
        else {
            return false;
        };
        // Owned copies so the immutable model borrow ends before the mutable insert.
        let Some((parent_title, parent_cwd, title_auto, title_manual)) =
            self.workspace.session_for(&parent_session_id).map(|s| {
                (
                    s.title.clone(),
                    s.cwd.clone(),
                    s.title_auto_generated,
                    s.title_manually_set,
                )
            })
        else {
            return false;
        };

        let cwd = fork.cwd.clone().unwrap_or(parent_cwd);
        let named = fork.job.name.clone().filter(|n| !n.is_empty());

        let session_id = self.ptys.mint_session_id("t");
        let claude_window_id = format!("{session_id}-claude");
        let terminal_window_id = format!("{session_id}-t1");
        let mut claude_window = TermWindow::new(&claude_window_id, "Claude", TermWindowKind::Claude);
        // Deferred resume: nothing is running in this window until the user opens
        // the session and presses Enter (the /branch parent's shape).
        claude_window.is_claude_running = false;
        let mut session = Session::new(&session_id, named.clone().unwrap_or(parent_title), &cwd);
        session.windows = vec![
            claude_window,
            TermWindow::new(&terminal_window_id, "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(claude_window_id.clone());
        session.claude_session_id = Some(fork.fork_claude_session_id.clone());
        session.title_auto_generated = if named.is_some() { false } else { title_auto };
        session.title_manually_set = if named.is_some() { true } else { title_manual };
        session.next_terminal_index = 2;

        if !self.workspace.insert_handoff_child(session, &parent_session_id) {
            return false;
        }
        self.spawn_deferred_resume_window(
            &session_id,
            &claude_window_id,
            &cwd,
            fork.fork_claude_session_id.clone(),
            cx,
        );
        true
    }

    /// Handle a `handoff` request from the `/nice-handoff` skill's helper — the
    /// Rust twin of Swift `SessionsModel.handleHandoffRequest`
    /// (`SessionsModel.swift:1108-1156`). Opens a fresh Claude session pre-loaded with
    /// the handoff notes: nested one indent under the originating session, or top-level
    /// on a resolution miss, and ALWAYS replies `ok` (D3).
    ///
    /// The originating session is resolved exactly as the `claude` request does
    /// ([`resolve_claude_request`](Self::resolve_claude_request)): a non-empty id,
    /// NOT in the Terminals group, present in the model, AND owning the sending
    /// window. A miss is NOT an error — a handoff from the Main Terminal (or a stale
    /// window id) must still open a session — so it falls back to a top-level insert
    /// (unlike the `claude` in-place-promotion path, where a miss opens a newtab
    /// too but never nests). Mirrors the `claude` arm's spawn shape (D6): borrow
    /// settings/model/session, build + spawn through
    /// [`create_nested_claude_session`](crate::pty_manager::PtyManager::create_nested_claude_session),
    /// then `cx.notify()`.
    ///
    /// **Focus (D7): the new session opens UNSELECTED.** `create_nested_claude_session` does not
    /// select it, so the originating session stays active and keyboard focus never
    /// moves — a handoff is background continuation prep, not a context switch.
    /// That is the ONE behavioral split from
    /// [`handle_claude_socket_request`](Self::handle_claude_socket_request), whose
    /// terminal-`claude` newtab still selects (and still syncs the active id).
    /// Only `cx.notify()` is needed here: nothing changed the active session, so the
    /// selection needs no re-sync.
    #[allow(clippy::too_many_arguments)]
    fn handle_handoff(
        &mut self,
        cwd: String,
        handoff_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
        reply: Reply,
        cx: &mut gpui::Context<WindowState>,
    ) {
        self.record_socket_message(RecordedSocketMessage::Handoff {
            cwd: cwd.clone(),
            handoff_file: handoff_file.clone(),
            instructions: instructions.clone(),
            model: model.clone(),
            effort: effort.clone(),
            session_id: session_id.clone(),
            term_window_id: term_window_id.clone(),
        });

        // Resolve the originating session (owned clones so the immutable model borrow
        // ends before the mutable spawn borrow). A miss ⇒ `None` fields, which
        // steer the session top-level (D3).
        let (originating_id, originating_title, spawn_cwd) = {
            let originating = if !session_id.is_empty()
                && !self.workspace.is_terminals_project_session(&session_id)
            {
                self.workspace
                    .session_for(&session_id)
                    .filter(|s| s.windows.iter().any(|w| w.id == term_window_id))
            } else {
                None
            };
            (
                originating.map(|s| s.id.clone()),
                originating.map(|s| s.title.clone()),
                // Prefer the resolved session's live cwd (it may have moved into a
                // worktree); else the payload cwd.
                originating.map(|s| s.cwd.clone()).unwrap_or(cwd),
            )
        };

        let title = crate::pty_manager::handoff_title(originating_title.as_deref());
        let prompt = crate::pty_manager::handoff_prompt(&handoff_file, &instructions);
        // Nest under the RESOLVED originating session, never the raw payload `session_id`:
        // on a miss we pass "" so `insert_handoff_child` rejects it and the session
        // opens top-level, keeping nesting coherent with the title/cwd (which
        // already key off the resolved session).
        let under = originating_id.unwrap_or_default();

        // (D5) --model/--effort (each omitted when empty) then the prompt LAST.
        let extra_args = crate::pty_manager::handoff_extra_args(&model, &effort, &prompt);

        let settings = self.claude_settings_path.clone();
        let workspace = &mut self.workspace;
        let ptys = &mut self.ptys;
        let created = ptys.create_nested_claude_session(
            workspace,
            &under,
            &spawn_cwd,
            title,
            &extra_args,
            settings.as_deref(),
            cx,
        );
        if created.is_some() {
            // Re-render so the nested / top-level session appears in the sidebar.
            // Deliberately NO `sync_active_session_id` (unlike the `claude` arm): the
            // handoff session opens UNSELECTED (D7), so the active session is untouched
            // and the "selection ⊇ {active session}" invariant already holds.
            cx.notify();
        }
        // The session opened (nested or top-level) — ALWAYS reply `ok`. Swift's only
        // hard error ("no window") cannot occur for a live WindowState.
        reply.send("ok");
    }

    /// Handle a `dispatch` request from the `/nice-dispatch` skill's helper: open
    /// a fresh Claude session that creates + enters a git worktree
    /// (`claude --worktree <name>`) and starts working from the task file the
    /// dispatcher wrote. Modelled on [`handle_handoff`](Self::handle_handoff) —
    /// nested one indent under the originating session, opened UNSELECTED, ALWAYS
    /// replying `ok` — with two deliberate deltas:
    ///
    /// * **The spawn cwd is ALWAYS the payload `cwd`** (the MAIN checkout root the
    ///   helper resolved via `--git-common-dir`), never the originating session's live
    ///   cwd. A dispatcher running inside a worktree must still create the new
    ///   worktree from the canonical checkout. The originating session is resolved
    ///   purely for NESTING; a miss (Main Terminals session, stale id, a window the session
    ///   doesn't own) is NOT an error — the session opens top-level.
    /// * **Model/effort are NOT inherited.** They arrive empty unless the user
    ///   explicitly asked for an override, and
    ///   [`dispatch_extra_args`](crate::pty_manager::dispatch_extra_args) then
    ///   omits the flags so the child runs on the configured default.
    ///
    /// Nesting nuance (the existing depth-1 invariant, deliberately unchanged):
    /// `insert_handoff_child` re-parents to the originating session's PARENT when the
    /// dispatcher is itself a nested child, so dispatching from a handoff-born
    /// dispatcher yields siblings under that parent, not grandchildren.
    #[allow(clippy::too_many_arguments)]
    fn handle_dispatch(
        &mut self,
        cwd: String,
        worktree_name: String,
        task_file: String,
        instructions: String,
        model: String,
        effort: String,
        session_id: String,
        term_window_id: String,
        reply: Reply,
        cx: &mut gpui::Context<WindowState>,
    ) {
        self.record_socket_message(RecordedSocketMessage::Dispatch {
            cwd: cwd.clone(),
            worktree_name: worktree_name.clone(),
            task_file: task_file.clone(),
            instructions: instructions.clone(),
            model: model.clone(),
            effort: effort.clone(),
            session_id: session_id.clone(),
            term_window_id: term_window_id.clone(),
        });

        // Resolve the originating session for NESTING ONLY (owned clone so the
        // immutable model borrow ends before the mutable spawn borrow). Same
        // predicate as `handle_handoff`: non-empty id, not the Terminals group,
        // present in the model, and owning the sending window.
        let originating_id = {
            let originating =
                if !session_id.is_empty() && !self.workspace.is_terminals_project_session(&session_id) {
                    self.workspace
                        .session_for(&session_id)
                        .filter(|s| s.windows.iter().any(|w| w.id == term_window_id))
                } else {
                    None
                };
            originating.map(|s| s.id.clone())
        };
        // On a miss, "" makes `insert_handoff_child` reject the anchor and the session
        // opens top-level.
        let under = originating_id.unwrap_or_default();

        let title = crate::pty_manager::dispatch_title(&worktree_name);
        let prompt = crate::pty_manager::dispatch_prompt(&task_file, &instructions);
        let extra_args = crate::pty_manager::dispatch_extra_args(
            &worktree_name,
            &task_file,
            &model,
            &effort,
            &prompt,
        );

        let settings = self.claude_settings_path.clone();
        let workspace = &mut self.workspace;
        let ptys = &mut self.ptys;
        // The payload cwd, NOT the originating session's: worktrees are always created
        // from the main checkout. The session then follows Claude into the worktree via
        // the existing `session_update` cwd-follow.
        let created = ptys.create_nested_claude_session(
            workspace,
            &under,
            &cwd,
            title,
            &extra_args,
            settings.as_deref(),
            cx,
        );
        if created.is_some() {
            // Re-render only: the dispatch session opens UNSELECTED, so the active session
            // is untouched and the "selection ⊇ {active session}" invariant holds.
            cx.notify();
        }
        // ALWAYS `ok` — as for handoff, no hard error can occur on a live
        // WindowState (the `error: …` reply shape belongs to the helper's own
        // no-reply / socket-failure paths).
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
    /// the window's exact tabId/paneId/cwd and a raw-`UnixStream` `session_update`
    /// surfaced normalized.
    pub(crate) fn recorded_socket_messages(&self) -> &[RecordedSocketMessage] {
        &self.recorded_socket_messages
    }

    // MARK: - W5 quit / window-close (R18)

    /// This window's live-window counts `(claude, terminal)` — the quit / close
    /// confirmation counting rule ([`nice_model::WorkspaceModel::live_window_counts`]).
    pub(crate) fn live_window_counts(&self) -> (usize, usize) {
        self.workspace.live_window_counts()
    }

    /// Whether the user explicitly closed this window (red button / ⌘W) — read by
    /// [`crate::window_registry::WindowRegistry::handle_window_closed`] to route
    /// the disk fate. Swift's `AppState.userInitiatedClose`.
    pub(crate) fn user_initiated_close(&self) -> bool {
        self.user_initiated_close
    }

    /// Flip the user-initiated-close flag (the confirmed red-button / ⌘W path, or
    /// the no-live-windows unconditional close). Only ever set to `true`; a window
    /// that stays open (Cancel) leaves it `false`.
    pub(crate) fn set_user_initiated_close(&mut self, value: bool) {
        self.user_initiated_close = value;
    }

    /// When a close scope emptied the WHOLE window ([`DissolveTerminus::WindowEmptied`]),
    /// mark it `user_initiated_close` so the close observer DROPS its disk slot
    /// rather than preserving an empty snapshot that would restore as a broken
    /// empty window next launch (mirrors the no-live-windows ⌘W close in
    /// [`crate::app::request_window_close`]). Called at every terminus-MINT site —
    /// the `close_*_via_session` methods and the pty-exit subscription — on the
    /// already-held `&mut self`. It must be set HERE, never from
    /// [`PtyManager::apply_dissolve_terminus`]: that actuator runs mid-update
    /// on the UI-close paths, so touching this `WindowState` there would re-lease
    /// the entity and abort (the crash this whole change removed).
    pub(crate) fn mark_removed_if_window_emptied(&mut self, terminus: DissolveTerminus) {
        if terminus == DissolveTerminus::WindowEmptied {
            self.user_initiated_close = true;
        }
    }

    /// The persisted snapshot of this window for the session store — id from the
    /// window's [`window_session_id`](Self::window_session_id) (the persisted window id; a fresh
    /// / ⌘N window mints a UUID, a restored one keeps its saved id),
    /// `sidebar_collapsed` from the live sidebar, projects via
    /// [`nice_model::snapshot_projects`] (empty non-Terminals projects dropped),
    /// and the W6 [`last_frame`](Self::last_frame) captured from the bounds
    /// observer (`None` until the first observation ⇒ default placement).
    pub(crate) fn persisted_snapshot(&self) -> crate::session_store::PersistedWindow {
        crate::session_store::PersistedWindow {
            id: self.window_session_id.clone(),
            active_session_id: self.workspace.active_session_id().map(|s| s.to_string()),
            sidebar_collapsed: self.sidebar.collapsed(),
            // R19: persist the live sidebar mode so a restored window reopens in
            // the mode it was last in (absent on decode ⇒ Sessions).
            sidebar_mode: Some(self.sidebar.mode()),
            // Phase 0: the user-resized sidebar width; None (⇒ key absent) while
            // never customized.
            sidebar_width: self.sidebar.width().map(f64::from),
            projects: nice_model::snapshot_projects(&self.workspace.projects),
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
    /// how every quit/close silently died: an idle shell keeps a window alive, so all
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
        // still counts as a live window). The terminal drain carries this exact kick;
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

    /// R19: drop the file-browser state of every session dissolved since the last
    /// drain (the session cascade records them; see
    /// [`PtyManager::take_dissolved_session_ids`]). Called after every cascade so a
    /// long session doesn't accumulate stale per-session browser states — the single
    /// removal path for [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore).
    fn prune_dissolved_file_browser_states(&mut self) {
        for session_id in self.ptys.take_dissolved_session_ids() {
            self.file_browser.remove_state(&session_id);
        }
    }

    /// Real close of a session through the session manager (pty release + dissolve
    /// cascade) — the shipped-path replacement for the model-only
    /// `SidebarActions::close_session` stub. Returns the terminus the caller actuates
    /// via [`PtyManager::apply_dissolve_terminus`], and schedules the dissolve
    /// save.
    pub(crate) fn close_session_via_pty_manager(&mut self, session_id: &str) -> DissolveTerminus {
        let terminus = self
            .ptys
            .close_session(&mut self.workspace, &mut self.selection, session_id);
        self.prune_dissolved_file_browser_states();
        self.mark_removed_if_window_emptied(terminus);
        self.save_to_store();
        terminus
    }

    /// Real close of a batch of sessions (the "Close N Tabs" path). Aggregates each
    /// session's terminus; schedules a single save at the end.
    pub(crate) fn close_sessions_via_pty_manager(&mut self, session_ids: &[String]) -> DissolveTerminus {
        let mut terminus = DissolveTerminus::None;
        for id in session_ids {
            terminus = terminus.or(self.ptys.close_session(&mut self.workspace, &mut self.selection, id));
        }
        self.prune_dissolved_file_browser_states();
        self.mark_removed_if_window_emptied(terminus);
        self.save_to_store();
        terminus
    }

    /// Real close of a whole project (the sidebar "Close Project" path), porting
    /// Swift's `CloseRequestCoordinator.hardKillProject` (`:369-389`): the pinned
    /// Terminals group is never closed; an already-empty non-Terminals project row
    /// drops directly; otherwise the project is marked pending-removal and each of
    /// its sessions is hard-closed — the last dissolve drops the now-empty row
    /// ([`PtyManager::finalize_dissolved_session`]). Schedules the dissolve save.
    pub(crate) fn close_project_via_session(&mut self, project_id: &str) -> DissolveTerminus {
        if project_id == WorkspaceModel::TERMINALS_PROJECT_ID {
            return DissolveTerminus::None;
        }
        let Some(pi) = self.workspace.projects.iter().position(|p| p.id == project_id) else {
            return DissolveTerminus::None;
        };
        let session_ids: Vec<String> = self.workspace.projects[pi]
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .collect();

        let terminus = if session_ids.is_empty() {
            // Empty non-Terminals project: drop the row directly + reselect.
            self.workspace.projects.remove(pi);
            let active_gone = self
                .workspace
                .active_session_id()
                .is_some_and(|a| self.workspace.session_for(a).is_none());
            if active_gone {
                if let Some(first) = self.workspace.navigable_sidebar_session_ids().into_iter().next() {
                    self.workspace.select_session(&first);
                }
            }
            if self.workspace.projects.iter().all(|p| p.sessions.is_empty()) {
                DissolveTerminus::WindowEmptied
            } else {
                DissolveTerminus::None
            }
        } else {
            self.ptys.mark_project_pending_removal(project_id);
            let mut terminus = DissolveTerminus::None;
            for id in &session_ids {
                terminus =
                    terminus.or(self.ptys.close_session(&mut self.workspace, &mut self.selection, id));
            }
            terminus
        };
        self.prune_dissolved_file_browser_states();
        self.mark_removed_if_window_emptied(terminus);
        self.save_to_store();
        terminus
    }

    /// Real close of one window on `session_id` (the toolbar pill × path) — the
    /// shipped-path replacement for the model-only `WindowStripActions::close_term_window`
    /// stub. A spawned window routes through [`PtyManager::terminate_window`]
    /// (SIGHUP→SIGKILL + model removal via `window_exited`, dissolving the session when
    /// it was the last window); a model-only window (a lazy companion never focused)
    /// is dropped from the model directly and the session dissolved if it was the last
    /// — so the × is never dead. Returns the terminus; schedules the dissolve save.
    pub(crate) fn close_term_window_via_pty_manager(&mut self, session_id: &str, term_window_id: &str) -> DissolveTerminus {
        let terminus = if self.ptys.window_is_spawned(session_id, term_window_id) {
            self.ptys
                .terminate_window(&mut self.workspace, &mut self.selection, session_id, term_window_id)
                .terminus
        } else {
            self.workspace.extract_window(term_window_id, session_id);
            self.ptys
                .dissolve_session_if_empty(&mut self.workspace, &mut self.selection, session_id)
        };
        self.prune_dissolved_file_browser_states();
        self.mark_removed_if_window_emptied(terminus);
        self.save_to_store();
        terminus
    }

    // MARK: - R20.5 busy-close gates (CloseRequestCoordinator port)
    //
    // The three UI close affordances (toolbar pill ✕, sidebar "Close Tab"/"Close
    // N Tabs", sidebar "Close Project") route through these gates instead of
    // calling `close_*_via_session` directly. Each classifies the close scope's
    // windows as BUSY (D-BUSY) — an alive Claude that is thinking/waiting, or an
    // alive terminal whose shell has a foreground child — and then either presents
    // the R18 `ConfirmationModal` (`destructive_confirm = true`, "Force quit") in
    // front of the existing kill route, or, when nothing is busy, runs the kill
    // route immediately (exactly today's unconfirmed behavior, D0). This is a
    // DISTINCT system from R18's alive-window quit/window-close confirmation (D0);
    // the two counters never chain.

    /// Whether one window is BUSY (D-BUSY, ported 1:1 from Swift's `isBusy`,
    /// `CloseRequestCoordinator.swift:268-279`), **ORed across its panes** since
    /// Phase 2: a build running in the shell pane beside Claude has to block the
    /// pill's close, and no pill-level signal can see it.
    ///
    /// Each leaf contributes its own signal — the Claude leaf its status, a
    /// shell leaf its own `tcgetpgrp` probe (synthetic seam first) — through the
    /// pure [`pane_is_busy_with`](Self::pane_is_busy_with) core. A never-split
    /// pill has one leaf, so this is the pre-splits predicate verbatim.
    fn window_is_busy(&self, session_id: &str, term_window: &TermWindow, cx: &gpui::App) -> bool {
        let signals: Vec<PaneBusySignal> = term_window
            .layout
            .leaves()
            .iter()
            .map(|pane| {
                let alive = term_window.is_alive && pane.is_alive;
                PaneBusySignal {
                    kind: pane.kind,
                    alive,
                    status: self.ptys.resolved_pane_status(session_id, term_window, pane),
                    // Short-circuit: only a live shell pane consults the
                    // foreground signal (the syscall is skipped entirely for
                    // Claude / dead panes).
                    has_foreground_child: matches!(pane.kind, TermWindowKind::Terminal)
                        && alive
                        && self.ptys.pane_has_foreground_child(
                            session_id,
                            &term_window.id,
                            &pane.id,
                            cx,
                        ),
                }
            })
            .collect();
        Self::any_pane_busy(&signals)
    }

    /// The pure OR — a pill is busy iff any of its panes is. Split out so the
    /// across-leaves rule is testable without a `PtyManager` or a gpui `App`.
    fn any_pane_busy(signals: &[PaneBusySignal]) -> bool {
        signals
            .iter()
            .any(|s| Self::pane_is_busy_with(s.kind, s.alive, s.status, s.has_foreground_child))
    }

    /// The pure D-BUSY predicate for ONE pane, given its foreground signal — the
    /// gpui-free core of [`window_is_busy`](Self::window_is_busy), unit-testable
    /// without a `PtyManager` / gpui `App`:
    /// 1. not alive ⇒ **not busy** (a held/dead pane is never busy — dead-first
    ///    guard).
    /// 2. `Claude` ⇒ busy iff `status` is `Thinking`/`Waiting` (an idle Claude at
    ///    rest is disposable; read the PER-PANE status, not any session aggregate).
    /// 3. `Terminal` ⇒ busy iff `has_foreground_child` (the caller's
    ///    `tcgetpgrp`/synthetic signal; a shell pane's status is meaningless).
    fn pane_is_busy_with(
        kind: TermWindowKind,
        alive: bool,
        status: SessionStatus,
        has_foreground_child: bool,
    ) -> bool {
        if !alive {
            return false;
        }
        match kind {
            TermWindowKind::Claude => {
                matches!(status, SessionStatus::Thinking | SessionStatus::Waiting)
            }
            TermWindowKind::Terminal => has_foreground_child,
        }
    }

    // MARK: - Command Compose (the `commandCompose` shortcut, ⌘↩)

    /// Command Compose dispatch (window-scoped, from the keymap handler). At an
    /// idle interactive zsh prompt — a live `Terminal` window with no foreground
    /// child — write [`crate::shell_inject::COMPOSE_TRIGGER_SEQ`] to the window's
    /// pty; the injected ZLE widget takes it from there. Otherwise replay
    /// exactly what an unbound ⌘↩ produced before this feature existed: the
    /// kitty CSI-u encoding when the foreground app forwards super chords
    /// (Claude Code, kitty TUIs), nothing at all otherwise. NEVER writes a
    /// newline — running the composed command is always the user's own Enter.
    pub(crate) fn dispatch_command_compose(&mut self, cx: &mut gpui::Context<WindowState>) {
        let Some(session_id) = self.workspace.active_session_id().map(str::to_owned) else {
            return;
        };
        let Some(session) = self.workspace.session_for(&session_id) else {
            return;
        };
        let Some(term_window_id) = session.active_window_id.clone() else {
            return;
        };
        let Some(term_window) = session.windows.iter().find(|w| w.id == term_window_id) else {
            return;
        };
        // The FOCUSED pane decides, not the pill: an idle shell pane focused
        // inside a Claude pill is exactly the "shell beside Claude" layout D1
        // exists for, and ⌘↩ has to compose there. A focused Claude leaf keeps
        // the pill-era behavior because its own kind is Claude.
        let pane_id = term_window.effective_pane_id();
        let Some(pane) = term_window.layout.pane(&pane_id) else {
            return;
        };
        let (kind, alive) = (pane.kind, term_window.is_alive && pane.is_alive);
        // A model-only pane (no cached session) has no pty to write to.
        let Some(handle) = self.ptys.pane_handle(&session_id, &term_window_id, &pane_id) else {
            return;
        };
        let fg_child = self
            .ptys
            .pane_has_foreground_child(&session_id, &term_window_id, &pane_id, cx);
        let kitty_super = handle.read(cx).session().kitty_forwards_super();
        let bytes: &[u8] = match Self::compose_route(kind, alive, fg_child, kitty_super) {
            ComposeRoute::Trigger => crate::shell_inject::COMPOSE_TRIGGER_SEQ,
            ComposeRoute::ForwardCmdEnter => KITTY_CMD_ENTER,
            ComposeRoute::Noop => return,
        };
        let _ = handle.read(cx).session().write_input(bytes);
    }

    /// The pure Command Compose routing core — the gpui-free truth table of
    /// [`dispatch_command_compose`](Self::dispatch_command_compose):
    /// 1. A live `Terminal` window with no foreground child ⇒ [`ComposeRoute::Trigger`]
    ///    (the kitty state is irrelevant: zsh at a prompt requests no kitty flags).
    /// 2. Anything else whose child forwards super chords ⇒
    ///    [`ComposeRoute::ForwardCmdEnter`] (vim/Claude Code/any kitty TUI keeps
    ///    receiving ⌘↩ exactly as before the chord was bound).
    /// 3. Otherwise ⇒ [`ComposeRoute::Noop`] (dead window, busy legacy-mode shell —
    ///    where an unbound ⌘↩ also produced no pty bytes).
    fn compose_route(
        kind: TermWindowKind,
        alive: bool,
        fg_child: bool,
        kitty_super: bool,
    ) -> ComposeRoute {
        if matches!(kind, TermWindowKind::Terminal) && alive && !fg_child {
            return ComposeRoute::Trigger;
        }
        if kitty_super {
            return ComposeRoute::ForwardCmdEnter;
        }
        ComposeRoute::Noop
    }

    /// The [`describe`](crate::close_confirm::describe)d busy windows of `session_id`, in
    /// window order, honoring the `is_alive && isBusy` pre-filter (Swift
    /// `requestCloseTab` `:126-129`). Empty when the session is absent or nothing is
    /// busy.
    fn busy_descriptions_in_session(&self, session_id: &str, cx: &gpui::App) -> Vec<String> {
        let Some(session) = self.workspace.session_for(session_id) else {
            return Vec::new();
        };
        session.windows
            .iter()
            .filter(|w| self.window_is_busy(session_id, w, cx))
            .map(crate::close_confirm::describe)
            .collect()
    }

    /// Prune + re-sync the selection against the surviving sessions after a close —
    /// the model/selection half of the sidebar handlers' post-close reconcile
    /// (formerly `SidebarShellView::reconcile_selection_after_close`). Runs in BOTH
    /// the idle-immediate and the confirm-completion paths (D9); a confirmed close
    /// that skipped it would strand a stale selection.
    pub(crate) fn reconcile_selection_after_close(&mut self) {
        let valid: HashSet<String> =
            self.workspace.navigable_sidebar_session_ids().into_iter().collect();
        let active = self.workspace.active_session_id().map(|s| s.to_string());
        self.selection.prune(&valid);
        self.selection.sync_active_session_id(active.as_deref());
    }

    /// Gate the toolbar pill ✕ close of one window (Swift `requestClosePane`
    /// `:104-117`). Busy ⇒ present the `.pane` modal; idle ⇒ close immediately.
    pub(crate) fn request_close_term_window(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_term_window({session_id}, {term_window_id}) ignored — a confirmation modal \
                 is already up"
            );
            return;
        }
        let busy_desc = self
            .workspace
            .session_for(session_id)
            .and_then(|s| s.windows.iter().find(|w| w.id == term_window_id))
            .filter(|w| self.window_is_busy(session_id, w, cx))
            .map(crate::close_confirm::describe);
        match busy_desc {
            Some(desc) => {
                let message = crate::close_confirm::window_message(&[desc]);
                let state = cx.entity();
                let tid = session_id.to_string();
                let pid = term_window_id.to_string();
                self.present_confirmation(
                    crate::close_confirm::TITLE,
                    message,
                    crate::close_confirm::CONFIRM_LABEL,
                    crate::close_confirm::CANCEL_LABEL,
                    true,
                    move |confirmed, window, app| {
                        if confirmed {
                            Self::commit_close_term_window(&state, &tid, &pid, window, app);
                        }
                    },
                    window,
                    cx,
                );
            }
            None => {
                let terminus = self.close_term_window_via_pty_manager(session_id, term_window_id);
                self.reconcile_selection_after_close();
                cx.notify();
                PtyManager::apply_dissolve_terminus(terminus, window, cx);
            }
        }
    }

    /// The confirmed-`.pane` completion: re-resolve by id (never a stale `TermWindow`,
    /// D2) and run the existing kill route + reconcile + terminus (D9).
    fn commit_close_term_window(
        state: &Entity<Self>,
        session_id: &str,
        term_window_id: &str,
        window: &mut gpui::Window,
        app: &mut gpui::App,
    ) {
        let terminus = state.update(app, |ws, cx| {
            let terminus = ws.close_term_window_via_pty_manager(session_id, term_window_id);
            ws.reconcile_selection_after_close();
            cx.notify();
            terminus
        });
        PtyManager::apply_dissolve_terminus(terminus, window, app);
    }

    /// Gate the sidebar "Close Tab" close of one session (Swift `requestCloseTab`
    /// `:123-135`). Any alive busy window ⇒ present the `.tab` modal; else close
    /// immediately.
    pub(crate) fn request_close_session(
        &mut self,
        session_id: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_session({session_id}) ignored — a confirmation modal is already up"
            );
            return;
        }
        let busy = self.busy_descriptions_in_session(session_id, cx);
        if busy.is_empty() {
            let terminus = self.close_session_via_pty_manager(session_id);
            self.reconcile_selection_after_close();
            cx.notify();
            PtyManager::apply_dissolve_terminus(terminus, window, cx);
            return;
        }
        let message = crate::close_confirm::session_message(&busy);
        let state = cx.entity();
        let tid = session_id.to_string();
        self.present_confirmation(
            crate::close_confirm::TITLE,
            message,
            crate::close_confirm::CONFIRM_LABEL,
            crate::close_confirm::CANCEL_LABEL,
            true,
            move |confirmed, window, app| {
                if confirmed {
                    Self::commit_close_sessions(&state, std::slice::from_ref(&tid), window, app);
                }
            },
            window,
            cx,
        );
    }

    /// Gate the sidebar project-context "Close Project" close (Swift
    /// `requestCloseProject` `:219-236`). The pinned Terminals group has no Close
    /// Project affordance and never presents a dialog (its kill route no-ops it,
    /// `close_project_via_session:1125`); otherwise any alive busy window across the
    /// project's sessions ⇒ present the `.project` modal, else close immediately.
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
        let busy = if project_id == WorkspaceModel::TERMINALS_PROJECT_ID {
            Vec::new()
        } else {
            self.workspace
                .projects
                .iter()
                .find(|p| p.id == project_id)
                .into_iter()
                .flat_map(|project| {
                    project.sessions.iter().flat_map(|s| {
                        s.windows
                            .iter()
                            .filter(|w| self.window_is_busy(&s.id, w, cx))
                            .map(crate::close_confirm::describe)
                    })
                })
                .collect::<Vec<_>>()
        };
        if busy.is_empty() {
            let terminus = self.close_project_via_session(project_id);
            self.reconcile_selection_after_close();
            cx.notify();
            PtyManager::apply_dissolve_terminus(terminus, window, cx);
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
        PtyManager::apply_dissolve_terminus(terminus, window, app);
    }

    /// Gate the sidebar "Close N Tabs" multi-select close — the partial-eager flow
    /// (Swift `requestCloseTabs` `:145-191`, D5/§T). A single id degrades to the
    /// `.tab` gate. Otherwise idle sessions are hard-killed NOW (rows vanish before any
    /// dialog); only busy survivors are gated behind ONE `.tabs` modal. On cancel
    /// the busy survivors stay ALIVE while the already-closed idle members stay
    /// CLOSED — a *partial* close, NOT a total no-op.
    pub(crate) fn request_close_sessions(
        &mut self,
        ids: &[String],
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // §T.1 — single id degrades to the identical `.tab` wording.
        if ids.len() == 1 {
            self.request_close_session(&ids[0], window, cx);
            return;
        }
        // §T.2 — re-entrancy guard (D7).
        if self.pending_modal().is_some() {
            eprintln!(
                "nice: request_close_sessions({} sessions) ignored — a confirmation modal is already up",
                ids.len()
            );
            return;
        }
        // §T.3 — classify each EXISTING id into idle vs busy.
        let SessionsCloseSplit {
            idle_ids,
            busy_ids,
            busy_summaries,
        } = split_sessions_close_batch(ids, |id| {
            self.workspace
                .session_for(id)
                .map(|s| (s.title.clone(), self.busy_descriptions_in_session(id, cx)))
        });
        // §T.4 — eagerly close the idle sessions NOW (rows vanish immediately). Any
        // terminus is at most `WindowEmptied`, which can only fire when NO busy
        // survivors remain (they keep the window non-empty) — so actuating it is
        // safe in both branches below.
        let idle_terminus = if idle_ids.is_empty() {
            DissolveTerminus::None
        } else {
            let terminus = self.close_sessions_via_pty_manager(&idle_ids);
            self.reconcile_selection_after_close();
            cx.notify();
            terminus
        };
        // §T.5 — everything was idle and is gone.
        if busy_ids.is_empty() {
            PtyManager::apply_dissolve_terminus(idle_terminus, window, cx);
            return;
        }
        // Busy survivors remain: actuate the idle terminus (never `WindowEmptied`
        // here) then present ONE `.tabs` modal over the survivors.
        PtyManager::apply_dissolve_terminus(idle_terminus, window, cx);
        let message = crate::close_confirm::sessions_message(&busy_summaries);
        let state = cx.entity();
        self.present_confirmation(
            crate::close_confirm::TITLE,
            message,
            crate::close_confirm::CONFIRM_LABEL,
            crate::close_confirm::CANCEL_LABEL,
            true,
            move |confirmed, window, app| {
                if confirmed {
                    Self::commit_close_sessions(&state, &busy_ids, window, app);
                }
            },
            window,
            cx,
        );
    }

    /// The confirmed-`.tab`/`.tabs` completion: re-resolve by id and run the batch
    /// kill route + reconcile + terminus (D2/D9). Shared by the singular `.tab`
    /// gate (a one-element slice) and the `.tabs` multi-select gate.
    fn commit_close_sessions(
        state: &Entity<Self>,
        session_ids: &[String],
        window: &mut gpui::Window,
        app: &mut gpui::App,
    ) {
        let terminus = state.update(app, |ws, cx| {
            let terminus = ws.close_sessions_via_pty_manager(session_ids);
            ws.reconcile_selection_after_close();
            cx.notify();
            terminus
        });
        PtyManager::apply_dissolve_terminus(terminus, window, app);
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
        // BUGHUNT1-D: drop the did-mutate save-drain likewise — no parked task
        // lingers past the window (the entity drop would cancel it anyway).
        self.save_drain = None;
        if let Some(socket) = self.control_socket.take() {
            socket.stop();
        }
        // Drop every retained pane subscription with the ptys they listen to —
        // the sweep that would otherwise retire them never runs again once this
        // window is gone.
        self.pane_subscriptions.clear();
        // Terminate this window's ptys: dropping each cached session handle tears
        // its child process group down (SIGHUP→SIGKILL), so no orphan zsh
        // survives. R18 flushes the session snapshot before this runs. Idempotent.
        self.ptys.teardown();
    }
}

/// The idle-vs-busy split of a `.tabs` (multi-select) close batch — the pure
/// result of [`split_sessions_close_batch`], consumed by
/// [`WindowState::request_close_sessions`] (§T.3).
#[derive(Debug, Default, PartialEq, Eq)]
struct SessionsCloseSplit {
    /// Sessions with no alive busy window — hard-killed eagerly, before any dialog.
    idle_ids: Vec<String>,
    /// Sessions with ≥1 alive busy window — gated behind the one `.tabs` modal.
    busy_ids: Vec<String>,
    /// The busy sessions' `<Title> (<p1>, <p2>)` summaries, parallel to `busy_ids`.
    busy_summaries: Vec<String>,
}

/// Bucket a multi-select close batch into idle vs busy (§T.3), the pure core of
/// [`WindowState::request_close_sessions`] — gpui-free, so the bucketing is
/// unit-testable without a live window. `classify(id)` returns `None` for a
/// vanished id (skipped — Swift iterates the id list, `:177-183`), else
/// `Some((title, busy_descriptions))`: an empty description list means idle,
/// non-empty means busy (its summary is built from the title + descriptions).
fn split_sessions_close_batch(
    ids: &[String],
    mut classify: impl FnMut(&str) -> Option<(String, Vec<String>)>,
) -> SessionsCloseSplit {
    let mut split = SessionsCloseSplit::default();
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
                .push(crate::close_confirm::busy_session_summary(&title, &busy));
        }
    }
    split
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_socket::{Reply, RecordedSocketMessage};
    use nice_model::{TermWindow, TermWindowKind, Project, Session, WorkspaceModel};
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    #[test]
    fn new_seeds_default_window_shape() {
        let state = WindowState::new("/home/u");
        // Seeded WorkspaceModel: the pinned Terminals group + Main session, Main active.
        assert_eq!(
            state.workspace.active_session_id(),
            Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID),
            "the Main terminal session is active on a fresh window"
        );
        assert!(
            state
                .workspace
                .projects
                .iter()
                .any(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group is present"
        );
        // Sidebar defaults: expanded, sessions mode (AppState convenience-init parity).
        assert!(!state.sidebar.collapsed(), "sidebar starts expanded");
        assert_eq!(state.sidebar.mode(), SidebarMode::Sessions);
        // Selection invariant: the active session is selected from construction.
        assert!(
            state.selection.contains(WorkspaceModel::MAIN_TERMINAL_SESSION_ID),
            "selection is seeded with the active session"
        );
    }

    /// The one collapse seam ([`WindowState::toggle_sidebar_collapsed`]) that the
    /// ⌘B keymap action, the titlebar collapse control
    /// ([`crate::toolbar::WindowToolbarView`]), and the sidebar view all route
    /// through: it flips the collapsed flag and clears any peek on EXPAND. This
    /// pins the shipped collapse behavior at the shared seam (the titlebar control
    /// otherwise had no test).
    #[gpui::test]
    fn toggle_sidebar_collapsed_flips_flag_and_clears_peek_on_expand(
        cx: &mut gpui::TestAppContext,
    ) {
        let state = cx.new(|_cx| WindowState::new("/home/u"));

        // Fresh window starts expanded.
        state.update(cx, |ws, _| assert!(!ws.sidebar.collapsed(), "starts expanded"));

        // First toggle collapses.
        state.update(cx, |ws, cx| ws.toggle_sidebar_collapsed(cx));
        state.update(cx, |ws, _| assert!(ws.sidebar.collapsed(), "first toggle collapses"));

        // A collapsed sidebar-session cycle would begin a peek; expanding must clear it.
        state.update(cx, |ws, _| ws.sidebar.begin_sidebar_peek());
        state.update(cx, |ws, _| {
            assert!(ws.sidebar.peeking(), "peek is active while collapsed")
        });

        // Second toggle expands AND clears the peek (the seam's expand-side cleanup).
        state.update(cx, |ws, cx| ws.toggle_sidebar_collapsed(cx));
        state.update(cx, |ws, _| {
            assert!(!ws.sidebar.collapsed(), "second toggle expands");
            assert!(!ws.sidebar.peeking(), "expanding clears the peek");
        });
    }

    // MARK: - Hold-to-hint overlay debounce (Phase 1, D5)

    /// The debounce delay the hint tests arm with — the keymap's shipped value is
    /// [`crate::keymap::HINT_OVERLAY_DELAY`]; the exact number is irrelevant here,
    /// only the before/after behavior around it.
    const TEST_HINT_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

    #[gpui::test]
    fn key_hint_shows_only_after_the_full_hold(cx: &mut gpui::TestAppContext) {
        let state = cx.new(|_cx| WindowState::new("/home/u"));
        state.update(cx, |ws, _| assert!(!ws.key_hint.visible(), "starts hidden"));

        state.update(cx, |ws, cx| ws.arm_key_hint(TEST_HINT_DELAY, cx));
        // Half-way through the hold: still nothing (a fast chord never flashes it).
        cx.executor().advance_clock(TEST_HINT_DELAY / 2);
        cx.run_until_parked();
        state.update(cx, |ws, _| {
            assert!(!ws.key_hint.visible(), "nothing shows inside the debounce window")
        });

        cx.executor().advance_clock(TEST_HINT_DELAY);
        cx.run_until_parked();
        state.update(cx, |ws, _| {
            assert!(ws.key_hint.visible(), "the surviving hold shows the overlay")
        });
    }

    #[gpui::test]
    fn cancelling_inside_the_debounce_never_shows_the_hint(cx: &mut gpui::TestAppContext) {
        // The fast-chord case: ⌃⌘L commits and the modifiers lift well before the
        // timer fires, so the pending task must produce nothing at all.
        let state = cx.new(|_cx| WindowState::new("/home/u"));
        state.update(cx, |ws, cx| ws.arm_key_hint(TEST_HINT_DELAY, cx));
        cx.executor().advance_clock(TEST_HINT_DELAY / 4);
        state.update(cx, |ws, cx| ws.cancel_key_hint(cx));

        cx.executor().advance_clock(TEST_HINT_DELAY * 4);
        cx.run_until_parked();
        state.update(cx, |ws, _| {
            assert!(!ws.key_hint.visible(), "a cancelled hold never paints badges")
        });
    }

    #[gpui::test]
    fn cancelling_hides_a_shown_hint_and_re_arming_shows_it_again(cx: &mut gpui::TestAppContext) {
        let state = cx.new(|_cx| WindowState::new("/home/u"));
        state.update(cx, |ws, cx| ws.arm_key_hint(TEST_HINT_DELAY, cx));
        cx.executor().advance_clock(TEST_HINT_DELAY * 2);
        cx.run_until_parked();
        state.update(cx, |ws, _| assert!(ws.key_hint.visible()));

        // Release: instant hide, no delay.
        state.update(cx, |ws, cx| ws.cancel_key_hint(cx));
        state.update(cx, |ws, _| {
            assert!(!ws.key_hint.visible(), "release hides immediately")
        });

        // Holding again re-arms from scratch.
        state.update(cx, |ws, cx| ws.arm_key_hint(TEST_HINT_DELAY, cx));
        cx.executor().advance_clock(TEST_HINT_DELAY * 2);
        cx.run_until_parked();
        state.update(cx, |ws, _| {
            assert!(ws.key_hint.visible(), "a second hold shows the overlay again")
        });
    }

    #[gpui::test]
    fn re_arming_mid_hold_does_not_restart_the_countdown(cx: &mut gpui::TestAppContext) {
        // One physical hold can produce several modifier events with the same held
        // set; each would arm again. If that restarted the timer, holding ⌃⌘ while
        // the OS repeats events could postpone the overlay forever.
        let state = cx.new(|_cx| WindowState::new("/home/u"));
        state.update(cx, |ws, cx| ws.arm_key_hint(TEST_HINT_DELAY, cx));
        cx.executor().advance_clock(TEST_HINT_DELAY * 3 / 4);
        cx.run_until_parked();
        state.update(cx, |ws, cx| ws.arm_key_hint(TEST_HINT_DELAY, cx));

        // Past the FIRST arm's deadline, short of a restarted one.
        cx.executor().advance_clock(TEST_HINT_DELAY / 2);
        cx.run_until_parked();
        state.update(cx, |ws, _| {
            assert!(ws.key_hint.visible(), "the original countdown still owns the hold")
        });
    }

    #[test]
    fn each_window_has_a_unique_session_id() {
        let a = WindowState::new("/home/u");
        let b = WindowState::new("/home/u");
        assert!(!a.window_session_id().is_empty());
        assert_ne!(
            a.window_session_id(),
            b.window_session_id(),
            "session ids must be unique per window (the undo-routing lookup key)"
        );
    }

    #[test]
    fn windows_are_isolated_mutating_one_model_leaves_the_other_untouched() {
        // The isolation guarantee at the state level: two windows own
        // independent WorkspaceModels, so a mutation to one's tree is invisible to the
        // other. (The live two-window itest — mutate A, B byte-identical — is the
        // scenario slice; this pins the underlying state ownership.)
        let mut a = WindowState::new("/home/u");
        let b = WindowState::new("/home/u");

        let before_b: Vec<usize> = b.workspace.projects.iter().map(|p| p.sessions.len()).collect();

        // Mutate A's tree through its own seam (the same surface the keymap slice
        // will drive): add a terminal session.
        let new_id = a
            .sidebar_actions
            .create_terminal_session(&mut a.workspace)
            .expect("Terminals group exists");
        assert!(a.workspace.session_for(&new_id).is_some(), "A gained the new session");

        let after_b: Vec<usize> = b.workspace.projects.iter().map(|p| p.sessions.len()).collect();
        assert_eq!(before_b, after_b, "B's tree is unchanged by A's mutation");
        assert!(
            b.workspace.session_for(&new_id).is_none(),
            "A's new session never appears in B"
        );
    }

    // ---- W5 (R18) UI-close wiring + snapshot --------------------------------

    /// Seed a window whose model has the pinned Terminals group plus one
    /// non-Terminals project "proj" with two model-only terminal sessions.
    fn window_with_project() -> WindowState {
        let mut model = WorkspaceModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        for id in ["t-a", "t-b"] {
            let mut session = Session::new(id, id, "/home/u/proj");
            let term_window = format!("{id}-p");
            session.windows = vec![TermWindow::new(&term_window, "Terminal 1", TermWindowKind::Terminal)];
            session.active_window_id = Some(term_window);
            model.projects[pi].sessions.push(session);
        }
        WindowState::with_model(model)
    }

    #[test]
    fn close_project_via_session_drops_the_project_row() {
        let mut ws = window_with_project();
        let terminus = ws.close_project_via_session("proj");
        assert!(
            ws.workspace.projects.iter().all(|p| p.id != "proj"),
            "Close Project drops the non-Terminals row once its sessions dissolve"
        );
        assert!(ws.workspace.session_for("t-a").is_none());
        assert!(ws.workspace.session_for("t-b").is_none());
        assert!(
            ws.workspace.projects.iter().any(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group is never closed"
        );
        // The Terminals group still has the Main session, so the window isn't empty.
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn close_project_via_session_empty_project_drops_directly() {
        let mut model = WorkspaceModel::new("/home/u");
        model.ensure_project("empty", "Empty", "/home/u/empty");
        let mut ws = WindowState::with_model(model);

        let terminus = ws.close_project_via_session("empty");

        assert!(
            ws.workspace.projects.iter().all(|p| p.id != "empty"),
            "an already-empty non-Terminals project row drops directly"
        );
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn close_project_via_session_refuses_terminals_group() {
        let mut ws = WindowState::new("/home/u");
        let terminus = ws.close_project_via_session(WorkspaceModel::TERMINALS_PROJECT_ID);
        assert!(
            ws.workspace.projects.iter().any(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group can never be closed"
        );
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn close_session_via_pty_manager_dissolves_a_model_only_session() {
        let mut ws = window_with_project();
        ws.close_session_via_pty_manager("t-a");
        assert!(ws.workspace.session_for("t-a").is_none(), "the model-only session dissolves");
        assert!(ws.workspace.session_for("t-b").is_some(), "its sibling survives");
        assert!(
            ws.workspace.projects.iter().any(|p| p.id == "proj"),
            "the project row survives while it still has a session"
        );
    }

    #[test]
    fn close_term_window_via_pty_manager_removes_a_model_only_window() {
        // A session with two model-only windows; closing one leaves the session with one.
        let mut model = WorkspaceModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        let mut session = Session::new("t", "T", "/home/u/proj");
        session.windows = vec![
            TermWindow::new("p1", "A", TermWindowKind::Terminal),
            TermWindow::new("p2", "B", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some("p1".into());
        model.projects[pi].sessions.push(session);
        let mut ws = WindowState::with_model(model);

        let terminus = ws.close_term_window_via_pty_manager("t", "p1");

        let session = ws.workspace.session_for("t").unwrap();
        assert_eq!(session.windows.len(), 1, "the closed model-only window is gone");
        assert_eq!(session.windows[0].id, "p2");
        assert_eq!(terminus, DissolveTerminus::None);
    }

    #[test]
    fn persisted_snapshot_carries_id_sidebar_and_projects() {
        let mut ws = window_with_project();
        ws.sidebar.toggle_sidebar(); // expanded → collapsed
        let snap = ws.persisted_snapshot();
        assert_eq!(snap.id, ws.window_session_id());
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
            .any(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID));
    }

    #[test]
    fn user_initiated_close_flag_defaults_false_and_sets() {
        let mut ws = WindowState::new("/home/u");
        assert!(!ws.user_initiated_close(), "defaults false (preserve is safe)");
        ws.set_user_initiated_close(true);
        assert!(ws.user_initiated_close());
    }

    // ---- L2/L3 restore (with_model selection re-seed + with_seed) -----------

    fn terminal_session(id: &str, cwd: &str) -> Session {
        let mut session = Session::new(id, id, cwd);
        let term_window = format!("{id}-p");
        session.windows = vec![TermWindow::new(&term_window, "Terminal 1", TermWindowKind::Terminal)];
        session.active_window_id = Some(term_window);
        session
    }

    #[test]
    fn with_model_reseeds_selection_from_non_default_active_session() {
        // The R13.5 caveat made load-bearing by restore: a `WindowState` built
        // around a model whose active session ISN'T the default Main must have its
        // multi-selection re-seeded from that active session (else the sidebar shows
        // no selected row). Build a two-project model active on a non-Main session.
        let mut model = WorkspaceModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        model.projects[pi].sessions.push(terminal_session("t-x", "/home/u/proj"));
        model.select_session("t-x");

        let ws = WindowState::with_model(model);
        assert_eq!(ws.workspace.active_session_id(), Some("t-x"));
        assert!(
            ws.selection.contains("t-x"),
            "with_model must re-seed the selection from the model's active session"
        );
        assert!(
            !ws.selection.contains(WorkspaceModel::MAIN_TERMINAL_SESSION_ID),
            "the default Main session is not selected when it isn't active"
        );
    }

    /// A hydrated seed: the pinned Terminals group (with Main) + a "proj" project
    /// carrying `t-a` and `t-b`, active on `t-b`, collapsed sidebar, saved id.
    fn restore_seed(window_id: &str, active: Option<&str>, collapsed: bool) -> WindowSeed {
        let terminals = Project {
            id: WorkspaceModel::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: "/home/u".into(),
            sessions: vec![terminal_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID, "/home/u")],
        };
        let proj = Project {
            id: "proj".into(),
            name: "Proj".into(),
            path: "/home/u/proj".into(),
            sessions: vec![
                terminal_session("t-a", "/home/u/proj"),
                terminal_session("t-b", "/home/u/proj"),
            ],
        };
        WindowSeed {
            window_id: window_id.into(),
            projects: vec![terminals, proj],
            active_session_id: active.map(str::to_string),
            sidebar_collapsed: collapsed,
            sidebar_mode: None,
            sidebar_width: None,
            frame: None,
        }
    }

    #[test]
    fn with_seed_adopts_id_collapse_and_rebuilds_saved_tree() {
        let ws = WindowState::with_seed(restore_seed("win-restored", Some("t-b"), true));
        // The saved window id is adopted verbatim (L2 identity), NOT a fresh mint.
        assert_eq!(ws.window_session_id(), "win-restored");
        assert!(ws.sidebar.collapsed(), "the saved collapse flag restores");
        // The saved grouping is trusted: proj + its two sessions + the Terminals group.
        assert!(ws.workspace.session_for("t-a").is_some());
        assert!(ws.workspace.session_for("t-b").is_some());
        assert!(ws.workspace.projects.iter().any(|p| p.id == "proj"));
        // The saved active session is re-applied and the selection re-seeded from it.
        assert_eq!(ws.workspace.active_session_id(), Some("t-b"));
        assert!(ws.selection.contains("t-b"));
    }

    #[test]
    fn with_seed_heals_duplicate_window_ids_from_a_pre_fix_save() {
        // A pre-fix save could carry two windows sharing one id (the strip's
        // reset-at-launch minter re-issued a persisted "window-N"), which
        // double-lights the strip and makes rename edit both pills. Restore
        // must re-mint the later duplicate.
        let mut seed = restore_seed("w-dup", None, false);
        seed.projects[0].sessions[0]
            .windows
            .push(TermWindow::new("dup-id", "Moldavite", TermWindowKind::Terminal));
        seed.projects[0].sessions[0]
            .windows
            .push(TermWindow::new("dup-id", "Terminal 15", TermWindowKind::Terminal));

        let ws = WindowState::with_seed(seed);

        let session = ws.workspace.session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID).unwrap();
        let mut ids: Vec<&str> = session.windows.iter().map(|w| w.id.as_str()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "restore must leave no duplicate window ids: {ids:?}");
    }

    #[test]
    fn with_seed_falls_back_to_first_navigable_when_active_absent() {
        // A saved active id that no longer resolves (e.g. its session was pruned) ⇒
        // the first navigable session (the Terminals Main session) becomes active.
        let ws = WindowState::with_seed(restore_seed("w", Some("ghost-tab"), false));
        assert_eq!(
            ws.workspace.active_session_id(),
            Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID),
            "an unresolved saved active session falls back to the first navigable session"
        );
    }

    #[test]
    fn with_seed_prunes_dangling_parent_reference() {
        // A restored child session whose parent didn't survive: the repair pass clears
        // the dangling parent link so the session renders at root instead of orphaned.
        let mut seed = restore_seed("w", Some("t-a"), false);
        let pi = seed.projects.iter().position(|p| p.id == "proj").unwrap();
        let ti = seed.projects[pi].sessions.iter().position(|s| s.id == "t-a").unwrap();
        seed.projects[pi].sessions[ti].parent_session_id = Some("never-existed".into());

        let ws = WindowState::with_seed(seed);
        let t_a = ws.workspace.session_for("t-a").expect("t-a survives");
        assert_eq!(
            t_a.parent_session_id, None,
            "prune_dangling_parent_references clears a link to a non-existent parent"
        );
    }

    // ---- R19: sidebar-mode persistence + file-browser dissolve cleanup ------

    #[test]
    fn persisted_snapshot_carries_sidebar_mode() {
        // R19: the live sidebar mode round-trips through the snapshot (Swift's
        // per-window SceneStorage mode). Fresh windows default to Sessions.
        let ws = window_with_project();
        assert_eq!(
            ws.persisted_snapshot().sidebar_mode,
            Some(SidebarMode::Sessions),
            "a fresh window snapshots the default Sessions mode"
        );
        let mut ws = window_with_project();
        ws.sidebar.toggle_sidebar_mode(); // Sessions → Files
        assert_eq!(
            ws.persisted_snapshot().sidebar_mode,
            Some(SidebarMode::Files),
            "toggling to files mode is captured in the snapshot"
        );
    }

    #[test]
    fn persisted_snapshot_carries_sidebar_width_absent_until_customized() {
        // Phase 0: a never-resized window snapshots NO width (the key stays
        // absent on disk); a set width round-trips through the snapshot.
        let ws = window_with_project();
        assert_eq!(
            ws.persisted_snapshot().sidebar_width,
            None,
            "a fresh window persists no sidebar width"
        );
        let mut ws = window_with_project();
        ws.sidebar.set_width(Some(320.0));
        assert_eq!(
            ws.persisted_snapshot().sidebar_width,
            Some(320.0),
            "a user-resized width is captured in the snapshot"
        );
    }

    #[test]
    fn with_seed_restores_sidebar_width_absent_stays_default() {
        // Phase 0: a saved width restores into the sidebar model; an absent
        // field (pre-Phase-0 save / never resized) leaves it None ⇒ the view
        // resolves its default.
        let mut seed = restore_seed("w-wide", Some("t-a"), false);
        seed.sidebar_width = Some(355.0);
        let ws = WindowState::with_seed(seed);
        assert_eq!(ws.sidebar.width(), Some(355.0), "the saved width restores");

        let ws = WindowState::with_seed(restore_seed("w-defw", Some("t-a"), false));
        assert_eq!(ws.sidebar.width(), None, "absent ⇒ never customized");
    }

    #[test]
    fn with_seed_restores_sidebar_mode_absent_defaults_sessions() {
        // R19: a saved Files mode restores; an absent field (pre-R19 save) ⇒ Sessions.
        let mut seed = restore_seed("w-files", Some("t-a"), false);
        seed.sidebar_mode = Some(SidebarMode::Files);
        let ws = WindowState::with_seed(seed);
        assert_eq!(ws.sidebar.mode(), SidebarMode::Files, "the saved mode restores");

        let ws = WindowState::with_seed(restore_seed("w-none", Some("t-a"), false));
        assert_eq!(
            ws.sidebar.mode(),
            SidebarMode::Sessions,
            "an absent sidebar_mode restores to Sessions (the pre-R19 default)"
        );
    }

    #[test]
    fn file_browser_state_dropped_on_session_dissolve() {
        // R19: the dissolve cascade drops the closed session's file-browser state (the
        // single removal path) so a long session doesn't leak per-session states.
        let mut ws = window_with_project();
        ws.file_browser.ensure_state("t-a", "/home/u/proj");
        ws.file_browser.ensure_state("t-b", "/home/u/proj");
        assert!(ws.file_browser.state_for("t-a").is_some());

        ws.close_session_via_pty_manager("t-a");

        assert!(
            ws.file_browser.state_for("t-a").is_none(),
            "the dissolved session's file-browser state is dropped"
        );
        assert!(
            ws.file_browser.state_for("t-b").is_some(),
            "a surviving sibling keeps its file-browser state"
        );
    }

    #[test]
    fn with_seed_does_not_seed_a_second_terminals_main() {
        // from_parts must NOT inject a fresh Terminals+Main (that's `new`'s job) —
        // restore trusts the saved grouping, so there is exactly ONE Main session.
        let ws = WindowState::with_seed(restore_seed("w", Some("t-a"), false));
        let mains = ws
            .workspace
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| s.id == WorkspaceModel::MAIN_TERMINAL_SESSION_ID)
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
    // nested `[H]` Claude session and replies `ok`. The new body takes a gpui
    // `Context` (it spawns a Claude window, like the `claude` arm), so it can no
    // longer be driven from a plain `#[test]` in this binary crate (which never
    // links gpui test-support) — its behavior (nested + top-level-fallback open,
    // the locked title, the `--session-id`/`--model`/`--effort`/prompt argv, and
    // the always-`ok` reply) is pinned end-to-end by the `handoff` self-test
    // scenario (`crate::handoff_live`), and its pure title/prompt/arg helpers are
    // unit-tested in `pty_manager`. Its SELECTION behavior (D7) is pinned by
    // the `#[gpui::test]` below, which drives both handlers under
    // `gpui/test-support`.

    /// Every session id in the model, in tree order — the before/after diff the
    /// selection-contrast test uses to name the session a handler just opened.
    fn session_ids(model: &WorkspaceModel) -> Vec<String> {
        model
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|s| s.id.clone()))
            .collect()
    }

    /// The first session id present in `model` but absent from `before`.
    fn new_session_id(model: &WorkspaceModel, before: &[String]) -> Option<String> {
        session_ids(model).into_iter().find(|id| !before.contains(id))
    }

    /// The ONE behavioral split between the two socket paths that open a session
    /// (D7): a terminal `claude` newtab SELECTS the session it opens; a
    /// `/nice-handoff` does NOT — the originating session keeps selection and key
    /// focus, because a handoff is background continuation prep, not a context
    /// switch. Both handlers need a gpui `Context` (each spawns a Claude window),
    /// hence `#[gpui::test]`.
    ///
    /// Only MODEL-level facts are asserted, so the pty spawn's fate is
    /// irrelevant (both constructors swallow it with `let _ =`). It is also made
    /// harmless: the resolved-`claude` global is pinned to `None` (the real
    /// binary is never reached) and every cwd is a path that does not exist, so
    /// the forked child `_exit`s at its `chdir` and no login shell is ever
    /// sourced.
    #[gpui::test]
    fn handoff_opens_unselected_while_claude_newtab_selects(cx: &mut gpui::TestAppContext) {
        const NO_SPAWN_CWD: &str = "/nice-unit-test-no-such-dir";
        cx.update(|app| app.set_global(crate::pty_manager::ResolvedClaudePath(None)));

        let state = cx.new(|_cx| WindowState::new("/home/u"));

        // Seed the originating Claude session and make it active — the sidebar state
        // a real `/nice-handoff` is invoked from.
        let orig_window = state.update(cx, |ws, _cx| {
            let term_window_id = "t-orig-claude".to_string();
            let mut claude = TermWindow::new(&term_window_id, "Claude", TermWindowKind::Claude);
            claude.is_claude_running = true;
            let mut session = Session::new("t-orig", "my-project", NO_SPAWN_CWD);
            session.windows = vec![
                claude,
                TermWindow::new("t-orig-t1", "Terminal 1", TermWindowKind::Terminal),
            ];
            session.active_window_id = Some(term_window_id.clone());
            session.claude_session_id = Some("orig-session".into());
            session.next_terminal_index = 2;
            ws.workspace.ensure_project("p", "P", NO_SPAWN_CWD);
            let pi = ws.workspace.projects.iter().position(|p| p.id == "p").unwrap();
            ws.workspace.projects[pi].sessions.push(session);
            ws.workspace.select_session("t-orig");
            ws.selection.sync_active_session_id(ws.workspace.active_session_id());
            term_window_id
        });

        // ---- `handoff`: opens a nested session and leaves selection alone ---------
        let before_handoff = state.update(cx, |ws, _cx| session_ids(&ws.workspace));
        let (client, server) = UnixStream::pair().unwrap();
        state.update(cx, |ws, cx| {
            ws.handle_handoff(
                NO_SPAWN_CWD.to_string(),
                "/tmp/handoff.md".to_string(),
                "keep going".to_string(),
                "claude-opus-4-8".to_string(),
                "high".to_string(),
                "t-orig".to_string(),
                orig_window.clone(),
                Reply::for_test(server),
                cx,
            )
        });
        assert_eq!(read_reply(client), "ok\n", "a handoff always replies ok");

        let handoff_session = state.update(cx, |ws, _cx| {
            let id = new_session_id(&ws.workspace, &before_handoff).expect("the handoff opened a session");
            let session = ws.workspace.session_for(&id).expect("the new session is in the model");
            assert_eq!(
                session.parent_session_id.as_deref(),
                Some("t-orig"),
                "the handoff session still nests under the originating session"
            );
            assert_eq!(
                ws.workspace.active_session_id(),
                Some("t-orig"),
                "the handoff must NOT steal the active session from the originating one"
            );
            assert!(
                ws.selection.contains("t-orig"),
                "the untouched active session stays selected"
            );
            assert!(
                !ws.selection.contains(&id),
                "the background handoff session is not selected"
            );
            id
        });

        // ---- `claude` newtab: still selects (deliberately NOT unified) --------
        let before_claude = state.update(cx, |ws, _cx| session_ids(&ws.workspace));
        let (client, server) = UnixStream::pair().unwrap();
        state.update(cx, |ws, cx| {
            ws.handle_claude_socket_request(
                NO_SPAWN_CWD.to_string(),
                Vec::new(),
                // An empty tabId is the newtab decision (no in-place promotion).
                String::new(),
                String::new(),
                Reply::for_test(server),
                cx,
            )
        });
        assert_eq!(read_reply(client), "newtab\n");

        state.update(cx, |ws, _cx| {
            let id = new_session_id(&ws.workspace, &before_claude).expect("the claude newtab opened a session");
            assert_ne!(id, handoff_session, "a second, distinct session");
            assert_eq!(
                ws.workspace.active_session_id(),
                Some(id.as_str()),
                "the terminal `claude` newtab path still selects the session it opens"
            );
            assert!(
                ws.selection.contains(&id),
                "and the selection follows the newly active session"
            );
        });

        // Drop every session container so no pty (however short-lived) outlives
        // the test.
        state.update(cx, |ws, _cx| ws.teardown());
    }

    /// The handler-level facts of `dispatch` that no pure helper can carry:
    /// nesting under the RESOLVED originating session, the top-level fallback on a
    /// stale `tabId`, the locked `[D] …` title, the background (unselected)
    /// open, and — the one real split from `handoff` — that the new session's cwd is
    /// the PAYLOAD cwd (the main checkout root) even though the dispatcher session
    /// itself sits in a worktree.
    ///
    /// Same containment as the handoff test above: only MODEL-level state is
    /// asserted (both constructors swallow the pty spawn with `let _ =`), the
    /// resolved-`claude` global is pinned to `None`, and every cwd is a path that
    /// does not exist so a forked child `_exit`s at its `chdir`.
    #[gpui::test]
    fn dispatch_nests_from_payload_cwd_without_stealing_focus(cx: &mut gpui::TestAppContext) {
        // The dispatcher session has followed claude into a worktree; the payload cwd
        // is the main checkout the helper resolved. They must not be confused.
        const MAIN_ROOT: &str = "/nice-unit-test-no-such-dir/main";
        const DISPATCHER_CWD: &str = "/nice-unit-test-no-such-dir/main/.claude/worktrees/other";
        const TASK_FILE: &str = "/nice-unit-test-no-such-dir/main/.claude/dispatch/fix-tabs-1.md";
        cx.update(|app| app.set_global(crate::pty_manager::ResolvedClaudePath(None)));

        let state = cx.new(|_cx| WindowState::new("/home/u"));

        let orig_window = state.update(cx, |ws, _cx| {
            let term_window_id = "t-orig-claude".to_string();
            let mut claude = TermWindow::new(&term_window_id, "Claude", TermWindowKind::Claude);
            claude.is_claude_running = true;
            let mut session = Session::new("t-orig", "dispatcher", DISPATCHER_CWD);
            session.windows = vec![
                claude,
                TermWindow::new("t-orig-t1", "Terminal 1", TermWindowKind::Terminal),
            ];
            session.active_window_id = Some(term_window_id.clone());
            session.claude_session_id = Some("orig-session".into());
            session.next_terminal_index = 2;
            ws.workspace.ensure_project("p", "P", DISPATCHER_CWD);
            let pi = ws.workspace.projects.iter().position(|p| p.id == "p").unwrap();
            ws.workspace.projects[pi].sessions.push(session);
            ws.workspace.select_session("t-orig");
            ws.selection.sync_active_session_id(ws.workspace.active_session_id());
            term_window_id
        });

        // ---- resolved originating session ⇒ nested, payload cwd, locked title -----
        let before = state.update(cx, |ws, _cx| session_ids(&ws.workspace));
        let (client, server) = UnixStream::pair().unwrap();
        state.update(cx, |ws, cx| {
            ws.handle_dispatch(
                MAIN_ROOT.to_string(),
                "fix-tabs".to_string(),
                TASK_FILE.to_string(),
                String::new(),
                // The default dispatch inherits NOTHING from the dispatcher.
                String::new(),
                String::new(),
                "t-orig".to_string(),
                orig_window.clone(),
                Reply::for_test(server),
                cx,
            )
        });
        assert_eq!(read_reply(client), "ok\n", "a dispatch always replies ok");

        state.update(cx, |ws, _cx| {
            let id = new_session_id(&ws.workspace, &before).expect("the dispatch opened a session");
            let session = ws.workspace.session_for(&id).expect("the new session is in the model");
            assert_eq!(
                session.parent_session_id.as_deref(),
                Some("t-orig"),
                "the dispatch session nests under the originating session"
            );
            assert_eq!(
                session.cwd, MAIN_ROOT,
                "the dispatch spawns from the PAYLOAD cwd (main checkout), never \
                 the dispatcher session's worktree cwd"
            );
            assert_eq!(session.title, "[D] fix-tabs");
            assert!(
                session.title_manually_set,
                "the [D] label is locked against Claude's OSC auto-title"
            );
            assert_eq!(
                ws.workspace.active_session_id(),
                Some("t-orig"),
                "the dispatch must NOT steal the active session"
            );
            assert!(
                !ws.selection.contains(&id),
                "the background dispatch session is not selected"
            );
        });

        // ---- stale tabId ⇒ top-level open, still not an error ------------------
        let before = state.update(cx, |ws, _cx| session_ids(&ws.workspace));
        let (client, server) = UnixStream::pair().unwrap();
        state.update(cx, |ws, cx| {
            ws.handle_dispatch(
                MAIN_ROOT.to_string(),
                "other-task".to_string(),
                TASK_FILE.to_string(),
                String::new(),
                String::new(),
                String::new(),
                "t-gone".to_string(),
                orig_window.clone(),
                Reply::for_test(server),
                cx,
            )
        });
        assert_eq!(read_reply(client), "ok\n", "a resolution miss is not an error");

        state.update(cx, |ws, _cx| {
            let id = new_session_id(&ws.workspace, &before).expect("the dispatch still opened a session");
            let session = ws.workspace.session_for(&id).expect("the new session is in the model");
            assert!(
                session.parent_session_id.is_none(),
                "an unresolvable tabId opens the dispatch session top-level"
            );
            assert_eq!(session.title, "[D] other-task");
            assert_eq!(
                ws.workspace.active_session_id(),
                Some("t-orig"),
                "the top-level fallback still steals no focus"
            );
        });

        // ---- live tabId but a paneId the session does NOT own ⇒ top-level too ------
        // The window-ownership `.filter(...)` guard: without it a payload carrying
        // a real session id plus a foreign/closed window id would nest under the wrong
        // parent.
        let before = state.update(cx, |ws, _cx| session_ids(&ws.workspace));
        let (client, server) = UnixStream::pair().unwrap();
        state.update(cx, |ws, cx| {
            ws.handle_dispatch(
                MAIN_ROOT.to_string(),
                "third-task".to_string(),
                TASK_FILE.to_string(),
                String::new(),
                String::new(),
                String::new(),
                "t-orig".to_string(),
                "not-a-pane-of-t-orig".to_string(),
                Reply::for_test(server),
                cx,
            )
        });
        assert_eq!(read_reply(client), "ok\n", "a window-ownership miss is not an error");

        state.update(cx, |ws, _cx| {
            let id = new_session_id(&ws.workspace, &before).expect("the dispatch still opened a session");
            let session = ws.workspace.session_for(&id).expect("the new session is in the model");
            assert!(
                session.parent_session_id.is_none(),
                "a paneId the resolved session does not own opens the session top-level"
            );
            assert_eq!(
                ws.workspace.active_session_id(),
                Some("t-orig"),
                "the window-ownership fallback still steals no focus"
            );
        });

        state.update(cx, |ws, _cx| ws.teardown());
    }

    // ---- R15 SessionsModelClaudeSocketRequestTests (decision + reply) --------
    //
    // Ported from Swift `SessionsModelClaudeSocketRequestTests`. Each drives
    // `resolve_claude_request` — the spawn-free half of the `claude` handler — so
    // the decision + reply + model mutation are observable without a gpui context
    // (the newtab SPAWN + the claude-lifecycle end-to-end are the slice-3 scenario).

    /// Seed a `[Claude, Terminal 1]` session (Claude focused) into a fresh non-Terminals
    /// project `p` — the Rust twin of `TabModelFixtures.seedClaudeTab`. Returns
    /// `(claude_window_id, terminal_window_id)`.
    fn seed_claude_session(
        model: &mut WorkspaceModel,
        session_id: &str,
        claude_session_id: &str,
        is_running: bool,
    ) -> (String, String) {
        let claude_id = format!("{session_id}-claude");
        let term_id = format!("{session_id}-t1");
        let mut claude = TermWindow::new(&claude_id, "Claude", TermWindowKind::Claude);
        claude.is_claude_running = is_running;
        let mut session = Session::new(session_id, "New session", "/tmp/p");
        session.windows = vec![
            claude,
            TermWindow::new(&term_id, "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(claude_id.clone());
        session.claude_session_id = Some(claude_session_id.to_string());
        session.next_terminal_index = 2;
        model.ensure_project("p", "P", "/tmp/p");
        let pi = model.projects.iter().position(|p| p.id == "p").unwrap();
        model.projects[pi].sessions.push(session);
        (claude_id, term_id)
    }

    /// Drive `resolve_claude_request` and return the single reply line it wrote
    /// (with its trailing `\n`). Ignores the returned newtab-spawn request (the
    /// spawn is the scenario's concern; these tests pin the decision + reply).
    fn drive_claude(
        state: &mut WindowState,
        cwd: &str,
        args: &[&str],
        session_id: &str,
        term_window_id: &str,
    ) -> String {
        let (client, server) = UnixStream::pair().unwrap();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let _ = state.resolve_claude_request(cwd, &args, session_id, term_window_id, Reply::for_test(server));
        read_reply(client)
    }

    #[test]
    fn claude_empty_session_id_replies_newtab() {
        let mut state = WindowState::new("/home/u");
        assert_eq!(drive_claude(&mut state, "/tmp/x", &[], "", "p"), "newtab\n");
    }

    #[test]
    fn claude_terminals_project_session_replies_newtab() {
        // The pinned Terminals group never hosts Claude — a bare `claude` from the
        // Main window opens a fresh session, never promotes the Main window in place.
        let mut state = WindowState::new("/home/u");
        let main = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
        let main_window = state.workspace.session_for(main).unwrap().windows[0].id.clone();

        assert_eq!(drive_claude(&mut state, "/tmp/x", &[], main, &main_window), "newtab\n");

        let term_window = &state.workspace.session_for(main).unwrap().windows[0];
        assert_eq!(term_window.kind, TermWindowKind::Terminal, "Main window must NOT promote");
        assert!(!term_window.is_claude_running, "Main window must NOT flip claude-running");
    }

    #[test]
    fn claude_window_id_not_in_session_replies_newtab() {
        // A stale paneId (window exited while the wrapper's nc was in flight) falls
        // through to a new session.
        let mut state = WindowState::new("/home/u");
        seed_claude_session(&mut state.workspace, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &[], "t1", "does-not-exist"),
            "newtab\n"
        );
    }

    #[test]
    fn claude_existing_running_claude_replies_newtab() {
        // The ≤1-Claude-per-session invariant: a session that already has a live Claude
        // window opens a fresh session rather than promoting a second one.
        let mut state = WindowState::new("/home/u");
        let (_c, term) = seed_claude_session(&mut state.workspace, "t1", "OLD", true);
        assert_eq!(drive_claude(&mut state, "/tmp/p", &[], "t1", &term), "newtab\n");
        let term_window = state
            .workspace
            .session_for("t1")
            .unwrap()
            .windows
            .iter()
            .find(|w| w.id == term)
            .unwrap();
        assert_eq!(term_window.kind, TermWindowKind::Terminal, "terminal window must NOT promote");
        assert!(!term_window.is_claude_running);
    }

    #[test]
    fn claude_inplace_with_session_id_flips_running_and_replies_inplace() {
        // Deferred-resume promotion: args already carry `--resume <uuid>`, so the
        // reply is the bare `inplace` (wrapper passes args through) and the window's
        // is_claude_running flips false→true (the gate-release signal T5 keys on).
        let mut state = WindowState::new("/home/u");
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            "inplace\n"
        );

        let session = state.workspace.session_for("t1").unwrap();
        let term_window = session.windows.iter().find(|w| w.id == claude).unwrap();
        assert!(term_window.is_claude_running, "deferred-resume promotion flips running");
        assert_eq!(term_window.kind, TermWindowKind::Claude);
        assert_eq!(term_window.title, "Claude", "pill reset to Claude until the OSC arrives");
        assert_eq!(session.active_window_id.as_deref(), Some(claude.as_str()));
        assert_eq!(
            session.claude_session_id.as_deref(),
            Some("abc-123"),
            "the id parsed from --resume overwrites the seeded session id"
        );
    }

    #[test]
    fn claude_inplace_without_session_id_mints_and_replies_with_it() {
        // Plain `claude` in a terminal window inside a Claude session: mint a fresh id and
        // ship it back so the wrapper can prepend `--session-id <uuid>`.
        let mut state = WindowState::new("/home/u");
        let (_c, term) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        let reply = drive_claude(&mut state, "/tmp/p", &[], "t1", &term);
        let reply = reply.trim_end();
        assert!(reply.starts_with("inplace "), "reply is 'inplace <uuid>': {reply:?}");
        let minted = reply.strip_prefix("inplace ").unwrap();
        assert!(!minted.is_empty(), "reply carries the freshly minted uuid");

        let session = state.workspace.session_for("t1").unwrap();
        assert_eq!(
            session.claude_session_id.as_deref(),
            Some(minted),
            "wrapper + model must agree on the persisted session id"
        );
        let term_window = session.windows.iter().find(|w| w.id == term).unwrap();
        assert_eq!(term_window.kind, TermWindowKind::Claude, "terminal window promotes to claude");
        assert!(term_window.is_claude_running);
        assert_eq!(term_window.title, "Claude");
    }

    #[test]
    fn claude_inplace_with_session_id_sync_on_appends_settings_pointer() {
        // Sync on + user-supplied session id → 'inplace - <pointer>' (the `-` sid
        // placeholder lets the --settings path follow as the 3rd field).
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);
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
        let (_c, term) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        let reply = drive_claude(&mut state, "/tmp/p", &[], "t1", &term);
        let parts: Vec<&str> = reply.trim_end().split(' ').collect();
        assert_eq!(parts.len(), 3, "reply is 'inplace <uuid> <pointer>': {reply:?}");
        assert_eq!(parts[0], "inplace");
        assert_ne!(parts[1], "-", "mint-new path uses the real minted id");
        assert_eq!(parts[2], "/ptr.json", "third field is the --settings pointer");
        assert_eq!(
            state.workspace.session_for("t1").unwrap().claude_session_id.as_deref(),
            Some(parts[1]),
            "minted id in the reply matches the persisted session's Claude session id"
        );
    }

    #[test]
    fn claude_inplace_sync_off_replies_byte_identical() {
        // Sync off (the default): reply is byte-identical to the pre-theming protocol.
        let mut state = WindowState::new("/home/u");
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", "abc-123"], "t1", &claude),
            "inplace\n"
        );
    }

    #[test]
    fn claude_inplace_sync_on_args_already_have_settings_does_not_double() {
        // A restored deferred window re-dispatches its pre-typed `claude --settings
        // <path> --resume <id>`; the reply must NOT append a second pointer.
        let mut state = WindowState::new("/home/u");
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);
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

    // ---- Fix D: exec-time resume/attach normalization -----------------------
    //
    // The wrapper ships every interactive `claude` argv through this handler, so
    // this is the one place that can decide — at the moment the user presses
    // Enter on a pre-typed command — whether the session it names is still
    // daemon-hosted (`attach <short id>`) or only on disk (`--resume <uuid>`).
    // Every case drives the same reply path with a FIXTURE jobs probe, so none
    // of them reads the developer's real `~/.claude`.

    /// A full session uuid whose first 8 characters key its jobs directory.
    const HOSTED_UUID: &str = "b8c8244b-e94e-4c38-95fb-31be9a28187e";

    /// A jobs probe reporting exactly one live daemon job: a
    /// `jobs/<first8(uuid)>/state.json` whose `sessionId` is `uuid`. Keying on
    /// the first 8 characters is what makes the collision case below expressible
    /// (a foreign job answering the same probe).
    fn hosted_job_probe(uuid: &'static str) -> impl Fn(&str) -> Option<ForkJobInfo> {
        move |probed: &str| {
            (probed.get(..8).is_some() && probed.get(..8) == uuid.get(..8)).then(|| ForkJobInfo {
                claude_session_id: Some(uuid.to_string()),
                ..Default::default()
            })
        }
    }

    #[test]
    fn claude_resume_of_a_daemon_hosted_session_replies_attach() {
        // The materialized fork session's pre-typed `claude --resume <fork id>`, run
        // while the daemon still hosts the job: resuming would spawn a SECOND
        // process against a live conversation, so the wrapper is told to attach.
        // The FULL uuid rides the reply — the wrapper derives attach's short id
        // from it and needs it for the fallback leg.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", HOSTED_UUID], "t1", &claude),
            format!("attach {HOSTED_UUID}\n")
        );
        assert_eq!(
            state.workspace.session_for("t1").unwrap().claude_session_id.as_deref(),
            Some(HOSTED_UUID),
            "the session keeps the full uuid — the short id is an exec detail"
        );
    }

    #[test]
    fn claude_resume_short_flag_and_settings_pointer_ride_the_attach_reply() {
        // `-r <uuid>` is the same shape (the shared `extract_claude_session_id`
        // never learned it), and theme sync still gets its 3rd field — the
        // wrapper needs it for the `--resume` fallback, not for attach itself.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["-r", HOSTED_UUID], "t1", &claude),
            format!("attach {HOSTED_UUID} /ptr.json\n")
        );
    }

    #[test]
    fn claude_resume_without_a_jobs_entry_is_unchanged() {
        // No jobs entry ⇒ nothing is hosting the session ⇒ today's flow, byte for
        // byte (the durable `--resume` the user typed).
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", HOSTED_UUID], "t1", &claude),
            "inplace\n"
        );
        assert_eq!(
            state.workspace.session_for("t1").unwrap().claude_session_id.as_deref(),
            Some(HOSTED_UUID)
        );
    }

    #[test]
    fn claude_resume_with_a_first8_collision_is_unchanged() {
        // A jobs entry is keyed by 8 hex characters, so a FOREIGN job can answer
        // the probe. `state.json`'s sessionId is the tiebreak: it names someone
        // else's conversation, so attaching would drop the user into it.
        let other = "b8c8244b-0000-0000-0000-000000000000";
        assert_eq!(&other[..8], &HOSTED_UUID[..8], "precondition: the prefixes collide");
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(other));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", HOSTED_UUID], "t1", &claude),
            "inplace\n"
        );
    }

    #[test]
    fn claude_resume_with_a_jobs_entry_missing_state_json_is_unchanged() {
        // The directory lands before `state.json` does. Until the uuid can be
        // CONFIRMED, the durable `--resume` stands.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| Some(ForkJobInfo::default()));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", HOSTED_UUID], "t1", &claude),
            "inplace\n"
        );
    }

    #[test]
    fn claude_attach_of_an_evicted_job_replies_resume() {
        // The user typed `claude attach <full uuid>` for a job the daemon has
        // dropped: attach can only fail (it prefix-matches jobs DIRECTORY names,
        // which a full uuid never matches, and the entry is gone besides), so the
        // wrapper is handed the recoverable `--resume <uuid>` instead.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        state.set_claude_settings_path_for_test(Some("/ptr.json".into()));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["attach", HOSTED_UUID], "t1", &claude),
            format!("resume {HOSTED_UUID} /ptr.json\n")
        );
        assert_eq!(
            state.workspace.session_for("t1").unwrap().claude_session_id.as_deref(),
            Some(HOSTED_UUID),
            "`attach <id>` is session-identifying — the session adopts the id"
        );
    }

    #[test]
    fn claude_attach_of_a_hosted_full_uuid_replies_attach() {
        // Same shape, still hosted: normalize the other way so attach gets the
        // short id it can actually resolve.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["attach", HOSTED_UUID], "t1", &claude),
            format!("attach {HOSTED_UUID}\n")
        );
    }

    #[test]
    fn claude_attach_of_a_short_id_passes_through_and_splices_no_session_id() {
        // The `claude agents`-shaped invocation. attach is already the right verb,
        // so the args run as typed — but they IDENTIFY a session, so the reply
        // must be the bare `inplace` (a spliced `--session-id <fresh uuid>` would
        // both fight the attach and orphan the conversation). The session adopts the
        // job's full uuid so a later relaunch can `--resume` it.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["attach", &HOSTED_UUID[..8]], "t1", &claude),
            "inplace\n"
        );
        assert_eq!(
            state.workspace.session_for("t1").unwrap().claude_session_id.as_deref(),
            Some(HOSTED_UUID),
            "the short id resolves to the job's full uuid"
        );
    }

    #[test]
    fn claude_attach_of_an_unresolvable_short_id_leaves_the_pinned_id_alone() {
        // No jobs entry and no uuid in the args: nothing is recoverable, so the
        // args pass through (attach reports the miss itself, exiting 1) and the
        // session keeps the session id it had — pinning `deadbeef` would leave it
        // holding an id no `--resume` could ever use.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["attach", "deadbeef"], "t1", &claude),
            "inplace\n"
        );
        assert_eq!(
            state.workspace.session_for("t1").unwrap().claude_session_id.as_deref(),
            Some("OLD"),
            "an unresolvable attach id must not overwrite the session's claude session id"
        );
    }

    #[test]
    fn claude_valueless_resume_picker_still_mints_a_session_id() {
        // A bare `-r` opens Claude's interactive picker: it names no session, so
        // there is nothing to normalize and the mint-new path stands.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let (_c, term) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        let reply = drive_claude(&mut state, "/tmp/p", &["-r"], "t1", &term);
        let minted = reply.trim_end().strip_prefix("inplace ").unwrap_or_default();
        assert!(!minted.is_empty(), "picker invocation mints an id: {reply:?}");
    }

    #[test]
    fn classify_claude_session_args_shapes() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let uuid = HOSTED_UUID.to_string();
        for form in [
            vec!["--resume", HOSTED_UUID],
            vec!["-r", HOSTED_UUID],
            vec![&format!("--resume={HOSTED_UUID}")],
            vec!["--model", "sonnet", "--resume", HOSTED_UUID],
        ] {
            assert_eq!(
                classify_claude_session_args(&args(&form)),
                ClaudeArgSession::Resume(uuid.clone()),
                "{form:?} names a session to resume"
            );
        }
        assert_eq!(
            classify_claude_session_args(&args(&["attach", "b8c8244b"])),
            ClaudeArgSession::Attach("b8c8244b".to_string())
        );
        for form in [
            vec![],
            vec!["-r"],
            vec!["--resume", "--model"],
            vec!["--resume="],
            vec!["attach"],
            vec!["attach", "-h"],
            // `attach` is a subcommand — only ever first.
            vec!["--model", "opus", "attach", "b8c8244b"],
            vec!["--session-id", HOSTED_UUID],
        ] {
            assert_eq!(
                classify_claude_session_args(&args(&form)),
                ClaudeArgSession::Neither,
                "{form:?} names no session"
            );
        }
    }

    #[test]
    fn looks_like_session_uuid_separates_full_ids_from_short_job_ids() {
        assert!(looks_like_session_uuid(HOSTED_UUID));
        assert!(!looks_like_session_uuid("b8c8244b"), "the 8-hex attach id");
        assert!(!looks_like_session_uuid(""));
        assert!(
            !looks_like_session_uuid("b8c8244b-e94e-4c38-95fb-31be9a28187"),
            "one character short"
        );
        assert!(
            !looks_like_session_uuid("b8c8244b-e94e-4c38-95fb-31be9a28187z"),
            "non-hex payload"
        );
        assert!(
            !looks_like_session_uuid("b8c8244be94e-4c38-95fb-31be9a28187e"),
            "hyphens in the wrong places"
        );
    }

    // ---- claude_exited (the attach child returned) --------------------------

    #[test]
    fn claude_exited_reopens_the_window_to_in_place_promotion() {
        // The validation break, end to end at the model layer: the window was
        // promoted (Fix D replied `attach`, which runs Claude as a CHILD), the
        // user detached, and the window is a shell prompt again. Until the wrapper
        // says so, `is_claude_running` stays true and the ≤1-Claude guard sends
        // the NEXT `claude` in this session to a brand-new session.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);

        // Promote in place — the state the attach verb leaves behind.
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", HOSTED_UUID], "t1", &claude),
            "inplace\n"
        );
        assert!(window(&state, "t1", &claude).is_claude_running);

        state.handle_claude_exited(claude.clone());
        assert!(
            !window(&state, "t1", &claude).is_claude_running,
            "a returned attach child leaves a shell prompt, not a running Claude"
        );

        // And the window promotes in place again instead of opening a stray session.
        assert_eq!(
            drive_claude(&mut state, "/tmp/p", &["--resume", HOSTED_UUID], "t1", &claude),
            "inplace\n"
        );
    }

    #[test]
    fn claude_exited_for_an_unknown_window_is_a_no_op() {
        let mut state = WindowState::new("/home/u");
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", true);
        state.handle_claude_exited("no-such-pane".to_string());
        assert!(
            window(&state, "t1", &claude).is_claude_running,
            "a stale window id must touch nothing"
        );
    }

    /// Read a window out of a session (test-only convenience).
    fn window<'a>(state: &'a WindowState, session_id: &str, window_id: &str) -> &'a TermWindow {
        state
            .workspace
            .session_for(session_id)
            .expect("session")
            .windows
            .iter()
            .find(|w| w.id == window_id)
            .expect("window")
    }

    // ---- Fix D on the NEWTAB branch ----------------------------------------
    //
    // The in-place guard refuses plenty of real invocations (a Terminals session, a
    // stale window id, a session that already runs a Claude), and the fresh session it
    // opens instead used to prepend its own minted `--session-id` to whatever
    // the user typed. Beside a `--resume`/`attach` that is an argv Claude Code
    // rejects outright, so the window died at once. These pin the decision AND the
    // exec line the session constructor ultimately builds from it.

    /// Drive `resolve_claude_request` with an EMPTY session id (the guard's
    /// unknown-session arm, always a newtab) and return the spawn request it owes
    /// the caller, asserting the reply was `newtab`.
    fn drive_claude_newtab(state: &mut WindowState, args: &[&str]) -> NewSessionSpawn {
        let (client, server) = UnixStream::pair().unwrap();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let spawn = state
            .resolve_claude_request("/tmp/p", &args, "", "p", Reply::for_test(server))
            .expect("the unknown-session arm owes a newtab spawn");
        assert_eq!(read_reply(client), "newtab\n");
        spawn
    }

    /// The exec line the new session's Claude window runs, built from the spawn the
    /// handler returned — the same composer `create_claude_tab` feeds.
    fn newtab_exec_line(spawn: &NewSessionSpawn, settings: Option<&str>) -> String {
        crate::pty_manager::build_claude_exec_command(
            "/c",
            &spawn.spec.mode,
            &spawn.args,
            false,
            settings,
        )
    }

    #[test]
    fn newtab_resume_request_splices_no_session_id() {
        // The observed break: `claude --resume <fork uuid>` typed in a session whose
        // Claude is already running opened a new session that exec'd
        // `claude --session-id <minted> --resume <fork uuid>` — which the CLI
        // refuses ("--session-id can only be used with --continue or --resume if
        // --fork-session is also specified"), killing the window on the spot.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        let spawn = drive_claude_newtab(&mut state, &["--resume", HOSTED_UUID]);

        assert_eq!(spawn.args, vec!["--resume".to_string(), HOSTED_UUID.to_string()]);
        assert_eq!(
            spawn.spec.pin.as_deref(),
            Some(HOSTED_UUID),
            "the new session remembers the session it resumed, not a minted phantom"
        );
        // The user's args ride through the shared single-quoter, so the flag is
        // quoted too — same argv, no `--session-id` anywhere in it.
        assert_eq!(
            newtab_exec_line(&spawn, Some("/ptr.json")),
            format!("exec '/c' --settings '/ptr.json' '--resume' '{HOSTED_UUID}'"),
        );
    }

    #[test]
    fn newtab_resume_of_a_daemon_hosted_session_attaches() {
        // Same normalization the in-place reply does, on the branch that spawns
        // its own window: a `--resume` would race the live background process.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let spawn = drive_claude_newtab(&mut state, &["--resume", HOSTED_UUID]);

        assert_eq!(spawn.args, vec!["attach".to_string(), HOSTED_UUID[..8].to_string()]);
        assert_eq!(spawn.spec.pin.as_deref(), Some(HOSTED_UUID));
        assert_eq!(
            newtab_exec_line(&spawn, Some("/ptr.json")),
            format!("exec '/c' attach '{}'", &HOSTED_UUID[..8]),
            "a global flag before the subcommand makes the CLI stop seeing one"
        );
    }

    #[test]
    fn newtab_attach_of_an_evicted_full_uuid_resumes() {
        // `attach` prefix-matches jobs DIRECTORY names, so a full uuid can only
        // ever miss — and with no job left there is nothing to attach to anyway.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        let spawn = drive_claude_newtab(&mut state, &["attach", HOSTED_UUID]);

        assert_eq!(spawn.args, vec!["--resume".to_string(), HOSTED_UUID.to_string()]);
        assert_eq!(spawn.spec.pin.as_deref(), Some(HOSTED_UUID));
        assert_eq!(
            newtab_exec_line(&spawn, None),
            format!("exec '/c' --resume '{HOSTED_UUID}'")
        );
    }

    #[test]
    fn newtab_attach_of_a_short_id_runs_the_subcommand() {
        // The `claude agents`-shaped invocation: already the right verb, so it
        // runs as typed — as a SUBCOMMAND (no theme pointer ahead of it) and
        // with no `--session-id` spliced in.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let spawn = drive_claude_newtab(&mut state, &["attach", &HOSTED_UUID[..8]]);

        assert_eq!(
            spawn.spec.pin.as_deref(),
            Some(HOSTED_UUID),
            "the short id resolves to the job's full uuid"
        );
        assert_eq!(
            newtab_exec_line(&spawn, Some("/ptr.json")),
            format!("exec '/c' attach '{}'", &HOSTED_UUID[..8])
        );
    }

    #[test]
    fn newtab_without_a_named_session_still_mints_one() {
        // The untouched majority path: a plain `claude …` opens a fresh session
        // under a minted `--session-id`, args verbatim after it.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(hosted_job_probe(HOSTED_UUID));
        let spawn = drive_claude_newtab(&mut state, &["-w", "feature"]);

        assert_eq!(spawn.args, vec!["-w".to_string(), "feature".to_string()]);
        let minted = spawn.spec.pin.clone().expect("a fresh session mints an id");
        assert!(looks_like_session_uuid(&minted), "a real v4 uuid: {minted}");
        assert_eq!(
            newtab_exec_line(&spawn, None),
            format!("exec '/c' --session-id '{minted}' '-w' 'feature'")
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
    fn session_update_records_normalized_message_and_unknown_window_is_no_op() {
        let mut state = WindowState::new("/home/u");
        // session_update is fire-and-forget and context-free — drive the sub-handler
        // directly. It records the parsed, normalized message, and an unknown window id
        // (no session owns "P1") classifies as a silent no-op ⇒ no branch-parent spawn.
        let outcome = state.handle_session_update("P1".into(), "S1".into(), Some("resume".into()), None);
        assert!(outcome.spawn.is_none(), "an unknown window must not materialize a branch parent");
        let recorded = state.recorded_socket_messages();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0],
            RecordedSocketMessage::SessionUpdate {
                term_window_id: "P1".into(),
                claude_session_id: "S1".into(),
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
    // PersistedSession JSON encode/decode tests. Their model half (`Session::parent_session_id`)
    // is landed and exercised by the branch cases below; the persisted-shape
    // round-trip lands with R18's session store.

    /// Seed a `[Claude, Terminal 1]` session (Claude focused, NOT running — deferred /
    /// pre-promotion shape) into a fresh non-Terminals project `project_id` at
    /// `path`, with `claude_session_id`. The Rust twin of `TabModelFixtures.seedClaudeTab`.
    /// The session cwd + project path are both `path`. Claude window `<session>-claude`,
    /// terminal `<session>-t1`.
    fn seed_rotation_session(
        model: &mut WorkspaceModel,
        project_id: &str,
        session_id: &str,
        claude_session_id: &str,
        path: &str,
    ) {
        let claude_id = format!("{session_id}-claude");
        let term_id = format!("{session_id}-t1");
        let mut session = Session::new(session_id, "New session", path);
        session.windows = vec![
            TermWindow::new(&claude_id, "Claude", TermWindowKind::Claude),
            TermWindow::new(&term_id, "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(claude_id);
        session.claude_session_id = Some(claude_session_id.to_string());
        session.next_terminal_index = 2;
        model.ensure_project(project_id, &project_id.to_uppercase(), path);
        let pi = model.projects.iter().position(|p| p.id == project_id).unwrap();
        model.projects[pi].sessions.push(session);
    }

    /// The sessions of project `project_id`, cloned for post-mutation assertions.
    fn project_sessions(state: &WindowState, project_id: &str) -> Vec<Session> {
        state
            .workspace
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.sessions.clone())
            .unwrap_or_default()
    }

    fn session_claude_id(state: &WindowState, session_id: &str) -> Option<String> {
        state
            .workspace
            .session_for(session_id)
            .and_then(|s| s.claude_session_id.clone())
    }

    // === AppStateClaudeSessionUpdateTests =====================================

    #[test]
    fn session_update_unknown_window_id_is_no_op() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/tmp/p");
        let out = state.apply_session_update("definitely-not-a-real-pane-id", "should-be-ignored", None, None);
        assert!(!out.did_mutate);
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("S1"), "unknown window must not mutate any session");
    }

    #[test]
    fn session_update_updates_target_session_when_multiple_projects_exist() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p1", "t1", "S1", "/tmp/p1");
        seed_rotation_session(&mut state.workspace, "p2", "t2", "S2", "/tmp/p2");
        seed_rotation_session(&mut state.workspace, "p3", "t3", "S3", "/tmp/p3");
        // Update the middle session — the reverse scan must hit the right project even
        // when it is not first.
        state.apply_session_update("t2-claude", "S2-NEW", None, None);
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("S1"));
        assert_eq!(session_claude_id(&state, "t2").as_deref(), Some("S2-NEW"));
        assert_eq!(session_claude_id(&state, "t3").as_deref(), Some("S3"));
    }

    #[test]
    fn session_update_resolves_by_window_id_not_session_id() {
        // Window ids and session ids are distinct namespaces; passing a session id must not
        // match a session (its window list holds "t1-claude"/"t1-t1", not "t1").
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/tmp/p");
        let out = state.apply_session_update("t1", "should-not-apply", None, None);
        assert!(!out.did_mutate);
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("S1"));
    }

    #[test]
    fn session_update_redundant_update_leaves_value_unchanged() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/tmp/p");
        let first = state.apply_session_update("t1-claude", "S1", None, None);
        let second = state.apply_session_update("t1-claude", "S1", None, None);
        assert!(!first.did_mutate, "same id must not mutate");
        assert!(!second.did_mutate, "the second redundant update must not mutate either");
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("S1"));
    }

    #[test]
    fn session_update_new_session_id_replaces_old() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        let out = state.apply_session_update("t1-claude", "NEW", None, None);
        assert!(out.did_mutate);
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("NEW"));
    }

    #[test]
    fn session_update_is_scoped_to_owning_window() {
        // Window A owns "tA-claude"; window B owns "tB-claude". A cross-window send
        // (A's handler receives B's window) is a no-op on both — A's session_id_owning
        // returns None, and nothing dispatched to B.
        let mut a = WindowState::new("/home/u");
        seed_rotation_session(&mut a.workspace, "pA", "tA", "A-INIT", "/tmp/pA");
        let mut b = WindowState::new("/home/u");
        seed_rotation_session(&mut b.workspace, "pB", "tB", "B-INIT", "/tmp/pB");

        a.apply_session_update("tB-claude", "LEAKED", None, None);
        assert_eq!(session_claude_id(&a, "tA").as_deref(), Some("A-INIT"), "A untouched by a B-shaped window");
        assert_eq!(session_claude_id(&b, "tB").as_deref(), Some("B-INIT"), "B untouched until its own handler runs");

        b.apply_session_update("tB-claude", "B-NEW", None, None);
        assert_eq!(session_claude_id(&b, "tB").as_deref(), Some("B-NEW"));
        assert_eq!(session_claude_id(&a, "tA").as_deref(), Some("A-INIT"), "B's mutation must not bleed into A");
    }

    #[test]
    fn session_update_stale_window_after_window_exited_is_no_op() {
        // The hook fires asynchronously: a session_update can land after its window
        // exited. The session still exists (its terminal window survives), but the window id
        // no longer maps to it, so the id must not be mutated.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/tmp/p");
        // Baseline: a live update lands.
        state.apply_session_update("t1-claude", "S1-LIVE", None, None);
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("S1-LIVE"));
        // The claude window exits (model-only removal — no live pty needed).
        let (model, selection) = (&mut state.workspace, &mut state.selection);
        state.ptys.window_exited(model, selection, "t1", "t1-claude");
        assert!(
            state.workspace.session_for("t1").is_some_and(|s| !s.windows.iter().any(|w| w.id == "t1-claude")),
            "precondition: claude window is gone after window_exited"
        );
        // A late update for the now-defunct window arrives.
        let out = state.apply_session_update("t1-claude", "S1-STALE", None, None);
        assert!(!out.did_mutate);
        assert_eq!(
            session_claude_id(&state, "t1").as_deref(),
            Some("S1-LIVE"),
            "stale window id must not mutate the surviving session"
        );
    }

    // -- cwd update path --------------------------------------------------------

    #[test]
    fn session_update_cwd_matching_current_is_no_op() {
        // Steady state: every SessionStart emits cwd even when nothing moved. The
        // matching-cwd + matching-id case must not churn (both branches short-circuit).
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let out = state.apply_session_update("t1-claude", "S1", Some("clear"), Some("/Users/nick/Projects/notes"));
        assert_eq!(state.workspace.session_for("t1").map(|s| s.cwd.as_str()), Some("/Users/nick/Projects/notes"));
        assert!(!out.did_mutate, "matching cwd + matching id must not fire the save signal");
    }

    #[test]
    fn session_update_cwd_differing_updates_session_and_claude_window() {
        // The shape the feature fixes: bare `claude -w` lands in an auto-named
        // worktree; the hook forwards it and session.cwd + the (nil-cwd) claude window follow.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        let out = state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let session = state.workspace.session_for("t1").unwrap();
        assert_eq!(session.cwd, worktree, "session.cwd moves to the worktree");
        let claude = session.windows.iter().find(|w| w.kind == TermWindowKind::Claude).unwrap();
        assert_eq!(claude.cwd.as_deref(), Some(worktree), "nil-cwd claude window follows the session");
        assert!(out.did_mutate, "cwd change must fire the save signal");
    }

    #[test]
    fn session_update_cwd_companion_terminal_follows_when_matching_old_cwd() {
        // A terminal companion still tracking the pre-update session.cwd (not yet cd'd
        // via OSC 7) is pulled along so a later shell lands inside the worktree.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        state.workspace.mutate_session("t1", |session| {
            for term_window in session.windows.iter_mut().filter(|w| w.kind == TermWindowKind::Terminal) {
                term_window.cwd = Some("/Users/nick/Projects/notes".into());
            }
        });
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let term = state.workspace.session_for("t1").unwrap().windows.iter().find(|w| w.kind == TermWindowKind::Terminal).unwrap().cwd.clone();
        assert_eq!(term.as_deref(), Some(worktree), "companion matching the old cwd follows into the worktree");
    }

    #[test]
    fn session_update_cwd_companion_terminal_diverged_stays_put() {
        // A companion already tracking the user elsewhere via OSC 7 must not snap
        // back into the claude worktree.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        let user_cd = "/Users/nick/Projects/notes/some/subdir";
        state.workspace.mutate_session("t1", |session| {
            for term_window in session.windows.iter_mut().filter(|w| w.kind == TermWindowKind::Terminal) {
                term_window.cwd = Some(user_cd.into());
            }
        });
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let term = state.workspace.session_for("t1").unwrap().windows.iter().find(|w| w.kind == TermWindowKind::Terminal).unwrap().cwd.clone();
        assert_eq!(term.as_deref(), Some(user_cd), "diverged OSC-7-tracked companion stays put");
    }

    #[test]
    fn session_update_cwd_nil_window_cwd_follows_the_session() {
        // A nil window.cwd is "still following the session" and inherits the new session.cwd
        // (the rule that makes the always-nil-cwd claude window track the worktree).
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        assert!(
            state.workspace.session_for("t1").unwrap().windows.iter().find(|w| w.kind == TermWindowKind::Terminal).unwrap().cwd.is_none(),
            "precondition: terminal window cwd starts nil"
        );
        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "S1", Some("startup"), Some(worktree));
        let term = state.workspace.session_for("t1").unwrap().windows.iter().find(|w| w.kind == TermWindowKind::Terminal).unwrap().cwd.clone();
        assert_eq!(term.as_deref(), Some(worktree), "nil window cwd inherits the new session.cwd");
    }

    #[test]
    fn session_update_cwd_nil_in_payload_is_no_op() {
        // An older hook script omits cwd; the socket normalizes it to None, and the
        // handler short-circuits without touching session.cwd.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        state.apply_session_update("t1-claude", "S1", Some("clear"), None);
        assert_eq!(state.workspace.session_for("t1").map(|s| s.cwd.as_str()), Some("/Users/nick/Projects/notes"));
    }

    #[test]
    fn session_update_cwd_empty_in_payload_is_no_op() {
        // Defense-in-depth: an empty-string cwd is treated as None even if the socket
        // layer regressed (the cwd field rode in from a user-modifiable hook script).
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
        state.apply_session_update("t1-claude", "S1", Some("clear"), Some(""));
        assert_eq!(state.workspace.session_for("t1").map(|s| s.cwd.as_str()), Some("/Users/nick/Projects/notes"));
    }

    #[test]
    fn session_update_cwd_identical_updates_mutate_exactly_once() {
        // Two same-cwd updates: only the first mutates; the second is already at the
        // target value and short-circuits in adopt_session_cwd's change detection.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S1", "/Users/nick/Projects/notes");
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
        // must inherit the OLD cwd even though the originating session moves. If the cwd
        // update ran before materialization, the sibling would pick up the
        // post-rotation worktree and its resume would point at the wrong bucket.
        let original_cwd = "/Users/nick/Projects/notes";
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD-ID", original_cwd);
        assert_eq!(state.workspace.session_for("t1").map(|s| s.cwd.as_str()), Some(original_cwd));

        let new_cwd = "/Users/nick/Projects/notes/.claude/worktrees/auto-name";
        state.apply_session_update("t1-claude", "NEW-ID", Some("resume"), Some(new_cwd));

        // The originating session — post-rotation — sits in the worktree with the new id.
        let orig = state.workspace.session_for("t1").unwrap();
        assert_eq!(orig.cwd, new_cwd, "originating session reflects the post-rotation cwd");
        assert_eq!(orig.claude_session_id.as_deref(), Some("NEW-ID"));

        // The sibling parent — pinned to OLD-ID — holds the PRE-rotation cwd.
        let sessions = project_sessions(&state, "p");
        let sibling = sessions.iter().find(|s| s.claude_session_id.as_deref() == Some("OLD-ID"));
        let sibling = sibling.expect("branch rotation must materialize a sibling parent");
        assert_eq!(
            sibling.cwd, original_cwd,
            "sibling parent inherits the OLD cwd — its old-id transcript lives in the pre-rotation bucket"
        );
    }

    // === AppStateBranchTrackingTests ==========================================

    #[test]
    fn branch_resume_with_id_change_creates_parent_session() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.workspace.mutate_session("t1", |s| s.title = "wire up the foo".into());

        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);

        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 2, "branch adds exactly one sibling parent session");
        // Parent inserted immediately above the originating session: order reads [parent, child].
        let (parent, child) = (&sessions[0], &sessions[1]);
        assert_eq!(child.id, "t1", "originating session keeps its id");
        assert_eq!(child.claude_session_id.as_deref(), Some("NEW"), "originating session adopts the post-rotation id");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent.id.as_str()), "originating session points at the new parent");
        assert_eq!(parent.claude_session_id.as_deref(), Some("OLD"), "parent pinned to the pre-rotation id");
        assert!(parent.parent_session_id.is_none(), "parent stays at root");
        assert_eq!(parent.title, "wire up the foo", "parent inherits the title");
        assert_eq!(parent.cwd, child.cwd, "parent inherits the cwd");
        assert_eq!(parent.windows.len(), 2);
        assert!(parent.windows.iter().any(|w| w.kind == TermWindowKind::Claude), "parent has a claude window");
        assert!(parent.windows.iter().any(|w| w.kind == TermWindowKind::Terminal), "parent has a companion terminal");
    }

    #[test]
    fn branch_clear_with_id_change_does_not_create_parent() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", Some("clear"), None);
        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 1, "/clear must not spawn a parent session");
        assert_eq!(sessions[0].claude_session_id.as_deref(), Some("NEW"), "/clear still updates the id in place");
        assert!(sessions[0].parent_session_id.is_none());
    }

    #[test]
    fn branch_missing_source_does_not_create_parent() {
        // Older hook payloads (and any future Claude that drops `source`) surface as
        // None; the conservative no-parent path — rather miss a /branch than
        // misclassify a /clear.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", None, None);
        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 1, "missing source must not spawn a parent session");
        assert_eq!(sessions[0].claude_session_id.as_deref(), Some("NEW"));
    }

    #[test]
    fn branch_resume_with_same_id_does_not_create_parent() {
        // A real `claude --resume <id>` keeps the id stable; the short-circuit
        // absorbs it and the id-change guard blocks the parent.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "SAME", "/tmp/p");
        state.apply_session_update("t1-claude", "SAME", Some("resume"), None);
        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 1, "resume without rotation must not spawn a parent session");
        assert_eq!(sessions[0].claude_session_id.as_deref(), Some("SAME"));
    }

    #[test]
    fn branch_first_promotes_parent_to_root_and_originating_becomes_child() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S0", "/tmp/p");
        state.apply_session_update("t1-claude", "S1", Some("resume"), None);
        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 2);
        let (parent, originating) = (&sessions[0], &sessions[1]);
        assert!(parent.parent_session_id.is_none(), "first parent becomes the lineage root");
        assert_eq!(originating.parent_session_id.as_deref(), Some(parent.id.as_str()), "originating session is a depth-1 child of the new root");
    }

    #[test]
    fn branch_second_adds_sibling_child_under_same_root() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S0", "/tmp/p");
        state.apply_session_update("t1-claude", "S1", Some("resume"), None);
        let root_id = project_sessions(&state, "p")[0].id.clone();
        state.apply_session_update("t1-claude", "S2", Some("resume"), None);

        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 3);
        let (root, second, originating) = (&sessions[0], &sessions[1], &sessions[2]);
        assert_eq!(root.id, root_id, "root never changes once established");
        assert_eq!(root.claude_session_id.as_deref(), Some("S0"), "root pins the very first pre-/branch session");
        assert!(root.parent_session_id.is_none(), "root stays at depth 0");
        assert_eq!(originating.id, "t1");
        assert_eq!(originating.claude_session_id.as_deref(), Some("S2"), "originating carries the freshest id");
        assert_eq!(originating.parent_session_id.as_deref(), Some(root_id.as_str()), "originating keeps pointing at the original root");
        assert_eq!(second.claude_session_id.as_deref(), Some("S1"), "second parent pins the id current right before the second /branch");
        assert_eq!(second.parent_session_id.as_deref(), Some(root_id.as_str()), "second parent is a sibling under the same root");
    }

    #[test]
    fn branch_third_keeps_adding_siblings_under_same_root() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S0", "/tmp/p");
        for (i, new_session) in ["S1", "S2", "S3"].iter().enumerate() {
            state.apply_session_update("t1-claude", new_session, Some("resume"), None);
            assert_eq!(project_sessions(&state, "p").len(), i + 2, "each /branch adds one parent");
        }
        let sessions = project_sessions(&state, "p");
        let root = &sessions[0];
        assert!(root.parent_session_id.is_none());
        assert_eq!(root.claude_session_id.as_deref(), Some("S0"));
        for session in sessions.iter().skip(1) {
            assert_eq!(session.parent_session_id.as_deref(), Some(root.id.as_str()), "every non-root session points at the original root");
        }
        assert_eq!(sessions.last().unwrap().id, "t1", "originating session stays at the bottom in display order");
        assert_eq!(sessions.last().unwrap().claude_session_id.as_deref(), Some("S3"));
    }

    #[test]
    fn branch_closing_parent_clears_child_parent_session_id() {
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);
        let parent = project_sessions(&state, "p")[0].clone();
        assert_eq!(project_sessions(&state, "p")[1].parent_session_id.as_deref(), Some(parent.id.as_str()), "precondition: child points at parent");

        // Dissolve the parent by exiting all its windows (model-level cascade).
        for term_window_id in parent.windows.iter().map(|w| w.id.clone()) {
            let (model, selection) = (&mut state.workspace, &mut state.selection);
            state.ptys.window_exited(model, selection, &parent.id, &term_window_id);
        }
        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 1, "parent is gone after its windows all exited");
        assert_eq!(sessions[0].id, "t1");
        assert!(sessions[0].parent_session_id.is_none(), "child's parent_session_id is cleared when parent dissolves");
    }

    #[test]
    fn branch_closing_child_does_not_mutate_parent() {
        // The dangling-pointer sweep only mutates sessions that pointed at the removed
        // id; closing a child (which nothing points at) leaves the parent's
        // parent_session_id (None) exactly as it was.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);
        let parent = project_sessions(&state, "p")[0].clone();
        let child = project_sessions(&state, "p")[1].clone();
        assert!(parent.parent_session_id.is_none(), "precondition: parent at root");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent.id.as_str()), "precondition: child under parent");

        for term_window_id in child.windows.iter().map(|w| w.id.clone()) {
            let (model, selection) = (&mut state.workspace, &mut state.selection);
            state.ptys.window_exited(model, selection, &child.id, &term_window_id);
        }
        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 1, "child is gone, parent remains");
        assert_eq!(sessions[0].id, parent.id);
        assert!(sessions[0].parent_session_id.is_none(), "parent's parent_session_id must NOT be cleared when an unrelated child closes");
    }

    #[test]
    fn branch_materialization_is_scoped_to_owning_window() {
        // A /branch-shaped signal addressed to B's window, dispatched into A, is a
        // no-op on both — A's session_id_owning returns None.
        let mut a = WindowState::new("/home/u");
        seed_rotation_session(&mut a.workspace, "pA", "tA", "A0", "/tmp/pA");
        let mut b = WindowState::new("/home/u");
        seed_rotation_session(&mut b.workspace, "pB", "tB", "B0", "/tmp/pB");

        a.apply_session_update("tB-claude", "B-LEAKED", Some("resume"), None);
        assert_eq!(project_sessions(&a, "pA").len(), 1, "A must not materialize a parent for a B-shaped window");
        assert_eq!(session_claude_id(&a, "tA").as_deref(), Some("A0"));
        assert_eq!(project_sessions(&b, "pB").len(), 1, "B untouched — A was the dispatch target");
        assert_eq!(session_claude_id(&b, "tB").as_deref(), Some("B0"));

        // B's own handler DOES materialize a parent (proves scoping wasn't a false negative).
        b.apply_session_update("tB-claude", "B1", Some("resume"), None);
        assert_eq!(project_sessions(&b, "pB").len(), 2, "B's own /branch materializes a parent in B");
        assert_eq!(project_sessions(&a, "pA").len(), 1, "B's /branch must not bleed a parent into A");
        assert_eq!(session_claude_id(&a, "tA").as_deref(), Some("A0"));
    }

    #[test]
    fn branch_on_root_preserves_depth1_by_reparenting_former_children() {
        // /branch on a lineage root: the new parent becomes the new root, the old
        // root slides to a depth-1 child, AND every former child of the old root is
        // re-parented to the new root (otherwise they'd become depth-2).
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "S0", "/tmp/p");
        state.apply_session_update("t1-claude", "S1", Some("resume"), None);
        let old_root = project_sessions(&state, "p")[0].clone();
        assert!(old_root.parent_session_id.is_none(), "precondition: old root is the root");
        state.apply_session_update("t1-claude", "S2", Some("resume"), None);

        // /branch on the OLD ROOT. Its claude window id and current session (S0).
        let old_root_claude = old_root.windows.iter().find(|w| w.kind == TermWindowKind::Claude).unwrap().id.clone();
        state.apply_session_update(&old_root_claude, "S0-PRIME", Some("resume"), None);

        let sessions = project_sessions(&state, "p");
        let roots: Vec<&Session> = sessions.iter().filter(|s| s.parent_session_id.is_none()).collect();
        assert_eq!(roots.len(), 1, "exactly one root remains in the lineage");
        let new_root = roots[0];
        assert_ne!(new_root.id, old_root.id, "old root is no longer at depth 0");
        for session in sessions.iter().filter(|s| s.id != new_root.id) {
            assert_eq!(session.parent_session_id.as_deref(), Some(new_root.id.as_str()), "every non-root session is re-parented to the new root");
        }
        assert_eq!(sessions.iter().find(|s| s.id == "t1").unwrap().claude_session_id.as_deref(), Some("S2"), "t1 untouched by the /branch on the root");
        assert_eq!(new_root.claude_session_id.as_deref(), Some("S0"), "new root pins the id current on old root right before its /branch");
        assert_eq!(sessions.iter().find(|s| s.id == old_root.id).unwrap().claude_session_id.as_deref(), Some("S0-PRIME"), "old root now holds its post-rotation id");
    }

    #[test]
    fn branch_on_nil_claude_session_id_is_no_op() {
        // A claude session whose session id is None (claude not yet started): the
        // id-change guard requires a non-None old id, so the id is set in place but
        // no parent spawns.
        let mut state = WindowState::new("/home/u");
        state.workspace.ensure_project("p-nil", "P-NIL", "/tmp/p-nil");
        let mut session = Session::new("t-nil", "Pre-claude", "/tmp/p-nil");
        session.windows = vec![
            TermWindow::new("t-nil-claude", "Claude", TermWindowKind::Claude),
            TermWindow::new("t-nil-t1", "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some("t-nil-claude".into());
        session.claude_session_id = None;
        let pi = state.workspace.projects.iter().position(|p| p.id == "p-nil").unwrap();
        state.workspace.projects[pi].sessions.push(session);

        state.apply_session_update("t-nil-claude", "FIRST", Some("resume"), None);
        let sessions = project_sessions(&state, "p-nil");
        assert_eq!(sessions.len(), 1, "no parent when the originating session had no prior session id");
        assert_eq!(sessions[0].claude_session_id.as_deref(), Some("FIRST"), "id still set in place");
        assert!(sessions[0].parent_session_id.is_none());
    }

    #[test]
    fn branch_signal_on_terminals_session_is_no_op() {
        // The pinned Terminals group never hosts Claude; a resume+rotation addressed
        // to a Terminals window must not materialize a parent (insert_branch_parent
        // refuses the Terminals project).
        let mut state = WindowState::new("/home/u");
        let terminals = WorkspaceModel::TERMINALS_PROJECT_ID;
        let before = project_sessions(&state, terminals).len();
        let main = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
        let main_window = state.workspace.session_for(main).unwrap().windows[0].id.clone();
        // Give the Main session a session id so the id-change guard would otherwise fire.
        state.workspace.mutate_session(main, |s| s.claude_session_id = Some("OLD".into()));

        state.apply_session_update(&main_window, "FRESH", Some("resume"), None);
        assert_eq!(project_sessions(&state, terminals).len(), before, "Terminals session count must not change on a spurious branch signal");
    }

    #[test]
    fn branch_parent_window_is_not_running_ignores_shell_osc_title() {
        // The materialized parent's claude window is is_claude_running == false
        // (deferred resume). Its pty hosts a plain zsh whose theme OSC titles must
        // NOT clobber the parent's inherited title — the OSC gate drops the whole
        // Claude branch until the socket in-place promotion opens it.
        let mut state = WindowState::new("/home/u");
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.workspace.mutate_session("t1", |s| s.title = "wire up the foo".into());
        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);

        let parent = project_sessions(&state, "p")[0].clone();
        let parent_claude = parent.windows.iter().find(|w| w.kind == TermWindowKind::Claude).unwrap().clone();
        assert!(!parent_claude.is_claude_running, "sanity: branch parent's claude window is deferred");

        let model = &mut state.workspace;
        state.ptys.window_title_changed(model, &parent.id, &parent_claude.id, "Nick@Nicks MacBook Air:~/Projects/nice");
        assert_eq!(
            project_sessions(&state, "p")[0].title, "wire up the foo",
            "branch parent's inherited title must survive its deferred-resume zsh's OSC titles"
        );
    }

    // === Fork classification (Claude Code ≥ 2.1.212 / 2.1.214) =================
    //
    // Two different events now arrive as `source: "fork"`:
    //   * an IN-WINDOW rotation (`/branch`, `--fork-session` resume) — what used to
    //     report `"resume"`; it must behave exactly like the legacy `/branch`;
    //   * the Claude daemon's DETACHED background `/fork` child — whose relayed
    //     window id belongs to whichever window first spawned the daemon, so acting
    //     on it rewrote an unrelated session's claude session id (bug 3).
    // The `~/.claude/jobs/<first8>/` entry is the discriminator. Every case here
    // injects the probe, so none of them read the developer's real `~/.claude`.

    /// A throwaway `jobs`-shaped directory removed on drop — the fixture for the
    /// REAL probe ([`probe_fork_job_in`]); never the developer's `~/.claude`.
    struct ScratchJobs(std::path::PathBuf);
    impl Drop for ScratchJobs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch_jobs() -> ScratchJobs {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fork-jobs-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch jobs dir");
        ScratchJobs(dir)
    }

    /// A full uuid whose first 8 chars are `first8` — the shape the daemon keys
    /// its jobs directory by.
    fn fork_uuid(first8: &str) -> String {
        format!("{first8}-1111-2222-3333-444455556666")
    }

    #[test]
    fn fork_source_without_jobs_entry_rotates_and_creates_parent() {
        // `/branch` on Claude ≥ 2.1.214 relays `"fork"`, not `"resume"`. With no
        // jobs entry it is an in-window rotation: the session adopts the new id AND the
        // pre-branch conversation is materialized as a sibling parent (bug 2).
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        state.workspace.mutate_session("t1", |s| s.title = "wire up the foo".into());

        let out = state.apply_session_update("t1-claude", "NEW", Some("fork"), None);
        assert!(out.did_mutate);
        assert!(out.background_fork.is_none(), "no jobs entry ⇒ not a background fork");
        assert!(out.spawn.is_some(), "an in-window fork owes a deferred-resume parent spawn");

        let sessions = project_sessions(&state, "p");
        assert_eq!(sessions.len(), 2, "in-window fork adds exactly one sibling parent session");
        let (parent, child) = (&sessions[0], &sessions[1]);
        assert_eq!(child.id, "t1");
        assert_eq!(child.claude_session_id.as_deref(), Some("NEW"), "originating session adopts the post-rotation id");
        assert_eq!(parent.claude_session_id.as_deref(), Some("OLD"), "parent pinned to the pre-rotation id");
        assert_eq!(parent.title, "wire up the foo", "parent inherits the title");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn fork_source_with_jobs_entry_leaves_the_addressed_session_untouched() {
        // BUG-3 REGRESSION PIN. The daemon relays the background fork's
        // SessionStart with the stale window id it inherited. Nothing about the
        // addressed session may change — not its session id, not its cwd, not the
        // tree — and the fork's identity is handed off for materialization.
        let fork_id = fork_uuid("298689bf");
        let mut state = WindowState::new("/home/u");
        let probe_id = fork_id.clone();
        state.set_fork_job_probe_for_test(move |id| {
            assert_eq!(id, probe_id, "the probe is asked about the INCOMING id");
            Some(ForkJobInfo {
                claude_session_id: Some(probe_id.clone()),
                fork_parent_session_id: Some("2f3b14e8-parent".into()),
                name: Some("⑂ tmux keybinds".into()),
            })
        });
        seed_rotation_session(&mut state.workspace, "p", "t1", "LIVE-ID", "/tmp/p");

        let out = state.apply_session_update("t1-claude", &fork_id, Some("fork"), Some("/tmp/forked"));

        assert_eq!(
            session_claude_id(&state, "t1").as_deref(),
            Some("LIVE-ID"),
            "a daemon-hosted fork must NEVER rewrite the relayed window's session id"
        );
        assert_eq!(
            state.workspace.session_for("t1").map(|s| s.cwd.clone()).as_deref(),
            Some("/tmp/p"),
            "nor adopt the fork's cwd onto that session"
        );
        assert_eq!(project_sessions(&state, "p").len(), 1, "and no parent session is spawned");
        assert!(!out.did_mutate, "nothing was mutated");
        assert!(out.spawn.is_none());

        let fork = out.background_fork.expect("a jobs-backed fork must hand off for materialization");
        assert_eq!(fork.fork_claude_session_id, fork_id);
        assert_eq!(fork.cwd.as_deref(), Some("/tmp/forked"));
        assert_eq!(fork.job.fork_parent_session_id.as_deref(), Some("2f3b14e8-parent"));
        assert_eq!(fork.job.name.as_deref(), Some("⑂ tmux keybinds"));
    }

    #[test]
    fn fork_with_jobs_entry_hands_off_even_when_the_window_is_unknown() {
        // The daemon's window id can be not just stale but GONE (the window that
        // spawned it closed). The fork still exists and still deserves a sidebar
        // entry, so the classification runs before the window is resolved.
        let fork_id = fork_uuid("b8c8244b");
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| Some(ForkJobInfo::default()));
        seed_rotation_session(&mut state.workspace, "p", "t1", "LIVE-ID", "/tmp/p");

        let out = state.apply_session_update("pane-that-no-longer-exists", &fork_id, Some("fork"), None);
        let fork = out.background_fork.expect("an unknown window must not swallow the fork");
        assert_eq!(fork.fork_claude_session_id, fork_id);
        assert_eq!(fork.cwd, None, "absent cwd stays absent");
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("LIVE-ID"));
    }

    #[test]
    fn fork_with_jobs_entry_normalizes_an_empty_cwd_to_none() {
        // The hook always sends a `cwd` key; a missing value arrives as "". The
        // hand-off carries None so the next slice falls back to the parent's cwd
        // rather than resolving a fork session at the filesystem root.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| Some(ForkJobInfo::default()));
        seed_rotation_session(&mut state.workspace, "p", "t1", "LIVE-ID", "/tmp/p");
        let out = state.apply_session_update("t1-claude", &fork_uuid("aaaaaaaa"), Some("fork"), Some(""));
        assert_eq!(out.background_fork.expect("fork hand-off").cwd, None);
    }

    #[test]
    fn resume_source_without_a_jobs_entry_still_materializes_a_parent() {
        // Legacy CLIs (< 2.1.214) report `"resume"` for `/branch`. The probe now
        // runs on this source too, but an id that names no job leaves the legacy
        // path byte for byte as it was: rotate, and materialize the parent.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        });
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");

        state.apply_session_update("t1-claude", "NEW", Some("resume"), None);

        assert_eq!(calls.load(Ordering::SeqCst), 1, "every source is screened against the jobs dir");
        assert_eq!(project_sessions(&state, "p").len(), 2, "legacy /branch still materializes a parent");
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("NEW"));
    }

    #[test]
    fn resume_source_with_a_jobs_entry_leaves_the_addressed_session_untouched() {
        // BUG-3 REGRESSION PIN, the `claude attach` route. Waking a COLD
        // background job fires SessionStart with `source: "resume"` — from the
        // daemon, carrying the stale window id it inherited, and naming a session
        // that belongs to some other session entirely. Observed live: it rewrote the
        // addressed session's id and invented a branch parent for the conversation it
        // had just displaced. Nothing about that session may change, and no fork is
        // materialized either (the job's session was created at its birth).
        let woken = fork_uuid("578ad088");
        let mut state = WindowState::new("/home/u");
        let probe_id = woken.clone();
        state.set_fork_job_probe_for_test(move |id| {
            (id == probe_id).then(|| ForkJobInfo {
                claude_session_id: Some(probe_id.clone()),
                fork_parent_session_id: Some("2f3b14e8-parent".into()),
                name: Some("⑂ woken job".into()),
            })
        });
        seed_rotation_session(&mut state.workspace, "p", "t1", "798e31f1-live", "/tmp/p");

        let out = state.apply_session_update("t1-claude", &woken, Some("resume"), Some("/tmp/forked"));

        assert!(!out.did_mutate, "a woken background job mutates nothing");
        assert!(out.spawn.is_none(), "and invents no branch parent");
        assert!(out.background_fork.is_none(), "the job's session already exists — do not duplicate it");
        assert_eq!(
            session_claude_id(&state, "t1").as_deref(),
            Some("798e31f1-live"),
            "the addressed session keeps its own conversation"
        );
        assert_eq!(
            state.workspace.session_for("t1").map(|s| s.cwd.clone()).as_deref(),
            Some("/tmp/p"),
            "nor adopts the job's cwd"
        );
        assert_eq!(project_sessions(&state, "p").len(), 1);
    }

    #[test]
    fn startup_source_with_a_jobs_entry_leaves_the_addressed_session_untouched() {
        // Same distrust for the daemon's other relay shapes: whatever source a
        // daemon-run session reports, its window id is not ours to act on.
        let job = fork_uuid("578ad088");
        let mut state = WindowState::new("/home/u");
        let probe_id = job.clone();
        state.set_fork_job_probe_for_test(move |id| {
            (id == probe_id).then(|| ForkJobInfo {
                claude_session_id: Some(probe_id.clone()),
                ..Default::default()
            })
        });
        seed_rotation_session(&mut state.workspace, "p", "t1", "LIVE-ID", "/tmp/p");

        let out = state.apply_session_update("t1-claude", &job, Some("startup"), None);

        assert!(!out.did_mutate);
        assert!(out.background_fork.is_none());
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("LIVE-ID"));
    }

    #[test]
    fn resume_source_with_a_first8_collision_still_rotates() {
        // The screen is keyed by 8 hex characters, so a FOREIGN job can answer the
        // probe. `state.json`'s sessionId is the tiebreak, exactly as it is at
        // exec time: it names someone else's conversation, so this relay is an
        // ordinary in-window rotation and must not be silenced.
        let mine = fork_uuid("578ad088");
        let theirs = "578ad088-9999-9999-9999-999999999999";
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(move |_| Some(ForkJobInfo {
            claude_session_id: Some(theirs.into()),
            ..Default::default()
        }));
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");

        let out = state.apply_session_update("t1-claude", &mine, Some("resume"), None);

        assert!(out.did_mutate);
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some(mine.as_str()));
        assert_eq!(project_sessions(&state, "p").len(), 2, "a foreign job must not swallow a real /branch");
    }

    #[test]
    fn fork_with_same_id_does_not_create_parent() {
        // A `fork`-sourced relay that carries the id the session already has (a
        // redundant forward) is absorbed by the equality short-circuit, exactly
        // as the `resume` shape is — no phantom parent.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        seed_rotation_session(&mut state.workspace, "p", "t1", "SAME", "/tmp/p");
        let out = state.apply_session_update("t1-claude", "SAME", Some("fork"), None);
        assert!(!out.did_mutate);
        assert_eq!(project_sessions(&state, "p").len(), 1, "no rotation ⇒ no parent session");
    }

    #[test]
    fn clear_with_a_stale_window_id_still_applies_to_that_session() {
        // ACCEPTED EXPOSURE, documented rather than fixed. The jobs screen keys
        // off the INCOMING id, and `/clear` inside a daemon-run child rotates to a
        // fresh id that keys no jobs directory (the entry stays under the job's
        // ORIGINAL first-8), so such a relay slips through and still rewrites the
        // addressed session. Pre-existing, not reachable through `/fork` (which never
        // fires `clear` in the fork child), and the only alternative — distrusting
        // an id no probe can place — would break the ordinary in-window `/clear`
        // this line is here to preserve.
        let mut state = WindowState::new("/home/u");
        state.set_fork_job_probe_for_test(|_| None);
        seed_rotation_session(&mut state.workspace, "p", "t1", "OLD", "/tmp/p");
        let out = state.apply_session_update("t1-claude", "CLEARED", Some("clear"), None);
        assert!(out.did_mutate);
        assert!(out.background_fork.is_none(), "`clear` is never classified as a fork");
        assert_eq!(session_claude_id(&state, "t1").as_deref(), Some("CLEARED"));
        assert_eq!(project_sessions(&state, "p").len(), 1, "/clear still spawns no parent");
    }

    // === Fork materialization (Fix B) =========================================
    //
    // A classified background fork becomes its own sidebar entry: a nested,
    // UNSELECTED child of the session whose conversation was forked, pinned to the
    // fork's id and carrying a deferred `claude --resume <fork id>` prefill.
    //
    // These drive the SHIPPED entry point (`route_socket_message` with a
    // `SessionUpdate`) so the classification, the deferred retry task, the parent
    // resolution, and the insert are all exercised together. They need a gpui
    // context (the materialization spawns a task and a deferred-resume window),
    // hence `#[gpui::test]`.
    //
    // Containment, as in the handoff/dispatch tests above: the resolved-`claude`
    // global is pinned to `None` and every cwd is a path that does not exist, so a
    // forked child `_exit`s at its `chdir` and no login shell is ever sourced;
    // each test tears its sessions down at the end.

    /// A cwd no spawn can chdir into (so the ResumeDeferred shell dies instantly).
    const NO_SPAWN_CWD: &str = "/nice-unit-test-no-such-dir";

    /// A `WindowState` entity with `p`/`t-parent` seeded and selected, standing in
    /// for the session the user ran `/fork` in. `probe` is the injected jobs-dir seam.
    fn fork_window(
        cx: &mut gpui::TestAppContext,
        cwd: &str,
        probe: impl Fn(&str) -> Option<ForkJobInfo> + 'static,
    ) -> Entity<WindowState> {
        cx.update(|app| app.set_global(crate::pty_manager::ResolvedClaudePath(None)));
        let state = cx.new(|_cx| WindowState::new("/home/u"));
        state.update(cx, |ws, _cx| {
            // The fork's deferred window spawns a real (immediately dying) pty from
            // inside the materialization task; its exit-watcher thread would wake
            // the parked drain across threads and trip gpui's determinism guard.
            ws.ptys.set_event_wakes_enabled_for_test(false);
            ws.set_fork_job_probe_for_test(probe);
            seed_rotation_session(&mut ws.workspace, "p", "t-parent", "PARENT-SID", cwd);
            ws.workspace
                .mutate_session("t-parent", |s| s.title = "wire up the foo".into());
            ws.workspace.select_session("t-parent");
            ws.selection.sync_active_session_id(ws.workspace.active_session_id());
        });
        state
    }

    /// Relay a background fork's SessionStart through the SHIPPED router. The window
    /// id is deliberately one no session owns — the daemon inherits whichever window
    /// first spawned it, and the fork path must never lean on it.
    fn relay_fork(
        cx: &mut gpui::TestAppContext,
        state: &Entity<WindowState>,
        fork_id: &str,
        cwd: Option<&str>,
    ) {
        state.update(cx, |ws, cx| {
            ws.route_socket_message(
                SocketMessage::SessionUpdate {
                    term_window_id: "daemon-inherited-pane".into(),
                    claude_session_id: fork_id.to_string(),
                    source: Some("fork".into()),
                    cwd: cwd.map(str::to_string),
                },
                cx,
            )
        });
    }

    /// The session in project `p` other than the seeded `t-parent`, if the fork landed.
    fn fork_child(state: &WindowState) -> Option<Session> {
        state
            .workspace
            .projects
            .iter()
            .find(|p| p.id == "p")?
            .sessions
            .iter()
            .find(|s| s.id != "t-parent")
            .cloned()
    }

    #[gpui::test]
    fn background_fork_nests_a_pinned_unselected_child_under_the_forked_session(
        cx: &mut gpui::TestAppContext,
    ) {
        // The headline of Fix B (bug 1): a `/fork` finally shows up in the sidebar.
        let fork_id = fork_uuid("b8c8244b");
        let fork_cwd = format!("{NO_SPAWN_CWD}/fork-worktree");
        let state = fork_window(cx, NO_SPAWN_CWD, |_| {
            Some(ForkJobInfo {
                claude_session_id: None,
                fork_parent_session_id: Some("PARENT-SID".into()),
                name: Some("⑂ wire up the foo".into()),
            })
        });

        relay_fork(cx, &state, &fork_id, Some(&fork_cwd));
        cx.run_until_parked();

        state.update(cx, |ws, _cx| {
            let child = fork_child(ws).expect("the fork materialized a session");
            assert_eq!(
                child.parent_session_id.as_deref(),
                Some("t-parent"),
                "the fork nests one indent under the session it was forked from"
            );
            assert_eq!(
                child.claude_session_id.as_deref(),
                Some(fork_id.as_str()),
                "the child is pinned to the FORK's id, so its deferred resume opens that conversation"
            );
            assert_eq!(
                child.cwd, fork_cwd,
                "a fork that relocated into its own worktree keeps that cwd (≥ 2.1.220)"
            );
            assert_eq!(
                child.title, "⑂ wire up the foo",
                "the job's name (carrying the ⑂ marker) titles the session"
            );
            assert!(
                child.title_manually_set,
                "and is locked, so resuming the fork cannot OSC the ⑂ marker away"
            );
            let claude = child
                .windows
                .iter()
                .find(|w| w.kind == TermWindowKind::Claude)
                .expect("the fork session has a Claude window");
            assert!(
                !claude.is_claude_running,
                "the fork's window is a DEFERRED resume — nothing runs until the user opens it"
            );

            // The foreground conversation is untouched: same id, same selection.
            assert_eq!(
                session_claude_id(ws, "t-parent").as_deref(),
                Some("PARENT-SID"),
                "the forked-from session keeps its own session id"
            );
            assert_eq!(
                ws.workspace.active_session_id(),
                Some("t-parent"),
                "materializing a fork must not steal the active session"
            );
            assert!(
                !ws.selection.contains(&child.id),
                "the fork session opens UNSELECTED (background offshoot, not a context switch)"
            );
        });
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_falls_back_to_the_parents_cwd_and_title(cx: &mut gpui::TestAppContext) {
        // An empty relayed cwd and a `state.json` without a `name` (the shapes an
        // older / partially-written job produces) must still yield a usable session.
        let fork_id = fork_uuid("aaaaaaaa");
        let state = fork_window(cx, NO_SPAWN_CWD, |_| {
            Some(ForkJobInfo {
                claude_session_id: None,
                fork_parent_session_id: Some("PARENT-SID".into()),
                name: None,
            })
        });

        relay_fork(cx, &state, &fork_id, Some(""));
        cx.run_until_parked();

        state.update(cx, |ws, _cx| {
            let child = fork_child(ws).expect("the fork materialized a session");
            assert_eq!(child.cwd, NO_SPAWN_CWD, "empty relayed cwd ⇒ the parent's cwd");
            assert_eq!(
                child.title, "wire up the foo",
                "no job name ⇒ the parent's title"
            );
            assert!(
                !child.title_manually_set,
                "an inherited title keeps the parent's title flags (nothing to lock)"
            );
        });
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_with_no_matching_parent_session_is_a_silent_no_op(
        cx: &mut gpui::TestAppContext,
    ) {
        // The forked conversation was never open in Nice (or its session has since
        // closed). There is nothing to nest under, and guessing would be worse
        // than nothing — so the fork is dropped without a trace.
        let state = fork_window(cx, NO_SPAWN_CWD, |_| {
            Some(ForkJobInfo {
                claude_session_id: None,
                fork_parent_session_id: Some("A-CONVERSATION-NICE-NEVER-SAW".into()),
                name: Some("⑂ elsewhere".into()),
            })
        });

        relay_fork(cx, &state, &fork_uuid("deadbeef"), None);
        cx.run_until_parked();

        state.update(cx, |ws, _cx| {
            assert!(fork_child(ws).is_none(), "no parent ⇒ no session");
            assert_eq!(project_sessions(ws, "p").len(), 1);
        });
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_relayed_twice_materializes_exactly_one_session(cx: &mut gpui::TestAppContext) {
        // A job can announce itself more than once (the daemon respawning a woken
        // one carries its `respawnFlags`). The fork's sidebar entry already exists,
        // so the second relay must add nothing: two sessions claiming one conversation
        // is exactly the corruption this feature set out to end.
        let fork_id = fork_uuid("b8c8244b");
        let state = fork_window(cx, NO_SPAWN_CWD, |_| {
            Some(ForkJobInfo {
                claude_session_id: None,
                fork_parent_session_id: Some("PARENT-SID".into()),
                name: Some("⑂ wire up the foo".into()),
            })
        });

        relay_fork(cx, &state, &fork_id, None);
        cx.run_until_parked();
        relay_fork(cx, &state, &fork_id, None);
        cx.run_until_parked();

        state.update(cx, |ws, _cx| {
            assert_eq!(
                project_sessions(ws, "p").len(),
                2,
                "the forked-from session plus ONE fork session"
            );
            assert_eq!(
                fork_child(ws).and_then(|s| s.claude_session_id).as_deref(),
                Some(fork_id.as_str())
            );
        });
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_waits_for_a_late_state_json(cx: &mut gpui::TestAppContext) {
        // The daemon creates `jobs/<first8>/` BEFORE spawning the fork child, so
        // the hook can beat `state.json` to disk. The first probe classifies the
        // event (directory present) but names no parent; a later re-probe does.
        use std::cell::Cell;
        use std::rc::Rc;
        let landed = Rc::new(Cell::new(false));
        let seen = landed.clone();
        let state = fork_window(cx, NO_SPAWN_CWD, move |_| {
            Some(ForkJobInfo {
                claude_session_id: None,
                fork_parent_session_id: seen
                    .get()
                    .then(|| "PARENT-SID".to_string()),
                name: None,
            })
        });

        relay_fork(cx, &state, &fork_uuid("b8c8244b"), None);
        cx.run_until_parked();
        state.update(cx, |ws, _cx| {
            assert!(
                fork_child(ws).is_none(),
                "nothing can be materialized while the parent id is unknown"
            );
        });

        // `state.json` lands; the next poll picks it up.
        landed.set(true);
        cx.executor().advance_clock(FORK_STATE_POLL_INTERVAL * 2);
        cx.run_until_parked();

        state.update(cx, |ws, _cx| {
            let child = fork_child(ws).expect("the retry materialized the fork");
            assert_eq!(child.parent_session_id.as_deref(), Some("t-parent"));
        });
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_gives_up_silently_when_state_json_never_lands(
        cx: &mut gpui::TestAppContext,
    ) {
        // An ABORTED fork: the daemon wrote the directory (and `tmp/`) and then
        // died, so `forkParentSessionId` never appears. The live `298689bf` job on
        // this machine is exactly that. The retry must expire and leave no session.
        let state = fork_window(cx, NO_SPAWN_CWD, |_| Some(ForkJobInfo::default()));

        relay_fork(cx, &state, &fork_uuid("298689bf"), None);
        // Well past FORK_STATE_POLL_ATTEMPTS × FORK_STATE_POLL_INTERVAL.
        cx.executor()
            .advance_clock(FORK_STATE_POLL_INTERVAL * (FORK_STATE_POLL_ATTEMPTS as u32 + 4));
        cx.run_until_parked();

        state.update(cx, |ws, _cx| {
            assert!(fork_child(ws).is_none(), "an aborted fork leaves no session behind");
            assert_eq!(project_sessions(ws, "p").len(), 1);
        });
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_gives_up_when_the_jobs_entry_disappears(cx: &mut gpui::TestAppContext) {
        // The daemon cleaned the aborted job up between the classification and a
        // retry. No entry, no fork — stop polling immediately rather than burning
        // the full retry budget.
        use std::cell::Cell;
        use std::rc::Rc;
        let gone = Rc::new(Cell::new(false));
        let swept = gone.clone();
        let state = fork_window(cx, NO_SPAWN_CWD, move |_| {
            (!swept.get()).then(ForkJobInfo::default)
        });

        relay_fork(cx, &state, &fork_uuid("298689bf"), None);
        cx.run_until_parked();
        gone.set(true);
        cx.executor().advance_clock(FORK_STATE_POLL_INTERVAL * 2);
        cx.run_until_parked();

        state.update(cx, |ws, _cx| assert!(fork_child(ws).is_none()));
        state.update(cx, |ws, _cx| ws.teardown());
    }

    #[gpui::test]
    fn background_fork_finds_its_parent_session_in_another_window(cx: &mut gpui::TestAppContext) {
        // THE cross-window case, and the reason the fork path can't stop at the
        // window `session_update` was routed to: the Claude daemon inherits
        // `NICE_PANE_ID` from whichever window first spawned it, which may belong to
        // a completely different window than the one the user forked in.
        use crate::window_registry::WindowRegistry;
        let fork_id = fork_uuid("b8c8244b");

        // Window A receives the relay but holds no session for the forked conversation.
        cx.update(|app| app.set_global(crate::pty_manager::ResolvedClaudePath(None)));
        let window_a = cx.new(|_cx| WindowState::new("/home/u"));
        window_a.update(cx, |ws, _cx| {
            ws.ptys.set_event_wakes_enabled_for_test(false);
            ws.set_fork_job_probe_for_test(|_| {
                Some(ForkJobInfo {
                    claude_session_id: None,
                    fork_parent_session_id: Some("PARENT-SID".into()),
                    name: None,
                })
            });
            seed_rotation_session(&mut ws.workspace, "other", "t-other", "UNRELATED", NO_SPAWN_CWD);
        });
        // Window B is where the user actually ran `/fork`.
        let window_b = fork_window(cx, NO_SPAWN_CWD, |_| None);

        let (id_a, id_b) = (
            cx.add_window(|_w, _cx| gpui::Empty).window_id(),
            cx.add_window(|_w, _cx| gpui::Empty).window_id(),
        );
        cx.update(|app| {
            app.set_global(WindowRegistry::default());
            WindowRegistry::register(app, id_a, window_a.clone());
            WindowRegistry::register(app, id_b, window_b.clone());
        });

        relay_fork(cx, &window_a, &fork_id, None);
        cx.run_until_parked();

        window_a.update(cx, |ws, _cx| {
            assert_eq!(
                ws.workspace
                    .projects
                    .iter()
                    .flat_map(|p| p.sessions.iter())
                    .count(),
                // The Main Terminal session + the seeded unrelated session, nothing new.
                project_sessions(ws, "other").len() + 1,
                "the receiving window must not grow a session it has no parent for"
            );
        });
        window_b.update(cx, |ws, _cx| {
            let child = fork_child(ws).expect("the fork landed in the window holding its parent");
            assert_eq!(child.parent_session_id.as_deref(), Some("t-parent"));
            assert_eq!(child.claude_session_id.as_deref(), Some(fork_id.as_str()));
        });

        window_a.update(cx, |ws, _cx| ws.teardown());
        window_b.update(cx, |ws, _cx| ws.teardown());
    }

    // -- the real (filesystem) half of the probe seam ---------------------------

    #[test]
    fn probe_returns_none_when_no_jobs_entry_exists() {
        let jobs = scratch_jobs();
        assert_eq!(probe_fork_job_in(&jobs.0, &fork_uuid("deadbeef")), None);
    }

    #[test]
    fn probe_returns_none_for_an_id_shorter_than_its_first8() {
        // A malformed / truncated id can't key a jobs directory; refusing it here
        // keeps `jobs_dir.join(..)` from ever being handed a partial prefix that
        // could match some other job.
        let jobs = scratch_jobs();
        assert_eq!(probe_fork_job_in(&jobs.0, "abc"), None);
        assert_eq!(probe_fork_job_in(&jobs.0, ""), None);
    }

    #[test]
    fn probe_returns_empty_info_when_state_json_has_not_landed() {
        // The daemon creates the directory (and `tmp/`) BEFORE spawning the fork
        // child, so the hook can fire while `state.json` is still missing — an
        // aborted fork never writes one at all. The directory alone still
        // classifies the event as a background fork.
        let jobs = scratch_jobs();
        std::fs::create_dir_all(jobs.0.join("298689bf").join("tmp")).unwrap();
        assert_eq!(
            probe_fork_job_in(&jobs.0, &fork_uuid("298689bf")),
            Some(ForkJobInfo::default())
        );
    }

    #[test]
    fn probe_reads_session_parent_and_name_from_state_json() {
        let jobs = scratch_jobs();
        let id = fork_uuid("b8c8244b");
        std::fs::create_dir_all(jobs.0.join("b8c8244b")).unwrap();
        std::fs::write(
            jobs.0.join("b8c8244b").join("state.json"),
            serde_json::to_vec(&serde_json::json!({
                "sessionId": id,
                "forkParentSessionId": "2f3b14e8-0000-0000-0000-000000000000",
                "forkBoundaryAt": 42,
                "name": "⑂ fix the thing",
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            probe_fork_job_in(&jobs.0, &id),
            Some(ForkJobInfo {
                claude_session_id: Some(id.clone()),
                fork_parent_session_id: Some("2f3b14e8-0000-0000-0000-000000000000".into()),
                name: Some("⑂ fix the thing".into()),
            })
        );
    }

    #[test]
    fn probe_tolerates_a_malformed_state_json() {
        // A half-written / hand-mangled state.json must not make the entry vanish:
        // the directory is what classifies, and losing the classification would
        // resurrect the bug-3 rotation.
        let jobs = scratch_jobs();
        std::fs::create_dir_all(jobs.0.join("cafebabe")).unwrap();
        std::fs::write(jobs.0.join("cafebabe").join("state.json"), b"{ not json").unwrap();
        assert_eq!(
            probe_fork_job_in(&jobs.0, &fork_uuid("cafebabe")),
            Some(ForkJobInfo::default())
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
    use crate::pty_manager::{build_claude_exec_command, build_claude_prefill_command};

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
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);
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
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);
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
        let (claude, _t) = seed_claude_session(&mut state.workspace, "t1", "OLD", false);
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
    // pure cores: the per-window busy predicate and the multi-select split. The
    // busy→modal WIRING is covered end-to-end by the `close-confirmation` live
    // scenario; the terminal foreground-child seam by `pty_manager` unit tests.

    fn claude_window(id: &str, status: SessionStatus) -> TermWindow {
        let mut p = TermWindow::new(id, "auth-refactor", TermWindowKind::Claude);
        p.status = status;
        p
    }

    /// The pre-splits shape of the busy predicate — a never-split window's sole
    /// leaf — so the ported Swift parity cases below still read as window-level
    /// facts now that the core is per-pane.
    fn window_is_busy_with(window: &TermWindow, terminal_has_foreground_child: bool) -> bool {
        let pane = window
            .layout
            .single_leaf()
            .expect("these cases model never-split windows");
        WindowState::pane_is_busy_with(
            pane.kind,
            window.is_alive && pane.is_alive,
            window.status,
            terminal_has_foreground_child,
        )
    }

    #[test]
    fn busy_idle_claude_and_idle_shell_are_not_busy() {
        // The core parity assert (Swift `isBusy` `:268-279`): an idle Claude at
        // rest (the default pre-first-title state) is DISPOSABLE, and an idle shell
        // (no foreground child) is NOT busy — both close with no dialog.
        let idle_claude = claude_window("c", SessionStatus::Idle);
        assert!(
            !window_is_busy_with(&idle_claude, false),
            "an idle Claude is disposable, not busy"
        );
        let shell = TermWindow::new("t", "npm run dev", TermWindowKind::Terminal);
        assert!(
            !window_is_busy_with(&shell, false),
            "a shell with no foreground child is idle, not busy"
        );
    }

    #[test]
    fn busy_thinking_or_waiting_claude_is_busy() {
        for status in [SessionStatus::Thinking, SessionStatus::Waiting] {
            assert!(
                window_is_busy_with(&claude_window("c", status), false),
                "a {status:?} Claude is busy"
            );
        }
    }

    #[test]
    fn busy_terminal_follows_the_foreground_child_signal() {
        let shell = TermWindow::new("t", "cat", TermWindowKind::Terminal);
        assert!(
            window_is_busy_with(&shell, true),
            "a shell WITH a foreground child is busy (the terminal arm follows the syscall/seam)"
        );
        assert!(
            !window_is_busy_with(&shell, false),
            "the same shell WITHOUT a foreground child is not busy"
        );
    }

    #[test]
    fn busy_dead_window_is_never_busy_even_when_thinking() {
        // The dead-first guard (D-BUSY §1): a held/dead window is never busy, even a
        // Claude frozen mid-`Thinking` or a terminal reporting a foreground child.
        let mut dead_claude = claude_window("c", SessionStatus::Thinking);
        dead_claude.is_alive = false;
        assert!(!window_is_busy_with(&dead_claude, false));
        let mut dead_shell = TermWindow::new("t", "cat", TermWindowKind::Terminal);
        dead_shell.is_alive = false;
        assert!(
            !window_is_busy_with(&dead_shell, true),
            "a dead shell is not busy even if a stale foreground-child signal is passed"
        );
    }

    // MARK: - Phase 2: the gates go pane-aware

    fn signal(
        kind: TermWindowKind,
        alive: bool,
        status: SessionStatus,
        has_foreground_child: bool,
    ) -> PaneBusySignal {
        PaneBusySignal {
            kind,
            alive,
            status,
            has_foreground_child,
        }
    }

    #[test]
    fn busy_ors_across_leaves_so_a_build_beside_claude_blocks_the_close() {
        // The layout D1 exists for: Claude idle in one pane, a build running in
        // the shell pane next to it. No pill-level signal can see that build —
        // the pill's own kind is Claude, whose arm never reads a foreground
        // child — so without the OR the close would go through unconfirmed.
        let signals = [
            signal(TermWindowKind::Claude, true, SessionStatus::Idle, false),
            signal(TermWindowKind::Terminal, true, SessionStatus::Idle, true),
        ];
        assert!(WindowState::any_pane_busy(&signals));
    }

    #[test]
    fn busy_ignores_a_dead_pane_and_falls_quiet_when_every_pane_is() {
        // A held corpse pane keeps its last status; it must not hold the pill
        // open (the dead-first guard, now per leaf).
        let signals = [
            signal(TermWindowKind::Claude, false, SessionStatus::Thinking, false),
            signal(TermWindowKind::Terminal, true, SessionStatus::Idle, false),
        ];
        assert!(!WindowState::any_pane_busy(&signals));
    }

    #[test]
    fn compose_routes_on_the_focused_pane_not_the_pill() {
        // A Claude pill split with a shell. With the SHELL focused, ⌘↩ must
        // compose — that is the whole point of "a shell beside Claude" (D1) —
        // even though the pill's own kind is Claude.
        let mut pill = TermWindow::new("c", "Claude", TermWindowKind::Claude);
        assert!(pill.layout.split(
            "c",
            nice_model::SplitOrient::Beside,
            nice_model::Pane::new("shell", TermWindowKind::Terminal),
        ));
        pill.active_pane_id = "shell".into();

        let focused = pill.layout.pane(&pill.effective_pane_id()).unwrap();
        assert_eq!(
            WindowState::compose_route(
                focused.kind,
                pill.is_alive && focused.is_alive,
                false,
                false
            ),
            ComposeRoute::Trigger
        );

        // Focus back on the Claude leaf ⇒ exactly today's behavior.
        pill.active_pane_id = "c".into();
        let focused = pill.layout.pane(&pill.effective_pane_id()).unwrap();
        assert_eq!(
            WindowState::compose_route(
                focused.kind,
                pill.is_alive && focused.is_alive,
                false,
                false
            ),
            ComposeRoute::Noop
        );
    }

    #[test]
    fn compose_route_truth_table() {
        use ComposeRoute::*;
        // 1. The trigger fires ONLY for a live Terminal window with no foreground
        //    child — regardless of the kitty state (zsh at a prompt has none,
        //    but a stale bit must not divert the trigger).
        for kitty in [false, true] {
            assert_eq!(
                WindowState::compose_route(TermWindowKind::Terminal, true, false, kitty),
                Trigger,
                "idle live terminal (kitty={kitty}) triggers compose"
            );
        }
        // 2. A busy window replays ⌘↩ iff the child forwards super chords —
        //    exactly the pre-feature byte contract.
        assert_eq!(
            WindowState::compose_route(TermWindowKind::Terminal, true, true, true),
            ForwardCmdEnter,
            "busy kitty window (Claude Code, vim+kitty) keeps receiving cmd-enter"
        );
        assert_eq!(
            WindowState::compose_route(TermWindowKind::Terminal, true, true, false),
            Noop,
            "busy legacy-mode shell got no bytes for an unbound cmd-enter either"
        );
        // 3. Dead windows and Claude windows never trigger; they may still forward.
        assert_eq!(
            WindowState::compose_route(TermWindowKind::Terminal, false, false, false),
            Noop,
            "dead terminal: nothing"
        );
        assert_eq!(
            WindowState::compose_route(TermWindowKind::Claude, true, false, true),
            ForwardCmdEnter,
            "a Claude window (kitty on) receives cmd-enter, never the trigger"
        );
        assert_eq!(
            WindowState::compose_route(TermWindowKind::Claude, true, false, false),
            Noop,
            "a Claude window without kitty gets nothing"
        );
    }

    #[test]
    fn split_sessions_buckets_idle_and_busy_and_builds_summaries() {
        // §T.3: idle sessions (empty busy list) bucket into `idle_ids`; busy sessions into
        // `busy_ids` with a `<Title> (<p1>, <p2>)` summary; a vanished id is skipped.
        let ids = vec![
            "idle-1".to_string(),
            "busy-1".to_string(),
            "gone".to_string(),
            "idle-2".to_string(),
        ];
        let split = split_sessions_close_batch(&ids, |id| match id {
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
    fn split_sessions_all_idle_yields_no_busy_survivors() {
        // Every member idle ⇒ the whole batch is eager-closed, nothing gated (§T.5).
        let ids = vec!["a".to_string(), "b".to_string()];
        let split = split_sessions_close_batch(&ids, |id| Some((id.to_string(), vec![])));
        assert_eq!(split.idle_ids, vec!["a", "b"]);
        assert!(split.busy_ids.is_empty());
        assert!(split.busy_summaries.is_empty());
    }
}
