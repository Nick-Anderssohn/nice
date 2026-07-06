//! `SessionManager` — the per-window pty/session subsystem, the Rust twin of
//! Swift's `SessionsModel` (`Sources/Nice/State/SessionsModel.swift`).
//!
//! One `SessionManager` per window (it lives on [`crate::window_state::WindowState`],
//! the R12 per-window state struct). It wires the R3–R7 terminal stack
//! (`nice_term_view::TerminalSessionHandle` gpui entities) to the R8
//! [`TabModel`] document: it owns the live pane sessions, spawns deferred panes
//! on focus, and routes the entity's OSC title/cwd events back into the model.
//!
//! ## What this slice (R13 slice 1) owns
//!
//! * **Pure model routing** — [`SessionManager::pane_cwd_changed`],
//!   [`SessionManager::pane_title_changed`], [`SessionManager::set_active_pane`]
//!   (the model half: active-pane + ack-when-viewed),
//!   [`SessionManager::select_next_pane`] / [`select_prev_pane`] /
//!   `step_active_pane`, [`SessionManager::add_pane`] /
//!   [`add_terminal_to_active_tab`], and [`SessionManager::route_terminal_event`]
//!   (map a decoded [`TerminalEvent`] into the right routing call). These take
//!   `&mut TabModel` and touch no gpui, so they are unit-tested with plain
//!   `#[test]` (the `nice` binary crate never links gpui test-support — see
//!   `crates/nice-itests`).
//! * **The gpui spawn/focus primitives** — [`SessionManager::spawn_pane`],
//!   [`ensure_active_pane_spawned`], [`focus_active_pane`],
//!   [`register_tab_session`], [`teardown`]. These are the building blocks the
//!   live app composes; they compile now and are exercised by the R13 slice-3
//!   live scenario (nothing wires an action to them yet, hence the
//!   module-level `dead_code` allow — the same seam pattern as
//!   `sidebar_actions` / `window_state`).
//!
//! ## What R13 slice 2 owns (this slice)
//!
//! * **The pane lifecycle handlers** — [`pane_exited`](SessionManager::pane_exited)
//!   (the exact 5-step Swift ordering: clear overlay → model removal + neighbor
//!   refocus → pty release → deferred-companion spawn → dissolve check) and
//!   [`pane_held`](SessionManager::pane_held) (flip `is_alive` / idle the status
//!   / clear overlay, keep the pane mounted). [`route_terminal_event`] now routes
//!   `Exited` / `OutputStarted` into them instead of dropping them.
//! * **The synchronous dissolve cascade**
//!   ([`finalize_dissolved_tab`](SessionManager::finalize_dissolved_tab)) — core
//!   `remove_tab` (the single removal entry point, parent-pointer sweep) → pty
//!   release → selection prune → active-tab fallback → the declared-but-inert
//!   R18/R19 hooks → the every-project-empty terminus. Three entry points share
//!   it: pane-exit, [`close_tab`](SessionManager::close_tab) (R10's action,
//!   unconditional this cycle), and the unused cross-window
//!   [`dissolve_tab_if_empty`](SessionManager::dissolve_tab_if_empty) (R25).
//! * **The launch-overlay registry** —
//!   [`register_pane_launch`](SessionManager::register_pane_launch) /
//!   [`clear_pane_launch`](SessionManager::clear_pane_launch) /
//!   [`promote_pane_launch`](SessionManager::promote_pane_launch), the
//!   `launch_overlay_grace` seam (default [`nice_term_view::DEFAULT_LAUNCH_OVERLAY_GRACE`],
//!   `<= 0` promotes synchronously). The grace deadline reuses R7's App-Nap-safe
//!   `LaunchDeadline` injection — the live caller arms it and calls
//!   `promote_pane_launch` on fire (the `Pending`-guard covers the clear race).
//! * **Termination** — [`terminate_pane`](SessionManager::terminate_pane) /
//!   [`terminate_all`](SessionManager::terminate_all) / [`teardown`], plus the
//!   synthetic held/armed test seams
//!   ([`mark_synthetic_held_pane`](SessionManager::mark_synthetic_held_pane) /
//!   [`mark_synthetic_armed_deferred_pane`](SessionManager::mark_synthetic_armed_deferred_pane)
//!   / [`pane_is_spawned`](SessionManager::pane_is_spawned)) so close-flow tests
//!   construct all three tri-state shapes without racing a real child.
//!
//! The gpui side effects the live caller composes on top of the pure cascade —
//! step-4 deferred spawn ([`ensure_active_pane_spawned`]) and the terminus
//! actuator ([`apply_dissolve_terminus`](SessionManager::apply_dissolve_terminus),
//! close-this-window-or-quit via R12's registry) — need a gpui context, so they
//! stay separate primitives the slice-3 wiring calls (same seam pattern as slice
//! 1's `spawn_pane` / `focus_active_pane`). [`pane_exited`] returns a
//! [`PaneExitResolution`] telling that caller which to run.
//!
//! ## Deliberately deferred (later R13 slices — do not add here)
//!
//! * action-seam rewiring (sidebar `+` / strip `+` / ⌘T / pill select / close),
//!   the `cx.subscribe` that feeds [`route_terminal_event`] from a live entity,
//!   the live arming of the launch-overlay `LaunchDeadline`, and the
//!   `session-lifecycle` live scenario — **slice 3**.
//! * Claude status parsing (braille/✳ → thinking/waiting), tab auto-title from
//!   the OSC label, socket, promotion, persistence — **R15/R18** (breadcrumbs
//!   below).
//!
//! [`ensure_active_pane_spawned`]: SessionManager::ensure_active_pane_spawned
//! [`focus_active_pane`]: SessionManager::focus_active_pane
//! [`register_tab_session`]: SessionManager::register_tab_session
//! [`teardown`]: SessionManager::teardown
//! [`select_prev_pane`]: SessionManager::select_prev_pane
//! [`add_terminal_to_active_tab`]: SessionManager::add_terminal_to_active_tab
//! [`route_terminal_event`]: SessionManager::route_terminal_event
//! [`pane_exited`]: SessionManager::pane_exited
//! [`pane_held`]: SessionManager::pane_held
//! [`close_tab`]: SessionManager::close_tab
//! [`terminate_pane`]: SessionManager::terminate_pane
//! [`terminate_all`]: SessionManager::terminate_all
//! [`register_pane_launch`]: SessionManager::register_pane_launch

// The gpui spawn/focus primitives + a few pure helpers have no live caller until
// R13 slice 3 wires the action seams and the entity subscription to them; the
// model-routing methods below ARE exercised by this module's tests. Same
// seam-for-a-later-slice pattern as `window_state` / `sidebar_actions`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gpui::{App, Entity, FocusHandle, Global, Window};

use nice_model::{Pane, PaneKind, SidebarTabSelection, Tab, TabModel, TabStatus};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{TerminalEvent, TerminalSessionHandle, DEFAULT_LAUNCH_OVERLAY_GRACE};

use crate::window_registry::WindowRegistry;

/// Terminal-pane pill titles clip at 40 chars so the toolbar pill never
/// overflows (`SessionsModel.swift:400-404`).
const PANE_TITLE_MAX: usize = 40;

/// The per-pane "Launching…" overlay state — the Rust twin of Swift's
/// `PaneLaunchStatus` (`SessionsModel.paneLaunchStates`). App-shaped (it carries
/// the launch command string the overlay renders), so it lives here in `crates/nice`
/// rather than in `nice-term-*` (the boundary block). The R7 view owns its own
/// zero-frame [`nice_term_view::LaunchOverlay`] timing machine; this registry is
/// the app-level mirror the shell reads to paint the placeholder, driven by the
/// same grace deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaneLaunchStatus {
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
    /// another is live, else quits the app (see [`SessionManager::apply_dissolve_terminus`]).
    WindowEmptied,
}

impl DissolveTerminus {
    /// Combine two terminus outcomes across a multi-pane close loop:
    /// `WindowEmptied` wins (once the window is empty it stays empty). Used by the
    /// `close_tab`/close batch loops here and by
    /// [`crate::window_state::WindowState`]'s multi-tab close aggregation.
    pub(crate) fn or(self, other: DissolveTerminus) -> DissolveTerminus {
        match (self, other) {
            (DissolveTerminus::WindowEmptied, _) | (_, DissolveTerminus::WindowEmptied) => {
                DissolveTerminus::WindowEmptied
            }
            _ => DissolveTerminus::None,
        }
    }
}

/// The outcome of a pane exit — what gpui side effects the live caller must run
/// on top of the pure model cascade [`pane_exited`](SessionManager::pane_exited)
/// already applied. Swift runs these inline (steps 4–5 of `paneExited`); the Rust
/// split keeps the model routing unit-testable without a gpui context, and the
/// two effects are mutually exclusive with the dissolve (a surviving tab may
/// spawn a companion; a dissolved one runs the terminus), so applying them after
/// the pure cascade is observably identical to Swift's inline order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaneExitResolution {
    /// `Some(tab_id)` when the tab **survived** the exit — the live caller runs
    /// [`ensure_active_pane_spawned`](SessionManager::ensure_active_pane_spawned)
    /// (Swift step 4) so a refocus onto a deferred companion spawns its shell.
    /// `None` when the tab dissolved (nothing to spawn) or the tab was unknown.
    pub(crate) refocus_tab: Option<String>,
    /// The dissolve terminus (whether the window emptied → close/quit).
    pub(crate) terminus: DissolveTerminus,
}

/// The routing outcome of a single [`TerminalEvent`] — empty for the title / cwd
/// / reset / first-output events (fully handled inline), carrying the pane-exit
/// resolution for an `Exited { held: false }` event so the live subscription
/// applies the same step-4 spawn + terminus the direct [`pane_exited`] caller
/// does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoutedExit {
    pub(crate) refocus_tab: Option<String>,
    pub(crate) terminus: DissolveTerminus,
}

