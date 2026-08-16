# Phase 2 — splits

Roadmap: `docs/tmux-port-roadmap.md` § "Phase 2 — splits (L, the core
investment)". Status: **SHIPPED** (signed off by Nick 2026-08-12;
implemented in five slices on `phase-2-splits`) — decisions resolved with
Nick 2026-08-12; single-Fable plan review same day (2 blocking + 6
important + 3 nit findings, all folded in below); Nick approved the plan
including the three flagged judgment calls (clean-Claude-exit kind flip,
break-pane env wart accepted, busy-close ORs across leaves).

This is the in-repo copy of the implemented plan (Phase 1 precedent:
`docs/plans/phase-1-keybind-scheme.md`). The "As shipped" section at the
end records where the code and the plan differ. The body below is left as
written, including its "Current-code facts" line numbers — those were
grounded against `495dbc0` and describe the code BEFORE this phase.

Vocabulary: sidebar row = `Session`, upper-bar pill = `TermWindow`. The name
`Pane` was freed by Phase R exactly for this phase: a **Pane is one leaf of a
pill's split tree**. Pre-splits, every pill is a single-leaf tree.

## Current-code facts the plan builds on

Grounded 2026-08-12 against main `495dbc0` (Explore sweep; line numbers from
that reading).

- **The one-view-per-pill assumption lives in `WindowHostView`**
  (`crates/nice/src/app_shell.rs`): `active_window_target()` (:485-489)
  resolves ONE `(session_id, term_window_id)`; `cache: HashMap<String,
  Entity<TerminalView>>` (:249-251) is keyed by `term_window_id`; `render()`
  (:618-722) mounts exactly one cached view (:677-693); focus follows via
  `window.focus(&fh, cx)` on `activation_changed` (:702-708) off the scalar
  `last_active: Option<(String, String)>` (:255); the present-kick is
  re-pointed to the single active handle (:666-672);
  `PtyManager::activate_term_window` is called reactively from render when
  the target changes (:664). Cache eviction (`stale_cache_ids`, :493-517) is
  id-based and generalizes to N mounted views unchanged.
- **`TerminalView` is MOSTLY view-cardinality-agnostic** (`nice-term-view/
  src/view.rs`): each instance owns its own `FocusHandle` (:637, `Focusable`
  at :1758) and, with `set_auto_refit(true)` (doc at :244), refits its
  OWN painted bounds → its own pty's `TIOCSWINSZ` independently. ONE
  single-instance assumption exists: every view grabs key focus on its
  FIRST render (`focused_once`, :1774-1778) — mounting several fresh views
  in one pass means the last-rendered one steals focus (Slice 3 handles
  this).
- **`TerminalSessionHandle` is view-independent** (`session_handle.rs`,
  lib.rs:14-17): a pty keeps running and draining events with zero views
  attached. The pty layer already supports N live ptys per OS window — one
  per `(session_id, term_window_id)` — with only the active one painted.
  What's missing for splits is a third key level, not new lifecycle.
- **Model** (`nice-model/src/session.rs`, `term_window.rs`): `TermWindow`
  (term_window.rs:34-67) is `{ id, title, kind, is_alive, status,
  waiting_acknowledged, is_claude_running (skip), cwd, title_manually_set }`
  — flat, no pane concept anywhere. `Session.windows: Vec<TermWindow>`
  (serde `"panes"`, frozen), `active_window_id` (serde `"activePaneId"`),
  `prev_active_window_id` (`#[serde(skip)]`).
  `switch_active_window` (session.rs:129-136) is the user-switch choke
  point. Status aggregation (`status()`, `has_claude()`, …) derives purely
  from `windows` (session.rs:169-195), pinned by the sidebar/toolbar
  no-drift test block (:357-451). All ids are plain `String`s (no newtypes
  anywhere in `nice-model`). MODEL-level serde spellings are snake_case
  (`"active_pane_id"` session.rs:35, `"parent_tab_id"` :64) — the frozen
  camelCase spellings (`"activePaneId"`, `"parentTabId"`, …) live ONLY in
  the persisted layer; don't copy one into the other. `TermWindow` derives
  `PartialEq, Eq, Hash` (term_window.rs:33) and so does `Session`
  (session.rs:20); no `HashSet`/`HashMap` keyed on either was found in the
  workspace.
- **Add/remove/reorder** live in `workspace_model.rs`:
  `neighbor_active_window_id` (:439-447) is the ONLY refocus algorithm in
  the codebase — index-neighbor, not spatial. `extract_window` (:519-544)
  and `insert_window` (:550-580) are the tested cross-window move seams;
  `add_window` (:591-608) is the "Terminal N" auto-naming path (terminal
  kind only — Claude windows come from a separate path preserving the
  ≤1-running-Claude-per-session invariant).
- **Pty layer** (`crates/nice/src/pty_manager.rs`): `sessions:
  HashMap<session_id, HashMap<term_window_id, WindowPty>>` (:242);
  `WindowPty` (:198-201) wraps exactly ONE `Entity<TerminalSessionHandle>`.
  `ensure_active_window_spawned` (:1698-1747) spawns only the active
  window; `activate_term_window` (:1762-1772) = `set_active_window` +
  spawn, called only from `WindowHostView::render`. Keymap handlers that
  move `active_window_id` mutate the model ONLY; spawn + focus is a side
  effect of the next render — the established action pattern.
  `window_exited` (:762-804) is the 5-step exit ordering ending in
  `neighbor_active_window_id` refocus and (if the session empties) the
  dissolve cascade; `window_held` (:814-829) flips window-level
  `is_alive = false`, `status = Idle`, `is_claude_running = false`.
  `term_window_handle(session, term_window)` (:1247) resolves one handle
  per pill — external callers: the `ScrollHalfPage*` handlers
  (`keymap.rs`), the event-subscription sweep (`window_state.rs:822`), and
  `dispatch_command_compose` (`window_state.rs:2490`). (A scenario-local
  wrapper, `session_lifecycle.rs:298`, also calls `activate_term_window`;
  the "only from render" claim holds for production code.)
