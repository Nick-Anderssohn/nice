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
//! `aria_label("nice-rs-file-browser-root")` — the shipped-surface AX anchor the
//! `file-browser` scenario walks for (`app_shell.rs:68` convention).

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, App, Context, Entity, FocusHandle, Focusable,
    FontWeight, MouseButton, MouseDownEvent, Pixels, Point, Rgba, ScrollStrategy, SharedString,
    Subscription, Task, UniformListScrollHandle, Window,
};

use nice_model::file_browser::listing::visible_order;
use nice_model::file_browser::menu::FileBrowserContextMenuItem;
use nice_model::file_browser::{
    file_browser_header_title, ClickModifier, FileBrowserClickRouter, FileBrowserContextMenuModel,
    FileBrowserSortCriterion, FileBrowserSortSettings,
};
use nice_theme::color::Srgba;
use nice_theme::palette::{slots, ColorScheme, Palette, Slots};

use crate::context_menu::{ContextMenu, ContextMenuItem};
use crate::file_browser::sort_settings_store::SortSettingsStore;
use crate::file_browser::watcher::DirectoryWatcherHub;
use crate::file_browser::workspace_ops::{open_with_entries, WorkspaceOpsGlobal};
use crate::sf_symbols::{sf_symbol_icon, SymbolWeight};
use crate::theme::{slot_to_rgba, slot_srgba, srgba_to_rgba, srgba_with_alpha};
use crate::window_state::WindowState;

/// The shipped-surface AX anchor label for the files-mode root (the `ax-probe` /
/// `app-shell` convention — role + label only, no title).
pub(crate) const FILE_BROWSER_ROOT_LABEL: &str = "nice-rs-file-browser-root";

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
    icon_symbol: &'static str,
    icon_glyph: &'static str,
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
        }
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

        let rows = projection
            .iter()
            .map(|p| {
                let is_dir = is_dir_lstat(p);
                let is_expanded = is_dir && expanded.contains(p);
                RowVm {
                    name: last_component(p),
                    depth: depth_of(&root, p),
                    is_dir,
                    is_expanded,
                    is_selected: selected.contains(p),
                    is_root: p == &root,
                    icon_symbol: icon_symbol(p, is_dir, is_expanded),
                    icon_glyph: icon_glyph(is_dir),
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

    /// Route a row click through the 280 ms detector and apply its effect.
    fn on_row_click(&mut self, path: &str, modifier: ClickModifier, cx: &mut Context<Self>) {
        use nice_model::file_browser::ClickAction::*;
        let projection = self.current_projection(cx);
        let action = self.router.route(path, modifier, Instant::now());
        match action {
            Toggle { path } => self.with_active_fb_state(cx, |st| st.selection_mut().toggle(&path)),
            Extend { path } => {
                self.with_active_fb_state(cx, |st| st.selection_mut().extend(&path, &projection))
            }
            SingleActivate { path } => {
                let is_dir = is_dir_lstat(&path);
                self.with_active_fb_state(cx, |st| {
                    st.selection_mut().replace(&[path.clone()], None);
                    // Primary action: a folder toggles expansion; a file only
                    // selects (files open on DOUBLE click).
                    if is_dir {
                        st.toggle_expansion(&path);
                    }
                });
            }
            DoubleActivate { path } => {
                if is_dir_lstat(&path) {
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

    /// Copy Path (R19): newline-join the target paths onto the clipboard
    /// (Finder "Copy as Pathname" parity). `// R20:` reroutes this through the
    /// pasteboard adapter so it also clears the cut companion.
    fn copy_paths(&self, paths: &[String], cx: &mut App) {
        if paths.is_empty() {
            return;
        }
        let joined = paths.join("\n");
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(joined));
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
        // R19 passes can_paste = can_rename = false; R20 flips them + adds the rest.
        let model = FileBrowserContextMenuModel::build(is_dir, is_root, false, false);
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
                // Rename / Copy / Cut / Paste / Move to Trash → R20 (model rows
                // exist; views + handlers land there). Not rendered this cycle.
                _ => {}
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
            ink: slot_to_rgba(s.ink),
            ink2: slot_to_rgba(s.ink2),
            ink3: slot_to_rgba(s.ink3),
        };
        let count = rows.len();
        let weak = cx.weak_entity();
        uniform_list("file-browser.tree", count, move |range, _window, app| {
            range
                .map(|i| render_row(&rows[i], weak.clone(), colors, scale, app))
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
        let is_dir = is_dir_lstat(path);
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
        let s = chrome_slots();

        // Deferred Open With ▸ second stage: open it now (render has the Window
        // `ContextMenu::new` needs; the first menu already dismissed).
        if self.context_menu.is_none() {
            if let Some((path, pos)) = self.pending_open_with.take() {
                self.open_open_with_menu(&path, pos, window, cx);
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
            .size_full()
            .flex()
            .flex_col()
            // Clicks outside any row (empty area, and the click-away replacement
            // for Swift's window monitor) clear the selection.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                    this.clear_selection(cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _e, _w, cx| {
                this.clear_selection(cx);
            }))
            .child(header)
            .child(strip)
            .child(tree)
            .children(self.context_menu.clone())
    }
}

// MARK: - Free helpers -------------------------------------------------------

fn chrome_slots() -> Slots {
    slots(Palette::Nice, ColorScheme::Dark).expect("Nice + Dark is a valid palette/scheme combo")
}

/// Row colours resolved once per render and copied into each row.
#[derive(Clone, Copy)]
struct RowColors {
    sel_bg: Rgba,
    hover: Rgba,
    ink: Rgba,
    ink2: Rgba,
    ink3: Rgba,
}

/// Render one tree row (free fn so the `uniform_list` `'static` closure builds it
/// without borrowing the view; clicks re-enter the view through `weak`).
fn render_row(
    row: &RowVm,
    weak: gpui::WeakEntity<FileBrowserView>,
    c: RowColors,
    scale: f32,
    app: &mut App,
) -> AnyElement {
    let indent = row.depth as f32 * INDENT_PER_LEVEL;
    let icon_color = if row.is_dir { c.ink2 } else { c.ink3 };
    let path_for_click = row.path.clone();
    let path_for_menu = row.path.clone();
    let is_dir = row.is_dir;
    let is_root = row.is_root;

    let mut el = div()
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
    // Disclosure slot (chevron for dirs; blank for files). Decorative — a plain
    // click anywhere on a folder row already toggles expansion (the router's
    // primary action), so the chevron needs no separate handler.
    let chevron = if is_dir {
        SharedString::from(if row.is_expanded { CHEVRON_OPEN } else { CHEVRON_CLOSED })
    } else {
        SharedString::from("")
    };
    el = el
        .child(
            div()
                .w(px(DISCLOSURE_SLOT))
                .text_size(px(9.0))
                .text_color(c.ink2)
                .child(chevron),
        )
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
        .child(
            div()
                .flex_1()
                .text_size(px(NAME_SIZE))
                .text_color(c.ink)
                .child(SharedString::from(row.name.clone())),
        );

    let weak_left = weak.clone();
    el.on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _window, app| {
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

/// The BSD lstat "is this a real directory" check (mirrors the pure listing's
/// private `path_is_dir_lstat`: a symlink-to-dir is NOT a directory row).
fn is_dir_lstat(path: &str) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
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
