# Phase 1 — held-`^⌘` keybind scheme

## Superseded (2026-08-11): hjkl ladder revision

D1's and D3's **chord assignments** were revised right after ship. The modifier
SET now selects the verb and `hjkl` selects the direction: bare `^⌘` navigates
containers (`h`/`l` = prev/next pill — the only pill pair — and `j`/`k` =
next/prev sidebar session), `^⌘⇧` = directional pane focus (the `FocusPane*`
actions, bound but inert until Phase 2), `^⌥⌘` = split resize and `^⌥⌘⇧` (Hyper)
= directional pane swap, both reserved rather than bound. `^⌘[`/`^⌘]` and the
`⌘⌥` arrows end up bound to nothing. `^⌘Space` for swap was rejected: it is the
macOS emoji picker, and Space is not a modifier. Everything else below still
holds — the frozen action ids, D2, D4, D5, and the guard/handler architecture.
Half-page scroll moved the same day too, off `^⌘u`/`^⌘d` onto `^⌘↑`/`^⌘↓`,
because macOS's dictionary hotkey swallows a real `^⌘D` keydown before the app
sees it — which also retires this plan's "ships as a default AND is reserved"
quirk. The historical body is left as written.

Roadmap: `docs/tmux-port-roadmap.md` § "Phase 1 — held-modifier keybind
scheme (M) — SHIPPED". Locked decisions (2026-08-05): held `^⌘` + vim keys,
no prefix state machine; `^⌘[`/`^⌘]` for prev/next pill; hold-to-hint overlay
is part of the scheme; never bind `^⌘Q` (lock screen), `^⌘F` (fullscreen),
`^⌘Space` (emoji), `^⌘D` (dictionary).

Status: **SHIPPED** (signed off by Nick 2026-08-11; implemented in four slices
on `phase-1-keybind-scheme`). This is the in-repo copy of the implemented plan;
the "As shipped" section at the end records where the code and the plan differ.
The launch appendix (dev-cycle args) is deliberately not carried over — it
described one run's mechanics, not the design.

## Current-code facts the plan builds on

- `ShortcutAction` (`crates/nice-model/src/shortcuts.rs:42`, 14 variants,
  `ALL` at :80) + `default_bindings()` (:250) + stable `id()` strings (:139,
  **frozen surface**). Store: `ShortcutBindings` in `shortcuts_store.rs`
  (`shortcuts` section of `ui_settings.json`); mutators call
  `keymap::rebuild_keymap` (:242) which does a total
  `clear_key_bindings()` + rebind of table + `non_rebindable_bindings()`
  (`keymap.rs:279-305`).
- **Prev/next pill already exists**: `NextWindow`/`PrevWindow` actions,
  default `⌘⌥→`/`⌘⌥←` (`shortcuts.rs:267-280`), dispatch via
  `keymap.rs:348-353` → `WindowState.window_strip_actions`
  (`ModelWindowStripActions`, `window_strip_actions.rs:110-139`), which
  writes `active_window_id` on the model DIRECTLY. Note:
  `PtyManager::select_next_window/select_prev_window`
  (`pty_manager.rs:613/619`) exist but have NO live caller (tests only) —
  do not route new handlers through them.
- **`⌘1-9` / `^⌘1-9` select-by-index exists nowhere** — greenfield. Pill
  order = `Session.windows` vec order (`nice-model/src/session.rs:31`);
  active pointer = `Session.active_window_id` (:36). No per-session
  last-active history exists (only single "current" slots).
- **The recorder has NO protected-combo guard today.** `decide_capture`
  (`settings/shortcuts_pane.rs:90-114`) only consults `conflicting_action`
  (`shortcuts.rs:462`), which scans the rebindable table. A user can record
  `⌘Q` or `^⌘F` over a fixed accelerator with zero warning. Guarding the
  reserved set is NEW logic, not an extension of an existing check.
- **Pty safety is dispatch order, not `should_encode`**: gpui fires bound
  actions before key listeners reach `dispatch_key`
  (`keymap.rs:50-56` module doc), so every chord this phase binds never
  reaches the terminal view at all. (`should_encode`'s `ctrl && !cmd`
  branch only governs unbound printable keys.)
- **`alacritty_terminal::grid::Scroll` has no half-page variant** (crates.io
  0.26: `Delta/PageUp/PageDown/Top/Bottom`). `^⌘u/d` needs a handle method
  computing `Scroll::Delta(±screen_lines/2)`.
