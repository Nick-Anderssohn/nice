# Bash support 02 — `BashProfile` core + prefill/discovery

Implements **migration steps 3 and 4** of `docs/design/shell-abstraction.md` (§9): the
`BashProfile`, its generated `nice.bashrc`, the `claude()` shadow and OSC 7 emitter ported to
POSIX-bash, app-typed deferred-resume prefill, and `bash -ilc` claude discovery. Covers
inventory findings 1, 2, 3, 7 (bash legs) plus 8 and 9.

**Compose is NOT in this plan.** Migration step 5 (the `bind -x` handler, the bash ≥ 4 version
probe, the dialect instruction line) is plan 03. Everything in THIS plan must run on
**bash 3.2** — macOS's `/bin/bash 3.2.57` is the unconditional baseline and the e2e target.

## Dependency: plan 01 must have landed

This plan assumes migration steps 1–2 (plan 01, framework + zsh extraction + resolution) are
on the branch already:

- `crates/nice/src/shell/` exists with the §2 contract: `ShellProfile`, `ShellKind`,
  `SpawnCtx`, `InjectPaths`, `UserShellEnv`, `ComposeSupport`, `PrefillStrategy`, `PaneShell`,
  `all_known_comm_names()` (plan 01 ships it as `["zsh"]` — **this plan flips it to
  `["zsh", "bash"]`**, work item 3), `resolve()` + `ShellSetting`, and the `ShellRuntime` gpui
  global installed by `install_shell_runtime` / `install_shell_inject_bootstrap`.
- Spawn argv, inject env, the rc writer, and the `run_which_claude` probe argv are already
  routed through the active profile. `ZshProfile` is byte-frozen; `FallbackProfile` exists.
- Panes snapshot `PaneShell` at spawn; `compose_route` gates on `ComposeSupport` (finding 6
  is already fixed for every non-zsh shell — bash panes in this plan ride that gate).
