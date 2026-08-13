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
            # `local -a pre` and `pre=()` stay on separate lines: bash 3.2's
            # `local name=(...)` initialization has historical quirks.
            local -a pre
            pre=()
            [[ -n "$settings" ]] && pre+=(--settings "$settings")
            [[ -n "$sid" && "$sid" != "-" ]] && pre+=(--session-id "$sid")
            # `${#pre[@]}` — the ARRAY length. `${#pre}` (the zsh spelling)
            # would be the length of element 0 in bash.
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
            # `${sid:0:8}` — bash substring. zsh's `${sid[1,8]}` subscript
            # expands to the WHOLE string in bash (inventory finding 3).
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
# on $PWD and emit only when the cwd actually changed. Accepted semantic
# delta vs zsh's chpwd: the report lands when the next prompt paints, not at
# `cd` time (`cd x && sleep 100` reports x only after the sleep).
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

# --- Command Compose (the commandCompose shortcut, cmd-enter) ----------------
# Nice writes the private trigger ESC[5099~ to this pty only when this pane's
# spawn-time snapshot says compose is supported — which the app grants only for
# bash >= 4.3 (`bind -x` needs 4.3 for key sequences longer than two
# characters; writable READLINE_LINE needs 4.0). The guard at the bottom of
# this section mirrors that same gate, so the two sides can never disagree in
# the dangerous direction (a dead binding plus trigger bytes would drop
# `[5099~` into the user's line — inventory finding F6).
#
# Degraded vs the zsh widget, deliberately: readline has no async fd handlers
# and no POSTDISPLAY ghost text, so this handler runs `claude -p`
# SYNCHRONOUSLY — a static accent-colored status line below the prompt instead
# of the pulsing star, and the prompt is BLOCKED until the reply lands
# (keystrokes buffer; ctrl-c is the only escape, killing the claude child and
# keeping the typed line). Cancellation is coarser than the zsh widget's
# precmd generation bump for the same reason. The composed command REPLACES
# the line for review; nothing here ever accepts it — running it is always the
# user's own Enter.
#
# Known cosmetic edges, accepted: a prompt sitting on the terminal's last row
# scrolls one line while the status prints; a wrapped multi-row input line or
# a multi-line PS1 can leave stale rows above the redrawn prompt; ctrl-c can
# abort before the cleanup printf and leave the status line in scrollback.
# Single-row prompt + single-row line — the overwhelmingly common case —
# renders clean.
#
# Everything except the `bind` calls is defined UNCONDITIONALLY: bash 3.2 must
# PARSE this whole file (it does — nothing below is a >= 4 feature), and
# defining the helpers everywhere lets them be tested against stock /bin/bash.

_nice_compose_instruction='Convert this plain-English request into a single bash command line for macOS. Reply with ONLY the command itself - no code fences, no backticks, no explanation, no surrounding quotes. If the request is already a valid shell command, return it unchanged.'

_nice_compose_conf_get() {
    # $1: key in the flat Nice-written JSON at $NICE_COMPOSE_CONF,
    # e.g. {"accent":"#7A94DB","model":"sonnet","effort":"medium"}.
    # Keys and values are Nice-controlled (no escapes), so parameter surgery
    # beats requiring a JSON tool on PATH. Same shape as the zsh stub.
    [[ -n "$NICE_COMPOSE_CONF" && -r "$NICE_COMPOSE_CONF" ]] || return 1
    local blob rest
    blob=$(<"$NICE_COMPOSE_CONF")
    rest="${blob#*\"$1\":\"}"
    [[ "$rest" == "$blob" ]] && return 1
    printf '%s' "${rest%%\"*}"
}

