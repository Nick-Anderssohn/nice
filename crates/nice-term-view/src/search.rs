//! Scrollback search — the engine half of Phase 3's copy mode (plan
//! `docs/plans/phase-3-copy-mode-search.md`, decisions P1/P7/P8).
//!
//! Copy mode itself is `TermMode::VI` (P1) — alacritty owns that bit and every
//! motion. Search has no library-side state at all: `Term::search_next` is a
//! pure query over the grid, so the *query*, its compiled matcher, the confirmed
//! direction and the active match live here, on a value the
//! [`TerminalSessionHandle`](crate::TerminalSessionHandle) owns (entity-scoped:
//! it survives a view unmount and dies with the pane; nothing is ever
//! persisted).
//!
//! Two things in here are Nice's, not the library's:
//!
//! * **The lazy compile** ([`SearchState::with_regex`]) — the
//!   [`UrlRegexCache`](crate::UrlRegexCache) shape: matching needs
//!   `&mut RegexSearch` (the lazy DFAs mutate their caches) but the render path
//!   only has `&self`, so the slot is a `RefCell` and the regex compiles on
//!   first use. An invalid pattern is remembered as [`Compiled::Invalid`] and
//!   simply yields no matches — a half-typed `(foo` must never surface an error
//!   (P7).
//! * **The origin advance** ([`step_origin`]) — `Term::search_next`'s accept
//!   predicate *includes* the origin cell (`term/search.rs:164-173`/`:203-212`),
//!   which is exactly right for confirming a query (the match under the cursor
//!   counts) and exactly wrong for `n`/`N`: a cursor parked on the active match
//!   would return that same match forever. So a step starts one cell off the
//!   active match in the direction of travel. Upstream alacritty's *app* does
//!   the same thing; the library does not.

use std::cell::RefCell;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::Term;

/// How many matches one [`viewport_matches`](crate::TerminalSessionHandle::viewport_matches)
/// pass may collect. The span is already viewport-bounded (P8), so this only
/// caps the pathological case — a one-character query over a dense grid — from
/// allocating thousands of ranges per frame. Sibling of
/// [`MAX_SEARCH_LINES`](crate::MAX_SEARCH_LINES), the hyperlink hover budget.
pub const MAX_VIEWPORT_MATCHES: usize = 512;

/// How many rows beyond the viewport [`viewport_matches`](crate::TerminalSessionHandle::viewport_matches)
/// scans by default, so a match that begins just off-screen and wraps into view
/// is still found. Small on purpose: the cost of this scan is per frame.
pub const VIEWPORT_MATCH_MARGIN: usize = 2;

/// The compiled-matcher slot: compiled on first use, and a compile failure is
/// *remembered* so a bad pattern is not re-built on every frame.
enum Compiled {
    /// Not compiled yet (fresh state, or the query just changed).
    Pending,
    Ok(Box<RegexSearch>),
    /// The query failed to compile — no matches, no error surface (P7).
    Invalid,
}

/// The per-pane search sub-state (P1): query, lazily compiled matcher, confirmed
/// direction, and the active (focused) match.
///
/// Main-thread only — it hangs off a gpui entity — hence the `RefCell` rather
/// than a mutex, exactly like [`UrlRegexCache`](crate::UrlRegexCache).
pub struct SearchState {
    query: String,
    regex: RefCell<Compiled>,
    /// The direction `n` repeats in; `N` reverses it without changing this (P7).
    direction: Direction,
    active: Option<Match>,
}

impl Default for SearchState {
    fn default() -> Self {
        // Backward is the flagship direction (P7: `⌃⌘/` looks for the thing that
        // scrolled past), so it is also the resting default.
        Self::new(Direction::Left)
    }
}

impl SearchState {
    /// An empty state searching in `direction`.
    pub fn new(direction: Direction) -> Self {
        Self {
            query: String::new(),
            regex: RefCell::new(Compiled::Pending),
            direction,
            active: None,
        }
    }

    /// The direction `n` repeats in.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Point a fresh search in `direction`, dropping any previous query.
    pub fn restart(&mut self, direction: Direction) {
        *self = Self::new(direction);
    }

    /// The current query (empty when no search is live).
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether a search is live — i.e. there is a non-empty query. An invalid
    /// pattern still counts as live (the badge shows what the user typed); it
    /// just matches nothing.
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    /// Replace the query, dropping the compiled matcher and the active match.
    /// Re-setting the *same* query is a no-op, so incremental typing that
    /// re-emits an unchanged string does not throw away the DFAs.
    pub fn set_query(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query.clear();
        self.query.push_str(query);
        *self.regex.borrow_mut() = Compiled::Pending;
        self.active = None;
    }