- Modifier observer: `on_window_modifiers_changed` (`keymap.rs:546-561`)
  already watches modifier state per window (sidebar-peek end);
  `peek_relevant_modifiers` (:570) reads live bindings from the store.
  Overlay precedent: `build_peek_overlay` (`sidebar_shell.rs:2255-2273`) —
  absolute-positioned child inside a `.relative()` parent.

## Decisions (RESOLVED — Nick, 2026-08-11)

<!-- PROTECTED --> **D1 — prev/next pill chords.** Change `NextWindow`/
`PrevWindow` **defaults** from `⌘⌥→`/`⌘⌥←` to `^⌘]`/`^⌘[`. Ids stay
frozen; only the default combos change; `⌘⌥←/→` are freed. No dual-chord
installs.

<!-- PROTECTED --> **D2 — `^⌘1-9` is rebindable via ONE template row.**
Everything in the scheme stays rebindable — nothing new goes in the fixed
(non-rebindable) set. A single `ShortcutAction::WindowByIndex` row covers
all nine chords: the user records any chord ending in a digit and the
recorded MODIFIER SET applies to digits 1-9 (design in Slice 1). Nine
separate settings rows are explicitly rejected.

<!-- PROTECTED --> **D3 — `^⌘h/j/k/l` pre-splits.** Actions are named for
their Phase 2 spatial meaning (`FocusPaneLeft/Down/Up/Right`, tmux
`select-pane -L/-D/-U/-R` over the future split tree). Pre-splits: `h`/`l`
alias prev/next pill; `j`/`k` are registered but inert (there is nothing
vertical to act on until splits land — Phase 2 swaps only the handler, no
new bindings or migration).

<!-- PROTECTED --> **D4 — future-phase chords (`^⌘z`, `^⌘v`, `^⌘s`,
`^⌘/`).** No no-op actions now. These chords go in the reserved-combo
guard (Slice 3) so nothing can squat on them before Phases 2/3 claim them.

<!-- PROTECTED --> **D5 — hint overlay trigger.** Overlay appears after
~200ms of `^⌘` held with no key committed (fast chords never flash it);
hides instantly on release or when the modifier set changes.

## Slice 1 — actions, defaults, dispatch (the working set)

New `ShortcutAction` variants + gpui action structs + `shortcut_binding`
match arms + handlers, following the existing end-to-end recipe (enum +
label/id + default row in `nice-model`; action + dispatch in `keymap.rs`;
recorder/settings pick them up via `ALL`):

- `FocusPaneLeft/Down/Up/Right` — defaults `^⌘h/j/k/l` (per D3: `h`/`l`
  route to the existing `WindowStripActions` prev/next path; `j`/`k`
  registered with no-op handlers carrying a Phase-2 comment).
- `LastActiveWindow` — default `^⌘o`. New single-slot
  `prev_active_window_id: Option<String>` on `Session` (`nice-model`),
  marked `#[serde(skip)]` so NO `sessions.json` key appears (`Session`
  derives serde with frozen renames, `session.rs:20-61`; precedent:
  `TermWindow.is_claude_running`). There is no single choke point for
  `active_window_id` writes — add a helper on `Session` (e.g.
  `switch_active_window(id)`: stash the old `active_window_id` into
  `prev_active_window_id` only when it actually changes) and route the
  USER-SWITCH sites through it: `ModelWindowStripActions::select_window` /
  `step_active_window` (`window_strip_actions.rs:119/106`),
  `PtyManager::set_active_window`, the new `select_window_by_index`, and
  the `LastActiveWindow` handler itself (which swaps the two). Clear it in
  `WorkspaceModel::extract_window` when the removed id equals it.
  Structural/seed writes (add_window, lineage insert, rename repair,
  restore seeding) do NOT go through the helper. tmux `last-window`
  semantics, not an MRU stack.
