//! `PtyManager` — the per-window pty/session subsystem, the Rust twin of
//! Swift's `SessionsModel` (`Sources/Nice/State/SessionsModel.swift`).
//!
//! One `PtyManager` per window (it lives on [`crate::window_state::WindowState`],
//! the R12 per-window state struct). It wires the R3–R7 terminal stack
//! (`nice_term_view::TerminalSessionHandle` gpui entities) to the R8
//! [`WorkspaceModel`] document: it owns the live window sessions, spawns deferred windows
//! on focus, and routes the entity's OSC title/cwd events back into the model.
//!
//! ## What this slice (R13 slice 1) owns
//!
//! * **Pure model routing** — [`PtyManager::window_cwd_changed`],
//!   [`PtyManager::window_title_changed`], [`PtyManager::set_active_window`]
//!   (the model half: active-window + ack-when-viewed),
//!   [`PtyManager::select_next_window`] / [`select_prev_window`] /
//!   `step_active_window`, [`PtyManager::add_window`] /
//!   [`add_terminal_to_active_session`], and [`PtyManager::route_terminal_event`]
//!   (map a decoded [`TerminalEvent`] into the right routing call). These take
//!   `&mut WorkspaceModel` and touch no gpui, so they are unit-tested with plain
//!   `#[test]` (the `nice` binary crate never links gpui test-support — see
//!   `crates/nice-itests`).
//! * **The gpui spawn primitives** — [`PtyManager::spawn_window`],
//!   [`ensure_active_window_spawned`],
//!   [`register_session_pty`], [`teardown`]. These are the building blocks the
//!   live app composes; they compile now and are exercised by the R13 slice-3
//!   live scenario (nothing wires an action to them yet, hence the
//!   module-level `dead_code` allow — the same seam pattern as
//!   `sidebar_actions` / `window_state`).
//!
//! ## What R13 slice 2 owns (this slice)
//!
//! * **The window lifecycle handlers** — [`window_exited`](PtyManager::window_exited)
//!   (the exact 5-step Swift ordering: clear overlay → model removal + neighbor
//!   refocus → pty release → deferred-companion spawn → dissolve check) and
//!   [`window_held`](PtyManager::window_held) (flip `is_alive` / idle the status
//!   / clear overlay, keep the window mounted). [`route_terminal_event`] now routes
//!   `Exited` / `OutputStarted` into them instead of dropping them.
//! * **The synchronous dissolve cascade**
//!   ([`finalize_dissolved_session`](PtyManager::finalize_dissolved_session)) — core
//!   `remove_session` (the single removal entry point, parent-pointer sweep) → pty
//!   release → selection prune → active-session fallback → the declared-but-inert
//!   R18/R19 hooks → the every-project-empty terminus. Three entry points share
//!   it: window-exit, [`close_session`](PtyManager::close_session) (R10's action,
//!   unconditional this cycle), and the unused cross-window
//!   [`dissolve_session_if_empty`](PtyManager::dissolve_session_if_empty) (R25).
//! * **The launch-overlay registry** —
//!   [`register_window_launch`](PtyManager::register_window_launch) /
//!   [`clear_window_launch`](PtyManager::clear_window_launch) /
//!   [`promote_window_launch`](PtyManager::promote_window_launch), the
//!   `launch_overlay_grace` seam (default [`nice_term_view::DEFAULT_LAUNCH_OVERLAY_GRACE`],
//!   `<= 0` promotes synchronously). The grace deadline reuses R7's App-Nap-safe
//!   `LaunchDeadline` injection — the live caller arms it and calls
//!   `promote_window_launch` on fire (the `Pending`-guard covers the clear race).
//! * **Termination** — [`terminate_window`](PtyManager::terminate_window) /
//!   [`terminate_all`](PtyManager::terminate_all) / [`teardown`], plus the
//!   synthetic held/armed test seams
//!   ([`mark_synthetic_held_window`](PtyManager::mark_synthetic_held_window) /
//!   [`mark_synthetic_armed_deferred_window`](PtyManager::mark_synthetic_armed_deferred_window)
//!   / [`window_is_spawned`](PtyManager::window_is_spawned)) so close-flow tests
//!   construct all three tri-state shapes without racing a real child.
//!
//! The gpui side effects the live caller composes on top of the pure cascade —
//! step-4 deferred spawn ([`ensure_active_window_spawned`]) and the terminus
//! actuator ([`apply_dissolve_terminus`](PtyManager::apply_dissolve_terminus),
//! close-this-window-or-quit via R12's registry) — need a gpui context, so they
//! stay separate primitives the slice-3 wiring calls (same seam pattern as slice
//! 1's `spawn_window`). [`window_exited`] returns a
//! [`WindowExitResolution`] telling that caller which to run.
//!
//! ## Deliberately deferred (later R13 slices — do not add here)
//!
//! * action-seam rewiring (sidebar `+` / strip `+` / ⌘T / pill select / close),
//!   the `cx.subscribe` that feeds [`route_terminal_event`] from a live entity,
//!   the live arming of the launch-overlay `LaunchDeadline`, and the
//!   `session-lifecycle` live scenario — **slice 3**.
//! * Claude status parsing (braille/✳ → thinking/waiting), session auto-title from
//!   the OSC label, socket, promotion, persistence — **R15/R18** (breadcrumbs
//!   below).
//!
//! [`ensure_active_window_spawned`]: PtyManager::ensure_active_window_spawned
//! [`register_session_pty`]: PtyManager::register_session_pty
//! [`teardown`]: PtyManager::teardown
//! [`select_prev_window`]: PtyManager::select_prev_window
//! [`add_terminal_to_active_session`]: PtyManager::add_terminal_to_active_session
//! [`route_terminal_event`]: PtyManager::route_terminal_event
//! [`window_exited`]: PtyManager::window_exited
//! [`window_held`]: PtyManager::window_held
//! [`close_session`]: PtyManager::close_session
//! [`terminate_window`]: PtyManager::terminate_window
//! [`terminate_all`]: PtyManager::terminate_all
//! [`register_window_launch`]: PtyManager::register_window_launch

// The gpui spawn/focus primitives + a few pure helpers have no live caller until
// R13 slice 3 wires the action seams and the entity subscription to them; the
// model-routing methods below ARE exercised by this module's tests. Same
// seam-for-a-later-slice pattern as `window_state` / `sidebar_actions`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gpui::{App, Entity, Global, Window};

use nice_model::{TermWindow, TermWindowKind, SidebarSessionSelection, Session, WorkspaceModel, SessionStatus};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{TerminalEvent, TerminalSessionHandle, DEFAULT_LAUNCH_OVERLAY_GRACE};

use crate::window_registry::WindowRegistry;

/// Terminal-window pill titles clip at 40 chars so the toolbar pill never
/// overflows (`SessionsModel.swift:400-404`).
const WINDOW_TITLE_MAX: usize = 40;

/// The per-window "Launching…" overlay state — the Rust twin of Swift's
/// `WindowLaunchStatus` (`SessionsModel.paneLaunchStates`). App-shaped (it carries
/// the launch command string the overlay renders), so it lives here in `crates/nice`
/// rather than in `nice-term-*` (the boundary block). The R7 view owns its own
/// zero-frame [`nice_term_view::LaunchOverlay`] timing machine; this registry is
/// the app-level mirror the shell reads to paint the placeholder, driven by the
/// same grace deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WindowLaunchStatus {
    /// Spawned, still within the grace window — overlay not yet shown.
    Pending { command: String },
    /// Grace elapsed with no output — the "Launching…" overlay is showing.
    Visible { command: String },
}

/// What a dissolve did to the window as a whole — the value the pure cascade
/// returns so the gpui caller can actuate Swift's every-project-empty terminus
/// (`AppState.finalizeDissolvedTab:359-372`) via R12's registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DissolveTerminus {
    /// The window still has content — nothing further to do.
    #[default]
    None,
    /// Every project is now empty. The live caller closes this window when
    /// another is live, else quits the app (see [`PtyManager::apply_dissolve_terminus`]).
    WindowEmptied,
}

impl DissolveTerminus {
    /// Combine two terminus outcomes across a multi-window close loop:
    /// `WindowEmptied` wins (once the window is empty it stays empty). Used by the
    /// `close_session`/close batch loops here and by
    /// [`crate::window_state::WindowState`]'s multi-session close aggregation.
    pub(crate) fn or(self, other: DissolveTerminus) -> DissolveTerminus {
        match (self, other) {
            (DissolveTerminus::WindowEmptied, _) | (_, DissolveTerminus::WindowEmptied) => {
                DissolveTerminus::WindowEmptied
            }
            _ => DissolveTerminus::None,
        }
    }
}

/// The outcome of a window exit — what gpui side effects the live caller must run
/// on top of the pure model cascade [`window_exited`](PtyManager::window_exited)
/// already applied. Swift runs these inline (steps 4–5 of `paneExited`); the Rust
/// split keeps the model routing unit-testable without a gpui context, and the
/// two effects are mutually exclusive with the dissolve (a surviving session may
/// spawn a companion; a dissolved one runs the terminus), so applying them after
/// the pure cascade is observably identical to Swift's inline order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowExitResolution {
    /// `Some(session_id)` when the session **survived** the exit — the live caller runs
    /// [`ensure_active_window_spawned`](PtyManager::ensure_active_window_spawned)
    /// (Swift step 4) so a refocus onto a deferred companion spawns its shell.
    /// `None` when the session dissolved (nothing to spawn) or the session was unknown.
    pub(crate) refocus_session: Option<String>,
    /// The dissolve terminus (whether the window emptied → close/quit).
    pub(crate) terminus: DissolveTerminus,
}

/// The routing outcome of a single [`TerminalEvent`] — empty for the title / cwd
/// / reset / first-output events (fully handled inline), carrying the window-exit
/// resolution for an `Exited { held: false }` event so the live subscription
/// applies the same step-4 spawn + terminus the direct [`window_exited`] caller
/// does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoutedExit {
    pub(crate) refocus_session: Option<String>,
    pub(crate) terminus: DissolveTerminus,
}

/// One live window session: the core→gpui adapter entity for this window. Dropping
/// the entity tears the child process group down (SIGHUP→SIGKILL via
/// `nice_term_core::Session::drop`), so a session entry removed from the cache leaks
/// no zsh.
///
/// Key focus is NOT owned here. The window's `TerminalView` mints and tracks its
/// own focus handle, and the window host ([`crate::app_shell::WindowHostView`]) —
/// which owns the views — routes key focus to it on activation. An earlier
/// design minted a focus handle on this struct for the manager to drive, but it
/// was never wired to any view, so focusing it did nothing; it has been removed.
struct WindowPty {
    /// The `nice-term-view` adapter entity owning this window's `Session`.
    handle: Entity<TerminalSessionHandle>,
}

/// The per-window pty/session manager. Session-keyed: each session maps to its live window
/// sessions (`term_window_id -> WindowPty`), mirroring Swift's session-keyed
/// `ptySessions` cache. A session entry existing (even empty) means Swift's
/// `makeSession` ran for that session — the precondition
/// [`ensure_active_window_spawned`](PtyManager::ensure_active_window_spawned)
/// checks before lazily spawning a deferred companion window.
/// The per-window shell-injection env, set once at window construction by
/// `crate::app::arm_window_control_socket` (the Rust twin of Swift
/// `SessionsModel.bootstrapSocket`'s `controlSocketExtraEnv`). Every pty this
/// window's [`PtyManager`] spawns gets these merged into its env
/// **spec-wins** (see [`spawn_window`](PtyManager::spawn_window)).
///
/// `None` on a manager whose window never bootstrapped a control socket (the
/// ~10 landed scenarios / itests that build a `WindowState` directly and spawn
/// ZDOTDIR-blanked fixture shells) — those spawn with **no** injection, so the
/// blanked `ZDOTDIR` they set via `SpawnSpec::with_env` is untouched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowShellEnv {
    /// `NICE_SOCKET` — the window's control-socket path (the `claude()` shadow's
    /// handshake target). Always `Some` in production (the path is minted before
    /// the socket binds); a bind failure leaves it set so shells' `nc … -w 2`
    /// fails fast and falls back to direct `command claude`.
    pub(crate) socket_path: Option<String>,
    /// `ZDOTDIR` — the synthetic rc-chain directory. `None` when the launch-time
    /// stub write failed (windows still get `NICE_SOCKET`; they just source the
    /// user's real rc directly).
    pub(crate) zdotdir: Option<String>,
    /// The value for `NICE_USER_ZDOTDIR`. `None` ⇒ the empty string is injected
    /// (Nice inherited no `ZDOTDIR`); the empty/absent distinction is semantic
    /// for the `.zshenv` stub's XDG discovery branch, so the var is ALWAYS set.
    pub(crate) user_zdotdir: Option<String>,
    /// `NICE_COMPOSE_CONF` — the Command Compose conf-file path the injected
    /// ZLE widget reads per compose (accent + `claude -p` flags). `None` ⇒ the
    /// var is not injected and the widget falls back to its built-in defaults.
    pub(crate) compose_conf: Option<String>,
}