- **Event subscription/routing is window-keyed and permanent**
  (`crates/nice/src/window_state.rs`): `subscribe_spawned_windows`
  (:812-892) sweeps `PtyManager::live_window_keys` (:1264-1273), creates
  ONE subscription per `(session_id, term_window_id)` deduped via
  `subscribed_windows` `"{t}:{p}"` strings (:818-825), CAPTURES `(t, p)`
  in the closure, routes every event through
  `route_terminal_event(model, selection, &t, &p, event)` (:846-848), and
  `.detach()`es (:892) — subscriptions can never be removed or re-keyed.
  Title→status parsing branches on WINDOW kind inside
  `window_title_changed` (pty_manager.rs:453-514), gated on
  `is_claude_running`.
- **Busy/close gates and Command Compose resolve kind at WINDOW level**
  (`window_state.rs`): `window_is_busy` (:2440-2464) has a dead-first
  guard on window `is_alive` and runs the `tcgetpgrp` foreground-child
  probe only for `Terminal`-kind windows, against the window's single
  handle; `compose_route` (:2512-2525) takes the window's
  `kind`/`is_alive` + `shell_has_foreground_child(session, window)`.
- **Keymap** (`nice-model/src/shortcuts.rs`, `crates/nice/src/keymap.rs`):
  `ShortcutAction` has 22 variants (`ALL`, shortcuts.rs:152-175) with
  frozen `id()` strings (:234-263). `RESERVED_COMBOS: [ReservedCombo; 20]`
  (:821-953), three `ReservedKind` groups; the `FuturePhase` entries this
  phase claims or frees: `⌃⌘Z`, `⌃⌘V`, `⌃⌘S` (:854-885) and the eight
  ladder rungs `⌃⌥⌘{h,j,k,l}` (resize) + `⌃⌥⌘⇧{h,j,k,l}` (swap)
  (:889-952). `⌃⌘/` stays reserved (Phase 3). A test pins
  `RESERVED_COMBOS` ∩ `default_bindings()` = ∅ (test at :1470, doc at
  :815-820) — promoting a reserved combo to a default REQUIRES removing
  its reserved entry — and
  `the_focus_rung_is_bound_and_the_phase_two_rungs_are_reserved` (:1417)
  pins the rungs AS reserved, so it must be updated in the same change.
  Modifier constants `Modifiers::CONTROL_ALT_COMMAND[_SHIFT]` already
  exist (:326-339). In `keymap.rs`: `actions!` list (:80-105) already
  declares `FocusPaneLeft/Down/Up/Right` structs (:97-100); their handlers
  are registered EMPTY (:441-444) with a comment saying Phase 2 fills the
  bodies in — no binding moves, nothing migrates. `table_bindings`/
  `rebuild_keymap` (:235, :270-299) derive bindings from
  `default_bindings()` + the live store, so a new defaults row is picked up
  at boot and on live rebind with no separate table edit. Handler pattern
  to copy: `NextWindow`/`PrevWindow` (:396-401) →
  `with_active_state` → `window_strip_actions` model mutation.
- **Persistence**: two layers. `nice-model/src/persisted.rs`:
  `PersistedTermWindow { id, title, kind, cwd, title_manually_set }`
  (:38-50) — no layout field; `PersistedSession` (:56-85) with frozen
  spellings `"activePaneId"`/`"panes"`/`"parentTabId"`; forward-compat
  decode (unknown keys ignored) is tested (:339-361).
  `crates/nice/src/session_store.rs`: `CURRENT_VERSION = 3` (:76), no
  version gate on read ("tolerant by SHAPE", :14-18); `sidebar_mode`/
  `sidebar_width` on `PersistedWindow` (:123,129) are the exact precedent
  for adding an optional field with `#[serde(default,
  skip_serializing_if = "Option::is_none")]` and no version bump.
- **Divider precedent** (`crates/nice/src/sidebar_shell.rs`): the sidebar
  resize handle is the complete pattern — invisible 6pt hit zone +
  `CursorStyle::ResizeLeftRight` (`build_resize_handle`, :2210-2222);
  origin + EFFECTIVE-width baseline captured on mouse-down, double-click
  resets (:1363-1388); drag tracked on the ROOT div's mouse-move/up so it
  survives leaving the handle, with missed-mouse-up detection (:1390-1428);
  clamp on move (`clamp_sidebar_width`, :187-199); commit only if moved.
  Mins: `SIDEBAR_MIN_WIDTH = 160`, `TERMINAL_MIN_WIDTH = 300` (:2506-2507).
- **Zoom precedent: none.** `niceties_zoom.rs` is font zoom; `⌃⌘F` is OS
  fullscreen. The closest structural precedent is `WindowHostView`'s own
  conditional content swap.
- **Frozen surfaces**: control-socket wire keys `"tabId"`/`"paneId"`
  (control_socket.rs:606-672) and env vars `NICE_TAB_ID`/`NICE_PANE_ID`
  (pty_manager.rs:1321-1322, :1961-1965) — **`paneId`/`NICE_PANE_ID`
  already mean the PILL id** (pre-Phase-R spelling), read back by
  `shell_inject.rs`, `claude_hook_installer.rs`, `skill_installer.rs`.
  AX labels incl. `WINDOW_STRIP_ROOT_LABEL = "nice-pane-strip-root"`
  (app_shell.rs:69,73). `sessions.json` keys above. All 22 action id
  strings. `ui_settings.json` load rules 1-5 (shortcuts_store.rs:23-40):
  new action ids load unbound for users who ever rebound (accepted
  precedent from Phase 1).
- **Test infra** (`crates/nice/src/input_live.rs`): the `keybind-scheme`
  scenario (:1247+) drives a seeded fixture via
  `Window::dispatch_keystroke`, asserting model movement AND zero pty leak
  per chord. Reusable helpers: `chord_leak` (:1120-1130), `nav_chord`
  (:1160-1185), `session_chord` (:1191-1216), `freed_chord` (:1223-1245).
  Known blind spot: gpui-level injection cannot see OS chord interception —
  hand-testing is the only gate for chord DELIVERY.

