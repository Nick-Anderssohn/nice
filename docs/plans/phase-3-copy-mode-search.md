# Phase 3 — copy mode + scrollback search

Roadmap: `docs/tmux-port-roadmap.md` § "Phase 3 — copy mode + scrollback
search (M)". Status: **IMPLEMENTED** (five slices on `phase-3-copy-mode`;
Nick's post-merge feel-check pending) — decisions resolved with Nick
2026-08-13; two single-Fable plan review rounds same day (round 1: 2
blocking + 5 important + 8 nit; round 2 verified every round-1 fold OK and
added 1 blocking + 3 important + 4 nit — all folded in below; reports kept
at `.claude/handoff/phase3-plan-review.md` and `…-round2.md`).

This is the in-repo copy of the implemented plan (Phase 1/2 precedent:
`docs/plans/phase-1-keybind-scheme.md`, `docs/plans/phase-2-splits.md`).
The body below is left as written, including its "Current-code facts" line
numbers — those were grounded against main `c0a31fc` and describe the code
BEFORE this phase. The "As shipped" section recording where the code and
the plan differ is appended after the feel-check, as it was for Phases 1
and 2.

Vocabulary (Phase R/2): sidebar row = `Session`, upper-bar pill =
`TermWindow`, `Pane` = one leaf of a pill's split tree. Copy mode and
search are **per-pane** — they belong to one pane's terminal, not to the
pill or the OS window.

## Current-code facts the plan builds on

Grounded 2026-08-13 against main `c0a31fc` (Explore sweep; line numbers
from that reading).

- **alacritty_terminal 0.26 ships a complete vi-mode engine, unused by
  Nice.** It is a plain crates.io dependency (`nice-term-core/
  Cargo.toml:23`, pinned 0.26.0 in the lock — NOT part of `vendor/zed`).
  `Term` has first-class support: `toggle_vi_mode()` (term/mod.rs:815 —
  flips the `TermMode::VI` bit; on entry seeds the cursor at the terminal
  cursor if visible, else viewport top-left), `vi_motion(ViMotion)`
  (:839 — no-ops unless VI is set), `vi_goto_point(Point)` (:855 —
  scrolls the point into view first), a public `vi_mode_cursor:
  ViModeCursor` field (:273) and `vi_mode_cursor_style` (:342).
  `ViMotion` (vi_mode.rs:15-52) models
  `Up/Down/Left/Right/First/Last/FirstOccupied/High/Middle/Low/
  SemanticLeft(End)/SemanticRight(End)/WordLeft(End)/WordRight(End)/
  Bracket/ParagraphUp/ParagraphDown` — h/j/k/l, `0`/`$`/`^`, `H`/`M`/`L`,
  `b`/`e`/`w` (+ WORD variants), `%`, `{`/`}`, 1:1 with vim.
  `ViModeCursor::motion`/`::scroll` (vi_mode.rs:74/:190) are pure over
  `Term`. `vi_mode_recompute_selection` (term/mod.rs:870) extends a live
  selection with `selection.update(cursor, Side::Left); include_all()` —
  the exact idiom `drag_selection_extend` already uses, so vi motions
  extend a selection with zero new logic, riding the same content-locked
  anchor machinery as the scroll-mid-drag fix.
- **`RegexSearch` is already linked and in production use.**
  `term/search.rs`: `RegexSearch::new(&str)` (:34, builds 4 lazy DFAs,
  case-insensitive unless the pattern has uppercase), `type Match =
  RangeInclusive<Point>` (:21), `Term::search_next(regex, origin,
  Direction, Side, max_lines) -> Option<Match>` (:121, the n/N entry
  point), `regex_search_left/right` (:228/:244), `RegexIter` (:620, all
  matches over a span). `regex-automata` is an unconditional dep — zero
  Cargo changes. Production precedent: the ⌘-hover hyperlink detector
  (`nice-term-view/src/hyperlink.rs`) — `UrlRegexCache` (:92-120) is the
  lazy-compile-once, interior-mutability shape to copy (`&mut
  RegexSearch` is needed because DFA caches mutate), and
  `MAX_SEARCH_LINES = 100` (:70) is the per-frame viewport budget
  pattern.
- **Selection driver** (`nice-term-view/src/session_handle.rs:519-615`):
  `set_selection`/`set_selection_typed` (:527/:537, buffer coords, i32
  line, negative = scrollback), `start_selection` (:568),
  `extend_selection` (:590 — returns `false` when the `Term` dropped the
  selection), `clear_selection` (:601), `selection_text` (:611 —
  alacritty's `selection_to_string`, the ⌘C source written to the
  clipboard at view.rs:1608-1611). `SelectionType::{Simple, Semantic,
  Lines, Block}` — the first three are used by click-count mouse
  selection (view.rs:1443-1445); `Block` is a real, unused variant.
  `drag_selecting: bool` (view.rs:272) is a gesture flag deliberately
  decoupled from selection liveness.
