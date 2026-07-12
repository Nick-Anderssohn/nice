//! Claude Code `SessionStart` hook installer (R16) — ports Swift
//! `ClaudeHookInstaller` (`Sources/Nice/Process/ClaudeHookInstaller.swift`).
//!
//! Installs a Claude Code `SessionStart` hook that relays the active session
//! UUID (plus its `source` and `cwd`) back to Nice's control socket whenever
//! Claude rotates it in-process via `/clear`, `/branch`, `--fork-session`, or a
//! cwd move (`/worktree`, bare `claude -w`). The script forwards on EVERY
//! source — it does not try to distinguish "Nice already knows this id" cases
//! client-side, because the receiver's id-equality short-circuit makes
//! redundant forwards a true no-op. Source-side filtering is also subtly wrong:
//! `/branch` reports `source: "resume"`, so a `resume`-excluding gate would
//! silently lose `/branch` rotations. Classification lives app-side.
//!
//! Two empirical constraints (learned the hard way in Swift) shape where the
//! script and the settings entry live:
//!   1. Claude does NOT fire hooks declared in `~/.claude/settings.local.json`
//!      — only `~/.claude/settings.json` (and project-local files) invoke them.
//!   2. Claude's hook runner word-splits the `command` field on whitespace, so
//!      any space in the path silently fails the exec. The script therefore
//!      lives at `~/.nice/nice-claude-hook.sh` (a no-space dotdir), NOT under
//!      `~/Library/Application Support/Nice…` (spaces).
//!
//! **The script body is a FROZEN socket-client compatibility contract.** The
//! Rust app installs the SAME path with the SAME bytes as the shipped Swift app,
//! so Swift and Rust installs cohabit via the write-only-if-changed rule (a
//! no-op launch leaves the on-disk file — content AND mtime — untouched). Any
//! intentional future divergence needs a DIFFERENT filename, never drifted bytes
//! at the same path. [`HOOK_SCRIPT`] is ported byte-for-byte from the Swift
//! `hookScript` literal and pinned by [`tests`].
//!
//! Idempotency: [`install`] is safe to call on every launch. The script is
//! rewritten only when its body changed; `settings.json` is rewritten only when
//! the merged JSON serializes to different bytes.
//!
//! Malformed `settings.json`: if the file exists with non-empty bytes that do
//! not parse as a JSON OBJECT, [`install`] refuses to write (logs and proceeds
//! without the hook). Overwriting a user's mid-edit file with our scaffolding
//! would lose their content, and we'd rather degrade than destroy.
//!
//! Both entry points take injectable base paths ([`install_with`]); production
//! [`install`] resolves them from `$HOME`. Tests and self-test scenarios drive
//! [`install_with`] against sandbox directories so the regression suite never
//! touches the developer's real `~/.claude` or `~/.nice` (tranche-3
//! hermeticity), and the bootstrap runs it in `app::run` ONLY, never
//! `run_selftest`.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic_file::write_atomic;

use serde_json::{json, Map, Value};

