//! Nice skill installer (R26) — ports Swift `SkillInstaller`
//! (`Sources/Nice/Process/SkillInstaller.swift`), extended to install a TABLE of
//! skill/helper pairs rather than the single handoff pair.
//!
//! Each entry of [`INSTALLED_PAIRS`] is one Claude Code skill plus the bash
//! helper it runs:
//!   1. a `SKILL.md` skill definition under
//!      `~/.claude/skills/<skill-name>/SKILL.md`, and
//!   2. a bash helper at `~/.nice/<helper>.sh` (mode 0755) that posts a message
//!      to Nice's control socket.
//!
//! Today that is two pairs:
//!   * **`nice-handoff`** + `~/.nice/nice-handoff.sh` → the `handoff` socket
//!     action (hand the current work to a fresh session in a new tab).
//!   * **`nice-dispatch`** + `~/.nice/nice-dispatch.sh` → the `dispatch` socket
//!     action (farm a task brief out to a fresh session in its OWN `claude
//!     --worktree` tab, opened in the background).
//!
//! Both ride the SINGLE `installHandoffSkill` CFPref toggle (there is no second
//! toggle): flipping it on installs every pair, off removes every pair.
//!
//! **Identity is the unsuffixed prod name (Swift parity).** This build IS Nice
//! (prod `Nice` / dev `Nice Dev`, having replaced the Swift app), so it installs
//! the SAME `~/.claude/skills/nice-handoff/` + `~/.nice/nice-handoff.sh` /
//! `name: nice-handoff` / `/nice-handoff` the retired Swift `Nice` installed — an
//! upgrading user keeps the exact same skill with no visible change. The handoff
//! `SKILL.md` + helper bytes are byte-identical to the Swift literals, so a launch
//! over a Swift-installed copy is a no-op (write-only-if-changed). Consequently
//! this installer DELIBERATELY owns the prod skill paths: toggle-off / uninstall
//! `rm -rf`s `~/.claude/skills/<skill-name>/` for every pair. That is correct for
//! the single-identity world (there is no other Nice to clobber); the earlier
//! `-rs`-suffixed isolation (Binding decision D2) is retired now that the Rust
//! build no longer coexists with a separate Swift `Nice`.
//!
//! Modelled byte-for-byte on the landed [`crate::claude_hook_installer`]:
//! [`sync`] resolves the base dirs from `$HOME`; [`sync_with`] takes injectable
//! dirs so tests / self-test scenarios sandbox against scratch dirs and never
//! touch the developer's real `~/.claude` / `~/.nice` (tranche-3 hermeticity).
//! Both entry points log-and-swallow failures — the app runs fine without the
//! skills; only the handoff / dispatch features degrade. The REAL installer runs
//! from `app::run` ONLY (the bootstrap reconcile, the toggle handler, the
//! first-launch prompt buttons), NEVER `run_selftest`.
//!
//! Idempotency: [`install_with`] writes a file only when the on-disk bytes
//! differ from the const, keeping mtime/ctime stable across no-op launches (a
//! helper's mode 0755 is likewise reset only on a real (re)write).
//! [`uninstall_with`] is asymmetric: it removes each pair's whole
//! `<skill-name>/` skill SUBTREE (Nice owns those names) but only each helper
//! FILE — neither the shared `~/.claude/skills/` root (full of the user's OWN
//! skills) nor `~/.nice/` (SHARED with the R16 hook) is ever removed. Missing
//! files are not an error (idempotent).

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

