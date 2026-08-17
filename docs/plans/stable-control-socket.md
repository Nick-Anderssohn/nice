# Stable control-socket paths (Phase 4 carve-out)

**Status:** SHIPPED — landed on main 2026-08-16 (squash `341d234`,
feel-check passed incl. the hand-only `/nice-handoff`-across-restart
gate). See § As shipped. Twice Fable-reviewed pre-implementation
(round 1: 1 blocking + 3 important + 6 nit, all folded; round 2: 10/10
folds verified, 0 blocking + 2 important + 4 nit fresh findings, all
folded — reports at `.claude/handoff/stable-socket-plan-review.md` +
`…-round2.md`). Carved out of Phase 4 (roadmap § "Phase 4 — detach,
adopt, tear-off") to ship ahead of the rest of the phase. The bug is live:
since Claude Code 2.1.139 daemon-hosts sessions across client restarts,
every Nice relaunch strands the fork-time `NICE_SOCKET` in every long-lived
session ("no reply from control socket", first hit 2026-08-13).

**Goal:** a window's control-socket path survives app relaunch, so a
long-lived session's `NICE_SOCKET` env keeps working after Nice quits and
reopens. Key the path by the persisted window id (`PersistedWindow.id`,
restart-stable) instead of app pid + nonce, and arbitrate ownership on bind
with a connect probe.

**Known gaps (deliberate):**
- A session whose window is never restored still holds a dead path
  (unchanged from roadmap). Revisit with Phase 4 adopt-into-window, which
  re-homes sessions across windows anyway.
- The fix only helps sessions forked AFTER the upgrade. Pre-upgrade
  sessions hold `nice-<oldpid>-<nonce>.sock` in their frozen env; that
  path never recurs under the new scheme, so the first post-upgrade
  restart is one last full stranding event. Nice cannot heal them — the
  manual `NICE_SOCKET=<live sock>` override remains the bridge.
- Trust boundary: `$TMPDIR` is per-user 0700. The stable path is
  predictable and D2 defers to any live listener, so a same-user process
  could squat a window's path (permanent legacy-fallback for that window).
  A same-user attacker is outside the threat model (they can already read
  the env and worse) — accepted; this deliberately weakens the old
  "blind unlink always wins" behavior.

## Current-code facts the plan builds on

- Path minted at `NiceControlSocket` construction, before bind, so it can
  ride pty env: `mint_socket_path()` → `$TMPDIR/nice-<pid>-<8hex>.sock`
  (`control_socket.rs:839-848`, constructor `:329-343`,
  `NICE_SOCKET_PATH` test override `:840-842`). Two-phase
  mint-then-bind is deliberate — preserve it.
- `bind_and_listen()` (`control_socket.rs:741-812`) runs synchronously on
  the calling (gpui main) thread from `start()` (`:357-386`); it
  **unconditionally unlinks** any existing file before bind (`:767-769`).
  Safe today only because pid+nonce paths never recur across processes;
  with a stable path this becomes a blind steal — this is the exact site
  the probe replaces. No `EADDRINUSE` handling exists anywhere in the file.
- Self-heal rebind: the accept loop (`:415-506`, own OS thread) funnels
  stop/`POLLERR`/file-vanished into one path that **drops the listener
  first**, then re-calls `bind_and_listen` with backoff (`:443-462`). It is
  same-process healing, not cross-process arbitration; it must keep working
  through any bind change.
- `SUN_PATH_CAP = 104` guarded loudly (`:97`, `:743-755`, test
  `bind_rejects_overlong_path_loudly` `:1952-1958`). macOS `$TMPDIR` is
  already ~40-50 bytes — a full 36-char UUID in the filename pushes the
  total near the cap.
- `PersistedWindow.id` (`session_store.rs:111-134`) is a UUIDv4 string,
  minted at `WindowState` construction (`window_state.rs:604-610`, helper
  `mint_window_session_id` `:104-106`; real v4 via `getentropy(2)`,
  `pty_manager.rs:2600-2609` — the version nibble sits at position 13,
  outside a 12-hex truncation), overwritten verbatim from `sessions.json`
  on restore (`window_state.rs:682-683`, `restore.rs:64-66` — "restored
  windows keep their saved id"). It is set **strictly before**
  `arm_window_control_socket` runs (`app.rs:1601-1655`), so the socket
  path can be keyed on it with no reordering.
- `arm_window_control_socket` (`app.rs:1490-1552`) today constructs the
  socket, stamps `WindowShellEnv` from `socket.path()`, and only THEN
  calls `start()`. `SocketShared.path` is an immutable `String` in an
  `Arc` shared with the accept-loop thread (`control_socket.rs:291-303`).
  Both facts matter for the D2 fallback — see Slice 1/2.
- `NICE_SOCKET` stamping: `session_window_env_pairs()`
  (`pty_manager.rs:1962-1983`) and `build_claude_extra_env()`
  (`:2705`) inject the window's `socket_path` string into every pty.
  `NICE_TAB_ID`/`NICE_PANE_ID` are separate routing ids — untouched here.
  No consumer parses the path's shape.
- Stale-sock sweep (`tmp_sweep.rs`): `parse_pid_from_socket_name`
  (`:74-81`) takes the first `-`-delimited token after `nice-` as a pid and
  reaps by pid-liveness; anything unparsable falls to `Ignore`
  **silently** (`:49-65`). Runs at app start before the first window's
  socket is minted (`app.rs:1391-1394`). A pid-free name silently stops
  being swept — the classifier must gain a new-format branch.
- No cross-window socket discovery exists anywhere; each pty knows only its
  own window's path via env. Cross-window routing is in-process via
  `WindowRegistry` (`window_registry.rs:170-221`). Nothing to update there.
- Teardown: window close and app quit always stop+unlink the socket
  (`window_registry.rs:258-313`, `window_state.rs:3108-3128`,
  `app.rs:889-901`); quit preserves every window's `sessions.json` row, so
  the id (and therefore the path) recurs on restore. Stale files on disk
  therefore come from crashes, not clean quits.
- Tests: hermetic in-process suite in `control_socket.rs:977-2026`
  (self-heal trio, parse matrices, `mint_path_matches_frozen_pattern`
  `:1919-1943` which asserts the current name shape), sweep classifier
  tests `tmp_sweep.rs:133-269`, teardown seam test
  `window_state.rs:4719-4740`, and the real-pty `shell-socket` live
  scenario (`shell_socket_live.rs`).

## Decisions (plan-level — flag at sign-off if any grates)

- **D1 — name: `nice-w-<12hex>.sock`**, where `<12hex>` is the first 12 hex
  chars of the persisted window id with dashes stripped. 24-byte filename
  (shorter than today's ~26) keeps `sun_path` headroom; 48 bits of UUID
  entropy makes cross-window collision negligible, and D2 makes the worst
  case graceful anyway. The `w-` discriminator keeps the legacy pid parser
  naturally inert on new names (`"w"` fails the `i32` parse → `Ignore`).
- **D2 — probe verdict "live owner" → fall back to the legacy pid+nonce
  path** for this run and log a warning. Never steal a live socket. The
  window stays fully functional, just not restart-stable — old behavior.
  This covers both the negligible truncated-id collision and any
  same-user cross-instance overlap.
  **Ordering constraint (review B1):** the fallback resolves the FINAL
  path inside `start()`, so `WindowShellEnv` must be stamped from
  `socket.path()` AFTER `start()` returns — otherwise fallback-case shells
  get `NICE_SOCKET` pointing at the foreign owner's live socket (silent
  cross-window/cross-instance misrouting, strictly worse than today's
  dead path). Reordering is safe: no pty forks until
  `arm_window_control_socket` returns (Main spawn is `app.rs:1657+`), so
  env-before-fork holds and mint-then-bind both complete pre-fork. The
  path swap rebuilds `SocketShared` before the accept thread spawns
  (fallback happens during the synchronous `start()`, no concurrent
  reader yet) — no lock needed.
- **D3 — sweep liveness for new-format names is a connect probe**: connect
  refused ⇒ stale ⇒ delete; connection accepted ⇒ live ⇒ keep; any other
  error ⇒ `Ignore`. The legacy pid-liveness branch stays for old-format
  names (upgrades leave `nice-<pid>-<8hex>.sock` strays behind for one
  sweep generation).
- **D4 — the probe lives inside `bind_and_listen`, uniformly.** The
  self-heal rebind path is safe through it: the loop drops its listener
  before rebinding, so its own probe sees `ECONNREFUSED` and proceeds to
  unlink+rebind exactly as today. No same-process/cross-process fork in
  the code.
  **`OwnedElsewhere` in the self-heal loop (review I2):** the rebind can
  also hit a live FOREIGN owner (our file unlinked externally, a peer
  takes the path during our downtime). The loop treats it as retryable
  like any bind error (`Err(_) => continue` with backoff gives this for
  free once `OwnedElsewhere` is an `io::Error`) — retry-forever at the
  stable path is CORRECT: the foreign owner may quit and free the path,
  at which point this window reclaims it and its already-stamped env
  becomes right again. No D2 fallback in the loop (env is stamped;
  switching paths mid-life helps nothing). Add a rate-limited log so a
  persistent foreign owner is visible, not a silent ≤5 s probe loop.
- **D5 — probe = plain blocking `UnixStream::connect`, no bytes sent.**
  On macOS AF_UNIX, `connect(2)` returns immediately (BSD semantics):
  `ECONNREFUSED` for a dead file AND for a full backlog — no
  deadline/nonblocking machinery needed (add one only if review finds a
  genuine blocking mode). Consequence: a live-but-backlogged owner probes
  as "refused" — bounded by the same self-heal recovery as the sweep race
  (see Slice 3). Probe closes without writing — the server already
  tolerates zero-byte connections (`read_framed_line` → `None` → silent
  close, `control_socket.rs:546-549, 574-576`); keep a test pinning that.
  **Error taxonomy (review F1): only a SUCCESSFUL connect proves a live
  owner ⇒ `OwnedElsewhere`. EVERY connect error — refused, `ENOENT` (file
  unlinked between the exists-check and the connect), not-a-socket, any
  residue — ⇒ stale ⇒ unlink + bind.** Deliberate dual of D3: the sweep
  must never delete a maybe-live foreign socket (Ignore on doubt); the
  bind must end with a working socket for THIS window and defers only to
  a proven-live owner (take on doubt). Unlink+bind absorbs the junk-file
  case; a directory squatting the path fails `remove_file` →
  `EADDRINUSE` → D6 → legacy fallback — graceful. Initial bind stays
  synchronous on the main thread; window-open latency unchanged.
- **D6 — TOCTOU backstop:** handle `EADDRINUSE` from `bind` (possible if a
  live peer re-creates the file between probe and bind): re-run
  probe+bind once; if it fails again, fall back to the legacy path (D2).
  **Scoping (review F2), mirroring D4's `OwnedElsewhere` rule:** the
  probe+bind retry-once may live in `bind_and_listen`/a shared helper,
  but the FALLBACK leg is initial-`start()`-only. In the accept loop a
  second `EADDRINUSE` is just an `io::Error` → `Err(_) => continue` with
  backoff (converging with D4: retry forever at the stable path, reclaim
  when the peer frees it). A mid-loop path switch would resurface B1's
  misrouting (env already stamped) AND break the lock-free
  `SocketShared` swap's no-concurrent-reader premise.
  Untested-by-design (the race isn't deterministically reproducible
  without a seam) — covered by code review. The symmetric interleaving
  (peer unlinks OUR file after our bind) does not surface as
  `EADDRINUSE`; it surfaces as the health-check "file vanished" rebind,
  which D4/I2 covers — that closes the TOCTOU analysis.
- **D7 — constructor threading:** `arm_window_control_socket` mints the
  window-keyed path (it has `ws.window_session_id()` in hand) and passes
  it into a new `NiceControlSocket::with_path(...)` constructor.
  `NICE_SOCKET_PATH` override still wins. Existing no-window call sites
  (unit tests, `window_state.rs:4721-4725`) keep the legacy
  `mint_socket_path()` default — no `Option` plumbed through the struct.
  `with_path` must compose with custom intervals — the arm site's
  scenario branch constructs `with_intervals(h, 500ms)`
  (`app.rs:1501-1504`); provide `with_path_and_intervals` (production
  defaults at the plain call) so scenarios keep both their fast health
  cadence and the window-keyed path.

## Slice 1 — control_socket: window-keyed path + probe takeover

`crates/nice/src/control_socket.rs`.

- Add `with_path` + `with_path_and_intervals` constructors (D7); keep
  `mint_socket_path()` as the no-arg default and the `NICE_SOCKET_PATH`
  override seam.
- Add `mint_window_socket_path(window_id: &str)` implementing D1.
- Replace the unconditional unlink at `:767-769` with: path exists →
  probe (D5) → refused ⇒ unlink + bind; accepted ⇒ return a
  distinguishable `OwnedElsewhere` error (as an `io::Error` so the
  accept loop's `Err(_) => continue` retries it — D4/I2 — with a
  rate-limited log). Handle `EADDRINUSE` per D6. During the synchronous
  initial `start()` ONLY, `OwnedElsewhere` triggers the D2 fallback:
  re-mint legacy path, rebuild `SocketShared` (pre-thread, no lock —
  see D2), bind that, log.
- **Mutation mechanism for the swap (review F3):** `start()` takes
  `&self` today (`:357`) and cannot replace `self.shared`. Change it to
  `&mut self` and assign `self.shared = Arc::new(...)` (or `Arc::get_mut`
  — sound: the sole `Arc::clone` happens after bind, at `:370`). Every
  call site owns the socket locally, so `&mut` is free. Do NOT wrap
  `path` in a `Mutex`/`RwLock` — that taxes every reader incl. the accept
  loop for nothing.
- **Fallback is single-shot (review F6):** if the fallback bind fails
  too, propagate `start()`'s `Err` — the arm site already logs it
  non-fatally (`app.rs:1521-1525`; shells fall back to direct `claude`).
  Never loop the fallback: under the `NICE_SOCKET_PATH` override,
  "re-mint legacy path" returns the SAME overridden path, so a retry
  loop would spin on `OwnedElsewhere` forever. (Today's behavior in that
  setup is a blind steal of the first binder's path — a clean `Err` is
  an improvement.)
- Length guard unchanged (new name is shorter).
- Tests: rewrite `mint_path_matches_frozen_pattern` for both minters;
  new tests — takeover over a stale file (pre-create a dead socket file,
  assert bind succeeds and unlinks it), live-owner fallback (bind a real
  `UnixListener` on the path first, assert construction lands on a legacy
  path and the live listener is untouched), self-heal trio still green
  through the probe, zero-byte connection tolerated by the client handler.

## Slice 2 — app wiring

`crates/nice/src/app.rs` (`arm_window_control_socket`, `:1490-1552`).

- Mint the window-keyed path from `ws.window_session_id()` and construct
  via `with_path` / `with_path_and_intervals` (scenario branch, D7).
- **Reorder (B1): `start()` FIRST, then `set_window_shell_env` from the
  post-start `socket.path()`** — the env must carry the RESOLVED path
  (D2 fallback can change it inside `start()`). Safe: no pty forks until
  arm returns. The pty-side stamping code itself is untouched.
- **The RETURN value moves too (review F4):** arm returns the minted path,
  captured today at `app.rs:1505` — before `start()`. The `shell-socket`
  scenario asserts against that return (`shell_socket_live.rs:221-413`),
  so it must also be read post-`start()` (same read as the env stamp),
  or the fallback case leaves the scenario asserting a path the socket
  no longer holds. Update arm's doc comment in the same pass — its
  ordering prose ("Set the window's shell-injection env BEFORE the
  caller forks", `:1507`) half-describes the old order.
- `shell-socket` live scenario: confirm it still passes; it exercises the
  transport, not the name.

## Slice 3 — sweep + docs

- `tmp_sweep.rs`: add the new-format branch (D3) ahead of the legacy pid
  branch; legacy branch and its tests unchanged. New tests: stale
  new-format socket deleted, live new-format socket kept (real listener),
  malformed `nice-w-*` names ignored, and the existing
  `socket_missing_pid_segment_is_ignored` family still passes.
  Note: the sweep now pokes live sockets of other instances with
  zero-byte connections — covered by the Slice 1 tolerance test.
- **Accepted cross-instance TOCTOU (review N4):** instance B's sweep can
  classify a path stale and unlink it in the gap after instance A binds
  it (simultaneous startups), leaving A's live socket unlinked; same
  outcome from D5's backlog-full false-refused. A's 30 s health `stat()`
  recreates it; meanwhile A's shells degrade gracefully (direct
  `claude`). Same family: our `stop()`/loop-exit unlink
  (`control_socket.rs:395, 505`) could remove a foreign owner's socket
  after a downtime takeover — foreign self-heal recovers. Tiny
  probability, automatic recovery — accepted, and one more reason D3's
  "any other error ⇒ Ignore" is the right conservative default.
- `docs/tmux-port-roadmap.md`: mark the stable-socket bullet in Phase 4 as
  carved out and shipped separately (dated), leaving the rest of the phase
  intact.

## Ordering

1 → 2 → 3. Slices 1 and 2 land together in one review round (2 is a small
call-site change); 3 is independent after 1 defines the name.

## Validation

- **Unit:** full `cargo test -p nice` for the touched crate; targeted
  modules `control_socket`, `tmp_sweep`, `window_state` teardown seam.
- **Live scenario:** `shell-socket` selftest under the worktree lock.
- **Black-box restart survival (the actual fix, scriptable):** scratch-env
  launch of the installed `Nice Dev` bundle (seeded HOME per CLAUDE.md).
  `$TMPDIR` is SHARED with the user's live prod Nice (the scratch recipe
  doesn't override it), so never grep `$TMPDIR` for "the" socket —
  **derive the expected name**: read the window id from the scratch
  `sessions.json` (under the scratch support root), compute
  `nice-w-<12hex>.sock`, and assert on that exact path (this doubles as
  an end-to-end check of the name derivation). Verify it answers an `nc`
  probe; graceful quit; **wait for the old pid to exit** (review F5 —
  the script launches the bundle binary directly, so nothing else
  serializes the relaunch; a still-live old listener would probe as a
  live owner → D2 fallback → false-fail of the very fix under test);
  relaunch same scratch env; assert the **same path** exists and answers
  again. Then the crash case: SIGKILL the app,
  relaunch, assert the restored window is live on the same path. (Note:
  the SIGKILL leg exercises the sweep's stale-delete, not the bind-probe
  takeover — the sweep runs before the first bind; same observable. The
  takeover itself is pinned by the Slice 1 unit test. Don't "fix" this
  check to assert a takeover log line — it never fires here.)
- **Hand-only gate (feel-check):** with a daemon-hosted Claude session
  alive across a Nice Dev restart, run `/nice-handoff` (or dispatch) from
  the pre-restart session and see it land — the end-to-end symptom that
  started this. The session MUST have been forked under the NEW build
  (a pre-upgrade session holds an old-format path and false-fails — see
  Known gaps). gpui-level scenarios can't see this; hand-test only.

## As shipped (2026-08-16)

Implemented by dev-cycle-orchestrator-fable run `stable-control-socket`
(state/report at `/Users/nick/Projects/nice-cycle-runs/stable-control-socket/`):
single cycle, squash `341d234` (9 files, +1265/−111), 3 review rounds,
zero drift, zero rejected findings. Feel-check passed 2026-08-16 incl.
the hand-only gate (`/nice-handoff` from a pre-restart daemon-hosted
session forked under the new build, delivered after a Nice Dev restart).

Beyond the plan — found and fixed in-cycle:

- **Listener fd now sets `FD_CLOEXEC`** (`set_cloexec` right after
  `libc::socket`, + a test pinning the asymmetry vs. accepted streams,
  which std already covers). Without it every pty child inherited the
  listener fd; harmless under pid+nonce paths, fatal under stable ones —
  any child outliving the app kept `connect(2)` succeeding at the stable
  path, so the next launch's probe read Nice's OWN orphan as a live
  owner → permanent D2 legacy fallback → fix silently defeated.
- **Sweep D3 is a real three-way verdict** (`SocketLiveness::{Live,
  Stale, Unknown}`, delete on Stale only — Stale = `ECONNREFUSED`/
  `ENOENT`). An early implementation collapsed it to
  delete-on-any-error; caught as a blocking review finding. The module
  header now states why the sweep and bind taxonomies deliberately
  differ — do not "unify" them.
- **`shell-socket` scenario reports a missing `--features selftest`
  build up front** instead of failing blind (record assertions compile
  to no-ops without the feature; the FAIL looks like a product bug).
- Arm-seam tests pin BOTH the uncontested window-keyed stamping
  (`arm_stamps_the_window_keyed_socket_path_when_uncontested`) and the
  contested post-`start()` stamp ordering
  (`arm_stamps_shell_env_from_the_path_start_resolved`, verified to
  bite on a reorder).

Validation as run: full `cargo test -p nice` 1114 green; `shell-socket`
selftest PASS under the worktree lock; black-box scratch-env restart
survival PASSED both legs (same `nice-w-<12hex>` path — derived from the
scratch `sessions.json` — answers `nc` after graceful quit→relaunch and
after SIGKILL→relaunch), plus an automated stand-in of the hand gate
(real `claude` request to the pre-restart frozen path answered after
restart).

Parked (deliberate — don't re-litigate unprompted): the accept-loop
contested-path retry branch and the bare-`EADDRINUSE` arm of the
contested-path predicate have no direct test (both graceful-degradation
backstops; non-steal + reclaim are covered via the tested probe and
self-heal paths).
