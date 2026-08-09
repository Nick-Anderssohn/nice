# Fork/branch session tracking — fix plan

## Background: what changed in Claude Code

Verified against the installed CLI (2.1.222, binary inspection), the changelog, and live on-disk state on this machine (2026-08-05).

- **`/fork` changed in v2.1.212.** It no longer rotates the current session (the old
  /branch-like behavior). It now copies the conversation into a **detached background
  session** run by the Claude Code daemon (`--resume <transcript copy> --fork-session`),
  registered under `~/.claude/jobs/<first-8-of-fork-id>/state.json` with
  `{sessionId, forkParentSessionId, forkBoundaryAt, name, ...}`. The foreground session
  keeps its id; no SessionStart fires in the foreground; the only foreground output is a
  human-readable confirmation line. Since 2.1.220 the fork may relocate into its own
  worktree (different cwd/project dir).
- **SessionStart `source` changed in v2.1.214.** In-pane rotations via `/branch` (and
  `--fork-session` resumes) now report `source: "fork"`, not `"resume"`. Documented
  sources are now `startup | resume | clear | compact | fork`.
- The fork's background child process **does** fire SessionStart (source `"fork"`, the
  new fork session id) — but in the daemon-spawned detached process, not the pane.

## The three bugs (single root cause: the two changes above)

Nice's only signal is the `SessionStart` hook (`claude_hook_installer.rs`) relaying
`{paneId: $NICE_PANE_ID, sessionId, source, cwd}` to the control socket, and
`WindowState::apply_session_update` (`window_state.rs`) classifying a rotation as
/branch **only** when `source == "resume"` && the pane's id actually changed.

1. **Forked conversations never appear in the sidebar** (reported bug). The foreground
   pane never rotates on `/fork`, and the fork's own SessionStart fires in a detached
   process. No Nice code path knows the fork exists.

2. **`/branch` parent materialization is broken on Claude ≥ 2.1.214.** `/branch` now
   relays `source: "fork"`; the `source == "resume"` gate misses it, so the tab adopts
   the new id as a plain update and the pre-branch conversation is silently dropped
   from the sidebar (no deferred-resume parent tab).