- The reaper prefilter already matches the registry union (`all_known_comm_names()` ∪ the active
  profile's `comm_name()`) — but the registry itself is still `["zsh"]`, so this plan owns adding
  `"bash"` to it as well as supplying the profile whose `comm_name()` agrees.

Before `resolve()` grows its bash arm here, a `$SHELL=/bin/bash` user gets `FallbackProfile`.
After this plan they get the real thing. Plan against the trait exactly as the design doc §2
defines it; where plan 01's as-built code differs in incidentals (helper names, file split),
follow the as-built code and keep the contract.

## Goal

A bash user (login shell `/bin/bash` or a homebrew bash, via the §4 resolution chain or
`NICE_SHELL`) gets:

1. Real bash panes running their own configuration — login chain honored, PATH correct
   (finding 1/2 for bash).
2. The `claude()` shadow/handshake working byte-compatibly against the existing control-socket
   protocol (finding 3).
3. OSC 7 cwd tracking via a change-deduped `PROMPT_COMMAND` hook (finding 7).
4. Deferred-resume panes with `claude --resume <sid>` pre-typed — app-typed on the pane's
   first OSC 7, since bash has no `print -z` (finding 8).
5. Claude binary discovery through a login-interactive **bash** probe, so a PATH set in
   `~/.bash_profile` / a profile-sourced `~/.bashrc` resolves `claude` (finding 9).

## Non-goals

- Command Compose for bash (plan 03: version probe, `bind -x`, instruction line). In this plan
  `BashProfile::compose_support()` is unconditionally `ComposeSupport::None` and `nice.bashrc`
  contains **no** compose section — the pane snapshot keeps trigger bytes away.
- The bash ≥ 4 version probe. It exists only to gate compose; deferring it keeps bootstrap
  untouched. `BashProfile` is a struct, so plan 03 adds the field without churn (noted in
  Open questions).
- Settings ▸ Advanced shell UI, tooltip/help copy, README wording (step 6).
- Byte-pinning `nice.bashrc`. Per §10 the bash script gets structural assertions now and a
  byte-pin only after step 5 freezes the contract.
- Anything fish/fallback — landed in plan 01/02.

## Dialect notes — where the zsh script does NOT translate 1:1

These are the load-bearing differences an implementer must not "simplify away". Each has a
structural test (see Test plan).

| zsh (frozen stub) | bash 3.2 port | Why not 1:1 |
|---|---|---|
| `exec command claude …` | `exec claude …` | bash's `exec` PATH-searches its first word as an external executable. `command` is not a precommand modifier after `exec` in bash — `exec command claude` would exec `/usr/bin/command` (the macOS shim), leaving an intermediate `sh` owning the pty. bash `exec` never resolves shell functions, so plain `exec claude` already bypasses the shadow. Non-exec child invocations keep `command claude "$@"` (builtin usage — fine). |
| `print -u2 "msg"` | `printf '%s\n' "msg" >&2` | No `print` builtin. `printf` (not `echo`) so a hostile `$response` in the error arm can't be backslash-interpreted under `xpg_echo`. |
| `print -z "$NICE_PREFILL_COMMAND"` | **nothing** — app-typed (§6.4) | No editor-buffer push exists. The rc file must NOT reference `NICE_PREFILL_COMMAND` at all; Nice writes the line into the pty on the first OSC 7. |
| `${sid[1,8]}` | `${sid:0:8}` | zsh subscript syntax expands to the whole string in bash — this was inventory finding 3's concrete bug. |
| `local -a pre=()` … `(( ${#pre} ))` | `local -a pre` then `pre=()` … `(( ${#pre[@]} ))` | `${#arr}` in bash is the length of element 0, not the array. Split the declaration from the `=()` assignment — `local name=(...)` initialization has historical quirks on 3.2; the two-line form is unambiguous. |
| `chpwd_functions+=(_nice_emit_cwd_osc7)` | `PROMPT_COMMAND` wrapper with `$PWD` change-dedup | No `chpwd` hook. `PROMPT_COMMAND` fires before **every** prompt, so the wrapper keeps `_nice_last_osc7_pwd` and emits only on change. Semantic delta, accepted: OSC 7 lands when the next prompt paints, not at `cd` time (a `cd x && sleep 100` reports x only after the sleep). |
| `${PWD//\%/%25}` (the `\%` arcana) | `${PWD//%/%25}` (bare `%`) | In zsh a bare `%` in a parameter pattern is the end-of-string anchor; the backslash is load-bearing there. In bash `%` has no special meaning in this pattern position — bare `%` is the correct spelling, and the mirror-image structural test pins the bare form. |
| `printf '\e]7;…\a'` | `printf '\033]7;…\007'` | Style choice, not a capability gap: bash 3.2's builtin `printf` **does** accept `\e` (verified on `/bin/bash 3.2.57`). Octal `\033`/`\007` is chosen because it is POSIX-portable and spelled consistently for both bytes. Do not justify it as "`\e` doesn't work on 3.2" — that is false, and a future reader would "correct" the code against it. |
| `"${HOST}"` | `"${HOSTNAME}"` | zsh sets `HOST`; bash sets `HOSTNAME` (self-set even under `env_clear` spawns, so tests need no seeding). |
| `emulate -L zsh`, `typeset -g` | dropped / plain assignments | No equivalents and no need — the file is bash-only. |

Kept as-is because they are POSIX/bash-portable: `_nice_json_escape`'s `${s//…}` substitution
set (incl. `$'\n'` patterns — valid on 3.2), `read -r mode sid settings <<< "$response"`
(herestrings since bash 2.05b), `[[ -t 0 ]]`, `case`/`for`, `args_json+=…` string append and
`arr+=(…)` array append (both bash 3.1+), `nc -U "$NICE_SOCKET" -w 2`.

### `$0` / argv / comm subtleties (`--rcfile` vs `-l`)

- **argv[0] is the full resolved bash path in all four spawn shapes** (e.g. `/bin/bash`,
  `/opt/homebrew/bin/bash`) — matching the zsh profile's convention where login-ness comes
  from the `-l` flag, never from a leading-dash argv[0].
- Under injected `--rcfile … -i` spawns the shell is **not** a login shell:
  `shopt -q login_shell` is false, `logout` is unavailable, and `$0` is a dash-less bash path,
  so user scripts branching on either take their non-login path. This is the design doc's
  documented limitation (§6.2) — same class as zsh's "`exec zsh` drops the injection". Do not
  fix; document in the module header. `exec bash` inside a pane likewise drops the injection.
- Non-injected spawns are genuine login shells via the `-l` flag (`-il`); bash treats `-l` and
  dash-argv[0] identically for startup-file selection.
- **Reaper comm name: `"bash"`, unconditionally.** macOS sets `p_comm` from the exec **path's
  basename** (truncated to MAXCOMLEN), not argv[0] — so `/bin/bash` and
  `/opt/homebrew/bin/bash` both report comm `bash`, identical under `--rcfile` and `-il`
  spawns, and no dash prefix can appear since Nice never sets one. `BashProfile::comm_name()`
  returns `"bash"`, agreeing with the `all_known_comm_names()` entry this plan adds to the
  registry (work item 3).

## Work items (ordered)

### 1. `BashProfile` — `crates/nice/src/shell/bash.rs` (new)

`pub struct BashProfile { path: String }` — `path` is the resolved binary kept as resolved
(§4: homebrew bash gets the profile *at that path*). Trait impl, exactly per §6.2/§6.5:

- `kind()` → `ShellKind::Bash`; `program()` → `&self.path`.
- `spawn_argv(ctx)` — the four shapes (argv[0] included):

  | `ctx.inject` | `ctx.command` | argv |
  |---|---|---|
  | `Some(p)` | `None` | `[path, "--rcfile", p.rcfile, "-i"]` |
  | `Some(p)` | `Some(cmd)` | `[path, "--rcfile", p.rcfile, "-i", "-c", "exec <cmd>"]` |
  | `None` | `None` | `[path, "-il"]` |
  | `None` | `Some(cmd)` | `[path, "-il", "-c", "exec <cmd>"]` |

  `p.rcfile` is `InjectPaths.rcfile.expect(…)` — `write_rc_files` for bash always returns
  `Some`. The `exec <cmd>` wrapping keeps the command-owns-the-pty contract; the command
  string is spliced verbatim (no tilde expansion), matching the zsh shapes.
- `inject_env(…)` → **empty Vec**. Bash injection rides argv; there is no `ZDOTDIR` analogue
  and no `NICE_USER_ZDOTDIR`. The generic pairs (`NICE_SOCKET` / `NICE_TAB_ID` /
  `NICE_PANE_ID` / `NICE_COMPOSE_CONF`) stay shell-agnostic in `pty_manager` (§2) — bash
  panes receive them unchanged; `NICE_COMPOSE_CONF` is simply unread until plan 03.
- `write_rc_files(dir)` → write `nice.bashrc` (from
  `include_str!("scripts/bash/nice.bashrc")`) into `dir` with the same atomic
  temp-sibling + rename and overwrite-always self-heal as the zsh writer (reuse plan 01's
  shared `write_atomic`); return `InjectPaths { dir, rcfile: Some(dir.join("nice.bashrc")) }`.
  Destination is the active-profile shellrc dir from plan 01 (`…/<CFBundleName>/shellrc/bash/`).
- `compose_support()` → `ComposeSupport::None` (this plan; plan 03 flips behind the probe).
- `prefill()` → `PrefillStrategy::AppTyped`.
- `probe_argv(cmd)` → `[path, "-ilc", cmd]` (clustered spelling per §6.5 — bash accepts it).
- `comm_name()` → `"bash"`; `display_name()` → `"bash"`.

### 2. `crates/nice/src/shell/scripts/bash/nice.bashrc` (new) — full sketch below

Single file (no four-file dance — nothing needs un-redirecting). Ordering rule is the same as
zsh: user config first, Nice hooks after so they win. The startup OSC 7 fire is the **final
statement of the file** — it is the app-typed-prefill readiness signal and must stay
post-everything; plan 03 inserts the compose section *above* it.

### 3. `resolve()` bash arm + registry flip — `crates/nice/src/shell/resolve.rs` / `shell/mod.rs`

Basename `bash` ⇒ `BashProfile { path }`. Until now it fell to `FallbackProfile`. No other
resolution change — the §4 chain, `NICE_SHELL`, setting plumbing, and existence checks all
landed in plan 01.

**Flip the registry in the same item** (`review-fable.md` B2 — plan 01 deliberately left this
to plan 02): `all_known_comm_names()` returns `&["zsh", "bash"]`, not `&["zsh"]`. This is what
makes the reaper catch bash orphans from a **prior** run when the current run resolved zsh —
the exact cross-run case design §7's registry union exists for. One line of code, one test row
(Test plan → Unit).

### 4. App-typed prefill (finding 8, §6.4) — `crates/nice/src/pty_manager.rs` + the event-subscription caller

Contract: `build_claude_prefill_command` (pty_manager.rs:2000) stays the single composer —
frozen wire string `claude[ --settings '<path>'] --resume <sid>`, untouched. What changes is
delivery for `PrefillStrategy::AppTyped`:

- **Spawn side** (`spawn_claude_window`, pty_manager.rs:1600, `ResumeDeferred` arm — also
  covers the `ensure_active_window_spawned` L3 restore arm, which flows through it): branch on
  the active profile's `prefill()`:
  - `ShellSide` (zsh): unchanged — `NICE_PREFILL_COMMAND` env + the profile's inject pairs,
    byte-frozen.
  - `AppTyped` (bash): do **not** set `NICE_PREFILL_COMMAND` (and no zsh env legs — bash's
    `inject_env` is empty; the pane still spawns injected, i.e. `SpawnCtx.inject = Some`, so
    the rcfile runs). After a successful spawn, record
    `pending_prefill: Some(build_claude_prefill_command(settings_path, sid))` on the pane's
    `WindowPty` entry (pty_manager.rs:198 — add the `Option<String>` field, `None` everywhere
    else). Runtime-only; never persisted (`PaneShell` rule, §2).
  - `Off` (fallback): never record; pane opens at a bare prompt.
- **Delivery side**: the pane's **first** `TerminalEvent::CwdChanged` (the rc file's
  unconditional startup fire — guaranteed post-rc). Hook the `CwdChanged` arm of
  `route_terminal_event` (pty_manager.rs:540): `take()` the window's `pending_prefill` (at
  most once, by construction of `take`). The routing fn has no `cx`, so hand the taken string
  back to the live subscription that called it — extend the routing outcome (the `RoutedExit`
  pattern) or a sibling return; *implementation latitude*. The subscription owns `cx` and the
  entity, and writes the bytes via the session's `write_input`
  (`nice-term-core/src/deferred.rs:443` → `session.rs:226`). **No trailing newline, ever** —
  the line must sit editable at the prompt; only the user's Enter runs it.
