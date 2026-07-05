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
//! ## Deliberately deferred (later R13 slices — do not add here)
//!
//! * exit / held / dissolve / terminate handlers + the `onTabBecameEmpty`
//!   dissolve cascade — **slice 2**; [`route_terminal_event`] leaves an
//!   `Exited` breadcrumb.
//! * the launch-overlay registry (`registerPaneLaunch` / `clearPaneLaunch`,
//!   grace seam) — **slice 2**; [`route_terminal_event`] leaves an
//!   `OutputStarted` breadcrumb.
//! * action-seam rewiring (sidebar `+` / strip `+` / ⌘T / pill select / close),
//!   the `cx.subscribe` that feeds [`route_terminal_event`] from a live entity,
//!   and the `session-lifecycle` live scenario — **slice 3**.
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

// The gpui spawn/focus primitives + a few pure helpers have no live caller until
// R13 slice 3 wires the action seams and the entity subscription to them; the
// model-routing methods below ARE exercised by this module's tests. Same
// seam-for-a-later-slice pattern as `window_state` / `sidebar_actions`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gpui::{App, Entity, FocusHandle, Window};

use nice_model::{PaneKind, TabModel};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{TerminalEvent, TerminalSessionHandle};

/// Terminal-pane pill titles clip at 40 chars so the toolbar pill never
/// overflows (`SessionsModel.swift:400-404`).
const PANE_TITLE_MAX: usize = 40;

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
        Self {
            tabs: HashMap::new(),
            mint_id: Box::new(default_mint_id),
        }
    }

    /// A manager with an injected id minter (the deterministic test seam).
    pub(crate) fn with_mint_id(mint: impl Fn(&str) -> String + 'static) -> Self {
        Self {
            tabs: HashMap::new(),
            mint_id: Box::new(mint),
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
    pub(crate) fn pane_title_changed(
        &mut self,
        model: &mut TabModel,
        tab_id: &str,
        pane_id: &str,
        title: &str,
    ) {
        // Read the pane's kind + lock facts, then drop the borrow before the
        // mutation (Swift reads `pane` then re-enters via `mutateTab`).
        let Some(tab) = model.tab_for(tab_id) else {
            return;
        };
        let Some(pane) = tab.panes.iter().find(|p| p.id == pane_id) else {
            return;
        };
        let kind = pane.kind;
        let title_manually_set = pane.title_manually_set;
        let is_claude_running = pane.is_claude_running;

        match kind {
            PaneKind::Terminal => {
                let trimmed = title.trim();
                // Whitespace-only titles never overwrite the current pill label.
                if trimmed.is_empty() {
                    return;
                }
                // A user pill-rename locks the title; OSC from the running program
                // (vim's `vim foo`, zsh theme spam) must not win.
                if title_manually_set {
                    return;
                }
                let clipped = clip_title(trimmed, PANE_TITLE_MAX);
                model.mutate_tab(tab_id, |tab| {
                    if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                        if pane.title != clipped {
                            pane.title = clipped;
                        }
                    }
                });
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
                    return;
                }
                // R15: status-prefix split + apply_auto_title land here.
            }
        }
    }

    /// Dispatch a decoded [`TerminalEvent`] from a pane's session entity to the
    /// right routing call. This is the pure connector the live entity
    /// subscription (slice 3) invokes per event; splitting it out keeps the
    /// routing unit-testable without a live pty or a gpui context.
    pub(crate) fn route_terminal_event(
        &mut self,
        model: &mut TabModel,
        tab_id: &str,
        pane_id: &str,
        event: &TerminalEvent,
    ) {
        match event {
            TerminalEvent::TitleChanged(title) => {
                self.pane_title_changed(model, tab_id, pane_id, title);
            }
            TerminalEvent::CwdChanged(path) => {
                // OSC 7 → `Pane.cwd` (plain path across the boundary; the app owns
                // the model type). The `to_string_lossy` is safe for the on-disk
                // absolute paths OSC 7 reports.
                let _ = self.pane_cwd_changed(model, tab_id, pane_id, &path.to_string_lossy());
            }
            TerminalEvent::TitleReset => {
                // The terminal title-policy (`SessionsModel.swift:391-414`) only
                // accepts a non-empty OSC *set*; a reset to the terminal default
                // carries no new label, so it is a no-op for the pane pill here.
            }
            TerminalEvent::OutputStarted => {
                // R13 slice 2: clear this pane's launch overlay (Swift's
                // `onPaneFirstOutput` → `clearPaneLaunch`).
            }
            TerminalEvent::Exited { .. } => {
                // R13 slice 2: `paneExited` / `paneHeld` — pane removal, neighbor
                // refocus, deferred-companion spawn, and the dissolve cascade.
            }
            // `TerminalEvent` is `#[non_exhaustive]`; a still-later lifecycle
            // variant reaches here until this manager learns to route it.
            _ => {}
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
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
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
