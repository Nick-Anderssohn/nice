//! Keyboard-shortcut data — the closed set of user-rebindable actions and their
//! default key combos, as pure data with **no `gpui` dependency**. Ported from
//! `Sources/Nice/State/KeyboardShortcuts.swift` (`ShortcutAction`, `KeyCombo`,
//! and the `defaults` map).
//!
//! This module is the single source of truth for *which* actions exist and
//! *what* each is bound to by default. Two consumers read it:
//!
//! * **R12's keymap wiring** (`crates/nice`, the next slice) generates gpui
//!   `actions!` + `bind_keys` from [`default_bindings`] — turning each
//!   [`KeyCombo`] into a `gpui::KeyBinding` via [`KeyCombo::chord_str`].
//! * **R24's rebinding UI** (Stage 6) consumes the same table — the action set
//!   ([`ShortcutAction::ALL`]), the per-action [`ShortcutAction::label`], and the
//!   default combos — so the recorder can present, diff against, and restore
//!   defaults. Only the defaults table is data now; the mutable binding store,
//!   persistence, and conflict UI are R24's.
//!
//! ## Documented divergence — character-based matching at the gpui pin
//!
//! The Swift monitor matched layout-independent physical `keyCode`s. gpui's
//! keymap matches on the produced key *character*, with layout handling via
//! `use_key_equivalents` / `PlatformKeyboardMapper` (verified: the pin exposes
//! no keycode-binding API). So the combos here are expressed as a modifier set
//! plus a gpui key *token* (e.g. `"down"`, `"t"`, `"="`), and [`chord_str`]
//! emits a canonical gpui keystroke string. The keymap slice binds these with
//! `use_key_equivalents` semantics and records the divergence; full layout
//! parity is R24's question (it owns rebinding). This crate stays gpui-free —
//! the token strings are plain data that the keymap slice feeds to gpui.
//!
//! [`chord_str`]: KeyCombo::chord_str

/// The closed set of user-rebindable actions surfaced in the (future) Settings ▸
/// Shortcuts pane. Ported case-for-case from Swift's `ShortcutAction`. Adding a
/// case here extends [`ShortcutAction::ALL`] (which the completeness test pins
/// against [`default_bindings`]) and the recorder list R24 iterates.
///
/// The set is intentionally exactly these 13 — the actions Nice lets a user
/// rebind. Window-management accelerators that are *not* rebindable (New Window
/// ⌘N, Toggle Full Screen ⌃⌘F) are deliberately absent: they live as fixed menu
/// actions in `crates/nice`, not in this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    /// Cycle to the next sidebar tab (⌘⌥↓ by default).
    NextSidebarTab,
    /// Cycle to the previous sidebar tab (⌘⌥↑).
    PrevSidebarTab,
    /// Focus the next pane in the active tab (⌘⌥→).
    NextPane,
    /// Focus the previous pane in the active tab (⌘⌥←).
    PrevPane,
    /// Add a new terminal pane to the active tab (⌘T).
    NewTerminalPane,
    /// Collapse / expand the sidebar (⌘B).
    ToggleSidebar,
    /// Switch the sidebar between tabs and files mode (⌘⇧B).
    ToggleSidebarMode,
    /// Toggle hidden files in the file browser (⌘⇧.). Deferred handler — R19.
    ToggleHiddenFiles,
    /// Grow the terminal font (⌘=).
    IncreaseFontSize,
    /// Shrink the terminal font (⌘−).
    DecreaseFontSize,
    /// Reset the terminal font size (⌘0).
    ResetFontSizes,
    /// Undo the last file operation (⌘Z). Deferred handler — R20.
    UndoFileOperation,
    /// Redo the last file operation (⌘⇧Z). Deferred handler — R20.
    RedoFileOperation,
}

impl ShortcutAction {
    /// Every action, in a stable order. Used by the completeness test and by
    /// R24's recorder (which renders one row per action). The order matches the
    /// enum declaration and Swift's `allCases`.
    pub const ALL: [ShortcutAction; 13] = [
        ShortcutAction::NextSidebarTab,
        ShortcutAction::PrevSidebarTab,
        ShortcutAction::NextPane,
        ShortcutAction::PrevPane,
        ShortcutAction::NewTerminalPane,
        ShortcutAction::ToggleSidebar,
        ShortcutAction::ToggleSidebarMode,
        ShortcutAction::ToggleHiddenFiles,
        ShortcutAction::IncreaseFontSize,
        ShortcutAction::DecreaseFontSize,
        ShortcutAction::ResetFontSizes,
        ShortcutAction::UndoFileOperation,
        ShortcutAction::RedoFileOperation,
    ];