- **`TerminalView::on_key_down` is the single pty-bound key choke point**
  (view.rs:984-1103), gate order: held-pane (:1005-1031) → IME
  commit-swallow (:1036) → composing (:1056) → ⌘V paste / ⌘C copy
  (:1068-1095, copy gated on `!kitty_forwards_super(mode)`) →
  `dispatch_key` (:1102) → kitty/legacy encoder → pty. Phase-0's
  `scrollback_key_action` (input.rs:133, pure `(key, mods, TermMode) ->
  Option<ScrollbackAction>`) is consumed INSIDE `dispatch_key`
  (view.rs:1143-1147). **Dispatch-order doc at view.rs:1086-1094: gpui
  Actions run BEFORE view key listeners** — so mode-toggle chords are
  global `ShortcutAction`s, while in-mode bare keys (`h`, `v`, `y`…)
  must be intercepted here, where "not in copy mode" can still fall
  through to the pty. **The key listener is NOT the only pty writer**:
  gpui routes composing and `key_char`-less keys (dead keys, active IME
  sources) to the NSTextInputClient BEFORE any key listener
  (gpui_macos/src/window.rs:2256-2282), and the three IME callbacks —
  `ime_set_marked` (view.rs:1656), `ime_commit` (:1673), `ime_unmark`
  (:1700) — unconditionally snap-to-bottom and/or write the pty. ASCII
  printables are safe (`prefers_ime_for_printable_keys = false`,
  input.rs:548-552, so `on_key_down` sees them first).
