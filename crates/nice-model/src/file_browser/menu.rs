//! `FileBrowserContextMenuModel` — the pure visibility model deciding which
//! entries appear in the file-browser right-click menu. Ported from the pure
//! model in `Sources/Nice/Views/FileBrowserContextMenu.swift:75-131`, minus the
//! CUT `openInEditorPane` entry (editor routing is cut — roadmap §2 — so it is
//! absent from both the model and this port's order).
//!
//! The **full** visibility matrix lives here, including the rows R20 owns
//! (Rename / Copy / Cut / Paste / Move to Trash): the model exposes every row
//! so R19's view can render its subset (Open, Open With ▸, Reveal in Finder,
//! ─, Copy Path) and R20 flips `can_paste` / `can_rename` and adds the
//! remaining handlers **without touching this model**.
//!
//! Frozen final order (`build` output when everything is visible):
//! Open, Open With ▸, Reveal in Finder, ─, Rename, Copy, Copy Path, Cut,
//! Paste, Move to Trash. Rules:
//! * Open / Open With hidden on directories.
//! * Copy / Cut / Move-to-Trash hidden on the root row.
//! * Rename only when `can_rename` (caller passes `false` for multi-select or
//!   the filesystem root `/`).
//! * Paste only when `can_paste`.
//! * Reveal in Finder + Copy Path always.

/// One entry in the file-browser context menu. `DividerOpen` is the single
/// separator between the open/reveal group and the edit group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileBrowserContextMenuItem {
    Open,
    OpenWith,
    RevealInFinder,
    DividerOpen,
    Rename,
    Copy,
    CopyPath,
    Cut,
    Paste,
    Trash,
}

/// The ordered list of entries a right-click should show. Build it with
/// [`FileBrowserContextMenuModel::build`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBrowserContextMenuModel {
    pub items: Vec<FileBrowserContextMenuItem>,
}

impl FileBrowserContextMenuModel {
    /// Compute the visible entries in frozen order.
    ///
    /// * `is_directory` — hides Open / Open With.
    /// * `is_root` — the browser root **row**; hides Copy / Cut / Move-to-Trash
    ///   (the project CWD row is still renameable; only the filesystem root `/`
    ///   is special-cased via `can_rename == false`).
    /// * `can_paste` — shows Paste.
    /// * `can_rename` — shows Rename; the caller passes `false` for a
    ///   multi-selection (rename is single-target) or for `/`
    ///   (`FileBrowserContextMenu.swift:99-130`).
    pub fn build(
        is_directory: bool,
        is_root: bool,
        can_paste: bool,
        can_rename: bool,
    ) -> Self {
        use FileBrowserContextMenuItem::*;
        let mut items = Vec::new();
        if !is_directory {
            items.push(Open);
            items.push(OpenWith);
        }
        items.push(RevealInFinder);
        items.push(DividerOpen);
        if can_rename {
            items.push(Rename);
        }
        if !is_root {
            items.push(Copy);
        }
        items.push(CopyPath);
        if !is_root {
            items.push(Cut);
        }
        if can_paste {
            items.push(Paste);
        }
        if !is_root {
            items.push(Trash);
        }
        Self { items }
    }

    /// Whether the menu includes `item`. A test-facing convenience for the
    /// visibility assertions; the view reads [`FileBrowserContextMenuModel::items`]
    /// directly.
    #[cfg(test)]
    fn contains(&self, item: FileBrowserContextMenuItem) -> bool {
        self.items.contains(&item)
    }
}

#[cfg(test)]
mod tests {
    use super::FileBrowserContextMenuItem::*;
    use super::*;

