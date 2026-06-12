//
//  SkillInstaller.swift
//  Nice
//
//  Installs (or removes) the global `/nice-handoff` Claude Code skill
//  and its companion shell helper. The skill lets Claude hand off the
//  current work to a fresh session in a new Nice tab: it writes a
//  handoff file capturing the current state, then shells out to the
//  helper, which posts a `handoff` message to Nice's control socket.
//
//  Two components are managed together:
//    • `~/.claude/skills/nice-handoff/SKILL.md` — the skill definition.
//      Claude Code discovers skills in `~/.claude/skills/` and treats
//      each subdirectory as one skill; the `SKILL.md` file carries
//      the frontmatter (`name`, `description`) and the prose instructions
//      that tell Claude how to run the handoff. Nice owns the entire
//      `nice-handoff/` directory and removes it on uninstall.
//    • `~/.nice/nice-handoff.sh` — the shell helper invoked by the skill.
//      Lives in the same no-space dotdir as `nice-claude-hook.sh` (see
//      ClaudeHookInstaller for the word-split rationale). Nice owns
//      only the file, not the directory — `~/.nice/` is shared with the
//      hook installer and must never be deleted here.
//
//  Idempotency: `install()` is safe to call on every launch. Each file
//  is rewritten only when its body changed; the on-disk mtime stays
//  stable across no-op launches.
//
//  Controlled by `Tweaks.installHandoffSkill`. `sync(enabled:)` is the
//  single production call site; tests pass explicit URLs to sandbox.
//

import Foundation

enum SkillInstaller {

    // MARK: - Public entry point

    /// Install or remove the skill depending on `enabled`. Resolves
    /// default paths via `NSHomeDirectory`. Call from the main actor
    /// whenever the toggle changes (or once at bootstrap to reconcile
    /// any drift between the on-disk state and the persisted flag).
    /// Failures are logged and swallowed — the app runs fine without
    /// the skill; only the `/nice-handoff` feature degrades.
    static func sync(enabled: Bool) {
        sync(
            enabled: enabled,
            skillDir: defaultSkillDir(),
            helperDir: defaultHelperDir()
        )
    }

    /// Test-friendly entry point. Production calls `sync(enabled:)` which
    /// resolves both directories off `homeBase()` (NSHomeDirectory, or
    /// the `NICE_HOME_OVERRIDE` test redirect); tests pass URLs directly
    /// so they can sandbox without touching the developer's real
    /// `~/.claude/skills/` or `~/.nice/`.
    static func sync(enabled: Bool, skillDir: URL, helperDir: URL) {
        if enabled {
            install(skillDir: skillDir, helperDir: helperDir)
        } else {
            uninstall(skillDir: skillDir, helperDir: helperDir)
        }
    }

    // MARK: - Install

    /// Write the skill dir + `SKILL.md` and the helper script. Each file
    /// is content-compared on disk: no write (and no mtime churn) when
    /// the bytes are unchanged. Executable perms are set on the helper
    /// via `setAttributes` so the skill can invoke it directly.
    static func install(skillDir: URL, helperDir: URL) {
        do {
            try ensureSkillInstalled(in: skillDir)
            try ensureHelperInstalled(in: helperDir)
        } catch {
            NSLog("SkillInstaller: install failed: \(error)")
        }
    }

    /// Create `skillDir` if needed and write `SKILL.md` only when the
    /// on-disk content differs from `skillMarkdown`. Returns without
    /// writing when the file matches — keeps mtime stable on no-op
    /// launches.
    private static func ensureSkillInstalled(in dir: URL) throws {
        try FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let fileURL = dir.appendingPathComponent(skillFilename)
        let existing = try? String(contentsOf: fileURL, encoding: .utf8)
        if existing == skillMarkdown {
            return
        }
        try skillMarkdown.write(to: fileURL, atomically: true, encoding: .utf8)
    }