- `ScrollHalfPageUp/Down` — defaults `^⌘u/d`. New `TerminalSessionHandle::
  scroll_half_page_up/down` beside `scroll_page_up` (`session_handle.rs:662`)
  using `Scroll::Delta(±(screen_lines/2).max(1))`, clearing `scroll_accum`
  like the others. Routing — NOTE: no keymap action reaches a terminal
  view today (Shift+nav scrollback lives in the VIEW's own key listener,
  not the keymap), so use the seam that does exist:
  `PtyManager::term_window_handle(session_id, term_window_id) ->
  Option<Entity<TerminalSessionHandle>>` (`pty_manager.rs:1244-1253`).
  Handler: `with_active_state` → `workspace.active_session_id()` + that
  session's `active_window_id` → `s.ptys.term_window_handle(..)` →
  `handle.update(cx, |h, hcx| { h.scroll_half_page_up(); hcx.notify(); })`
  — the `hcx.notify()` is `perform_scrollback`'s repaint discipline
  (`view.rs:949-959`). Alt-screen gate at the same level:
  `handle.read(cx).term()` (pub, `session_handle.rs:471`) →
  `*term.lock().mode()` (the `current_mode` pattern, `view.rs:904-911`);
  on `TermMode::ALT_SCREEN` the handler no-ops — the chord never encoded
  to the pty, so there is nothing to fall through TO (contrast
  Shift+PageUp, which falls through and encodes). Held sessions work on
  this path automatically (the handle's scroll methods no-op pre-spawn).
- `NextWindow`/`PrevWindow` default flip per D1 (no new actions; only the
  two rows in `default_bindings()` change).
- `WindowByIndex` per D2 — one action, nine chords:
  - One `ShortcutAction::WindowByIndex` variant, id `"windowByIndex"`
    (new id — additive to the frozen surface), label "Window 1-9",
    default stored combo = `^⌘` + `"1"`. Convention: the stored digit is
    always normalized to `"1"`; the combo means "these modifiers + digits
    1-9". Settings row renders the chord as `⌃⌘1…9`.
  - `keymap.rs` expands the single stored combo into nine
    `gpui::KeyBinding`s (modifiers + `1`..`9`) targeting a data-carrying
    `SelectWindowIndex { index: u8 }` action — declared with
    `#[derive(Clone, PartialEq, Action)]` + `#[action(namespace = nice,
    no_json)]` (the `no_json` attr avoids the derive's serde/schemars
    requirement; `crates/nice` has no schemars dep). `KeyBinding::load`
    takes `Box<dyn Action>`, so nine bindings each carrying a distinct
    index work; `cx.on_action::<SelectWindowIndex>` receives the bound
    instance. The nine-binding expansion is a special case in BOTH
    keymap paths (`table_bindings` AND `rebuild_keymap`), and
    `shortcut_binding`'s one-binding-per-action match (`keymap.rs:501-523`)
    grows a `WindowByIndex` carve-out. Handler: new
    `select_window_by_index(model, i)` on the `WindowStripActions` trait,
    implemented on `ModelWindowStripActions` mirroring
    `step_active_window`/`select_window` (`window_strip_actions.rs:86-139`)
    — index into the active session's `Session.windows` order,
    out-of-range = no-op. (NOT via `PtyManager` — see Current-code facts.)
  - Recorder special case for this row (`shortcuts_pane.rs`): a capture
    only commits if the pressed key is a digit 1-9; the stored digit
    normalizes to `"1"`. Any digit + the recorded modifiers rebinds all
    nine.
  - `conflicting_action` (`shortcuts.rs:462`) treats `WindowByIndex` as
    claiming its modifiers + EVERY digit 1-9 (both directions: recording
    `^⌘3` elsewhere conflicts with `WindowByIndex`, and recording
    `WindowByIndex` to modifiers that collide with an existing
    digit-keyed binding conflicts too).

Store-migration note: the store persists the FULL map on any change
(`shortcuts_store.rs:229-244`), so a user who ever rebound ANYTHING has
the old `cmd-alt-right/left` combos stored — for them the D1 flip does
not land and the new action ids load unbound (frozen rule 5, no seeding).
<!-- PROTECTED --> This consequence is ACCEPTED — Nick runs defaults; do
not add migration/seeding logic for it. The five frozen load rules in
`shortcuts_store.rs:22-39` are untouched; `windowByIndex` is a new id,
absent from any existing store file, so old `ui_settings.json` files load
unchanged.

## Slice 2 — reserved-combo guard in the recorder

New `CaptureOutcome::Reserved` in `decide_capture`
(`shortcuts_pane.rs:90-114`), checked BEFORE `conflicting_action`:

- Reserved set, defined as data in `nice-model` beside
  `conflicting_action` (gpui-free, unit-testable), one table with a reason
  string per entry:
  - (a) the fixed-accelerator chords from `keymap::non_rebindable_bindings`
    (⌘Q, ⌘N, ⌘W, ⌘,, `^⌘F`) — `keymap.rs` derives its fixed installs from
    this shared table where they overlap, so the two can't drift. The
    sixth entry there (context-scoped Esc@`SidebarShell`) stays
    keymap-only: plain Esc is uncapturable anyway (`decide_capture`
    cancels on it, `shortcuts_pane.rs:102-104`);
  - (b) macOS-system-reserved: `^⌘Q` (lock screen — the OS intercepts it),
    `^⌘Space` (emoji picker), `^⌘D` (dictionary);
  - (c) per D4, future-phase chords: `^⌘z`, `^⌘v`, `^⌘s`, `^⌘/`.
- Recorder UI shows the reason ("reserved: macOS emoji picker", "reserved
  for a future Nice feature") instead of committing. No store changes; the
  frozen load rules are untouched.

## Slice 3 — hold-to-hint overlay

tmux `display-panes`, live while `^⌘` is held (per D5, ~200ms delay):

- Trigger: extend `on_window_modifiers_changed` (`keymap.rs:546`) — when
  modifiers become exactly ctrl+cmd, start a ~200ms timer on the app
  executor (`cx.background_executor().timer` / foreground equivalent — NOT
  `smol::Timer`, per the repo's GPUI dispatcher rule); if the modifiers
  are still exactly ctrl+cmd when it fires, set the hint flag; any
  modifier change clears the flag (and pending timer) immediately. The
  flag lives in `nice-model` window-level UI state (NOT persisted —
  mirrors `sidebar.peeking()`), but the pending timer `Task`/generation
  counter CANNOT (`nice-model` is gpui-free) — it lives on `WindowState`
  (or a Global). `on_window_modifiers_changed`'s `(event, &mut App)`
  signature suffices: state via `WindowRegistry::active_state`, timer via
  `cx.spawn` + the executor timer; the observer present-kicks the window
  like the peek path.
- The hint modifier pair is ctrl+cmd as long as the scheme's bindings use
  it; derive it from the live `FocusPaneRight`/`NextWindow` binding
  modifiers via the store (the `peek_relevant_modifiers` pattern,
  `keymap.rs:570`) so a user who rebinds the scheme keeps a working
  overlay.
- Render: index badges (1-9) on the pill chips in `WindowToolbarView`
  (`toolbar.rs`) using the absolute-overlay-in-relative-parent pattern
  from `build_peek_overlay` (`sidebar_shell.rs:2255`). Badge = the digit
  that jumps to that pill. Pre-splits that is the whole hint story;
  Phase 2 reuses the same flag for pane numbers.
- No new keymap entries — pure modifier observation, so it can never
  swallow a chord.

## Slice 4 — docs + selftest scenario

- New `keybind-scheme` selftest scenario (`crates/nice/src/input_live.rs`,
  registered in `app.rs`; keystrokes via `Window::dispatch_keystroke`, no
  Accessibility grant needed — gpui runs action bindings on that path).
  CAUTION: the `scrollback-keys` pattern alone is NOT enough — it opens a
  bare view with no keymap, no `WindowState`, no registry, so every `^⌘`
  chord would silently no-op and the 0-bytes assertion would pass
  vacuously. The scenario must additionally: call `install_shortcuts` and
  seed `ShortcutBindings::with_defaults` at a temp path (the
  `run_selftest` seam, `shortcuts_store.rs:152`); stand up a `WindowState`
  registered via `WindowRegistry::register`; and spawn the capture-tee
  child THROUGH that state's `PtyManager` (`spawn_window`,
  `pty_manager.rs:1334-1338`) so `term_window_handle` resolves. Then seed
  two+ pills; assert `^⌘]`/`^⌘[` cycle, `^⌘2` jumps by index, `^⌘o`
  bounces between the last two, `^⌘u` half-page scrolls, and the
  capture-tee pty shows 0 bytes for every chord while a plain `u` still
  encodes.
- Docs: roadmap Phase 1 section → shipped wording recording D1-D5;
  tracker `docs/tmux-port-progress.html` `data-status` flips + decisions
  list entries; root README keyboard table; crates/README shortcut notes.

## Ordering

1 → 2 → 3 → 4. Slice 2 depends on Slice 1 only for the shared
reserved-table plumbing; Slice 3 is independent of 2; Slice 4 last.

## Validation

Automated — the cycle's validator runs these (build + tests are the gate;
log to a file and check `$?`, never pipe `cargo test` through
`tail`/`head`):

1. `cargo build --workspace`.
2. Unit tests (new/updated):
   - defaults table: every new action has a row; chord spellings;
     id round-trips (`from_id(id(a)) == a`) including `windowByIndex`.
   - `decide_capture` matrix: every reserved chord (all three groups) →
     `Reserved`; benign chords still `Commit`; intra-table conflicts still
     `Conflict`; `WindowByIndex` recording rejects non-digit keys and
     normalizes the stored digit.
   - `conflicting_action`: digit-expansion both directions.
   - `select_window_by_index` bounds (out-of-range no-op).
   - `prev_active_window_id`: set on switch, swap-on-`LastActiveWindow`,
     cleared when the pointed-at window closes.
   - half-page delta math (`screen_lines/2`, min 1, sign per direction);
     alt-screen no-op.
   - keymap expansion: one `WindowByIndex` combo produces nine bindings
     with the stored modifiers.
3. Targeted `cargo test`: `-p nice-model` (shortcuts, session,
   workspace_model), `-p nice` (keymap, shortcuts_store, shortcuts_pane,
   window_strip_actions, pty_manager), `-p nice-term-view`
   (session_handle) during fix rounds; one full
   `cargo test --workspace` before merge.
4. Live selftest scenario: `NICE_SELFTEST=keybind-scheme
   <target-dir>/debug/nice` — no app bundle or install needed (the
   scenario is hermetic and needs no Accessibility grant; note the shared
   target-dir redirect puts the binary under the MAIN checkout's
   `target/`). Run under the repo worktree lock
   (`scripts/worktree-lock.sh acquire <op>` … `release`) with the display
   kept awake (`caffeinate -d`). Hard assertions must pass.
   Frontmost/Space postconditions may defer if another app owns the
   display Space (environmental, per the scenario conventions in
   `input_live.rs`).

Post-merge human feel-check (Nick — NOT part of the automated cycle;
after `scripts/rust-install.sh` under the worktree lock):

1. Hold `^⌘` — after ~200ms pill index badges appear; release — instantly
   gone; a fast `^⌘]` never flashes them.
2. `^⌘[`/`^⌘]` cycle pills with OS key-repeat when held; `^⌘1-9` jump
   directly; `^⌘o` bounces between the last two pills.
3. `^⌘u`/`^⌘d` half-page through scrollback in a scrolled shell; in vim
   (alt screen) they do nothing and vim receives nothing.
4. Settings → Shortcuts: recording `^⌘Q`, `^⌘Space`, `^⌘D`, `^⌘F`, `^⌘z`
   is refused with a reason; rebinding "Window 1-9" via any digit works;
   reset-to-default shows the new `^⌘[`/`]` defaults.

## As shipped

Four slices, in the plan's order. Everything above landed as written except for
these, which a reader comparing plan to code will notice:

- **`⌃⌘D` is both a default and a reserved combo.** The plan makes it
  `ScrollHalfPageDown`'s default (Slice 1) *and* a group-(b) macOS-reserved
  chord (Slice 2). Implemented verbatim: it ships as a working default that the
  recorder will never let you re-record by hand. Reserved wins over the
  intra-table conflict check, pinned by two tests in `nice-model`'s `shortcuts`
  and documented at the table entry. Worth a human decision if it ever grates.
- **The hint overlay's modifiers come from the nav chords, its badges from
  `WindowByIndex`.** D5 derives the hold from `FocusPaneRight` / `NextWindow`,
  while the digits it paints belong to `WindowByIndex`. A user who rebinds ONLY
  `WindowByIndex` therefore still gets the badges on the nav-chord hold. Noted
  at `hint_relevant_modifiers` as a Phase 2 problem, when panes (not just
  pills) get numbers.
- **The scenario needed a gpui fix first.** `input_live::dispatch_key` drove
  keystrokes through `WindowHandle<V>::update`, which leases the root view for
  the whole call; `dispatch_key_event` re-draws a dirty window before
  dispatching, and that draw re-enters the root — a double-lease abort whenever
  a chord's handler notifies mid-dispatch. It now routes through
  `AnyWindowHandle::update`, which leases nothing. `scrollback-keys` shares the
  helper and was aborting intermittently for the same reason.
