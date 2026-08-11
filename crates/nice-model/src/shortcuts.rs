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
/// The set is intentionally exactly these 22 — the actions Nice lets a user
/// rebind. Window-management accelerators that are *not* rebindable (New Window
/// ⌘N, Toggle Full Screen ⌃⌘F) are deliberately absent: they live as fixed menu
/// actions in `crates/nice`, not in this table.
///
/// The trailing eight are the tmux-port Phase 1 held-`⌃⌘` scheme (roadmap
/// "Phase 1 — held-modifier keybind scheme"): the four `FocusPane*` directions,
/// [`LastActiveWindow`](ShortcutAction::LastActiveWindow), the two half-page
/// scrollback actions, and the single
/// [`WindowByIndex`](ShortcutAction::WindowByIndex) template row that covers all
/// nine `⌃⌘1`…`⌃⌘9` chords (D2 — one settings row, not nine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    /// Cycle to the next sidebar session (⌘⌥↓ by default).
    NextSidebarSession,
    /// Cycle to the previous sidebar session (⌘⌥↑).
    PrevSidebarSession,
    /// Focus the next window in the active session (⌃⌘] since Phase 1, D1).
    NextWindow,
    /// Focus the previous window in the active session (⌃⌘[ since Phase 1, D1).
    PrevWindow,
    /// Add a new terminal window to the active session (⌘T).
    NewTerminalWindow,
    /// Collapse / expand the sidebar (⌘B).
    ToggleSidebar,
    /// Switch the sidebar between sessions and files mode (⌘⇧B).
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
    /// Command Compose (⌘↩): rewrite the plain-English text in zsh's line buffer
    /// into a real command via `claude -p`. Fires only at an idle interactive
    /// prompt — the Nice-side gate and the ZLE widget live in `crates/nice`.
    /// The first Rust-only action: it has no Swift-prod `rawValue` counterpart.
    CommandCompose,
    /// Move focus one pane to the LEFT (⌃⌘H). Named for its Phase-2 spatial
    /// meaning — tmux `select-pane -L` over the future split tree (D3).
    /// Pre-splits there is nothing horizontal but the pill strip, so it aliases
    /// "previous window"; Phase 2 swaps the handler, not the binding.
    FocusPaneLeft,
    /// Move focus one pane DOWN (⌃⌘J) — tmux `select-pane -D` (D3). Registered
    /// but inert pre-splits (nothing is stacked vertically yet).
    FocusPaneDown,
    /// Move focus one pane UP (⌃⌘K) — tmux `select-pane -U` (D3). Registered but
    /// inert pre-splits.
    FocusPaneUp,
    /// Move focus one pane to the RIGHT (⌃⌘L) — tmux `select-pane -R` (D3).
    /// Pre-splits it aliases "next window".
    FocusPaneRight,
    /// Jump back to the window that was active before the current one (⌃⌘O) —
    /// tmux `last-window`. A single "previous" slot, not an MRU stack.
    LastActiveWindow,
    /// Scroll the active window's viewport half a screen toward history (⌃⌘U) —
    /// tmux copy-mode `halfpage-up`.
    ScrollHalfPageUp,
    /// Scroll the active window's viewport half a screen toward the bottom
    /// (⌃⌘D) — tmux copy-mode `halfpage-down`.
    ScrollHalfPageDown,
    /// Focus the Nth window of the active session (⌃⌘1…⌃⌘9) — tmux
    /// `select-window -t N`. **One** action covers all nine chords (D2): the
    /// stored combo carries the modifier set plus a normalized digit
    /// ([`WINDOW_INDEX_STORED_KEY`]), and the keymap expands it into nine
    /// bindings over [`WINDOW_INDEX_KEYS`].
    WindowByIndex,
}

/// The nine key tokens the single [`ShortcutAction::WindowByIndex`] row expands
/// over (D2) — index 1 is `WINDOW_INDEX_KEYS[0]`. Shared by the keymap's
/// nine-binding expansion, the recorder's digit gate, and
/// [`conflicting_action`]'s digit expansion so the three can't drift.
pub const WINDOW_INDEX_KEYS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// The digit [`ShortcutAction::WindowByIndex`] normalizes to when stored (D2):
/// the persisted combo always spells digit `1`, and *means* "these modifiers +
/// any of [`WINDOW_INDEX_KEYS`]". Recording `⌃⌘7` therefore stores `ctrl-cmd-1`
/// and rebinds all nine chords.
pub const WINDOW_INDEX_STORED_KEY: &str = "1";

/// Whether `key` is one of the nine digits [`ShortcutAction::WindowByIndex`]
/// claims. The recorder commits a `WindowByIndex` capture only for these.
pub fn is_window_index_key(key: &str) -> bool {
    WINDOW_INDEX_KEYS.contains(&key)
}

