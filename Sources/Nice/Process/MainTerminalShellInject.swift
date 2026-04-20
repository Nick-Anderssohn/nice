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
    # so running it in the Main Terminal opens a new tab instead of
    # exec'ing the CLI in place.
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
        # Passthrough to the real binary (no new tab) when:
        #   1. Not inside a Nice Main Terminal ($NICE_SOCKET unset).
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

        # Always ask Nice to open a new tab rooted at PWD. Running
        # `claude` from a companion terminal inside an existing Nice
        # tab would otherwise add a second Claude pane there; we want
        # the invariant "at most one Claude pane per tab", so every
        # interactive `claude` invocation opens its own tab.
        local cwd_json
        cwd_json=$(_nice_json_escape "$PWD")
        local payload="{\"action\":\"newtab\",\"cwd\":${cwd_json},\"args\":${args_json}}"
        if ! printf '%s\n' "$payload" | nc -U "$NICE_SOCKET" -w 1 2>/dev/null; then
            print -u2 "nice: control socket unreachable; running claude directly"
            command claude "$@"
        fi
    }
    """#
}
