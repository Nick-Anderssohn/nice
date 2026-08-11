//! `WorkspaceModel` — Nice's per-window document — ported from
//! `Sources/Nice/State/TabModel.swift`. The projects/sessions/windows value tree
//! plus which session is selected, with all the tree mutation, cwd
//! bucketing/repair, renames + title locks, depth-1 `/branch`+handoff lineage,
//! and the arg parsers. Pure value-tree: nothing here spawns a process, opens
//! a socket, or writes to disk. The model's only impurities — existence probes
//! and the home-dir lookup — go through the injected [`FsProbe`] seam.
//!
//! The pinned "Terminals" group at the top of the sidebar is a regular
//! [`Project`] with the reserved id [`WorkspaceModel::TERMINALS_PROJECT_ID`]; it is
//! always present at index 0 and cannot be removed by the user, but its sessions
//! are ordinary terminal-only sessions.
//!
//! ## The did-mutate signal (`onTreeMutation` port)
//!
//! Swift's `onTreeMutation` closure + `@Observable` write-back are consolidated
//! here into one explicit "did-mutate" signal (`set_on_tree_mutation`). The
//! observer is wired once per window to the debounced session save (BUGHUNT1-D),
//! so the rule is now structural: **every `&mut self` method that changes
//! persisted state fires it**, and read-only / pure helpers never do. Most
//! change-guarded mutators still fire exactly once on a real change and not at
//! all on a no-op. Two mutators fire without proving a change:
//! [`WorkspaceModel::mutate_session`] (it cannot see whether the caller's transform
//! actually changed anything, so it fires whenever the session is found) and
//! [`WorkspaceModel::repair_project_structure`] (it runs at boot before the observer
//! is wired, so its unconditional fire costs nothing in practice). Those
//! spurious fires are tolerated — the save is debounced. The only mutation that
//! deliberately does NOT fire is the selection-side
//! `acknowledge_waiting_on_active_window`, whose sole write is the runtime-only,
//! non-persisted `waiting_acknowledged` flag.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::term_window::{TermWindow, TermWindowKind};
use crate::project::Project;
use crate::session::Session;

/// The filesystem seam. The model's only impurities are existence probes
/// (`.git` discovery, worktree-cwd liveness) and the home-dir lookup for
/// tilde-expansion (`TabModel.swift:1099, 928, 948, 996, 1024, 1072-1074`).
/// Production uses [`StdFs`] (`std::fs`); tests inject a fake so they never
/// touch the real disk (the Swift tests plant real temp dirs; the seam lets
/// the Rust ports stay hermetic).
pub trait FsProbe {
    /// Whether a filesystem entry exists at `path` (a `.git` marker, or a
    /// session/project cwd). Mirrors `FileManager.default.fileExists(atPath:)`.
    fn exists(&self, path: &str) -> bool;
    /// The user's home directory, for tilde-expansion. Mirrors
    /// `NSHomeDirectory()`.
    fn home(&self) -> String;
}

/// Production [`FsProbe`] backed by `std::fs` / the `HOME` environment.
struct StdFs;

impl FsProbe for StdFs {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }
    fn home(&self) -> String {
        std::env::var("HOME").unwrap_or_default()
    }
}

/// The per-window document. `projects` is public (mirroring the Swift `var
/// projects`) so callers/tests can seed and read the tree directly, but
/// `active_session_id` is private — every write goes through [`WorkspaceModel::select_session`]
/// (or the internal navigation setter) so the selection side effects can't be
/// skipped by a stray field assignment.
pub struct WorkspaceModel {
    /// The sidebar's project sections, in display order. The pinned Terminals
    /// group is expected at index 0 (kept there by
    /// [`WorkspaceModel::ensure_terminals_project_seeded`]).
    pub projects: Vec<Project>,
    /// Currently-selected session. Private: the only writer is
    /// [`WorkspaceModel::select_session`] / the navigation stepper, which carry the
    /// Swift `didSet` side effects (acknowledge waiting on the target's active
    /// window + fire the did-mutate signal, only when the id actually changed —
    /// `TabModel.swift:43-53`).
    active_session_id: Option<String>,
    /// The filesystem seam (existence + home).
    fs: Box<dyn FsProbe>,
    /// The did-mutate signal. `RefCell` so it can fire while the tree is
    /// borrowed for the mutation that triggered it (the callback never
    /// re-enters the model, exactly like the Swift closure).
    on_tree_mutation: Option<RefCell<Box<dyn FnMut()>>>,
}

impl WorkspaceModel {
    /// Reserved id for the pinned Terminals project at index 0 of `projects`.
    /// The project is always present and cannot be deleted by the user; its
    /// sessions are ordinary terminal-only sessions.
    pub const TERMINALS_PROJECT_ID: &'static str = "terminals";
    /// Stable id for the default "Main" session seeded into the Terminals project
    /// on fresh launches.
    pub const MAIN_TERMINAL_SESSION_ID: &'static str = "terminals-main";

    // MARK: - Construction

    /// Seed a fresh window: a pinned Terminals project at index 0 holding one
    /// "Main" session with a single "Terminal 1" window, `next_terminal_index = 2`,
    /// and the Main session selected (`TabModel.swift:63-87`). Uses the production
    /// [`StdFs`] seam.
    pub fn new(initial_main_cwd: impl Into<String>) -> Self {
        Self::with_fs(initial_main_cwd, Box::new(StdFs))
    }

    /// [`WorkspaceModel::new`] with a caller-supplied [`FsProbe`] (tests inject a
    /// fake so existence/home lookups are deterministic and disk-free).
    pub fn with_fs(initial_main_cwd: impl Into<String>, fs: Box<dyn FsProbe>) -> Self {
        let initial_main_cwd = initial_main_cwd.into();
        let main_session_id = Self::MAIN_TERMINAL_SESSION_ID;
        let window_id = mint_window_id(main_session_id);
        let window = TermWindow::new(window_id.clone(), "Terminal 1", TermWindowKind::Terminal);
        let mut main_session = Session::new(main_session_id, "Main", &initial_main_cwd);
        main_session.windows = vec![window];
        main_session.active_window_id = Some(window_id);
        main_session.next_terminal_index = 2;
        let terminals = Project {
            id: Self::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: initial_main_cwd,
            sessions: vec![main_session],
        };
        WorkspaceModel {
            projects: vec![terminals],
            // Init assignment does not run the `didSet` (Swift initializers
            // don't); no acknowledge, no mutation event.
            active_session_id: Some(main_session_id.to_string()),
            fs,
            on_tree_mutation: None,
        }
    }

    /// Construct a document from already-hydrated `projects` + a saved
    /// `active_session_id`, WITHOUT seeding a Terminals/Main session. The restore
    /// constructor: [`WorkspaceModel::new`]/[`WorkspaceModel::with_fs`] always seed a fresh
    /// Terminals project + Main session, which restore must NOT do — it trusts the
    /// saved grouping (`WindowSession.restoreSavedWindow` rebuilds from the
    /// persisted projects). Like the initializers, the `active_session_id`
    /// assignment does not run the `didSet` side effects (no acknowledge, no
    /// mutation event) — the caller runs restore's single explicit save.
    pub fn from_parts(
        projects: Vec<Project>,
        active_session_id: Option<String>,
        fs: Box<dyn FsProbe>,
    ) -> Self {
        WorkspaceModel {
            projects,
            active_session_id,
            fs,
            on_tree_mutation: None,
        }
    }

    /// [`WorkspaceModel::from_parts`] with the production [`StdFs`] probe — the R18
    /// restore call site (`crate::window_state::WindowState::with_seed`) has no
    /// injected fs, so this is the disk-backed default.
    pub fn from_parts_std(projects: Vec<Project>, active_session_id: Option<String>) -> Self {
        Self::from_parts(projects, active_session_id, Box::new(StdFs))
    }

