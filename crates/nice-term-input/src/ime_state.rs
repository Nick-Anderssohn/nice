//! The pure marked-text (preedit) state machine for the terminal's IME path.
//!
//! No gpui / AppKit types — everything here is unit-testable without a display,
//! which is the whole reason it lives in this gpui-free crate alongside the
//! encoders (the `cargo test` matrix for the five G1 gating behaviours compiles
//! none of the gpui stack). The R5 InputHandler adapter in `nice-term-view` is a
//! thin shell over this value: it translates the platform `NSTextInputClient`
//! callbacks into these methods and reads back the ranges.
//!
//! Model (from the ime-spike `IMPLEMENTATION-PLAN.md` §Data model, productionized
//! here): a terminal has no editable document, so the document the platform text
//! system sees is **only** the preedit buffer. Committed text leaves immediately
//! (written to the pty as data), so the document length is `utf16_len(preedit)` —
//! 0 when idle.
//!
//! `NSTextInputClient` offsets are UTF-16; Rust strings are UTF-8. Every range
//! crossing the boundary goes through [`utf16_to_byte`] / [`byte_to_utf16`],
//! which snap an offset that lands inside a surrogate pair to the enclosing
//! char's end/start boundary (never panic, never split a char).
//!
//! ## The five G1 gating behaviours this state machine underwrites
//!
//! 1. **Compose/commit writes nothing to the pty until commit** — [`set_marked_text`]
//!    never returns pty bytes; only [`commit_text`] / [`unmark`] do.
//! 2. **Enter mid-composition commits and is swallowed** — a commit that ended a
//!    composition arms [`commit_swallow_armed`]; the key path reads it via
//!    [`take_commit_swallow`] so the same-cycle re-dispatched Enter sends no CR.
//! 3. **Editing the preedit stays pty-silent** — arrow/backspace edits arrive as
//!    further [`set_marked_text`] calls; still no pty bytes.
//! 4. **The candidate window anchors at the grid cursor** — [`selected_range_utf16`]
//!    is always a valid (possibly collapsed) range and [`marked_range_utf16`] is
//!    `Some` while composing, so the adapter's `bounds_for_range` never has to
//!    return `None`.
//! 5. **⌘-keybindings still fire while an IME is active but idle** — this state
//!    machine holds no key state; the adapter never swallows modifier chords.
//!
//! [`set_marked_text`]: ImeState::set_marked_text
//! [`commit_text`]: ImeState::commit_text
//! [`unmark`]: ImeState::unmark
//! [`commit_swallow_armed`]: ImeState::commit_swallow_armed
//! [`take_commit_swallow`]: ImeState::take_commit_swallow
//! [`selected_range_utf16`]: ImeState::selected_range_utf16
//! [`marked_range_utf16`]: ImeState::marked_range_utf16

use std::ops::Range;

/// The number of UTF-16 code units `s` occupies (an `NSTextInputClient` length).
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// UTF-16 offset -> UTF-8 byte offset into `s`. Offsets past the end clamp to
/// `s.len()`; an offset inside a surrogate pair snaps forward to the end of that
/// char (never splits it).
pub fn utf16_to_byte(s: &str, utf16_offset: usize) -> usize {
    let mut u16s = 0;
    for (byte_idx, ch) in s.char_indices() {
        if u16s >= utf16_offset {
            return byte_idx;
        }
        u16s += ch.len_utf16();
    }
    s.len()
}

/// UTF-8 byte offset -> UTF-16 offset into `s`. Byte offsets inside a char snap
/// back to that char's start; offsets past the end clamp.
pub fn byte_to_utf16(s: &str, byte_offset: usize) -> usize {
    let mut u16s = 0;
    for (byte_idx, ch) in s.char_indices() {
        if byte_idx >= byte_offset {
            return u16s;
        }
        u16s += ch.len_utf16();
    }
    u16s
}

/// The result of an `insertText:` commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    /// Raw UTF-8 for the pty. Committed IME text is **data**, not keys — the
    /// caller writes it to the session directly, bypassing the key encoder.
    pub pty_text: String,
    /// Whether a preedit existed when the commit arrived (i.e. this commit
    /// ended/reduced a composition rather than being plain typed text). Drives
    /// the Enter-swallow arming.
    pub was_composing: bool,
    /// The IME asked to replace document text that is not in the preedit — it
    /// already left for the pty and cannot be recalled. The caller should log
    /// this; the text is inserted regardless.
    pub unhonored_replacement: Option<Range<usize>>,
}

