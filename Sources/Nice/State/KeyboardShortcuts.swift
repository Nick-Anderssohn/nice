//
//  KeyboardShortcuts.swift
//  Nice
//
//  Source of truth for the user-rebindable keyboard shortcuts. Mirrors the
//  `Tweaks` pattern: an `@MainActor @Observable` class whose mutations write
//  through to `UserDefaults` immediately, and an injectable `defaults`
//  parameter on the initializer so unit tests can stand it up against an
//  isolated suite.
//
//  The model owns:
//    • `ShortcutAction` — the closed enum of user-bindable actions
//    • `KeyCombo`       — a (keyCode, modifierFlags) pair plus display glyphs
//    • `bindings`       — current map; missing actions are unbound
//    • lookup + conflict detection used by the global monitor and the
//      Settings recorder field
//
//  Bindings are persisted as one JSON-encoded blob under a single
//  `UserDefaults` key so the schema is self-contained — no per-action key
//  proliferation, easy to read/write atomically.
//
//  We compare on `keyCode` (a layout-independent physical key id), not on
//  characters, so combos work the same on a Dvorak or non-US layout as
//  they do on US-QWERTY.
//

import AppKit
import Carbon.HIToolbox
import Foundation

// MARK: - Action

/// The closed set of user-rebindable actions surfaced in the Settings ▸
/// Shortcuts pane. Adding a case here automatically extends the recorder
/// list (it iterates `allCases`) and the persistence schema (rawValue =
/// JSON key).
enum ShortcutAction: String, CaseIterable, Codable, Sendable {
    case nextSidebarTab
    case prevSidebarTab
    case nextPane
    case prevPane
    case newTerminalPane
    case toggleSidebar
    case toggleSidebarMode
    case toggleHiddenFiles
    case increaseFontSize
    case decreaseFontSize
    case resetFontSizes
    case undoFileOperation
    case redoFileOperation

    /// Human-readable label for the recorder row.
    var label: String {
        switch self {
        case .nextSidebarTab:   "Next sidebar tab"
        case .prevSidebarTab:   "Previous sidebar tab"
        case .nextPane:         "Next pane"
        case .prevPane:         "Previous pane"
        case .newTerminalPane:  "New terminal pane"
        case .toggleSidebar:    "Toggle sidebar"
        case .toggleSidebarMode: "Toggle sidebar mode"
        case .toggleHiddenFiles: "Toggle hidden files"
        case .increaseFontSize: "Increase font size"
        case .decreaseFontSize: "Decrease font size"
        case .resetFontSizes:   "Reset font size"
        case .undoFileOperation: "Undo file operation"
        case .redoFileOperation: "Redo file operation"
        }
    }
}

// MARK: - Key combo

/// A (key, modifiers) pair that the global monitor matches against
/// incoming `NSEvent` keyDowns. Modifier flags are stored as a raw `UInt`
/// already masked to `relevantModifierMask` (only ⌃ ⌥ ⇧ ⌘ — the four
/// modifiers users actually bind shortcuts on). Caps Lock, numeric keypad,
/// help, and function bits are stripped at construction so they can't
/// silently break a binding when held.
struct KeyCombo: Hashable, Codable, Sendable {
    /// Modifiers we honour for shortcut matching. Caps Lock, numeric
    /// keypad, help, and function are *not* in this set — those bits get
    /// stripped at every entry point so a binding of "⌘T" still fires
    /// when the user happens to have Caps Lock on.
    static let relevantModifierMask: NSEvent.ModifierFlags =
        [.control, .option, .shift, .command]

    let keyCode: UInt16
    let modifierFlagsRaw: UInt

    init(keyCode: UInt16, modifierFlags: NSEvent.ModifierFlags) {
        self.keyCode = keyCode
        self.modifierFlagsRaw = modifierFlags
            .intersection(Self.relevantModifierMask)
            .rawValue
    }

    var modifierFlags: NSEvent.ModifierFlags {
        NSEvent.ModifierFlags(rawValue: modifierFlagsRaw)
    }

    /// Modifier glyphs in Apple HIG order (`⌃ ⌥ ⇧ ⌘`) followed by the key
    /// glyph. Used to drive the `KeyPills` display in Settings.
    var displayPills: [String] {
        var pills: [String] = []
        let mods = modifierFlags
        if mods.contains(.control) { pills.append("⌃") }
        if mods.contains(.option)  { pills.append("⌥") }
        if mods.contains(.shift)   { pills.append("⇧") }
        if mods.contains(.command) { pills.append("⌘") }
        pills.append(Self.glyph(for: keyCode))
        return pills
    }

    /// Single-character (or short label) display glyph for a virtual key
    /// code. Falls back to `"?"` for keys we don't have an entry for —
    /// callers that need a known set should validate before storing.
    static func glyph(for keyCode: UInt16) -> String {
        Self.keyCodeGlyphs[Int(keyCode)] ?? "?"
    }