/// One live pane session: the core→gpui adapter entity plus the key-focus handle
/// the pane's terminal view tracks. Dropping the entity tears the child process
/// group down (SIGHUP→SIGKILL via `nice_term_core::Session::drop`), so a tab
/// entry removed from the cache leaks no zsh.
struct PaneSession {
    /// The `nice-term-view` adapter entity owning this pane's `Session`.
    handle: Entity<TerminalSessionHandle>,
    /// This pane's terminal key-focus handle — minted by the manager at spawn so
    /// [`SessionManager::focus_active_pane`] can move focus here; the pane's
    /// `TerminalView` tracks it (wired live in slice 3).
    focus: FocusHandle,
}

/// The per-window pty/session manager. Tab-keyed: each tab maps to its live pane
/// sessions (`pane_id -> PaneSession`), mirroring Swift's tab-keyed
/// `ptySessions` cache. A tab entry existing (even empty) means Swift's
/// `makeSession` ran for that tab — the precondition
/// [`ensure_active_pane_spawned`](SessionManager::ensure_active_pane_spawned)
/// checks before lazily spawning a deferred companion pane.
/// The per-window shell-injection env, set once at window construction by
/// `crate::app::arm_window_control_socket` (the Rust twin of Swift
/// `SessionsModel.bootstrapSocket`'s `controlSocketExtraEnv`). Every pty this
/// window's [`SessionManager`] spawns gets these merged into its env
/// **spec-wins** (see [`spawn_pane`](SessionManager::spawn_pane)).
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
    /// stub write failed (panes still get `NICE_SOCKET`; they just source the
    /// user's real rc directly).
    pub(crate) zdotdir: Option<String>,
    /// The value for `NICE_USER_ZDOTDIR`. `None` ⇒ the empty string is injected
    /// (Nice inherited no `ZDOTDIR`); the empty/absent distinction is semantic
    /// for the `.zshenv` stub's XDG discovery branch, so the var is ALWAYS set.
    pub(crate) user_zdotdir: Option<String>,
}

pub(crate) struct SessionManager {
    /// `tab_id -> (pane_id -> live session)`.
    tabs: HashMap<String, HashMap<String, PaneSession>>,
    /// Per-pane "Launching…" overlay entries (Swift's `paneLaunchStates`). A
    /// pane is inserted `Pending` at spawn and promoted to `Visible` when the
    /// grace deadline fires with no output; cleared on first output, exit, or
    /// held.
    pane_launch_states: HashMap<String, PaneLaunchStatus>,
    /// The grace window before a silent pane's overlay promotes to `Visible`
    /// (Swift's `launchOverlayGraceSeconds`). Default
    /// [`DEFAULT_LAUNCH_OVERLAY_GRACE`]; a `<= 0` value promotes synchronously
    /// inside [`register_pane_launch`](Self::register_pane_launch) (the test seam).
    launch_overlay_grace: Duration,
    /// Test-only: `<tab>:<pane>` keys [`pane_is_spawned`](Self::pane_is_spawned)
    /// reports as spawned without a real session (Swift's `syntheticSpawnedPanes`).
    /// Always empty in production — nothing populates it outside the `mark_*`
    /// test seams.
    synthetic_spawned: HashSet<String>,
    /// Subset of [`synthetic_spawned`](Self::synthetic_spawned) whose
    /// [`terminate_pane`](Self::terminate_pane) fires
    /// [`pane_exited`](Self::pane_exited) synchronously, mirroring the production
    /// held-pane fast path (`syntheticHeldPanes`). One-shot: consumed on terminate.
    synthetic_held: HashSet<String>,
    /// Subset of [`synthetic_spawned`](Self::synthetic_spawned) whose
    /// [`terminate_pane`](Self::terminate_pane) fires
    /// [`pane_exited`](Self::pane_exited) synchronously with no real child ever
    /// having run (`syntheticArmedDeferredPanes` — the armed-but-not-fired
    /// deferred spawn). One-shot: consumed on terminate.
    synthetic_armed: HashSet<String>,
    /// Injectable id minter (test seam). Production default:
    /// `<prefix><ms>-<suffix>` — the millisecond keeps ids roughly time-sortable
    /// for log triage; the short suffix keeps two creations in the same
    /// millisecond from colliding (Swift saw two `/branch`es in one ms collide).
    /// Unit tests inject a deterministic counter and assert by id.
    mint_id: Box<dyn Fn(&str) -> String>,
    /// R14: the per-window shell-injection env, set once at window construction
    /// (before the Main pane forks). `None` until a control socket is bootstrapped
    /// for this window, so managers built directly by scenarios/itests inject
    /// nothing. See [`WindowShellEnv`].
    window_shell_env: Option<WindowShellEnv>,
    /// W5 (R18): project ids the user asked to close whole (Swift's
    /// `CloseRequestCoordinator.projectsPendingRemoval`). Read — not cleared —
    /// by [`finalize_dissolved_tab`](Self::finalize_dissolved_tab) on each tab
    /// dissolve so a multi-tab project keeps the flag across earlier dissolves;
    /// cleared when its last tab empties and the row drops.
    pending_project_removal: HashSet<String>,
    /// R19: tab ids dissolved since the last drain — the file-browser per-tab
    /// cleanup hook. [`finalize_dissolved_tab`](Self::finalize_dissolved_tab) (the
    /// single tab-removal entry point) pushes here; [`WindowState`](crate::window_state::WindowState),
    /// which owns the [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore),
    /// drains it via [`take_dissolved_tab_ids`](Self::take_dissolved_tab_ids) after
    /// each cascade to drop the closed tab's browser state. Kept here (not threaded
    /// through the cascade signatures) so every dissolve path — UI close AND the
    /// route_terminal_event pane-exit — funnels one removal list without rippling
    /// the store into `SessionManager`.
    dissolved_tab_ids: Vec<String>,
}

