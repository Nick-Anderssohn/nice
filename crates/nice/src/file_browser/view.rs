//! `FileBrowserView` — the gpui view for the sidebar's files mode (R19 "What to
//! build" #5/#6). Mounted by [`crate::sidebar_shell::SidebarShellView`]'s
//! `build_body` in place of the landed placeholder when the sidebar is in files
//! mode and not peeking (peeking always shows the tab list — the preserved
//! invariant).
//!
//! It renders a disclosure tree rooted at the active tab's cwd over the pure
//! [`nice_model::file_browser`] model family:
//!
//! * a project header (title via [`nice_model::file_browser::file_browser_header_title`];
//!   click resets the root to the tab cwd),
//! * a control strip (up-nav, sort-criterion menu, direction toggle, hidden
//!   toggle) driving the persisted [`SortSettingsStore`](crate::file_browser::sort_settings_store::SortSettingsStore)
//!   and the per-tab [`FileBrowserState`](nice_model::file_browser::FileBrowserState),
//! * a [`gpui::uniform_list`] over the flattened
//!   [`visible_order`](nice_model::file_browser::listing::visible_order) projection
//!   (fixed-height rows index straight in), and
//! * the missing-folder / no-active-tab empty states.
//!
//! Clicks route through the hand-rolled 280 ms
//! [`FileBrowserClickRouter`](nice_model::file_browser::FileBrowserClickRouter)
//! (never gpui's native `click_count`); a right-click opens the R19 context menu
//! (Open, Open With ▸, Reveal in Finder, ─, Copy Path — R20 adds the rest).
//! Every OS action routes through the injectable
//! [`WorkspaceOps`](crate::file_browser::workspace_ops::WorkspaceOps) Global, so
//! no test or scenario ever launches a real app. A per-window
//! [`DirectoryWatcherHub`] watches the expanded dirs currently on screen and, on
//! a change, re-renders (a fresh read heals the row set — reload-on-render).
//!
//! The files-mode root carries `.id()` + `role(Group)` +
//! `aria_label("nice-file-browser-root")` — the shipped-surface AX anchor the
//! `file-browser` scenario walks for (`app_shell.rs:68` convention).

use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, App, Context, Entity, ExternalPaths, FocusHandle,
    Focusable, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, Rgba,
    ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window,
};

use nice_model::file_browser::listing::{path_is_dir, visible_order};
use nice_model::file_browser::menu::FileBrowserContextMenuItem;
use nice_model::file_browser::{
    file_browser_header_title, preselect_len, ClickModifier, FileBrowserClickRouter,
    FileBrowserContextMenuModel, FileBrowserSortCriterion, FileBrowserSortSettings, TextFieldEditor,
    TextFieldKey, DOUBLE_CLICK_WINDOW,
};
use nice_model::InlineRenameClickGate;

use crate::app_shell::PaneHostView;
use crate::file_browser::cwd_snapshot::build_snapshot;
use crate::file_browser::rename::{self, ConfirmSpec, RenameCommit};
use crate::inline_rename::{
    apply_rename_click, dispatch_rename_key, edit_spans, EditSpans, FieldColors, FieldProbe,
    RenameKeyOutcome,
};
use nice_theme::color::Srgba;
use nice_theme::palette::Slots;

use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::file_browser::history::FileOperationHistoryGlobal;
use crate::file_browser::ops::{paste_destination, FileOperationOrigin};
use crate::file_browser::pasteboard::{FilePasteboardGlobal, Intent};
use crate::file_browser::sort_settings_store::SortSettingsStore;
use crate::file_browser::watcher::DirectoryWatcherHub;
use crate::file_browser::workspace_ops::{open_with_entries, WorkspaceOpsGlobal};
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::theme::{slot_to_rgba, slot_srgba, srgba_to_rgba, srgba_with_alpha};
use crate::window_state::WindowState;

/// The shipped-surface AX anchor label for the files-mode root (the `ax-probe` /
/// `app-shell` convention — role + label only, no title).
pub(crate) const FILE_BROWSER_ROOT_LABEL: &str = "nice-file-browser-root";

// -- geometry (Swift parity) --------------------------------------------------

/// Fixed row height (uniform per the `uniform_list` contract).
const ROW_HEIGHT: f32 = 22.0;
/// Depth indent per tree level (`FileBrowserView.swift` row leading).
const INDENT_PER_LEVEL: f32 = 16.0;
/// Disclosure chevron slot width.
const DISCLOSURE_SLOT: f32 = 12.0;
/// Icon frame width.
const ICON_FRAME: f32 = 16.0;
/// Name text size.
const NAME_SIZE: f32 = 13.0;
/// Icon glyph point size.
const ICON_SIZE: f32 = 13.0;
/// Control-strip icon-button frame (the 20 pt landed idiom).
const STRIP_BUTTON: f32 = 20.0;
/// Row hover fill: 6% ink (the sidebar row convention).
const HOVER_INK_ALPHA: f32 = 0.06;
/// Selection tint alpha on the accent.
const SEL_ALPHA: f32 = 0.22;
/// Drag-hover highlight alpha on the accent (F9 — the target folder's tint).
const DRAG_HOVER_ALPHA: f32 = 0.30;
/// The trailing quiet-window poll cadence for the watcher drain (nap-safe: a
/// background-executor timer runs on an OS thread, never the App-Nap-deferred
/// runloop — the watcher's own `wake_main_runloop` keeps latency tight).
const WATCH_DRAIN_MS: u64 = 120;

// -- disclosure glyphs (glyph swap, like the sidebar chevron) -----------------

const CHEVRON_CLOSED: &str = "\u{25B8}"; // ▸
const CHEVRON_OPEN: &str = "\u{25BE}"; // ▾

/// One rendered tree row, snapshotted from the model each render so the
/// `uniform_list` closure (which is `'static`) can index straight in.
#[derive(Clone)]
struct RowVm {
    path: String,
    name: String,
    depth: usize,
    is_dir: bool,
    is_expanded: bool,
    is_selected: bool,
    is_root: bool,
    /// R20 (F7): the row is on the pasteboard with cut intent — rendered ghosted
    /// (0.45 opacity) until the cut is pasted or invalidated.
    is_cut: bool,
    icon_symbol: &'static str,
    icon_glyph: &'static str,
    /// R20 (F8): when this row is being renamed, the field's text split into the
    /// pre-selection / selection / post-selection spans (else the plain name label
    /// renders). `None` for every non-editing row.
    editing: Option<EditSpans>,
    /// R20 (F9): the drag set this row's `on_drag` carries — the whole selection
    /// when the row is selected, else just this row (Finder's select-then-drag).
    drag_paths: Vec<String>,
}

/// The per-render snapshot the view builds its element tree from.
struct Snapshot {
    root: String,
    header: String,
    show_hidden: bool,
    settings: FileBrowserSortSettings,
    rows: Vec<RowVm>,
    /// Whether the root path exists on disk (else the missing-folder empty state).
    root_exists: bool,
}

/// The file-browser view (see the module docs). One per window, held by the
/// window's [`SidebarShellView`](crate::sidebar_shell::SidebarShellView) and
/// created lazily when the sidebar first enters files mode.
pub(crate) struct FileBrowserView {
    /// The window's shared state — the [`FileBrowserStore`](nice_model::file_browser::FileBrowserStore),
    /// the [`TabModel`](nice_model::TabModel) (active tab + cwd + header title).
    state: Entity<WindowState>,
    /// Re-render when the shared state notifies (active-tab / cwd changes).
    _state_sub: Subscription,
    /// The uniform-list scroll handle (reset to the top on a root/tab change).
    scroll: UniformListScrollHandle,
    /// The hand-rolled 280 ms double-click + modifier router.
    router: FileBrowserClickRouter,
    /// The per-window kqueue watcher (lazily created; `None` if `kqueue()` failed).
    watcher: Option<DirectoryWatcherHub>,
    /// The foreground drain loop pumping watcher changes into re-renders. Held so
    /// dropping the view cancels it.
    _watch_drain: Option<Task<()>>,
    /// The last desired watch set applied, so `set_watched` only fires on change.
    last_watched: Vec<String>,
    /// The root path at the last render — a change resets the scroll to the top.
    last_root: Option<String>,
    /// The flattened projection at the last render — the scenario's "what's on
    /// screen" read (refreshed only by a real re-render, so a watcher-driven row
    /// appearance is observable through it).
    rendered_paths: Vec<String>,
    /// The open context menu (first-stage or the Open With ▸ second stage).
    context_menu: Option<Entity<ContextMenu>>,
    /// The menu's dismiss subscription.
    menu_sub: Option<Subscription>,
    /// A deferred Open With ▸ second-stage open: `(target_path, anchor)`. Set by
    /// the first menu's "Open With ▸" entry; consumed on the next render (which
    /// has the `Window` `ContextMenu::new` needs).
    pending_open_with: Option<(String, Point<Pixels>)>,
    /// Root focus handle (hosts the AX anchor).
    focus_handle: FocusHandle,
    /// The user's accent (selection tint).
    accent: Srgba,
    /// The window backing scale (re-sampled each render for the SF-symbol cache).
    window_scale: f32,
    /// R20 (F8): a one-shot rename request set by the context-menu "Rename"
    /// handler, the Return trigger, and the slow-second-click deferral. Consumed
    /// by the next render (which has the `Window` [`begin_rename`](Self::begin_rename)
    /// needs to grab field focus) exactly like [`pending_open_with`](Self::pending_open_with).
    pending_rename_path: Option<String>,
    /// R20 (F8): the active inline-rename edit session (the field's editing model
    /// + its target), or `None` when no row is being renamed.
    rename: Option<RenameState>,
    /// The rename field's focus handle — grabbed when a rename begins (so
    /// commit-on-blur fires when focus leaves), distinct from the panel
    /// [`focus_handle`](Self::focus_handle).
    rename_focus: FocusHandle,
    /// Commit-on-blur subscription, alive while a rename is active (dropped on
    /// commit / cancel so a stale blur can't re-fire).
    rename_blur_sub: Option<Subscription>,
    /// The Swift focus-call-counter test seam: bumped on EVERY rename exit path
    /// (commit / cancel / validation failure / modal cancel) so a test asserts
    /// focus was handed back exactly once per rename.
    refocus_count: usize,
    /// Generation guard for the slow-second-click deferral — a bumped value
    /// cancels an armed-but-not-yet-fired deferred rename (a fast second click,
    /// a selection change, or a rename begin).
    rename_click_gen: u64,
    /// The path currently the SOLE selection and when it became so — the
    /// `activated_at` stamp the slow-second-click gate ([`InlineRenameClickGate`])
    /// reads.
    sole_activated: Option<(String, Instant)>,
    /// The window's pane host, so a rename exit hands key focus back to the active
    /// terminal (`refocus_terminal_after_rename` parity). Pushed down by
    /// [`SidebarShellView`](crate::sidebar_shell::SidebarShellView).
    pane_host: Option<Entity<PaneHostView>>,
    /// The rename field's painted geometry (text-run + field-box left edges,
    /// window coordinates), written by the field's layout probes each paint and
    /// read by its click handler to turn a click-x into a caret position.
    rename_probe: Rc<Cell<FieldProbe>>,
}

