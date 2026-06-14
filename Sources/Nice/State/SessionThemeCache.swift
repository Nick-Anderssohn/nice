//
//  SessionThemeCache.swift
//  Nice
//
//  Per-window cache of the chrome scheme/palette/accent and terminal
//  theme/font that every live `TabPtySession` should be painted with.
//  Carved out of `SessionsModel` so the theme fan-out can be reasoned
//  about — and unit-tested — independently of the pty/socket
//  plumbing that fills the rest of `SessionsModel`.
//
//  `SessionsModel` keeps thin `updateScheme` / `updateTerminalFontSize`
//  / `updateTerminalTheme` / `updateTerminalFontFamily` forwarders so
//  production callers (`AppState.init`, `AppShellHost`) still write
//  through `appState.sessions.updateX(...)`. The forwarders just
//  delegate to this cache's matching method; the cache walks
//  whichever receivers `SessionsModel` exposes through the closure
//  passed at init.
//
//  Why a closure rather than a stored array: the live receiver list
//  changes on every `makeSession` / `removePtySession`, and threading
//  add/remove notifications into the cache would couple it back to
//  `SessionsModel`'s pty-lifecycle paths. The closure indirection
//  lets the cache stay decoupled — it asks for the latest list each
//  time it fans out.
//

import AppKit
import Foundation
import SwiftUI

@MainActor
final class SessionThemeCache {
    /// Tracks the SwiftUI `ColorScheme` currently showing. New
    /// `TabPtySession`s seeded via `applyAll(to:)` pick this up.
    private(set) var scheme: ColorScheme = .dark

    /// Tracks the active chrome palette (nice | macOS | catppuccin*).
    /// `TabPtySession.applyTerminalTheme` reads this when the active
    /// terminal theme has chrome-coupled bg/fg, so a stale value
    /// would paint the terminal with the wrong light/dark variant.
    private(set) var palette: Palette = .nice

    /// Tracks the user's active accent colour. Used to paint the
    /// terminal caret when the active terminal theme leaves
    /// `cursor == nil`. Seeded with terracotta to match Tweaks'
    /// default; `updateScheme` overwrites on every call.
    private(set) var accent: NSColor = AccentPreset.terracotta.nsColor

    /// User's terminal font size. New sessions pick this up at
    /// creation; `updateTerminalFontSize` fans changes out to every
    /// live receiver.
    private(set) var terminalFontSize: CGFloat = FontSettings.defaultTerminalSize

    /// Terminal theme every live pane is currently painted with.
    /// Seeded from Nice's built-in dark default so sessions created
    /// before `updateTerminalTheme` runs still get sensible colours.
    /// `AppShellHost` calls `updateTerminalTheme` eagerly on first
    /// appear, so this only acts as a fallback.
    private(set) var terminalTheme: TerminalTheme = BuiltInTerminalThemes.niceDefaultDark

    /// User-chosen terminal font family. `nil` => default chain
    /// (SF Mono → JetBrains Mono NL → system monospaced).
    private(set) var terminalFontFamily: String? = nil

    /// Whether terminal panes use smooth scrolling. Placeholder seed (OFF — the
    /// product default) until `updateSmoothScrolling` syncs the real value from
    /// `Tweaks`; thereafter it fans changes to every live receiver.
    private(set) var smoothScrolling: Bool = false

    /// Whether Nice mirrors the active terminal theme into Claude Code
    /// (`~/.claude/themes/nice.json`) so Claude sessions Nice launches
    /// re-theme to match — live. Placeholder seed OFF (like
    /// `smoothScrolling`) so the cache never writes the theme file before
    /// it's reconciled from the persisted `Tweaks.syncClaudeTheme` at
    /// window setup (`AppShellHost.onAppear` → `updateSyncClaudeTheme`,
    /// after the scheme/theme seed) and on every toggle change
    /// (`AppShellHost.onChange`). Were it seeded ON, the onAppear
    /// scheme/theme seed would write `nice.json` for an opted-out user
    /// before the reconcile flips it off. Read in
    /// `SessionsModel.makeSession` to decide whether a spawned Claude pane
    /// gets the `--settings` theme pointer.
    private(set) var syncClaudeTheme: Bool = false

    /// Returns the current fan-out targets. Called on every
    /// `updateX`. `SessionsModel` wires this to `ptySessions.values`
    /// (its live `TabPtySession`s); tests pass a closure that
    /// returns whichever fakes they want to receive the calls.
    /// Mutable so `SessionsModel.init` can construct the cache
    /// before `self` is fully formed and then bind a `[weak self]`
    /// closure to it once initialization is complete.
    var receivers: () -> [any TabPtySessionThemeable]