impl ShortcutAction {
    /// Every action, in a stable order. Used by the completeness test and by
    /// R24's recorder (which renders one row per action). The order matches the
    /// enum declaration and Swift's `allCases`.
    pub const ALL: [ShortcutAction; 22] = [
        ShortcutAction::NextSidebarSession,
        ShortcutAction::PrevSidebarSession,
        ShortcutAction::NextWindow,
        ShortcutAction::PrevWindow,
        ShortcutAction::NewTerminalWindow,
        ShortcutAction::ToggleSidebar,
        ShortcutAction::ToggleSidebarMode,
        ShortcutAction::ToggleHiddenFiles,
        ShortcutAction::IncreaseFontSize,
        ShortcutAction::DecreaseFontSize,
        ShortcutAction::ResetFontSizes,
        ShortcutAction::UndoFileOperation,
        ShortcutAction::RedoFileOperation,
        ShortcutAction::CommandCompose,
        ShortcutAction::FocusPaneLeft,
        ShortcutAction::FocusPaneDown,
        ShortcutAction::FocusPaneUp,
        ShortcutAction::FocusPaneRight,
        ShortcutAction::LastActiveWindow,
        ShortcutAction::ScrollHalfPageUp,
        ShortcutAction::ScrollHalfPageDown,
        ShortcutAction::WindowByIndex,
    ];

