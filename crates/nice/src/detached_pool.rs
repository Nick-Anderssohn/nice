//! `DetachedPool` — the app-global pool of sessions that outlived their OS
//! window (tmux-port Phase 4, plan §P1/§P3).
//!
//! ## What it is, and deliberately is not
//!
//! Windows keep owning their ATTACHED sessions exactly as they always have:
//! each [`WindowState`](crate::window_state::WindowState) owns its own
//! `WorkspaceModel` + `PtyManager`, and that isolation is load-bearing for
//! multi-window correctness. This pool holds ONLY detached entries — the
//! sessions with no window to live in — so nothing about the ownership story of
//! an attached session changes (plan §P1: no wholesale "sessions become
//! app-level objects" migration).
//!
//! It is a gpui `Entity` behind a [`DetachedPoolGlobal`] handle rather than a
//! plain `Global` for one reason: plain globals are not observable, and every
//! window's sidebar has to re-render when the pool changes. Consumers
//! `cx.observe` the entity (the `FileOperationHistoryGlobal` precedent).
//!
//! ## Passive while pooled
//!
//! The source window's `pane_subscriptions` die with it and nobody
//! re-subscribes, so a pooled session emits no status updates and its rows
//! render a static detached glyph. That is mechanically safe because
//! `TerminalSessionHandle` is view-independent: the feeder thread keeps reading
//! the pty and the drain task is held by the entity on the app executor, so a
//! pooled child never blocks on a full buffer and subscriber-less `cx.emit`s
//! drop harmlessly. Missed transitions are discovered lazily at adopt, where
//! liveness is re-read from the handles (see [`DetachedPtys::has_live`]).
//!
//! ## Persistence is write-through
//!
//! Every mutation here writes the whole bucket into the session-store CACHE via
//! [`crate::session_store::set_detached`] (debounced, the `save_to_store`
//! pattern), so EVERY existing flush point covers the pool for free (plan
//! review I5). The plan's "persist the pool" steps in the close and adopt paths
//! are therefore ordering guarantees, not extra writes.
//!
//! ## Absent ⇒ no-op
//!
//! The global is installed by `app::run` only ([`install`]). Scenario- and
//! `run_selftest`-built states have no pool, so every consumer reads it through
//! [`pool`] and no-ops / renders nothing when it is absent — the hermeticity
//! rule, and the optional-observer precedent the sidebar and keymap already
//! follow.
//!
//! Every door is wired now: the close-path detach, the window-less quit check,
//! the sidebar's Detached section, adoption, and tear-off's synthetic entry. The
//! module-wide `dead_code` allow stays only for the few reads the tests exercise
//! directly (`len`, `is_empty`, `contains`, `DetachedProject::is_terminals`) —
//! the shipped paths reach the pool through the drivers below.

#![allow(dead_code)]

use std::collections::HashSet;

use gpui::{App, AppContext, Context, Entity, Global};

use nice_model::{Session, WorkspaceModel};

use crate::pty_manager::{DetachedPtys, DissolveTerminus};
use crate::session_store::{
    PersistedDetachedProject, PersistedDetachedSession, PersistedState,
};
use crate::window_state::WindowState;

/// Where a detached session came from, so adoption can re-home it (plan §P3).
/// A runtime value type, kept separate from the frozen serde shape
/// [`PersistedDetachedProject`] the way the model types are kept separate from
/// the persisted leaves.
///
/// `is_terminals` is deliberately NOT stored: it re-derives from
/// `id == WorkspaceModel::TERMINALS_PROJECT_ID`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetachedProject {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: String,
}

impl DetachedProject {
    /// Whether this is the pinned Terminals group (which every window seeds, so
    /// adoption reuses the target's own rather than creating a project).
    pub(crate) fn is_terminals(&self) -> bool {
        self.id == WorkspaceModel::TERMINALS_PROJECT_ID
    }

    fn snapshot(&self) -> PersistedDetachedProject {
        PersistedDetachedProject {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
        }
    }

    fn hydrate(persisted: &PersistedDetachedProject) -> Self {
        DetachedProject {
            id: persisted.id.clone(),
            name: persisted.name.clone(),
            path: persisted.path.clone(),
        }
    }
}