/// The shell script Claude invokes on every `SessionStart` — ported byte-for-
/// byte from the Swift `hookScript` literal (`ClaudeHookInstaller.swift:116`).
/// Extracts `session_id`, `source`, and `cwd` from Claude's stdin JSON with
/// `sed` (no jq dependency) and posts a `session_update` payload to
/// `$NICE_SOCKET` via `/usr/bin/nc` (ABSOLUTE path — so a stub `nc` on `PATH`
/// cannot intercept it). No-ops outside a Nice pane (`NICE_SOCKET` /
/// `NICE_PANE_ID` unset) and on a missing/empty `session_id`.
///
/// FROZEN: this is a compatibility contract shared with the Swift installer at
/// the same on-disk path. It has NO trailing newline (matching Swift's
/// multiline-literal value), so the write-only-if-changed byte compare against a
/// Swift-written file is exact. The `\1` sed back-references and the wide
/// `[^"]+` / `[^"]*` source/cwd classes are load-bearing (a narrower class
/// truncated dotted sources like `branch.auto` → `branch`).
pub const HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# nice-claude-hook.sh — relays the SessionStart hook's session_id,
# source, and cwd to Nice's control socket so each tab's stored
# claudeSessionId tracks /clear, /compact, and /branch rotations
# across relaunches, and so Nice's tab.cwd follows Claude into a
# worktree when `/worktree` or bare `claude -w` (auto-named)
# moves the session's working directory mid-flight.
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
# Source value: any sequence of non-quote bytes between the
# surrounding `"`s. Wider than JSON strictly allows (which permits
# \" escapes), but Claude's source values are constrained
# identifiers in practice and have never carried embedded quotes.
# The narrower `[a-zA-Z0-9_-]+` form silently truncated dotted or
# spaced sources (e.g. "branch.auto" → "branch"); the receiver's
# source-classification gate would then mis-label rotations.
SRC=$(printf '%s' "$INPUT" | /usr/bin/sed -nE 's/.*"source"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | /usr/bin/head -1)
# Cwd value: top-level "cwd" field from the SessionStart payload —
# the absolute path Claude is running in. Anchors on the literal
# `"cwd":` key so `transcript_path`'s inner segments can't bleed
# in, and uses `[^"]*` (same shape as the source class) so the
# captured byte run is guaranteed quote-free. That guarantee is
# what lets us splice the raw bytes straight back into the
# outgoing JSON without re-escaping: Claude already JSON-encoded
# the value, so its `\` runs are already `\\` in the byte stream,
# and an additional escape pass would double them.
#
# Known limitation: a cwd whose JSON-encoded form contains `\"`
# (a literal `"` byte in the path) is truncated at the first `"`
# byte the regex sees. The splice that follows produces a
# malformed outgoing JSON that the socket parser will drop. We
# accept the silent-drop because (a) macOS paths essentially
# never carry an embedded `"` and (b) on the next restart,
# `WindowSession.healSpawnCwd` finds the transcript by session id
# and recovers the real cwd from the transcript file's content
# — so the forward-path drop is covered by the heal safety net.
#
# The receiver normalizes an empty string to nil, so a missing
# cwd surfaces as a no-op there rather than churning a save.
CWD=$(printf '%s' "$INPUT" | /usr/bin/sed -nE 's/.*"cwd"[[:space:]]*:[[:space:]]*"([^"]*)".*/\1/p' | /usr/bin/head -1)
PAYLOAD=$(printf '{"action":"session_update","paneId":"%s","sessionId":"%s","source":"%s","cwd":"%s"}' "$NICE_PANE_ID" "$SID" "$SRC" "$CWD")
printf '%s\n' "$PAYLOAD" | /usr/bin/nc -U -w 1 "$NICE_SOCKET" >/dev/null 2>&1 || true
exit 0"#;

/// Filename of the installed hook script (kept distinct enough to avoid
/// colliding with anything else a user drops next to it).
pub const SCRIPT_FILENAME: &str = "nice-claude-hook.sh";

/// The Claude Code hook event Nice registers under. A nested GROUP shape —
/// `{"hooks":[{"type":"command","command":"…"}]}` — under `hooks.SessionStart`;
/// a flat `{"type":"command",…}` entry never fires in Claude and defeats mutual
/// dedup with the Swift installer.
pub const HOOK_EVENT_NAME: &str = "SessionStart";

/// Pre-migration Nice builds registered the hook under this event instead.
/// [`merge_hook_settings`] strips leftover entries pointing at our script path
/// so upgraders don't carry the redundant registration (a socket round-trip per
/// prompt) forever.
pub const LEGACY_HOOK_EVENT_NAME: &str = "UserPromptSubmit";

/// A hook-installer failure. All variants are logged-and-swallowed by
/// [`install`]; [`install_with`] surfaces them so tests can assert the refusal.
#[derive(Debug)]
pub enum InstallError {
    /// A filesystem operation failed (mkdir / write / rename / read).
    Io(io::Error),
    /// `settings.json` parses as JSON but is not an OBJECT (e.g. a top-level
    /// array or string), OR does not parse at all — either way, non-empty bytes
    /// we refuse to clobber.
    SettingsNotAObject,
    /// `settings.json` has non-empty bytes that don't parse as JSON at all.
    SettingsUnparseable(serde_json::Error),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::Io(e) => write!(f, "filesystem error: {e}"),
            InstallError::SettingsNotAObject => {
                write!(f, "settings.json is not a JSON object; refusing to overwrite")
            }
            InstallError::SettingsUnparseable(e) => {
                write!(f, "settings.json is not valid JSON ({e}); refusing to overwrite")
            }
        }
    }
}