_nice_compose_translate() {
    # stdin: the plain-English request; stdout: the composed command. Split
    # from the handler so it is testable without a tty. The request rides
    # stdin — user text is never placed on a command line, so no quoting of it
    # can ever be wrong.
    local -a flags
    flags=()
    local v
    v="$(_nice_compose_conf_get model)" && [[ -n "$v" ]] && flags+=(--model "$v")
    v="$(_nice_compose_conf_get effort)" && [[ -n "$v" ]] && flags+=(--effort "$v")
    # Guard the expansion: "${flags[@]}" on an empty array trips `set -u` on
    # bash < 4.4 (same rationale as the zsh guard).
    if (( ${#flags[@]} )); then
        command claude -p "$_nice_compose_instruction" "${flags[@]}" 2>/dev/null
    else
        command claude -p "$_nice_compose_instruction" 2>/dev/null
    fi
}

_nice_compose_strip() {
    # $1: raw model output. Trim whitespace, then defensively unwrap a ```
    # fence or a wrapping backtick pair. bash 3.2-safe trims — the zsh
    # version's extendedglob patterns do not carry over.
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
    # $1: "#rrggbb" accent from the conf -> truecolor SGR prefix.
    # Missing/malformed -> the dim fallback (matches the zsh widget's fg=8).
    local h="${1#\#}"
    if [[ "$h" == [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F] ]]; then
        printf '\033[38;2;%d;%d;%dm' "$((16#${h:0:2}))" "$((16#${h:2:2}))" "$((16#${h:4:2}))"
    else
        printf '\033[38;5;8m'
    fi
}

# Idempotence guard for the queued-double-trigger case below. It is never
# reset, deliberately: a later freshly-typed line that happens to equal an
# older composed result no-ops instead of recomposing, which costs nothing
# real (the instruction returns an already-valid command unchanged, so that
# compose would be an expensive no-op anyway). The failure path never sets it,
# so a failed compose stays retryable without editing the line.
_nice_compose_last=

_nice_command_compose() {
    [[ -z "$READLINE_LINE" ]] && return 0
    # A second cmd-enter pressed while composing replays after this handler
    # returns (readline buffers the bytes); skip recomposing what we just
    # composed.
    [[ -n "$_nice_compose_last" && "$READLINE_LINE" == "$_nice_compose_last" ]] && return 0
    local request="$READLINE_LINE" sgr out rc
    sgr="$(_nice_compose_sgr "$(_nice_compose_conf_get accent)")"
    # Status line one row below the prompt. bash forces a full readline
    # redisplay after a `bind -x` handler returns (the same behavior that makes
    # the folk `bind -x '"\C-l": clear'` binding work), so the cleanup only has
    # to put the cursor back on a cleared prompt row: clear the status row,
    # step back up, clear the old prompt row.
    printf '\n\033[2K%s\342\234\273 Composing\342\200\246 (ctrl-c cancels)\033[0m' "$sgr" >&2
    out="$(_nice_compose_translate <<< "$request")"
    rc=$?
    printf '\r\033[2K\033[1A\r\033[2K' >&2
    if (( rc >= 128 )); then
        # Interrupted (ctrl-c killed the claude child): keep the typed line.
        return 0
    fi
    out="$(_nice_compose_strip "$out")"
    if [[ -z "$out" ]]; then
        printf '%s\n' "nice: compose failed (is claude on PATH?)" >&2
        return 1
    fi
    _nice_compose_last="$out"
    READLINE_LINE="$out"
    READLINE_POINT=${#out}
}

# `bind -x` on a multi-byte sequence needs bash >= 4.3, and BashProfile gates
# ComposeSupport on the same pair — when this guard is closed the trigger bytes
# are never sent. Bound in all three keymaps that can hold an idle prompt:
# parity with the zsh widget's emacs/viins/vicmd triple.
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 3) )); then
    bind -x '"\e[5099~": _nice_command_compose'
    bind -m vi-insert -x '"\e[5099~": _nice_command_compose'
    bind -m vi-command -x '"\e[5099~": _nice_command_compose'
fi

# Fire once at startup so the initial cwd is reported even if the user never
# cd's. This is also the readiness signal Nice's app-typed prefill waits for
# — it MUST stay the final statement of this file (the compose section sits
# above it, deliberately). Note the signal is "first OSC 7 of the pane", not
# "this line": a user profile sourced by the login emulation above may carry
# its own terminal-integration OSC 7 emitter and report first, which only
# means Nice types the prefill a moment earlier, into the tty input queue.
_nice_last_osc7_pwd=$PWD
_nice_emit_cwd_osc7
