//! Shell quoting — a faithful port of `Sources/Nice/Process/ShellQuoting.swift`.
//!
//! Two flavors, semantics preserved byte-for-byte:
//!
//! - [`shell_single_quote`] wraps in `'…'` for splicing into a built command
//!   line (pane spawn: `zsh -ilc "exec <quoted-path> …"`).
//! - [`shell_backslash_escape`] per-character escapes for inserting a path at a
//!   live prompt or inside a bracketed-paste frame (drag-and-drop into a
//!   running pane).
//!
//! A bad encoding here silently corrupts every shell command Nice builds, so
//! the ported test table (`tests/quoting.rs`) mirrors `ShellQuotingTests.swift`
//! case-for-case.

/// Wrap `s` in single quotes, escaping embedded single quotes via the standard
/// `'\''` close-open-escape-reopen sequence. The result is safe to splice into
/// a zsh command line as one token.
///
/// Port of `shellSingleQuote` (`ShellQuoting.swift`). Empty input yields `''`.
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Backslash-escape POSIX shell metacharacters in `s` so it can be inserted at
/// a shell prompt (or inside a bracketed-paste frame reaching a paste-aware
/// TUI) and read as a single argument without re-quoting. Unlike
/// [`shell_single_quote`], empty input returns the empty string — this is the
/// encoding macOS Terminal.app, iTerm2, Ghostty, and Warp emit for
/// drag-and-drop.
///
/// Safe (passed through): `A-Z a-z 0-9 . _ / + : = @ , -` and any non-ASCII
/// codepoint. Every other printable ASCII byte is preceded by `\`. Notable
/// choices: `%` is escaped (zsh job specs at start-of-token), `~` is escaped
/// (tilde expansion), `!` is escaped (zsh history expansion), `#` is escaped
/// (zsh `interactivecomments`).
///
/// Callers must filter C0 control bytes (`< 0x20`) and DEL (`0x7f`) upstream —
/// this helper does not handle them. Port of `shellBackslashEscape`.
pub fn shell_backslash_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if shell_escape_is_safe(ch) {
            out.push(ch);
        } else {
            out.push('\\');
            out.push(ch);
        }
    }
    out
}

/// The safe punctuation set from `ShellQuoting.swift`: `._/+:=@,-`.
const SHELL_SAFE_PUNCTUATION: &[char] = &['.', '_', '/', '+', ':', '=', '@', ',', '-'];

/// Mirror of `shellEscapeIsSafe` (`ShellQuoting.swift`): any non-ASCII scalar,
/// `0-9`, `A-Z`, `a-z`, or a member of the safe punctuation set is safe.
fn shell_escape_is_safe(ch: char) -> bool {
    let v = ch as u32;
    if v >= 0x80 {
        return true; // any non-ASCII
    }
    if ('0'..='9').contains(&ch) {
        return true;
    }
    if ('A'..='Z').contains(&ch) {
        return true;
    }
    if ('a'..='z').contains(&ch) {
        return true;
    }
    SHELL_SAFE_PUNCTUATION.contains(&ch)
}
