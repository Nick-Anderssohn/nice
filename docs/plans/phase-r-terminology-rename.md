# Phase R — adopt tmux terminology across the codebase

Self-contained implementation plan (written 2026-08-08). First phase of the
tmux-port roadmap (`docs/tmux-port-roadmap.md`); tracker at
`docs/tmux-port-progress.html`.

## Goal

Rename the code's session/window vocabulary to match tmux's, so all later
tmux-port phases (splits, copy mode, detach) are built under the right names.
This is a refactor with **zero behavior change** except deliberate UI copy
updates. All disk and wire formats stay byte-identical.

## The mapping (decided, do not re-litigate)

| tmux term | Today | New name |
|---|---|---|
| session | `Tab` (sidebar row) | `Session` |
| window | `Pane` (upper-bar pill, one pty) | `TermWindow` |
| pane | — (arrives with splits in a later phase) | reserved: `Pane` |
| client | OS window (`WindowState`) | unchanged |
| — | `Project` (sidebar section) | unchanged |

Decisions already made:
- **`TermWindow`, not `Window`** — avoids collision with `gpui::Window`
  (present in most render signatures). Chosen 2026-08-08.
- **UI copy renames too** (sidebar rows are "sessions", pills are "windows"
  in menus/tooltips/settings text). Chosen 2026-08-08.
- The word "pane" must be FREE at the end of this phase (no type, module,
  field, or action named pane) — it is reserved for future split leaves.

## Naming rules for derived identifiers

- The model **type** is `TermWindow`; derived identifiers use plain
  `window`/`windows` wording wherever that does not collide with a gpui name
  or read as the OS window. Examples: `Tab.panes` → `Session.windows`,
  `active_pane_id` → `active_window_id`, `add_pane` → `add_window`,
  `pane_strip` → `window_strip`.
