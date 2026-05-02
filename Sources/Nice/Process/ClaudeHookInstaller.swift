//
//  ClaudeHookInstaller.swift
//  Nice
//
//  Installs a Claude Code `SessionStart` hook that relays the active
//  session UUID back to Nice's control socket whenever Claude rotates
//  it in-process via `/clear` or `/branch` (and any future commands
//  that fire SessionStart). The script forwards on every source — it
//  does not try to distinguish "Nice already knows this id" cases
//  client-side, because the receiver's `if newId != claudeSessionId`
//  short-circuit makes redundant forwards a true no-op. Source-side
//  filtering also turns out to be subtly wrong: `/branch` reports
//  `source: "resume"` (not `"branch"`), so a `resume`-excluding gate
//  would silently lose `/branch` rotations. `/compact`, in current
//  Claude Code, fires SessionStart with the same id (no rotation),
//  which the same short-circuit absorbs.
//
//  Why SessionStart and not UserPromptSubmit (the previous choice):
//  UserPromptSubmit only sees a rotation on the *next* user prompt
//  after the rotating command. If the user runs `/clear` and quits
//  before typing again, the rotation is lost and Nice resumes the
//  pre-clear session on relaunch. SessionStart fires synchronously
//  with the rotation, closing that window.
//
//  Two empirical constraints shape where we install the script and
//  the settings entry; both were learned the hard way:
//    1. Claude does NOT fire hooks declared in `~/.claude/settings
//       .local.json`. Only `~/.claude/settings.json` (and project-
//       local `.claude/settings.json` / `.local.json`) actually
//       invoke them. The `.local` variant at the user level seems to
//       be parsed for other settings but skipped for hook execution.
//    2. The `command` field is run through a shell that word-splits
//       on whitespace, so any space in the path silently fails the
//       hook (the OS tries to exec the prefix and gives up). The
//       script therefore lives at `~/.nice/nice-claude-hook.sh` —
//       no spaces anywhere, no quoting required in the JSON.
//
//  Components:
//    • A small shell script (`nice-claude-hook.sh`) installed at
//      `~/.nice/nice-claude-hook.sh`. Claude invokes it on every
//      `SessionStart`; it no-ops outside Nice (NICE_SOCKET /
//      NICE_PANE_ID unset), no-ops on non-rotation sources
//      (startup/resume), and otherwise posts a `session_update`
//      message containing `paneId` + `sessionId` to NICE_SOCKET.
//    • A Nice-tagged entry in `~/.claude/settings.json` pointing
//      at that script. Merges non-destructively with any existing
//      user hooks. Identified by the absolute command path so
//      reinstalls detect their own entry without duplicating or
//      clobbering the user's other hooks.
//
//  Idempotency: `install()` is safe to call on every launch. The
//  script is rewritten only when the body changed; settings.json is
//  rewritten only when the merged JSON serializes to different bytes.
//
//  Malformed settings.json: if the file exists but doesn't parse,
//  install bails out with a log instead of overwriting. The user
//  keeps their (possibly mid-edit) file and the hook simply doesn't
//  register this launch — better than silent data loss.
//

import Foundation

enum ClaudeHookInstaller {

    // MARK: - Public entry point

    /// Install (or refresh) the hook script and settings entry. Call
    /// once on startup from the main actor. Failures are logged and
    /// swallowed — the app runs fine without the hook; only the
    /// session-sync feature degrades.
    static func install() {
        install(
            scriptDir: defaultScriptDir(),
            settingsURL: defaultSettingsURL()
        )
    }

    /// Test-friendly entry point. Production calls `install()` which
    /// resolves these via NSHomeDirectory; tests pass URLs directly so
    /// they can sandbox without touching process-global env or the
    /// developer's real `~/.claude/`.
    static func install(scriptDir: URL, settingsURL: URL) {
        do {
            let scriptPath = try ensureScriptInstalled(in: scriptDir)
            try mergeHookSettings(scriptPath: scriptPath, settingsURL: settingsURL)
        } catch {
            NSLog("ClaudeHookInstaller: install failed: \(error)")
        }
    }

    // MARK: - Script

