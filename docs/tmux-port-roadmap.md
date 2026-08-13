# Porting tmux features into Nice — concept mapping & roadmap

Status: proposal (2026-08-05). Based on a full sweep of the current Rust tree.

## 1. The concept mapping

tmux's model is: **server → session → window → pane**. A session is a named
group of windows; a window is the full-screen unit (one visible at a time);
a pane is one pty, a leaf in a window's split tree. The server outlives
clients, which is what makes detach/reattach work.

Nice's model is: **app process → OS window (`WindowState`) → `Project` →
`Tab` → `Pane`**, where a `Tab` is a sidebar row owning `panes: Vec<Pane>`
with exactly one active, and a `Pane` is one pty rendered as an upper-bar
pill (`crates/nice-model/src/tab.rs:17-52`, `pane.rs:34-67`).

The mapping falls out almost perfectly — Nice's names are just shifted one
level from tmux's:

| tmux | Nice today | Notes |
|---|---|---|
| server | the app process | No daemon; ptys die with the app (`SessionManager::teardown`, `session_manager.rs:1740`) |
| session | **`Tab` (sidebar row)** | Named group of ptys with one active — exactly a session |
| window | **`Pane` (upper-bar pill)** | One pty, one visible at a time, background ones keep running — exactly a tmux window |
| pane (split) | **nothing** | No split concept anywhere in the tree; the main gap |
| client | an OS window | `WindowState` per gpui window, `WindowRegistry` tracks them |
| status line / choose-tree | the sidebar + titlebar | Sidebar = session list; toolbar pill strip = window list |
| prefix key table | **nothing in Nice**, but vendored gpui supports multi-keystroke bindings natively | `KeyBinding::load` splits on whitespace; `Window::pending_input` + observers exist at the pin (`vendor/zed/crates/gpui/src/keymap/binding.rs:56-99`, `window.rs:4861-5053`) |
| detach/reattach | `sessions.json` restore (weak substitute) | Restores cwd + `claude --resume` prefill, not live processes |

**Decision this implies:** when splits arrive, an upper-bar pill = a tmux
*window* that contains a split tree of ptys. The pill strip stays a linear
list (of windows); the split tree lives inside the active pill. `Tab.panes`
stays the flat inventory of ptys; a separate per-window layout tree of pane
ids describes the visible arrangement. This avoids fighting the strip's
linear-order assumptions (`toolbar.rs:557-575`, `pane_strip_drop.rs`).

## 2. The sidebar is not a pane — and shouldn't become one

The sidebar never hosts a terminal. It is the shell **root**: `SidebarShellView`
owns the whole layout and receives the toolbar and `PaneHostView` as injected
slots (`sidebar_shell.rs:507-600`, `717-727`; composition at `app.rs:1754-1881`).
Its content is chrome — the session list (`build_tab_list`) or the file browser —
i.e. it plays the role of tmux's status line + choose-tree, not of a pane.

Making it "literally behave exactly like a pane" would mean rebuilding the
shell root as a split-tree member. That buys nothing tmux-shaped: tmux has no
sidebar; its session/window navigation UI is the status line. What the
sidebar request actually decomposes into:

1. **Remove the width cap** — DONE in Phase 0: the fixed `SIDEBAR_MAX_WIDTH`
   (480) is retired for a viewport-derived clamp (`viewport −
   TERMINAL_MIN_WIDTH`, 300pt, in `chrome_geometry.rs`; `clamp_sidebar_width`
   in `sidebar_shell.rs`), and the committed width lives in `SidebarModel`,
   persisted as the optional `sidebarWidth` slot in `PersistedWindow`
   alongside `sidebar_collapsed`/`sidebar_mode`.
2. **Real side-by-side terminals** — DONE in Phase 2 (splits), in the
   terminal area where it belongs. The sidebar itself is still not a pane,
   and deliberately so.

## 3. What Nice already has (more than expected)

- **Background ptys without views.** `TerminalSessionHandle` is deliberately
  view-independent; hidden panes' sessions keep pumping (title/cwd/exit)
  with no `TerminalView` mounted (`session_handle.rs:1-13`, `143-148`).
  tmux-window semantics already work.
- **Multi-keystroke chords at the gpui pin, no patch needed.** Partial-match
  detection, 1s pending timeout with replay, `has_pending_keystrokes()`,
  `pending_input_observers` — all present in vendored gpui.