- Edge cases (behavior, pinned by tests): pane exits before OSC 7 ⇒ the slot dies with the
  `WindowPty` entry (nothing to do); a respawn re-records; command panes emit a startup OSC 7
  too but never have a pending slot; subsequent OSC 7s are plain cwd updates. The
  cwd-tracking side of `CwdChanged` (`window_cwd_changed`) is untouched.
- If plan 01's as-built `build_claude_extra_env` still hardcodes the `ResumeDeferred` zsh legs
  (`ZDOTDIR`/`NICE_USER_ZDOTDIR`/`NICE_PREFILL_COMMAND`) rather than composing profile
  `inject_env` + a strategy switch, generalize it here — the zsh **output pairs** must stay
  byte-identical (the existing env-matrix tests keep pinning them).

### 5. Discovery probe (finding 9, §6.5)

Plan 01 already routes `run_which_claude` (app.rs:1456) through
`ShellRuntime.profile.probe_argv("command -v -- claude")`. This plan's work is only item 1's
`probe_argv` impl: `[bash, "-ilc", "command -v -- claude"]` — a login-interactive bash reads
`/etc/profile` + the user's profile chain, which is where a bash user's PATH lives (directly
or via a profile-sourced `~/.bashrc`). Everything else (async probe, exit-0 +
absolute-path validation, `NICE_CLAUDE_OVERRIDE` seam) is untouched. Verify the routing
landed; if plan 01 somehow left the `/bin/zsh` literal, fix it here. Known consistent
limitation: a PATH set *only* in a never-sourced `~/.bashrc` is invisible to the probe — and
to the pane itself (same login emulation), so discovery and panes agree.

