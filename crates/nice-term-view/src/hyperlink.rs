//! URL detection in the terminal grid — the matching half of ⌘+click / ⌘-hover
//! link handling.
//!
//! The matching is pure grid reading: given an alacritty [`Term`] and a cell,
//! find the URL under it. Beside it lives the one other pure decision over
//! match state, [`hover_changed`] — the did-the-underline-move comparison that
//! gates hover repaints. The release verdict for an armed ⌘+click is a mouse
//! decision, not a matching one, so it lives with the other pure mouse-event
//! rules in [`crate::mouse`] (`link_click_verdict`) — which also keeps this
//! module free of gpui types (std + alacritty only). The gpui event glue
//! (holding the armed/hover state, doing the opening) lives in [`crate::view`];
//! the locked-`Term` wrapper is
//! [`TerminalSessionHandle::hyperlink_at`](crate::TerminalSessionHandle::hyperlink_at).
//! Splitting the decisions out keeps them unit-testable against a bare `Term`,
//! with no session, pty, or window. The one non-matching item here is
//! [`UrlOpener`], the injected "open this URL" seam that completes the feature —
//! it lives beside the matcher for the same reason [`ImageDropProvider`] lives
//! beside the drop builder.
//!
//! [`ImageDropProvider`]: crate::ImageDropProvider
//!
//! ## Detection is regex-only
//!
//! Links are found by running [`URL_REGEX`] over the grid via alacritty's
//! `RegexIter` — the same mechanism Alacritty's own URL hints and Zed's terminal
//! links use. OSC 8 *explicit* hyperlinks (the escape sequence a program can emit
//! to attach a URL to arbitrary text) are deliberately NOT handled; that is a
//! separate, deferred feature.
//!
//! ## Why the search span is bounded
//!
//! Hover runs this on **every** ⌘-held mouse-move, so the search must be O(screen),
//! not O(scrollback). The span is the visible viewport, expanded at both edges by
//! `line_search_left`/`line_search_right` so a URL that soft-wraps across the
//! viewport boundary is still matched whole, then clamped to [`MAX_SEARCH_LINES`]
//! beyond the viewport in either direction. Without the clamp a pathological
//! buffer (every line wrapped) would make one mouse-move walk the entire history.
//!
//! ## Why the compiled regex is cached
//!
//! `RegexSearch::new` builds four lazy DFAs. Rebuilding those per mouse-move is
//! not acceptable, so callers hold a [`UrlRegexCache`] and compile once. Matching
//! needs `&mut RegexSearch` (the DFA caches are mutated as they fill), and this
//! is all main-thread-only, so the cache is a plain `RefCell` — no sync.

use std::cell::RefCell;
use std::sync::Arc;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::Term;

/// The URL pattern, matching Alacritty's (and Zed's) default hint regex: a broad
/// scheme set, then a run of everything that is not a control character,
/// whitespace, or one of the delimiters that conventionally end a URL in prose.
///
/// The `\u{…}` escapes are **Rust** escapes — the regex engine (regex-automata,
/// via alacritty) sees the literal control characters inside the class, which is
/// how Alacritty writes it too. `{-}` is the `{`..`}` range; `\\s` and `\\^` are
/// the regex escapes for whitespace and a literal caret.
///
/// Deliberately scheme-anchored: a bare `localhost:5173` is not a link (there is
/// no way to know what to open), while `http://localhost:5173/` — the reported
/// npm-serve case — is.
pub const URL_REGEX: &str = "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`]+";

/// How far beyond the viewport the soft-wrap expansion may reach, in lines
/// (Zed's number). See the module docs: this bounds one hover's work.
pub const MAX_SEARCH_LINES: usize = 100;

/// The injected "open this URL with the OS default handler" side-channel, handed
/// to [`TerminalView::set_url_opener`](crate::TerminalView::set_url_opener) —
/// the same pattern as [`KeyCodeProbe`](crate::KeyCodeProbe) and
/// [`ImageDropProvider`](crate::ImageDropProvider), so this crate stays
/// objc2-free.
///
/// The app injects `crates/nice/src/platform`'s `workspace_open_url`, which
/// **defers** the `NSWorkspace openURL:` to its own main-queue turn: an AppKit
/// workspace call made while a gpui `App` borrow is live can re-enter and panic
/// (commit 4342e23, Reveal in Finder), and a ⌘+click open runs from inside a
/// mouse-up listener — squarely inside such a borrow. Without an opener the view
/// falls back to `cx.open_url`, which keeps the crate usable standalone (tests,
/// embeddings) but is NOT what production wires.
pub type UrlOpener = Arc<dyn Fn(&str)>;