pub(crate) struct PtyManager {
    /// `session_id -> (term_window_id -> live session)`.
    sessions: HashMap<String, HashMap<String, WindowPty>>,
    /// Per-window "Launching…" overlay entries (Swift's `paneLaunchStates`). A
    /// window is inserted `Pending` at spawn and promoted to `Visible` when the
    /// grace deadline fires with no output; cleared on first output, exit, or
    /// held.
    window_launch_states: HashMap<String, WindowLaunchStatus>,
    /// The grace window before a silent window's overlay promotes to `Visible`
    /// (Swift's `launchOverlayGraceSeconds`). Default
    /// [`DEFAULT_LAUNCH_OVERLAY_GRACE`]; a `<= 0` value promotes synchronously
    /// inside [`register_window_launch`](Self::register_window_launch) (the test seam).
    launch_overlay_grace: Duration,
    /// Test-only: `<session>:<window>` keys [`window_is_spawned`](Self::window_is_spawned)
    /// reports as spawned without a real session (Swift's `syntheticSpawnedPanes`).
    /// Always empty in production — nothing populates it outside the `mark_*`
    /// test seams.
    synthetic_spawned: HashSet<String>,
    /// Subset of [`synthetic_spawned`](Self::synthetic_spawned) whose
    /// [`terminate_window`](Self::terminate_window) fires
    /// [`window_exited`](Self::window_exited) synchronously, mirroring the production
    /// held-window fast path (`syntheticHeldPanes`). One-shot: consumed on terminate.
    synthetic_held: HashSet<String>,
    /// Subset of [`synthetic_spawned`](Self::synthetic_spawned) whose
    /// [`terminate_window`](Self::terminate_window) fires
    /// [`window_exited`](Self::window_exited) synchronously with no real child ever
    /// having run (`syntheticArmedDeferredPanes` — the armed-but-not-fired
    /// deferred spawn). One-shot: consumed on terminate.
    synthetic_armed: HashSet<String>,
    /// R20.5 test seam: `<session>:<window>` keys
    /// [`shell_has_foreground_child`](Self::shell_has_foreground_child) reports as
    /// busy (a shell with a foreground child) WITHOUT a real pty running a real
    /// command. Lets the busy-close → confirmation-modal wiring be unit-tested off
    /// the live `tcgetpgrp` syscall (which needs a real foreground child and is
    /// covered once by the live scenario). Always empty in production — nothing
    /// populates it outside the [`mark_synthetic_foreground_child`](Self::mark_synthetic_foreground_child)
    /// test seam. Mirrors the `synthetic_spawned`/`_held`/`_armed` seams.
    synthetic_foreground_child: HashSet<String>,
    /// Test seam (gpui's deterministic scheduler): when `false`, every session
    /// spawned from here on has its event-driven drain wake disabled
    /// ([`TerminalSessionHandle::set_event_wake_enabled`]). A pty's feeder /
    /// exit-watcher threads wake the drain task from a BACKGROUND thread, which
    /// gpui's test scheduler flags as non-determinism ("schedule_local must run on
    /// the test thread") — so a `#[gpui::test]` that both spawns a window and runs
    /// the executor (`run_until_parked` / `advance_clock`, which is what parks the
    /// drain and registers the waker the exit-watcher then wakes) fails
    /// intermittently. Applied at spawn time, which is the ONLY point early enough
    /// for a window the test does not spawn itself (the background-`/fork`
    /// materialization spawns its deferred window from inside a spawned task). Always
    /// `true` in production — the field only exists under `cfg(test)`.
    #[cfg(test)]
    event_wakes_enabled: bool,
    /// Injectable id minter (test seam). Production default:
    /// `<prefix><ms>-<suffix>` — the millisecond keeps ids roughly time-sortable
    /// for log triage; the short suffix keeps two creations in the same
    /// millisecond from colliding (Swift saw two `/branch`es in one ms collide).
    /// Unit tests inject a deterministic counter and assert by id.
    mint_id: Box<dyn Fn(&str) -> String>,
    /// R14: the per-window shell-injection env, set once at window construction
    /// (before the Main window forks). `None` until a control socket is bootstrapped
    /// for this window, so managers built directly by scenarios/itests inject
    /// nothing. See [`WindowShellEnv`].
    window_shell_env: Option<WindowShellEnv>,
    /// W5 (R18): project ids the user asked to close whole (Swift's
    /// `CloseRequestCoordinator.projectsPendingRemoval`). Read — not cleared —
    /// by [`finalize_dissolved_session`](Self::finalize_dissolved_session) on each session
    /// dissolve so a multi-session project keeps the flag across earlier dissolves;
    /// cleared when its last session empties and the row drops.
    pending_project_removal: HashSet<String>,
    /// R19: session ids dissolved since the last drain — the file-browser per-session
    /// cleanup hook. [`finalize_dissolved_session`](Self::finalize_dissolved_session) (the
    /// single session-removal entry point) pushes here; [`WindowState`](crate::window_state::WindowState),
    /// which owns the [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore),
    /// drains it via [`take_dissolved_session_ids`](Self::take_dissolved_session_ids) after
    /// each cascade to drop the closed session's browser state. Kept here (not threaded
    /// through the cascade signatures) so every dissolve path — UI close AND the
    /// route_terminal_event window-exit — funnels one removal list without rippling
    /// the store into `PtyManager`.
    dissolved_session_ids: Vec<String>,
}

impl PtyManager {
    /// A fresh manager with the production id minter and an empty session cache.
    pub(crate) fn new() -> Self {
        Self::build(Box::new(default_mint_id))
    }

    /// A manager with an injected id minter (the deterministic test seam).
    pub(crate) fn with_mint_id(mint: impl Fn(&str) -> String + 'static) -> Self {
        Self::build(Box::new(mint))
    }

    /// Shared constructor: empty caches, default launch grace, the given minter.
    fn build(mint_id: Box<dyn Fn(&str) -> String>) -> Self {
        Self {
            sessions: HashMap::new(),
            window_launch_states: HashMap::new(),
            launch_overlay_grace: DEFAULT_LAUNCH_OVERLAY_GRACE,
            synthetic_spawned: HashSet::new(),
            synthetic_held: HashSet::new(),
            synthetic_armed: HashSet::new(),
            synthetic_foreground_child: HashSet::new(),
            #[cfg(test)]
            event_wakes_enabled: true,
            mint_id,
            window_shell_env: None,
            pending_project_removal: HashSet::new(),
            dissolved_session_ids: Vec::new(),
        }
    }