    /// Snapshot of this window's live windows grouped by kind — the quit /
    /// window-close confirmation counting rule (`TabModel.swift:186-200`). A
    /// pure fold over `window.is_alive`: BOTH kinds count, held (not-alive) windows
    /// don't, and modelled-but-unspawned windows (a restored window hydrates
    /// `is_alive = true`) DO — the Swift quirk, preserved deliberately.
    pub fn live_window_counts(&self) -> (usize, usize) {
        let mut claude = 0;
        let mut terminal = 0;
        for project in &self.projects {
            for session in &project.sessions {
                for window in session.windows.iter().filter(|w| w.is_alive) {
                    match window.kind {
                        TermWindowKind::Claude => claude += 1,
                        TermWindowKind::Terminal => terminal += 1,
                    }
                }
            }
        }
        (claude, terminal)
    }

    /// Install the did-mutate observer. Replaces any previously-installed one.
    pub fn set_on_tree_mutation(&mut self, cb: impl FnMut() + 'static) {
        self.on_tree_mutation = Some(RefCell::new(Box::new(cb)));
    }

    /// Fire the did-mutate signal once (no-op when no observer is installed).
    fn fire_mutation(&self) {
        if let Some(cell) = &self.on_tree_mutation {
            (cell.borrow_mut())();
        }
    }

    // MARK: - Lookup