    /// Human-readable label for the (future) recorder row. Ported verbatim from
    /// Swift's `ShortcutAction.label`.
    pub fn label(self) -> &'static str {
        match self {
            ShortcutAction::NextSidebarTab => "Next sidebar tab",
            ShortcutAction::PrevSidebarTab => "Previous sidebar tab",
            ShortcutAction::NextPane => "Next pane",
            ShortcutAction::PrevPane => "Previous pane",
            ShortcutAction::NewTerminalPane => "New terminal pane",
            ShortcutAction::ToggleSidebar => "Toggle sidebar",
            ShortcutAction::ToggleSidebarMode => "Toggle sidebar mode",
            ShortcutAction::ToggleHiddenFiles => "Toggle hidden files",
            ShortcutAction::IncreaseFontSize => "Increase font size",
            ShortcutAction::DecreaseFontSize => "Decrease font size",
            ShortcutAction::ResetFontSizes => "Reset font size",
            ShortcutAction::UndoFileOperation => "Undo file operation",
            ShortcutAction::RedoFileOperation => "Redo file operation",
        }
    }
}

/// The four modifiers a shortcut can carry — the Rust mirror of Swift's
/// `KeyCombo.relevantModifierMask` (`⌃ ⌥ ⇧ ⌘`). Caps Lock / numeric-keypad /
/// function bits are not represented: they are never part of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// ⌘ (`cmd` in a gpui keystroke).
    pub command: bool,
    /// ⌃ (`ctrl`).
    pub control: bool,
    /// ⌥ (`alt`).
    pub alt: bool,
    /// ⇧ (`shift`).
    pub shift: bool,
}

impl Modifiers {
    /// ⌘ only.
    pub const COMMAND: Modifiers = Modifiers {
        command: true,
        control: false,
        alt: false,
        shift: false,
    };
    /// ⌘⌥.
    pub const COMMAND_ALT: Modifiers = Modifiers {
        command: true,
        control: false,
        alt: true,
        shift: false,
    };
    /// ⌘⇧.
    pub const COMMAND_SHIFT: Modifiers = Modifiers {
        command: true,
        control: false,
        alt: false,
        shift: true,
    };
}

/// A default key combo: a [`Modifiers`] set plus a gpui key *token* (the string
/// gpui's `Keystroke::parse` expects for the key — e.g. `"down"`, `"t"`, `"="`,
/// `"-"`, `"."`). The Rust analogue of Swift's `KeyCombo`, but character-token
/// based rather than physical-keycode based (see the module divergence note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    /// The modifier set held with the key.
    pub modifiers: Modifiers,
    /// The gpui key token. Kept as data so the keymap slice can hand it to
    /// `gpui::KeyBinding` without this crate importing gpui.
    pub key: &'static str,
}

impl KeyCombo {
    /// The canonical gpui keystroke string for this combo — modifiers in a fixed
    /// `cmd`, `ctrl`, `alt`, `shift` order followed by the key token, joined with
    /// `-` (gpui's `Keystroke::parse` syntax; e.g. `⌘⌥↓` → `"cmd-alt-down"`,
    /// `⌘−` → `"cmd--"`, `⌘⇧.` → `"cmd-shift-."`). The keymap slice feeds this to
    /// `KeyBinding::new`. Modifier order is irrelevant to gpui matching (it sets
    /// flags), so the fixed order is purely for a stable, readable string.
    pub fn chord_str(&self) -> String {
        let mut s = String::new();
        if self.modifiers.command {
            s.push_str("cmd-");
        }
        if self.modifiers.control {
            s.push_str("ctrl-");
        }
        if self.modifiers.alt {
            s.push_str("alt-");
        }
        if self.modifiers.shift {
            s.push_str("shift-");
        }
        s.push_str(self.key);
        s
    }
}