impl SessionManager {
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
            tabs: HashMap::new(),
            pane_launch_states: HashMap::new(),
            launch_overlay_grace: DEFAULT_LAUNCH_OVERLAY_GRACE,
            synthetic_spawned: HashSet::new(),
            synthetic_held: HashSet::new(),
            synthetic_armed: HashSet::new(),
            mint_id,
            window_shell_env: None,
            pending_project_removal: HashSet::new(),
            dissolved_tab_ids: Vec::new(),
        }
    }

    /// Drain the tab ids dissolved since the last call — the R19 file-browser
    /// cleanup hook. [`WindowState`](crate::window_state::WindowState) calls this
    /// after every session cascade and drops each id's browser state from its
    /// [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore).
    pub(crate) fn take_dissolved_tab_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut self.dissolved_tab_ids)
    }

    /// Mark `project_id` for whole-project removal (W5 "Close Project"): its row
    /// drops from the tree once its last tab dissolves
    /// ([`finalize_dissolved_tab`](Self::finalize_dissolved_tab)). The pinned
    /// Terminals group is never marked. Swift's
    /// `CloseRequestCoordinator.projectsPendingRemoval.insert`.
    pub(crate) fn mark_project_pending_removal(&mut self, project_id: &str) {
        if project_id != TabModel::TERMINALS_PROJECT_ID {
            self.pending_project_removal.insert(project_id.to_string());
        }
    }

    /// Mint a unique id for a freshly-created pane, via the injected seam.
    fn mint(&self, prefix: &str) -> String {
        (self.mint_id)(prefix)
    }

    /// Mint a fresh tab id via the injected seam — the branch-parent
    /// materialization path (`WindowState::materialize_branch_parent`) mints its
    /// tab + `-claude`/`-t1` pane ids up front to hand to the model's
    /// `insert_branch_parent` (which takes them as params), mirroring
    /// `create_claude_tab`'s internal `self.mint("t")`.
    pub(crate) fn mint_tab_id(&self, prefix: &str) -> String {
        self.mint(prefix)
    }

    // MARK: - Pane title / cwd routing (pure model, unit-tested)

    /// A pane's shell emitted OSC 7 with a new working directory. Stash it on
    /// `Pane.cwd` **only** so a relaunch respawns the pane where it was — never
    /// `Tab.cwd`, which is load-bearing for `claude --resume`'s working dir and
    /// would silently relocate the session on restore if a companion terminal's
    /// `cd` overwrote it (`SessionsModel.swift:483-497`). Silently drops a stale
    /// tab/pane id. Returns whether anything changed — the caller fires the
    /// debounced session save on `true` (the `onSessionMutation` seam; R18).
    pub(crate) fn pane_cwd_changed(
        &mut self,
        model: &mut TabModel,
        tab_id: &str,
        pane_id: &str,
        cwd: &str,
    ) -> bool {
        let mut changed = false;
        model.mutate_tab(tab_id, |tab| {
            if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                if pane.cwd.as_deref() != Some(cwd) {
                    pane.cwd = Some(cwd.to_string());
                    changed = true;
                }
            }
        });
        changed
    }

    /// A pane's program emitted an OSC 0/2 title. **Terminal-branch policy only**
    /// (`SessionsModel.swift:385-414`): the emitted title becomes the pill label
    /// verbatim, except an empty/whitespace title is ignored, a manually-renamed
    /// pane (`title_manually_set`) is never clobbered by OSC, and an accepted
    /// title clips at [`PANE_TITLE_MAX`] chars.
    ///
    /// The **Claude branch is gated on `is_claude_running`** and dropped whole
    /// this cycle: `is_claude_running` stays `false` for every pane in R13 (only
    /// R15's socket promotion flips it), so a claude-kind pane contributes no
    /// status and no OSC-driven tab title — a deferred-resume Claude pane is a
    /// plain `zsh` whose theme OSC titles must not clobber the persisted session
    /// label (`SessionsModel.swift:416-435`). Silently drops a stale tab/pane id.
    ///
    /// Returns whether the pill label actually changed — the caller fires the
    /// debounced session save on `true` (Swift's `@Observable` write-back →
    /// `onTreeMutation`, byte-equality-skipped; R18 owns the save). A no-op
    /// re-report of the current title returns `false` (Validation probe (b)),
    /// mirroring [`pane_cwd_changed`](Self::pane_cwd_changed)'s did-change signal.
    pub(crate) fn pane_title_changed(
        &mut self,
        model: &mut TabModel,
        tab_id: &str,
        pane_id: &str,
        title: &str,
    ) -> bool {
        // Read the pane's kind + lock facts, then drop the borrow before the
        // mutation (Swift reads `pane` then re-enters via `mutateTab`).
        let Some(tab) = model.tab_for(tab_id) else {
            return false;
        };
        let Some(pane) = tab.panes.iter().find(|p| p.id == pane_id) else {
            return false;
        };
        let kind = pane.kind;
        let title_manually_set = pane.title_manually_set;
        let is_claude_running = pane.is_claude_running;

        match kind {
            PaneKind::Terminal => {
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
                let clipped = clip_title(trimmed, PANE_TITLE_MAX);
                let mut changed = false;
                model.mutate_tab(tab_id, |tab| {
                    if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                        if pane.title != clipped {
                            pane.title = clipped;
                            changed = true;
                        }
                    }
                });
                changed
            }
            PaneKind::Claude => {
                // R15 T5: the Claude branch — split the braille-spinner (U+2800..
                // U+28FF → thinking) / sparkle (U+2733 → waiting) status prefix via
                // [`parse_claude_title`], apply the status transition, and feed the
                // trailing label into the tab auto-title (dropping the "Claude Code"
                // placeholder). Gated on `is_claude_running`: a deferred-resume
                // Claude pane is a plain `zsh` whose theme OSC titles must not
                // clobber the persisted session label, so the whole branch drops
                // until the socket in-place promotion (the only production
                // false→true flip) opens the gate (`SessionsModel.swift:416-474`).
                if !is_claude_running {
                    return false;
                }
                let (status, label) = parse_claude_title(title);
                if let Some(new_status) = status {
                    // Acknowledge the pulse only when the user is actually looking at
                    // this pane — the viewed tab's active pane (Swift's
                    // `viewing && isActivePane`). A manually-renamed Claude pane still
                    // flips status: the title lock lives in the terminal branch, not
                    // here (`AppStatePaneLifecycleTests.claudePane_manuallySet_...`).
                    let viewing = model.active_tab_id() == Some(tab_id);
                    model.mutate_tab(tab_id, |tab| {
                        let is_active_pane = tab.active_pane_id.as_deref() == Some(pane_id);
                        if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                            pane.apply_status_transition(new_status, viewing && is_active_pane);
                        }
                    });
                }
                // The trailing label humanizes into the TAB auto-title — never the
                // Claude pane's own pill (that stays "Claude"/the user's rename).
                // Skip an empty label and Claude's generic "Claude Code" placeholder.
                let raw_label = label.trim();
                if raw_label.is_empty() || raw_label == "Claude Code" {
                    return false;
                }
                model.apply_auto_title(tab_id, raw_label);
                // This branch never writes the pane pill, so the pill-label-changed
                // signal is always `false` (status + tab title flow through the
                // model's own mutation hooks).
                false
            }
        }
    }

    /// Dispatch a decoded [`TerminalEvent`] from a pane's session entity to the
    /// right routing call. This is the pure connector the live entity
    /// subscription (slice 3) invokes per event; splitting it out keeps the
    /// routing unit-testable without a live pty or a gpui context.
    ///
    /// Returns a [`RoutedExit`]: empty for title / cwd / reset / first-output
    /// (fully handled here), carrying the pane-exit resolution for a clean
    /// `Exited { held: false }` so the live subscription runs the same step-4
    /// spawn + terminus a direct [`pane_exited`](Self::pane_exited) caller does.
    pub(crate) fn route_terminal_event(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        tab_id: &str,
        pane_id: &str,
        event: &TerminalEvent,
    ) -> RoutedExit {
        match event {
            TerminalEvent::TitleChanged(title) => {
                let _ = self.pane_title_changed(model, tab_id, pane_id, title);
                RoutedExit::default()
            }
            TerminalEvent::CwdChanged(path) => {
                // OSC 7 → `Pane.cwd` (plain path across the boundary; the app owns
                // the model type). The `to_string_lossy` is safe for the on-disk
                // absolute paths OSC 7 reports.
                let _ = self.pane_cwd_changed(model, tab_id, pane_id, &path.to_string_lossy());
                RoutedExit::default()
            }
            TerminalEvent::TitleReset => {
                // The terminal title-policy (`SessionsModel.swift:391-414`) only
                // accepts a non-empty OSC *set*; a reset to the terminal default
                // carries no new label, so it is a no-op for the pane pill here.
                RoutedExit::default()
            }
            TerminalEvent::OutputStarted => {
                // First pty byte — dismiss the "Launching…" overlay (Swift's
                // `NiceTerminalView.onFirstData` → `clearPaneLaunch`).
                self.clear_pane_launch(pane_id);
                RoutedExit::default()
            }
            TerminalEvent::Exited { held: true, .. } => {
                // `TabPtySession` decided to keep the view mounted (non-clean /
                // pre-first-byte exit) — flip the model to dead-but-on-screen and
                // clear the overlay. No removal, no dissolve.
                self.pane_held(model, tab_id, pane_id);
                RoutedExit::default()
            }
            TerminalEvent::Exited { held: false, .. } => {
                // Clean exit — the full 5-step `paneExited` cascade. The
                // resolution tells the live caller to run step-4 spawn on a
                // surviving tab and to actuate the terminus.
                let r = self.pane_exited(model, selection, tab_id, pane_id);
                RoutedExit {
                    refocus_tab: r.refocus_tab,
                    terminus: r.terminus,
                }
            }
            // `TerminalEvent` is `#[non_exhaustive]`; a still-later lifecycle
            // variant reaches here until this manager learns to route it.
            _ => RoutedExit::default(),
        }
    }

    // MARK: - Selection / pane navigation (pure model, unit-tested)

    /// Pick which pane is focused in `tab_id` — the **model half** of Swift's
    /// `setActivePane` (`SessionsModel.swift:534-545`): re-point `active_pane_id`
    /// (a no-op if `pane_id` isn't on the tab, so selection never dangles) and,
    /// when the tab is the one being viewed, acknowledge the newly-active pane if
    /// it was waiting.
    ///
    /// The live app composes the two side effects Swift's `setActivePane` also
    /// runs on top of this: [`ensure_active_pane_spawned`] (deferred spawn) and
    /// [`focus_active_pane`] (key focus). Those need a gpui context, so they are
    /// separate primitives the slice-3 action wiring calls right after this.
    ///
    /// [`ensure_active_pane_spawned`]: SessionManager::ensure_active_pane_spawned
    /// [`focus_active_pane`]: SessionManager::focus_active_pane
    pub(crate) fn set_active_pane(&mut self, model: &mut TabModel, tab_id: &str, pane_id: &str) {
        let viewing = model.active_tab_id() == Some(tab_id);
        model.mutate_tab(tab_id, |tab| {
            if tab.panes.iter().any(|p| p.id == pane_id) {
                tab.active_pane_id = Some(pane_id.to_string());
                if viewing {
                    if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                        pane.mark_acknowledged_if_waiting();
                    }
                }
            }
        });
    }

    /// Move focus to the next pane within the active tab, wrapping. No-op when
    /// the active tab has fewer than two panes (`SessionsModel.swift:569`).
    pub(crate) fn select_next_pane(&mut self, model: &mut TabModel) {
        self.step_active_pane(model, 1);
    }

    /// Move focus to the previous pane within the active tab, wrapping
    /// (`SessionsModel.swift:572`).
    pub(crate) fn select_prev_pane(&mut self, model: &mut TabModel) {
        self.step_active_pane(model, -1);
    }

    /// Wrapping step of the active tab's active pane by `offset`, routed through
    /// [`set_active_pane`](Self::set_active_pane) so the ack side effect rides
    /// along (and, in the live app, the deferred spawn + focus the caller adds).
    /// No-op when there is no active tab, the tab has fewer than two panes, or
    /// its active pane isn't resolvable (`SessionsModel.swift:574-584`).
    fn step_active_pane(&mut self, model: &mut TabModel, offset: isize) {
        let Some(tab_id) = model.active_tab_id().map(str::to_owned) else {
            return;
        };
        let Some(tab) = model.tab_for(&tab_id) else {
            return;
        };
        let count = tab.panes.len();
        if count < 2 {
            return;
        }
        let Some(active) = tab.active_pane_id.clone() else {
            return;
        };
        let Some(cur) = tab.panes.iter().position(|p| p.id == active) else {
            return;
        };
        // `((i + off) % n + n) % n`, expressed with rem_euclid.
        let next = (cur as isize + offset).rem_euclid(count as isize) as usize;
        let next_id = tab.panes[next].id.clone();
        self.set_active_pane(model, &tab_id, &next_id);
    }

    /// Append a new **terminal** pane to `tab_id`, focus it, and return its new
    /// id (`None` if the tab is unknown). The model half of Swift's `addPane`
    /// (`SessionsModel.swift:592-636`): only terminal-kind panes are
    /// constructible here — Claude panes are created exclusively by the
    /// claude-tab paths, preserving the ≤1-Claude-per-tab creation edge. The
    /// monotonic `next_terminal_index` counter is consumed via
    /// [`TabModel::add_pane`] (an explicit `title` consumes the slot too).
    ///
    /// The live app spawns the pty behind this immediately (explicit adds are
    /// **not** deferred — deferred spawn is only for panes modelled up front by a
    /// tab-creation path); slice 3 composes [`spawn_pane`](Self::spawn_pane) after
    /// the model mutation.
    pub(crate) fn add_pane(
        &mut self,
        model: &mut TabModel,
        tab_id: &str,
        title: Option<String>,
    ) -> Option<String> {
        // Guard before minting so an unknown tab wastes no id (Swift guards
        // `tabs.tab(for:)` first).
        model.tab_for(tab_id)?;
        let new_id = self.mint(&format!("{tab_id}-p"));
        model.add_pane(tab_id, new_id, title)
    }

    /// Append a terminal pane to the active tab and focus it; no-op (returns
    /// `None`) when there is no active tab (`SessionsModel.swift:640-643`).
    pub(crate) fn add_terminal_to_active_tab(&mut self, model: &mut TabModel) -> Option<String> {
        let tab_id = model.active_tab_id().map(str::to_owned)?;
        self.add_pane(model, &tab_id, None)
    }

    // MARK: - Launch overlay registry (pure model, unit-tested)

    /// Record that a pane was just spawned and start the grace window (Swift's
    /// `registerPaneLaunch`, `SessionsModel.swift:506-520`). The entry lands
    /// `Pending`; if it stays silent past [`launch_overlay_grace`](Self::launch_overlay_grace)
    /// it promotes to `Visible` and the shell paints "Launching…", and if
    /// [`clear_pane_launch`](Self::clear_pane_launch) fires first (first byte /
    /// exit / held) the overlay never appears.
    ///
    /// A `<= 0` grace promotes **synchronously** here (the test seam — no
    /// deadline hop). Otherwise this returns `true`: the live caller (slice 3)
    /// arms R7's App-Nap-safe [`nice_term_view::LaunchDeadline`] and calls
    /// [`promote_pane_launch`](Self::promote_pane_launch) when it fires. That
    /// method's `Pending`-guard covers the clear-before-fire race, so a coalesced
    /// or late deadline never resurrects a cleared overlay.
    pub(crate) fn register_pane_launch(&mut self, pane_id: &str, command: impl Into<String>) -> bool {
        let command = command.into();
        self.pane_launch_states
            .insert(pane_id.to_string(), PaneLaunchStatus::Pending { command });
        if self.launch_overlay_grace <= Duration::ZERO {
            self.promote_pane_launch(pane_id);
            false
        } else {
            true
        }
    }

    /// Promote a still-`Pending` launch entry to `Visible` — the grace deadline
    /// fired (Swift's inline `promote` closure). A no-op once the entry was
    /// cleared or already promoted, so a deadline that fires after the first byte
    /// never resurrects the overlay.
    pub(crate) fn promote_pane_launch(&mut self, pane_id: &str) {
        if let Some(PaneLaunchStatus::Pending { command }) = self.pane_launch_states.get(pane_id) {
            let command = command.clone();
            self.pane_launch_states
                .insert(pane_id.to_string(), PaneLaunchStatus::Visible { command });
        }
    }

    /// Remove any pending/visible overlay for `pane_id` (Swift's `clearPaneLaunch`).
    /// Fired on first pty byte, pane exit, and held so a process that dies before
    /// emitting anything leaves no orphan "Launching…" placeholder.
    pub(crate) fn clear_pane_launch(&mut self, pane_id: &str) {
        self.pane_launch_states.remove(pane_id);
    }

    /// The launch-overlay entry for `pane_id`, if any (the shell reads it to
    /// paint the placeholder; tests assert on it).
    pub(crate) fn pane_launch_state(&self, pane_id: &str) -> Option<&PaneLaunchStatus> {
        self.pane_launch_states.get(pane_id)
    }

    /// Override the launch-overlay grace window (the `launchOverlayGraceSeconds`
    /// test seam — set to `Duration::ZERO` for synchronous promotion).
    pub(crate) fn set_launch_overlay_grace(&mut self, grace: Duration) {
        self.launch_overlay_grace = grace;
    }

    // MARK: - Pane lifecycle handlers (pure model + cascade; unit-tested)

    /// A pane's child exited cleanly — the exact 5-step Swift `paneExited`
    /// ordering (`SessionsModel.swift:318-346`): (1) clear the launch overlay;
    /// (2) remove the pane from its tab, re-pointing `active_pane_id` to the slot
    /// neighbor via the same rule a cross-window move uses
    /// ([`TabModel::neighbor_active_pane_id`]); (3) release the pane's pty session;
    /// (5) if the tab is now empty, run the dissolve cascade synchronously with
    /// indices resolved at that instant.
    ///
    /// **Step 4 — the deferred-companion spawn — is the caller's gpui side
    /// effect.** It has no model-observable effect (it only forks a pty) and is
    /// mutually exclusive with the dissolve (a surviving tab may spawn; a
    /// dissolved one cannot), so this returns [`PaneExitResolution`]: the live
    /// caller runs [`ensure_active_pane_spawned`](Self::ensure_active_pane_spawned)
    /// on `refocus_tab` (Swift's step 4) and actuates `terminus`. Applying them
    /// after this pure cascade is observably identical to Swift's inline order.
    /// Silently drops a stale tab/pane id.
    pub(crate) fn pane_exited(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        tab_id: &str,
        pane_id: &str,
    ) -> PaneExitResolution {
        // (1) clear the launch overlay.
        self.clear_pane_launch(pane_id);
        // (2) model removal + neighbor refocus.
        model.mutate_tab(tab_id, |tab| {
            if let Some(idx) = tab.panes.iter().position(|p| p.id == pane_id) {
                tab.panes.remove(idx);
                if tab.active_pane_id.as_deref() == Some(pane_id) {
                    tab.active_pane_id = TabModel::neighbor_active_pane_id(idx, &tab.panes);
                }
            }
        });
        // (3) pty release.
        self.release_pane_session(tab_id, pane_id);
        // (5) dissolve check — the empty-tab callback's indices are valid only
        // because nothing runs in between (Swift keeps this synchronous). The
        // caller runs step 4 (spawn) on `refocus_tab` on the way out.
        match model.tab_for(tab_id) {
            Some(tab) if tab.panes.is_empty() => {
                let terminus = match model.project_tab_index(tab_id) {
                    Some((pi, ti)) => self.finalize_dissolved_tab(model, selection, pi, ti, tab_id),
                    None => DissolveTerminus::None,
                };
                PaneExitResolution {
                    refocus_tab: None,
                    terminus,
                }
            }
            Some(_) => PaneExitResolution {
                // Tab survived: focus may have auto-switched onto a deferred
                // companion — the live caller spawns it before anything else.
                refocus_tab: Some(tab_id.to_string()),
                terminus: DissolveTerminus::None,
            },
            None => PaneExitResolution::default(),
        }
    }

    /// A pane's process exited but its view stays mounted so the user can read
    /// the scrollback (Swift's `paneHeld`, `SessionsModel.swift:362-377`): clear
    /// the launch overlay, flip `is_alive` false, and idle out any pulsing status
    /// so the rest of the model (sidebar dot, live counts, `has_claude`) treats
    /// the pane as dead — while leaving it in `tab.panes` so the pill + view stay
    /// on screen. The model removal happens later when the user closes the tab
    /// ([`terminate_pane`](Self::terminate_pane) synthesizes the deferred exit).
    /// Silently drops a stale tab/pane id.
    pub(crate) fn pane_held(&mut self, model: &mut TabModel, tab_id: &str, pane_id: &str) {
        self.clear_pane_launch(pane_id);
        model.mutate_tab(tab_id, |tab| {
            if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                pane.is_alive = false;
                // A held-dead pane is not thinking or waiting regardless of its
                // last OSC title; idle it and clear the ack so a future fresh
                // waiting pane can pulse again.
                pane.status = TabStatus::Idle;
                pane.waiting_acknowledged = false;
                // Clear the promotion flag so a fresh `claude` in this tab routes
                // correctly (R15) — a held pty is a corpse, not a live shell.
                pane.is_claude_running = false;
            }
        });
    }

    /// Drop a single pane's pty session from the cache (Swift's
    /// `ptySessions[tabId]?.removePane`). Keeps the (possibly now-empty) per-tab
    /// container; the dissolve cascade drops that separately. Dropping the
    /// [`TerminalSessionHandle`] tears its child process group down
    /// (SIGHUP→SIGKILL via `nice_term_core::Session::drop`), so no orphan zsh.
    fn release_pane_session(&mut self, tab_id: &str, pane_id: &str) {
        if let Some(panes) = self.tabs.get_mut(tab_id) {
            panes.remove(pane_id);
        }
    }

    // MARK: - Dissolve cascade (pure core + gpui terminus; unit-tested)

    /// Finish dissolving a tab whose `panes` array reached zero — the synchronous
    /// core of Swift's `AppState.finalizeDissolvedTab` (`AppState.swift:326-373`),
    /// in its exact order: `remove_tab` (the **single** removal entry point, which
    /// does the parent-pointer sweep) → pty-session release → selection prune →
    /// active-tab fallback in [`TabModel::navigable_sidebar_tab_ids`] order. The
    /// later-row subscriber hooks stay **declared but inert** (see the body).
    /// Returns the every-project-empty [`DissolveTerminus`] the gpui caller
    /// actuates via [`apply_dissolve_terminus`](Self::apply_dissolve_terminus).
    ///
    /// Delivery is synchronous by contract: `(pi, ti)` are valid only because
    /// nothing runs between the empty-tab check and this call.
    fn finalize_dissolved_tab(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        pi: usize,
        ti: usize,
        tab_id: &str,
    ) -> DissolveTerminus {
        // Core: the single removal entry point (array remove + parent-pointer
        // sweep, atomically — a future close path can't orphan a /branch child).
        model.remove_tab(pi, ti);
        // pty-session release (Swift's `removePtySession`).
        self.tabs.remove(tab_id);

        // Subscriber hooks (later rows):
        //   * file-browser per-tab cleanup (R19): record the dissolved tab id so
        //     `WindowState` drops its `FileBrowserStore` entry after the cascade
        //     (the single tab-removal entry point, so every dissolve path — UI
        //     close AND the pane-exit route — funnels one removal list).
        self.dissolved_tab_ids.push(tab_id.to_string());
        //   * debounced session save (onSessionMutation) → the UI-close callers
        //     (`WindowState::save_to_store`) schedule it; R18.

        // Selection prune (R10 multi-select): drop the dissolved id (and clear a
        // dangling anchor/active mirror) before any view re-renders against the
        // shrunken tree. Uses the post-removal navigable set.
        let valid: HashSet<String> = model.navigable_sidebar_tab_ids().into_iter().collect();
        selection.prune(&valid);

        // Active-tab fallback via navigable order (Swift's `firstAvailableTabId`).
        if model.active_tab_id() == Some(tab_id) {
            if let Some(fallback) = model.navigable_sidebar_tab_ids().into_iter().next() {
                model.select_tab(&fallback);
            }
            // else: no navigable tab remains — the window is empty and closes /
            // quits below (the `TabModel` has no `None` active-tab writer, and
            // the window is going away, so leaving the stale id is harmless).
        }

        // W5 (R18) project-pending-removal (Swift `AppState.finalizeDissolvedTab:349-355`):
        // if the user asked to close this whole project and its last tab just
        // dissolved, drop the (non-Terminals) row. Read without clearing until it
        // empties so earlier-tab dissolves in a multi-tab project keep the flag.
        if pi < model.projects.len() {
            let project_id = model.projects[pi].id.clone();
            if self.pending_project_removal.contains(&project_id)
                && model.projects[pi].tabs.is_empty()
                && project_id != TabModel::TERMINALS_PROJECT_ID
            {
                self.pending_project_removal.remove(&project_id);
                model.projects.remove(pi);
            }
        }

        // Every-project-empty terminus (Swift closes this window when another is
        // live, else quits the app).
        if model.projects.iter().all(|p| p.tabs.is_empty()) {
            DissolveTerminus::WindowEmptied
        } else {
            DissolveTerminus::None
        }
    }

    /// Dissolve `tab_id` if a cross-window move / tear-off left it with no panes,
    /// running the same cascade a last-pane exit would (Swift's
    /// `dissolveTabIfEmpty`, `AppState.swift:382-387`). No-op when the tab still
    /// has panes or doesn't exist. This is the dissolve entry point for the R25
    /// `extract_pane` path, which bypasses the pane-exit callback — **modelled
    /// now, unused this cycle** (no cross-window migration until R25).
    pub(crate) fn dissolve_tab_if_empty(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        tab_id: &str,
    ) -> DissolveTerminus {
        match model.project_tab_index(tab_id) {
            Some((pi, ti)) if model.projects[pi].tabs[ti].panes.is_empty() => {
                self.finalize_dissolved_tab(model, selection, pi, ti, tab_id)
            }
            _ => DissolveTerminus::None,
        }
    }

    /// Close an entire tab unconditionally (this cycle has no confirmation — W5 is
    /// R18), the Rust twin of `CloseRequestCoordinator.hardKillTab`
    /// (`CloseRequestCoordinator.swift:297-363`). The third dissolve entry point.
    ///
    /// Splits panes by [`pane_is_spawned`](Self::pane_is_spawned).
    /// [`terminate_pane`](Self::terminate_pane) is a no-op for a **model-only**
    /// pane (no session at all — the lazy companion the user never focused), so
    /// those are dropped from the model directly; otherwise a SIGHUP-only close
    /// would leave them behind and the tab would never dissolve. Unspawned rows
    /// are dropped **before** terminating the spawned ones so a held pane's
    /// synchronous `pane_exited` sees an already-pruned array and its empty-tab
    /// check fires (the tri-state close bug the Swift reorder fixed). Returns the
    /// aggregate [`DissolveTerminus`].
    pub(crate) fn close_tab(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        tab_id: &str,
    ) -> DissolveTerminus {
        let Some(tab) = model.tab_for(tab_id) else {
            return DissolveTerminus::None;
        };
        let mut spawned: Vec<String> = Vec::new();
        let mut unspawned: Vec<String> = Vec::new();
        for pane in &tab.panes {
            if self.pane_is_spawned(tab_id, &pane.id) {
                spawned.push(pane.id.clone());
            } else {
                unspawned.push(pane.id.clone());
            }
        }

        if !unspawned.is_empty() {
            if spawned.is_empty() {
                // Model-only tab: nothing async to hook into — clear the panes and
                // dissolve synchronously (Validation probe (d)).
                model.mutate_tab(tab_id, |tab| {
                    tab.panes.clear();
                    tab.active_pane_id = None;
                });
                return match model.project_tab_index(tab_id) {
                    Some((pi, ti)) => self.finalize_dissolved_tab(model, selection, pi, ti, tab_id),
                    None => DissolveTerminus::None,
                };
            }
            // Drop unspawned rows up front (before terminating spawned ones).
            let drop: HashSet<String> = unspawned.into_iter().collect();
            model.mutate_tab(tab_id, |tab| {
                tab.panes.retain(|p| !drop.contains(&p.id));
                let active_dropped = tab
                    .active_pane_id
                    .as_deref()
                    .is_some_and(|a| drop.contains(a));
                if active_dropped {
                    tab.active_pane_id = tab.panes.first().map(|p| p.id.clone());
                }
            });
        }

        let mut terminus = DissolveTerminus::None;
        for pane_id in spawned {
            terminus = terminus.or(self.terminate_pane(model, selection, tab_id, &pane_id).terminus);
        }
        terminus
    }

    // MARK: - Termination (pure model + synthetic seams; unit-tested)

    /// SIGHUP→SIGKILL the named pane and drop its pty, driving the model removal
    /// through [`pane_exited`](Self::pane_exited) — the Rust twin of
    /// `TabPtySession.terminatePane` (`TabPtySession.swift:680-715`). Three fast
    /// paths mirror Swift, in order:
    ///
    /// * **Synthetic held** — fires `pane_exited` synchronously (the production
    ///   held-pane fast path); the marker is consumed (one-shot).
    /// * **Synthetic armed-but-not-fired** — same, for a captured deferred spawn
    ///   that never forked (nil-status synthesized exit).
    /// * **Live/held real session** — `pane_exited`'s step-3 drop tears the child
    ///   group down and unconditionally removes the model pane. This is the
    ///   "intentional-terminate flag set **before** the pid guard" contract:
    ///   the pane always drops (never holds), even if its child never got a pid.
    ///
    /// A **model-only** pane (no session, no synthetic marker) is a no-op —
    /// matching Swift's `guard var entry = entries[id] else { return }`;
    /// [`close_tab`](Self::close_tab) removes those from the model up front.
    /// Returns the [`PaneExitResolution`] of the synthesized exit (so a
    /// single-pane close can spawn a refocused companion / actuate the terminus).
    pub(crate) fn terminate_pane(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        tab_id: &str,
        pane_id: &str,
    ) -> PaneExitResolution {
        let key = synthetic_key(tab_id, pane_id);
        if self.synthetic_held.remove(&key) {
            self.synthetic_spawned.remove(&key);
            return self.pane_exited(model, selection, tab_id, pane_id);
        }
        if self.synthetic_armed.remove(&key) {
            self.synthetic_spawned.remove(&key);
            return self.pane_exited(model, selection, tab_id, pane_id);
        }
        if self.has_pane(tab_id, pane_id) {
            return self.pane_exited(model, selection, tab_id, pane_id);
        }
        PaneExitResolution::default()
    }

    /// Tear down every live pane on `tab_id` (Swift's `SessionsModel.terminateAll`
    /// → `TabPtySession.terminateAll`, `:838-854`). **Snapshots the pane ids up
    /// front** because each [`terminate_pane`](Self::terminate_pane) → held
    /// `pane_exited` mutates the cache and the tree mid-loop (synthesized exits
    /// re-enter removal); a live iterator would skip or double-visit an entry.
    /// Returns the aggregate [`DissolveTerminus`].
    pub(crate) fn terminate_all(
        &mut self,
        model: &mut TabModel,
        selection: &mut SidebarTabSelection,
        tab_id: &str,
    ) -> DissolveTerminus {
        // Snapshot: every live-session pane id for this tab, plus any synthetic
        // marker (held/armed panes have no `self.tabs` entry).
        let mut ids: Vec<String> = self
            .tabs
            .get(tab_id)
            .map(|panes| panes.keys().cloned().collect())
            .unwrap_or_default();
        let prefix = format!("{tab_id}:");
        for key in &self.synthetic_spawned {
            if let Some(pane_id) = key.strip_prefix(&prefix) {
                let pane_id = pane_id.to_string();
                if !ids.contains(&pane_id) {
                    ids.push(pane_id);
                }
            }
        }

        let mut terminus = DissolveTerminus::None;
        for pane_id in ids {
            terminus = terminus.or(self.terminate_pane(model, selection, tab_id, &pane_id).terminus);
        }
        terminus
    }

    /// Whether `(tab_id, pane_id)` counts as spawned for close routing — a real
    /// live session **or** a synthetic marker (Swift's `paneIsSpawned`). Drives
    /// [`close_tab`](Self::close_tab)'s spawned/unspawned split.
    pub(crate) fn pane_is_spawned(&self, tab_id: &str, pane_id: &str) -> bool {
        self.synthetic_spawned
            .contains(&synthetic_key(tab_id, pane_id))
            || self.has_pane(tab_id, pane_id)
    }

    /// Test seam: mark `(tab_id, pane_id)` as a **held** pane without a real pty —
    /// [`pane_is_spawned`](Self::pane_is_spawned) then returns `true` and
    /// [`terminate_pane`](Self::terminate_pane) fires `pane_exited` synchronously,
    /// letting close-flow tests build the held tri-state shape without racing a
    /// real child (Swift's `markSyntheticHeldPaneForTesting`).
    pub(crate) fn mark_synthetic_held_pane(&mut self, tab_id: &str, pane_id: &str) {
        let key = synthetic_key(tab_id, pane_id);
        self.synthetic_spawned.insert(key.clone());
        self.synthetic_held.insert(key);
    }

    /// Test seam: mark `(tab_id, pane_id)` as an **armed-but-not-fired** deferred
    /// spawn (a resume-deferred Claude pane whose view captured a spawn that never
    /// forked) — [`pane_is_spawned`](Self::pane_is_spawned) returns `true` and
    /// [`terminate_pane`](Self::terminate_pane) fires the nil-status `pane_exited`
    /// synchronously (Swift's `markSyntheticArmedDeferredPaneForTesting`).
    pub(crate) fn mark_synthetic_armed_deferred_pane(&mut self, tab_id: &str, pane_id: &str) {
        let key = synthetic_key(tab_id, pane_id);
        self.synthetic_spawned.insert(key.clone());
        self.synthetic_armed.insert(key);
    }

    /// Actuate a [`DissolveTerminus`] via R12's registry (the gpui side of the
    /// every-project-empty terminus — live-wired slice 3): close this window when
    /// another live window remains, else quit the app. A no-op for
    /// [`DissolveTerminus::None`]. Mirrors `AppState.finalizeDissolvedTab:359-372`.
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

    /// Whether `tab_id` has a session container (Swift's `ptySessions[tabId]`).
    fn tab_has_session(&self, tab_id: &str) -> bool {
        self.tabs.contains_key(tab_id)
    }

    /// Whether `(tab_id, pane_id)` currently has a live pane session (Swift's
    /// `session.hasPane`).
    pub(crate) fn has_pane(&self, tab_id: &str, pane_id: &str) -> bool {
        self.tabs
            .get(tab_id)
            .is_some_and(|panes| panes.contains_key(pane_id))
    }

    /// The live session entity for `(tab_id, pane_id)`, if one is cached — the
    /// **slice-3 subscription seam**. The live wiring clones this out to
    /// `cx.subscribe` the window's [`crate::window_state::WindowState`] to the
    /// pane's OSC / exit events (feeding them through
    /// [`route_terminal_event`](Self::route_terminal_event)), to read its grid for
    /// a readiness poll, and to write input. Cloning an [`Entity`] is a cheap
    /// refcount bump that does **not** keep the session alive past the manager's
    /// own release — a transient clone dropped after subscribing leaves the manager
    /// the sole owner, so a later [`pane_exited`](Self::pane_exited) /
    /// [`teardown`](Self::teardown) still tears the child process group down.
    pub(crate) fn pane_handle(
        &self,
        tab_id: &str,
        pane_id: &str,
    ) -> Option<Entity<TerminalSessionHandle>> {
        self.tabs
            .get(tab_id)
            .and_then(|panes| panes.get(pane_id))
            .map(|session| session.handle.clone())
    }

    /// Every `(tab_id, pane_id)` with a live pane session right now — the
    /// enumeration the shipped window's subscribe-once sweep
    /// ([`crate::window_state::WindowState::subscribe_spawned_panes`]) walks to
    /// wire each freshly-spawned pane's entity to [`route_terminal_event`](Self::route_terminal_event).
    /// Order is unspecified (a `HashMap` walk); the sweep dedupes by key, so
    /// order does not matter.
    pub(crate) fn live_pane_keys(&self) -> Vec<(String, String)> {
        self.tabs
            .iter()
            .flat_map(|(tab_id, panes)| {
                panes
                    .keys()
                    .map(move |pane_id| (tab_id.clone(), pane_id.clone()))
            })
            .collect()
    }

    /// Register an **empty** per-tab session container without spawning any pane.
    /// On the claude-tab creation path it runs just before the eager Claude spawn
    /// (`create_claude_tab` calls `spawn_claude_pane` immediately — claude-kind
    /// panes never lazy-spawn) while the companion terminal stays deferred.
    /// It exists so [`ensure_active_pane_spawned`](Self::ensure_active_pane_spawned)'s
    /// "the tab already has a session" precondition holds when the user first
    /// focuses the deferred companion. Idempotent.
    pub(crate) fn register_tab_session(&mut self, tab_id: &str) {
        self.tabs.entry(tab_id.to_string()).or_default();
    }

    /// Set this window's shell-injection env (Swift `SessionsModel.bootstrapSocket`).
    /// Called once at window construction, BEFORE the Main pane forks, so every
    /// pty spawned through [`spawn_pane`](Self::spawn_pane) inherits `NICE_SOCKET`
    /// / `ZDOTDIR` / `NICE_USER_ZDOTDIR` from launch (the "env before fork"
    /// invariant the shell's `claude()` shadow depends on).
    pub(crate) fn set_window_shell_env(&mut self, env: WindowShellEnv) {
        self.window_shell_env = Some(env);
    }

    /// The per-pane terminal env pairs this window injects into every pty
    /// (Swift `TabPtySession.addTerminalPane`'s `extraEnv`): `NICE_SOCKET` +
    /// `ZDOTDIR` (each only when set) + `NICE_USER_ZDOTDIR` (ALWAYS, empty string
    /// when Nice inherited none — the empty/absent distinction is semantic for the
    /// `.zshenv` stub) + this pane's `NICE_TAB_ID` / `NICE_PANE_ID` (the handshake
    /// identity the `claude()` shadow includes in its socket payload). Empty when
    /// the window bootstrapped no socket. Pure — no `cx`, so the env matrix is
    /// unit-tested directly (Validation §3 b/c).
    fn window_pane_env_pairs(&self, tab_id: &str, pane_id: &str) -> Vec<(String, String)> {
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
        pairs.push(("NICE_TAB_ID".to_string(), tab_id.to_string()));
        pairs.push(("NICE_PANE_ID".to_string(), pane_id.to_string()));
        pairs
    }

    /// Spawn a live terminal session for `(tab_id, pane_id)` from `spec` and
    /// cache it with a fresh key-focus handle. Idempotent per `(tab, pane)`.
    ///
    /// R14: the window's shell-injection env
    /// ([`window_pane_env_pairs`](Self::window_pane_env_pairs) — `NICE_SOCKET` /
    /// `ZDOTDIR` / `NICE_USER_ZDOTDIR` / `NICE_TAB_ID` / `NICE_PANE_ID`) is merged
    /// into `spec.env` **spec-wins** ([`merge_env_spec_wins`]): a key already
    /// present on the caller-built spec (e.g. a deliberately-blanked `ZDOTDIR`)
    /// survives the injection. This is the single choke point every pty spawn
    /// passes through, so it covers the Main pane, `ensure_active_pane_spawned`,
    /// and every future R15/R18 path for free.
    pub(crate) fn spawn_pane(
        &mut self,
        tab_id: &str,
        pane_id: &str,
        mut spec: SpawnSpec,
        cx: &mut App,
    ) -> Result<()> {
        if self.has_pane(tab_id, pane_id) {
            return Ok(());
        }
        merge_env_spec_wins(&mut spec.env, self.window_pane_env_pairs(tab_id, pane_id));
        self.spawn_session_raw(tab_id, pane_id, spec, cx)
    }

    /// Spawn + cache a live session from `spec` **verbatim** — no window
    /// injection. The Claude spawn path ([`spawn_claude_pane`](Self::spawn_claude_pane))
    /// uses this because a Claude pane's env is fully determined by
    /// [`build_claude_extra_env`] (it deliberately omits `ZDOTDIR` for a
    /// non-deferred pane, so it `exec`s claude under the user's own rc — matching
    /// Swift's per-mode env); routing it through [`spawn_pane`](Self::spawn_pane)'s
    /// blanket injection would re-add `ZDOTDIR`/`NICE_USER_ZDOTDIR` it doesn't
    /// want. Idempotent per `(tab, pane)`.
    fn spawn_session_raw(
        &mut self,
        tab_id: &str,
        pane_id: &str,
        spec: SpawnSpec,
        cx: &mut App,
    ) -> Result<()> {
        if self.has_pane(tab_id, pane_id) {
            return Ok(());
        }
        let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
        let focus = cx.focus_handle();
        self.tabs
            .entry(tab_id.to_string())
            .or_default()
            .insert(pane_id.to_string(), PaneSession { handle, focus });
        Ok(())
    }

    /// The ONE shared Claude-tab constructor — the Rust twin of Swift's
    /// near-duplicate `createTabFromMainTerminal` (socket newtab path) /
    /// `createClaudeTabInProject` (sidebar project-`+` path)
    /// (`SessionsModel.swift:650-714, :758-794`). Builds the `[Claude, Terminal 1]`
    /// shape (Claude focused), places it, selects it, registers the tab session,
    /// and spawns the Claude pane from `spawn_cwd` (claude resolves/creates its own
    /// `-w` worktree) — the companion terminal stays **deferred** (model-only until
    /// first focus, per Swift `makeSession(initialTerminalPaneId: nil)`).
    ///
    /// The Claude pane is created with `is_claude_running = true` from day one (the
    /// PROTECTED creation invariant: it gates the ≤1-Claude promotion refusal, the
    /// OSC title/status pulse, and auto-titles). The session UUID is pre-minted
    /// (real v4, via [`mint_session_uuid`]) so `--session-id` is passed now and the
    /// same id persists for later `--resume`.
    ///
    /// `settings_path` is the injectable theme-sync provider's output (R17 fills it;
    /// `None` until then). Returns the new tab id, or `None` for a bad placement (an
    /// unknown / pinned-Terminals project id).
    pub(crate) fn create_claude_tab(
        &mut self,
        model: &mut TabModel,
        placement: ClaudeTabPlacement,
        args: &[String],
        settings_path: Option<&str>,
        cx: &mut App,
    ) -> Option<String> {
        // Placement-specific facts, resolved before we mint anything that would
        // otherwise leak on a bad project id.
        let (title, tab_cwd, spawn_cwd, extra_args): (String, String, String, Vec<String>) =
            match &placement {
                ClaudeTabPlacement::Bucket { cwd } => (
                    // The bucketing anchor (`project_path`) stays `cwd`; the tab cwd
                    // follows the `-w` worktree in.
                    claude_tab_title_from_args(args),
                    claude_worktree_cwd(cwd, args),
                    cwd.clone(),
                    args.to_vec(),
                ),
                ClaudeTabPlacement::Project { project_id } => {
                    // The pinned Terminals group only holds terminal tabs.
                    if project_id == TabModel::TERMINALS_PROJECT_ID {
                        return None;
                    }
                    let pi = model.projects.iter().position(|p| &p.id == project_id)?;
                    let path = model.projects[pi].path.clone();
                    ("New tab".to_string(), path.clone(), path, Vec::new())
                }
            };

        let tab_id = self.mint("t");
        let claude_pane_id = format!("{tab_id}-claude");
        let terminal_pane_id = format!("{tab_id}-t1");
        let session_id = mint_session_uuid();

        // The Claude pane is `is_claude_running = true` at creation (PROTECTED).
        let mut claude_pane = Pane::new(&claude_pane_id, "Claude", PaneKind::Claude);
        claude_pane.is_claude_running = true;
        let mut tab = Tab::new(&tab_id, title, tab_cwd);
        tab.panes = vec![
            claude_pane,
            Pane::new(&terminal_pane_id, "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some(claude_pane_id.clone());
        tab.claude_session_id = Some(session_id.clone());
        tab.next_terminal_index = 2;

        match &placement {
            ClaudeTabPlacement::Bucket { cwd } => model.add_tab_to_projects(tab, cwd),
            ClaudeTabPlacement::Project { project_id } => {
                let pi = model.projects.iter().position(|p| &p.id == project_id)?;
                model.projects[pi].tabs.push(tab);
            }
        }
        model.select_tab(&tab_id);
        // The (empty) session container so the deferred companion's later
        // `ensure_active_pane_spawned` precondition ("the tab has a session") holds.
        self.register_tab_session(&tab_id);

        // Spawn the Claude pane immediately (claude-kind panes never lazy-spawn).
        let _ = self.spawn_claude_pane(
            &tab_id,
            &claude_pane_id,
            &spawn_cwd,
            &ClaudeSessionMode::New(session_id),
            &extra_args,
            settings_path,
            cx,
        );
        Some(tab_id)
    }

    /// Spawn a **Claude-kind** pane's child — the Rust twin of Swift
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
    ///   `environment: nil` fallback: the pane renders as Claude but is really a
    ///   shell). No retro-upgrade when the probe later resolves.
    ///
    /// The env comes wholly from [`build_claude_extra_env`] (which reads this
    /// window's socket / zdotdir facts) and the spawn bypasses the blanket window
    /// injection ([`spawn_session_raw`](Self::spawn_session_raw)) — see that method.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_claude_pane(
        &mut self,
        tab_id: &str,
        pane_id: &str,
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
                tab_id,
                pane_id,
                socket_path.as_deref(),
                zdotdir.as_deref(),
                user_zdotdir.as_deref(),
                settings_path.map(str::to_string),
            );
            SpawnSpec::shell(cwd).with_env(env)
        } else if let Some(claude) = claude.as_deref() {
            let env = build_claude_extra_env(
                mode,
                tab_id,
                pane_id,
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

        self.spawn_session_raw(tab_id, pane_id, spec, cx)?;

        // Launch-overlay policy: register the user-facing command string; a
        // deferred-resume pane suppresses it (Swift `installLaunchOverlayHooks`'s
        // early return for `.resumeDeferred`). The live window root clears it on
        // first output / exit via the routed events (the subscription lift).
        if !matches!(mode, ClaudeSessionMode::ResumeDeferred(_)) {
            let _ = self.register_pane_launch(pane_id, claude_launch_display_command(mode, extra_args));
        }
        Ok(())
    }

    /// Spawn the active pane's deferred pty if it was modelled up front — Swift's
    /// `ensureActivePaneSpawned` (`SessionsModel.swift:553-565`), extended for R18
    /// restore. Two lazy-spawn arms, both gated on the tab having a session
    /// container and the pty not being live yet:
    ///
    /// * a **terminal-kind** active pane spawns a plain login shell in its
    ///   resolved cwd (last OSC 7, else the tab/project fallback) — unchanged;
    /// * a **claude-kind** active pane lazy-spawns **only in resume-deferred
    ///   form** (L3): iff the tab carries a `claude_session_id`, the pane is not
    ///   yet spawned, and no Claude is running, it spawns a plain login shell
    ///   carrying `claude --resume <sid>` as `NICE_PREFILL_COMMAND` (nothing runs
    ///   until the user opens the tab and presses Enter). This **supersedes** R15's
    ///   "claude never lazy-spawns" note: a *restored* Claude pane returns modelled
    ///   but unspawned and must lazy-spawn its deferred-resume shell on first
    ///   activation. A *running* Claude pane (already spawned, or one promoted in
    ///   place) still never lazy-spawns — the `is_claude_running` / already-spawned
    ///   guards below reject it.
    ///
    /// Never creates a tab container itself. `settings_path` is R17's theme
    /// `--settings` pointer (threaded from the window's provider), spliced into the
    /// deferred-resume prefill; `None` ⇒ no `--settings` (sync off / gate unset).
    pub(crate) fn ensure_active_pane_spawned(
        &mut self,
        model: &TabModel,
        tab_id: &str,
        settings_path: Option<&str>,
        cx: &mut App,
    ) {
        let Some(tab) = model.tab_for(tab_id) else {
            return;
        };
        let Some(pane_id) = tab.active_pane_id.clone() else {
            return;
        };
        let Some(pane) = tab.panes.iter().find(|p| p.id == pane_id) else {
            return;
        };
        if !self.tab_has_session(tab_id) || self.has_pane(tab_id, &pane_id) {
            return;
        }
        // L3 restore arm: a claude-kind active pane lazy-spawns its deferred-resume
        // shell (never a running claude). A running-claude or session-less pane is
        // left to its eager/socket spawn path.
        if pane.kind == PaneKind::Claude {
            if pane.is_claude_running {
                return;
            }
            let Some(sid) = tab.claude_session_id.clone() else {
                return;
            };
            let cwd = model.resolved_spawn_cwd_for_pane(tab, pane);
            let _ = self.spawn_claude_pane(
                tab_id,
                &pane_id,
                &cwd,
                &ClaudeSessionMode::ResumeDeferred(sid),
                &[],
                settings_path,
                cx,
            );
            return;
        }
        if pane.kind != PaneKind::Terminal {
            return;
        }
        let cwd = model.resolved_spawn_cwd_for_pane(tab, pane);
        // R14: the extra-env hook threads NICE_SOCKET/NICE_TAB_ID/NICE_PANE_ID
        // onto this spec before spawn.
        let spec = SpawnSpec::shell(cwd);
        let _ = self.spawn_pane(tab_id, &pane_id, spec, cx);
    }

    /// Move key focus to the active pane's terminal (the focus-follow on tab /
    /// pane switch). No-op when the active pane has no live session (a model-only
    /// or not-yet-spawned pane has nothing to focus yet).
    pub(crate) fn focus_active_pane(
        &self,
        model: &TabModel,
        tab_id: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(tab) = model.tab_for(tab_id) else {
            return;
        };
        let Some(pane_id) = tab.active_pane_id.as_deref() else {
            return;
        };
        if let Some(session) = self.tabs.get(tab_id).and_then(|panes| panes.get(pane_id)) {
            window.focus(&session.focus, cx);
        }
    }

    /// The **full** Swift `setActivePane` behavior (`SessionsModel.swift:534-546`)
    /// — the live composition the slice-3 action seams call: the model half
    /// ([`set_active_pane`](Self::set_active_pane), which acknowledges a waiting
    /// pane on the viewed tab) plus the two gpui side effects it runs on top —
    /// [`ensure_active_pane_spawned`](Self::ensure_active_pane_spawned) (a
    /// deferred terminal companion spawns on first focus) and
    /// [`focus_active_pane`](Self::focus_active_pane) (key focus follows). The
    /// navigation steppers compose the same three pieces in the live app so the
    /// ack + spawn + focus ride along; the pure `set_active_pane` /
    /// `select_next_pane` methods are its unit-testable model half.
    pub(crate) fn activate_pane(
        &mut self,
        model: &mut TabModel,
        tab_id: &str,
        pane_id: &str,
        settings_path: Option<&str>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.set_active_pane(model, tab_id, pane_id);
        self.ensure_active_pane_spawned(model, tab_id, settings_path, cx);
        self.focus_active_pane(model, tab_id, window, cx);
    }

    /// Tear down every session this window owns. Dropping each
    /// [`TerminalSessionHandle`] tears its child process group down
    /// (SIGHUP→SIGKILL via `nice_term_core::Session::drop`), so no orphan zsh
    /// survives (the R3 teardown contract). Idempotent — the window-close hook
    /// calls it once, but app-terminate paths may double up. R18 extends this to
    /// flush the session snapshot first.
    pub(crate) fn teardown(&mut self) {
        self.tabs.clear();
        self.pane_launch_states.clear();
        self.synthetic_spawned.clear();
        self.synthetic_held.clear();
        self.synthetic_armed.clear();
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// The `<tab>:<pane>` key the synthetic spawned/held/armed sets index by, matching
/// Swift's `SessionsModel.syntheticPaneKey`.
fn synthetic_key(tab_id: &str, pane_id: &str) -> String {
    format!("{tab_id}:{pane_id}")
}

/// Clip a pane title to `max` **characters** (not bytes), trimming any trailing
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
fn default_mint_id(prefix: &str) -> String {
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
/// a SEPARATE mint from [`default_mint_id`]: tab/pane ids stay the time-sortable
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

/// How a Claude pane attaches to the Claude CLI's session layer. Ports Swift
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
    /// Restore path: don't run claude — spawn a plain `zsh -il` with
    /// `claude --resume <uuid>` pre-typed at the prompt via the stub's
    /// `print -z "$NICE_PREFILL_COMMAND"` tail. This is the only mode that needs
    /// `ZDOTDIR` + `NICE_PREFILL_COMMAND` in the pane env.
    ResumeDeferred(String),
}

/// Build the extra-env pairs for a **Claude** pane. Pure port of Swift
/// `TabPtySession.buildClaudeExtraEnv` (`TabPtySession.swift:875-902`).
///
/// The per-mode matrix is R14's FROZEN spec (R15 wired this function into the
/// live spawn path and may extend the signature — never the matrix): EVERY mode sets `TERM_PROGRAM`,
/// `NICE_TAB_ID`, `NICE_PANE_ID`, and `NICE_SOCKET` (when a socket exists) so the
/// SessionStart hook can reach Nice; ONLY [`ResumeDeferred`](ClaudeSessionMode::ResumeDeferred)
/// adds `ZDOTDIR` (when set), the always-present `NICE_USER_ZDOTDIR` (empty when
/// none), and the `NICE_PREFILL_COMMAND` the stub's `print -z` tail pre-types.
///
/// Was production-unused before R15; R15's [`spawn_claude_pane`](SessionManager::spawn_claude_pane)
/// now wires it as the live env composer for every Claude pane spawn.
/// `settings_path` is threaded now but always `None` until
/// R17 fills R15's theme-sync provider; when `Some`, it splices a single-quoted
/// `--settings <path>` before `--resume` in the prefill line (theme parity for a
/// deferred-resumed session), matching the Swift source byte-for-byte.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_claude_extra_env(
    mode: &ClaudeSessionMode,
    tab_id: &str,
    pane_id: &str,
    socket_path: Option<&str>,
    zdotdir_path: Option<&str>,
    user_zdotdir: Option<&str>,
    settings_path: Option<String>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("TERM_PROGRAM".to_string(), "ghostty".to_string()),
        ("NICE_TAB_ID".to_string(), tab_id.to_string()),
        ("NICE_PANE_ID".to_string(), pane_id.to_string()),
    ];
    if let Some(sp) = socket_path {
        env.push(("NICE_SOCKET".to_string(), sp.to_string()));
    }
    if let ClaudeSessionMode::ResumeDeferred(session_id) = mode {
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
            build_claude_prefill_command(settings_path.as_deref(), session_id),
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
pub(crate) fn build_claude_prefill_command(settings_path: Option<&str>, session_id: &str) -> String {
    let settings_arg = settings_path
        .map(|p| format!(" --settings {}", nice_term_core::shell_single_quote(p)))
        .unwrap_or_default();
    format!("claude{settings_arg} --resume {session_id}")
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
        // between `--session-id`/`--resume` and their UUID.
        if let Some(sp) = settings_path {
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
    /// Open a new sidebar tab — reply `newtab`.
    NewTab,
    /// Promote the requesting pane in place. `parsed_from_args` is true when the
    /// client's `args` already carried the session id (`--resume`/`--session-id`),
    /// which selects the bare `inplace` / `-` placeholder forms; `session_id` is
    /// the resolved id (parsed, or a freshly minted UUID) the wrapper prepends.
    InPlace {
        parsed_from_args: bool,
        session_id: String,
    },
}

/// Compose the socket `claude` reply — the FROZEN R14 grammar (≤3
/// whitespace-separated positional fields). Pure port of the reply tail of
/// Swift `handleClaudeSocketRequest` (`SessionsModel.swift:897-910`); an
/// R15-owned protocol composer.
///
/// The four byte-exact variants:
/// - `newtab`
/// - `inplace` — in-place, args already carried the id, theme sync off
/// - `inplace <uuid>` — in-place, minted id, theme sync off
/// - `inplace <uuid|-> <path>` — theme sync on: the third field is the
///   `--settings` path the wrapper splices; the second is the minted uuid, or
///   `-` when the client's args already named the session.
///
/// `settings_path` is the injectable theme-sync provider's output (R17 fills
/// it; `None` until then). With `settings_path == None` the replies are
/// byte-identical to the two shorter forms.
pub(crate) fn compose_claude_reply(
    decision: &ClaudeReplyDecision,
    settings_path: Option<&str>,
) -> String {
    match decision {
        ClaudeReplyDecision::NewTab => "newtab".to_string(),
        ClaudeReplyDecision::InPlace {
            parsed_from_args,
            session_id,
        } => match settings_path {
            Some(path) => {
                // `-` sid placeholder when the client's args already carry the
                // session, so the pointer can follow as the 3rd field; else the
                // freshly minted id.
                let sid_field = if *parsed_from_args {
                    "-"
                } else {
                    session_id.as_str()
                };
                format!("inplace {sid_field} {path}")
            }
            None => {
                if *parsed_from_args {
                    "inplace".to_string()
                } else {
                    format!("inplace {session_id}")
                }
            }
        },
    }
}

/// Split a Claude OSC title into its status prefix and the trailing label,
/// per the T5 grammar. Pure port of the status-prefix extraction in Swift
/// `paneTitleChanged`'s Claude branch (`SessionsModel.swift:439-453`): the
/// first Unicode scalar in `U+2800..=U+28FF` (braille spinner) ⇒
/// [`Thinking`](TabStatus::Thinking); exactly `U+2733` (✳ sparkle) ⇒
/// [`Waiting`](TabStatus::Waiting); anything else ⇒ no status change and the
/// whole string is the label.
///
/// Returns `(status, label)` where `label` is the input with the status prefix
/// scalar removed (untrimmed — the caller trims, drops the empty / `Claude Code`
/// placeholder, and feeds the rest to `apply_auto_title`; that wiring is R15
/// slice-3's `pane_title_changed` branch).
pub(crate) fn parse_claude_title(title: &str) -> (Option<TabStatus>, &str) {
    let Some(first) = title.chars().next() else {
        return (None, title);
    };
    let cp = first as u32;
    if (0x2800..=0x28FF).contains(&cp) {
        (Some(TabStatus::Thinking), &title[first.len_utf8()..])
    } else if cp == 0x2733 {
        (Some(TabStatus::Waiting), &title[first.len_utf8()..])
    } else {
        (None, title)
    }
}

/// Where [`SessionManager::create_claude_tab`] puts the new tab — the two Swift
/// call sites' only real divergence (`SessionsModel.swift:650-714, :758-794`).
pub(crate) enum ClaudeTabPlacement {
    /// The socket `newtab` path (Swift `createTabFromMainTerminal`): bucket the tab
    /// by `cwd` via [`TabModel::add_tab_to_projects`] (git-root / longest-prefix),
    /// title from `args`, `-w` worktree split honored.
    Bucket { cwd: String },
    /// The sidebar project-`+` path (Swift `createClaudeTabInProject`): append
    /// directly to `project_id`, title `"New tab"`, no worktree split, no extra args.
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

/// The Claude tab's title from its invocation `args` — Swift
/// `createTabFromMainTerminal`'s title closure (`SessionsModel.swift:653-659`):
/// join with spaces, take the first 40 chars, trim; an empty result (no args, or
/// all-whitespace) falls back to `"New tab"`. A third, independent 40-char cap
/// (pane pills clip at 40 too — [`PANE_TITLE_MAX`] — but separately).
fn claude_tab_title_from_args(args: &[String]) -> String {
    if args.is_empty() {
        return "New tab".to_string();
    }
    let joined = args.join(" ");
    let capped: String = joined.chars().take(40).collect();
    let trimmed = capped.trim();
    if trimmed.is_empty() {
        "New tab".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The Claude tab's `Tab.cwd` — Swift `createTabFromMainTerminal`'s `sessionCwd`
/// (`SessionsModel.swift:675-683`): when the user ran `claude -w <name>`, Claude
/// creates and runs inside a worktree at `<cwd>/.claude/worktrees/<sanitized>`
/// (`/`→`+` via [`TabModel::sanitize_worktree_name`]); otherwise the tab cwd is
/// `cwd`. The bucketing anchor (`project_path`) stays `cwd` regardless, so the
/// sidebar still buckets the tab under the parent project. The `-w`/`--worktree`
/// **space form** only is recognized (the extractor is landed in `nice-model`); the
/// `=` form is deliberately NOT a worktree while session-id takes both.
fn claude_worktree_cwd(cwd: &str, args: &[String]) -> String {
    match TabModel::extract_worktree_name(args) {
        Some(name) => {
            let sanitized = TabModel::sanitize_worktree_name(&name);
            format!("{}/.claude/worktrees/{}", cwd.trim_end_matches('/'), sanitized)
        }
        None => cwd.to_string(),
    }
}

/// The human-readable command string the launch overlay shows for a fresh Claude
/// pane — Swift `TabPtySession.launchDisplayCommand` (`TabPtySession.swift:618-634`):
/// deliberately skips the `zsh -ilc "exec …"` wrapper and the `--session-id <uuid>`
/// plumbing so the user sees what *they* asked for. `.resume` → `claude --resume`;
/// otherwise `claude` (no args) or `claude <user args>`. `.resumeDeferred` is
/// suppressed by the caller, so it never reaches here.
fn claude_launch_display_command(mode: &ClaudeSessionMode, extra_args: &[String]) -> String {
    match mode {
        ClaudeSessionMode::Resume(_) => "claude --resume".to_string(),
        _ => {
            if extra_args.is_empty() {
                "claude".to_string()
            } else {
                format!("claude {}", extra_args.join(" "))
            }
        }
    }
}

#[cfg(test)]
mod tests;