/// The `SKILL.md` written to `~/.claude/skills/nice-dispatch/SKILL.md`. Teaches
/// the DISPATCHER-side Claude the four steps of a dispatch: pick a kebab-case
/// worktree name, write the task brief into the MAIN checkout's
/// `.claude/dispatch/`, run [`DISPATCH_HELPER_SCRIPT`], report back.
///
/// Two facts here are load-bearing and must stay in step with the rest of the
/// feature: the brief lives in the MAIN checkout (the child's cwd becomes the
/// worktree, and `dispatch_extra_args` `--add-dir`s the brief's directory in for
/// exactly that reason), and model/effort are per-dispatch OVERRIDES only — a
/// dispatch deliberately does NOT inherit the dispatcher's, the opposite of
/// `/nice-handoff`. No trailing newline (parity with the handoff const, so the
/// write-only-if-changed byte compare is exact).
pub const DISPATCH_SKILL_MARKDOWN: &str = r#"---
name: nice-dispatch
description: Dispatch a task to a fresh Claude session working in its own git worktree, in a new background Nice tab. Use when the user asks to dispatch, farm out, or parallelise a task into its own worktree. Writes a task brief and opens a nested tab running `claude --worktree <name>` on it, without stealing focus from the current tab.
---

Follow these steps exactly to dispatch a task to a new worktree session.

## 1. Choose the worktree name

Pick a short kebab-case name (e.g. `fix-drag-crash`, `sidebar-perf`). Use
the name the user gave you when they named one; otherwise derive it from
the task. Pass it through verbatim — nothing checks it for clashes, and
whatever `claude --worktree <existing-name>` does is the behaviour.

## 2. Write the task file

Resolve the MAIN checkout root first. The dispatching session may itself
be running inside a worktree, and the task file must live in the main
checkout — worktrees are always created from the canonical checkout:

```
git rev-parse --path-format=absolute --git-common-dir
```

Strip the trailing `/.git` from that path; the result is the main root.
Create `<main-root>/.claude/dispatch/` if it does not already exist, then
write the task file at:

```
<main-root>/.claude/dispatch/<worktree-name>-<UTC timestamp>.md
```

where `<UTC timestamp>` uses the format `20060102-150405` (year, month,
day, hyphen, hour, minute, second — all in UTC, zero-padded). Example:
`sidebar-perf-20240315-143022.md`.

The brief must be thorough enough that a fresh Claude session with **no
prior context** can start working immediately, without asking clarifying
questions. Include all of:

- **The goal** — what the dispatched session must build, fix, or
  investigate, stated as something it can start on right away.
- **Context and why** — the background it cannot infer from the code.
- **Constraints** — what it must not change, conventions to follow, and
  decisions already made that are not up for re-litigation.
- **Ordered first steps** — what to do first, second, third.
- **Key files and paths** — every file central to the task, each with a
  one-line note about its role.
- **Gotchas** — traps, non-obvious behaviour, and mistakes to avoid.

End the brief by telling the session to commit its work on its worktree
branch.

## 3. Open the dispatch tab

Run the helper with five arguments:

```
~/.nice/nice-dispatch.sh "<worktree name>" "<absolute path to the task file>" "<instructions>" "<model>" "<effort>"
```

1. The **worktree name** from step 1.
2. The **absolute path** to the task file you just wrote.
3. **Instructions** — any extra steer appended to the dispatched
   session's opening prompt. Pass an empty string when there is none.
4. **Model** — a per-dispatch override, empty unless the user explicitly
   asked for one (e.g. "dispatch this on opus").
5. **Effort** — a per-dispatch override too, empty unless the user
   explicitly asked (e.g. "dispatch it at high effort").

Model and effort are OVERRIDES ONLY: leave both empty and the dispatched
session launches on the user's configured defaults. Do NOT forward your
own model or effort — unlike `/nice-handoff`, a dispatch deliberately
does not inherit them. When the user does ask for one, pass their string
verbatim, whether it is an alias like `opus` or a full model id.

So the common case, with no instructions and no overrides, is:

```
~/.nice/nice-dispatch.sh "<worktree name>" "<absolute path to the task file>" "" "" ""
```

To dispatch several tasks, repeat steps 1-3 once per task: each gets its
own worktree name, its own task file, and its own tab.

## 4. Report back

Tell the user the dispatch tab is opening in the background and that the
current tab keeps focus, naming the worktree (or relay any error the
helper printed to stderr). Keep it brief — one or two sentences."#;

