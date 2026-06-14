//
//  ClaudeThemeSync.swift
//  Nice
//
//  Mirrors Nice's active terminal theme into Claude Code so a Claude
//  session launched inside Nice re-themes itself to match — live, with
//  no `/theme` and no restart.
//
//  How the two halves fit together (both verified against the shipped
//  Claude Code CLI):
//
//    1. The COLORS live in a custom-theme file at
//       `~/.claude/themes/nice.json` (honoring `$CLAUDE_CONFIG_DIR`),
//       shaped `{ name, base, overrides: { token: "#rrggbb" } }`.
//       Claude runs a file-watcher on that directory and LIVE-RELOADS a
//       running session whenever the active theme file's contents
//       change. So we keep one file, named `nice`, and rewrite its
//       contents on every Nice theme change — every Nice-launched
//       Claude repaints within a moment.
//
//    2. The POINTER that selects this theme (`"theme": "custom:nice"`)
//       is read ONCE at Claude startup, so editing it live does nothing.
//       Instead we hand each Claude we spawn a `--settings <file>` flag
//       pointing at `~/.nice/claude-theme-settings.json`, whose sole
//       contents are `{"theme":"custom:nice"}`. That `flag` settings
//       source outranks the user's `~/.claude/settings.json`, so the
//       override applies ONLY to sessions Nice launches — the user's
//       global Claude theme (e.g. Claude run in another terminal) is
//       left untouched. `TabPtySession` adds the flag; see
//       `settingsFlagPath()`.
//
//  Why a per-file marker (`_niceManaged`): the slug `nice` is short and
//  could collide with a theme a user hand-authored. Before overwriting
//  `nice.json` we check for our inert `"_niceManaged": true` key and
//  refuse to clobber a file that lacks it (mirrors
//  `ClaudeHookInstaller`'s refuse-to-clobber stance for a foreign
//  `settings.json`). Claude ignores unknown top-level keys, so the
//  marker is invisible to it.
//
//  Idempotency: every write is atomic and only-if-changed (byte-stable
//  via `[.prettyPrinted, .sortedKeys]`), so identical themes never
//  touch disk and Claude's watcher isn't woken needlessly. Failures are
//  logged and swallowed — Claude renders fine with its own theme; only
//  the sync degrades. Structurally modeled on `ClaudeHookInstaller`.
//
//  Concurrency: `Nice`, `Nice Dev`, and every window write the same
//  user-global files (like `~/.nice/nice-claude-hook.sh`). Two instances
//  with different active themes are last-writer-wins; atomic writes
//  prevent torn reads and Claude live-reloads to whatever is current.
//

import AppKit
import Foundation
import SwiftUI

enum ClaudeThemeSync {

    /// Slug for the managed theme. Drives the filename (`nice.json`) and
    /// the pointer value (`custom:nice`).
    static let slug = "nice"

    /// `name` shown in Claude's `/theme` picker.
    static let displayName = "Nice"

    /// Prefix Claude uses to reference a custom theme by slug.
    static let customThemePrefix = "custom:"

    /// Inert top-level key marking a `nice.json` as Nice-authored so we
    /// only ever overwrite our own file.
    static let managedMarker = "_niceManaged"

    // MARK: - Public entry points

    /// Write/refresh `~/.claude/themes/nice.json` from the given theme and
    /// ensure the per-session settings file exists. Call on startup and
    /// on every Nice theme change. Main-actor because it reads an
    /// `NSColor` accent; the heavy lifting is in the pure, testable
    /// `makeThemeJSON` + the URL-injectable overload below.
    @MainActor
    static func write(theme: TerminalTheme, scheme: ColorScheme, accent: NSColor) {
        write(
            theme: theme,
            scheme: scheme,
            accent: themeColor(accent),
            themesDir: defaultThemesDir(),
            settingsURL: defaultThemeSettingsURL()
        )
    }

    /// Test-friendly entry point. Production calls `write(theme:scheme:accent:)`
    /// (NSColor) which resolves the real paths; tests pass an already-sRGB
    /// accent and sandboxed URLs so they never touch the developer's real
    /// `~/.claude/`.
    static func write(
        theme: TerminalTheme,
        scheme: ColorScheme,
        accent: ThemeColor,
        themesDir: URL,
        settingsURL: URL
    ) {
        do {
            try ensureThemeFile(theme: theme, scheme: scheme, accent: accent, in: themesDir)
            try ensureSettingsFile(at: settingsURL)
        } catch {
            NSLog("ClaudeThemeSync: write failed: \(error)")
        }
    }