    /// Human-readable label for the (future) recorder row. Ported verbatim from
    /// Swift's `ShortcutAction.label`.
    pub fn label(self) -> &'static str {
        match self {
            ShortcutAction::NextSidebarSession => "Next sidebar session",
            ShortcutAction::PrevSidebarSession => "Previous sidebar session",
            ShortcutAction::NextWindow => "Next window",
            ShortcutAction::PrevWindow => "Previous window",
            ShortcutAction::NewTerminalWindow => "New terminal window",
            ShortcutAction::ToggleSidebar => "Toggle sidebar",
            ShortcutAction::ToggleSidebarMode => "Toggle sidebar mode",
            ShortcutAction::ToggleHiddenFiles => "Toggle hidden files",
            ShortcutAction::IncreaseFontSize => "Increase font size",
            ShortcutAction::DecreaseFontSize => "Decrease font size",
            ShortcutAction::ResetFontSizes => "Reset font size",
            ShortcutAction::UndoFileOperation => "Undo file operation",
            ShortcutAction::RedoFileOperation => "Redo file operation",
            ShortcutAction::CommandCompose => "Compose command",
            ShortcutAction::FocusPaneLeft => "Focus pane left",
            ShortcutAction::FocusPaneDown => "Focus pane down",
            ShortcutAction::FocusPaneUp => "Focus pane up",
            ShortcutAction::FocusPaneRight => "Focus pane right",
            ShortcutAction::LastActiveWindow => "Last active window",
            ShortcutAction::ScrollHalfPageUp => "Scroll half page up",
            ShortcutAction::ScrollHalfPageDown => "Scroll half page down",
            ShortcutAction::WindowByIndex => "Window 1-9",
        }
    }

    /// Explanatory tooltip text for the recorder row's ⓘ affordance, or `None`
    /// for the actions whose label is self-explanatory. Two carry one:
    /// `CommandCompose` (its label alone doesn't say what the feature does) and
    /// `WindowByIndex` (one row standing for nine chords is not guessable).
    pub fn info(self) -> Option<&'static str> {
        match self {
            ShortcutAction::WindowByIndex => Some(
                "One shortcut covering nine chords: the recorded modifiers plus \
                 the digits 1 through 9 jump straight to that window in the \
                 active session. Record it with any digit — all nine rebind \
                 together.",
            ),
            ShortcutAction::CommandCompose => Some(
                "Turns plain English typed at a zsh prompt into a real command \
                 using Claude Code. The command is placed at the prompt for \
                 review — press Enter yourself to run it. Does nothing while a \
                 program is running in the window.",
            ),
            _ => None,
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
            ShortcutAction::NextSidebarSession => "nextSidebarTab",
            ShortcutAction::PrevSidebarSession => "prevSidebarTab",
            ShortcutAction::NextWindow => "nextPane",
            ShortcutAction::PrevWindow => "prevPane",
            ShortcutAction::NewTerminalWindow => "newTerminalPane",
            ShortcutAction::ToggleSidebar => "toggleSidebar",
            ShortcutAction::ToggleSidebarMode => "toggleSidebarMode",
            ShortcutAction::ToggleHiddenFiles => "toggleHiddenFiles",
            ShortcutAction::IncreaseFontSize => "increaseFontSize",
            ShortcutAction::DecreaseFontSize => "decreaseFontSize",
            ShortcutAction::ResetFontSizes => "resetFontSizes",
            ShortcutAction::UndoFileOperation => "undoFileOperation",
            ShortcutAction::RedoFileOperation => "redoFileOperation",
            ShortcutAction::CommandCompose => "commandCompose",
            // Phase 1 (tmux port) — new ids, additive to the frozen surface.
            // Absent from every existing `ui_settings.json`, so old files load
            // unchanged (frozen load rule 5 ships them unbound for anyone who
            // ever rebound something — accepted, no seeding).
            ShortcutAction::FocusPaneLeft => "focusPaneLeft",
            ShortcutAction::FocusPaneDown => "focusPaneDown",
            ShortcutAction::FocusPaneUp => "focusPaneUp",
            ShortcutAction::FocusPaneRight => "focusPaneRight",
            ShortcutAction::LastActiveWindow => "lastActiveWindow",
            ShortcutAction::ScrollHalfPageUp => "scrollHalfPageUp",
            ShortcutAction::ScrollHalfPageDown => "scrollHalfPageDown",
            ShortcutAction::WindowByIndex => "windowByIndex",
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
    /// ⌃⌘ — the tmux-port Phase 1 held-modifier pair (D1/D2/D3).
    pub const CONTROL_COMMAND: Modifiers = Modifiers {
        command: true,
        control: true,
        alt: false,
        shift: false,
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
/// new window, ⌘B for the sidebar). Every action has exactly one default combo,
/// and no two actions share a combo — both pinned by this module's tests, and by
/// the keymap slice which would otherwise register a colliding binding.
/// The eight trailing rows are Phase 1's held-`⌃⌘` scheme; `NextWindow` /
/// `PrevWindow` moved off `⌘⌥→`/`⌘⌥←` onto `⌃⌘]`/`⌃⌘[` (D1) so the whole
/// pill-navigation vocabulary lives under one held modifier pair.
pub fn default_bindings() -> [(ShortcutAction, KeyCombo); 22] {
    use ShortcutAction::*;
    [
        (
            NextSidebarSession,
            KeyCombo {
                modifiers: Modifiers::COMMAND_ALT,
                key: "down",
            },
        ),
        (
            PrevSidebarSession,
            KeyCombo {
                modifiers: Modifiers::COMMAND_ALT,
                key: "up",
            },
        ),
        // D1: the prev/next pill chords moved to ⌃⌘]/⌃⌘[; ⌘⌥←/→ are freed.
        (
            NextWindow,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "]",
            },
        ),
        (
            PrevWindow,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "[",
            },
        ),
        (
            NewTerminalWindow,
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
        (
            CommandCompose,
            KeyCombo {
                modifiers: Modifiers::COMMAND,
                key: "enter",
            },
        ),
        // -- Phase 1 (tmux port): the held-⌃⌘ vim-key scheme -----------------
        (
            FocusPaneLeft,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "h",
            },
        ),
        (
            FocusPaneDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "j",
            },
        ),
        (
            FocusPaneUp,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "k",
            },
        ),
        (
            FocusPaneRight,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "l",
            },
        ),
        (
            LastActiveWindow,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "o",
            },
        ),
        (
            ScrollHalfPageUp,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "u",
            },
        ),
        (
            ScrollHalfPageDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "d",
            },
        ),
        // D2: the stored digit is always the normalized `1`; the combo MEANS
        // "these modifiers + digits 1-9", expanded by the keymap.
        (
            WindowByIndex,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: WINDOW_INDEX_STORED_KEY,
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
/// (`KeyboardShortcuts.swift:238-252`): it scans the whole `bindings` map, skips
/// `excluding` (so re-saving an action's own combo is not a self-conflict), and
/// returns the first OTHER action whose bound combo equals `combo`. Modifier
/// comparison is already masked to ⌃⌥⇧⌘ ([`Modifiers`] carries only those four).
///
/// It deliberately does NOT consider the fixed accelerators (⌘N / ⌃⌘F / ⌘, / ⌘Q /
/// ⌘W) or system shortcuts — a collision with one of those is undetected, the same
/// documented blind spot Swift has. `bindings` yields `(action, Option<&combo>)`;
/// an unbound action (`None`) never conflicts.
///
/// **Digit expansion (D2).** [`ShortcutAction::WindowByIndex`] is one row standing
/// for nine chords, so equality is not enough: its combo claims its modifier set
/// paired with EVERY key in [`WINDOW_INDEX_KEYS`]. Two combos conflict when their
/// modifier sets match and their claimed key sets intersect — which covers both
/// directions (recording `⌃⌘3` on another action hits `WindowByIndex`, and
/// recording `WindowByIndex` onto modifiers that already hold a digit-keyed
/// binding hits that action).
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
            Some(c) if combos_overlap((action, c), (excluding, combo)) => Some(action),
            _ => None,
        }
    })
}

