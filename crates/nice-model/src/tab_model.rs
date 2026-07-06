//! `TabModel` — Nice's per-window document — ported from
//! `Sources/Nice/State/TabModel.swift`. The projects/tabs/panes value tree
//! plus which tab is selected, with all the tree mutation, cwd
//! bucketing/repair, renames + title locks, depth-1 `/branch`+handoff lineage,
//! and the arg parsers. Pure value-tree: nothing here spawns a process, opens
//! a socket, or writes to disk. The model's only impurities — existence probes
//! and the home-dir lookup — go through the injected [`FsProbe`] seam.
//!
//! The pinned "Terminals" group at the top of the sidebar is a regular
//! [`Project`] with the reserved id [`TabModel::TERMINALS_PROJECT_ID`]; it is
//! always present at index 0 and cannot be removed by the user, but its tabs
//! are ordinary terminal-only tabs.
//!
//! ## The did-mutate signal (`onTreeMutation` port)
//!
//! Swift's `onTreeMutation` closure + `@Observable` write-back are consolidated
//! here into one explicit "did-mutate" signal (`set_on_tree_mutation`). Its
//! observable contract survives verbatim: **a no-op transform produces no
//! mutation event; a real change produces exactly one.** The signal fires from
//! the same methods Swift fired `onTreeMutation` from — selection, reorder
//! ([`TabModel::move_tab`]/[`TabModel::move_pane`]), pane insert/extract, and
//! the rename/auto-title paths. The methods Swift deliberately routes through
//! the caller instead — [`TabModel::adopt_tab_cwd`] (returns a did-change bool),
//! [`TabModel::insert_branch_parent`]/[`TabModel::insert_handoff_child`] (caller
//! fires after spawning the pty), [`TabModel::remove_tab`] (the dissolve
//! cascade owns the save), [`TabModel::add_pane`], and the
//! bucketing/repair/ensure helpers — do **not** fire the signal.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::pane::{Pane, PaneKind};
use crate::project::Project;
use crate::tab::Tab;

/// The filesystem seam. The model's only impurities are existence probes
/// (`.git` discovery, worktree-cwd liveness) and the home-dir lookup for
/// tilde-expansion (`TabModel.swift:1099, 928, 948, 996, 1024, 1072-1074`).
/// Production uses [`StdFs`] (`std::fs`); tests inject a fake so they never
/// touch the real disk (the Swift tests plant real temp dirs; the seam lets
/// the Rust ports stay hermetic).
pub trait FsProbe {
    /// Whether a filesystem entry exists at `path` (a `.git` marker, or a
    /// tab/project cwd). Mirrors `FileManager.default.fileExists(atPath:)`.
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
/// `active_tab_id` is private — every write goes through [`TabModel::select_tab`]
/// (or the internal navigation setter) so the selection side effects can't be
/// skipped by a stray field assignment.
pub struct TabModel {
    /// The sidebar's project sections, in display order. The pinned Terminals
    /// group is expected at index 0 (kept there by
    /// [`TabModel::ensure_terminals_project_seeded`]).
    pub projects: Vec<Project>,
    /// Currently-selected tab. Private: the only writer is
    /// [`TabModel::select_tab`] / the navigation stepper, which carry the
    /// Swift `didSet` side effects (acknowledge waiting on the target's active
    /// pane + fire the did-mutate signal, only when the id actually changed —
    /// `TabModel.swift:43-53`).
    active_tab_id: Option<String>,
    /// The filesystem seam (existence + home).
    fs: Box<dyn FsProbe>,
    /// The did-mutate signal. `RefCell` so it can fire while the tree is
    /// borrowed for the mutation that triggered it (the callback never
    /// re-enters the model, exactly like the Swift closure).
    on_tree_mutation: Option<RefCell<Box<dyn FnMut()>>>,
}

impl TabModel {
    /// Reserved id for the pinned Terminals project at index 0 of `projects`.
    /// The project is always present and cannot be deleted by the user; its
    /// tabs are ordinary terminal-only tabs.
    pub const TERMINALS_PROJECT_ID: &'static str = "terminals";
    /// Stable id for the default "Main" tab seeded into the Terminals project
    /// on fresh launches.
    pub const MAIN_TERMINAL_TAB_ID: &'static str = "terminals-main";

    // MARK: - Construction

    /// Seed a fresh window: a pinned Terminals project at index 0 holding one
    /// "Main" tab with a single "Terminal 1" pane, `next_terminal_index = 2`,
    /// and the Main tab selected (`TabModel.swift:63-87`). Uses the production
    /// [`StdFs`] seam.
    pub fn new(initial_main_cwd: impl Into<String>) -> Self {
        Self::with_fs(initial_main_cwd, Box::new(StdFs))
    }

