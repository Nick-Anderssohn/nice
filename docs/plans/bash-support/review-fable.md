# Deep review — shell-abstraction design + bash-support plans 01–04

Reviewer: single deep pre-implementation review (Fable). Scope: `docs/design/shell-abstraction.md`
and `docs/plans/bash-support/01…04`, checked against the F1–F12 inventory
(`zsh-inventory-opus.md`) and against the actual code in this worktree. Method: every cited
file:line was opened and compared; every load-bearing bash-3.2 claim was executed against the
real `/bin/bash 3.2.57` on this machine (results in the appendix). The settled decisions
(trait + `Box<dyn>`, `--rcfile`, app-typed prefill, synchronous compose, resolution order) were
not re-litigated; no correctness hole was found in any of them.

Verdict up front: the plan set is unusually sound — the code citations are accurate to the
line, the dialect tables are correct (several subtle claims verified empirically), and the
4.3 gate correction in plan 03 is right. Two blocking seam problems and three important
defects need fixing before implementation; the rest is nits.

---

## Blocking

### B1. Mid-run shell switching: design §4 contradicts itself, and plans 01 and 04 each picked a different half

- **Where:** design §4 ("Where the result lives" + "Setting changes mid-run"); plan 01 W2.4
  (last bullet); plan 04 W4 (items 1–6).
- **The claim(s):** Design §4 says `ShellRuntime` is *"Set once in
  install_shell_inject_bootstrap; read-only afterwards"* and that panes never re-resolve — and
  two paragraphs later says *"Setting changes mid-run apply to newly spawned panes only"*,
  which is only possible if something re-resolves mid-run. Plan 01 implements the first half:
  W2.4 says *"Mid-run setting changes: nothing to do — ShellRuntime is written once at
  bootstrap."* Plan 04 implements the second half: W4 re-resolves on a picker click, writes the
  new profile's rc files, **replaces the `ShellRuntime` global**, fans the new inject pairs out
  to every live window's `WindowShellEnv`, and re-probes `claude` — without listing the
  "read-only afterwards" violation as a deviation.
- **What the code says:** `WindowShellEnv` is set per window at `arm_window_control_socket`
  (`app.rs:1508`, via `ws.ptys.set_window_shell_env(...)`) and is never refreshed today, so
  plan 04's fan-out is genuinely required for "new panes in existing windows get the new
  shell". `kickoff_claude_probe` (`app.rs:1433-1448`) sets `ResolvedClaudePath`
  unconditionally at `:1446`, so plan 04's replace-only-on-success guard is also genuinely
  required. Both of plan 04's "if plan 01 didn't build it, build it here" hedges are live.
- **Why blocking:** if plan 01 lands as written, its implementer will (reasonably) pin the
  set-once contract — doc comments, maybe a test asserting `resolve()` is called exactly once —
  and structure the bootstrap as one inline block. Plan 04 then has to un-pin that and
  restructure `install_shell_inject_bootstrap` after the fact. This is the one seam where the
  plans build *conflicting* shapes, not just adjacent ones.
- **Fix:**
  1. Amend design §4: "read-only afterwards" becomes "replaced only by an explicit settings
     pick (step 6); panes still never re-resolve — they keep their spawn-time `PaneShell`."
  2. Plan 01 factors the resolve → `write_rc_files` → `set_global(ShellRuntime)` sequence into
     a reusable `install_shell_runtime(cx, &ShellSetting)` called by the bootstrap (a few
     lines, no behavior change in 01), and must NOT pin set-once semantics anywhere.
  3. Plan 04 keeps ownership of the `WindowShellEnv` fan-out and the guarded re-probe, exactly
     as it wrote them.

### B2. `"bash"` never enters `all_known_comm_names()` — each plan assumes the other does it

- **Where:** plan 01 W1.1 + Open question 3; plan 02 "Dependency" section + work item 3.
- **The claim(s):** Plan 01 ships the registry as `["zsh"]` and its OQ3 says *"step 3 adding
  `"bash"`"*. Plan 02's dependency section asserts the registry is *"already `["zsh", "bash"]`
  per the design doc"*, and its §"$0 / argv / comm" section says `BashProfile::comm_name()`
  *"agree[s] with the `all_known_comm_names()` entry the reaper already matches since plan
  01"*. Plan 02's work items add the `resolve()` bash arm (item 3) — and **no item touches the
  registry**. Implemented as written, the registry stays `["zsh"]` forever.