    /// Drain the session ids dissolved since the last call — the R19 file-browser
    /// cleanup hook. [`WindowState`](crate::window_state::WindowState) calls this
    /// after every session cascade and drops each id's browser state from its
    /// [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore).
    pub(crate) fn take_dissolved_session_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut self.dissolved_session_ids)
    }

    /// Mark `project_id` for whole-project removal (W5 "Close Project"): its row
    /// drops from the tree once its last session dissolves
    /// ([`finalize_dissolved_session`](Self::finalize_dissolved_session)). The pinned
    /// Terminals group is never marked. Swift's
    /// `CloseRequestCoordinator.projectsPendingRemoval.insert`.
    pub(crate) fn mark_project_pending_removal(&mut self, project_id: &str) {
        if project_id != WorkspaceModel::TERMINALS_PROJECT_ID {
            self.pending_project_removal.insert(project_id.to_string());
        }
    }

    /// Mint a unique id for a freshly-created window, via the injected seam.
    fn mint(&self, prefix: &str) -> String {
        (self.mint_id)(prefix)
    }

    /// Mint a fresh session id via the injected seam — the branch-parent
    /// materialization path (`WindowState::materialize_branch_parent`) mints its
    /// session + `-claude`/`-t1` window ids up front to hand to the model's
    /// `insert_branch_parent` (which takes them as params), mirroring
    /// `create_claude_session`'s internal `self.mint("t")`.
    pub(crate) fn mint_session_id(&self, prefix: &str) -> String {
        self.mint(prefix)
    }

    // MARK: - Window title / cwd routing (pure model, unit-tested)

    /// A window's shell emitted OSC 7 with a new working directory. Stash it on
    /// `TermWindow.cwd` **only** so a relaunch respawns the window where it was — never
    /// `Session.cwd`, which is load-bearing for `claude --resume`'s working dir and
    /// would silently relocate the session on restore if a companion terminal's
    /// `cd` overwrote it (`SessionsModel.swift:483-497`). Silently drops a stale
    /// session/window id. Returns whether anything changed — the caller fires the
    /// debounced session save on `true` (the `onSessionMutation` seam; R18).
    pub(crate) fn window_cwd_changed(
        &mut self,
        model: &mut WorkspaceModel,
        session_id: &str,
        term_window_id: &str,
        cwd: &str,
    ) -> bool {
        let mut changed = false;
        model.mutate_session(session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|p| p.id == term_window_id) {
                if term_window.cwd.as_deref() != Some(cwd) {
                    term_window.cwd = Some(cwd.to_string());
                    changed = true;
                }
            }
        });
        changed
    }

    /// A window's program emitted an OSC 0/2 title. **Terminal-branch policy only**
    /// (`SessionsModel.swift:385-414`): the emitted title becomes the pill label
    /// verbatim, except an empty/whitespace title is ignored, a manually-renamed
    /// window (`title_manually_set`) is never clobbered by OSC, and an accepted
    /// title clips at [`WINDOW_TITLE_MAX`] chars.
    ///
    /// The **Claude branch is gated on `is_claude_running`** and dropped whole
    /// this cycle: `is_claude_running` stays `false` for every window in R13 (only
    /// R15's socket promotion flips it), so a claude-kind window contributes no
    /// status and no OSC-driven session title — a deferred-resume Claude window is a
    /// plain `zsh` whose theme OSC titles must not clobber the persisted session
    /// label (`SessionsModel.swift:416-435`). Silently drops a stale session/window id.
    ///
    /// Returns whether the pill label actually changed — the caller fires the
    /// debounced session save on `true` (Swift's `@Observable` write-back →
    /// `onTreeMutation`, byte-equality-skipped; R18 owns the save). A no-op
    /// re-report of the current title returns `false` (Validation probe (b)),
    /// mirroring [`window_cwd_changed`](Self::window_cwd_changed)'s did-change signal.
    pub(crate) fn window_title_changed(
        &mut self,
        model: &mut WorkspaceModel,
        session_id: &str,
        term_window_id: &str,
        title: &str,
    ) -> bool {
        // Read the window's kind + lock facts, then drop the borrow before the
        // mutation (Swift reads `pane` then re-enters via `mutateTab` — parity note).
        let Some(session) = model.session_for(session_id) else {
            return false;
        };
        let Some(term_window) = session.windows.iter().find(|p| p.id == term_window_id) else {
            return false;
        };
        let kind = term_window.kind;
        let title_manually_set = term_window.title_manually_set;
        let is_claude_running = term_window.is_claude_running;

        match kind {
            TermWindowKind::Terminal => {
                let trimmed = title.trim();
                // Whitespace-only titles never overwrite the current pill label.
                if trimmed.is_empty() {
                    return false;
                }
                // A user pill-rename locks the title; OSC from the running program
                // (vim's `vim foo`, zsh theme spam) must not win.
                if title_manually_set {
                    return false;
                }
                let clipped = clip_title(trimmed, WINDOW_TITLE_MAX);
                let mut changed = false;
                model.mutate_session(session_id, |session| {
                    if let Some(term_window) = session.windows.iter_mut().find(|p| p.id == term_window_id) {
                        if term_window.title != clipped {
                            term_window.title = clipped;
                            changed = true;
                        }
                    }
                });
                changed
            }
            TermWindowKind::Claude => {
                // R15 T5: the Claude branch — split the braille-spinner (U+2800..
                // U+28FF → thinking) / sparkle (U+2733 → waiting) status prefix via
                // [`parse_claude_title`], apply the status transition, and feed the
                // trailing label into the session auto-title (dropping the "Claude Code"
                // placeholder). Gated on `is_claude_running`: a deferred-resume
                // Claude window is a plain `zsh` whose theme OSC titles must not
                // clobber the persisted session label, so the whole branch drops
                // until the socket in-place promotion (the only production
                // false→true flip) opens the gate (`SessionsModel.swift:416-474`).
                if !is_claude_running {
                    return false;
                }
                let (status, label) = parse_claude_title(title);
                if let Some(new_status) = status {
                    // Acknowledge the pulse only when the user is actually looking at
                    // this window — the viewed session's active window (Swift's
                    // `viewing && isActivePane`). A manually-renamed Claude window still
                    // flips status: the title lock lives in the terminal branch, not
                    // here (`AppStatePaneLifecycleTests.claudePane_manuallySet_...`).
                    let viewing = model.active_session_id() == Some(session_id);
                    model.mutate_session(session_id, |session| {
                        let is_active_window = session.active_window_id.as_deref() == Some(term_window_id);
                        if let Some(term_window) = session.windows.iter_mut().find(|p| p.id == term_window_id) {
                            term_window.apply_status_transition(new_status, viewing && is_active_window);
                        }
                    });
                }
                // The trailing label humanizes into the TAB auto-title — never the
                // Claude window's own pill (that stays "Claude"/the user's rename).
                // Skip an empty label and Claude's generic "Claude Code" placeholder.
                let raw_label = label.trim();
                if raw_label.is_empty() || raw_label == "Claude Code" {
                    return false;
                }
                model.apply_auto_title(session_id, raw_label);
                // This branch never writes the window pill, so the pill-label-changed
                // signal is always `false` (status + session title flow through the
                // model's own mutation hooks).
                false
            }
        }
    }

    /// Dispatch a decoded [`TerminalEvent`] from a window's session entity to the
    /// right routing call. This is the pure connector the live entity
    /// subscription (slice 3) invokes per event; splitting it out keeps the
    /// routing unit-testable without a live pty or a gpui context.
    ///
    /// Returns a [`RoutedExit`]: empty for title / cwd / reset / first-output
    /// (fully handled here), carrying the window-exit resolution for a clean
    /// `Exited { held: false }` so the live subscription runs the same step-4
    /// spawn + terminus a direct [`window_exited`](Self::window_exited) caller does.
    pub(crate) fn route_terminal_event(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        session_id: &str,
        term_window_id: &str,
        event: &TerminalEvent,
    ) -> RoutedExit {
        match event {
            TerminalEvent::TitleChanged(title) => {
                let _ = self.window_title_changed(model, session_id, term_window_id, title);
                RoutedExit::default()
            }
            TerminalEvent::CwdChanged(path) => {
                // OSC 7 → `TermWindow.cwd` (plain path across the boundary; the app owns
                // the model type). The `to_string_lossy` is safe for the on-disk
                // absolute paths OSC 7 reports.
                let _ = self.window_cwd_changed(model, session_id, term_window_id, &path.to_string_lossy());
                RoutedExit::default()
            }
            TerminalEvent::TitleReset => {
                // The terminal title-policy (`SessionsModel.swift:391-414`) only
                // accepts a non-empty OSC *set*; a reset to the terminal default
                // carries no new label, so it is a no-op for the window pill here.
                RoutedExit::default()
            }
            TerminalEvent::OutputStarted => {
                // First pty byte — dismiss the "Launching…" overlay (Swift's
                // `NiceTerminalView.onFirstData` → `clearPaneLaunch`).
                self.clear_window_launch(term_window_id);
                RoutedExit::default()
            }
            TerminalEvent::Exited { held: true, .. } => {
                // `TabPtySession` decided to keep the view mounted (non-clean /
                // pre-first-byte exit) — flip the model to dead-but-on-screen and
                // clear the overlay. No removal, no dissolve.
                self.window_held(model, session_id, term_window_id);
                RoutedExit::default()
            }
            TerminalEvent::Exited { held: false, .. } => {
                // Clean exit — the full 5-step `paneExited` cascade. The
                // resolution tells the live caller to run step-4 spawn on a
                // surviving session and to actuate the terminus.
                let r = self.window_exited(model, selection, session_id, term_window_id);
                RoutedExit {
                    refocus_session: r.refocus_session,
                    terminus: r.terminus,
                }
            }
            // `TerminalEvent` is `#[non_exhaustive]`; a still-later lifecycle
            // variant reaches here until this manager learns to route it.
            _ => RoutedExit::default(),
        }
    }

    // MARK: - Selection / window navigation (pure model, unit-tested)

    /// Pick which window is focused in `session_id` — the **model half** of Swift's
    /// `setActivePane` (`SessionsModel.swift:534-545`): re-point `active_window_id`
    /// (a no-op if `term_window_id` isn't on the session, so selection never dangles) and,
    /// when the session is the one being viewed, acknowledge the newly-active window if
    /// it was waiting.
    ///
    /// The live app composes the side effect Swift's `setActivePane` also runs
    /// on top of this: [`ensure_active_window_spawned`] (deferred spawn), which
    /// needs a gpui context and so is a separate primitive the slice-3 action
    /// wiring calls right after this. Key focus is Swift's third piece, but in
    /// the Rust app it lives in the window host ([`crate::app_shell::WindowHostView`],
    /// which owns the terminal views), not here.
    ///
    /// [`ensure_active_window_spawned`]: PtyManager::ensure_active_window_spawned
    pub(crate) fn set_active_window(&mut self, model: &mut WorkspaceModel, session_id: &str, term_window_id: &str) {
        let viewing = model.active_session_id() == Some(session_id);
        model.mutate_session(session_id, |session| {
            if session.windows.iter().any(|p| p.id == term_window_id) {
                session.active_window_id = Some(term_window_id.to_string());
                if viewing {
                    if let Some(term_window) = session.windows.iter_mut().find(|p| p.id == term_window_id) {
                        term_window.mark_acknowledged_if_waiting();
                    }
                }
            }
        });
    }

    /// Move focus to the next window within the active session, wrapping. No-op when
    /// the active session has fewer than two windows (`SessionsModel.swift:569`).
    pub(crate) fn select_next_window(&mut self, model: &mut WorkspaceModel) {
        self.step_active_window(model, 1);
    }

    /// Move focus to the previous window within the active session, wrapping
    /// (`SessionsModel.swift:572`).
    pub(crate) fn select_prev_window(&mut self, model: &mut WorkspaceModel) {
        self.step_active_window(model, -1);
    }

    /// Wrapping step of the active session's active window by `offset`, routed through
    /// [`set_active_window`](Self::set_active_window) so the ack side effect rides
    /// along (and, in the live app, the deferred spawn + focus the caller adds).
    /// No-op when there is no active session, the session has fewer than two windows, or
    /// its active window isn't resolvable (`SessionsModel.swift:574-584`).
    fn step_active_window(&mut self, model: &mut WorkspaceModel, offset: isize) {
        let Some(session_id) = model.active_session_id().map(str::to_owned) else {
            return;
        };
        let Some(session) = model.session_for(&session_id) else {
            return;
        };
        let count = session.windows.len();
        if count < 2 {
            return;
        }
        let Some(active) = session.active_window_id.clone() else {
            return;
        };
        let Some(cur) = session.windows.iter().position(|p| p.id == active) else {
            return;
        };
        // `((i + off) % n + n) % n`, expressed with rem_euclid.
        let next = (cur as isize + offset).rem_euclid(count as isize) as usize;
        let next_id = session.windows[next].id.clone();
        self.set_active_window(model, &session_id, &next_id);
    }

    /// Append a new **terminal** window to `session_id`, focus it, and return its new
    /// id (`None` if the session is unknown). The model half of Swift's `addPane`
    /// (`SessionsModel.swift:592-636`): only terminal-kind windows are
    /// constructible here — Claude windows are created exclusively by the
    /// claude-session paths, preserving the ≤1-Claude-per-session creation edge. The
    /// monotonic `next_terminal_index` counter is consumed via
    /// [`WorkspaceModel::add_window`] (an explicit `title` consumes the slot too).
    ///
    /// The live app spawns the pty behind this immediately (explicit adds are
    /// **not** deferred — deferred spawn is only for windows modelled up front by a
    /// session-creation path); slice 3 composes [`spawn_window`](Self::spawn_window) after
    /// the model mutation.
    pub(crate) fn add_window(
        &mut self,
        model: &mut WorkspaceModel,
        session_id: &str,
        title: Option<String>,
    ) -> Option<String> {
        // Guard before minting so an unknown session wastes no id (Swift guards
        // `sessions.session(for:)` first).
        model.session_for(session_id)?;
        let new_id = self.mint(&format!("{session_id}-p"));
        model.add_window(session_id, new_id, title)
    }

    /// Append a terminal window to the active session and focus it; no-op (returns
    /// `None`) when there is no active session (`SessionsModel.swift:640-643`).
    pub(crate) fn add_terminal_to_active_session(&mut self, model: &mut WorkspaceModel) -> Option<String> {
        let session_id = model.active_session_id().map(str::to_owned)?;
        self.add_window(model, &session_id, None)
    }

    // MARK: - Launch overlay registry (pure model, unit-tested)

    /// Record that a window was just spawned and start the grace window (Swift's
    /// `registerPaneLaunch`, `SessionsModel.swift:506-520`). The entry lands
    /// `Pending`; if it stays silent past [`launch_overlay_grace`](Self::launch_overlay_grace)
    /// it promotes to `Visible` and the shell paints "Launching…", and if
    /// [`clear_window_launch`](Self::clear_window_launch) fires first (first byte /
    /// exit / held) the overlay never appears.
    ///
    /// A `<= 0` grace promotes **synchronously** here (the test seam — no
    /// deadline hop). Otherwise this returns `true`: the live caller (slice 3)
    /// arms R7's App-Nap-safe [`nice_term_view::LaunchDeadline`] and calls
    /// [`promote_window_launch`](Self::promote_window_launch) when it fires. That
    /// method's `Pending`-guard covers the clear-before-fire race, so a coalesced
    /// or late deadline never resurrects a cleared overlay.
    pub(crate) fn register_window_launch(&mut self, term_window_id: &str, command: impl Into<String>) -> bool {
        let command = command.into();
        self.window_launch_states
            .insert(term_window_id.to_string(), WindowLaunchStatus::Pending { command });
        if self.launch_overlay_grace <= Duration::ZERO {
            self.promote_window_launch(term_window_id);
            false
        } else {
            true
        }
    }

    /// Promote a still-`Pending` launch entry to `Visible` — the grace deadline
    /// fired (Swift's inline `promote` closure). A no-op once the entry was
    /// cleared or already promoted, so a deadline that fires after the first byte
    /// never resurrects the overlay.
    pub(crate) fn promote_window_launch(&mut self, term_window_id: &str) {
        if let Some(WindowLaunchStatus::Pending { command }) = self.window_launch_states.get(term_window_id) {
            let command = command.clone();
            self.window_launch_states
                .insert(term_window_id.to_string(), WindowLaunchStatus::Visible { command });
        }
    }

    /// Remove any pending/visible overlay for `term_window_id` (Swift's `clearPaneLaunch`).
    /// Fired on first pty byte, window exit, and held so a process that dies before
    /// emitting anything leaves no orphan "Launching…" placeholder.
    pub(crate) fn clear_window_launch(&mut self, term_window_id: &str) {
        self.window_launch_states.remove(term_window_id);
    }

    /// The launch-overlay entry for `term_window_id`, if any (the shell reads it to
    /// paint the placeholder; tests assert on it).
    pub(crate) fn window_launch_state(&self, term_window_id: &str) -> Option<&WindowLaunchStatus> {
        self.window_launch_states.get(term_window_id)
    }

    /// Override the launch-overlay grace window (the `launchOverlayGraceSeconds`
    /// test seam — set to `Duration::ZERO` for synchronous promotion).
    pub(crate) fn set_launch_overlay_grace(&mut self, grace: Duration) {
        self.launch_overlay_grace = grace;
    }

    // MARK: - Window lifecycle handlers (pure model + cascade; unit-tested)

    /// A window's child exited cleanly — the exact 5-step Swift `paneExited`
    /// ordering (`SessionsModel.swift:318-346`): (1) clear the launch overlay;
    /// (2) remove the window from its session, re-pointing `active_window_id` to the slot
    /// neighbor via the same rule a cross-window move uses
    /// ([`WorkspaceModel::neighbor_active_window_id`]); (3) release the window's pty session;
    /// (5) if the session is now empty, run the dissolve cascade synchronously with
    /// indices resolved at that instant.
    ///
    /// **Step 4 — the deferred-companion spawn — is the caller's gpui side
    /// effect.** It has no model-observable effect (it only forks a pty) and is
    /// mutually exclusive with the dissolve (a surviving session may spawn; a
    /// dissolved one cannot), so this returns [`WindowExitResolution`]: the live
    /// caller runs [`ensure_active_window_spawned`](Self::ensure_active_window_spawned)
    /// on `refocus_session` (Swift's step 4) and actuates `terminus`. Applying them
    /// after this pure cascade is observably identical to Swift's inline order.
    /// Silently drops a stale session/window id.
    pub(crate) fn window_exited(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        session_id: &str,
        term_window_id: &str,
    ) -> WindowExitResolution {
        // (1) clear the launch overlay.
        self.clear_window_launch(term_window_id);
        // (2) model removal + neighbor refocus.
        model.mutate_session(session_id, |session| {
            if let Some(idx) = session.windows.iter().position(|p| p.id == term_window_id) {
                session.windows.remove(idx);
                if session.active_window_id.as_deref() == Some(term_window_id) {
                    session.active_window_id = WorkspaceModel::neighbor_active_window_id(idx, &session.windows);
                }
            }
        });
        // (3) pty release.
        self.release_window_pty(session_id, term_window_id);
        // (5) dissolve check — the empty-session callback's indices are valid only
        // because nothing runs in between (Swift keeps this synchronous). The
        // caller runs step 4 (spawn) on `refocus_session` on the way out.
        match model.session_for(session_id) {
            Some(session) if session.windows.is_empty() => {
                let terminus = match model.project_session_index(session_id) {
                    Some((pi, ti)) => self.finalize_dissolved_session(model, selection, pi, ti, session_id),
                    None => DissolveTerminus::None,
                };
                WindowExitResolution {
                    refocus_session: None,
                    terminus,
                }
            }
            Some(_) => WindowExitResolution {
                // Session survived: focus may have auto-switched onto a deferred
                // companion — the live caller spawns it before anything else.
                refocus_session: Some(session_id.to_string()),
                terminus: DissolveTerminus::None,
            },
            None => WindowExitResolution::default(),
        }
    }

    /// A window's process exited but its view stays mounted so the user can read
    /// the scrollback (Swift's `paneHeld`, `SessionsModel.swift:362-377`): clear
    /// the launch overlay, flip `is_alive` false, and idle out any pulsing status
    /// so the rest of the model (sidebar dot, live counts, `has_claude`) treats
    /// the window as dead — while leaving it in `session.term_windows` so the pill + view stay
    /// on screen. The model removal happens later when the user closes the session
    /// ([`terminate_window`](Self::terminate_window) synthesizes the deferred exit).
    /// Silently drops a stale session/window id.
    pub(crate) fn window_held(&mut self, model: &mut WorkspaceModel, session_id: &str, term_window_id: &str) {
        self.clear_window_launch(term_window_id);
        model.mutate_session(session_id, |session| {
            if let Some(term_window) = session.windows.iter_mut().find(|p| p.id == term_window_id) {
                term_window.is_alive = false;
                // A held-dead window is not thinking or waiting regardless of its
                // last OSC title; idle it and clear the ack so a future fresh
                // waiting window can pulse again.
                term_window.status = SessionStatus::Idle;
                term_window.waiting_acknowledged = false;
                // Clear the promotion flag so a fresh `claude` in this session routes
                // correctly (R15) — a held pty is a corpse, not a live shell.
                term_window.is_claude_running = false;
            }
        });
    }

    /// Drop a single window's pty session from the cache (Swift's
    /// `ptySessions[tabId]?.removePane`). Keeps the (possibly now-empty) per-session
    /// container; the dissolve cascade drops that separately. Dropping the
    /// [`TerminalSessionHandle`] tears its child process group down
    /// (SIGHUP→SIGKILL via `nice_term_core::Session::drop`), so no orphan zsh.
    fn release_window_pty(&mut self, session_id: &str, term_window_id: &str) {
        if let Some(windows) = self.sessions.get_mut(session_id) {
            windows.remove(term_window_id);
        }
    }

    // MARK: - Dissolve cascade (pure core + gpui terminus; unit-tested)

    /// Finish dissolving a session whose `term_windows` array reached zero — the synchronous
    /// core of Swift's `AppState.finalizeDissolvedTab` (`AppState.swift:326-373`),
    /// in its exact order: `remove_session` (the **single** removal entry point, which
    /// does the parent-pointer sweep) → pty-session release → selection prune →
    /// active-session fallback in [`WorkspaceModel::navigable_sidebar_session_ids`] order. The
    /// later-row subscriber hooks stay **declared but inert** (see the body).
    /// Returns the every-project-empty [`DissolveTerminus`] the gpui caller
    /// actuates via [`apply_dissolve_terminus`](Self::apply_dissolve_terminus).
    ///
    /// Delivery is synchronous by contract: `(pi, ti)` are valid only because
    /// nothing runs between the empty-session check and this call.
    fn finalize_dissolved_session(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        pi: usize,
        ti: usize,
        session_id: &str,
    ) -> DissolveTerminus {
        // Core: the single removal entry point (array remove + parent-pointer
        // sweep, atomically — a future close path can't orphan a /branch child).
        model.remove_session(pi, ti);
        // pty-session release (Swift's `removePtySession`).
        self.sessions.remove(session_id);

        // Subscriber hooks (later rows):
        //   * file-browser per-session cleanup (R19): record the dissolved session id so
        //     `WindowState` drops its `FileBrowserStore` entry after the cascade
        //     (the single session-removal entry point, so every dissolve path — UI
        //     close AND the window-exit route — funnels one removal list).
        self.dissolved_session_ids.push(session_id.to_string());
        //   * debounced session save (onSessionMutation) → the UI-close callers
        //     (`WindowState::save_to_store`) schedule it; R18.

        // Selection prune (R10 multi-select): drop the dissolved id (and clear a
        // dangling anchor/active mirror) before any view re-renders against the
        // shrunken tree. Uses the post-removal navigable set.
        let valid: HashSet<String> = model.navigable_sidebar_session_ids().into_iter().collect();
        selection.prune(&valid);

        // Active-session fallback via navigable order (Swift's `firstAvailableTabId`).
        if model.active_session_id() == Some(session_id) {
            if let Some(fallback) = model.navigable_sidebar_session_ids().into_iter().next() {
                model.select_session(&fallback);
            }
            // else: no navigable session remains — the window is empty and closes /
            // quits below (the `WorkspaceModel` has no `None` active-session writer, and
            // the window is going away, so leaving the stale id is harmless).
        }

        // W5 (R18) project-pending-removal (Swift `AppState.finalizeDissolvedTab:349-355`):
        // if the user asked to close this whole project and its last session just
        // dissolved, drop the (non-Terminals) row. Read without clearing until it
        // empties so earlier-session dissolves in a multi-session project keep the flag.
        if pi < model.projects.len() {
            let project_id = model.projects[pi].id.clone();
            if self.pending_project_removal.contains(&project_id)
                && model.projects[pi].sessions.is_empty()
                && project_id != WorkspaceModel::TERMINALS_PROJECT_ID
            {
                self.pending_project_removal.remove(&project_id);
                model.projects.remove(pi);
            }
        }

        // Every-project-empty terminus (Swift closes this window when another is
        // live, else quits the app).
        if model.projects.iter().all(|p| p.sessions.is_empty()) {
            DissolveTerminus::WindowEmptied
        } else {
            DissolveTerminus::None
        }
    }

    /// Dissolve `session_id` if a cross-window move / tear-off left it with no windows,
    /// running the same cascade a last-window exit would (Swift's
    /// `dissolveTabIfEmpty`, `AppState.swift:382-387`). No-op when the session still
    /// has windows or doesn't exist. This is the dissolve entry point for the R25
    /// `extract_window` path, which bypasses the window-exit callback — **modelled
    /// now, unused this cycle** (no cross-window migration until R25).
    pub(crate) fn dissolve_session_if_empty(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        session_id: &str,
    ) -> DissolveTerminus {
        match model.project_session_index(session_id) {
            Some((pi, ti)) if model.projects[pi].sessions[ti].windows.is_empty() => {
                self.finalize_dissolved_session(model, selection, pi, ti, session_id)
            }
            _ => DissolveTerminus::None,
        }
    }

    /// Close an entire session unconditionally (this cycle has no confirmation — W5 is
    /// R18), the Rust twin of `CloseRequestCoordinator.hardKillTab`
    /// (`CloseRequestCoordinator.swift:297-363`). The third dissolve entry point.
    ///
    /// Splits windows by [`window_is_spawned`](Self::window_is_spawned).
    /// [`terminate_window`](Self::terminate_window) is a no-op for a **model-only**
    /// window (no session at all — the lazy companion the user never focused), so
    /// those are dropped from the model directly; otherwise a SIGHUP-only close
    /// would leave them behind and the session would never dissolve. Unspawned rows
    /// are dropped **before** terminating the spawned ones so a held window's
    /// synchronous `window_exited` sees an already-pruned array and its empty-session
    /// check fires (the tri-state close bug the Swift reorder fixed). Returns the
    /// aggregate [`DissolveTerminus`].
    pub(crate) fn close_session(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        session_id: &str,
    ) -> DissolveTerminus {
        let Some(session) = model.session_for(session_id) else {
            return DissolveTerminus::None;
        };
        let mut spawned: Vec<String> = Vec::new();
        let mut unspawned: Vec<String> = Vec::new();
        for term_window in &session.windows {
            if self.window_is_spawned(session_id, &term_window.id) {
                spawned.push(term_window.id.clone());
            } else {
                unspawned.push(term_window.id.clone());
            }
        }

        if !unspawned.is_empty() {
            if spawned.is_empty() {
                // Model-only session: nothing async to hook into — clear the windows and
                // dissolve synchronously (Validation probe (d)).
                model.mutate_session(session_id, |session| {
                    session.windows.clear();
                    session.active_window_id = None;
                });
                return match model.project_session_index(session_id) {
                    Some((pi, ti)) => self.finalize_dissolved_session(model, selection, pi, ti, session_id),
                    None => DissolveTerminus::None,
                };
            }
            // Drop unspawned rows up front (before terminating spawned ones).
            let drop: HashSet<String> = unspawned.into_iter().collect();
            model.mutate_session(session_id, |session| {
                session.windows.retain(|p| !drop.contains(&p.id));
                let active_dropped = session
                    .active_window_id
                    .as_deref()
                    .is_some_and(|a| drop.contains(a));
                if active_dropped {
                    session.active_window_id = session.windows.first().map(|p| p.id.clone());
                }
            });
        }

        let mut terminus = DissolveTerminus::None;
        for term_window_id in spawned {
            terminus = terminus.or(self.terminate_window(model, selection, session_id, &term_window_id).terminus);
        }
        terminus
    }

    // MARK: - Termination (pure model + synthetic seams; unit-tested)

    /// SIGHUP→SIGKILL the named window and drop its pty, driving the model removal
    /// through [`window_exited`](Self::window_exited) — the Rust twin of
    /// `TabPtySession.terminatePane` (`TabPtySession.swift:680-715`). Three fast
    /// paths mirror Swift, in order:
    ///
    /// * **Synthetic held** — fires `window_exited` synchronously (the production
    ///   held-window fast path); the marker is consumed (one-shot).
    /// * **Synthetic armed-but-not-fired** — same, for a captured deferred spawn
    ///   that never forked (nil-status synthesized exit).
    /// * **Live/held real session** — `window_exited`'s step-3 drop tears the child
    ///   group down and unconditionally removes the model window. This is the
    ///   "intentional-terminate flag set **before** the pid guard" contract:
    ///   the window always drops (never holds), even if its child never got a pid.
    ///
    /// A **model-only** window (no session, no synthetic marker) is a no-op —
    /// matching Swift's `guard var entry = entries[id] else { return }`;
    /// [`close_session`](Self::close_session) removes those from the model up front.
    /// Returns the [`WindowExitResolution`] of the synthesized exit (so a
    /// single-window close can spawn a refocused companion / actuate the terminus).
    pub(crate) fn terminate_window(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        session_id: &str,
        term_window_id: &str,
    ) -> WindowExitResolution {
        let key = synthetic_key(session_id, term_window_id);
        if self.synthetic_held.remove(&key) {
            self.synthetic_spawned.remove(&key);
            return self.window_exited(model, selection, session_id, term_window_id);
        }
        if self.synthetic_armed.remove(&key) {
            self.synthetic_spawned.remove(&key);
            return self.window_exited(model, selection, session_id, term_window_id);
        }
        if self.has_window(session_id, term_window_id) {
            return self.window_exited(model, selection, session_id, term_window_id);
        }
        WindowExitResolution::default()
    }

    /// Tear down every live window on `session_id` (Swift's `SessionsModel.terminateAll`
    /// → `TabPtySession.terminateAll`, `:838-854`). **Snapshots the window ids up
    /// front** because each [`terminate_window`](Self::terminate_window) → held
    /// `window_exited` mutates the cache and the tree mid-loop (synthesized exits
    /// re-enter removal); a live iterator would skip or double-visit an entry.
    /// Returns the aggregate [`DissolveTerminus`].
    pub(crate) fn terminate_all(
        &mut self,
        model: &mut WorkspaceModel,
        selection: &mut SidebarSessionSelection,
        session_id: &str,
    ) -> DissolveTerminus {
        // Snapshot: every live-session window id for this session, plus any synthetic
        // marker (held/armed windows have no `self.sessions` entry).
        let mut ids: Vec<String> = self
            .sessions
            .get(session_id)
            .map(|windows| windows.keys().cloned().collect())
            .unwrap_or_default();
        let prefix = format!("{session_id}:");
        for key in &self.synthetic_spawned {
            if let Some(term_window_id) = key.strip_prefix(&prefix) {
                let term_window_id = term_window_id.to_string();
                if !ids.contains(&term_window_id) {
                    ids.push(term_window_id);
                }
            }
        }

        let mut terminus = DissolveTerminus::None;
        for term_window_id in ids {
            terminus = terminus.or(self.terminate_window(model, selection, session_id, &term_window_id).terminus);
        }
        terminus
    }

    /// Whether `(session_id, term_window_id)` counts as spawned for close routing — a real
    /// live session **or** a synthetic marker (Swift's `paneIsSpawned`). Drives
    /// [`close_session`](Self::close_session)'s spawned/unspawned split.
    pub(crate) fn window_is_spawned(&self, session_id: &str, term_window_id: &str) -> bool {
        self.synthetic_spawned
            .contains(&synthetic_key(session_id, term_window_id))
            || self.has_window(session_id, term_window_id)
    }

    /// Test seam: mark `(session_id, term_window_id)` as a **held** window without a real pty —
    /// [`window_is_spawned`](Self::window_is_spawned) then returns `true` and
    /// [`terminate_window`](Self::terminate_window) fires `window_exited` synchronously,
    /// letting close-flow tests build the held tri-state shape without racing a
    /// real child (Swift's `markSyntheticHeldPaneForTesting`).
    pub(crate) fn mark_synthetic_held_window(&mut self, session_id: &str, term_window_id: &str) {
        let key = synthetic_key(session_id, term_window_id);
        self.synthetic_spawned.insert(key.clone());
        self.synthetic_held.insert(key);
    }

    /// Test seam: mark `(session_id, term_window_id)` as an **armed-but-not-fired** deferred
    /// spawn (a resume-deferred Claude window whose view captured a spawn that never
    /// forked) — [`window_is_spawned`](Self::window_is_spawned) returns `true` and
    /// [`terminate_window`](Self::terminate_window) fires the nil-status `window_exited`
    /// synchronously (Swift's `markSyntheticArmedDeferredPaneForTesting`).
    pub(crate) fn mark_synthetic_armed_deferred_window(&mut self, session_id: &str, term_window_id: &str) {
        let key = synthetic_key(session_id, term_window_id);
        self.synthetic_spawned.insert(key.clone());
        self.synthetic_armed.insert(key);
    }

    /// Test seam: turn the event-driven drain wake off for every pty this
    /// manager spawns from now on — see [`event_wakes_enabled`](Self::event_wakes_enabled).
    /// A `#[gpui::test]` that spawns a window AND runs the executor must call this
    /// first, or the window's exit-watcher thread eventually wakes the parked drain
    /// task cross-thread and trips gpui's determinism guard.
    #[cfg(test)]
    pub(crate) fn set_event_wakes_enabled_for_test(&mut self, enabled: bool) {
        self.event_wakes_enabled = enabled;
    }

    /// R20.5 test seam: mark `(session_id, term_window_id)` as a terminal window whose shell
    /// has a **foreground child** — [`shell_has_foreground_child`](Self::shell_has_foreground_child)
    /// then reports it busy WITHOUT a real pty running a real command, so the
    /// busy-close → confirmation-modal wiring is unit-testable off the live
    /// `tcgetpgrp` syscall (covered once by the live scenario). Mirrors
    /// [`mark_synthetic_held_window`](Self::mark_synthetic_held_window).
    pub(crate) fn mark_synthetic_foreground_child(&mut self, session_id: &str, term_window_id: &str) {
        self.synthetic_foreground_child
            .insert(synthetic_key(session_id, term_window_id));
    }

    /// Whether `(session_id, term_window_id)`'s shell has a foreground child — R20.5's
    /// terminal-busy signal (a `TermWindowKind::Terminal` window is busy iff this is
    /// `true`). Consults the synthetic seam FIRST (a
    /// [`mark_synthetic_foreground_child`](Self::mark_synthetic_foreground_child)
    /// marker ⇒ `true`, so the busy→modal wiring is unit-testable without a real
    /// child), else reads the real session handle's
    /// [`has_foreground_child`](nice_term_view::TerminalSessionHandle::has_foreground_child)
    /// (which runs the `tcgetpgrp` probe inside `nice-term-core`; only a `bool`
    /// crosses the boundary). A **model-only / absent** window — no cached session
    /// and no synthetic marker — is NOT busy (`false`; mirrors Swift's
    /// `guard let entry = entries[id] else { return false }` — a lazy companion
    /// terminal never focused is idle, not busy).
    pub(crate) fn shell_has_foreground_child(
        &self,
        session_id: &str,
        term_window_id: &str,
        cx: &App,
    ) -> bool {
        match self.synthetic_or_absent_foreground_child(session_id, term_window_id) {
            Some(answer) => answer,
            // A real session is cached and no synthetic override applies — read
            // the true fd predicate off its handle.
            None => self
                .term_window_handle(session_id, term_window_id)
                .map(|handle| handle.read(cx).has_foreground_child())
                .unwrap_or(false),
        }
    }

    /// The synthetic-seam / absent-window answer for
    /// [`shell_has_foreground_child`](Self::shell_has_foreground_child), or `None`
    /// when a real handle must be read. Pure (no `cx`), so the seam-first and
    /// model-only-`false` paths are unit-testable without a gpui context (the
    /// `nice` crate links no gpui test-support):
    /// - a synthetic-foreground-child marker ⇒ `Some(true)`;
    /// - no live session cached for this window ⇒ `Some(false)` (model-only/absent);
    /// - otherwise ⇒ `None` (a real handle exists; read its fd predicate).
    fn synthetic_or_absent_foreground_child(&self, session_id: &str, term_window_id: &str) -> Option<bool> {
        if self
            .synthetic_foreground_child
            .contains(&synthetic_key(session_id, term_window_id))
        {
            return Some(true);
        }
        if !self.has_window(session_id, term_window_id) {
            return Some(false);
        }
        None
    }

    /// Actuate a [`DissolveTerminus`] via R12's registry (the gpui side of the
    /// every-project-empty terminus — live-wired slice 3): close this window when
    /// another live window remains, else quit the app. A no-op for
    /// [`DissolveTerminus::None`]. Mirrors `AppState.finalizeDissolvedTab:359-372`.
    ///
    /// LEASE CONTRACT: `remove_window()` drives gpui's window-removal trail, which
    /// synchronously fires the `on_window_closed` observer →
    /// [`WindowRegistry::route_close_disk_fate`] → `state.update(.., teardown)` on
    /// the closing window's [`WindowState`]. So this is safe to call while a
    /// `WindowState` is being updated ONLY when `window` IS that same window (the
    /// trail runs at the outermost `update_window_id` unwind, after the lease
    /// releases) — e.g. the UI-close action handlers. It is NOT safe to call from
    /// inside a `WindowState` entity lease via a *nested* `handle.update` on that
    /// same window (a `cx.subscribe` callback): the teardown would re-enter the
    /// still-leased entity and abort. Such callers MUST `cx.defer` this out of the
    /// lease first (see [`WindowState::subscribe_spawned_windows`]).
    ///
    /// This actuator does NOT touch the closing window's [`WindowState`] (that
    /// would re-lease it on the UI-close paths that call this mid-update). The
    /// disk-fate intent — dropping an emptied window's slot so it doesn't restore
    /// as a broken empty window — is instead set at the terminus MINT sites, on
    /// the already-held `&mut WindowState`, via
    /// [`WindowState::mark_removed_if_window_emptied`].
    pub(crate) fn apply_dissolve_terminus(
        terminus: DissolveTerminus,
        window: &mut Window,
        cx: &mut App,
    ) {
        if terminus == DissolveTerminus::WindowEmptied {
            if WindowRegistry::count(cx) > 1 {
                window.remove_window();
            } else {
                cx.quit();
            }
        }
    }

    // MARK: - Session spawn / focus primitives (gpui; live-wired slice 3)

    /// Whether `session_id` has a session container (Swift's `ptySessions[tabId]`).
    fn session_has_pty(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// Whether `(session_id, term_window_id)` currently has a live window session (Swift's
    /// `session.hasPane`).
    pub(crate) fn has_window(&self, session_id: &str, term_window_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(|windows| windows.contains_key(term_window_id))
    }

    /// The live session entity for `(session_id, term_window_id)`, if one is cached — the
    /// **slice-3 subscription seam**. The live wiring clones this out to
    /// `cx.subscribe` the window's [`crate::window_state::WindowState`] to the
    /// window's OSC / exit events (feeding them through
    /// [`route_terminal_event`](Self::route_terminal_event)), to read its grid for
    /// a readiness poll, and to write input. Cloning an [`Entity`] is a cheap
    /// refcount bump that does **not** keep the session alive past the manager's
    /// own release — a transient clone dropped after subscribing leaves the manager
    /// the sole owner, so a later [`window_exited`](Self::window_exited) /
    /// [`teardown`](Self::teardown) still tears the child process group down.
    pub(crate) fn term_window_handle(
        &self,
        session_id: &str,
        term_window_id: &str,
    ) -> Option<Entity<TerminalSessionHandle>> {
        self.sessions
            .get(session_id)
            .and_then(|windows| windows.get(term_window_id))
            .map(|session| session.handle.clone())
    }

    /// Every `(session_id, term_window_id)` with a live window session right now — the
    /// enumeration the shipped window's subscribe-once sweep
    /// ([`crate::window_state::WindowState::subscribe_spawned_windows`]) walks to
    /// wire each freshly-spawned window's entity to [`route_terminal_event`](Self::route_terminal_event).
    /// Order is unspecified (a `HashMap` walk); the sweep dedupes by key, so
    /// order does not matter.
    pub(crate) fn live_window_keys(&self) -> Vec<(String, String)> {
        self.sessions
            .iter()
            .flat_map(|(session_id, windows)| {
                windows
                    .keys()
                    .map(move |term_window_id| (session_id.clone(), term_window_id.clone()))
            })
            .collect()
    }

    /// Register an **empty** per-session session container without spawning any window.
    /// On the claude-session creation path it runs just before the eager Claude spawn
    /// (`create_claude_session` calls `spawn_claude_window` immediately — claude-kind
    /// windows never lazy-spawn) while the companion terminal stays deferred.
    /// It exists so [`ensure_active_window_spawned`](Self::ensure_active_window_spawned)'s
    /// "the session already has a session" precondition holds when the user first
    /// focuses the deferred companion. Idempotent.
    pub(crate) fn register_session_pty(&mut self, session_id: &str) {
        self.sessions.entry(session_id.to_string()).or_default();
    }

    /// Set this window's shell-injection env (Swift `SessionsModel.bootstrapSocket`).
    /// Called once at window construction, BEFORE the Main window forks, so every
    /// pty spawned through [`spawn_window`](Self::spawn_window) inherits `NICE_SOCKET`
    /// / `ZDOTDIR` / `NICE_USER_ZDOTDIR` from launch (the "env before fork"
    /// invariant the shell's `claude()` shadow depends on).
    pub(crate) fn set_window_shell_env(&mut self, env: WindowShellEnv) {
        self.window_shell_env = Some(env);
    }

    /// The per-window terminal env pairs this window injects into every pty
    /// (Swift `TabPtySession.addTerminalPane`'s `extraEnv`): `NICE_SOCKET` +
    /// `ZDOTDIR` (each only when set) + `NICE_USER_ZDOTDIR` (ALWAYS, empty string
    /// when Nice inherited none — the empty/absent distinction is semantic for the
    /// `.zshenv` stub) + this window's `NICE_TAB_ID` / `NICE_PANE_ID` (the handshake
    /// identity the `claude()` shadow includes in its socket payload). Empty when
    /// the window bootstrapped no socket. Pure — no `cx`, so the env matrix is
    /// unit-tested directly (Validation §3 b/c).
    fn session_window_env_pairs(&self, session_id: &str, term_window_id: &str) -> Vec<(String, String)> {
        let Some(env) = &self.window_shell_env else {
            return Vec::new();
        };
        let mut pairs = Vec::new();
        if let Some(sp) = &env.socket_path {
            pairs.push(("NICE_SOCKET".to_string(), sp.clone()));
        }
        if let Some(zp) = &env.zdotdir {
            pairs.push(("ZDOTDIR".to_string(), zp.clone()));
        }
        pairs.push((
            "NICE_USER_ZDOTDIR".to_string(),
            env.user_zdotdir.clone().unwrap_or_default(),
        ));
        if let Some(conf) = &env.compose_conf {
            pairs.push(("NICE_COMPOSE_CONF".to_string(), conf.clone()));
        }
        pairs.push(("NICE_TAB_ID".to_string(), session_id.to_string()));
        pairs.push(("NICE_PANE_ID".to_string(), term_window_id.to_string()));
        pairs
    }

    /// Spawn a live terminal session for `(session_id, term_window_id)` from `spec` and
    /// cache it with a fresh key-focus handle. Idempotent per `(session, window)`.
    ///
    /// R14: the window's shell-injection env
    /// ([`session_window_env_pairs`](Self::session_window_env_pairs) — `NICE_SOCKET` /
    /// `ZDOTDIR` / `NICE_USER_ZDOTDIR` / `NICE_TAB_ID` / `NICE_PANE_ID`) is merged
    /// into `spec.env` **spec-wins** ([`merge_env_spec_wins`]): a key already
    /// present on the caller-built spec (e.g. a deliberately-blanked `ZDOTDIR`)
    /// survives the injection. This is the single choke point every pty spawn
    /// passes through, so it covers the Main window, `ensure_active_window_spawned`,
    /// and every future R15/R18 path for free.
    pub(crate) fn spawn_window(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        mut spec: SpawnSpec,
        cx: &mut App,
    ) -> Result<()> {
        if self.has_window(session_id, term_window_id) {
            return Ok(());
        }
        merge_env_spec_wins(&mut spec.env, self.session_window_env_pairs(session_id, term_window_id));
        self.spawn_session_raw(session_id, term_window_id, spec, cx)
    }

    /// Spawn + cache a live session from `spec` **verbatim** — no window
    /// injection. The Claude spawn path ([`spawn_claude_window`](Self::spawn_claude_window))
    /// uses this because a Claude window's env is fully determined by
    /// [`build_claude_extra_env`] (it deliberately omits `ZDOTDIR` for a
    /// non-deferred window, so it `exec`s claude under the user's own rc — matching
    /// Swift's per-mode env); routing it through [`spawn_window`](Self::spawn_window)'s
    /// blanket injection would re-add `ZDOTDIR`/`NICE_USER_ZDOTDIR` it doesn't
    /// want. Idempotent per `(session, window)`.
    fn spawn_session_raw(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        spec: SpawnSpec,
        cx: &mut App,
    ) -> Result<()> {
        if self.has_window(session_id, term_window_id) {
            return Ok(());
        }
        let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
        // Deterministic-scheduler opt-out (see `event_wakes_enabled`). Set here,
        // before control returns to the executor, so it lands before this
        // pty's drain task first parks and registers the waker its pty
        // threads would otherwise wake cross-thread.
        #[cfg(test)]
        if !self.event_wakes_enabled {
            handle.read(cx).set_event_wake_enabled(false);
        }
        self.sessions
            .entry(session_id.to_string())
            .or_default()
            .insert(term_window_id.to_string(), WindowPty { handle });
        Ok(())
    }

    /// The ONE shared Claude-session constructor — the Rust twin of Swift's
    /// near-duplicate `createTabFromMainTerminal` (socket newtab path) /
    /// `createClaudeTabInProject` (sidebar project-`+` path)
    /// (`SessionsModel.swift:650-714, :758-794`). Builds the `[Claude, Terminal 1]`
    /// shape (Claude focused), places it, selects it, registers the session's pty container,
    /// and spawns the Claude window from `spawn_cwd` (claude resolves/creates its own
    /// `-w` worktree) — the companion terminal stays **deferred** (model-only until
    /// first focus, per Swift `makeSession(initialTerminalPaneId: nil)`).
    ///
    /// The Claude window is created with `is_claude_running = true` from day one (the
    /// PROTECTED creation invariant: it gates the ≤1-Claude promotion refusal, the
    /// OSC title/status pulse, and auto-titles). `spec` says which claude session
    /// the window runs: [`ClaudeSessionSpec::mint`] (the caller's default) pre-mints
    /// a real v4 UUID so `--session-id` is passed now and the same id persists for
    /// later `--resume`; a request whose args already NAME a claude session hands
    /// one in instead, because splicing `--session-id` beside `--resume`/`attach`
    /// is an argv Claude Code refuses to run.
    ///
    /// `settings_path` is the injectable theme-sync provider's output (R17 fills it;
    /// `None` until then). Returns the new session id, or `None` for a bad placement (an
    /// unknown / pinned-Terminals project id).
    pub(crate) fn create_claude_session(
        &mut self,
        model: &mut WorkspaceModel,
        placement: ClaudeSessionPlacement,
        args: &[String],
        spec: ClaudeSessionSpec,
        settings_path: Option<&str>,
        cx: &mut App,
    ) -> Option<String> {
        // Placement-specific facts, resolved before we mint anything that would
        // otherwise leak on a bad project id.
        let (title, session_cwd, spawn_cwd, extra_args): (String, String, String, Vec<String>) =
            match &placement {
                ClaudeSessionPlacement::Bucket { cwd } => (
                    // The bucketing anchor (`project_path`) stays `cwd`; the session cwd
                    // follows the `-w` worktree in.
                    claude_session_title_from_args(args),
                    claude_worktree_cwd(cwd, args),
                    cwd.clone(),
                    args.to_vec(),
                ),
                ClaudeSessionPlacement::Project { project_id } => {
                    // The pinned Terminals group only holds terminal sessions.
                    if project_id == WorkspaceModel::TERMINALS_PROJECT_ID {
                        return None;
                    }
                    let pi = model.projects.iter().position(|p| &p.id == project_id)?;
                    let path = model.projects[pi].path.clone();
                    ("New session".to_string(), path.clone(), path, Vec::new())
                }
            };

        let session_id = self.mint("t");
        let claude_window_id = format!("{session_id}-claude");
        let terminal_window_id = format!("{session_id}-t1");

        // The Claude window is `is_claude_running = true` at creation (PROTECTED).
        let mut claude_window = TermWindow::new(&claude_window_id, "Claude", TermWindowKind::Claude);
        claude_window.is_claude_running = true;
        let mut session = Session::new(&session_id, title, session_cwd);
        session.windows = vec![
            claude_window,
            TermWindow::new(&terminal_window_id, "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(claude_window_id.clone());
        session.claude_session_id = spec.pin.clone();
        session.next_terminal_index = 2;

        match &placement {
            ClaudeSessionPlacement::Bucket { cwd } => model.add_session_to_projects(session, cwd),
            ClaudeSessionPlacement::Project { project_id } => {
                let pi = model.projects.iter().position(|p| &p.id == project_id)?;
                model.projects[pi].sessions.push(session);
            }
        }
        model.select_session(&session_id);
        // The (empty) session container so the deferred companion's later
        // `ensure_active_window_spawned` precondition ("the session has a session") holds.
        self.register_session_pty(&session_id);

        // Spawn the Claude window immediately (claude-kind windows never lazy-spawn).
        let _ = self.spawn_claude_window(
            &session_id,
            &claude_window_id,
            &spawn_cwd,
            &spec.mode,
            &extra_args,
            settings_path,
            cx,
        );
        Some(session_id)
    }

    /// The nested-child Claude-session constructor shared by the `handoff` and
    /// `dispatch` socket handlers — the Rust twin of Swift
    /// `SessionsModel.createHandoffTab` (`SessionsModel.swift:1246-1303`). The two
    /// flavors differ ONLY in the `title` / `extra_args` their handlers build
    /// (`handoff_title` + [`handoff_extra_args`] vs [`dispatch_title`] +
    /// [`dispatch_extra_args`]), so the session-construction half lives here once.
    /// Modelled on [`create_claude_session`](Self::create_claude_session) (the same
    /// `[Claude, Terminal 1]` shape, Claude focused + `is_claude_running = true`
    /// from creation, the deferred companion terminal, a pre-minted v4 session
    /// UUID passed as `--session-id`, `next_terminal_index = 2`, its session
    /// container registered), differing in exactly the D3/D4/D5/D7 ways:
    ///
    /// * **(D7) the new session opens UNSELECTED** — unlike [`create_claude_session`],
    ///   which selects the session it builds, this never calls
    ///   [`WorkspaceModel::select_session`]: a handoff is background continuation prep, not
    ///   a context switch, so the originating session stays active and keyboard focus
    ///   never moves. The session is still immediately VISIBLE (sidebar children have
    ///   no collapse state), and its Claude pty runs unrendered — the session
    ///   entity owns it ([`TerminalSessionHandle::spawn`] in
    ///   [`spawn_session_raw`](Self::spawn_session_raw)), not the view — exactly
    ///   as the pty of a session the user opened and then switched AWAY from keeps
    ///   running while nothing renders it. (Not the restore precedent: a
    ///   restored-but-unopened session has no pty at all — it is modelled unspawned
    ///   and only lazy-spawns a deferred-resume shell on first activation, via
    ///   [`ensure_active_window_spawned`](Self::ensure_active_window_spawned)'s
    ///   [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred) arm. A handoff session
    ///   is the first construct whose Claude pty runs before ANY view has been
    ///   attached.) The companion terminal stays deferred until first
    ///   activation, as before.
    /// * **(D4) the title is fixed AND locked** — set to `title` up front with
    ///   `title_manually_set = true`, so Claude's OSC auto-title cannot overwrite
    ///   the `[HANDOFF] …` / `[DISPATCH] …` label once the fresh session names
    ///   itself (unlike an ordinary claude session, which keeps auto-title).
    /// * **(D3) placement nests one indent under the originating session** — via
    ///   [`WorkspaceModel::insert_handoff_child`] (depth-1 lineage, the invariant
    ///   `/branch` uses), falling back to [`WorkspaceModel::add_session_to_projects`] (cwd
    ///   bucketing) when the anchor is empty / unknown / in the Terminals group,
    ///   so a top-level handoff (Main Terminal, or a stale id) still opens.
    /// * **(D5) `extra_args` are emitted verbatim after `--session-id`** — each
    ///   flavor's builder already ends them with the seeded prompt as the FINAL
    ///   positional arg, so the launch line becomes
    ///   `claude --session-id <id> <extra args…> '<prompt>'`, which auto-runs the
    ///   prompt. This constructor never reorders or appends to them.
    ///
    /// `settings_path` threads the R17 theme-sync pointer exactly as
    /// [`create_claude_session`] does. Returns the new session id (placement here never
    /// fails — the fallback always buckets — but the signature mirrors
    /// [`create_claude_session`] for symmetry).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_nested_claude_session(
        &mut self,
        model: &mut WorkspaceModel,
        under_session_id: &str,
        cwd: &str,
        title: String,
        extra_args: &[String],
        settings_path: Option<&str>,
        cx: &mut App,
    ) -> Option<String> {
        let session_id = self.mint("t");
        let claude_window_id = format!("{session_id}-claude");
        let terminal_window_id = format!("{session_id}-t1");
        // Pre-mint the session UUID so `--session-id` is passed now and the same
        // id persists for later `--resume` (create_claude_session parity).
        let claude_session_id = mint_session_uuid();

        let mut claude_window = TermWindow::new(&claude_window_id, "Claude", TermWindowKind::Claude);
        claude_window.is_claude_running = true;
        let mut session = Session::new(&session_id, title, cwd);
        session.windows = vec![
            claude_window,
            TermWindow::new(&terminal_window_id, "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(claude_window_id.clone());
        session.claude_session_id = Some(claude_session_id.clone());
        // (D4) Lock the "[HANDOFF] …" / "[DISPATCH] …" title against Claude's OSC
        // auto-title.
        session.title_manually_set = true;
        session.next_terminal_index = 2;

        // (D3) Nest under the originating session; else bucket at top level so a
        // Main-Terminal (or stale-id) request still opens. `insert_handoff_child`
        // consumes `session` on the success path, so clone for the attempt and hand
        // the original to the bucketing fallback (Swift passes a value type twice).
        if !model.insert_handoff_child(session.clone(), under_session_id) {
            model.add_session_to_projects(session, cwd);
        }
        // (D7) Deliberately NO `model.select_session(&session_id)`: the nested session opens
        // in the background so the originating session keeps selection + key focus.
        // The (empty) session container so the deferred companion's later
        // `ensure_active_window_spawned` precondition ("the session has a session") holds.
        self.register_session_pty(&session_id);

        // (D5) The caller's args verbatim — already prompt-last.
        let _ = self.spawn_claude_window(
            &session_id,
            &claude_window_id,
            cwd,
            &ClaudeSessionMode::New(claude_session_id),
            extra_args,
            settings_path,
            cx,
        );
        Some(session_id)
    }

    /// Spawn a **Claude-kind** window's child — the Rust twin of Swift
    /// `TabPtySession.spawnClaudePane` (`TabPtySession.swift:275-340`). The spec is
    /// mode-driven:
    ///
    /// * [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred) → a plain login shell
    ///   (`zsh -il`) carrying `NICE_PREFILL_COMMAND` (the injected zshrc pre-types
    ///   `claude --resume <id>`); the launch overlay is suppressed (a quiescent
    ///   prefilled shell isn't "launching").
    /// * Probe resolved a `claude` binary → `zsh -ilc "exec <claude> …"` via
    ///   [`build_claude_exec_command`], env from [`build_claude_extra_env`].
    /// * Probe unresolved → a plain `zsh -il` with **no** Nice env (Swift's
    ///   `environment: nil` fallback: the window renders as Claude but is really a
    ///   shell). No retro-upgrade when the probe later resolves.
    ///
    /// The env comes wholly from [`build_claude_extra_env`] (which reads this
    /// window's socket / zdotdir facts) and the spawn bypasses the blanket window
    /// injection ([`spawn_session_raw`](Self::spawn_session_raw)) — see that method.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_claude_window(
        &mut self,
        session_id: &str,
        term_window_id: &str,
        cwd: &str,
        mode: &ClaudeSessionMode,
        extra_args: &[String],
        settings_path: Option<&str>,
        cx: &mut App,
    ) -> Result<()> {
        // Window shell-injection facts (None on a manager that never armed a socket).
        let (socket_path, zdotdir, user_zdotdir) = match &self.window_shell_env {
            Some(env) => (
                env.socket_path.clone(),
                env.zdotdir.clone(),
                env.user_zdotdir.clone(),
            ),
            None => (None, None, None),
        };
        // `NICE_CLAUDE_OVERRIDE` in the env means the wrapper owns the full argv —
        // suppress every Nice-injected flag (re-read here, the test seam).
        let is_override = std::env::var("NICE_CLAUDE_OVERRIDE")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        let claude = resolve_claude_binary(cx);

        let spec = if matches!(mode, ClaudeSessionMode::ResumeDeferred(_)) {
            let env = build_claude_extra_env(
                mode,
                session_id,
                term_window_id,
                socket_path.as_deref(),
                zdotdir.as_deref(),
                user_zdotdir.as_deref(),
                settings_path.map(str::to_string),
            );
            SpawnSpec::shell(cwd).with_env(env)
        } else if let Some(claude) = claude.as_deref() {
            let env = build_claude_extra_env(
                mode,
                session_id,
                term_window_id,
                socket_path.as_deref(),
                zdotdir.as_deref(),
                user_zdotdir.as_deref(),
                settings_path.map(str::to_string),
            );
            let exec_line =
                build_claude_exec_command(claude, mode, extra_args, is_override, settings_path);
            // `SpawnSpec::command` wraps its arg as `zsh -ilc "exec <cmd>"`; the
            // composer already emits `exec <claude> …`, so hand it the post-`exec`
            // remainder (the composer always prefixes `exec `, so the strip is total).
            let command = exec_line
                .strip_prefix("exec ")
                .unwrap_or(&exec_line)
                .to_string();
            SpawnSpec::command(command, cwd).with_env(env)
        } else {
            // Probe unresolved: plain shell, no Nice env (Swift `environment: nil`).
            SpawnSpec::shell(cwd)
        };

        self.spawn_session_raw(session_id, term_window_id, spec, cx)?;

        // Launch-overlay policy: register the user-facing command string; a
        // deferred-resume window suppresses it (Swift `installLaunchOverlayHooks`'s
        // early return for `.resumeDeferred`). The live window root clears it on
        // first output / exit via the routed events (the subscription lift).
        if !matches!(mode, ClaudeSessionMode::ResumeDeferred(_)) {
            let _ = self.register_window_launch(term_window_id, claude_launch_display_command(mode, extra_args));
        }
        Ok(())
    }

    /// Spawn the active window's deferred pty if it was modelled up front — Swift's
    /// `ensureActivePaneSpawned` (`SessionsModel.swift:553-565`), extended for R18
    /// restore. Two lazy-spawn arms, both gated on the session having a session
    /// container and the pty not being live yet:
    ///
    /// * a **terminal-kind** active window spawns a plain login shell in its
    ///   resolved cwd (last OSC 7, else the session/project fallback) — unchanged;
    /// * a **claude-kind** active window lazy-spawns **only in resume-deferred
    ///   form** (L3): iff the session carries a `claude_session_id`, the window is not
    ///   yet spawned, and no Claude is running, it spawns a plain login shell
    ///   carrying `claude --resume <sid>` as `NICE_PREFILL_COMMAND` (nothing runs
    ///   until the user opens the session and presses Enter). This **supersedes** R15's
    ///   "claude never lazy-spawns" note: a *restored* Claude window returns modelled
    ///   but unspawned and must lazy-spawn its deferred-resume shell on first
    ///   activation. A *running* Claude window (already spawned, or one promoted in
    ///   place) still never lazy-spawns — the `is_claude_running` / already-spawned
    ///   guards below reject it.
    ///
    /// Never creates a session container itself. `settings_path` is R17's theme
    /// `--settings` pointer (threaded from the window's provider), spliced into the
    /// deferred-resume prefill; `None` ⇒ no `--settings` (sync off / gate unset).
    pub(crate) fn ensure_active_window_spawned(
        &mut self,
        model: &WorkspaceModel,
        session_id: &str,
        settings_path: Option<&str>,
        cx: &mut App,
    ) {
        let Some(session) = model.session_for(session_id) else {
            return;
        };
        let Some(term_window_id) = session.active_window_id.clone() else {
            return;
        };
        let Some(term_window) = session.windows.iter().find(|p| p.id == term_window_id) else {
            return;
        };
        if !self.session_has_pty(session_id) || self.has_window(session_id, &term_window_id) {
            return;
        }
        // L3 restore arm: a claude-kind active window lazy-spawns its deferred-resume
        // shell (never a running claude). A running-claude or session-less window is
        // left to its eager/socket spawn path.
        if term_window.kind == TermWindowKind::Claude {
            if term_window.is_claude_running {
                return;
            }
            let Some(sid) = session.claude_session_id.clone() else {
                return;
            };
            let cwd = model.resolved_spawn_cwd_for_window(session, term_window);
            let _ = self.spawn_claude_window(
                session_id,
                &term_window_id,
                &cwd,
                &ClaudeSessionMode::ResumeDeferred(sid),
                &[],
                settings_path,
                cx,
            );
            return;
        }
        if term_window.kind != TermWindowKind::Terminal {
            return;
        }
        let cwd = model.resolved_spawn_cwd_for_window(session, term_window);
        // R14: the extra-env hook threads NICE_SOCKET/NICE_TAB_ID/NICE_PANE_ID
        // onto this spec before spawn.
        let spec = SpawnSpec::shell(cwd);
        let _ = self.spawn_window(session_id, &term_window_id, spec, cx);
    }

    /// The **full** Swift `setActivePane` behavior (`SessionsModel.swift:534-546`)
    /// — the live composition the slice-3 action seams call: the model half
    /// ([`set_active_window`](Self::set_active_window), which acknowledges a waiting
    /// window on the viewed session) plus [`ensure_active_window_spawned`](Self::ensure_active_window_spawned)
    /// (a deferred terminal companion spawns on first focus). The navigation
    /// steppers compose the same pieces in the live app so the ack + spawn ride
    /// along; the pure `set_active_window` / `select_next_window` methods are its
    /// unit-testable model half.
    ///
    /// Key focus is NOT moved here — the window host
    /// ([`crate::app_shell::WindowHostView`]) owns the terminal views and focuses
    /// the newly-active one on the same activation render (right after it fills
    /// the view cache), so the manager doesn't need a view handle it doesn't own.
    pub(crate) fn activate_term_window(
        &mut self,
        model: &mut WorkspaceModel,
        session_id: &str,
        term_window_id: &str,
        settings_path: Option<&str>,
        cx: &mut App,
    ) {
        self.set_active_window(model, session_id, term_window_id);
        self.ensure_active_window_spawned(model, session_id, settings_path, cx);
    }

    /// Tear down every session this window owns. Dropping each
    /// [`TerminalSessionHandle`] tears its child process group down
    /// (SIGHUP→SIGKILL via `nice_term_core::Session::drop`), so no orphan zsh
    /// survives (the R3 teardown contract). Idempotent — the window-close hook
    /// calls it once, but app-terminate paths may double up. R18 extends this to
    /// flush the session snapshot first.
    pub(crate) fn teardown(&mut self) {
        self.sessions.clear();
        self.window_launch_states.clear();
        self.synthetic_spawned.clear();
        self.synthetic_held.clear();
        self.synthetic_armed.clear();
        self.synthetic_foreground_child.clear();
    }
}

impl Default for PtyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// The `<session>:<window>` key the synthetic spawned/held/armed sets index by, matching
/// Swift's `SessionsModel.syntheticPaneKey`.
fn synthetic_key(session_id: &str, term_window_id: &str) -> String {
    format!("{session_id}:{term_window_id}")
}

/// Clip a window title to `max` **characters** (not bytes), trimming any trailing
/// whitespace the cut exposed — `SessionsModel.swift:400-404`
/// (`trimmingCharacters(in: .whitespaces)` after the 40-char cut). The input is
/// already outer-trimmed, so only a trailing space from a mid-word cut matters.
fn clip_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let clipped: String = title.chars().take(max).collect();
    clipped.trim().to_string()
}

/// Production id minter: `<prefix><ms>-<suffix>` (e.g. `t1751234567890-0002`).
/// Dependency-free (no `uuid` crate, matching `nice-model`'s minting): the
/// millisecond keeps ids roughly time-sortable for log triage, and the four-hex
/// suffix carries the low bits of a process-global monotonic counter so two
/// mints in the same millisecond can't collide — the collision Swift's UUID
/// suffix closes (`SessionsModel.swift:175-179`), here made exact rather than
/// probabilistic (distinct counter ⇒ distinct `(ms, suffix)` at human creation
/// rates; the 16-bit space only wraps after 65536 mints inside one ms).
pub(crate) fn default_mint_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{ms}-{:04x}", c & 0xffff)
}