- **A rebindable-shortcut system with persistence + recorder UI**
  (`shortcuts.rs`, `shortcuts_store.rs`, `shortcuts_pane.rs`) — the natural
  home for prefix-table config. Limitation: `OwnedCombo::from_token` cannot
  represent a two-keystroke sequence yet (`shortcuts.rs:363-449`), and the
  recorder captures exactly one keystroke. Since Phase 1 the recorder also
  refuses the reserved combos (`RESERVED_COMBOS`) with a per-entry reason.
- **A scriptable IPC seam.** Per-window AF_UNIX control socket with NDJSON
  protocol (`control_socket.rs`), `NICE_SOCKET`/`NICE_TAB_ID`/`NICE_PANE_ID`
  injected into every pty env — the foundation for tmux-style
  `send-keys` / `split-window` CLI scripting.
- **Search/selection primitives.** Vanilla alacritty_terminal 0.26:
  `RegexSearch`/`RegexIter` already linked (used for hyperlinks,
  `hyperlink.rs:92-233`); full programmatic selection API on the handle
  (`session_handle.rs:517-618`); scrollback + scroll-position API, with
  keyboard scroll bindings since Phase 0 (Shift+PageUp/PageDown/Home/End)
  and half-page jumps since Phase 1 (`^⌘↑`/`^⌘↓`).
  Missing is only UI: no copy-mode state machine, no search overlay.
- **Overlay building blocks** for prefix-pending indicators, search fields,
  and pane-number popups: peek overlay + modifier observers
  (`keymap.rs:546-606`), `InlineRename`, `ConfirmationModal`, `ContextMenu`,
  and the present-kick pattern for occluded windows
  (`window_state.rs:1346-1414`).
- **Vestigial tear-off seams.** Cross-window move / tear-off was CUT from the
  Rust port (`crates/README.md:603-605`), but `TabModel::extract_pane` /
  `insert_pane` (`tab_model.rs:499/522`), `dissolve_tab_if_empty`, and
  `WindowRegistry::state_for_window` survived for exactly this future.

## 4. The genuinely hard parts

### Splits (the big one) — SOLVED in Phase 2

The survey below is the pre-Phase-R reading that sized the work (old type
names, old line numbers); it is kept for the archaeology. What it got right
is the shape of the fix — a layout tree, `active_pane_id` as focused leaf,
per-view refit — and that is what shipped. What it under-counted is event
routing: subscriptions were window-keyed AND permanently detached, so
pane-keying them (and retaining them, so they can be dropped and re-keyed)
turned out to be load-bearing for background-pane exits and for break-pane.

Ten places hard-code one-visible-terminal-per-tab; the load-bearing ones:

1. `active_pane_target` returns a single `(tab_id, pane_id)` — `app_shell.rs:485-489`
2. `PaneHostView::render` builds ONE content element — `app_shell.rs:677-694`
3. Single-activation path `last_active` + `SessionManager::activate_pane` — `app_shell.rs:263-266, 707-716`; `session_manager.rs:1722`
4. One padded root div, one set of insets — `app_shell.rs:722-730`
5. No layout field in persistence — `window_state.rs:1298-1309`, `persisted.rs:55-80`
6. `Tab { panes: Vec<Pane>, active_pane_id }` flat-list assumption — `tab.rs:26-29`
7. Pill strip renders a linear `Vec<Pane>` with one active — `toolbar.rs:557-575`
8. `step_active_pane` index arithmetic (⌘⌥←/→) — `pane_strip_actions.rs:85-107`
9. Close-refocus picks the index neighbor, not spatial — `session_manager.rs:419`
10. View cache never mounts two `TerminalView`s at once — `app_shell.rs:245-252, 668-671`

Design that minimizes churn: keep `Tab.panes` as the pty inventory; add a
per-tmux-window **layout tree** (binary splits with ratios, leaves = pane
ids). `active_pane_id` becomes "focused pane". Each mounted `TerminalView`
already owns its own refit → TIOCSWINSZ (`app_shell.rs:311`), so N mounted
views should size independently — needs validation, plus divider
drag-resize, directional navigation, zoom (temporary single-leaf render),
and a persistence bump (schema is shape-tolerant; add `layout` to
`PersistedTab`).

### Detach / reattach
No daemon; `SessionManager` (per-window) owns the ptys, and quit/close drops
them (SIGHUP→SIGKILL, `deferred.rs:531-559`, `pty.rs:445-475`). Options:

- **(a) In-app detach** — move pty ownership from per-window `SessionManager`
  to an app-global session registry; closing a window can orphan its
  sessions into a "detached" pool shown in every sidebar; any window can
  adopt them. Also revives cross-window pane move / tear-off via the
  surviving seams. Covers the daily tmux win (rearrange freely, close
  windows without killing work) for an app that stays running. Medium-large.
- **(b) Full daemon split** — a `nice-server` process owning ptys, app as
  client. Survives app crash/restart; huge architectural change (every
  `FairMutex<Term>` access becomes IPC or the server owns the grids).
  Not recommended until (a) proves insufficient.

Recommendation: (a), plus the existing `sessions.json` restore as the
across-restart story.

### Copy mode
State machine + rendering, not plumbing: keyboard-driven cursor in
scrollback, vi-style motions, selection start/extend via the existing handle
API, search input overlay (InlineRename pattern) + match highlighting in
`element.rs`. Medium.

## 5. Roadmap

Phases are ordered so each ships something usable on its own.

### Phase R — terminology rename (S-M, DO FIRST)

**DECIDED (2026-08-07): code adopts tmux terminology before any feature
work.** `Tab` → `Session`, `Pane` → `TermWindow` (decided 2026-08-08 over
qualified `model::Window` — avoids the `gpui::Window` collision), `TabModel`
→ `WorkspaceModel`, Phase 2's split leaf takes the freed name `Pane`.
`SessionManager` → `PtyManager` to free the word "session" at the app layer
(pty-sense names inside `nice-term-core`/`nice-term-view` stay). `Project`,
`WindowState` (OS window ≈ tmux client) keep their names.

Includes the UI copy pass (decided 2026-08-08): sidebar rows say "session",
pills say "window"; macOS-standard menu items (⌘N "New Window") keep their
wording. Disk and wire formats are frozen: `sessions.json` keys, the
`ui_settings.json` shortcut ids, the control-socket NDJSON protocol, and the
`NICE_TAB_ID`/`NICE_PANE_ID` pty env vars keep their current spellings via
serde aliases. Verify restore from a pre-rename `sessions.json` before
calling it done.

Full implementation plan: `docs/plans/phase-r-terminology-rename.md`.

### Phase 0 — quick wins (S)
- Sidebar width: retire the fixed `SIDEBAR_MAX_WIDTH` for a viewport-derived
  clamp (`viewport − TERMINAL_MIN_WIDTH`, **decided: 300pt**); persist the
  width per window (`sidebarWidth` slot in `PersistedWindow`, absent = never
  customized; double-click reset clears it).
- Keyboard scrollback (**decided 2026-08-10: Shift variants ONLY** — plain
  PageUp/PageDown/Home/End keep encoding to the pty for less/vim, and on the
  alternate screen even Shift variants fall through to the TUI): Shift+PageUp/
  PageDown page (`scroll_page_up`/`scroll_page_down`), Shift+Home/End jump
  (`scroll_to_top`/`scroll_to_bottom`). Scrolling was wheel-only before.

Full implementation plan: `docs/plans/phase-0-quick-wins.md`.

### Phase 1 — held-modifier keybind scheme (M) — SHIPPED

**DECIDED (2026-08-05): no tmux-style prefix sequences. The scheme is
vim keys held under `^⌘` (control+command).** Held-modifier chords are
single keystrokes to gpui — no pending-prefix state machine, no timeout,
no send-literal escape hatch needed (nothing is stolen from the pty: a
bound chord fires its action before the terminal's key listener ever sees
it, so it leaks zero bytes). OS key-repeat gives free continuous
navigation (hold `^⌘` + hold `j`), which tmux only approximates with
`repeat-time`.

**REVISED (2026-08-11) — the hjkl ladder: the modifier SET selects the
verb, the `hjkl` key selects the direction.** Sessions join the bare `^⌘`
layer, so both container axes (pills across, sessions down) live on one
held pair; pane-level verbs climb a rung per modifier. `^⌘Space` for swap
was rejected — it is the macOS emoji picker, and Space is not a modifier —
so swap took the Hyper cluster (`^⌥⌘⇧`). The revision frees `^⌘[`/`^⌘]`
(the shipped D1 spelling) and `⌘⌥↑`/`⌘⌥↓` (the pre-Phase-1 session
chords): nothing binds them.

| Rung | `h` | `j` | `k` | `l` | Verb | Phase |
|---|---|---|---|---|---|---|
| `^⌘` | prev pill | next session | prev session | next pill | navigate containers | 1 |
| `^⌘⇧` | focus pane left | down | up | right | move pane focus (`FocusPane*`) | 2 |
| `^⌥⌘` | resize left | down | up | right | resize split (`ResizePane*`) | 2 |
| `^⌥⌘⇧` | swap left | down | up | right | directional pane swap (`SwapPane*`) | 2 |