    /// The currently-selected session id, if any.
    pub fn active_session_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }

    /// Look up a session by id across every project, including the pinned
    /// Terminals group (`TabModel.swift:93-100`).
    pub fn session_for(&self, id: &str) -> Option<&Session> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| s.id == id)
    }

    /// Project + session index for the session with id `id`, for in-place mutation
    /// (`TabModel.swift:132-139`).
    pub fn project_session_index(&self, id: &str) -> Option<(usize, usize)> {
        for (pi, project) in self.projects.iter().enumerate() {
            if let Some(ti) = project.sessions.iter().position(|s| s.id == id) {
                return Some((pi, ti));
            }
        }
        None
    }

    /// Mutate the session identified by `id` in place; returns true if the session was
    /// found (`TabModel.swift:120-128`). Fires the did-mutate signal whenever
    /// the session is found: this is the generic mutation path (it carries the OSC
    /// cwd/title changes), and it cannot see whether the caller's transform
    /// actually changed anything, so it fires unconditionally on a hit. A
    /// spurious fire on an unchanged re-delivery is tolerated — the save is
    /// debounced (BUGHUNT1-D, D4).
    pub fn mutate_session(&mut self, id: &str, transform: impl FnOnce(&mut Session)) -> bool {
        let found = self.mutate_session_silent(id, transform);
        if found {
            self.fire_mutation();
        }
        found
    }

    /// [`WorkspaceModel::mutate_session`] without firing the did-mutate signal — the
    /// internal path for a mutation that is not a persisted-state change. The
    /// only caller is `acknowledge_waiting_on_active_window`, whose sole write is
    /// the runtime-only `waiting_acknowledged` flag (dropped from the persisted
    /// snapshot); the selection change that triggers it already fires via
    /// [`WorkspaceModel::set_active_session_id`]. Returns true if the session was found.
    fn mutate_session_silent(&mut self, id: &str, transform: impl FnOnce(&mut Session)) -> bool {
        match self.project_session_index(id) {
            Some((pi, ti)) => {
                transform(&mut self.projects[pi].sessions[ti]);
                true
            }
            None => false,
        }
    }

    /// True when `session_id` lives inside the pinned Terminals project
    /// (`TabModel.swift:176-181`).
    pub fn is_terminals_project_session(&self, session_id: &str) -> bool {
        self.projects
            .iter()
            .find(|p| p.id == Self::TERMINALS_PROJECT_ID)
            .is_some_and(|t| t.sessions.iter().any(|x| x.id == session_id))
    }

    /// Flat list of sidebar session ids in displayed order — the pinned Terminals
    /// project first, then project sessions in project/then-session order. The single
    /// source of truth for keyboard navigation and the dissolve-selection
    /// fallback (`TabModel.swift:206-208`).
    pub fn navigable_sidebar_session_ids(&self) -> Vec<String> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|s| s.id.clone()))
            .collect()
    }

    /// The id of the session whose window list contains `window_id`, scanning every
    /// project including the pinned Terminals group (`TabModel.swift:211-220`).
    /// The reverse index the SessionStart hook's `session_update` uses to route a
    /// window's rotated session id / cwd back onto its owning session. Scoped to this
    /// `WorkspaceModel` — a per-window lookup, never a global index — so a window owned by
    /// a sibling window returns `None` here. Returns an owned id (the id must
    /// outlive later `&mut self` mutations the rotation handler makes).
    pub fn session_id_owning(&self, window_id: &str) -> Option<String> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| s.windows.iter().any(|window| window.id == window_id))
            .map(|s| s.id.clone())
    }

    /// The id of the session pinned to Claude session `claude_session_id`, in
    /// tree order — the window-free twin of
    /// [`session_id_owning`](Self::session_id_owning), for the events that
    /// identify a conversation rather than a window. A daemon-hosted background
    /// `/fork` is the first: its SessionStart relays a window id that belongs to
    /// whichever window happened to spawn the Claude daemon, so the forked-from
    /// conversation is resolvable only by its claude session id.
    ///
    /// Claude session ids are unique across sessions by construction (each is
    /// minted per session or adopted from a rotation), so the first match is the
    /// only match; a corrupt snapshot with duplicates resolves to the first in
    /// tree order rather than failing.
    pub fn session_id_for_claude_session(&self, claude_session_id: &str) -> Option<String> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| s.claude_session_id.as_deref() == Some(claude_session_id))
            .map(|s| s.id.clone())
    }

    // MARK: - Selection

    /// Select a session. The single `active_session_id` writer — carries the Swift
    /// `didSet` side effects.
    pub fn select_session(&mut self, id: &str) {
        self.set_active_session_id(Some(id.to_string()));
    }

    /// The sole `active_session_id` writer. When the id actually changes to a
    /// non-`None` value, dismiss the attention pulse on the target's active
    /// window and fire the did-mutate signal (`TabModel.swift:43-53`).
    fn set_active_session_id(&mut self, new: Option<String>) {
        if self.active_session_id == new {
            return;
        }
        self.active_session_id = new.clone();
        if let Some(id) = new {
            self.acknowledge_waiting_on_active_window(&id);
            self.fire_mutation();
        }
    }

    /// Move focus to the next sidebar session, wrapping. No-op when there's only
    /// one navigable session (`TabModel.swift:452, 457-463`).
    pub fn select_next_sidebar_session(&mut self) {
        self.step_sidebar_session(1);
    }

    /// Move focus to the previous sidebar session, wrapping.
    pub fn select_prev_sidebar_session(&mut self) {
        self.step_sidebar_session(-1);
    }

    fn step_sidebar_session(&mut self, offset: isize) {
        let ids = self.navigable_sidebar_session_ids();
        if ids.len() <= 1 {
            return;
        }
        let current_idx = self
            .active_session_id
            .as_ref()
            .and_then(|a| ids.iter().position(|x| x == a))
            .unwrap_or(0) as isize;
        let n = ids.len() as isize;
        let next_idx = (((current_idx + offset) % n + n) % n) as usize;
        self.set_active_session_id(Some(ids[next_idx].clone()));
    }

    /// Clear the waiting-attention pulse on whichever window is focused in
    /// `session_id` — the `active_session_id` `didSet` side effect
    /// (`TabModel.swift:468-475`).
    fn acknowledge_waiting_on_active_window(&mut self, session_id: &str) {
        // Silent: the only write is the runtime-only `waiting_acknowledged`
        // flag (not persisted), and the selection that triggers this already
        // fired via `set_active_session_id`.
        self.mutate_session_silent(session_id, |session| {
            if let Some(window_id) = session.active_window_id.clone() {
                if let Some(w) = session.windows.iter_mut().find(|w| w.id == window_id) {
                    w.mark_acknowledged_if_waiting();
                }
            }
        });
    }

    // MARK: - Reordering (sessions)

    /// Move `session_id` to a new slot within the same project, relative to
    /// `target_session_id`. No-op — and no event — when the sessions aren't in the same
    /// project, either id is unknown, the slot is illegal for the dragged session's
    /// lineage, or the move wouldn't change order. Sessions in the pinned Terminals
    /// project reorder internally but never leave it (cross-project is a no-op)
    /// (`TabModel.swift:485-500`).
    ///
    /// **Deliberate divergence from prod** (M7.8 feel-check round 3): Swift's
    /// `moveTab` moves a single row and ignores the depth-1 lineage, so
    /// dragging a parent strands its children and a foreign session can land
    /// inside another parent's group. Here the move is subtree-aware — see
    /// [`plan_session_move`] for the full slot legality rules:
    /// * a ROOT session moves with its entire child block, gathered contiguously;
    /// * a root can only land at another BLOCK's boundary (never interleaved,
    ///   never inside its own subtree);
    /// * a CHILD reorders among its own siblings only.
    pub fn move_session(&mut self, session_id: &str, target_session_id: &str, place_after: bool) {
        let (Some((sp, _)), Some((dp, _))) = (
            self.project_session_index(session_id),
            self.project_session_index(target_session_id),
        ) else {
            return;
        };
        if sp != dp {
            return;
        }
        let sessions = &mut self.projects[sp].sessions;
        let Some(order) = plan_session_move(sessions, session_id, target_session_id, place_after) else {
            return;
        };
        let mut old: Vec<Option<Session>> = sessions.drain(..).map(Some).collect();
        *sessions = order
            .iter()
            .map(|&i| old[i].take().expect("plan_session_move yields a permutation"))
            .collect();
        self.fire_mutation();
    }

    /// Mirrors [`WorkspaceModel::move_session`] without mutating — true iff the drop
    /// would actually reorder (`TabModel.swift:505-514`, with the same
    /// subtree-aware divergence as [`WorkspaceModel::move_session`]).
    pub fn would_move_session(&self, session_id: &str, target_session_id: &str, place_after: bool) -> bool {
        let (Some((sp, _)), Some((dp, _))) = (
            self.project_session_index(session_id),
            self.project_session_index(target_session_id),
        ) else {
            return false;
        };
        if sp != dp {
            return false;
        }
        plan_session_move(&self.projects[sp].sessions, session_id, target_session_id, place_after).is_some()
    }

    // MARK: - TermWindows: reorder / insert / extract

    /// Which window id should receive focus after the window at `idx` is removed
    /// (the post-removal array): prefer the window that slid into the freed slot,
    /// else the new last window, else `None`. Shared by [`WorkspaceModel::extract_window`]
    /// and the R13 process-exit path so a moved window and an exited window
    /// re-focus the same neighbor (`TabModel.swift:554-558`).
    pub fn neighbor_active_window_id(after_removing_index: usize, windows: &[TermWindow]) -> Option<String> {
        if after_removing_index < windows.len() {
            return Some(windows[after_removing_index].id.clone());
        }
        if after_removing_index > 0 {
            return Some(windows[after_removing_index - 1].id.clone());
        }
        None
    }

    /// Move `window_id` within session `session_id`'s window list, relative to
    /// `target_window_id`. No-op (no event) when the session is unknown, either window
    /// isn't in it, or the move wouldn't change order. Never touches
    /// `active_window_id` (`TabModel.swift:526-546`).
    pub fn move_window(
        &mut self,
        window_id: &str,
        session_id: &str,
        target_window_id: &str,
        place_after: bool,
    ) {
        if window_id == target_window_id {
            return;
        }
        let mut moved = false;
        if let Some((pi, ti)) = self.project_session_index(session_id) {
            let session = &mut self.projects[pi].sessions[ti];
            if let (Some(src), Some(dst)) = (
                session.windows.iter().position(|w| w.id == window_id),
                session.windows.iter().position(|w| w.id == target_window_id),
            ) {
                let mut insert_index = if place_after { dst + 1 } else { dst };
                if src < insert_index {
                    insert_index -= 1;
                }
                if insert_index != src {
                    let window = session.windows.remove(src);
                    session.windows.insert(insert_index, window);
                    moved = true;
                }
            }
        }
        if moved {
            self.fire_mutation();
        }
    }

    /// Mirrors [`WorkspaceModel::move_window`] without mutating (`TabModel.swift:648-657`).
    pub fn would_move_window(
        &self,
        window_id: &str,
        session_id: &str,
        target_window_id: &str,
        place_after: bool,
    ) -> bool {
        if window_id == target_window_id {
            return false;
        }
        let Some(session) = self.session_for(session_id) else {
            return false;
        };
        let (Some(src), Some(dst)) = (
            session.windows.iter().position(|w| w.id == window_id),
            session.windows.iter().position(|w| w.id == target_window_id),
        ) else {
            return false;
        };
        let mut insert_index = if place_after { dst + 1 } else { dst };
        if src < insert_index {
            insert_index -= 1;
        }
        insert_index != src
    }

    /// Remove `window_id` from session `session_id`, returning the removed [`TermWindow`] so a
    /// destination window can re-insert it. When the removed window was active,
    /// focus re-points to a neighbor via [`WorkspaceModel::neighbor_active_window_id`].
    /// Fires the did-mutate signal on a real removal; returns `None` (no
    /// mutation, no event) when the session or window isn't found
    /// (`TabModel.swift:572-587`).
    pub fn extract_window(&mut self, window_id: &str, session_id: &str) -> Option<TermWindow> {
        let mut removed = None;
        if let Some((pi, ti)) = self.project_session_index(session_id) {
            let session = &mut self.projects[pi].sessions[ti];
            if let Some(idx) = session.windows.iter().position(|w| w.id == window_id) {
                let was_active = session.active_window_id.as_deref() == Some(window_id);
                let r = session.windows.remove(idx);
                if was_active {
                    session.active_window_id = Self::neighbor_active_window_id(idx, &session.windows);
                }
                // tmux `last-window` must never bounce to a window that no longer
                // exists — drop the previous slot when it pointed at this one. The
                // structural re-point above deliberately does NOT go through
                // `Session::switch_active_window`: closing a window is not a user
                // switch, so it must not overwrite the bounce target.
                if session.prev_active_window_id.as_deref() == Some(window_id) {
                    session.prev_active_window_id = None;
                }
                removed = Some(r);
            }
        }
        if removed.is_some() {
            self.fire_mutation();
        }
        removed
    }

    /// Insert an externally-sourced `window` into session `session_id` relative to
    /// `target_window_id` (a `None`/unknown target appends). No-op (no event)
    /// when the session is unknown or already contains a window with this id. Does
    /// **not** change `active_window_id` (`TabModel.swift:598-613`).
    pub fn insert_window(
        &mut self,
        window: TermWindow,
        session_id: &str,
        target_window_id: Option<&str>,
        place_after: bool,
    ) {
        let mut inserted = false;
        if let Some((pi, ti)) = self.project_session_index(session_id) {
            let session = &mut self.projects[pi].sessions[ti];
            if !session.windows.iter().any(|w| w.id == window.id) {
                let insert_index = match target_window_id
                    .and_then(|t| session.windows.iter().position(|p| p.id == t))
                {
                    Some(t) => {
                        if place_after {
                            t + 1
                        } else {
                            t
                        }
                    }
                    None => session.windows.len(),
                };
                session.windows.insert(insert_index, window);
                inserted = true;
            }
        }
        if inserted {
            self.fire_mutation();
        }
    }

    /// The model half of window creation: append an auto-named terminal window to
    /// `session_id` and focus it, all in one mutation — counter read → "Terminal N"
    /// (or the explicit `title`) → increment. The counter increments
    /// unconditionally (an explicit title consumes the slot too), and only
    /// terminal-kind windows are constructible through this method — the
    /// ≤1-running-Claude creation edge (`SessionsModel.swift:603-626`).
    /// Returns the new window id, or `None` when the session isn't found. Fires the
    /// did-mutate signal on a real append (BUGHUNT1-D) so the new window persists
    /// by construction.
    pub fn add_window(
        &mut self,
        session_id: &str,
        new_window_id: impl Into<String>,
        title: Option<String>,
    ) -> Option<String> {
        let (pi, ti) = self.project_session_index(session_id)?;
        let new_window_id = new_window_id.into();
        let session = &mut self.projects[pi].sessions[ti];
        let n = session.next_terminal_index;
        let resolved_title = title.unwrap_or_else(|| format!("Terminal {}", n));
        session.windows
            .push(TermWindow::new(new_window_id.clone(), resolved_title, TermWindowKind::Terminal));
        session.active_window_id = Some(new_window_id.clone());
        session.next_terminal_index = n + 1;
        self.fire_mutation();
        Some(new_window_id)
    }

    // MARK: - Titles

    /// Default display title for a window of `kind`. Terminal windows use the session's
    /// monotonic `next_terminal_index` — the single source of truth
    /// [`WorkspaceModel::rename_window`]'s empty-submit reset also reads
    /// (`TabModel.swift:666-671`).
    pub fn default_window_title(kind: TermWindowKind, terminal_index: u32) -> String {
        match kind {
            TermWindowKind::Claude => "Claude".to_string(),
            TermWindowKind::Terminal => format!("Terminal {}", terminal_index),
        }
    }

    /// User-initiated rename for an individual window. **Non-empty** input sets
    /// the title and locks it (`title_manually_set = true`) so OSC titles can't
    /// clobber the user's choice. **Empty** input resets to the per-kind
    /// auto-default and clears the lock; for terminal windows the reset consumes
    /// and increments `next_terminal_index` (the monotonic-never-reuse policy —
    /// asymmetry 3) (`TabModel.swift:687-727`).
    pub fn rename_window(&mut self, session_id: &str, window_id: &str, new_title: &str) {
        let trimmed = new_title.trim();
        let mut changed = false;
        if let Some((pi, ti)) = self.project_session_index(session_id) {
            let session = &mut self.projects[pi].sessions[ti];
            if let Some(idx) = session.windows.iter().position(|w| w.id == window_id) {
                if trimmed.is_empty() {
                    // Empty submit: release the lock and recompute the
                    // auto-default. A terminal reset consumes the next slot from
                    // the monotonic counter (unconditionally — the increment
                    // happens before the change check, matching the Swift order).
                    let reset_title = match session.windows[idx].kind {
                        TermWindowKind::Claude => Self::default_window_title(TermWindowKind::Claude, 0),
                        TermWindowKind::Terminal => {
                            let n = session.next_terminal_index;
                            let t = Self::default_window_title(TermWindowKind::Terminal, n);
                            session.next_terminal_index = n + 1;
                            t
                        }
                    };
                    if session.windows[idx].title != reset_title || session.windows[idx].title_manually_set {
                        session.windows[idx].title = reset_title;
                        session.windows[idx].title_manually_set = false;
                        changed = true;
                    }
                } else if session.windows[idx].title != trimmed || !session.windows[idx].title_manually_set {
                    session.windows[idx].title = trimmed.to_string();
                    session.windows[idx].title_manually_set = true;
                    changed = true;
                }
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    /// User-initiated session rename from the sidebar editor. Trims whitespace,
    /// **ignores empty input** (a no-op — asymmetry 3, the mirror of
    /// [`WorkspaceModel::rename_window`]'s reset), and locks the title so
    /// [`WorkspaceModel::apply_auto_title`] skips it (`TabModel.swift:732-744`).
    pub fn rename_session(&mut self, id: &str, new_title: &str) {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return;
        }
        let mut changed = false;
        if let Some((pi, ti)) = self.project_session_index(id) {
            let session = &mut self.projects[pi].sessions[ti];
            if session.title != trimmed || !session.title_manually_set {
                session.title = trimmed.to_string();
                session.title_manually_set = true;
                changed = true;
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    /// Apply a Claude-generated session title, humanized into sentence case.
    /// Skipped entirely once the user has manually renamed the session, keyed on
    /// `session_id` so locking one session never affects another
    /// (`TabModel.swift:752-767`).
    pub fn apply_auto_title(&mut self, session_id: &str, raw_title: &str) {
        match self.session_for(session_id) {
            Some(t) if !t.title_manually_set => {}
            _ => return,
        }
        let humanized = humanize_session_title(raw_title);
        if humanized.is_empty() {
            return;
        }
        let mut changed = false;
        if let Some((pi, ti)) = self.project_session_index(session_id) {
            let session = &mut self.projects[pi].sessions[ti];
            if session.title != humanized {
                session.title = humanized;
                changed = true;
            }
            session.title_auto_generated = true;
        }
        if changed {
            self.fire_mutation();
        }
    }

    // MARK: - Project structure

    /// Guarantee a pinned Terminals project sits at `projects[0]`. Synthesize
    /// one (Main session + fresh "Terminal 1" window) when absent, or move it to
    /// index 0 when merely out of place. `spawn_hook` fires **exactly once**,
    /// with the synthesized Main session, only when the project had to be created
    /// from scratch — the one-way bridge into pty-aware callers
    /// (`TabModel.swift:803-839`).
    pub fn ensure_terminals_project_seeded(&mut self, spawn_hook: impl FnOnce(&Session)) {
        if let Some(idx) = self
            .projects
            .iter()
            .position(|p| p.id == Self::TERMINALS_PROJECT_ID)
        {
            if idx != 0 {
                let project = self.projects.remove(idx);
                self.projects.insert(0, project);
                // Reordering the pinned group changes the persisted layout.
                self.fire_mutation();
            }
            if self.active_session_id.is_none() {
                if let Some(first_id) = self.projects[0].sessions.first().map(|s| s.id.clone()) {
                    // `set_active_session_id` fires the did-mutate signal itself.
                    self.set_active_session_id(Some(first_id));
                }
            }
            return;
        }

        let cwd = self.fs.home();
        let main_session_id = Self::MAIN_TERMINAL_SESSION_ID;
        let window_id = mint_window_id(main_session_id);
        let window = TermWindow::new(window_id.clone(), "Terminal 1", TermWindowKind::Terminal);
        let mut main_session = Session::new(main_session_id, "Main", &cwd);
        main_session.windows = vec![window];
        main_session.active_window_id = Some(window_id);
        main_session.next_terminal_index = 2;
        let project = Project {
            id: Self::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: cwd,
            sessions: vec![main_session.clone()],
        };
        self.projects.insert(0, project);
        // Synthesizing the pinned Terminals project is a persisted change.
        // (`set_active_session_id` below fires again when it sets selection; the
        // duplicate schedule is harmless — the save is debounced.)
        self.fire_mutation();
        if self.active_session_id.is_none() {
            self.set_active_session_id(Some(main_session_id.to_string()));
        }
        spawn_hook(&main_session);
    }

    /// Look up `projects` by saved id; append a fresh empty `Project` with the
    /// saved name/path if absent. Returns the matched-or-appended index
    /// (`TabModel.swift:844-850`).
    pub fn ensure_project(&mut self, id: &str, name: &str, path: &str) -> usize {
        if let Some(i) = self.projects.iter().position(|p| p.id == id) {
            return i;
        }
        self.projects.push(Project {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            sessions: vec![],
        });
        // Appending a new project grouping is a persisted change.
        self.fire_mutation();
        self.projects.len() - 1
    }

    /// Find a non-Terminals project whose expanded `path` matches; else append
    /// a fresh project carrying the supplied id/name/path verbatim. Matches by
    /// filesystem path (distinct from [`WorkspaceModel::ensure_project`]'s id match),
    /// and never appends a second project with the reserved Terminals id
    /// (`TabModel.swift:623-643`).
    pub fn ensure_project_by_path(&mut self, id: &str, name: &str, path: &str) -> usize {
        if id == Self::TERMINALS_PROJECT_ID {
            if let Some(i) = self
                .projects
                .iter()
                .position(|p| p.id == Self::TERMINALS_PROJECT_ID)
            {
                return i;
            }
        }
        let expanded = self.expand_tilde(path);
        if let Some(i) = self.projects.iter().position(|p| {
            p.id != Self::TERMINALS_PROJECT_ID && self.expand_tilde(&p.path) == expanded
        }) {
            return i;
        }
        self.projects.push(Project {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            sessions: vec![],
        });
        // Appending a new project grouping is a persisted change.
        self.fire_mutation();
        self.projects.len() - 1
    }

    /// Bucket `session` into the project anchoring `cwd`'s git repo, creating one at
    /// the git root when none matches. Falls back to legacy longest-prefix
    /// matching (excluding Terminals) when `cwd` is not inside any git repo
    /// (`TabModel.swift:857-878`).
    pub fn add_session_to_projects(&mut self, session: Session, cwd: &str) {
        let normalized = self.expand_tilde(cwd);
        if let Some(git_root) = self.find_git_root(&normalized) {
            self.append_or_insert(session, &git_root);
            // Adding a session always changes persisted state.
            self.fire_mutation();
            return;
        }
        // No git root: legacy longest-prefix, excluding the pinned Terminals
        // group (whose path — typically $HOME — would prefix-match almost any
        // cwd and swallow new Claude sessions). Ties keep the first max, matching
        // Swift's `max(by:)`.
        let mut best: Option<(usize, usize)> = None;
        for (idx, p) in self.projects.iter().enumerate() {
            if p.id == Self::TERMINALS_PROJECT_ID {
                continue;
            }
            let ppath = self.expand_tilde(&p.path);
            if normalized.starts_with(&ppath) {
                let len = p.path.len();
                match best {
                    Some((_, blen)) if blen >= len => {}
                    _ => best = Some((idx, len)),
                }
            }
        }
        match best {
            Some((idx, _)) => self.projects[idx].sessions.push(session),
            None => self.append_new_project(&normalized, session),
        }
        // Adding a session always changes persisted state.
        self.fire_mutation();
    }

    /// Append `session` to the existing non-Terminals project rooted at `path`, or
    /// create a new project there (`TabModel.swift:882-888`).
    fn append_or_insert(&mut self, session: Session, path: &str) {
        if let Some(idx) = self.first_index_of_non_terminals_project_at(path) {
            self.projects[idx].sessions.push(session);
        } else {
            self.append_new_project(path, session);
        }
    }

    /// Index of the first non-Terminals project whose expanded `path` equals
    /// `path` (`TabModel.swift:893-898`).
    fn first_index_of_non_terminals_project_at(&self, path: &str) -> Option<usize> {
        self.projects.iter().position(|p| {
            p.id != Self::TERMINALS_PROJECT_ID && self.expand_tilde(&p.path) == path
        })
    }

    /// Append a fresh project rooted at `path`, deriving the display name from
    /// the last path component. A unique suffix (Swift uses a UUID prefix)
    /// keeps back-to-back appends in the same instant from colliding on `id`
    /// (`TabModel.swift:904-910`).
    fn append_new_project(&mut self, path: &str, session: Session) {
        let dir_name = last_path_component(path).to_uppercase();
        let project_id = format!("p-{}-{}", dir_name.to_lowercase(), unique_suffix());
        self.projects.push(Project {
            id: project_id,
            name: dir_name,
            path: path.to_string(),
            sessions: vec![session],
        });
    }

    /// Self-heal the persisted project structure — idempotent, Terminals immune.
    /// Four passes (`TabModel.swift:924-985`):
    /// 1. promote each non-Terminals project's `path` to its enclosing git root
    ///    (when a strict descendant of one);
    /// 2. move sessions whose own git-root anchor differs from their project (sessions
    ///    whose cwd no longer exists stay put);
    /// 3. merge non-Terminals projects that converged on the same expanded path
    ///    (lowest index wins);
    /// 4. drop empty non-Terminals projects.
    pub fn repair_project_structure(&mut self) {
        // Pass 1: promote project paths to git roots.
        for i in 0..self.projects.len() {
            if self.projects[i].id == Self::TERMINALS_PROJECT_ID {
                continue;
            }
            let path = self.expand_tilde(&self.projects[i].path);
            if !self.fs.exists(&path) {
                continue;
            }
            let Some(root) = self.find_git_root(&path) else {
                continue;
            };
            if root == path {
                continue;
            }
            self.projects[i].name = last_path_component(&root).to_uppercase();
            self.projects[i].path = root;
        }

        // Pass 2: collect mis-bucketed sessions, then re-insert at the right anchor.
        struct Move {
            session: Session,
            target_git_root: String,
        }
        let mut moves: Vec<Move> = Vec::new();
        for i in 0..self.projects.len() {
            if self.projects[i].id == Self::TERMINALS_PROJECT_ID {
                continue;
            }
            let project_anchor = self.expand_tilde(&self.projects[i].path);
            let sessions = std::mem::take(&mut self.projects[i].sessions);
            let mut keep: Vec<Session> = Vec::with_capacity(sessions.len());
            for session in sessions {
                let session_cwd = self.expand_tilde(&session.cwd);
                if !self.fs.exists(&session_cwd) {
                    keep.push(session);
                    continue;
                }
                let anchor = self.find_git_root(&session_cwd).unwrap_or(session_cwd);
                if anchor == project_anchor {
                    keep.push(session);
                } else {
                    moves.push(Move {
                        session,
                        target_git_root: anchor,
                    });
                }
            }
            self.projects[i].sessions = keep;
        }
        for m in moves {
            self.append_or_insert(m.session, &m.target_git_root);
        }

        // Pass 3: merge duplicates targeting the same expanded path.
        let mut canonical: HashMap<String, usize> = HashMap::new();
        let mut dupes: Vec<usize> = Vec::new();
        for i in 0..self.projects.len() {
            if self.projects[i].id == Self::TERMINALS_PROJECT_ID {
                continue;
            }
            let key = self.expand_tilde(&self.projects[i].path);
            if let Some(&c) = canonical.get(&key) {
                let moved = std::mem::take(&mut self.projects[i].sessions);
                self.projects[c].sessions.extend(moved);
                dupes.push(i);
            } else {
                canonical.insert(key, i);
            }
        }
        dupes.sort_unstable_by(|a, b| b.cmp(a));
        for idx in dupes {
            self.projects.remove(idx);
        }

        // Pass 4: drop empty non-Terminals projects.
        self.projects
            .retain(|p| p.id == Self::TERMINALS_PROJECT_ID || !p.sessions.is_empty());

        // Fire unconditionally: repair may have rewritten the tree, and tracking
        // a change flag across four passes buys nothing. In production repair
        // runs at boot BEFORE the observer is wired (D5), so this fire is a
        // no-op there; a later explicit call persists any real repair. The
        // spurious-fire case is tolerated — the save is debounced (D4).
        self.fire_mutation();
    }

    // MARK: - Cwd resolution

    /// Resolve the spawn cwd for `session`: prefer `session.cwd`, falling back to the
    /// containing project's path when the session's cwd no longer exists on disk
    /// (`TabModel.swift:994-1003`).
    pub fn resolved_spawn_cwd(&self, session: &Session) -> String {
        let expanded = self.expand_tilde(&session.cwd);
        if self.fs.exists(&expanded) {
            return expanded;
        }
        if let Some(project) = self
            .projects
            .iter()
            .find(|p| p.sessions.iter().any(|s| s.id == session.id))
        {
            return self.expand_tilde(&project.path);
        }
        expanded
    }

    /// Per-window variant: prefer `window.cwd` (last-observed via OSC 7) when set
    /// and still on disk, else fall back to [`WorkspaceModel::resolved_spawn_cwd`]
    /// (`TabModel.swift:1021-1029`).
    pub fn resolved_spawn_cwd_for_window(&self, session: &Session, window: &TermWindow) -> String {
        if let Some(raw) = &window.cwd {
            let expanded = self.expand_tilde(raw);
            if self.fs.exists(&expanded) {
                return expanded;
            }
        }
        self.resolved_spawn_cwd(session)
    }

    /// Resolve the cwd for a new window in `session`: an explicit `caller_provided`
    /// cwd wins; else inherit from the active window; else fall back to `session.cwd`
    /// (`TabModel.swift:1009-1016`).
    pub fn spawn_cwd_for_new_window(&self, session: &Session, caller_provided: Option<&str>) -> String {
        if let Some(cwd) = caller_provided {
            return cwd.to_string();
        }
        if let Some(active_id) = &session.active_window_id {
            if let Some(active_window) = session.windows.iter().find(|w| &w.id == active_id) {
                return self.resolved_spawn_cwd_for_window(session, active_window);
            }
        }
        session.cwd.clone()
    }

    /// Update `session.cwd` to `new_cwd` and pull along any window whose `cwd` was
    /// `None` or still tracking the old `session.cwd` (diverged windows stay put —
    /// asymmetry 4, per-window not all-or-nothing). Returns `true` iff anything
    /// changed. Fires the did-mutate signal on a real change (BUGHUNT1-D) so an
    /// OSC 7 cwd adoption persists by construction (`TabModel.swift:1052-1067`).
    pub fn adopt_session_cwd(&mut self, session_id: &str, new_cwd: &str) -> bool {
        let mut changed = false;
        if let Some((pi, ti)) = self.project_session_index(session_id) {
            let session = &mut self.projects[pi].sessions[ti];
            let old_cwd = session.cwd.clone();
            if old_cwd != new_cwd {
                session.cwd = new_cwd.to_string();
                for window in session.windows.iter_mut() {
                    if window.cwd.is_none() || window.cwd.as_deref() == Some(old_cwd.as_str()) {
                        window.cwd = Some(new_cwd.to_string());
                    }
                }
                changed = true;
            }
        }
        if changed {
            self.fire_mutation();
        }
        changed
    }

    // MARK: - Lineage (depth-1 /branch + handoff)

    /// Insert a fresh "branch parent" session into the same project as
    /// `originating_session_id`, applying the depth-1 lineage rule. The claude window
    /// is created **not running** (deferred resume). Root promotion: when the
    /// originating session has no parent, the new parent becomes the root and the
    /// originating session plus all its former children are re-parented to it so
    /// the depth-1 invariant survives (`TabModel.swift:297-365`).
    ///
    /// Returns the inserted parent, or `None` when the originating session is
    /// unknown or lives in the pinned Terminals project.
    pub fn insert_branch_parent(
        &mut self,
        originating_session_id: &str,
        new_session_id: &str,
        claude_window_id: &str,
        terminal_window_id: &str,
        old_session_id: &str,
    ) -> Option<Session> {
        let (pi, ti) = self.project_session_index(originating_session_id)?;
        if self.is_terminals_project_session(originating_session_id) {
            return None;
        }
        let originating = self.projects[pi].sessions[ti].clone();
        let inherited_root = originating.parent_session_id.clone();
        if let Some(root) = &inherited_root {
            // Defensive: parent_session_id is a within-project reference. A
            // cross-project pointer would mean prior corruption; don't compound
            // it by inheriting the bad pointer.
            debug_assert!(
                self.projects[pi].sessions.iter().any(|s| &s.id == root),
                "originating session's parent_session_id must live in the same project"
            );
        }

        let mut claude_window = TermWindow::new(claude_window_id, "Claude", TermWindowKind::Claude);
        claude_window.is_claude_running = false;
        let terminal_window = TermWindow::new(terminal_window_id, "Terminal 1", TermWindowKind::Terminal);
        let mut parent = Session::new(new_session_id, originating.title.clone(), originating.cwd.clone());
        parent.windows = vec![claude_window, terminal_window];
        parent.active_window_id = Some(claude_window_id.to_string());
        parent.title_auto_generated = originating.title_auto_generated;
        parent.title_manually_set = originating.title_manually_set;
        parent.claude_session_id = Some(old_session_id.to_string());
        parent.parent_session_id = inherited_root.clone();
        parent.next_terminal_index = 2;

        // Insert immediately above the originating session: order reads [parent, child].
        self.projects[pi].sessions.insert(ti, parent.clone());

        if inherited_root.is_none() {
            // First-branch root promotion: re-parent the originating session and
            // every session already pointing at it to the new root.
            for j in 0..self.projects[pi].sessions.len() {
                let (id, ptid) = {
                    let t = &self.projects[pi].sessions[j];
                    (t.id.clone(), t.parent_session_id.clone())
                };
                if id == originating_session_id || ptid.as_deref() == Some(originating_session_id) {
                    self.projects[pi].sessions[j].parent_session_id = Some(new_session_id.to_string());
                }
            }
        }

        // Inserting the branch parent (+ any root re-parenting) is a persisted
        // change (BUGHUNT1-D). The caller still spawns the pty afterward.
        self.fire_mutation();
        Some(parent)
    }

    /// Nest an already-constructed `session` one indent under `under_session_id`,
    /// applying the same depth-1 rule — but, unlike
    /// [`WorkspaceModel::insert_branch_parent`], **without** re-parenting the anchor's
    /// former children (the anchor stays the root, so its existing depth-1
    /// children remain valid; this asymmetry is deliberate). Inserted
    /// immediately after the anchor. Returns `false` (mutating nothing) when
    /// the anchor is unknown or in the Terminals group (`TabModel.swift:401-416`).
    ///
    /// **`parent_session_id` is the ONLY field this touches.** The child arrives
    /// fully built and every other field — windows, title flags, and in
    /// particular `claude_session_id` — is inserted verbatim. So a caller that
    /// needs the child PINNED to a specific claude session id (a handoff session
    /// to its pre-minted `--session-id`, a background `/fork` session to the
    /// fork's id, so its later deferred resume opens that exact conversation)
    /// simply sets `session.claude_session_id` before calling; no variant of
    /// this method is needed for it. [`WorkspaceModel::insert_branch_parent`]
    /// pins the old id itself only because it MINTS the session it inserts.
    pub fn insert_handoff_child(&mut self, session: Session, under_session_id: &str) -> bool {
        let Some((pi, ti)) = self.project_session_index(under_session_id) else {
            return false;
        };
        if self.is_terminals_project_session(under_session_id) {
            return false;
        }
        let originating_parent = self.projects[pi].sessions[ti].parent_session_id.clone();
        let mut child = session;
        child.parent_session_id = Some(originating_parent.unwrap_or_else(|| under_session_id.to_string()));
        self.projects[pi].sessions.insert(ti + 1, child);
        // Nesting the handoff child is a persisted change (BUGHUNT1-D).
        self.fire_mutation();
        true
    }

    // MARK: - Removal

    /// Remove the session at `(project_index, session_index)` and sweep any sibling
    /// `parent_session_id` references that pointed at it, atomically. The single
    /// removal entry point — every removal path must funnel through here so the
    /// parent-pointer sweep can't be skipped (`TabModel.swift:237-241`). Fires
    /// the did-mutate signal (BUGHUNT1-D) — a removal always changes persisted
    /// state — so the ctrl+d / pty-exit dissolve persists by construction.
    pub fn remove_session(&mut self, project_index: usize, session_index: usize) -> Session {
        let removed = self.projects[project_index].sessions.remove(session_index);
        // The sweep fires its own signal only when it actually clears a
        // reference; the removal itself always warrants a fire.
        self.clear_dangling_parent_references(&removed.id);
        self.fire_mutation();
        removed
    }

    /// Clear `parent_session_id` on every session that pointed at `removed_session_id`
    /// (`TabModel.swift:249-257`). Fires the did-mutate signal when it clears at
    /// least one reference (BUGHUNT1-D).
    pub fn clear_dangling_parent_references(&mut self, removed_session_id: &str) {
        let mut changed = false;
        for pi in 0..self.projects.len() {
            for ti in 0..self.projects[pi].sessions.len() {
                if self.projects[pi].sessions[ti].parent_session_id.as_deref() == Some(removed_session_id) {
                    self.projects[pi].sessions[ti].parent_session_id = None;
                    changed = true;
                }
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    /// Sweep every `parent_session_id` against the set of present session ids and clear
    /// any dangling one. Called after a full-tree restore so a hand-edited or
    /// partially-corrupt snapshot can't leave a child indented under a session that
    /// doesn't exist. Pure cleanup — safe to call repeatedly
    /// (`TabModel.swift:427-442`). Fires the did-mutate signal when it clears at
    /// least one dangling reference (BUGHUNT1-D). In production this runs during
    /// restore, before the observer is wired (D5), so it self-heals silently
    /// there; a stray later call persists any real cleanup.
    pub fn prune_dangling_parent_references(&mut self) {
        let valid: HashSet<String> = self
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|s| s.id.clone()))
            .collect();
        let mut changed = false;
        for pi in 0..self.projects.len() {
            for ti in 0..self.projects[pi].sessions.len() {
                if let Some(parent) = &self.projects[pi].sessions[ti].parent_session_id {
                    if !valid.contains(parent) {
                        self.projects[pi].sessions[ti].parent_session_id = None;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    /// Sweep every window id in the tree for duplicates and re-mint any repeat —
    /// first occurrence (in project/session/window order) keeps the id, later ones get
    /// the lowest unused `<id>-dup<n>` suffix. Restore-time self-heal: a
    /// pre-fix launch could persist two windows sharing one id (the strip/sidebar
    /// minters restarted their counter at 0 every launch while restored windows
    /// kept their persisted ids verbatim), and every id-keyed strip affordance
    /// then matches both windows (double-selected pills, click-inert select,
    /// rename editing both). When a rename orphans a session's `active_window_id`
    /// (the duplicate lived on another session), the pointer follows the renamed
    /// window. Pure cleanup — safe to call repeatedly. Fires the did-mutate
    /// signal when it renamed at least one window; in production this runs during
    /// restore, before the observer is wired, so it self-heals silently there.
    pub fn dedupe_window_ids(&mut self) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut changed = false;
        for project in &mut self.projects {
            for session in &mut project.sessions {
                // (old, new) renames on this session, for active_window_id repair.
                let mut renames: Vec<(String, String)> = Vec::new();
                for window in &mut session.windows {
                    if seen.insert(window.id.clone()) {
                        continue;
                    }
                    let old = window.id.clone();
                    let mut n = 2u32;
                    let new_id = loop {
                        let candidate = format!("{old}-dup{n}");
                        if seen.insert(candidate.clone()) {
                            break candidate;
                        }
                        n += 1;
                    };
                    window.id = new_id.clone();
                    renames.push((old, new_id));
                    changed = true;
                }
                // Re-point active_window_id only when the rename left it dangling
                // (no window on this session retains the old id — the duplicate that
                // kept it lives on another session). When a window on this session kept
                // the id, the pointer already resolves unambiguously to it.
                if let Some(active) = session.active_window_id.clone() {
                    if !session.windows.iter().any(|w| w.id == active) {
                        if let Some((_, new_id)) =
                            renames.iter().find(|(old, _)| *old == active)
                        {
                            session.active_window_id = Some(new_id.clone());
                        }
                    }
                }
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    // MARK: - Filesystem-seam helpers

    /// Tilde-expand a path using the [`FsProbe`] home (`TabModel.swift:1071-1077`).
    pub fn expand_tilde(&self, path: &str) -> String {
        if path == "~" {
            return self.fs.home();
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return format!("{}/{}", self.fs.home(), rest);
        }
        path.to_string()
    }

    /// Walk up from `cwd` (after stripping any Nice worktree suffix), returning
    /// the nearest ancestor containing a `.git` entry (matches both `.git/`
    /// dirs and `.git` files). `None` if none is found before the filesystem
    /// root (`TabModel.swift:1095-1107`).
    pub fn find_git_root(&self, cwd: &str) -> Option<String> {
        let mut current = strip_nice_worktree_suffix(cwd).to_string();
        while !current.is_empty() && current != "/" {
            let dot_git = format!("{}/.git", current);
            if self.fs.exists(&dot_git) {
                return Some(current);
            }
            let parent = parent_path(&current);
            if parent == current {
                break;
            }
            current = parent;
        }
        None
    }

    // MARK: - Arg parsers

    /// Extract the value of `-w` / `--worktree` from Claude args. Only the
    /// **space-delimited** form is recognized (matches Claude Code's CLI; the
    /// `=`-form is deliberately not) (`TabModel.swift:1113-1124`).
    pub fn extract_worktree_name<S: AsRef<str>>(args: &[S]) -> Option<String> {
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_ref();
            if (a == "-w" || a == "--worktree") && i + 1 < args.len() {
                let v = args[i + 1].as_ref();
                return if v.is_empty() { None } else { Some(v.to_string()) };
            }
            i += 1;
        }
        None
    }

    /// Scan `args` for the session UUID from `--resume <id>`, `--session-id
    /// <id>`, `--resume=<id>`, or `--session-id=<id>` (both forms, unlike
    /// [`WorkspaceModel::extract_worktree_name`]) (`TabModel.swift:1129-1145`).
    pub fn extract_claude_session_id<S: AsRef<str>>(args: &[S]) -> Option<String> {
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_ref();
            if a == "--resume" || a == "--session-id" {
                if i + 1 < args.len() {
                    return Some(args[i + 1].as_ref().to_string());
                }
            } else if let Some(v) = a.strip_prefix("--resume=") {
                return Some(v.to_string());
            } else if let Some(v) = a.strip_prefix("--session-id=") {
                return Some(v.to_string());
            }
            i += 1;
        }
        None
    }

    /// Derive the on-disk worktree directory name Claude creates from a `-w`
    /// value. Claude Code sanitizes `/` → `+` when materializing the worktree
    /// directory (so `foo/bar` becomes `foo+bar`); Nice mirrors that so the
    /// companion terminal's `Session.cwd` lands in the same directory Claude
    /// actually created (`SessionsModel.swift:677-682`). Pure counterpart to
    /// [`WorkspaceModel::extract_worktree_name`] (which pulls the raw `-w` value);
    /// the caller joins `<cwd>/.claude/worktrees/<sanitized>`.
    pub fn sanitize_worktree_name(name: &str) -> String {
        name.replace('/', "+")
    }
}

// MARK: - Pure free helpers

/// Plan the reorder [`WorkspaceModel::move_session`] performs on one project's session list:
/// the resulting display order as a permutation of indices into `sessions`, or
/// `None` when the drop is illegal or would not change the order.
///
/// The depth-1 lineage (`Session::parent_session_id`) partitions a project's sessions into
/// BLOCKS: a root session plus every session pointing at it. The slot rules keep every
/// block visually contiguous (children always read as nested under their
/// parent — a foreign row interleaved into a group would visually adopt the
/// children below it):
///
/// * **Root drag** (dragged session has no parent): the root moves together with
///   its whole child block, child order preserved, gathered contiguously (a
///   previously scattered block self-heals). The landing slot is a block
///   boundary of the target's block — BEFORE it when the drop names the target
///   root itself with `place_after == false`, otherwise AFTER the entire
///   block (an interior slot — a child row, or "just after the parent row" —
///   normalizes to the block's end; lineage is never rewritten by a drag, so
///   a root cannot be dropped INTO a group). A drop anywhere inside the
///   dragged session's own block is a no-op.
/// * **Child drag**: the child reorders among its own siblings only — legal
///   targets are its root with `place_after == true` (the slot at the top of
///   the sibling run) or a sibling with either edge. Everything else
///   (before its root, another block, another project) is illegal: dragging
///   can't re-parent, so a child leaving its block would keep its indent and
///   read as nested under whatever row it landed beneath.
fn plan_session_move(
    sessions: &[Session],
    session_id: &str,
    target_session_id: &str,
    place_after: bool,
) -> Option<Vec<usize>> {
    if session_id == target_session_id {
        return None;
    }
    let src = sessions.iter().position(|s| s.id == session_id)?;
    let dst = sessions.iter().position(|s| s.id == target_session_id)?;

    let order = match sessions[src].parent_session_id.clone() {
        None => plan_root_block_move(sessions, src, dst, place_after)?,
        Some(root_id) => plan_child_move(sessions, src, dst, place_after, &root_id)?,
    };
    // A plan that reproduces the current order is a no-op (no event).
    if order.iter().enumerate().all(|(i, &j)| i == j) {
        return None;
    }
    Some(order)
}

/// Indices of the depth-1 children of the root at `root_idx`, in display order.
fn child_indices(sessions: &[Session], root_idx: usize) -> Vec<usize> {
    let root_id = sessions[root_idx].id.as_str();
    sessions.iter()
        .enumerate()
        .filter(|(_, s)| s.parent_session_id.as_deref() == Some(root_id))
        .map(|(i, _)| i)
        .collect()
}

/// The root-drag half of [`plan_session_move`].
fn plan_root_block_move(
    sessions: &[Session],
    src: usize,
    dst: usize,
    place_after: bool,
) -> Option<Vec<usize>> {
    // The dragged block re-inserts in canonical order — root first, then its
    // children in display order — so a block the old single-row move already
    // scattered self-heals on the next real move.
    let mut dragged_block = vec![src];
    dragged_block.extend(child_indices(sessions, src));
    // Resolve the target's block root. A dangling parent pointer (should be
    // impossible after `prune_dangling_parent_references`) rejects the drop.
    let target_root = match sessions[dst].parent_session_id.as_deref() {
        None => dst,
        Some(pid) => sessions.iter().position(|s| s.id == pid)?,
    };
    if target_root == src {
        // The slot lands inside the dragged session's own subtree.
        return None;
    }
    // Block boundaries are the target block's displayed extremes (min/max
    // index), robust to a scattered target block too.
    let target_first = child_indices(sessions, target_root)
        .into_iter()
        .fold(target_root, usize::min);
    let target_last = child_indices(sessions, target_root)
        .into_iter()
        .fold(target_root, usize::max);
    // Before the target block only when the drop names its root's leading
    // edge; every interior slot normalizes to after the whole block.
    let before = dst == target_root && !place_after;

    let rest: Vec<usize> = (0..sessions.len())
        .filter(|i| !dragged_block.contains(i))
        .collect();
    let anchor_index = if before {
        rest.iter().position(|i| *i == target_first)?
    } else {
        rest.iter().position(|i| *i == target_last)? + 1
    };
    let mut order = Vec::with_capacity(sessions.len());
    order.extend_from_slice(&rest[..anchor_index]);
    order.extend_from_slice(&dragged_block);
    order.extend_from_slice(&rest[anchor_index..]);
    Some(order)
}

/// The child-drag half of [`plan_session_move`].
fn plan_child_move(
    sessions: &[Session],
    src: usize,
    dst: usize,
    place_after: bool,
    root_id: &str,
) -> Option<Vec<usize>> {
    let dst_session = &sessions[dst];
    let legal = if dst_session.id == root_id {
        place_after
    } else {
        dst_session.parent_session_id.as_deref() == Some(root_id)
    };
    if !legal {
        return None;
    }
    // Single-row move (the original `moveTab` index math).
    let mut insert = if place_after { dst + 1 } else { dst };
    if src < insert {
        insert -= 1;
    }
    if insert == src {
        return None;
    }
    let mut order: Vec<usize> = (0..sessions.len()).filter(|i| *i != src).collect();
    order.insert(insert, src);
    Some(order)
}

/// Strip any `<X>/.claude/worktrees/<name>/...` suffix and return `<X>`. A
/// Nice-specific convention: a session in a Nice-managed worktree resolves to
/// the parent repo, not the worktree's own `.git` marker
/// (`TabModel.swift:1083-1088`).
fn strip_nice_worktree_suffix(path: &str) -> &str {
    match path.find("/.claude/worktrees/") {
        Some(i) => &path[..i],
        None => path,
    }
}

/// Humanize a kebab/snake-case session title into sentence case, capped at 40
/// characters (`TabModel.swift:769-785`).
fn humanize_session_title(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let pieces: Vec<&str> = trimmed
        .split(|c| c == '-' || c == '_')
        .filter(|s| !s.is_empty())
        .collect();
    if pieces.is_empty() {
        return String::new();
    }
    let mut joined = pieces.join(" ");
    if let Some(first) = joined.chars().next() {
        if first.is_lowercase() {
            let upper: String = first.to_uppercase().collect();
            joined = format!("{}{}", upper, &joined[first.len_utf8()..]);
        }
    }
    if joined.chars().count() > 40 {
        let truncated: String = joined.chars().take(40).collect();
        joined = truncated.trim().to_string();
    }
    joined
}

/// Last path component of an absolute path (NSString `lastPathComponent`
/// analog for the paths this model handles).
fn last_path_component(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Parent directory, mirroring NSString `deletingLastPathComponent` for the
/// absolute paths [`WorkspaceModel::find_git_root`] walks: "/a/b" → "/a", "/a" → "/",
/// "/" → "/" (its own parent, terminating the walk).
fn parent_path(p: &str) -> String {
    match Path::new(p).parent() {
        Some(parent) => {
            let s = parent.to_string_lossy().to_string();
            if s.is_empty() {
                "/".to_string()
            } else {
                s
            }
        }
        None => p.to_string(),
    }
}

/// Mint a window id shaped like the Swift seed's (`<prefix>-p<ms>`).
fn mint_window_id(prefix: &str) -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}-p{}", prefix, ms)
}

/// A short unique suffix for a generated project id (Swift uses a UUID prefix).
/// A process-local counter mixed with the clock keeps back-to-back appends in
/// the same instant — e.g. inside the repair session-move loop — from colliding.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{:08x}", mixed & 0xffff_ffff)
}

#[cfg(test)]
mod tests;
