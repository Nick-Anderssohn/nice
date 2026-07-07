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
//!
//! ## R20 pure ports
//!
//! R20's own gpui-free logic lives here too, each table-tested and consumed by
//! the `crates/nice` file-ops layer:
//! * [`naming::split_name_and_extension`] — the Finder last-dot filename split,
//!   ported ONCE and shared by collision auto-rename, `is_extension_change`,
//!   and rename-field preselection.
//! * [`drop_resolver`] — the drag-to-folder [`drop_resolver::can_drop`] /
//!   [`drop_resolver::operation`] rules + [`drop_resolver::FileDragOperation`].
//! * [`rename_validator`] — [`rename_validator::can_rename`],
//!   [`rename_validator::validate`] (over an injected `exists` predicate), and
//!   [`rename_validator::is_extension_change`].
//! * [`cwd_impact`] — the pure CWD-invalidation [`cwd_impact::affected_by`] rule
//!   + the snapshot value types (the registry-walking builder stays in
//!   `crates/nice`).
//! * [`text_field`] — the NEW inline-rename editing model
//!   ([`text_field::TextFieldEditor`], `{text, cursor, selection}`) +
//!   [`text_field::preselect_len`]; no Swift twin (the Swift field is
//!   `NSTextField`-backed).

pub mod click_router;
pub mod cwd_impact;
pub mod drop_resolver;
pub mod header;
pub mod listing;
pub mod menu;
pub mod naming;
pub mod open_with;
pub mod rename_validator;
pub mod selection;
pub mod sort;
pub mod state;
pub mod store;
pub mod text_field;

pub use click_router::{ClickAction, ClickModifier, FileBrowserClickRouter, DOUBLE_CLICK_WINDOW};
pub use cwd_impact::{affected_by, PaneCWDRef, PaneCWDSnapshot};
pub use drop_resolver::{can_drop, operation as drop_operation, FileDragOperation};
pub use header::file_browser_header_title;
pub use menu::{FileBrowserContextMenuItem, FileBrowserContextMenuModel};
pub use naming::split_name_and_extension;
pub use open_with::{entries as open_with_entries, OpenWithEntry, OpenWithLookups};
pub use rename_validator::{
    can_rename, is_extension_change, validate as validate_rename, RenameValidation,
};
pub use selection::FileBrowserSelection;
pub use sort::{FileBrowserSortCriterion, FileBrowserSortSettings};
pub use state::FileBrowserState;
pub use store::FileBrowserStore;
pub use text_field::{preselect_len, Key as TextFieldKey, TextFieldEditor};
