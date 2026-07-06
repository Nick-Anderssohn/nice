//! Restore-time cwd heal helpers (L3/C5) — pure ports of Swift
//! `WindowSession`'s `encodeClaudeBucket` / `readCwdFromTranscript` /
//! `healSpawnCwd` (`WindowSession.swift:412-589`).
//!
//! The bug shape: a bare `claude -w` (no name) auto-generates a worktree dir
//! Nice can't predict at the args layer, so an older `tab.cwd` records the
//! pre-worktree project path while Claude bucketed the transcript under the
//! real worktree. `claude --resume` from the wrong bucket fails with "No
//! conversation found". On restore, before spawning a deferred-resume Claude
//! tab, we check whether the expected transcript exists; if not, we locate it
//! by session id across every projects bucket, recover the real cwd from the
//! transcript head, and adopt it.
//!
//! Every entry point takes an injectable `projects_root` (default
//! `~/.claude/projects`, resolved only in `app::run`) so tests plant a temp
//! `<root>/<bucket>/<sid>.jsonl` tree and never touch the developer's real
//! `~/.claude`. Claude tabs ONLY — terminal tabs never heal — and silent on
//! unrecoverable sessions.
//!
//! These are the pure helpers; the restore-loop wiring (adopt_tab_cwd +
//! schedule-save) lands with the fan-out in a later slice, hence the
//! module-wide `dead_code` allow (the established later-slice-consumer pattern,
//! per `control_socket`).

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Claude Code's per-cwd bucketing root, relative to `$HOME`. Resolved against
/// the real home only at the production call site in `app::run`.
pub const CLAUDE_PROJECTS_DIR_RELATIVE: &str = "/.claude/projects";

/// Filename suffix Claude Code uses for per-session transcripts (JSON-lines).
pub const TRANSCRIPT_EXTENSION: &str = ".jsonl";

/// How many transcript lines [`read_cwd_from_transcript`] scans before giving
/// up. Sized to cover Claude's transcript head (permission-mode +
/// worktree-state + file-history-snapshot + the first message records) without
/// ballooning per-restore I/O.
pub const TRANSCRIPT_HEAD_SCAN_LINES: usize = 30;

/// Production-shape projects root: Claude Code's bucketing tree under the
/// current user's home. Called only from `app::run`.
pub fn default_claude_projects_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    PathBuf::from(format!("{home}{CLAUDE_PROJECTS_DIR_RELATIVE}"))
}

