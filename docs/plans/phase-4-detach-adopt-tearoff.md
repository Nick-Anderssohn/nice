# Phase 4 — detach, adopt, tear-off

**Status:** READY FOR SIGN-OFF — twice Fable-reviewed. Round 1: 3
blocking + 7 important + 7 nit, all folded
(`.claude/handoff/phase-4-plan-review.md`). Round 2: 15/17 folds
verified, 2 partial (one-line edits, folded), fresh 1 blocking + 2
important + 6 nit, all folded
(`.claude/handoff/phase-4-plan-review-round2.md`); round-2 verdict:
sign-off ready once folded, no third round needed. Grounded against
main `6aee7ac` (post bash-support merge `1c0c374`, post
stable-control-socket `341d234`).

**Goal:** sessions can outlive their OS window. Closing a window detaches
its running sessions into an app-global pool instead of killing them; the
pool renders as a "Detached" sidebar section in every window; clicking a
detached row (or a menu item / chord) adopts it into a live window — pty
moved live, no respawn, when the process still runs; respawn-on-activate
when it doesn't. A focused pane can be torn off into its own new OS window
by action. This is tmux `detach` / `attach` / `break-pane -d`+`move-window`
in Nice's native shape, and it closes the stable-control-socket plan's
carried-forward gap (a session whose window is never restored holds a dead
socket path — adoption re-homes it; see § Decisions D-P11).

The stable-socket bullet of roadmap § Phase 4 is already shipped
(2026-08-16, `docs/plans/stable-control-socket.md`) — nothing here
re-plans it.

## Current-code facts the plan builds on

Everything below is on main today; cites are file:line at `6aee7ac`.

### Ownership is strictly per-OS-window

- `WindowRegistry` (`window_registry.rs:98-103`, a gpui `Global`,
  installed once at `:113-117`) maps `WindowId → Entity<WindowState>` plus
  an MRU order. It holds **only** live windows' states; `deregister`
  (`:241-245`) drops the strong entity handle. Nothing in it survives a
  window's close. Cross-window lookups already exist and are exercised:
  `state_for_window` (`:166-168`, 20+ call sites),
  `state_for_window_session_id` (`:172-187`), `state_for_claude_session`
  (`:203-221`), `all_states` (`:234-238`).