    /// [`TabModel::new`] with a caller-supplied [`FsProbe`] (tests inject a
    /// fake so existence/home lookups are deterministic and disk-free).
    pub fn with_fs(initial_main_cwd: impl Into<String>, fs: Box<dyn FsProbe>) -> Self {
        let initial_main_cwd = initial_main_cwd.into();
        let main_tab_id = Self::MAIN_TERMINAL_TAB_ID;
        let pane_id = mint_pane_id(main_tab_id);
        let pane = Pane::new(pane_id.clone(), "Terminal 1", PaneKind::Terminal);
        let mut main_tab = Tab::new(main_tab_id, "Main", &initial_main_cwd);
        main_tab.panes = vec![pane];
        main_tab.active_pane_id = Some(pane_id);
        main_tab.next_terminal_index = 2;
        let terminals = Project {
            id: Self::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: initial_main_cwd,
            tabs: vec![main_tab],
        };
        TabModel {
            projects: vec![terminals],
            // Init assignment does not run the `didSet` (Swift initializers
            // don't); no acknowledge, no mutation event.
            active_tab_id: Some(main_tab_id.to_string()),
            fs,
            on_tree_mutation: None,
        }
    }

    /// Construct a document from already-hydrated `projects` + a saved
    /// `active_tab_id`, WITHOUT seeding a Terminals/Main tab. The restore
    /// constructor: [`TabModel::new`]/[`TabModel::with_fs`] always seed a fresh
    /// Terminals project + Main tab, which restore must NOT do — it trusts the
    /// saved grouping (`WindowSession.restoreSavedWindow` rebuilds from the
    /// persisted projects). Like the initializers, the `active_tab_id`
    /// assignment does not run the `didSet` side effects (no acknowledge, no
    /// mutation event) — the caller runs restore's single explicit save.
    pub fn from_parts(
        projects: Vec<Project>,
        active_tab_id: Option<String>,
        fs: Box<dyn FsProbe>,
    ) -> Self {
        TabModel {
            projects,
            active_tab_id,
            fs,
            on_tree_mutation: None,
        }
    }

    /// [`TabModel::from_parts`] with the production [`StdFs`] probe — the R18
    /// restore call site (`crate::window_state::WindowState::with_seed`) has no
    /// injected fs, so this is the disk-backed default.
    pub fn from_parts_std(projects: Vec<Project>, active_tab_id: Option<String>) -> Self {
        Self::from_parts(projects, active_tab_id, Box::new(StdFs))
    }