/// The bash helper written to `<helper_dir>/nice-dispatch.sh` (mode 0755) —
/// posts the `dispatch` control-socket message. Shaped on [`HELPER_SCRIPT`]
/// (same `NICE_SOCKET`/`NICE_PANE_ID` guard, the verbatim `_nice_esc`
/// JSON-escaper, the same `nc -U -w 2` post and reply handling), with two
/// deliberate deltas:
///
///   * **No `CLAUDE_EFFORT` fallback.** Handoff inherits the dispatcher's
///     effort from the environment; dispatch does the opposite by decision — an
///     empty `$5` means Nice omits `--effort` and the child runs at the user's
///     configured default.
///   * **It resolves the MAIN checkout root itself** (`git rev-parse
///     --path-format=absolute --git-common-dir`, minus the trailing `/.git`)
///     instead of posting `$PWD`, so a dispatcher running INSIDE a worktree
///     still creates the new worktree from the canonical checkout. A layout
///     whose common dir does not end in `/.git` (bare repos and friends) errors
///     out rather than guessing.
///
/// The `_nice_esc` tab-`sed` (`s/<TAB>/\t/g`) carries a LITERAL horizontal-tab
/// byte between the slashes — load-bearing, preserved verbatim. NO trailing
/// newline (parity with [`HELPER_SCRIPT`]).
pub const DISPATCH_HELPER_SCRIPT: &str = r#"#!/usr/bin/env bash
# nice-dispatch.sh — opens a new background Nice tab running
# `claude --worktree <name>` on a task file the dispatching session wrote,
# so the task is worked in its own worktree. Posts a JSON `dispatch`
# message to Nice's control socket.
# Installed automatically by Nice; safe to delete.
#
# Args: $1 = worktree name (required)
#       $2 = absolute path to the task file (required)
#       $3 = extra instructions (optional)
#       $4 = model to launch the dispatched session with (optional)
#       $5 = effort tier to launch it with (optional)
# Unlike nice-handoff.sh there is NO CLAUDE_EFFORT fallback: a dispatched
# session deliberately does NOT inherit the dispatcher's model or effort.
# An empty $4/$5 means Nice omits the matching launch flag and the child
# runs on the user's configured defaults.
set -u

if [ -z "${NICE_SOCKET:-}" ] || [ -z "${NICE_PANE_ID:-}" ]; then
  printf 'nice: not running inside a Nice pane; cannot open a dispatch tab\n' >&2
  exit 1
fi

WORKTREE_NAME="${1:-}"
TASK_FILE="${2:-}"
if [ -z "$WORKTREE_NAME" ] || [ -z "$TASK_FILE" ]; then
  printf 'usage: nice-dispatch.sh <worktree-name> <absolute-path-to-task-file> [instructions] [model] [effort]\n' >&2
  exit 1
fi

INSTRUCTIONS="${3:-}"
MODEL="${4:-}"
EFFORT="${5:-}"

# Resolve the MAIN checkout root — NOT $PWD. The dispatching session may
# itself be inside a worktree; --git-common-dir points at the SHARED .git
# directory (the main checkout's) from anywhere in the repo, so stripping
# the trailing /.git yields the main root. Layouts that don't end in /.git
# (bare repos and friends) are an error rather than a guess.
GIT_COMMON_DIR=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
if [ -z "$GIT_COMMON_DIR" ]; then
  printf 'nice: not inside a git repository; cannot resolve the main checkout root\n' >&2
  exit 1
fi
case "$GIT_COMMON_DIR" in
  */.git)
    MAIN_ROOT="${GIT_COMMON_DIR%/.git}"
    ;;
  *)
    printf 'nice: unexpected git layout (%s); cannot resolve the main checkout root\n' "$GIT_COMMON_DIR" >&2
    exit 1
    ;;
esac

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

