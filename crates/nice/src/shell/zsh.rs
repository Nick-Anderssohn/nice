//! [`ZshProfile`] — zsh's shell profile: the synthetic `ZDOTDIR` rc chain (R14).
//!
//! Ports Swift `MainTerminalShellInject`
//! (`Sources/Nice/Process/MainTerminalShellInject.swift`). We write a `ZDOTDIR`
//! directory the Main Terminal's zsh picks up. It contains stub `.zshenv` /
//! `.zprofile` / `.zlogin` / `.zshrc` that chain back to the user's real startup
//! files (resolved from `$NICE_USER_ZDOTDIR` if set, else by sourcing
//! `~/.zshenv` to discover the user's intended `ZDOTDIR`), then — in `.zshrc` —
//! restore `ZDOTDIR` to that intended value BEFORE sourcing the user's `.zshrc`
//! and define a `claude()` function that intercepts *interactive* invocations
//! and forwards them to Nice's control socket so a new session opens instead of the
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
//!   * `exec zsh` inside a Nice window drops the injection (the new zsh runs with
//!     the user's restored `ZDOTDIR`, not our temp value).
//!   * `/etc/zshenv` setting `ZDOTDIR` bypasses the injection entirely (zsh
//!     re-resolves `$ZDOTDIR/.zshenv` from the new value before reading our
//!     stub). macOS ships no `/etc/zshenv`, so this is documented, not fixed.
//!
//! Storage location: the stubs live in a fixed, per-variant directory under
//! Application Support (`…/<CFBundleName>/shellrc/zsh`) — NOT `$TMPDIR`. macOS's
//! `com.apple.bsd.dirhelper` sweeps `$TMPDIR` files older than 3 days; when Nice
//! ran longer than that, the sweep deleted the stubs out from under the live
//! process and every new window's zsh then sourced nothing. Application Support is
//! never swept. Because the stub contents are static, one shared directory
//! serves every window and every process of a variant; each variant stays
//! isolated in its own per-variant Application Support folder via `CFBundleName`.
//! [`write_stubs`] rewrites the stubs on every launch, so the directory
//! self-heals if anything ever removes a file. Older builds wrote the stubs one
//! level up, in `…/<CFBundleName>/zdotdir`; that directory is left in place on
//! disk rather than swept, because deleting shared Application Support state
//! would race an older build of the same variant that is still reading it.
//!
//! **The four rc-stub bodies in `scripts/zsh/` are a FROZEN compatibility
//! contract.** Installed helpers already on users' disks
//! (`~/.nice/nice-claude-hook.sh`, `~/.nice/nice-handoff.sh`) and the shadow
//! function's muscle-memory behavior must keep working byte-for-byte against the
//! app. They are ported character-for-character from the Swift source and pinned
//! by both the static-text tests and the real-zsh end-to-end tests below. Do not
//! "clean them up" — the `\%` OSC 7 escape is load-bearing zsh arcana (a bare `%`
//! is a parameter pattern anchor), and the `_nice_json_escape` dialect
//! (backslash, double-quote, LF, CR, tab — nothing else) is exactly what Nice's
//! JSON decoder expects. The Command Compose widget tail (`_nice_compose_*` +
//! [`COMPOSE_TRIGGER_SEQ`](super::COMPOSE_TRIGGER_SEQ)) joined the frozen
//! contract with the `commandCompose` shortcut: the trigger bytes and the
//! `$NICE_COMPOSE_CONF` key names are app↔shell interchange, pinned the same way.
//!
//! The bodies live as plain `.zsh` files pulled in with `include_str!` (design
//! §8) rather than inline raw strings: no templating, ever — every dynamic value
//! reaches the shell through an env var. The files carry **no trailing newline**,
//! matching the raw strings they were extracted from; `stub_bodies_and_argv_sha256_frozen`
//! pins that (and every other) byte.
//!
//! The `$NICE_SOCKET` env var the `claude()` function reads, and the per-window
//! `NICE_TAB_ID` / `NICE_PANE_ID` / `NICE_USER_ZDOTDIR` / `NICE_PREFILL_COMMAND`
//! values, are injected separately (R14 slice 3 / slice 4); these stubs only
//! reference them. This module owns the stub text, the writer, the per-variant
//! location, and the zsh [`ShellProfile`] implementation that routes them.