    /// Snapshot of this window's live panes grouped by kind — the quit /
    /// window-close confirmation counting rule (`TabModel.swift:186-200`). A
    /// pure fold over `pane.is_alive`: BOTH kinds count, held (not-alive) panes
    /// don't, and modelled-but-unspawned panes (a restored pane hydrates
    /// `is_alive = true`) DO — the Swift quirk, preserved deliberately.
    pub fn live_pane_counts(&self) -> (usize, usize) {
        let mut claude = 0;
        let mut terminal = 0;
        for project in &self.projects {
            for tab in &project.tabs {
                for pane in tab.panes.iter().filter(|p| p.is_alive) {
                    match pane.kind {
                        PaneKind::Claude => claude += 1,
                        PaneKind::Terminal => terminal += 1,
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

    /// The currently-selected tab id, if any.
    pub fn active_tab_id(&self) -> Option<&str> {
        self.active_tab_id.as_deref()
    }

    /// Look up a tab by id across every project, including the pinned
    /// Terminals group (`TabModel.swift:93-100`).
    pub fn tab_for(&self, id: &str) -> Option<&Tab> {
        self.projects
            .iter()
            .flat_map(|p| p.tabs.iter())
            .find(|t| t.id == id)
    }

    /// Project + tab index for the tab with id `id`, for in-place mutation
    /// (`TabModel.swift:132-139`).
    pub fn project_tab_index(&self, id: &str) -> Option<(usize, usize)> {
        for (pi, project) in self.projects.iter().enumerate() {
            if let Some(ti) = project.tabs.iter().position(|t| t.id == id) {
                return Some((pi, ti));
            }
        }
        None
    }

    /// Mutate the tab identified by `id` in place; returns true if the tab was
    /// found (`TabModel.swift:120-128`). Does **not** fire the did-mutate
    /// signal — callers that need the event track their own change flag (the
    /// SwiftUI byte-equality write-back skip has no analog here; its observable
    /// contract lives in the callers).
    pub fn mutate_tab(&mut self, id: &str, transform: impl FnOnce(&mut Tab)) -> bool {
        match self.project_tab_index(id) {
            Some((pi, ti)) => {
                transform(&mut self.projects[pi].tabs[ti]);
                true
            }
            None => false,
        }
    }

    /// True when `tab_id` lives inside the pinned Terminals project
    /// (`TabModel.swift:176-181`).
    pub fn is_terminals_project_tab(&self, tab_id: &str) -> bool {
        self.projects
            .iter()
            .find(|p| p.id == Self::TERMINALS_PROJECT_ID)
            .is_some_and(|t| t.tabs.iter().any(|x| x.id == tab_id))
    }

    /// Flat list of sidebar tab ids in displayed order — the pinned Terminals
    /// project first, then project tabs in project/then-tab order. The single
    /// source of truth for keyboard navigation and the dissolve-selection
    /// fallback (`TabModel.swift:206-208`).
    pub fn navigable_sidebar_tab_ids(&self) -> Vec<String> {
        self.projects
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect()
    }

    /// The id of the tab whose pane list contains `pane_id`, scanning every
    /// project including the pinned Terminals group (`TabModel.swift:211-220`).
    /// The reverse index the SessionStart hook's `session_update` uses to route a
    /// pane's rotated session id / cwd back onto its owning tab. Scoped to this
    /// `TabModel` — a per-window lookup, never a global index — so a pane owned by
    /// a sibling window returns `None` here. Returns an owned id (the id must
    /// outlive later `&mut self` mutations the rotation handler makes).
    pub fn tab_id_owning(&self, pane_id: &str) -> Option<String> {
        self.projects
            .iter()
            .flat_map(|p| p.tabs.iter())
            .find(|t| t.panes.iter().any(|pane| pane.id == pane_id))
            .map(|t| t.id.clone())
    }

    // MARK: - Selection

    /// Select a tab. The single `active_tab_id` writer — carries the Swift
    /// `didSet` side effects.
    pub fn select_tab(&mut self, id: &str) {
        self.set_active_tab_id(Some(id.to_string()));
    }

    /// The sole `active_tab_id` writer. When the id actually changes to a
    /// non-`None` value, dismiss the attention pulse on the target's active
    /// pane and fire the did-mutate signal (`TabModel.swift:43-53`).
    fn set_active_tab_id(&mut self, new: Option<String>) {
        if self.active_tab_id == new {
            return;
        }
        self.active_tab_id = new.clone();
        if let Some(id) = new {
            self.acknowledge_waiting_on_active_pane(&id);
            self.fire_mutation();
        }
    }

    /// Move focus to the next sidebar tab, wrapping. No-op when there's only
    /// one navigable tab (`TabModel.swift:452, 457-463`).
    pub fn select_next_sidebar_tab(&mut self) {
        self.step_sidebar_tab(1);
    }

    /// Move focus to the previous sidebar tab, wrapping.
    pub fn select_prev_sidebar_tab(&mut self) {
        self.step_sidebar_tab(-1);
    }

    fn step_sidebar_tab(&mut self, offset: isize) {
        let ids = self.navigable_sidebar_tab_ids();
        if ids.len() <= 1 {
            return;
        }
        let current_idx = self
            .active_tab_id
            .as_ref()
            .and_then(|a| ids.iter().position(|x| x == a))
            .unwrap_or(0) as isize;
        let n = ids.len() as isize;
        let next_idx = (((current_idx + offset) % n + n) % n) as usize;
        self.set_active_tab_id(Some(ids[next_idx].clone()));
    }

    /// Clear the waiting-attention pulse on whichever pane is focused in
    /// `tab_id` — the `active_tab_id` `didSet` side effect
    /// (`TabModel.swift:468-475`).
    fn acknowledge_waiting_on_active_pane(&mut self, tab_id: &str) {
        self.mutate_tab(tab_id, |tab| {
            if let Some(pane_id) = tab.active_pane_id.clone() {
                if let Some(p) = tab.panes.iter_mut().find(|p| p.id == pane_id) {
                    p.mark_acknowledged_if_waiting();
                }
            }
        });
    }

    // MARK: - Reordering (tabs)

    /// Move `tab_id` to a new slot within the same project, relative to
    /// `target_tab_id`. No-op — and no event — when the tabs aren't in the same
    /// project, either id is unknown, or the move wouldn't change order. Tabs
    /// in the pinned Terminals project reorder internally but never leave it
    /// (cross-project is a no-op) (`TabModel.swift:485-500`).
    pub fn move_tab(&mut self, tab_id: &str, target_tab_id: &str, place_after: bool) {
        if tab_id == target_tab_id {
            return;
        }
        let (Some((sp, si)), Some((dp, di))) = (
            self.project_tab_index(tab_id),
            self.project_tab_index(target_tab_id),
        ) else {
            return;
        };
        if sp != dp {
            return;
        }
        let mut insert_index = if place_after { di + 1 } else { di };
        if si < insert_index {
            insert_index -= 1;
        }
        if insert_index == si {
            return;
        }
        let tab = self.projects[sp].tabs.remove(si);
        self.projects[sp].tabs.insert(insert_index, tab);
        self.fire_mutation();
    }

    /// Mirrors [`TabModel::move_tab`] without mutating — true iff the drop
    /// would actually reorder (`TabModel.swift:505-514`).
    pub fn would_move_tab(&self, tab_id: &str, target_tab_id: &str, place_after: bool) -> bool {
        if tab_id == target_tab_id {
            return false;
        }
        let (Some((sp, si)), Some((dp, di))) = (
            self.project_tab_index(tab_id),
            self.project_tab_index(target_tab_id),
        ) else {
            return false;
        };
        if sp != dp {
            return false;
        }
        let mut insert_index = if place_after { di + 1 } else { di };
        if si < insert_index {
            insert_index -= 1;
        }
        insert_index != si
    }

    // MARK: - Panes: reorder / insert / extract

    /// Which pane id should receive focus after the pane at `idx` is removed
    /// (the post-removal array): prefer the pane that slid into the freed slot,
    /// else the new last pane, else `None`. Shared by [`TabModel::extract_pane`]
    /// and the R13 process-exit path so a moved pane and an exited pane
    /// re-focus the same neighbor (`TabModel.swift:554-558`).
    pub fn neighbor_active_pane_id(after_removing_index: usize, panes: &[Pane]) -> Option<String> {
        if after_removing_index < panes.len() {
            return Some(panes[after_removing_index].id.clone());
        }
        if after_removing_index > 0 {
            return Some(panes[after_removing_index - 1].id.clone());
        }
        None
    }

    /// Move `pane_id` within tab `tab_id`'s pane list, relative to
    /// `target_pane_id`. No-op (no event) when the tab is unknown, either pane
    /// isn't in it, or the move wouldn't change order. Never touches
    /// `active_pane_id` (`TabModel.swift:526-546`).
    pub fn move_pane(
        &mut self,
        pane_id: &str,
        tab_id: &str,
        target_pane_id: &str,
        place_after: bool,
    ) {
        if pane_id == target_pane_id {
            return;
        }
        let mut moved = false;
        if let Some((pi, ti)) = self.project_tab_index(tab_id) {
            let tab = &mut self.projects[pi].tabs[ti];
            if let (Some(src), Some(dst)) = (
                tab.panes.iter().position(|p| p.id == pane_id),
                tab.panes.iter().position(|p| p.id == target_pane_id),
            ) {
                let mut insert_index = if place_after { dst + 1 } else { dst };
                if src < insert_index {
                    insert_index -= 1;
                }
                if insert_index != src {
                    let pane = tab.panes.remove(src);
                    tab.panes.insert(insert_index, pane);
                    moved = true;
                }
            }
        }
        if moved {
            self.fire_mutation();
        }
    }

    /// Mirrors [`TabModel::move_pane`] without mutating (`TabModel.swift:648-657`).
    pub fn would_move_pane(
        &self,
        pane_id: &str,
        tab_id: &str,
        target_pane_id: &str,
        place_after: bool,
    ) -> bool {
        if pane_id == target_pane_id {
            return false;
        }
        let Some(tab) = self.tab_for(tab_id) else {
            return false;
        };
        let (Some(src), Some(dst)) = (
            tab.panes.iter().position(|p| p.id == pane_id),
            tab.panes.iter().position(|p| p.id == target_pane_id),
        ) else {
            return false;
        };
        let mut insert_index = if place_after { dst + 1 } else { dst };
        if src < insert_index {
            insert_index -= 1;
        }
        insert_index != src
    }

    /// Remove `pane_id` from tab `tab_id`, returning the removed [`Pane`] so a
    /// destination window can re-insert it. When the removed pane was active,
    /// focus re-points to a neighbor via [`TabModel::neighbor_active_pane_id`].
    /// Fires the did-mutate signal on a real removal; returns `None` (no
    /// mutation, no event) when the tab or pane isn't found
    /// (`TabModel.swift:572-587`).
    pub fn extract_pane(&mut self, pane_id: &str, tab_id: &str) -> Option<Pane> {
        let mut removed = None;
        if let Some((pi, ti)) = self.project_tab_index(tab_id) {
            let tab = &mut self.projects[pi].tabs[ti];
            if let Some(idx) = tab.panes.iter().position(|p| p.id == pane_id) {
                let was_active = tab.active_pane_id.as_deref() == Some(pane_id);
                let r = tab.panes.remove(idx);
                if was_active {
                    tab.active_pane_id = Self::neighbor_active_pane_id(idx, &tab.panes);
                }
                removed = Some(r);
            }
        }
        if removed.is_some() {
            self.fire_mutation();
        }
        removed
    }

    /// Insert an externally-sourced `pane` into tab `tab_id` relative to
    /// `target_pane_id` (a `None`/unknown target appends). No-op (no event)
    /// when the tab is unknown or already contains a pane with this id. Does
    /// **not** change `active_pane_id` (`TabModel.swift:598-613`).
    pub fn insert_pane(
        &mut self,
        pane: Pane,
        tab_id: &str,
        target_pane_id: Option<&str>,
        place_after: bool,
    ) {
        let mut inserted = false;
        if let Some((pi, ti)) = self.project_tab_index(tab_id) {
            let tab = &mut self.projects[pi].tabs[ti];
            if !tab.panes.iter().any(|p| p.id == pane.id) {
                let insert_index = match target_pane_id
                    .and_then(|t| tab.panes.iter().position(|p| p.id == t))
                {
                    Some(t) => {
                        if place_after {
                            t + 1
                        } else {
                            t
                        }
                    }
                    None => tab.panes.len(),
                };
                tab.panes.insert(insert_index, pane);
                inserted = true;
            }
        }
        if inserted {
            self.fire_mutation();
        }
    }

    /// The model half of pane creation: append an auto-named terminal pane to
    /// `tab_id` and focus it, all in one mutation — counter read → "Terminal N"
    /// (or the explicit `title`) → increment. The counter increments
    /// unconditionally (an explicit title consumes the slot too), and only
    /// terminal-kind panes are constructible through this method — the
    /// ≤1-running-Claude creation edge (`SessionsModel.swift:603-626`).
    /// Returns the new pane id, or `None` when the tab isn't found. Does not
    /// fire the did-mutate signal (the save is the caller's concern, mirroring
    /// the Swift `SessionsModel`/`@Observable` path).
    pub fn add_pane(
        &mut self,
        tab_id: &str,
        new_pane_id: impl Into<String>,
        title: Option<String>,
    ) -> Option<String> {
        let (pi, ti) = self.project_tab_index(tab_id)?;
        let new_pane_id = new_pane_id.into();
        let tab = &mut self.projects[pi].tabs[ti];
        let n = tab.next_terminal_index;
        let resolved_title = title.unwrap_or_else(|| format!("Terminal {}", n));
        tab.panes
            .push(Pane::new(new_pane_id.clone(), resolved_title, PaneKind::Terminal));
        tab.active_pane_id = Some(new_pane_id.clone());
        tab.next_terminal_index = n + 1;
        Some(new_pane_id)
    }

    // MARK: - Titles

    /// Default display title for a pane of `kind`. Terminal panes use the tab's
    /// monotonic `next_terminal_index` — the single source of truth
    /// [`TabModel::rename_pane`]'s empty-submit reset also reads
    /// (`TabModel.swift:666-671`).
    pub fn default_pane_title(kind: PaneKind, terminal_index: u32) -> String {
        match kind {
            PaneKind::Claude => "Claude".to_string(),
            PaneKind::Terminal => format!("Terminal {}", terminal_index),
        }
    }

    /// User-initiated rename for an individual pane. **Non-empty** input sets
    /// the title and locks it (`title_manually_set = true`) so OSC titles can't
    /// clobber the user's choice. **Empty** input resets to the per-kind
    /// auto-default and clears the lock; for terminal panes the reset consumes
    /// and increments `next_terminal_index` (the monotonic-never-reuse policy —
    /// asymmetry 3) (`TabModel.swift:687-727`).
    pub fn rename_pane(&mut self, tab_id: &str, pane_id: &str, new_title: &str) {
        let trimmed = new_title.trim();
        let mut changed = false;
        if let Some((pi, ti)) = self.project_tab_index(tab_id) {
            let tab = &mut self.projects[pi].tabs[ti];
            if let Some(idx) = tab.panes.iter().position(|p| p.id == pane_id) {
                if trimmed.is_empty() {
                    // Empty submit: release the lock and recompute the
                    // auto-default. A terminal reset consumes the next slot from
                    // the monotonic counter (unconditionally — the increment
                    // happens before the change check, matching the Swift order).
                    let reset_title = match tab.panes[idx].kind {
                        PaneKind::Claude => Self::default_pane_title(PaneKind::Claude, 0),
                        PaneKind::Terminal => {
                            let n = tab.next_terminal_index;
                            let t = Self::default_pane_title(PaneKind::Terminal, n);
                            tab.next_terminal_index = n + 1;
                            t
                        }
                    };
                    if tab.panes[idx].title != reset_title || tab.panes[idx].title_manually_set {
                        tab.panes[idx].title = reset_title;
                        tab.panes[idx].title_manually_set = false;
                        changed = true;
                    }
                } else if tab.panes[idx].title != trimmed || !tab.panes[idx].title_manually_set {
                    tab.panes[idx].title = trimmed.to_string();
                    tab.panes[idx].title_manually_set = true;
                    changed = true;
                }
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    /// User-initiated tab rename from the sidebar editor. Trims whitespace,
    /// **ignores empty input** (a no-op — asymmetry 3, the mirror of
    /// [`TabModel::rename_pane`]'s reset), and locks the title so
    /// [`TabModel::apply_auto_title`] skips it (`TabModel.swift:732-744`).
    pub fn rename_tab(&mut self, id: &str, new_title: &str) {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return;
        }
        let mut changed = false;
        if let Some((pi, ti)) = self.project_tab_index(id) {
            let tab = &mut self.projects[pi].tabs[ti];
            if tab.title != trimmed || !tab.title_manually_set {
                tab.title = trimmed.to_string();
                tab.title_manually_set = true;
                changed = true;
            }
        }
        if changed {
            self.fire_mutation();
        }
    }

    /// Apply a Claude-generated session title, humanized into sentence case.
    /// Skipped entirely once the user has manually renamed the tab, keyed on
    /// `tab_id` so locking one tab never affects another
    /// (`TabModel.swift:752-767`).
    pub fn apply_auto_title(&mut self, tab_id: &str, raw_title: &str) {
        match self.tab_for(tab_id) {
            Some(t) if !t.title_manually_set => {}
            _ => return,
        }
        let humanized = humanize_session_title(raw_title);
        if humanized.is_empty() {
            return;
        }
        let mut changed = false;
        if let Some((pi, ti)) = self.project_tab_index(tab_id) {
            let tab = &mut self.projects[pi].tabs[ti];
            if tab.title != humanized {
                tab.title = humanized;
                changed = true;
            }
            tab.title_auto_generated = true;
        }
        if changed {
            self.fire_mutation();
        }
    }

    // MARK: - Project structure

    /// Guarantee a pinned Terminals project sits at `projects[0]`. Synthesize
    /// one (Main tab + fresh "Terminal 1" pane) when absent, or move it to
    /// index 0 when merely out of place. `spawn_hook` fires **exactly once**,
    /// with the synthesized Main tab, only when the project had to be created
    /// from scratch — the one-way bridge into pty-aware callers
    /// (`TabModel.swift:803-839`).
    pub fn ensure_terminals_project_seeded(&mut self, spawn_hook: impl FnOnce(&Tab)) {
        if let Some(idx) = self
            .projects
            .iter()
            .position(|p| p.id == Self::TERMINALS_PROJECT_ID)
        {
            if idx != 0 {
                let project = self.projects.remove(idx);
                self.projects.insert(0, project);
            }
            if self.active_tab_id.is_none() {
                if let Some(first_id) = self.projects[0].tabs.first().map(|t| t.id.clone()) {
                    self.set_active_tab_id(Some(first_id));
                }
            }
            return;
        }

        let cwd = self.fs.home();
        let main_tab_id = Self::MAIN_TERMINAL_TAB_ID;
        let pane_id = mint_pane_id(main_tab_id);
        let pane = Pane::new(pane_id.clone(), "Terminal 1", PaneKind::Terminal);
        let mut main_tab = Tab::new(main_tab_id, "Main", &cwd);
        main_tab.panes = vec![pane];
        main_tab.active_pane_id = Some(pane_id);
        main_tab.next_terminal_index = 2;
        let project = Project {
            id: Self::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: cwd,
            tabs: vec![main_tab.clone()],
        };
        self.projects.insert(0, project);
        if self.active_tab_id.is_none() {
            self.set_active_tab_id(Some(main_tab_id.to_string()));
        }
        spawn_hook(&main_tab);
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
            tabs: vec![],
        });
        self.projects.len() - 1
    }

    /// Find a non-Terminals project whose expanded `path` matches; else append
    /// a fresh project carrying the supplied id/name/path verbatim. Matches by
    /// filesystem path (distinct from [`TabModel::ensure_project`]'s id match),
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
            tabs: vec![],
        });
        self.projects.len() - 1
    }

    /// Bucket `tab` into the project anchoring `cwd`'s git repo, creating one at
    /// the git root when none matches. Falls back to legacy longest-prefix
    /// matching (excluding Terminals) when `cwd` is not inside any git repo
    /// (`TabModel.swift:857-878`).
    pub fn add_tab_to_projects(&mut self, tab: Tab, cwd: &str) {
        let normalized = self.expand_tilde(cwd);
        if let Some(git_root) = self.find_git_root(&normalized) {
            self.append_or_insert(tab, &git_root);
            return;
        }
        // No git root: legacy longest-prefix, excluding the pinned Terminals
        // group (whose path — typically $HOME — would prefix-match almost any
        // cwd and swallow new Claude tabs). Ties keep the first max, matching
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
            Some((idx, _)) => self.projects[idx].tabs.push(tab),
            None => self.append_new_project(&normalized, tab),
        }
    }

    /// Append `tab` to the existing non-Terminals project rooted at `path`, or
    /// create a new project there (`TabModel.swift:882-888`).
    fn append_or_insert(&mut self, tab: Tab, path: &str) {
        if let Some(idx) = self.first_index_of_non_terminals_project_at(path) {
            self.projects[idx].tabs.push(tab);
        } else {
            self.append_new_project(path, tab);
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
    fn append_new_project(&mut self, path: &str, tab: Tab) {
        let dir_name = last_path_component(path).to_uppercase();
        let project_id = format!("p-{}-{}", dir_name.to_lowercase(), unique_suffix());
        self.projects.push(Project {
            id: project_id,
            name: dir_name,
            path: path.to_string(),
            tabs: vec![tab],
        });
    }

    /// Self-heal the persisted project structure — idempotent, Terminals immune.
    /// Four passes (`TabModel.swift:924-985`):
    /// 1. promote each non-Terminals project's `path` to its enclosing git root
    ///    (when a strict descendant of one);
    /// 2. move tabs whose own git-root anchor differs from their project (tabs
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

        // Pass 2: collect mis-bucketed tabs, then re-insert at the right anchor.
        struct Move {
            tab: Tab,
            target_git_root: String,
        }
        let mut moves: Vec<Move> = Vec::new();
        for i in 0..self.projects.len() {
            if self.projects[i].id == Self::TERMINALS_PROJECT_ID {
                continue;
            }
            let project_anchor = self.expand_tilde(&self.projects[i].path);
            let tabs = std::mem::take(&mut self.projects[i].tabs);
            let mut keep: Vec<Tab> = Vec::with_capacity(tabs.len());
            for tab in tabs {
                let tab_cwd = self.expand_tilde(&tab.cwd);
                if !self.fs.exists(&tab_cwd) {
                    keep.push(tab);
                    continue;
                }
                let anchor = self.find_git_root(&tab_cwd).unwrap_or(tab_cwd);
                if anchor == project_anchor {
                    keep.push(tab);
                } else {
                    moves.push(Move {
                        tab,
                        target_git_root: anchor,
                    });
                }
            }
            self.projects[i].tabs = keep;
        }
        for m in moves {
            self.append_or_insert(m.tab, &m.target_git_root);
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
                let moved = std::mem::take(&mut self.projects[i].tabs);
                self.projects[c].tabs.extend(moved);
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
            .retain(|p| p.id == Self::TERMINALS_PROJECT_ID || !p.tabs.is_empty());
    }

    // MARK: - Cwd resolution

    /// Resolve the spawn cwd for `tab`: prefer `tab.cwd`, falling back to the
    /// containing project's path when the tab's cwd no longer exists on disk
    /// (`TabModel.swift:994-1003`).
    pub fn resolved_spawn_cwd(&self, tab: &Tab) -> String {
        let expanded = self.expand_tilde(&tab.cwd);
        if self.fs.exists(&expanded) {
            return expanded;
        }
        if let Some(project) = self
            .projects
            .iter()
            .find(|p| p.tabs.iter().any(|t| t.id == tab.id))
        {
            return self.expand_tilde(&project.path);
        }
        expanded
    }

    /// Per-pane variant: prefer `pane.cwd` (last-observed via OSC 7) when set
    /// and still on disk, else fall back to [`TabModel::resolved_spawn_cwd`]
    /// (`TabModel.swift:1021-1029`).
    pub fn resolved_spawn_cwd_for_pane(&self, tab: &Tab, pane: &Pane) -> String {
        if let Some(raw) = &pane.cwd {
            let expanded = self.expand_tilde(raw);
            if self.fs.exists(&expanded) {
                return expanded;
            }
        }
        self.resolved_spawn_cwd(tab)
    }

    /// Resolve the cwd for a new pane in `tab`: an explicit `caller_provided`
    /// cwd wins; else inherit from the active pane; else fall back to `tab.cwd`
    /// (`TabModel.swift:1009-1016`).
    pub fn spawn_cwd_for_new_pane(&self, tab: &Tab, caller_provided: Option<&str>) -> String {
        if let Some(cwd) = caller_provided {
            return cwd.to_string();
        }
        if let Some(active_id) = &tab.active_pane_id {
            if let Some(active_pane) = tab.panes.iter().find(|p| &p.id == active_id) {
                return self.resolved_spawn_cwd_for_pane(tab, active_pane);
            }
        }
        tab.cwd.clone()
    }

    /// Update `tab.cwd` to `new_cwd` and pull along any pane whose `cwd` was
    /// `None` or still tracking the old `tab.cwd` (diverged panes stay put —
    /// asymmetry 4, per-pane not all-or-nothing). Returns `true` iff anything
    /// changed. Does **not** fire the did-mutate signal — the caller fires the
    /// save (`TabModel.swift:1052-1067`).
    pub fn adopt_tab_cwd(&mut self, tab_id: &str, new_cwd: &str) -> bool {
        let mut changed = false;
        if let Some((pi, ti)) = self.project_tab_index(tab_id) {
            let tab = &mut self.projects[pi].tabs[ti];
            let old_cwd = tab.cwd.clone();
            if old_cwd != new_cwd {
                tab.cwd = new_cwd.to_string();
                for pane in tab.panes.iter_mut() {
                    if pane.cwd.is_none() || pane.cwd.as_deref() == Some(old_cwd.as_str()) {
                        pane.cwd = Some(new_cwd.to_string());
                    }
                }
                changed = true;
            }
        }
        changed
    }

    // MARK: - Lineage (depth-1 /branch + handoff)

    /// Insert a fresh "branch parent" tab into the same project as
    /// `originating_tab_id`, applying the depth-1 lineage rule. The claude pane
    /// is created **not running** (deferred resume). Root promotion: when the
    /// originating tab has no parent, the new parent becomes the root and the
    /// originating tab plus all its former children are re-parented to it so
    /// the depth-1 invariant survives (`TabModel.swift:297-365`).
    ///
    /// Returns the inserted parent, or `None` when the originating tab is
    /// unknown or lives in the pinned Terminals project.
    pub fn insert_branch_parent(
        &mut self,
        originating_tab_id: &str,
        new_tab_id: &str,
        claude_pane_id: &str,
        terminal_pane_id: &str,
        old_session_id: &str,
    ) -> Option<Tab> {
        let (pi, ti) = self.project_tab_index(originating_tab_id)?;
        if self.is_terminals_project_tab(originating_tab_id) {
            return None;
        }
        let originating = self.projects[pi].tabs[ti].clone();
        let inherited_root = originating.parent_tab_id.clone();
        if let Some(root) = &inherited_root {
            // Defensive: parent_tab_id is a within-project reference. A
            // cross-project pointer would mean prior corruption; don't compound
            // it by inheriting the bad pointer.
            debug_assert!(
                self.projects[pi].tabs.iter().any(|t| &t.id == root),
                "originating tab's parent_tab_id must live in the same project"
            );
        }

        let mut claude_pane = Pane::new(claude_pane_id, "Claude", PaneKind::Claude);
        claude_pane.is_claude_running = false;
        let terminal_pane = Pane::new(terminal_pane_id, "Terminal 1", PaneKind::Terminal);
        let mut parent = Tab::new(new_tab_id, originating.title.clone(), originating.cwd.clone());
        parent.panes = vec![claude_pane, terminal_pane];
        parent.active_pane_id = Some(claude_pane_id.to_string());
        parent.title_auto_generated = originating.title_auto_generated;
        parent.title_manually_set = originating.title_manually_set;
        parent.claude_session_id = Some(old_session_id.to_string());
        parent.parent_tab_id = inherited_root.clone();
        parent.next_terminal_index = 2;

        // Insert immediately above the originating tab: order reads [parent, child].
        self.projects[pi].tabs.insert(ti, parent.clone());

        if inherited_root.is_none() {
            // First-branch root promotion: re-parent the originating tab and
            // every tab already pointing at it to the new root.
            for j in 0..self.projects[pi].tabs.len() {
                let (id, ptid) = {
                    let t = &self.projects[pi].tabs[j];
                    (t.id.clone(), t.parent_tab_id.clone())
                };
                if id == originating_tab_id || ptid.as_deref() == Some(originating_tab_id) {
                    self.projects[pi].tabs[j].parent_tab_id = Some(new_tab_id.to_string());
                }
            }
        }

        Some(parent)
    }

    /// Nest an already-constructed `tab` one indent under `under_tab_id`,
    /// applying the same depth-1 rule — but, unlike
    /// [`TabModel::insert_branch_parent`], **without** re-parenting the anchor's
    /// former children (the anchor stays the root, so its existing depth-1
    /// children remain valid; this asymmetry is deliberate). Inserted
    /// immediately after the anchor. Returns `false` (mutating nothing) when
    /// the anchor is unknown or in the Terminals group (`TabModel.swift:401-416`).
    pub fn insert_handoff_child(&mut self, tab: Tab, under_tab_id: &str) -> bool {
        let Some((pi, ti)) = self.project_tab_index(under_tab_id) else {
            return false;
        };
        if self.is_terminals_project_tab(under_tab_id) {
            return false;
        }
        let originating_parent = self.projects[pi].tabs[ti].parent_tab_id.clone();
        let mut child = tab;
        child.parent_tab_id = Some(originating_parent.unwrap_or_else(|| under_tab_id.to_string()));
        self.projects[pi].tabs.insert(ti + 1, child);
        true
    }

    // MARK: - Removal

    /// Remove the tab at `(project_index, tab_index)` and sweep any sibling
    /// `parent_tab_id` references that pointed at it, atomically. The single
    /// removal entry point — every removal path must funnel through here so the
    /// parent-pointer sweep can't be skipped (`TabModel.swift:237-241`).
    pub fn remove_tab(&mut self, project_index: usize, tab_index: usize) -> Tab {
        let removed = self.projects[project_index].tabs.remove(tab_index);
        self.clear_dangling_parent_references(&removed.id);
        removed
    }

    /// Clear `parent_tab_id` on every tab that pointed at `removed_tab_id`
    /// (`TabModel.swift:249-257`).
    pub fn clear_dangling_parent_references(&mut self, removed_tab_id: &str) {
        for pi in 0..self.projects.len() {
            for ti in 0..self.projects[pi].tabs.len() {
                if self.projects[pi].tabs[ti].parent_tab_id.as_deref() == Some(removed_tab_id) {
                    self.projects[pi].tabs[ti].parent_tab_id = None;
                }
            }
        }
    }

    /// Sweep every `parent_tab_id` against the set of present tab ids and clear
    /// any dangling one. Called after a full-tree restore so a hand-edited or
    /// partially-corrupt snapshot can't leave a child indented under a tab that
    /// doesn't exist. Pure cleanup — safe to call repeatedly
    /// (`TabModel.swift:427-442`).
    pub fn prune_dangling_parent_references(&mut self) {
        let valid: HashSet<String> = self
            .projects
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect();
        for pi in 0..self.projects.len() {
            for ti in 0..self.projects[pi].tabs.len() {
                if let Some(parent) = &self.projects[pi].tabs[ti].parent_tab_id {
                    if !valid.contains(parent) {
                        self.projects[pi].tabs[ti].parent_tab_id = None;
                    }
                }
            }
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
    /// [`TabModel::extract_worktree_name`]) (`TabModel.swift:1129-1145`).
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
    /// companion terminal's `Tab.cwd` lands in the same directory Claude
    /// actually created (`SessionsModel.swift:677-682`). Pure counterpart to
    /// [`TabModel::extract_worktree_name`] (which pulls the raw `-w` value);
    /// the caller joins `<cwd>/.claude/worktrees/<sanitized>`.
    pub fn sanitize_worktree_name(name: &str) -> String {
        name.replace('/', "+")
    }
}

// MARK: - Pure free helpers

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
/// absolute paths [`TabModel::find_git_root`] walks: "/a/b" → "/a", "/a" → "/",
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

/// Mint a pane id shaped like the Swift seed's (`<prefix>-p<ms>`).
fn mint_pane_id(prefix: &str) -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}-p{}", prefix, ms)
}

/// A short unique suffix for a generated project id (Swift uses a UUID prefix).
/// A process-local counter mixed with the clock keeps back-to-back appends in
/// the same instant — e.g. inside the repair tab-move loop — from colliding.
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
