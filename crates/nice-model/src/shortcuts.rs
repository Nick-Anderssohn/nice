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

    /// The stable string id for this action — the persistence key in the
    /// `shortcuts` section of `ui_settings.json` and R24's binding-map key. Ported
    /// verbatim from Swift's `ShortcutAction` `rawValue` (`enum ShortcutAction:
    /// String`, `KeyboardShortcuts.swift:37-70`), so the two apps agree on the JSON
    /// key even though their persisted VALUES (gpui token vs keyCode) diverge
    /// deliberately. Adding a case here must add its id (and a defaults-table row).
    pub fn id(self) -> &'static str {
        match self {
            ShortcutAction::NextSidebarTab => "nextSidebarTab",
            ShortcutAction::PrevSidebarTab => "prevSidebarTab",
            ShortcutAction::NextPane => "nextPane",
            ShortcutAction::PrevPane => "prevPane",
            ShortcutAction::NewTerminalPane => "newTerminalPane",
            ShortcutAction::ToggleSidebar => "toggleSidebar",
            ShortcutAction::ToggleSidebarMode => "toggleSidebarMode",
            ShortcutAction::ToggleHiddenFiles => "toggleHiddenFiles",
            ShortcutAction::IncreaseFontSize => "increaseFontSize",
            ShortcutAction::DecreaseFontSize => "decreaseFontSize",
            ShortcutAction::ResetFontSizes => "resetFontSizes",
            ShortcutAction::UndoFileOperation => "undoFileOperation",
            ShortcutAction::RedoFileOperation => "redoFileOperation",
        }
    }

    /// The action for a stable string [`id`](ShortcutAction::id), or `None` for an
    /// unknown id (the persistence load rule "an unknown action key ⇒ dropped
    /// silently" — the store simply skips a key `from_id` rejects).
    pub fn from_id(id: &str) -> Option<ShortcutAction> {
        ShortcutAction::ALL.into_iter().find(|a| a.id() == id)
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

/// An **owned** key combo — a [`Modifiers`] set plus an owned gpui key-token
/// `String`. The mutable / persisted counterpart of [`KeyCombo`], whose `key` is a
/// `&'static str` fixed at compile time by the defaults table. R24's binding store
/// holds `OwnedCombo`s because a user-recorded or persisted chord is not `'static`.
/// It carries the same canonical token format as [`KeyCombo::chord_str`], so the
/// two interconvert losslessly ([`From<KeyCombo>`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OwnedCombo {
    /// The modifier set held with the key.
    pub modifiers: Modifiers,
    /// The owned gpui key token (e.g. `"down"`, `"t"`, `"-"`, `"."`).
    pub key: String,
}

impl From<KeyCombo> for OwnedCombo {
    /// Own a static default combo (the seed for the mutable map from
    /// [`default_bindings`]).
    fn from(c: KeyCombo) -> Self {
        Self {
            modifiers: c.modifiers,
            key: c.key.to_string(),
        }
    }
}

impl OwnedCombo {
    /// The canonical gpui keystroke token for this combo — identical format to
    /// [`KeyCombo::chord_str`]: the modifiers in the fixed `cmd`,`ctrl`,`alt`,
    /// `shift` order (each with a trailing `-`) followed by the key token. This is
    /// the exact string persisted in the `shortcuts` section and fed to
    /// `gpui::KeyBinding` (e.g. `"cmd-alt-down"`, `"cmd--"`, `"cmd-shift-."`).
    pub fn to_token(&self) -> String {
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
        s.push_str(&self.key);
        s
    }