    /// Hardcoded virtual-keycode → display table for the keys we expect to
    /// bind. Sourced from `<Carbon/HIToolbox/Events.h>` (kVK_*). Letters,
    /// digits, arrows, and the few specials a user might pick.
    private static let keyCodeGlyphs: [Int: String] = [
        // Letters (kVK_ANSI_*)
        kVK_ANSI_A: "A", kVK_ANSI_S: "S", kVK_ANSI_D: "D", kVK_ANSI_F: "F",
        kVK_ANSI_H: "H", kVK_ANSI_G: "G", kVK_ANSI_Z: "Z", kVK_ANSI_X: "X",
        kVK_ANSI_C: "C", kVK_ANSI_V: "V", kVK_ANSI_B: "B", kVK_ANSI_Q: "Q",
        kVK_ANSI_W: "W", kVK_ANSI_E: "E", kVK_ANSI_R: "R", kVK_ANSI_Y: "Y",
        kVK_ANSI_T: "T", kVK_ANSI_O: "O", kVK_ANSI_U: "U", kVK_ANSI_I: "I",
        kVK_ANSI_P: "P", kVK_ANSI_L: "L", kVK_ANSI_J: "J", kVK_ANSI_K: "K",
        kVK_ANSI_N: "N", kVK_ANSI_M: "M",
        // Digits
        kVK_ANSI_0: "0", kVK_ANSI_1: "1", kVK_ANSI_2: "2", kVK_ANSI_3: "3",
        kVK_ANSI_4: "4", kVK_ANSI_5: "5", kVK_ANSI_6: "6", kVK_ANSI_7: "7",
        kVK_ANSI_8: "8", kVK_ANSI_9: "9",
        // Punctuation users sometimes bind
        kVK_ANSI_Backslash:    "\\",
        kVK_ANSI_Slash:        "/",
        kVK_ANSI_LeftBracket:  "[",
        kVK_ANSI_RightBracket: "]",
        kVK_ANSI_Comma:        ",",
        kVK_ANSI_Period:       ".",
        kVK_ANSI_Semicolon:    ";",
        kVK_ANSI_Quote:        "'",
        kVK_ANSI_Grave:        "`",
        kVK_ANSI_Minus:        "-",
        kVK_ANSI_Equal:        "=",
        // Arrows
        kVK_LeftArrow:  "←",
        kVK_RightArrow: "→",
        kVK_UpArrow:    "↑",
        kVK_DownArrow:  "↓",
        // Specials
        kVK_Return:    "↩",
        kVK_Tab:       "⇥",
        kVK_Space:     "Space",
        kVK_Delete:    "⌫",
        kVK_Escape:    "⎋",
        kVK_ForwardDelete: "⌦",
        kVK_Home:      "↖",
        kVK_End:       "↘",
        kVK_PageUp:    "⇞",
        kVK_PageDown:  "⇟",
    ]
}

// MARK: - Store

/// Observable map of bindings. Mutating with `setBinding(_:for:)` /
/// `resetToDefault(_:)` writes through to `UserDefaults` synchronously.
///
/// Invariant: `bindings[a]` is `nil` ⇔ action `a` is unbound (no key
/// combo will trigger it). The default map (`Self.defaults`) is what
/// `resetToDefault` restores.
@MainActor
@Observable
final class KeyboardShortcuts {
    static let defaultsKey = "keyboardShortcuts"

    /// Default binding map applied on first run and restored by
    /// `resetToDefault(_:)`. Built from the Option B + pure-wrap scheme
    /// the user picked: directional arrows for both axes, ⌘T for new
    /// pane, ⌘B for sidebar toggle.
    static let defaults: [ShortcutAction: KeyCombo] = [
        .nextSidebarTab:   KeyCombo(keyCode: UInt16(kVK_DownArrow),  modifierFlags: [.command, .option]),
        .prevSidebarTab:   KeyCombo(keyCode: UInt16(kVK_UpArrow),    modifierFlags: [.command, .option]),
        .nextPane:         KeyCombo(keyCode: UInt16(kVK_RightArrow), modifierFlags: [.command, .option]),
        .prevPane:         KeyCombo(keyCode: UInt16(kVK_LeftArrow),  modifierFlags: [.command, .option]),
        .newTerminalPane:  KeyCombo(keyCode: UInt16(kVK_ANSI_T),     modifierFlags: [.command]),
        .toggleSidebar:    KeyCombo(keyCode: UInt16(kVK_ANSI_B),     modifierFlags: [.command]),
        .toggleSidebarMode: KeyCombo(keyCode: UInt16(kVK_ANSI_B),    modifierFlags: [.command, .shift]),
        .toggleHiddenFiles: KeyCombo(keyCode: UInt16(kVK_ANSI_Period), modifierFlags: [.command, .shift]),
        .increaseFontSize: KeyCombo(keyCode: UInt16(kVK_ANSI_Equal), modifierFlags: [.command]),
        .decreaseFontSize: KeyCombo(keyCode: UInt16(kVK_ANSI_Minus), modifierFlags: [.command]),
        .resetFontSizes:   KeyCombo(keyCode: UInt16(kVK_ANSI_0),     modifierFlags: [.command]),
        .undoFileOperation: KeyCombo(keyCode: UInt16(kVK_ANSI_Z),    modifierFlags: [.command]),
        .redoFileOperation: KeyCombo(keyCode: UInt16(kVK_ANSI_Z),    modifierFlags: [.command, .shift]),
    ]