### 6. Hermetic-bash test helper (§10)

The zsh tests blank `ZDOTDIR`; bash reads `$HOME` paths instead. Two sanctioned tricks, both
used in this plan's tests (*implementation latitude* on packaging):

- **Scratch `$HOME`**: `SpawnSpec` env pairs override `base_env`'s `HOME` pass-through
  (`spawn.rs upsert`), so pointing `HOME` at a scratch dir makes the login emulation source
  only fixture files. `/etc/profile` still runs (absolute path) — harmless as long as tests
  don't depend on inherited PATH ordering (see the path_helper note in the Test plan).
- **`--norc --noprofile` argv** for fully-quiet spawns that don't want `nice.bashrc` either
  (a test-only helper, not a production `SpawnCtx` axis).

Existing zsh fixtures (`pty.rs:557`, `nice-itests/src/session.rs`, scenario `ZDOTDIR`
blanking) keep working untouched; `nice-term-core`'s `SpawnSpec::shell/command` convenience
constructors keep their zsh-shaped default (§8).

## The generated `nice.bashrc` — full sketch

Implementers: this is the intended content, modulo comment wording. Structural tests (below)
pin the marked properties; the byte-pin waits for plan 03.

```bash
# Nice: injected bash rc, read via `bash --rcfile` (see shell/bash.rs).
#
# Nice spawns bash NON-login — bash ignores --rcfile for login shells — so
# this file first emulates bash's documented login sequence, then layers
# Nice's hooks on top (defined AFTER user config so they win; a user can
# still `unset -f claude` to opt out).
#
# Known limitation (documented, not fixed): under --rcfile this is not a
# login shell — `shopt -q login_shell` is false and `logout` is unavailable;
# profile code branching on it takes its non-login path. `exec bash` inside
# a pane drops the injection, exactly like `exec zsh` does on the zsh side.
#
# Baseline dialect: bash 3.2 (macOS /bin/bash). Nothing here may use >= 4
# features: no associative arrays, no ${var^^}, no ;&.

# --- Login emulation ---------------------------------------------------------
# A real login bash reads /etc/profile, then the FIRST existing of
# ~/.bash_profile, ~/.bash_login, ~/.profile. The user's profile
# conventionally sources ~/.bashrc itself; we deliberately do NOT source
# ~/.bashrc on top (double-source risk). A PATH living only in an unsourced
# ~/.bashrc is equally absent from a real login bash — their convention,
# honored.
if [ -f /etc/profile ]; then
    . /etc/profile
fi
if [ -f "$HOME/.bash_profile" ]; then
    . "$HOME/.bash_profile"
elif [ -f "$HOME/.bash_login" ]; then
    . "$HOME/.bash_login"
elif [ -f "$HOME/.profile" ]; then
    . "$HOME/.profile"
fi

# --- Nice hooks --------------------------------------------------------------

_nice_json_escape() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    printf '"%s"' "$s"
}

# Tell Nice the Claude we ran as a CHILD has returned (attach verb only —
# every other verb execs). Fire-and-forget; same wire shape as the zsh stub.
_nice_claude_exited() {
    [[ -z "$NICE_SOCKET" ]] && return 0
    local pane_id_json
    pane_id_json=$(_nice_json_escape "${NICE_PANE_ID:-}")
    printf '%s\n' "{\"action\":\"claude_exited\",\"paneId\":${pane_id_json}}" \
        | nc -U "$NICE_SOCKET" -w 2 >/dev/null 2>&1
    return 0
}

claude() {
    # Passthrough (no handshake): outside a Nice pty, piped stdin,
    # non-interactive flags, non-interactive subcommands.
    if [[ -z "$NICE_SOCKET" ]]; then
        command claude "$@"
        return
    fi
    if [[ ! -t 0 ]]; then
        command claude "$@"
        return
    fi
    local a
    for a in "$@"; do
        case "$a" in
            -p|--print|-h|--help|--version|--output-format|--output-format=*)
                command claude "$@"
                return
                ;;
        esac
    done
    case "${1-}" in
        mcp|config|migrate-installer|update|doctor)
            command claude "$@"
            return
            ;;
    esac

    local args_json="["
    local first=1
    for a in "$@"; do
        [[ $first -eq 1 ]] || args_json+=","
        args_json+=$(_nice_json_escape "$a")
        first=0
    done
    args_json+="]"

    # {cwd, args, tabId, paneId} — byte-identical payload shape to the zsh
    # stub; the socket server is dialect-agnostic.
    local cwd_json session_id_json window_id_json
    cwd_json=$(_nice_json_escape "$PWD")
    session_id_json=$(_nice_json_escape "${NICE_TAB_ID:-}")
    window_id_json=$(_nice_json_escape "${NICE_PANE_ID:-}")
    local payload="{\"action\":\"claude\",\"cwd\":${cwd_json},\"args\":${args_json},\"tabId\":${session_id_json},\"paneId\":${window_id_json}}"

    local response
    response=$(printf '%s\n' "$payload" | nc -U "$NICE_SOCKET" -w 2 2>/dev/null)
    if [[ -z "$response" ]]; then
        printf '%s\n' "nice: control socket unreachable; running claude directly" >&2
        # bash exec PATH-searches an external binary and never resolves
        # functions — plain `exec claude` already bypasses this shadow.
        # (`exec command claude` would exec the /usr/bin/command shim.)
        exec claude "$@"
    fi

    # Reply grammar (unchanged): newtab | inplace [sid|-] [settings]
    #                          | attach <uuid> [settings] | resume <uuid> [settings]
    local mode sid settings
    read -r mode sid settings <<< "$response"
    case "$mode" in
        newtab)
            return 0
            ;;
        inplace)
            local -a pre
            pre=()
            [[ -n "$settings" ]] && pre+=(--settings "$settings")
            [[ -n "$sid" && "$sid" != "-" ]] && pre+=(--session-id "$sid")
            if (( ${#pre[@]} )); then
                exec claude "${pre[@]}" "$@"
            else
                exec claude "$@"
            fi
            ;;
        attach)
            # attach runs as a CHILD; a dead jobs entry degrades to --resume
            # instead of stranding the user (same contract as the zsh stub).
            local -a post
            post=(--resume "$sid")
            [[ -n "$settings" ]] && post=(--settings "$settings" "${post[@]}")
            if command claude attach "${sid:0:8}"; then
                _nice_claude_exited
                return 0
            fi
            exec claude "${post[@]}"
            ;;
        resume)
            local -a post
            post=(--resume "$sid")
            [[ -n "$settings" ]] && post=(--settings "$settings" "${post[@]}")
            exec claude "${post[@]}"
            ;;
        *)
            printf '%s\n' "nice: unexpected response '$response'; running claude directly" >&2
            exec claude "$@"
            ;;
    esac
}

# --- OSC 7 cwd reporting -----------------------------------------------------
_nice_emit_cwd_osc7() {
    # Minimal URL encoding: % first (so the %20 below isn't double-encoded),
    # then space. Bare `%` is a literal in bash patterns — the zsh stub's
    # `\%` escape is zsh-only arcana and does not carry over.
    local p=${PWD//%/%25}
    p=${p// /%20}
    # Octal \033 / \007 for POSIX portability and a consistent spelling of both
    # bytes. (bash 3.2's printf does accept \e — this is a style choice, not a
    # 3.2 workaround.)
    printf '\033]7;file://%s%s\007' "${HOSTNAME}" "$p"
}

# bash has no chpwd hook; PROMPT_COMMAND fires before EVERY prompt, so dedup
# on $PWD and emit only when the cwd actually changed.
_nice_last_osc7_pwd=
_nice_osc7_prompt_command() {
    if [[ "$PWD" != "$_nice_last_osc7_pwd" ]]; then
        _nice_last_osc7_pwd=$PWD
        _nice_emit_cwd_osc7
    fi
}
# Cooperative append: keep whatever the user's profile registered.
# bash >= 5.1 may hold PROMPT_COMMAND as an array; 3.2 is always a string.
case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    "declare -a"*)
        PROMPT_COMMAND+=(_nice_osc7_prompt_command)
        ;;
    *)
        PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }_nice_osc7_prompt_command"
        ;;
esac

# Fire once at startup so the initial cwd is reported even if the user never
# cd's. This is also the readiness signal Nice's app-typed prefill waits for
# — it MUST stay the final statement of this file (plan 03's compose section
# is inserted above it).
_nice_last_osc7_pwd=$PWD
_nice_emit_cwd_osc7
```