impl std::error::Error for InstallError {}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// Install (or refresh) the hook script + settings entry against the real
/// `$HOME`. Call once on startup from `app::run` (NEVER `run_selftest`).
/// Failures are logged and swallowed — the app runs fine without the hook; only
/// the session-sync feature degrades.
pub fn install() {
    if let Err(e) = install_with(&default_script_dir(), &default_settings_path()) {
        eprintln!("nice: ClaudeHookInstaller: install failed: {e}");
    }
}

/// Test/scenario-friendly entry point: production [`install`] resolves the base
/// paths from `$HOME`; callers here pass them directly so they can sandbox
/// without touching the developer's real `~/.claude` / `~/.nice`.
pub fn install_with(script_dir: &Path, settings_path: &Path) -> Result<(), InstallError> {
    let script_path = ensure_script_installed(script_dir)?;
    merge_hook_settings(&script_path, settings_path)?;
    Ok(())
}

/// Write [`HOOK_SCRIPT`] into `dir/nice-claude-hook.sh` at mode 0755, returning
/// its absolute path (the `command` value the settings entry points at). Skips
/// BOTH the write and the perms reset when the on-disk script already matches —
/// keeping the file's mtime/ctime stable across no-op launches (and letting a
/// Swift-written script survive untouched, since the bytes are identical).
fn ensure_script_installed(dir: &Path) -> Result<String, InstallError> {
    fs::create_dir_all(dir)?;
    let script_path = dir.join(SCRIPT_FILENAME);
    let path_str = script_path.to_string_lossy().into_owned();
    if fs::read_to_string(&script_path).ok().as_deref() == Some(HOOK_SCRIPT) {
        return Ok(path_str);
    }
    write_atomic(&script_path, HOOK_SCRIPT.as_bytes(), Some(0o755))?;
    Ok(path_str)
}

/// Merge a `SessionStart` hook entry pointing at `script_path` into
/// `settings_path`. Ports Swift `mergeHookSettings`:
///   * absent / empty file ⇒ start from `{}`;
///   * non-empty bytes that don't parse as a JSON OBJECT ⇒ refuse to write;
///   * append our entry as one nested group under `hooks.SessionStart` unless a
///     group's inner `hooks` array already carries our command (matched by
///     absolute path — [`contains_command`]);
///   * strip stale `hooks.UserPromptSubmit` entries pointing at our script
///     (pre-migration builds), preserving user-authored siblings and dropping
///     emptied groups / the emptied key;
///   * serialize pretty + stable-sorted keys and write ONLY when the bytes
///     differ (a present entry with no stale UPS early-outs before serializing,
///     preserving a hand-edited file's formatting).
fn merge_hook_settings(script_path: &str, settings_path: &Path) -> Result<(), InstallError> {
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing: Option<Vec<u8>> = fs::read(settings_path).ok();

    let mut root: Map<String, Value> = match existing.as_deref() {
        Some(bytes) if !bytes.is_empty() => match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(map)) => map,
            // Valid JSON but not an object (array / string / number / bool).
            Ok(_) => return Err(InstallError::SettingsNotAObject),
            // Non-empty bytes that don't parse at all — mid-edit / hand-edited
            // typo / arbitrary file. Refuse to overwrite.
            Err(e) => return Err(InstallError::SettingsUnparseable(e)),
        },
        _ => Map::new(),
    };

    let mut hooks: Map<String, Value> = match root.get("hooks") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };

    let already_has_session_start = contains_command(hooks.get(HOOK_EVENT_NAME), script_path);
    let has_stale_ups = contains_command(hooks.get(LEGACY_HOOK_EVENT_NAME), script_path);

    // Early-out preserves the user's settings.json formatting on no-op launches.
    // Without it, parsing and re-emitting through the pretty/sorted serializer
    // would rewrite a hand-edited file even when nothing logically changed.
    if already_has_session_start && !has_stale_ups {
        return Ok(());
    }

    if !already_has_session_start {
        let mut session_start: Vec<Value> = match hooks.get(HOOK_EVENT_NAME) {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        session_start.push(json!({
            "hooks": [ { "type": "command", "command": script_path } ],
        }));
        hooks.insert(HOOK_EVENT_NAME.to_string(), Value::Array(session_start));
    }

    if has_stale_ups {
        // Migration: pre-migration builds registered our hook under
        // UserPromptSubmit. Strip leftovers pointing at our script path;
        // preserve user-authored siblings; drop any group that empties out; drop
        // the UserPromptSubmit key entirely if it ends up empty so we don't
        // leave a husk.
        let mut ups: Vec<Value> = match hooks.get(LEGACY_HOOK_EVENT_NAME) {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        for i in (0..ups.len()).rev() {
            let Some(inner) = ups[i].get("hooks").and_then(Value::as_array) else {
                continue;
            };
            let filtered: Vec<Value> = inner
                .iter()
                .filter(|entry| {
                    entry.get("command").and_then(Value::as_str) != Some(script_path)
                })
                .cloned()
                .collect();
            if filtered.is_empty() {
                ups.remove(i);
            } else if let Some(obj) = ups[i].as_object_mut() {
                obj.insert("hooks".to_string(), Value::Array(filtered));
            }
        }
        if ups.is_empty() {
            hooks.remove(LEGACY_HOOK_EVENT_NAME);
        } else {
            hooks.insert(LEGACY_HOOK_EVENT_NAME.to_string(), Value::Array(ups));
        }
    }

    root.insert("hooks".to_string(), Value::Object(hooks));

    // Stable-sorted pretty serialization so the only-if-changed byte compare is
    // meaningful regardless of serde_json's `preserve_order` feature (the sort
    // recurses through every nested object; arrays keep their order).
    let sorted = sort_value(&Value::Object(root));
    let serialized = serde_json::to_vec_pretty(&sorted)
        .expect("serialize settings.json (in-memory Value never fails)");

    if existing.as_deref() == Some(serialized.as_slice()) {
        return Ok(());
    }
    write_atomic(settings_path, &serialized, None)?;
    Ok(())
}

/// True when any group in `groups` (a `hooks.<Event>` array) has an inner hook
/// whose `command` equals `command`. Used both to detect "our SessionStart entry
/// already present" and "stale UserPromptSubmit entry from a pre-migration
/// build". Absent / wrong-typed input ⇒ `false`.
fn contains_command(groups: Option<&Value>, command: &str) -> bool {
    let Some(Value::Array(groups)) = groups else {
        return false;
    };
    groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|inner| {
                inner
                    .iter()
                    .any(|entry| entry.get("command").and_then(Value::as_str) == Some(command))
            })
    })
}