    /// Create `helperDir` if needed and write `helperScript` only when
    /// the on-disk content differs. Resets permissions to 0o755 only
    /// when the file is (re)written — keeps perms stable on no-op runs.
    private static func ensureHelperInstalled(in dir: URL) throws {
        try FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let fileURL = dir.appendingPathComponent(helperFilename)
        let path = fileURL.path
        let existing = try? String(contentsOf: fileURL, encoding: .utf8)
        if existing == helperScript {
            return
        }
        try helperScript.write(to: fileURL, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o755)],
            ofItemAtPath: path
        )
    }

    // MARK: - Uninstall

    /// Remove Nice's skill dir and the helper script. Logs on failure and
    /// continues — missing files are not an error (uninstall is idempotent),
    /// and permission failures are noted but not fatal. Does NOT remove
    /// `~/.nice/` itself; that directory is shared with ClaudeHookInstaller.
    static func uninstall(skillDir: URL, helperDir: URL) {
        let fm = FileManager.default
        let helperURL = helperDir.appendingPathComponent(helperFilename)

        // Remove the entire nice-handoff skill directory (Nice owns that
        // name unconditionally; no user content should ever live there).
        if fm.fileExists(atPath: skillDir.path) {
            do {
                try fm.removeItem(at: skillDir)
            } catch {
                NSLog("SkillInstaller: could not remove skill dir \(skillDir.path): \(error)")
            }
        }

        // Remove only our helper file; leave any other ~/.nice/ contents
        // untouched (e.g. nice-claude-hook.sh from ClaudeHookInstaller).
        if fm.fileExists(atPath: helperURL.path) {
            do {
                try fm.removeItem(at: helperURL)
            } catch {
                NSLog("SkillInstaller: could not remove helper \(helperURL.path): \(error)")
            }
        }
    }

    // MARK: - Skill content

    /// The SKILL.md written into `~/.claude/skills/nice-handoff/SKILL.md`.
    ///
    /// Frontmatter keys:
    ///   `name`        — the slash-command name (`/nice-handoff`).
    ///   `description` — how Claude Code decides when to auto-suggest or
    ///                   self-invoke the skill; keep it tightly scoped to
    ///                   the handoff use case.
    ///
    /// No `disable-model-invocation` key: agents must be able to fire this
    /// skill themselves when they detect a full context window, which is
    /// exactly the primary use case.
    ///
    /// The body gives Claude imperative, step-by-step instructions so
    /// there is no ambiguity about what to write, where, or what to call.
    static let skillMarkdown = #"""
    ---
    name: nice-handoff
    description: Hand off the current work to a fresh Claude session in a new Nice tab. Use when the context window is getting full, or when the user asks to hand off / continue work in a clean session. Writes a handoff file capturing the current state and opens a new nested tab that picks up where this one left off.
    ---

    Follow these steps exactly to hand off to a fresh session:

    ## 1. Write the handoff file

    Create the directory `.claude/handoff/` inside the current working
    directory if it does not already exist. Then write a handoff file at:

    ```
    .claude/handoff/handoff-<UTC timestamp>.md
    ```

    where `<UTC timestamp>` uses the format `20060102-150405` (year, month,
    day, hyphen, hour, minute, second — all in UTC, zero-padded). Example:
    `handoff-20240315-143022.md`.

    The file must be thorough enough that a fresh Claude session with **no
    prior context** can continue the work without asking clarifying questions.
    Include all of:

    - **Overall goal / task** — what is being built or accomplished and why.
    - **What has been done so far** — completed steps, decisions made, and
      their rationale.
    - **Current state** — exactly where things stand right now (files edited,
      commands run, outstanding changes, build/test status).
    - **Concrete next steps** — an ordered list of what the new session
      should do first.
    - **Key files and paths** — every file that is central to the task,
      with a one-line note about its role.
    - **Gotchas and things to watch out for** — constraints, traps,
      non-obvious decisions, or anything the new session must know to avoid
      repeating mistakes.

    ## 2. Open the handoff tab

    Run the helper, passing the **absolute path** to the handoff file you
    just wrote as the first argument, and forwarding any arguments the user
    provided to this skill verbatim as the second argument:

    ```
    ~/.nice/nice-handoff.sh "<absolute path to the handoff file>" "$ARGUMENTS"
    ```

    If the user provided no arguments to this skill, pass an empty string
    for the second argument:

    ```
    ~/.nice/nice-handoff.sh "<absolute path to the handoff file>" ""
    ```

    The second argument lets the user customise what the new session does
    after reading the handoff file. When it is empty the new session will
    read the handoff file and then wait for the user to say how to proceed —
    it does not start working on its own. When the user passes a custom
    instruction (e.g. `/nice-handoff keep going` or `/nice-handoff focus only
    on the UI layer`) that string tells the new session what to do after
    reading the file, so it can continue the work right away.

    ## 3. Report back

    Tell the user that the handoff tab is opening (or relay any error the
    helper printed to stderr). Keep it brief — one or two sentences.
    """#

    // MARK: - Helper script content

    /// The bash helper written to `~/.nice/nice-handoff.sh`.
    ///
    /// The script posts a `handoff` action to Nice's control socket so
    /// Nice can open a new tab pre-loaded with the handoff file. It mirrors
    /// the robustness conventions of `nice-claude-hook.sh`:
    ///   • `set -u` surfaces typos without blocking the guard exits.
    ///   • `${VAR:-}` form for env-var guards so `set -u` doesn't fire on
    ///     unset variables that are intentionally absent outside Nice.
    ///   • Pure-sed JSON escaping — no `jq` dependency, consistent with
    ///     the hook script.
    ///   • `nc -U -w 2` with a reply check that surfaces socket errors to
    ///     the user rather than silently succeeding.
    ///
    /// JSON-escape approach: three sed passes — backslash first (so the
    /// later escapes don't double-escape it), then double-quote, then
    /// tab. Embedded newlines (possible in `$INSTRUCTIONS`) are handled
    /// by a fourth sed pass using BSD sed's hold-space join idiom.
    /// Carriage-return is intentionally not escaped: macOS file paths
    /// never carry CR, and Claude's skill arguments are shell-expanded
    /// strings that don't produce CR in practice. This mirrors the
    /// sed-only approach of `nice-claude-hook.sh` (no `jq` dependency).
    static let helperScript = #"""
    #!/usr/bin/env bash
    # nice-handoff.sh — opens a new Nice tab pre-loaded with a handoff file
    # so a fresh Claude session can continue the current work. Posts a JSON
    # `handoff` message to Nice's control socket.
    # Installed automatically by Nice; safe to delete.
    set -u

    if [ -z "${NICE_SOCKET:-}" ] || [ -z "${NICE_PANE_ID:-}" ]; then
      printf 'nice: not running inside a Nice pane; cannot open a handoff tab\n' >&2
      exit 1
    fi

    HANDOFF_FILE="${1:-}"
    if [ -z "$HANDOFF_FILE" ]; then
      printf 'usage: nice-handoff.sh <absolute-path-to-handoff-file> [instructions]\n' >&2
      exit 1
    fi

    INSTRUCTIONS="${2:-}"

    # JSON-escape a single string value (without surrounding quotes).
    # Passes in order:
    #   1. Backslash — must come first; later passes introduce `\` bytes
    #      that must not be double-escaped.
    #   2. Double-quote — required by JSON.
    #   3. Tab — literal horizontal-tab character → the two-char sequence \t.
    #   4. Newline — BSD sed hold-space join: accumulates all lines into
    #      hold space, swaps at EOF, then replaces literal newlines with \n.
    #      Handles multi-line instructions gracefully; a no-op for the
    #      common single-line case.
    # `printf '%s'` avoids shell word-splitting and glob-expansion on the
    # input; `sed` receives the raw bytes without shell interpretation.
    _nice_esc() {
      printf '%s' "$1" \
        | /usr/bin/sed 's/\\/\\\\/g' \
        | /usr/bin/sed 's/"/\\"/g' \
        | /usr/bin/sed 's/	/\\t/g' \
        | /usr/bin/sed -e 'H;1h;$!d;x' -e 's/\n/\\n/g'
    }

    HANDOFF_ESC=$(_nice_esc "$HANDOFF_FILE")
    INSTRUCTIONS_ESC=$(_nice_esc "$INSTRUCTIONS")
    CWD_ESC=$(_nice_esc "$PWD")
    TAB_ID_ESC=$(_nice_esc "${NICE_TAB_ID:-}")
    PANE_ID_ESC=$(_nice_esc "$NICE_PANE_ID")

    PAYLOAD=$(printf '{"action":"handoff","cwd":"%s","handoffFile":"%s","tabId":"%s","paneId":"%s","instructions":"%s"}' \
      "$CWD_ESC" "$HANDOFF_ESC" "$TAB_ID_ESC" "$PANE_ID_ESC" "$INSTRUCTIONS_ESC")

    REPLY=$(printf '%s\n' "$PAYLOAD" | /usr/bin/nc -U -w 2 "$NICE_SOCKET")

    if [ -z "$REPLY" ]; then
      printf 'nice: no reply from control socket; handoff tab may not have opened\n' >&2
      exit 1
    fi

    case "$REPLY" in
      error*)
        printf '%s\n' "$REPLY" >&2
        exit 1
        ;;
      *)
        printf 'nice: handoff tab opening…\n'
        exit 0
        ;;
    esac
    """#

    // MARK: - Default paths (production)

    /// Filename of the skill definition file inside the skill directory.
    private static let skillFilename = "SKILL.md"

    /// Filename of the helper script inside `~/.nice/`.
    private static let helperFilename = "nice-handoff.sh"

    /// `~/.claude/skills/nice-handoff/`. Claude Code discovers skills
    /// in `~/.claude/skills/` at startup; each subdirectory is one skill,
    /// identified by the `name` key in its `SKILL.md` frontmatter.
    /// Nice owns this directory name exclusively, so uninstall can safely
    /// remove the entire subtree.
    static func defaultSkillDir() -> URL {
        homeBase()
            .appendingPathComponent(".claude/skills/nice-handoff", isDirectory: true)
    }

    /// `~/.nice/` — the no-space dotdir the helper script lives in,
    /// shared with `ClaudeHookInstaller` (see its `defaultScriptDir`
    /// for the word-split rationale). Resolved off `homeBase()` so the
    /// helper follows the same home redirect as the skill dir.
    static func defaultHelperDir() -> URL {
        homeBase().appendingPathComponent(".nice", isDirectory: true)
    }

    /// The home directory both default paths hang off. Honors
    /// `NICE_HOME_OVERRIDE` when set (non-empty) so UITests can redirect
    /// the skill/helper writes into a sandbox HOME instead of the
    /// developer's real `~/.claude/` and `~/.nice/` — mirrors the
    /// `NICE_APPLICATION_SUPPORT_ROOT` / `NICE_SOCKET_PATH` test seams
    /// elsewhere in the app. Production never sets it, so this falls
    /// back to `NSHomeDirectory()` and behavior is unchanged.
    private static func homeBase() -> URL {
        if let override = ProcessInfo.processInfo.environment["NICE_HOME_OVERRIDE"],
           !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        return URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
    }
}
