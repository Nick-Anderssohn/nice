//! Shell injection — the synthetic `ZDOTDIR` rc chain (R14).
//!
//! Ports Swift `MainTerminalShellInject`
//! (`Sources/Nice/Process/MainTerminalShellInject.swift`). We write a `ZDOTDIR`
//! directory the Main Terminal's zsh picks up. It contains stub `.zshenv` /
//! `.zprofile` / `.zlogin` / `.zshrc` that chain back to the user's real startup
//! files (resolved from `$NICE_USER_ZDOTDIR` if set, else by sourcing
//! `~/.zshenv` to discover the user's intended `ZDOTDIR`), then — in `.zshrc` —
//! restore `ZDOTDIR` to that intended value BEFORE sourcing the user's `.zshrc`
//! and define a `claude()` function that intercepts *interactive* invocations
//! and forwards them to Nice's control socket so a new tab opens instead of the
//! shell exec'ing claude in place.
//!
//! The "restore `ZDOTDIR` *before sourcing user's .zshrc*" dance is what stops
//! shell tools (Powerlevel10k, oh-my-zsh, nvm, asdf, starship init…) from
//! scribbling on our temp dir when they probe `${ZDOTDIR:-$HOME}/...` — both at
//! the interactive prompt AND during the user's `.zshrc` init (oh-my-zsh sets
//! `ZSH_COMPDUMP="${ZDOTDIR:-$HOME}/.zcompdump-..."` at load time, p10k sources
//! `${ZDOTDIR:-$HOME}/.p10k.zsh`, etc.). Ordering "restore → source user's
//! .zshrc → install our hooks" gives correctness for those init-time probes and
//! lets our `claude()` shadow / OSC 7 hook layer on top of (and survive)
//! anything the user defines.
//!
//! Documented limitations (kept as-is, NOT fixed — see the Swift header):
//!   * `exec zsh` inside a Nice pane drops the injection (the new zsh runs with
//!     the user's restored `ZDOTDIR`, not our temp value).
//!   * `/etc/zshenv` setting `ZDOTDIR` bypasses the injection entirely (zsh
//!     re-resolves `$ZDOTDIR/.zshenv` from the new value before reading our
//!     stub). macOS ships no `/etc/zshenv`, so this is documented, not fixed.
//!
//! Storage location: the stubs live in a fixed, per-variant directory under
//! Application Support (`…/<CFBundleName>/zdotdir`) — NOT `$TMPDIR`. macOS's
//! `com.apple.bsd.dirhelper` sweeps `$TMPDIR` files older than 3 days; when Nice
//! ran longer than that, the sweep deleted the stubs out from under the live
//! process and every new pane's zsh then sourced nothing. Application Support is
//! never swept. Because the stub contents are static, one shared directory
//! serves every window and every process of a variant; each variant stays
//! isolated in its own per-variant Application Support folder via `CFBundleName`.
//! [`write_stubs`] rewrites the stubs on every launch, so the directory
//! self-heals if anything ever removes a file.
//!
//! **The four rc-stub bodies below are a FROZEN compatibility contract.**
//! Installed helpers already on users' disks (`~/.nice/nice-claude-hook.sh`,
//! `~/.nice/nice-handoff.sh`) and the shadow function's muscle-memory behavior
//! must keep working byte-for-byte against the app. They are ported
//! character-for-character from the Swift source and pinned by both the
//! static-text tests and the real-zsh end-to-end tests below. Do not "clean
//! them up" — the `\%` OSC 7 escape is load-bearing zsh arcana (a bare `%` is a
//! parameter pattern anchor), and the `_nice_json_escape` dialect (backslash,
//! double-quote, LF, CR, tab — nothing else) is exactly what Nice's JSON decoder
//! expects. The Command Compose widget tail (`_nice_compose_*` +
//! [`COMPOSE_TRIGGER_SEQ`]) joined the frozen contract with the `commandCompose`
//! shortcut: the trigger bytes and the `$NICE_COMPOSE_CONF` key names are
//! app↔shell interchange, pinned the same way.
//!
//! The `$NICE_SOCKET` env var the `claude()` function reads, and the per-pane
//! `NICE_TAB_ID` / `NICE_PANE_ID` / `NICE_USER_ZDOTDIR` / `NICE_PREFILL_COMMAND`
//! values, are injected separately (R14 slice 3 / slice 4); these stubs only
//! reference them. This module owns the stub text, the writer, and the
//! per-variant location; the `app::run` bootstrap wiring is R14 slice 3.

#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};

/// The private pty trigger for Command Compose. Nice's `commandCompose` action
/// handler writes exactly these bytes to the pane's pty (only when the shell
/// sits at an idle interactive prompt); the `ZSHRC_BODY` widget below binds
/// exactly this sequence. CSI 5099~ (DECFNK style): no terminal emits
/// tilde-coded numbers anywhere near 5099 from real keys (the real ones top out
/// in the 30s), and it is deliberately far from bracketed paste's 200/201.
/// Written in a single `write_input` call so ZLE's keymap trie matches it
/// atomically against the `\e[5~` (PageUp) shared prefix.
pub const COMPOSE_TRIGGER_SEQ: &[u8] = b"\x1b[5099~";

/// The zsh-side `bindkey` spelling of [`COMPOSE_TRIGGER_SEQ`] (`\e` = ESC).
/// The static tests below pin that `ZSHRC_BODY` binds exactly this string in
/// all three keymaps, and that its bytes agree with the Rust constant.
pub const COMPOSE_TRIGGER_BINDKEY: &str = r"\e[5099~";

/// Stub `.zshenv`: discover + stash the user's intended `ZDOTDIR`, then restore
/// our temp dir so zsh keeps reading the other stubs.
pub const ZSHENV_BODY: &str = r#"# Nice: discover and stash the user's intended ZDOTDIR, then
# restore our temp dir so zsh keeps reading our other stubs
# (.zprofile / .zshrc). See file header for the cooperation contract.
NICE_TEMP_ZDOTDIR="$ZDOTDIR"
if [[ -n "$NICE_USER_ZDOTDIR" ]]; then
    # Inherited from Nice's launch env (launchctl / parent process).
    USER_ZDOTDIR="$NICE_USER_ZDOTDIR"
else
    # Source ~/.zshenv ourselves to discover any ZDOTDIR set there
    # (XDG-style). This is the FIRST source of ~/.zshenv this
    # session — zsh read OUR stub, not the user's, because ZDOTDIR
    # was overridden — so no double-source / non-idempotency risk.
    unset ZDOTDIR
    [[ -f "$HOME/.zshenv" ]] && source "$HOME/.zshenv"
    USER_ZDOTDIR="${ZDOTDIR:-$HOME}"
fi
export ZDOTDIR="$NICE_TEMP_ZDOTDIR"
export NICE_USER_ZDOTDIR="$USER_ZDOTDIR"
unset NICE_TEMP_ZDOTDIR USER_ZDOTDIR"#;

