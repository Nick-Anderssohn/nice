# Phase 0 — quick wins (sidebar width + keyboard scrollback) + two parked nits

Roadmap: `docs/tmux-port-roadmap.md` § "Phase 0 — quick wins (S)". Implemented
inline in the `tmux-keybinds` worktree (branch `worktree-tmux-keybinds`, based
on main `4eceb20`). Two Phase 0 features plus two nits parked from the Phase R
cycle.

## Slice 1 — sidebar width: dynamic max + persistence

Today: `SIDEBAR_MAX_WIDTH = 480` (`crates/nice-theme/src/chrome_geometry.rs:32`),
enforced by `clamp_sidebar_width` (`crates/nice/src/sidebar_shell.rs:187`).
Width is view-local in `SidebarShellView.sidebar_width`, reset to
`SIDEBAR_DEFAULT_WIDTH` every launch.

### 1a. Clamp against remaining terminal width

- Delete `SIDEBAR_MAX_WIDTH`; the max becomes dynamic:
  `viewport_width - TERMINAL_MIN_WIDTH`, where `TERMINAL_MIN_WIDTH`
  is a new constant in `chrome_geometry.rs` (**proposal: 300pt** — enough for
  a usable ~40-col terminal at default font; Nick may tune).
  Keep `SIDEBAR_MIN_WIDTH = 160` as-is. Guard the degenerate case (tiny
  window): max never drops below `SIDEBAR_MIN_WIDTH`, i.e.
  `clamp(width, MIN, max(MIN, viewport_w - TERMINAL_MIN_WIDTH))`.
- `clamp_sidebar_width` / `resize_width` grow a `viewport_width: f32` param
  (stay pure/unit-testable). Drag handlers (`on_resize_mouse_down` /
  `on_root_mouse_move`) read `window.viewport_size()` — they already receive
  `&mut Window`.
- **Window-shrink re-clamp:** the rendered width also clamps at render time
  against the current viewport, so shrinking the OS window never leaves the
  sidebar swallowing the terminal. The *stored* width is NOT rewritten by a
  transient window shrink — clamp on read, so re-widening the window restores
  the user's chosen width (matches how frame restore treats fullscreen).
- Update the Swift-parity assert in `chrome_geometry.rs` (drops the
  `SIDEBAR_MAX_WIDTH == 480` line — deliberate divergence, note it) and the
  `sidebar_shell.rs` clamp unit tests. Update the `sidebar_live.rs` self-test:
  the "drag far right" expectation changes from `SIDEBAR_MAX_WIDTH` to the
  viewport-derived max.

### 1b. Persist `sidebar_width` in `PersistedWindow`

- Move the committed width from `SidebarShellView` into `SidebarModel`
  (`nice-model`), beside `collapsed`/`mode` — the shell keeps only transient
  drag state (`drag_start_width`, `resize_origin_x`) and reads/writes the
  model through `WindowState`, mirroring the collapse path
  (`toggle_sidebar_collapsed`). Width commits on drag-end and double-click
  reset, not per-mouse-move (avoids store churn mid-drag; the store debounce
  would absorb it anyway, but per-move writes through WindowState would
  notify-storm).
- New OPTIONAL field on `PersistedWindow` (`session_store.rs`): JSON key
  `sidebarWidth` (camelCase via the struct's rename-all), `Option<f64>`,
  `skip_serializing_if = "Option::is_none"` + `#[serde(default)]` — same
  non-breaking schema-slot pattern as R19's `sidebar_mode`. Absent ⇒
  `SIDEBAR_DEFAULT_WIDTH`. Adding a NEW key does not violate the frozen-v3
  surface (shape-tolerant decode; old files stay byte-stable — the
  byte-identity fixture has no `sidebarWidth`, so `is_none` skip keeps it
  byte-identical).
- Wire snapshot (`persisted_snapshot` reads `self.sidebar.width()`) and
  restore (`restore.rs` `WindowSeed` gains the width; seeds `SidebarModel::new`).
  Restored width is clamped on read against the restored window's width.

## Slice 2 — keyboard scrollback (PageUp/PageDown/Home/End)

Today scrolling is wheel-only (`TerminalSessionHandle::scroll_lines` /
`scroll_to_bottom`, `session_handle.rs:630/641`). All four named keys always
encode to the pty (`dispatch_key`'s `named ⇒ should_encode` branch,
`view.rs:1090`).

- **Key policy (pure fn, unit-tested):** a keystroke scrolls Nice's viewport
  instead of encoding when:
  - `Shift+PageUp` / `Shift+PageDown` / `Shift+Home` / `Shift+End` — always
    scroll (page up / page down / jump top / jump bottom), UNLESS the terminal
    is in the alternate screen (`TermMode::ALT_SCREEN`) — a fullscreen TUI has
    no scrollback, keys go to the app.
  - Plain (unshifted) PageUp/PageDown/Home/End keep today's behavior: encode
    to the pty (`\e[5~`/`\e[6~`/Home/End — less, vim, Claude Code depend on
    them). This is the alacritty/Ghostty convention.
- Intercept in `dispatch_key` BEFORE the encode branch (after IME/held gates
  in `on_key_down`, which already run). A scrolled key consumes the event
  (`stop_propagation`), notifies only via the handle's context (the wheel
  path's repaint discipline), and deliberately does NOT snap-to-bottom (it is
  navigation, not typing — same carve-out as ⌘C).
