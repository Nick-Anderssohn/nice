//! The bracketed-paste (DECSET 2004) wrap helper.
//!
//! When the application has enabled bracketed paste, pasted text is framed with
//! `ESC [ 200 ~` … `ESC [ 201 ~` so the program can tell a paste from typed
//! input. The caller decides `active` by consulting the core's
//! `bracketed_paste_active()` query (R6). When bracketed paste is off, the data
//! is written through unchanged. R7's drag-drop path reuses this same seam.

/// The paste-start marker.
pub const PASTE_START: &[u8] = b"\x1b[200~";
/// The paste-end marker.
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Wrap `data` for pasting. When `active`, frame it in the DECSET 2004 markers
/// and strip any embedded `ESC [ 201 ~` so the paste cannot be terminated early
/// (the standard guard against paste-injection); when inactive, return the data
/// unchanged.
pub fn wrap_bracketed_paste(data: &[u8], active: bool) -> Vec<u8> {
    if !active {
        return data.to_vec();
    }
    let sanitized = strip_end_marker(data);
    let mut out = Vec::with_capacity(PASTE_START.len() + sanitized.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);
    out.extend_from_slice(&sanitized);
    out.extend_from_slice(PASTE_END);
    out
}

/// Remove every occurrence of the paste-end marker from `data`.
fn strip_end_marker(data: &[u8]) -> Vec<u8> {
    if !contains_subslice(data, PASTE_END) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(PASTE_END) {
            i += PASTE_END.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_when_active() {
        let out = wrap_bracketed_paste(b"hello", true);
        assert_eq!(out, b"\x1b[200~hello\x1b[201~".to_vec());
    }

    #[test]
    fn passes_through_when_inactive() {
        let out = wrap_bracketed_paste(b"hello", false);
        assert_eq!(out, b"hello".to_vec());
    }

    #[test]
    fn empty_paste_when_active_is_bare_frame() {
        let out = wrap_bracketed_paste(b"", true);
        assert_eq!(out, b"\x1b[200~\x1b[201~".to_vec());
    }

    #[test]
    fn strips_embedded_end_marker_when_active() {
        // A paste containing the end marker must not be able to terminate early.
        let out = wrap_bracketed_paste(b"a\x1b[201~b", true);
        assert_eq!(out, b"\x1b[200~ab\x1b[201~".to_vec());
    }

    #[test]
    fn embedded_end_marker_untouched_when_inactive() {
        let out = wrap_bracketed_paste(b"a\x1b[201~b", false);
        assert_eq!(out, b"a\x1b[201~b".to_vec());
    }

    #[test]
    fn multiline_paste_preserved() {
        let out = wrap_bracketed_paste(b"line1\nline2", true);
        assert_eq!(out, b"\x1b[200~line1\nline2\x1b[201~".to_vec());
    }
}