/// Whether `combo`, held by `action`, claims the key token `key`. Normally that is
/// plain equality; the [`ShortcutAction::WindowByIndex`] template row instead claims
/// every key in [`WINDOW_INDEX_KEYS`] (D2). A `WindowByIndex` combo whose stored key
/// is somehow NOT a digit (reachable only via a hand-edited `ui_settings.json`)
/// falls back to equality, so a corrupt file can't silently swallow all nine digits.
fn claims_key(action: ShortcutAction, combo: &OwnedCombo, key: &str) -> bool {
    if action == ShortcutAction::WindowByIndex && is_window_index_key(&combo.key) {
        is_window_index_key(key)
    } else {
        combo.key == key
    }
}

/// Do the two (action, combo) pairs claim any chord in common? Same modifier set,
/// and either one's claimed key set contains the other's key (symmetric, so the
/// digit expansion catches a collision from whichever side it is recorded).
fn combos_overlap(a: (ShortcutAction, &OwnedCombo), b: (ShortcutAction, &OwnedCombo)) -> bool {
    if a.1.modifiers != b.1.modifiers {
        return false;
    }
    claims_key(a.0, a.1, &b.1.key) || claims_key(b.0, b.1, &a.1.key)
}

// ===========================================================================
// Reserved combos — the recorder's protected set (Phase 1, Slice 2)
// ===========================================================================

/// Why a combo is reserved — which of the three groups the entry belongs to.
/// Only [`FixedAccelerator`](ReservedKind::FixedAccelerator) is also a *live
/// binding*: `crates/nice`'s `keymap::non_rebindable_bindings` installs those five
/// from these very entries, so the guard and the installs cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservedKind {
    /// Group (a) — a FIXED Nice accelerator that lives outside the rebindable
    /// table (⌘Q, ⌘N, ⌘W, ⌘, , ⌃⌘F). Recording one would shadow a menu command
    /// with no way to get it back.
    FixedAccelerator,
    /// Group (b) — claimed by macOS itself, so the chord may never even reach
    /// Nice (⌃⌘Q lock screen, ⌃⌘Space emoji picker, ⌃⌘D dictionary lookup).
    SystemReserved,
    /// Group (c) — held for a later tmux-port phase (D4: ⌃⌘Z, ⌃⌘V, ⌃⌘S, ⌃⌘/), so
    /// nothing can squat on the chord before Phases 2/3 claim it.
    FuturePhase,
}

/// One reserved chord plus the user-facing reason the recorder shows when a
/// capture lands on it. Pure data — the recorder refuses the capture and prints
/// [`reason`](ReservedCombo::reason); no store write happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReservedCombo {
    /// The protected chord.
    pub combo: KeyCombo,
    /// Which group it comes from.
    pub kind: ReservedKind,
    /// The recorder's explanation, e.g. `"Reserved: the macOS emoji picker"`.
    pub reason: &'static str,
}

/// ⌘Q — Nice's Quit accelerator (`crate::app::Quit`).
pub const RESERVED_QUIT: ReservedCombo = ReservedCombo {
    combo: KeyCombo {
        modifiers: Modifiers::COMMAND,
        key: "q",
    },
    kind: ReservedKind::FixedAccelerator,
    reason: "Reserved: Nice's Quit shortcut",
};

/// ⌘N — Nice's New Window accelerator (`crate::app::NewWindow`).
pub const RESERVED_NEW_WINDOW: ReservedCombo = ReservedCombo {
    combo: KeyCombo {
        modifiers: Modifiers::COMMAND,
        key: "n",
    },
    kind: ReservedKind::FixedAccelerator,
    reason: "Reserved: Nice's New Window shortcut",
};

/// ⌘W — Nice's Close Window accelerator (`crate::app::CloseWindow`).
pub const RESERVED_CLOSE_WINDOW: ReservedCombo = ReservedCombo {
    combo: KeyCombo {
        modifiers: Modifiers::COMMAND,
        key: "w",
    },
    kind: ReservedKind::FixedAccelerator,
    reason: "Reserved: Nice's Close Window shortcut",
};

/// ⌘, — Nice's Settings accelerator (`crate::settings::window::OpenSettings`).
pub const RESERVED_OPEN_SETTINGS: ReservedCombo = ReservedCombo {
    combo: KeyCombo {
        modifiers: Modifiers::COMMAND,
        key: ",",
    },
    kind: ReservedKind::FixedAccelerator,
    reason: "Reserved: Nice's Settings shortcut",
};