/// Mirror of Claude Code's bucket-name convention: replace every `/` and `.` in
/// the absolute path with `-` (e.g.
/// `/Users/nick/Projects/notes/.claude/worktrees/foo` →
/// `-Users-nick-Projects-notes--claude-worktrees-foo`). Lossy in general (two
/// distinct paths can collide), which is exactly why
/// [`read_cwd_from_transcript`] pulls the real path from the file content
/// instead of decoding the bucket name.
pub fn encode_claude_bucket(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len());
    for ch in cwd.chars() {
        if ch == '/' || ch == '.' {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Read the first [`TRANSCRIPT_HEAD_SCAN_LINES`] newline-delimited JSON records
/// of a Claude transcript and return the first cwd found. Per-message records
/// carry a top-level `"cwd"`; worktree sessions emit a
/// `{"type":"worktree-state","worktreeSession":{"worktreePath":"…"}}` record
/// near the top, used as a fallback. Returns `None` if the file is missing,
/// unreadable, non-UTF-8, or the scanned head carries neither field — the
/// caller then falls back to `resolved_spawn_cwd`.
///
/// A top-level non-empty `cwd` wins over a nested `worktreePath` on the same
/// record (the per-message format reflects where Claude is now). Non-object
/// lines (arrays, bare scalars) and unparseable lines are skipped.
pub fn read_cwd_from_transcript(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content
        .split('\n')
        .filter(|l| !l.is_empty())
        .take(TRANSCRIPT_HEAD_SCAN_LINES)
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if let Some(cwd) = obj.get("cwd").and_then(|v| v.as_str()) {
            if !cwd.is_empty() {
                return Some(cwd.to_string());
            }
        }
        if let Some(worktree_path) = obj
            .get("worktreeSession")
            .and_then(|v| v.as_object())
            .and_then(|nested| nested.get("worktreePath"))
            .and_then(|v| v.as_str())
        {
            if !worktree_path.is_empty() {
                return Some(worktree_path.to_string());
            }
        }
    }
    None
}

/// Locate a Claude session's actual on-disk bucket when the persisted
/// `persisted_cwd` doesn't match what Claude bucketed under. Returns the
/// recovered cwd (suitable for both `tab.cwd` persistence and as the
/// deferred-shell spawn dir), or `None` when no heal is necessary (transcript
/// already at the expected path) or possible (session id absent from every
/// bucket, transcript unreadable, recovered path no longer exists on disk).
///
/// `projects_root` is injectable so tests drive the scan against a temp root
/// rather than `~/.claude/projects`.
pub fn heal_spawn_cwd(
    session_id: &str,
    persisted_cwd: &str,
    projects_root: &Path,
) -> Option<String> {
    let expected_bucket = encode_claude_bucket(&expand_tilde(persisted_cwd));
    let expected_transcript = projects_root
        .join(&expected_bucket)
        .join(format!("{session_id}{TRANSCRIPT_EXTENSION}"));
    if expected_transcript.exists() {
        // The transcript is already where the persisted cwd implies — no heal.
        return None;
    }

    // Enumerate every sibling bucket for `<session_id>.jsonl`. A missing /
    // unreadable projects dir yields no entries (not an error) — we bail with
    // `None`, matching Swift's empty-contents guard.
    let entries = fs::read_dir(projects_root).ok()?;
    let mut matches: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let candidate = entry
            .path()
            .join(format!("{session_id}{TRANSCRIPT_EXTENSION}"));
        if let Ok(meta) = fs::metadata(&candidate) {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            matches.push((candidate, mtime));
        }
    }
    // Newest mtime wins (Swift sorts descending and takes first).
    let chosen = matches.into_iter().max_by_key(|(_, mtime)| *mtime)?.0;

    let recovered = read_cwd_from_transcript(&chosen)?;
    // No point rewriting `tab.cwd` to a phantom path: if the worktree was
    // deleted, the resume is unrecoverable and `resolved_spawn_cwd`'s fallback
    // still drops the user into the project root.
    if !Path::new(&expand_tilde(&recovered)).exists() {
        return None;
    }
    Some(recovered)
}