    /// Parse a canonical gpui keystroke token into an [`OwnedCombo`]. Strips the
    /// four modifier prefixes (`cmd-` / `ctrl-` / `alt-` / `shift-`) off the front
    /// in a loop, then takes whatever remains as the key token — so the trailing-`-`
    /// minus (`"cmd--"` ⇒ key `"-"`) and the shifted period (`"cmd-shift-."` ⇒ key
    /// `"."`) parse correctly. Returns `None` for an empty token or a token that is
    /// all modifiers with no key (e.g. `""`, `"cmd-"`). Tolerant of modifier order
    /// on input; [`to_token`](OwnedCombo::to_token) always re-emits canonical order.
    /// Our key tokens never collide with a modifier name, so greedy stripping is
    /// unambiguous.
    pub fn from_token(token: &str) -> Option<Self> {
        let mut rest = token;
        let mut modifiers = Modifiers::default();
        loop {
            if let Some(r) = rest.strip_prefix("cmd-") {
                modifiers.command = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("ctrl-") {
                modifiers.control = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("alt-") {
                modifiers.alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("shift-") {
                modifiers.shift = true;
                rest = r;
            } else {
                break;
            }
        }
        if rest.is_empty() {
            return None;
        }
        Some(Self {
            modifiers,
            key: rest.to_string(),
        })
    }
}

/// The OTHER rebindable action already bound to `combo`, or `None` if the combo is
/// free within the table. **Intra-table only**, Swift's rule verbatim
/// (`KeyboardShortcuts.swift:238-252`): it scans the 13-action `bindings`, skips
/// `excluding` (so re-saving an action's own combo is not a self-conflict), and
/// returns the first OTHER action whose bound combo equals `combo`. Modifier
/// comparison is already masked to ⌃⌥⇧⌘ ([`Modifiers`] carries only those four).
///
/// It deliberately does NOT consider the fixed accelerators (⌘N / ⌃⌘F / ⌘, / ⌘Q /
/// ⌘W) or system shortcuts — a collision with one of those is undetected, the same
/// documented blind spot Swift has. `bindings` yields `(action, Option<&combo>)`;
/// an unbound action (`None`) never conflicts.
pub fn conflicting_action<'a>(
    bindings: impl IntoIterator<Item = (ShortcutAction, Option<&'a OwnedCombo>)>,
    combo: &OwnedCombo,
    excluding: ShortcutAction,
) -> Option<ShortcutAction> {
    bindings.into_iter().find_map(|(action, bound)| {
        if action == excluding {
            return None;
        }
        match bound {
            Some(c) if c == combo => Some(action),
            _ => None,
        }
    })
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

    #[test]
    fn action_ids_round_trip_and_are_distinct() {
        // Every id maps back to its action, and the 13 ids are unique — the JSON
        // key set the `shortcuts` persistence section is keyed by.
        let mut ids = HashSet::new();
        for action in ShortcutAction::ALL {
            let id = action.id();
            assert!(!id.is_empty(), "{action:?} has an id");
            assert!(ids.insert(id), "id {id:?} is not unique");
            assert_eq!(ShortcutAction::from_id(id), Some(action));
        }
        // A spot-check of the Swift rawValues (KeyboardShortcuts.swift:37-70).
        assert_eq!(ShortcutAction::NewTerminalPane.id(), "newTerminalPane");
        assert_eq!(ShortcutAction::UndoFileOperation.id(), "undoFileOperation");
        // An unknown id is dropped (persistence load rule 3).
        assert_eq!(ShortcutAction::from_id("notAnAction"), None);
    }

    /// Owned-combo ↔ token-string round-trip, covering the three format edge cases
    /// the persistence schema names: the trailing-`-` minus (`cmd--`), the shifted
    /// period (`cmd-shift-.`), and a modifier+arrow (`cmd-alt-down`). Also the
    /// no-modifier and all-modifier cases.
    #[test]
    fn owned_combo_token_round_trip() {
        let cases = [
            ("cmd-alt-down", Modifiers::COMMAND_ALT, "down"),
            ("cmd--", Modifiers::COMMAND, "-"),
            ("cmd-shift-.", Modifiers::COMMAND_SHIFT, "."),
            ("cmd-shift-z", Modifiers::COMMAND_SHIFT, "z"),
            ("cmd-0", Modifiers::COMMAND, "0"),
            (
                "cmd-ctrl-alt-shift-t",
                Modifiers {
                    command: true,
                    control: true,
                    alt: true,
                    shift: true,
                },
                "t",
            ),
            (
                "-",
                Modifiers::default(), // a bare key, no modifiers
                "-",
            ),
        ];
        for (token, modifiers, key) in cases {
            let parsed = OwnedCombo::from_token(token).expect("token parses");
            assert_eq!(parsed.modifiers, modifiers, "modifiers for {token:?}");
            assert_eq!(parsed.key, key, "key for {token:?}");
            // The canonical re-emission is exactly the input (all inputs canonical).
            assert_eq!(parsed.to_token(), token, "round-trips to {token:?}");
        }
    }

    /// Modifier order on INPUT is tolerated; output is canonical.
    #[test]
    fn from_token_tolerates_modifier_order() {
        let parsed = OwnedCombo::from_token("shift-cmd-alt-down").unwrap();
        assert_eq!(parsed.modifiers, {
            let mut m = Modifiers::COMMAND_ALT;
            m.shift = true;
            m
        });
        assert_eq!(parsed.key, "down");
        // Re-emitted in canonical cmd,ctrl,alt,shift order.
        assert_eq!(parsed.to_token(), "cmd-alt-shift-down");
    }

    /// A token that is empty or all-modifiers-no-key is rejected.
    #[test]
    fn from_token_rejects_keyless() {
        assert_eq!(OwnedCombo::from_token(""), None);
        assert_eq!(OwnedCombo::from_token("cmd-"), None);
        assert_eq!(OwnedCombo::from_token("cmd-shift-"), None);
    }

    /// Every default combo owns-and-round-trips through the token string — the
    /// interchange the persistence layer writes.
    #[test]
    fn default_combos_own_and_round_trip() {
        for (action, combo) in default_bindings() {
            let owned = OwnedCombo::from(combo);
            assert_eq!(owned.to_token(), combo.chord_str(), "{action:?} token");
            assert_eq!(
                OwnedCombo::from_token(&owned.to_token()),
                Some(owned.clone()),
                "{action:?} round-trips"
            );
        }
    }

    /// `conflicting_action` — Swift's intra-table rule
    /// (`KeyboardShortcuts.swift:238-252`): a free combo → `None`; a combo held by
    /// another action → that action; an action's OWN combo excluding itself →
    /// `None` (re-saving is not a self-conflict); comparison is masked to ⌃⌥⇧⌘.
    #[test]
    fn conflicting_action_intra_table_rules() {
        // The default map as an owned (action, Some(combo)) list.
        let bindings: Vec<(ShortcutAction, Option<OwnedCombo>)> = default_bindings()
            .into_iter()
            .map(|(a, c)| (a, Some(OwnedCombo::from(c))))
            .collect();
        let view = || bindings.iter().map(|(a, c)| (*a, c.as_ref()));

        // A distinct, unbound combo conflicts with nothing.
        let free = OwnedCombo::from_token("cmd-y").unwrap();
        assert_eq!(
            conflicting_action(view(), &free, ShortcutAction::NewTerminalPane),
            None
        );

        // `cmd-t` is NewTerminalPane's default. Asking on behalf of a DIFFERENT
        // action (ToggleSidebar) finds the holder.
        let cmd_t = OwnedCombo::from_token("cmd-t").unwrap();
        assert_eq!(
            conflicting_action(view(), &cmd_t, ShortcutAction::ToggleSidebar),
            Some(ShortcutAction::NewTerminalPane)
        );

        // Re-saving NewTerminalPane's own combo, excluding itself, is not a
        // self-conflict.
        assert_eq!(
            conflicting_action(view(), &cmd_t, ShortcutAction::NewTerminalPane),
            None
        );

        // An unbound action never conflicts: drop NewTerminalPane's binding, then
        // `cmd-t` is free.
        let mut cleared = bindings.clone();
        for (a, c) in cleared.iter_mut() {
            if *a == ShortcutAction::NewTerminalPane {
                *c = None;
            }
        }
        assert_eq!(
            conflicting_action(
                cleared.iter().map(|(a, c)| (*a, c.as_ref())),
                &cmd_t,
                ShortcutAction::ToggleSidebar
            ),
            None
        );
    }

    /// Conflict comparison is over the full masked `(modifiers, key)` pair: the
    /// same key with a different modifier set does not conflict.
    #[test]
    fn conflicting_action_compares_modifiers() {
        let bindings: Vec<(ShortcutAction, Option<OwnedCombo>)> = default_bindings()
            .into_iter()
            .map(|(a, c)| (a, Some(OwnedCombo::from(c))))
            .collect();
        let view = || bindings.iter().map(|(a, c)| (*a, c.as_ref()));

        // NewTerminalPane holds plain `cmd-t`. `cmd-shift-t` shares the key but not
        // the modifier set — no conflict.
        let cmd_shift_t = OwnedCombo::from_token("cmd-shift-t").unwrap();
        assert_eq!(
            conflicting_action(view(), &cmd_shift_t, ShortcutAction::ToggleSidebar),
            None
        );
    }
}
