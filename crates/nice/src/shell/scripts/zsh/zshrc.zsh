# Stash the resolved user-side ZDOTDIR before we drop NICE_USER_ZDOTDIR.
# Trim trailing slashes so an accidental "/Users/nick/" (from launchctl
# or weird shells) compares equal to "/Users/nick" for the unset branch.
NICE_RESOLVED_USER_ZDOTDIR="${NICE_USER_ZDOTDIR%/}"

# Restore ZDOTDIR to the user's intended value BEFORE sourcing
# their .zshrc — so anything they pull in during init (oh-my-zsh
# `ZSH_COMPDUMP="${ZDOTDIR:-$HOME}/.zcompdump-..."`, p10k's
# `source "${ZDOTDIR:-$HOME}/.p10k.zsh"`, plugin-manager caches,
# etc.) probes the user's real config path instead of our temp
# dir. The whole point of this PR is closing that gap; restoring
# after the source would only fix tools the user runs at the
# interactive prompt, not the much larger surface of init-time
# tooling.
if [[ "$NICE_RESOLVED_USER_ZDOTDIR" == "${HOME%/}" ]]; then
    unset ZDOTDIR    # match standard convention when $HOME resolves
else
    export ZDOTDIR="$NICE_RESOLVED_USER_ZDOTDIR"
fi
unset NICE_USER_ZDOTDIR

# Source the user's real .zshrc from where they actually keep it
# (handles XDG-style ZDOTDIR layouts under e.g. ~/.config/zsh).
[[ -n "$NICE_RESOLVED_USER_ZDOTDIR" && -f "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc" ]] \
    && source "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc"
unset NICE_RESOLVED_USER_ZDOTDIR

# Now shadow `claude` so running it handshakes with Nice over
# NICE_SOCKET. The socket either tells us to exit (Nice is opening
# a new session) or to exec claude in place (Nice is promoting this
# window to Claude). Defining the function AFTER user's .zshrc
# ensures we win over anything they may have defined themselves —
# if a user wants to opt out, they can still `unfunction claude`
# in a precmd hook.
_nice_json_escape() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    printf '"%s"' "$s"
}

# Tell Nice the Claude we ran as a CHILD has returned, so this window is a
# plain shell prompt again. Only the `attach` verb below runs one as a
# child; every other verb execs, and a window whose pty exits tells Nice by
# exiting. Without this Nice's per-window "a Claude is running here" flag
# would stay set forever and every later `claude` in this session would open
# a NEW session instead of promoting this window. Fire-and-forget: Nice closes the
# connection before handling it, so `nc` returns at once.
_nice_claude_exited() {
    [[ -z "$NICE_SOCKET" ]] && return 0
    local pane_id_json
    pane_id_json=$(_nice_json_escape "${NICE_PANE_ID:-}")
    printf '%s\n' "{\"action\":\"claude_exited\",\"paneId\":${pane_id_json}}" \
        | nc -U "$NICE_SOCKET" -w 2 >/dev/null 2>&1
    return 0
}

