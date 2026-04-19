//
//  ShellQuoting.swift
//  Nice
//
//  Shared helper for wrapping arbitrary strings so they survive
//  untouched through a zsh command line — used by pane spawn
//  (`zsh -ilc "exec <quoted-path> …"`) and by the drag-and-drop
//  handler that types file paths into a running pane.
//

import Foundation

/// Wrap `s` in single quotes, escaping embedded single quotes via the
/// standard `'\''` close-open-escape-reopen sequence. The result is
/// safe to splice into a zsh command line as one token.
func shellSingleQuote(_ s: String) -> String {
    "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
}