/// Minimal tilde expansion for the heal path — mirrors `TabModel::expand_tilde`
/// but self-contained (this module is gpui/model-free). Reads `$HOME` only when
/// the path actually starts with `~`; the heal tests use absolute paths, so no
/// real-home read occurs.
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        return format!("{home}/{rest}");
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    //! Ported from `Tests/NiceUnitTests/WindowSessionHealHelpersTests.swift`.
    //! Each test plants a temp `<root>/<bucket>/<sid>.jsonl` tree and passes the
    //! same root to the SUT, so no `TestHomeSandbox` is needed.
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A throwaway directory removed on drop (mirrors the claude_hook_installer
    /// / shell_inject test plumbing).
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

    /// Plant a transcript with the given lines (joined `\n` + trailing newline,
    /// matching Swift's `plantFile`). Returns the file path.
    fn plant_file(dir: &Path, lines: &[&str]) -> PathBuf {
        let path = dir.join(format!("transcript{TRANSCRIPT_EXTENSION}"));
        let body = format!("{}\n", lines.join("\n"));
        fs::write(&path, body).expect("write transcript");
        path
    }

    // MARK: - encode_claude_bucket

    #[test]
    fn encode_claude_bucket_empty_string() {
        assert_eq!(encode_claude_bucket(""), "");
    }

    #[test]
    fn encode_claude_bucket_plain_path() {
        assert_eq!(
            encode_claude_bucket("/Users/nick/Projects/notes"),
            "-Users-nick-Projects-notes"
        );
    }

    #[test]
    fn encode_claude_bucket_dots_become_dashes() {
        // The double-dash run `/.claude` is the production smoking gun.
        assert_eq!(
            encode_claude_bucket("/Users/nick/Projects/notes/.claude/worktrees/foo"),
            "-Users-nick-Projects-notes--claude-worktrees-foo"
        );
    }

    #[test]
    fn encode_claude_bucket_other_punctuation_passes_through() {
        assert_eq!(encode_claude_bucket("/tmp/foo_bar-2"), "-tmp-foo_bar-2");
    }

    // MARK: - read_cwd_from_transcript

    #[test]
    fn read_cwd_returns_none_for_missing_file() {
        let s = scratch("nice-heal-missing");
        let missing = s.0.join("no.jsonl");
        assert_eq!(read_cwd_from_transcript(&missing), None);
    }

    #[test]
    fn read_cwd_returns_none_for_non_json_content() {
        let s = scratch("nice-heal-nonjson");
        let path = plant_file(&s.0, &["not json at all", "still not json", "{ unbalanced"]);
        assert_eq!(read_cwd_from_transcript(&path), None);
    }

    #[test]
    fn read_cwd_returns_none_when_no_cwd_anywhere() {
        let s = scratch("nice-heal-nocwd");
        let path = plant_file(
            &s.0,
            &[
                r#"{"type":"permission-mode","permissionMode":"auto"}"#,
                r#"{"type":"file-history-snapshot","isSnapshotUpdate":false}"#,
            ],
        );
        assert_eq!(read_cwd_from_transcript(&path), None);
    }

    #[test]
    fn read_cwd_finds_top_level_cwd() {
        let s = scratch("nice-heal-toplevel");
        let path = plant_file(
            &s.0,
            &[r#"{"type":"user","cwd":"/Users/nick/Projects/notes","sessionId":"s"}"#],
        );
        assert_eq!(
            read_cwd_from_transcript(&path).as_deref(),
            Some("/Users/nick/Projects/notes")
        );
    }

    #[test]
    fn read_cwd_falls_back_to_worktree_path() {
        let s = scratch("nice-heal-worktree");
        let path = plant_file(
            &s.0,
            &[
                r#"{"type":"permission-mode","permissionMode":"auto"}"#,
                r#"{"type":"worktree-state","worktreeSession":{"worktreePath":"/Users/nick/Projects/notes/.claude/worktrees/foo","originalCwd":"/Users/nick/Projects/notes"}}"#,
            ],
        );
        assert_eq!(
            read_cwd_from_transcript(&path).as_deref(),
            Some("/Users/nick/Projects/notes/.claude/worktrees/foo")
        );
    }

    #[test]
    fn read_cwd_prefers_top_level_cwd_over_worktree_path() {
        let s = scratch("nice-heal-prefers");
        let path = plant_file(
            &s.0,
            &[r#"{"type":"user","cwd":"/Users/nick/Projects/notes","worktreeSession":{"worktreePath":"/somewhere/else"}}"#],
        );
        assert_eq!(
            read_cwd_from_transcript(&path).as_deref(),
            Some("/Users/nick/Projects/notes")
        );
    }

    #[test]
    fn read_cwd_skips_non_object_lines() {
        let s = scratch("nice-heal-nonobj");
        let path = plant_file(
            &s.0,
            &[
                r#"[1, 2, 3]"#,
                r#""bare string""#,
                r#"42"#,
                r#"{"type":"user","cwd":"/recovered"}"#,
            ],
        );
        assert_eq!(read_cwd_from_transcript(&path).as_deref(), Some("/recovered"));
    }

    #[test]
    fn read_cwd_skips_empty_cwd_field() {
        let s = scratch("nice-heal-emptycwd");
        let path = plant_file(
            &s.0,
            &[
                r#"{"type":"system","cwd":""}"#,
                r#"{"type":"user","cwd":"/recovered"}"#,
            ],
        );
        assert_eq!(read_cwd_from_transcript(&path).as_deref(), Some("/recovered"));
    }

    #[test]
    fn read_cwd_finds_cwd_at_boundary_last_line() {
        // The scan budget is inclusive: a cwd record at exactly line N is found.
        let s = scratch("nice-heal-boundary");
        let mut lines: Vec<String> =
            vec![r#"{"type":"system"}"#.to_string(); TRANSCRIPT_HEAD_SCAN_LINES - 1];
        lines.push(r#"{"type":"user","cwd":"/atBoundary"}"#.to_string());
        assert_eq!(lines.len(), TRANSCRIPT_HEAD_SCAN_LINES);
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let path = plant_file(&s.0, &refs);
        assert_eq!(
            read_cwd_from_transcript(&path).as_deref(),
            Some("/atBoundary"),
            "cwd record at line {TRANSCRIPT_HEAD_SCAN_LINES} must be within budget"
        );
    }

    #[test]
    fn read_cwd_ignores_cwd_just_beyond_budget() {
        // Companion: a cwd record one line past the budget is NOT picked up.
        let s = scratch("nice-heal-beyond");
        let mut lines: Vec<String> =
            vec![r#"{"type":"system"}"#.to_string(); TRANSCRIPT_HEAD_SCAN_LINES];
        lines.push(r#"{"type":"user","cwd":"/beyondBoundary"}"#.to_string());
        assert_eq!(lines.len(), TRANSCRIPT_HEAD_SCAN_LINES + 1);
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let path = plant_file(&s.0, &refs);
        assert_eq!(
            read_cwd_from_transcript(&path),
            None,
            "cwd record at line {} must be beyond the scan budget",
            TRANSCRIPT_HEAD_SCAN_LINES + 1
        );
    }

    // MARK: - heal_spawn_cwd projects_root injection

    /// Plant `<root>/<bucket(bucket_cwd)>/<sid>.jsonl` carrying a top-level
    /// `cwd` of `message_cwd`. Mirrors Swift's `plantTranscript`.
    fn plant_transcript(root: &Path, bucket_cwd: &str, sid: &str, message_cwd: &str) {
        let bucket = encode_claude_bucket(bucket_cwd);
        let dir = root.join(bucket);
        fs::create_dir_all(&dir).expect("create bucket dir");
        let path = dir.join(format!("{sid}{TRANSCRIPT_EXTENSION}"));
        let body = format!("{{\"type\":\"user\",\"cwd\":\"{message_cwd}\",\"sessionId\":\"{sid}\"}}\n");
        fs::write(&path, body).expect("write transcript");
    }

    #[test]
    fn heal_spawn_cwd_uses_injected_projects_root() {
        let root = scratch("nice-heal-root");
        // `recovered` must exist on disk for the heal to be adopted.
        let recovered = scratch("nice-heal-recovered");
        let recovered_str = recovered.0.to_string_lossy().to_string();
        let persisted = "/tmp/nice-heal-persisted-does-not-exist";
        plant_transcript(&root.0, &recovered_str, "sid-inject", &recovered_str);

        let healed = heal_spawn_cwd("sid-inject", persisted, &root.0);
        assert_eq!(
            healed.as_deref(),
            Some(recovered_str.as_str()),
            "heal_spawn_cwd must scan the injected projects root, not $HOME"
        );
    }

    #[test]
    fn heal_spawn_cwd_returns_none_when_injected_root_is_empty() {
        let root = scratch("nice-heal-emptyroot");
        // No transcripts planted — directory exists but is empty.
        let healed = heal_spawn_cwd("sid-empty", "/tmp/nope", &root.0);
        assert_eq!(
            healed, None,
            "empty projects root must yield None with no enumeration crash"
        );
    }

    #[test]
    fn heal_spawn_cwd_returns_none_when_transcript_already_at_expected_bucket() {
        // No heal necessary: the transcript already lives at the bucket the
        // persisted cwd implies.
        let root = scratch("nice-heal-noop");
        let persisted = scratch("nice-heal-persisted-exists");
        let persisted_str = persisted.0.to_string_lossy().to_string();
        plant_transcript(&root.0, &persisted_str, "sid-noop", &persisted_str);
        assert_eq!(heal_spawn_cwd("sid-noop", &persisted_str, &root.0), None);
    }

    #[test]
    fn heal_spawn_cwd_returns_none_when_recovered_path_gone() {
        // Transcript recovered from another bucket, but its cwd no longer exists
        // on disk → abandon the heal (resume is unrecoverable either way).
        let root = scratch("nice-heal-gone");
        let bucket_cwd = "/tmp/nice-heal-some-bucket";
        let phantom = "/tmp/nice-heal-phantom-does-not-exist";
        plant_transcript(&root.0, bucket_cwd, "sid-gone", phantom);
        assert_eq!(
            heal_spawn_cwd("sid-gone", "/tmp/nice-heal-persisted-x", &root.0),
            None
        );
    }
}