## Decisions (RESOLVED — Nick, 2026-08-12)

<!-- PROTECTED --> **D1 — any pill splits; panes are plain shells.** Every
`TermWindow` (Claude or terminal kind) can be split. A new pane always runs
a plain shell spawned in the focused pane's cwd; Nice never spawns Claude
into a split pane, so the ≤1-running-Claude-per-session invariant is
untouched. Claude stays in its original pane; the pill's status/spinner
aggregates across the tree (see Slice 2). This is the core use case: Claude
in one pane, a working shell beside it.

<!-- PROTECTED --> **D2 — divider-mnemonic split chords; no
"vertical"/"horizontal" anywhere.** `^⌘-` = **Split Down** (stacked, the
divider looks like `-`); `^⌘\` = **Split Right** (side-by-side, the `|`
key). Settings labels are "Split Down" / "Split Right"; the words
"vertical" and "horizontal" appear nowhere in UI, action names, or ids
(vim and tmux assign them opposite meanings — sidestep the war entirely).
The `⌃⌘V`/`⌃⌘S` reserved entries are REMOVED and those chords end bound to
nothing (like `^⌘u`/`^⌘d` after the ladder revision).

<!-- PROTECTED --> **D3 — scope: core + break-pane; even-layout presets
deferred.** Ships: split, directional focus, directional resize,
directional swap, zoom, break-pane-to-new-pill, spatial close-refocus,
layout persistence. Deferred: tmux `select-layout` presets (revisit when
the tree exists and the need is felt).

### Plan-level decisions (mine — flag at sign-off if any grates)

- **P1 — binary tree.** `PaneLayout` is a binary tree: `Leaf(Pane)` |
  `Split { orient, ratio, first, second }`. Every split action bisects one
  leaf; tmux's model is equivalent in practice and the geometry, resize,
  and persistence stay simple. Orientation enum is `SplitOrient::{Beside,
  Stacked}` (D2 naming: no vertical/horizontal).
- **P2 — pane identity stays OFF the frozen surfaces.** Pane ids are plain
  `String`s internal to the model + pty map. Every pane's pty gets the SAME
  `NICE_TAB_ID`/`NICE_PANE_ID` (= session id / pill id) as today — socket
  messages from any pane still route to the right pill, which is where
  status lives. No new env var, no new wire key. Pane-level addressing is
  deferred to Phase 5 (`nice` CLI `select-pane`), which can mint a new name
  then; `paneId`/`NICE_PANE_ID` permanently mean "pill". One accepted
  exception: after break-pane the moved pty's env still names the SOURCE
  pill (fixed at fork) — see the Slice 2 break-pane bullet.
- **P3 — break-pane chord `^⌘b`, refused on the Claude pane.** `^⌘b`
  ("break") is unclaimed (not reserved, not a default, no digit-expansion
  overlap). Break-pane extracts the focused SHELL pane into a new
  terminal-kind pill inserted after the current one, moving the live pty
  handle; focus follows to the new pill (tmux `break-pane` behavior).
  No-op when the focused pane is the Claude pane (a Claude leaf cannot
  become a pill through this path — pill kind stays coherent) or when the
  pill has a single leaf.
- **P4 — zoom is a transient render flag.** `zoomed: bool` on `TermWindow`
  (`#[serde(skip)]`, never persisted — mirrors `is_claude_running`).
  Zoomed = only the focused pane is painted, full-size; all ptys stay
  live. Any structural or focus change (split, close, swap, break,
  directional focus, pane click) un-zooms first, then applies — tmux's
  default `select-pane`-unzooms behavior, generalized. `^⌘z` toggles.
  *As shipped:* the "pane click" trigger is vacuous and therefore inert —
  zoom paints only the focused pane, so the sole pane a click can reach is
  the one already focused, and a click there is not a focus change. It
  deliberately does NOT un-zoom: it would break mouse selection in a zoomed
  pane, since the press that sets the anchor would reflow the pane under
  the cursor. tmux behaves the same way. Flag at the feel-check if the
  other reading is wanted.
- **P5 — directional focus does NOT wrap and does NOT fall through to pill
  nav.** `^⌘⇧h` with no pane to the left is a no-op — bare `^⌘h/l` is the
  pill rung and stays the way to leave the pill. (Phase 1's D3 "h/l alias
  prev/next pill" applied to the pre-splits bare rung only; the ⇧ rung was
  always pane-focus.)
- **P6 — min pane size refuses, resize clamps.** New constants
  `PANE_MIN_WIDTH`/`PANE_MIN_HEIGHT` (start ~120×80 px; feel-check tunes).
  A split that would produce a pane under the mins is a no-op; divider
  drag and `^⌥⌘hjkl` resize clamp against them. `TERMINAL_MIN_WIDTH` (the
  window-level min) is unchanged.
- **P7 — resize semantics: nearest matching ancestor.** `^⌥⌘h/l` adjusts
  the ratio of the nearest ancestor `Beside` split (moving the divider
  left/right by a fixed step, ~40px equivalent); `^⌥⌘j/k` the nearest
  `Stacked` ancestor. No matching ancestor → no-op. Which side of the
  ratio moves follows which child the focused pane sits in, so "resize
  left" always moves the focused pane's relevant edge left — tmux
  `resize-pane -L/-D/-U/-R` semantics.
- **P8 — swap swaps leaves, not subtrees.** `^⌥⌘⇧hjkl` finds the
  directional neighbor leaf (same algorithm as focus) and swaps the two
  `Pane` payloads in place; ratios and structure don't move. Focus follows
  the moved pane (tmux `swap-pane` keeps focus on the same content).