/// The lazily-compiled, reusable [`URL_REGEX`] matcher.
///
/// Main-thread only (a view field), hence `RefCell` rather than a mutex. Compiled
/// on first use so a pane that is never ⌘-hovered never pays for the DFAs.
#[derive(Default)]
pub struct UrlRegexCache(RefCell<Option<RegexSearch>>);

impl UrlRegexCache {
    /// An empty cache; the regex compiles on the first [`with`](Self::with).
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `f` against the compiled URL regex, compiling it on first use.
    ///
    /// `None` iff [`URL_REGEX`] fails to compile — impossible in practice (a
    /// unit test pins that it compiles), but returned rather than panicked so a
    /// bad edit to the constant degrades to "links stop working", not a crash on
    /// the render thread.
    pub fn with<R>(&self, f: impl FnOnce(&mut RegexSearch) -> R) -> Option<R> {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            *slot = RegexSearch::new(URL_REGEX).ok();
        }
        slot.as_mut().map(f)
    }
}

/// Whether a hover transition moves the underline — i.e. whether paint must
/// re-run. The comparison is over the match RANGES alone: the underline rides
/// the element's `SnapshotKey`, so only a range that appeared, vanished, or
/// moved changes pixels. Equal ranges — the common case of a ⌘-held pointer
/// sweeping across one URL, one mouse-move per pixel — must NOT repaint: a
/// notify per pixel is exactly what the r5b row cache exists to avoid. Pure so
/// that rule is pinned by unit tests (`crate::view`'s `set_hovered_hyperlink`
/// just calls it).
pub(crate) fn hover_changed(
    before: &Option<(String, Match)>,
    next: &Option<(String, Match)>,
) -> bool {
    match (before, next) {
        (Some((_, b)), Some((_, a))) => b != a,
        (None, None) => false,
        _ => true,
    }
}

/// The number of trailing characters to drop from a matched URL.
///
/// [`URL_REGEX`]'s trailing class is deliberately permissive, so a URL written in
/// prose swallows the punctuation after it (`(see http://a.b/c).` matches
/// `http://a.b/c).`). This strips that back off:
///
/// * `. , ; : ! ? ' "` always go — they never end a URL in practice.
/// * `) ] }` go **only** when the match has no matching opener, so
///   `https://en.wikipedia.org/wiki/Foo_(bar)` keeps its closer while the
///   `…/c)` above loses one.
///
/// Pure and counted in characters (every character it strips is ASCII, so the
/// count is also the number of cells to shrink the match range by — the
/// underline must not extend over trimmed punctuation).
pub(crate) fn trailing_trim_len(text: &str) -> usize {
    let mut end = text.len();
    let mut dropped = 0;
    while let Some(c) = text[..end].chars().next_back() {
        let head = &text[..end];
        let strip = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => !head.contains('('),
            ']' => !head.contains('['),
            '}' => !head.contains('{'),
            _ => false,
        };
        if !strip {
            break;
        }
        end -= c.len_utf8();
        dropped += 1;
    }
    dropped
}

/// The hyperlink under `point` (buffer coordinates), or `None`.
///
/// Returns the URL text with trailing punctuation trimmed, plus the **trimmed**
/// match range — the two stay in sync so the hover underline covers exactly the
/// characters the click would open.
///
/// The caller supplies the compiled regex (see [`UrlRegexCache`]) because
/// matching needs `&mut RegexSearch`; `term` is read-only. This is the whole
/// matching algorithm — see the module docs for the bounded span.
pub(crate) fn hyperlink_at_point<T>(
    term: &Term<T>,
    point: Point,
    regex: &mut RegexSearch,
) -> Option<(String, Match)> {
    let (start, end) = search_span(term, point)?;

    // Walk forward over the span, discarding matches that end before the cell and
    // stopping at the first that begins after it — so a viewport full of URLs
    // costs one scan up to `point`, not a scan of every match in the span.
    let matched = RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(|m| *m.end() < point)
        .take_while(|m| *m.start() <= point)
        .next()?;

    let text = term.bounds_to_string(*matched.start(), *matched.end());
    let drop = trailing_trim_len(&text);
    if drop == 0 {
        return Some((text, matched));
    }

    // Shrink text and range together. `sub` walks back over the grid the same way
    // the match ran forward, so a trimmed soft-wrapped match ends on the right
    // cell of the right line.
    let text = text[..text.len() - trailing_byte_len(&text, drop)].to_string();
    if text.is_empty() {
        return None;
    }
    let new_end = matched.end().sub(term, Boundary::Grid, drop);
    if new_end < *matched.start() {
        return None;
    }
    Some((text, *matched.start()..=new_end))
}