/// One pooled session: the model subtree, its live pty payload, and the project
/// it was detached out of (plan §P3).
///
/// There is no order field — the pool's `Vec` position IS the order (most
/// recently detached first), and that array order is what persists.
pub(crate) struct DetachedEntry {
    /// The whole model subtree, removed verbatim from its source window's
    /// `WorkspaceModel`.
    pub(crate) session: Session,
    /// The live half — opaque, owned by `pty_manager`. Empty for a structural
    /// entry (post-relaunch rows, and model-alive-but-ptyless sessions).
    pub(crate) ptys: DetachedPtys,
    /// Provenance for re-homing on adopt.
    pub(crate) project: DetachedProject,
}

impl DetachedEntry {
    /// A structural entry — model subtree only, no live child (plan §P2/§P6).
    pub(crate) fn structural(session: Session, project: DetachedProject) -> Self {
        DetachedEntry {
            session,
            ptys: DetachedPtys::empty(),
            project,
        }
    }

    /// Whether this row has a live child behind it (see
    /// [`DetachedPtys::has_live`]).
    pub(crate) fn has_live(&self, cx: &App) -> bool {
        self.ptys.has_live(cx)
    }

    fn snapshot(&self) -> PersistedDetachedSession {
        PersistedDetachedSession {
            project: self.project.snapshot(),
            session: nice_model::PersistedSession::from_model(&self.session),
        }
    }
}

/// The app-global pool of detached sessions. Order is most recently detached
/// first: [`push_front`](Self::push_front) prepends and
/// [`take_head`](Self::take_head) pops that head (the adopt-latest chord).
#[derive(Default)]
pub(crate) struct DetachedPool {
    entries: Vec<DetachedEntry>,
}

impl DetachedPool {
    /// An empty pool (the pre-hydrate state, and what tests build directly).
    pub(crate) fn new() -> Self {
        DetachedPool {
            entries: Vec::new(),
        }
    }

    // MARK: - reads