/// The active inline-rename edit session (F8): the pure editing model plus the
/// target it renames.
struct RenameState {
    path: String,
    is_dir: bool,
    editor: TextFieldEditor,
}

impl FileBrowserView {
    /// Construct over the window's shared [`WindowState`] entity. Spawns the
    /// per-window watcher + its foreground drain loop.
    pub(crate) fn new(state: Entity<WindowState>, accent: Srgba, cx: &mut Context<Self>) -> Self {
        let state_sub = cx.observe(&state, |_this, _state, cx| cx.notify());
        let watcher = DirectoryWatcherHub::new().ok();
        let watch_drain = watcher.as_ref().map(|_| {
            cx.spawn(async move |this, acx| loop {
                acx.background_executor()
                    .timer(Duration::from_millis(WATCH_DRAIN_MS))
                    .await;
                let alive = this
                    .update(acx, |this: &mut Self, cx| {
                        let changed = this
                            .watcher
                            .as_ref()
                            .map(|w| !w.drain_changes().is_empty())
                            .unwrap_or(false);
                        if changed {
                            // A watched dir changed: re-render, which re-reads the
                            // affected listings (reload-on-render self-healing).
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    return; // the view entity is gone
                }
            })
        });
        Self {
            state,
            _state_sub: state_sub,
            scroll: UniformListScrollHandle::new(),
            router: FileBrowserClickRouter::new(),
            watcher,
            _watch_drain: watch_drain,
            last_watched: Vec::new(),
            last_root: None,
            rendered_paths: Vec::new(),
            context_menu: None,
            menu_sub: None,
            pending_open_with: None,
            focus_handle: cx.focus_handle(),
            accent,
            window_scale: 2.0,
            pending_rename_path: None,
            rename: None,
            rename_focus: cx.focus_handle(),
            rename_blur_sub: None,
            refocus_count: 0,
            rename_click_gen: 0,
            sole_activated: None,
            pane_host: None,
            rename_probe: Rc::new(Cell::new(FieldProbe::default())),
        }
    }

    /// Push down the window's pane host so a rename exit hands key focus back to
    /// the active terminal (called by [`SidebarShellView`](crate::sidebar_shell::SidebarShellView)).
    pub(crate) fn set_pane_host(&mut self, host: Entity<PaneHostView>) {
        self.pane_host = Some(host);
    }

    // MARK: - Snapshot

    /// Ensure the active tab's browser state exists and read a render snapshot.
    /// `None` when there is no active tab (⇒ the empty panel).
    fn snapshot(&mut self, cx: &mut Context<Self>) -> Option<Snapshot> {
        let (tab_id, cwd) = {
            let ws = self.state.read(cx);
            let tab_id = ws.model.active_tab_id()?.to_string();
            let cwd = ws
                .model
                .tab_for(&tab_id)
                .map(|t| t.cwd.clone())
                .unwrap_or_default();
            (tab_id, cwd)
        };
        // Lazily create the per-tab state (cwd is a seed used only on first
        // creation — a re-render never resets the user's in-state navigation).
        self.state.update(cx, |ws, _| {
            ws.file_browser.ensure_state(&tab_id, &cwd);
        });

        let settings = cx
            .try_global::<SortSettingsStore>()
            .map(|s| s.settings())
            .unwrap_or_default();

        let ws = self.state.read(cx);
        let st = ws.file_browser.state_for(&tab_id)?;
        let root = st.root_path().to_string();
        let expanded: BTreeSet<String> = st.expanded_paths().clone();
        let show_hidden = st.show_hidden();
        let selected: HashSet<String> = st.selection().selected_paths().clone();
        let header = file_browser_header_title(&ws.model, &tab_id);
        // The shared-state borrow (`ws`) ends here (NLL) — everything below is
        // computed from the owned locals cloned out above.

        // R20 (F7): the observable cut set — rows in it render ghosted. Empty when
        // no pasteboard Global is installed or the cut companion is stale.
        let cut: HashSet<PathBuf> = cx
            .try_global::<FilePasteboardGlobal>()
            .map(|g| g.0.cut_paths())
            .unwrap_or_default();

        // R20 (F8): the row currently in rename edit mode, plus its field spans.
        let editing: Option<(String, EditSpans)> = self
            .rename
            .as_ref()
            .map(|r| (r.path.clone(), edit_spans(&r.editor)));

        let root_exists = Path::new(&root).exists();
        let projection = if root_exists {
            visible_order(
                &root,
                &expanded,
                show_hidden,
                settings.criterion,
                settings.ascending,
            )
        } else {
            Vec::new()
        };

        // If the row being renamed disappeared (deleted / collapsed out of view),
        // drop the draft (Swift parity — the field goes away with the row).
        if let Some((path, _)) = &editing {
            if !projection.iter().any(|p| p == path) {
                self.rename = None;
                self.rename_blur_sub = None;
            }
        }

        // R20 (F9): the selected rows in on-screen order — a drag of any selected
        // row carries the whole selection.
        let ordered_selection: Vec<String> =
            projection.iter().filter(|p| selected.contains(*p)).cloned().collect();

        let rows = projection
            .iter()
            .map(|p| {
                let is_dir = is_dir_resolved(p);
                let is_expanded = is_dir && expanded.contains(p);
                let is_selected = selected.contains(p);
                RowVm {
                    name: last_component(p),
                    depth: depth_of(&root, p),
                    is_dir,
                    is_expanded,
                    is_selected,
                    is_root: p == &root,
                    is_cut: cut.contains(Path::new(p)),
                    icon_symbol: icon_symbol(p, is_dir, is_expanded),
                    icon_glyph: icon_glyph(is_dir),
                    editing: editing
                        .as_ref()
                        .filter(|(path, _)| path == p)
                        .map(|(_, spans)| spans.clone()),
                    drag_paths: if is_selected && ordered_selection.len() > 1 {
                        ordered_selection.clone()
                    } else {
                        vec![p.clone()]
                    },
                    path: p.clone(),
                }
            })
            .collect();

        self.rendered_paths = projection;
        Some(Snapshot {
            root,
            header,
            show_hidden,
            settings,
            rows,
            root_exists,
        })
    }

    /// Recompute the watcher's desired set (expanded dirs currently on screen +
    /// the root, in visible order) and apply it only when it changed.
    fn sync_watcher(&mut self, snap: &Snapshot) {
        let Some(watcher) = &self.watcher else { return };
        let mut desired: Vec<String> = Vec::new();
        for row in &snap.rows {
            if row.is_root || (row.is_dir && row.is_expanded) {
                desired.push(row.path.clone());
            }
        }
        if desired != self.last_watched {
            watcher.set_watched(desired.clone());
            self.last_watched = desired;
        }
    }

    // MARK: - Mutation helpers (all through the per-tab FileBrowserState)

    /// Run `f` against the active tab's browser state (creating it if needed),
    /// then notify. `None`-tab is a no-op.
    fn with_active_fb_state(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut nice_model::file_browser::FileBrowserState),
    ) {
        let Some((tab_id, cwd)) = self.active_tab_cwd(cx) else {
            return;
        };
        self.state.update(cx, |ws, _| {
            let st = ws.file_browser.ensure_state(&tab_id, &cwd);
            f(st);
        });
        cx.notify();
    }

    fn active_tab_cwd(&self, cx: &App) -> Option<(String, String)> {
        let ws = self.state.read(cx);
        let tab_id = ws.model.active_tab_id()?.to_string();
        let cwd = ws
            .model
            .tab_for(&tab_id)
            .map(|t| t.cwd.clone())
            .unwrap_or_default();
        Some((tab_id, cwd))
    }

    /// The current flattened projection (fresh read) — the ⇧-range order source.
    fn current_projection(&self, cx: &App) -> Vec<String> {
        let settings = cx
            .try_global::<SortSettingsStore>()
            .map(|s| s.settings())
            .unwrap_or_default();
        let ws = self.state.read(cx);
        let Some((tab_id, _)) = self.active_tab_cwd(cx) else {
            return Vec::new();
        };
        let Some(st) = ws.file_browser.state_for(&tab_id) else {
            return Vec::new();
        };
        visible_order(
            st.root_path(),
            st.expanded_paths(),
            st.show_hidden(),
            settings.criterion,
            settings.ascending,
        )
    }

    /// Route a row click through the 280 ms detector and apply its effect. Any
    /// click first cancels a pending slow-second-click deferral (bumps the
    /// generation); a slow second click on an already-sole-selected FILE re-arms
    /// it. Folders keep their single-click expand/collapse (a folder's slow second
    /// click is claimed by expand/collapse, so folder rename stays on the menu /
    /// Return triggers — a documented divergence that keeps R19's expand/collapse
    /// contract intact).
    fn on_row_click(&mut self, path: &str, modifier: ClickModifier, cx: &mut Context<Self>) {
        use nice_model::file_browser::ClickAction::*;
        self.rename_click_gen += 1; // any click cancels a pending deferral
        let now = Instant::now();
        let was_sole = self.is_sole_selected(path, cx);
        let projection = self.current_projection(cx);
        let action = self.router.route(path, modifier, now);
        match action {
            Toggle { path } => {
                self.sole_activated = None;
                self.with_active_fb_state(cx, |st| st.selection_mut().toggle(&path));
            }
            Extend { path } => {
                self.sole_activated = None;
                self.with_active_fb_state(cx, |st| st.selection_mut().extend(&path, &projection));
            }
            SingleActivate { path } => {
                let is_dir = is_dir_resolved(&path);
                self.with_active_fb_state(cx, |st| {
                    st.selection_mut().replace(&[path.clone()], None);
                    // Primary action: a folder toggles expansion; a file only
                    // selects (files open on DOUBLE click).
                    if is_dir {
                        st.toggle_expansion(&path);
                    }
                });
                // Slow-second-click rename (files only): if the row was ALREADY the
                // sole selection and the click gate has elapsed, arm the deferral.
                let activated_at = self.sole_activated.as_ref().and_then(|(p, t)| {
                    if p == &path {
                        Some(*t)
                    } else {
                        None
                    }
                });
                if !is_dir
                    && was_sole
                    && InlineRenameClickGate::can_begin_edit(activated_at, now, DOUBLE_CLICK_WINDOW)
                {
                    self.arm_slow_rename(path.clone(), cx);
                } else {
                    // Newly sole-selected — stamp the activation clock.
                    self.sole_activated = Some((path.clone(), now));
                }
            }
            DoubleActivate { path } => {
                self.sole_activated = None;
                if is_dir_resolved(&path) {
                    self.with_active_fb_state(cx, |st| st.set_root_path(&path));
                } else {
                    self.workspace_open(&path, cx);
                }
            }
        }
    }

    /// Clear the selection (empty-area click / click-away deselect handlers).
    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.with_active_fb_state(cx, |st| st.selection_mut().clear());
    }

