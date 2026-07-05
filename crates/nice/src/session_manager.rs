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
use gpui::{App, Entity, FocusHandle, Window};

use nice_model::{PaneKind, SidebarTabSelection, TabModel, TabStatus};
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
    /// `WindowEmptied` wins (once the window is empty it stays empty).
    fn or(self, other: DissolveTerminus) -> DissolveTerminus {
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
        }
    }

    /// Mint a unique id for a freshly-created pane, via the injected seam.
    fn mint(&self, prefix: &str) -> String {
        (self.mint_id)(prefix)
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
                // R15: the Claude branch — split the braille-spinner (U+2800..
                // U+28FF → thinking) / sparkle (U+2733 → waiting) status prefix,
                // apply the status transition, and feed the trailing label into
                // the tab auto-title (dropping the "Claude Code" placeholder).
                // Gated on `is_claude_running`, which is `false` for all of R13
                // (only R15's socket promotion flips it), so the entire branch
                // drops now — no status, no OSC-driven tab title. R15 fills this
                // in without retrofitting the gate.
                if !is_claude_running {
                    return false;
                }
                // R15: status-prefix split + apply_auto_title land here.
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

        // Declared-but-inert subscriber hooks (later rows):
        //   * file-browser per-tab cleanup           → R19
        //   * project-pending-removal flag + row drop → R18 (Close Project owns it)
        //   * debounced session save (onSessionMutation) → R18

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

        // Every-project-empty terminus (Swift closes this window when another is
        // live, else quits the app). Project-row removal is R18 (inert above), so
        // an emptied project row still counts as empty here.
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

    /// Register an **empty** per-tab session container without spawning any pane
    /// — the claude-tab creation path this cycle, where the claude pane is
    /// model-only (no process until R15) and the companion terminal is deferred.
    /// It exists so [`ensure_active_pane_spawned`](Self::ensure_active_pane_spawned)'s
    /// "the tab already has a session" precondition holds when the user first
    /// focuses the deferred companion. Idempotent.
    pub(crate) fn register_tab_session(&mut self, tab_id: &str) {
        self.tabs.entry(tab_id.to_string()).or_default();
    }

    /// Spawn a live terminal session for `(tab_id, pane_id)` from `spec` and
    /// cache it with a fresh key-focus handle. Idempotent per `(tab, pane)`. The
    /// spawn-time extra-env hook (R14 injects `NICE_SOCKET` / `NICE_TAB_ID` /
    /// `NICE_PANE_ID`) rides on the caller-built `spec.env`.
    pub(crate) fn spawn_pane(
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

    /// Spawn the active pane's deferred pty if it was modelled up front — Swift's
    /// `ensureActivePaneSpawned` (`SessionsModel.swift:553-565`). Only for a
    /// **terminal-kind** active pane (claude-kind panes never lazy-spawn — they
    /// stay model-only until R15) whose tab already has a session container and
    /// whose pty isn't live yet. The spawn cwd resolves per-pane (last OSC 7,
    /// else the tab/project fallback). Never creates a tab container itself.
    pub(crate) fn ensure_active_pane_spawned(
        &mut self,
        model: &TabModel,
        tab_id: &str,
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
        if pane.kind != PaneKind::Terminal {
            return;
        }
        if !self.tab_has_session(tab_id) || self.has_pane(tab_id, &pane_id) {
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
        window: &mut Window,
        cx: &mut App,
    ) {
        self.set_active_pane(model, tab_id, pane_id);
        self.ensure_active_pane_spawned(model, tab_id, cx);
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

#[cfg(test)]
mod tests;
