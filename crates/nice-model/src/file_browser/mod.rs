//! `file_browser` — the pure, gpui-free model family behind the sidebar's
//! files mode (R19). Ports the pure-Swift `FileBrowser*` state, listing, sort,
//! selection, context-menu, and Open-With logic case-for-case, plus the
//! hand-rolled click detector the interaction contract requires. Views, the
//! kqueue watcher, settings-file I/O, and objc2 platform calls live in
//! `crates/nice`; everything here runs under plain `cargo test -p nice-model`.
//!
//! ## R20 boundary
//!
//! R20 (file operations, rename, pasteboard, trash, drag & drop) consumes:
//! * [`listing::visible_order`] for op-target resolution;
//! * [`selection::FileBrowserSelection`] snap hooks + the click router's
//!   `activated_at` stamp, fed into [`crate::rename_gate::InlineRenameClickGate`];
//! * [`menu::FileBrowserContextMenuModel`]'s frozen order + full visibility
//!   matrix — R20 flips `can_paste` / `can_rename` and adds the
//!   Rename/Copy/Cut/Paste/Move-to-Trash **handlers**, never reordering the
//!   model.
//!
//! Modules that ship values R19's `crates/nice` layer wraps: [`sort`] (the F2
//! settings value type reused as the `ui_settings.json` schema surface),
//! [`open_with`] (the pure ordering function the `WorkspaceOps` production
//! lookups feed).

pub mod click_router;
pub mod header;
pub mod listing;
pub mod menu;
pub mod open_with;
pub mod selection;
pub mod sort;
pub mod state;
pub mod store;

pub use click_router::{ClickAction, ClickModifier, FileBrowserClickRouter, DOUBLE_CLICK_WINDOW};
pub use header::file_browser_header_title;
pub use menu::{FileBrowserContextMenuItem, FileBrowserContextMenuModel};
pub use open_with::{entries as open_with_entries, OpenWithEntry, OpenWithLookups};
pub use selection::FileBrowserSelection;
pub use sort::{FileBrowserSortCriterion, FileBrowserSortSettings};
pub use state::FileBrowserState;
pub use store::FileBrowserStore;