    /// The focused match, if a search has landed on one.
    pub fn active_match(&self) -> Option<&Match> {
        self.active.as_ref()
    }

    /// Record the focused match (set by confirm / `n` / `N`).
    pub fn set_active_match(&mut self, m: Match) {
        self.active = Some(m);
    }

    /// Drop the query, the matcher and the active match, keeping the direction.
    pub fn clear(&mut self) {
        self.query.clear();
        *self.regex.borrow_mut() = Compiled::Pending;
        self.active = None;
    }

    /// Run `f` against the compiled matcher, compiling on first use.
    ///
    /// `None` when there is no query or the query does not compile — both are
    /// "no matches", never an error (P7). Smart case is the library's:
    /// `RegexSearch::new` builds case-insensitive DFAs unless the pattern
    /// contains an uppercase character.
    pub fn with_regex<R>(&self, f: impl FnOnce(&mut RegexSearch) -> R) -> Option<R> {
        if self.query.is_empty() {
            return None;
        }
        let mut slot = self.regex.borrow_mut();
        if matches!(*slot, Compiled::Pending) {
            *slot = match RegexSearch::new(&self.query) {
                Ok(regex) => Compiled::Ok(Box::new(regex)),
                Err(_) => Compiled::Invalid,
            };
        }
        match &mut *slot {
            Compiled::Ok(regex) => Some(f(regex)),
            _ => None,
        }
    }
}

/// The origin for one `n`/`N` step: **one cell off the active match** in the
/// direction of travel, or the raw vi cursor when nothing is focused yet.
///
/// See the module docs: `search_next` accepts a match *at* the origin, so
/// stepping from the active match's own cell would never advance. Forward
/// travel starts past the match's END (so a multi-cell match cannot re-match
/// from its own interior); backward travel starts before its START. Both use
/// [`Boundary::None`], which lets the point run off the grid — `search_next`
/// then falls back to the whole-buffer scan, which is what produces the wrap at
/// either end.
pub(crate) fn step_origin<D: Dimensions>(
    dimensions: &D,
    active: Option<&Match>,
    cursor: Point,
    direction: Direction,
) -> Point {
    match active {
        Some(m) => match direction {
            Direction::Right => (*m.end()).add(dimensions, Boundary::None, 1),
            Direction::Left => (*m.start()).sub(dimensions, Boundary::None, 1),
        },
        None => cursor,
    }
}

/// The inclusive grid span covering the viewport grown by `margin` rows, clamped
/// to the grid (P8). Column 0 of the top row through the last column of the
/// bottom row, so a match anywhere on an edge row is inside the span.
pub(crate) fn viewport_span<T>(term: &Term<T>, margin: usize) -> (Point, Point) {
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
    let margin = i32::try_from(margin).unwrap_or(i32::MAX);
    let viewport_top = -offset;
    // `screen_lines` can be 0 for a beat during a resize; `max` keeps the span
    // ordered (bottom is never above top) instead of yielding an inverted range.
    let viewport_bottom = viewport_top
        .saturating_add(i32::try_from(term.screen_lines()).unwrap_or(i32::MAX))
        .saturating_sub(1)
        .max(viewport_top);

    let top = Line(viewport_top.saturating_sub(margin)).grid_clamp(term, Boundary::Grid);
    let bottom = Line(viewport_bottom.saturating_add(margin)).grid_clamp(term, Boundary::Grid);
    (Point::new(top, Column(0)), Point::new(bottom, term.last_column()))
}

/// Every match inside [`viewport_span`], capped at `budget` (P8: recomputed per
/// frame, so it must stay cheap and can never go stale).
pub(crate) fn viewport_matches_in<T>(
    term: &Term<T>,
    regex: &mut RegexSearch,
    margin: usize,
    budget: usize,
) -> Vec<Match> {
    let (start, end) = viewport_span(term, margin);
    RegexIter::new(start, end, Direction::Right, term, regex)
        .take(budget)
        .collect()
}

/// Search from `origin` and land the vi cursor on the hit.
///
/// `Side::Left` matches on the match's START — the cell the cursor jumps to —
/// and `max_lines: None` searches the WHOLE buffer (scrollback included), which
/// is also what makes `search_next` wrap at the ends. Returns the match so the
/// caller can record it as active; `None` leaves the cursor where it was.
pub(crate) fn run_search<L: EventListener>(
    term: &mut Term<L>,
    regex: &mut RegexSearch,
    origin: Point,
    direction: Direction,
) -> Option<Match> {
    let found = term.search_next(regex, origin, direction, Side::Left, None)?;
    term.vi_goto_point(*found.start());
    Some(found)
}