/// The marked-text (preedit) state machine.
#[derive(Debug, Default)]
pub struct ImeState {
    /// Current marked/composing text (UTF-8); empty = not composing.
    preedit: String,
    /// Caret/selection within the preedit, UTF-16 offsets.
    sel_utf16: Range<usize>,
    /// Set by a commit that ended a composition; read+cleared by the key-down
    /// path so an Enter re-dispatched in the SAME native key cycle as the commit
    /// is swallowed instead of sending `\r` (the zed#23003 policy).
    commit_swallow_armed: bool,
}

impl ImeState {
    /// A fresh, idle state machine (no composition).
    pub fn new() -> Self {
        ImeState {
            preedit: String::new(),
            sel_utf16: 0..0,
            commit_swallow_armed: false,
        }
    }

    /// Whether a composition is currently in progress (non-empty preedit).
    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// The current preedit (marked) text.
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// `markedRange` — `Some(whole preedit)` iff composing. This is what makes
    /// the platform arbitration route keys to the IME first while composing (and
    /// keeps `bounds_for_range` from ever needing `None`).
    pub fn marked_range_utf16(&self) -> Option<Range<usize>> {
        if self.preedit.is_empty() {
            None
        } else {
            Some(0..utf16_len(&self.preedit))
        }
    }

    /// `selectedRange` — always a valid (possibly collapsed) range; the caller
    /// must never surface `None` to the platform (some IMEs misbehave on it).
    pub fn selected_range_utf16(&self) -> Range<usize> {
        self.sel_utf16.clone()
    }

    /// `attributedSubstringForProposedRange` — clamped substring of the preedit
    /// plus the actually-used range. `None` when idle or the clamped range is
    /// empty.
    pub fn text_for_range_utf16(&self, range: Range<usize>) -> Option<(String, Range<usize>)> {
        if !self.is_composing() {
            return None;
        }
        let (lo, hi) = self.clamped_byte_range(&range);
        if lo >= hi {
            return None;
        }
        let actual = byte_to_utf16(&self.preedit, lo)..byte_to_utf16(&self.preedit, hi);
        Some((self.preedit[lo..hi].to_string(), actual))
    }

    /// `setMarkedText:selectedRange:replacementRange:` — update the preedit. **No
    /// pty write.** `new_selected_utf16` is relative to `new_text` (Apple
    /// semantics), re-based onto the preedit.
    pub fn set_marked_text(
        &mut self,
        replacement_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_utf16: Option<Range<usize>>,
    ) {
        let (lo, hi) = match replacement_utf16 {
            // A sub-range of the existing marked text is being re-marked.
            Some(range) if self.is_composing() => self.clamped_byte_range(&range),
            // Idle (doc is empty) or no range: replace the whole marked text.
            _ => (0, self.preedit.len()),
        };
        let start_utf16 = byte_to_utf16(&self.preedit, lo);
        self.preedit.replace_range(lo..hi, new_text);
        let len16 = utf16_len(&self.preedit);
        self.sel_utf16 = match new_selected_utf16 {
            Some(sel) => {
                let s = (start_utf16 + sel.start.min(sel.end)).min(len16);
                let e = (start_utf16 + sel.start.max(sel.end)).min(len16);
                s..e
            }
            None => {
                let end = (start_utf16 + utf16_len(new_text)).min(len16);
                end..end
            }
        };
    }