    // MARK: - WorkspaceOps (all OS integration through the injectable Global)

    fn workspace_open(&self, path: &str, cx: &App) {
        if let Some(g) = cx.try_global::<WorkspaceOpsGlobal>() {
            g.0.open(path);
        }
    }

    fn workspace_reveal(&self, path: &str, cx: &App) {
        if let Some(g) = cx.try_global::<WorkspaceOpsGlobal>() {
            g.0.reveal(path);
        }
    }

    fn workspace_open_with(&self, path: &str, app_path: &str, cx: &App) {
        if let Some(g) = cx.try_global::<WorkspaceOpsGlobal>() {
            g.0.open_with(path, app_path);
        }
    }

    /// Copy Path: newline-join the target paths onto the pasteboard (Finder "Copy
    /// as Pathname" parity). R20 reroutes this through the pasteboard adapter (the
    /// browser's SINGLE pasteboard writer) so a text write also clears any cut
    /// companion — the gpui clipboard API is no longer used here. A no-op when no
    /// pasteboard Global is installed (never a fallback to the real general
    /// pasteboard — hermeticity).
    fn copy_paths(&self, paths: &[String], cx: &mut App) {
        if paths.is_empty() {
            return;
        }
        let joined = paths.join("\n");
        if cx.has_global::<FilePasteboardGlobal>() {
            cx.global_mut::<FilePasteboardGlobal>().0.write_text(&joined);
        }
    }

    // MARK: - Context menu (right-click → pure-read selection → R19 entries)

