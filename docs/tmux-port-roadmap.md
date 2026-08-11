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
2. **Real side-by-side terminals** — that's Phase 2 (splits), in the terminal
   area where it belongs.

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
  recorder captures exactly one keystroke.
- **A scriptable IPC seam.** Per-window AF_UNIX control socket with NDJSON
  protocol (`control_socket.rs`), `NICE_SOCKET`/`NICE_TAB_ID`/`NICE_PANE_ID`
  injected into every pty env — the foundation for tmux-style
  `send-keys` / `split-window` CLI scripting.
- **Search/selection primitives.** Vanilla alacritty_terminal 0.26:
  `RegexSearch`/`RegexIter` already linked (used for hyperlinks,
  `hyperlink.rs:92-233`); full programmatic selection API on the handle
  (`session_handle.rs:517-618`); scrollback + scroll-position API, with
  keyboard scroll bindings since Phase 0 (Shift+PageUp/PageDown/Home/End).
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

### Splits (the big one)
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

### Phase 1 — held-modifier keybind scheme (M)

**DECIDED (2026-08-05): no tmux-style prefix sequences. The scheme is
vim keys held under `^⌘` (control+command).** Held-modifier chords are
single keystrokes to gpui — no pending-prefix state machine, no timeout,
no send-literal escape hatch needed (nothing is stolen from the pty:
`should_encode`'s ctrl branch is `ctrl && !cmd`, so any ⌘-bearing chord
is never terminal-owned). OS key-repeat gives free continuous navigation
(hold `^⌘` + hold `j`), which tmux only approximates with `repeat-time`.

Locked-in bindings:

| Chord | Action |
|---|---|
| `^⌘h/j/k/l` | Directional pane focus (pre-splits: `h`/`l` = prev/next pane) |
| `^⌘⇧h/j/k/l` | Resize split toward that edge (or swap — finalize in Phase 2) |
| `^⌘[` / `^⌘]` | Prev / next upper-bar pill **(decided over `^⌘n`/`^⌘p`)** |
| `^⌘1-9` | Pill by index |
| `^⌘o` | Last-active pane |
| `^⌘z` | Zoom pane |
| `^⌘v` / `^⌘s` | Vertical / horizontal split |
| `^⌘u` / `^⌘d` | Half-page scrollback up / down |
| `^⌘/` | Scrollback search |

Reserved — never bind: `^⌘Q` (macOS lock screen, system-intercepted),
`^⌘F` (fullscreen, in Nice's protected set), `^⌘Space` (emoji picker),
`^⌘D` (macOS dictionary lookup).

- **Hold-to-hint overlay (decided, part of the scheme):** while `^⌘` is
  held, show pane numbers/hints as an overlay — tmux `display-panes`, but
  live for the duration of the hold. Build on the existing modifier-state
  observer used by sidebar peek (`keymap::on_window_modifiers_changed`,
  `keymap.rs:546-606`) + the present-kick pattern.
- Grow the rebindable action set accordingly; all chords rebindable via
  the existing shortcuts store/recorder (single-keystroke capture already
  suffices — no sequence support needed).
- Keystroke-sequence support in `OwnedCombo` is DEFERRED — only needed if
  a tmux-compat prefix mode is ever added.

### Phase 2 — splits (L, the core investment)
- Model: layout tree per upper-bar pill (leaves = pane ids, splits with
  ratios); `active_pane_id` = focused leaf.
- `PaneHostView`: recursive render, N mounted views, dividers with
  drag-resize, focused-pane affordance.
- Actions (all on the Phase-1 `^⌘` scheme): split h/v, directional
  focus move, resize, zoom toggle, swap, break-pane-to-new-pill,
  even-layout presets; finalize whether `^⌘⇧h/j/k/l` means resize or swap.
- Spatial close-refocus; multi-activation in `SessionManager`.
- Persist `layout` in `PersistedTab` (version bump; loader stays
  shape-tolerant).

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
