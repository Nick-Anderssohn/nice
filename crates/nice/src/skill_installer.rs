//! Nice Handoff skill installer (R26) — ports Swift `SkillInstaller`
//! (`Sources/Nice/Process/SkillInstaller.swift`).
//!
//! Installs (or removes) the two files that make the `/nice-handoff` Claude
//! Code skill work:
//!   1. a `SKILL.md` skill definition under
//!      `~/.claude/skills/nice-handoff/SKILL.md`, and
//!   2. a bash helper at `~/.nice/nice-handoff.sh` (mode 0755) that posts a
//!      `handoff` message to Nice's control socket.
//!
//! **Identity is the unsuffixed prod name `nice-handoff` (Swift parity).**
//! This build IS Nice (prod `Nice` / dev `Nice Dev`, having replaced the Swift
//! app), so it installs the SAME `~/.claude/skills/nice-handoff/` +
//! `~/.nice/nice-handoff.sh` / `name: nice-handoff` / `/nice-handoff` the retired
//! Swift `Nice` installed — an upgrading user keeps the exact same skill with no
//! visible change. The `SKILL.md` + helper bytes are byte-identical to the Swift
//! literals, so a launch over a Swift-installed copy is a no-op (write-only-if-
//! changed). Consequently this installer now DELIBERATELY owns the prod skill
//! path: toggle-off / uninstall `rm -rf`s `~/.claude/skills/nice-handoff/`. That
//! is correct for the single-identity world (there is no other Nice to clobber);
//! the earlier `-rs`-suffixed isolation (Binding decision D2) is retired now that
//! the Rust build no longer coexists with a separate Swift `Nice`.
//!
//! Modelled byte-for-byte on the landed [`crate::claude_hook_installer`]:
//! [`sync`] resolves the base dirs from `$HOME`; [`sync_with`] takes injectable
//! dirs so tests / self-test scenarios sandbox against scratch dirs and never
//! touch the developer's real `~/.claude` / `~/.nice` (tranche-3 hermeticity).
//! Both entry points log-and-swallow failures — the app runs fine without the
//! skill; only the handoff feature degrades. The REAL installer runs from
//! `app::run` ONLY (the bootstrap reconcile, the toggle handler, the
//! first-launch prompt buttons), NEVER `run_selftest`.
//!
//! Idempotency: [`install_with`] writes a file only when the on-disk bytes
//! differ from the const, keeping mtime/ctime stable across no-op launches (the
//! helper's mode 0755 is likewise reset only on a real (re)write).
//! [`uninstall_with`] is asymmetric: it removes the whole `nice-handoff/`
//! skill SUBTREE (Nice owns that name) but only the helper FILE — `~/.nice/`
//! itself is SHARED with the R16 hook and must survive. Missing files are not an
//! error (idempotent).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic_file::write_atomic;

/// The `SKILL.md` skill definition written to `<skill_dir>/SKILL.md` (via
/// `write_atomic(_, _, None)`). Byte-identical to the Swift `skillMarkdown`
/// literal (`name: nice-handoff`, `~/.nice/nice-handoff.sh`, `/nice-handoff`) —
/// verified equal to the retired Swift build's literal — with NO trailing
/// newline, so the write-only-if-changed byte compare is exact and a launch over
/// a Swift-installed copy is a no-op.
pub const SKILL_MARKDOWN: &str = r#"---
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

Run the helper, passing three arguments:

1. The **absolute path** to the handoff file you just wrote.
2. Any arguments the user provided to this skill, forwarded verbatim
   (or an empty string `""` when the user provided none).
3. Your **exact current model id** — the precise `claude-…` identifier
   you are running as right now (e.g. `claude-opus-4-8`), so the fresh
   session continues on the same model. If you are not certain of your
   exact model id, pass an empty string `""` rather than guessing; the
   new session then falls back to the default model.

```
~/.nice/nice-handoff.sh "<absolute path to the handoff file>" "$ARGUMENTS" "<your exact model id>"
```

If the user provided no arguments to this skill, pass an empty string
for the second argument:

```
~/.nice/nice-handoff.sh "<absolute path to the handoff file>" "" "<your exact model id>"
```