3. **Background forks corrupt an unrelated tab's session id.** The Claude daemon
   inherits `NICE_SOCKET` + `NICE_PANE_ID` from whichever Nice pane first spawned it
   (verified: daemon pid env carries the tmux-keybinds pane's ids). Every
   daemon-spawned fork child fires SessionStart; the hook relays it with that **stale
   pane id**; Nice resolves the pane to a live tab and rewrites its
   `claudeSessionId` to the fork's id. Live evidence: the "tmux keybinds" tab in
   `sessions.json` now points at `298689bf-…`, a session id with **no transcript
   anywhere on disk** (an aborted fork's id); its real conversation `2f3b14e8-…` is
   orphaned. That tab's deferred resume will fail after a relaunch.

## Design

One discriminator cleanly separates the two `source: "fork"` cases: the daemon creates
`~/.claude/jobs/<first8(sessionId)>/` (and copies the parent transcript into
`…/tmp/`) **before** spawning the fork child, so at hook time:

- `source == "fork"` **and** `~/.claude/jobs/<first8(new id)>/` exists ⇒ background
  fork. Never touch the pane's tab (the relayed paneId is untrustworthy). Materialize
  the fork as a new sidebar entry keyed off `forkParentSessionId`.
- `source == "fork"` **and** no jobs dir ⇒ in-pane rotation (`/branch`,
  `--fork-session` resume). Run the existing branch-parent flow.
- `source == "resume"` with an id change: keep the existing flow (pre-2.1.214 CLIs).

### Fix A — widen the /branch gate (bug 2)

In `apply_session_update` (`window_state.rs:908`), classify the branch-parent
materialization on `source ∈ {"resume", "fork"}` (with an actual id change), where the
`"fork"` arm is additionally gated on **no jobs-dir entry** for the incoming id.
`/clear` (`source: "clear"`) and unknown/absent sources stay plain id updates.

The jobs-dir probe must be injectable for tests: add a small seam on `WindowState`
(e.g. `fork_job_probe: Box<dyn Fn(&str) -> Option<ForkJobInfo>>`) defaulting to a real
filesystem read of `~/.claude/jobs/<first8>/`, overridable in unit tests. Respect
`$CLAUDE_CONFIG_DIR`-style overrides only if we already do elsewhere (we don't —
hardcode `~/.claude` like the hook installer does).

### Fix B — materialize background forks in the sidebar (bug 1)

On the background-fork classification:

1. Do **not** rotate any tab (this alone fixes bug 3's corruption vector).
2. Read `forkParentSessionId` (+ `name`) from `jobs/<first8>/state.json`. The hook can
   fire before `state.json` lands (the aborted `298689bf` job only ever got `tmp/`), so
   read it via a short deferred retry (e.g. poll a few times over ~5–10 s on a
   background task, then give up silently — an aborted fork should produce nothing).
3. Resolve the parent tab as the tab whose `claude_session_id == forkParentSessionId`.
   Search the owning window first, then all windows (the stale paneId says nothing
   about which window the parent lives in). No match ⇒ drop silently.
4. Insert the fork as a **nested child tab under the parent tab** (handoff shape:
   `TabModel::insert_handoff_child`, unselected, no focus steal — the foreground
   conversation stays where it is; the fork is the offshoot). Pin
   `claude_session_id = <fork id>`, cwd = the relayed cwd if non-empty else the parent
   tab's cwd, title = the job's `name` when available (it carries the `⑂` marker)
   else the parent's title.
5. Spawn its Claude pane deferred, mirroring `spawn_branch_parent`:
   `ClaudeSessionMode::ResumeDeferred(<fork id>)` ⇒ prefill `claude --resume <fork id>`.
   The prefill stays `--resume` (a neutral, human-readable form) — Fix D rewrites
   it at exec time: `attach` whenever the jobs entry exists (attach also wakes an
   evicted job), plain resume only when the entry is gone.

### Fix C — stop trusting stale pane ids (bug 3)

The jobs-dir probe screens **every** source, not just `"fork"`. The original
premise — that `startup`/`resume` in a bg child keep their ids stable and are
absorbed by the equality short-circuit — holds only for the child's own tab: the
daemon carries whichever pane last spawned it, so the incoming id is compared
against an UNRELATED tab's id and always differs. Live evidence (validation round
2): waking a cold background job with `claude attach` — the routine path once Fix
D landed — fired `source: "resume"` with the daemon's stale pane id, which rotated
that pane's tab onto the woken job's id and invented a branch parent for the
conversation it had just displaced. So: a jobs entry for the incoming id ⇒
daemon-originated relay ⇒ touch no tab. `source == "fork"` additionally
materializes the fork (Fix B) — it is the job's birth; every other source is a
later life-cycle event for a job that already has its tab.

The screen carries the same first-8 collision guard as Fix D's (a readable
`state.json` must name the incoming id), so a foreign job sharing the prefix can
never silence a genuine in-pane rotation — but it accepts a jobs dir whose
`state.json` has not landed, which is precisely the newborn fork whose relay must
not rotate anything.

Remaining exposure, unchanged: a bg `/clear` relays `source: "clear"` with a FRESH
id that keys no jobs directory (the entry stays under the job's original first-8),
so it still lands on the addressed tab. Pre-existing, not reachable through
`/fork`, and distrusting an id no probe can place would break the ordinary in-pane
`/clear`.

Hook script: unchanged. It already relays `source` verbatim (the wide `[^"]+` class
passes `fork` through), and per-child env is not something the script can validate.
Version note: bump nothing — the script bytes stay identical, so the
write-only-if-changed compare stays quiet.

### Fix D — exec-time resume/attach normalization (the `claude` shadow handshake)

A background session whose `~/.claude/jobs/<first8>/` entry exists must be opened
with `claude attach` (attach wakes an evicted job before attaching); only a session
with no jobs entry is opened with `claude --resume <uuid>`. Which one is correct can
only be decided at **exec time**
(the deferred prefill sits in the shell until the user presses Enter), and the user
may type either form by hand — so intercept both and normalize.

The interception point already exists: every interactive `claude` in a Nice pane runs
through the zsh shadow function (`shell_inject.rs`), which ships the full argv to Nice
over the control socket and execs what the reply says. Put the decision on the Nice
side (Rust: testable, fast file probes) and extend the one-line reply grammar
(`mode sid settings`) with two new mode verbs — the stub and the app ship together, so
the protocol can evolve safely:

- Handshake sees `--resume <uuid>` / `-r <uuid>`: if `~/.claude/jobs/<first8>/state.json`
  exists **and** its `sessionId` equals the full uuid (guards against first-8
  collisions) ⇒ the id is a daemon job ⇒ reply `attach <first8>` and the wrapper
  execs `command claude attach <first8>`. Otherwise keep today's flow.
- Handshake sees `attach <id>`: if the jobs entry is gone and the full uuid is
  recoverable (user passed a full uuid, or `state.json` still maps it) ⇒ reply
  `resume <uuid> <settings>` ⇒ wrapper execs
  `command claude [--settings …] --resume <uuid>`. If nothing is recoverable, pass
  through — attach errors out on its own.
- Wrapper safety net for the rewritten attach: run attach as a child first,
  `command claude attach <id> || exec command claude --resume <uuid>` — a stale
  jobs entry (daemon crashed without cleanup) then degrades to resume instead of
  stranding the user. Verify attach's exit-code semantics during implementation
  (Ctrl+Z detach suspends the shell job — that's SIGTSTP, not an exit — and a normal
  quit should exit 0; only failure takes the fallback).

Facts this rests on (verified on 2.1.222/2.1.223, incl. validation round 1):
`claude attach <id>` resolves ids by prefix-matching `~/.claude/jobs/` DIRECTORY
names (exit 1 on no match; `error ? 1 : 0` otherwise, so Ctrl+Z/normal quit never
takes the fallback); a **"done" background fork stays daemon-hosted with a live
pid** (observed: `b8c8244b`, state done, pid 28068), so `--resume` races it even
after the work finishes; the CLI **refuses** `--resume` of a daemon-hosted session
("currently running as a background agent"); a daemon-evicted entry keeps its jobs
dir (`pid: null` + `respawnFlags`) and attach WAKES it ("Waking session …") before
attaching; deleting `jobs/<first8>/` DESTROYS the fork (its source transcript is
`jobs/<first8>/tmp/parent-transcript.jsonl`) — with the dir gone the session is
unreachable by BOTH verbs and the CLI's own error is the correct outcome. Hence:
jobs entry present ⇒ attach (covers live AND evicted); entry absent ⇒ resume. The
file probe stays (the handshake must reply inside the wrapper's `nc -w 2` budget).

Two as-built additions from validation round 1: the NEWTAB spawn arm now treats
session-identifying args like the in-place arm (never splices a minted
`--session-id` over `--resume`/`attach` — a pre-existing main bug this feature
surfaced), and the wrapper reports a RETURNED attach back to Nice so the pane's
claude-running state clears when the attach client exits back to the shell.

Out of scope, noted: `agents --json` also exposes *interactive* sessions with live
pids, so "user resumes a session that's already open in another tab" is detectable
the same way. Skip it (YAGNI) unless it bites.

### Repair note (one-off, not code)

The live "tmux keybinds" tab (`t1785706616426-000d`) points at the phantom
`298689bf-…`; its real conversation is `2f3b14e8-…` (transcript in the
`…worktrees-tmux-keybinds` project dir). After landing the fix, repair by re-pointing
the tab (while Nice is quit, edit `sessions.json`) or just note that resuming that tab
needs `claude --resume 2f3b14e8-…` manually.

## Touch points

- `crates/nice/src/window_state.rs` — `apply_session_update` classification; new
  fork-materialization path + deferred state.json retry task; jobs-probe seam;
  cross-window parent resolution (router currently dispatches `session_update` to the
  pane-owning window — the fork path needs an all-windows search hook at the app/router
  layer, mirror how other cross-window lookups are done or thread it through the
  existing dispatch).
- `crates/nice-model/src/tab_model.rs` — reuse `insert_handoff_child` (likely a thin
  variant that also pins `claude_session_id` on the child before insert; check whether
  the handoff path already supports that).
- `crates/nice/src/session_manager.rs` — nothing new expected;
  `ResumeDeferred` + `register_tab_session` already cover the deferred spawn.
- `crates/nice/src/claude_hook_installer.rs` — doc comment updates only (source list,
  /fork semantics).
- `crates/nice/src/shell_inject.rs` — Fix D: new `attach`/`resume` reply verbs in the
  wrapper's mode dispatch + the attach-fallback chain; the zdotdir stub is
  regenerated by Nice on startup, so stub and app stay in lockstep.
- The socket `claude`-request handler (`window_state.rs` /
  `resolve_claude_request`) — Fix D's decision: parse resume/attach-shaped args,
  probe the jobs dir, choose the reply verb. Treat `attach <id>` as
  session-identifying (like `--resume`) so no `--session-id` gets spliced in.

## Tests

- `window_state` unit tests (extend the existing `apply_session_update` suite):
  - `source: "fork"`, id change, **no** jobs entry ⇒ rotation + branch parent (Fix A).
  - `source: "resume"`, id change ⇒ unchanged legacy behavior.
  - `source: "fork"`, jobs entry present ⇒ pane's tab id **unchanged** (bug-3
    regression pin) and fork materialization requested.
  - fork materialization: parent resolved by session id ⇒ nested child inserted,
    unselected, pinned to fork id, deferred pane; parent-not-found ⇒ no-op;
    state.json missing after retries ⇒ no-op.
  - `/clear` with stale pane id still applies (documenting the accepted exposure).
- `tab_model` tests for the child-insert variant (depth-1 rule, Terminals refusal).
- `claude_e2e_live.rs`: extend the `(f)` session_update leg with a `source: "fork"`
  rotation (no jobs dir) asserting the branch parent, and a jobs-dir-backed fork
  materialization leg (point the probe seam / a temp `$HOME`-shaped fixture at a
  scratch jobs dir).
- Hook script blackbox tests: add a `source":"fork"` payload case (relay verbatim).
- Fix D tests: request-handler units (resume-shaped args + jobs hit ⇒ `attach` reply;
  jobs miss ⇒ unchanged; first-8 collision with mismatched `state.json` sessionId ⇒
  unchanged; attach-shaped args + evicted job ⇒ `resume` reply); `shell_inject`
  template assertions for the new mode verbs and the fallback chain (extend
  `zshrc_dispatches_newtab_and_inplace_modes`).

## Verification (real app)

Scratch-env `Nice Dev` launch (per CLAUDE.md), then in a Claude pane:
1. `/branch` ⇒ old conversation appears as sibling parent tab (deferred), tab follows
   the new id.
2. `/fork` ⇒ foreground tab unchanged (id and selection); a nested, unselected child
   tab appears pinned to the fork id; opening it and pressing Enter resumes the fork.
3. Fork from a pane in window A while a tab of window B owns the daemon's stale pane
   id ⇒ B's tab is untouched (bug-3 regression).
4. Open the fork tab while the daemon still hosts the job ⇒ the prefilled
   `claude --resume` execs as `claude attach` and lands in the live session.
5. Evicted-job leg: quit the daemon (or let it reap the job) while leaving
   `~/.claude/jobs/` UNTOUCHED — a real eviction keeps the entry (`pid: null` +
   `respawnFlags`). Opening the fork tab still replies `attach`; attach wakes the
   job ("Waking session …") and attaches, with the wrapper's `|| --resume` net
   covering a failed wake. Do NOT delete `jobs/<first8>/` to simulate eviction —
   the fork's source transcript lives inside it, so with the dir gone the fork is
   unrecoverable by BOTH verbs and the CLI's own error is the correct outcome
   (assert only that Nice itself didn't kill the pane).
6. Hand-typed forms: `claude --resume <uuid>` and `claude attach <uuid>` typed at
   a pane prompt spawn a valid argv on EVERY handshake arm — in particular the
   newtab arm must not splice a minted `--session-id` over session-identifying
   args (pre-existing main bug, fixed in this branch).