/// Mint a fresh lowercased UUIDv4 session id (Swift's
/// `UUID().uuidString.lowercased()` at `SessionsModel.swift:664, :866`). This is
/// a SEPARATE mint from [`default_mint_id`]: session/window ids stay the time-sortable
/// ms+counter form, but a Claude session id is handed to the `claude` CLI as
/// `--session-id`/`--resume` and must be a real v4 UUID (the CLI validates the
/// shape), so it needs 122 bits of real entropy with the version/variant bits
/// set — not a counter.
///
/// Hand-rolled over `getentropy` rather than pulling the `uuid` crate: the
/// workspace is deliberately dependency-frugal and `libc` is already a dep
/// (matching `nice-model`'s no-`uuid` minting note). 16 random bytes, then
/// RFC 4122 §4.4: byte 6 high nibble → `0100` (version 4), byte 8 top two bits
/// → `10` (variant 1). Rendered lowercase `8-4-4-4-12`.
pub(crate) fn mint_session_uuid() -> String {
    let mut b = random_16_bytes();
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 1 (RFC 4122)
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

/// 16 cryptographically-random bytes via `getentropy(2)` (macOS, buflen ≤ 256 so
/// a single call always suffices). On the near-impossible failure path
/// (`getentropy` only fails on `EFAULT`/`EIO`, neither reachable with a valid
/// 16-byte stack buffer) fall back to a time+counter+address mix so minting
/// stays infallible like Swift's `UUID()`; the version/variant bits are set by
/// the caller regardless, so the UUID shape is always valid even in the
/// degraded case.
fn random_16_bytes() -> [u8; 16] {
    let mut buf = [0u8; 16];
    // SAFETY: `buf` is a live 16-byte stack buffer; `getentropy` writes exactly
    // `buflen` bytes into it and reads nothing. 16 ≤ 256 (the `GETENTROPY_MAX`),
    // so it never short-fills.
    let rc = unsafe { libc::getentropy(buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if rc == 0 {
        return buf;
    }
    // Degraded fallback — see the doc comment. Never expected to run.
    static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let c = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mix = nanos ^ (c.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ (&buf as *const _ as u64);
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = (mix >> ((i % 8) * 8)) as u8 ^ (c >> (i % 8)) as u8;
    }
    buf
}

/// Merge `injected` env pairs into `spec_env` **spec-wins**: a key already
/// present on the caller-built spec keeps its value; only keys absent from the
/// spec are appended. The inverse of `nice_term_core::build_env`'s caller-wins
/// upsert direction (there the caller wins over the base; here the spec — the
/// caller — wins over the manager injection). Load-bearing: ~10 landed scenarios
/// / itests spawn shells with `ZDOTDIR` deliberately blanked via
/// `SpawnSpec::with_env`; blanket injection would clobber that. Order is stable
/// (spec pairs first, then the new injected keys in matrix order).
fn merge_env_spec_wins(spec_env: &mut Vec<(String, String)>, injected: Vec<(String, String)>) {
    for (k, v) in injected {
        if !spec_env.iter().any(|(ek, _)| *ek == k) {
            spec_env.push((k, v));
        }
    }
}

/// How a Claude window attaches to the Claude CLI's session layer. Ports Swift
/// `TabPtySession.ClaudeSessionMode` (`TabPtySession.swift:180-197`). The env
/// matrix ([`build_claude_extra_env`]) branches on the `ResumeDeferred` variant
/// only. R15 owns the decision logic that selects a mode; R14 ports the enum +
/// the pure env matrix so the FROZEN prefill format is pinned now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaudeSessionMode {
    /// No session id; the CLI picks one.
    None,
    /// Fresh session under a caller-provided UUID (`--session-id <uuid>`).
    New(String),
    /// Resume a prior session by UUID (`--resume <uuid>`).
    Resume(String),
    /// Fix D: attach to a background session the Claude daemon still hosts
    /// (`claude attach <short id>`). Carries the SHORT (8-hex) job id, because
    /// that is what `attach` resolves — it prefix-matches `~/.claude/jobs`
    /// directory names, which a full uuid never matches.
    ///
    /// `attach` is a SUBCOMMAND, and the CLI only recognizes a subcommand in
    /// FIRST position: `claude --settings <path> attach <id>` parses as the
    /// default command with `attach <id>` as its PROMPT (verified against
    /// 2.1.223 — it starts a brand-new conversation). So this mode emits no
    /// `--settings` pointer and no `extra_claude_args`.
    Attach(String),
    /// Restore path: don't run claude — spawn a plain `zsh -il` with
    /// `claude --resume <uuid>` pre-typed at the prompt via the stub's
    /// `print -z "$NICE_PREFILL_COMMAND"` tail. This is the only mode that needs
    /// `ZDOTDIR` + `NICE_PREFILL_COMMAND` in the window env.
    ResumeDeferred(String),
}

/// Build the extra-env pairs for a **Claude** window. Pure port of Swift
/// `TabPtySession.buildClaudeExtraEnv` (`TabPtySession.swift:875-902`).
///
/// The per-mode matrix is R14's FROZEN spec (R15 wired this function into the
/// live spawn path and may extend the signature — never the matrix): EVERY mode sets `TERM_PROGRAM`,
/// `NICE_TAB_ID`, `NICE_PANE_ID`, and `NICE_SOCKET` (when a socket exists) so the
/// SessionStart hook can reach Nice; ONLY [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred)
/// adds `ZDOTDIR` (when set), the always-present `NICE_USER_ZDOTDIR` (empty when
/// none), and the `NICE_PREFILL_COMMAND` the stub's `print -z` tail pre-types.
///
/// Was production-unused before R15; R15's [`spawn_claude_window`](PtyManager::spawn_claude_window)
/// now wires it as the live env composer for every Claude window spawn.
/// `settings_path` is threaded now but always `None` until
/// R17 fills R15's theme-sync provider; when `Some`, it splices a single-quoted
/// `--settings <path>` before `--resume` in the prefill line (theme parity for a
/// deferred-resumed session), matching the Swift source byte-for-byte.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_claude_extra_env(
    mode: &ClaudeSessionMode,
    session_id: &str,
    term_window_id: &str,
    socket_path: Option<&str>,
    zdotdir_path: Option<&str>,
    user_zdotdir: Option<&str>,
    settings_path: Option<String>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("TERM_PROGRAM".to_string(), "ghostty".to_string()),
        ("NICE_TAB_ID".to_string(), session_id.to_string()),
        ("NICE_PANE_ID".to_string(), term_window_id.to_string()),
    ];
    if let Some(sp) = socket_path {
        env.push(("NICE_SOCKET".to_string(), sp.to_string()));
    }
    if let ClaudeSessionMode::ResumeDeferred(claude_session_id) = mode {
        if let Some(zp) = zdotdir_path {
            env.push(("ZDOTDIR".to_string(), zp.to_string()));
        }
        // Pair NICE_USER_ZDOTDIR with ZDOTDIR — the .zshenv stub resolves the
        // user's intended layout from it before our injection unwinds. Always set
        // (empty string when Nice inherited none).
        env.push((
            "NICE_USER_ZDOTDIR".to_string(),
            user_zdotdir.unwrap_or("").to_string(),
        ));
        // Pre-type the resume command the user runs with Enter. The prefill
        // string is an R15-owned protocol composer (see
        // [`build_claude_prefill_command`]); the FROZEN format is
        // `claude[ --settings '<path>'] --resume <sid>`.
        env.push((
            "NICE_PREFILL_COMMAND".to_string(),
            build_claude_prefill_command(settings_path.as_deref(), claude_session_id),
        ));
    }
    env
}

