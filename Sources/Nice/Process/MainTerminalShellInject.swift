//
//  MainTerminalShellInject.swift
//  Nice
//
//  Phase 7: writes a per-launch `ZDOTDIR` directory the Main Terminal's
//  zsh picks up. The directory contains stub `.zshenv` / `.zprofile` /
//  `.zlogin` / `.zshrc` that first chain back to the user's real
//  startup files in `$HOME`, then (in `.zshrc`) define a `claude()`
//  function that intercepts *interactive* invocations and forwards
//  them to Nice's control socket so a new tab opens instead of the
//  shell exec'ing claude in place. Every interactive `claude` — whether
//  typed in the built-in Terminals tab or in a companion terminal
//  inside an existing Claude tab — opens its own new tab.
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

    // Each stub explicitly sources the user's real file. When `ZDOTDIR`
    // is set, zsh reads startup files from there and stops consulting
    // `$HOME/.*`, so without these chain-backs the user would lose
    // their PATH, aliases, plugins, etc.

    private static let zshenvBody = """
    # Nice: chain back to the user's real .zshenv.
    [[ -f "$HOME/.zshenv" ]] && source "$HOME/.zshenv"
    """

    private static let zprofileBody = """
    # Nice: chain back to the user's real .zprofile.
    [[ -f "$HOME/.zprofile" ]] && source "$HOME/.zprofile"
    """

    private static let zloginBody = """
    # Nice: chain back to the user's real .zlogin.
    [[ -f "$HOME/.zlogin" ]] && source "$HOME/.zlogin"
    """

    private static let zshrcBody = #"""
    # Nice: chain back to the user's real .zshrc, then shadow `claude`
    # so running it handshakes with Nice over NICE_SOCKET. The socket
    # either tells us to exit (Nice is opening a new tab) or to exec
    # claude in place (Nice is promoting this pane to Claude).
    [[ -f "$HOME/.zshrc" ]] && source "$HOME/.zshrc"

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

    # Nice: if the app asked us to pre-type a command at the next
    # prompt (set when a restored Claude tab boots), push it onto zsh's
    # line-editor buffer. The user sees the command typed and ready;
    # nothing runs until they hit Enter.
    if [[ -n "$NICE_PREFILL_COMMAND" ]]; then
        print -z "$NICE_PREFILL_COMMAND"
    fi
    """#
}