The second argument lets the user customise what the new session does
after reading the handoff file. When it is empty the new session will
read the handoff file and then wait for the user to say how to proceed —
it does not start working on its own. When the user passes a custom
instruction (e.g. `/nice-handoff keep going` or `/nice-handoff focus only
on the UI layer`) that string tells the new session what to do after
reading the file, so it can continue the work right away.

The third argument carries your model id so the new tab launches on the
same model. Your effort level is forwarded automatically by the helper
(it reads `CLAUDE_EFFORT` from the environment), so you do not pass it.

## 3. Report back

Tell the user that the handoff tab is opening (or relay any error the
helper printed to stderr). Keep it brief — one or two sentences."#;

/// The bash helper written to `<helper_dir>/nice-handoff.sh` (via
/// `write_atomic(_, _, Some(0o755))`). Ported from the Swift `helperScript`
/// literal (`SkillInstaller.swift:267-352`) — byte-identical, including the
/// `"action":"handoff"` FROZEN wire protocol. NO trailing newline (Swift-literal
/// parity).
///
/// The `_nice_esc` tab-`sed` (`s/<TAB>/\t/g`) carries a LITERAL horizontal-tab
/// byte between the slashes — load-bearing, preserved verbatim.
pub const HELPER_SCRIPT: &str = r#"#!/usr/bin/env bash
# nice-handoff.sh — opens a new Nice tab pre-loaded with a handoff file
# so a fresh Claude session can continue the current work. Posts a JSON
# `handoff` message to Nice's control socket.
# Installed automatically by Nice; safe to delete.
#
# Args: $1 = absolute path to handoff file (required)
#       $2 = continuation instructions (optional)
#       $3 = model id to launch the new session with (optional)
# The effort level is NOT an argument: it is read from the CLAUDE_EFFORT
# environment variable Claude Code exports into the pane, so the new
# session inherits the current effort tier automatically. CLAUDE_EFFORT
# already holds the literal `claude --effort` token (low/medium/high/
# xhigh/max) — Nice forwards it verbatim and does NOT translate it.
# Both model and effort are forwarded empty-when-unknown; Nice omits the
# matching launch flag for any empty value.
set -u

if [ -z "${NICE_SOCKET:-}" ] || [ -z "${NICE_PANE_ID:-}" ]; then
  printf 'nice: not running inside a Nice pane; cannot open a handoff tab\n' >&2
  exit 1
fi

HANDOFF_FILE="${1:-}"
if [ -z "$HANDOFF_FILE" ]; then
  printf 'usage: nice-handoff.sh <absolute-path-to-handoff-file> [instructions] [model]\n' >&2
  exit 1
fi

INSTRUCTIONS="${2:-}"
MODEL="${3:-}"
# Effort tier is read from the environment, not passed as an argument:
# Claude Code exports CLAUDE_EFFORT (e.g. "xhigh") into the pane. Empty
# when the user is at the implicit default — Nice then omits --effort.
EFFORT="${CLAUDE_EFFORT:-}"

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
MODEL_ESC=$(_nice_esc "$MODEL")
EFFORT_ESC=$(_nice_esc "$EFFORT")

PAYLOAD=$(printf '{"action":"handoff","cwd":"%s","handoffFile":"%s","tabId":"%s","paneId":"%s","instructions":"%s","model":"%s","effort":"%s"}' \
  "$CWD_ESC" "$HANDOFF_ESC" "$TAB_ID_ESC" "$PANE_ID_ESC" "$INSTRUCTIONS_ESC" "$MODEL_ESC" "$EFFORT_ESC")

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
esac"#;

/// Filename of the installed skill definition inside the skill dir.
pub const SKILL_FILENAME: &str = "SKILL.md";

/// Filename of the installed helper script inside the helper dir — the prod name
/// `nice-handoff.sh`, matching the retired Swift build.
pub const HELPER_FILENAME: &str = "nice-handoff.sh";

/// Reconcile the on-disk skill files to `enabled` against the real `$HOME` —
/// the production entry (bootstrap reconcile, toggle handler, first-launch
/// prompt buttons). `enabled ⇒ install`, else `⇒ uninstall`. Resolves
/// [`default_skill_dir`] / [`default_helper_dir`] from `$HOME` and delegates to
/// [`sync_with`]. Call from `app::run` ONLY (NEVER `run_selftest` — the
/// regression suite must not write the real `~/.claude` / `~/.nice`). Failures
/// are logged and swallowed.
pub fn sync(enabled: bool) {
    sync_with(enabled, &default_skill_dir(), &default_helper_dir());
}