Deliberately absent, with structural tests asserting the absence: any `NICE_PREFILL_COMMAND`
reference, any compose code, `print -z`, `print -u2`, `emulate`, `chpwd_functions`, `${…[1,8]}`
subscripting, `exec command`.

## Test plan

Follows §10's split. Fix rounds run targeted tests only.

### Unit (pure, no pty)

In `shell/bash.rs` tests, mirroring the zsh/exec-args table style:

- `spawn_argv` over the full `SpawnCtx` grid — the four-row table above, including verbatim
  `exec <cmd>` splicing and a homebrew-path profile keeping its own path in argv[0].
- `probe_argv` → `[path, "-ilc", cmd]`; `inject_env` → empty; `kind`/`comm_name`/
  `display_name`/`prefill`/`compose_support` values.
- `write_rc_files` into a tempdir: file set (`nice.bashrc` only), `InjectPaths.rcfile` is
  `Some`, overwrite-self-heal (delete then rewrite).
- `resolve()`: basename `bash` (at `/bin/bash` and `/opt/homebrew/bin/bash` fixture paths) ⇒
  `BashProfile` at that path — no longer `FallbackProfile`.
- Registry / reaper comm-union: `all_known_comm_names()` contains both `"zsh"` and `"bash"`;
  extend plan 01's `comm_accepted` union test with the row that matters —
  **registry covers `bash` while the ACTIVE profile is zsh** (union = registry ∪
  `ZshProfile::comm_name()` still accepts a `bash` comm), which is the cross-run orphan case.