All three pane rungs are live as of Phase 2; the `^⌘⇧` handlers were bound
but inert through Phase 1, and the other two rungs were reserved-unbound.

Everything else on the scheme is unchanged:

| Chord | Action | Phase |
|---|---|---|
| `^⌘1-9` | Window by index — **D2**: ONE rebindable row (`windowByIndex`, "Window 1-9") whose recorded modifier set applies to all nine digits; nine separate rows were rejected | 1 |
| `^⌘o` | Last-active window (tmux `last-window`, a single bounce slot — not an MRU stack) | 1 |
| `^⌘z` | Zoom pane | 2 |
| `^⌘-` / `^⌘\` | Split Down / Split Right — **Phase 2 D2**: divider mnemonics. The `^⌘v`/`^⌘s` penciled in here are FREED and bind to nothing | 2 |
| `^⌘b` | Break pane out to its own window | 2 |
| `^⌘↑` / `^⌘↓` | Half-page scrollback up / down (no-op on the alternate screen) — moved off `^⌘u`/`^⌘d` on 2026-08-11; both of those are now bound to nothing | 1 |
| `^⌘/` | Scrollback search | 3 |
| *hold* `^⌘` | Window-index badges on the pills — **D5**, ~200 ms debounce | 1 |

**DECIDED (2026-08-11): half-page scroll moved to `^⌘↑`/`^⌘↓`** — real `^⌘D`
keydowns are swallowed by the macOS dictionary hotkey before the app sees
them (found in hand-testing; the gpui-level `keybind-scheme` scenario cannot
detect OS-level interception, because it injects downstream of it). Both
halves moved together to keep the pair symmetric; `^⌘u` and `^⌘d` are now
bound to nothing, and `^⌘D` is a pure reserved-table entry again.

All Phase 1 rows are rebindable in Settings ▸ Shortcuts; the frozen action
ids never moved, only the default combos.

Reserved — never bind: `^⌘Q` (macOS lock screen, system-intercepted),
`^⌘F` (fullscreen, in Nice's protected set), `^⌘Space` (emoji picker),
`^⌘D` (macOS dictionary lookup). **D4**: the Phase 2/3 chords join them in
the guard rather than shipping as no-op actions, so nothing can squat on
them before those phases land — and the 2026-08-11 revision put the
ladder's two Phase 2 rungs (`^⌥⌘hjkl` resize, `^⌥⌘⇧hjkl` swap, eight
chords) in the same group for the same reason. They all live in one
`RESERVED_COMBOS` table in `nice-model`; the recorder refuses them with a
per-entry reason, and `keymap` installs the five fixed accelerators FROM
those same entries so guard and install cannot drift.

**Phase 2 shrank that table from 20 entries to 9.** A chord may never be
both reserved and a default — a test pins the two sets disjoint — so
claiming a reserved chord means deleting its reserved entry in the same
change. Phase 2 claimed `^⌘z` and all eight ladder rungs, and freed
`^⌘v`/`^⌘s` outright (D2 spent the split verbs on the divider mnemonics
instead), leaving the five fixed accelerators, `^⌘Q`/`^⌘Space`/`^⌘D`, and
`^⌘/` held for Phase 3.

- **Hold-to-hint overlay (D5, shipped):** holding `^⌘` for ~200 ms with no
  chord committed paints the jump digit on each of the first nine pills;
  release (or any change to the held set) clears it instantly, so a fast
  `^⌘]` never flashes it. Driven purely by
  `keymap::on_window_modifiers_changed` — it binds no keys and can never
  swallow a chord — over a never-persisted `KeyHintModel` flag, with the
  debounce `Task` on `WindowState` (`nice-model` is gpui-free). The watched
  modifier pair is read from the LIVE next-pill binding, so rebinding the
  scheme keeps a working overlay.
- **Holding `^⌘` also floats the collapsed-sidebar peek** — a side effect of
  the revision, and intended: the peek watches the LIVE sidebar-session
  chords, which are now `^⌘j`/`^⌘k`. One held pair, both affordances.
- The rebindable set grew from 14 to 22 actions. Store migration is
  DELIBERATELY absent: a user who ever rebound anything has the full map on
  disk, so for them the D1 flip does not land and the new ids load unbound
  (frozen load rule 5). Accepted — defaults users get the new board.
- Keystroke-sequence support in `OwnedCombo` is DEFERRED — only needed if
  a tmux-compat prefix mode is ever added.

Full implementation plan: `docs/plans/phase-1-keybind-scheme.md`. Live
gate: the `keybind-scheme` self-test scenario.

### Phase 2 — splits (L, the core investment) — SHIPPED

A **pane** is one leaf of a pill's split tree — the name Phase R freed for
exactly this. Pre-splits every pill is a single-leaf tree, and a pill that
is never split behaves, renders, and persists exactly as it did before.

What shipped:

- **Model** (`nice-model/src/pane_layout.rs`): `PaneLayout` is a binary
  tree, `Leaf(Pane)` or `Split { orient, ratio, first, second }`.
  `TermWindow` grows `layout`, `active_pane_id` (the focused leaf) and a
  never-persisted `zoomed` flag. One geometry function set — `leaf_rects`,
  `directional_neighbor`, `spatial_refocus` — is shared by render,
  keyboard focus, swap and close-refocus, so paint and keyboard can't
  disagree about which pane is where.
- **Pty layer**: `WindowPty` holds a `pane_id → handle` map. Exits, holds,
  titles, cwds and status all route by pane; event subscriptions are
  pane-keyed and retained (droppable and re-keyable) rather than
  permanently detached, so a background pane's exit routes and break-pane
  can re-home a live pty.
- **Host view**: `WindowHostView` mounts the active pill's whole tree as
  nested flex rows/columns landing on exactly the rects `leaf_rects`
  computes. Dividers follow the sidebar resize-handle pattern (cursor flip
  per orientation, root-level drag tracking, min clamp, double-click reset
  to half). The focused pane in a split pill carries four accent corner
  ticks (an enclosure — unambiguous in any layout, and it costs unfocused
  panes' legibility nothing; revised from an accent border 2026-08-13,
  mocks at `docs/mocks/pane-focus-mocks.html`), and every split pane gets
  its own content inset so glyphs never touch a divider.
- **Actions**: twelve new rebindable actions — `^⌘-` Split Down, `^⌘\`
  Split Right, `^⌘z` Zoom Pane, `^⌘b` Break Pane to Window, plus the
  `^⌥⌘hjkl` resize and `^⌥⌘⇧hjkl` swap rungs. The `^⌘⇧hjkl` focus handlers
  that Phase 1 bound inert are now filled in. `ALL` went 22 → 34 actions
  and `RESERVED_COMBOS` 20 → 9.
- **Persistence**: `PersistedTermWindow` grows optional `layout` /
  `activeLeafId`, written only for multi-pane pills, so a never-split
  user's `sessions.json` is byte-identical. `CURRENT_VERSION` stays 3 (the
  loader is tolerant by shape, not by version) and hydration validates or
  falls back to a single leaf — it never errors.

**Deferred** (D3): tmux `select-layout` even-layout presets. Revisit once
the tree exists and the need is felt.

#### Decisions

Product decisions (Nick, 2026-08-12):

- **D1 — any pill splits; panes are plain shells.** Both Claude and
  terminal pills can be split. A new pane always runs a plain shell in the
  focused pane's cwd; Nice never spawns Claude into a split pane, so the
  ≤1-running-Claude-per-session invariant is untouched. Claude in one pane
  with a working shell beside it is the core use case.
- **D2 — divider-mnemonic split chords; the words "vertical" and
  "horizontal" appear nowhere.** `^⌘-` splits Down (stacked — the divider
  looks like `-`); `^⌘\` splits Right (side by side — the `|` key). vim
  and tmux assign those two words opposite meanings, so the scheme
  sidesteps the war entirely. `^⌘v`/`^⌘s` end bound to nothing.
- **D3 — scope: core plus break-pane.** Ships split, directional focus,
  directional resize, directional swap, zoom, break-pane-to-new-pill,
  spatial close-refocus and layout persistence. Even-layout presets wait.

Plan-level decisions:

- **P1 — binary tree.** Every split bisects one leaf. tmux's model is
  equivalent in practice, and geometry, resize and persistence all stay
  simple. Orientation is `SplitOrient::{Beside, Stacked}` per D2.
- **P2 — pane identity stays OFF the frozen surfaces.** Pane ids are plain
  strings internal to the model and pty map. Every pane's pty gets the
  same `NICE_TAB_ID`/`NICE_PANE_ID` as before, so `paneId`/`NICE_PANE_ID`
  permanently mean "pill" and socket traffic from any pane still routes to
  the pill, which is where status lives. Pane-level addressing waits for
  Phase 5's `select-pane`.
- **P3 — break-pane is `^⌘b`, refused on the Claude pane.** It extracts
  the focused shell pane into a new terminal-kind pill after the current
  one, moving the live pty rather than respawning; focus follows, matching
  tmux `break-pane`. No-op on a Claude leaf (pill kind stays coherent) or
  on a single-leaf pill.
- **P4 — zoom is a transient render flag.** Never persisted, all ptys stay
  live, only the focused pane paints. Any structural or focus change
  un-zooms first, then applies — tmux's `select-pane`-unzooms behavior,
  generalized. `^⌘z` toggles.
- **P5 — directional focus does not wrap and does not fall through to
  pill nav.** `^⌘⇧h` with nothing to the left is a no-op; bare `^⌘h`/`^⌘l`
  stays the way to leave the pill.
- **P6 — min pane size refuses, resize clamps.** `PANE_MIN_WIDTH` 120 px /
  `PANE_MIN_HEIGHT` 80 px. A split that would produce a pane under the
  mins is a no-op; divider drags and `^⌥⌘hjkl` clamp against them. The
  window-level `TERMINAL_MIN_WIDTH` is unchanged.
- **P7 — resize moves the nearest matching ancestor.** `^⌥⌘h`/`^⌥⌘l` move
  the nearest `Beside` ancestor's divider, `^⌥⌘j`/`^⌥⌘k` the nearest
  `Stacked` one; no match is a no-op. Which side of the ratio moves
  follows which child the focused pane sits in, so "resize left" always
  moves the focused pane's own edge left — tmux `resize-pane -L/-D/-U/-R`.
  The step is a fixed ~40 px converted against exactly the px that divider
  divides, so a chord moves the same distance regardless of tree depth.
- **P8 — swap swaps leaves, not subtrees.** `^⌥⌘⇧hjkl` finds the
  directional neighbor with the same algorithm focus uses and trades the
  two `Pane` payloads in place; ratios and structure don't move. Focus
  follows the content, as tmux `swap-pane` does.
- **P9 — pill title follows the active pane; status aggregates by OR.**
  Busy/thinking status ORs across all the pill's panes, so a manually-run
  `claude` in a shell pane still lights the pill, while `kind` and
  `is_claude_running` stay driven by the Claude leaf. Session-level
  aggregation is untouched — it still reads per-`TermWindow` fields.

Accepted warts, recorded rather than fixed:

- After break-pane the moved pty's `NICE_TAB_ID`/`NICE_PANE_ID` still name
  the SOURCE pill — env is fixed at fork and can't change. Socket traffic
  from that shell targets the old pill. Same class of staleness as moving
  any live process; Phase 5's pane addressing is where to revisit it.
- A clean Claude exit inside a multi-pane pill removes the Claude leaf and
  flips the pill's kind to Terminal. That is what makes the invariant "at
  most one Claude leaf, and kind == Claude iff one exists" hold. A HELD
  exit keeps the pill Claude-kind with the corpse in its own pane.

Full implementation plan: `docs/plans/phase-2-splits.md`. Live gate: the
`splits` self-test scenario.

### Phase 3 — copy mode + scrollback search (M)
- Keyboard selection state machine over the existing selection API; vi keys.
- Search overlay + `RegexSearch` over scrollback + match highlighting +
  n/N navigation.

### Phase 4 — detach, adopt, tear-off (M-L)
- App-global session registry; "detached sessions" sidebar section;
  close-window-keeps-sessions option; adopt-into-window.
- Revive cross-window pane move / tear-off on the surviving seams
  (`extract_pane`/`insert_pane`, `zed-external-drag-out` already proves the
  NSDraggingSource path).

### Phase 5 — power features (à la carte, S-M each)
- `nice` CLI speaking the control socket: `split`, `send-keys`,
  `new-session`, `select-pane` — tmux's scriptability.
- Synchronize-panes (fan input to all panes in a pill).
- Display-panes numbered-overlay jump; respawn-pane (exists as held-pane
  dismiss; expose as an action).
- Popup terminal (floating overlay pane, `display-popup` analogue).

### Explicitly not porting (native app covers it or out of scope)
- Multiple simultaneous clients on one session; remote multiplexing over SSH
  (run real tmux on the remote); mouse mode (native); config-file reload
  (settings UI + `ui_settings.json` are the config story); status-line
  format strings (sidebar/titlebar already carry the state).