- **P9 — pill title follows the ACTIVE pane; status aggregates by OR.**
  The pill's displayed title tracks its focused pane's pty title (tmux
  window-title behavior). Busy/thinking status aggregates across ALL the
  pill's pane handles (OR), so a manually-run `claude` in a shell pane
  still lights the pill; `kind`/`is_claude_running` semantics stay driven
  by the Claude leaf. The session-level aggregation invariant
  (session.rs:169-195) is untouched — it still reads per-`TermWindow`
  fields.

## Slice 1 — `nice-model`: the pane tree + persistence shape

New `crates/nice-model/src/pane_layout.rs` (pure data, gpui-free, inline
`#[test]`s like the rest of the crate):

- `Pane { id: String, kind: TermWindowKind, cwd: Option<String> }` — leaf
  payload. `kind` marks the (at most one) Claude leaf in a Claude pill;
  every other leaf is `Terminal`. `cwd` mirrors `TermWindow.cwd`'s
  OSC-updated role, per pane, for restore.
- `PaneLayout { Leaf(Pane) | Split { orient: SplitOrient, ratio: f32,
  first: Box<PaneLayout>, second: Box<PaneLayout> } }` (P1). Ratio =
  `first`'s share, clamped to a sane band on every write.
- Pure mutations, each returning enough for the caller to react:
  - `split(target_pane_id, orient, new_pane) -> bool` — replace the leaf
    with a `Split` at ratio 0.5; new pane is `second` (down/right of the
    target, matching D2's verbs).
  - `remove(pane_id) -> Option<Pane>` — collapse the parent split to the
    sibling subtree.
  - `swap(a_id, b_id) -> bool` — swap leaf payloads (P8).
  - `resize(focused_pane_id, direction, delta) -> bool` — nearest
    matching-orientation ancestor per P7; clamps (P6 needs px context, so
    the px→ratio conversion and min enforcement live at the call site in
    `crates/nice`; the model API takes a ratio delta and a clamp band).
  - Queries: `leaves() -> Vec<&Pane>`, `contains(pane_id)`,
    `single_leaf() -> Option<&Pane>`.
- Pure geometry, shared by render, hit-testing, focus, and refocus:
  - `leaf_rects(bounds, divider_px) -> Vec<(pane_id, Rect)>` — recursive
    rect assignment (plain f32 rect type local to the crate; `nice-model`
    stays gpui-free).
  - `directional_neighbor(rects, from_id, direction) -> Option<pane_id>` —
    the leaf whose rect is adjacent in that direction with the largest
    shared edge overlap, tie-broken by centroid distance. Used by focus
    (`^⌘⇧hjkl`), swap (P8), and…
  - `spatial_refocus(rects_before_close, closed_id) -> Option<pane_id>` —
    nearest surviving leaf by shared-edge overlap with the closed rect,
    falling back to centroid distance. This replaces index-neighbor
    semantics for PANE closes only; pill closes keep
    `neighbor_active_window_id` untouched.
- `TermWindow` grows `layout: PaneLayout` (constructor: single leaf whose
  `Pane.id` == a fresh pane id, kind/cwd copied from the window),
  `active_pane_id: String`, and `zoomed: bool` (`#[serde(skip)]`, P4).
  Model-serde additions carry `#[serde(default)]` shims (snake_case
  spellings — model serde is NOT the frozen persisted layer) so any
  existing model-level serde use keeps decoding. **Derive impact**:
  `ratio: f32` is not `Eq`/`Hash`, so the `Eq, Hash` derives come OFF
  `TermWindow` (term_window.rs:33) and `Session` (session.rs:20) —
  verified unused as map/set keys; `PartialEq` stays. Invariants
  (tested): `layout` always contains `active_pane_id`; a tree contains AT
  MOST one Claude leaf; `kind == Claude` iff a Claude leaf exists (a
  terminal pill's tree contains none). "Exactly one" is deliberately NOT
  the invariant — a Claude leaf can exit out of a multi-pane pill (Slice
  2), which removes the leaf and flips the pill's `kind` to `Terminal`.

Persistence (same slice — it's the model's shape):

- `persisted.rs`: `PersistedTermWindow` grows `layout:
  Option<PersistedPaneLayout>` and `active_pane_id: Option<String>`, both
  `#[serde(default, skip_serializing_if = "Option::is_none")]` — the
  `sidebar_mode`/`sidebar_width` precedent. `PersistedPaneLayout` is the
  tree with `{ orient, ratio, first, second }` splits and
  `{ id, kind, cwd }` leaves; NEW key spellings (nothing frozen yet — pick
  clean ones: `"layout"`, `"activeLeafId"` … final spellings frozen at
  ship). Old files: absent field → `None` → hydrate as today's single-leaf
  window. Old Nice reading a new file ignores the keys (forward-compat
  test :339-361 already pins this tolerance). `CURRENT_VERSION` stays 3
  (shape-tolerant policy, session_store.rs:14-18).
- Hydration: `None` layout → single-leaf tree from the window's own
  `kind`/`cwd`; `Some` → validate (active id present, Claude-leaf count
  matches kind), fall back to single-leaf on any violation — the loader
  never errors.
- Round-trip + tolerance + fallback tests beside the existing persisted
  tests.

## Slice 2 — pty layer: pane keying, multi-spawn, exit, status

`crates/nice/src/pty_manager.rs`:

- `WindowPty` (:198-201) becomes `{ panes: HashMap<String /*pane_id*/,
  Entity<TerminalSessionHandle>> }`. `term_window_handle(session, window)`
  (:1247) keeps its signature and resolves the window's ACTIVE pane's
  handle. New `pane_handle(session, window, pane_id)` and
  `live_pane_keys()` (the `(session, window, pane)` triple sweep) beside
  it. Caller-by-caller resolution — each of the three existing
  `term_window_handle` callers gets an explicit answer:
  - `ScrollHalfPage*` handlers (keymap.rs): active pane — "the terminal
    the user is looking at". Correct via the kept signature.
  - `dispatch_command_compose` (window_state.rs:2490): FOCUSED pane's
    handle + kind (see the compose/busy bullet below).
  - `subscribe_spawned_windows` (window_state.rs:822): must see EVERY
    pane handle — it switches to `live_pane_keys` + `pane_handle` (next
    bullet).
- **Event routing goes pane-keyed and un-detached** — the load-bearing
  change the grounding sweep missed. `subscribe_spawned_windows`
  (window_state.rs:812-892) today creates one PERMANENT (`.detach()`ed)
  subscription per window, capturing `(session_id, window_id)` in the
  closure. As-is, splits would subscribe only the active pane (dedupe key
  blocks the rest forever — a background pane's exit would NEVER route),
  and break-pane would strand a moved pane's events on the old window
  key. The fix, both halves mandatory:
  - Subscribe per PANE (`live_pane_keys`, dedupe key `"{t}:{w}:{pane}"`),
    and STOP detaching: retain `Subscription` objects in a
    `HashMap<pane_key, Subscription>` on `WindowState`, dropped when the
    pane dies and dropped+re-created under the new key when break-pane
    re-homes a pane.
  - `route_terminal_event` (:846-848) gains the pane dimension; the
    closure captures the PANE id and resolves the owning
    `(session, window)` from the model at event time (so a re-homed
    pane's events land in its current pill even between sweep passes).
- `ensure_active_window_spawned` (:1698-1747): spawning a window now means
  spawning EVERY leaf of its tree that isn't yet live — the Claude leaf
  through the existing `--resume` path, shell leaves through the fresh-
  shell path with the leaf's persisted `cwd` (fallback: session cwd).
  Split-created panes spawn eagerly at split time (the split action
  targets the active window by definition); restore-hydrated multi-pane
  windows spawn lazily on activation exactly like windows do today.
- Spawn env (P2): all panes get the window's existing
  `NICE_TAB_ID`/`NICE_PANE_ID` values (:1321-1322, :1961-1965 — both
  sites). No new vars.
- **Pane exit**: a pane pty's exit routes to a new `pane_exited(session,
  window, pane_id)`: if the tree has >1 leaf — `layout.remove`, spatial
  refocus via `spatial_refocus` (Slice 1) when the closed pane was
  focused, drop that handle + its retained subscription only; if it was
  the LAST leaf — delegate to the existing `window_exited` 5-step flow
  (:762-804) unchanged (pill close semantics, index-neighbor refocus at
  the PILL level, dissolve cascade).
- **Claude-leaf exit in a multi-pane pill** (single-pane pills behave
  exactly as today, both cases):
  - CLEAN exit (`held: false`): the Claude leaf is removed like any pane
    close, and the pill's `kind` flips to `Terminal` with the window's
    Claude bookkeeping cleared (`is_claude_running = false`,
    `claude_session_id` handling unchanged at session level) — the pill
    is now just its shells. This is what makes the Slice-1 invariant
    "at most one Claude leaf, kind == Claude iff one exists" hold.
  - HELD exit (crash/holds): the Claude LEAF becomes the held slot —
    held-ness moves from window-level to a per-leaf flag; the pill keeps
    `kind = Claude`, shows the held placeholder in that pane's rect, and
    held-dismiss removes the leaf (then the clean-exit kind-flip rule
    applies). `window_held` (:814-829) must NOT flip window-level
    `is_alive` when other panes survive.
  - **Window-level field semantics, redefined once here**: `is_alive` =
    any leaf's pty alive; `is_claude_running` = the Claude leaf's spinner
    state (false when no Claude leaf); Claude-presence predicates
    (`live_claude_windows`, `has_claude`, session.rs:100-113) key off
    "Claude leaf exists AND alive", not bare window `is_alive`.
- **Status/title (P9)** — mechanism, not just goal: per-pane runtime
  status lives in a watcher-side map in `PtyManager` (NOT on the model
  `Pane` — status is never persisted). `window_title_changed`
  (pty_manager.rs:453-514) currently branches on WINDOW kind; it branches
  on the emitting PANE's kind + active-ness instead: the Claude leaf's
  titles feed spinner/status parsing; the ACTIVE pane's title feeds the
  pill title; a shell pane's titles never enter the Claude branch and
  can't clobber the spinner. Window-level `status` recomputes as the OR
  over the per-pane map on every transition (so one pane going idle
  un-lights the pill only when all are idle). Socket `SessionUpdate`
  messages (keyed by pill id, P2) keep updating window-level status
  exactly as today.
- **Busy-close gates and Command Compose go pane-aware**
  (window_state.rs): `window_is_busy` (:2440-2464) drops its dead-first
  window-`is_alive` guard in favor of per-leaf checks and ORs across
  leaves — the Claude leaf by status, each shell leaf by its own
  `tcgetpgrp` foreground-child probe — so a shell pane running a build in
  a Claude pill correctly blocks close. `compose_route` (:2512-2525)
  resolves the FOCUSED pane's kind + handle: an idle shell pane focused
  in a Claude pill gets `Trigger` (⌘↩ compose works in exactly the
  "shell beside Claude" layout D1 exists for); a focused Claude leaf
  keeps today's behavior.
- Break-pane plumbing: `move_pane_to_new_window(session, window, pane_id)`
  — `layout.remove` + re-key the handle into a fresh `WindowPty` under a
  new `TermWindow` built through `insert_window` (workspace_model.rs:550)
  after the source pill; the new pill's `TermWindow.cwd`/title seed from
  the moved pane. No respawn — the handle moves. Event routing survives
  the move via the retained-subscription re-key + model-resolved routing
  (above). **Accepted wart, document at the code**: the moved pty's
  `NICE_TAB_ID`/`NICE_PANE_ID` env was fixed at fork and still names the
  SOURCE pill — env can't change post-fork — so socket traffic from that
  shell (a manually-typed `claude` promotion, handoff/dispatch) targets
  the old pill. Accepted for Phase 2 (same class of staleness as moving
  any live process); Phase 5's pane addressing is the place to revisit.

## Slice 3 — view/host: recursive mount, dividers, focus

`crates/nice/src/app_shell.rs` (`WindowHostView`) — where the one-view
assumption dies:

- Cache re-keys by `pane_id` (`stale_cache_ids` sweep gains pane-level
  liveness from the active model walk — same id-set diff pattern,
  :493-517).
- `active_window_target()` grows the third element: `(session_id,
  window_id, active_pane_id)`; `last_active` likewise. `activation_changed`
  still means "focus + activate": `activate_term_window` on window change,
  `window.focus(pane's fh)` on pane change.
- `render()` builds the active window's tree: `leaf_rects(content_bounds,
  DIVIDER_PX)` → one absolutely-positioned child per leaf (each mounting
  that pane's cached `TerminalView` — auto-refit sizes each pty
  independently, view.rs:26) + one divider hit-zone per split. Zoomed (P4):
  render only the focused leaf at full content bounds, skip dividers.
- **Present-kick** (:666-672): install for EVERY visible pane handle, not
  just one — mirror the loop over mounted leaves.
- **Dividers** — copy the sidebar pattern wholesale (sidebar_shell.rs
  :1363-1428, :2210-2222): 6px invisible hit zone straddling each divider,
  `ResizeLeftRight`/`ResizeUpDown` cursor per orientation, mouse-down
  captures `(split_node_path, origin, effective_ratio_baseline)`,
  root-level mouse-move/up drives it (survives leaving the zone; treats a
  missed mouse-up as drag end), min-size clamp per P6, commit persists via
  the normal debounced store upsert, double-click resets that split to
  0.5. Per-divider identity = the path from the tree root
  (`Vec<FirstOrSecond>`). A drag can't restructure the tree, but a pane
  pty can EXIT mid-drag and collapse a split — a stale path lookup
  resolves to a no-op end-of-drag, never a panic or a wrong-node write.
- **Focus-grab suppression** (the `focused_once` steal, view.rs
  :1774-1778): `TerminalView` gains a constructor-time/setter flag to
  skip the first-render focus grab; `WindowHostView` sets it for every
  non-active pane it mounts, and explicitly focuses the ACTIVE pane's
  handle after any pass that mounted new views — restoring a 3-pane pill
  focuses `active_pane_id`, not whichever view rendered last.
- **Painted-size stash for the keymap layer**: each render,
  `WindowHostView` writes the current content bounds (px) onto
  `WindowState`. Slice 4's `SplitDown/Right` min-size refusal and
  `ResizePane*` px→ratio conversion read it — App-level action handlers
  have no `&mut Window` and can't measure anything themselves. Absent
  stash (window never painted): split proceeds at 0.5, resize no-ops.
  Focus/swap need no px context (adjacency is scale-invariant on unit
  bounds).
- **Focus routing**: `focus_active_terminal`/`active_terminal_focus_handle`
  (:362-374) resolve the focused PANE's view. Mouse-down anywhere in a
  pane's rect sets `active_pane_id` (un-zooming first per P4) — the click
  path is new; pill/session switching keeps its existing focus flow.
- **Focused-pane affordance**: with >1 leaf, the focused pane gets a 1px
  accent-tinted inner border (or divider-side highlight — pick at
  implementation; feel-check tunes); single-leaf pills render exactly as
  today, zero visual change.
- Held/exited pane slots reuse the existing window placeholder mechanism
  per leaf (`window_placeholder()` swap, :677-693 pattern).

## Slice 4 — actions, keymap, recorder

`nice-model/src/shortcuts.rs`:

- 12 new `ShortcutAction` variants + frozen ids (additive — new ids load
  unbound for rebound users, accepted Phase-1 precedent):
  `SplitDown`/`splitDown` (`^⌘-`), `SplitRight`/`splitRight` (`^⌘\`),
  `ZoomPane`/`zoomPane` (`^⌘z`), `BreakPane`/`breakPane` (`^⌘b`),
  `ResizePaneLeft/Down/Up/Right`/`resizePane*` (`^⌥⌘hjkl`),
  `SwapPaneLeft/Down/Up/Right`/`swapPane*` (`^⌥⌘⇧hjkl`). `ALL` 22 → 34;
  completeness-test literals updated. Labels per D2: "Split Down",
  "Split Right", "Zoom Pane", "Break Pane to Window", "Resize Pane …",
  "Swap Pane …".
- `RESERVED_COMBOS` 20 → 9: remove `⌃⌘Z`, `⌃⌘V`, `⌃⌘S` and all eight
  `⌃⌥⌘[⇧]hjkl` entries (the disjointness test forces this); `⌃⌘/` stays
  (Phase 3). `⌃⌘V`/`⌃⌘S` become plain unbound chords — recordable by
  users, bound to nothing by default (D2).
- `conflicting_action`/`combos_overlap` need no logic changes (the
  `WindowByIndex` digit expansion doesn't intersect any new default:
  `^⌘-`, `^⌘\`, `^⌘z`, `^⌘b` carry no digits; the hjkl rungs use
  different modifier sets).

`crates/nice/src/keymap.rs`:

- Fill the four inert bodies (:441-444) — `FocusPane*` handlers:
  `with_active_state` → active session/window → un-zoom (P4) →
  `leaf_rects` + `directional_neighbor` → set `active_pane_id` (no-op at
  edges, P5). Model-mutation-only; render reacts (the established
  pattern, :396-401).
- Add the 12 new action structs to `actions!` (:80-105) + registration:
  - `SplitDown`/`SplitRight`: refuse under P6 mins (via the Slice-3
    painted-size stash); mint pane id, spawn the shell pane eagerly
    (Slice 2), focus follows the NEW pane (tmux behavior).
  - `ZoomPane`: toggle `zoomed` (single-leaf pill → no-op).
  - `BreakPane`: P3 (`move_pane_to_new_window`); focus follows the new
    pill via `switch_active_window`.
  - `ResizePane*`: P7 — px step → ratio delta against the relevant
    ancestor's px extent (via the Slice-3 painted-size stash), clamp per
    P6.
  - `SwapPane*`: P8 — un-zoom, swap, focus follows content.
- `table_bindings`/`rebuild_keymap` pick the new rows up automatically
  from `default_bindings()`; no expansion special cases (unlike
  `WindowByIndex`).
- Hint overlay: untouched (pane-number badges belong to Phase 5's
  display-panes analogue).

Recorder/settings: rows appear via `ALL` automatically; the freed
`⌃⌘V`/`⌃⌘S` simply stop being refused. Phase 1's recorder guard,
conflict detection, and store rules all apply unchanged.

## Slice 5 — selftest scenario + docs

- New `splits` selftest scenario (`input_live.rs`, registered in
  `app.rs`), reusing `chord_leak`/`freed_chord` verbatim and adding a
  pane-level `nav_chord` twin that asserts `active_pane_id`:
  1. Seeded single-pane pill: `^⌘\` splits (2 leaves, focus on new pane,
     zero leak); `^⌘-` splits again (3 leaves, mixed orientations).
  2. `^⌘⇧hjkl` walks focus spatially; edge chords no-op (P5); zero leak.
  3. `^⌥⌘l` changes the relevant ratio; clamp pinned at min (P6).
  4. `^⌥⌘⇧h` swaps payloads; focus followed content (P8).
  5. `^⌘z` zooms (render assertion via model flag); `^⌘⇧h` un-zooms and
     moves (P4).
  6. `^⌘b` breaks the focused shell pane into a new pill (pill count +1,
     focus on it); on the Claude leaf it no-ops (P3).
  7. Close one pane by writing `exit\n` through its `pane_handle` (the
     split panes are REAL zsh — `SpawnSpec::shell` has no fixture
     injection point, so exit-by-command is the spec) → tree collapses,
     spatial refocus picked the shared-edge neighbor; exit the last
     pane → existing pill-close flow.
  8. `^⌘v`/`^⌘s` via `freed_chord` — inert, nothing leaks (D2).
  9. Persistence: store round-trip mid-scenario (`flush` + decode)
     carries the layout; a hand-mangled layout blob hydrates to
     single-leaf (loader-tolerance assertion).
- Existing-test fallout (verified inventory — the `keybind-scheme`
  scenario has NO `freed_chord` assertion on the `FocusPane*` chords, so
  nothing flips there): the shortcuts test
  `the_focus_rung_is_bound_and_the_phase_two_rungs_are_reserved`
  (shortcuts.rs:1417) pins the rungs as reserved and must be rewritten;
  the reserved-count/disjointness tests update for 20 → 9;
  `scenario_terminal_for(term_window_id)` (app_shell.rs:380) reads the
  view cache by WINDOW id and must re-key by pane (the claude-lifecycle
  scenario depends on it).
- Docs: roadmap Phase 2 section → shipped wording + decision record
  (D1-D3, P1-P9); tracker `docs/tmux-port-progress.html` flips;
  `docs/plans/phase-1-keybind-scheme.md` gets a one-line "Phase 2 claimed
  the reserved rungs as planned" note; README keyboard table; the
  `keybind-scheme` scenario doc comment's reserved-chord list updated.

## Ordering

1 → 2 → 3 → 4 → 5. Slice 2 needs Slice 1's tree; Slice 3 needs both;
Slice 4 needs 1-3 (handlers touch model, pty, and render state); Slice 5
last. No slice is parallel-safe with its predecessor.

## Validation

Automated — the cycle's validator runs these (build + tests are the gate;
log to a file and check `$?`, never pipe `cargo test` through
`tail`/`head`):

1. `cargo build --workspace`.
2. Unit tests (new/updated):
   - `pane_layout`: split/remove/swap/resize mutations; ratio clamping;
     `leaf_rects` geometry (nested mixed orientations, divider space);
     `directional_neighbor` (adjacency, overlap ranking, tie-break, edge
     no-op); `spatial_refocus` (shared-edge pick, centroid fallback);
     invariants (active id containment, Claude-leaf count).
   - `TermWindow`: single-leaf constructor; zoomed skip-serde.
   - persisted: layout round-trip; absent-field → single-leaf; mangled
     layout falls back; forward-compat unknown keys still ignored.
   - shortcuts: 34-action completeness; new defaults rows + spellings
     (`^⌘-`, `^⌘\`, `^⌘z`, `^⌘b`, rungs); reserved table down to 9;
     disjointness holds; freed `⌃⌘V`/`⌃⌘S` no longer refused by
     `decide_capture`; id round-trips.
   - pty_manager: pane spawn/exit paths; last-leaf delegation to
     `window_exited`; Claude-leaf clean-exit kind-flip vs held-leaf
     retention; break-pane handle + subscription re-key; per-pane status
     map + OR recomputation on transitions.
   - window_state: pane-keyed subscription dedupe + retained-subscription
     drop on pane death; `window_is_busy` OR across leaves (shell-pane
     fg-child blocks close in a Claude pill); `compose_route` resolves
     the focused pane's kind.
3. Targeted `cargo test`: `-p nice-model` (pane_layout, term_window,
   session, workspace_model, persisted, shortcuts), `-p nice` (keymap,
   pty_manager, app_shell if it has tests, shortcuts_pane), `-p
   nice-term-view` during fix rounds; one full `cargo test --workspace`
   before merge.
4. Live selftest: `NICE_SELFTEST=splits <target-dir>/debug/nice` plus a
   re-run of `NICE_SELFTEST=keybind-scheme` (it changed). Under the
   worktree lock (`scripts/worktree-lock.sh acquire <op>` … `release`),
   display awake (`caffeinate -d`). Hard assertions must pass.

Post-merge human feel-check (Nick — after `scripts/rust-install.sh` under
the worktree lock). Chord DELIVERY is only provable by hand (OS
interception blind spot):

1. `^⌘\` and `^⌘-` split a Claude pill; the shell pane lands in Claude's
   cwd; Claude keeps streaming while you type in the shell.
2. `^⌘⇧hjkl` walks focus; the focused-pane affordance reads clearly at a
   glance; edges no-op without flicker.
3. Drag every divider: cursor flips per orientation, mins clamp, fast
   drags don't drop, double-click resets to half.
4. `^⌥⌘hjkl` resize and `^⌥⌘⇧hjkl` swap (Hyper) — verify macOS delivers
   all sixteen chords (the `⌃⌘D`-style OS-swallow risk is exactly here).
5. `^⌘z` zoom in a 3-pane pill; un-zoom via focus move and via re-toggle.
6. `^⌘b` on a shell pane → new pill after the current one, focus follows;
   on the Claude pane → nothing.
7. Exit a shell pane (`exit`) → neighbor focus feels spatially right;
   exit the last pane → pill closes exactly as today.
8. Quit + relaunch: layout, ratios, cwds, and focused pane restore; a
   pre-Phase-2 `sessions.json` restores single-pane pills cleanly.
9. Settings ▸ Shortcuts: the 12 new rows render; recording `⌃⌘V` now
   works; reserved refusals still fire for `⌃⌘Q`/`⌃⌘Space`/`⌃⌘D`/`⌃⌘F`/
   `⌃⌘/`; half-page scroll (`^⌘↑/↓`) scrolls the FOCUSED pane.

## As shipped

Everything above landed. The list below is only where the code and the
plan differ, plus the few places the plan left a choice open.

### Decisions that survived verbatim

D1-D3 and P1-P9 all shipped as written. In particular the three judgment
calls Nick was asked to sign off on are in the code exactly as described:
a clean Claude-leaf exit inside a multi-pane pill flips the pill's kind to
`Terminal`, break-pane's stale `NICE_TAB_ID`/`NICE_PANE_ID` wart is
accepted and documented at the call site, and the busy-close gate ORs
across leaves.

### Where the code diverges

- **Split panes are flex children, not absolutely-positioned ones.** The
  plan's Slice 3 said "one absolutely-positioned child per leaf". They
  render as nested flex rows/columns instead, each child carrying its
  split's ratio as a flex basis and shrinking against the 6 px divider.
  The point of the plan's phrasing was that paint and keyboard must agree
  on geometry, and that still holds — the flex arithmetic lands on exactly
  the rects `PaneLayout::leaf_rects` computes, which is what the keyboard
  verbs walk.
- **`resize` was split in two.** The plan gave the model a single
  `resize(focused_pane_id, direction, delta)`. Converting a 40 px step
  into a ratio delta needs to know WHICH divider is about to move, so
  `resize_target_path` was factored out and made public: the keymap asks
  for the path, measures that divider's px extent, then calls `resize`.
  Without the split, the model and the px conversion could disagree about
  the target and a chord would move one divider by another's step.
- **`P6`'s mins are exactly 120 x 80 px.** The plan said "start ~120x80;
  feel-check tunes". They shipped at those values as
  `PANE_MIN_WIDTH`/`PANE_MIN_HEIGHT`; the post-merge feel-check is where
  they get tuned if they grate.
- **The focused-pane affordance is an accent border.** The plan offered
  "1px accent-tinted inner border (or divider-side highlight — pick at
  implementation)". The border won — then the 2026-08-13 feel-check
  replaced it: the ring drew flush against the grid, and the mock round
  (`docs/mocks/pane-focus-mocks.html`) settled on four accent CORNER TICKS
  plus a per-pane content inset (`PANE_CONTENT_INSET_X/Y`). Dimming was
  rejected (reading unfocused panes is the flagship use; composes badly
  with themes/blur), and edge bars were rejected (ambiguous at a stacked
  boundary — only an enclosure reads without convention).
- **`Eq`/`Hash` came off `Project` too.** The plan predicted the derives
  would have to leave `TermWindow` and `Session` because `ratio: f32` is
  not `Eq`. `Project` contains `Session`, so it followed. As the plan
  required, no hash container keyed on any of the three was found.
- **`freed_chord` changed signature.** Slice 5 was specified as reusing
  `chord_leak`/`freed_chord` "verbatim". `freed_chord` took a
  `KeybindFixture`, which the `splits` scenario does not have, so it now
  takes the `WindowState` entity and session id directly. Both scenarios
  call the one helper; no behavior changed.
- **`TerminalView`'s focus-grab suppression is a setter, not a
  constructor argument.** `set_focus_on_first_render(false)` — the plan
  allowed either.

### What the `splits` scenario does and does not prove

The scenario mounts ONE `TerminalView` over a real `WindowState`, the way
`keybind-scheme` does — not the shipped `WindowHostView`. That buys
hermeticity and costs paint coverage, so three things are covered
elsewhere and named here so nobody reads the green scenario as more than
it is:

- **Paint.** Nothing renders a pane tree in the scenario, so zoom, the
  focused-pane affordance and the divider drags are unit-tested instead.
  The driver stands in for the paint by stashing a 1200x800 pane area on
  `WindowState`, which is the px context the split refusal and every
  resize step are denominated against.
- **The Claude-pane refusals (P3).** Standing a real Claude up is not
  hermetic. `keymap` and `pty_manager` unit-test break-pane's refusal on a
  Claude leaf. The refusals the scenario CAN prove honestly — zoom and
  break-pane on a single-leaf pill — it does prove.
- **Chord delivery.** `dispatch_keystroke` injects downstream of the OS
  hotkey layer, so a chord macOS itself swallows still looks live in the
  scenario. This is the `^⌘D` lesson from Phase 1, and sixteen of Phase
  2's chords are Hyper-cluster rungs. Only the hand feel-check gates
  those.

The split panes are real `zsh` — `SpawnSpec::shell` has no fixture
injection point — which is what makes the close legs honest: a pane is
closed by writing `exit` through its own `pane_handle` and the tree
collapse is observed, not simulated.