- New `TerminalSessionHandle` methods beside `scroll_lines`:
  `scroll_page_up` / `scroll_page_down` (`Scroll::PageUp`/`PageDown` — core
  computes the page size) and `scroll_to_top` (`Scroll::Top`);
  `scroll_to_bottom` exists. Each clears `scroll_accum` like
  `scroll_to_bottom` does.
- Note: `^⌘u`/`^⌘d` half-page scroll is Phase 1 (keybind scheme), not here.

## Slice 3 — nit: `|p|`/`|t|` closure-binding rename sweep

Phase R renamed types but left closure params bound to the old initials:
`|p|` where the bound value is a `TermWindow` (old "pane") and `|t|` where it
is a `Session` (old "tab"). Sweep them to `|w|` / `|s|` respectively.

- NOT a blind regex: `|p|` legitimately binds projects/paths/points and `|t|`
  binds themes/terms. Only rebind where the closure's subject is a
  `TermWindow`/`Session` (or their ids). Grep candidates crate-by-crate, edit
  with type awareness, matching multi-param forms (`|p, cx|` etc.).
- Zero behavior change; `cargo test --workspace` green is the gate.

## Slice 4 — nit: byte-identity fixture's hand-appended `\n`

`re_serializing_pre_rename_fixture_is_byte_identical`
(`session_store/tests.rs:947`) does `bytes.push(b'\n')` because the checked-in
fixture has a trailing newline the production writer (`serialize_state` →
`write_atomic`) never emits. Fix the fixture, not the test: strip the trailing
newline from `crates/nice/src/fixtures/pre_rename_sessions_v3.json` and drop
the `push` line, so the test compares the writer's true byte output. Verify
first that the writer indeed emits no trailing newline; confirm the other
fixture consumer (`restores_pre_rename_sessions_json_fixture`) is
newline-insensitive (it decodes JSON — it is).

## Ordering

Slices 4 → 1 → 2 → 3 (fixture nit first so the byte-identity gate is honest
before Slice 1b touches `PersistedWindow`; the big mechanical sweep last so it
never conflicts with real edits).

## Validation

- **Unit:** new/updated pure-fn tests — dynamic clamp (min floor, dynamic max,
  degenerate tiny window), scroll-key policy (shift/plain × primary/alt
  screen), `PersistedWindow` round-trip with/without `sidebarWidth`, byte
  identity vs the fixed fixture.
- **Targeted `cargo test`:** `-p nice-theme`, `-p nice-term-view`, `-p nice`
  (session_store, sidebar_shell, window_state, restore modules) during fix
  rounds; one full `cargo test --workspace` before install (required by the
  Slice 3 sweep anyway). Output to a log file, check `$?` (no `tail` piping).
- **Live self-test:** updated `sidebar_live.rs` scenario under the worktree
  lock (drag-far-right now settles at viewport-derived max; low clamp
  unchanged).
- **Manual feel-check (Nick, after `scripts/rust-install.sh` under the lock):**
  1. Drag sidebar wide — stops short of crushing the terminal; shrink the OS
     window — sidebar yields; re-widen — chosen width returns.
  2. Quit + relaunch — sidebar width restored per window.
  3. In a scrolled shell: Shift+PageUp/PageDown page through history,
     Shift+Home/End jump to top/bottom; plain PageUp still reaches `less`/vim;
     inside vim (alt screen) Shift+PageUp goes to vim, not the viewport.