use std::io;
use std::path::{Path, PathBuf};

use super::{
    ComposeSupport, InjectPaths, PrefillStrategy, ShellKind, ShellProfile, SpawnCtx, UserShellEnv,
};

/// Stub `.zshenv`: discover + stash the user's intended `ZDOTDIR`, then restore
/// our temp dir so zsh keeps reading the other stubs.
pub const ZSHENV_BODY: &str = include_str!("scripts/zsh/zshenv.zsh");

/// Stub `.zprofile`: chain back to the user's real `.zprofile` (login shells).
pub const ZPROFILE_BODY: &str = include_str!("scripts/zsh/zprofile.zsh");

/// Stub `.zlogin`: defensive chain-back to the user's real `.zlogin`.
pub const ZLOGIN_BODY: &str = include_str!("scripts/zsh/zlogin.zsh");

/// Stub `.zshrc`: restore `ZDOTDIR` before sourcing the user's `.zshrc`, then
/// install the `claude()` shadow, the OSC 7 cwd emitter, and the prefill tail.
pub const ZSHRC_BODY: &str = include_str!("scripts/zsh/zshrc.zsh");

/// zsh — the profile Nice has always run, byte-frozen by the extraction
/// (design §6.1). `path` is the resolved binary (normally `/bin/zsh`; a
/// homebrew zsh keeps its own path).
pub struct ZshProfile {
    path: String,
}

impl ZshProfile {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

impl ShellProfile for ZshProfile {
    fn kind(&self) -> ShellKind {
        ShellKind::Zsh
    }

    fn program(&self) -> &str {
        &self.path
    }

    /// `[path, "-il"]` / `[path, "-ilc", "exec <cmd>"]`. The clustered spellings
    /// come from [`nice_term_core::build_exec_args`], the one implementation the
    /// term-core exec-args tests already pin.
    ///
    /// `ctx.inject` is deliberately ignored: zsh's argv is identical with and
    /// without Nice's injection — the `ZDOTDIR` env pair decides
    /// ([`Self::inject_env`]), which is exactly why the injected/non-injected
    /// split has to be an explicit axis for the shells where it *is* argv-shaped.
    fn spawn_argv(&self, ctx: &SpawnCtx) -> Vec<String> {
        let mut argv = Vec::with_capacity(3);
        argv.push(self.path.clone());
        argv.extend(nice_term_core::build_exec_args(ctx.command));
        argv
    }

    /// `ZDOTDIR` points zsh at our stub directory; `NICE_USER_ZDOTDIR` carries
    /// Nice's own inherited value across to the `.zshenv` stub. The latter is
    /// **always set, empty string included** — the stub branches on
    /// `[[ -n "$NICE_USER_ZDOTDIR" ]]`, and an absent var and an empty one must
    /// stay indistinguishable there. Load-bearing; preserved verbatim.
    fn inject_env(&self, inject: &InjectPaths, user: &UserShellEnv) -> Vec<(String, String)> {
        vec![
            (
                "ZDOTDIR".to_string(),
                inject.dir.to_string_lossy().into_owned(),
            ),
            (
                "NICE_USER_ZDOTDIR".to_string(),
                user.user_zdotdir.clone().unwrap_or_default(),
            ),
        ]
    }

    fn write_rc_files(&self, dir: &Path) -> io::Result<InjectPaths> {
        let dir = write_stubs(dir)?;
        Ok(InjectPaths { dir, rcfile: None })
    }

    fn compose_support(&self) -> ComposeSupport {
        ComposeSupport::Trigger
    }

    /// The `.zshrc` tail consumes `NICE_PREFILL_COMMAND` via `print -z` — a
    /// FROZEN contract (`zshrc_prefill_command_uses_print_z`).
    fn prefill(&self) -> PrefillStrategy {
        PrefillStrategy::ShellSide
    }

    fn probe_argv(&self, probe_cmd: &str) -> Vec<String> {
        vec![self.path.clone(), "-ilc".to_string(), probe_cmd.to_string()]
    }