claude() {
    # Passthrough to the real binary (no handshake) when:
    #   1. Not inside a Nice pty ($NICE_SOCKET unset).
    #   2. stdin is piped — caller is streaming input to claude.
    #   3. User passed a flag that makes claude non-interactive.
    #   4. User invoked a non-interactive subcommand.
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

    # Send {cwd, args, tabId, paneId} and read a single-line reply.
    # NICE_TAB_ID / NICE_PANE_ID are empty in the Main Terminal —
    # Nice uses empty tabId as the signal for "always open a new
    # sidebar session."
    local cwd_json session_id_json window_id_json
    cwd_json=$(_nice_json_escape "$PWD")
    session_id_json=$(_nice_json_escape "${NICE_TAB_ID:-}")
    window_id_json=$(_nice_json_escape "${NICE_PANE_ID:-}")
    local payload="{\"action\":\"claude\",\"cwd\":${cwd_json},\"args\":${args_json},\"tabId\":${session_id_json},\"paneId\":${window_id_json}}"

    local response
    response=$(printf '%s\n' "$payload" | nc -U "$NICE_SOCKET" -w 2 2>/dev/null)
    if [[ -z "$response" ]]; then
        print -u2 "nice: control socket unreachable; running claude directly"
        exec command claude "$@"
    fi

    # The reply is one line of up to three positional fields:
    #   newtab
    #   inplace [<uuid>|-] [<settings path>]
    #   attach  <uuid> [<settings path>]   (exec-time normalization)
    #   resume  <uuid> [<settings path>]   (exec-time normalization)
    # The last two carry Nice's decision about whether the named session is
    # still hosted by the Claude daemon — only Nice can tell, and only at this
    # moment (a deferred window's pre-typed command may have waited hours).
    local mode sid settings
    read -r mode sid settings <<< "$response"
    case "$mode" in
        newtab)
            # Nice is opening a new sidebar session; nothing to do here.
            return 0
            ;;
        inplace)
            # Nice promoted this window to Claude. Build the exec line:
            #   --settings <path>  when Nice's theme sync is on (the
            #     3rd reply field), so this in-place Claude matches
            #     the Nice theme like a from-scratch Nice Claude window;
            #   --session-id <sid> when Nice minted an id so it can
            #     resume later. A sid of "-" (or empty) means the
            #     user's own args (e.g. --resume <uuid>) already
            #     identify the session, so no --session-id is added.
            local -a pre=()
            [[ -n "$settings" ]] && pre+=(--settings "$settings")
            [[ -n "$sid" && "$sid" != "-" ]] && pre+=(--session-id "$sid")
            # Guard the expansion so an empty `pre` never trips the
            # user's `setopt nounset` (and never injects an empty arg).
            if (( ${#pre} )); then
                exec command claude "${pre[@]}" "$@"
            else
                exec command claude "$@"
            fi
            ;;
        attach)
            # Nice resolved this invocation to a background session the Claude
            # daemon STILL hosts (a `--resume <uuid>` would spawn a second
            # process against a live conversation). `sid` is the FULL uuid;
            # `claude attach` matches jobs by ~/.claude/jobs directory-name
            # prefix, so it gets the leading 8 characters.
            #
            # Run attach as a CHILD, not exec: a jobs entry the daemon left
            # behind when it died exits non-zero, and the fallback leg then
            # degrades to a plain --resume instead of stranding the user at an
            # error. Detaching is NOT a failure and never takes that leg —
            # attach exits 0 both when the ctrl-z detach key fires and when the
            # session ends, and non-zero only on an error outcome or an
            # unmatched id (verified against 2.1.223).
            #
            # A child leaves this shell alive, which is exactly what the CLI
            # promises ("Ctrl+Z drops back to your shell") — so tell Nice the
            # window is a prompt again, or its promotion flag stays set forever.
            local -a post=(--resume "$sid")
            [[ -n "$settings" ]] && post=(--settings "$settings" "${post[@]}")
            if command claude attach "${sid[1,8]}"; then
                _nice_claude_exited
                return 0
            fi
            exec command claude "${post[@]}"
            ;;
        resume)
            # The mirror: the user ran `claude attach <id>` for a session the
            # daemon no longer hosts, so their args can only fail. Replace them
            # wholesale with `--resume <uuid>` (never "$@" — that still says
            # `attach`).
            local -a post=(--resume "$sid")
            [[ -n "$settings" ]] && post=(--settings "$settings" "${post[@]}")
            exec command claude "${post[@]}"
            ;;
        *)
            print -u2 "nice: unexpected response '$response'; running claude directly"
            exec command claude "$@"
            ;;
    esac
}