/// The opposite direction — `N`'s reversal of the confirmed `n` direction (P7).
pub(crate) fn flip(direction: Direction) -> Direction {
    match direction {
        Direction::Right => Direction::Left,
        Direction::Left => Direction::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, TermMode};
    use alacritty_terminal::vte::ansi::Processor;

    /// A 80x24 term holding 30 lines of seeded scrollback, copy mode on.
    ///
    /// 30 printed lines plus the cursor's row is 31 against a 24-row screen, so
    /// history holds 7 rows and grid `Line(l)` shows printed line `7 + l`:
    ///
    /// | printed | text        | grid line | in the viewport? |
    /// |---------|-------------|-----------|------------------|
    /// | 2       | `needle 2`  | `-5`      | no (scrollback)  |
    /// | 10      | `Widget 10` | `3`       | yes              |
    /// | 15      | `needle 15` | `8`       | yes              |
    /// | 20      | `widget 20` | `13`      | yes              |
    /// | 28      | `needle 28` | `21`      | yes              |
    ///
    /// `needle` walks the buffer; the `Widget`/`widget` pair is the smart-case
    /// probe. Every seeded token starts at column 0.
    fn seeded_term() -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..30 {
            let line = match i {
                2 | 15 | 28 => format!("needle {i}\r\n"),
                10 => format!("Widget {i}\r\n"),
                20 => format!("widget {i}\r\n"),
                _ => format!("filler {i}\r\n"),
            };
            parser.advance(&mut term, line.as_bytes());
        }
        term.toggle_vi_mode();
        assert!(term.mode().contains(TermMode::VI));
        term
    }

    fn state_for(query: &str, direction: Direction) -> SearchState {
        let mut state = SearchState::new(direction);
        state.set_query(query);
        state
    }

    /// One search, exactly as the handle runs it.
    fn search(
        term: &mut Term<VoidListener>,
        state: &SearchState,
        origin: Point,
        direction: Direction,
    ) -> Option<Match> {
        state
            .with_regex(|regex| run_search(term, regex, origin, direction))
            .flatten()
    }

    /// The grid line a match starts on — what every assertion below is about.
    fn line_of(m: &Match) -> i32 {
        m.start().line.0
    }

    /// `⌃⌘/` searches BACKWARD (P7): from the live bottom it finds the most
    /// recent match ABOVE the cursor, and lands the vi cursor on its first cell.
    #[test]
    fn a_backward_search_lands_on_the_nearest_match_above_the_cursor() {
        let mut term = seeded_term();
        let state = state_for("needle", Direction::Left);
        let origin = term.vi_mode_cursor.point;

        let found = search(&mut term, &state, origin, Direction::Left).expect("a match");
        assert_eq!(line_of(&found), 21, "the most recent `needle`");
        assert_eq!(*found.start(), Point::new(Line(21), Column(0)));
        assert_eq!(*found.end(), Point::new(Line(21), Column(5)), "the whole token");
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(21), Column(0)));
    }

    /// **The `n`/`N` origin advance** — the reason [`step_origin`] exists.
    ///
    /// `search_next`'s accept predicate includes the origin cell, so re-searching
    /// from the cursor's own position returns the match it is already sitting on,
    /// forever. This test pins BOTH halves: the naive origin repeats, the
    /// advanced origin walks.
    #[test]
    fn stepping_advances_off_the_active_match_instead_of_repeating_it() {
        let mut term = seeded_term();
        let state = state_for("needle", Direction::Left);
        let origin = term.vi_mode_cursor.point;
        let first = search(&mut term, &state, origin, Direction::Left).expect("a match");
        assert_eq!(line_of(&first), 21);

        // The bug, made visible: searching again from the raw cursor is a no-op.
        let naive_origin = term.vi_mode_cursor.point;
        let repeat = search(&mut term, &state, naive_origin, Direction::Left).expect("a match");
        assert_eq!(repeat, first, "a raw-cursor origin re-finds the active match");

        // The fix: start one cell off the active match, in the direction of travel.
        let advanced = step_origin(&term, Some(&first), naive_origin, Direction::Left);
        assert_eq!(advanced, Point::new(Line(20), Column(79)), "one cell before the match");
        let second = search(&mut term, &state, advanced, Direction::Left).expect("a match");
        assert_eq!(line_of(&second), 8, "the next `needle` further into history");
        assert_eq!(term.vi_mode_cursor.point, Point::new(Line(8), Column(0)));
    }

    /// Walking backward off the end of the buffer wraps to the far end — the
    /// library's whole-buffer fallback, pinned because `n` depends on it.
    #[test]
    fn stepping_wraps_at_the_ends_of_the_buffer() {
        let mut term = seeded_term();
        let state = state_for("needle", Direction::Left);
        let origin = term.vi_mode_cursor.point;
        let mut active = search(&mut term, &state, origin, Direction::Left);

        // 21 → 8 → -5, i.e. every seeded `needle`, oldest last.
        let mut walked = vec![line_of(active.as_ref().expect("a match"))];
        for _ in 0..2 {
            let origin = step_origin(
                &term,
                active.as_ref(),
                term.vi_mode_cursor.point,
                Direction::Left,
            );
            active = search(&mut term, &state, origin, Direction::Left);
            walked.push(line_of(active.as_ref().expect("a match")));
        }
        assert_eq!(walked, vec![21, 8, -5]);

        // One more step runs out of buffer and comes back round to the newest.
        let origin = step_origin(
            &term,
            active.as_ref(),
            term.vi_mode_cursor.point,
            Direction::Left,
        );
        let wrapped = search(&mut term, &state, origin, Direction::Left).expect("a match");
        assert_eq!(line_of(&wrapped), 21, "wrapped to the newest match");
    }

    /// `N` reverses the travel without touching the search's own direction.
    #[test]
    fn reversing_the_direction_walks_the_other_way() {
        let mut term = seeded_term();
        let state = state_for("needle", Direction::Left);
        let origin = term.vi_mode_cursor.point;
        let first = search(&mut term, &state, origin, Direction::Left);
        let origin = step_origin(&term, first.as_ref(), term.vi_mode_cursor.point, Direction::Left);
        let second = search(&mut term, &state, origin, Direction::Left).expect("a match");
        assert_eq!(line_of(&second), 8);

        // `N` from there: flip and step forward again.
        let back_dir = flip(state.direction());
        assert!(matches!(back_dir, Direction::Right));
        let origin = step_origin(
            &term,
            Some(&second),
            term.vi_mode_cursor.point,
            back_dir,
        );
        assert_eq!(origin, Point::new(Line(8), Column(6)), "one cell past the match end");
        let back = search(&mut term, &state, origin, back_dir).expect("a match");
        assert_eq!(line_of(&back), 21, "back to where `n` came from");
        assert!(
            matches!(state.direction(), Direction::Left),
            "`N` does not re-point the search"
        );
    }

    /// With no active match, a step starts from the raw vi cursor — the
    /// at-cursor match counts, which is what `confirm_search` wants too.
    #[test]
    fn stepping_without_an_active_match_starts_from_the_cursor() {
        let term = seeded_term();
        let cursor = Point::new(Line(4), Column(2));
        assert_eq!(step_origin(&term, None, cursor, Direction::Left), cursor);
        assert_eq!(step_origin(&term, None, cursor, Direction::Right), cursor);
    }

    /// Smart case is the library's rule and Nice inherits it verbatim: an
    /// all-lowercase query is case-insensitive, a query with any uppercase is
    /// case-sensitive.
    #[test]
    fn search_is_smart_case() {
        let mut term = seeded_term();

        let insensitive = state_for("widget", Direction::Left);
        let matches = insensitive
            .with_regex(|regex| viewport_matches_in(&term, regex, 0, MAX_VIEWPORT_MATCHES))
            .expect("compiled");
        assert_eq!(
            matches.iter().map(line_of).collect::<Vec<_>>(),
            vec![3, 13],
            "lowercase matches `Widget` and `widget`"
        );

        let sensitive = state_for("Widget", Direction::Left);
        let matches = sensitive
            .with_regex(|regex| viewport_matches_in(&term, regex, 0, MAX_VIEWPORT_MATCHES))
            .expect("compiled");
        assert_eq!(
            matches.iter().map(line_of).collect::<Vec<_>>(),
            vec![3],
            "an uppercase character makes it case-sensitive"
        );

        // And the cursor lands on the case-sensitive hit, not the other one.
        let origin = term.vi_mode_cursor.point;
        let found = search(&mut term, &sensitive, origin, Direction::Left).expect("a match");
        assert_eq!(line_of(&found), 3);
    }

    /// A half-typed pattern must never surface an error — it just matches
    /// nothing, on every call (the compile failure is remembered, not retried).
    #[test]
    fn an_invalid_query_matches_nothing_and_never_errors() {
        let mut term = seeded_term();
        let state = state_for("(unclosed", Direction::Left);
        assert!(state.is_active(), "the query is still what the user typed");
        assert!(state.with_regex(|_| ()).is_none());
        assert!(state.with_regex(|_| ()).is_none(), "still no matcher on a second pass");

        let before = term.vi_mode_cursor.point;
        assert!(search(&mut term, &state, before, Direction::Left).is_none());
        assert_eq!(term.vi_mode_cursor.point, before, "the cursor stayed put");
    }

    /// An empty query is not a search at all — no matcher, no matches, no badge.
    #[test]
    fn an_empty_query_is_not_a_live_search() {
        let state = SearchState::new(Direction::Left);
        assert!(!state.is_active());
        assert!(state.with_regex(|_| ()).is_none());
    }

    /// A query with no hits leaves the cursor exactly where it was.
    #[test]
    fn a_query_with_no_hits_leaves_the_cursor_alone() {
        let mut term = seeded_term();
        let state = state_for("nosuchtext", Direction::Left);
        let before = term.vi_mode_cursor.point;
        assert!(search(&mut term, &state, before, Direction::Left).is_none());
        assert_eq!(term.vi_mode_cursor.point, before);
    }

    /// Editing the query drops the focused match (it belonged to the old
    /// pattern); re-setting the SAME text changes nothing, so incremental typing
    /// that re-emits an unchanged string keeps its DFAs.
    #[test]
    fn changing_the_query_drops_the_active_match() {
        let mut state = state_for("needle", Direction::Left);
        state.set_active_match(Point::new(Line(1), Column(0))..=Point::new(Line(1), Column(5)));
        assert!(state.active_match().is_some());

        state.set_query("needle");
        assert!(state.active_match().is_some(), "an unchanged query is a no-op");

        state.set_query("needles");
        assert!(state.active_match().is_none());
        assert_eq!(state.query(), "needles");
    }

    /// Clearing drops the query and the focused match but keeps the direction —
    /// the search-bar Esc path (P7), which leaves you in copy mode.
    #[test]
    fn clearing_keeps_the_direction() {
        let mut state = state_for("needle", Direction::Right);
        state.set_active_match(Point::new(Line(1), Column(0))..=Point::new(Line(1), Column(5)));
        state.clear();
        assert!(!state.is_active());
        assert!(state.active_match().is_none());
        assert!(matches!(state.direction(), Direction::Right));
    }

    /// The scanned span is the viewport, grown by `margin` rows and clamped to
    /// the grid — never the whole buffer (P8: this runs per frame).
    #[test]
    fn the_scanned_span_is_the_viewport_plus_its_margin() {
        let term = seeded_term();
        assert_eq!(term.grid().display_offset(), 0, "parked at the bottom");

        let (start, end) = viewport_span(&term, 0);
        assert_eq!(start, Point::new(Line(0), Column(0)));
        assert_eq!(end, Point::new(Line(23), Column(79)));

        let (start, end) = viewport_span(&term, 3);
        assert_eq!(start, Point::new(Line(-3), Column(0)));
        assert_eq!(end, Point::new(Line(23), Column(79)), "clamped at the live bottom");

        // A margin past the ends of the buffer clamps instead of running off.
        let (start, end) = viewport_span(&term, 1_000);
        assert_eq!(start, Point::new(Line(-7), Column(0)), "the top of history");
        assert_eq!(end, Point::new(Line(23), Column(79)));
    }

    /// Only matches inside that span are collected, and never more than the
    /// budget.
    #[test]
    fn viewport_matches_are_bounded_by_the_span_and_the_budget() {
        let term = seeded_term();
        let state = state_for("needle", Direction::Left);
        let lines = |margin: usize, budget: usize| {
            state
                .with_regex(|regex| viewport_matches_in(&term, regex, margin, budget))
                .expect("compiled")
                .iter()
                .map(line_of)
                .collect::<Vec<_>>()
        };

        // `needle 2` sits at Line(-5), five rows above the viewport.
        assert_eq!(lines(0, MAX_VIEWPORT_MATCHES), vec![8, 21], "on-screen matches only");
        assert_eq!(lines(4, MAX_VIEWPORT_MATCHES), vec![8, 21], "still short of it");
        assert_eq!(lines(5, MAX_VIEWPORT_MATCHES), vec![-5, 8, 21], "the margin reaches it");

        // The budget caps a pathological query instead of allocating per cell.
        assert_eq!(lines(5, 2), vec![-5, 8]);
    }
}