CWD_ESC=$(_nice_esc "$MAIN_ROOT")
WORKTREE_ESC=$(_nice_esc "$WORKTREE_NAME")
TASK_FILE_ESC=$(_nice_esc "$TASK_FILE")
TAB_ID_ESC=$(_nice_esc "${NICE_TAB_ID:-}")
PANE_ID_ESC=$(_nice_esc "$NICE_PANE_ID")
INSTRUCTIONS_ESC=$(_nice_esc "$INSTRUCTIONS")
MODEL_ESC=$(_nice_esc "$MODEL")
EFFORT_ESC=$(_nice_esc "$EFFORT")

PAYLOAD=$(printf '{"action":"dispatch","cwd":"%s","worktreeName":"%s","taskFile":"%s","tabId":"%s","paneId":"%s","instructions":"%s","model":"%s","effort":"%s"}' \
  "$CWD_ESC" "$WORKTREE_ESC" "$TASK_FILE_ESC" "$TAB_ID_ESC" "$PANE_ID_ESC" "$INSTRUCTIONS_ESC" "$MODEL_ESC" "$EFFORT_ESC")

REPLY=$(printf '%s\n' "$PAYLOAD" | /usr/bin/nc -U -w 2 "$NICE_SOCKET")

if [ -z "$REPLY" ]; then
  printf 'nice: no reply from control socket; dispatch tab may not have opened\n' >&2
  exit 1
fi

case "$REPLY" in
  error*)
    printf '%s\n' "$REPLY" >&2
    exit 1
    ;;
  *)
    printf 'nice: dispatch tab opening…\n'
    exit 0
    ;;
esac"#;

/// Filename of the installed skill definition inside every skill dir.
pub const SKILL_FILENAME: &str = "SKILL.md";

/// Filename of the installed handoff helper inside the helper dir — the prod
/// name `nice-handoff.sh`, matching the retired Swift build.
pub const HELPER_FILENAME: &str = "nice-handoff.sh";

/// Filename of the installed dispatch helper inside the helper dir.
pub const DISPATCH_HELPER_FILENAME: &str = "nice-dispatch.sh";

/// One skill/helper pair the toggle installs and removes as a unit.
pub struct SkillPair {
    /// Directory name under the skills root (`~/.claude/skills/<skill_dir_name>`),
    /// which is also the skill's `name:` frontmatter and its slash command. Nice
    /// OWNS this name: uninstall removes the whole subtree.
    pub skill_dir_name: &'static str,
    /// The `SKILL.md` bytes written into that dir (default mode).
    pub skill_markdown: &'static str,
    /// The helper's filename inside the SHARED helper dir (`~/.nice/`).
    pub helper_filename: &'static str,
    /// The helper's bytes, written at mode 0755.
    pub helper_script: &'static str,
}

/// Every pair the `installHandoffSkill` toggle installs — handoff and dispatch.
/// Adding a pair here is the whole job: [`install_with`] / [`uninstall_with`]
/// iterate this table, so install/uninstall stay symmetric by construction.
pub const INSTALLED_PAIRS: &[SkillPair] = &[
    SkillPair {
        skill_dir_name: "nice-handoff",
        skill_markdown: SKILL_MARKDOWN,
        helper_filename: HELPER_FILENAME,
        helper_script: HELPER_SCRIPT,
    },
    SkillPair {
        skill_dir_name: "nice-dispatch",
        skill_markdown: DISPATCH_SKILL_MARKDOWN,
        helper_filename: DISPATCH_HELPER_FILENAME,
        helper_script: DISPATCH_HELPER_SCRIPT,
    },
];

/// Reconcile the on-disk skill files to `enabled` against the real `$HOME` —
/// the production entry (bootstrap reconcile, toggle handler, first-launch
/// prompt buttons). `enabled ⇒ install`, else `⇒ uninstall`, for EVERY pair in
/// [`INSTALLED_PAIRS`]. Resolves [`default_skills_root`] / [`default_helper_dir`]
/// from `$HOME` and delegates to [`sync_with`]. Call from `app::run` ONLY (NEVER
/// `run_selftest` — the regression suite must not write the real `~/.claude` /
/// `~/.nice`). Failures are logged and swallowed.
pub fn sync(enabled: bool) {
    sync_with(enabled, &default_skills_root(), &default_helper_dir());
}

