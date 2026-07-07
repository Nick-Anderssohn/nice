//! `file_browser` — the `crates/nice` half of R19's sidebar files mode: the
//! objc2 / gpui / disk-I/O layer wrapping the pure `nice_model::file_browser`
//! model family.
//!
//! The pure state, listing, sort, selection, context-menu, and Open-With logic
//! lives gpui-free in [`nice_model::file_browser`]; this module owns the impure
//! seams that model can't. So far:
//!
//! * [`watcher`] — the kqueue [`watcher::DirectoryWatcherHub`], one per window:
//!   one kqueue fd + one dedicated OS thread, a `set_watched` desired-set diff,
//!   a 120 ms thread-side trailing quiet window, waker-woken delivery, and an
//!   `EVFILT_USER` teardown wake so the thread joins promptly (no leaked fds).
//! * [`sort_settings_store`] — the F2 `ui_settings.json` store: the
//!   [`nice_model::file_browser::FileBrowserSortSettings`] value type as its
//!   schema, unknown top-level keys preserved on rewrite, atomic
//!   temp-file+rename only-if-changed (reusing [`crate::atomic_file::write_atomic`]
//!   by name), path injected.
//! * [`workspace_ops`] — the [`workspace_ops::WorkspaceOps`] seam: the trait,
//!   its recording fake (for `run_selftest` + tests), the production impl over
//!   `platform.rs`'s objc2 calls, and the Open-With ordering wiring onto
//!   [`nice_model::file_browser::open_with`].
//!
//! * [`view`] — the [`view::FileBrowserView`] gpui view: the `uniform_list`
//!   disclosure tree over the pure model, the header / control strip / empty
//!   states, click routing through the hand-rolled detector, the R19 context menu
//!   (Open / Open With ▸ / Reveal / Copy Path) + the two-stage Open With, all OS
//!   actions behind the [`workspace_ops`] Global, and the AX root anchor. Mounted
//!   by [`crate::sidebar_shell::SidebarShellView`]'s `build_body`.

// R20 file-operations layer (F5–F9): the ops engine, the app-wide undo/redo
// history, the pasteboard adapter, rename, and in-tree DnD. Slice 2 wired the
// impure halves onto them — the objc2 `ProductionTrasher` /
// `ProductionFilePasteboard` (forwarding to `platform.rs`), the history `Entity`
// + `Global`, the `FilePasteboardGlobal`, the ⌘Z/⌘⇧Z keymap bodies, the
// per-window drift `banner`, and the menu handlers + ghost rows; slice 3 added
// rename + DnD. The final-composition slice fills the production focus-follow
// closure ([`focus_route`], the cross-window undo routing) so ⌘Z in one window
// routes focus back to the originator. Some fakes and future-consumer seams stay
// test-/future-only, hence the retained `allow(dead_code)`.
#[allow(dead_code)]
pub mod banner;
#[allow(dead_code)]
pub mod cwd_snapshot;
#[allow(dead_code)]
pub mod focus_route;
#[allow(dead_code)]
pub mod history;
#[allow(dead_code)]
pub mod ops;
#[allow(dead_code)]
pub mod pasteboard;
#[allow(dead_code)]
pub mod rename;
pub mod sort_settings_store;
pub mod view;
pub mod watcher;
pub mod workspace_ops;