    /// `insertText:replacementRange:` — **commit**. Returns the UTF-8 for the pty.
    ///
    /// * `None` range (the common case): the whole composition is committed; the
    ///   preedit clears.
    /// * `Some` range within the preedit (partial commit, e.g. Japanese
    ///   clause-by-clause 確定): that part is spliced OUT of the preedit and the
    ///   rest stays marked (the IME's follow-up `setMarkedText` re-syncs it).
    /// * `Some` range outside the preedit: cannot be honored — the target text
    ///   already went to the pty. Flagged for logging; text inserted regardless.
    pub fn commit_text(
        &mut self,
        replacement_utf16: Option<Range<usize>>,
        text: &str,
    ) -> CommitOutcome {
        let was_composing = self.is_composing();
        let len16 = utf16_len(&self.preedit);
        let mut unhonored_replacement = None;

        match replacement_utf16 {
            Some(range) => {
                if range.start > len16 || range.end > len16 {
                    unhonored_replacement = Some(range.clone());
                }
                let (lo, hi) = self.clamped_byte_range(&range);
                self.preedit.replace_range(lo..hi, "");
                let caret = byte_to_utf16(&self.preedit, lo);
                self.sel_utf16 = caret..caret;
            }
            None => {
                self.preedit.clear();
                self.sel_utf16 = 0..0;
            }
        }

        if was_composing {
            self.commit_swallow_armed = true;
        }
        CommitOutcome {
            pty_text: text.to_string(),
            was_composing,
            unhonored_replacement,
        }
    }

    /// `unmarkText` — the pending composition is accepted as if typed
    /// (Terminal.app behavior: fires on focus loss / input-source switch;
    /// silently dropping it would lose user text). Returns the text to commit to
    /// the pty, if any. Does **not** arm the Enter swallow.
    pub fn unmark(&mut self) -> Option<String> {
        if self.preedit.is_empty() {
            return None;
        }
        let pending = std::mem::take(&mut self.preedit);
        self.sel_utf16 = 0..0;
        Some(pending)
    }

    /// Read+clear, called at the START of every key-down. Combined with the async
    /// disarm the adapter schedules after each commit, only an Enter re-dispatched
    /// synchronously in the same native key cycle as a composition commit ever
    /// observes `true`.
    pub fn take_commit_swallow(&mut self) -> bool {
        std::mem::take(&mut self.commit_swallow_armed)
    }

    /// End-of-native-key-cycle disarm (scheduled as a foreground task by the
    /// adapter). Prevents a commit with no same-cycle re-dispatch (e.g. Pinyin
    /// Space-commit) from swallowing a LATER bare Enter.
    pub fn disarm_commit_swallow(&mut self) {
        self.commit_swallow_armed = false;
    }

    /// Whether the Enter swallow is currently armed (diagnostics / tests).
    pub fn commit_swallow_armed(&self) -> bool {
        self.commit_swallow_armed
    }

    /// Clamp a (possibly reversed / out-of-range) UTF-16 range to the preedit and
    /// convert to a UTF-8 byte range on char boundaries.
    fn clamped_byte_range(&self, range: &Range<usize>) -> (usize, usize) {
        let len16 = utf16_len(&self.preedit);
        let lo16 = range.start.min(range.end).min(len16);
        let hi16 = range.start.max(range.end).min(len16);
        (
            utf16_to_byte(&self.preedit, lo16),
            utf16_to_byte(&self.preedit, hi16),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- UTF-16/UTF-8 helpers -------------------------------------------------

    #[test]
    fn utf16_byte_roundtrip_mixed() {
        // 'a'(1B/1u16) 'é'(2B/1u16) '你'(3B/1u16) '🎉'(4B/2u16)
        let s = "aé你🎉";
        assert_eq!(utf16_len(s), 5);
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1);
        assert_eq!(utf16_to_byte(s, 2), 3);
        assert_eq!(utf16_to_byte(s, 3), 6);
        assert_eq!(utf16_to_byte(s, 5), 10);
        assert_eq!(utf16_to_byte(s, 99), 10); // clamps
        assert_eq!(byte_to_utf16(s, 0), 0);
        assert_eq!(byte_to_utf16(s, 1), 1);
        assert_eq!(byte_to_utf16(s, 3), 2);
        assert_eq!(byte_to_utf16(s, 6), 3);
        assert_eq!(byte_to_utf16(s, 10), 5);
        assert_eq!(byte_to_utf16(s, 99), 5); // clamps
    }

    #[test]
    fn utf16_offset_inside_surrogate_pair_snaps_to_char_boundary() {
        let s = "🎉x"; // 🎉 = 2 UTF-16 units, 4 bytes
        assert_eq!(utf16_to_byte(s, 1), 4); // inside the pair: snap forward
        assert_eq!(utf16_to_byte(s, 2), 4);
        assert_eq!(utf16_to_byte(s, 3), 5);
    }

    // -- composition (G1 items 1 & 3) -----------------------------------------

    #[test]
    fn pinyin_compose_updates_preedit_without_commit() {
        let mut ime = ImeState::new();
        assert!(!ime.is_composing());
        assert_eq!(ime.marked_range_utf16(), None);
        assert_eq!(ime.selected_range_utf16(), 0..0); // collapsed, never "None"

        ime.set_marked_text(None, "n", Some(1..1));
        ime.set_marked_text(None, "ni", Some(2..2));
        ime.set_marked_text(None, "nihao", Some(5..5));
        assert!(ime.is_composing());
        assert_eq!(ime.preedit(), "nihao");
        assert_eq!(ime.marked_range_utf16(), Some(0..5));
        assert_eq!(ime.selected_range_utf16(), 5..5);
        assert!(!ime.commit_swallow_armed()); // no pty write happened
    }

    #[test]
    fn set_marked_replacement_range_replaces_subrange_and_rebases_selection() {
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "nihao", Some(5..5));
        ime.set_marked_text(Some(2..5), "HAO", Some(3..3));
        assert_eq!(ime.preedit(), "niHAO");
        assert_eq!(ime.selected_range_utf16(), 5..5);
        assert_eq!(ime.marked_range_utf16(), Some(0..5));
    }