/// Test/scenario-friendly entry point: production [`sync`] resolves the base
/// dirs from `$HOME`; callers here pass them directly so they can sandbox
/// against scratch dirs without touching the developer's real `~/.claude` /
/// `~/.nice`. `enabled ⇒ install_with`, else `⇒ uninstall_with`; the `Result`
/// is logged and swallowed.
pub fn sync_with(enabled: bool, skill_dir: &Path, helper_dir: &Path) {
    let result = if enabled {
        install_with(skill_dir, helper_dir)
    } else {
        uninstall_with(skill_dir, helper_dir)
    };
    if let Err(e) = result {
        eprintln!("nice: SkillInstaller: sync(enabled={enabled}) failed: {e}");
    }
}

/// Install both files: the `SKILL.md` definition into `skill_dir` and the helper
/// into `helper_dir`. Write-only-if-changed (mtime stable on no-op).
fn install_with(skill_dir: &Path, helper_dir: &Path) -> io::Result<()> {
    ensure_skill_installed(skill_dir)?;
    ensure_helper_installed(helper_dir)?;
    Ok(())
}

/// Write [`SKILL_MARKDOWN`] into `dir/SKILL.md` (default mode). Skips the write
/// when the on-disk bytes already match — keeping mtime/ctime stable across
/// no-op launches.
fn ensure_skill_installed(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(SKILL_FILENAME);
    if fs::read_to_string(&path).ok().as_deref() == Some(SKILL_MARKDOWN) {
        return Ok(());
    }
    write_atomic(&path, SKILL_MARKDOWN.as_bytes(), None)
}

/// Write [`HELPER_SCRIPT`] into `dir/nice-handoff.sh` at mode 0755. Skips
/// BOTH the write and the perms reset when the on-disk bytes already match —
/// the mode 0755 is (re)applied ONLY on a real (re)write.
fn ensure_helper_installed(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(HELPER_FILENAME);
    if fs::read_to_string(&path).ok().as_deref() == Some(HELPER_SCRIPT) {
        return Ok(());
    }
    write_atomic(&path, HELPER_SCRIPT.as_bytes(), Some(0o755))
}

/// Remove the installed files: the WHOLE `skill_dir` subtree (Nice owns the
/// `nice-handoff/` name) IF it exists, and the helper FILE only — NEVER
/// `remove_dir` on `helper_dir`, because `~/.nice/` is SHARED with the R16 hook.
/// Missing files are not an error (idempotent).
fn uninstall_with(skill_dir: &Path, helper_dir: &Path) -> io::Result<()> {
    if skill_dir.exists() {
        fs::remove_dir_all(skill_dir)?;
    }
    let helper = helper_dir.join(HELPER_FILENAME);
    if helper.exists() {
        fs::remove_file(&helper)?;
    }
    Ok(())
}

/// `~/.claude/skills/nice-handoff` — the prod skill dir (matching the retired
/// Swift build). Nice owns the whole subtree, so uninstall removes it entirely.
fn default_skill_dir() -> PathBuf {
    PathBuf::from(home_dir()).join(".claude/skills/nice-handoff")
}

/// `~/.nice/` — the SHARED no-space dotdir (also home to the R16 hook script).
/// Uninstall removes only the helper FILE inside it, never the dir.
fn default_helper_dir() -> PathBuf {
    PathBuf::from(home_dir()).join(".nice")
}

