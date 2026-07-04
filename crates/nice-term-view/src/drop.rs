//! Drag-drop escaped-path typing (T7) — the pure byte builder + path-safety
//! filter behind the terminal's file / image drop handler.
//!
//! A faithful port of `NiceTerminalView.performDragOperation` / `isSafePath`
//! (`Sources/Nice/Process/NiceTerminalView.swift:399-474`). Dropped POSIX paths
//! are backslash-escaped ([`shell_backslash_escape`], the same encoding
//! Terminal.app / iTerm2 / Ghostty / Warp emit for drag-drop) and space-joined
//! in drop order, then:
//!
//! * with bracketed paste **on** (the app enabled DECSET 2004) the run is framed
//!   in `ESC[200~ … ESC[201~` via the R5 [`wrap_bracketed_paste`] seam, with **no**
//!   padding — a paste-aware TUI (Claude Code, fzf) treats it as one pasted token;
//! * with it **off** the run is padded with a leading + trailing space so the path
//!   separates from surrounding prompt text, and is **not** framed.
//!
//! There is **never** a trailing newline: a drop must not auto-submit.
//!
//! The escaping + wrap are the two shared seams the plan calls for — this module
//! only owns the join / padding / filter glue, so it is table-tested here against
//! the Swift semantics (`tests` below) and the escaping table itself stays in
//! `nice-term-core`'s `tests/quoting.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use nice_term_core::shell_backslash_escape;
use nice_term_input::wrap_bracketed_paste;

/// A pasteboard image-drop provider: reads raw image data off the drag
/// pasteboard, transcodes it to a temp PNG, and returns that file's path — or
/// `None` when the drag carried no image. Injected from `crates/nice/src/platform`
/// (the sole objc2 home) so this crate stays objc2-free, exactly like
/// [`KeyCodeProbe`](crate::KeyCodeProbe). Consulted only when an
/// [`ExternalPaths`](gpui::ExternalPaths) drop carried no file URLs (the Swift
/// raw-image fallback: browser / Messages / Preview drags).
pub type ImageDropProvider = Arc<dyn Fn() -> Option<PathBuf>>;

/// Whether `path` is safe to type at the prompt: it must contain no C0 control
/// byte (`< 0x20`) or DEL (`0x7f`). macOS filenames legally contain those bytes;
/// letting one through would break out of the `ESC[200~ … ESC[201~` paste frame
/// and deliver crafted input to the TUI as if typed. Port of `isSafePath`.
///
/// This also guarantees the escaped payload never contains a raw `ESC` (0x1b),
/// so the [`wrap_bracketed_paste`] end-marker sanitizer is a no-op on drop bytes
/// and the framed output is byte-identical to the Swift naive concatenation.
pub fn is_safe_path(path: &str) -> bool {
    !path
        .chars()
        .any(|c| (c as u32) < 0x20 || (c as u32) == 0x7f)
}