    pub(crate) fn entries(&self) -> &[DetachedEntry] {
        &self.entries
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the pool holds this session id — the target-side half of the
    /// duplicate-id guard (the model-side half refuses in `adopt_entry`).
    pub(crate) fn contains(&self, session_id: &str) -> bool {
        self.entries.iter().any(|e| e.session.id == session_id)
    }

    /// Whether ANY pooled entry still has a live child. This is what keeps the
    /// app alive window-less (D5): quitting with this `true` destroys running
    /// processes, so the quit path must confirm rather than cascade.
    pub(crate) fn has_live(&self, cx: &App) -> bool {
        self.entries.iter().any(|e| e.has_live(cx))
    }

    /// The persist shape of the whole bucket, in pool order.
    pub(crate) fn snapshot(&self) -> Vec<PersistedDetachedSession> {
        self.entries.iter().map(DetachedEntry::snapshot).collect()
    }

    // MARK: - mutations (each notifies + writes through)

    /// Push a freshly detached entry onto the head of the pool.
    pub(crate) fn push_front(&mut self, entry: DetachedEntry, cx: &mut Context<Self>) {
        self.entries.insert(0, entry);
        self.did_mutate(cx);
    }

    /// Remove and RETURN the entry with this session id, for adoption. `None`
    /// when the id is not pooled — which is the same row clicked twice across a
    /// notify gap, and must stay a silent no-op rather than a double adopt.
    pub(crate) fn take(&mut self, session_id: &str, cx: &mut Context<Self>) -> Option<DetachedEntry> {
        let idx = self.entries.iter().position(|e| e.session.id == session_id)?;
        let entry = self.entries.remove(idx);
        self.did_mutate(cx);
        Some(entry)
    }

    /// Remove and RETURN the most recently detached entry (the adopt-latest
    /// chord). `None` on an empty pool.
    pub(crate) fn take_head(&mut self, cx: &mut Context<Self>) -> Option<DetachedEntry> {
        if self.entries.is_empty() {
            return None;
        }
        let entry = self.entries.remove(0);
        self.did_mutate(cx);
        Some(entry)
    }

    /// Drop a pooled entry outright — dropping its [`DetachedPtys`] SIGHUPs the
    /// child's process group, exactly as dropping a `PaneState` does inside a
    /// window. Returns whether anything was removed.
    pub(crate) fn kill(&mut self, session_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(idx) = self.entries.iter().position(|e| e.session.id == session_id) else {
            return false;
        };
        self.entries.remove(idx);
        self.did_mutate(cx);
        true
    }

    /// Drop every pooled entry (the quit cascade's teardown, after the store
    /// cache already holds the bucket — write-through means the persisted rows
    /// survive the process, the children do not).
    pub(crate) fn clear_live(&mut self, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.clear();
        cx.notify();
    }

    /// Notify observers (every window's sidebar) and write the bucket through
    /// into the store cache.
    fn did_mutate(&self, cx: &mut Context<Self>) {
        cx.notify();
        crate::session_store::set_detached(self.snapshot());
    }
}

// MARK: - the producer side (detach)

/// Whether closing a window should DETACH its running sessions rather than kill
/// them (D1) — the single source of truth both close-path halves read, so the
/// ⌘W confirmation decision and the actual extraction can never disagree.
///
/// Two terms, both no-ops in the same direction:
///
/// * a pool must be installed. `run_selftest` and the scenarios install none
///   (the hermeticity rule), so every landed close test keeps its pre-Phase-4
///   behaviour;
/// * the Settings ▸ Advanced toggle must be ON. Absent settings read as ON, the
///   shipped default (see `SettingsPrefsStore::close_window_detaches`).
pub(crate) fn detach_on_close_enabled(cx: &App) -> bool {
    pool(cx).is_some()
        && cx
            .try_global::<crate::settings::prefs_store::SettingsPrefsStore>()
            .map_or(true, |s| s.close_window_detaches())
}

/// The detach-eligibility partition (plan §P2), in sidebar order and pure so the
/// matrix is unit-testable: a session is eligible when **the model says it is
/// alive** — at least one of its term windows carries `is_alive`. That is the
/// same fold the close confirmation counts
/// ([`WorkspaceModel::live_window_counts`]).
///
/// The subtle half, and the reason this predicate is model-side rather than
/// pty-side: the fold DELIBERATELY counts a restored-but-never-activated window
/// as alive (hydration sets `is_alive: true`). Those sessions have no pty at all,
/// and they detach as STRUCTURAL entries — the exact shape the post-relaunch pool
/// already uses, so no new machinery. Detaching them is what keeps D1's
/// no-confirm close non-destructive: today they make the live count nonzero and
/// earn a confirmation, so silently `Remove`-ing them under a silent close would
/// NARROW the safety net for precisely the resumable-Claude rows detach exists to
/// protect.
///
/// Only a model-DEAD session (every window held or exited) is ineligible; it
/// follows today's `Remove` fate.
pub(crate) fn detach_eligible_session_ids(model: &WorkspaceModel) -> Vec<String> {
    model
        .projects
        .iter()
        .flat_map(|p| p.sessions.iter())
        .filter(|s| s.windows.iter().any(|w| w.is_alive))
        .map(|s| s.id.clone())
        .collect()
}

/// Extract `session_id` out of `state`'s window and push it onto the pool head —
/// the one driver both detach doors share (the close-path bulk detach and the
/// explicit Detach Session action).
///
/// Returns the [`DissolveTerminus`] the extraction left behind (whether the
/// window now holds no sessions at all), or `None` when there is no pool
/// installed or the window does not own that session. The window actuation for a
/// `WindowEmptied` terminus is the CALLER's (plan §P7) — the close path is
/// already closing, and the explicit action routes through the normal window
/// close so the pool-aware quit check stays in charge.
///
/// The pool push writes the bucket through into the store cache on the spot
/// (write-through, plan review I5), which is what makes the close path's single
/// flush cover the detached rows.
pub(crate) fn detach_into_pool(
    cx: &mut App,
    state: &Entity<WindowState>,
    session_id: &str,
) -> Option<DissolveTerminus> {
    let pool = pool(cx)?;
    // The extraction runs inside — and finishes with — the window's own update,
    // so the entity lease is released before the pool mutation re-enters the app.
    let (entry, terminus) = state.update(cx, |ws, wcx| {
        let out = ws.detach_session(session_id, wcx);
        if out.is_some() {
            wcx.notify();
        }
        out
    })?;
    pool.update(cx, |p, pcx| p.push_front(entry, pcx));
    Some(terminus)
}

// MARK: - the consumer side (adopt / kill)

/// Adopt the pooled entry named `session_id` into `state`'s window — the one
/// driver behind a sidebar row click, the context menu's "Adopt into This
/// Window", and (through [`adopt_head_into`]) the ⌃⌘A chord.
///
/// Returns whether a session was adopted. Every failure is a SILENT no-op that
/// leaves the pool exactly as it was:
///
/// * no pool installed (`run_selftest` / the scenarios);
/// * the id is no longer pooled — the SAME row clicked in two windows across a
///   notify gap. gpui's single-threaded main loop makes the second click's take
///   return `None`, and it must stay a no-op rather than a double adopt (review
///   N-r2-6c);
/// * the target window already owns that session id, which is checked BEFORE the
///   take so the refusal never round-trips the row out of the pool and back.
///
/// The take writes the shrunken bucket into the store cache and `adopt_entry`'s
/// `upsert` joins it in the same debounced batch — the §P6/B3b ordering, which is
/// what keeps a hard kill from finding the session in both buckets at once.
pub(crate) fn adopt_into(cx: &mut App, state: &Entity<WindowState>, session_id: &str) -> bool {
    let Some(pool) = pool(cx) else {
        return false;
    };
    if state.read(cx).workspace.session_for(session_id).is_some() {
        // Unreachable in the shipped flow — detach mints a process-unique id, so
        // no pooled row can name a session the target already owns. It stays as a
        // guard because a corrupt/hand-edited `sessions.json` can still say
        // otherwise, and a refusal here is a click that visibly does NOTHING:
        // say so on the console rather than leaving a dead row.
        eprintln!(
            "nice: refusing to adopt '{session_id}' — the target window already owns that session id"
        );
        return false;
    }
    let Some(entry) = pool.update(cx, |p, pcx| p.take(session_id, pcx)) else {
        return false;
    };
    settle_adoption(cx, state, &pool, entry)
}

/// Adopt the most recently detached entry (the pool head) into `state`'s window —
/// the ⌃⌘A chord's driver. `false` on an empty or absent pool.
pub(crate) fn adopt_head_into(cx: &mut App, state: &Entity<WindowState>) -> bool {
    let Some(pool) = pool(cx) else {
        return false;
    };
    let Some(entry) = pool.update(cx, |p, pcx| p.take_head(pcx)) else {
        return false;
    };
    settle_adoption(cx, state, &pool, entry)
}

/// Hand an already-taken entry to the window, putting it BACK on the pool head if
/// the window refuses it (the duplicate-id guard). Refusing must never drop the
/// entry: dropping SIGHUPs a live child over what is a belt-and-braces check.
// The `Err` variant IS the entry — see `WindowState::adopt_entry`.
#[allow(clippy::result_large_err)]
fn settle_adoption(
    cx: &mut App,
    state: &Entity<WindowState>,
    pool: &Entity<DetachedPool>,
    entry: DetachedEntry,
) -> bool {
    match state.update(cx, |ws, wcx| ws.adopt_entry(entry, wcx)) {
        Ok(()) => true,
        Err(entry) => {
            eprintln!(
                "nice: window refused to adopt '{}' (duplicate session id) — putting it back on the pool",
                entry.session.id
            );
            pool.update(cx, |p, pcx| p.push_front(entry, pcx));
            false
        }
    }
}

/// Adopt the pooled entry into a WINDOW OF ITS OWN — the context menu's "Open in
/// New Window".
///
/// The entry is taken out of the pool FIRST and handed to the window construction
/// path, which seeds it before the window opens (plan §P9,
/// [`crate::app::open_managed_window_adopting`]) — the same door tear-off uses, so
/// there is one construction path rather than two. A stale click on a row another
/// window already adopted finds nothing to take and opens no window at all.
///
/// An open that fails puts the entry BACK on the pool head rather than dropping
/// it: dropping would SIGHUP a live child over a failure the user can retry.
pub(crate) fn adopt_into_new_window(cx: &mut App, session_id: &str) -> bool {
    let Some(pool) = pool(cx) else {
        return false;
    };
    let Some(entry) = pool.update(cx, |p, pcx| p.take(session_id, pcx)) else {
        return false;
    };
    match crate::app::open_managed_window_adopting(cx, entry) {
        Ok(()) => true,
        Err(entry) => {
            pool.update(cx, |p, pcx| p.push_front(entry, pcx));
            false
        }
    }
}

/// Drop a pooled entry outright — the context menu's Kill. Dropping its payload
/// SIGHUPs the child exactly as closing its session inside a window would, and
/// the pool's write-through takes the row off disk in the same beat.
pub(crate) fn kill_pooled(cx: &mut App, session_id: &str) -> bool {
    let Some(pool) = pool(cx) else {
        return false;
    };
    pool.update(cx, |p, pcx| p.kill(session_id, pcx))
}

/// A render-ready view of the pool: `(session_id, title, has_claude)` per row, in
/// pool order (most recently detached first) — what every window's sidebar paints
/// its Detached section from (§P12). Empty when no pool is installed, which is
/// what makes the section absent rather than special-cased in the scenarios.
pub(crate) fn pool_rows(cx: &App) -> Vec<(String, String, bool)> {
    let Some(pool) = pool(cx) else {
        return Vec::new();
    };
    pool.read(cx)
        .entries()
        .iter()
        .map(|e| {
            (
                e.session.id.clone(),
                e.session.title.clone(),
                e.session.has_claude(),
            )
        })
        .collect()
}

/// Whether the installed pool (if any) holds an entry with a live child — the
/// window-less-survival predicate (D5). An absent pool reads as `false`, so
/// scenario / `run_selftest` states behave exactly as they always have.
pub(crate) fn pool_has_live(cx: &App) -> bool {
    pool(cx).is_some_and(|p| p.read(cx).has_live(cx))
}

/// The pool's contribution to the ⌘Q live-window count `(claude, terminal)`
/// (plan §P10). Only entries whose child is still running contribute — a
/// structural row (post-relaunch, or a never-activated session) has no process to
/// destroy, so it must not inflate the confirmation or block the quit.
pub(crate) fn pool_live_window_counts(cx: &App) -> (usize, usize) {
    let Some(pool) = pool(cx) else {
        return (0, 0);
    };
    let (mut claude, mut terminal) = (0, 0);
    for entry in pool.read(cx).entries() {
        if !entry.has_live(cx) {
            continue;
        }
        for window in entry.session.windows.iter().filter(|w| w.is_alive) {
            match window.kind {
                nice_model::TermWindowKind::Claude => claude += 1,
                nice_model::TermWindowKind::Terminal => terminal += 1,
            }
        }
    }
    (claude, terminal)
}

// MARK: - launch hydration (the B3c duplicate-id reconcile)

/// Build the pool's entries from a persisted document, applying the launch-time
/// reconcile (plan §P4, review B3c).
///
/// Pure — no gpui, no store — so the reconcile is unit-tested directly.
///
/// **The reconcile:** a `detached[]` row whose `session.id` ALSO appears
/// somewhere under `windows[]` is dropped; the attached copy wins, because it
/// is the one with a window to live in. Without this, a hard kill inside a
/// close-or-adopt debounce window mints two live sessions with the same id, and
/// every id-keyed cross-window lookup (`WindowRegistry::state_for_*`) silently
/// returns whichever window matches first. Same belt-and-braces class as
/// `WorkspaceModel::dedupe_window_ids`, which exists because a persisted-id
/// invariant "that couldn't happen" did.
///
/// A duplicate id WITHIN `detached[]` is REPAIRED rather than dropped: the later
/// row is re-keyed to a fresh minted id and kept. Dropping it would destroy a
/// whole detached session's persisted record (its cwd, its `claude_session_id`,
/// its pane tree) to enforce an invariant that costs one mint to restore —
/// unacceptable for a bucket whose entire job is not losing sessions. Detach
/// itself now mints a process-unique id
/// ([`WindowState::detach_session`](crate::window_state::WindowState::detach_session)),
/// so this only fires for a file written before that or hand-edited since.
///
/// **`terminals-main` is re-keyed on sight, and never dropped.** Every fresh
/// window seeds that constant
/// ([`WorkspaceModel::MAIN_TERMINAL_SESSION_ID`]), so a pooled row holding it
/// would collide with the Main of whatever window later adopts it — and the
/// "drop it, the attached copy wins" rule is wrong for this one id, because two
/// windows' Mains are DIFFERENT sessions that merely share a name. Re-keying
/// keeps both.
///
/// Every hydrated entry is STRUCTURAL: processes cannot survive a quit (D2 — no
/// daemon), so a persisted row comes back as a re-attachable shell that
/// respawns on adopt-activate.
pub(crate) fn hydrate_entries(state: &PersistedState) -> Vec<DetachedEntry> {
    hydrate_entries_with(state, &mut || crate::pty_manager::default_mint_id("t"))
}

/// [`hydrate_entries`] with the id minter injected, so the duplicate repair is
/// unit-testable against a deterministic sequence.
pub(crate) fn hydrate_entries_with(
    state: &PersistedState,
    mint: &mut dyn FnMut() -> String,
) -> Vec<DetachedEntry> {
    let attached: HashSet<&str> = state
        .windows
        .iter()
        .flat_map(|w| w.projects.iter())
        .flat_map(|p| p.sessions.iter())
        .map(|s| s.id.as_str())
        .collect();

    // Seeded with the constant every fresh window mints for its own Main, so a
    // row carrying it is re-keyed by the same loop that handles a duplicate.
    let mut seen: HashSet<String> =
        HashSet::from([WorkspaceModel::MAIN_TERMINAL_SESSION_ID.to_string()]);
    let mut entries = Vec::new();
    for row in &state.detached {
        let is_seeded_main = row.session.id == WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
        if !is_seeded_main && attached.contains(row.session.id.as_str()) {
            continue;
        }
        let mut session = row.session.hydrate();
        while !seen.insert(session.id.clone()) {
            // Re-key until the id is free of BOTH buckets — the minter is
            // monotonic, so this settles on the first spin in practice.
            let mut fresh = mint();
            while attached.contains(fresh.as_str()) {
                fresh = mint();
            }
            session.id = fresh;
        }
        entries.push(DetachedEntry::structural(
            session,
            DetachedProject::hydrate(&row.project),
        ));
    }
    entries
}

// MARK: - the Global handle

/// The process handle to the pool entity. An `Entity` (not a plain `Global`)
/// because plain globals are not observable and every sidebar has to re-render
/// on a pool change.
pub(crate) struct DetachedPoolGlobal(pub(crate) Entity<DetachedPool>);

impl Global for DetachedPoolGlobal {}

/// The installed pool, if any. Absent in `run_selftest` / scenario states —
/// every consumer must treat `None` as "no detached sessions" rather than
/// unwrapping.
pub(crate) fn pool(cx: &App) -> Option<Entity<DetachedPool>> {
    cx.try_global::<DetachedPoolGlobal>().map(|g| g.0.clone())
}

/// Create + hydrate the pool and install it as the process global. Called from
/// `app::run` ONLY, immediately after the session store is installed and BEFORE
/// the restore fan-out (plan review F3).
///
/// **Why that boot point is safe, so nobody re-derives it:** the reconcile runs
/// against the store cache as loaded from disk. The fan-out's two mutators
/// cannot change its verdict — the ghost pre-pass and
/// `prune_empty_windows_keeping` both only drop session-LESS window slots,
/// which by definition contribute no session id to the attached set.
///
/// The reconciled bucket is written straight back through, so a launch that
/// dropped duplicate rows persists the repair instead of re-running it forever.
pub(crate) fn install(cx: &mut App) {
    let state = crate::session_store::load();
    let entries = hydrate_entries(&state);
    let pool = cx.new(|_| DetachedPool { entries });
    let snapshot = pool.read(cx).snapshot();
    crate::session_store::set_detached(snapshot);
    cx.set_global(DetachedPoolGlobal(pool));
}

/// Uninstall the pool global, dropping the entity — and with it every payload
/// still pooled, which SIGHUPs those children exactly as the quit cascade does.
///
/// `app::run` never calls this; the `detach-adopt` self-test scenario does, at
/// teardown, so the rest of the suite runs with no pool installed (the
/// hermeticity rule the `persistence-restore` scenario asserts). A no-op when
/// none is installed.
pub(crate) fn clear_global(cx: &mut App) {
    if cx.has_global::<DetachedPoolGlobal>() {
        cx.remove_global::<DetachedPoolGlobal>();
    }
}

#[cfg(test)]
mod adopt_tests;
#[cfg(test)]
mod tearoff_tests;
#[cfg(test)]
mod tests;