# Nice: emit OSC 7 (current working directory) on every cd so the
# host terminal can capture and persist it. Format:
#   ESC ] 7 ; file://hostname/path BEL
# The injected hook appends to chpwd_functions rather than replacing
# chpwd directly so anything the user already registered (in their
# real .zshrc, sourced above) keeps firing.
_nice_emit_cwd_osc7() {
    # Minimal URL encoding: % first (so we don't double-encode the
    # %20 we're about to emit), then space. macOS paths almost
    # never need more; anything exotic (?, #, non-ASCII) flows
    # through unencoded and SwiftTerm tolerates the raw bytes.
    # The `\%` escape is load-bearing — a bare `%` in a zsh
    # parameter pattern is the "anchor at end of string" matcher,
    # which makes `${PWD//%/%25}` append `%25` to every path.
    local p=${PWD//\%/%25}
    p=${p// /%20}
    printf '\e]7;file://%s%s\a' "${HOST}" "$p"
}
typeset -ga chpwd_functions
chpwd_functions+=(_nice_emit_cwd_osc7)
# Fire once at shell startup so the initial cwd is reported even
# if the user never cd's.
_nice_emit_cwd_osc7

# Nice: if the app asked us to pre-type a command at the next
# prompt (set when a restored Claude session boots), push it onto zsh's
# line-editor buffer. The user sees the command typed and ready;
# nothing runs until they hit Enter.
if [[ -n "$NICE_PREFILL_COMMAND" ]]; then
    print -z "$NICE_PREFILL_COMMAND"
fi

# Nice: Command Compose (the `commandCompose` shortcut, cmd-enter by
# default). Nice writes the private trigger ESC[5099~ to this pty only
# when the shell sits at an idle interactive prompt (no foreground
# child); the widget below then rewrites the line buffer's plain-English
# text into a real zsh command via `claude -p`, painting an animated
# spinner under the line (in Nice's accent color, read from the
# $NICE_COMPOSE_CONF file) while it thinks. The composed command
# REPLACES the buffer for review — nothing here ever accepts the line;
# running it is always the user's own Enter. A new prompt (Enter or
# ctrl-c) abandons an in-flight compose via the precmd hook.
typeset -g _nice_compose_gen=0
typeset -g _nice_compose_my_gen=0
typeset -g _nice_compose_fd= _nice_compose_pid=
typeset -g _nice_compose_spin_fd= _nice_compose_spin_pid=
typeset -g _nice_compose_frame=0
typeset -g _nice_compose_color=
typeset -g _nice_compose_hl=
typeset -g _nice_compose_instruction='Convert this plain-English request into a single zsh command line for macOS. Reply with ONLY the command itself - no code fences, no backticks, no explanation, no surrounding quotes. If the request is already a valid shell command, return it unchanged.'

_nice_compose_conf_get() {
    # $1: key in the flat Nice-written JSON at $NICE_COMPOSE_CONF,
    # e.g. {"accent":"#7A94DB","model":"sonnet","effort":"medium"}.
    # Prints the string value; fails if the file or key is missing.
    # Keys and values are Nice-controlled (no escapes), so plain
    # parameter surgery beats requiring a JSON tool on PATH.
    emulate -L zsh
    [[ -n "$NICE_COMPOSE_CONF" && -r "$NICE_COMPOSE_CONF" ]] || return 1
    local blob rest
    blob="$(<$NICE_COMPOSE_CONF)"
    rest="${blob#*\"$1\":\"}"
    [[ "$rest" == "$blob" ]] && return 1
    print -rn -- "${rest%%\"*}"
}

_nice_compose_translate() {
    # stdin: the plain-English request; stdout: the composed command.
    # Split from the widget so it is testable without a tty/ZLE. The
    # request rides stdin — user text is never placed on a command
    # line, so no quoting of it can ever be wrong.
    emulate -L zsh
    local -a flags=()
    local v
    v="$(_nice_compose_conf_get model)" && [[ -n "$v" ]] && flags+=(--model "$v")
    v="$(_nice_compose_conf_get effort)" && [[ -n "$v" ]] && flags+=(--effort "$v")
    # Guard the expansion so an empty `flags` never trips a user's
    # `setopt nounset` (same pattern as the claude() shadow above).
    if (( ${#flags} )); then
        command claude -p "$_nice_compose_instruction" "${flags[@]}" 2>/dev/null
    else
        command claude -p "$_nice_compose_instruction" 2>/dev/null
    fi
}

_nice_compose_strip() {
    # $1: raw model output. Prints the cleaned command: trim
    # whitespace, then defensively unwrap a ``` fence or a wrapping
    # backtick pair if the model ignored the instruction.
    emulate -L zsh -o extendedglob
    local out=$1
    out="${out##[[:space:]]#}"
    out="${out%%[[:space:]]#}"
    if [[ "$out" == '```'* && "$out" == *$'\n'* ]]; then
        out="${out#*$'\n'}"
        out="${out%$'\n'*}"
        out="${out##[[:space:]]#}"
        out="${out%%[[:space:]]#}"
    fi
    if [[ "$out" == \`*\` ]]; then
        out="${out#\`}"
        out="${out%\`}"
    fi
    print -rn -- "$out"
}

_nice_compose_display() {
    # $1: POSTDISPLAY text ('' clears). Colors it with the accent
    # captured at compose start; our region_highlight entry is tracked
    # in _nice_compose_hl so repeated frames never stack entries.
    # WIDGET context only — POSTDISPLAY/region_highlight are ZLE special
    # parameters, live only inside widget calls (an fd handler is NOT
    # widget context; handlers below re-enter one via `zle <widget>`).
    if [[ -n "$_nice_compose_hl" ]]; then
        region_highlight=("${(@)region_highlight:#$_nice_compose_hl}")
        _nice_compose_hl=
    fi
    POSTDISPLAY="$1"
    if [[ -n "$1" && -n "$_nice_compose_color" ]]; then
        # region_highlight offsets index BUFFER then POSTDISPLAY appended
        # after it (the P flag would ADD PREDISPLAY to the indexing — it
        # does NOT mean "relative to POSTDISPLAY"), so the spinner span
        # starts at the buffer's character length.
        _nice_compose_hl="${#BUFFER} $(( ${#BUFFER} + ${#POSTDISPLAY} )) fg=$_nice_compose_color"
        region_highlight+=("$_nice_compose_hl")
    fi
    zle -R
}

_nice_compose_show_frame() {
    # Claude Code's own thinking indicator: a star on the LEFT that pulses
    # through growing/shrinking asterisk glyphs (quoted — bare * would glob).
    local -a frames=('·' '✢' '✳' '✶' '✻' '✽' '✻' '✶' '✳' '✢')
    _nice_compose_display $'\n'"${frames[$(( _nice_compose_frame % 10 + 1 ))]} Composing… (ctrl-c cancels)"
}

_nice_compose_stop() {
    # Unregister + close both fds and reap both children. ZLE-active
    # context only (`zle -F` needs ZLE).
    if [[ -n "$_nice_compose_fd" ]]; then
        zle -F "$_nice_compose_fd" 2>/dev/null
        exec {_nice_compose_fd}<&-
        _nice_compose_fd=
    fi
    if [[ -n "$_nice_compose_spin_fd" ]]; then
        zle -F "$_nice_compose_spin_fd" 2>/dev/null
        exec {_nice_compose_spin_fd}<&-
        _nice_compose_spin_fd=
    fi
    [[ -n "$_nice_compose_pid" ]] && kill "$_nice_compose_pid" 2>/dev/null
    [[ -n "$_nice_compose_spin_pid" ]] && kill "$_nice_compose_spin_pid" 2>/dev/null
    _nice_compose_pid= _nice_compose_spin_pid=
}

# Widget: repaint the current spinner frame (fd handlers re-enter widget
# context through this so POSTDISPLAY is actually live).
_nice_compose_spin_widget() {
    _nice_compose_show_frame
}
zle -N _nice_compose_spin_widget

# Widget: clear the spinner line (stale-compose cleanup path).
_nice_compose_clear_widget() {
    _nice_compose_display ""
}
zle -N _nice_compose_clear_widget

# Widget: land the composed command in the buffer ($1 = raw model output).
# NEVER accepts the line — running it is always the user's own Enter.
_nice_compose_apply_widget() {
    emulate -L zsh
    _nice_compose_display ""
    local out
    out="$(_nice_compose_strip "$1")"
    if [[ -z "$out" ]]; then
        zle -M "nice: compose failed (is claude on PATH?)"
        return 1
    fi
    BUFFER="$out"
    CURSOR=${#BUFFER}
    zle -R
}
zle -N _nice_compose_apply_widget

_nice_compose_tick() {
    # zle -F handler (NOT widget context — no direct BUFFER/POSTDISPLAY).
    emulate -L zsh
    if (( _nice_compose_my_gen != _nice_compose_gen )); then
        # Stale ticker from an abandoned compose: full cleanup.
        _nice_compose_stop
        zle _nice_compose_clear_widget
        return 0
    fi
    local junk
    if ! read -r -k 1 -u "$1" junk 2>/dev/null; then
        # Ticker hit EOF (crashed); drop just the spinner side.
        zle -F "$1" 2>/dev/null
        if [[ "$1" == "$_nice_compose_spin_fd" ]]; then
            exec {_nice_compose_spin_fd}<&-
            _nice_compose_spin_fd=
        fi
        return 0
    fi
    (( _nice_compose_frame++ ))
    zle _nice_compose_spin_widget
}

_nice_compose_done() {
    # zle -F handler (NOT widget context): drain + clean up, then hand
    # the result to the apply widget where the ZLE params are live.
    emulate -L zsh
    local fd=$1 out=
    zle -F "$fd" 2>/dev/null
    out="$(command cat <&$fd 2>/dev/null)"
    exec {fd}<&-
    [[ "$fd" == "$_nice_compose_fd" ]] && _nice_compose_fd=
    local stale=$(( _nice_compose_my_gen != _nice_compose_gen ))
    _nice_compose_stop
    if (( stale )); then
        zle _nice_compose_clear_widget
        return 0
    fi
    zle _nice_compose_apply_widget -- "$out"
}

_nice_command_compose() {
    emulate -L zsh
    [[ -z "$BUFFER" ]] && return 0
    _nice_compose_stop
    (( _nice_compose_gen++ ))
    _nice_compose_my_gen=$_nice_compose_gen
    _nice_compose_color="$(_nice_compose_conf_get accent)"
    [[ -n "$_nice_compose_color" ]] || _nice_compose_color=8
    local request="$BUFFER"
    exec {_nice_compose_fd}< <(_nice_compose_translate <<< "$request")
    _nice_compose_pid=$!
    zle -F "$_nice_compose_fd" _nice_compose_done
    exec {_nice_compose_spin_fd}< <(
        while :; do printf x; command sleep 0.1; done
    )
    _nice_compose_spin_pid=$!
    zle -F "$_nice_compose_spin_fd" _nice_compose_tick
    _nice_compose_frame=0
    _nice_compose_show_frame
}

_nice_compose_precmd() {
    # A new prompt (accepted line or ctrl-c) abandons any in-flight
    # compose: bump the generation so a pending fd handler discards its
    # result, and reap the children now. The fds stay registered until
    # ZLE is next active — the handlers self-clean on the stale path
    # (`zle -F` is unavailable outside ZLE, so it cannot happen here).
    (( _nice_compose_gen++ ))
    [[ -n "$_nice_compose_pid" ]] && kill "$_nice_compose_pid" 2>/dev/null
    [[ -n "$_nice_compose_spin_pid" ]] && kill "$_nice_compose_spin_pid" 2>/dev/null
    _nice_compose_pid= _nice_compose_spin_pid=
}
typeset -ga precmd_functions
precmd_functions+=(_nice_compose_precmd)

zle -N _nice_command_compose
bindkey -M emacs '\e[5099~' _nice_command_compose
bindkey -M viins '\e[5099~' _nice_command_compose
bindkey -M vicmd '\e[5099~' _nice_command_compose