/// Build the bytes a drop types at the prompt from `paths` (POSIX paths in drop
/// order), gated on `bracketed_active` (the core's `bracketed_paste_active()`
/// DECSET-2004 state). Unsafe paths ([`is_safe_path`]) are filtered out first,
/// preserving order; if nothing survives, returns `None` and the caller sends
/// nothing (Swift's `guard !paths.isEmpty`).
///
/// See the module docs for the join / padding / framing contract.
pub fn drop_bytes(paths: &[String], bracketed_active: bool) -> Option<Vec<u8>> {
    let escaped: Vec<String> = paths
        .iter()
        .filter(|p| is_safe_path(p))
        .map(|p| shell_backslash_escape(p))
        .collect();
    if escaped.is_empty() {
        return None;
    }
    let joined = escaped.join(" ");
    // Bracketed: frame the bare run (no padding). Plain: pad with a leading +
    // trailing space so it separates from surrounding prompt text. Never a
    // newline. `wrap_bracketed_paste` is the R5 seam — active ⇒ frame, inactive ⇒
    // pass through unchanged.
    let payload = if bracketed_active {
        joined
    } else {
        format!(" {joined} ")
    };
    Some(wrap_bracketed_paste(payload.as_bytes(), bracketed_active))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: build the drop bytes for owned string paths.
    fn bytes(paths: &[&str], active: bool) -> Option<Vec<u8>> {
        let owned: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        drop_bytes(&owned, active)
    }

    // -- is_safe_path ------------------------------------------------------

    #[test]
    fn safe_path_accepts_ordinary_and_unicode_paths() {
        assert!(is_safe_path("/Users/nick/file.txt"));
        assert!(is_safe_path("/Users/nick/My File (final).txt"));
        assert!(is_safe_path("/Users/nick/café 🫠.png"));
    }

    #[test]
    fn safe_path_rejects_c0_controls_and_del() {
        assert!(!is_safe_path("/tmp/a\x1bb")); // ESC — would break the paste frame
        assert!(!is_safe_path("/tmp/a\nb")); // LF
        assert!(!is_safe_path("/tmp/a\rb")); // CR
        assert!(!is_safe_path("/tmp/a\tb")); // HT
        assert!(!is_safe_path("/tmp/a\x7fb")); // DEL
    }

    // -- plain (DECSET 2004 off): space-padded, unwrapped -------------------

    #[test]
    fn plain_single_path_is_space_padded_no_wrap() {
        // `" " + escaped + " "`, no newline (NiceTerminalView.swift:421-424).
        assert_eq!(bytes(&["/Users/nick/file.txt"], false).unwrap(), b" /Users/nick/file.txt ");
    }

    #[test]
    fn plain_path_with_spaces_is_escaped() {
        assert_eq!(
            bytes(&["/Users/nick/My File.txt"], false).unwrap(),
            br#" /Users/nick/My\ File.txt "#.to_vec()
        );
    }

    #[test]
    fn plain_path_with_shell_metacharacters_is_escaped() {
        // Quotes, `$`, parens, backslash — all backslash-escaped per the Swift set.
        assert_eq!(
            bytes(&[r#"/x/it's $HOME (a\b).txt"#], false).unwrap(),
            br#" /x/it\'s\ \$HOME\ \(a\\b\).txt "#.to_vec()
        );
    }

    #[test]
    fn plain_unicode_path_passes_through_except_spaces() {
        assert_eq!(
            bytes(&["/x/café 🫠.png"], false).unwrap(),
            " /x/café\\ 🫠.png ".as_bytes().to_vec()
        );
    }

    #[test]
    fn plain_multiple_paths_space_joined_in_order() {
        // Space-joined in drop order, then the whole run space-padded.
        assert_eq!(
            bytes(&["/a/one", "/b/two three", "/c/four"], false).unwrap(),
            br#" /a/one /b/two\ three /c/four "#.to_vec()
        );
    }

    // -- bracketed (DECSET 2004 on): ESC[200~ … ESC[201~, no padding --------

    #[test]
    fn bracketed_single_path_is_framed_without_padding() {
        assert_eq!(
            bytes(&["/Users/nick/file.txt"], true).unwrap(),
            b"\x1b[200~/Users/nick/file.txt\x1b[201~".to_vec()
        );
    }

    #[test]
    fn bracketed_path_with_spaces_is_escaped_and_framed() {
        assert_eq!(
            bytes(&["/Users/nick/My File.txt"], true).unwrap(),
            b"\x1b[200~/Users/nick/My\\ File.txt\x1b[201~".to_vec()
        );
    }

    #[test]
    fn bracketed_multiple_paths_space_joined_and_framed() {
        assert_eq!(
            bytes(&["/a/one", "/b/two three"], true).unwrap(),
            b"\x1b[200~/a/one /b/two\\ three\x1b[201~".to_vec()
        );
    }

    // -- filtering / emptiness ---------------------------------------------

    #[test]
    fn unsafe_paths_are_filtered_out_keeping_the_safe_ones() {
        // The middle path holds a raw CR and is dropped; the survivors join.
        assert_eq!(
            bytes(&["/a/ok", "/b/ba\rd", "/c/ok2"], false).unwrap(),
            br#" /a/ok /c/ok2 "#.to_vec()
        );
    }

    #[test]
    fn all_unsafe_yields_none() {
        assert!(bytes(&["/a/\x1bx", "/b/\ny"], false).is_none());
    }

    #[test]
    fn empty_input_yields_none() {
        assert!(bytes(&[], false).is_none());
        assert!(bytes(&[], true).is_none());
    }

    // -- invariants --------------------------------------------------------

    #[test]
    fn never_appends_a_trailing_newline() {
        for active in [false, true] {
            let out = bytes(&["/a/one", "/b/two"], active).unwrap();
            assert!(
                !out.ends_with(b"\n") && !out.ends_with(b"\r"),
                "drop bytes must never end in a newline (active={active}): {out:?}"
            );
        }
    }

    #[test]
    fn temp_image_path_passes_through_unescaped() {
        // The raw-image fallback types a caches temp path — all-safe chars.
        let p = "/private/var/folders/ab/T/Nice/dropped-images/DEAD-BEEF.png";
        assert_eq!(
            bytes(&[p], false).unwrap(),
            format!(" {p} ").into_bytes()
        );
    }
}