/// Build the deferred-resume prefill command the injected zshrc's `print -z`
/// pre-types at the prompt — the FROZEN wire string
/// `claude[ --settings '<path>'] --resume <sid>` (a compatibility contract with
/// the shell helpers already installed on user disks). Pure port of the
/// `NICE_PREFILL_COMMAND` construction in Swift
/// `TabPtySession.buildClaudeExtraEnv` (`TabPtySession.swift:898-899`),
/// extracted as a discrete composer per the R15 "owns ALL protocol/exec
/// composers" decision.
///
/// `settings_path` is the injectable theme-sync provider's output (R17 fills
/// it; `None` until then): when `Some`, a single-quoted `--settings <path>` is
/// spliced BEFORE `--resume` so the deferred-resumed session adopts Nice's
/// theme, matching the exec builder's flag order.
pub(crate) fn build_claude_prefill_command(settings_path: Option<&str>, claude_session_id: &str) -> String {
    let settings_arg = settings_path
        .map(|p| format!(" --settings {}", nice_term_core::shell_single_quote(p)))
        .unwrap_or_default();
    format!("claude{settings_arg} --resume {claude_session_id}")
}

/// Assemble the `exec <claude> …` command line for the inner `zsh -ilc`
/// invocation. Pure port of Swift `TabPtySession.buildClaudeExecCommand`
/// (`TabPtySession.swift:938-970`) — factored out so unit tests lock the flag
/// ordering contract without spawning a pty.
///
/// Flag-order rule (load-bearing): `--settings <path>` (a global flag with its
/// own value) is emitted FIRST, then `--session-id`/`--resume` and their UUID,
/// then `extra_claude_args` — so the UUID is never consumed as the value of a
/// trailing flag. Every splice goes through
/// [`shell_single_quote`](nice_term_core::shell_single_quote).
///
/// - `is_override == true` (set when `NICE_CLAUDE_OVERRIDE` is in the env)
///   suppresses EVERY Nice-injected flag — the wrapper owns the full argv;
///   the result is just `exec '<claude>'`.
/// - [`Resume`](ClaudeSessionMode::Resume) deliberately DROPS `extra_claude_args`
///   (the transcript already carries the session's flags).
/// - [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred) is handled outside
///   this helper (it spawns a plain shell, not `exec claude`); passing it here
///   returns just `exec '<claude>'` defensively.
/// - `settings_path` is the injectable theme-sync provider's output (R17 fills
///   it; `None` until then). It is skipped under `is_override`.
pub(crate) fn build_claude_exec_command(
    claude: &str,
    mode: &ClaudeSessionMode,
    extra_claude_args: &[String],
    is_override: bool,
    settings_path: Option<&str>,
) -> String {
    let mut parts = vec![
        "exec".to_string(),
        nice_term_core::shell_single_quote(claude),
    ];
    if !is_override {
        // Nice-managed theme pointer (`{"theme":"custom:nice"}`) — a global flag
        // with its own value; emit it before the session flags so it never sits
        // between `--session-id`/`--resume` and their UUID. Suppressed for
        // [`Attach`](ClaudeSessionMode::Attach): a global flag BEFORE the
        // `attach` subcommand makes the CLI stop seeing a subcommand at all
        // (the id degrades into a prompt), so attach takes no theme pointer.
        if let (Some(sp), false) = (settings_path, matches!(mode, ClaudeSessionMode::Attach(_))) {
            parts.push("--settings".to_string());
            parts.push(nice_term_core::shell_single_quote(sp));
        }
        match mode {
            ClaudeSessionMode::None => {
                parts.extend(
                    extra_claude_args
                        .iter()
                        .map(|a| nice_term_core::shell_single_quote(a)),
                );
            }
            ClaudeSessionMode::New(id) => {
                parts.push("--session-id".to_string());
                parts.push(nice_term_core::shell_single_quote(id));
                parts.extend(
                    extra_claude_args
                        .iter()
                        .map(|a| nice_term_core::shell_single_quote(a)),
                );
            }
            ClaudeSessionMode::Resume(id) => {
                parts.push("--resume".to_string());
                parts.push(nice_term_core::shell_single_quote(id));
            }
            ClaudeSessionMode::Attach(short_id) => {
                // Subcommand FIRST, and nothing after the id: `attach` takes one
                // positional argument, so `extra_claude_args` is dropped exactly
                // as [`Resume`](ClaudeSessionMode::Resume) drops it.
                parts.push("attach".to_string());
                parts.push(nice_term_core::shell_single_quote(short_id));
            }
            ClaudeSessionMode::ResumeDeferred(_) => {}
        }
    }
    parts.join(" ")
}