- **What the code/design say:** the reaper's accepted set (design §7) is *registry union ∪
  active profile's comm*. With the registry stuck at `["zsh"]`, a user who ran bash panes,
  crashed, and relaunched under a zsh setting leaves bash orphans the prefilter never sees —
  the exact cross-run scenario §7's registry union exists to cover (today's comm filter is the
  literal `"zsh"` at `orphan_reaper.rs:177`). Plan 02's acceptance test ("reaper comm-union
  test covers a bash argv fixture") would pass anyway when the active profile is bash, so the
  gap ships silently.
- **Fix:** plan 02, work item 3 (or item 1) explicitly: flip `all_known_comm_names()` to
  `["zsh", "bash"]` and extend the comm-union unit test with the registry-covers-bash-while-
  active-profile-is-zsh row. One line of code + one test row; the point is assigning an owner.

---

## Important

### I1. Plan 04's user-facing copy says "bash 4 or newer" — the corrected gate is 4.3

- **Where:** plan 04 W6 (`compose_model_info` unavailable sentence: *"it needs zsh, or bash 4
  or newer"*); also the W2 rationale aside *"Compose only works on the ≥4 one"*.
- **What the sibling docs say:** plan 03 (and the already-corrected design §6.3) establish
  ≥ 4.3, with 4.0–4.2 deliberately unsupported (multi-char `bind -x` sequences fire only from
  4.3). Plan 03's non-goals explicitly hand step 6 the copy *"naming the 'bash ≥ 4.3'
  requirement"*. Plan 04 was evidently written against the pre-correction design and reintroduces
  the same class of error the 4.0→4.3 fix removed — this time in the string users read.
- **Fix:** *"…it needs zsh, or bash 4.3 or newer."* Update the W2 aside and the copy unit test
  (`compose_model_info("bash", false)` should pin the "4.3" literal so it can't regress).

### I2. Plan 01's degraded-env "preserved quirk" leaks `NICE_USER_ZDOTDIR` into non-zsh panes

- **Where:** plan 01 W1.4, the `window_inject_pairs` sketch (legacy `None` arm) + Open
  question 2.
- **The claim:** on `inject: None` (rc write failed), emit
  `[("NICE_USER_ZDOTDIR", <inherited-or-empty>)]` to preserve today's behavior
  (`session_window_env_pairs`, `pty_manager.rs:1311-1314` — verified: the var is always set
  once a socket is armed, even when `zdotdir` is `None`).
- **The problem:** the sketch branches on `runtime.inject` only, not the profile kind. Once
  step 2 lands, a **bash or fallback** profile whose `write_rc_files` failed (or the scenario
  reset path) takes the same arm — injecting a zsh-only variable into bash/fish panes. That
  directly contradicts plan 02's own contract ("injected spawns carry no bash-specific env";
  `BashProfile::inject_env` → empty) and its acceptance criteria; worse, these are
  *non*-injected spawns carrying a zsh var. Harmless to bash itself, but it's exactly the kind
  of cross-shell leakage the whole abstraction exists to prevent, and it would trip plan 02's
  `env`-output verification (step 4 of its real-app check: "no `NICE_PREFILL_COMMAND` in `env`
  output" — a stray `NICE_USER_ZDOTDIR` invites the same scrutiny).
- **Fix:** gate the legacy arm on `runtime.profile.kind() == ShellKind::Zsh` — it is a
  *zsh* quirk preserved for byte-freeze parity, so scope it to zsh. This also answers plan 01's
  OQ2: keep the quirk, zsh-only. Non-zsh profiles with `inject: None` emit nothing.

### I3. Design §2's `fn comm_name(&self) -> &'static str` is unimplementable for `FallbackProfile` — bless the relaxation now

- **Where:** design §2 trait sketch; plan 01 W2.3 + Open question 1.
- **The problem:** `FallbackProfile`'s comm is the basename of a runtime-resolved path
  (`/opt/homebrew/bin/fish` → `"fish"`), which cannot be `&'static str` without leaking. Plan
  01 caught it and recommends `fn comm_name(&self) -> &str`; plan 02 (which writes
  `comm_name() → "bash"`) and the reaper union code both work unchanged under the relaxation.
  Because the plans' rule is "do not silently deviate from Contract," this needs an explicit
  design-owner amendment **before** step 1 lands the trait, or the two implementers may resolve
  it differently (leak vs borrow vs `String`).
- **Fix:** amend design §2: `fn comm_name(&self) -> &str` (and note `display_name` gets the
  same treatment for the fallback's runtime basename — plan 04's `active_display_name` already
  returns `String` for this reason). Callers collect owned strings; zero impact elsewhere.

---

## Nits

### N1. Plan 02's `\e`-in-printf rationale is factually wrong (the prescription is still fine)

Dialect table row: *"`\e` is not a guaranteed escape in bash 3.2's printf; octal always is."*
Verified on this machine: `/bin/bash 3.2.57`'s builtin `printf '\e'` emits `0x1b` (bash's
printf has accepted `\e` for a long time). Keep the `\033`/`\007` spelling if you like
(POSIX-portable, consistent), but fix the comment/test rationale so a future reader doesn't
"correct" real code against a false premise. The structural test should pin the chosen
spelling, not the false claim.

### N2. Plan 04 misattributes the reaper stderr line to "plan 02"

Non-goals: *"The bootstrap stderr line … belongs to plan 02 (reaper comm-union, design §7)."*
The reaper comm-union and the de-zsh'd stderr line are plan **01** W2.5 (migration step 2).
Plan 02 only supplies `comm_name() == "bash"`. Fix the reference so the verification step
points at the right owner.

### N3. Plan 04's repo-level "no zsh literal" test will trip on `settings/scenario.rs`

The acceptance grep / proposed test covers `crates/nice/src/settings/`, but
`settings/scenario.rs:765` calls the zsh stub writer and stays zsh-pinned by design (plan 01
W2.7 re-points it to `crate::shell::zsh` — the literal moves, it doesn't vanish). A mechanical
assertion over the directory can't distinguish "user-facing". Scope it to the actual UI-string
surfaces (`shortcuts.rs` `label()`/`info()` output, the pane copy helpers, `README.md`) or
exclude `scenario.rs`.

### N4. Design §5 fallback: tcsh ignores `-l` when it isn't the sole flag

`[path, "-i", "-l", "-c", cmd]` is *accepted* by tcsh (no error), but tcsh's man page makes
`-l` effective only when it is the only argument — so the fallback's "login" spawn is a
non-login tcsh. Harmless in practice (tcsh reads `~/.tcshrc` for all shells, so PATH still
resolves), and fish honors `-l` fine. Worth one comment line in `fallback.rs` so nobody
files it as a bug later; no behavior change warranted.

### N5. "First OSC 7 ⇒ guaranteed post-rc" has a small hole (user profile emitters)

Plan 02's prefill delivery keys on the pane's first `CwdChanged`. The startup fire is the last
statement of `nice.bashrc`, but the login emulation sources the user's profile chain *first* —
profile code that itself emits OSC 7 (terminal-integration snippets) delivers the first
`CwdChanged` mid-rc. Verified receiver-side: the tee forwards **every** decoded OSC 7
(`session.rs:467-472`, no dedup), so delivery still happens; the prefill bytes just get typed
early and sit in the tty input queue until readline starts — which still lands them on the
prompt line unless the profile reads stdin. Consequence is benign in almost all cases; add one
sentence to the delivery-side comment and (optionally) an e2e leg with an OSC7-emitting fixture
profile pinning the queued-early case. (Note: Apple's `/etc/bashrc_Apple_Terminal` emitter is
gated on `TERM_PROGRAM=Apple_Terminal` and won't fire under Nice.)

### N6. Plan 03's `_nice_compose_last` never resets

The idempotence guard persists across prompts: a *later, freshly typed* line that happens to
equal an old composed result silently no-ops instead of composing. Observable difference is
~zero (the instruction returns valid commands unchanged, so the compose would be an expensive
no-op), and the guard correctly absorbs the queued-double-trigger replay. Keep it, but say in
the comment that the staleness is deliberate and why it's safe — or clear the var inside the
handler when the compose result is rejected/errored so the error path can be retried without
editing the line (currently an error path doesn't set it, so retry works; only the
success-equal case is affected).

---

## Seam map — who owns each cross-plan open question

| Seam | Owner | Status |
|---|---|---|
| Bash version probe (major.minor, resolve-time) | **Plan 03** (work item 1). Plan 02 ships `compose_support()` hard-`None`, no probe — its OQ "flag rather than choose" is answered: leave it to 03; 03's boundary assumption ("extend rather than duplicate") already matches. | Consistent as written |
| `all_known_comm_names()` gains `"bash"` | **Plan 02** (with the `resolve()` bash arm) | **B2 — currently unowned; must be assigned** |
| `build_claude_extra_env` generalization (inject-pairs param + `PrefillStrategy` switch) | **Plan 01** lands the shape (W1.5 + W2.7, `AppTyped` arm intentionally inert); **plan 02** item 4 makes `AppTyped` real (pending-prefill recording). Plan 02's "reconcile against as-built" hedge is the right posture. | Consistent |
| `advanced.shell` persistence | **Plan 01** W2.2: field + `shell_setting()` read (no setter); **plan 04** W1: `set_shell` setter + UI. Plan 04's "only if plan 01 didn't" guard resolves the overlap. | Consistent — record it |
| `install_shell_runtime` factoring + mutable `ShellRuntime` | **Plan 01** provides the factored installer; **plan 04** consumes it (fan-out + guarded probe stay in 04); **design §4 amended** | **B1 — must be decided first** |
| `comm_name` signature relaxation to `&str` | **Design owner** amends §2 now; plan 01 implements | **I3** |
| Hermetic-bash test helper (scratch `$HOME`, `--norc --noprofile`) | **Plan 02** (design §10 assigns step 3); plan 03 reuses (its work item 4 already says so) | Consistent |
| "bash ≥ 4.3" user-facing copy | **Plan 04** (per plan 03's non-goals) — with the **4.3** number | **I1** |
| Compose trigger constants relocation | **Plan 01** W1.1 moves them to `shell/mod.rs`; plan 03 consumes | Consistent |
| Degraded-env legacy quirk (`inject: None`) | **Plan 01**, gated zsh-only | **I2** |

## Inventory coverage — F1–F12

| Finding | Resolved by | Note |
|---|---|---|
| F1 spawn hardcode | 01 (routing) + 02 (BashProfile shapes) | ✓ |
| F2 injection channel | 01 (InjectPaths/inject_env) + 02 (`--rcfile` + login emulation) | ✓ |
| F3 `claude()` shadow | 02 (bash port; dialect table verified) | ✓ |
| F4 compose widget | 03 (`bind -x`, ≥ 4.3) | ✓ |
| F5 instruction dialect | 03 (in-script "bash" word + structural pin) | ✓ |
| F6 trigger bytes | 01 (PaneShell + `compose_route` gate) + 03 (version-gate half, double-gated in Rust and rc) | ✓ |
| F7 OSC 7 emitter | 02 (`PROMPT_COMMAND` + dedup + startup fire) | ✓ |
| F8 prefill | 01 (strategy gating) + 02 (app-typed delivery on first `CwdChanged`) | ✓ (N5 edge) |
| F9 discovery | 01 (probe routing) + 02 (`-ilc` bash probe) | ✓ |
| F10 reaper | 01 (union mechanics) + 02 (comm) | **gap: B2** |
| F11 help copy | 04 (W5/W6) | ✓ (I1 wording) |
| F12 README | 04 (W7) | ✓ |

No finding is dropped between the seams other than the F10 registry entry (B2).

---

## Appendix — claims verified against code and live bash (all correct unless noted)

**Code citations spot-verified to the line** (every one matched the worktree): `spawn.rs:10/90/101`
(`ZSH_PATH`, `build_exec_args`, `build_argv`), `pty.rs:191-192` (execve choke point),
`pty_manager.rs:198` (`WindowPty`), `:220` (`WindowShellEnv` fields), `:1300-1321`
(env pairs incl. always-set `NICE_USER_ZDOTDIR`), `:1611-1660` (the three Claude spawn arms at
the cited lines 1636/1656/1659), `:1742`, `:1947-1985` (`build_claude_extra_env` — ZDOTDIR/
prefill only in the ResumeDeferred arm, exactly as plan 01 W1.8 claims), `:2000-2005` (frozen
prefill composer), `route_terminal_event`/`CwdChanged` at `:526/:539` (no `cx`, `RoutedExit`
return — plan 02's delivery-side latitude is well-founded), `app.rs:1356/1390/1433/1446/1456/
1490/1508/1649-1663` (bootstrap order, unconditional probe-global set, `arm_window_control_socket`,
`set_window_shell_env` exists), `:1155` store-before-`:1186`-bootstrap ordering (plan 01 W2.1's
precondition holds), `orphan_reaper.rs:177/186` (comm filter + `comm_name`),
`window_state.rs:2366/2386-2416` (`dispatch_command_compose` + `compose_route` truth table),
`prefs_store.rs:42-46` (`AdvancedSection` = `smooth_scroll` only), `advanced_pane.rs` +
`root.rs:570` (pane takes `&mut App`; sibling panes take `Context` — plan 04 W3's alignment is
as small as claimed), `claude_pane.rs:61-90/137-141` (dropdown pattern, "zsh prompt" copy),
`shortcuts.rs:69-71/121-131` (`info()` is `&'static str` on a plain enum — plan 04's
static-not-dynamic call for `nice-model` is right), `shell_inject.rs` stub bodies (the
`${sid[1,8]}` / `print -u2` / `print -z` / `chpwd_functions` / `\%`-arcana / `${HOST}` sites the
dialect table ports), `write_stubs`/`write_atomic`/`default_location` (`:587/:600/:619`),
`exec_args.rs` (pins `/bin/zsh -il`/`-ilc` via `build_argv` — kept alive by plan 01 W1.3's
constructor-default approach), `compose_live.rs:45/:111`, session/deferred `write_input`
(`session.rs:226`, `deferred.rs:443`).

**Shell semantics executed on this machine** (`/bin/bash 3.2.57(1)-release`):

- `bash --rcfile rc -i -c 'cmd'` **sources the rcfile** before running `-c` — the pin the whole
  injected-command-pane shape rests on. ✓
- `bash --rcfile rc -l -i -c` does **not** source the rcfile (login ignores `--rcfile`) — the
  premise for non-login spawn + in-script login emulation. ✓
- `bash -il -c 'cmd'` with scratch `$HOME` reads the profile chain. ✓
- `exec command echo hi` execs `/usr/bin/command` — a real 120-byte external trampoline exists
  on macOS, so the plan-02 claim (zsh's `exec command claude` spelling would leave an
  intermediate `sh` owning the pty; plain `exec claude` is right for bash since `exec` never
  resolves functions) is confirmed. ✓
- 3.2 compatibility of every construct in the sketches: `${s//$'\n'/\\n}`, two-line
  `local -a pre` + `pre=()`, `(( ${#pre[@]} ))`, herestrings, the `declare -p PROMPT_COMMAND`
  array sniff with `+=(…)` in the (dead-on-3.2) array arm, the nested-expansion whitespace
  trims, `$((16#…))` hex, the `[0-9a-fA-F]…` class match, `$(<file)` reads, the
  `BASH_VERSINFO` guard arithmetic (evaluates *closed* on 3.2). ✓ all
- The **full combined sketch** (plan 02 body + plan 03 compose section) passes `/bin/bash -n`
  under 3.2, sources cleanly with the guard closed (no `bind` warnings), fires exactly one
  startup OSC 7, encodes ` ` → `%20` and `%` → `%25` correctly, and its
  `_nice_compose_strip` / `_nice_compose_conf_get` behave as specified on 3.2. ✓
- Exception: the `\e`-not-guaranteed-in-3.2-printf rationale is **false** (N1) — `printf '\e'`
  emits `0x1b` on Apple's 3.2.57.

**Receiver-side prefill premise:** the OSC 7 tee forwards every decoded report
(`session.rs:456-473` — `Osc7Scanner::feed` → `SessionEvent::CwdChanged`, no change-dedup), so
the rc's unconditional startup fire always produces the `CwdChanged` plan 02 keys delivery on,
even when the reported cwd equals the spawn cwd. ✓

**Not empirically verified here** (documented behavior, correctly pinned by planned e2e):
multi-char `bind -x` requiring ≥ 4.3 (well-documented; the double gate — Rust probe + in-script
guard — is the right defense either way), and the redisplay-after-`bind -x` choreography
(plan 03 OQ2 already carries the pinned-e2e + never-garbled fallback).

**YAGNI check:** nothing in the four plans builds beyond a finding or a design requirement.
Plan 04's fan-out (W4.4) is the one candidate, and it is what makes the tooltip's promise
literally true; its own documented fallback ("new windows" copy) is the correct escape hatch if
plan 01's as-built shape makes it large.