    /// Ensure the per-session settings file exists and return its absolute
    /// path for `claude --settings <path>`. Returns `nil` on failure so the
    /// caller can simply omit the flag and let Claude use its own theme.
    @MainActor
    static func settingsFlagPath() -> String? {
        let url = defaultThemeSettingsURL()
        do {
            try ensureSettingsFile(at: url)
            return url.path
        } catch {
            NSLog("ClaudeThemeSync: settings file failed: \(error)")
            return nil
        }
    }

    // MARK: - Theme JSON

    /// Build Claude's custom-theme dict from a Nice `TerminalTheme`.
    ///
    /// `base` flips light/dark so any token we don't override stays
    /// legible against the matching preset. `overrides` map Nice's
    /// colors onto Claude's semantic tokens. A Nice theme carries up to
    /// 20 themeable colors — `foreground`, `background`, `selection`,
    /// `cursor`, and 16 ANSI entries (normal 0–7, **bright 8–15**).
    /// `cursor` is drawn by the terminal, so it has no Claude token.
    ///
    /// Rule of thumb: normal tokens ← matching normal ANSI / accent;
    /// brighter "emphasis" tokens (`*Shimmer`, word-level diffs, bright
    /// spinners) ← the bright ANSI variant or a lightened accent; and
    /// background-tinted tokens ← a flat blend over the background
    /// (Claude tokens take no alpha). Unknown/invalid values are ignored
    /// by Claude, and any token left unmapped falls through to `base`.
    static func makeThemeJSON(
        theme: TerminalTheme,
        scheme: ColorScheme,
        accent: ThemeColor
    ) -> [String: Any] {
        let base = (scheme == .dark) ? "dark" : "light"
        var json: [String: Any] = [
            "name": displayName,
            "base": base,
            managedMarker: true,
        ]

        // ANSI palette is contractually 16 entries; guard so a malformed
        // imported theme degrades to a clean light/dark flip rather than
        // trapping.
        guard theme.ansi.count == 16 else { return json }

        let fg = theme.foreground
        let bg = theme.background
        let a = theme.ansi              // 0 blk 1 red 2 grn 3 yel 4 blu 5 mag 6 cyn 7 wht; 8–15 bright
        let acc = accent.hexString
        let accLight = lighten(accent, amount: 0.25).hexString

        var o: [String: String] = [:]

        // Text & surfaces
        o["text"] = fg.hexString
        o["inverseText"] = bg.hexString
        o["background"] = bg.hexString
        if let sel = theme.selection { o["selectionBg"] = sel.hexString }
        o["userMessageBackground"] = blend(fg, over: bg, alpha: 0.06).hexString
        o["userMessageBackgroundHover"] = blend(fg, over: bg, alpha: 0.10).hexString
        o["bashMessageBackgroundColor"] = blend(a[5], over: bg, alpha: 0.08).hexString
        o["memoryBackgroundColor"] = blend(a[4], over: bg, alpha: 0.08).hexString

        // Status
        o["error"] = a[1].hexString
        o["success"] = a[2].hexString
        o["warning"] = a[3].hexString
        o["warningShimmer"] = a[11].hexString

        // Accent family
        o["claude"] = acc
        o["autoAccept"] = acc
        o["skill"] = a[6].hexString
        o["fastMode"] = acc
        o["fastModeShimmer"] = accLight
        o["effortUltra"] = acc
        o["merged"] = a[5].hexString
        o["claudeShimmer"] = accLight
        o["clawd_body"] = acc
        o["clawd_background"] = bg.hexString
        o["briefLabelClaude"] = acc

        // Info / links / modes
        o["permission"] = a[4].hexString
        o["permissionShimmer"] = a[12].hexString
        o["suggestion"] = a[4].hexString
        o["remember"] = a[4].hexString
        o["ide"] = a[4].hexString
        o["planMode"] = a[6].hexString
        o["bashBorder"] = a[5].hexString
        o["professionalBlue"] = a[4].hexString
        o["chromeYellow"] = a[3].hexString
        o["briefLabelYou"] = a[4].hexString
        o["claudeBlue_FOR_SYSTEM_SPINNER"] = a[4].hexString
        o["claudeBlueShimmer_FOR_SYSTEM_SPINNER"] = a[12].hexString

        // Muted / chrome
        o["promptBorder"] = a[8].hexString
        o["promptBorderShimmer"] = a[15].hexString
        o["inactive"] = a[8].hexString
        o["inactiveShimmer"] = a[15].hexString
        o["subtle"] = a[8].hexString
        o["rate_limit_fill"] = a[4].hexString
        o["rate_limit_empty"] = blend(a[8], over: bg, alpha: 0.5).hexString

        // Diffs — block tints over the background, word-level on bright
        o["diffAdded"] = blend(a[2], over: bg, alpha: 0.30).hexString
        o["diffRemoved"] = blend(a[1], over: bg, alpha: 0.30).hexString
        o["diffAddedDimmed"] = blend(a[2], over: bg, alpha: 0.15).hexString
        o["diffRemovedDimmed"] = blend(a[1], over: bg, alpha: 0.15).hexString
        o["diffAddedWord"] = blend(a[10], over: bg, alpha: 0.55).hexString
        o["diffRemovedWord"] = blend(a[9], over: bg, alpha: 0.55).hexString

        // Per-agent palette (distinct hues from the ANSI set)
        o["red_FOR_SUBAGENTS_ONLY"] = a[1].hexString
        o["green_FOR_SUBAGENTS_ONLY"] = a[2].hexString
        o["yellow_FOR_SUBAGENTS_ONLY"] = a[3].hexString
        o["blue_FOR_SUBAGENTS_ONLY"] = a[4].hexString
        o["cyan_FOR_SUBAGENTS_ONLY"] = a[6].hexString
        o["purple_FOR_SUBAGENTS_ONLY"] = a[5].hexString
        o["pink_FOR_SUBAGENTS_ONLY"] = a[13].hexString
        o["orange_FOR_SUBAGENTS_ONLY"] = blend(a[1], over: a[3], alpha: 0.5).hexString

        // Rainbow gradient — best-fit onto the ~6 ANSI hues; shimmer
        // stops use the bright variants.
        o["rainbow_red"] = a[1].hexString
        o["rainbow_orange"] = blend(a[1], over: a[3], alpha: 0.5).hexString
        o["rainbow_yellow"] = a[3].hexString
        o["rainbow_green"] = a[2].hexString
        o["rainbow_blue"] = a[4].hexString
        o["rainbow_indigo"] = a[5].hexString
        o["rainbow_violet"] = a[13].hexString
        o["rainbow_red_shimmer"] = a[9].hexString
        o["rainbow_orange_shimmer"] = blend(a[9], over: a[11], alpha: 0.5).hexString
        o["rainbow_yellow_shimmer"] = a[11].hexString
        o["rainbow_green_shimmer"] = a[10].hexString
        o["rainbow_blue_shimmer"] = a[12].hexString
        o["rainbow_indigo_shimmer"] = a[13].hexString
        o["rainbow_violet_shimmer"] = a[13].hexString

        json["overrides"] = o
        return json
    }