/// The default binding for every [`ShortcutAction`], in [`ShortcutAction::ALL`]
/// order. Ported from Swift's `KeyboardShortcuts.defaults` (the Option-B +
/// pure-wrap scheme the user picked: directional arrows for both axes, ⌘T for a
/// new pane, ⌘B for the sidebar). Every action has exactly one default combo,
/// and no two actions share a combo — both pinned by this module's tests, and by
/// the keymap slice which would otherwise register a colliding binding.
pub fn default_bindings() -> [(ShortcutAction, KeyCombo); 13] {
    use ShortcutAction::*;
    [
        (
            NextSidebarTab,
            KeyCombo {
                modifiers: Modifiers::COMMAND_ALT,
                key: "down",
            },
        ),
        (
            PrevSidebarTab,
            KeyCombo {
                modifiers: Modifiers::COMMAND_ALT,
                key: "up",
            },
        ),
        (
            NextPane,
            KeyCombo {
                modifiers: Modifiers::COMMAND_ALT,
                key: "right",
            },
        ),
        (
            PrevPane,
            KeyCombo {
                modifiers: Modifiers::COMMAND_ALT,
                key: "left",
            },
        ),
        (
            NewTerminalPane,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "t",
            },
        ),
        (
            ToggleSidebar,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "b",
            },
        ),
        (
            ToggleSidebarMode,
            KeyCombo {
                modifiers: Modifiers::COMMAND_SHIFT,
                key: "b",
            },
        ),
        (
            ToggleHiddenFiles,
            KeyCombo {
                modifiers: Modifiers::COMMAND_SHIFT,
                key: ".",
            },
        ),
        (
            IncreaseFontSize,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "=",
            },
        ),
        (
            DecreaseFontSize,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "-",
            },
        ),
        (
            ResetFontSizes,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "0",
            },
        ),
        (
            UndoFileOperation,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "z",
            },
        ),
        (
            RedoFileOperation,
            KeyCombo {
                modifiers: Modifiers::COMMAND_SHIFT,
                key: "z",
            },
        ),
    ]
}

/// Look up an action's default combo, or `None` if (impossibly) absent. A thin
/// convenience over [`default_bindings`] for the keymap slice and R24.
pub fn default_combo(action: ShortcutAction) -> Option<KeyCombo> {
    default_bindings()
        .into_iter()
        .find(|(a, _)| *a == action)
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_is_complete_every_action_bound_exactly_once() {
        let table = default_bindings();
        assert_eq!(table.len(), 13, "13 rebindable actions");
        assert_eq!(
            ShortcutAction::ALL.len(),
            13,
            "ALL enumerates all 13 actions"
        );
        // Every action in ALL appears exactly once as a table key.
        for action in ShortcutAction::ALL {
            let hits = table.iter().filter(|(a, _)| *a == action).count();
            assert_eq!(
                hits, 1,
                "{action:?} must have exactly one default binding, found {hits}"
            );
        }
        // And the table introduces no action outside ALL.
        let all: HashSet<ShortcutAction> = ShortcutAction::ALL.into_iter().collect();
        for (action, _) in table {
            assert!(all.contains(&action), "{action:?} is not in ShortcutAction::ALL");
        }
    }

    #[test]
    fn every_default_combo_is_unique() {
        // No two actions share a combo — a collision would make one binding
        // shadow another in the keymap. Uniqueness is over the full
        // (modifiers, key) pair.
        let combos: Vec<KeyCombo> = default_bindings().into_iter().map(|(_, c)| c).collect();
        let distinct: HashSet<KeyCombo> = combos.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            combos.len(),
            "default combos must be pairwise distinct; found a collision"
        );
    }

    #[test]
    fn chord_str_emits_canonical_gpui_keystrokes() {
        // Pins the interchange format the keymap slice depends on. Covers the
        // arrow + letter combos, the trailing-'-' minus case, and the shifted
        // period (the character-based-matching divergence).
        let combo = |a| default_combo(a).unwrap().chord_str();
        assert_eq!(combo(ShortcutAction::NextSidebarTab), "cmd-alt-down");
        assert_eq!(combo(ShortcutAction::PrevSidebarTab), "cmd-alt-up");
        assert_eq!(combo(ShortcutAction::NextPane), "cmd-alt-right");
        assert_eq!(combo(ShortcutAction::PrevPane), "cmd-alt-left");
        assert_eq!(combo(ShortcutAction::NewTerminalPane), "cmd-t");
        assert_eq!(combo(ShortcutAction::ToggleSidebar), "cmd-b");
        assert_eq!(combo(ShortcutAction::ToggleSidebarMode), "cmd-shift-b");
        assert_eq!(combo(ShortcutAction::ToggleHiddenFiles), "cmd-shift-.");
        assert_eq!(combo(ShortcutAction::IncreaseFontSize), "cmd-=");
        assert_eq!(combo(ShortcutAction::DecreaseFontSize), "cmd--");
        assert_eq!(combo(ShortcutAction::ResetFontSizes), "cmd-0");
        assert_eq!(combo(ShortcutAction::UndoFileOperation), "cmd-z");
        assert_eq!(combo(ShortcutAction::RedoFileOperation), "cmd-shift-z");
    }

    #[test]
    fn every_action_has_a_nonempty_label() {
        for action in ShortcutAction::ALL {
            assert!(!action.label().is_empty(), "{action:?} has a label");
        }
    }
}
