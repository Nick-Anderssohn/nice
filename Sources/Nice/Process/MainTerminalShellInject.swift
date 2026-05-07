//
//  MainTerminalShellInject.swift
//  Nice
//
//  Phase 7: writes a per-launch `ZDOTDIR` directory the Main Terminal's
//  zsh picks up. The directory contains stub `.zshenv` / `.zprofile` /
//  `.zlogin` / `.zshrc` that chain back to the user's real startup
//  files (resolved from `$NICE_USER_ZDOTDIR` if set, else by sourcing
//  `~/.zshenv` to discover the user's intended `ZDOTDIR`), then (in
//  `.zshrc`) restore `ZDOTDIR` to that intended value and define a
//  `claude()` function that intercepts *interactive* invocations and
//  forwards them to Nice's control socket so a new tab opens instead
//  of the shell exec'ing claude in place. Every interactive `claude`
//  — whether typed in the built-in Terminals tab or in a companion
//  terminal inside an existing Claude tab — opens its own new tab.
//
//  The "restore `ZDOTDIR` *before sourcing user's .zshrc*" dance is
//  what stops shell tools (Powerlevel10k, oh-my-zsh, nvm, asdf,
//  starship init…) from scribbling on our temp dir when they probe
//  `${ZDOTDIR:-$HOME}/...` — both at the interactive prompt AND
//  during user's `.zshrc` init (oh-my-zsh sets `ZSH_COMPDUMP=
//  "${ZDOTDIR:-$HOME}/.zcompdump-..."` at load time, p10k sources
//  `${ZDOTDIR:-$HOME}/.p10k.zsh`, etc.). Ordering "restore → source
//  user's .zshrc → install our hooks" gives us correctness for those
//  init-time probes and also lets our `claude()` shadow / OSC 7
//  hook layer on top of (and survive) anything the user defines.
//
//  Trade-off: this means `exec zsh` inside a Nice pane drops our
//  injection (the new zsh runs with the user's restored ZDOTDIR, not
//  our temp value), so users who re-exec the shell mid-session lose
//  `claude()` and OSC 7 until they open a new tab. The same caveat
//  applies to most terminal-app shell integration (iTerm2, VS Code's
//  terminal). Pre-fix, `exec zsh` retained the hooks because the
//  temp ZDOTDIR was still in env; we accept this regression as the
//  cost of doing the bigger fix.
//
//  Known limitation: `/etc/zshenv` setting `ZDOTDIR` bypasses our
//  injection entirely (zsh re-resolves `$ZDOTDIR/.zshenv` from the
//  new value before reading our stub). macOS ships no `/etc/zshenv`
//  and setting `ZDOTDIR` system-wide is non-idiomatic; documented
//  rather than fixed.
//

import Foundation

enum MainTerminalShellInject {
    /// Write the ZDOTDIR contents for this launch and return its path.
    /// The `$NICE_SOCKET` env var the `claude()` function reads is
    /// injected separately — the script just references it.
    static func make() throws -> URL {
        let dir = URL(
            fileURLWithPath: NSTemporaryDirectory(),
            isDirectory: true
        ).appendingPathComponent("nice-zdotdir-\(getpid())", isDirectory: true)

        try FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true, attributes: nil
        )

        try zshenvBody.write(
            to: dir.appendingPathComponent(".zshenv"),
            atomically: true, encoding: .utf8
        )
        try zprofileBody.write(
            to: dir.appendingPathComponent(".zprofile"),
            atomically: true, encoding: .utf8
        )
        try zloginBody.write(
            to: dir.appendingPathComponent(".zlogin"),
            atomically: true, encoding: .utf8
        )
        try zshrcBody.write(
            to: dir.appendingPathComponent(".zshrc"),
            atomically: true, encoding: .utf8
        )

        return dir
    }

    // MARK: - File bodies

    // The four stubs cooperate so that:
    //   1. zsh keeps reading our temp-dir stubs (we control startup).
    //   2. The user's real .zshenv/.zprofile/.zshrc/.zlogin all run.
    //   3. By the time the shell is interactive, $ZDOTDIR points at
    //      whatever the *user* intended (XDG-style custom location,
    //      launchctl-set value, or just $HOME) — never our temp dir —
    //      so tools like p10k that probe ${ZDOTDIR:-$HOME}/.zshrc
    //      write to the right files instead of our soon-to-be-deleted
    //      temp dir.
    //
    // The "user's intended ZDOTDIR" is resolved in our .zshenv: prefer
    // `$NICE_USER_ZDOTDIR` (Nice captured this from its own process env
    // before overriding ZDOTDIR for the pty), and fall back to sourcing
    // `~/.zshenv` ourselves to honor users who set it there
    // (XDG-style — most common). The resolved value is stashed back
    // into NICE_USER_ZDOTDIR so the later stubs can see it.

    private static let zshenvBody = """
    # Nice: discover and stash the user's intended ZDOTDIR, then
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
    unset NICE_TEMP_ZDOTDIR USER_ZDOTDIR
    """

    private static let zprofileBody = """
    # Nice: source the user's real .zprofile from the location resolved
    # in our .zshenv. (Without this, login-shell users silently lose
    # .zprofile because zsh's $ZDOTDIR/.zprofile lookup hits our stub.)
    [[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zprofile" ]] \\
        && source "$NICE_USER_ZDOTDIR/.zprofile"
    """

    private static let zloginBody = """
    # Nice: defensive — if our .zshrc somehow exited before restoring
    # ZDOTDIR (user .zshrc errored out, etc.), source the user's real
    # .zlogin from where they actually keep it. In the success path
    # ZDOTDIR has already been restored to the user's value by our
    # .zshrc, so zsh reads the user's .zlogin directly and this stub
    # is never reached.
    [[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zlogin" ]] \\
        && source "$NICE_USER_ZDOTDIR/.zlogin"
    """

    private static let zshrcBody = #"""
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

        local mode sid
        read -r mode sid <<< "$response"
        case "$mode" in
            newtab)
                # Nice is opening a new sidebar tab; nothing to do here.
                return 0
                ;;
            inplace)
                # Nice promoted this pane to Claude. If it minted a
                # session id for us, prepend --session-id so it can
                # resume the session later; otherwise the user's own
                # args (e.g. --resume <uuid>) already identify it.
                if [[ -n "$sid" ]]; then
                    exec command claude --session-id "$sid" "$@"
                else
                    exec command claude "$@"
                fi
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
    """#
}