    /// Persists the active terminal theme into Claude Code's custom-theme
    /// file whenever the displayed colors change. Injected so unit tests
    /// stay hermetic — the default is a no-op, and `SessionsModel` wires it
    /// to `ClaudeThemeSync.write`. Takes the resolved scheme/accent/theme
    /// rather than reading globals so it's trivially stubbable and
    /// assertable.
    var claudeThemeWriter: @MainActor (TerminalTheme, ColorScheme, NSColor) -> Void = { _, _, _ in }

    init(receivers: @escaping () -> [any TabPtySessionThemeable] = { [] }) {
        self.receivers = receivers
    }

    /// Set the chrome scheme/palette/accent triple. Used both for
    /// the initial seed (before any spawn) and for live fan-out once
    /// sessions exist.
    func updateScheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        self.scheme = scheme
        self.palette = palette
        self.accent = accent
        for receiver in receivers() {
            receiver.applyTheme(scheme, palette: palette, accent: accent)
        }
        writeClaudeThemeIfEnabled()
    }

    /// Fan a new terminal font size out to every live receiver.
    /// Called by `AppShellHost` on launch and whenever
    /// `FontSettings.terminalFontSize` changes (slider drag or
    /// Cmd+/-).
    func updateTerminalFontSize(_ size: CGFloat) {
        terminalFontSize = size
        for receiver in receivers() {
            receiver.applyTerminalFont(size: size)
        }
    }

    /// Fan out a terminal-theme change. Called by `AppShellHost`
    /// when the user picks a new theme in Settings, when the active
    /// scheme flips (sync-with-OS), or when an imported theme is
    /// removed while selected.
    func updateTerminalTheme(_ theme: TerminalTheme) {
        terminalTheme = theme
        for receiver in receivers() {
            receiver.applyTerminalTheme(theme)
        }
        writeClaudeThemeIfEnabled()
    }

    /// Fan out a terminal-font-family change. `nil` resets to the
    /// default chain defined in
    /// `TabPtySession.terminalFont(named:size:)`.
    func updateTerminalFontFamily(_ name: String?) {
        terminalFontFamily = name
        for receiver in receivers() {
            receiver.applyTerminalFontFamily(name)
        }
    }

    /// Fan out a smooth-scrolling change to every live receiver. Called
    /// by `SessionsModel.updateSmoothScrolling` when the user toggles
    /// the setting in the Advanced pane.
    func updateSmoothScrolling(_ enabled: Bool) {
        smoothScrolling = enabled
        for receiver in receivers() {
            receiver.applySmoothScrolling(enabled)
        }
    }

    /// Enable/disable mirroring Nice's theme into Claude. On enable, write
    /// the current theme immediately so the next spawn (and any session
    /// already pointed at `custom:nice`) matches. On disable, leave the
    /// existing `nice.json` in place — deleting it would make Claude's
    /// resolver fall back to a dark base for sessions still pointed at
    /// `custom:nice` (the pointer is read once at startup); newly spawned
    /// panes simply won't be handed the `--settings` flag.
    func updateSyncClaudeTheme(_ enabled: Bool) {
        syncClaudeTheme = enabled
        if enabled { writeClaudeThemeIfEnabled() }
    }

    /// Rewrite `~/.claude/themes/nice.json` from the cached scheme +
    /// accent + terminal theme when sync is on. Atomic + only-if-changed,
    /// so the scheme-flip pair (`updateScheme` then `updateTerminalTheme`,
    /// fired together) collapses to at most one real disk write — a Claude
    /// file-watcher therefore sees at most one content change.
    private func writeClaudeThemeIfEnabled() {
        guard syncClaudeTheme else { return }
        claudeThemeWriter(terminalTheme, scheme, accent)
    }

    /// Apply the full cached state to a brand-new receiver. Called
    /// from `SessionsModel.makeSession` after creating a new
    /// `TabPtySession` so it joins the population painted with the
    /// current scheme/palette/accent + terminal theme/font.
    ///
    /// Order matters: `applyTheme` must run before
    /// `applyTerminalTheme` so the receiver has its current scheme /
    /// palette cached — the Nice Default (chrome-coupled) paths in
    /// `applyTerminalTheme` derive bg / fg from those values, and
    /// reading them stale paints the terminal with the wrong
    /// light/dark variant.
    func applyAll(to receiver: any TabPtySessionThemeable) {
        receiver.applyTerminalFontFamily(terminalFontFamily)
        receiver.applyTheme(scheme, palette: palette, accent: accent)
        receiver.applyTerminalTheme(terminalTheme)
        receiver.applyTerminalFont(size: terminalFontSize)
        receiver.applySmoothScrolling(smoothScrolling)
    }
}
