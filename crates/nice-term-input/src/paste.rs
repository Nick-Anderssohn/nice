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
///
/// Iterates to a fixed point: removing an embedded marker splices its left and
/// right neighbours together, and the spliced bytes can themselves form a new
/// marker (`ESC[20` + `ESC[201~` + `1~` fuses into `ESC[201~`) — a single pass
/// would let that reconstructed marker terminate the paste frame early. Each
/// pass that finds a marker shrinks the buffer, so the loop terminates.
fn strip_end_marker(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    while contains_subslice(&out, PASTE_END) {
        out = strip_end_marker_once(&out);
    }
    out
}

/// One left-to-right removal pass (see [`strip_end_marker`] for why a single
/// pass is not enough on its own).
fn strip_end_marker_once(data: &[u8]) -> Vec<u8> {
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

    /// The body of a wrapped paste (between the frame markers).
    fn body(out: &[u8]) -> &[u8] {
        assert!(out.starts_with(PASTE_START) && out.ends_with(PASTE_END));
        &out[PASTE_START.len()..out.len() - PASTE_END.len()]
    }

    #[test]
    fn overlap_cannot_reconstruct_end_marker() {
        // Removing the embedded marker splices "ESC[20" and "1~" into a NEW
        // end marker; a single-pass strip shipped exactly that (the classic
        // sanitizer-overlap injection). The trailing command must stay inside
        // the frame.
        let mut payload = b"\x1b[20".to_vec();
        payload.extend_from_slice(PASTE_END);
        payload.extend_from_slice(b"1~; rm -rf ~\n");
        let out = wrap_bracketed_paste(&payload, true);
        assert!(
            !contains_subslice(body(&out), PASTE_END),
            "end marker survived inside the paste frame: {:?}",
            String::from_utf8_lossy(&out)
        );
        // Pass 1 removes the literal marker (fusing a new one); pass 2 removes
        // the fused marker. What remains is inert body text.
        assert_eq!(body(&out), b"; rm -rf ~\n");
    }

    #[test]
    fn nested_overlap_needs_multiple_passes() {
        // Two stacked overlaps: pass 1 removes the two literal markers and
        // fuses a new one, pass 2 removes that. No depth of nesting may
        // survive.
        let mut payload = b"\x1b[20".to_vec();
        payload.extend_from_slice(PASTE_END);
        payload.extend_from_slice(b"\x1b[20");
        payload.extend_from_slice(PASTE_END);
        payload.extend_from_slice(b"1~");
        payload.extend_from_slice(PASTE_END);
        payload.extend_from_slice(b"1~");
        let out = wrap_bracketed_paste(&payload, true);
        assert!(
            !contains_subslice(body(&out), PASTE_END),
            "nested overlap reconstructed a live end marker: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn start_marker_and_partial_sequences_pass_through() {
        // Only the END marker terminates the frame; start markers and partial
        // end markers are legitimate bytes and must survive sanitizing.
        let mut payload = b"keep \x1b[200~ and \x1b[201".to_vec();
        payload.extend_from_slice(b" tails");
        let out = wrap_bracketed_paste(&payload, true);
        assert_eq!(body(&out), payload.as_slice());
    }
}