/// ⌃⌘F — Nice's Full Screen accelerator (`crate::app::ToggleFullScreen`).
pub const RESERVED_TOGGLE_FULL_SCREEN: ReservedCombo = ReservedCombo {
    combo: KeyCombo {
        modifiers: Modifiers::CONTROL_COMMAND,
        key: "f",
    },
    kind: ReservedKind::FixedAccelerator,
    reason: "Reserved: Nice's Full Screen shortcut",
};

/// Every reserved chord, in group order (a → b → c). The recorder consults this
/// BEFORE the intra-table conflict check, and the keymap installs the
/// [`FixedAccelerator`](ReservedKind::FixedAccelerator) five from the same entries.
///
/// The sixth non-rebindable install — the context-scoped Esc in `SidebarShell` —
/// is deliberately NOT here: plain Escape cancels the capture before any reserved
/// lookup runs, so it is uncapturable anyway and the entry would be dead data.
///
/// ⌃⌘D carries the Phase 1 default for "Scroll half page down" AND is a macOS
/// dictionary chord. Both come from the plan (Slice 1's defaults; Slice 2's group
/// b). The guard only governs the RECORDER, so the shipped default binding is
/// unaffected — it just cannot be re-recorded by hand.
pub const RESERVED_COMBOS: [ReservedCombo; 12] = [
    // (a) Nice's own fixed accelerators.
    RESERVED_QUIT,
    RESERVED_NEW_WINDOW,
    RESERVED_CLOSE_WINDOW,
    RESERVED_OPEN_SETTINGS,
    RESERVED_TOGGLE_FULL_SCREEN,
    // (b) macOS-system-reserved.
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "q",
        },
        kind: ReservedKind::SystemReserved,
        reason: "Reserved: the macOS lock screen",
    },
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "space",
        },
        kind: ReservedKind::SystemReserved,
        reason: "Reserved: the macOS emoji picker",
    },
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "d",
        },
        kind: ReservedKind::SystemReserved,
        reason: "Reserved: the macOS dictionary lookup",
    },
    // (c) Held for later tmux-port phases (D4).
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "z",
        },
        kind: ReservedKind::FuturePhase,
        reason: "Reserved for a future Nice feature",
    },
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "v",
        },
        kind: ReservedKind::FuturePhase,
        reason: "Reserved for a future Nice feature",
    },
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "s",
        },
        kind: ReservedKind::FuturePhase,
        reason: "Reserved for a future Nice feature",
    },
    ReservedCombo {
        combo: KeyCombo {
            modifiers: Modifiers::CONTROL_COMMAND,
            key: "/",
        },
        kind: ReservedKind::FuturePhase,
        reason: "Reserved for a future Nice feature",
    },
];