/// Test/scenario-friendly entry point: production [`sync`] resolves the base
/// dirs from `$HOME`; callers here pass them directly so they can sandbox
/// against scratch dirs without touching the developer's real `~/.claude` /
/// `~/.nice`. `skills_root` is the skills PARENT (`~/.claude/skills`) — each
/// pair's own dir is `skills_root/<skill_dir_name>`. `enabled ⇒ install_with`,
/// else `⇒ uninstall_with`; the `Result` is logged and swallowed.
pub fn sync_with(enabled: bool, skills_root: &Path, helper_dir: &Path) {
    let result = if enabled {
        install_with(skills_root, helper_dir)
    } else {
        uninstall_with(skills_root, helper_dir)
    };
    if let Err(e) = result {
        eprintln!("nice: SkillInstaller: sync(enabled={enabled}) failed: {e}");
    }
}

/// Install every pair: each `SKILL.md` into `skills_root/<skill_dir_name>/` and
/// each helper into `helper_dir`. Write-only-if-changed (mtime stable on no-op).
fn install_with(skills_root: &Path, helper_dir: &Path) -> io::Result<()> {
    for pair in INSTALLED_PAIRS {
        ensure_skill_installed(&skills_root.join(pair.skill_dir_name), pair.skill_markdown)?;
        ensure_helper_installed(helper_dir, pair.helper_filename, pair.helper_script)?;
    }
    Ok(())
}

/// Write `markdown` into `dir/SKILL.md` (default mode). Skips the write when the
/// on-disk bytes already match — keeping mtime/ctime stable across no-op
/// launches.
fn ensure_skill_installed(dir: &Path, markdown: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(SKILL_FILENAME);
    if fs::read_to_string(&path).ok().as_deref() == Some(markdown) {
        return Ok(());
    }
    write_atomic(&path, markdown.as_bytes(), None)
}

/// Write `script` into `dir/<filename>` at mode 0755. Skips BOTH the write and
/// the perms reset when the on-disk bytes already match — the mode 0755 is
/// (re)applied ONLY on a real (re)write.
fn ensure_helper_installed(dir: &Path, filename: &str, script: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(filename);
    if fs::read_to_string(&path).ok().as_deref() == Some(script) {
        return Ok(());
    }
    write_atomic(&path, script.as_bytes(), Some(0o755))
}

/// Remove every pair's installed files: the WHOLE `skills_root/<skill_dir_name>`
/// subtree (Nice owns those names) IF it exists, and the helper FILE only.
/// NEVER `remove_dir` on `skills_root` (it holds the user's OWN skills) or on
/// `helper_dir` (`~/.nice/` is SHARED with the R16 hook). Missing files are not
/// an error (idempotent).
fn uninstall_with(skills_root: &Path, helper_dir: &Path) -> io::Result<()> {
    for pair in INSTALLED_PAIRS {
        let skill_dir = skills_root.join(pair.skill_dir_name);
        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir)?;
        }
        let helper = helper_dir.join(pair.helper_filename);
        if helper.exists() {
            fs::remove_file(&helper)?;
        }
    }
    Ok(())
}

/// `~/.claude/skills` — the skills ROOT, shared with every skill the USER
/// installed. Nice owns only its own `<skill_dir_name>` subdirs inside it, so
/// uninstall removes those and never the root.
fn default_skills_root() -> PathBuf {
    PathBuf::from(home_dir()).join(".claude/skills")
}