- Prefill plumbing (pty_manager tests): `AppTyped` + `ResumeDeferred` records
  `pending_prefill` and omits `NICE_PREFILL_COMMAND` from the env; `ShellSide` keeps the
  frozen env (existing matrix tests unchanged); `Off` records nothing; the `CwdChanged`
  routing takes the slot exactly once (second event ⇒ `None`) and normal cwd routing still
  runs; the taken string has no trailing newline.

### Script structural tests (the pre-byte-pin net, §10)

- `/bin/bash -n <nice.bashrc>` exits 0 — a real 3.2 syntax gate, cheap and unconditional.
- Positive: sources `/etc/profile` then the `bash_profile → bash_login → profile` first-match
  chain in documented order (assert order of appearance); defines `claude()`,
  `_nice_claude_exited`, `_nice_json_escape`, `_nice_emit_cwd_osc7`,
  `_nice_osc7_prompt_command` after the login block; `${sid:0:8}`; `${PWD//%/%25}` bare-`%`
  spelling on the substitution line (and NOT the `\%` form); octal `\033]7;file://` +
  `\007` (pinning the chosen spelling only — do **not** assert anything about `\e` being
  unsupported on 3.2; it is supported); both `PROMPT_COMMAND` append arms; the startup fire as the file's final statement;
  the `NICE_SOCKET`-unset bypass.