/// The reserved entry claiming `combo`, or `None` when the chord is free to
/// record. Matching is the full masked `(modifiers, key)` pair, exactly like
/// [`conflicting_action`]'s comparison — a reserved key under a DIFFERENT modifier
/// set (⌘⌥D, say) is not reserved. Key tokens are compared case-insensitively
/// because a recorded keystroke's token casing is the platform's business, not
/// this table's.
pub fn reserved_combo(combo: &OwnedCombo) -> Option<ReservedCombo> {
    RESERVED_COMBOS.into_iter().find(|r| {
        r.combo.modifiers == combo.modifiers && r.combo.key.eq_ignore_ascii_case(&combo.key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_is_complete_every_action_bound_exactly_once() {
        let table = default_bindings();
        assert_eq!(table.len(), 22, "22 rebindable actions");
        assert_eq!(
            ShortcutAction::ALL.len(),
            22,
            "ALL enumerates all 22 actions"
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
        assert_eq!(combo(ShortcutAction::NextSidebarSession), "cmd-alt-down");
        assert_eq!(combo(ShortcutAction::PrevSidebarSession), "cmd-alt-up");
        // D1: prev/next pill moved onto the held-⌃⌘ pair.
        assert_eq!(combo(ShortcutAction::NextWindow), "cmd-ctrl-]");
        assert_eq!(combo(ShortcutAction::PrevWindow), "cmd-ctrl-[");
        assert_eq!(combo(ShortcutAction::NewTerminalWindow), "cmd-t");
        assert_eq!(combo(ShortcutAction::ToggleSidebar), "cmd-b");
        assert_eq!(combo(ShortcutAction::ToggleSidebarMode), "cmd-shift-b");
        assert_eq!(combo(ShortcutAction::ToggleHiddenFiles), "cmd-shift-.");
        assert_eq!(combo(ShortcutAction::IncreaseFontSize), "cmd-=");
        assert_eq!(combo(ShortcutAction::DecreaseFontSize), "cmd--");
        assert_eq!(combo(ShortcutAction::ResetFontSizes), "cmd-0");
        assert_eq!(combo(ShortcutAction::UndoFileOperation), "cmd-z");
        assert_eq!(combo(ShortcutAction::RedoFileOperation), "cmd-shift-z");
        assert_eq!(combo(ShortcutAction::CommandCompose), "cmd-enter");
        // Phase 1's held-⌃⌘ scheme.
        assert_eq!(combo(ShortcutAction::FocusPaneLeft), "cmd-ctrl-h");
        assert_eq!(combo(ShortcutAction::FocusPaneDown), "cmd-ctrl-j");
        assert_eq!(combo(ShortcutAction::FocusPaneUp), "cmd-ctrl-k");
        assert_eq!(combo(ShortcutAction::FocusPaneRight), "cmd-ctrl-l");
        assert_eq!(combo(ShortcutAction::LastActiveWindow), "cmd-ctrl-o");
        assert_eq!(combo(ShortcutAction::ScrollHalfPageUp), "cmd-ctrl-u");
        assert_eq!(combo(ShortcutAction::ScrollHalfPageDown), "cmd-ctrl-d");
        // D2: the template row stores the normalized digit.
        assert_eq!(combo(ShortcutAction::WindowByIndex), "cmd-ctrl-1");
    }

    /// The ⓘ tooltip contract: exactly `CommandCompose` and `WindowByIndex` carry
    /// info text (neither label explains itself); every other label stands alone.
    #[test]
    fn info_is_some_only_for_the_two_documented_actions() {
        for action in ShortcutAction::ALL {
            match action {
                ShortcutAction::CommandCompose | ShortcutAction::WindowByIndex => {
                    let info = action.info().expect("has info text");
                    assert!(!info.is_empty());
                }
                _ => assert_eq!(action.info(), None, "{action:?} needs no tooltip"),
            }
        }
    }

    #[test]
    fn every_action_has_a_nonempty_label() {
        for action in ShortcutAction::ALL {
            assert!(!action.label().is_empty(), "{action:?} has a label");
        }
    }

    #[test]
    fn action_ids_round_trip_and_are_distinct() {
        // Every id maps back to its action, and the ids are unique — the JSON
        // key set the `shortcuts` persistence section is keyed by.
        let mut ids = HashSet::new();
        for action in ShortcutAction::ALL {
            let id = action.id();
            assert!(!id.is_empty(), "{action:?} has an id");
            assert!(ids.insert(id), "id {id:?} is not unique");
            assert_eq!(ShortcutAction::from_id(id), Some(action));
        }
        // A spot-check of the Swift rawValues (KeyboardShortcuts.swift:37-70).
        assert_eq!(ShortcutAction::NewTerminalWindow.id(), "newTerminalPane");
        assert_eq!(ShortcutAction::UndoFileOperation.id(), "undoFileOperation");
        // Phase 1's additive ids — `windowByIndex` is the one the store note
        // pins by name (absent from every existing `ui_settings.json`).
        assert_eq!(ShortcutAction::WindowByIndex.id(), "windowByIndex");
        assert_eq!(ShortcutAction::LastActiveWindow.id(), "lastActiveWindow");
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
            conflicting_action(view(), &free, ShortcutAction::NewTerminalWindow),
            None
        );

        // `cmd-t` is NewTerminalWindow's default. Asking on behalf of a DIFFERENT
        // action (ToggleSidebar) finds the holder.
        let cmd_t = OwnedCombo::from_token("cmd-t").unwrap();
        assert_eq!(
            conflicting_action(view(), &cmd_t, ShortcutAction::ToggleSidebar),
            Some(ShortcutAction::NewTerminalWindow)
        );

        // Re-saving NewTerminalWindow's own combo, excluding itself, is not a
        // self-conflict.
        assert_eq!(
            conflicting_action(view(), &cmd_t, ShortcutAction::NewTerminalWindow),
            None
        );

        // An unbound action never conflicts: drop NewTerminalWindow's binding, then
        // `cmd-t` is free.
        let mut cleared = bindings.clone();
        for (a, c) in cleared.iter_mut() {
            if *a == ShortcutAction::NewTerminalWindow {
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

        // NewTerminalWindow holds plain `cmd-t`. `cmd-shift-t` shares the key but not
        // the modifier set — no conflict.
        let cmd_shift_t = OwnedCombo::from_token("cmd-shift-t").unwrap();
        assert_eq!(
            conflicting_action(view(), &cmd_shift_t, ShortcutAction::ToggleSidebar),
            None
        );
    }

    /// The live default map as a `(action, Some(combo))` list — the shape
    /// `conflicting_action` consumes.
    fn default_binding_list() -> Vec<(ShortcutAction, Option<OwnedCombo>)> {
        default_bindings()
            .into_iter()
            .map(|(a, c)| (a, Some(OwnedCombo::from(c))))
            .collect()
    }

    /// D2, direction 1: recording ANY `⌃⌘<digit>` on another action collides with
    /// the single `WindowByIndex` row, even though that row stores only digit `1`.
    #[test]
    fn conflicting_action_expands_window_by_index_digits_when_recording_elsewhere() {
        let bindings = default_binding_list();
        let view = || bindings.iter().map(|(a, c)| (*a, c.as_ref()));

        for key in WINDOW_INDEX_KEYS {
            let combo = OwnedCombo::from_token(&format!("cmd-ctrl-{key}")).unwrap();
            assert_eq!(
                conflicting_action(view(), &combo, ShortcutAction::ToggleSidebar),
                Some(ShortcutAction::WindowByIndex),
                "⌃⌘{key} must report the Window 1-9 row as the holder"
            );
        }
        // A digit outside 1-9 is NOT claimed — ⌃⌘0 is free.
        let zero = OwnedCombo::from_token("cmd-ctrl-0").unwrap();
        assert_eq!(
            conflicting_action(view(), &zero, ShortcutAction::ToggleSidebar),
            None,
            "⌃⌘0 is outside the 1-9 range the row claims"
        );
        // And a different modifier set over a claimed digit is free.
        let other_mods = OwnedCombo::from_token("cmd-alt-3").unwrap();
        assert_eq!(
            conflicting_action(view(), &other_mods, ShortcutAction::ToggleSidebar),
            None,
            "the claim is modifier-scoped"
        );
    }

    /// D2, direction 2: recording `WindowByIndex` onto a modifier set that already
    /// holds a digit-keyed binding conflicts with that action.
    #[test]
    fn conflicting_action_expands_window_by_index_digits_when_recording_the_row() {
        // ResetFontSizes holds ⌘0 by default — outside 1-9, so re-point a row we
        // can control: bind ToggleSidebar to ⌘4 and record WindowByIndex on ⌘1.
        let mut bindings = default_binding_list();
        for (a, c) in bindings.iter_mut() {
            if *a == ShortcutAction::ToggleSidebar {
                *c = OwnedCombo::from_token("cmd-4");
            }
        }
        let view = || bindings.iter().map(|(a, c)| (*a, c.as_ref()));

        let recorded = OwnedCombo::from_token("cmd-1").unwrap();
        assert_eq!(
            conflicting_action(view(), &recorded, ShortcutAction::WindowByIndex),
            Some(ShortcutAction::ToggleSidebar),
            "recording the row on ⌘1 must see the ⌘4 holder — the row claims all nine"
        );

        // ⌘0 does not collide (outside the claimed range) even though the recorded
        // row would normalize a digit — the claim only covers 1-9.
        let mut zero_bindings = default_binding_list();
        for (a, c) in zero_bindings.iter_mut() {
            if *a == ShortcutAction::ToggleSidebar {
                *c = OwnedCombo::from_token("cmd-alt-0");
            }
        }
        let zero_view = zero_bindings.iter().map(|(a, c)| (*a, c.as_ref()));
        let recorded = OwnedCombo::from_token("cmd-alt-1").unwrap();
        assert_eq!(
            conflicting_action(zero_view, &recorded, ShortcutAction::WindowByIndex),
            None
        );
    }

    /// The default board is self-consistent: every default combo, re-recorded on its
    /// own action, reports no conflict. This is what pins the D1 flip and the eight
    /// new ⌃⌘ rows against each other AND against the `WindowByIndex` digit claim.
    #[test]
    fn no_default_combo_conflicts_with_another_default() {
        let bindings = default_binding_list();
        for (action, combo) in default_bindings() {
            let owned = OwnedCombo::from(combo);
            let view = bindings.iter().map(|(a, c)| (*a, c.as_ref()));
            assert_eq!(
                conflicting_action(view, &owned, action),
                None,
                "{action:?}'s default {owned:?} collides with another default"
            );
        }
    }

    /// A `WindowByIndex` combo whose stored key is not a digit (only reachable by
    /// hand-editing `ui_settings.json`) falls back to plain equality — it must not
    /// swallow the whole digit range.
    #[test]
    fn window_by_index_with_a_non_digit_key_claims_only_that_key() {
        let bindings: Vec<(ShortcutAction, Option<OwnedCombo>)> =
            vec![(ShortcutAction::WindowByIndex, OwnedCombo::from_token("cmd-ctrl-q"))];
        let view = || bindings.iter().map(|(a, c)| (*a, c.as_ref()));

        let digit = OwnedCombo::from_token("cmd-ctrl-3").unwrap();
        assert_eq!(conflicting_action(view(), &digit, ShortcutAction::ToggleSidebar), None);
        let same = OwnedCombo::from_token("cmd-ctrl-q").unwrap();
        assert_eq!(
            conflicting_action(view(), &same, ShortcutAction::ToggleSidebar),
            Some(ShortcutAction::WindowByIndex)
        );
    }

    // ---- The reserved-combo table (Slice 2) ---------------------------------

    /// Every reserved chord looks up to its own entry, and every entry carries a
    /// non-empty reason (the recorder prints it verbatim).
    #[test]
    fn every_reserved_chord_looks_up_to_its_entry() {
        for entry in RESERVED_COMBOS {
            let owned = OwnedCombo::from(entry.combo);
            assert_eq!(
                reserved_combo(&owned),
                Some(entry),
                "{:?} must look up to its own entry",
                owned.to_token()
            );
            assert!(!entry.reason.is_empty(), "{owned:?} needs a reason");
        }
    }

    /// All three groups are represented, with the counts the plan names: five
    /// fixed Nice accelerators, three macOS chords, four future-phase chords.
    #[test]
    fn reserved_table_covers_the_three_groups() {
        let count = |kind| RESERVED_COMBOS.iter().filter(|r| r.kind == kind).count();
        assert_eq!(count(ReservedKind::FixedAccelerator), 5, "⌘Q ⌘N ⌘W ⌘, ⌃⌘F");
        assert_eq!(count(ReservedKind::SystemReserved), 3, "⌃⌘Q ⌃⌘Space ⌃⌘D");
        assert_eq!(count(ReservedKind::FuturePhase), 4, "⌃⌘Z ⌃⌘V ⌃⌘S ⌃⌘/");

        // The exact chord spellings, by token.
        let tokens: HashSet<String> = RESERVED_COMBOS
            .iter()
            .map(|r| r.combo.chord_str())
            .collect();
        for token in [
            "cmd-q",
            "cmd-n",
            "cmd-w",
            "cmd-,",
            "cmd-ctrl-f",
            "cmd-ctrl-q",
            "cmd-ctrl-space",
            "cmd-ctrl-d",
            "cmd-ctrl-z",
            "cmd-ctrl-v",
            "cmd-ctrl-s",
            "cmd-ctrl-/",
        ] {
            assert!(tokens.contains(token), "{token} must be reserved");
        }
        assert_eq!(tokens.len(), RESERVED_COMBOS.len(), "no duplicate entries");
    }

    /// The claim is modifier-scoped and key-scoped: a free chord is `None`, and a
    /// reserved key under other modifiers stays free.
    #[test]
    fn reserved_lookup_is_scoped_to_the_full_combo() {
        assert_eq!(reserved_combo(&OwnedCombo::from_token("cmd-y").unwrap()), None);
        // ⌘⌥D is not the dictionary chord; ⌘F is not Nice's full-screen chord.
        assert_eq!(reserved_combo(&OwnedCombo::from_token("cmd-alt-d").unwrap()), None);
        assert_eq!(reserved_combo(&OwnedCombo::from_token("cmd-f").unwrap()), None);
        // Casing of the recorded key token does not matter.
        assert_eq!(
            reserved_combo(&OwnedCombo {
                modifiers: Modifiers::COMMAND,
                key: "Q".to_string(),
            }),
            Some(RESERVED_QUIT)
        );
    }

    /// The Phase 1 defaults do not collide with the reserved table — except the
    /// one collision the plan itself specifies (⌃⌘D is both "Scroll half page
    /// down" and the macOS dictionary chord). Pinned so a future defaults edit
    /// that adds a SECOND such overlap is noticed.
    #[test]
    fn only_scroll_half_page_down_overlaps_the_reserved_table() {
        let overlapping: Vec<ShortcutAction> = default_bindings()
            .into_iter()
            .filter(|(_, c)| reserved_combo(&OwnedCombo::from(*c)).is_some())
            .map(|(a, _)| a)
            .collect();
        assert_eq!(overlapping, vec![ShortcutAction::ScrollHalfPageDown]);
    }

    /// The digit-key helpers the recorder and the keymap expansion share.
    #[test]
    fn window_index_key_helpers() {
        assert_eq!(WINDOW_INDEX_KEYS.len(), 9);
        assert_eq!(WINDOW_INDEX_KEYS[0], WINDOW_INDEX_STORED_KEY);
        assert!(is_window_index_key(WINDOW_INDEX_STORED_KEY));
        for key in WINDOW_INDEX_KEYS {
            assert!(is_window_index_key(key));
        }
        assert!(!is_window_index_key("0"), "0 is not a window index");
        assert!(!is_window_index_key("a"));
        assert!(!is_window_index_key("10"));
    }
}