- `WindowState` (`window_state.rs:383-551`) owns, per window: the whole
  model tree (`workspace: WorkspaceModel` — "two windows are isolated
  precisely because each owns its own `WorkspaceModel`", `:384-385`), the
  pty manager (`ptys: PtyManager`, `:412-417`), the persisted identity
  (`window_session_id`, `:419`), the control socket (`:427-432`), and the
  pane event subscriptions (`pane_subscriptions`, `:478`, idempotent
  ensure/retain pass at `:1023-1143`).
- `PtyManager` is per-window (`pty_manager.rs:4`; constructed at
  `window_state.rs:610`). Its registry is
  `sessions: HashMap<session_id, HashMap<term_window_id, WindowPty>>`
  (`:323-325`); `WindowPty` holds `panes: HashMap<pane_id, PaneState>`
  (`:210-232`); `PaneState { handle: Entity<TerminalSessionHandle>,
  shell: PaneShell, pending_prefill: Option<String> }` (`:238-266`,
  post-bash-support). Dropping a `PaneState` SIGHUP→SIGKILLs the child's
  process group (comment `:210-212`; `nice-term-core/src/session.rs:391`).

### What window close does today

- Every close routes through `WindowRegistry::handle_window_closed`
  (`:258-263`) → `route_close_disk_fate` (`:295-313`): deregister → read
  `user_initiated_close` + `persisted_snapshot()` → disk fate via
  `lifecycle::close_disposition(app_quitting, user_initiated)`
  (`lifecycle.rs:73-83`: quit ⇒ `Preserve`, confirmed user close ⇒
  `Remove`, default ⇒ `Preserve`) → `session_store::flush()` →
  `state.teardown()`.
- `WindowState::teardown` (`:3186-3206`): drop drains → stop+unlink the
  control socket → `pane_subscriptions.clear()` → `ptys.teardown()`
  (`pty_manager.rs:2734-2742`, `sessions.clear()`) — **every live pty in
  the window dies, including running `claude` processes.** The Claude
  conversation survives only as a transcript; Nice records
  `claude_session_id` (`nice-model/src/session.rs:58-60`) and respawns
  `claude --resume` in a fresh pty on next activation.
- The ⌘W / red-button gate is `request_window_close` (`app.rs:947-980`):
  `AppQuitting` ⇒ close unconditionally; zero live windows
  (`live_window_counts`, `window_state.rs:2388` →
  `workspace_model.rs:172`) ⇒ set `user_initiated_close` and close; else
  present the confirmation (`present_confirmation`,
  `window_state.rs:2488`) and veto the immediate close.
- App quit: `should_quit_after_close` (`window_registry.rs:278-285`,
  `:321-323`) quits when the registry is empty **and** no Settings window
  is live — Settings is today's only window-less-app precedent.
- ⌘Q counts live windows across all windows: `total_live_window_counts`
  (`app.rs:654-658`).

### Persistence (`sessions.json` v3 — FROZEN surface)

- `PersistedState { version, windows }` (`session_store.rs:146-150`),
  `CURRENT_VERSION = 3` (`:76`). Read is shape-tolerant: no version gate,
  unknown fields ignored, corrupt file ⇒ `empty()` (`:14-18`,
  `:217-225`). Adding an optional top-level field needs **no version
  bump** (precedent: `layout`, `sidebarMode`, `sidebarWidth`).
- `PersistedWindow` (`:111-134`): `id` (identity; restored windows keep it
  verbatim — `restore.rs:29-31`, `window_state.rs:684`),
  `active_session_id` (JSON `activeTabId`), sidebar fields, `projects`,
  `frame`. `PersistedProject` (`persisted.rs:154-161`): `id`, `name`,
  `path`, `sessions` (JSON `tabs`). `PersistedSession`
  (`persisted.rs:114-143`): `id`, `title`, `cwd`, `claude_session_id`,
  `active_window_id` (JSON `activePaneId`), `windows` (JSON `panes`),
  `title_manually_set`, `parent_session_id` (JSON `parentTabId`),
  `next_terminal_index`. `PersistedTermWindow` (`persisted.rs:84-106`)
  and `PersistedPane` (`persisted.rs:40-47`) carry the pane tree.
  Runtime-only state (pty handles, scrollback, liveness) is dropped on
  snapshot by design (`persisted.rs:9-12`, `:226-230`).
- Restore: `run_restore_fan_out` (`app.rs:614-650`) → per restorable
  window `hydrate_seed` (`restore.rs:64-74`) →
  `open_managed_window_with(cx, Some(seed), …)` (`app.rs:1661-1764`).
  Restored windows **lazy-spawn** ptys on activation (`app.rs:1657-1658`);
  a session with a `claude_session_id` respawns via the resume machinery
  (`ClaudeSessionMode::ResumeDeferred`, `window_state.rs:1965`).
- `build_window_root` (`app.rs:1831+`) registers the state, wires the
  close gate, the debounced-save observer (`wire_tree_mutation_save`,
  `:1789-1817`) and MRU/bounds observers. Its comment already anticipates
  this phase: "R18 will hand this restored state, R25 an adopted window —
  they change what `WindowState::new` produces, not this wiring"
  (`app.rs:1828-1830`).

### Live-move precedent, and what does NOT exist

- `PtyManager::move_pane_to_new_window` (`pty_manager.rs:1259-1333`,
  break-pane) is the only live pty move: extract the pane from its
  `PaneLayout` (`pane_layout.rs:412` `remove`), mint a new pill via
  `WorkspaceModel::insert_window` (`workspace_model.rs:551`), re-key the
  live `PaneState` — same `Entity`, no respawn, "the child never
  notices." Confined to one session in one window; refuses single-leaf
  pills and Claude leaves (`:1269-1271`). Its **accepted wart (P2)** is
  documented at `:1253-1258`: `NICE_TAB_ID`/`NICE_PANE_ID` were fixed at
  fork and still name the source pill — env cannot change post-fork;
  Phase 5's pane addressing revisits.
- The roadmap's `extract_pane`/`insert_pane` names are stale: what exists
  is pill-level `extract_window`/`insert_window`
  (`workspace_model.rs:520`/`:551`), both same-session only. There is
  **no** primitive for splicing a pane into an existing split tree
  (`PaneLayout` has only `single` `:248` and `remove` `:412`), and no
  cross-`WorkspaceModel` move of any kind (cross-window pill move was
  deliberately CUT — `crates/README.md:635-637`).
- Held panes (`pty_manager.rs:1344-1408`) are the "keep the handle, flip
  the liveness bits" template: a dead process's pane stays mounted with
  scrollback readable; the pty entry is deliberately not released
  (`:1378-1380`).
- Model-side session surgery that exists: `remove_session(project_index,
  session_index) -> Session` (`workspace_model.rs:1193`),
  `project_session_index` (`:218`), `clear_dangling_parent_references`
  (`:1205`), `ensure_project_by_path` (`:794`),
  `add_session_to_projects` (`:825`), `is_terminals_project_session`
  (`:260`), `ensure_terminals_project_seeded` (`:725`).

### Sockets and env (post stable-control-socket)

- Each window's socket path is `$TMPDIR/nice-w-<12hex>.sock` keyed on the
  **OS window's** persisted id (`mint_window_socket_path`,
  `control_socket.rs:1076-1090`), armed by `arm_window_control_socket`
  (`app.rs:1557-1622`) which stamps `WindowShellEnv` only after `start()`
  resolves the final path (`:1585-1596`).
- `NICE_SOCKET`/`NICE_TAB_ID`/`NICE_PANE_ID` are injected per pty at fork
  (`session_window_env_pairs`, `pty_manager.rs:2141-2156`;
  `build_claude_extra_env`, `:2935-2951`) and are frozen in the child's
  env. There is no re-stamping mechanism of any kind.
- The socket plan's Known-gaps section carries exactly this phase's
  target: "a session whose window is never restored still holds a dead
  path — revisit with adopt-into-window"
  (`docs/plans/stable-control-socket.md:21-24`).

### Sidebar, settings, shortcuts, misc

- Sidebar sections today = projects only: `snapshot_groups`
  (`sidebar_shell.rs:764-805`) reads one window's `workspace.projects`
  into `GroupVm { id, name, is_terminals, is_open, hovered, sessions }`
  (`:421`) with `SessionVm` rows (`:406`). No non-window-scoped source
  exists.
- `ui_settings.json` sections are co-owned via the read-merge-write
  writer; the Advanced pane's `AdvancedSection { smooth_scroll, shell }`
  (`settings/prefs_store.rs:48-54`) is the natural home for a new
  boolean.