    #[test]
    fn set_marked_out_of_range_inputs_clamp() {
        let mut ime = ImeState::new();
        ime.set_marked_text(Some(7..99), "abc", Some(50..60)); // idle: whole doc
        assert_eq!(ime.preedit(), "abc");
        assert_eq!(ime.selected_range_utf16(), 3..3); // clamped to len

        ime.set_marked_text(Some(99..1), "X", None); // reversed+overlong range
        assert_eq!(ime.preedit(), "aX");
    }

    #[test]
    fn empty_set_marked_ends_composition() {
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "ni", None);
        ime.set_marked_text(None, "", None); // e.g. Esc cancels composition
        assert!(!ime.is_composing());
        assert_eq!(ime.marked_range_utf16(), None);
        assert_eq!(ime.selected_range_utf16(), 0..0);
    }

    #[test]
    fn text_for_range_clamps_and_reports_actual_range() {
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "nihao", None);
        assert_eq!(ime.text_for_range_utf16(0..2), Some(("ni".to_string(), 0..2)));
        assert_eq!(ime.text_for_range_utf16(3..99), Some(("ao".to_string(), 3..5)));
        assert_eq!(ime.text_for_range_utf16(5..9), None); // empty after clamp
    }

    // -- commit & Enter swallow (G1 item 2) -----------------------------------

    #[test]
    fn commit_clears_preedit_and_arms_swallow() {
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "nihao", Some(5..5));
        let outcome = ime.commit_text(None, "你好");
        assert_eq!(outcome.pty_text, "你好");
        assert!(outcome.was_composing);
        assert_eq!(outcome.unhonored_replacement, None);
        assert!(!ime.is_composing());
        assert_eq!(ime.selected_range_utf16(), 0..0);

        // Same-cycle re-dispatched Enter: swallowed exactly once.
        assert!(ime.take_commit_swallow());
        assert!(!ime.take_commit_swallow());
    }

    #[test]
    fn plain_ascii_commit_does_not_arm_swallow() {
        // ABC layout 'a': insertText with no preedit — a later Enter must still
        // send \r.
        let mut ime = ImeState::new();
        let outcome = ime.commit_text(None, "a");
        assert_eq!(outcome.pty_text, "a");
        assert!(!outcome.was_composing);
        assert!(!ime.take_commit_swallow());
    }

    #[test]
    fn bare_enter_after_cycle_disarm_is_not_swallowed() {
        // Pinyin Space-commit (IME handles the space; no re-dispatch), then the
        // adapter's end-of-cycle task disarms, then a bare Enter arrives.
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "nihao", None);
        ime.commit_text(None, "你好");
        assert!(ime.commit_swallow_armed());
        ime.disarm_commit_swallow();
        assert!(!ime.take_commit_swallow()); // bare Enter sends \r
    }

    #[test]
    fn japanese_enter_commit_sequence_swallows_then_passes() {
        // handleEvent(Enter): insertText("愛") then
        // doCommandBySelector(insertNewline:) -> synchronous re-dispatch.
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "あい", Some(2..2));
        let outcome = ime.commit_text(None, "愛");
        assert_eq!(outcome.pty_text, "愛");
        assert!(ime.take_commit_swallow()); // re-dispatched Enter: swallow
        ime.disarm_commit_swallow(); // end of cycle (no-op here)
        assert!(!ime.take_commit_swallow()); // next bare Enter: send \r
    }

    #[test]
    fn partial_commit_splices_committed_clause_out_of_preedit() {
        // Japanese clause commit: insertText("最初", replacementRange 0..3) while
        // "さいしょの" (5 BMP chars = 5 UTF-16 units) is marked.
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "さいしょの", None);
        let outcome = ime.commit_text(Some(0..3), "最初");
        assert_eq!(outcome.pty_text, "最初");
        assert!(outcome.was_composing);
        assert_eq!(outcome.unhonored_replacement, None);
        assert_eq!(ime.preedit(), "ょの"); // remainder stays marked
        assert!(ime.is_composing());
        assert_eq!(ime.selected_range_utf16(), 0..0);
        assert!(ime.commit_swallow_armed());
    }

    // -- dead keys ------------------------------------------------------------

    #[test]
    fn dead_key_sequence_option_e_then_e() {
        // ⌥e: setMarkedText("´"); then e: insertText("é").
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "´", Some(0..1));
        assert!(ime.is_composing());
        assert_eq!(ime.marked_range_utf16(), Some(0..1));
        let outcome = ime.commit_text(None, "é");
        assert_eq!(outcome.pty_text, "é");
        assert!(!ime.is_composing());
    }

    // -- replacement-range edge (press-and-hold class) ------------------------

    #[test]
    fn unhonorable_replacement_range_is_flagged_and_inserts() {
        // insertText("é", replacementRange 3..4) with NO preedit: the target char
        // already went to the pty and cannot be replaced.
        let mut ime = ImeState::new();
        let outcome = ime.commit_text(Some(3..4), "é");
        assert_eq!(outcome.pty_text, "é");
        assert!(!outcome.was_composing);
        assert_eq!(outcome.unhonored_replacement, Some(3..4));
        assert!(!ime.take_commit_swallow());
    }

    // -- unmark (focus loss / input-source switch) ----------------------------

    #[test]
    fn unmark_commits_pending_preedit_without_arming_swallow() {
        let mut ime = ImeState::new();
        ime.set_marked_text(None, "nihao", None);
        assert_eq!(ime.unmark(), Some("nihao".to_string()));
        assert!(!ime.is_composing());
        assert!(!ime.take_commit_swallow());
        assert_eq!(ime.unmark(), None); // idempotent when idle
    }

    // -- surrogate pairs in the preedit ---------------------------------------

    #[test]
    fn surrogate_pair_preedit_offsets_are_utf16_correct() {
        let mut ime = ImeState::new();
        // 👨‍👩‍👦 = 👨 ZWJ 👩 ZWJ 👦 → UTF-16 len 2+1+2+1+2 = 8
        ime.set_marked_text(None, "👨\u{200d}👩\u{200d}👦", Some(0..99));
        assert_eq!(ime.marked_range_utf16(), Some(0..8));
        assert_eq!(ime.selected_range_utf16(), 0..8); // clamped to len
        let (text, actual) = ime.text_for_range_utf16(0..2).unwrap();
        assert_eq!(text, "👨");
        assert_eq!(actual, 0..2);
        // Mid-surrogate query snaps to char boundaries instead of panicking.
        let (text, actual) = ime.text_for_range_utf16(1..3).unwrap();
        assert_eq!(text, "\u{200d}");
        assert_eq!(actual, 2..3);
        let outcome = ime.commit_text(None, "👨\u{200d}👩\u{200d}👦");
        assert_eq!(outcome.pty_text, "👨\u{200d}👩\u{200d}👦");
        assert!(!ime.is_composing());
    }
}
