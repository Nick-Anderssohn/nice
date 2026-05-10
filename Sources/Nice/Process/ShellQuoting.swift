//
//  ShellQuoting.swift
//  Nice
//
//  Shared helpers for encoding arbitrary strings so they survive
//  untouched through a zsh command line. Two flavors:
//
//  - `shellSingleQuote` wraps in `'…'` for splicing into a built
//    command line (pane spawn: `zsh -ilc "exec <quoted-path> …"`).
//  - `shellBackslashEscape` per-character escapes for inserting
//    a path at a live prompt or inside a bracketed-paste frame
//    (drag-and-drop into a running pane).
//

import Foundation

/// Wrap `s` in single quotes, escaping embedded single quotes via the
/// standard `'\''` close-open-escape-reopen sequence. The result is
/// safe to splice into a zsh command line as one token.
func shellSingleQuote(_ s: String) -> String {
    "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
}

/// Backslash-escape POSIX shell metacharacters in `s` so it can be
/// inserted at a shell prompt (or inside a bracketed-paste frame
/// reaching a paste-aware TUI) and read as a single argument
/// without re-quoting. Unlike `shellSingleQuote`, empty input
/// returns the empty string — this is the encoding macOS
/// Terminal.app, iTerm2, Ghostty, and Warp emit for drag-and-drop.
///
/// Safe (passed through): `A-Z a-z 0-9 . _ / + : = @ , -` and any
/// non-ASCII codepoint. Every other printable ASCII byte is
/// preceded by `\`. Notable choices: `%` is escaped (zsh job specs
/// at start-of-token), `~` is escaped (tilde expansion at
/// start-of-token), `!` is escaped (zsh history expansion), `#`
/// is escaped (zsh `interactivecomments`).
///
/// Callers must filter C0 control bytes (`< 0x20`) and DEL (`0x7f`)
/// upstream — `NiceTerminalView.isSafePath` is the contract — so
/// this helper does not handle them.
func shellBackslashEscape(_ s: String) -> String {
    var out = ""
    out.reserveCapacity(s.utf8.count)
    for scalar in s.unicodeScalars {
        if shellEscapeIsSafe(scalar) {
            out.unicodeScalars.append(scalar)
        } else {
            out.append("\\")
            out.unicodeScalars.append(scalar)
        }
    }
    return out
}

private let shellSafePunctuation: Set<Unicode.Scalar> =
    Set("._/+:=@,-".unicodeScalars)

private func shellEscapeIsSafe(_ scalar: Unicode.Scalar) -> Bool {
    let v = scalar.value
    if v >= 0x80 { return true }                      // any non-ASCII
    if (0x30...0x39).contains(v) { return true }      // 0-9
    if (0x41...0x5a).contains(v) { return true }      // A-Z
    if (0x61...0x7a).contains(v) { return true }      // a-z
    return shellSafePunctuation.contains(scalar)
}