- Collision exceptions (check each against gpui's public names):
  `PaneKind` → `TermWindowKind` (gpui already has a `WindowKind`).
- Inside `WindowState` (which manages the OS window), method names where
  bare "window" would read as the OS window use `term_window`:
  e.g. `request_close_pane` → `request_close_term_window`.
- The pty-sense of "session" moves out of the way at the app layer:
  `SessionManager` → `PtyManager` (`session_manager.rs` → `pty_manager.rs`),
  `PaneSession` → `WindowPty`. **Do NOT rename pty-sense "session" types in
  `nice-term-core` / `nice-term-view`** (`TermSession`, `Session` in
  `deferred.rs`, `SessionEvent`, `TerminalSessionHandle`) — they are
  crate-namespaced, mean "pty session", and renaming them balloons the
  diff for no ambiguity gain. Optional later cleanup, out of scope here.
- Rename files/modules to match their types (`tab.rs` → `session.rs`,
  `pane.rs` → `term_window.rs`, `tab_model.rs` → `workspace_model.rs`, …).

## Concrete rename inventory (primary surfaces)

`crates/nice-model/`:
- `Tab` → `Session` (`tab.rs` → `session.rs`); fields `panes` → `windows`,
  `active_pane_id` → `active_window_id`.
- `Pane` → `TermWindow` (`pane.rs` → `term_window.rs`); `PaneKind` →
  `TermWindowKind`.
- `TabModel` → `WorkspaceModel` (`tab_model.rs` → `workspace_model.rs`) —
  it is the per-OS-window document root (projects → sessions → windows),
  so "workspace", not "session", is the honest name. Its API renames
  mechanically (`add_pane` → `add_window`, `move_tab` → `move_session`,
  `select_next_sidebar_tab` → `select_next_sidebar_session`,
  `insert_handoff_child` keeps its name, etc.).
- `pane_strip_drop.rs` → `window_strip_drop.rs`; `persisted.rs` types:
  `PersistedTab` → `PersistedSession`, `PersistedPane` →
  `PersistedTermWindow` (serde compat below).
- `shortcuts.rs`: `ShortcutAction` variants referencing panes/tabs rename in
  code but keep their serialized ids (serde compat below).

`crates/nice/`:
- `session_manager.rs` → `pty_manager.rs`: `SessionManager` → `PtyManager`,
  `PaneSession` → `WindowPty`; methods mechanically (`activate_pane` →
  `activate_window`, `close_tab` → `close_session`, `create_claude_tab` →
  `create_claude_session`, `pane_handle` → `window_handle`,
  `dissolve_tab_if_empty` → `dissolve_session_if_empty`, …).
- `pane_strip_actions.rs` → `window_strip_actions.rs` (`PaneStripActions` →
  `WindowStripActions`, `step_active_pane` → `step_active_window`).
- `sidebar_actions.rs`: tab-sense methods → session wording.
- `app_shell.rs`: `PaneHostView` → `WindowHostView`; `active_pane_target` →
  `active_window_target`; `pane_placeholder` → `window_placeholder`.
- `toolbar.rs`: `snapshot_panes` → `snapshot_windows`, `PaneDragPayload` →
  `WindowDragPayload`; comments describing "pane strip" → "window strip".
- `window_state.rs`: field `model: TabModel` → `workspace: WorkspaceModel`;
  `request_close_tab` → `request_close_session`; `request_close_pane` →
  `request_close_term_window`; keep `WindowState` itself.
- `keymap.rs`, `settings/shortcuts_pane.rs`, `settings_import.rs`,
  `restore.rs`, `session_store.rs`, `lifecycle.rs`, `inline_rename.rs`,
  `status_dot.rs`, `sidebar_shell.rs`: follow the ripple mechanically.
- Tests everywhere: rename to match; test *behavior* unchanged.

Comments and internal docs (`crates/README.md`, module docs) that describe
tabs/panes in the old sense: update the wording where it would now mislead.
Do not rewrite history-flavored docs under `docs/research/`.

## Frozen surfaces — MUST stay byte-identical

1. **`sessions.json`** (schema version 3, unchanged): every serialized key
   keeps its current spelling — `panes`, `activePaneId`, `parentTabId`,
   `nextTerminalIndex`, `kind` values, all of it. Renamed struct fields get
   `#[serde(rename = "...")]` (or keep the old field name where simpler).
   The pretty-printed sorted-key writer must produce output identical to
   pre-rename for the same state.
2. **`ui_settings.json` `shortcuts` section**: `ShortcutAction` ids as
   serialized (map keys) keep their exact current strings via serde
   attributes. The frozen load rules in `shortcuts_store.rs:22-39` must
   hold unmodified.
3. **Control socket NDJSON protocol** (`control_socket.rs`): message names
   and field names unchanged (`handoff`, `dispatch`, `session_update`,
   `claude`, and their payload keys).
4. **Pty environment**: `NICE_TAB_ID`, `NICE_PANE_ID`, `NICE_SOCKET`,
   `ZDOTDIR`, `NICE_PREFILL_COMMAND` — external scripts and skills read
   these; spellings frozen.
5. Any accessibility/test identifier strings (e.g. `test.*` ids) keep their
   current values.

## UI copy pass

- Sidebar rows: "tab" → "session" wherever user-visible (context menus,
  rename affordances, tooltips, settings text, confirmation modals —
  e.g. "Close Tab" → "Close Session").
- Upper-bar pills: "tab"/"pane" → "window" ("New Window" for ⌘T, etc.).
- Ambiguity rule for menus: never relabel macOS-standard items — ⌘N stays
  "New Window" (the OS window). Where the tmux sense would collide with it
  in the same menu, qualify the tmux sense (e.g. ⌘T as "New Terminal
  Window") rather than the OS sense. Implementer picks final wording;
  flag choices in the PR description for feel-check.
- Default window titles ("Terminal N") are user-visible data, not copy —
  unchanged.

## Suggested slice seams (implementer may re-slice)

1. **nice-model**: all model-crate renames + serde compat shims + a
   restore-compat fixture test (see below). Compiles green standalone.
2. **App crate ripple**: `PtyManager`, actions seams, `WindowState`,
   keymap/shortcuts serde ids, store/restore.
3. **Views + UI copy**: `toolbar.rs`, `app_shell.rs`, `sidebar_shell.rs`,
   settings panes; the copy pass.

## Acceptance criteria

- `cargo build --workspace` and `cargo test --workspace` green;
  `cargo test -p nice-itests` green.
- Grep gates: no remaining identifier uses of `pane`/`Pane` anywhere in
  `crates/nice*/` (the word is reserved), except (a) the frozen strings
  listed above, (b) `docs/research/` history. No remaining tmux-sense uses
  of `tab`/`Tab` in `nice-model`/`nice` identifiers (gpui's own API names
  and the frozen strings excepted).
- **Restore-compat test**: a checked-in pre-rename `sessions.json` fixture
  (version 3, with projects/tabs/panes, a claudeSessionId, a parentTabId)
  loads correctly, and re-serializing produces the same keys.
- **Shortcuts-compat test**: a pre-rename `shortcuts` section loads with
  every binding intact and round-trips with identical ids.
- No behavior change: all existing tests pass with only naming updates.
- UI copy changes enumerated in the PR description.

## Validation

All validation is build/test-level. **Live-app validation: none — justified
opt-out**: this plan has zero behavior change beyond UI copy strings; the
wording feel-check happens at merge review (screenshots of renamed
menus/context menus optional). Do not install or launch any app bundle.

1. `cargo build --workspace` — expected: clean compile, no new warnings.
2. `cargo test --workspace` — expected: all tests pass (tests are renamed
   with the code, never deleted to get green).
3. `cargo test -p nice-itests` — expected: all pass.
4. Restore-compat fixture test added by this plan (suggested:
   `restores_pre_rename_sessions_json_fixture`) — expected: pass; proves a
   checked-in pre-rename v3 `sessions.json` loads correctly and
   re-serializes with byte-identical keys.
5. Shortcuts-compat fixture test added by this plan — expected: pass; a
   pre-rename `ui_settings.json` `shortcuts` section loads with every
   binding intact and round-trips with identical action ids.
6. Grep gates, run from the worktree root; expected: empty output after an
   allowlist covering exactly the frozen strings (implementer pastes the
   final commands + their empty output into the cycle summary):
   - tmux-sense `pane`/`Pane` identifiers:
     `grep -rnE '\b[Pp]anes?\b' crates/ --include='*.rs'` filtered to
     exclude only `NICE_PANE_ID`, serialized-key literals/serde renames
     (`activePaneId`, `panes`, …), and frozen test-id strings.
   - tmux-sense `tab`/`Tab` identifiers in `nice-model`/`nice` similarly
     (gpui API names and frozen strings excepted).
7. `git diff --stat` sanity — expected: no changes under `vendor/` or to
   any file in the frozen-surfaces list beyond serde attributes.

## Notes for the cycle

- Base: `main`. Suggested branch: `phase-r-tmux-rename`. Suggested merge
  title: `refactor: adopt tmux terminology (sessions/windows) — Phase R`.
- Test notes: targeted tests per touched module during fix rounds; the two
  compat fixtures are the critical additions. No GUI/black-box validation
  needed (no behavior change beyond copy); an optional screenshot of the
  renamed menus/context menus for the feel-check.
- This plan is self-contained; reviewers should check the frozen-surfaces
  list above against the diff explicitly.