/// The process `$HOME`, falling back to `/` (production-only; the app always has
/// a real home). Tests never call this — they drive [`sync_with`] /
/// [`install_with`] / [`uninstall_with`] directly. Copied from
/// [`crate::claude_hook_installer`]'s `home_dir`.
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---- temp-dir plumbing (mirrors claude_hook_installer.rs:385-416) ------

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

    /// A fresh scratch root (auto-removed) plus the two injected dirs a test
    /// drives: `skill_dir` (`<root>/claude/skills/nice-handoff`) and
    /// `helper_dir` (`<root>/nice`). Neither is the developer's real `~/.claude`
    /// / `~/.nice` (hermeticity). The dirs are NOT pre-created — the installer
    /// `create_dir_all`s them, matching the production path.
    fn sandbox(prefix: &str) -> (Scratch, PathBuf, PathBuf) {
        let root = scratch(prefix);
        let skill_dir = root.0.join("claude").join("skills").join("nice-handoff");
        let helper_dir = root.0.join("nice");
        (root, skill_dir, helper_dir)
    }

    // ---- content pins ------------------------------------------------------

    /// `SKILL_MARKDOWN` carries the UNSUFFIXED prod identity `nice-handoff`
    /// (byte-parity with the retired Swift build), with no `-rs` suffix leaking.
    #[test]
    fn skill_markdown_content_pins() {
        assert!(
            SKILL_MARKDOWN.starts_with("---\nname: nice-handoff\n"),
            "SKILL.md must open with the prod frontmatter name (Swift parity)"
        );
        assert!(
            SKILL_MARKDOWN.contains("~/.nice/nice-handoff.sh"),
            "SKILL.md must reference the prod helper path"
        );
        assert!(
            SKILL_MARKDOWN.contains("/nice-handoff"),
            "SKILL.md must reference the prod slash command"
        );
        // The `-rs` dev identity is retired: no suffixed name may leak, or an
        // upgrading user would see a stale duplicate skill. (String built so a
        // future global `nice-handoff-rs`→`nice-handoff` replace can't neuter it.)
        let retired = format!("nice-handoff{}", "-rs");
        assert!(
            !SKILL_MARKDOWN.contains(&retired),
            "SKILL.md must not carry the retired -rs identity"
        );
        assert!(
            !SKILL_MARKDOWN.ends_with('\n'),
            "SKILL.md must have no trailing newline (Swift-literal parity)"
        );
    }

    /// `HELPER_SCRIPT` is a bash script carrying the frozen wire schema + the
    /// absolute `nc` invocation, the `nice-handoff.sh` self-reference, and no
    /// trailing newline.
    #[test]
    fn helper_script_content_pins() {
        assert!(
            HELPER_SCRIPT.starts_with("#!/usr/bin/env bash\n"),
            "helper must start with the bash shebang"
        );
        assert!(
            HELPER_SCRIPT.contains("# nice-handoff.sh —"),
            "helper header must self-reference the prod name"
        );
        assert!(
            HELPER_SCRIPT.contains(
                r#"{"action":"handoff","cwd":"%s","handoffFile":"%s","tabId":"%s","paneId":"%s","instructions":"%s","model":"%s","effort":"%s"}"#
            ),
            "helper must carry the frozen handoff wire schema"
        );
        assert!(
            HELPER_SCRIPT.contains(r#"/usr/bin/nc -U -w 2 "$NICE_SOCKET""#),
            "helper must post via the absolute nc path"
        );
        // The load-bearing literal-tab sed pass (a real horizontal-tab byte
        // between the slashes) survives verbatim in the const.
        assert!(
            HELPER_SCRIPT.contains("/usr/bin/sed 's/\t/\\\\t/g'"),
            "helper must carry the literal-tab sed pass"
        );
        assert!(
            !HELPER_SCRIPT.ends_with('\n'),
            "helper must have no trailing newline (Swift-literal parity)"
        );
    }

    // ---- install writes both files + perms ---------------------------------

    /// `install_with` lays SKILL.md (default mode) and the helper (mode 0755)
    /// down with the exact const bytes.
    #[test]
    fn install_writes_both_files_and_helper_perms() {
        let (_root, skill_dir, helper_dir) = sandbox("skill-install");
        install_with(&skill_dir, &helper_dir).expect("install");

        let skill = fs::read_to_string(skill_dir.join(SKILL_FILENAME)).expect("read SKILL.md");
        assert_eq!(skill, SKILL_MARKDOWN, "SKILL.md must equal the const");

        let helper_path = helper_dir.join(HELPER_FILENAME);
        let helper = fs::read_to_string(&helper_path).expect("read helper");
        assert_eq!(helper, HELPER_SCRIPT, "helper must equal the const");

        let mode = fs::metadata(&helper_path).expect("stat helper").permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "helper must be mode 0755");
    }

    /// A second `install_with` over identical files rewrites NOTHING — both
    /// mtimes stay stable (the no-op-launch cheapness contract).
    #[test]
    fn install_is_mtime_stable_when_unchanged() {
        let (_root, skill_dir, helper_dir) = sandbox("skill-stable");
        install_with(&skill_dir, &helper_dir).expect("first install");
        let skill_path = skill_dir.join(SKILL_FILENAME);
        let helper_path = helper_dir.join(HELPER_FILENAME);
        let skill_m1 = fs::metadata(&skill_path).unwrap().modified().unwrap();
        let helper_m1 = fs::metadata(&helper_path).unwrap().modified().unwrap();

        install_with(&skill_dir, &helper_dir).expect("second install");
        let skill_m2 = fs::metadata(&skill_path).unwrap().modified().unwrap();
        let helper_m2 = fs::metadata(&helper_path).unwrap().modified().unwrap();

        assert_eq!(skill_m1, skill_m2, "unchanged SKILL.md must not be rewritten");
        assert_eq!(helper_m1, helper_m2, "unchanged helper must not be rewritten");
    }

    // ---- uninstall asymmetry -----------------------------------------------

    /// `uninstall_with` removes the whole skill SUBTREE and the helper FILE, but
    /// `helper_dir` itself (`~/.nice/`, shared with the R16 hook) SURVIVES — a
    /// planted sibling `nice-claude-hook.sh` is untouched. A second uninstall
    /// over already-absent files is a clean no-op.
    #[test]
    fn uninstall_removes_skill_subtree_and_helper_file_but_keeps_shared_dir() {
        let (_root, skill_dir, helper_dir) = sandbox("skill-uninstall");
        install_with(&skill_dir, &helper_dir).expect("install");

        // Plant the R16 hook sibling in the SHARED dir; it must survive uninstall.
        let sibling = helper_dir.join("nice-claude-hook.sh");
        fs::write(&sibling, b"#!/usr/bin/env bash\nexit 0").expect("plant sibling");

        uninstall_with(&skill_dir, &helper_dir).expect("uninstall");

        assert!(!skill_dir.exists(), "the whole nice-handoff/ subtree must be gone");
        assert!(
            !helper_dir.join(HELPER_FILENAME).exists(),
            "the helper file must be gone"
        );
        assert!(helper_dir.exists(), "the shared ~/.nice/ dir must survive");
        assert!(sibling.exists(), "the R16 hook sibling must be untouched");

        // Idempotent: a second uninstall over already-absent files does not panic
        // or surface an error.
        uninstall_with(&skill_dir, &helper_dir).expect("second uninstall is a clean no-op");
    }

    /// `sync_with(false)` on a fresh dir (nothing installed) creates nothing and
    /// does not error — removing absent files is a clean no-op.
    #[test]
    fn sync_with_false_on_fresh_dir_is_a_noop() {
        let (_root, skill_dir, helper_dir) = sandbox("skill-sync-false");
        // Nothing installed yet; neither dir exists.
        sync_with(false, &skill_dir, &helper_dir);
        assert!(!skill_dir.exists(), "sync_with(false) must not create the skill dir");
        assert!(
            !helper_dir.join(HELPER_FILENAME).exists(),
            "sync_with(false) must not create the helper file"
        );
    }

    /// `sync_with(true)` then `sync_with(false)` round-trips: install lands both
    /// files, uninstall removes them — the injectable entry the toggle handler
    /// and scenario drive.
    #[test]
    fn sync_with_round_trip() {
        let (_root, skill_dir, helper_dir) = sandbox("skill-sync-roundtrip");
        sync_with(true, &skill_dir, &helper_dir);
        assert!(skill_dir.join(SKILL_FILENAME).exists(), "install lands SKILL.md");
        assert!(helper_dir.join(HELPER_FILENAME).exists(), "install lands the helper");

        sync_with(false, &skill_dir, &helper_dir);
        assert!(!skill_dir.exists(), "uninstall removes the skill subtree");
        assert!(
            !helper_dir.join(HELPER_FILENAME).exists(),
            "uninstall removes the helper file"
        );
    }
}