/// Recursively rebuild `v` with every object's keys in sorted order (arrays keep
/// their element order). Serializing the result yields byte-stable output even
/// if a workspace dependency flips serde_json's `preserve_order` on.
fn sort_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), sort_value(&map[k]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

/// `~/.nice/` — a no-space dotdir so Claude's shell-based hook runner doesn't
/// word-split the command path. Both Nice variants (`Nice` / `Nice Dev`) share it because the
/// script content is variant-agnostic (both write the same body).
fn default_script_dir() -> PathBuf {
    PathBuf::from(home_dir()).join(".nice")
}

/// `~/.claude/settings.json` — Claude does NOT fire hooks declared in
/// `settings.local.json`, and a user-level file is one write covering every
/// Nice-spawned Claude (vs. one project-local write per cwd).
fn default_settings_path() -> PathBuf {
    PathBuf::from(home_dir()).join(".claude/settings.json")
}

/// The process `$HOME`, falling back to `/` (production-only; the app always has
/// a real home). Tests never call this — they drive [`install_with`] directly.
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    // ---- temp-dir plumbing (mirrors shell_inject.rs) -----------------------

    /// A throwaway directory removed on drop. A panicking assertion leaves it
    /// behind, which is harmless.
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn unique(prefix: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()))
    }

    fn scratch(prefix: &str) -> Scratch {
        let dir = unique(prefix);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// The sandbox script-dir + settings-path pair for one test, under a fresh
    /// scratch root (auto-removed): never the developer's real `~/.nice` /
    /// `~/.claude` (hermeticity).
    fn sandbox(prefix: &str) -> (Scratch, PathBuf, PathBuf) {
        let root = scratch(prefix);
        let script_dir = root.0.join("nice");
        let settings = root.0.join("claude").join("settings.json");
        (root, script_dir, settings)
    }

    fn read_settings(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).expect("read settings")).expect("parse settings")
    }

    fn expected_script_path(script_dir: &Path) -> String {
        script_dir.join(SCRIPT_FILENAME).to_string_lossy().into_owned()
    }

    // ---- script bytes (ClaudeHookInstallerTests: script bytes) -------------

    /// The FROZEN script begins with the bash shebang and — matching Swift's
    /// multiline-literal value — has NO trailing newline, so the byte compare
    /// against a Swift-written file is exact.
    #[test]
    fn hook_script_shebang_and_no_trailing_newline() {
        assert!(
            HOOK_SCRIPT.starts_with("#!/usr/bin/env bash\n"),
            "hook script must start with the bash shebang"
        );
        assert!(
            !HOOK_SCRIPT.ends_with('\n'),
            "hook script must not end with a trailing newline (Swift-literal parity)"
        );
        assert!(HOOK_SCRIPT.ends_with("exit 0"), "hook script must end with `exit 0`");
    }

    /// The load-bearing frozen fragments: `set -u`, the guard exits, the three
    /// `sed -nE` extractions with their exact classes, the payload shape, and
    /// the ABSOLUTE `/usr/bin/nc` invocation (a stub `nc` on PATH must not be
    /// able to intercept the send).
    #[test]
    fn hook_script_frozen_fragments() {
        for needle in [
            "set -u",
            r#"if [ -z "${NICE_SOCKET:-}" ] || [ -z "${NICE_PANE_ID:-}" ]; then"#,
            r#""([a-fA-F0-9-]+)""#,
            r#""source"[[:space:]]*:[[:space:]]*"([^"]+)""#,
            r#""cwd"[[:space:]]*:[[:space:]]*"([^"]*)""#,
            r#"{"action":"session_update","paneId":"%s","sessionId":"%s","source":"%s","cwd":"%s"}"#,
            r#"/usr/bin/nc -U -w 1 "$NICE_SOCKET""#,
        ] {
            assert!(HOOK_SCRIPT.contains(needle), "hook script missing frozen fragment: {needle}");
        }
    }

    /// The writer lays the exact frozen bytes down at mode 0755 and returns the
    /// absolute command path the settings entry points at.
    #[test]
    fn ensure_script_writes_frozen_bytes_at_0755() {
        let (_root, script_dir, _settings) = sandbox("hook-script");
        let path = ensure_script_installed(&script_dir).expect("install script");
        assert_eq!(path, expected_script_path(&script_dir));

        let on_disk = fs::read_to_string(&path).expect("read script");
        assert_eq!(on_disk, HOOK_SCRIPT, "on-disk script must equal the frozen const");

        let mode = fs::metadata(&path).expect("stat script").permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "hook script must be mode 0755");
    }

    /// Re-running the writer over an identical on-disk script leaves its mtime
    /// untouched (no rewrite), the no-op-launch cheapness contract.
    #[test]
    fn ensure_script_is_mtime_stable_when_unchanged() {
        let (_root, script_dir, _settings) = sandbox("hook-script-stable");
        let path = ensure_script_installed(&script_dir).expect("install script");
        let mtime1 = fs::metadata(&path).unwrap().modified().unwrap();
        let path2 = ensure_script_installed(&script_dir).expect("re-install script");
        let mtime2 = fs::metadata(&path2).unwrap().modified().unwrap();
        assert_eq!(mtime1, mtime2, "unchanged script must not be rewritten");
    }

    // ---- merge matrix (ClaudeHookInstallerTests) ---------------------------

    /// Absent settings file ⇒ start from `{}` and register the nested-group
    /// SessionStart entry.
    #[test]
    fn merge_absent_creates_nested_session_start_group() {
        let (_root, script_dir, settings) = sandbox("hook-absent");
        install_with(&script_dir, &settings).expect("install");

        let v = read_settings(&settings);
        let group = &v["hooks"]["SessionStart"][0];
        assert_eq!(group["hooks"][0]["type"], "command");
        assert_eq!(group["hooks"][0]["command"], expected_script_path(&script_dir));
    }

    /// An empty (0-byte) settings file behaves exactly like an absent one.
    #[test]
    fn merge_empty_file_treated_as_absent() {
        let (_root, script_dir, settings) = sandbox("hook-empty");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, b"").unwrap();

        install_with(&script_dir, &settings).expect("install");
        let v = read_settings(&settings);
        assert_eq!(
            v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            Value::String(expected_script_path(&script_dir))
        );
    }

    /// A foreign event (`PreToolUse`), a user-authored SessionStart sibling, and
    /// unrelated top-level keys all survive; our entry is APPENDED after the
    /// user's SessionStart group.
    #[test]
    fn merge_preserves_foreign_hooks_and_siblings() {
        let (_root, script_dir, settings) = sandbox("hook-foreign");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let pre = serde_json::json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [ { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] } ],
                "SessionStart": [ { "hooks": [ { "type": "command", "command": "/other/hook.sh" } ] } ],
            }
        });
        fs::write(&settings, serde_json::to_vec(&pre).unwrap()).unwrap();

        install_with(&script_dir, &settings).expect("install");
        let v = read_settings(&settings);

        assert_eq!(v["model"], "opus", "unrelated top-level key preserved");
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "/usr/bin/true",
            "foreign event preserved"
        );
        let ss = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 2, "user SessionStart group kept + ours appended");
        assert_eq!(ss[0]["hooks"][0]["command"], "/other/hook.sh");
        assert_eq!(ss[1]["hooks"][0]["command"], expected_script_path(&script_dir));
    }

    /// Second install with our entry already present and no stale UPS is a true
    /// no-op: identical bytes AND stable mtime (formatting untouched).
    #[test]
    fn merge_already_present_is_byte_and_mtime_stable() {
        let (_root, script_dir, settings) = sandbox("hook-stable");
        install_with(&script_dir, &settings).expect("first install");
        let bytes1 = fs::read(&settings).unwrap();
        let mtime1 = fs::metadata(&settings).unwrap().modified().unwrap();

        install_with(&script_dir, &settings).expect("second install");
        let bytes2 = fs::read(&settings).unwrap();
        let mtime2 = fs::metadata(&settings).unwrap().modified().unwrap();

        assert_eq!(bytes1, bytes2, "no-op launch must not rewrite settings.json");
        assert_eq!(mtime1, mtime2, "no-op launch must leave the mtime untouched");
    }

    /// Stale UPS whose only inner hook is OURS ⇒ the emptied group is dropped and
    /// the emptied UserPromptSubmit key is removed entirely (no husk); the
    /// SessionStart entry is left intact.
    #[test]
    fn merge_strips_stale_ups_and_removes_emptied_key() {
        let (_root, script_dir, settings) = sandbox("hook-ups-key");
        let script_path = expected_script_path(&script_dir);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let pre = serde_json::json!({
            "hooks": {
                "SessionStart": [ { "hooks": [ { "type": "command", "command": script_path } ] } ],
                "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": script_path } ] } ],
            }
        });
        fs::write(&settings, serde_json::to_vec(&pre).unwrap()).unwrap();

        install_with(&script_dir, &settings).expect("install");
        let v = read_settings(&settings);
        assert!(
            v["hooks"].get("UserPromptSubmit").is_none(),
            "emptied UserPromptSubmit key must be removed entirely"
        );
        assert_eq!(
            v["hooks"]["SessionStart"][0]["hooks"][0]["command"], script_path,
            "SessionStart entry left intact"
        );
    }

    /// Stale UPS group that ALSO holds a user-authored sibling ⇒ only our command
    /// is stripped; the group and the UserPromptSubmit key survive.
    #[test]
    fn merge_strips_stale_ups_preserving_user_sibling() {
        let (_root, script_dir, settings) = sandbox("hook-ups-sibling");
        let script_path = expected_script_path(&script_dir);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let pre = serde_json::json!({
            "hooks": {
                "SessionStart": [ { "hooks": [ { "type": "command", "command": script_path } ] } ],
                "UserPromptSubmit": [ {
                    "hooks": [
                        { "type": "command", "command": script_path },
                        { "type": "command", "command": "/user/thing.sh" },
                    ]
                } ],
            }
        });
        fs::write(&settings, serde_json::to_vec(&pre).unwrap()).unwrap();

        install_with(&script_dir, &settings).expect("install");
        let v = read_settings(&settings);
        let ups = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1, "user-authored group retained");
        let inner = ups[0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1, "only our command stripped from the group");
        assert_eq!(inner[0]["command"], "/user/thing.sh");
    }

    /// Stale UPS with two groups — one holding only ours, one holding only a
    /// user hook ⇒ the ours-only group is removed, the user-only group and the
    /// key are kept.
    #[test]
    fn merge_strips_stale_ups_drops_only_emptied_group() {
        let (_root, script_dir, settings) = sandbox("hook-ups-groups");
        let script_path = expected_script_path(&script_dir);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let pre = serde_json::json!({
            "hooks": {
                "SessionStart": [ { "hooks": [ { "type": "command", "command": script_path } ] } ],
                "UserPromptSubmit": [
                    { "hooks": [ { "type": "command", "command": script_path } ] },
                    { "hooks": [ { "type": "command", "command": "/user/x.sh" } ] },
                ],
            }
        });
        fs::write(&settings, serde_json::to_vec(&pre).unwrap()).unwrap();

        install_with(&script_dir, &settings).expect("install");
        let v = read_settings(&settings);
        let ups = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1, "ours-only group removed, user-only group kept");
        assert_eq!(ups[0]["hooks"][0]["command"], "/user/x.sh");
    }

    /// Valid JSON that is NOT an object (a top-level array) ⇒ refuse to write;
    /// the user's file is left byte-for-byte untouched.
    #[test]
    fn merge_refuses_non_object_settings() {
        let (_root, script_dir, settings) = sandbox("hook-nonobj");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let original = b"[1, 2, 3]";
        fs::write(&settings, original).unwrap();

        let err = install_with(&script_dir, &settings).expect_err("must refuse");
        assert!(matches!(err, InstallError::SettingsNotAObject), "got: {err:?}");
        assert_eq!(fs::read(&settings).unwrap(), original, "file must be untouched");
    }

    /// Non-empty bytes that don't parse as JSON at all ⇒ refuse to write; file
    /// left untouched.
    #[test]
    fn merge_refuses_unparseable_settings() {
        let (_root, script_dir, settings) = sandbox("hook-badjson");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let original = b"{ not valid json";
        fs::write(&settings, original).unwrap();

        let err = install_with(&script_dir, &settings).expect_err("must refuse");
        assert!(matches!(err, InstallError::SettingsUnparseable(_)), "got: {err:?}");
        assert_eq!(fs::read(&settings).unwrap(), original, "file must be untouched");
    }

    /// Serialized settings are stable-sorted at every level so the only-if-
    /// changed byte compare is meaningful.
    #[test]
    fn merge_output_keys_are_sorted() {
        let (_root, script_dir, settings) = sandbox("hook-sorted");
        install_with(&script_dir, &settings).expect("install");
        let text = fs::read_to_string(&settings).unwrap();
        // The group object's keys sort "command" before "type".
        let cmd = text.find(r#""command""#).unwrap();
        let typ = text.find(r#""type""#).unwrap();
        assert!(cmd < typ, "object keys must serialize sorted (command < type)");
    }

    // ---- black-box hook-script tests (item 5) ------------------------------
    //
    // Run the INSTALLED script bytes under its own shebang with canned
    // SessionStart JSON on stdin and a fixture Unix-socket listener at
    // $NICE_SOCKET. The script invokes `/usr/bin/nc` by ABSOLUTE path, so a stub
    // `nc` on PATH cannot intercept it — the fixture listener is the only
    // interception point. This pins the `sed` extraction + payload splice
    // WITHOUT depending on the real Claude binary (tranche-3 hermeticity).

    /// A short-named Unix socket under /tmp (macOS `sun_path` caps at ~104
    /// bytes, and $TMPDIR is a long `/var/folders/...` path), removed on drop.
    struct SockPath(PathBuf);
    impl Drop for SockPath {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    fn sock_path() -> SockPath {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        SockPath(PathBuf::from(format!(
            "/tmp/nrs-hook-{}-{n}.sock",
            std::process::id()
        )))
    }

    /// Run the installed hook script with the given env + stdin. When `sock` is
    /// `Some`, a fixture listener is bound BEFORE the run and the single payload
    /// line the script sends (via `nc`) is captured. Returns
    /// `(exit_success, payload_line_without_trailing_newline)`.
    ///
    /// The script's `nc` connection completes (and its bytes are buffered)
    /// before the pipeline finishes and the script exits, so a single accept
    /// AFTER `child.wait()` reliably observes a sent payload; a short poll then
    /// distinguishes "nothing sent".
    fn run_hook(
        script: &Path,
        sock: Option<&Path>,
        pane_id: Option<&str>,
        stdin_json: &str,
    ) -> (bool, Option<String>) {
        let listener = sock.map(|p| {
            let _ = fs::remove_file(p);
            let l = UnixListener::bind(p).expect("bind fixture socket");
            l.set_nonblocking(true).expect("set_nonblocking");
            l
        });

        let mut cmd = Command::new(script);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(p) = sock {
            cmd.env("NICE_SOCKET", p);
        }
        if let Some(pid) = pane_id {
            cmd.env("NICE_PANE_ID", pid);
        }
        let mut child = cmd.spawn().expect("spawn hook script");
        {
            let mut stdin = child.stdin.take().expect("child stdin");
            stdin.write_all(stdin_json.as_bytes()).expect("write stdin");
            // stdin drops here → EOF for the script's `INPUT=$(cat)`.
        }
        let status = child.wait().expect("wait for hook script");

        let payload = listener.and_then(|l| {
            let deadline = Instant::now() + Duration::from_millis(1000);
            loop {
                match l.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        let mut line = String::new();
                        let _ = BufReader::new(stream).read_line(&mut line);
                        let trimmed = line.trim_end_matches('\n').to_string();
                        return if trimmed.is_empty() { None } else { Some(trimmed) };
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return None;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("fixture accept error: {e}"),
                }
            }
        });
        (status.success(), payload)
    }

    fn installed_script() -> (Scratch, PathBuf) {
        let (root, script_dir, _settings) = sandbox("hook-blackbox");
        let path = ensure_script_installed(&script_dir).expect("install script");
        (root, PathBuf::from(path))
    }

    /// Normal rotation: full SessionStart JSON ⇒ the exact `session_update`
    /// payload with paneId / sessionId / source / cwd spliced through.
    #[test]
    fn blackbox_normal_rotation_emits_payload() {
        let (_root, script) = installed_script();
        let sock = sock_path();
        let input = r#"{"session_id":"550e8400-e29b-41d4-a716-446655440000","source":"resume","cwd":"/Users/me/proj","transcript_path":"/x/y.jsonl"}"#;
        let (ok, payload) = run_hook(&script, Some(&sock.0), Some("pane-7"), input);
        assert!(ok, "script must exit 0");
        assert_eq!(
            payload.as_deref(),
            Some(r#"{"action":"session_update","paneId":"pane-7","sessionId":"550e8400-e29b-41d4-a716-446655440000","source":"resume","cwd":"/Users/me/proj"}"#),
            "normal rotation payload"
        );
    }

    /// Missing session_id ⇒ the script exits 0 and sends nothing.
    #[test]
    fn blackbox_missing_session_id_sends_nothing() {
        let (_root, script) = installed_script();
        let sock = sock_path();
        let input = r#"{"source":"startup","cwd":"/Users/me/proj"}"#;
        let (ok, payload) = run_hook(&script, Some(&sock.0), Some("pane-7"), input);
        assert!(ok, "script must exit 0");
        assert_eq!(payload, None, "no session_id ⇒ nothing sent");
    }

    /// Unset NICE_SOCKET ⇒ the script exits 0 without invoking `nc`.
    #[test]
    fn blackbox_unset_socket_exits_clean() {
        let (_root, script) = installed_script();
        let input = r#"{"session_id":"abc-123","source":"resume","cwd":"/x"}"#;
        // No socket bound and NICE_SOCKET unset (env_clear + omit).
        let (ok, payload) = run_hook(&script, None, Some("pane-7"), input);
        assert!(ok, "script must exit 0 with NICE_SOCKET unset");
        assert_eq!(payload, None);
    }

    /// A dotted source survives the wide `[^"]+` class (the narrow class
    /// truncated `branch.auto` → `branch`).
    #[test]
    fn blackbox_dotted_source_survives() {
        let (_root, script) = installed_script();
        let sock = sock_path();
        let input = r#"{"session_id":"aaaa-1111","source":"branch.auto","cwd":"/tmp/work"}"#;
        let (ok, payload) = run_hook(&script, Some(&sock.0), Some("p1"), input);
        assert!(ok);
        assert_eq!(
            payload.as_deref(),
            Some(r#"{"action":"session_update","paneId":"p1","sessionId":"aaaa-1111","source":"branch.auto","cwd":"/tmp/work"}"#),
            "dotted source must survive verbatim"
        );
    }

    /// A cwd containing spaces survives the `[^"]*` class and splices raw into
    /// the outgoing JSON.
    #[test]
    fn blackbox_cwd_with_spaces_survives() {
        let (_root, script) = installed_script();
        let sock = sock_path();
        let input = r#"{"session_id":"bbbb-2222","source":"resume","cwd":"/Users/me/My Project"}"#;
        let (ok, payload) = run_hook(&script, Some(&sock.0), Some("p2"), input);
        assert!(ok);
        assert_eq!(
            payload.as_deref(),
            Some(r#"{"action":"session_update","paneId":"p2","sessionId":"bbbb-2222","source":"resume","cwd":"/Users/me/My Project"}"#),
            "cwd with spaces must survive verbatim"
        );
    }
}