    /// Current map. `nil` value = action is unbound. Always reflects what
    /// the next save would write — `setBinding` updates this and the
    /// persisted blob in lock-step.
    private(set) var bindings: [ShortcutAction: KeyCombo]

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.bindings = Self.load(from: defaults)
    }

    // MARK: - Lookup

    func binding(for action: ShortcutAction) -> KeyCombo? {
        bindings[action]
    }

    /// First action whose binding matches the incoming keyDown, or `nil`
    /// if no binding fires. Modifier flags are masked to
    /// `KeyCombo.relevantModifierMask` (only ⌃⌥⇧⌘) before comparing so a
    /// stuck Caps Lock or stray numeric-keypad bit can't break a binding.
    func actionMatching(
        keyCode: UInt16,
        modifiers: NSEvent.ModifierFlags
    ) -> ShortcutAction? {
        let masked = modifiers
            .intersection(KeyCombo.relevantModifierMask)
            .rawValue
        for (action, combo) in bindings
        where combo.keyCode == keyCode && combo.modifierFlagsRaw == masked {
            return action
        }
        return nil
    }

    /// If `combo` is already bound to some other action, return that
    /// action — used by the recorder to surface a "Already used by …"
    /// warning. `excluding` lets the recorder ignore the action being
    /// re-recorded so re-saving the same combo isn't flagged as a self-
    /// conflict.
    func conflictingAction(
        for combo: KeyCombo,
        excluding action: ShortcutAction
    ) -> ShortcutAction? {
        for (other, existing) in bindings
        where other != action && existing == combo {
            return other
        }
        return nil
    }

    // MARK: - Mutation

    /// Set or clear a binding. `nil` removes the action's binding (it
    /// becomes unbound — useful if a user wants to explicitly disable a
    /// shortcut without touching another).
    func setBinding(_ combo: KeyCombo?, for action: ShortcutAction) {
        if let combo {
            bindings[action] = combo
        } else {
            bindings.removeValue(forKey: action)
        }
        persist()
    }

    /// Restore an action to its default binding. If the action has no
    /// default (shouldn't happen in current build — every case is
    /// defaulted), this clears the binding.
    func resetToDefault(_ action: ShortcutAction) {
        setBinding(Self.defaults[action], for: action)
    }

    /// True if `action`'s current binding matches its default. Used by
    /// the recorder to decide whether to show the Reset button.
    func isAtDefault(_ action: ShortcutAction) -> Bool {
        bindings[action] == Self.defaults[action]
    }

    // MARK: - Persistence

    /// Read the bindings blob from defaults, falling back to the default
    /// map on missing/corrupt data. Public-static so tests can hit the
    /// load path without instantiating the class.
    ///
    /// Actions absent from the persisted blob are loaded as unbound
    /// (`nil`). This preserves explicit `setBinding(nil, ...)` clears
    /// across launches. The trade-off: a future build that adds a new
    /// `ShortcutAction` case will leave that action unbound for upgrading
    /// users — they can rebind it from Settings.
    static func load(from defaults: UserDefaults) -> [ShortcutAction: KeyCombo] {
        guard let data = defaults.data(forKey: Self.defaultsKey) else {
            return Self.defaults
        }
        do {
            let decoded = try JSONDecoder().decode([String: KeyCombo].self, from: data)
            var out: [ShortcutAction: KeyCombo] = [:]
            for (key, combo) in decoded {
                if let action = ShortcutAction(rawValue: key) {
                    out[action] = combo
                }
                // Unknown keys (from a future schema) are dropped silently.
            }
            return out
        } catch {
            NSLog("KeyboardShortcuts: defaults blob decode failed (\(error)); using defaults")
            return Self.defaults
        }
    }

    private func persist() {
        var encodable: [String: KeyCombo] = [:]
        for (action, combo) in bindings {
            encodable[action.rawValue] = combo
        }
        do {
            let data = try JSONEncoder().encode(encodable)
            defaults.set(data, forKey: Self.defaultsKey)
        } catch {
            NSLog("KeyboardShortcuts: persist failed: \(error)")
        }
    }
}