    // MARK: - Color helpers

    /// `fg` composited over `bg` at the given alpha (0…1), opaque result.
    /// Claude tokens take no alpha, so background-tinted tokens are
    /// pre-flattened against the terminal background.
    static func blend(_ fg: ThemeColor, over bg: ThemeColor, alpha: Double) -> ThemeColor {
        func mix(_ f: UInt8, _ b: UInt8) -> UInt8 {
            let v = Double(f) * alpha + Double(b) * (1 - alpha)
            return UInt8(max(0, min(255, v.rounded())))
        }
        return ThemeColor(mix(fg.red, bg.red), mix(fg.green, bg.green), mix(fg.blue, bg.blue))
    }

    /// Blend `c` toward white by `amount` (0…1) — the "brighter ramp" for
    /// shimmer tokens whose base is the accent (which has no ANSI-bright
    /// sibling).
    static func lighten(_ c: ThemeColor, amount: Double) -> ThemeColor {
        blend(ThemeColor(255, 255, 255), over: c, alpha: amount)
    }

    /// `NSColor` → 8-bit sRGB `ThemeColor`. Used to fold the accent (a
    /// semantic AppKit color) into the same pipeline as the theme's own
    /// 8-bit colors. Main-actor-only in practice (called from `write`).
    static func themeColor(_ ns: NSColor) -> ThemeColor {
        let c = ns.usingColorSpace(.sRGB) ?? ns
        func byte(_ v: CGFloat) -> UInt8 { UInt8(max(0, min(255, (v * 255).rounded()))) }
        return ThemeColor(byte(c.redComponent), byte(c.greenComponent), byte(c.blueComponent))
    }