    /// `FileBrowserContextMenuModelTests.test_menuItems_onFile_includesOpenAndOpenWith`
    /// (the `openInEditorPane` assertion is dropped — editors are CUT).
    #[test]
    fn menu_items_on_file_includes_open_and_open_with() {
        let model = FileBrowserContextMenuModel::build(false, false, false, true);
        assert!(model.contains(Open));
        assert!(model.contains(OpenWith));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_onDirectory_omitsOpenAndOpenWith`
    #[test]
    fn menu_items_on_directory_omits_open_and_open_with() {
        let model = FileBrowserContextMenuModel::build(true, false, false, true);
        assert!(!model.contains(Open));
        assert!(!model.contains(OpenWith));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_onRoot_omitsCutCopyTrash`
    #[test]
    fn menu_items_on_root_omits_cut_copy_trash() {
        let model = FileBrowserContextMenuModel::build(true, true, false, true);
        assert!(!model.contains(Copy));
        assert!(!model.contains(Cut));
        assert!(!model.contains(Trash));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_onRoot_keepsRevealAndCopyPath`
    #[test]
    fn menu_items_on_root_keeps_reveal_and_copy_path() {
        let model = FileBrowserContextMenuModel::build(true, true, false, true);
        assert!(model.contains(RevealInFinder));
        assert!(model.contains(CopyPath));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_pasteHidden_whenPasteboardEmpty`
    #[test]
    fn menu_items_paste_hidden_when_pasteboard_empty() {
        let model = FileBrowserContextMenuModel::build(true, false, false, true);
        assert!(!model.contains(Paste));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_pasteVisible_whenPasteboardHasFileURLs`
    #[test]
    fn menu_items_paste_visible_when_pasteboard_has_file_urls() {
        let model = FileBrowserContextMenuModel::build(true, false, true, true);
        assert!(model.contains(Paste));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_orderMatchesSpec_fileRow_canPaste`
    /// (order minus the CUT `openInEditorPane`).
    #[test]
    fn menu_items_order_matches_spec_file_row_can_paste() {
        let model = FileBrowserContextMenuModel::build(false, false, true, true);
        assert_eq!(
            model.items,
            vec![Open, OpenWith, RevealInFinder, DividerOpen, Rename, Copy, CopyPath, Cut, Paste, Trash]
        );
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_orderMatchesSpec_dirRow_noPaste`
    #[test]
    fn menu_items_order_matches_spec_dir_row_no_paste() {
        let model = FileBrowserContextMenuModel::build(true, false, false, true);
        assert_eq!(
            model.items,
            vec![RevealInFinder, DividerOpen, Rename, Copy, CopyPath, Cut, Trash]
        );
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_orderMatchesSpec_rootRow_canPaste`
    #[test]
    fn menu_items_order_matches_spec_root_row_can_paste() {
        let model = FileBrowserContextMenuModel::build(true, true, true, true);
        assert_eq!(
            model.items,
            vec![RevealInFinder, DividerOpen, Rename, CopyPath, Paste]
        );
    }

    // MARK: - Rename visibility

    /// `FileBrowserContextMenuModelTests.test_menuItems_rename_visible_whenCanRenameTrue`
    #[test]
    fn menu_items_rename_visible_when_can_rename_true() {
        let model = FileBrowserContextMenuModel::build(false, false, false, true);
        assert!(model.contains(Rename));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_rename_hidden_whenCanRenameFalse`
    #[test]
    fn menu_items_rename_hidden_when_can_rename_false() {
        let model = FileBrowserContextMenuModel::build(false, false, false, false);
        assert!(!model.contains(Rename));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_rename_visible_onProjectRoot`
    #[test]
    fn menu_items_rename_visible_on_project_root() {
        let model = FileBrowserContextMenuModel::build(true, true, false, true);
        assert!(model.contains(Rename));
    }

    /// `FileBrowserContextMenuModelTests.test_menuItems_rename_positionedBetweenDividerAndCopy`
    #[test]
    fn menu_items_rename_positioned_between_divider_and_copy() {
        let model = FileBrowserContextMenuModel::build(false, false, false, true);
        let divider = model.items.iter().position(|i| *i == DividerOpen).unwrap();
        let rename = model.items.iter().position(|i| *i == Rename).unwrap();
        let copy = model.items.iter().position(|i| *i == Copy).unwrap();
        assert!(divider < rename);
        assert!(rename < copy);
    }
}