- **Scroll/viewport API** (session_handle.rs:632-739): `scroll_lines`,
  `scroll_to_top/bottom`, `scroll_page_up/down`,
  `scroll_half_page_up/down` (Nice computes half-page deltas itself,
  :843/:851 — alacritty's `Scroll` enum has no half-page variant),
  `display_offset()` (:727), `is_at_bottom()` (:737), `is_alt_screen()`
  (:716 — gates the half-page chords). Coordinates:
  `alacritty_terminal::index::{Point, Line, Column}`, `Line` signed,
  negative = history; `Dimensions::total_lines/screen_lines/history_size`
  (grid/mod.rs:488-548).
- **Keymap template** — the pane-scoped action resolution chain
  (`scroll_active_window_half_page`, keymap.rs:911-936):
  `active_session_window` → `effective_pane_id()`
  (term_window.rs:154-162, focused leaf else sole leaf) →
  `PtyManager::pane_handle(session, window, pane)`
  (pty_manager.rs:1882-1891) → `handle.update(...)`.
- **Shortcuts**: `default_bindings()` (shortcuts.rs:481) is 34 entries;
  on the bare ⌃⌘ rung the taken letters are h/j/k/l/o/z/b plus
  `-`/`\`/arrows/digits; **⌃⌘C is free** (not a default, not reserved).
  `RESERVED_COMBOS` (:1016-1058) has 9 entries; the LAST is ⌃⌘/ —
  the only `FuturePhase` entry. Doctrine (:1009-1015): an entry must be
  REMOVED to become a default; `no_default_combo_is_reserved` (:1686)
  enforces disjointness; `reserved_table_covers_the_three_groups`
  (:1526-1552) pins len == 9, `count(FuturePhase) == 1`, and the literal
  token `"cmd-ctrl-/"` — all three assertions change when ⌃⌘/ is
  promoted. `every_reserved_chord_looks_up_to_its_entry` (:1512) sweeps
  the table generically.
- **Highlight rendering has exactly two channels, both hand-plumbed the
  same way** (`nice-term-view/src/element.rs`): `selection:
  Option<SelectionRange>` and `hovered_hyperlink: Option<Match>` are
  resolved per cell in `fill_row` (~:1786-1815) and both sit in the
  frame-cache key `SnapshotKey` (:602-609) — any change forces a full
  re-plan. There is no generic region-highlight abstraction; a third
  channel is added the identical way. alacritty's `Term::damage()`
  deliberately excludes selection (mod.rs:590 comment) — match-set
  changes must ride `SnapshotKey`, not damage. Themed-tint fallback
  precedent at element.rs:118-120.
- **The text-input precedent is inline rename, NOT Command Compose.**
  Command Compose is a zsh ZLE widget (`shell_inject.rs:353-511`) — no
  gpui overlay exists for it. The real precedent:
  `nice-model/src/file_browser/text_field.rs` `TextFieldEditor`
  (:72-76, pure, char-offset, anchor/caret, explicitly single-line) +
  `Key` enum (:28-67), and `crates/nice/src/inline_rename.rs`:
  `dispatch_rename_key` (:138, key-event → `Key` translator),
  `RenameTextElement` (:406, hand-painted element — no native gpui text
  input exists in this codebase), `rename_field` (:615, `.track_focus` +
  `.key_context` + a paint-phase canvas registering window-level mouse
  listeners :654-711; teardown = stop painting, :609-613). ⌘C/⌘X/⌘V
  already work in it.
- **Layering constraint**: `nice-term-view` has NO `nice-model`
  dependency, by doctrine (Cargo.toml header; session_handle.rs:285 "no
  `nice-model` types") — so the query-field UI cannot live in the view
  crate without breaking the layering rule. `crates/nice` (app) depends
  on both.
- **Per-pane state homes**: `TerminalSessionHandle` (session_handle.rs)
  is the view-independent per-pane entity — hidden panes keep it alive
  with no `TerminalView` mounted; `is_alt_screen()` (:716) is the
  precedent for reading a `TermMode` bit through it. View-local fields
  (`drag_selecting`, `held`, `overlay: LaunchOverlay`, overlay.rs:70-80)
  die with the view. Mode-indicator UI precedents: toolbar hint badge
  (toolbar.rs:135-137), `StatusDot`.
- **Test infra**: scenario table in app.rs (~:3800-3890) — `keybind-
  scheme`/`splits` are `Gate::SelfReported` with chord-count budgets,
  registered BEFORE `multiwindow` because they only `register` their
  window (no `WindowRegistry::install`) so closing them can't trip
  quit-when-empty. `input_live.rs` (2561 lines): `dispatch_key` (:780,
  in-process, no Accessibility grant), `chord_leak` (:1128),
  `freed_chord` (:1235), `settle`/`tap`/`type_ascii` (:157-198),
  `prepare_dir` (:126), `open_splits_window` (:1522). Headless
  `Term`+`Processor` harness: `nice-term-core/src/vt.rs` tests
  (:393-565, `feed` helper :427); session_handle tests (:1680-1930)
  drive a real `Term<VoidListener>` and assert on `display_offset()`/
  content, never viewport rows.

## Decisions (RESOLVED — Nick, 2026-08-13)

<!-- PROTECTED --> **D1 — one integrated mode; search is "copy mode with
a query."** `⌃⌘/` opens the search field; confirming jumps the keyboard
cursor to the match and lands IN copy mode with matches highlighted.
`n`/`N`, all motions, `v`/`y` work from there. One state machine, one
highlight system; found text is immediately selectable (search → land →
`v` → `y`). tmux's model.

<!-- PROTECTED --> **D2 — `⌃⌘c` enters copy mode directly.** Free on the
bare rung; "c = Copy Mode" is the Settings label. Sits beside the other
pane verbs (`z` zoom, `b` break). Accepted cost: it neighbors ⌘C copy,
so a slipped Ctrl enters the mode instead of copying — Esc recovers.

<!-- PROTECTED --> **D3 — full vi key set.** hjkl; `w`/`b`/`e` +
`W`/`B`/`E`; `0`/`$`/`^`; `H`/`M`/`L`; `%`; `{`/`}`; `g`/`G` (top/bottom
of scrollback); `⌃u`/`⌃d`/`⌃f`/`⌃b` paging; `v`/`V`/`⌃v` (char/line/
block — `SelectionType::Block` exists, unused until now); `y` yanks to
the clipboard and exits; Esc/`q` exit. The library models every motion —
the cost difference over a minimal set is key-table size and scenario
legs, not machinery.

### Plan-level decisions (mine — flag at sign-off if any grates)

- **P1 — copy mode IS `TermMode::VI`; no duplicate Nice-side mode
  flag.** "In copy mode" is read through the handle as
  `term.mode().contains(TermMode::VI)` (the `is_alt_screen` pattern).
  Search sub-state (query, compiled regex, direction, active match) has
  no alacritty equivalent and lives on `TerminalSessionHandle` —
  entity-scoped, survives view unmounts, dies with the pane. Nothing is
  ever persisted (`sessions.json` untouched).
- **P2 — the query-field UI lives in the app crate, not the view
  crate.** The layering rule (view crate has no `nice-model` dep) plus
  the inline-rename precedent decide this: a new `search_bar.rs` in
  `crates/nice` owns a `TextFieldEditor` + hand-painted element +
  its own `FocusHandle`, and `WindowHostView` overlays it on the focused
  pane's rect. The engine (matches, navigation) stays on the handle in
  the view crate; the bar pushes the query down through handle API.
  In-mode `/`/`?` need to open the app-side bar from inside the view —
  the handle emits a new typed event (`SearchRequested { backward }`),
  routed pane-keyed like every other terminal event.
- **P3 — key routing splits by dispatch order.** `⌃⌘c`/`⌃⌘/` are global
  `ShortcutAction`s (they fire pre-view, like `ScrollHalfPageUp`).
  In-mode keys are intercepted in `on_key_down` after the held/IME gates
  and before the ⌘C/⌘V/encoder path, via a pure table
  `copy_mode_key_action(key, mods) -> Option<CopyModeAction>` beside
  `scrollback_key_action` — a bare `h` falls through to the pty whenever
  VI is off. While the search FIELD is open, the field's own focus
  handle owns the keys (gpui focus routing); the pane view sees nothing.
- **P4 — in-mode ⌘-key behavior**: ⌘C copies the selection and STAYS in
  the mode (matches today's ⌘C exactly); Enter copies and EXITS (tmux
  copy-and-cancel); `y` copies and exits; `y`/Enter with no selection
  no-op (stay). ⌘V is swallowed (pasting into scrollback is
  meaningless). Every other unrecognized key no-ops — nothing leaks to
  the pty while VI is on. That guarantee needs FOUR gates, not one,
  because `on_key_down` is not the only pty writer: (1) the
  `on_key_down` interception (Slice 2); (2) `on_key_up` (view.rs:
  1112-1128) — under kitty `REPORT_EVENT_TYPES` a swallowed press would
  otherwise still emit its release report; (3) the three IME callbacks
  (dead keys and in-flight compositions bypass key listeners entirely —
  see the choke-point fact) — these drop the snap-to-bottom and the pty
  write but STILL run the `ImeState` transitions with the output
  discarded, so marked state always clears (a bare early return would
  strand an in-flight composition and leave the pane keyboard-dead
  after exit); (4) mouse-report suspension (P10). Accepted asymmetries,
  same class as today's intercepted Shift+PageUp: a press whose release
  straddles entry/exit is sent without its pair.
- **P5 — selection keys toggle vim-style.** `v`/`V`/`⌃v` with no live
  selection start one at the vi cursor (Simple/Lines/Block); pressing
  the SAME kind again clears it; a DIFFERENT kind rebuilds the selection
  with the same anchor. The anchor `(Point, SelectionType)` is tracked
  in the handle's copy-mode sub-state (alacritty doesn't expose the
  anchor publicly); motion extension itself is library-side
  (`vi_mode_recompute_selection`).
- **P6 — exit semantics**: leaving copy mode (Esc/`q`/`y`/Enter-yank or
  `⌃⌘c` re-toggle) clears the selection, clears search state, scrolls to
  the bottom (`display_offset` 0), and flips VI off — tmux's
  exit-returns-you-to-live behavior. Entry seeds the cursor at the
  terminal cursor (library behavior).
- **P7 — search semantics**: `⌃⌘/` searches BACKWARD (history-ward —
  the flagship "find the thing that scrolled past" direction); in-mode
  `/` = forward and `?` = backward keep vi meanings. `n` repeats in the
  confirmed direction, `N` reverses; `search_next` wraps at the buffer
  ends (library behavior, pinned in a test). Smart-case regex (library
  default: case-insensitive unless the query has an uppercase). Esc in
  the field closes it and leaves you IN copy mode at the current
  position (D1); Enter confirms: jump to the nearest match
  (`search_next` from the raw vi cursor — at-cursor match included,
  correct for confirm), close the field, stay in copy mode. Re-opening
  is `⌃⌘/` again (or in-mode `/`/`?`). Opening the field enters copy
  mode immediately (highlights are live while typing). No match counter
  in v1 (needs a full-scrollback walk per keystroke; revisit at
  feel-check if its absence grates).
- **P8 — match highlighting is viewport-bounded and recomputed per
  frame.** A third `SnapshotKey` channel: viewport matches (`RegexIter`
  over the visible region ± a small margin, the hyperlink budget
  pattern) + the active match. Per-frame recompute means grid rotation
  can never stale the highlight set — no invalidation bookkeeping.
  `Rc<[Match]>` compares by VALUE (do not "optimize" to pointer
  compare), so a fresh per-frame allocation with equal contents keeps
  the frame cache warm. Two named costs, accepted and watched at
  feel-check: over a STREAMING pane match points shift with rotation
  every frame → full re-plan per throttled frame while a search is live
  (bounded by viewport size); per-cell containment is O(matches) on
  re-plans — bucket matches per row if it shows up. Navigation
  (`n`/`N`) uses `Term::search_next` over the FULL scrollback. Colors
  derive from the existing selection tint + accent (dim tint for
  matches, accent emphasis for the active match); no new theme keys in
  v1 — feel-check may promote them.
- **P9 — the vi cursor replaces the shell cursor while VI is on**
  (alacritty semantics) — and the library nearly does it already:
  `RenderableCursor::new` returns the vi cursor's point (never Hidden)
  while VI is on (mod.rs:2370-2387), and Nice's `viewport_cursor`
  already places `content.cursor` correctly in scrollback with an
  in-viewport guard (element.rs:1826-1846). The only real work is the
  BLOCK shape: set `Config::vi_mode_cursor_style = Some(block)` where
  the term config is built (nice-term-core session.rs:166) — no
  hand-rolling in element.rs. A small per-pane mode badge (`COPY` / the
  query text when searching) renders view-side top-right
  (LaunchOverlay-style, inside the Phase-2 content inset); feel-check
  tunes its look.
- **P10 — orthogonal to every Phase-2 gate.** Copy mode never counts as
  busy, never blocks close, doesn't touch pill status/title aggregation,
  and works identically in a Claude pane (copying from Claude's
  scrollback is a flagship use). Alt screen: entry is allowed (motions
  and search work over the visible grid — vim/less content is
  selectable); the `⌃⌘↑`/`⌃⌘↓` half-page chords keep their existing
  alt-screen no-op. HELD panes: the copy-mode gate runs BEFORE the
  held-pane gate, so copy mode works on a dead pane's output —
  keyboard-selecting what a finished process printed is the pane's
  whole remaining purpose (in-mode Enter means yank-exit there; the
  held gate's dismiss-Enter applies once VI is off). **Mouse reporting
  is SUSPENDED while VI is on**: a mouse-mode TUI (Claude, vim) owns
  the mouse via `reporting_active` at all four mouse gates
  (`on_mouse_down` view.rs:1423, `on_mouse_move` :1516, `on_mouse_up`
  :1574, `on_scroll_wheel` :2129) with Shift as the only local
  override — copy mode acts like that Shift override at all four, so
  wheel/click/drag take the LOCAL branches (scroll the viewport, move
  the vi cursor, select) instead of leaking reports to the pty; tmux
  captures the mouse in copy mode the same way. Accepted edge: a
  press/release pair straddling entry or exit desyncs the app's button
  state (same class as the key-up asymmetry in P4). Mouse: wheel
  scrolls and drag-select still work, a click doesn't exit the mode —
  but NOT independently of the vi cursor: `scroll_display` in vi mode
  clamps the cursor into the viewport and recomputes a live selection's
  end to it (mod.rs:389-408, :870-881), so scrolling with a live
  selection drags its end along — vi behavior, accepted for
  `v`-selections. To keep MOUSE selections benign, mouse-down in copy
  mode also `vi_goto_point`s the click point (the recompute then
  targets where the drag is anyway); any residual wheel-mid-drag tug is
  feel-check territory. Accepted oddity, documented not gated: ⌘↩
  Command Compose still fires shell-side while the pane sits in copy
  mode.

## Slice 1 — engine: vi mode + search on the handle (no UI)

`nice-term-view/src/session_handle.rs` (+ a sibling `search.rs` module
if it reads better):

- Copy-mode API over the term lock, mirroring the scroll block's shape:
  `copy_mode_active()` (reads `TermMode::VI`), `enter_copy_mode()`,
  `exit_copy_mode()` (P6 ordering: clear selection → clear search →
  `scroll_to_bottom` → toggle VI off; lock discipline — alacritty's
  `FairMutex` is non-reentrant, so either take the lock ONCE and inline
  all four steps, or call the sibling methods sequentially, each taking
  its own brief lock — never a sibling call while holding the lock),
  `vi_motion(ViMotion)`, `vi_goto(Point)`, `vi_cursor_point()`,
  `vi_page(up/down, half/full)` — BOTH halves: `ViModeCursor::scroll`
  only computes the target cursor point (vi_mode.rs:190-201, pure
  `#[must_use]`); pair it with `scroll_display(Scroll::Delta(..))` via
  the existing half-page-delta helpers so the viewport pages while the
  cursor keeps its relative row (upstream alacritty pairs them the same
  way). `vi_top()`/`vi_bottom()` (`g`/`G` — `vi_goto_point` to the
  topmost history point / the terminal cursor line).
- Selection toggling per P5: `toggle_copy_selection(SelectionType)`
  with the tracked anchor; `yank() -> Option<String>` (selection_text;
  caller decides stay-vs-exit per P4).
- Search engine per P7/P8: `SearchState { query, regex:
  RefCell<Option<RegexSearch>>, direction, active_match, origin }`
  (the `UrlRegexCache` shape); `set_search_query(&str)` (recompile
  lazily, invalid regex → no matches, never an error surface),
  `confirm_search()` (`search_next` from the RAW vi cursor — at-cursor
  match included), `next_match()` / `prev_match()` — these must NOT
  search from the raw cursor: `search_next`'s accept predicate includes
  the origin (search.rs:164-173/:203-212), so a cursor sitting on the
  active match would return the same match forever. They advance the
  origin one cell off the active match in the search direction
  (`Point::add`/`sub` with `Boundary::None` — wrap-safe) before calling
  `search_next`, then `vi_goto_point` the result and update
  `active_match` (this origin-advance lives in the alacritty APP, not
  the library — Nice supplies it). `clear_search()`,
  `viewport_matches(margin) -> Vec<Match>` for the render path,
  `search_active()` / `active_search_query()` for the badge.
- New handle event `SearchRequested { backward: bool }` (for in-mode
  `/`/`?` — Slice 2 emits, Slice 4 routes).
- Wheel/chord scrolling while VI is on needs NO handle-side clamp:
  `Term::scroll_display` already clamps the vi cursor into the viewport
  at this pin (mod.rs:397-401, upstream tests mod.rs:2522-2573) — and
  recomputes a live selection's end to it (the P10 semantics).
- Tests (headless `Term<VoidListener>` harness, the vt.rs `feed`
  pattern): enter/exit invariants (P6 ordering observable via
  `display_offset`/selection), every `ViMotion` mapping moves as vim
  does on a seeded grid, selection toggle matrix (none/same/different
  kind), yank text across a history boundary, search over seeded
  scrollback (backward-first hit; n/N advance OFF the active match and
  wrap at the buffer ends — `search_next` wraps via full-buffer
  fallback, search.rs:162-173; the origin-advance test is MANDATORY, it
  pins B2's fix), smart-case, invalid-regex no-match, viewport_matches
  bounds.

## Slice 2 — view: in-mode key interception + cursor/badge hooks

`nice-term-view/src/input.rs`:

- Pure `copy_mode_key_action(key: &str, mods: Modifiers) ->
  Option<CopyModeAction>` beside `scrollback_key_action`, with
  `CopyModeAction = Motion(ViMotion) | Top | Bottom | Page{dir, half} |
  ToggleSelection(SelectionType) | Yank | YankStay | SwallowPaste |
  OpenSearch{backward} | NextMatch | PrevMatch | Exit | Swallow`.
  The D3 table lives here in one match, unit-tested exhaustively
  (every binding + the leak-proof default arm). The table ALSO maps
  everything `scrollback_key_action` accepts —
  Shift+PageUp/PageDown/Home/End → `Page`/`Top`/`Bottom` — because the
  swallow-everything default would otherwise kill today's scrollback
  keys exactly while the user is navigating scrollback (they normally
  live inside `dispatch_key`, which the copy-mode gate never reaches).
- Tug on the copy-mode scenario honesty rule: nothing here covers the
  IME path (next bullet) — in-process `dispatch_keystroke` never
  exercises it.

`nice-term-view/src/view.rs`:

- `on_key_down`: the copy-mode gate runs FIRST — before the held-pane
  gate (P10: copy mode must work on a dead pane's output) and before
  the IME gates. When `copy_mode_active()`, translate through
  `copy_mode_key_action` and drive the handle; EVERY key is consumed
  while VI is on (the table's default arm is `Swallow`) — nothing
  reaches ⌘V/⌘C handling or `dispatch_key`. ⌘C maps to `YankStay`,
  Enter to `Yank`, ⌘V to `SwallowPaste` (P4). `OpenSearch` emits
  `SearchRequested` on the handle.
- **`on_key_up` gated on copy mode (P4 gate 2)**: view.rs:1112-1128
  gates only on composing + `REPORT_EVENT_TYPES`; add an early return
  when `copy_mode_active()` so swallowed presses don't leak release
  reports under kitty event-types. The Esc-that-exits edge (press
  swallowed, release encoded after VI is off) is the accepted asymmetry
  P4 names.
- **IME callbacks gated on copy mode (P4 gate 3)**: dead keys and
  compositions reach `ime_set_marked` (:1656) / `ime_commit` (:1673) /
  `ime_unmark` (:1700) WITHOUT passing any key listener. The gates drop
  the snap-to-bottom and the pty write but STILL run the `ImeState`
  transitions (`commit_text` with its output discarded, `unmark` with
  the pending text discarded) — a bare early return would leave marked
  state `Some`, gpui would keep routing every key through the input
  context, and after mode exit the composing gate (:1056) would eat all
  keystrokes: a keyboard-dead pane. Running the transitions output-
  discarded means a composition in flight when `⌃⌘c` fires clears
  itself at commit time. (The gated `ime_set_marked` case is safe as a
  plain skip: Nice never learns of the composition, so `is_composing`
  never arms.)
- **Mouse-report suspension (P4 gate 4 / P10)**: at the four
  `reporting_active(mode) && !shift` gates (`on_mouse_down` :1423,
  `on_mouse_move` :1516, `on_mouse_up` :1574, `on_scroll_wheel`
  :2129), treat `copy_mode_active()` exactly like the Shift override —
  the local branches (viewport scroll, selection, and P10's
  mouse-down `vi_goto_point`) already do what copy mode needs. Local
  (non-reporting) panes' wheel handling is untouched.

## Slice 3 — render: vi cursor + match highlights

`nice-term-view/src/element.rs`:

- Cursor: per P9 the library does the position swap already
  (`RenderableCursor::new` + `viewport_cursor` paint a history-placed
  vi cursor with zero changes) — the slice's cursor work is exactly one
  line: `Config::vi_mode_cursor_style = Some(block)` at the term-config
  build site (nice-term-core session.rs:166).
- `SnapshotKey` grows the third channel: `search_matches:
  Option<Rc<[Match]>>` + `active_match: Option<Match>` (P8), resolved
  per cell in `fill_row` beside the selection/hover checks; tints per
  P8 (derived, `DEFAULT_SELECTION_TINT`-fallback pattern). Mechanical
  fallout: the `test_key` literal (element.rs:2453-2463) feeds the whole
  cache test family — two new fields there.
- Mode badge: a small top-right per-pane overlay div in
  `TerminalView::render` (LaunchOverlay precedent) showing `COPY`, or
  the query while the search field is open / a search is live.
- Tests: cursor-position selection logic and highlight containment as
  pure unit tests where extractable; paint itself is feel-check
  territory (the Phase-2 honesty rule — name what the scenario can't
  see).

## Slice 4 — app: actions, keymap, search bar

`nice-model/src/shortcuts.rs`:

- Two new `ShortcutAction` variants + frozen ids: `CopyMode`/`copyMode`
  (`⌃⌘c`, label "Copy Mode"), `SearchScrollback`/`searchScrollback`
  (`⌃⌘/`, label "Search Scrollback"). `ALL` 34 → 36; completeness-test
  literals updated.
- `RESERVED_COMBOS` 9 → 8: the ⌃⌘/ `FuturePhase` entry is REMOVED
  (doctrine :1009-1015). `reserved_table_covers_the_three_groups`
  updates: len 9 → 8, `count(FuturePhase)` 1 → 0, the `"cmd-ctrl-/"`
  literal assertion is replaced by a default-binding assertion. The
  reserved table now holds only OS chords + Nice-claimed chords — the
  roadmap's last standing reservation is spent.

`crates/nice/src/keymap.rs`:

- Two handlers on the `scroll_active_window_half_page` template
  (effective_pane_id → pane_handle) — MINUS the template's alt-screen
  early-return (keymap.rs:924-926; P10 allows entry on the alt screen):
  `CopyMode` toggles enter/exit; `SearchScrollback` opens the bar
  (backward, P7) — which first ensures copy mode is on, then flips the
  app-side bar state.
- Binding-table tests per the existing style (`bound(&CopyMode,
  "cmd-ctrl-c")`, `bound(&SearchScrollback, "cmd-ctrl-/")`).

New `crates/nice/src/search_bar.rs` + `WindowHostView` wiring
(`app_shell.rs`):

- `SearchBarState { editor: TextFieldEditor, focus: FocusHandle,
  backward: bool, pane_key: (session, window, pane) }` on
  `WindowState`; open on the `SearchScrollback` action or a routed
  `SearchRequested` event; render as an ABSOLUTE-positioned child of
  the focused pane's `relative().overflow_hidden()` wrapper in
  `leaf_element` (app_shell.rs:609-611 — the `corner_ticks` precedent,
  :893-941), anchored bottom-right. That parentage makes zoom, divider
  drags, window resizes, and tree restructures position it correctly
  for free, and keeps it off the TerminalView's focus/bubble path.
  (NOT `pane_content_rect` — that method returns origin-normalized
  whole-content EXTENTS with no per-pane geometry; anchoring off it
  would place the bar over the wrong pane in any split.)
- Key handling via a `dispatch_rename_key`-style translator into
  `text_field::Key`; ⌘C/⌘X/⌘V work (rename precedent). Every edit
  pushes `set_search_query` down the handle (incremental highlighting,
  P8). Enter → `confirm_search()` + close + refocus the pane's view;
  Esc → close + refocus, copy mode stays (P7). The bar closes itself
  when its pane dies, the focused pane changes, OR **its target pane's
  `copy_mode_active()` goes false** — `⌃⌘c` is a global action and
  fires even while the bar holds focus, and Esc/`q`/`y` can end the
  mode after a click back into the pane; without this check the bar
  survives showing a query whose state P6 already cleared. (All three
  are the same stale-key render check → close, never a panic — the
  Phase-2 divider-drag rule.) A click into the pane that leaves the bar
  open just unfocuses it; `⌃⌘/` (or in-mode `/`/`?`) refocuses.
- `window_state.rs`: route `SearchRequested` pane-keyed (the retained
  per-pane subscription path from Phase 2 already delivers it;
  `TerminalEvent` is `#[non_exhaustive]` so the new variant fits). The
  subscription closure has NO `&mut Window` — it sets the bar state +
  `cx.notify()` only, and key focus moves via the stashed
  `window_handle` defer pattern (window_state.rs:948-959) or on the
  next host render. Focusing inline from the closure would silently
  not focus. **The `SearchScrollback` ACTION path has the identical
  constraint** — keymap handlers are App-level closures with no
  `&mut Window` (keymap.rs:379-473) — so the action handler uses the
  same defer for the bar's focus. Note: the routing match's wildcard
  arm means forgetting to route the new variant COMPILES silently —
  the scenario's search leg is the guard.

Recorder/settings: the two rows appear via `ALL` automatically;
recording `⌃⌘/` stops being refused (its reserved entry is gone).

## Slice 5 — selftest scenario + docs

- New `copy-mode` scenario (`input_live.rs`, registered in `app.rs`
  register-only, before `multiwindow`, `Gate::SelfReported` with a
  generous budget):
  1. Seed a pane with numbered scrollback lines (shell `for` loop →
     `settle`), scroll state at bottom.
  2. `⌃⌘c` enters (VI bit set via the handle); type `hjkl`-cluster keys
     and assert ZERO pty leak while in mode (chord_leak-style guard on
     the pty stream).
  3. Motions: `k`/`0`/`$`/`w`/`b`/`g`/`G` move `vi_cursor_point` as
     asserted; `⌃u` pages (display_offset moved); Shift+PageUp also
     pages IN mode (the I4 table rows — today's scrollback keys must
     not go dead).
  4. `v` + motions + `y`: clipboard holds the exact seeded text, VI bit
     off, display back at bottom (P6); typed keys reach the pty again.
  5. `⌃⌘/` opens the bar (bar state assert), type a query that hits a
     seeded line, Enter lands the cursor on the match (offset + point
     assert), `n`/`N` walk matches, Esc → bar closed, VI still on,
     Esc → VI off.
  6. `⌘C` in-mode with a live selection: clipboard updated, VI still on.
  7. Re-`⌃⌘c` toggles out from any state — including with the bar open
     (bar closes, I2).
  What the scenario CANNOT see (named per the Phase-2 honesty rule):
  the IME path (`dispatch_keystroke` never routes through the
  NSTextInputClient — B1's gates are unit-tested on the view and
  hand-checked), chord DELIVERY of `⌃⌘c`/`⌃⌘/` (OS interception blind
  spot), and paint (badge, cursor shape, tints — unit + feel-check).
  Held-pane gate ordering (P10) is unit-tested in the view/keymap
  layer, not staged live.
- `keybind-scheme` scenario: its reserved-chord doc list / any ⌃⌘/
  assertion updates for the promotion (Phase-2 precedent: the reserved
  list is pinned in scenario docs).
- Docs: roadmap Phase 3 section → shipped wording + decision record
  (D1-D3, P1-P10); tracker `docs/tmux-port-progress.html` flips; README
  keyboard table gains the two rows + a copy-mode key table; this plan
  gains its "As shipped" section.

## Ordering

1 → 2 → 3 → 4 → 5. Slice 2 needs 1's handle API; 3 needs 1's search
state (and is render-only — it may overlap 2 if the cycle wants, but
sequential is the default); 4 needs 1-3 (the bar drives the engine and
the badge); 5 last.

## Validation

Automated — the cycle's validator runs these (build + tests are the
gate; log to a file and check `$?`, never pipe `cargo test` through
`tail`/`head`):

1. `cargo build --workspace`.
2. Unit tests (new/updated):
   - session_handle/search: the Slice-1 list (motions, selection
     toggle matrix, yank, exit ordering, search direction/wrap/
     smart-case/invalid-regex, n/N origin-advance, viewport_matches
     bounds).
   - input.rs: `copy_mode_key_action` exhaustive table + default-arm
     swallow + the Shift+Nav rows (I4). view.rs has NO test module
     (TerminalView needs a spawned session + gpui context), so the
     gate DECISIONS — IME-swallow-vs-transition (B1/F3), copy-before-
     held ordering (P10), mouse-suspension predicate (F1) — are
     extracted as pure functions beside `copy_mode_key_action` and
     unit-tested there; the callback WIRING is covered by the scenario
     and the named feel-check items, not by a first-ever TerminalView
     harness.
   - element.rs: highlight containment where extractable; `test_key`
     two-field fallout across the cache test family.
   - shortcuts: 36-action completeness; new default rows (`⌃⌘c`,
     `⌃⌘/`); reserved table 9 → 8 with `FuturePhase` count 0;
     disjointness holds; id round-trips.
   - keymap: binding-table assertions for both actions.
   - search_bar: editor dispatch, open/close/stale-pane close.
3. Targeted `cargo test`: `-p nice-term-view`, `-p nice-model`,
   `-p nice` (keymap, search_bar, window_state); one full
   `cargo test --workspace` before merge.
4. Live selftest: `NICE_SELFTEST=copy-mode <target-dir>/debug/nice`
   plus a re-run of `NICE_SELFTEST=keybind-scheme` and
   `NICE_SELFTEST=splits` (routing touched). Under the worktree lock,
   display awake (`caffeinate -d`). Hard assertions must pass.

Post-merge human feel-check (Nick — after `scripts/rust-install.sh`
under the worktree lock). Chord DELIVERY is only provable by hand:

1. `⌃⌘c` and `⌃⌘/` arrive at the app at all (the ⌃⌘D OS-swallow risk).
2. In a streaming Claude pane: enter copy mode, scroll up, `v`/`y` a
   paragraph — Claude keeps streaming, exit returns to the live bottom.
3. Motion spot-check across the D3 set; `V` and `⌃v` block selection.
4. Search: incremental highlight while typing; Enter lands on the most
   recent match above; `n`/`N` walk; active-match emphasis reads in
   light + dark themes, over translucency, and on the 1x display.
5. Esc ladder: field → copy mode → normal.
6. A kitty-protocol TUI pane (claude): in-mode keys don't leak
   (presses AND releases); exit restores normal typing. Alt screen
   (vim/less): copy mode selects visible content. **Mouse over a
   mouse-reporting pane in copy mode** (the F1 hole no scenario can
   reach — the seeded scenario pane is a plain shell): wheel scrolls
   the viewport locally, click moves the vi cursor, drag selects —
   nothing reaches the running app; exit restores mouse forwarding.
7. **IME/dead keys in copy mode** (the B1 hole no scenario can reach):
   `⌥e` and a CJK input source while VI is on — nothing reaches the
   pty, the viewport doesn't snap to bottom.
8. Copy mode on a HELD pane: enter, select, yank from a dead pane's
   output; after exit, Enter still dismisses the placeholder.
9. Mode badge + vi cursor legibility at a glance; mouse wheel + drag
   selection still behave while in mode (watch for the wheel-mid-drag
   selection tug P10 accepts — flag if it grates); streaming-pane
   search: typing + highlights stay smooth while Claude streams (the
   P8 re-plan cost).
10. Settings ▸ Shortcuts: both new rows render, record, and reset;
    recording `⌃⌘/` is accepted now.
