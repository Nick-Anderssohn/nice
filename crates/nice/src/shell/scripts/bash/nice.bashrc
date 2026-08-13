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

# Fire once at startup so the initial cwd is reported even if the user never
# cd's. This is also the readiness signal Nice's app-typed prefill waits for
# — it MUST stay the final statement of this file (a later compose section is
# inserted above it). Note the signal is "first OSC 7 of the pane", not
# "this line": a user profile sourced by the login emulation above may carry
# its own terminal-integration OSC 7 emitter and report first, which only
# means Nice types the prefill a moment earlier, into the tty input queue.
_nice_last_osc7_pwd=$PWD
_nice_emit_cwd_osc7