    /// Open the R19 row context menu at `position`. Visibility comes from the pure
    /// [`FileBrowserContextMenuModel`] (Open / Open With hidden on directories);
    /// R19 renders only its owned entries (Open, Open With ▸, Reveal in Finder, ─,
    /// Copy Path). The snap-on-action rule fires from each item's handler, not on
    /// the right-click itself.
    fn open_row_menu(
        &mut self,
        path: &str,
        is_dir: bool,
        is_root: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // R20 flips the two R19-frozen capabilities (no model reordering):
        //  * can_paste — a lazy adapter read at menu open (file URLs present?).
        //  * can_rename — the pure `/`-gate AND single-target (multi-select hides it).
        let can_paste = cx
            .try_global::<FilePasteboardGlobal>()
            .map(|g| g.0.read().is_some())
            .unwrap_or(false);
        let can_rename = nice_model::file_browser::rename_validator::can_rename(path)
            && self.selection_count(cx) <= 1;
        let model = FileBrowserContextMenuModel::build(is_dir, is_root, can_paste, can_rename);
        let weak = cx.weak_entity();
        let clicked = path.to_string();
        let mut items: Vec<ContextMenuItem> = Vec::new();
        for item in &model.items {
            match item {
                FileBrowserContextMenuItem::Open => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Open", move |_w, app| {
                        let _ = e.update(app, |this, cx| {
                            let targets = this.snap_and_resolve(&p, cx);
                            for t in &targets {
                                this.workspace_open(t, cx);
                            }
                        });
                    }));
                }
                FileBrowserContextMenuItem::OpenWith => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    // The two-stage divergence: dismiss this menu, then open a
                    // SECOND menu at the same anchor listing the apps (enumeration
                    // runs lazily at that second-stage open). `▸` signals the drill.
                    items.push(ContextMenuItem::entry("Open With \u{25B8}", move |_w, app| {
                        let _ = e.update(app, |this, _cx| {
                            this.pending_open_with = Some((p.clone(), position));
                        });
                    }));
                }
                FileBrowserContextMenuItem::RevealInFinder => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Reveal in Finder", move |_w, app| {
                        let _ = e.update(app, |this, cx| {
                            let targets = this.snap_and_resolve(&p, cx);
                            for t in &targets {
                                this.workspace_reveal(t, cx);
                            }
                        });
                    }));
                }
                FileBrowserContextMenuItem::DividerOpen => {
                    items.push(ContextMenuItem::separator());
                }
                FileBrowserContextMenuItem::CopyPath => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Copy Path", move |_w, app| {
                        let _ = e.update(app, |this, cx| {
                            let targets = this.snap_and_resolve(&p, cx);
                            this.copy_paths(&targets, cx);
                        });
                    }));
                }
                // R20 handlers — each snaps the selection first (Finder's
                // snap-on-action), then performs its op through the shared service /
                // pasteboard adapter.
                FileBrowserContextMenuItem::Rename => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Rename", move |_w, app| {
                        let _ = e.update(app, |this, cx| this.menu_rename(&p, cx));
                    }));
                }
                FileBrowserContextMenuItem::Copy => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Copy", move |_w, app| {
                        let _ = e.update(app, |this, cx| this.menu_copy(&p, cx));
                    }));
                }
                FileBrowserContextMenuItem::Cut => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Cut", move |_w, app| {
                        let _ = e.update(app, |this, cx| this.menu_cut(&p, cx));
                    }));
                }
                FileBrowserContextMenuItem::Paste => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Paste", move |_w, app| {
                        let _ = e.update(app, |this, cx| this.menu_paste(&p, is_dir, cx));
                    }));
                }
                FileBrowserContextMenuItem::Trash => {
                    let e = weak.clone();
                    let p = clicked.clone();
                    items.push(ContextMenuItem::entry("Move to Trash", move |_w, app| {
                        let _ = e.update(app, |this, cx| this.menu_trash(&p, cx));
                    }));
                }
            }
        }
        self.present_menu(items, position, window, cx);
    }

    /// Build the Open With ▸ second-stage menu: enumerate the apps that can open
    /// `path` through the [`WorkspaceOps`](crate::file_browser::workspace_ops::WorkspaceOps)
    /// Global (lazy — runs here), order them (default first labeled "<Name>
    /// (default)", remainder alphabetized, deduped, synthesized default if
    /// missing), plus a trailing "Other…" chooser.
    fn open_open_with_menu(
        &mut self,
        path: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let apps = match cx.try_global::<WorkspaceOpsGlobal>() {
            Some(g) => g.0.apps_for(path),
            None => return,
        };
        let entries = open_with_entries(&apps);
        let weak = cx.weak_entity();
        let mut items: Vec<ContextMenuItem> = Vec::new();
        for entry in entries {
            let label = if entry.is_default {
                format!("{} (default)", entry.display_name)
            } else {
                entry.display_name.clone()
            };
            let e = weak.clone();
            let p = path.to_string();
            let app_path = entry.app_path.clone();
            items.push(ContextMenuItem::entry(label, move |_w, app| {
                let _ = e.update(app, |this, cx| {
                    this.snap_and_resolve(&p, cx);
                    this.workspace_open_with(&p, &app_path, cx);
                });
            }));
        }
        // Trailing "Other…" — the NSOpenPanel chooser through the same seam.
        let e = weak.clone();
        let p = path.to_string();
        items.push(ContextMenuItem::entry("Other\u{2026}", move |_w, app| {
            let _ = e.update(app, |this, cx| {
                let chosen = cx
                    .try_global::<WorkspaceOpsGlobal>()
                    .and_then(|g| g.0.choose_application());
                if let Some(app_path) = chosen {
                    this.snap_and_resolve(&p, cx);
                    this.workspace_open_with(&p, &app_path, cx);
                }
            });
        }));
        self.present_menu(items, position, window, cx);
    }

    /// Present `items` as a [`ContextMenu`] and subscribe to its dismissal.
    fn present_menu(
        &mut self,
        items: Vec<ContextMenuItem>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|mcx| ContextMenu::new(position, items, window, mcx));
        let sub = cx.subscribe(&menu, |this, _menu, _e: &gpui::DismissEvent, cx| {
            this.context_menu = None;
            this.menu_sub = None;
            cx.notify();
        });
        self.context_menu = Some(menu);
        self.menu_sub = Some(sub);
        cx.notify();
    }

    /// Apply the Finder snap-on-action rule for a right-clicked `path` and return
    /// the resolved target paths (the whole selection when the clicked row is in
    /// it, else just the clicked row).
    fn snap_and_resolve(&mut self, path: &str, cx: &mut Context<Self>) -> Vec<String> {
        let mut selected: HashSet<String> = HashSet::new();
        self.with_active_fb_state(cx, |st| {
            st.selection_mut().snap_if_right_click_outside(path);
            selected = st.selection().selected_paths().clone();
        });
        if selected.is_empty() {
            return vec![path.to_string()];
        }
        // Order the resolved targets by the current visible projection so ordered
        // output (Copy Path in particular) is deterministic and matches on-screen
        // order — the selection itself is an unordered `HashSet`.
        let mut ordered: Vec<String> = self
            .current_projection(cx)
            .into_iter()
            .filter(|p| selected.contains(p))
            .collect();
        // Any selected paths that aren't currently visible (e.g. under a
        // since-collapsed parent) still get resolved, appended in a stable
        // sorted order rather than arbitrary hash order.
        let mut extras: Vec<String> = selected
            .into_iter()
            .filter(|p| !ordered.contains(p))
            .collect();
        extras.sort();
        ordered.extend(extras);
        if ordered.is_empty() {
            ordered.push(path.to_string());
        }
        ordered
    }

    // MARK: - R20 file-operation menu handlers (all through the snap hook)

    /// The number of paths selected for the active tab (multi-select gate).
    fn selection_count(&self, cx: &App) -> usize {
        let Some((tab_id, _)) = self.active_tab_cwd(cx) else {
            return 0;
        };
        self.state
            .read(cx)
            .file_browser
            .state_for(&tab_id)
            .map(|s| s.selection().selected_paths().len())
            .unwrap_or(0)
    }

    /// The op origin: this window's session id + active tab (undo routes back here).
    fn origin(&self, cx: &App) -> FileOperationOrigin {
        let ws = self.state.read(cx);
        FileOperationOrigin::new(
            ws.session_id().to_string(),
            ws.model.active_tab_id().map(str::to_string),
        )
    }

    /// The shared history entity, if a Global is installed.
    fn history(&self, cx: &App) -> Option<Entity<crate::file_browser::history::FileOperationHistory>> {
        cx.try_global::<FileOperationHistoryGlobal>().map(|g| g.0.clone())
    }

    /// Context-menu "Rename": snap, then record the one-shot rename request the
    /// rename-UI slice consumes. Rename is single-target (the menu item is hidden
    /// on a multi-selection).
    fn menu_rename(&mut self, path: &str, cx: &mut Context<Self>) {
        self.snap_and_resolve(path, cx);
        self.pending_rename_path = Some(path.to_string());
        cx.notify();
    }

    /// Context-menu "Copy": snap, then write the resolved targets to the pasteboard
    /// with copy intent (external pasters see a plain `public.file-url` copy).
    fn menu_copy(&mut self, path: &str, cx: &mut Context<Self>) {
        let targets = self.snap_and_resolve(path, cx);
        self.write_pasteboard(&targets, Intent::Copy, cx);
    }

    /// Context-menu "Cut": snap, then write with cut intent (an in-process fiction
    /// that ghosts the rows; external pasters still see a copy).
    fn menu_cut(&mut self, path: &str, cx: &mut Context<Self>) {
        let targets = self.snap_and_resolve(path, cx);
        self.write_pasteboard(&targets, Intent::Cut, cx);
    }

    fn write_pasteboard(&mut self, targets: &[String], intent: Intent, cx: &mut Context<Self>) {
        if targets.is_empty() || !cx.has_global::<FilePasteboardGlobal>() {
            return;
        }
        let paths: Vec<PathBuf> = targets.iter().map(PathBuf::from).collect();
        cx.global_mut::<FilePasteboardGlobal>().0.write(&paths, intent);
        // Re-render so the cut ghost appears / clears on these rows.
        cx.notify();
    }

    /// Context-menu "Paste": snap, read the pasteboard, resolve the destination
    /// (into a directory row / a file's parent), dispatch copy (copy intent) or
    /// move (cut intent) through the shared service, push to the history, and clear
    /// the cut companion after a move. Service errors land on the drift banner.
    fn menu_paste(&mut self, clicked: &str, is_dir: bool, cx: &mut Context<Self>) {
        // Snap first (the Finder snap-on-action rule fires from every handler).
        self.snap_and_resolve(clicked, cx);
        let read = if cx.has_global::<FilePasteboardGlobal>() {
            cx.global::<FilePasteboardGlobal>().0.read()
        } else {
            None
        };
        let Some(read) = read else {
            return;
        };
        let Some(history) = self.history(cx) else {
            return;
        };
        let dest = paste_destination(Path::new(clicked), is_dir);
        let origin = self.origin(cx);
        let sources = read.urls.clone();
        let intent = read.intent;
        history.update(cx, |h, hcx| {
            let result = match intent {
                Intent::Copy => h.service().copy(&sources, &dest, origin),
                Intent::Cut => h.service().move_(&sources, &dest, origin),
            };
            match result {
                Ok(op) => h.push(op),
                Err(e) => h.set_drift_message(format!("Couldn't paste: {e}")),
            }
            hcx.notify();
        });
        if intent == Intent::Cut && cx.has_global::<FilePasteboardGlobal>() {
            cx.global_mut::<FilePasteboardGlobal>().0.clear_cut_intent();
        }
        cx.notify();
    }

    /// Context-menu "Move to Trash": snap, recycle the resolved targets through the
    /// injected `Trasher`, push to the history. Errors land on the drift banner.
    fn menu_trash(&mut self, path: &str, cx: &mut Context<Self>) {
        let targets = self.snap_and_resolve(path, cx);
        if targets.is_empty() {
            return;
        }
        let Some(history) = self.history(cx) else {
            return;
        };
        let origin = self.origin(cx);
        let sources: Vec<PathBuf> = targets.iter().map(PathBuf::from).collect();
        history.update(cx, |h, hcx| {
            match h.service().trash(&sources, origin) {
                Ok(op) => h.push(op),
                Err(e) => h.set_drift_message(format!("Couldn't move to Trash: {e}")),
            }
            hcx.notify();
        });
        cx.notify();
    }

    // MARK: - R20 inline rename (F8)

    /// Enter rename edit mode for `path`: seed the field with the basename
    /// preselected (files with an extension select the base only; folders /
    /// extension-less / dotfiles select everything), grab field focus, and arm
    /// commit-on-blur. Cancels any pending slow-second-click deferral.
    fn begin_rename(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_click_gen += 1; // cancel any armed deferral
        let is_dir = is_dir_resolved(path);
        let name = last_component(path);
        let editor = TextFieldEditor::with_selection(&name, preselect_len(&name, is_dir));
        self.rename = Some(RenameState {
            path: path.to_string(),
            is_dir,
            editor,
        });
        self.rename_focus.focus(window, cx);
        self.arm_commit_on_blur(window, cx);
        cx.notify();
    }

    /// (Re)install the commit-on-blur subscription (the ported one-shot guard: a
    /// prior subscription is dropped OUTSIDE its own callback).
    fn arm_commit_on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_blur_sub = Some(cx.on_blur(&self.rename_focus, window, |this, window, cx| {
            this.commit_rename(window, cx);
        }));
    }

    /// Apply one editing key to the active field.
    fn apply_editor_key(&mut self, key: TextFieldKey, cx: &mut Context<Self>) {
        if let Some(state) = self.rename.as_mut() {
            state.editor.apply_key(key);
            cx.notify();
        }
    }

    /// Apply a click hit-test to the rename field — single click drops the caret,
    /// double selects the word, triple selects all ([`apply_rename_click`]) — then
    /// re-grab field focus. The field's click handler already `stop_propagation`ed,
    /// so the row's begin-rename gate never re-trips.
    fn place_rename_cursor(
        &mut self,
        index: usize,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.rename.as_mut() {
            apply_rename_click(&mut state.editor, index, click_count);
            self.rename_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// The field's key handler: Return commits, Esc cancels, everything else edits
    /// the pure model through the shared [`dispatch_rename_key`]. Always stops
    /// propagation while editing so the keystroke never leaks to the keymap /
    /// terminal.
    fn on_rename_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        // The file-browser field owns its own Esc binding (there is no shell Esc
        // action over it), so cancel here before the shared dispatch (which leaves
        // Escape Ignored for owners like this).
        if ks.key == "escape" {
            self.cancel_rename(window, cx);
            cx.stop_propagation();
            return;
        }
        let outcome = {
            let Some(state) = self.rename.as_mut() else {
                return;
            };
            dispatch_rename_key(
                &mut state.editor,
                &ks.key,
                ks.key_char.as_deref(),
                ks.modifiers.shift,
                ks.modifiers.platform,
                ks.modifiers.control,
                window.capslock().on,
            )
        };
        match outcome {
            RenameKeyOutcome::Commit => self.commit_rename(window, cx),
            RenameKeyOutcome::Edited => cx.notify(),
            RenameKeyOutcome::Ignored => {}
        }
        cx.stop_propagation();
    }

    /// Commit the active rename. One-shot via `rename.take()` (an Esc-cancel /
    /// prior commit can't double-fire on the follow-on blur). Empty / unchanged
    /// cancels silently; `/`-or-`:` stays in edit mode; a sibling collision
    /// surfaces the frozen banner + cancels; otherwise proceeds through the two
    /// async confirmation modals to the apply.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.rename.take() else {
            return;
        };
        self.rename_blur_sub = None;
        let draft = state.editor.text();
        match rename::evaluate_commit(&state.path, &draft, |c| Path::new(c).exists()) {
            RenameCommit::Cancel => self.end_rename_refocus(window, cx),
            RenameCommit::StayInEdit => {
                // Keep the field open so the user fixes the illegal character.
                self.rename = Some(state);
                self.rename_focus.focus(window, cx);
                self.arm_commit_on_blur(window, cx);
                cx.notify();
            }
            RenameCommit::Collision(msg) => {
                self.publish_drift(msg, cx);
                self.end_rename_refocus(window, cx);
            }
            RenameCommit::Proceed { dest } => {
                let new_name = last_component(&dest.to_string_lossy());
                let snapshot = build_snapshot(cx);
                let specs = rename::modals_for(&state.path, &new_name, state.is_dir, &snapshot);
                let source = PathBuf::from(&state.path);
                let origin = self.origin(cx);
                self.run_rename_modals(specs, source, dest, origin, window, cx);
            }
        }
    }

    /// Cancel the active rename (Esc / row-disappear) and hand focus back.
    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.take().is_none() {
            return;
        }
        self.rename_blur_sub = None;
        self.end_rename_refocus(window, cx);
    }

    /// Present the ORDERED confirmation modals (extension-change, then CWD-impact)
    /// before applying; each modal's confirm advances to the next, any cancel
    /// aborts and refocuses the terminal (the fs stays untouched). Empty specs ⇒
    /// apply immediately.
    fn run_rename_modals(
        &mut self,
        specs: Vec<ConfirmSpec>,
        source: PathBuf,
        dest: PathBuf,
        origin: FileOperationOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((first, rest)) = specs.split_first() else {
            self.perform_rename(&source, &dest, origin, cx);
            self.end_rename_refocus(window, cx);
            return;
        };
        let first = first.clone();
        let rest = rest.to_vec();
        let weak = cx.weak_entity();
        self.state.update(cx, |ws, wcx| {
            ws.present_confirmation(
                first.title,
                first.message,
                first.confirm_label,
                first.cancel_label,
                true,
                move |confirmed, window, app| {
                    let _ = weak.update(app, |this, cx| {
                        if confirmed {
                            this.run_rename_modals(
                                rest.clone(),
                                source.clone(),
                                dest.clone(),
                                origin.clone(),
                                window,
                                cx,
                            );
                        } else {
                            this.end_rename_refocus(window, cx);
                        }
                    });
                },
                window,
                wcx,
            );
        });
    }

    /// Apply the validated rename through the shared service (a raw single-pair
    /// Move — collision auto-rename bypassed) and push to the history; a collision
    /// / error lands on the drift banner.
    fn perform_rename(
        &mut self,
        source: &Path,
        dest: &Path,
        origin: FileOperationOrigin,
        cx: &mut Context<Self>,
    ) {
        let Some(history) = self.history(cx) else {
            return;
        };
        let mut err = None;
        history.update(cx, |h, hcx| {
            match rename::apply_rename(h.service(), source, dest, origin) {
                Ok(op) => h.push(op),
                Err(e) => err = Some(e),
            }
            hcx.notify();
        });
        if let Some(e) = err {
            self.publish_drift(e, cx);
        }
        cx.notify();
    }

    /// Route a transient failure to the ONE drift channel (the per-window banner).
    fn publish_drift(&self, message: String, cx: &mut App) {
        if let Some(h) = self.history(cx) {
            h.update(cx, |h, hcx| {
                h.set_drift_message(message);
                hcx.notify();
            });
        }
    }

    /// Every rename exit funnels here: bump the focus-call counter (the test seam)
    /// and hand key focus back to the active terminal via the pane host.
    fn end_rename_refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refocus_count += 1;
        if let Some(host) = self.pane_host.clone() {
            host.update(cx, |host, cx| host.focus_active_terminal(window, cx));
        }
        cx.notify();
    }

    /// The path currently the sole selection, if exactly one row is selected.
    fn single_selected_path(&self, cx: &App) -> Option<String> {
        let (tab_id, _) = self.active_tab_cwd(cx)?;
        let paths = self
            .state
            .read(cx)
            .file_browser
            .state_for(&tab_id)?
            .selection()
            .selected_paths()
            .clone();
        if paths.len() == 1 {
            paths.into_iter().next()
        } else {
            None
        }
    }

    /// Whether `path` is exactly the sole selection.
    fn is_sole_selected(&self, path: &str, cx: &App) -> bool {
        self.single_selected_path(cx).as_deref() == Some(path)
    }

    /// Arm a deferred slow-second-click rename: after the 280 ms double-click
    /// window, if no fast second click bumped the generation and `path` is still
    /// the sole selection, request the rename (consumed on the next render). A
    /// fast second click reads as a double-click (open / re-root) and cancels this.
    fn arm_slow_rename(&mut self, path: String, cx: &mut Context<Self>) {
        self.rename_click_gen += 1;
        let generation = self.rename_click_gen;
        cx.spawn(async move |this, acx| {
            acx.background_executor()
                .timer(DOUBLE_CLICK_WINDOW)
                .await;
            let _ = this.update(acx, |this, cx| {
                if this.rename_click_gen == generation
                    && this.rename.is_none()
                    && this.is_sole_selected(&path, cx)
                {
                    this.pending_rename_path = Some(path.clone());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // MARK: - R20 in-tree drag & drop (F9)

    /// The active-tab selection in on-screen order (empty when none / no tab).
    fn ordered_selection(&self, cx: &App) -> Vec<String> {
        let Some((tab_id, _)) = self.active_tab_cwd(cx) else {
            return Vec::new();
        };
        let selected = match self.state.read(cx).file_browser.state_for(&tab_id) {
            Some(st) => st.selection().selected_paths().clone(),
            None => return Vec::new(),
        };
        self.current_projection(cx)
            .into_iter()
            .filter(|p| selected.contains(p))
            .collect()
    }

    /// The in-tree drag source set for `path`: the whole selection when `path` is
    /// selected, else just `path` (Finder's select-then-drag) — the same set the
    /// row's `on_drag` [`gpui::ExternalPaths`] payload carries. Pure; no state is
    /// recorded (the payload is authoritative at the drop seam).
    fn begin_row_drag(&mut self, path: &str, cx: &mut Context<Self>) -> Vec<String> {
        let selection = self.ordered_selection(cx);
        if selection.iter().any(|p| p == path) {
            selection
        } else {
            vec![path.to_string()]
        }
    }

    /// Handle a drop onto directory `dest`, moving/copying exactly the dropped
    /// [`gpui::ExternalPaths`] payload. Internal drags and Finder-inbound drops are
    /// indistinguishable here: the in-tree drag source carries the same
    /// `ExternalPaths` set (select-then-drag), so the payload is always the source
    /// of truth — no stale in-tree drag can redirect a later drop. Rejected by the
    /// pure `can_drop` rule ⇒ no-op.
    fn handle_drop(
        &mut self,
        dropped: &gpui::ExternalPaths,
        dest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sources: Vec<String> = dropped
            .paths()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.perform_internal_drop(&sources, dest, window, cx);
    }

    /// Resolve move-vs-copy (Option modifier read at drop time + same/cross-volume)
    /// and commit the drop. Rejected drops are dropped silently by the pure rule.
    fn perform_internal_drop(
        &mut self,
        sources: &[String],
        dest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
        if !nice_model::file_browser::can_drop(&refs, dest) {
            return;
        }
        let option_held = window.modifiers().alt;
        let same_volume = sources_share_volume(sources, dest);
        let op = nice_model::file_browser::drop_operation(option_held, same_volume);
        self.move_or_copy(sources, dest, op, cx);
    }

    /// Commit a resolved drop into `dest` (the pasteboard is skipped — a drag is
    /// not a cut). Push to the history; failures land on the drift channel.
    fn move_or_copy(
        &mut self,
        sources: &[String],
        dest: &str,
        op: nice_model::file_browser::FileDragOperation,
        cx: &mut Context<Self>,
    ) {
        use nice_model::file_browser::FileDragOperation;
        let Some(history) = self.history(cx) else {
            return;
        };
        let origin = self.origin(cx);
        let src_paths: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
        let dest_path = PathBuf::from(dest);
        history.update(cx, |h, hcx| {
            let result = match op {
                FileDragOperation::Move => h.service().move_(&src_paths, &dest_path, origin),
                FileDragOperation::Copy => h.service().copy(&src_paths, &dest_path, origin),
            };
            match result {
                Ok(recorded) => h.push(recorded),
                Err(e) => h.set_drift_message(format!("Couldn't move: {e}")),
            }
            hcx.notify();
        });
        cx.notify();
    }

    // MARK: - Control strip actions

    fn go_to_parent(&mut self, cx: &mut Context<Self>) {
        let Some((tab_id, _)) = self.active_tab_cwd(cx) else {
            return;
        };
        let root = self
            .state
            .read(cx)
            .file_browser
            .state_for(&tab_id)
            .map(|s| s.root_path().to_string())
            .unwrap_or_default();
        if root == "/" || root.is_empty() {
            return;
        }
        let parent = parent_path(&root);
        self.with_active_fb_state(cx, |st| st.set_root_path(parent));
    }

    fn reset_root_to_cwd(&mut self, cx: &mut Context<Self>) {
        let Some((_, cwd)) = self.active_tab_cwd(cx) else {
            return;
        };
        if cwd.is_empty() {
            return;
        }
        self.with_active_fb_state(cx, |st| st.set_root_path(cwd));
    }

    fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.with_active_fb_state(cx, |st| st.toggle_show_hidden());
    }

    /// Mutate the persisted sort settings through the write-through Global.
    fn update_sort(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut FileBrowserSortSettings)) {
        if !cx.has_global::<SortSettingsStore>() {
            return;
        }
        let mut s = cx.global::<SortSettingsStore>().settings();
        f(&mut s);
        let _ = cx.global_mut::<SortSettingsStore>().set(s);
        cx.notify();
    }

    fn set_criterion(&mut self, criterion: FileBrowserSortCriterion, cx: &mut Context<Self>) {
        self.update_sort(cx, |s| s.criterion = criterion);
    }

    fn toggle_direction(&mut self, cx: &mut Context<Self>) {
        self.update_sort(cx, |s| s.ascending = !s.ascending);
    }

    // MARK: - Rendering

    fn build_header(&self, snap: &Snapshot, s: &Slots, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("file-browser.header")
            .w_full()
            .px(px(14.0))
            .pt(px(6.0))
            .pb(px(2.0))
            .text_size(px(NAME_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(slot_to_rgba(s.ink))
            .cursor_pointer()
            .child(SharedString::from(snap.header.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                    this.reset_root_to_cwd(cx);
                    cx.stop_propagation();
                }),
            )
    }

    fn build_control_strip(
        &self,
        snap: &Snapshot,
        s: &Slots,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let at_root = snap.root == "/" || snap.root.is_empty();
        let ascending = snap.settings.ascending;
        let is_name = snap.settings.criterion == FileBrowserSortCriterion::Name;
        let show_hidden = snap.show_hidden;
        let scale = self.window_scale;

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .pt(px(2.0))
            .pb(px(6.0))
            // up-nav (parent dir)
            .child(self.strip_button(
                "file-browser.up",
                "chevron.up",
                "\u{2191}",
                at_root,
                s,
                scale,
                |this: &mut Self, _e: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Self>| {
                    this.go_to_parent(cx);
                    cx.stop_propagation();
                },
                cx,
            ))
            .child(div().flex_1())
            // sort-criterion button (icon reflects criterion) opening a small menu
            .child(self.strip_button(
                "file-browser.sort",
                if is_name { "textformat" } else { "clock" },
                if is_name { "Aa" } else { "\u{25F7}" },
                false,
                s,
                scale,
                move |this: &mut Self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>| {
                    this.open_sort_menu(is_name, e.position, window, cx);
                    cx.stop_propagation();
                },
                cx,
            ))
            // direction toggle
            .child(self.strip_button(
                "file-browser.direction",
                if ascending { "arrow.up" } else { "arrow.down" },
                if ascending { "\u{2191}" } else { "\u{2193}" },
                false,
                s,
                scale,
                |this: &mut Self, _e: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Self>| {
                    this.toggle_direction(cx);
                    cx.stop_propagation();
                },
                cx,
            ))
            // hidden toggle
            .child(self.strip_button(
                "file-browser.hidden",
                if show_hidden { "eye" } else { "eye.slash" },
                if show_hidden { "\u{25C9}" } else { "\u{25CB}" },
                false,
                s,
                scale,
                |this: &mut Self, _e: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Self>| {
                    this.toggle_hidden(cx);
                    cx.stop_propagation();
                },
                cx,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    fn strip_button(
        &self,
        id: &'static str,
        symbol: &'static str,
        glyph: &'static str,
        disabled: bool,
        s: &Slots,
        scale: f32,
        on_down: impl Fn(&mut Self, &MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = if disabled {
            slot_to_rgba(s.ink3)
        } else {
            slot_to_rgba(s.ink2)
        };
        let hover = srgba_to_rgba(srgba_with_alpha(slot_srgba(s.ink), HOVER_INK_ALPHA));
        let mut btn = div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(STRIP_BUTTON))
            .h(px(STRIP_BUTTON))
            .rounded(px(4.0))
            .child(sf_symbol_icon(symbol, glyph, 11.0, SymbolWeight::Regular, color, scale, cx));
        if !disabled {
            btn = btn
                .cursor_pointer()
                .hover(move |st| st.bg(hover))
                .on_mouse_down(MouseButton::Left, cx.listener(on_down));
        }
        btn.into_any_element()
    }

    fn open_sort_menu(
        &mut self,
        is_name: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        let e1 = weak.clone();
        let name_label = if is_name { "\u{2713} Name" } else { "  Name" };
        let date_label = if is_name { "  Date Modified" } else { "\u{2713} Date Modified" };
        let items = vec![
            ContextMenuItem::entry(name_label, move |_w, app| {
                let _ = e1.update(app, |this, cx| {
                    this.set_criterion(FileBrowserSortCriterion::Name, cx)
                });
            }),
            ContextMenuItem::entry(date_label, move |_w, app| {
                let _ = weak.update(app, |this, cx| {
                    this.set_criterion(FileBrowserSortCriterion::DateModified, cx)
                });
            }),
        ];
        self.present_menu(items, position, window, cx);
    }

    fn build_tree(&self, snap: &Snapshot, s: &Slots, cx: &mut Context<Self>) -> AnyElement {
        if !snap.root_exists {
            return self.build_missing_state(snap, s);
        }
        let rows = snap.rows.clone();
        let scale = self.window_scale;
        let colors = RowColors {
            sel_bg: srgba_to_rgba(srgba_with_alpha(self.accent, SEL_ALPHA)),
            hover: srgba_to_rgba(srgba_with_alpha(slot_srgba(s.ink), HOVER_INK_ALPHA)),
            drag_hover: srgba_to_rgba(srgba_with_alpha(self.accent, DRAG_HOVER_ALPHA)),
            ink: slot_to_rgba(s.ink),
            ink2: slot_to_rgba(s.ink2),
            ink3: slot_to_rgba(s.ink3),
            field_bg: slot_to_rgba(s.background3),
            field_border: slot_to_rgba(s.line_strong),
            caret: srgba_to_rgba(self.accent),
        };
        let count = rows.len();
        let weak = cx.weak_entity();
        let rename_focus = self.rename_focus.clone();
        let probe = self.rename_probe.clone();
        uniform_list("file-browser.tree", count, move |range, _window, app| {
            range
                .map(|i| {
                    render_row(
                        &rows[i],
                        weak.clone(),
                        colors,
                        scale,
                        &rename_focus,
                        probe.clone(),
                        app,
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .flex_1()
        .w_full()
        .into_any_element()
    }

    fn build_missing_state(&self, snap: &Snapshot, s: &Slots) -> AnyElement {
        div()
            .flex_1()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .py(px(24.0))
            .child(
                div()
                    .text_size(px(22.0))
                    .text_color(slot_to_rgba(s.ink2))
                    .child(SharedString::from("\u{25A2}")),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(slot_to_rgba(s.ink))
                    .child(SharedString::from("Folder not found")),
            )
            .child(
                div()
                    .px(px(12.0))
                    .text_size(px(10.0))
                    .text_color(slot_to_rgba(s.ink2))
                    .child(SharedString::from(snap.root.clone())),
            )
            .into_any_element()
    }

    fn build_empty_panel(&self, s: &Slots) -> AnyElement {
        div()
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.0))
            .text_color(slot_to_rgba(s.ink3))
            .child(SharedString::from("No active tab"))
            .into_any_element()
    }

    // MARK: - Scenario / driver seams (the `file-browser` self-test reads these)

    /// The current root path of the active tab's browser, if any.
    pub(crate) fn scenario_root(&self, cx: &App) -> Option<String> {
        let (tab_id, _) = self.active_tab_cwd(cx)?;
        self.state
            .read(cx)
            .file_browser
            .state_for(&tab_id)
            .map(|s| s.root_path().to_string())
    }

    /// The paths rendered in the tree at the last render (the live "what's on
    /// screen" read — refreshed only by a real re-render, so a watcher-driven row
    /// appearance surfaces through it).
    pub(crate) fn scenario_rendered_paths(&self) -> Vec<String> {
        self.rendered_paths.clone()
    }

    /// Whether `path` is currently expanded for the active tab.
    pub(crate) fn scenario_is_expanded(&self, path: &str, cx: &App) -> bool {
        let Some((tab_id, _)) = self.active_tab_cwd(cx) else {
            return false;
        };
        self.state
            .read(cx)
            .file_browser
            .state_for(&tab_id)
            .map(|s| s.expanded_paths().contains(path))
            .unwrap_or(false)
    }

    /// Drive a plain single click on `path` (the real router path).
    pub(crate) fn drive_single_click(&mut self, path: &str, cx: &mut Context<Self>) {
        self.on_row_click(path, ClickModifier::Plain, cx);
    }

    /// Drive a double click on `path` (two plain clicks within the window).
    pub(crate) fn drive_double_click(&mut self, path: &str, cx: &mut Context<Self>) {
        self.on_row_click(path, ClickModifier::Plain, cx);
        self.on_row_click(path, ClickModifier::Plain, cx);
    }

    /// Open the row context menu for `path` (the real right-click path).
    pub(crate) fn drive_right_click(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let is_dir = is_dir_resolved(path);
        let is_root = self.scenario_root(cx).as_deref() == Some(path);
        self.open_row_menu(path, is_dir, is_root, Point::default(), window, cx);
    }

    /// The labels of the currently-open context menu, in order (menu-visibility
    /// assertion read).
    pub(crate) fn scenario_menu_labels(&self, cx: &App) -> Vec<String> {
        self.context_menu
            .as_ref()
            .map(|m| m.read(cx).item_labels())
            .unwrap_or_default()
    }

    /// Drive the "Open With ▸" first-stage entry (arms the second stage), then
    /// open the second-stage menu — the scenario reads its labels after.
    pub(crate) fn drive_open_with(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_open_with_menu(path, Point::default(), window, cx);
    }

    /// Drive the control-strip hidden-files toggle.
    pub(crate) fn drive_toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.toggle_hidden(cx);
    }

    /// Drive the control-strip sort-direction toggle.
    pub(crate) fn drive_toggle_direction(&mut self, cx: &mut Context<Self>) {
        self.toggle_direction(cx);
    }

    // MARK: - R20 rename / cut / paste / trash driver seams (scenario reads)

    /// Begin an inline rename for `path` (the menu / Return / slow-click terminus).
    pub(crate) fn drive_begin_rename(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_rename(path, window, cx);
    }

    /// Whether a row is currently being renamed.
    pub(crate) fn scenario_is_renaming(&self) -> bool {
        self.rename.is_some()
    }

    /// The current rename field text, if editing (the field-model read the
    /// scenario asserts a typed edit against).
    pub(crate) fn scenario_rename_text(&self) -> Option<String> {
        self.rename.as_ref().map(|r| r.editor.text())
    }

    /// The current rename field selection `(start, end)` — the scenario asserts the
    /// basename preselection through it.
    pub(crate) fn scenario_rename_selection(&self) -> Option<(usize, usize)> {
        self.rename.as_ref().map(|r| r.editor.selection())
    }

    /// Type one printable character into the active rename field.
    pub(crate) fn drive_rename_type(&mut self, ch: char, cx: &mut Context<Self>) {
        self.apply_editor_key(TextFieldKey::Char(ch), cx);
    }

    /// The x-offset (from the text's left edge) of char boundary `index` in the
    /// active field — the scenario picks a click target with it.
    pub(crate) fn scenario_rename_x_for_index(&self, index: usize, window: &Window) -> Option<f32> {
        let text = self.rename.as_ref()?.editor.text();
        Some(crate::inline_rename::char_boundary_x(
            window, &text, NAME_SIZE, index,
        ))
    }

    /// The field's painted geometry `(field_left, text_left)` (window
    /// coordinates), as recorded by the two live layout probes. The scenario
    /// cross-checks them against each other (`text_left - field_left` must be the
    /// 6px field padding) so a probe regression to the field-box bias — the click
    /// off-by-one — cannot cancel out of the click assertions.
    pub(crate) fn scenario_rename_probe(&self) -> (f32, f32) {
        let p = self.rename_probe.get();
        (p.field_left, p.text_left)
    }

    /// Drive a click INSIDE the open rename field at WINDOW x `window_x`,
    /// through the exact production math: hit-test against the live probe's
    /// `text_left` (as the field's mouse handler does) and reposition the caret.
    /// Returns the char index the caret landed at. Proves a click repositions the
    /// cursor WITHOUT restarting the edit (the rename session is untouched — no
    /// `begin_rename`).
    pub(crate) fn drive_rename_click_at_window_x(
        &mut self,
        window_x: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let text = self.rename.as_ref()?.editor.text();
        let index = crate::inline_rename::char_index_for_click(
            window,
            &text,
            NAME_SIZE,
            self.rename_probe.get().text_left,
            window_x,
        );
        self.place_rename_cursor(index, 1, window, cx);
        Some(index)
    }

    /// Select the whole rename field (⌘A) — the scenario helper for retyping a
    /// full new name (the basename preselection alone keeps the old extension, so
    /// an extension-change rename must replace the whole field).
    pub(crate) fn drive_rename_select_all(&mut self, cx: &mut Context<Self>) {
        self.apply_editor_key(TextFieldKey::SelectAll, cx);
    }

    /// The title of the confirmation modal currently presented over this view's
    /// window, if any — the scenario asserts a rename confirmation was presented
    /// (and matches its wording) through it. Read-only; the ANSWER is driven from
    /// the raw app context (never inside a `FileBrowserView` update — the modal's
    /// completion re-enters this view to recurse/refocus, so resolving it inside
    /// an update would double-borrow this entity).
    pub(crate) fn scenario_pending_modal_title(&self, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .pending_modal()
            .map(|m| m.read(cx).scenario_title())
    }

    /// Commit the active rename (the Return / click-away terminus).
    pub(crate) fn drive_rename_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rename(window, cx);
    }

    /// Cancel the active rename (the Esc terminus).
    pub(crate) fn drive_rename_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_rename(window, cx);
    }

    /// The focus-call counter (the Swift test seam): total rename exits so far.
    pub(crate) fn scenario_refocus_count(&self) -> usize {
        self.refocus_count
    }

    /// Drive the context-menu "Move to Trash" op for `path` (the real handler).
    pub(crate) fn drive_trash(&mut self, path: &str, cx: &mut Context<Self>) {
        self.menu_trash(path, cx);
    }

    /// Drive the context-menu "Copy" op for `path`.
    pub(crate) fn drive_copy(&mut self, path: &str, cx: &mut Context<Self>) {
        self.menu_copy(path, cx);
    }

    /// Drive the context-menu "Cut" op for `path`.
    pub(crate) fn drive_cut(&mut self, path: &str, cx: &mut Context<Self>) {
        self.menu_cut(path, cx);
    }

    /// Drive the context-menu "Paste" op onto `path`.
    pub(crate) fn drive_paste(&mut self, path: &str, cx: &mut Context<Self>) {
        let is_dir = is_dir_resolved(path);
        self.menu_paste(path, is_dir, cx);
    }

    /// The paths currently rendered ghosted (cut intent) — the scenario's cut-ghost
    /// read.
    pub(crate) fn scenario_cut_paths(&self, cx: &App) -> Vec<String> {
        cx.try_global::<FilePasteboardGlobal>()
            .map(|g| {
                g.0.cut_paths()
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drive an in-tree drag of the current selection (or just `path`) onto the
    /// directory `dest` — the DnD commit seam. Goes through the real `handle_drop`
    /// path with the same [`gpui::ExternalPaths`] payload the row's `on_drag`
    /// constructs, so the drop moves exactly the dragged set.
    pub(crate) fn drive_drag_drop(&mut self, path: &str, dest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let sources = self.begin_row_drag(path, cx);
        let payload = ExternalPaths(sources.iter().map(PathBuf::from).collect());
        self.handle_drop(&payload, dest, window, cx);
    }

    /// Whether a drag of the current selection (or just `path`) onto `dest` would
    /// be accepted — the pure `can_drop` rule that gates the accent hover highlight
    /// AND the drop (the scenario asserts the highlight predicate through it).
    pub(crate) fn scenario_can_drop(&self, path: &str, dest: &str, cx: &App) -> bool {
        let selection = self.ordered_selection(cx);
        let sources = if selection.iter().any(|p| p == path) {
            selection
        } else {
            vec![path.to_string()]
        };
        let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
        nice_model::file_browser::can_drop(&refs, dest)
    }

    /// Select `path` as the sole selection (scenario drag setup).
    pub(crate) fn drive_select(&mut self, path: &str, cx: &mut Context<Self>) {
        self.with_active_fb_state(cx, |st| st.selection_mut().replace(&[path.to_string()], None));
    }

    /// Add `path` to the selection (⌘-click parity — scenario multi-select setup).
    pub(crate) fn drive_add_to_selection(&mut self, path: &str, cx: &mut Context<Self>) {
        self.with_active_fb_state(cx, |st| st.selection_mut().toggle(path));
    }

    // MARK: - Render body (called by SidebarShellView::build_body)
}

impl Focusable for FileBrowserView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for FileBrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_scale = window.scale_factor();
        let s = chrome_slots(cx);

        // Deferred Open With ▸ second stage: open it now (render has the Window
        // `ContextMenu::new` needs; the first menu already dismissed).
        if self.context_menu.is_none() {
            if let Some((path, pos)) = self.pending_open_with.take() {
                self.open_open_with_menu(&path, pos, window, cx);
            }
        }

        // R20 (F8): consume a queued rename request (context-menu "Rename", the
        // Return trigger, or the slow-second-click deferral) now that render has
        // the `Window` `begin_rename` needs to grab field focus. Only `/` is
        // refused (defense in depth — the triggers already gate).
        if self.rename.is_none() {
            if let Some(path) = self.pending_rename_path.take() {
                if nice_model::file_browser::can_rename(&path) {
                    self.begin_rename(&path, window, cx);
                }
            }
        }

        let Some(snap) = self.snapshot(cx) else {
            return div()
                .id(SharedString::from(FILE_BROWSER_ROOT_LABEL))
                .role(gpui::Role::Group)
                .aria_label(FILE_BROWSER_ROOT_LABEL)
                .size_full()
                .child(self.build_empty_panel(&s));
        };

        // Scroll resets to the top on a root change (re-root / up-nav / tab
        // switch); expansion survives, scroll does not (Swift parity).
        if self.last_root.as_deref() != Some(snap.root.as_str()) {
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.last_root = Some(snap.root.clone());
        }
        self.sync_watcher(&snap);

        let header = self.build_header(&snap, &s, cx);
        let strip = self.build_control_strip(&snap, &s, cx);
        let tree = self.build_tree(&snap, &s, cx);

        div()
            .id(SharedString::from(FILE_BROWSER_ROOT_LABEL))
            .role(gpui::Role::Group)
            .aria_label(FILE_BROWSER_ROOT_LABEL)
            .track_focus(&self.focus_handle)
            // R20 (F8): the browser panel's own key context. A row click parks
            // focus here; Return then begins rename iff exactly one row is
            // selected. With a terminal (or any field) focused the context never
            // matches, so terminals keep Return — the structural first-responder
            // guard replacing Swift's NSEvent monitor.
            .key_context("FileBrowser")
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.rename.is_some() {
                    return; // the field owns keys while editing
                }
                if matches!(e.keystroke.key.as_str(), "enter" | "return") {
                    if let Some(path) = this.single_selected_path(cx) {
                        if nice_model::file_browser::can_rename(&path) {
                            this.begin_rename(&path, window, cx);
                            cx.stop_propagation();
                        }
                    }
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            // Clicks outside any row (empty area, and the click-away replacement
            // for Swift's window monitor) clear the selection and commit any
            // active rename (a click-away commit).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseDownEvent, window, cx| {
                    this.commit_rename(window, cx);
                    this.clear_selection(cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _e, window, cx| {
                this.commit_rename(window, cx);
                this.clear_selection(cx);
            }))
            .child(header)
            .child(strip)
            .child(tree)
            .children(self.context_menu.clone())
    }
}

// MARK: - Free helpers -------------------------------------------------------

/// The active chrome slot table for the file browser — the live
/// [`SharedThemeState`](crate::theme_settings::SharedThemeState) (Nice/Dark
/// fallback when the theme global is absent). R21: was a fixed Nice/Dark table.
fn chrome_slots(cx: &gpui::App) -> Slots {
    crate::theme_settings::active_chrome_slots(cx)
}

/// Row colours resolved once per render and copied into each row.
#[derive(Clone, Copy)]
struct RowColors {
    sel_bg: Rgba,
    hover: Rgba,
    drag_hover: Rgba,
    ink: Rgba,
    ink2: Rgba,
    ink3: Rgba,
    /// Rename-field chrome — the same slots the sidebar tab / pane pill
    /// `rename_field` uses (background3 fill + line_strong border), so the
    /// editor reads as a field against the accent-tinted selected row instead
    /// of vanishing into it.
    field_bg: Rgba,
    field_border: Rgba,
    /// Full-alpha accent for the collapsed caret bar (sel_bg's 22% tint is
    /// invisible at 1px).
    caret: Rgba,
}

/// Render one tree row (free fn so the `uniform_list` `'static` closure builds it
/// without borrowing the view; clicks re-enter the view through `weak`).
#[allow(clippy::too_many_arguments)]
fn render_row(
    row: &RowVm,
    weak: gpui::WeakEntity<FileBrowserView>,
    c: RowColors,
    scale: f32,
    rename_focus: &FocusHandle,
    probe: Rc<Cell<FieldProbe>>,
    app: &mut App,
) -> AnyElement {
    let indent = row.depth as f32 * INDENT_PER_LEVEL;
    let icon_color = if row.is_dir { c.ink2 } else { c.ink3 };
    let path_for_click = row.path.clone();
    let path_for_menu = row.path.clone();
    let is_dir = row.is_dir;
    let is_root = row.is_root;

    let mut el = div()
        .id(SharedString::from(row.path.clone()))
        .flex()
        .flex_row()
        .items_center()
        .h(px(ROW_HEIGHT))
        .w_full()
        .pl(px(6.0 + indent))
        .pr(px(6.0))
        .gap(px(4.0))
        .cursor_pointer();
    if row.is_selected {
        el = el.bg(c.sel_bg);
    } else {
        let hover = c.hover;
        el = el.hover(move |st| st.bg(hover));
    }
    // R20 (F7): a cut row is ghosted at 0.45 opacity until the cut is pasted or
    // invalidated (any pasteboard mutation un-ghosts it via the snapshot's cut set).
    if row.is_cut {
        el = el.opacity(0.45);
    }
    // Disclosure slot (chevron for dirs; blank 12px spacer for files so names
    // stay aligned). Decorative — a plain click anywhere on a folder row
    // already toggles expansion (the router's primary action), so the chevron
    // needs no separate handler. Prod parity (FileBrowserView.swift:581-592):
    // SF Symbol chevron.right at 10pt semibold, 0.7 opacity, rotated 90° when
    // expanded — rendered here as a chevron.right/chevron.down glyph swap,
    // matching this file's existing chevron idiom.
    let mut disclosure = div().w(px(DISCLOSURE_SLOT)).flex().justify_center();
    if is_dir {
        let (symbol, fallback) = if row.is_expanded {
            ("chevron.down", CHEVRON_OPEN)
        } else {
            ("chevron.right", CHEVRON_CLOSED)
        };
        disclosure = disclosure.opacity(0.7).child(sf_symbol_icon(
            symbol,
            fallback,
            10.0,
            SymbolWeight::Semibold,
            c.ink2,
            scale,
            app,
        ));
    }
    el = el
        .child(disclosure)
        .child(
            div()
                .w(px(ICON_FRAME))
                .flex()
                .justify_center()
                .child(sf_symbol_icon(
                    row.icon_symbol,
                    row.icon_glyph,
                    ICON_SIZE,
                    SymbolWeight::Regular,
                    icon_color,
                    scale,
                    app,
                )),
        )
        .child(match &row.editing {
            Some(spans) => {
                render_rename_field(spans, rename_focus, weak.clone(), c, probe.clone())
            }
            // min_w_0 lets the flex item shrink below the name's intrinsic
            // width; middle truncation matches prod
            // (FileBrowserView.swift:914-918, `.truncationMode(.middle)`).
            None => div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_middle()
                .text_size(px(NAME_SIZE))
                .text_color(c.ink)
                .child(SharedString::from(row.name.clone()))
                .into_any_element(),
        });

    // R20 (F9): drag source — the payload IS `gpui::ExternalPaths` (the app's
    // first `on_drag` consumer), carrying the select-then-drag set (`drag_paths`).
    // One payload type means a directory row's drop handler serves both internal
    // drags and Finder-inbound drops, AND dragging a row onto a terminal feeds T7's
    // target for free. Suppressed while editing.
    if row.editing.is_none() {
        let drag_paths = row.drag_paths.clone();
        el = el.on_drag(
            ExternalPaths(drag_paths.iter().map(PathBuf::from).collect()),
            move |paths: &ExternalPaths, _offset, _window, app| {
                let count = paths.paths().len();
                app.new(|_| DragPreview { count })
            },
        );
    }

    // R20 (F9): directory rows are drop targets for the same `ExternalPaths`
    // payload — the pure `can_drop` rule gates them and the accent hover highlight
    // shows a valid target.
    if is_dir {
        let dest_can = row.path.clone();
        let dest_drop = row.path.clone();
        let drag_hover = c.drag_hover;
        let weak_drop = weak.clone();
        el = el
            .drag_over::<ExternalPaths>(move |style, _paths, _window, _app| style.bg(drag_hover))
            .can_drop(move |dragged, _window, _app| {
                dragged
                    .downcast_ref::<ExternalPaths>()
                    .map(|ep| {
                        let srcs: Vec<String> = ep
                            .paths()
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect();
                        let refs: Vec<&str> = srcs.iter().map(String::as_str).collect();
                        nice_model::file_browser::can_drop(&refs, &dest_can)
                    })
                    .unwrap_or(false)
            })
            .on_drop::<ExternalPaths>(move |paths: &ExternalPaths, window, app| {
                let _ = weak_drop.update(app, |this, cx| {
                    this.handle_drop(paths, &dest_drop, window, cx);
                });
            });
    }

    let weak_left = weak.clone();
    el.on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, app| {
        let modifier = if e.modifiers.platform {
            ClickModifier::Command
        } else if e.modifiers.shift {
            ClickModifier::Shift
        } else {
            ClickModifier::Plain
        };
        let p = path_for_click.clone();
        let _ = weak_left.update(app, |this, cx| {
            this.on_row_click(&p, modifier, cx);
            // Parking focus in the browser panel makes Return-to-rename work and
            // fires commit-on-blur when the user later clicks away / switches tabs.
            this.focus_handle.focus(window, cx);
            cx.stop_propagation();
        });
    })
    .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, window, app| {
        let p = path_for_menu.clone();
        let _ = weak.update(app, |this, cx| {
            this.open_row_menu(&p, is_dir, is_root, e.position, window, cx);
            cx.stop_propagation();
        });
    })
    .into_any_element()
}

/// Render the inline-rename field for the editing row (F8) via the shared
/// [`crate::inline_rename::rename_field`]: the pure editing model's text with a
/// caret (collapsed cursor) or a highlighted selection range, the shared field
/// chrome (background3 fill + line_strong border), key routing back through
/// [`FileBrowserView::on_rename_key`], and click-to-position through
/// [`FileBrowserView::place_rename_cursor`].
fn render_rename_field(
    spans: &EditSpans,
    rename_focus: &FocusHandle,
    weak: gpui::WeakEntity<FileBrowserView>,
    c: RowColors,
    probe: Rc<Cell<FieldProbe>>,
) -> AnyElement {
    let colors = FieldColors {
        bg: c.field_bg,
        border: c.field_border,
        text: c.ink,
        caret: c.caret,
        selection: c.sel_bg,
    };
    let weak_key = weak.clone();
    crate::inline_rename::rename_field(
        rename_focus,
        spans,
        "FileBrowserRename",
        colors,
        NAME_SIZE,
        probe,
        move |e: &KeyDownEvent, window, app| {
            let _ = weak_key.update(app, |this, cx| this.on_rename_key(e, window, cx));
        },
        move |index, click_count, window, app| {
            let _ = weak.update(app, |this, cx| {
                this.place_rename_cursor(index, click_count, window, cx)
            });
        },
    )
    .into_any_element()
}

/// "Is this row a directory" — the pure listing's [`path_is_dir`] predicate
/// (follows symlinks: a symlink-to-dir IS a directory row; a broken symlink
/// isn't). One predicate shared with `entries`/`visible_order` so icons,
/// expansion, menus, and sorting all agree.
fn is_dir_resolved(path: &str) -> bool {
    path_is_dir(path)
}

/// Whether every source shares `dest`'s volume (device id). An unreadable source
/// or dest is treated as cross-volume so the drop defensively COPIES (a raw
/// cross-volume rename would fail) — the Swift `areOnSameVolume` fallback.
fn sources_share_volume(sources: &[String], dest: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(dest_dev) = std::fs::metadata(dest).map(|m| m.dev()) else {
        return false;
    };
    sources.iter().all(|s| {
        std::fs::metadata(s)
            .map(|m| m.dev() == dest_dev)
            .unwrap_or(false)
    })
}

/// The small "N items" drag preview (F9) — gpui has no drag-cursor operation cue
/// at the pin, so this floating chip is the only drag affordance.
struct DragPreview {
    count: usize,
}

impl gpui::Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = chrome_slots(cx);
        let label = if self.count == 1 {
            "1 item".to_string()
        } else {
            format!("{} items", self.count)
        };
        div()
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .bg(slot_to_rgba(s.panel))
            .border_1()
            .border_color(slot_to_rgba(s.line))
            .text_size(px(NAME_SIZE))
            .text_color(slot_to_rgba(s.ink))
            .child(SharedString::from(label))
    }
}

fn last_component(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn parent_path(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

fn depth_of(root: &str, path: &str) -> usize {
    let rc = root.trim_end_matches('/').split('/').count();
    let pc = path.trim_end_matches('/').split('/').count();
    pc.saturating_sub(rc)
}

fn icon_glyph(is_dir: bool) -> &'static str {
    if is_dir {
        "\u{1F4C1}"
    } else {
        "\u{1F4C4}"
    }
}

fn icon_symbol(path: &str, is_dir: bool, is_expanded: bool) -> &'static str {
    if is_dir {
        return if is_expanded { "folder.fill" } else { "folder" };
    }
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "swift" | "m" | "mm" | "h" | "c" | "cpp" | "rs" | "go" | "py" | "rb" | "ts" | "tsx"
        | "js" | "jsx" => "chevron.left.forwardslash.chevron.right",
        "md" | "markdown" | "txt" | "rst" => "doc.text",
        "json" | "yml" | "yaml" | "toml" | "plist" | "xml" => "doc.text.below.ecg",
        "png" | "jpg" | "jpeg" | "gif" | "heic" | "tiff" | "bmp" | "webp" => "photo",
        "mp4" | "mov" | "m4v" | "avi" | "mkv" => "film",
        "mp3" | "wav" | "aac" | "m4a" | "flac" => "music.note",
        "pdf" => "doc.richtext",
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" => "doc.zipper",
        "sh" | "zsh" | "bash" | "fish" => "terminal",
        _ => "doc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_browser::history::FileOperationHistory;
    use crate::file_browser::ops::{FakeTrasher, FileOperationsService};

    /// A throwaway temp tree for the DnD commit-seam tests: `A.txt` + `B.txt` at
    /// the root and an empty directory `D`. Dropped ⇒ the tree is removed.
    struct DropFixture {
        root: PathBuf,
    }

    impl DropFixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nice-fb-drop-{}-{}-{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(root.join("D")).unwrap();
            std::fs::write(root.join("A.txt"), b"A\n").unwrap();
            std::fs::write(root.join("B.txt"), b"B\n").unwrap();
            Self { root }
        }

        fn p(&self, rel: &str) -> String {
            self.root.join(rel).to_string_lossy().into_owned()
        }
    }

    impl Drop for DropFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn exists(p: &str) -> bool {
        Path::new(p).exists()
    }

    /// Install the file-op history Global over a temp-dir `FakeTrasher` (hermetic —
    /// never the production Trash). No focus-follow seam (single-view test).
    fn install_history(cx: &mut gpui::TestAppContext, trash_root: PathBuf) {
        cx.update(|app| {
            let service = FileOperationsService::new(Box::new(FakeTrasher::new(trash_root)));
            let history = app.new(|_| FileOperationHistory::new(service, None));
            app.set_global(FileOperationHistoryGlobal(history));
        });
    }

    /// Bug 2 regression pin (BUGS.md #3, repro: drag A out of the browser, abandon
    /// it, then drop B from Finder onto folder D). An in-tree drag that never lands
    /// on a directory row must NOT redirect the next drop. Pre-fix, `begin_row_drag`
    /// stashed the dragged set in `drag.session` and `handle_drop` preferred that
    /// leftover over the actual payload, so dropping B moved the previously-dragged
    /// A. With the session mechanism removed, `handle_drop` moves exactly its
    /// `ExternalPaths` payload — here B moves into D and A stays put.
    #[gpui::test]
    fn drop_after_abandoned_drag_moves_only_the_dropped_payload(cx: &mut gpui::TestAppContext) {
        let fx = DropFixture::new("abandoned-drag");
        let trash_root = fx.root.join(".fake-trash");
        std::fs::create_dir_all(&trash_root).unwrap();
        install_history(cx, trash_root);

        let root_str = fx.root.to_string_lossy().into_owned();
        let state = cx.update(|app| app.new(|_| WindowState::new(root_str)));
        let accent = Srgba::rgb(0.2, 0.4, 0.9);
        let window = cx.add_window(|_window, cx| FileBrowserView::new(state, accent, cx));

        let a = fx.p("A.txt");
        let b = fx.p("B.txt");
        let d = fx.p("D");

        window
            .update(cx, |view, window, cx| {
                // Simulate an abandoned in-tree drag of A: it computes the drag set
                // but records nothing (pre-fix this poisoned `drag.session`).
                let _ = view.begin_row_drag(&a, cx);
                // A separate Finder-inbound drop of B onto directory D.
                let payload = ExternalPaths([PathBuf::from(&b)].into_iter().collect());
                view.handle_drop(&payload, &d, window, cx);
            })
            .unwrap();

        // Exactly B moved into D; A is untouched at the root and never entered D.
        assert!(exists(&fx.p("D/B.txt")), "the dropped B must move into D");
        assert!(!exists(&b), "B must no longer be at the root");
        assert!(exists(&a), "the abandoned-drag A must stay put");
        assert!(
            !exists(&fx.p("D/A.txt")),
            "the stale A must NOT be redirected into D"
        );
    }
}