/// The socket `claude` handler's newtab/inplace decision, minus the wire
/// formatting. R15 slice-2's handler builds this from the model; the composer
/// ([`compose_claude_reply`]) renders it byte-exact. Ported from the reply
/// tail of Swift `handleClaudeSocketRequest` (`SessionsModel.swift:897-910`).
pub(crate) enum ClaudeReplyDecision {
    /// Open a new sidebar session — reply `newtab`.
    NewSession,
    /// Promote the requesting window in place. `parsed_from_args` is true when the
    /// client's `args` already carried the session id (`--resume`/`--session-id`),
    /// which selects the bare `inplace` / `-` placeholder forms; `claude_session_id` is
    /// the resolved id (parsed, or a freshly minted UUID) the wrapper prepends.
    InPlace {
        parsed_from_args: bool,
        claude_session_id: String,
    },
    /// Fix D: promote in place, but exec `claude attach` instead of what the
    /// user typed — their `--resume <uuid>` names a background session the
    /// Claude daemon STILL hosts, and a second `--resume` would race the live
    /// process. The FULL uuid rides the wire (not the 8-hex short id `attach`
    /// takes): the wrapper derives the short id from it, and the fallback leg
    /// (`attach` failed ⇒ the daemon dropped the job after we probed) needs the
    /// uuid to resume with.
    Attach { claude_session_id: String },
    /// Fix D: promote in place, but exec `claude --resume <uuid>` instead of
    /// what the user typed — they ran `claude attach <id>` for a session the
    /// daemon no longer hosts, which `attach` alone can only fail on.
    Resume { claude_session_id: String },
}