    // MARK: - File writers

    /// Write `nice.json` only when its bytes change, and only when an
    /// existing file is ours (carries `_niceManaged`). A foreign or
    /// unparseable file is left intact (logged), never destroyed.
    private static func ensureThemeFile(
        theme: TerminalTheme,
        scheme: ColorScheme,
        accent: ThemeColor,
        in dir: URL
    ) throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("\(slug).json")

        if let existing = try? Data(contentsOf: url), !existing.isEmpty {
            let parsed = try? JSONSerialization.jsonObject(with: existing)
            guard let dict = parsed as? [String: Any] else {
                NSLog("ClaudeThemeSync: refusing to overwrite non-JSON \(url.path)")
                return
            }
            if dict[managedMarker] as? Bool != true {
                NSLog("ClaudeThemeSync: refusing to overwrite foreign \(url.path)")
                return
            }
        }

        let json = makeThemeJSON(theme: theme, scheme: scheme, accent: accent)
        let data = try JSONSerialization.data(
            withJSONObject: json, options: [.prettyPrinted, .sortedKeys]
        )
        if data == (try? Data(contentsOf: url)) { return }
        try data.write(to: url, options: .atomic)
    }

    /// Write the per-session settings file (`{"theme":"custom:nice"}`)
    /// once; rewritten only if the contents drift. The directory is the
    /// no-space `~/.nice/` (shared with the hook script) so the path
    /// survives Claude's shell-based flag handling without quoting
    /// surprises.
    private static func ensureSettingsFile(at url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        let payload: [String: Any] = ["theme": "\(customThemePrefix)\(slug)"]
        let data = try JSONSerialization.data(
            withJSONObject: payload, options: [.prettyPrinted, .sortedKeys]
        )
        if data == (try? Data(contentsOf: url)) { return }
        try data.write(to: url, options: .atomic)
    }

    // MARK: - Default paths (production)

    /// `$CLAUDE_CONFIG_DIR/themes` if set, else `<home>/.claude/themes` —
    /// the directory Claude watches for custom themes. (Best-effort: if a
    /// user only sets `CLAUDE_CONFIG_DIR` inside their shell rc, Nice's
    /// process env won't see it; the default covers the common case.)
    static func defaultThemesDir() -> URL {
        let configBase: URL
        if let dir = ProcessInfo.processInfo.environment["CLAUDE_CONFIG_DIR"], !dir.isEmpty {
            configBase = URL(fileURLWithPath: dir, isDirectory: true)
        } else {
            configBase = homeBase().appendingPathComponent(".claude", isDirectory: true)
        }
        return configBase.appendingPathComponent("themes", isDirectory: true)
    }

    /// `<home>/.nice/claude-theme-settings.json` — the per-session
    /// `{"theme":"custom:nice"}` pointer handed to `claude --settings`.
    /// Distinct from `ClaudeHookInstaller.defaultSettingsURL()`
    /// (`~/.claude/settings.json`): this is Nice's own pointer file, not
    /// Claude's global settings. Shares the no-space `~/.nice` dotdir.
    static func defaultThemeSettingsURL() -> URL {
        homeBase().appendingPathComponent(".nice/claude-theme-settings.json")
    }

    /// Home directory for resolving `~/.claude` and `~/.nice`. Honors the
    /// `NICE_HOME_OVERRIDE` UITest seam and a runtime-redirected `$HOME`
    /// (the unit-test `TestHomeSandbox` `setenv`), falling back to
    /// `NSHomeDirectory()`. Reading `$HOME` from the process environment —
    /// rather than `NSHomeDirectory()`, which caches the user record and
    /// ignores a `setenv` — is what keeps `makeSession`'s non-injectable
    /// `settingsFlagPath()` write hermetic under the unit-test sandbox.
    /// Mirrors `SkillInstaller.homeBase()` (plus the `$HOME` tier, since
    /// this path is reached by unit tests that redirect `$HOME`).
    private static func homeBase() -> URL {
        let env = ProcessInfo.processInfo.environment
        if let override = env["NICE_HOME_OVERRIDE"], !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        if let home = env["HOME"], !home.isEmpty {
            return URL(fileURLWithPath: home, isDirectory: true)
        }
        return URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
    }
}
