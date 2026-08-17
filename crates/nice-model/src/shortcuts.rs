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
/// The set is intentionally exactly these 39 — the actions Nice lets a user
/// rebind. Window-management accelerators that are *not* rebindable (New Window
/// ⌘N, Toggle Full Screen ⌃⌘F) are deliberately absent: they live as fixed menu
/// actions in `crates/nice`, not in this table.
///
/// Eight of them are the tmux-port Phase 1 held-`⌃⌘` scheme (roadmap "Phase 1 —
/// held-modifier keybind scheme"): the four `FocusPane*` directions,
/// [`LastActiveWindow`](ShortcutAction::LastActiveWindow), the two half-page
/// scrollback actions, and the single
/// [`WindowByIndex`](ShortcutAction::WindowByIndex) template row that covers all
/// nine `⌃⌘1`…`⌃⌘9` chords (D2 — one settings row, not nine).
///
/// The trailing twelve are Phase 2 (splits): the two split verbs, zoom,
/// break-pane, and the two directional rungs the ladder held in reserve.
///
/// ## The hjkl ladder (revised 2026-08-11, completed by Phase 2)
///
/// The modifier SET selects the verb and the `hjkl` key selects the direction:
/// bare `⌃⌘` navigates containers (`h`/`l` = prev/next pill, `j`/`k` = next/prev
/// sidebar session), `⌃⌘⇧` moves pane focus, `⌃⌥⌘` resizes a split and `⌃⌥⌘⇧`
/// swaps panes. Phase 1 shipped the first two rungs and held the last two in
/// [`RESERVED_COMBOS`]; Phase 2 promotes them to real actions, which is why
/// their reserved entries are gone (a chord may never be both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    /// Cycle to the next sidebar session — `j`, down the sidebar list (⌃⌘J since
    /// the hjkl-ladder revision; the old ⌘⌥↓ default is freed).
    NextSidebarSession,
    /// Cycle to the previous sidebar session — `k`, up the list (⌃⌘K; ⌘⌥↑ freed).
    PrevSidebarSession,
    /// Focus the next window in the active session (⌃⌘L since the hjkl-ladder
    /// revision; Phase 1 shipped ⌃⌘], which the revision frees).
    NextWindow,
    /// Focus the previous window in the active session (⌃⌘H; ⌃⌘[ freed).
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
    /// Command Compose (⌘↩): rewrite the plain-English text in the shell's line
    /// buffer into a real command via `claude -p`. Fires only at an idle
    /// interactive prompt — the Nice-side gate and the shell-side handler (zsh's
    /// ZLE widget, bash's `bind -x`) live in `crates/nice`.
    /// The first Rust-only action: it has no Swift-prod `rawValue` counterpart.
    CommandCompose,
    /// Move focus one pane to the LEFT (⌃⌘⇧H) — tmux `select-pane -L` over the
    /// future split tree (D3). Bound but INERT until Phase 2 lands splits: the
    /// hjkl-ladder revision moved the whole family onto `⌃⌘⇧`, so it no longer
    /// aliases "previous window" (bare `⌃⌘H` owns that).
    FocusPaneLeft,
    /// Move focus one pane DOWN (⌃⌘⇧J) — tmux `select-pane -D` (D3). Bound but
    /// inert pre-splits (nothing is stacked vertically yet).
    FocusPaneDown,
    /// Move focus one pane UP (⌃⌘⇧K) — tmux `select-pane -U` (D3). Bound but
    /// inert pre-splits.
    FocusPaneUp,
    /// Move focus one pane to the RIGHT (⌃⌘⇧L) — tmux `select-pane -R` (D3).
    /// Bound but inert pre-splits.
    FocusPaneRight,
    /// Jump back to the window that was active before the current one (⌃⌘O) —
    /// tmux `last-window`. A single "previous" slot, not an MRU stack.
    LastActiveWindow,
    /// Scroll the active window's viewport half a screen toward history (⌃⌘↑) —
    /// tmux copy-mode `halfpage-up`. Was ⌃⌘U until 2026-08-11, when the pair
    /// moved to the arrows (see [`ScrollHalfPageDown`](Self::ScrollHalfPageDown));
    /// ⌃⌘U is now bound to nothing.
    ScrollHalfPageUp,
    /// Scroll the active window's viewport half a screen toward the bottom
    /// (⌃⌘↓) — tmux copy-mode `halfpage-down`.
    ///
    /// **Why the arrows.** This shipped on ⌃⌘D, which macOS's dictionary-lookup
    /// hotkey swallows before the keydown ever reaches the app — the chord did
    /// nothing in hand-testing while the self-test scenario passed, because
    /// injected keystrokes enter downstream of the OS intercept. Both halves
    /// moved together to keep the pair symmetric.
    ScrollHalfPageDown,
    /// Focus the Nth window of the active session (⌃⌘1…⌃⌘9) — tmux
    /// `select-window -t N`. **One** action covers all nine chords (D2): the
    /// stored combo carries the modifier set plus a normalized digit
    /// ([`WINDOW_INDEX_STORED_KEY`]), and the keymap expands it into nine
    /// bindings over [`WINDOW_INDEX_KEYS`].
    WindowByIndex,
    // -- Phase 2 (tmux port): splits ----------------------------------------
    /// Split the focused pane so the new one sits BELOW it (⌃⌘-) — tmux
    /// `split-window -v`. D2: the chord is the divider's own glyph (`-` reads
    /// like the rule the split draws), and the words "vertical"/"horizontal"
    /// appear nowhere, because vim and tmux assign them opposite meanings.
    SplitDown,
    /// Split the focused pane so the new one sits to its RIGHT (⌃⌘\) — tmux
    /// `split-window -h`. D2 again: `\` is the `|` key, and `|` is what the
    /// divider looks like.
    SplitRight,
    /// Toggle "only the focused pane is painted, full size" (⌃⌘Z) — tmux
    /// `resize-pane -Z`. Every pane's pty stays live underneath, and any
    /// structural or focus change un-zooms first (P4).
    ZoomPane,
    /// Move the focused SHELL pane out into a pill of its own (⌃⌘B) — tmux
    /// `break-pane`. Refused on the Claude pane and on a single-pane pill (P3).
    BreakPane,
    /// Walk the nearest enclosing side-by-side split's divider LEFT (⌃⌥⌘H) —
    /// tmux `resize-pane -L`. The ladder's resize rung; Phase 1 held it in
    /// [`RESERVED_COMBOS`] for exactly this.
    ResizePaneLeft,
    /// Walk the nearest enclosing stacked split's divider DOWN (⌃⌥⌘J) — tmux
    /// `resize-pane -D`.
    ResizePaneDown,
    /// Walk the nearest enclosing stacked split's divider UP (⌃⌥⌘K) — tmux
    /// `resize-pane -U`.
    ResizePaneUp,
    /// Walk the nearest enclosing side-by-side split's divider RIGHT (⌃⌥⌘L) —
    /// tmux `resize-pane -R`.
    ResizePaneRight,
    /// Trade places with the pane to the LEFT (⌃⌥⌘⇧H) — tmux `swap-pane`.
    /// Payloads swap, structure and ratios stay put, and focus follows the
    /// content (P8).
    SwapPaneLeft,
    /// Trade places with the pane BELOW (⌃⌥⌘⇧J) — tmux `swap-pane` (P8).
    SwapPaneDown,
    /// Trade places with the pane ABOVE (⌃⌥⌘⇧K) — tmux `swap-pane` (P8).
    SwapPaneUp,
    /// Trade places with the pane to the RIGHT (⌃⌥⌘⇧L) — tmux `swap-pane` (P8).
    SwapPaneRight,
    // -- Phase 3 (tmux port): copy mode + scrollback search ------------------
    /// Enter / leave the focused pane's copy mode (⌃⌘C) — tmux `copy-mode`
    /// (D2). "c = Copy Mode"; it sits beside the other directionless pane verbs
    /// on the bare ⌃⌘ rung. Copy mode IS the terminal's vi mode (P1), so the
    /// action is a straight toggle on the pane's handle.
    CopyMode,
    /// Open the focused pane's scrollback search field (⌃⌘/) — tmux
    /// `copy-mode` + `search-backward` (D1/P7). Searching enters copy mode
    /// first: confirming a query lands the keyboard cursor on the match with
    /// the mode still on, so `n`/`N`, the motions and `v`/`y` all work from
    /// there. The default direction is BACKWARD — the flagship "find what
    /// scrolled past" direction.
    SearchScrollback,
    // -- Phase 4 (tmux port): detach / adopt ---------------------------------
    /// Detach the active session out of its window into the app-global pool
    /// (⌃⌘⇧D) — tmux `detach-client` for one session. "D = Detach", placed on
    /// the ⌃⌘⇧ rung because BARE ⌃⌘D never reaches the app: macOS's
    /// dictionary-lookup hotkey swallows that keydown (the same OS intercept
    /// that moved [`ScrollHalfPageDown`](Self::ScrollHalfPageDown) onto the
    /// arrows). The session keeps running with no window; adopting it back is
    /// the pool's job.
    DetachSession,
    /// Adopt the most recently detached session out of the app-global pool into
    /// the active window (⌃⌘A) — tmux `attach-session` aimed at the pool head.
    /// "A = Attach", free on the bare ⌃⌘ rung. A no-op on an empty pool.
    AdoptDetachedSession,
    /// Move the focused pane into an OS window of its own (⌃⌘N) — tmux
    /// `break-pane -d` + `move-window`. "N = New window", pairing with ⌘N on the
    /// ⌃⌘ rung; the reservations are exact-combo, so ⌘N's does not cover it.
    /// Refuses the Claude pane exactly as [`BreakPane`](Self::BreakPane) does.
    TearOffPane,
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
    pub const ALL: [ShortcutAction; 39] = [
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
        // Phase 2 (splits).
        ShortcutAction::SplitDown,
        ShortcutAction::SplitRight,
        ShortcutAction::ZoomPane,
        ShortcutAction::BreakPane,
        ShortcutAction::ResizePaneLeft,
        ShortcutAction::ResizePaneDown,
        ShortcutAction::ResizePaneUp,
        ShortcutAction::ResizePaneRight,
        ShortcutAction::SwapPaneLeft,
        ShortcutAction::SwapPaneDown,
        ShortcutAction::SwapPaneUp,
        ShortcutAction::SwapPaneRight,
        // Phase 3 (copy mode + search).
        ShortcutAction::CopyMode,
        ShortcutAction::SearchScrollback,
        // Phase 4 (detach / adopt / tear-off).
        ShortcutAction::DetachSession,
        ShortcutAction::AdoptDetachedSession,
        ShortcutAction::TearOffPane,
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
            // Phase 2 (splits). D2 fixes the two split labels verbatim — "Split
            // Down" / "Split Right", never "vertical"/"horizontal" — and the
            // rest of the pane family follows that capitalization so the block
            // reads as one family in the recorder.
            ShortcutAction::SplitDown => "Split Down",
            ShortcutAction::SplitRight => "Split Right",
            ShortcutAction::ZoomPane => "Zoom Pane",
            ShortcutAction::BreakPane => "Break Pane to Window",
            ShortcutAction::ResizePaneLeft => "Resize Pane Left",
            ShortcutAction::ResizePaneDown => "Resize Pane Down",
            ShortcutAction::ResizePaneUp => "Resize Pane Up",
            ShortcutAction::ResizePaneRight => "Resize Pane Right",
            ShortcutAction::SwapPaneLeft => "Swap Pane Left",
            ShortcutAction::SwapPaneDown => "Swap Pane Down",
            ShortcutAction::SwapPaneUp => "Swap Pane Up",
            ShortcutAction::SwapPaneRight => "Swap Pane Right",
            // Phase 3 (copy mode + search) — same pane-family capitalization.
            ShortcutAction::CopyMode => "Copy Mode",
            ShortcutAction::SearchScrollback => "Search Scrollback",
            // Phase 4 (detach / adopt) — same pane-family capitalization.
            ShortcutAction::DetachSession => "Detach Session",
            ShortcutAction::AdoptDetachedSession => "Adopt Detached Session",
            ShortcutAction::TearOffPane => "Tear Off Pane to Window",
        }
    }

    /// Explanatory tooltip text for the recorder row's ⓘ affordance, or `None`
    /// for the actions whose label is self-explanatory. Two carry one:
    /// `CommandCompose` (its label alone doesn't say what the feature does) and
    /// `WindowByIndex` (one row standing for nine chords is not guessable).
    ///
    /// Shell-neutral by construction: `info` is a `&'static str` on a plain enum
    /// in a crate that does not (and must not) depend on `nice`, so it cannot
    /// name the active shell. The Settings ▸ Claude copy, which lives in `nice`
    /// and *can* read the active profile, is the dynamic one.
    pub fn info(self) -> Option<&'static str> {
        match self {
            ShortcutAction::WindowByIndex => Some(
                "One shortcut covering nine chords: the recorded modifiers plus \
                 the digits 1 through 9 jump straight to that window in the \
                 active session. Record it with any digit — all nine rebind \
                 together.",
            ),
            ShortcutAction::CommandCompose => Some(
                "Turns plain English typed at a shell prompt into a real command \
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
            // Phase 2 (tmux port) — additive again, same load rule 5.
            ShortcutAction::SplitDown => "splitDown",
            ShortcutAction::SplitRight => "splitRight",
            ShortcutAction::ZoomPane => "zoomPane",
            ShortcutAction::BreakPane => "breakPane",
            ShortcutAction::ResizePaneLeft => "resizePaneLeft",
            ShortcutAction::ResizePaneDown => "resizePaneDown",
            ShortcutAction::ResizePaneUp => "resizePaneUp",
            ShortcutAction::ResizePaneRight => "resizePaneRight",
            ShortcutAction::SwapPaneLeft => "swapPaneLeft",
            ShortcutAction::SwapPaneDown => "swapPaneDown",
            ShortcutAction::SwapPaneUp => "swapPaneUp",
            ShortcutAction::SwapPaneRight => "swapPaneRight",
            // Phase 3 (tmux port) — additive again, same load rule 5.
            ShortcutAction::CopyMode => "copyMode",
            ShortcutAction::SearchScrollback => "searchScrollback",
            // Phase 4 (tmux port) — additive again, same load rule 5.
            ShortcutAction::DetachSession => "detachSession",
            ShortcutAction::AdoptDetachedSession => "adoptDetachedSession",
            ShortcutAction::TearOffPane => "tearOffPane",
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
    /// ⌃⌘ — the tmux-port Phase 1 held-modifier pair (D1/D2/D3), the ladder's
    /// bare "navigate containers" rung.
    pub const CONTROL_COMMAND: Modifiers = Modifiers {
        command: true,
        control: true,
        alt: false,
        shift: false,
    };
    /// ⌃⌘⇧ — the ladder's "move pane focus" rung (Phase 2's `FocusPane*`).
    pub const CONTROL_COMMAND_SHIFT: Modifiers = Modifiers {
        command: true,
        control: true,
        alt: false,
        shift: true,
    };
    /// ⌃⌥⌘ — the ladder's "resize split" rung (Phase 2's `ResizePane*`).
    pub const CONTROL_ALT_COMMAND: Modifiers = Modifiers {
        command: true,
        control: true,
        alt: true,
        shift: false,
    };
    /// ⌃⌥⌘⇧ (the "Hyper" cluster) — the ladder's "swap panes" rung (Phase 2's
    /// `SwapPane*`).
    pub const CONTROL_ALT_COMMAND_SHIFT: Modifiers = Modifiers {
        command: true,
        control: true,
        alt: true,
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
/// new window, ⌘B for the sidebar). Every action has exactly one default combo,
/// and no two actions share a combo — both pinned by this module's tests, and by
/// the keymap slice which would otherwise register a colliding binding.
/// The Phase 1 rows are the held-`⌃⌘` scheme, in its 2026-08-11 hjkl-ladder
/// spelling: bare `⌃⌘H`/`⌃⌘L` step the pill strip and `⌃⌘J`/`⌃⌘K` step the
/// sidebar sessions (so both container axes sit on one held pair), while the
/// `FocusPane*` family sits a rung up on `⌃⌘⇧`. `⌃⌘[`/`⌃⌘]` (the shipped D1
/// spelling) and `⌘⌥↑`/`⌘⌥↓` are freed — nothing binds them.
/// The Phase 2 rows finish the ladder: `⌃⌥⌘hjkl` resizes and `⌃⌥⌘⇧hjkl` swaps,
/// with the pane verbs that have no direction (`⌃⌘-`, `⌃⌘\`, `⌃⌘z`, `⌃⌘b`) on
/// the bare rung. Phase 3 adds two more of those: `⌃⌘c` (copy mode) and `⌃⌘/`
/// (search scrollback).
pub fn default_bindings() -> [(ShortcutAction, KeyCombo); 39] {
    use ShortcutAction::*;
    [
        // The ladder's bare-⌃⌘ rung, vertical axis: j = down the sidebar list
        // = next session, k = up = previous. ⌘⌥↓/↑ are freed.
        (
            NextSidebarSession,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "j",
            },
        ),
        (
            PrevSidebarSession,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "k",
            },
        ),
        // The ladder's bare-⌃⌘ rung, horizontal axis: the ONLY pill pair.
        // ⌘⌥←/→ (pre-Phase-1) and ⌃⌘[/⌃⌘] (shipped Phase 1) are both freed.
        (
            NextWindow,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "l",
            },
        ),
        (
            PrevWindow,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "h",
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
        // The ⌃⌘⇧ rung: directional PANE focus, bound but inert until Phase 2
        // lands splits (the handlers are no-ops, `crates/nice/src/keymap.rs`).
        (
            FocusPaneLeft,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND_SHIFT,
                key: "h",
            },
        ),
        (
            FocusPaneDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND_SHIFT,
                key: "j",
            },
        ),
        (
            FocusPaneUp,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND_SHIFT,
                key: "k",
            },
        ),
        (
            FocusPaneRight,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND_SHIFT,
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
        // 2026-08-11: the half-page pair moved off ⌃⌘U/⌃⌘D onto the arrows.
        // macOS's dictionary hotkey eats a real ⌃⌘D keydown before the app sees
        // it, so the shipped chord did nothing in the hand. ⌃⌘U and ⌃⌘D are now
        // bound to nothing (⌃⌘D stays RESERVED as the macOS chord it is; ⌃⌘U is
        // simply free).
        (
            ScrollHalfPageUp,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "up",
            },
        ),
        (
            ScrollHalfPageDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "down",
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
        // -- Phase 2 (tmux port): splits -------------------------------------
        // D2's divider mnemonics on the bare ⌃⌘ rung: `-` is the rule a stacked
        // split draws, `\` is the `|` key a side-by-side split looks like. Both
        // keys are already spoken for under PLAIN ⌘ (font zoom out, nothing),
        // which is why the modifier set matters and not the key alone.
        (
            SplitDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "-",
            },
        ),
        (
            SplitRight,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "\\",
            },
        ),
        // ⌃⌘Z and ⌃⌘B were held as `FuturePhase` reserved entries until this
        // phase; a chord may never be both reserved and a default, so those
        // entries are gone (the disjointness test enforces it).
        (
            ZoomPane,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "z",
            },
        ),
        (
            BreakPane,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "b",
            },
        ),
        // The ladder's top two rungs, claimed at last: ⌃⌥⌘ resizes the split,
        // ⌃⌥⌘⇧ (the "Hyper" cluster) swaps panes — same hjkl directions as the
        // rungs below them.
        (
            ResizePaneLeft,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND,
                key: "h",
            },
        ),
        (
            ResizePaneDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND,
                key: "j",
            },
        ),
        (
            ResizePaneUp,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND,
                key: "k",
            },
        ),
        (
            ResizePaneRight,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND,
                key: "l",
            },
        ),
        (
            SwapPaneLeft,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND_SHIFT,
                key: "h",
            },
        ),
        (
            SwapPaneDown,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND_SHIFT,
                key: "j",
            },
        ),
        (
            SwapPaneUp,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND_SHIFT,
                key: "k",
            },
        ),
        (
            SwapPaneRight,
            KeyCombo {
                modifiers: Modifiers::CONTROL_ALT_COMMAND_SHIFT,
                key: "l",
            },
        ),
        // -- Phase 3 (tmux port): copy mode + scrollback search --------------
        // Two more directionless pane verbs on the bare ⌃⌘ rung. `c` was free
        // there (D2); `/` was the table's last `FuturePhase` reservation, and a
        // chord may never be both reserved and a default, so that entry is gone
        // (the disjointness test enforces it).
        (
            CopyMode,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "c",
            },
        ),
        (
            SearchScrollback,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "/",
            },
        ),
        // -- Phase 4 (tmux port): detach ------------------------------------
        // "D = Detach" on the ⌃⌘⇧ rung, whose only other tenants are the four
        // pane-focus letters. BARE ⌃⌘D is unusable: macOS's dictionary hotkey
        // eats that keydown before the app sees it. The shift variant is
        // expected to survive, but injected keystrokes enter downstream of the
        // OS intercept, so only a hand press can prove it.
        (
            DetachSession,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND_SHIFT,
                key: "d",
            },
        ),
        // "A = Attach" on the BARE rung, which still has `a` free. The inverse
        // verb of the row above, so the two read as a pair in the recorder.
        (
            AdoptDetachedSession,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "a",
            },
        ),
        // "N = New window", the ⌃⌘ echo of ⌘N. `n` is free on the bare rung:
        // RESERVED_NEW_WINDOW claims the exact combo ⌘N, not the key.
        (
            TearOffPane,
            KeyCombo {
                modifiers: Modifiers::CONTROL_COMMAND,
                key: "n",
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
    /// Group (c) — held for a later tmux-port phase, so nothing can squat on
    /// the chord before that phase claims it. **Empty since Phase 3.** Phase 2
    /// emptied most of it (⌃⌘Z and ⌃⌘B became real actions, the ⌃⌥⌘hjkl /
    /// ⌃⌥⌘⇧hjkl rungs became the resize + swap families, and ⌃⌘V / ⌃⌘S were
    /// released outright), and Phase 3 spent the last one: ⌃⌘/ is now
    /// [`ShortcutAction::SearchScrollback`]. The variant stays for the next
    /// phase that needs to hold a chord.
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
/// No entry doubles as a shipped default: ⌃⌘D was both the "Scroll half page
/// down" default and the macOS dictionary chord until 2026-08-11, when the
/// half-page pair moved to ⌃⌘↑/⌃⌘↓ (macOS eats a real ⌃⌘D keydown before the app
/// sees it, so the binding never worked in the hand). It is a pure group-b entry
/// again, and the `no_default_combo_is_reserved` test pins that the table and the
/// defaults stay disjoint.
///
/// **Phase 2 shrank this table 20 → 9; Phase 3 took it to 8.** Reserving a chord
/// is a promise to claim it later, and the disjointness test makes good on that
/// promise the only way it can: an entry must be REMOVED to become a default. So
/// ⌃⌘Z (zoom), ⌃⌘B (break-pane) and the eight ⌃⌥⌘[⇧]hjkl rungs (resize + swap)
/// left this table the moment they became actions, and ⌃⌘V / ⌃⌘S left with them —
/// D2 spent the split verbs on the divider mnemonics ⌃⌘- / ⌃⌘\ instead, so those
/// two are now ordinary free chords a user may record (the same end state ⌃⌘U
/// reached). Phase 3 spent the last one: ⌃⌘/ is
/// [`ShortcutAction::SearchScrollback`]'s default, so the group-(c) block is
/// gone and what remains is only OS chords plus Nice's own fixed accelerators.
pub const RESERVED_COMBOS: [ReservedCombo; 8] = [
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
    // (c) Held for a later tmux-port phase — EMPTY since Phase 3 claimed ⌃⌘/
    // for `SearchScrollback`. The next phase that needs to hold a chord adds
    // its entry back here.
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

    /// No shortcut string names a shell. Nice runs whichever shell the user
    /// picked (Settings ▸ Advanced), and this crate cannot read that choice —
    /// so its copy must not claim one. A regression here is invisible in a
    /// screenshot but wrong for every bash user.
    #[test]
    fn no_shortcut_copy_names_a_shell() {
        for action in ShortcutAction::ALL {
            assert!(
                !action.label().contains("zsh"),
                "{action:?}'s label names zsh: {}",
                action.label()
            );
            if let Some(info) = action.info() {
                assert!(!info.contains("zsh"), "{action:?}'s info names zsh: {info}");
                assert!(!info.contains("bash"), "{action:?}'s info names bash: {info}");
            }
        }
        // The one action that carries info still explains itself.
        let compose = ShortcutAction::CommandCompose.info().unwrap();
        assert!(compose.contains("shell prompt"), "{compose}");
    }

    #[test]
    fn table_is_complete_every_action_bound_exactly_once() {
        let table = default_bindings();
        assert_eq!(table.len(), 39, "39 rebindable actions");
        assert_eq!(
            ShortcutAction::ALL.len(),
            39,
            "ALL enumerates all 39 actions"
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
        // The ladder's bare-⌃⌘ rung: j/k sessions, h/l pills.
        assert_eq!(combo(ShortcutAction::NextSidebarSession), "cmd-ctrl-j");
        assert_eq!(combo(ShortcutAction::PrevSidebarSession), "cmd-ctrl-k");
        assert_eq!(combo(ShortcutAction::NextWindow), "cmd-ctrl-l");
        assert_eq!(combo(ShortcutAction::PrevWindow), "cmd-ctrl-h");
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
        // The ⌃⌘⇧ rung: directional pane focus (bound, inert until Phase 2).
        assert_eq!(combo(ShortcutAction::FocusPaneLeft), "cmd-ctrl-shift-h");
        assert_eq!(combo(ShortcutAction::FocusPaneDown), "cmd-ctrl-shift-j");
        assert_eq!(combo(ShortcutAction::FocusPaneUp), "cmd-ctrl-shift-k");
        assert_eq!(combo(ShortcutAction::FocusPaneRight), "cmd-ctrl-shift-l");
        assert_eq!(combo(ShortcutAction::LastActiveWindow), "cmd-ctrl-o");
        // The half-page pair lives on the ARROWS (⌃⌘U/⌃⌘D are bound to nothing —
        // macOS's dictionary hotkey swallows a real ⌃⌘D before Nice sees it).
        assert_eq!(combo(ShortcutAction::ScrollHalfPageUp), "cmd-ctrl-up");
        assert_eq!(combo(ShortcutAction::ScrollHalfPageDown), "cmd-ctrl-down");
        // D2: the template row stores the normalized digit.
        assert_eq!(combo(ShortcutAction::WindowByIndex), "cmd-ctrl-1");
        // -- Phase 2 (splits) -------------------------------------------------
        // D2's divider mnemonics. `cmd-ctrl--` is the trailing-minus format the
        // token parser handles specially, so it is pinned here as well as in the
        // round-trip test.
        assert_eq!(combo(ShortcutAction::SplitDown), "cmd-ctrl--");
        assert_eq!(combo(ShortcutAction::SplitRight), "cmd-ctrl-\\");
        assert_eq!(combo(ShortcutAction::ZoomPane), "cmd-ctrl-z");
        assert_eq!(combo(ShortcutAction::BreakPane), "cmd-ctrl-b");
        // The ladder's resize rung (⌃⌥⌘) …
        assert_eq!(combo(ShortcutAction::ResizePaneLeft), "cmd-ctrl-alt-h");
        assert_eq!(combo(ShortcutAction::ResizePaneDown), "cmd-ctrl-alt-j");
        assert_eq!(combo(ShortcutAction::ResizePaneUp), "cmd-ctrl-alt-k");
        assert_eq!(combo(ShortcutAction::ResizePaneRight), "cmd-ctrl-alt-l");
        // … and its swap rung (⌃⌥⌘⇧, the Hyper cluster).
        assert_eq!(combo(ShortcutAction::SwapPaneLeft), "cmd-ctrl-alt-shift-h");
        assert_eq!(combo(ShortcutAction::SwapPaneDown), "cmd-ctrl-alt-shift-j");
        assert_eq!(combo(ShortcutAction::SwapPaneUp), "cmd-ctrl-alt-shift-k");
        assert_eq!(combo(ShortcutAction::SwapPaneRight), "cmd-ctrl-alt-shift-l");
        // -- Phase 3 (copy mode + search) -------------------------------------
        assert_eq!(combo(ShortcutAction::CopyMode), "cmd-ctrl-c");
        assert_eq!(combo(ShortcutAction::SearchScrollback), "cmd-ctrl-/");
        // -- Phase 4 (detach / adopt / tear-off) ------------------------------
        assert_eq!(combo(ShortcutAction::DetachSession), "cmd-ctrl-shift-d");
        assert_eq!(combo(ShortcutAction::AdoptDetachedSession), "cmd-ctrl-a");
        assert_eq!(combo(ShortcutAction::TearOffPane), "cmd-ctrl-n");
    }

    /// The pane family's ids, spelled out — they are persistence keys the moment
    /// this ships, so a rename after that orphans a user's binding.
    #[test]
    fn phase_two_action_ids_are_spelled_out() {
        for (action, id) in [
            (ShortcutAction::SplitDown, "splitDown"),
            (ShortcutAction::SplitRight, "splitRight"),
            (ShortcutAction::ZoomPane, "zoomPane"),
            (ShortcutAction::BreakPane, "breakPane"),
            (ShortcutAction::ResizePaneLeft, "resizePaneLeft"),
            (ShortcutAction::ResizePaneDown, "resizePaneDown"),
            (ShortcutAction::ResizePaneUp, "resizePaneUp"),
            (ShortcutAction::ResizePaneRight, "resizePaneRight"),
            (ShortcutAction::SwapPaneLeft, "swapPaneLeft"),
            (ShortcutAction::SwapPaneDown, "swapPaneDown"),
            (ShortcutAction::SwapPaneUp, "swapPaneUp"),
            (ShortcutAction::SwapPaneRight, "swapPaneRight"),
            // Phase 4's ids freeze on the same terms.
            (ShortcutAction::DetachSession, "detachSession"),
            (
                ShortcutAction::AdoptDetachedSession,
                "adoptDetachedSession",
            ),
            (ShortcutAction::TearOffPane, "tearOffPane"),
        ] {
            assert_eq!(action.id(), id);
            assert_eq!(ShortcutAction::from_id(id), Some(action));
        }
    }

    /// The Phase 3 ids, spelled out for the same reason as the pane family's:
    /// they are persistence keys from the moment this ships.
    #[test]
    fn phase_three_action_ids_are_spelled_out() {
        for (action, id) in [
            (ShortcutAction::CopyMode, "copyMode"),
            (ShortcutAction::SearchScrollback, "searchScrollback"),
        ] {
            assert_eq!(action.id(), id);
            assert_eq!(ShortcutAction::from_id(id), Some(action));
        }
    }

    /// D2's naming ban, enforced on the surface a user actually reads: no label
    /// says "vertical" or "horizontal" (vim and tmux mean opposite things by
    /// them), and the two split labels are exactly the spellings D2 fixes.
    #[test]
    fn no_label_says_vertical_or_horizontal() {
        for action in ShortcutAction::ALL {
            let label = action.label().to_ascii_lowercase();
            assert!(
                !label.contains("vertical") && !label.contains("horizontal"),
                "{action:?}'s label breaks D2: {:?}",
                action.label()
            );
        }
        assert_eq!(ShortcutAction::SplitDown.label(), "Split Down");
        assert_eq!(ShortcutAction::SplitRight.label(), "Split Right");
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

    /// Two groups are left, with the counts Phase 3 leaves behind: five fixed
    /// Nice accelerators and three macOS chords. The future-phase group is
    /// EMPTY — ⌃⌘/ was its last entry and is now `SearchScrollback`'s default.
    #[test]
    fn reserved_table_covers_the_three_groups() {
        let count = |kind| RESERVED_COMBOS.iter().filter(|r| r.kind == kind).count();
        assert_eq!(count(ReservedKind::FixedAccelerator), 5, "⌘Q ⌘N ⌘W ⌘, ⌃⌘F");
        assert_eq!(count(ReservedKind::SystemReserved), 3, "⌃⌘Q ⌃⌘Space ⌃⌘D");
        assert_eq!(
            count(ReservedKind::FuturePhase),
            0,
            "Phase 3 spent the last reservation"
        );
        assert_eq!(RESERVED_COMBOS.len(), 8, "the table is 8 entries after Phase 3");

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
        ] {
            assert!(tokens.contains(token), "{token} must be reserved");
        }
        assert_eq!(tokens.len(), RESERVED_COMBOS.len(), "no duplicate entries");
        // ⌃⌘/ is no longer reserved: it is a shipped default now, which is what
        // the doctrine above demands of a promoted reservation.
        assert!(!tokens.contains("cmd-ctrl-/"), "⌃⌘/ was promoted, not held");
    }

    /// Phase 3's promotion, from the other side: the recorder no longer refuses
    /// ⌃⌘/, and the chord belongs to `SearchScrollback`. Recording it is now the
    /// ordinary conflict path (a user can resolve that), not a refusal they
    /// cannot.
    #[test]
    fn phase_three_promoted_the_last_reserved_chord() {
        let search = OwnedCombo::from_token("cmd-ctrl-/").unwrap();
        assert_eq!(reserved_combo(&search), None, "⌃⌘/ is recordable now");
        assert_eq!(
            default_combo(ShortcutAction::SearchScrollback).map(|c| OwnedCombo::from(c)),
            Some(search)
        );
        // ⌃⌘C was never reserved — it was simply free on the bare rung (D2).
        let copy = OwnedCombo::from_token("cmd-ctrl-c").unwrap();
        assert_eq!(reserved_combo(&copy), None);
        assert_eq!(
            default_combo(ShortcutAction::CopyMode).map(|c| OwnedCombo::from(c)),
            Some(copy)
        );
    }

    /// The chords Phase 2 took off the reserved table are FREE to record now —
    /// the two it turned into actions (⌃⌘Z zoom, ⌃⌘B break-pane), the eight
    /// ladder rungs it turned into the resize + swap families, and the two D2
    /// released outright (⌃⌘V, ⌃⌘S). "Free" here means the recorder's reserved
    /// guard no longer refuses them; the ten that became actions are of course
    /// held by those actions, which is the ordinary conflict path a user can
    /// resolve, not a refusal they cannot.
    #[test]
    fn phase_two_released_its_reserved_chords() {
        for token in [
            "cmd-ctrl-z",
            "cmd-ctrl-b",
            "cmd-ctrl-v",
            "cmd-ctrl-s",
            "cmd-ctrl-alt-h",
            "cmd-ctrl-alt-j",
            "cmd-ctrl-alt-k",
            "cmd-ctrl-alt-l",
            "cmd-ctrl-alt-shift-h",
            "cmd-ctrl-alt-shift-j",
            "cmd-ctrl-alt-shift-k",
            "cmd-ctrl-alt-shift-l",
        ] {
            let combo = OwnedCombo::from_token(token).unwrap();
            assert_eq!(
                reserved_combo(&combo),
                None,
                "{token} is no longer reserved"
            );
        }
        // ⌃⌘V and ⌃⌘S are bound to NOTHING — released, not re-spent (D2 gave the
        // split verbs the divider mnemonics instead).
        for token in ["cmd-ctrl-v", "cmd-ctrl-s"] {
            let combo = OwnedCombo::from_token(token).unwrap();
            assert!(
                !default_bindings()
                    .into_iter()
                    .any(|(_, c)| OwnedCombo::from(c) == combo),
                "{token} must stay bound to nothing"
            );
        }
    }

    /// Every rung of the hjkl ladder is BOUND, and to the action its modifier
    /// set names: bare ⌃⌘ navigates containers, ⌃⌘⇧ moves pane focus, ⌃⌥⌘
    /// resizes, ⌃⌥⌘⇧ swaps. Pins the ladder's shape so a later edit can't
    /// quietly re-reserve a chord that ships as an action, or slide a direction
    /// onto the wrong rung.
    #[test]
    fn every_ladder_rung_is_bound_to_its_own_verb() {
        let holder = |modifiers, key: &str| -> Option<ShortcutAction> {
            let combo = OwnedCombo {
                modifiers,
                key: key.to_string(),
            };
            assert_eq!(
                reserved_combo(&combo),
                None,
                "{} ships as an action, so it must not also be reserved",
                combo.to_token()
            );
            default_bindings()
                .into_iter()
                .find(|(_, c)| OwnedCombo::from(*c) == combo)
                .map(|(a, _)| a)
        };
        use ShortcutAction::*;
        let rungs = [
            (
                Modifiers::CONTROL_COMMAND,
                [PrevWindow, NextSidebarSession, PrevSidebarSession, NextWindow],
            ),
            (
                Modifiers::CONTROL_COMMAND_SHIFT,
                [FocusPaneLeft, FocusPaneDown, FocusPaneUp, FocusPaneRight],
            ),
            (
                Modifiers::CONTROL_ALT_COMMAND,
                [
                    ResizePaneLeft,
                    ResizePaneDown,
                    ResizePaneUp,
                    ResizePaneRight,
                ],
            ),
            (
                Modifiers::CONTROL_ALT_COMMAND_SHIFT,
                [SwapPaneLeft, SwapPaneDown, SwapPaneUp, SwapPaneRight],
            ),
        ];
        for (modifiers, actions) in rungs {
            for (key, action) in ["h", "j", "k", "l"].into_iter().zip(actions) {
                assert_eq!(
                    holder(modifiers, key),
                    Some(action),
                    "the {key} key of this rung belongs to {action:?}"
                );
            }
        }
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

    /// NO default collides with the reserved table. The one exception the plan
    /// used to specify — ⌃⌘D as both "Scroll half page down" and the macOS
    /// dictionary chord — is gone since 2026-08-11: shipping a default a system
    /// hotkey swallows meant shipping a chord that did nothing in the hand, so the
    /// half-page pair moved to ⌃⌘↑/⌃⌘↓. A default that lands on a reserved chord
    /// is now simply a bug.
    #[test]
    fn no_default_combo_is_reserved() {
        let overlapping: Vec<(ShortcutAction, String)> = default_bindings()
            .into_iter()
            .filter(|(_, c)| reserved_combo(&OwnedCombo::from(*c)).is_some())
            .map(|(a, c)| (a, c.chord_str()))
            .collect();
        assert!(
            overlapping.is_empty(),
            "these defaults sit on reserved chords: {overlapping:?}"
        );
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