    fn comm_name(&self) -> &str {
        "zsh"
    }

    fn display_name(&self) -> &str {
        "zsh"
    }
}

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

/// The fixed, per-variant `ZDOTDIR` location: zsh's own subdirectory of the
/// shared rc root, `<app support root>/<CFBundleName>/shellrc/zsh`. Ports Swift
/// `MainTerminalShellInject.defaultLocation()`; the per-shell split (and the
/// `zdotdir` → `shellrc/zsh` rename it brought) is design §8's, so bash's
/// `nice.bashrc` can land beside these stubs without either shell seeing the
/// other's files. Pure — creates nothing (unlike the Swift
/// `FileManager.url(create:true)`, which is unnecessary here because
/// [`write_stubs`] creates the dir).
///
/// The bootstrap does not call this: it writes into
/// [`crate::shell::rc_dir_for`] the ACTIVE profile, which for a [`ZshProfile`]
/// is exactly this path.
pub fn default_location() -> PathBuf {
    super::shellrc_root().join("zsh")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{COMPOSE_TRIGGER_BINDKEY, COMPOSE_TRIGGER_SEQ};
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

    // ---- byte-freeze proof -------------------------------------------------

    /// SHA-256 of `bytes`, via the system `shasum` — the same tool a reviewer
    /// runs by hand against the on-disk script files, so a mismatch between the
    /// test and a manual spot-check is impossible.
    fn sha256_hex(bytes: &[u8]) -> String {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new("/usr/bin/shasum")
            .args(["-a", "256"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn shasum");
        child
            .stdin
            .take()
            .expect("shasum stdin")
            .write_all(bytes)
            .expect("write body to shasum");
        let out = child.wait_with_output().expect("shasum output");
        assert!(out.status.success(), "shasum exited {:?}", out.status);
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .expect("shasum digest field")
            .to_string()
    }

    /// The byte-freeze proof for the shell-profile extraction: the four rc-stub
    /// bodies and the argv/trigger goldens are pinned by content hash and by
    /// literal value, so moving the bodies into `include_str!` script files (or
    /// re-homing the constants) cannot perturb a single byte unnoticed.
    ///
    /// A failure here means one of two things: the extraction changed the shell
    /// text (fix the text — these bodies are a FROZEN compatibility contract
    /// against helpers already installed on users' disks), or an `include_str!`
    /// points at the wrong file. Re-pin the literals ONLY together with a
    /// deliberate, reviewed change to the frozen contract.
    #[test]
    fn stub_bodies_and_argv_sha256_frozen() {
        for (name, body, want) in [
            (
                "ZSHENV_BODY",
                ZSHENV_BODY,
                "d74a811318154a9083f1e53e224d341bfb4840410e3f1b3e6d7de0769053b0d0",
            ),
            (
                "ZPROFILE_BODY",
                ZPROFILE_BODY,
                "4656c980ad7e520fc8610e22734e179621235eccacf2c90bf3e0ab300ff405e8",
            ),
            (
                "ZLOGIN_BODY",
                ZLOGIN_BODY,
                "ef704c0a3087039ae34c4696c8f1056c3f8039746cf408b85de5b9e340e76570",
            ),
            (
                "ZSHRC_BODY",
                ZSHRC_BODY,
                "482b4f0d30a1d16b1bd8939cc71e1977479e5988a87e404d204e56f23fc9376b",
            ),
        ] {
            assert_eq!(sha256_hex(body.as_bytes()), want, "{name} bytes changed");
            assert!(
                !body.ends_with('\n'),
                "{name} must not end with a newline (the inline raw strings did not)"
            );
        }

        // The spawn argv goldens the zsh profile must keep producing.
        assert_eq!(
            nice_term_core::build_argv(None),
            vec!["/bin/zsh".to_string(), "-il".to_string()]
        );
        assert_eq!(
            nice_term_core::build_argv(Some("x")),
            vec![
                "/bin/zsh".to_string(),
                "-ilc".to_string(),
                "exec x".to_string()
            ]
        );

        // The compose trigger, on both sides of the app↔shell interchange.
        assert_eq!(COMPOSE_TRIGGER_SEQ, b"\x1b[5099~");
        assert_eq!(COMPOSE_TRIGGER_BINDKEY, r"\e[5099~");
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

    /// The ZDOTDIR must be zsh's own subdirectory of the per-variant `shellrc`
    /// root under Application Support, NOT `$TMPDIR` (which macOS sweeps after 3
    /// days), and be stable across calls so the dir is reused rather than
    /// re-namespaced per launch.
    #[test]
    fn default_location_is_under_app_support_not_temp() {
        let dir = default_location();
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("zsh"),
            "ZDOTDIR directory should be the `zsh` leaf of the shellrc root"
        );
        let parent = dir.parent().expect("has parent");
        assert_eq!(
            parent.file_name().and_then(|n| n.to_str()),
            Some("shellrc"),
            "zsh's stubs live under the shared per-variant `shellrc` root"
        );
        assert!(
            !dir.starts_with(std::env::temp_dir()),
            "ZDOTDIR must not live in $TMPDIR (macOS dirhelper sweeps it after 3 days). Got: {dir:?}"
        );
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

    // The `NICE_APPLICATION_SUPPORT_ROOT` override-seam tests moved to
    // `shell/mod.rs` with `application_support_root` itself — the rc root is now
    // shared by every profile, not zsh's alone.

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
    /// the user back to their prompt), so the wrapper must report the window back
    /// to Nice — otherwise its promotion flag stays set and every later `claude`
    /// in the session opens a new session instead of promoting the window.
    #[test]
    fn zshrc_reports_a_returned_attach_back_to_nice() {
        let body = zshrc();
        assert!(
            body.contains(r#""{\"action\":\"claude_exited\",\"paneId\":${pane_id_json}}""#),
            "the notifier must send the claude_exited action with this window's id"
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

    /// Phase R pin: the wrapper's payload interpolates shell locals that the
    /// `local` line declares AND the two `_nice_json_escape` lines assign. A
    /// rename that touches one spelling and not the others silently ships an
    /// empty `tabId`/`paneId`, which reads as "always open a new session".
    #[test]
    fn zshrc_payload_locals_are_declared_and_assigned() {
        let body = zshrc();
        for var in ["cwd_json", "session_id_json", "window_id_json"] {
            assert!(
                body.contains(&format!("{var}=$(_nice_json_escape")),
                "`{var}` is interpolated into the payload but never assigned"
            );
            assert!(
                body.contains(&format!("${{{var}}}")),
                "`{var}` is assigned but never interpolated into the payload"
            );
        }
        assert!(
            body.contains("local cwd_json session_id_json window_id_json"),
            "the payload locals must all be declared `local`"
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
            "restored Claude sessions rely on print -z to pre-type the resume command"
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
            // Non-empty socket + window ids: what a real Nice window injects.
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
        // must be told the window is a prompt again, or its promotion flag stays
        // set and every later `claude` here opens a new session.
        assert_eq!(
            ok.payloads.last().map(String::as_str),
            Some(r#"{"action":"claude_exited","paneId":"t1-claude"}"#),
            "a returned attach must report the window back to Nice. payloads: {:?}",
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
            "the fallback EXECS claude — the pty's own exit reports that window. payloads: {:?}",
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

    // ---- the ShellProfile surface -----------------------------------------

    fn profile() -> ZshProfile {
        ZshProfile::new("/bin/zsh")
    }

    #[test]
    fn zsh_profile_identity() {
        let p = profile();
        assert_eq!(p.kind(), ShellKind::Zsh);
        assert_eq!(p.program(), "/bin/zsh");
        assert_eq!(p.comm_name(), "zsh");
        assert_eq!(p.display_name(), "zsh");
        assert_eq!(p.compose_support(), ComposeSupport::Trigger);
        assert_eq!(p.prefill(), PrefillStrategy::ShellSide);
    }

    /// A homebrew zsh keeps its own path everywhere argv is built — the profile
    /// never rewrites it back to `/bin/zsh`.
    #[test]
    fn zsh_profile_carries_its_resolved_path() {
        let p = ZshProfile::new("/opt/homebrew/bin/zsh");
        assert_eq!(p.program(), "/opt/homebrew/bin/zsh");
        assert_eq!(
            p.spawn_argv(&SpawnCtx {
                inject: None,
                command: None
            })[0],
            "/opt/homebrew/bin/zsh"
        );
        assert_eq!(
            p.probe_argv("command -v -- claude")[0],
            "/opt/homebrew/bin/zsh"
        );
    }

    /// The full `SpawnCtx` grid: zsh's argv depends ONLY on the command axis.
    /// Injection rides `ZDOTDIR` in the env, so the inject axis must be inert —
    /// this is the pin that says so out loud, because for bash it will not be.
    #[test]
    fn zsh_spawn_argv_ignores_the_inject_axis() {
        let paths = InjectPaths {
            dir: PathBuf::from("/tmp/nice-zdotdir"),
            rcfile: None,
        };
        let p = profile();
        for inject in [None, Some(&paths)] {
            assert_eq!(
                p.spawn_argv(&SpawnCtx {
                    inject,
                    command: None
                }),
                vec!["/bin/zsh".to_string(), "-il".to_string()]
            );
            assert_eq!(
                p.spawn_argv(&SpawnCtx {
                    inject,
                    command: Some("claude --resume abc")
                }),
                vec![
                    "/bin/zsh".to_string(),
                    "-ilc".to_string(),
                    "exec claude --resume abc".to_string()
                ]
            );
        }
    }

    /// `NICE_USER_ZDOTDIR` is set even when Nice inherited no `ZDOTDIR` — the
    /// `.zshenv` stub branches on `-n`, so "absent" and "empty" must stay
    /// indistinguishable to it.
    #[test]
    fn zsh_inject_env_always_sets_user_zdotdir() {
        let paths = InjectPaths {
            dir: PathBuf::from("/tmp/nice-zdotdir"),
            rcfile: None,
        };
        let p = profile();
        assert_eq!(
            p.inject_env(
                &paths,
                &UserShellEnv {
                    user_zdotdir: Some("/Users/u/.config/zsh".to_string())
                }
            ),
            vec![
                ("ZDOTDIR".to_string(), "/tmp/nice-zdotdir".to_string()),
                (
                    "NICE_USER_ZDOTDIR".to_string(),
                    "/Users/u/.config/zsh".to_string()
                ),
            ]
        );
        assert_eq!(
            p.inject_env(&paths, &UserShellEnv { user_zdotdir: None }),
            vec![
                ("ZDOTDIR".to_string(), "/tmp/nice-zdotdir".to_string()),
                ("NICE_USER_ZDOTDIR".to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn zsh_probe_argv_is_an_interactive_login_shell() {
        assert_eq!(
            profile().probe_argv("command -v -- claude"),
            vec![
                "/bin/zsh".to_string(),
                "-ilc".to_string(),
                "command -v -- claude".to_string()
            ]
        );
    }

    /// The trait writer is the same overwrite-always self-heal as
    /// [`write_stubs`], and reports the directory back as the `InjectPaths` the
    /// env injection references — with no `rcfile`, because zsh is injected
    /// through the environment, not argv.
    #[test]
    fn zsh_write_rc_files_reports_paths_and_self_heals() {
        let dir = Scratch(unique("nice-profile-rc-test"));
        let paths = profile().write_rc_files(&dir.0).expect("write rc files");
        assert_eq!(
            paths,
            InjectPaths {
                dir: dir.0.clone(),
                rcfile: None
            }
        );
        for name in [".zshenv", ".zprofile", ".zlogin", ".zshrc"] {
            assert!(dir.0.join(name).is_file(), "expected {name}");
        }

        std::fs::remove_file(dir.0.join(".zshrc")).expect("remove a stub");
        profile().write_rc_files(&dir.0).expect("rewrite rc files");
        assert_eq!(
            std::fs::read_to_string(dir.0.join(".zshrc")).expect("read"),
            ZSHRC_BODY,
            "a removed stub must be restored byte-for-byte on the next write"
        );
    }
}