/// Stub `.zprofile`: chain back to the user's real `.zprofile` (login shells).
pub const ZPROFILE_BODY: &str = r#"# Nice: source the user's real .zprofile from the location resolved
# in our .zshenv. (Without this, login-shell users silently lose
# .zprofile because zsh's $ZDOTDIR/.zprofile lookup hits our stub.)
[[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zprofile" ]] \
    && source "$NICE_USER_ZDOTDIR/.zprofile""#;

/// Stub `.zlogin`: defensive chain-back to the user's real `.zlogin`.
pub const ZLOGIN_BODY: &str = r#"# Nice: defensive — if our .zshrc somehow exited before restoring
# ZDOTDIR (user .zshrc errored out, etc.), source the user's real
# .zlogin from where they actually keep it. In the success path
# ZDOTDIR has already been restored to the user's value by our
# .zshrc, so zsh reads the user's .zlogin directly and this stub
# is never reached.
[[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zlogin" ]] \
    && source "$NICE_USER_ZDOTDIR/.zlogin""#;

/// Stub `.zshrc`: restore `ZDOTDIR` before sourcing the user's `.zshrc`, then
/// install the `claude()` shadow, the OSC 7 cwd emitter, and the prefill tail.
pub const ZSHRC_BODY: &str = r##"# Stash the resolved user-side ZDOTDIR before we drop NICE_USER_ZDOTDIR.
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
# a new tab) or to exec claude in place (Nice is promoting this
# pane to Claude). Defining the function AFTER user's .zshrc
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

# Tell Nice the Claude we ran as a CHILD has returned, so this pane is a
# plain shell prompt again. Only the `attach` verb below runs one as a
# child; every other verb execs, and a pane whose pty exits tells Nice by
# exiting. Without this Nice's per-pane "a Claude is running here" flag
# would stay set forever and every later `claude` in this tab would open a
# NEW tab instead of promoting this pane. Fire-and-forget: Nice closes the
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
    # sidebar tab."
    local cwd_json tab_id_json pane_id_json
    cwd_json=$(_nice_json_escape "$PWD")
    tab_id_json=$(_nice_json_escape "${NICE_TAB_ID:-}")
    pane_id_json=$(_nice_json_escape "${NICE_PANE_ID:-}")
    local payload="{\"action\":\"claude\",\"cwd\":${cwd_json},\"args\":${args_json},\"tabId\":${tab_id_json},\"paneId\":${pane_id_json}}"

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
    # moment (a deferred pane's pre-typed command may have waited hours).
    local mode sid settings
    read -r mode sid settings <<< "$response"
    case "$mode" in
        newtab)
            # Nice is opening a new sidebar tab; nothing to do here.
            return 0
            ;;
        inplace)
            # Nice promoted this pane to Claude. Build the exec line:
            #   --settings <path>  when Nice's theme sync is on (the
            #     3rd reply field), so this in-place Claude matches
            #     the Nice theme like a from-scratch Nice Claude pane;
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
            # pane is a prompt again, or its promotion flag stays set forever.
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
# prompt (set when a restored Claude tab boots), push it onto zsh's
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
bindkey -M vicmd '\e[5099~' _nice_command_compose"##;

/// Write the four `ZDOTDIR` stubs into `dir`, creating it (and any missing
/// parents) if needed, and return `dir`. Ports Swift
/// `MainTerminalShellInject.make(at:)`. Every file is (over)written every call
/// so the directory self-heals if a stub was ever removed; each write is atomic
/// (temp sibling + rename) so a pty child mid-`source` never reads a half-written
/// stub when a second window/process of the same variant rewrites the shared dir.
pub fn write_stubs(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    write_atomic(&dir.join(".zshenv"), ZSHENV_BODY)?;
    write_atomic(&dir.join(".zprofile"), ZPROFILE_BODY)?;
    write_atomic(&dir.join(".zlogin"), ZLOGIN_BODY)?;
    write_atomic(&dir.join(".zshrc"), ZSHRC_BODY)?;
    Ok(dir.to_path_buf())
}

/// Atomically replace `path` with `contents`: write to a pid-suffixed sibling in
/// the same directory, then rename over the target (rename is atomic within a
/// filesystem). The pid suffix keeps two concurrent same-variant processes from
/// colliding on the temp name.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stub");
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// The fixed, per-variant `ZDOTDIR` location:
/// `<app support root>/<CFBundleName>/zdotdir`. Ports Swift
/// `MainTerminalShellInject.defaultLocation()`. Honors the
/// `NICE_APPLICATION_SUPPORT_ROOT` override seam (tests redirect it into a
/// sandbox; production leaves it unset). The folder name tracks `CFBundleName`
/// so each variant gets its own directory (`Nice` / `Nice Dev`).
/// Pure — creates nothing (unlike the Swift `FileManager.url(create:true)`,
/// which is unnecessary here because [`write_stubs`] creates the dir).
pub fn default_location() -> PathBuf {
    let override_value = std::env::var("NICE_APPLICATION_SUPPORT_ROOT").ok();
    let home = std::env::var("HOME").ok();
    application_support_root(override_value.as_deref(), home.as_deref())
        .join(bundle_folder_name())
        .join("zdotdir")
}

/// Resolve the Application Support root. The `NICE_APPLICATION_SUPPORT_ROOT`
/// override wins when present and non-empty (the test seam); otherwise
/// `<home>/Library/Application Support`. Factored out of [`default_location`] so
/// the override seam is unit-tested without mutating the process environment.
fn application_support_root(override_value: Option<&str>, home: Option<&str>) -> PathBuf {
    if let Some(root) = override_value {
        if !root.is_empty() {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(home.unwrap_or("/")).join("Library/Application Support")
}

/// The per-variant folder name: the running app's `CFBundleName` (`"Nice"` /
/// `"Nice Dev"` for the shipped bundles), falling back to `"Nice (unbundled)"`
/// when unbundled — so an unbundled `cargo run` gets its own directory. Delegates
/// to [`crate::platform::support_folder_name`], the single source of truth.
fn bundle_folder_name() -> String {
    crate::platform::support_folder_name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---- temp-dir plumbing -------------------------------------------------

    /// A throwaway directory removed on drop (mirrors Swift's
    /// `addTeardownBlock { removeItem }`). Its `Drop` runs on normal test exit;
    /// a panicking assertion leaves the temp dir behind, which is harmless.
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unique(prefix: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()))
    }

    /// A fresh empty scratch directory.
    fn scratch(prefix: &str) -> Scratch {
        let dir = unique(prefix);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Write the four stubs into a throwaway `ZDOTDIR` (auto-removed) and return
    /// it — the twin of Swift `makeIsolated()`.
    fn make_isolated() -> Scratch {
        let dir = unique("nice-zdotdir-test");
        write_stubs(&dir).expect("write stubs");
        Scratch(dir)
    }

    /// Read one stub file's contents after writing the stubs to disk (exercises
    /// the writer round-trip, like Swift's read-after-`make`).
    fn read_stub(name: &str) -> String {
        let dir = make_isolated();
        std::fs::read_to_string(dir.0.join(name)).expect("read stub")
    }

    fn zshrc() -> String {
        read_stub(".zshrc")
    }

    // ---- file layout -------------------------------------------------------

    #[test]
    fn make_creates_all_four_stubs() {
        let dir = make_isolated();
        for name in [".zshenv", ".zprofile", ".zlogin", ".zshrc"] {
            assert!(
                dir.0.join(name).is_file(),
                "expected ZDOTDIR to contain {name}"
            );
        }
    }

    /// The writer must round-trip the FROZEN constants byte-for-byte — a writer
    /// bug that mangled the stub text would silently break the socket handshake.
    #[test]
    fn writer_round_trips_frozen_bytes() {
        let dir = make_isolated();
        for (name, body) in [
            (".zshenv", ZSHENV_BODY),
            (".zprofile", ZPROFILE_BODY),
            (".zlogin", ZLOGIN_BODY),
            (".zshrc", ZSHRC_BODY),
        ] {
            let on_disk = std::fs::read_to_string(dir.0.join(name)).expect("read");
            assert_eq!(on_disk, body, "{name} on disk must equal the frozen const");
        }
    }

    /// The ZDOTDIR must live under Application Support, NOT `$TMPDIR` (which
    /// macOS sweeps after 3 days), and be stable across calls so the dir is
    /// reused rather than re-namespaced per launch.
    #[test]
    fn default_location_is_under_app_support_not_temp() {
        let dir = default_location();
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("zdotdir"),
            "ZDOTDIR directory should be named `zdotdir`"
        );
        assert!(
            !dir.starts_with(std::env::temp_dir()),
            "ZDOTDIR must not live in $TMPDIR (macOS dirhelper sweeps it after 3 days). Got: {dir:?}"
        );
        let parent = dir.parent().expect("has parent");
        assert!(
            parent.to_string_lossy().contains("Application Support"),
            "ZDOTDIR should live under Application Support. Got: {dir:?}"
        );
        assert_eq!(
            dir,
            default_location(),
            "ZDOTDIR location must be stable across calls (one shared, reused dir)"
        );
    }

    // ---- the NICE_APPLICATION_SUPPORT_ROOT override seam -------------------
    //
    // Driven through the pure `application_support_root` so no process env is
    // mutated (which would race parallel tests).

    #[test]
    fn override_root_wins_when_set() {
        assert_eq!(
            application_support_root(Some("/sandbox/appsup"), Some("/home/u")),
            PathBuf::from("/sandbox/appsup")
        );
    }

    #[test]
    fn empty_override_falls_back_to_home() {
        assert_eq!(
            application_support_root(Some(""), Some("/home/u")),
            PathBuf::from("/home/u/Library/Application Support")
        );
    }

    #[test]
    fn default_root_uses_home_app_support() {
        assert_eq!(
            application_support_root(None, Some("/home/u")),
            PathBuf::from("/home/u/Library/Application Support")
        );
    }

    // ---- chain-back stubs --------------------------------------------------

    /// `.zprofile`, `.zlogin`, `.zshrc` chain back through the resolved user-side
    /// ZDOTDIR var so XDG-style layouts and `~/.zshenv`-set values are honored.
    #[test]
    fn chain_backs_source_from_user_zdotdir() {
        for (filename, var) in [
            (".zprofile", "NICE_USER_ZDOTDIR"),
            (".zlogin", "NICE_USER_ZDOTDIR"),
            (".zshrc", "NICE_RESOLVED_USER_ZDOTDIR"),
        ] {
            let body = read_stub(filename);
            let needle = format!(r#"source "${var}/{filename}""#);
            assert!(
                body.contains(&needle),
                "{filename} must source ${var}/{filename}"
            );
        }
    }

    /// `.zshenv` discovers the user's intended ZDOTDIR (preferring
    /// `$NICE_USER_ZDOTDIR`, falling back to sourcing `~/.zshenv`), then restores
    /// `$ZDOTDIR` to our temp dir so zsh keeps reading our other stubs.
    #[test]
    fn zshenv_discovers_user_zdotdir() {
        let body = read_stub(".zshenv");
        assert!(
            body.contains(r#"if [[ -n "$NICE_USER_ZDOTDIR" ]]; then"#),
            ".zshenv must branch on NICE_USER_ZDOTDIR"
        );
        assert!(
            body.contains(r#"source "$HOME/.zshenv""#),
            ".zshenv must source ~/.zshenv as the fallback discovery path"
        );
        assert!(
            body.contains(r#"export ZDOTDIR="$NICE_TEMP_ZDOTDIR""#),
            ".zshenv must restore ZDOTDIR to our temp value"
        );
        assert!(
            body.contains(r#"export NICE_USER_ZDOTDIR="$USER_ZDOTDIR""#),
            ".zshenv must persist the resolved value back into NICE_USER_ZDOTDIR"
        );
    }

    /// `.zshrc` restores `$ZDOTDIR` to the user's value BEFORE sourcing their
    /// `.zshrc`, and installs `claude()` AFTER, so our hooks win.
    #[test]
    fn zshrc_restores_user_zdotdir_before_sourcing() {
        let body = zshrc();
        let restore = body
            .find(r#"export ZDOTDIR="$NICE_RESOLVED_USER_ZDOTDIR""#)
            .expect("restore marker present");
        let source = body
            .find(r#"source "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc""#)
            .expect("source marker present");
        let claude = body.find("claude() {").expect("claude marker present");
        assert!(
            restore < source,
            ".zshrc must restore ZDOTDIR BEFORE sourcing user's .zshrc"
        );
        assert!(
            source < claude,
            ".zshrc must source user's .zshrc BEFORE installing claude()"
        );
        assert!(
            body.contains("unset NICE_USER_ZDOTDIR"),
            ".zshrc must clear NICE_USER_ZDOTDIR"
        );
        assert!(
            body.contains(r#"if [[ "$NICE_RESOLVED_USER_ZDOTDIR" == "${HOME%/}" ]]; then"#)
                && body.contains("unset ZDOTDIR"),
            ".zshrc must unset (not export) ZDOTDIR when the resolved value matches $HOME"
        );
    }

    // ---- .zshrc shell wrapper contract ------------------------------------

    #[test]
    fn zshrc_defines_claude_function() {
        assert!(
            zshrc().contains("claude() {"),
            "zshrc must shadow `claude` with a function"
        );
    }

    #[test]
    fn zshrc_defines_json_escape_helper() {
        let body = zshrc();
        assert!(body.contains("_nice_json_escape()"), "JSON escape helper required");
        assert!(
            body.contains(r#"s=${s//\\/\\\\}"#),
            "escape must replace backslashes first"
        );
        assert!(
            body.contains(r#"s=${s//\"/\\\"}"#),
            "escape must replace double quotes"
        );
        assert!(body.contains(r#"$'\n'"#), "escape must handle embedded newlines");
    }

    #[test]
    fn zshrc_handshake_payload_shape() {
        let body = zshrc();
        assert!(
            body.contains(r#""action":"claude""#) || body.contains(r#"\"action\":\"claude\""#),
            "payload must label itself as the claude action"
        );
        assert!(body.contains("cwd"), "payload must include cwd");
        assert!(body.contains("args"), "payload must include args");
        assert!(body.contains("tabId"), "payload must include tabId");
        assert!(body.contains("paneId"), "payload must include paneId");
    }

    #[test]
    fn zshrc_uses_nc_with_socket_path() {
        assert!(
            zshrc().contains(r#"nc -U "$NICE_SOCKET""#),
            "must speak AF_UNIX to Nice's control socket via nc -U"
        );
    }

    #[test]
    fn zshrc_dispatches_newtab_and_inplace_modes() {
        let body = zshrc();
        assert!(body.contains("newtab)"), "wrapper must handle the `newtab` mode");
        assert!(body.contains("inplace)"), "wrapper must handle the `inplace` mode");
        assert!(
            body.contains(r#"pre+=(--session-id "$sid")"#),
            "inplace must splice --session-id"
        );
        assert!(
            body.contains(r#"pre+=(--settings "$settings")"#),
            "inplace must splice --settings"
        );
        // Fix D's exec-time normalization verbs. They reuse the same three
        // positional fields (`mode sid settings`), so only the dispatch is new.
        assert!(body.contains("attach)"), "wrapper must handle the `attach` mode");
        assert!(body.contains("resume)"), "wrapper must handle the `resume` mode");
    }

    /// The `attach` verb runs attach as a CHILD and degrades to `--resume` when
    /// it fails, so a jobs entry the daemon left behind never strands the user
    /// at an error. The short id `claude attach` matches on is derived from the
    /// full uuid the reply carries (attach prefix-matches `~/.claude/jobs`
    /// directory names, which are the first 8 characters).
    #[test]
    fn zshrc_attach_mode_falls_back_to_resume() {
        let body = zshrc();
        assert!(
            body.contains(r#"if command claude attach "${sid[1,8]}"; then"#),
            "attach must run as a child so its failure can fall back"
        );
        assert!(
            body.contains(r#"local -a post=(--resume "$sid")"#),
            "the fallback resumes the FULL uuid from the reply"
        );
        assert!(
            !body.contains(r#"exec command claude attach"#),
            "attach must NOT be exec'd — that would discard the fallback"
        );
    }

    /// A returned attach leaves this shell alive (the CLI's ctrl-z detach drops
    /// the user back to their prompt), so the wrapper must report the pane back
    /// to Nice — otherwise its promotion flag stays set and every later `claude`
    /// in the tab opens a new tab instead of promoting the pane.
    #[test]
    fn zshrc_reports_a_returned_attach_back_to_nice() {
        let body = zshrc();
        assert!(
            body.contains(r#""{\"action\":\"claude_exited\",\"paneId\":${pane_id_json}}""#),
            "the notifier must send the claude_exited action with this pane's id"
        );
        let arm = body
            .split_once("        attach)")
            .expect("attach arm present")
            .1;
        let arm = arm.split_once(";;").expect("attach arm terminated").0;
        assert!(
            arm.contains("_nice_claude_exited"),
            "the attach arm must notify on the success leg. Got: <{arm}>"
        );
    }

    /// The `resume` verb replaces the user's args wholesale: they still say
    /// `attach <id>`, which is exactly what Nice decided cannot work.
    #[test]
    fn zshrc_resume_mode_replaces_the_users_args() {
        let body = zshrc();
        let arm = body
            .split_once("        resume)")
            .expect("resume arm present")
            .1;
        let arm = arm.split_once(";;").expect("resume arm terminated").0;
        // Code only — the arm's comment names `"$@"` to explain why it is absent.
        let code: String = arm
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains(r#"exec command claude "${post[@]}""#),
            "resume must exec the rebuilt argv. Got: <{code}>"
        );
        assert!(
            !code.contains(r#""$@""#),
            "resume must NOT pass the user's `attach` args through. Got: <{code}>"
        );
        assert!(
            arm.contains(r#"post=(--settings "$settings" "${post[@]}")"#),
            "resume must still splice the theme pointer. Got: <{arm}>"
        );
    }

    #[test]
    fn zshrc_socket_unreachable_falls_back_to_command() {
        let body = zshrc();
        assert!(
            body.contains("control socket unreachable"),
            "must warn when the socket is gone"
        );
        assert!(
            body.contains(r#"exec command claude "$@""#),
            "unreachable socket must fall back to running claude directly"
        );
    }

    #[test]
    fn zshrc_non_interactive_flags_short_circuit_to_command() {
        let body = zshrc();
        for flag in ["-p", "--print", "-h", "--help", "--version", "--output-format"] {
            assert!(
                body.contains(flag),
                "non-interactive flag {flag} must be short-circuited"
            );
        }
    }

    #[test]
    fn zshrc_non_interactive_subcommands_short_circuit() {
        let body = zshrc();
        for sub in ["mcp", "config", "migrate-installer", "update", "doctor"] {
            assert!(
                body.contains(sub),
                "non-interactive subcommand {sub} must be short-circuited"
            );
        }
    }

    #[test]
    fn zshrc_prefill_command_uses_print_z() {
        assert!(
            zshrc().contains(r#"print -z "$NICE_PREFILL_COMMAND""#),
            "restored Claude tabs rely on print -z to pre-type the resume command"
        );
    }

    #[test]
    fn zshrc_no_handshake_when_socket_unset() {
        assert!(
            zshrc().contains(r#"if [[ -z "$NICE_SOCKET" ]]"#),
            "missing NICE_SOCKET must bypass the wrapper entirely"
        );
    }

    // ---- OSC 7 cwd-update emitter -----------------------------------------

    #[test]
    fn zshrc_defines_osc7_emitter() {
        assert!(
            zshrc().contains("_nice_emit_cwd_osc7()"),
            "zshrc must define the OSC 7 emitter"
        );
    }

    #[test]
    fn zshrc_emitter_hooks_into_chpwd_functions() {
        assert!(
            zshrc().contains("chpwd_functions+=(_nice_emit_cwd_osc7)"),
            "emitter must append to chpwd_functions to fire on every cd"
        );
    }

    #[test]
    fn zshrc_emitter_fires_once_at_shell_start() {
        let body = zshrc();
        let plain_call = body.lines().any(|l| l.trim() == "_nice_emit_cwd_osc7");
        assert!(
            plain_call,
            "emitter must be invoked as a bare statement to capture spawn cwd"
        );
    }

    /// A bare `%` in zsh's `${var//pattern/repl}` is the end-of-string anchor
    /// matcher — it would append `%25` to every path. The backslash escape forces
    /// literal interpretation. Assert on the actual substitution line so comments
    /// mentioning the bare form don't trip the negative check.
    #[test]
    fn zshrc_percent_escape_is_literal_pattern() {
        let body = zshrc();
        let assign = body
            .lines()
            .find(|l| l.contains("local p=") && l.contains("PWD"))
            .unwrap_or("");
        assert!(
            assign.contains(r#"${PWD//\%/%25}"#),
            "% in the substitution pattern must be backslash-escaped. Got: <{assign}>"
        );
        assert!(
            !assign.contains(r#"${PWD//%/%25}"#),
            "bare `%` in the substitution line is the end-of-string anchor. Got: <{assign}>"
        );
    }

    #[test]
    fn zshrc_emitter_format_is_osc7_file_url() {
        assert!(
            zshrc().contains(r#"printf '\e]7;file://%s%s\a'"#),
            "emitter must produce a well-formed OSC 7 file:// URL terminated with BEL"
        );
    }

    // ---- real-zsh end-to-end ----------------------------------------------

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.len() > haystack.len() {
            return None;
        }
        (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
    }

    /// Drive the injected `claude()` shadow END-TO-END in a real pty: a fake
    /// `nc` answers the handshake with `reply`, and a fake `claude` appends its
    /// argv to a record file (exiting `attach_exit` when invoked as
    /// `attach …`, 0 otherwise). Returns the recorded argv lines, one per exec.
    ///
    /// The pty is what makes the dispatch reachable at all: the wrapper passes
    /// straight through to the real binary when stdin is not a tty, so the
    /// `-ic` helpers above can never enter it. Same zpty machinery as the
    /// Command Compose pty test below.
    /// What [`run_claude_shadow_e2e`] observed: the fake `claude`'s recorded
    /// argv lines (one per exec) plus the pty transcript, which the assertions
    /// quote so a failure shows what the shell actually did.
    struct ShadowRun {
        execs: Vec<String>,
        /// Every payload the wrapper wrote to the socket, one per line — the
        /// handshake first, then any fire-and-forget notification.
        payloads: Vec<String>,
        transcript: String,
    }

    fn run_claude_shadow_e2e(reply: &str, attach_exit: i32, command: &str) -> ShadowRun {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;

        let home = scratch("nice-shadow-home");
        let bin = home.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let record = home.0.join("argv");
        let sent = home.0.join("payloads");

        // The handshake partner: record the payload, print Nice's one-line reply.
        let nc = bin.join("nc");
        std::fs::write(
            &nc,
            format!(
                "#!/bin/zsh\ncommand cat >> {sent}\nprint -r -- {reply:?}\n",
                sent = sent.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&nc, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The exec target: record argv, then honor the requested attach outcome.
        let fake = bin.join("claude");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/zsh\nprint -r -- \"$@\" >> {rec}\n[[ \"$1\" == attach ]] && exit {attach_exit}\nexit 0\n",
                rec = record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let zdotdir = make_isolated();
        let capture = home.0.join("pty.bin");
        let driver = home.0.join("driver.zsh");
        std::fs::write(
            &driver,
            "emulate -L zsh\n\
             zmodload zsh/zpty || exit 2\n\
             out=$2\n\
             : > $out\n\
             drain() { local c; while zpty -rt n c 2>/dev/null; do print -rn -- \"$c\" >> $out; done }\n\
             zpty n /bin/zsh -i\n\
             sleep 1.5; drain\n\
             zpty -w n \"$1\"\n\
             repeat 25; do sleep 0.1; drain; done\n\
             zpty -d n 2>/dev/null\n",
        )
        .unwrap();

        let status = Command::new("/bin/zsh")
            .arg(driver.to_str().unwrap())
            .arg(command)
            .arg(capture.to_str().unwrap())
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", &home.0)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
            )
            .env("HOST", "test.local")
            .env("NICE_USER_ZDOTDIR", "")
            // Non-empty socket + pane ids: what a real Nice pane injects.
            .env("NICE_SOCKET", home.0.join("nice.sock"))
            .env("NICE_TAB_ID", "t1")
            .env("NICE_PANE_ID", "t1-claude")
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8")
            .current_dir(&home.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn zpty driver");
        assert!(status.success(), "zpty driver failed: {status:?}");

        ShadowRun {
            execs: std::fs::read_to_string(&record)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect(),
            payloads: std::fs::read_to_string(&sent)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect(),
            transcript: String::from_utf8_lossy(
                &std::fs::read(&capture).unwrap_or_default(),
            )
            .escape_debug()
            .to_string(),
        }
    }

    /// The `attach` reply execs `claude attach <first 8 of the uuid>` — and,
    /// when that fails (a jobs entry the daemon left behind), degrades to
    /// `--resume <full uuid>` with the theme pointer rather than stranding the
    /// user at attach's error.
    #[test]
    fn claude_shadow_attach_mode_attaches_then_falls_back_e2e() {
        let uuid = "b8c8244b-e94e-4c38-95fb-31be9a28187e";

        let ok = run_claude_shadow_e2e(
            &format!("attach {uuid} /ptr.json"),
            0,
            &format!("claude --resume {uuid}"),
        );
        assert_eq!(
            ok.execs,
            vec!["attach b8c8244b".to_string()],
            "a successful attach must be the only exec — no resume behind it. pty: <{}>",
            ok.transcript
        );
        // The attached Claude ran as a CHILD, so this shell outlived it: Nice
        // must be told the pane is a prompt again, or its promotion flag stays
        // set and every later `claude` here opens a new tab.
        assert_eq!(
            ok.payloads.last().map(String::as_str),
            Some(r#"{"action":"claude_exited","paneId":"t1-claude"}"#),
            "a returned attach must report the pane back to Nice. payloads: {:?}",
            ok.payloads
        );

        let fell_back = run_claude_shadow_e2e(
            &format!("attach {uuid} /ptr.json"),
            1,
            &format!("claude --resume {uuid}"),
        );
        assert_eq!(
            fell_back.execs,
            vec![
                "attach b8c8244b".to_string(),
                format!("--settings /ptr.json --resume {uuid}"),
            ],
            "a failed attach must degrade to the durable --resume. pty: <{}>",
            fell_back.transcript
        );
        assert!(
            !fell_back
                .payloads
                .iter()
                .any(|p| p.contains("claude_exited")),
            "the fallback EXECS claude — the pty's own exit reports that pane. payloads: {:?}",
            fell_back.payloads
        );
    }

    /// The `resume` reply execs `--resume <uuid>` and DROPS the user's original
    /// `attach <id>` args — they name a session the daemon no longer hosts.
    #[test]
    fn claude_shadow_resume_mode_replaces_the_attach_args_e2e() {
        let uuid = "b8c8244b-e94e-4c38-95fb-31be9a28187e";
        let run = run_claude_shadow_e2e(&format!("resume {uuid}"), 0, "claude attach b8c8244b");
        assert_eq!(
            run.execs,
            vec![format!("--resume {uuid}")],
            "pty: <{}>",
            run.transcript
        );
    }

    /// Launch a real `/bin/zsh` under the synthetic ZDOTDIR with a controlled
    /// `$HOME` and env, returning its stdout. `login_shell` runs `-ilc` so
    /// `.zprofile` / `.zlogin` lookups fire. `NICE_USER_ZDOTDIR` is always set —
    /// empty string when `None` — to match production (which always sets it).
    /// The env is fully replaced (`env_clear`), mirroring Swift's
    /// `proc.environment = [...]`, so no ambient `ZDOTDIR` / `NICE_SOCKET` leaks
    /// into the child. Never touches the real `$HOME`.
    fn run_zsh_under_injection(
        home: &Path,
        nice_user_zdotdir: Option<&str>,
        commands: &str,
        login_shell: bool,
    ) -> String {
        let zdotdir = make_isolated();
        let out = Command::new("/bin/zsh")
            .arg(if login_shell { "-ilc" } else { "-ic" })
            .arg(commands)
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOST", "test.local")
            .env("NICE_USER_ZDOTDIR", nice_user_zdotdir.unwrap_or(""))
            .current_dir(home)
            .output()
            .expect("spawn /bin/zsh");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// End-to-end: launch real zsh and confirm the OSC 7 payload contains the
    /// actual cwd without spurious bytes (the `%` regression sentinel). Uses an
    /// empty sandbox `$HOME` so no user dotfiles are sourced — the emitter still
    /// fires from our stub.
    #[test]
    fn zshrc_emitter_produces_clean_osc7_at_runtime() {
        let zdotdir = make_isolated();
        let home = scratch("nice-osc7-home");
        let workcwd = scratch("nice-osc7-work");

        let out = Command::new("/bin/zsh")
            .arg("-ic")
            .arg("exit")
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", &home.0)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOST", "test.local")
            .current_dir(&workcwd.0)
            .output()
            .expect("spawn /bin/zsh");
        let bytes = out.stdout;

        let osc_start = find_subsequence(&bytes, &[0x1b, 0x5d, 0x37, 0x3b])
            .unwrap_or_else(|| panic!("zsh did not emit OSC 7. Captured: {:?}", &bytes));
        let payload_start = osc_start + 4;
        let bel_rel = bytes[payload_start..]
            .iter()
            .position(|&b| b == 0x07)
            .expect("OSC 7 emission missing BEL terminator");
        let payload =
            String::from_utf8_lossy(&bytes[payload_start..payload_start + bel_rel]).into_owned();

        assert!(
            payload.starts_with("file://"),
            "OSC 7 payload must be a file:// URL. Got: <{payload}>"
        );
        // The workdir has no space/percent, so a clean encoding leaves the last
        // component intact and the whole payload percent-free.
        let last = workcwd.0.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(
            payload.contains(last),
            "payload path must contain the cwd's last component ({last}). Got: <{payload}>"
        );
        assert!(
            !payload.contains('%'),
            "decoded path must not contain `%`. Got: <{payload}>"
        );
    }

    /// THE bug: oh-my-zsh / p10k probe `${ZDOTDIR:-$HOME}/...` while the user's
    /// `.zshrc` is being sourced. ZDOTDIR must already be the user's value at
    /// that point (restored BEFORE the source), not our temp dir.
    #[test]
    fn end_to_end_user_zshrc_sees_restored_zdotdir_during_init() {
        let home = scratch("nice-e2e-home");
        std::fs::write(
            home.0.join(".zshrc"),
            "touch \"${ZDOTDIR:-$HOME}/.during-zshrc-marker\"\n\
             print -r -- \"DURING_ZSHRC_ZDOTDIR=${ZDOTDIR-<unset>}\"\n",
        )
        .unwrap();

        let out = run_zsh_under_injection(&home.0, None, "true", false);

        assert!(
            out.contains("DURING_ZSHRC_ZDOTDIR=<unset>"),
            "ZDOTDIR must be restored BEFORE sourcing user's .zshrc. Output: <{out}>"
        );
        assert!(
            home.0.join(".during-zshrc-marker").is_file(),
            "files written via ${{ZDOTDIR:-$HOME}}/... during user's .zshrc must land in real $HOME"
        );
    }

    /// Default case: no NICE_USER_ZDOTDIR, no custom ZDOTDIR in `~/.zshenv`. The
    /// injection resolves ZDOTDIR to $HOME (unset), so tooling writes to real home.
    #[test]
    fn end_to_end_default_user_zdotdir_resolves_to_home() {
        let home = scratch("nice-e2e-home");

        let out = run_zsh_under_injection(
            &home.0,
            None,
            "touch \"${ZDOTDIR:-$HOME}/.p10k.zsh\"\n\
             print -r -- \"FINAL_ZDOTDIR=${ZDOTDIR-<unset>}\"",
            false,
        );

        assert!(
            out.contains("FINAL_ZDOTDIR=<unset>"),
            "default user: expected ZDOTDIR unset by .zshrc restore. Output: <{out}>"
        );
        assert!(
            home.0.join(".p10k.zsh").is_file(),
            "expected .p10k.zsh to land in the real home, not our temp dir"
        );
    }

    /// XDG-style: user sets `export ZDOTDIR=~/.config/zsh` in `~/.zshenv`. The
    /// injection sources that during discovery and resolves to the custom path.
    #[test]
    fn end_to_end_xdg_style_zdotdir_honored_from_zshenv() {
        let home = scratch("nice-e2e-home");
        let custom = home.0.join(".config/zsh");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(home.0.join(".zshenv"), r#"export ZDOTDIR="$HOME/.config/zsh""#).unwrap();
        std::fs::write(custom.join(".zshrc"), "echo NICE-XDG-ZSHRC-LOADED").unwrap();

        let out = run_zsh_under_injection(
            &home.0,
            None,
            r#"print -r -- "FINAL_ZDOTDIR=$ZDOTDIR""#,
            false,
        );

        assert!(
            out.contains("NICE-XDG-ZSHRC-LOADED"),
            "custom ZDOTDIR's .zshrc must be sourced. Output: <{out}>"
        );
        assert!(
            out.contains(&format!("FINAL_ZDOTDIR={}", custom.display())),
            "ZDOTDIR must be restored to the user's intended XDG path. Output: <{out}>"
        );
    }

    /// Login-shell bonus fix: our `.zprofile` chains through
    /// `$NICE_USER_ZDOTDIR/.zprofile` so login-shell users keep their `~/.zprofile`.
    #[test]
    fn end_to_end_login_shell_sources_user_zprofile() {
        let home = scratch("nice-e2e-home");
        std::fs::write(home.0.join(".zprofile"), "echo NICE-ZPROFILE-LOADED").unwrap();

        let out = run_zsh_under_injection(&home.0, None, "true", true);

        assert!(
            out.contains("NICE-ZPROFILE-LOADED"),
            "login shells must source ~/.zprofile through the synthetic stub. Output: <{out}>"
        );
    }

    /// launchctl-style: Nice inherited a ZDOTDIR from its launch env, passed as
    /// NICE_USER_ZDOTDIR; the shell restores that value verbatim.
    #[test]
    fn end_to_end_launchctl_style_zdotdir_honored_from_env() {
        let home = scratch("nice-e2e-home");
        let custom = home.0.join("launchctl-zsh");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join(".zshrc"), "echo NICE-LAUNCHCTL-ZSHRC-LOADED").unwrap();

        let out = run_zsh_under_injection(
            &home.0,
            Some(custom.to_str().unwrap()),
            r#"print -r -- "FINAL_ZDOTDIR=$ZDOTDIR""#,
            false,
        );

        assert!(
            out.contains("NICE-LAUNCHCTL-ZSHRC-LOADED"),
            "launchctl-style: custom ZDOTDIR's .zshrc must be sourced. Output: <{out}>"
        );
        assert!(
            out.contains(&format!("FINAL_ZDOTDIR={}", custom.display())),
            "launchctl-style: ZDOTDIR must be restored from NICE_USER_ZDOTDIR. Output: <{out}>"
        );
    }

    // ---- Command Compose: static pins ---------------------------------------

    /// The widget exists, is registered as a ZLE widget, and is bound to the
    /// trigger in ALL THREE keymaps — with the zsh-side trigger text derived
    /// from the Rust constant so the two sides can never drift.
    #[test]
    fn zshrc_compose_defines_widget_and_binds_trigger_in_all_keymaps() {
        let body = ZSHRC_BODY;
        assert!(body.contains("_nice_command_compose() {"));
        assert!(body.contains("zle -N _nice_command_compose"));
        for keymap in ["emacs", "viins", "vicmd"] {
            let line = format!("bindkey -M {keymap} '{COMPOSE_TRIGGER_BINDKEY}' _nice_command_compose");
            assert!(body.contains(&line), "missing: {line}");
        }
        // Byte agreement: `\e` + the rest of the bindkey text == the pty bytes.
        let mut expected = vec![0x1b_u8];
        expected.extend_from_slice(COMPOSE_TRIGGER_BINDKEY.strip_prefix(r"\e").unwrap().as_bytes());
        assert_eq!(COMPOSE_TRIGGER_SEQ, expected.as_slice());
    }

    /// The never-auto-execute invariant as a pinned NEGATIVE: no code path in
    /// the injected rc may accept the line — running the composed command is
    /// always the user's own Enter.
    #[test]
    fn zshrc_compose_never_accepts_line() {
        assert!(
            !ZSHRC_BODY.contains("accept-line"),
            "the injected rc must never call zle accept-line"
        );
    }

    /// The user's text reaches claude via STDIN (a herestring off a `$BUFFER`
    /// copy), never argv — so no quoting of user text can ever be wrong — and
    /// the flags ride `command claude -p` (shadow-proof; the shadow's `-p`
    /// passthrough would also be safe, `command` makes it a non-question).
    #[test]
    fn zshrc_compose_pipes_buffer_via_stdin() {
        let body = ZSHRC_BODY;
        assert!(body.contains(r#"local request="$BUFFER""#));
        assert!(body.contains(r#"_nice_compose_translate <<< "$request""#));
        assert!(body.contains(r#"command claude -p "$_nice_compose_instruction""#));
    }

    /// An empty buffer is a no-op — no subprocess, no spinner.
    #[test]
    fn zshrc_compose_empty_buffer_is_noop() {
        assert!(ZSHRC_BODY.contains(r#"[[ -z "$BUFFER" ]] && return 0"#));
    }

    /// The widget reads its runtime knobs from `$NICE_COMPOSE_CONF` (accent for
    /// the spinner, model/effort for the flags) and abandons in-flight composes
    /// from a precmd hook (Enter / ctrl-c mid-compose).
    #[test]
    fn zshrc_compose_reads_conf_and_hooks_precmd() {
        let body = ZSHRC_BODY;
        assert!(body.contains("NICE_COMPOSE_CONF"));
        assert!(body.contains("precmd_functions+=(_nice_compose_precmd)"));
    }

    // ---- Command Compose: real-zsh end-to-end -------------------------------

    /// Run real `/bin/zsh -ic` under the injection with a scratch bin dir
    /// prepended to PATH and optional extra env — the compose flavor of
    /// [`run_zsh_under_injection`] (which pins PATH and takes no extra env).
    fn run_zsh_compose(home: &Path, bin: &Path, extra_env: &[(&str, &str)], commands: &str) -> String {
        let zdotdir = make_isolated();
        let mut cmd = Command::new("/bin/zsh");
        cmd.arg("-ic")
            .arg(commands)
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", home)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
            )
            .env("HOST", "test.local")
            .env("NICE_USER_ZDOTDIR", "")
            .current_dir(home);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn /bin/zsh");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        // The injected rc emits its startup OSC 7 (…BEL) before the command's
        // own output — strip through the last BEL so assertions see only the
        // command's stdout.
        match stdout.rfind('\u{07}') {
            Some(i) => stdout[i + 1..].to_string(),
            None => stdout,
        }
    }

    /// Write an executable fake `claude` into `bin` that records its argv and
    /// stdin to files and prints `reply` (exit 0) — or exits 1 with no output.
    fn write_fake_claude(bin: &Path, record_dir: &Path, reply: &str, fail: bool) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin).unwrap();
        let body = if fail {
            "#!/bin/zsh\nexit 1\n".to_string()
        } else {
            format!(
                "#!/bin/zsh\nprint -r -- \"$@\" > {rec}/argv\ncat > {rec}/stdin\nprint -rn -- {reply:?}\n",
                rec = record_dir.display()
            )
        };
        let path = bin.join("claude");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// End-to-end: `_nice_compose_translate` pipes the request through the fake
    /// claude's STDIN, passes `-p` + the conf file's `--model`/`--effort`, and
    /// prints the reply verbatim.
    #[test]
    fn compose_translate_pipes_stdin_and_conf_flags_e2e() {
        let home = scratch("nice-compose-home");
        let bin = home.0.join("bin");
        let rec = scratch("nice-compose-rec");
        write_fake_claude(&bin, &rec.0, "ls -la", false);
        let conf = home.0.join("compose.json");
        std::fs::write(&conf, r##"{"accent":"#7a94db","model":"opus","effort":"high"}"##).unwrap();

        let out = run_zsh_compose(
            &home.0,
            &bin,
            &[("NICE_COMPOSE_CONF", conf.to_str().unwrap())],
            r#"_nice_compose_translate <<< "list files with details""#,
        );

        assert_eq!(out, "ls -la", "translate prints the model reply verbatim");
        let argv = std::fs::read_to_string(rec.0.join("argv")).expect("fake claude ran");
        assert!(argv.contains("-p"), "argv carries -p. Got: <{argv}>");
        assert!(argv.contains("--model opus"), "conf model rides argv. Got: <{argv}>");
        assert!(argv.contains("--effort high"), "conf effort rides argv. Got: <{argv}>");
        let stdin = std::fs::read_to_string(rec.0.join("stdin")).unwrap();
        assert_eq!(
            stdin, "list files with details\n",
            "the user text reaches claude on stdin, never argv"
        );
        assert!(
            !argv.contains("list files"),
            "the user text must NOT appear on the command line"
        );
    }

    /// Without a conf file, translate still runs — bare `claude -p`, no flags
    /// (the widget's built-in fallback); a failing claude yields empty output.
    #[test]
    fn compose_translate_no_conf_and_failure_e2e() {
        let home = scratch("nice-compose-noconf-home");
        let bin = home.0.join("bin");
        let rec = scratch("nice-compose-noconf-rec");
        write_fake_claude(&bin, &rec.0, "echo hi", false);

        let out = run_zsh_compose(&home.0, &bin, &[], r#"_nice_compose_translate <<< "say hi""#);
        assert_eq!(out, "echo hi");
        let argv = std::fs::read_to_string(rec.0.join("argv")).unwrap();
        assert!(!argv.contains("--model"), "no conf ⇒ no --model. Got: <{argv}>");
        assert!(!argv.contains("--effort"), "no conf ⇒ no --effort. Got: <{argv}>");

        // Failure path: claude exits non-zero with no output ⇒ empty result
        // (the ZLE handler shows the failure message and leaves the buffer).
        let fail_bin = home.0.join("failbin");
        write_fake_claude(&fail_bin, &rec.0, "", true);
        let out = run_zsh_compose(&home.0, &fail_bin, &[], r#"_nice_compose_translate <<< "x""#);
        assert_eq!(out, "", "a failing claude yields empty translate output");
    }

    /// `_nice_compose_strip` unwraps fences/backticks and trims — driven through
    /// real zsh so the parameter-expansion arcana is the thing under test.
    #[test]
    fn compose_strip_unwraps_fences_e2e() {
        let home = scratch("nice-compose-strip-home");
        let bin = home.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();

        let script = concat!(
            "print -rn -- \"$(_nice_compose_strip $'```zsh\\nls -la\\n```')\"\n",
            "print -rn -- '|'\n",
            "print -rn -- \"$(_nice_compose_strip $'  \\tfind . -name \"*.rs\"\\n')\"\n",
            "print -rn -- '|'\n",
            "print -rn -- \"$(_nice_compose_strip '`echo hi`')\"\n",
            "print -rn -- '|'\n",
            // A multi-line command inside a fence survives with its inner newline.
            "print -rn -- \"$(_nice_compose_strip $'```\\nfor f in *; do\\n  echo $f\\ndone\\n```')\"\n",
        );
        let out = run_zsh_compose(&home.0, &bin, &[], script);
        let parts: Vec<&str> = out.split('|').collect();
        assert_eq!(parts[0], "ls -la", "fence unwrapped");
        assert_eq!(parts[1], r#"find . -name "*.rs""#, "whitespace trimmed");
        assert_eq!(parts[2], "echo hi", "wrapping backticks stripped");
        assert_eq!(
            parts[3],
            "for f in *; do\n  echo $f\ndone",
            "multi-line composition survives the fence strip"
        );
    }

    /// The zsh conf parser and the Rust writer's parser agree key-for-key on a
    /// production-shaped blob (the app↔shell interchange pin).
    #[test]
    fn compose_conf_get_matches_rust_parser_e2e() {
        let home = scratch("nice-compose-conf-home");
        let bin = home.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let blob = r##"{"accent":"#c96442","model":"sonnet","effort":"medium"}"##;
        let conf = home.0.join("compose.json");
        std::fs::write(&conf, blob).unwrap();

        let script = "print -rn -- \"$(_nice_compose_conf_get accent)|$(_nice_compose_conf_get model)|$(_nice_compose_conf_get effort)\"";
        let out = run_zsh_compose(
            &home.0,
            &bin,
            &[("NICE_COMPOSE_CONF", conf.to_str().unwrap())],
            script,
        );
        let zsh_values: Vec<&str> = out.split('|').collect();
        for (i, key) in ["accent", "model", "effort"].iter().enumerate() {
            assert_eq!(
                Some(zsh_values[i].to_string()),
                crate::compose_conf::parse_value(blob, key),
                "zsh and Rust parsers agree on {key}"
            );
        }
    }

    /// Full-visual e2e in a REAL pty (zsh's zpty module drives an interactive
    /// child — ZLE only paints under a pty, so `-ic` tests can't see this):
    /// trigger a compose and assert the spinner line is painted in the conf
    /// accent as a truecolor SGR on the wire. Pins the region_highlight offset
    /// semantics — `P` offsets index PREDISPLAY (not POSTDISPLAY), so the
    /// spinner span must be anchored at ${#BUFFER} with no P flag; the P form
    /// highlighted nothing, in every terminal.
    #[test]
    fn compose_spinner_paints_accent_in_real_pty_e2e() {
        use std::process::Stdio;

        let home = scratch("nice-compose-pty-home");
        let bin = home.0.join("bin");
        let rec = scratch("nice-compose-pty-rec");
        std::fs::create_dir_all(&bin).unwrap();
        // Slow reply so the spinner paints several frames before the apply.
        {
            use std::os::unix::fs::PermissionsExt;
            let path = bin.join("claude");
            std::fs::write(
                &path,
                format!(
                    "#!/bin/zsh\ncat > {rec}/stdin\nsleep 0.8\nprint -rn -- \"ls -la\"\n",
                    rec = rec.0.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let conf = home.0.join("compose.json");
        std::fs::write(
            &conf,
            r##"{"accent":"#c96442","model":"sonnet","effort":"medium"}"##,
        )
        .unwrap();

        let zdotdir = make_isolated();
        let capture = home.0.join("raw.bin");
        let driver = home.0.join("driver.zsh");
        std::fs::write(
            &driver,
            format!(
                r#"emulate -L zsh
zmodload zsh/zpty || exit 2
out=$1
: > $out
drain() {{ local c; while zpty -rt n c 2>/dev/null; do print -rn -- "$c" >> $out; done }}
zpty n /bin/zsh -i
sleep 1; drain
zpty -w -n n "list all files with details"
sleep 0.3; drain
zpty -w -n n $'{trigger}'
repeat 25; do sleep 0.1; drain; done
zpty -d n 2>/dev/null
"#,
                trigger = COMPOSE_TRIGGER_BINDKEY
            ),
        )
        .unwrap();

        let status = Command::new("/bin/zsh")
            .arg(driver.to_str().unwrap())
            .arg(capture.to_str().unwrap())
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", &home.0)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
            )
            .env("HOST", "test.local")
            .env("NICE_USER_ZDOTDIR", "")
            .env("NICE_COMPOSE_CONF", conf.to_str().unwrap())
            // What Nice's spawn env really sets (nice-term-core spawn.rs).
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "en_US.UTF-8")
            .current_dir(&home.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn zpty driver");
        assert!(status.success(), "zpty driver failed: {status:?}");

        let bytes = std::fs::read(&capture).expect("read pty capture");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("Composing"),
            "spinner line never painted. Captured: <{}>",
            text.escape_debug()
        );
        // #c96442 → SGR 38;2;201;100;66 (the fg truecolor form zsh emits).
        assert!(
            text.contains("38;2;201;100;66"),
            "spinner must be painted in the conf accent as truecolor. Captured: <{}>",
            text.escape_debug()
        );
        assert!(
            text.contains("ls -la"),
            "composed command never landed in the buffer. Captured: <{}>",
            text.escape_debug()
        );
        // The reply landed via apply (buffer replace), not execution: the fake
        // claude recorded the request, and no `total`-style ls output follows.
        let stdin = std::fs::read_to_string(rec.0.join("stdin")).unwrap();
        assert_eq!(stdin, "list all files with details\n");
    }
}