/// Byte length of the last `chars` characters of `text` (the trim, applied to
/// the extracted string).
fn trailing_byte_len(text: &str, chars: usize) -> usize {
    text.chars().rev().take(chars).map(char::len_utf8).sum()
}

/// The bounded search span for a hover at `point`: the viewport, extended over
/// soft wraps at both edges and clamped to [`MAX_SEARCH_LINES`] beyond it.
///
/// `None` when `point` falls outside the span entirely (a stale hit-test against
/// a grid that has since scrolled) — nothing to search for it there.
fn search_span<T>(term: &Term<T>, point: Point) -> Option<(Point, Point)> {
    let display_offset = term.grid().display_offset() as i32;
    let viewport_top = Line(-display_offset);
    let viewport_bottom = Line(term.screen_lines() as i32 - 1 - display_offset);

    // Both alacritty walks are already clamped to the buffer (left stops at
    // `topmost_line`, right at the last screen line); the MAX_SEARCH_LINES clamp
    // is what bounds an all-wrapped history.
    let mut start = term.line_search_left(Point::new(viewport_top, Column(0)));
    let mut end = term.line_search_right(Point::new(viewport_bottom, term.last_column()));

    let limit = MAX_SEARCH_LINES as i32;
    if start.line < viewport_top - limit {
        start = Point::new(viewport_top - limit, Column(0));
    }
    if end.line > viewport_bottom + limit {
        end = Point::new(viewport_bottom + limit, term.last_column());
    }

    (start <= point && point <= end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::{
        hover_changed, hyperlink_at_point, trailing_trim_len, UrlRegexCache, MAX_SEARCH_LINES,
        URL_REGEX,
    };
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::search::RegexSearch;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

    /// A term of `cols`x`rows` fed `text` as if the child had printed it.
    fn term_of(cols: usize, rows: usize, text: &str) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(cols, rows), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    fn term_with(text: &str) -> Term<VoidListener> {
        term_of(80, 24, text)
    }

    fn regex() -> RegexSearch {
        RegexSearch::new(URL_REGEX).expect("URL_REGEX must compile")
    }

    /// The link at buffer `(line, col)`, as the view will ask for it.
    fn link_at(term: &Term<VoidListener>, line: i32, col: usize) -> Option<(String, (Point, Point))> {
        let mut re = regex();
        hyperlink_at_point(term, Point::new(Line(line), Column(col)), &mut re)
            .map(|(text, m)| (text, (*m.start(), *m.end())))
    }

    fn text_at(term: &Term<VoidListener>, line: i32, col: usize) -> Option<String> {
        link_at(term, line, col).map(|(t, _)| t)
    }

    // ---- The regex itself -----------------------------------------------------

    #[test]
    fn url_regex_compiles() {
        // The constant is a literal handed straight to regex-automata; if an edit
        // ever makes it unbuildable, links silently stop working. Pin it here.
        assert!(RegexSearch::new(URL_REGEX).is_ok(), "URL_REGEX must compile");
    }

    #[test]
    fn url_regex_cache_compiles_once_and_matches() {
        // The cache is the hover-path storage: it must hand out a usable regex
        // (and keep handing out the same one — a second `with` after the first
        // must still succeed, i.e. the compiled DFA is retained, not consumed).
        let cache = UrlRegexCache::new();
        let term = term_with("open http://localhost:5173/ now");
        for _ in 0..2 {
            let found = cache
                .with(|re| {
                    hyperlink_at_point(&term, Point::new(Line(0), Column(8)), re)
                        .map(|(t, _)| t)
                })
                .expect("URL_REGEX compiles, so `with` runs");
            assert_eq!(found.as_deref(), Some("http://localhost:5173/"));
        }
    }

    // ---- Matching -------------------------------------------------------------

    #[test]
    fn finds_the_npm_serve_url_at_every_cell_and_nowhere_else() {
        // The reported case: a URL printed mid-line. Every cell OF the URL must
        // resolve to it, and the cells immediately either side must not.
        let prefix = "Local: ";
        let url = "http://localhost:5173/";
        let term = term_with(&format!("{prefix}{url} (ready)"));
        let start = prefix.len();

        for col in start..start + url.len() {
            assert_eq!(
                text_at(&term, 0, col).as_deref(),
                Some(url),
                "cell {col} is inside the URL"
            );
        }
        assert_eq!(
            text_at(&term, 0, start - 1),
            None,
            "the space before the URL is not a link"
        );
        assert_eq!(
            text_at(&term, 0, start + url.len()),
            None,
            "the space after the URL is not a link"
        );
    }

    #[test]
    fn soft_wrapped_url_matches_whole_from_either_line() {
        // A URL longer than the row wraps; both halves must resolve to the same
        // full URL (the match range spans the wrap).
        let url = "http://example.com/aaaaaaaaaa/bbbbbbbbbb/cccccccccc";
        let term = term_of(20, 24, url);
        assert!(url.len() > 20, "the fixture must actually wrap");

        let first = link_at(&term, 0, 5).expect("match on the first row");
        let second = link_at(&term, 1, 3).expect("match on the wrapped row");
        assert_eq!(first.0, url);
        assert_eq!(second.0, url);
        assert_eq!(first.1, second.1, "both cells resolve to the same match");
        assert!(
            first.1 .1.line > first.1 .0.line,
            "the match range spans the soft wrap"
        );
    }

    #[test]
    fn url_in_scrollback_matches_while_scrolled_up() {
        // Scrolled into history, a URL on a negative buffer line is found — the
        // search span follows the viewport, not the screen's line 0.
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"see http://example.com/old\r\n");
        for i in 0..40 {
            parser.advance(&mut term, format!("filler {i}\r\n").as_bytes());
        }
        term.scroll_display(Scroll::Delta(30));
        let offset = term.grid().display_offset() as i32;
        assert!(offset > 0, "scrolled up into history");

        // The URL line scrolled off the top long ago; locate it by scanning the
        // visible span rather than recomputing where history put it.
        let found = (-(offset + 5)..24)
            .find_map(|line| text_at(&term, line, 6).map(|t| (line, t)));
        let (line, text) = found.expect("the scrollback URL is matched while scrolled up");
        assert_eq!(text, "http://example.com/old");
        assert!(
            line < 0,
            "the match is on a scrollback (negative) buffer line, not on screen"
        );
    }

    #[test]
    fn broad_scheme_set_matches_and_bare_hostport_does_not() {
        for (line, col, want) in [
            ("mailto:x@y.z", 3, Some("mailto:x@y.z")),
            ("file:///tmp/a", 2, Some("file:///tmp/a")),
            ("ssh:host", 1, Some("ssh:host")),
            ("localhost:5173", 4, None),
            ("just some words", 4, None),
        ] {
            let term = term_with(line);
            assert_eq!(
                text_at(&term, 0, col).as_deref(),
                want,
                "matching {line:?} at col {col}"
            );
        }
    }

    #[test]
    fn a_cell_outside_the_search_span_has_no_link() {
        // A stale hit-test (a buffer line the viewport no longer covers) must
        // return None rather than matching something elsewhere on screen.
        let term = term_with("http://example.com/");
        assert_eq!(
            text_at(&term, -(MAX_SEARCH_LINES as i32) - 5, 0),
            None,
            "a line far outside the clamped span yields nothing"
        );
    }

    // ---- Trailing-punctuation trim -------------------------------------------

    #[test]
    fn trim_strips_sentence_punctuation() {
        for (text, want) in [
            ("http://a.b/c.", 1),
            ("http://a.b/c,", 1),
            ("http://a.b/c;", 1),
            ("http://a.b/c:", 1),
            ("http://a.b/c!", 1),
            ("http://a.b/c?", 1),
            ("http://a.b/c'", 1),
            ("http://a.b/c\"", 1),
            ("http://a.b/c", 0),
        ] {
            assert_eq!(trailing_trim_len(text), want, "trimming {text:?}");
        }
    }

    #[test]
    fn trim_strips_an_unmatched_closer_but_keeps_a_matched_one() {
        assert_eq!(trailing_trim_len("http://a.b/c)."), 2, "`).` both go");
        assert_eq!(trailing_trim_len("http://a.b/c]"), 1);
        assert_eq!(trailing_trim_len("http://a.b/c}"), 1);
        assert_eq!(
            trailing_trim_len("https://en.wikipedia.org/wiki/Foo_(bar)"),
            0,
            "a matched opener keeps the closer — Wikipedia-style URLs stay whole"
        );
        assert_eq!(
            trailing_trim_len("https://en.wikipedia.org/wiki/Foo_(bar)."),
            1,
            "…but the sentence period after it still goes"
        );
    }

    #[test]
    fn parenthesised_url_in_prose_is_trimmed_and_the_range_shrinks() {
        // End to end: the regex over-matches `http://a.b/c).`, the trim takes the
        // two trailing characters off, and the returned range shrinks with the
        // text so the underline stops at the real last character.
        let term = term_with("(see http://a.b/c).");
        let (text, (start, end)) = link_at(&term, 0, 8).expect("match");
        assert_eq!(text, "http://a.b/c");
        assert_eq!(start, Point::new(Line(0), Column(5)));
        assert_eq!(
            end,
            Point::new(Line(0), Column(5 + text.len() - 1)),
            "the range ends on the URL's last character, not on `)` or `.`"
        );
    }

    #[test]
    fn wikipedia_style_url_keeps_its_closing_paren() {
        let url = "https://en.wikipedia.org/wiki/Foo_(bar)";
        let term = term_with(url);
        let (text, (_, end)) = link_at(&term, 0, 10).expect("match");
        assert_eq!(text, url);
        assert_eq!(end, Point::new(Line(0), Column(url.len() - 1)));
    }

    #[test]
    fn trimming_keeps_text_and_range_the_same_length() {
        // The invariant the underline depends on: however much is trimmed, the
        // range covers exactly as many cells as the text has characters.
        let term = term_with("(see http://a.b/c).");
        let (text, (start, end)) = link_at(&term, 0, 8).expect("match");
        assert_eq!(
            end.column.0 - start.column.0 + 1,
            text.chars().count(),
            "range width == trimmed text length"
        );
    }

    // ---- Choosing among several URLs -----------------------------------------

    #[test]
    fn two_urls_on_one_line_resolve_independently() {
        // The npm-serve shape that motivated the feature prints two URLs on
        // adjacent columns; picking the wrong one would silently open the wrong
        // page. Each cell must resolve to ITS url, and the gap between them to
        // neither.
        let first = "http://localhost:5173/";
        let second = "http://192.168.1.9:5173/";
        let term = term_with(&format!("Local: {first}  Network: {second}"));
        let first_start = "Local: ".len();
        let second_start = format!("Local: {first}  Network: ").len();

        assert_eq!(text_at(&term, 0, first_start).as_deref(), Some(first));
        assert_eq!(
            text_at(&term, 0, first_start + first.len() - 1).as_deref(),
            Some(first),
            "last cell of the first URL"
        );
        assert_eq!(
            text_at(&term, 0, second_start).as_deref(),
            Some(second),
            "first cell of the second URL"
        );
        assert_eq!(
            text_at(&term, 0, second_start + second.len() - 1).as_deref(),
            Some(second),
            "last cell of the second URL"
        );
        // The two gap cells between the URLs, and the word between them.
        assert_eq!(text_at(&term, 0, first_start + first.len()), None);
        assert_eq!(text_at(&term, 0, second_start - "Network: ".len()), None);
    }

    // ---- The hover-change gate ------------------------------------------------

    /// A `(url, match)` pair spanning `cols` on line 0 — enough shape for the
    /// range comparison, which never reads the string.
    fn hover(text: &str, start: usize, end: usize) -> Option<(String, super::Match)> {
        Some((
            text.to_string(),
            Point::new(Line(0), Column(start))..=Point::new(Line(0), Column(end)),
        ))
    }

    #[test]
    fn hover_changed_gates_on_the_range_alone() {
        // Same range ⇒ no repaint (the per-pixel sweep case) — even if the
        // string differed, the pixels wouldn't.
        assert!(!hover_changed(&hover("http://a.b/", 0, 10), &hover("http://a.b/", 0, 10)));
        assert!(!hover_changed(&None, &None));

        // A range that moved, appeared, or vanished repaints.
        assert!(hover_changed(&hover("http://a.b/", 0, 10), &hover("http://a.b/", 5, 15)));
        assert!(hover_changed(&None, &hover("http://a.b/", 0, 10)));
        assert!(hover_changed(&hover("http://a.b/", 0, 10), &None));
    }
}
