# Session-move rework: direct "Move to Window", pool removed, close kills again

**Status: APPROVED by Nick (2026-08-22, one Fable round folded) — ready to implement.**

Supersedes the user-facing surface of Phase 4
(`docs/plans/phase-4-detach-adopt-tearoff.md`, implemented at `b69a5ff` on
branch `phase-4-detach-adopt-tearoff`, never landed on main). Nick's
feel-check verdict: the detach → sidebar-limbo → click-to-adopt round trip
feels wrong ("if you detach a terminal, it is still in the sidebar, but just
in a different spot; click it and it moves back — that's just weird"). The
useful core is moving sessions between windows and tear-off. This plan keeps
the Phase-4 transfer mechanics as plumbing, deletes the user-visible
detached-pool concept, and reverts close/quit to their pre-Phase-4 behavior.

Base: branch `phase-4-detach-adopt-tearoff` @ `b69a5ff` (= main `6aee7ac`
+ Phase 4). The rework lands as commits on top, so the kept mechanics retain
their review history. Nothing here touches main until Nick's explicit go.

Vocabulary (unchanged): sidebar row = `Session` (≈ tmux session); upper-bar
pill = `TermWindow`; `Pane` = leaf of a pill's split tree.
`window_session_id` = the OS-window's persisted id.

## Nick's decisions (protected — do not reopen)

- **N1 — No user-visible detach.** The explicit detach verb dies: ⌃⌘⇧D,
  the `DetachSession` action, the sidebar "Detach Session" context-menu
  item, the Detached sidebar section, and click/⌃⌘A adopt all go. So does
  `AdoptDetachedSession`.
- **N2 — Direct "Move to Window".** Session rows get context-menu items
  that move the session straight into another open window or a new one.
  One gesture, no intermediate visible state.
- **N3 — Close kills again.** ⌘W / red-button returns to the pre-Phase-4
  behavior: zero live sessions → close; otherwise confirm popup, then kill.
  The `close_window_detaches` setting and its Advanced-pane toggle are
  deleted (not defaulted off — deleted).
- **N4 — No window-less mode.** Closing the last window quits the app.
  ⌘Q returns to the pre-Phase-4 flow. Dock-reopen handling (`on_reopen`)
  and the recovery window are deleted.
- **N5 — Moving a window's last session out closes that window.** Nothing
  died — the session moved — so this close bypasses the confirm
  (`window.remove_window()` path, same as tear-off's emptied-source close).
- **Kept:** tear-off ⌃⌘N (`TearOffPane`, action position 39) exactly as
  shipped in Phase 4, including the Claude-pane refusal and open-failure
  recovery.

## Current-code facts (grounded at `b69a5ff`, 2026-08-22)

Transfer plumbing (KEEP — pool-independent, proven by tear-off which never
touches the pool):

- `PtyManager::take_session` (`pty_manager.rs:1434`) → opaque
  `DetachedPtys` (`pty_manager.rs:297-346`); live `Entity<TerminalSessionHandle>`
  moves by value — no respawn, no SIGHUP. Clears `pending_prefill` on taken
  panes (`:1448`) because the prefill-consume event could fire
  subscriber-less mid-move. `insert_session` (`:1467`) is the inverse;
  creates the container even for a ptyless (structural) payload so
  lazy-respawn works. `mint_session_id` (`:544`) re-keys ids that leave a
  window (ids are only unique per-window; every fresh window seeds the
  constant `terminals-main`).
- `WindowState::detach_session` (`window_state.rs:2677`) — model removal +
  `take_session` + re-key + scrubs (selection/file-browser/search-bar) +
  synchronous `subscribe_spawned_windows` + `save_to_store()`. Returns
  `(DetachedEntry, DissolveTerminus)`; `DissolveTerminus::WindowEmptied`
  when the window went empty.
- `WindowState::adopt_entry` (`window_state.rs:2873`) — the one adoption
  primitive behind every door: duplicate-id guard that hands the entry
  BACK on refusal (never drop a live child), `ensure_project_by_path`,
  `/branch` parent-pointer clear, `insert_session`, synchronous
  `subscribe_spawned_windows` (hard requirement — first OSC 7 can arrive
  next foreground turn), select+focus, one `save_to_store()`.
- `app::open_managed_window_adopting` (`app.rs:1916`) — new window seeded
  empty (no eager Main fork), payload moves before the window exists,
  in/out `&mut Option<DetachedEntry>` slot hands the entry back on
  construction failure.
- Tear-off chain (`keymap.rs:918` → `window_state.rs:2779` →
  `pty_manager.rs:1524`): multi-leaf break-pane extraction vs single-leaf
  `extract_window` whole-pill move; Claude-pane refusal at
  `pty_manager.rs:1536-1538` (silent, mirrors break-pane); open-failure
  recovery `return_torn_off_entry` (`keymap.rs:969`) re-adopts into the
  source and unlatches `user_initiated_close` (f4e7fe2).
- Deferred source close: `keymap.rs:872-876` / `:946-950` —
  `cx.defer(… window.remove_window())`, deferred OUT of the entity lease
  (driving window removal inside a leased `WindowState` update re-enters
  the entity and aborts; documented `keymap.rs:853-855`).
  `window.remove_window()` bypasses the `on_window_should_close` confirm
  gate by design (verified self-test `multiwindow.rs:855-869`) and still
  runs `route_close_disk_fate` + `should_quit_after_close`.
- Cross-window enumeration: `WindowRegistry::all_states`
  (`window_registry.rs:234`), `state_for_window` (`:166`).

Pool surface (DELETE):

- `crates/nice/src/detached_pool.rs` (635 lines) — `DetachedPool` entity,
  global install, `adopt_into`/`adopt_head_into`/`adopt_into_new_window`/
  `kill_pooled`/`pool_rows`/`pool_has_live`/`pool_live_window_counts`,
  hydrate reconcile, `detach_on_close_enabled`. EXCEPT the two structs
  `DetachedEntry` + `DetachedProject` (`detached_pool.rs:74-170`), which
  the kept plumbing passes around — they move out (see P3).
- Sidebar Detached section: `sidebar_shell.rs` constants `:177-195`
  (incl. frozen a11y ids `sidebar.detached.*` — un-frozen by this plan,
  they never shipped to prod), `snapshot_detached_group` (`:826`),
  `build_detached_group` (`:2247`), `build_detached_row` (`:2330`),
  `adopt_detached_row` (`:2421`), `open_detached_context_menu` (`:2433`),
  `pool_sub` (`:719`), tests `:3053+`.
- Attached-row "Detach Session" menu item (`sidebar_shell.rs:1380-1394`).
- Actions: `DetachSession` + `AdoptDetachedSession`
  (`nice-model/src/shortcuts.rs:194,198`, `ALL` entries `:268-269`, labels
  `:319-320`, json ids `:403-404`), keymap decls/wiring/bindings
  (`keymap.rs:125-126,492-493,1343-1344,2102-2113`), handler
  `adopt_detached_session` (`keymap.rs:887`). `detach_session_from_window`
  (`keymap.rs:858`) is repurposed, not deleted — see Design. No
  tombstone convention exists for retired actions (positions are
  array-order in `ALL`, persistence keys are the `id()` strings) — plain
  removal is correct; the ids `detachSession`/`adoptDetachedSession` must
  never be reused for a different meaning.
- Parity lists: `settings_import.rs` `RUST_ONLY` shrinks 26 → **24**
  (`TearOffPane` stays); `nice-itests/src/multiwindow.rs:112-114,432-437,
  501-508` drops the two retired actions, keeps `TearOffPane`.

Close/quit surface (REVERT to main `6aee7ac` shapes):

- `app::request_window_close` (branch `app.rs:1142-1184`): delete
  `WindowCloseDecision`/`window_close_decision` (`:1092-1127`), restore
  main's shape (main `app.rs:947-975`): zero live → latch
  `user_initiated_close` + close; else `present_confirmation`.
- `window_registry.rs`: remove the `detach_eligible_sessions_into_pool`
  call (`:311`) and fn (`:349-363`); `should_quit_after_close` drops the
  `pool_live` read (`:287`); `should_quit_on_window_close` back to two
  args (main `:321-323`).
- `app::quit_cascade` (`:1039-1054`): delete the pool `clear_live` block
  (`:1045-1052`).
- `app::request_quit` (`:803-860`): delete `quit_decision`/`QuitDecision`'s
  `OpenWindowThenPresent` arm and the `pool_live` read; restore main's
  linear shape (main `:729-772`). Delete
  `open_window_less_recovery_window` (`:876`), `handle_dock_reopen`
  (`:912`), and the `cx.on_reopen` wiring (`:1300`).
  **KEEP `empty_terminals_window_seed` (`:885`)** — it seeds every
  adopting window empty and `open_managed_window_adopting` (tear-off's
  new window, and this plan's Move-to-New-Window leg) depends on it;
  re-doc it as the adopting-window seed (its F1 pool rationale goes).
  (Review B1.)
- `app.rs:675` — the `pool_live_window_counts` read inside
  `total_live_window_counts` (feeds the ⌘Q confirm counts) — delete with
  the revert; not covered by the `request_quit` hunk alone. (Review I4.)
- `app.rs:1459` — the `crate::detached_pool::install(cx)` call in
  `app::run`. (Review I4.)
- `window_registry.rs:578-828` — three test bodies installing
  `DetachedPoolGlobal`/calling `detached_pool::install` (pool-aware
  quit-check tests). `app.rs:5473+` `phase4_decision_tests` (imports
  `detach_eligible_session_ids`, tests `window_close_decision` /
  `quit_decision` / the recovery-window seed). All deleted. (Review I4.)
- Settings: `prefs_store.rs:62,142-143,229-234` + tests `:547-590`;
  `advanced_pane.rs:43-62,217-231` (toggle row
  `settings.advanced.closeWindowDetaches`). NOTE: deleted in Slice 2, not
  Slice 1 — `detach_on_close_enabled` (`detached_pool.rs:265-270`) reads
  the pref and survives until the pool dies. (Review I2.)

Persistence (DELETE):

- `session_store.rs`: `PersistedDetachedProject` (`:158`),
  `PersistedDetachedSession` (`:175`), `PersistedState.detached` (`:198`),
  `set_detached`/`detached` (`:468-481`, free fns `:671-673`), the four
  struct-literal carry sites (`:209-214,395-399,419-423,451-455`), tests
  `session_store/tests.rs:1003-1148`. The store has NO
  `deny_unknown_fields` (module doc `session_store.rs:16`), so a leftover
  `"detached": […]` key in an existing dev `sessions.json` parses fine and
  is ignored. Prod never shipped Phase 4 — no migration.
  **Data-loss note (accepted):** any live `detached[]` rows in Nick's dev
  sessions.json at upgrade are silently dropped. Dev-only, Nick knows.
- `persistence_restore_live.rs:316-330` pool-absence guard + the
  `detached: Vec::new()` literal (~`:789`) — revert to main.

Also delete: `detach_adopt_live.rs` (the `detach-adopt` live selftest,
registered `app.rs:4534`, module `main.rs:147`) — replaced, see Slice 4.
KEEP: the `error unknown-session` socket reply (crates/README.md R15) —
still reachable (tear-off makes wrong-window socket queries possible) and
already documented.

## Design

### The verb

Single-session context-menu items on attached sidebar rows (single-row
selection only, like Rename; multi-row Move is out of scope — YAGNI):

- `Move to "<label>"` — one flat item per OTHER open window.
- `Move to New Window` — always present.

Flat items, not a submenu: the context-menu infra has no nested-menu
variant (`context_menu.rs:71-180`, `ContextMenuItem` = Entry | Separator)
and building flyout infra for this is not warranted. With one window open,
only "Move to New Window" appears. No keybinding for Move (menu-only;
tear-off keeps ⌃⌘N as the keyboard path to "pane → new window").

**P1 — window labels.** No per-OS-window user-visible name exists (OS
titles are all the app name, `app.rs:148`). Synthesize: the target
window's ACTIVE session title (`workspace.active_session_id()` →
`Session.title`), fallback `"Untitled"` when `active_session_id()` is
`None`, truncated middle-ellipsis at ~30 chars. Duplicate labels across
windows are acceptable — menu items key on `WindowId`. Ordering: the
registry's `entries` is a HashMap and `all_states`
(`window_registry.rs:234`) iterates it nondeterministically, and no
accessor exposes `WindowId` pairs — add a small registry accessor
iterating the private MRU `order` Vec (`window_registry.rs:102`)
returning `(WindowId, Entity<WindowState>)`; menu order = MRU at
menu-open time, deterministic. (Review I1, N-e.)

### The two legs

**Existing window (A → B):** on A, `detach_session(session_id)` →
`(entry, terminus)`; on B (resolved via `state_for_window` by the menu
item's `WindowId`), `adopt_entry(entry)`. On adopt refusal (duplicate-id
guard hands the entry back), re-adopt into A — same never-drop contract as
tear-off recovery. If B's window handle is gone by click time (closed
while the menu was open), treat as refusal: re-adopt into A, no toast.

**New window:** on A, `detach_session` → `open_managed_window_adopting`.
On open failure, `return_torn_off_entry`-style recovery: re-adopt into A
and unlatch `user_initiated_close` if the move had emptied A. This is
tear-off's exact recovery, generalized — see P4.

**Source close (N5):** when `detach_session` returns
`DissolveTerminus::WindowEmptied`, FIRST latch the source's disk fate via
`ws.mark_removed_if_window_emptied(terminus)` (`keymap.rs:871`, sets
`user_initiated_close`, `window_state.rs:2449-2453`) — without it,
`route_close_disk_fate` PRESERVES the emptied window's slot and it
restores as a broken empty window — then close A via the deferred
`window.remove_window()` pattern (`keymap.rs:870-876`) — never inside the
entity lease. Latch strictly AFTER a successful adopt; a refused/failed
adopt must not have marked. (Review I3.) `remove_window` bypasses the confirm gate (correct — nothing
is being killed) and still routes disk fate + quit-check. With the pool
gone, `route_close_disk_fate` has no detach step, and
`should_quit_after_close` sees other windows open (B or the new window
exists before A's deferred close runs — ordering guaranteed because the
adopt/open happens synchronously in the same update while the close is
deferred), so the app never mistakenly quits mid-move.

**P2 — store coalescing (successor to Phase 4's P6).** A's
`detach_session` and B's `adopt_entry` each `save_to_store()` into the
shared debounced store cache. Both must land in one flush: perform
detach → adopt in the same synchronous update chain (no await, no defer
between them) so the debounce window covers both writes. A hard kill
between "A's bucket shrunk" and "B's bucket grew" would lose the session
from disk; same-flush NARROWS that gap to the writer-thread
snapshot-between-upserts race (microseconds, self-heals on next
debounce — same residual Phase 4 shipped, `adopt_entry` doc step 6) plus
the process-kill-during-flush case v3's atomic-write already handles.
(Review N-a.) The implementer must NOT
insert an async hop between the two calls.

**P3 — relocate the transfer structs.** `DetachedEntry` +
`DetachedProject` move from the deleted `detached_pool.rs` into a new slim
`crates/nice/src/session_transfer.rs` (structs + doc comment only; the
free-function choreography dies with the pool). Keep the names — they are
accurate for a session mid-transfer ("detached" from any window), tear-off
docs/comments already use them, and renaming churns ~5 files for zero
behavior. Reviewer may overrule with a concrete better name.

**P4 — one recovery helper.** Tear-off's `return_torn_off_entry`
(`keymap.rs:969`) and Move's two failure paths want the same "put it back
in the source window, unlatch `user_initiated_close` if we emptied it"
logic. Extract one helper (e.g. `return_entry_to_source`) used by both.
The helper contract also owns the SUCCESS-side latch: source disk fate is
marked via `mark_removed_if_window_emptied` only after a successful
adopt/open (see N5 above; review I3).
This also resolves the parked Phase-4 cycle nit (c) — the duplicated
deferred `remove_window` closures (`keymap.rs:874` + `:948`) collapse into
the shared move/tear-off plumbing.

### What Move preserves (from Phase-4 semantics, unchanged)

- Whole-session moves carry Claude pills intact: `claude_session_id` moves
  with the session, never re-minted (resume identity is per-conversation);
  no resume refires. A ptyless/structural session (never-activated
  restore, resumable Claude) moves as an empty payload and lazy-respawns
  on activation in the destination — `insert_session` already creates the
  container.
- `pane_status` entries travel in `DetachedPtys.statuses` — destination
  sidebar dot is correct on arrival.
- Session id re-keys on leaving a window (`mint_session_id`) — the
  `terminals-main` collision defense. Not a bug.
- **Accepted wart (P11 successor):** a moved session's children keep their
  fork-time `NICE_SOCKET`/`NICE_TAB_ID`/`NICE_PANE_ID` env — a hand-typed
  `claude` in a moved pane lights the SOURCE window (or errors if that
  window closed). Same three shapes Phase 4 documented; Phase 5 pane
  addressing is the designated fix. Move-to-Window adds no new shape —
  it IS the explicit-detach shape with a destination.

## Slices

Ordered; each compiles + tests green before the next.

**Slice 1 — revert close/quit/window-less/settings.**
`request_window_close`, `request_quit`, `quit_cascade`,
`should_quit_after_close`/`should_quit_on_window_close`,
`route_close_disk_fate` restored to their `6aee7ac` bodies (use
`git diff 6aee7ac..HEAD` per file to scope; hand-apply, don't blind-revert
— Slice-2 surfaces still reference the pool at this point, so Slice 1 may
temporarily keep a dead `detach_eligible_sessions_into_pool` fn if needed
for compile order, deleted in Slice 2). Delete `on_reopen` +
`handle_dock_reopen` + recovery-window fns, `WindowCloseDecision`,
`QuitDecision`'s extra arm, the `close_window_detaches` pref +
Advanced-pane row + all their tests. Behavior gates after this slice:
⌘W with live sessions → confirm → kill; last-window close quits; ⌘Q
confirms as on main.

**Slice 2 — delete the pool, sidebar section, actions, persistence.**
Move `DetachedEntry`/`DetachedProject` to `session_transfer.rs` (P3);
delete `detached_pool.rs` + TWO of its three test submodules (`tests`,
`adopt_tests`) — `tearoff_tests.rs` PORTS to the new module home, it
tests kept behavior (review N-f); when deleting `adopt_tests.rs`, PORT
its pool-free coverage of the kept primitives into Slice 3's suite: the
re-homing matrix (`adopt_tests.rs:214` existing-project reuse, `:239`
project creation, `:259` Terminals re-home) and the parent-ref rules
(`:356` clear-unresolvable, `:385` keep-resolvable) (review I5). Delete
the sidebar Detached section + its tests + the attached-row "Detach
Session" item; remove actions `DetachSession`/`AdoptDetachedSession`
end-to-end (shortcuts.rs, keymap.rs incl. the ⌃⌘⇧D handler
`detach_active_session` at `keymap.rs:~838` (review N-c),
settings_import `RUST_ONLY` → 24, multiwindow itest stubs); the
`close_window_detaches` pref + Advanced-pane row land here per I2;
delete the `detached[]` persistence surface + tests +
`persistence_restore_live.rs` guard; delete `detach_adopt_live.rs` + its
registration. Also: `DetachedPtys::has_live` (`pty_manager.rs:~326-346`)
and `DetachedEntry::{structural, has_live}` + snapshot/hydrate impls die
with the pool; scrub pool doc-refs at `pty_manager.rs:281`,
`main.rs:114`, `session_store.rs:460` (reviews N-b, N-g). Rewire
tear-off's test fixtures off `DetachedPool`/`DetachedPoolGlobal`
(`keymap.rs:2494`). Tear-off itself must not change.

**Slice 3 — Move to Window.**
Menu items (P1 labels) in `open_session_context_menu`; existing-window
leg, new-window leg, N5 source close, P4 shared recovery helper, P2
same-update ordering. Repurpose `detach_session_from_window` as the
move-source step or fold it into the new flow. Unit tests: move A→B live
(pty handle identity preserved, scrollback intact — assert the same
`Entity<TerminalSessionHandle>` before/after), move structural session
(lazy respawn in destination), move last session (source closes, no
confirm, app does NOT quit), adopt-refusal returns to source, new-window
open-failure returns to source + unlatches, Claude session moves whole
with `claude_session_id` intact, store state after move has the session
in exactly one bucket, menu lists other windows + New Window and skips
self, single-window shows only New Window.

**Slice 4 — live selftest + docs.**
New `move_session_live.rs` scenario (`NICE_SELFTEST=move-session`),
reusing the deleted `detach_adopt_live.rs` harness patterns: two windows,
run a child in A, Move to B via the real menu path, assert the child
keeps running + scrollback present, then Move-to-New-Window leg, then
last-session-out closes the source. Update `crates/README.md` (drop
pool/detach references, keep R15), `docs/tmux-port-roadmap.md` Phase-4
entry (+ pane addressing pulled into this phase, rest of Phase 5 on ice),
`docs/tmux-port-progress.html` Phase-4 rows (`:274-280`, `:309` still
describe the pool/Detached section — review N-d), and stamp
`docs/plans/phase-4-detach-adopt-tearoff.md` with a short superseded-by
header (one line of why; the body stays as history).

## Validation

- `cargo test --workspace` green (expect a large net test DELETION —
  pool/adopt/close-detach/window-less suites go; Slice-3 tests arrive).
- Live selftest `move-session` PASS against installed Nice Dev.
- Black-box scratch-env: ⌘W with a running child → confirm popup → kill
  (pre-Phase-4 behavior restored); last-window close quits the app; ⌘Q
  confirm works; NO Detached section ever renders; `sessions.json`
  contains no `detached` key after a session move; a leftover `detached`
  key from the previous build parses without error.
- Hand feel-check (Nick): Move to Window between two real windows;
  Move to New Window; move-last-session closes source; tear-off ⌃⌘N
  unchanged; ⌘W confirm feels like pre-Phase-4.
- Scripted quits may use ⌘Q again after Slice 1 (the pool confirm-hang
  gotcha dies), but terminate-path (`osascript … quit`) remains the safe
  default in harnesses.

## Out of scope

- Drag-based move/tear-off (stays Phase 5).
- Multi-row Move; a keybinding for Move.
- Fixing the fork-time env staleness wart — **pulled out of Phase 5 into
  this phase as the NEXT plan** (Nick, 2026-08-22), grounded against the
  post-rework tree once this plan is implemented. Hard constraints from
  Nick for that design: (a) no socket may outlive its owner without a
  guaranteed cleanup path — "keep the source window's socket alive for
  moved panes" is ruled out; prefer app-global pane resolution.
  (b) Nice has external users now (Nick, 2026-08-22) — but a heavy
  old+new dual-serving bridge is NOT needed: shells never survive a Nice
  restart, so every post-upgrade fork gets the new env (Nick's own
  catch). What compat remains is thin: keep stamping the old env var
  NAMES as aliases into the new scheme (`NICE_SOCKET` → the app-global
  socket path) for external users' scripts that read them, and keep the
  socket wire protocol backward-compatible. No per-window sockets served
  post-upgrade. The rest of Phase 5 (CLI,
  synchronize-panes, display-panes, popup) is ON ICE. Slice 4's roadmap
  edit records both.
- Any change to tear-off behavior.
- Submenu/flyout infra for context menus.