/// Compose the socket `claude` reply — the FROZEN R14 grammar (≤3
/// whitespace-separated positional fields). Pure port of the reply tail of
/// Swift `handleClaudeSocketRequest` (`SessionsModel.swift:897-910`); an
/// R15-owned protocol composer.
///
/// The byte-exact variants:
/// - `newtab`
/// - `inplace` — in-place, args already carried the id, theme sync off
/// - `inplace <uuid>` — in-place, minted id, theme sync off
/// - `inplace <uuid|-> <path>` — theme sync on: the third field is the
///   `--settings` path the wrapper splices; the second is the minted uuid, or
///   `-` when the client's args already named the session.
/// - `attach <uuid> [path]` / `resume <uuid> [path]` — Fix D's exec-time
///   normalization. Same three positional fields (the second is always the FULL
///   uuid, the third the optional `--settings` pointer), so the frozen
///   `read -r mode sid settings` grammar is untouched; only the verb is new.
///   An older wrapper that predates these verbs falls into its `*)` arm and
///   runs the user's original args — the pre-Fix-D behavior, never a crash.
///
/// `settings_path` is the injectable theme-sync provider's output (R17 fills
/// it; `None` until then). With `settings_path == None` the replies are
/// byte-identical to the two shorter forms.
pub(crate) fn compose_claude_reply(
    decision: &ClaudeReplyDecision,
    settings_path: Option<&str>,
) -> String {
    match decision {
        ClaudeReplyDecision::NewSession => "newtab".to_string(),
        ClaudeReplyDecision::InPlace {
            parsed_from_args,
            claude_session_id,
        } => match settings_path {
            Some(path) => {
                // `-` sid placeholder when the client's args already carry the
                // session, so the pointer can follow as the 3rd field; else the
                // freshly minted id.
                let sid_field = if *parsed_from_args {
                    "-"
                } else {
                    claude_session_id.as_str()
                };
                format!("inplace {sid_field} {path}")
            }
            None => {
                if *parsed_from_args {
                    "inplace".to_string()
                } else {
                    format!("inplace {claude_session_id}")
                }
            }
        },
        ClaudeReplyDecision::Attach { claude_session_id } => {
            compose_id_reply("attach", claude_session_id, settings_path)
        }
        ClaudeReplyDecision::Resume { claude_session_id } => {
            compose_id_reply("resume", claude_session_id, settings_path)
        }
    }
}