    /// Shell script that Claude invokes on every SessionStart. Extracts
    /// `session_id` from claude's stdin JSON (no jq dependency — `sed`
    /// handles the UUID pattern reliably) and posts a `session_update`
    /// socket payload. Forwards on every source (`clear`, `compact`,
    /// `resume`, `startup`, `branch`, anything Claude introduces
    /// later) — the receiver's `if newId != tab.claudeSessionId`
    /// short-circuit makes redundant forwards a true no-op (no save,
    /// no churn), so source-side filtering is unnecessary and
    /// occasionally wrong (e.g. `/branch` reports `source: "resume"`
    /// in current Claude, so a `resume`-excluding gate would silently
    /// lose `/branch` rotations).
    ///
    /// `set -u` catches typos without blocking the fast-path exits for
    /// unset NICE_* vars (`${X:-}` is the no-op form). `nc -w 1` keeps
    /// the hook under claude's hook timeout even if the socket is
    /// unresponsive.
    static let hookScript = #"""
    #!/bin/bash
    # nice-claude-hook.sh — relays the SessionStart hook's session_id
    # to Nice's control socket so each tab's stored claudeSessionId
    # tracks /clear, /compact, and /branch rotations across relaunches.
    # Installed automatically by Nice on startup; safe to delete.
    set -u
    if [ -z "${NICE_SOCKET:-}" ] || [ -z "${NICE_PANE_ID:-}" ]; then
      exit 0
    fi
    INPUT=$(cat)
    SID=$(printf '%s' "$INPUT" | /usr/bin/sed -nE 's/.*"session_id"[[:space:]]*:[[:space:]]*"([a-fA-F0-9-]+)".*/\1/p' | /usr/bin/head -1)
    if [ -z "$SID" ]; then
      exit 0
    fi
    PAYLOAD=$(printf '{"action":"session_update","paneId":"%s","sessionId":"%s"}' "$NICE_PANE_ID" "$SID")
    printf '%s\n' "$PAYLOAD" | /usr/bin/nc -U -w 1 "$NICE_SOCKET" >/dev/null 2>&1 || true
    exit 0
    """#

    /// Write `hookScript` into `dir/nice-claude-hook.sh` and ensure it's
    /// executable. Returns the absolute path. Skips both the write and
    /// the perms reset when the on-disk script already matches — keeps
    /// the file's mtime/ctime stable across no-op launches.
    private static func ensureScriptInstalled(in dir: URL) throws -> String {
        try FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let scriptURL = dir.appendingPathComponent(scriptFilename)
        let path = scriptURL.path
        let existing = try? String(contentsOf: scriptURL, encoding: .utf8)
        if existing == hookScript {
            return path
        }
        try hookScript.write(to: scriptURL, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o755)],
            ofItemAtPath: path
        )
        return path
    }

    // MARK: - Settings merge

    /// Merge a `SessionStart` hook entry into `settingsURL` pointing at
    /// `scriptPath`. If a matching entry is already present, returns
    /// without writing. Other hooks (user-authored, other tools) pass
    /// through untouched.
    ///
    /// Serializes the merged dict and only writes if the bytes differ
    /// from what's on disk — avoids reformatting a hand-edited file
    /// just because we re-emitted it with `[.prettyPrinted, .sortedKeys]`.
    ///
    /// If the file exists but isn't valid JSON, throws — the caller
    /// logs and the launch proceeds without the hook. Overwriting
    /// arbitrary bytes with our scaffolding would lose the user's
    /// content (mid-edit or otherwise) and we'd rather degrade than
    /// destroy.
    private static func mergeHookSettings(
        scriptPath: String, settingsURL: URL
    ) throws {
        try FileManager.default.createDirectory(
            at: settingsURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let existingData = try? Data(contentsOf: settingsURL)
        var root: [String: Any] = [:]
        if let existingData, !existingData.isEmpty {
            // Distinguish "valid JSON object" (merge into) from
            // "non-empty bytes that don't parse" (refuse to clobber).
            let parsed = try JSONSerialization.jsonObject(with: existingData)
            guard let dict = parsed as? [String: Any] else {
                throw ClaudeHookInstallerError.settingsNotAJSONObject
            }
            root = dict
        }
        var hooks = (root["hooks"] as? [String: Any]) ?? [:]
        var sessionStart = (hooks[hookEventName] as? [[String: Any]]) ?? []
        let already = sessionStart.contains { group in
            guard let inner = group["hooks"] as? [[String: Any]] else { return false }
            return inner.contains { ($0["command"] as? String) == scriptPath }
        }
        if already { return }
        sessionStart.append([
            "hooks": [
                ["type": "command", "command": scriptPath],
            ],
        ])
        hooks[hookEventName] = sessionStart
        root["hooks"] = hooks
        let data = try JSONSerialization.data(
            withJSONObject: root,
            options: [.prettyPrinted, .sortedKeys]
        )
        if data == existingData {
            return
        }
        try data.write(to: settingsURL, options: .atomic)
    }

    /// Claude Code hook event name we register under. Exposed
    /// `internal` so tests can read the same constant rather than
    /// hard-coding "SessionStart" in two places.
    static let hookEventName = "SessionStart"

    enum ClaudeHookInstallerError: Error {
        /// `settings.json` exists with non-empty bytes that don't
        /// parse as a JSON object (e.g. mid-edit, hand-edited typo,
        /// arbitrary file). Caller refuses to overwrite.
        case settingsNotAJSONObject
    }

    // MARK: - Default paths (production)

    /// Filename of the installed hook script. Kept distinct enough to
    /// avoid collisions with anything else a user might drop next to
    /// it.
    private static let scriptFilename = "nice-claude-hook.sh"

    /// `~/.nice/`. A no-space dotdir so claude's shell-based hook
    /// runner doesn't word-split the command path. `~/Library/
    /// Application Support/Nice Dev/` would be the macOS-conventional
    /// location, but the spaces in `Application Support` and `Nice
    /// Dev` make claude silently fail to exec the script.
    /// Nice and Nice Dev share this directory because the script
    /// content is variant-agnostic — both write the same body.
    static func defaultScriptDir() -> URL {
        URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            .appendingPathComponent(".nice", isDirectory: true)
    }

    /// `~/.claude/settings.json`. Claude does NOT fire hooks declared
    /// in `~/.claude/settings.local.json` (the `.local` user-level
    /// variant is parsed for some settings but not hook execution),
    /// so we register here. Project-local files would also work but
    /// would require a write per cwd Nice opens; user-level is one
    /// write covering every Nice-spawned claude.
    static func defaultSettingsURL() -> URL {
        URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            .appendingPathComponent(".claude/settings.json")
    }
}