- Shortcuts: 36 actions (`shortcuts.rs:210`; the "34" in the type doc
  `:37` is stale). `RESERVED_COMBOS` = 8, all OS/Nice-claimed
  (`:1068-1103`); the `FuturePhase` group is empty. New action ids are
  additive-safe (`:349-351`). The bare-⌃⌘ rung has spent: hjkl, o,
  up/down, 1-9, `-`, `\`, z, b, c, `/`; ⌃⌘U is explicitly free
  (`:653-657` — which also records that macOS's dictionary hotkey eats a
  real ⌃⌘D keydown before the app sees it).
- gpui exposes a dock-reopen hook Nice doesn't use yet: `on_reopen`
  (vendor `gpui/src/app.rs:224`, platform seam `platform.rs:190`).
- `--uitest-tearoff-hook` / `test.tearOffActivePane` (memory) are
  Swift-era: zero hits in any `.rs` file — only historical docs. Nothing
  to reuse.

## Decisions (RESOLVED — Nick, 2026-08-16)

<!-- PROTECTED --> **D1 — closing a window detaches by default.** ⌘W /
red button moves the window's detach-eligible sessions (see P2) into the
app-global detached pool; sessions with nothing live keep today's
`Remove` fate. A Settings ▸ Advanced toggle ("Closing a window detaches
its running sessions", default ON) restores kill-on-close, which
reinstates today's confirm-then-kill flow. With detach ON the close
confirm is skipped entirely — nothing is destroyed, so there is nothing
to confirm.

<!-- PROTECTED --> **D2 — the detached pool persists across app quit.**
A new deliberately-designed frozen bucket in `sessions.json` (see P4).
Processes cannot survive quit (no daemon — the roadmap's chosen
architecture); detached rows reappear after relaunch as re-attachable
structural entries (cwd + `claude --resume` prefill) and respawn on
adopt-activate.

<!-- PROTECTED --> **D3 — adoption is click + context menu + keybind; no
drag.** Clicking a detached row adopts it into the window whose sidebar
was clicked, then focuses it. Right-click offers Adopt into This Window /
Open in New Window / Kill. A chord adopts the most recently detached
session into the active window. Drag-to-adopt is out of scope.

<!-- PROTECTED --> **D4 — tear-off ships as an action only.** A menu item
+ chord moves the focused pane into its own new OS window on the proven
seams (break-pane extraction + `open_managed_window_with`). Drag-a-pane-
out (and drag-into-another-window) stays in Phase 5 — the gpui
cross-OS-window drag mechanism is unproven and does not gate this phase.

<!-- PROTECTED --> **D5 — the app stays alive window-less while the pool
holds live sessions.** tmux-server parity: detach never kills. Closing
the last window leaves Nice running (dock + menu bar only);
`should_quit_after_close` gains a pool check; dock-icon reopen (or ⌘N)
opens a fresh window showing the Detached section. ⌘Q still quits — and
must count the pool's live sessions in its confirm.

### Plan-level decisions (mine — flag at sign-off if any grates)

- **P1 — no wholesale "sessions become app-level objects" migration.**
  Windows keep owning their attached sessions exactly as today; the
  app-global piece is a `DetachedPool` holding ONLY detached entries.
  The roadmap's "app-global session registry" is satisfied by the pool
  plus the already-live `WindowRegistry` cross-window lookups — moving
  every attached session app-global would rewrite the ownership story of
  every feature shipped on it (YAGNI, and the isolation invariant at
  `window_state.rs:384-385` is load-bearing for multi-window
  correctness). The pool is a gpui `Entity<DetachedPool>` held via a
  `Global` handle (the `SettingsPrefsStore` global precedent +
  entity-observation so every window's sidebar can `cx.observe` it —
  plain globals aren't observable).
- **P2 — detach-eligibility = model liveness, and model-alive-but-ptyless
  sessions detach as structural entries** (review B1). The predicate is
  the same fold the close confirm counts: ≥1 term window with
  `is_alive` (`live_window_counts`, `workspace_model.rs:168-172`) — and
  that fold DELIBERATELY counts restored-unspawned windows as alive
  (hydrate sets `is_alive: true`: `persisted.rs:260-261`,
  `term_window.rs:120`, `pane_layout.rs:219`; "the Swift quirk, preserved
  deliberately"). So: sessions with live ptys detach as live entries;
  model-alive sessions with NO pty (never-activated lazy-restored rows,
  resumable Claude included) detach as **structural entries** (empty pty
  map — the exact shape P6 already supports for the post-restart case;
  zero new machinery). Only model-dead sessions (every window held/exited)
  follow today's `Remove`. This is what makes D1's no-confirm branch
  genuinely non-destructive: today those restored rows make the count
  nonzero and get a confirm (`app.rs:955-960`); silently `Remove`-ing
  them under a no-confirm close would NARROW today's safety net for
  exactly the resumable-Claude class detach exists to protect. The
  Slice-2 partition tests must include a restored-never-activated
  session (both behaviors would pass a matrix that omits it).
- **P3 — a `DetachedEntry` is model subtree + live pty payload + project
  provenance:** `{ session: Session, ptys: DetachedPtys, project:
  { id, name, path } }` — no order field: Vec position IS the order,
  most recent first, and P4's persisted order is array order (review
  N-r2-2). `WindowPty`/`PaneState` are private to
  `pty_manager.rs` (`:210`, `:238`) — the live half is therefore an
  **opaque `DetachedPtys` payload owned by `pty_manager`** (built by
  `take_session`, consumed by `insert_session`); the pool stores it
  without seeing inside (review N1 — this also draws the Slice 1/2
  boundary). `is_terminals` is not stored: it re-derives from
  `project.id == TERMINALS_PROJECT_ID` (the fixed string `"terminals"`,
  `workspace_model.rs:97`).
  While detached the pool is **passive**: the source window's
  `pane_subscriptions` die with it, nobody re-subscribes, no status
  updates render (rows show a static detached glyph). This is
  mechanically sound because `TerminalSessionHandle` is deliberately
  view-independent (`nice-term-view/src/session_handle.rs:1-13`): the
  feeder thread keeps reading the pty and the `_drain` task is held by
  the entity on the app executor — a pooled child never blocks on a full
  buffer, and subscriber-less `cx.emit`s drop harmlessly. Missed
  transitions (cwd change, process exit) are discovered lazily — at
  adopt, the subscription ensure pass re-wires and liveness is re-read
  from the handles via `TermSession::try_status()`
  (`nice-term-core/src/session.rs:360-363` — the one poll-style liveness
  primitive; use it, don't invent a second, review N2). A process that
  died while detached adopts as a held-style dead pane (scrollback
  intact). No pool-side event plumbing.
- **P4 — persistence schema (NEW FROZEN surface — designed here, freezes
  at ship):** a top-level optional `detached` array in `sessions.json`,
  serde-default (absent ⇒ empty; old files load unchanged — v3 read
  tolerance, `session_store.rs:14-18`):

  ```json
  "detached": [
    {
      "project": { "id": "…", "name": "…", "path": "…" },
      "session": { …PersistedSession, exactly as inside windows[]… }
    }
  ]
  ```

  The `project` object is a **new frozen 3-field struct**
  (`PersistedDetachedProject { id, name, path }`) — NOT the frozen
  `PersistedProject`, whose required `tabs` array makes the 3-field shape
  a decode failure that would degrade the WHOLE document to `empty()`
  and wipe `sessions.json` on the next write (review I3;
  `persisted.rs:154-161`, `session_store.rs:217-225`). `session` is
  `PersistedSession` verbatim. Frozen keys introduced: `detached`,
  `project`, and the project struct's `id`/`name`/`path` — the Slice-1
  frozen-string test pins all of them. Order = most recently detached
  first (the adopt-latest chord pops the head). No timestamp, no
  source-window id — no consumer needs them (YAGNI; additive later).
  Two honesty notes (review I2): (a) `upsert`, `remove`,
  `prune_empty_windows_keeping`, and `empty()` each rebuild the cache as
  a fresh `PersistedState` literal (`session_store.rs:337-340`,
  `:358-361`, `:387-390`, `:154-159`) — the new field makes them compile
  errors, and the implementer must CARRY the value forward, not write
  `detached: Vec::new()` to satisfy the compiler (which would wipe the
  bucket on every window save; the Slice-1 round-trip test pins against
  it). (b) The store's serializer is a typed struct with no unknown-key
  preservation (`session_store.rs:190-193`) — an OLD build reading a new
  file decodes fine, but its first debounced write permanently deletes
  `detached[]`. A release downgrade loses persisted detached rows;
  accepted and stated, not implied inert.
  **Launch-time reconcile (review B3c):** `hydrate` drops any
  `detached[]` entry whose `session.id` also appears under `windows[]`
  (prefer `windows[]` — the attached copy is the one with a window to
  live in), and `adopt_entry` refuses when the target model already
  contains the session id. Without this, a hard kill inside a
  close/adopt debounce window can mint DUPLICATE live sessions with the
  same id, breaking every id-keyed cross-window lookup
  (`window_registry.rs:172-221` returns whichever window matches
  first). Same belt-and-braces class as `dedupe_window_ids`
  (`workspace_model.rs:1262`), which exists because a persisted-id
  invariant "that couldn't happen" did. Slice-1 tests pin the reconcile.
- **P5 — close-path wiring.** `request_window_close` (`app.rs:947-980`):
  with the setting ON and ≥1 detach-eligible session, set
  `user_initiated_close` and return true — no confirm. The actual move
  happens in `route_close_disk_fate` (`window_registry.rs:295-313`)
  between deregister and teardown — verified safe: that stretch is
  synchronous and nothing in it touches the sessions besides the
  snapshot read this reorders. **Disk-fate ordering is pinned (review
  B3a): extract eligible sessions into the pool (P6) → write the pool
  bucket into the store CACHE (`set_detached`) and the window slot's
  `Remove` as one batch → the ONE existing `flush()` already in
  `route_close_disk_fate` (`window_registry.rs:302-310`).** The pool
  write must never land after that flush — a crash between two flushes
  would leave the detached sessions in NEITHER `windows[]` nor
  `detached[]` (permanent loss). Then tear down the remainder. Quit is
  untouched: `AppQuitting` still `Preserve`s whole windows — sessions
  attached at quit stay in their windows and restore there; the pool
  persists only what was already detached.
- **P6 — extraction/adoption primitives.**
  `WindowState::detach_session(session_id) -> DetachedEntry`: model side
  via `project_session_index` + `remove_session` +
  `clear_dangling_parent_references` (`workspace_model.rs:218/1193/1205`),
  pty side via a new `PtyManager::take_session(session_id) ->
  DetachedPtys` that removes and RETURNS the session's live entries
  without dropping. **The scrub list is explicit (review I4/I7)** —
  these matter for the EXPLICIT detach action where the window stays
  open (the close path gets most of them free via teardown):
  - PtyManager: `window_launch_states` is keyed by BARE `term_window_id`
    (`pty_manager.rs:326-330`) — scrub by the session's window-id set;
    `pane_status` entries MOVE with the payload (the
    `move_pane_to_new_window` precedent, `:1311-1319`) and are re-seeded
    on `insert_session` so an adopted Claude pill isn't stuck dim/lit
    until its next status event; `dissolved_session_ids` (`:392-401`)
    and `pending_project_removal` (`:386-391`) are deliberately LEFT
    (no dissolve happened; project-removal flags are project-scoped);
    **`pending_prefill` is CLEARED on every taken pane** — the slot's
    consume event (`CwdChanged`) can fire subscriber-less while pooled
    and be dropped, after which the armed line would splice into the
    user's first post-adopt `cd` (`pty_manager.rs:247-261`); a pane live
    enough to detach no longer needs its prefill, and a structural
    re-adopt arms a fresh one at respawn. Unit test: armed prefill +
    take+insert ⇒ slot empty.
  - WindowState: prune `selection` (the dissolve-cascade rule,
    `pty_manager.rs:39-45`), prune the session's `file_browser` states
    (`window_state.rs:509` — a leaked entry would resurrect stale
    browser state on re-adopt into the same window), close the
    `search_bar` if its pane belongs to the detached session
    (`window_state.rs:550`), and run the subscription ensure pass
    SYNCHRONOUSLY in the same update — on detach this is hygiene (a
    stale subscription early-returns at the model lookup,
    `window_state.rs:1059-1066`, before any arm that could act); the
    HARD requirement is on the adopt/insert side, per the pass's own
    arming-site rule (`window_state.rs:1005-1013`) (review N-r2-1) —
    then active-session fallback + save.
  Used by both the close-path bulk detach and the explicit Detach
  Session action. Adoption is the inverse:
  `WindowState::adopt_entry(entry)` — **refuses if the target model
  already contains the session id (review B3c)**; `ensure_project_by_path`
  (or the seeded Terminals project when `project.id == "terminals"`),
  insert the `Session` — clearing its own `parent_session_id` when the
  target model lacks the parent (review N7; otherwise the sidebar
  renders an indented row under a nonexistent parent,
  `sidebar_shell.rs:779`; `prune_dangling_parent_references` precedent,
  `workspace_model.rs:1228`) — `PtyManager::insert_session(...)` for the
  live payload, run the existing idempotent subscription ensure pass
  (`window_state.rs:1023-1143`, keyed on `live_pane_keys()` so inserted
  sessions are picked up), select + focus. **The disk transition is one
  update (review B3b): pool-take + `set_detached` + the adopting
  window's `upsert` land in the same store-cache batch before any flush
  point** — split batches + a hard kill in the 500 ms debounce gap
  (`session_store.rs:81`) would leave the session in BOTH buckets.
  Structural entries (post-restart, or P2's ptyless rows: payload empty)
  ride the existing lazy respawn on activation — the same
  `ResumeDeferred`/fresh-shell machinery restored windows use; no new
  spawn path.
  **Pool persistence is write-through (review I5):** every `DetachedPool`
  mutation (detach-push, adopt-take, kill) writes `set_detached` into
  the store cache immediately (debounced, the `save_to_store` pattern) —
  so EVERY existing flush point (`route_close_disk_fate`'s,
  `quit_cascade`'s, `on_app_quit`'s at `app.rs:532-536`, Drop's) covers
  the pool for free. P5/P10's "persist the pool" steps are ordering
  guarantees, not extra writes.
- **P7 — explicit Detach Session action** (context menu on any live
  sidebar row + chord). Detaching the last session of a window closes the
  window through the normal close path (it is now empty;
  `mark_removed_if_window_emptied` precedent, `window_state.rs:2416-2420`).
- **P8 — keybinds and actions (36 → 39; ids frozen at ship; no new
  RESERVED_COMBOS entries):**
  - `DetachSession` — **⌃⌘⇧D** ("D = Detach" on the ⌃⌘⇧ rung, whose only
    tenants are the four pane-focus letters). Bare ⌃⌘D is
    macOS-dictionary-eaten (`shortcuts.rs:653-657`); the shift variant is
    expected to pass but this is exactly the class gpui injection cannot
    see — hand feel-check gate (Validation). If it's eaten too, fallback
    ⌃⌘U (free, noted at `:656-657`).
  - `AdoptDetachedSession` — **⌃⌘A** ("A = Attach", free on the bare
    rung). Adopts the pool head into the active window; no-op with an
    empty pool.
  - `TearOffPane` — **⌃⌘N** ("pane → New window", pairs with ⌘N; free —
    reservations are exact-combo).
- **P9 — tear-off = extract + adopt into a fresh window, with TWO
  extraction branches (review I6).** `PaneLayout::remove` refuses the
  last leaf by contract ("a pill without a pane is not representable",
  `pane_layout.rs:408-414`) — so:
  - **Multi-leaf pill:** break-pane's extraction (remove + spatial
    refocus + `is_alive` recompute, `pty_manager.rs:1276-1296`), source
    pill survives.
  - **Single-leaf pill:** move the WHOLE `TermWindow` out via
    `extract_window` (`workspace_model.rs:520`) feeding the
    synthetic-entry wrap. `extract_window` already repairs
    `active_window_id`/`prev_active_window_id` internally
    (`workspace_model.rs:524-537`) — don't duplicate it; the genuinely
    new work is the empty-session dissolve if that was the session's
    last pill (the window-emptied terminus,
    `mark_removed_if_window_emptied` precedent) (review N-r2-6b).
  The extracted pane/pill is wrapped `→ single-leaf TermWindow → new
  Session` and handed as a synthetic `DetachedEntry` to a new-window
  construction path: `open_managed_window_with` grows an adopt variant
  (the `app.rs:1828-1830` comment's anticipated shape) — seed the new
  `WindowState` with the entry, move the live payload into its
  `PtyManager`, suppress the fresh window's eager Main spawn, and fire
  the same one explicit post-open `save_to_store` the restore path does
  (`app.rs:1760-1762`) — the mutation observer only catches mutations
  after construction, so a seeded-then-crashed window would otherwise
  persist nothing (review N3). Rendering needs no new machinery:
  `WindowHostView` builds `TerminalView`s lazily per pane id from
  `PtyManager::pane_handle` on activation (`app_shell.rs:36-37`,
  `:278-282`), so a pane whose handle predates the window renders with
  scrollback. Same session-level machinery as adoption; one
  implementation, two doors. Scope guard, mirroring break-pane for the
  same reason: Claude leaves are refused (`pty_manager.rs:1269-1271` — a
  Claude session moves whole via Detach → Open in New Window instead,
  which the context menu offers).
- **P10 — window-less mode mechanics.** `should_quit_after_close`
  (`window_registry.rs:278-285`) additionally stays alive when the pool
  has ≥1 entry with live ptys. `cx.on_reopen` (vendor seam, unused today)
  opens a window when none is open. ⌘Q's confirm counts pool-live
  sessions via `total_live_window_counts` (`app.rs:654-658`) gaining a
  pool term. **The window-less confirm needs a HOST engineered
  (review B2):** `request_quit` (`app.rs:737-749`) presents on a
  registered window via `resolve_modal_host` (`:716-730`), which returns
  `None` with an empty registry — and also in the Settings-only state
  (Settings is unregistered, `:717-721`) — and today's `None` branch
  falls straight through to `quit_cascade`. Unchanged, window-less ⌘Q
  would silently SIGHUP the entire live pool — the exact outcome D5 says
  must be confirmed. Fix: when the count is nonzero, the host is `None`,
  and the pool has live entries, OPEN a window first and present the
  confirm on it (it renders the Detached section — the right context for
  the question). **Cancel leaves that window open** — acceptable (it
  shows the pool), stated here so it is a designed outcome, not a
  surprise (review F1).
  **The window-less recovery windows are EMPTY, not fresh (review F1,
  blocking):** a fresh (`seed = None`) window eagerly spawns a live Main
  shell (`app.rs:1699-1743`), which is P2-eligible — so a fresh-window
  reopen under D1's no-confirm close would add one junk "Main" pool row
  per reopen→close cycle: the pool would self-pollute in exactly the
  mode this phase exists to enable. The `on_reopen` window and the
  ⌘Q-confirm host therefore open through the SEEDED path as an empty
  Terminals-only window — a shape restore already represents and the
  prune deliberately keeps ("a legitimately empty Terminals-only
  restored window survives the prune", `session_store.rs:371-375`;
  `with_seed` handles `active_session_id: None`). D5's letter is
  untouched: the window still shows the Detached section. ⌘N stays a
  true fresh window (a user asking for a new window plausibly wants a
  shell) — accepted, stated consequence: closing an untouched fresh
  window pools its Main.
  **Quit/close decisions are pure seams (review F2):**
  `present_confirmation` panics on the headless test platform — the
  `resolve_modal_host` doc records exactly this split (`app.rs:711-716`)
  — so the ⌘Q decision (counts, host-resolved?, pool-live ⇒ `QuitNow` /
  `PresentOn(host)` / `OpenWindowThenPresent`) and the ⌘W close decision
  (setting, eligibility ⇒ `DetachAndClose` / `Confirm` /
  `CloseAndRemove`) are extracted as pure functions and unit-tested as
  such (the `should_quit_after_close` seam precedent,
  `window_registry.rs:437-478`); actual presentation is covered by the
  live scenario + hand gates.
  **Lingering window-less state (review N-r2-3):** the pool is passive
  and `should_quit_after_close` runs only on window close, so a
  window-less app whose last pooled process dies sits dock-only until
  the user acts. Accepted deliberately — dock-reopen recovers, nothing
  is at risk; no poll/event is added for it.
  The quit cascade tears the pool down after the store cache holds it
  (write-through per P6; clean SIGHUP for detached children, matching
  `PtyManager::teardown` semantics).
- **P11 — sockets/env for moved panes: accepted staleness in THREE
  distinct shapes (review I1), stated honestly.** Env is frozen at fork
  (`pty_manager.rs:2141-2156`, `:2944-2951`); nothing re-stamps a live
  child. The three flows fail differently:
  1. **Close-path detach → adopt: DEAD socket.** The source window
     closed; its path was unlinked, its slot `Remove`d, so the id — and
     the path — never recur (`control_socket.rs:1076-1090`).
     Handoff/dispatch/promotion from that pane fails loudly ("no reply
     from control socket") until the pane respawns; manual `NICE_SOCKET`
     override remains the bridge.
  2. **Explicit detach (P7) from a window that stays open → adopt: LIVE
     socket, wrong window, no session.** The source socket is healthy;
     traffic handshakes with the source window and resolves
     `NICE_TAB_ID` against a session that window no longer owns. Slice-2
     task: verify what `route_socket_message` does with an unknown tab
     id, and make the reply an explicit error so the shell shadow falls
     back to `command claude` (the "user always gets claude" contract) —
     never a partial behavior.
  3. **Tear-off: LIVE socket, wrong window, and the session RESOLVES.**
     Only the pane moved; the source session still exists in the source
     window, so promotions light the SOURCE window's pill and spawned
     Claude windows land there — a different OS window than the one the
     user is typing in, and nothing fails loudly. This is the break-pane
     P2 wart (`pty_manager.rs:1253-1258`) escalated from "wrong pill,
     same window" to "wrong window entirely", and it is the shape a user
     hits first (tear off a shell pane, type `claude`, watch the other
     window light up).
  All three accepted for the same reason: env is frozen at fork and
  Phase 5's pane addressing is the designated revisit. What this phase
  DOES fix is the socket plan's carried gap: structural detached entries
  (the restart case, and P2's ptyless rows) respawn on adopt with the
  ADOPTING window's env — fresh live socket, no dead path. Update
  `stable-control-socket.md` § Known gaps accordingly. Considered and
  deferred: having the adopting window additionally bind the source
  window's stable path and route it — real fix, real complexity; Phase 5
  candidate, noted so reviewers don't re-derive it.
- **P12 — sidebar presentation.** One synthetic "Detached" group appended
  after the project groups, rendered from the pool (every window shows
  the same section), visible only when non-empty. Rows reuse `SessionVm`
  shape (title, `has_claude` glyph, static detached status; no live
  status). Click = adopt into this window (D3). New a11y/test ids
  (frozen at ship): `sidebar.detached.section`, `sidebar.detached.row`.
  Sidebar re-render rides `cx.observe` on the pool entity (P1).

## Slice 1 — model + persistence: the pool exists

`crates/nice/src/detached_pool.rs` (new), `session_store.rs`,
`settings/prefs_store.rs`.

- `DetachedPool` entity + global handle (P1): `entries:
  Vec<DetachedEntry>` (P3, live half = `pty_manager`-owned opaque
  `DetachedPtys`), ops `push_front`, `take(session_id)`, `take_head`,
  `kill(session_id)` (drops the payload → SIGHUP), `has_live` (via
  `try_status`, N2), `snapshot()`, `hydrate(...)` (with the B3c
  duplicate-id reconcile: prefer `windows[]`, drop the detached copy).
  `cx.notify` on every mutation, and every mutation writes through to
  the store cache (`set_detached`, debounced — P6/I5).
  **Boot point (review F3):** the pool global is created + hydrated in
  `run` immediately after `install_session_store` (`app.rs:555-559`),
  BEFORE `run_restore_fan_out` (`app.rs:614-650`); the reconcile runs
  against the store cache at that point. This is ordering-independent
  w.r.t. the restore passes — the ghost pre-pass (`app.rs:617-623`) and
  `prune_empty_windows_keeping` (`app.rs:648`) only drop session-LESS
  slots, which can never collide with a `detached[]` session id — but
  the argument lives here, not re-derived by the implementer. All pool
  consumers (sidebar wiring, `should_quit_after_close`'s pool check,
  keymap handlers) read via `try_global` and no-op/render-nothing when
  absent — `run_selftest`/scenario-built states have no pool (the
  hermeticity rule; optional-observer precedents
  `sidebar_shell.rs:667-675`, `keymap.rs:1038-1041`).
- `session_store.rs`: `PersistedDetachedSession { project:
  PersistedDetachedProject, session }` (P4/I3) + optional serde-default
  `detached` field on `PersistedState`; `set_detached` / read-at-launch
  API. Carry the field through the four struct-literal rebuild sites
  (`upsert`/`remove`/`prune_empty_windows_keeping`/`empty()`,
  `session_store.rs:337-390`, `:154-159`) — carry the VALUE, never
  `Vec::new()` (I2).
- `prefs_store.rs`: `close_window_detaches: Option<bool>` in
  `AdvancedSection` (`prefs_store.rs:48-54`; accessor `unwrap_or(true)`,
  checkbox writes `Some(...)` explicitly), Settings ▸ Advanced row.
- Tests: pool ops; persistence round-trip (snapshot → JSON → hydrate),
  frozen-string schema test pinning ALL new keys (`detached`, `project`,
  `id`, `name`, `path`); old-file tolerance (no `detached` key ⇒ empty
  pool); **bucket survives window writes** (upsert/remove/prune cycles
  leave `detached[]` intact — the literal-rebuild pin, I2); the hydrate
  reconcile (session id in both buckets ⇒ windows[] wins). The round-1
  draft's "unknown keys preserved by co-writers" test is DROPPED —
  `sessions.json` has no unknown-key-preserving writer (that story
  belongs to `ui_settings.json`'s merge writer); the downgrade data-loss
  consequence is accepted in P4 instead.

## Slice 2 — detach: close-path + explicit action + window-less app

`window_registry.rs`, `window_state.rs`, `pty_manager.rs`, `app.rs`,
`keymap.rs`, `shortcuts.rs`.

- `PtyManager::take_session` / `insert_session` (P6) — move without
  drop; the FULL scrub list from P6 (launch states by window-id set,
  pane-status move/re-seed, prefill clear, the two deliberately-left
  maps); unit tests pinning "no SIGHUP on take" (marker child stays
  alive across take+insert) and "prefill cleared on take" (I7).
- `WindowState::detach_session` (P6, incl. the WindowState-side scrub:
  selection, file-browser states, search bar, synchronous subscription
  ensure pass) + the close-path partition in `route_close_disk_fate`
  (P5, pinned flush ordering) + the no-confirm branch in
  `request_window_close` (P5, gated on the P1 setting).
- `DetachSession` action: `shortcuts.rs` variant + id + label + ⌃⌘⇧D
  default (P8), `keymap.rs` handler, sidebar-row context-menu item.
  Last-session detach closes the window (P7).
- Window-less mode (P10): `should_quit_after_close` pool check,
  `on_reopen` hook (empty Terminals-only seeded window — F1), ⌘Q count +
  the B2 window-less confirm host (open-empty-window-then-present),
  quit-cascade pool teardown; the F2 pure decision seams (⌘Q:
  `QuitNow`/`PresentOn`/`OpenWindowThenPresent`; ⌘W:
  `DetachAndClose`/`Confirm`/`CloseAndRemove`).
- Socket-side check (P11 shape 2): pin `route_socket_message`'s
  unknown-tab-id reply as an explicit error (shell shadow falls back to
  `command claude`).
- Tests: partition matrix — MUST include a restored-never-activated
  session (B1: detaches as a structural entry, never `Remove`d
  unconfirmed) alongside live/held/dead rows; setting OFF ⇒ today's
  behavior byte-for-byte; teardown-order seam (pool extraction and the
  cache writes happen before the one flush, before `teardown`); quit
  path leaves attached sessions in `windows[]` and detached ones in
  `detached`; the ⌘Q/⌘W decision-seam matrices — pinned as PURE unit
  tests, never driven through `present_confirmation` (it panics on the
  headless test platform, `app.rs:711-716` — review F2) — including:
  window-less ⌘Q with a live pool ⇒ `OpenWindowThenPresent`, never
  `QuitNow` (Settings-only variant included); and reopen-then-close in
  window-less mode does NOT grow the pool (the F1 empty-window shape).
- Verification notes (non-test): one live check that a pooled pane
  producing output no-ops silently on its stale present-kick at the dead
  window (`app.rs:1638-1640`; dead-handle `update` returns `Err` by
  pattern — N4). The `persistence-restore` scenario drives
  `route_close_disk_fate` directly via a scoped observer
  (`window_registry.rs:290-294`) and now exercises the partition —
  ensure its fixture windows classify as no-eligible or assert the new
  behavior deliberately (N5).

## Slice 3 — sidebar section + adoption

`sidebar_shell.rs`, `window_state.rs`, `keymap.rs`, `shortcuts.rs`.

- Detached `GroupVm` from the pool (P12), observe-wiring, row render
  (static glyph, dimmed), a11y/test ids.
- Click-adopts + context menu (Adopt into This Window / Open in New
  Window / Kill — kill via pool op + store write) (D3). Click-adopt
  handles `take(session_id)` returning `None` as a silent no-op — the
  same row clicked in two windows across a notify gap must not
  double-adopt (single-threaded main loop makes it easy; the handler
  still handles the `None`) (review N-r2-6c).
- `WindowState::adopt_entry` (P6): model insert, pty insert,
  subscription ensure pass, select+focus, save. Structural entries ride
  lazy respawn; dead-while-detached panes adopt held-style (P3).
- `AdoptDetachedSession` ⌃⌘A (P8): pool head → active window.
- Open in New Window = adopt via the Slice 4 window-construction path
  when it exists; until then, an F1-style empty Terminals-only seeded
  window + `adopt_entry` after open (a plain fresh window would mint an
  unasked-for eager Main next to the adopted session — review N-r2-5).
- Tests: adopt round-trip in-process (detach → adopt → same
  `Entity<TerminalSessionHandle>` pointer, no respawn; scrollback marker
  survives), structural-adopt spawns lazily on activation with resume
  prefill, project re-homing (`ensure_project_by_path` reuse vs create;
  Terminals → Terminals).

## Slice 4 — tear-off

`pty_manager.rs`, `window_state.rs`, `app.rs`, `keymap.rs`,
`shortcuts.rs`.

- Pane extraction → synthetic `DetachedEntry`, BOTH P9 branches:
  multi-leaf (break-pane extraction, source pill survives) and
  single-leaf (`extract_window` whole-pill move + source-session
  `active_window_id` repair + empty-session dissolve).
- `open_managed_window_with` adopt variant (P9): seeded `WindowState`
  carrying the entry, no eager Main spawn, live payload moved before
  open, restore-style one explicit post-open `save_to_store` (N3);
  `build_window_root` wiring unchanged (`app.rs:1828-1830`).
- `TearOffPane` ⌃⌘N + menu item; Claude-leaf refusal with the break-pane
  key-hint pattern for "why did nothing happen" feedback (whatever
  break-pane does today on refusal — match it).
- Tests: tear-off moves the live handle (marker survives, new OS window
  in registry, source tree healed), BOTH branches (multi-leaf: source
  pill healed + refocused; single-leaf: pill gone, source session's
  active window repaired, empty session dissolved), Claude leaf refused,
  frame/persistence of the new window (it persists as a normal
  `PersistedWindow` on close/quit, incl. the pre-first-mutation crash
  window the N3 save closes).

## Slice 5 — selftest scenario + docs

- New live selftest scenario `detach-adopt` (the `niceties-held` /
  `shell-socket` pattern): real pty, echo a marker → close the window
  (detach) → verify pool row renders → adopt into a second window →
  marker present without respawn → explicit detach → adopt-latest chord.
  A `tear-off` leg: split, tear off, assert the second OS window hosts
  the marker pane.
- Docs: roadmap Phase 4 section marked shipped (dated, socket bullet
  already annotated); `stable-control-socket.md` Known-gaps update
  (P11); `crates/README.md` ownership-story paragraph (pool + the CUT
  note at `:635-637` now partially superseded — action tear-off exists,
  drag still cut).

## Ordering

1 → 2 → 3 → 4 → 5. 2 needs 1's pool; 3 needs 2's primitives; 4 reuses
3's adoption machinery; 5 gates the merge. Slices 2 and 3 are the
riskiest (ownership surgery) — keep them separate review rounds.

## Validation

- **Unit/targeted:** per-slice tests above; full `cargo test --workspace`
  before merge (23 suites green at base). Fix rounds run only touched
  modules' tests.
- **In-process scenarios:** the Slice 2/3/4 seam tests run under
  `run_until_parked` with the executor timer (never `smol::Timer` —
  CLAUDE.md gotcha).
- **Live scenario:** `detach-adopt` (+ tear-off leg) selftest against the
  installed `Nice Dev` under the worktree lock, held through the whole
  install+test window; `caffeinate -d` for GUI legs.
- **Black-box restart survival (scriptable, scratch-env per CLAUDE.md —
  seed keychain symlink + `.claude.json` BEFORE launch):** launch, open
  a shell session, mark it, close the window (detach), assert the
  `detached` bucket in the scratch `sessions.json`; graceful quit **via
  the terminate path (`osascript … quit`), not ⌘Q** — with a live pool,
  ⌘Q now presents the P10 confirm and would hang the script; the
  terminate path bypasses confirmation by design (`on_app_quit`,
  `app.rs:509-536`), and the ⌘Q confirm itself is covered by the F2 seam
  test + hand gate (review N-r2-4); wait
  for pid exit; relaunch; assert the Detached section renders the row;
  adopt; assert a fresh pty spawns in the recorded cwd (and for a Claude
  row, that resume prefill fires). Window-less leg: close the LAST
  window with a live session, assert the process stays alive with zero
  windows, reopen via dock-reopen (scriptable via `open -a`), assert a
  fresh window shows the pool.
- **Hand-only gates (feel-check — gpui injection cannot see these):**
  1. ⌃⌘⇧D actually reaches the app (macOS dictionary-hotkey class —
     `shortcuts.rs:653-657`); fallback ⌃⌘U pre-approved if eaten (P8).
  2. Dock-icon reopen in real window-less state feels right (icon stays,
     menu bar present, reopen is instant).
  3. An adopted live Claude session: conversation continues in place,
     scrollback intact; and the P11 warts are OBSERVED, not fixed — all
     three shapes: close-detach → handoff fails with the documented dead
     symptom ("no reply from control socket", manual `NICE_SOCKET`
     override works); explicit-detach → the source window's socket
     answers with the explicit unknown-tab error and the shell falls
     back to `command claude`; tear-off → a hand-typed `claude` in the
     torn-off pane lights the SOURCE window (the shape a user hits
     first — observe it so the feel-check knows it's the documented
     wart, not a regression).
  4. Close-window-detach feels non-destructive enough to skip the
     confirm (D1) — if a silent ⌘W surprises in the hand, revisit with a
     one-time toast, not a modal.
- **Ad-hoc signing note:** any real-CGEvent harness leg needs the
  Accessibility grant re-added after reinstall (remove+re-add;
  `AXIsProcessTrusted` check first).