/// The `<verb> <uuid> [settings]` shape shared by Fix D's two normalizing
/// replies. Unlike `inplace` the id field is never a `-` placeholder: these
/// verbs exist precisely to hand the wrapper an id it did not have.
fn compose_id_reply(verb: &str, session_id: &str, settings_path: Option<&str>) -> String {
    match settings_path {
        Some(path) => format!("{verb} {session_id} {path}"),
        None => format!("{verb} {session_id}"),
    }
}

/// Split a Claude OSC title into its status prefix and the trailing label,
/// per the T5 grammar. Pure port of the status-prefix extraction in Swift
/// `paneTitleChanged`'s Claude branch (`SessionsModel.swift:439-453`): the
/// first Unicode scalar in `U+2800..=U+28FF` (braille spinner) ⇒
/// [`Thinking`](SessionStatus::Thinking); exactly `U+2733` (✳ sparkle) ⇒
/// [`Waiting`](SessionStatus::Waiting); anything else ⇒ no status change and the
/// whole string is the label.
///
/// Returns `(status, label)` where `label` is the input with the status prefix
/// scalar removed (untrimmed — the caller trims, drops the empty / `Claude Code`
/// placeholder, and feeds the rest to `apply_auto_title`; that wiring is R15
/// slice-3's `window_title_changed` branch).
pub(crate) fn parse_claude_title(title: &str) -> (Option<SessionStatus>, &str) {
    let Some(first) = title.chars().next() else {
        return (None, title);
    };
    let cp = first as u32;
    if (0x2800..=0x28FF).contains(&cp) {
        (Some(SessionStatus::Thinking), &title[first.len_utf8()..])
    } else if cp == 0x2733 {
        (Some(SessionStatus::Waiting), &title[first.len_utf8()..])
    } else {
        (None, title)
    }
}

/// WHICH claude session a [`PtyManager::create_claude_session`] spawn runs — the
/// newtab twin of the in-place reply's exec-time decision (Fix D).
///
/// The default ([`mint`](Self::mint)) is the pre-Fix-D behavior: mint a fresh
/// v4 uuid and pass it as `--session-id`, so the session can `--resume` it after
/// a relaunch. That is WRONG when the invocation already names a claude session:
/// Claude Code rejects `--session-id` beside `--resume`/`--continue` outright
/// (`--session-id can only be used with --continue or --resume if
/// --fork-session is also specified`), so the window dies on the spot. Those
/// invocations carry their own mode and pin instead.
pub(crate) struct ClaudeSessionSpec {
    /// How the window's `claude` is exec'd ([`build_claude_exec_command`]).
    pub(crate) mode: ClaudeSessionMode,
    /// The claude session id the new session remembers, or `None` when the
    /// request names a claude session that cannot be resolved to a full uuid (a
    /// short `attach <id>` with no readable jobs entry). Minting one there would
    /// pin a phantom id no later `--resume` could use.
    pub(crate) pin: Option<String>,
}

impl ClaudeSessionSpec {
    /// A fresh claude session: mint a real v4 uuid, pass it as `--session-id`, pin it.
    pub(crate) fn mint() -> Self {
        let id = mint_session_uuid();
        Self {
            mode: ClaudeSessionMode::New(id.clone()),
            pin: Some(id),
        }
    }
}

/// Where [`PtyManager::create_claude_session`] puts the new session — the two Swift
/// call sites' only real divergence (`SessionsModel.swift:650-714, :758-794`).
pub(crate) enum ClaudeSessionPlacement {
    /// The socket `newtab` path (Swift `createTabFromMainTerminal`): bucket the session
    /// by `cwd` via [`WorkspaceModel::add_session_to_projects`] (git-root / longest-prefix),
    /// title from `args`, `-w` worktree split honored.
    Bucket { cwd: String },
    /// The sidebar project-`+` path (Swift `createClaudeTabInProject`): append
    /// directly to `project_id`, title `"New session"`, no worktree split, no extra args.
    Project { project_id: String },
}

/// Process-global resolved absolute path to the `claude` binary — the Rust twin of
/// Swift `SessionsModel.resolvedClaudePath`, delivered by the C11 bootstrap probe
/// (`crate::app`). The Claude spawn path consults it via
/// [`resolve_claude_binary`]. `Some(None)` means the probe ran and found no
/// `claude` (the spawn falls back to a plain shell); absent means the probe hasn't
/// delivered yet (early launch — same "no retro-upgrade" race Swift tolerates).
#[derive(Clone)]
pub(crate) struct ResolvedClaudePath(pub(crate) Option<String>);

impl Global for ResolvedClaudePath {}

/// Resolve the `claude` binary at spawn time (Swift `resolvedClaudePath` read):
/// `NICE_CLAUDE_OVERRIDE` wins **synchronously** — re-read every spawn because it
/// is the test seam pointing "claude" at a stub, and `run_selftest` deliberately
/// skips the bootstrap probe that would otherwise seed the global — else the
/// process-global [`ResolvedClaudePath`] the bootstrap probe set.
fn resolve_claude_binary(cx: &App) -> Option<String> {
    if let Ok(over) = std::env::var("NICE_CLAUDE_OVERRIDE") {
        if !over.is_empty() {
            return Some(over);
        }
    }
    cx.try_global::<ResolvedClaudePath>()
        .and_then(|g| g.0.clone())
}

/// The Claude session's title from its invocation `args` — Swift
/// `createTabFromMainTerminal`'s title closure (`SessionsModel.swift:653-659`):
/// join with spaces, take the first 40 chars, trim; an empty result (no args, or
/// all-whitespace) falls back to `"New session"`. A third, independent 40-char cap
/// (window pills clip at 40 too — [`WINDOW_TITLE_MAX`] — but separately).
fn claude_session_title_from_args(args: &[String]) -> String {
    if args.is_empty() {
        return "New session".to_string();
    }
    let joined = args.join(" ");
    let capped: String = joined.chars().take(40).collect();
    let trimmed = capped.trim();
    if trimmed.is_empty() {
        "New session".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The Claude session's `Session.cwd` — Swift `createTabFromMainTerminal`'s `sessionCwd`
/// (`SessionsModel.swift:675-683`): when the user ran `claude -w <name>`, Claude
/// creates and runs inside a worktree at `<cwd>/.claude/worktrees/<sanitized>`
/// (`/`→`+` via [`WorkspaceModel::sanitize_worktree_name`]); otherwise the session cwd is
/// `cwd`. The bucketing anchor (`project_path`) stays `cwd` regardless, so the
/// sidebar still buckets the session under the parent project. The `-w`/`--worktree`
/// **space form** only is recognized (the extractor is landed in `nice-model`); the
/// `=` form is deliberately NOT a worktree while session-id takes both.
fn claude_worktree_cwd(cwd: &str, args: &[String]) -> String {
    match WorkspaceModel::extract_worktree_name(args) {
        Some(name) => {
            let sanitized = WorkspaceModel::sanitize_worktree_name(&name);
            format!("{}/.claude/worktrees/{}", cwd.trim_end_matches('/'), sanitized)
        }
        None => cwd.to_string(),
    }
}

/// The human-readable command string the launch overlay shows for a fresh Claude
/// window — Swift `TabPtySession.launchDisplayCommand` (`TabPtySession.swift:618-634`):
/// deliberately skips the `zsh -ilc "exec …"` wrapper and the `--session-id <uuid>`
/// plumbing so the user sees what *they* asked for. `.resume` → `claude --resume`;
/// otherwise `claude` (no args) or `claude <user args>`. `.resumeDeferred` is
/// suppressed by the caller, so it never reaches here.
fn claude_launch_display_command(mode: &ClaudeSessionMode, extra_args: &[String]) -> String {
    match mode {
        ClaudeSessionMode::Resume(_) => "claude --resume".to_string(),
        ClaudeSessionMode::Attach(short_id) => format!("claude attach {short_id}"),
        _ => {
            if extra_args.is_empty() {
                "claude".to_string()
            } else {
                format!("claude {}", extra_args.join(" "))
            }
        }
    }
}

/// The prefix on every handoff-session title — Swift `handoffTitlePrefix`
/// (`SessionsModel.swift:1161`). A single existing occurrence is stripped before
/// re-prefixing so a handoff-fired-from-a-handoff reads `[HANDOFF] Foo`, not
/// `[HANDOFF] [HANDOFF] Foo`.
const HANDOFF_TITLE_PREFIX: &str = "[HANDOFF] ";

/// Build the locked `[HANDOFF] …` title for a handoff session from the originating
/// session's current title — pure port of Swift `handoffTitle`
/// (`SessionsModel.swift:1173-1181`), unit-tested directly like
/// [`build_claude_exec_command`]. Strips a single leading `[HANDOFF] ` (no
/// stacking), trims whitespace/newlines, and falls back to `Session` when the
/// result is empty (a `None` / blank / whitespace-only originating title — which
/// would otherwise yield a ragged `[HANDOFF]    `).
pub(crate) fn handoff_title(originating_title: Option<&str>) -> String {
    let raw = originating_title.unwrap_or("");
    // `strip_prefix` mirrors Swift's `hasPrefix` + `dropFirst(prefix.count)`.
    let stripped = raw.strip_prefix(HANDOFF_TITLE_PREFIX).unwrap_or(raw);
    let trimmed = stripped.trim();
    let base = if trimmed.is_empty() { "Session" } else { trimmed };
    format!("{HANDOFF_TITLE_PREFIX}{base}")
}

/// Build the initial prompt seeded into a handoff session — pure port of Swift
/// `handoffPrompt` (`SessionsModel.swift:1194-1200`). Always points Claude at the
/// notes file; the continuation is the skill's custom `instructions` when
/// non-blank (direct `/nice-handoff <args>` invocations), else a default
/// read-and-wait directive (a no-arg / model-triggered handoff must NOT
/// auto-resume — it lands the fresh session read-and-await so the user stays in
/// control). The default's em-dash `—` is load-bearing (byte parity with Swift).
pub(crate) fn handoff_prompt(handoff_file: &str, instructions: &str) -> String {
    let trimmed = instructions.trim();
    let directive = if trimmed.is_empty() {
        "Do not start working yet — once you have read it, wait for the user to tell you how to proceed."
    } else {
        trimmed
    };
    format!("Read the handoff notes at {handoff_file}. {directive}")
}

/// Build the `extra_claude_args` for a handoff session so the fresh session
/// launches matched to the originating one — pure port of Swift
/// `handoffExtraArgs` (`SessionsModel.swift:1215-1221`). `model`/`effort` become
/// optional `--model <id>` / `--effort <tier>` flags (each omitted when empty, so
/// an unknown model / absent `CLAUDE_EFFORT` falls back to claude's own
/// defaults); the `prompt` MUST stay the FINAL element — it is the single
/// positional arg claude auto-runs, and flags must precede it. Combined with
/// [`build_claude_exec_command`] (which emits `--session-id <id>` then these args
/// verbatim), the launch line becomes
/// `claude --session-id <id> [--model <m>] [--effort <e>] '<prompt>'`.
pub(crate) fn handoff_extra_args(model: &str, effort: &str, prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if !model.is_empty() {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if !effort.is_empty() {
        args.push("--effort".to_string());
        args.push(effort.to_string());
    }
    args.push(prompt.to_string());
    args
}

/// The prefix on every dispatch-session title. Unlike [`HANDOFF_TITLE_PREFIX`] there
/// is no stripping rule: a dispatch title is built from the WORKTREE NAME, not
/// from another session's title, so it can never stack.
const DISPATCH_TITLE_PREFIX: &str = "[DISPATCH] ";

/// Build the locked `[DISPATCH] <worktree-name>` title for a dispatch session. The
/// locked title keeps the sidebar's session→worktree mapping stable against Claude's
/// OSC auto-title. Trims and falls back to `Session` on a blank name exactly as
/// [`handoff_title`] does, so a whitespace-only name can't render a ragged
/// `[DISPATCH]    ` (the socket parser only rejects a truly empty
/// `worktreeName`).
pub(crate) fn dispatch_title(worktree_name: &str) -> String {
    let trimmed = worktree_name.trim();
    let base = if trimmed.is_empty() { "Session" } else { trimmed };
    format!("{DISPATCH_TITLE_PREFIX}{base}")
}

/// Build the initial prompt seeded into a dispatched session. Unlike
/// [`handoff_prompt`] (which lands the fresh session read-and-WAIT), a dispatch
/// child is meant to start working immediately from the task file the dispatcher
/// wrote. Extra `instructions` are appended as a second sentence when non-blank,
/// same concatenation style as [`handoff_prompt`].
pub(crate) fn dispatch_prompt(task_file: &str, instructions: &str) -> String {
    let base = format!(
        "Read the dispatch task file at {task_file}, then start working on the task it describes."
    );
    let trimmed = instructions.trim();
    if trimmed.is_empty() {
        base
    } else {
        format!("{base} {trimmed}")
    }
}

/// Build the `extra_claude_args` for a dispatched session. **The argument order
/// is load-bearing:**
///
/// 1. `--add-dir <dirname(task_file)>` — the task file lives in the MAIN
///    checkout's `.claude/dispatch/`, outside the child's eventual worktree cwd,
///    so without this the child's very first action (reading the brief) stalls
///    on an out-of-cwd read permission prompt.
/// 2. `--worktree <name>` — `--add-dir` is a VARIADIC option that swallows every
///    following non-flag token, so a single-token option MUST follow it. Were
///    the prompt the next non-flag token (the default dispatch, where model and
///    effort are omitted) it would be eaten as a second directory and the child
///    would launch with NO prompt.
/// 3. `--model` / `--effort`, each omitted when empty. Dispatch deliberately does
///    NOT inherit the dispatcher's model/effort (the opposite of handoff): an
///    empty value means the child runs on the user's configured default.
/// 4. the `prompt`, LAST — the single positional arg claude auto-runs.
///
/// Long forms throughout, for launch-line greppability. `--add-dir` is dropped
/// when `task_file` has no parent directory component (a bare relative name),
/// which also keeps `--worktree` first and the variadic hazard moot.
pub(crate) fn dispatch_extra_args(
    worktree_name: &str,
    task_file: &str,
    model: &str,
    effort: &str,
    prompt: &str,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Some(dir) = std::path::Path::new(task_file)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
    {
        args.push("--add-dir".to_string());
        args.push(dir.to_string());
    }
    args.push("--worktree".to_string());
    args.push(worktree_name.to_string());
    if !model.is_empty() {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if !effort.is_empty() {
        args.push("--effort".to_string());
        args.push(effort.to_string());
    }
    args.push(prompt.to_string());
    args
}

#[cfg(test)]
mod tests;