/// `~/.nice/` — the SHARED no-space dotdir (also home to the R16 hook script).
/// Uninstall removes only the helper FILES inside it, never the dir.
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
    /// drives: `skills_root` (`<root>/claude/skills`, the PARENT every pair's
    /// dir hangs off) and `helper_dir` (`<root>/nice`). Neither is the
    /// developer's real `~/.claude` / `~/.nice` (hermeticity). The dirs are NOT
    /// pre-created — the installer `create_dir_all`s them, matching the
    /// production path.
    fn sandbox(prefix: &str) -> (Scratch, PathBuf, PathBuf) {
        let root = scratch(prefix);
        let skills_root = root.0.join("claude").join("skills");
        let helper_dir = root.0.join("nice");
        (root, skills_root, helper_dir)
    }

    /// The handoff pair's skill dir under an injected skills root.
    fn handoff_skill_dir(skills_root: &Path) -> PathBuf {
        skills_root.join("nice-handoff")
    }

    /// The dispatch pair's skill dir under an injected skills root.
    fn dispatch_skill_dir(skills_root: &Path) -> PathBuf {
        skills_root.join("nice-dispatch")
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

    /// `DISPATCH_SKILL_MARKDOWN` carries the `nice-dispatch` identity, points at
    /// its own helper, and teaches the two decisions the rest of the feature
    /// depends on: the brief lives in the MAIN checkout, and model/effort are
    /// explicit overrides rather than inherited.
    #[test]
    fn dispatch_skill_markdown_content_pins() {
        assert!(
            DISPATCH_SKILL_MARKDOWN.starts_with("---\nname: nice-dispatch\n"),
            "the dispatch SKILL.md must open with its frontmatter name"
        );
        assert!(
            DISPATCH_SKILL_MARKDOWN.contains("~/.nice/nice-dispatch.sh"),
            "the dispatch SKILL.md must reference its own helper path"
        );
        assert!(
            DISPATCH_SKILL_MARKDOWN.contains("--path-format=absolute --git-common-dir"),
            "the dispatch SKILL.md must teach the main-checkout-root resolution"
        );
        assert!(
            DISPATCH_SKILL_MARKDOWN.contains("<main-root>/.claude/dispatch/"),
            "the dispatch SKILL.md must place the task file in the MAIN checkout"
        );
        assert!(
            DISPATCH_SKILL_MARKDOWN.contains("does not inherit them"),
            "the dispatch SKILL.md must state that model/effort are NOT inherited"
        );
        assert!(
            !DISPATCH_SKILL_MARKDOWN.ends_with('\n'),
            "the dispatch SKILL.md must have no trailing newline (const parity)"
        );
    }

    /// `DISPATCH_HELPER_SCRIPT` carries the dispatch wire schema, resolves the
    /// main root from `--git-common-dir` (never `$PWD`), reuses the `_nice_esc`
    /// passes verbatim, and — the locked decision — has NO `CLAUDE_EFFORT`
    /// fallback.
    #[test]
    fn dispatch_helper_script_content_pins() {
        assert!(
            DISPATCH_HELPER_SCRIPT.starts_with("#!/usr/bin/env bash\n"),
            "the dispatch helper must start with the bash shebang"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT.contains("# nice-dispatch.sh —"),
            "the dispatch helper header must self-reference its name"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT.contains(
                r#"{"action":"dispatch","cwd":"%s","worktreeName":"%s","taskFile":"%s","tabId":"%s","paneId":"%s","instructions":"%s","model":"%s","effort":"%s"}"#
            ),
            "the dispatch helper must carry the dispatch wire schema the parser reads"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT
                .contains("git rev-parse --path-format=absolute --git-common-dir"),
            "the dispatch helper must resolve the main root from --git-common-dir"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT.contains(r#"CWD_ESC=$(_nice_esc "$MAIN_ROOT")"#),
            "the dispatch helper must post the resolved MAIN root as cwd, not $PWD"
        );
        // The header comment NAMES `CLAUDE_EFFORT` to explain the difference; what
        // must not exist is a READ of it (handoff's `EFFORT="${CLAUDE_EFFORT:-}"`).
        assert!(
            !DISPATCH_HELPER_SCRIPT.contains("${CLAUDE_EFFORT"),
            "dispatch must NOT inherit the dispatcher's effort (locked decision)"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT.contains(r#"EFFORT="${5:-}""#),
            "the dispatch helper's effort must come from $5 only"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT.contains(r#"/usr/bin/nc -U -w 2 "$NICE_SOCKET""#),
            "the dispatch helper must post via the absolute nc path"
        );
        assert!(
            DISPATCH_HELPER_SCRIPT.contains("/usr/bin/sed 's/\t/\\\\t/g'"),
            "the dispatch helper must carry the literal-tab sed pass verbatim"
        );
        assert!(
            !DISPATCH_HELPER_SCRIPT.ends_with('\n'),
            "the dispatch helper must have no trailing newline (const parity)"
        );
    }

    /// The install table is well-formed: distinct skill dirs and distinct helper
    /// filenames, so no pair can silently clobber another's files.
    #[test]
    fn installed_pairs_are_distinct() {
        assert_eq!(INSTALLED_PAIRS.len(), 2, "handoff + dispatch are installed");
        for (i, a) in INSTALLED_PAIRS.iter().enumerate() {
            for b in INSTALLED_PAIRS.iter().skip(i + 1) {
                assert_ne!(
                    a.skill_dir_name, b.skill_dir_name,
                    "two pairs must not share a skill dir"
                );
                assert_ne!(
                    a.helper_filename, b.helper_filename,
                    "two pairs must not share a helper filename"
                );
            }
        }
    }

    // ---- install writes every pair + perms ---------------------------------

    /// `install_with` lays every pair's SKILL.md (default mode) and helper
    /// (mode 0755) down with the exact const bytes.
    #[test]
    fn install_writes_every_pair_and_helper_perms() {
        let (_root, skills_root, helper_dir) = sandbox("skill-install");
        install_with(&skills_root, &helper_dir).expect("install");

        for pair in INSTALLED_PAIRS {
            let skill_path = skills_root.join(pair.skill_dir_name).join(SKILL_FILENAME);
            let skill = fs::read_to_string(&skill_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", skill_path.display()));
            assert_eq!(
                skill, pair.skill_markdown,
                "{}/SKILL.md must equal the const",
                pair.skill_dir_name
            );

            let helper_path = helper_dir.join(pair.helper_filename);
            let helper = fs::read_to_string(&helper_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", helper_path.display()));
            assert_eq!(helper, pair.helper_script, "{} must equal the const", pair.helper_filename);

            let mode = fs::metadata(&helper_path).expect("stat helper").permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "{} must be mode 0755", pair.helper_filename);
        }

        // Both pairs land side by side under the one skills root.
        assert!(handoff_skill_dir(&skills_root).exists(), "the handoff skill dir exists");
        assert!(dispatch_skill_dir(&skills_root).exists(), "the dispatch skill dir exists");
    }

    /// A second `install_with` over identical files rewrites NOTHING — every
    /// mtime stays stable (the no-op-launch cheapness contract).
    #[test]
    fn install_is_mtime_stable_when_unchanged() {
        let (_root, skills_root, helper_dir) = sandbox("skill-stable");
        install_with(&skills_root, &helper_dir).expect("first install");

        let paths: Vec<PathBuf> = INSTALLED_PAIRS
            .iter()
            .flat_map(|p| {
                [
                    skills_root.join(p.skill_dir_name).join(SKILL_FILENAME),
                    helper_dir.join(p.helper_filename),
                ]
            })
            .collect();
        let before: Vec<_> =
            paths.iter().map(|p| fs::metadata(p).unwrap().modified().unwrap()).collect();

        install_with(&skills_root, &helper_dir).expect("second install");
        let after: Vec<_> =
            paths.iter().map(|p| fs::metadata(p).unwrap().modified().unwrap()).collect();

        for ((path, m1), m2) in paths.iter().zip(before).zip(after) {
            assert_eq!(m1, m2, "unchanged {} must not be rewritten", path.display());
        }
    }

    // ---- uninstall asymmetry -----------------------------------------------

    /// `uninstall_with` removes EVERY pair's skill SUBTREE and helper FILE, but
    /// the shared containers survive: `skills_root` itself (a planted unrelated
    /// user skill is untouched) and `helper_dir` (`~/.nice/`, shared with the
    /// R16 hook — a planted `nice-claude-hook.sh` is untouched). A second
    /// uninstall over already-absent files is a clean no-op.
    #[test]
    fn uninstall_removes_every_pair_but_keeps_shared_dirs() {
        let (_root, skills_root, helper_dir) = sandbox("skill-uninstall");
        install_with(&skills_root, &helper_dir).expect("install");

        // Plant a skill Nice does not own; uninstall must not touch it.
        let foreign = skills_root.join("someone-elses-skill");
        fs::create_dir_all(&foreign).expect("create foreign skill dir");
        fs::write(foreign.join(SKILL_FILENAME), b"not ours").expect("plant foreign skill");
        // Plant the R16 hook sibling in the SHARED helper dir; it must survive.
        let sibling = helper_dir.join("nice-claude-hook.sh");
        fs::write(&sibling, b"#!/usr/bin/env bash\nexit 0").expect("plant sibling");

        uninstall_with(&skills_root, &helper_dir).expect("uninstall");

        for pair in INSTALLED_PAIRS {
            assert!(
                !skills_root.join(pair.skill_dir_name).exists(),
                "the whole {}/ subtree must be gone",
                pair.skill_dir_name
            );
            assert!(
                !helper_dir.join(pair.helper_filename).exists(),
                "{} must be gone",
                pair.helper_filename
            );
        }
        assert!(skills_root.exists(), "the shared skills root must survive");
        assert!(foreign.join(SKILL_FILENAME).exists(), "a foreign skill must be untouched");
        assert!(helper_dir.exists(), "the shared ~/.nice/ dir must survive");
        assert!(sibling.exists(), "the R16 hook sibling must be untouched");

        // Idempotent: a second uninstall over already-absent files does not panic
        // or surface an error.
        uninstall_with(&skills_root, &helper_dir).expect("second uninstall is a clean no-op");
    }

    /// `sync_with(false)` on a fresh dir (nothing installed) creates nothing and
    /// does not error — removing absent files is a clean no-op.
    #[test]
    fn sync_with_false_on_fresh_dir_is_a_noop() {
        let (_root, skills_root, helper_dir) = sandbox("skill-sync-false");
        // Nothing installed yet; neither dir exists.
        sync_with(false, &skills_root, &helper_dir);
        assert!(!skills_root.exists(), "sync_with(false) must not create the skills root");
        for pair in INSTALLED_PAIRS {
            assert!(
                !helper_dir.join(pair.helper_filename).exists(),
                "sync_with(false) must not create {}",
                pair.helper_filename
            );
        }
    }

    /// `sync_with(true)` then `sync_with(false)` round-trips: install lands both
    /// pairs, uninstall removes them — the injectable entry the toggle handler
    /// and scenario drive.
    #[test]
    fn sync_with_round_trip() {
        let (_root, skills_root, helper_dir) = sandbox("skill-sync-roundtrip");
        sync_with(true, &skills_root, &helper_dir);
        for pair in INSTALLED_PAIRS {
            assert!(
                skills_root.join(pair.skill_dir_name).join(SKILL_FILENAME).exists(),
                "install lands {}/SKILL.md",
                pair.skill_dir_name
            );
            assert!(
                helper_dir.join(pair.helper_filename).exists(),
                "install lands {}",
                pair.helper_filename
            );
        }

        sync_with(false, &skills_root, &helper_dir);
        for pair in INSTALLED_PAIRS {
            assert!(
                !skills_root.join(pair.skill_dir_name).exists(),
                "uninstall removes the {}/ subtree",
                pair.skill_dir_name
            );
            assert!(
                !helper_dir.join(pair.helper_filename).exists(),
                "uninstall removes {}",
                pair.helper_filename
            );
        }
    }
}