- Negative (the dialect table): no `print -z`, no `print -u2`, no `NICE_PREFILL_COMMAND`, no
  `emulate`, no `chpwd_functions`, no `exec command`, no `~/.bashrc` sourcing, no
  `bind -x` / `READLINE_LINE` / `5099` (compose is plan 03).

### Real-bash end-to-end (unconditional — `/bin/bash` 3.2 ships on every macOS)

Live beside the bash profile (the pattern of `shell_inject.rs`'s real-zsh suite; those zsh
tests moved in plan 01 and keep running unchanged as the regression net).

- **`-i -c` rcfile-sourcing pin** (design doc demands this test): `bash --rcfile <rc> -i -c
  'echo ran'` with a marker in the rc ⇒ marker present. The whole injected-command-pane shape
  rests on this bash behavior.
- **Login-emulation order**, scratch `$HOME` (`env_clear` + fixture files):
  all three profile files present ⇒ only `.bash_profile` runs; without it ⇒ `.bash_login`;
  without both ⇒ `.profile`; a `.bash_profile` that sources `.bashrc` ⇒ both run exactly once
  (the convention-honored leg, and proof we don't double-source).
- **`claude()` shadow e2e**: reuse the zpty harness shape (`run_claude_shadow_e2e`,
  shell_inject.rs:1157) with the inner shell swapped to
  `/bin/bash --rcfile <dir>/nice.bashrc -i` — the zsh zpty *driver* is test infrastructure
  and stays (always present on macOS). Fake `nc` + fake `claude` fixtures as today. Legs:
  handshake payload shape; `newtab`; `inplace` with/without settings/sid; `attach` success ⇒
  single exec + `claude_exited` payload; `attach` failure ⇒ `--resume` fallback exec, no
  `claude_exited`; `resume` replaces attach args; empty response ⇒ direct exec.
  **Load-bearing fixture detail:** our login emulation sources `/etc/profile`, whose
  `path_helper` RESETS `PATH` — a fixture PATH passed only via env is clobbered. The scratch
  `$HOME/.bash_profile` must `export PATH="<fixture bin>:$PATH"`, which simultaneously proves
  the login chain ran and restores the fakes to the front. (The zsh suite never hit this — its
  zpty spawns `-i` without the profile chain.)
- **OSC 7**: startup fire produces a clean `file://` payload for the spawn cwd (mirror
  `zshrc_emitter_produces_clean_osc7_at_runtime`, spawning `--rcfile <rc> -i -c exit`);
  encoding legs for a cwd containing a space (`%20`) and a `%` (`%25`); dedup — functions are
  callable from the `-c` string after the rcfile sourced, so
  `-c 'cd "$W2"; _nice_osc7_prompt_command; _nice_osc7_prompt_command; cd "$W3"; _nice_osc7_prompt_command'`
  emits exactly 3 OSC 7s total (startup + W2 + W3, the repeat suppressed).
- **Prefill type-in after first OSC 7** (integration): spawn a real deferred-resume-shaped
  bash pane through `PtyManager` (scratch-`HOME` hermetic helper, item 6; profile injected via
  plan 01's test seam / `NICE_SHELL`), wait for `CwdChanged`, assert the prefill bytes were
  written to the pty (visible in the grid / echoed) and that **nothing executed** (no newline
  reached the pty). Placement latitude: `pty_manager` tests with `TestAppContext` or
  `nice-itests`.

Compose e2e (bash ≥ 4 probe-and-skip) is plan 03. No new live GUI scenario in this slice
(§10 adds the bash smoke scenario in step 5); existing scenarios stay pinned via
`NICE_SHELL=/bin/zsh` from plan 01/02.

## Verification (real app)

Scratch-env `Nice Dev` launch per CLAUDE.md, with `NICE_SHELL=/bin/bash` added to the launch
env (and a scratch `$HOME/.bash_profile` exporting a marker + PATH):

1. New pane is bash: `echo $BASH_VERSION` works, profile marker present, `~/.local/bin` on
   PATH (probe + panes agree).
2. `claude` at the prompt opens a new sidebar session (newtab); `claude` in a fresh session
   window promotes in place; exiting Claude returns the prompt and the pane's running flag
   clears.
3. `cd` around ⇒ file browser root follows; new window inherits the cwd (OSC 7 live).
4. Quit, relaunch, open a restored Claude session ⇒ `claude --resume <sid>` sits **typed but
   not executed** at the bash prompt; Enter resumes.
5. ⌘↩ at a bash prompt does nothing visible (no `[5099~` garbage — the `ComposeSupport::None`
   snapshot gates it; pre-existing `ForwardCmdEnter`/`Noop` legs apply).
6. `kill -9` the Nice process, relaunch ⇒ bootstrap reaps the orphaned bash (stderr line),
   no zsh regression (unset `NICE_SHELL` ⇒ everything behaves exactly as today).

## Acceptance criteria

- `resolve()` maps a bash login shell to `BashProfile` at its resolved path; zsh and fallback
  behavior unchanged; all zsh frozen-contract tests untouched and green.
- All four `spawn_argv` shapes exactly as §6.2; injected spawns carry no bash-specific env;
  non-injected spawns are genuine `-il` login shells.
- `nice.bashrc` passes `/bin/bash -n`, the structural positive/negative sets, and every
  real-`/bin/bash`-3.2 e2e above — including the `-i -c` sourcing pin and the four handshake
  reply modes.
- Deferred-resume bash panes: no `NICE_PREFILL_COMMAND` in env; resume line app-typed exactly
  once on first OSC 7, no trailing newline; zsh deferred-resume path byte-identical to today.
- `run_which_claude` probes via `[bash, "-ilc", "command -v -- claude"]` under a bash profile
  and finds a claude whose PATH entry lives in `~/.bash_profile`.
- `BashProfile::comm_name() == "bash"` **and `all_known_comm_names() == ["zsh", "bash"]`**;
  the reaper comm-union test covers a bash argv fixture *and* the registry-covers-bash-while-
  the-active-profile-is-zsh row.
- `cargo test --workspace` green; no compose bytes can reach a bash pane (`PaneShell.compose
  == None` for every pane this plan spawns).

## Open questions

- **Version probe placement.** §3/§6.3 say `BashProfile` carries its probed major version from
  resolve time; since `compose_support()` is hard `None` in this slice, the probe is deferred
  to plan 03 (YAGNI — nothing here reads it). If plan 03's author prefers the probe landing
  here to keep bootstrap changes in one place, it is a ~ms synchronous addition to item 1;
  flagging rather than silently choosing for them.
- **`build_claude_extra_env` shape after plan 01.** The design doc implies the zsh
  `ResumeDeferred` env legs become profile-composed; if plan 01 shipped them still hardcoded,
  item 4 generalizes them here (zsh output pairs byte-frozen either way). Implementers should
  reconcile against plan 01's as-built code, not this plan's guess.
- **`PROMPT_COMMAND` array detection via `declare -p`.** The `case "$(declare -p …)"` sniff is
  the only 3.2-parseable way to honor the ≥ 5.1 array form; if a user's profile sets an exotic
  `declare -ax` combination the string arm still degrades safely (string append onto element
  0 is what plain bash users get today in other terminals). Accepted; noting for the reviewer.
