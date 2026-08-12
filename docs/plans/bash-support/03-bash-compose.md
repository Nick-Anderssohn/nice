# Bash Command Compose — plan 03 (shell-abstraction migration step 5)

Implements migration step 5 of `docs/design/shell-abstraction.md` (§6.3, §9): Command
Compose for bash panes. Covers inventory findings F4 (the compose implementation), F5
(the LLM prompt's dialect line), and F6 (never send trigger bytes to a shell that
didn't bind them — the version-gate half; the routing half landed with the framework).

## Goal

⌘↩ at an idle prompt in a bash ≥ 4.3 pane rewrites the typed plain-English line into a
real bash command via `claude -p`, exactly like the zsh feature: the composed command
REPLACES the line for review, nothing ever runs until the user's own Enter. On stock
macOS `/bin/bash` 3.2 the feature is cleanly absent: `ComposeSupport::None`, no trigger
bytes ever written, ⌘↩ behaves exactly as it did before the feature existed.

## Non-goals

- No async spinner parity with zsh. readline has no `zle -F` fd watchers and no
  `POSTDISPLAY`/`region_highlight`; the bash handler is synchronous by design (design
  doc §6.3, "UX degradation vs zsh, accepted"). The precise degraded UX is specified
  below — this plan does not attempt background jobs, `PROMPT_COMMAND` polling, or any
  other async emulation.
- No compose support for bash 4.0–4.2. See the version-gate decision below.
- No new UI for the unavailable case. When a pane's `PaneShell.compose` is `None`, ⌘↩
  takes the framework's existing `ForwardCmdEnter`/`Noop` legs — identical to an unbound
  ⌘↩ pre-feature. No toast, no status-bar hint (a nag firing on every ⌘↩ in every
  stock-bash pane would be worse than silence). The Settings ▸ Claude / tooltip copy
  naming the "bash ≥ 4.3" requirement belongs to migration step 6 (the copy + UI plan),
  not here.
- No zsh changes of any kind. The zsh script bodies stay byte-frozen.

## Dependencies

Assumes plans 01 and 02 of this series have landed:

- **Plan 01 (framework, migration steps 1–2):** `crates/nice/src/shell/` exists with
  the `ShellProfile` trait, `ComposeSupport`, `ShellRuntime`, the resolution chain, and
  — load-bearing for this plan — the per-pane `PaneShell` snapshot plus the
  `compose_route` gate: `window_state.rs`'s `compose_route` already takes a
  `ComposeSupport` parameter and returns `Trigger` only when it is
  `ComposeSupport::Trigger`. `COMPOSE_TRIGGER_SEQ` / `COMPOSE_TRIGGER_BINDKEY` moved to
  `shell/mod.rs`. This plan does NOT touch that routing; it only makes `BashProfile`
  report `Trigger` when the pane can honor it.
- **Plan 02 (bash core, migration steps 3–4):** `BashProfile` exists
  (`crates/nice/src/shell/bash.rs`) with the `--rcfile` spawn shapes and
  `scripts/bash/nice.bashrc` (login emulation, `claude()` shadow, OSC 7 emitter).
  `NICE_COMPOSE_CONF` already reaches bash panes — it is one of the generic
  shell-agnostic env pairs pty_manager sets for every injected spawn (design §2).

**Boundary assumption to reconcile at implementation time:** this plan assumes plan 02
landed `BashProfile::compose_support()` as a hardcoded `ComposeSupport::None` with no
version probe (compose is step 5's scope per the design doc). If plan 02 already added
a major-version probe, extend it to major.minor rather than adding a second probe.

## Background: what the zsh feature does, and what bash can't

The zsh implementation (currently `crates/nice/src/shell_inject.rs:348-579`; after
plan 01, `shell/scripts/zsh/zshrc.zsh`) works like this: Nice writes
`COMPOSE_TRIGGER_SEQ` (`ESC[5099~`) to the pty; a ZLE widget fires; it forks
`claude -p <instruction>` with the buffer on stdin via `exec {fd}< <(…)`, registers the
fd with `zle -F`, and returns immediately — the prompt stays live. A second fd runs a
0.1s ticker that repaints a pulsing accent-colored "✻ Composing… (ctrl-c cancels)"
line via `POSTDISPLAY` + `region_highlight`. A precmd hook (Enter/ctrl-c → new prompt)
bumps a generation counter so in-flight results are abandoned. On completion the
result is stripped (whitespace/fences/backticks) and lands via `BUFFER=`/`CURSOR=`.

readline has none of that: no fd watchers, no way to repaint a ghost line under the
prompt from outside a `bind -x` invocation, no hook that can deliver a result into the
line buffer later. What it does have: `bind -x` runs a shell command with
`READLINE_LINE`/`READLINE_POINT` exposed as writable variables, and redisplays the
edited line when the handler returns. That forces the synchronous shape.

### Version facts (verified; this is the exact gate)

- `READLINE_LINE`/`READLINE_POINT` writable from `bind -x` handlers: **bash 4.0**
  (bash 4.0 NEWS).
- `bind -x` with a key sequence **longer than two characters**: broken (silently never
  fires) until **bash 4.3** (bash 4.3 fixed multi-char `-x` bindings; widely documented,
  and the reason fzf's bash 3/4.2 paths avoid multi-char `-x` binds). Our trigger is 8
  bytes (`\e[5099~`).
- Stock macOS `/bin/bash` is 3.2.57 forever; homebrew bash is 5.x. There is no real
  macOS population on 4.0–4.2.

So the gate is **bash ≥ 4.3**, not the design doc's "≥ 4.0". Gating at 4.0 with a
major-version-only check would be the exact finding-F6 failure mode on 4.0–4.2: Nice
would send trigger bytes, the bind would have silently failed, and `[5099~` garbage
would land in the user's line. Supporting 4.0–4.2 via the macro-indirection trick
(bind the long sequence as a readline macro expanding to a 2-char sequence that is
`bind -x`'d) would burn a `\C-x`-prefix binding users may own, for zero real users —
rejected (YAGNI). Both gates — the Rust probe and the in-script guard — check ≥ 4.3
so they cannot disagree in the dangerous direction. This is a factual correction to
the design doc, flagged under Open questions.

## The degraded UX, precisely

**Contract:** ⌘↩ on a non-empty line prints a single static accent-colored status line
below the prompt — `✻ Composing… (ctrl-c cancels)` — runs `claude -p` synchronously,
then clears the status line and lets readline redraw the prompt with the composed
command replacing the typed English, cursor at end of line. Enter is never written.

Behavior by case:

| Case | Behavior |
|---|---|
| Empty line | Handler returns immediately; nothing painted (parity with zsh's `[[ -z "$BUFFER" ]]` guard). |
| Success | Status line erased; prompt + composed line redrawn in place; `READLINE_POINT` at end. |
| ctrl-c mid-compose | SIGINT kills the `claude` child (same process group). The typed English line is preserved unchanged. If bash aborts the handler before the cleanup `printf`, the status line may remain as a scrollback residue line — accepted. |
| `claude` missing / empty output | Status line erased; one stderr line `nice: compose failed (is claude on PATH?)`; typed line preserved. |
| ⌘↩ again while composing | The queued trigger replays after the handler returns. An idempotence guard (`_nice_compose_last`) makes re-composing the just-composed text a no-op, so a double-press costs nothing. Editing the result and pressing ⌘↩ again composes normally. |

**Redisplay mechanics** (the one fragile part — pinned by the pty e2e test): bash
forces a full readline redisplay after a `bind -x` handler returns (this is why the
folk `bind -x '"\C-l": clear'` binding works). The handler therefore: (1) prints
`\n` + status to stderr — cursor moves to the row below the prompt; (2) runs claude;
(3) prints `\r ESC[2K ESC[1A \r ESC[2K` — clear status row, step back up, clear the
old prompt row; (4) returns, and bash redraws prompt + new line at the cursor. The
exact escape choreography is *implementation latitude* (design doc's term) — whatever
the implementer ships must pass the real-pty e2e assertion "composed line rendered,
no stray fragments of the status line or the old English text".

**Honest zsh-parity losses** (record these in the script's header comment):

1. No animation — a static indicator, not the pulsing star.
2. The prompt is **blocked** while composing. zsh's prompt stays live (you can keep
   editing; a new prompt abandons the compose). In bash, keystrokes buffer until the
   handler returns; ctrl-c is the only escape from a hung `claude`.
3. Cancellation is coarser: zsh cleanly abandons via the precmd generation bump; bash
   relies on SIGINT killing the child, and may leave a residue status line.
4. Known cosmetic edges, accepted and documented in the script comment: a prompt
   sitting on the terminal's last row scrolls one line when the status prints; a
   multi-row wrapped input line or multi-line `PS1` can leave stale rows above the
   redrawn prompt. Single-row prompt + single-row line (the overwhelmingly common
   case) renders clean, and the e2e pins that case.

What survives with full parity: buffer replacement for review (never auto-run), the
request riding stdin (no quoting of user text on a command line), conf-driven
`--model`/`--effort` flags, accent-colored indicator, whitespace/fence/backtick
stripping, empty-line no-op.

## Work items (ordered)

### 1. Version probe + `compose_support` on `BashProfile`

`crates/nice/src/shell/bash.rs`:

- Store `version: Option<(u32, u32)>` on `BashProfile`, probed once at resolve time
  (design §6.3 allows this synchronously in bootstrap — `--norc --noprofile` is ~ms):

  ```text
  <path> --norc --noprofile -c 'printf %s.%s "${BASH_VERSINFO[0]}" "${BASH_VERSINFO[1]}"'
  ```

  Parse `major.minor`; any spawn failure, non-zero exit, or unparseable output ⇒
  `None` (fails toward the safe direction).
- `compose_support()`: `Trigger` iff `version >= Some((4, 3))`, else `None`.
- Factor the probe so tests can point it at an arbitrary "bash" path (a stub script
  printing a canned version) — same seam style as the resolution chain's injectable
  inputs.

No other Rust changes: `compose_route`, `PaneShell`, and the dispatch site are plan-01
machinery and already consume `ComposeSupport` correctly.

### 2. The compose section of `nice.bashrc`

Append to `crates/nice/src/shell/scripts/bash/nice.bashrc` (after the plan-02 hooks —
same "Nice hooks win by coming last" ordering rule). Full sketch below. Structural
rules the implementation must keep:

- **The whole file must PARSE under bash 3.2** — bash parses the entire rc before the
  version guard can run. Everything in the sketch is 3.2-parseable (no `${var^^}`, no
  associative arrays, no `;;&`, no `[[ -v`). Helper functions are defined
  **unconditionally** (3.2 can define and even run them; they're plain POSIX-ish
  bash); only the `bind` calls sit inside the ≥ 4.3 guard. This lets the
  function-level e2e tests run against `/bin/bash` 3.2 unconditionally (always
  present, CI-safe).
- The trigger spelling in the `bind` lines must be exactly `COMPOSE_TRIGGER_BINDKEY`
  (`\e[5099~`) — the structural test pins script↔constant agreement, mirroring the zsh
  test.
- Bind in all three relevant keymaps: default (emacs), `vi-insert`, `vi-command` —
  parity with zsh's emacs/viins/vicmd triple.
- No `PROMPT_COMMAND` hook, no generation counters: the synchronous shape means
  nothing is ever in flight across prompts.
- The dialect line (finding F5): the instruction string lives in this script — the
  single word changes from "zsh" to "bash", everything else byte-identical to the zsh
  instruction. It flows exactly as in zsh: spliced as the `-p` argument to
  `command claude`, with conf-driven `--model`/`--effort` appended. No Rust-side
  dialect plumbing (design §6.3: "the instruction lives inside each profile's script").

### 3. Structural tests for the script

In `bash.rs`'s test module (or wherever plan 02 put the bashrc structural tests —
follow its convention), mirroring the zsh static-text suite
(`zshrc_compose_defines_widget_and_binds_trigger_in_all_keymaps` etc.):

- binds `COMPOSE_TRIGGER_BINDKEY` via `bind -x` in emacs + vi-insert + vi-command,
  all inside the `BASH_VERSINFO` ≥ 4.3 guard (assert both the guard expression and
  that the binds are within it — a plain substring-order check is enough, like the
  zsh tests do);
- instruction says `a single bash command line` and does NOT contain `zsh`;
- handler never accepts the line: no `accept-line`, no newline written, mutations
  limited to `READLINE_LINE`/`READLINE_POINT`;
- request rides stdin (`<<< "$request"` present; `$request`/`$READLINE_LINE` never
  interpolated into the claude argv);
- empty-`READLINE_LINE` early return present;
- reads `NICE_COMPOSE_CONF` for `accent`/`model`/`effort`;
- byte agreement between the script's bind spelling and `COMPOSE_TRIGGER_SEQ` (port
  of the zsh constant-agreement assertion).

Plus one new test with no zsh analogue: **`/bin/bash -n <script>` exits 0** — pins
"parses under 3.2" forever.

### 4. Function-level e2e against `/bin/bash` 3.2 (unconditional)

Port of `compose_translate_pipes_stdin_and_conf_flags_e2e`,
`compose_translate_no_conf_and_failure_e2e`, `compose_strip_unwraps_fences_e2e`,
`compose_conf_get_matches_rust_parser_e2e` (`shell_inject.rs:1631-1743`): write a fake
`claude` recording argv + stdin, a conf JSON, then run

```text
/bin/bash -c 'source <nice.bashrc-or-extracted-fns>; _nice_compose_translate <<< "req"'
```

with a controlled `HOME`/`PATH`. Because the helpers are defined outside the version
guard, these run on stock 3.2 — no homebrew dependency, runs everywhere including CI.
If sourcing the full rc in `-c` mode trips on plan-02 sections (login emulation
sourcing real profiles), reuse plan 02's hermetic-source helper or source with a
scratch `$HOME`; the zsh suite's `run_zsh_compose` helper (`shell_inject.rs:1581`) is
the model. Cases: conf present ⇒ `--model`/`--effort` flags + instruction as `-p` arg
+ request on stdin; no conf ⇒ bare invocation; failing claude ⇒ empty output, nonzero;
strip unwraps fences/backticks/whitespace; `_nice_compose_conf_get` agrees with the
Rust `compose_conf::parse_value` on the same JSON bytes.

### 5. Interactive real-pty e2e against modern bash (skip-if-absent)

Port of `compose_spinner_paints_accent_in_real_pty_e2e` (`shell_inject.rs:1751`). The
zpty harness works unchanged for a bash child — zsh's `zpty` module is just the pty
driver; spawn `zpty n <modern-bash> --rcfile <nice.bashrc> -i` instead of
`/bin/zsh -i`. Script: type `list all files with details`, send the trigger bytes,
poll-drain, then assert on the raw capture:

- `Composing` painted, in the conf accent as truecolor SGR (`38;2;R;G;B` for the conf
  hex — same assertion bytes as the zsh test);
- the composed command (`ls -la` from the slow fake claude) appears in the redrawn
  line;
- NOT executed: fake claude recorded the request on stdin, and no `total`-style ls
  output follows;
- after sending Enter (add this leg): the command runs — proving the redrawn buffer is
  real editing state, not screen paint;
- no stray `[5099~` fragments anywhere in the capture.

**Harness bash discovery** (also used by the smoke scenario): a test helper
`find_compose_bash() -> Option<PathBuf>` checking, in order: `NICE_TEST_BASH` env
override → `/opt/homebrew/bin/bash` → `/usr/local/bin/bash` → `bash` on the test
process's PATH — each candidate validated with the work-item-1 probe ≥ (4,3). `None`
⇒ `eprintln!` a skip notice and return early (same policy shape as other
environment-gated tests; design §10). Note for CI: if the runner image lacks homebrew
bash the test self-skips; adding `brew install bash` to the workflow is a separate
one-line decision (Open questions).

Add one guard-side e2e on stock 3.2 (no skip): source the rc in `/bin/bash --rcfile
… -i -c 'bind -p'`-style probe or simply assert sourcing succeeds and
`_nice_command_compose` is defined while no error output mentions `bind` — pinning
that 3.2 sources the file cleanly with the guard closed (no `bind -x` warnings
leaking to the user's terminal).

### 6. Version-probe + gating unit tests

- Probe parser: canned outputs `3.2`, `4.2`, `4.3`, `5.3`, empty, garbage,
  spawn-failure ⇒ expected `Option<(u32,u32)>`.
- Probe against stub executables (scripts printing versions) via the seam.
- `compose_support` truth: `(3,2)`/`(4,2)`/`None` ⇒ `ComposeSupport::None`;
  `(4,3)`/`(5,3)` ⇒ `Trigger`.
- Regression pin for F6 at the routing layer: a `PaneShell { kind: Bash, compose:
  None }` pane never yields `ComposeRoute::Trigger` — extend plan 01's
  `compose_route` truth-table test with the bash-3.2-shaped row if it doesn't already
  have one.

### 7. `compose-live-bash` smoke scenario

Design §10 asks for "one added bash smoke scenario in step 5". Add a variant of the
`compose-live` self-test scenario (`crates/nice/src/compose_live.rs`): same
prepare/trigger/assert flow, but the pane spawns under `NICE_SHELL=<modern bash>`
(resolved via `find_compose_bash`; scenario self-reports "skipped: no bash ≥ 4.3"
when absent). Asserts the same three things as the zsh scenario: buffer replaced,
not executed without Enter, Enter runs it. Keep the existing `compose-live` zsh
scenario untouched (it stays pinned via `NICE_SHELL=/bin/zsh` per design §10).

## The bash compose script (full sketch)

Appended to `scripts/bash/nice.bashrc`. This is the reference implementation the
structural tests pin; wording of comments is latitude, structure is not.

```bash
# ── Nice: Command Compose (the commandCompose shortcut, cmd-enter) ──
# Nice writes the private trigger ESC[5099~ to this pty only when this
# pane's spawn-time snapshot says compose is supported — which the app
# grants only for bash >= 4.3 (bind -x needs 4.3 for key sequences
# longer than two characters; READLINE_LINE needs 4.0). The guard at
# the bottom mirrors that same gate so the two can never disagree in
# the dangerous direction.
#
# Degraded vs the zsh widget, deliberately: readline has no async fd
# handlers and no POSTDISPLAY ghost text, so this handler runs
# `claude -p` SYNCHRONOUSLY — a static accent-colored status line
# below the prompt instead of the pulsing star, and the prompt is
# blocked until the reply lands (ctrl-c cancels, killing the claude
# child and keeping the typed line). The composed command REPLACES
# the line for review; nothing here ever accepts it — running it is
# always the user's own Enter. Known cosmetic edges: a prompt on the
# terminal's last row scrolls one line while composing; ctrl-c can
# leave the status line behind as scrollback.
#
# Everything except the `bind` calls is defined unconditionally: bash
# 3.2 must PARSE this whole file, and defining the helpers everywhere
# lets them be tested against stock /bin/bash.

_nice_compose_instruction='Convert this plain-English request into a single bash command line for macOS. Reply with ONLY the command itself - no code fences, no backticks, no explanation, no surrounding quotes. If the request is already a valid shell command, return it unchanged.'

_nice_compose_conf_get() {
    # $1: key in the flat Nice-written JSON at $NICE_COMPOSE_CONF,
    # e.g. {"accent":"#7A94DB","model":"sonnet","effort":"medium"}.
    # Keys and values are Nice-controlled (no escapes), so parameter
    # surgery beats requiring a JSON tool on PATH. Same shape as zsh.
    [[ -n "$NICE_COMPOSE_CONF" && -r "$NICE_COMPOSE_CONF" ]] || return 1
    local blob rest
    blob=$(<"$NICE_COMPOSE_CONF")
    rest="${blob#*\"$1\":\"}"
    [[ "$rest" == "$blob" ]] && return 1
    printf '%s' "${rest%%\"*}"
}

_nice_compose_translate() {
    # stdin: the plain-English request; stdout: the composed command.
    # The request rides stdin — user text is never placed on a command
    # line, so no quoting of it can ever be wrong.
    local -a flags
    flags=()
    local v
    v="$(_nice_compose_conf_get model)" && [[ -n "$v" ]] && flags+=(--model "$v")
    v="$(_nice_compose_conf_get effort)" && [[ -n "$v" ]] && flags+=(--effort "$v")
    # Guard the expansion: "${flags[@]}" with an empty array trips
    # `set -u` on bash < 4.4 (same rationale as the zsh guard).
    if (( ${#flags[@]} )); then
        command claude -p "$_nice_compose_instruction" "${flags[@]}" 2>/dev/null
    else
        command claude -p "$_nice_compose_instruction" 2>/dev/null
    fi
}

_nice_compose_strip() {
    # $1: raw model output. Trim whitespace, then defensively unwrap a
    # ``` fence or a wrapping backtick pair (bash 3.2-safe trims — the
    # zsh version's extendedglob patterns do not carry over).
    local out=$1
    out="${out#"${out%%[![:space:]]*}"}"
    out="${out%"${out##*[![:space:]]}"}"
    if [[ "$out" == '```'* && "$out" == *$'\n'* ]]; then
        out="${out#*$'\n'}"
        out="${out%$'\n'*}"
        out="${out#"${out%%[![:space:]]*}"}"
        out="${out%"${out##*[![:space:]]}"}"
    fi
    if [[ "$out" == \`*\` ]]; then
        out="${out#\`}"
        out="${out%\`}"
    fi
    printf '%s' "$out"
}

_nice_compose_sgr() {
    # $1: "#rrggbb" accent from the conf → truecolor SGR prefix.
    # Missing/malformed → the dim fallback (matches zsh's fg=8).
    local h="${1#\#}"
    if [[ "$h" == [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F] ]]; then
        printf '\033[38;2;%d;%d;%dm' "$((16#${h:0:2}))" "$((16#${h:2:2}))" "$((16#${h:4:2}))"
    else
        printf '\033[38;5;8m'
    fi
}

_nice_compose_last=

_nice_command_compose() {
    [[ -z "$READLINE_LINE" ]] && return 0
    # A second cmd-enter queued while composing replays after this
    # handler returns; skip re-composing the text we just composed.
    [[ -n "$_nice_compose_last" && "$READLINE_LINE" == "$_nice_compose_last" ]] && return 0
    local request="$READLINE_LINE" sgr out rc
    sgr="$(_nice_compose_sgr "$(_nice_compose_conf_get accent)")"
    # Status line one row below the prompt. bash forces a readline
    # redisplay after a bind -x handler, so the cleanup only has to
    # put the cursor back on a cleared prompt row.
    printf '\n\033[2K%s\342\234\273 Composing\342\200\246 (ctrl-c cancels)\033[0m' "$sgr" >&2
    out="$(_nice_compose_translate <<< "$request")"
    rc=$?
    printf '\r\033[2K\033[1A\r\033[2K' >&2
    if (( rc >= 128 )); then
        # Interrupted (ctrl-c killed the claude child): keep the line.
        return 0
    fi
    out="$(_nice_compose_strip "$out")"
    if [[ -z "$out" ]]; then
        printf 'nice: compose failed (is claude on PATH?)\n' >&2
        return 1
    fi
    _nice_compose_last="$out"
    READLINE_LINE="$out"
    READLINE_POINT=${#out}
}

# bind -x with a multi-byte sequence needs bash >= 4.3. The app's
# BashProfile gates ComposeSupport on the same version, so when this
# guard is closed the trigger bytes are never sent (finding F6).
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 3) )); then
    bind -x '"\e[5099~": _nice_command_compose'
    bind -m vi-insert -x '"\e[5099~": _nice_command_compose'
    bind -m vi-command -x '"\e[5099~": _nice_command_compose'
fi
```

(The `\342\234\273` / `\342\200\246` octal spellings of `✻`/`…` are optional latitude —
literal UTF-8 in the file is fine too; the zsh script uses literals.)

## Test plan

How zsh compose is tested today (the models to mirror):

| Layer | zsh test | bash equivalent (this plan) |
|---|---|---|
| Static/structural | `zshrc_compose_defines_widget_and_binds_trigger_in_all_keymaps`, `_never_accepts_line`, `_pipes_buffer_via_stdin`, `_empty_buffer_is_noop`, `_reads_conf_and_hooks_precmd` (`shell_inject.rs:1522-1580`) | work item 3: same assertions minus precmd (none needed), plus the version-guard placement, the "bash not zsh" instruction pin, and `/bin/bash -n` parseability |
| Function e2e, fake claude | `compose_translate_pipes_stdin_and_conf_flags_e2e`, `_no_conf_and_failure_e2e`, `compose_strip_unwraps_fences_e2e`, `compose_conf_get_matches_rust_parser_e2e` (`:1631-1743`) | work item 4: identical cases against stock `/bin/bash` 3.2 — unconditional, CI-safe (helpers live outside the version guard) |
| Real-pty visual e2e | `compose_spinner_paints_accent_in_real_pty_e2e` (`:1751`, zpty-driven) | work item 5: zpty driving `<modern-bash> --rcfile … -i`; **requires bash ≥ 4.3 — found via `NICE_TEST_BASH` → `/opt/homebrew/bin/bash` → `/usr/local/bin/bash` → PATH, validated by the version probe; skip with a printed notice when absent** |
| Rust routing | `compose_route` truth table (`window_state.rs` tests, `ComposeSupport` axis from plan 01) | work item 6: bash-3.2-shaped row pins no-trigger |
| Rust probe/gating | — (new surface) | work item 6: parser + stub-executable + truth tests |
| Live GUI scenario | `compose-live` (`compose_live.rs`, real zsh) | work item 7: `compose-live-bash`, `NICE_SHELL=<modern bash>`, self-skips when absent |

Fix-round policy per repo rules: run the targeted tests above, not the full suite.

## Acceptance criteria

1. In a pane running homebrew bash (≥ 4.3) under Nice injection: type plain English,
   press ⌘↩ ⇒ status line appears below the prompt, then the line is replaced by a
   bash command with the cursor at end; nothing executes until Enter; Enter runs it.
2. ctrl-c during compose ⇒ the typed English line survives untouched; no garbage in
   the buffer.
3. In a stock `/bin/bash` 3.2 pane: ⌘↩ produces exactly the pre-feature behavior
   (kitty forward or nothing) — zero bytes of `[5099~` ever visible, and sourcing the
   rc emits no warnings. `BashProfile::compose_support()` reports `None`.
4. `bash -n` (3.2) accepts the full `nice.bashrc`.
5. The instruction string sent to `claude -p` says "a single bash command line"; the
   zsh script bytes are unchanged (frozen-contract tests still green).
6. Version probe: 3.2/4.2/garbage/failure ⇒ `None`; 4.3/5.x ⇒ `Trigger`; probe adds
   no perceptible bootstrap latency (`--norc --noprofile`).
7. All new tests green; compose e2e + smoke scenario skip cleanly (with notice) on
   machines without a modern bash; zsh compose suite untouched and green.

## Open questions

1. **Design-doc deviation (factual correction): version gate is ≥ 4.3, not ≥ 4.0.**
   §6.3 says "writable `READLINE_LINE` requires bash ≥ 4.0" and majors-only probing.
   True but insufficient: multi-char `bind -x` sequences only work from 4.3, and the
   trigger is 8 bytes — a 4.0-gate would recreate finding F6 on bash 4.0–4.2. This
   plan gates both sides (probe and rc guard) at 4.3 and probes major.minor. The
   design doc should be amended when this lands.
2. **Redisplay-after-`bind -x` assumption.** The cleanup choreography relies on bash
   forcing a readline redisplay after the handler (the `bind -x '"\C-l": clear'`
   precedent). The pty e2e pins it on the bash we actually gate in (≥ 4.3). If some
   supported version doesn't redraw, the fallback within latitude is the
   non-transient variant: leave the status line in scrollback and let the prompt
   redraw below (uglier, never garbled).
3. **Plan-02 boundary.** Whether plan 02 already landed a version probe or bashrc
   structural-test scaffolding — reconcile at implementation start; extend rather
   than duplicate.
4. **CI coverage for the ≥ 4.3 path.** Skip-if-absent means the pty e2e and smoke
   scenario only run where homebrew bash exists. If CI's macOS image lacks it, decide
   whether to `brew install bash` in the workflow (one line) or accept local-only
   coverage, mirroring the display-bound-test precedent.
