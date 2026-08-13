//! [`BashProfile`] — bash's shell profile: `--rcfile` injection plus an
//! in-script login emulation (design §6.2).
//!
//! bash has no `ZDOTDIR` analogue, so injection rides **argv**, not the
//! environment: Nice spawns `bash --rcfile <nice.bashrc> -i` and the single
//! generated rc file does the rest. `--rcfile` is read *instead of* `~/.bashrc`
//! for interactive shells — and is **ignored for login shells**, which is why
//! injected panes are spawned NON-login and [`nice.bashrc`](NICE_BASHRC_BODY)
//! opens by emulating bash's documented login sequence (`/etc/profile`, then the
//! first existing of `~/.bash_profile` / `~/.bash_login` / `~/.profile`). Nice's
//! own hooks are defined after that user config, so they win — the same ordering
//! rule the zsh stubs follow.
//!
//! Non-injected spawns (`SpawnCtx.inject == None`: non-deferred Claude windows,
//! hermetic tests) are genuine `-il` login shells with no rc of ours at all,
//! mirroring how the zsh path deliberately omits `ZDOTDIR` there.
//!
//! **Documented limitations** (kept as-is, NOT fixed — same class as zsh's
//! "`exec zsh` drops the injection"):
//!   * Under `--rcfile … -i` the pane is not a login shell: `shopt -q
//!     login_shell` is false, `logout` is unavailable, and `$0` is a dash-less
//!     bash path, so profile code branching on any of those takes its non-login
//!     path.
//!   * `exec bash` inside a pane drops the injection.
//!   * A PATH set only in a never-sourced `~/.bashrc` is invisible — to the pane
//!     *and* to the `claude` discovery probe ([`ShellProfile::probe_argv`]),
//!     which runs the same login emulation. Discovery and panes therefore agree,
//!     and both match what a real login bash would see.
//!
//! **argv\[0\] is the full resolved bash path** in all four spawn shapes
//! (`/bin/bash`, `/opt/homebrew/bin/bash`, …) — login-ness comes from the `-l`
//! flag, never from a leading-dash argv\[0\], matching the zsh profile's
//! convention. macOS sets `p_comm` from the exec path's *basename*, so every
//! shape reports comm `bash` and [`ShellProfile::comm_name`] is unconditionally
//! `"bash"`.
//!
//! **Dialect baseline: bash 3.2** (macOS ships `/bin/bash` 3.2.57 forever).
//! Nothing in the script may use ≥ 4 features. Command Compose needs bash ≥ 4.3
//! (`bind -x` on a multi-character sequence), so [`ShellProfile::compose_support`]
//! is unconditionally [`ComposeSupport::None`] here and the script carries no
//! compose section — a bash pane's [`PaneShell`](super::PaneShell) snapshot keeps
//! the trigger bytes away from a prompt that could not consume them.
//!
//! Prefill is [`PrefillStrategy::AppTyped`]: bash has no `print -z`, so Nice
//! types the deferred-resume line into the pty itself once the pane's first OSC 7
//! reports readiness (design §6.4). The script therefore never references
//! `NICE_PREFILL_COMMAND` — pinned as a negative by the structural tests.
//!
//! Like the zsh stubs, the script body is a static file pulled in with
//! `include_str!` (design §8): no templating, ever — every dynamic value reaches
//! the shell through an env var. Unlike them it is not yet byte-pinned; the
//! contract freezes once compose lands (design §10), so until then the
//! structural positive/negative sets below are the net.

use std::io;
use std::path::Path;

use super::{
    ComposeSupport, InjectPaths, PrefillStrategy, ShellKind, ShellProfile, SpawnCtx, UserShellEnv,
};

/// The generated `nice.bashrc`: login emulation, the `claude()` shadow, and the
/// `PROMPT_COMMAND` OSC 7 emitter. Written into the profile's rc directory by
/// [`ShellProfile::write_rc_files`] and referenced by `--rcfile`.
pub const NICE_BASHRC_BODY: &str = include_str!("scripts/bash/nice.bashrc");

/// The rc file's name inside the profile's rc directory.
const RC_FILE_NAME: &str = "nice.bashrc";

/// bash — the `--rcfile`-injected profile (design §6.2). `path` is the resolved
/// binary, kept as resolved: a homebrew `/opt/homebrew/bin/bash` gets the bash
/// profile *at that path*, never rewritten to `/bin/bash`.
pub struct BashProfile {
    path: String,
}

impl BashProfile {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

impl ShellProfile for BashProfile {
    fn kind(&self) -> ShellKind {
        ShellKind::Bash
    }

    fn program(&self) -> &str {
        &self.path
    }

    /// The four shapes (design §6.2):
    ///
    /// | `inject` | `command` | argv |
    /// |---|---|---|
    /// | `Some` | `None` | `[path, "--rcfile", <rc>, "-i"]` |
    /// | `Some` | `Some(cmd)` | `[path, "--rcfile", <rc>, "-i", "-c", "exec <cmd>"]` |
    /// | `None` | `None` | `[path, "-il"]` |
    /// | `None` | `Some(cmd)` | `[path, "-il", "-c", "exec <cmd>"]` |
    ///
    /// Injected spawns are deliberately NON-login (`-i`, no `-l`): bash ignores
    /// `--rcfile` for login shells, so the rc file emulates the login chain
    /// itself. `-i` is what makes bash source the rcfile even under `-c`.
    ///
    /// The `exec <cmd>` wrapping keeps the command-owns-the-pty contract, and the
    /// command string is spliced verbatim (no tilde expansion), matching the zsh
    /// shapes.
    ///
    /// # Panics
    ///
    /// If `ctx.inject` is `Some` but carries no `rcfile`. Unreachable by
    /// construction: bash's [`Self::write_rc_files`] always returns `Some`, and a
    /// failed rc write leaves `ShellRuntime.inject` as `None`, which takes the
    /// non-injected rows above.
    fn spawn_argv(&self, ctx: &SpawnCtx) -> Vec<String> {
        let mut argv = vec![self.path.clone()];
        match ctx.inject {
            Some(paths) => {
                let rcfile = paths
                    .rcfile
                    .as_ref()
                    .expect("bash injection needs an rcfile; write_rc_files always supplies one");
                argv.push("--rcfile".to_string());
                argv.push(rcfile.to_string_lossy().into_owned());
                argv.push("-i".to_string());
            }
            None => argv.push("-il".to_string()),
        }
        if let Some(cmd) = ctx.command {
            argv.push("-c".to_string());
            argv.push(format!("exec {cmd}"));
        }
        argv
    }

    /// Empty — bash injection rides argv ([`Self::spawn_argv`]), and there is no
    /// `ZDOTDIR` analogue to carry across. The generic pairs (`NICE_SOCKET` /
    /// `NICE_TAB_ID` / `NICE_PANE_ID` / `NICE_COMPOSE_CONF`) stay shell-agnostic
    /// in `pty_manager`; bash panes receive them unchanged.
    fn inject_env(&self, _inject: &InjectPaths, _user: &UserShellEnv) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Write `nice.bashrc` into `dir` (creating it and any missing parents),
    /// returning the paths argv references. Same write contract as the zsh
    /// writer: overwrite-always so the directory self-heals if the file was ever
    /// removed, and atomic (temp sibling + rename) so a pty child mid-`source`
    /// never reads a half-written rc when a second window rewrites the shared
    /// directory.
    fn write_rc_files(&self, dir: &Path) -> io::Result<InjectPaths> {
        std::fs::create_dir_all(dir)?;
        let rcfile = dir.join(RC_FILE_NAME);
        crate::atomic_file::write_atomic(&rcfile, NICE_BASHRC_BODY.as_bytes(), None)?;
        Ok(InjectPaths {
            dir: dir.to_path_buf(),
            rcfile: Some(rcfile),
        })
    }

    /// Compose needs `bind -x` on a multi-character sequence — bash ≥ 4.3, which
    /// the 3.2 baseline is not. Unconditionally off in this slice; the version
    /// probe that lifts it for a homebrew bash 5 lands with the compose section.
    fn compose_support(&self) -> ComposeSupport {
        ComposeSupport::None
    }

    /// bash has no editor-buffer push (`print -z`), so Nice types the line
    /// itself on the pane's first OSC 7 (design §6.4).
    fn prefill(&self) -> PrefillStrategy {
        PrefillStrategy::AppTyped
    }

    /// `[path, "-ilc", probe_cmd]` — a login-interactive bash reads
    /// `/etc/profile` and the user's profile chain, which is where a bash user's
    /// PATH lives (directly, or via a profile-sourced `~/.bashrc`). Clustered
    /// spelling per design §6.5; bash accepts it.
    fn probe_argv(&self, probe_cmd: &str) -> Vec<String> {
        vec![self.path.clone(), "-ilc".to_string(), probe_cmd.to_string()]
    }

    /// Unconditionally `"bash"`: macOS sets `p_comm` from the exec path's
    /// basename (MAXCOMLEN-truncated), not argv\[0\], so `/bin/bash` and
    /// `/opt/homebrew/bin/bash` report the same comm under every spawn shape —
    /// and Nice never prefixes a dash. Agrees with the
    /// [`all_known_comm_names`](super::all_known_comm_names) registry entry.
    fn comm_name(&self) -> &str {
        "bash"
    }

    fn display_name(&self) -> &str {
        "bash"
    }
}

/// Hermetic-bash test plumbing (design §10), shared by this module's real-bash
/// e2e suite and by the app-typed-prefill integration test that drives a real
/// bash pane through `PtyManager`.
///
/// The zsh suite gets hermeticism by blanking `ZDOTDIR`. That trick is
/// zsh-shaped: bash reads `$HOME`-relative paths, which no env var can redirect
/// away. The two sanctioned replacements, both used below:
///
///   * **A scratch `$HOME`** ([`ScratchHome`]). Spawn-side env pairs override the
///     inherited `HOME`, so [`nice.bashrc`](NICE_BASHRC_BODY)'s login emulation
///     sources fixture files and nothing of the developer's.
///   * **`--norc --noprofile` argv** ([`quiet_argv`]) for spawns that want no user
///     config *and* no `nice.bashrc`. Test-only — never a production `SpawnCtx`
///     axis.
///
/// **`/etc/profile` still runs** under a scratch `$HOME` (absolute path), and on
/// macOS it `eval`s `path_helper`, which REBUILDS `PATH` from `/etc/paths` and
/// pushes whatever was inherited to the back. A fixture `bin/` handed over only
/// through the environment therefore loses to `/usr/bin` — where a real `nc`
/// lives — so any fixture that shadows a real binary must be re-prepended from
/// the scratch profile: see [`ScratchHome::write_path_restoring_bash_profile`],
/// which doubles as proof that the login chain ran at all.
#[cfg(test)]
pub(crate) mod hermetic {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// macOS's guaranteed bash — 3.2.57, the dialect baseline every script test
    /// targets. Present on every machine, so the e2e tier is unconditional.
    pub(crate) const SYSTEM_BASH: &str = "/bin/bash";

    /// A throwaway directory removed on drop (the zsh module's twin). A
    /// panicking assertion leaves it behind, which is harmless.
    pub(crate) struct Scratch(pub(crate) PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn unique(prefix: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()))
    }

    /// A fresh empty scratch directory.
    pub(crate) fn scratch(prefix: &str) -> Scratch {
        let dir = unique(prefix);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// A throwaway `$HOME` with a `bin/` for fake binaries, plus the env pairs a
    /// bash spawn needs to see it and nothing else.
    pub(crate) struct ScratchHome {
        dir: Scratch,
    }

    impl ScratchHome {
        pub(crate) fn new(prefix: &str) -> Self {
            let dir = scratch(prefix);
            std::fs::create_dir_all(dir.0.join("bin")).expect("create scratch bin");
            Self { dir }
        }

        pub(crate) fn path(&self) -> &Path {
            &self.dir.0
        }

        /// Where fake binaries live — first on the [`Self::env`] `PATH`, and put
        /// back there after `path_helper` by
        /// [`Self::write_path_restoring_bash_profile`].
        pub(crate) fn bin(&self) -> PathBuf {
            self.dir.0.join("bin")
        }

        /// Write a file under the scratch home (dotfiles included), creating
        /// parent directories.
        pub(crate) fn write_file(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.dir.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture parent");
            }
            std::fs::write(&path, body).expect("write fixture file");
            path
        }

        /// Install an executable fixture into [`Self::bin`].
        pub(crate) fn install_executable(&self, name: &str, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.bin().join(name);
            std::fs::write(&path, body).expect("write fixture executable");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fixture executable");
            path
        }

        /// Write a `.bash_profile` that re-prepends [`Self::bin`] to `PATH`
        /// (before any `extra` lines).
        ///
        /// Mandatory for every fixture that shadows a real binary: the login
        /// emulation sources `/etc/profile` first, whose `path_helper` rebuilds
        /// `PATH` from `/etc/paths` and demotes the inherited fixture bin behind
        /// `/usr/bin`. Restoring it here also proves the profile chain ran — a
        /// fixture `nc` that answers the handshake could not have been found
        /// otherwise.
        pub(crate) fn write_path_restoring_bash_profile(&self, extra: &str) -> PathBuf {
            self.write_file(
                ".bash_profile",
                &format!("export PATH=\"{}:$PATH\"\n{extra}", self.bin().display()),
            )
        }

        /// The env a hermetic bash spawn needs on top of a cleared environment:
        /// the scratch `$HOME`, a `PATH` led by the fixture bin, and the pair of
        /// terminal vars Nice's own spawn env sets. Deliberately no `HOSTNAME` —
        /// bash sets it itself, which is why the OSC 7 emitter can use it.
        pub(crate) fn env(&self) -> Vec<(String, String)> {
            vec![
                ("HOME".to_string(), self.dir.0.display().to_string()),
                (
                    "PATH".to_string(),
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.bin().display()),
                ),
                ("TERM".to_string(), "xterm-256color".to_string()),
                ("LANG".to_string(), "en_US.UTF-8".to_string()),
            ]
        }

        /// A `Command` for `program` with the environment fully replaced by
        /// [`Self::env`] (`env_clear`, mirroring Nice's own spawn) and the cwd at
        /// the scratch home. Stdin is `/dev/null` so an interactive `-i` child
        /// never blocks on the test runner's terminal.
        pub(crate) fn command(&self, program: &str) -> Command {
            let mut cmd = Command::new(program);
            cmd.env_clear().current_dir(&self.dir.0).stdin(Stdio::null());
            for (k, v) in self.env() {
                cmd.env(k, v);
            }
            cmd
        }
    }

    /// Argv for a fully-quiet bash: no user config, and no `nice.bashrc` either.
    /// The bash-shaped analogue of the zsh suite's empty `ZDOTDIR`, for tests
    /// that want a shell with nothing injected at all.
    pub(crate) fn quiet_argv(bash: &str) -> Vec<String> {
        vec![
            bash.to_string(),
            "--norc".to_string(),
            "--noprofile".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::hermetic::{quiet_argv, scratch, unique, Scratch, ScratchHome, SYSTEM_BASH};
    use super::*;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    fn profile() -> BashProfile {
        BashProfile::new(SYSTEM_BASH)
    }

    fn inject_paths(dir: &str) -> InjectPaths {
        InjectPaths {
            dir: PathBuf::from(dir),
            rcfile: Some(PathBuf::from(dir).join(RC_FILE_NAME)),
        }
    }

    /// The script with every full-line comment removed. Negative assertions run
    /// against THIS, never the raw body: the comments deliberately name the
    /// zsh spellings they are warning against (`\%`, `exec command`, `~/.bashrc`),
    /// and a comment must never trip a "the code must not contain X" check.
    fn code_only() -> String {
        NICE_BASHRC_BODY
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Byte offset of `needle` in the script, or a panic naming it.
    fn at(needle: &str) -> usize {
        NICE_BASHRC_BODY
            .find(needle)
            .unwrap_or_else(|| panic!("nice.bashrc must contain `{needle}`"))
    }

    // ---- the ShellProfile surface ------------------------------------------

    #[test]
    fn bash_profile_identity() {
        let p = profile();
        assert_eq!(p.kind(), ShellKind::Bash);
        assert_eq!(p.program(), "/bin/bash");
        assert_eq!(p.comm_name(), "bash");
        assert_eq!(p.display_name(), "bash");
        // Compose is off until the >= 4.3 probe lands; prefill is app-typed
        // because bash has no `print -z`.
        assert_eq!(p.compose_support(), ComposeSupport::None);
        assert_eq!(p.prefill(), PrefillStrategy::AppTyped);
    }

    /// The full `SpawnCtx` grid. Unlike zsh — whose injection rides `ZDOTDIR`,
    /// leaving argv identical on both axes — bash's inject axis is argv-shaped,
    /// so all four cells differ.
    #[test]
    fn bash_spawn_argv_covers_the_full_ctx_grid() {
        let p = profile();
        let paths = inject_paths("/managed/rc/bash");
        let rc = "/managed/rc/bash/nice.bashrc";

        assert_eq!(
            p.spawn_argv(&SpawnCtx {
                inject: Some(&paths),
                command: None
            }),
            vec!["/bin/bash", "--rcfile", rc, "-i"],
            "injected shell pane: NON-login so bash honors --rcfile"
        );
        assert_eq!(
            p.spawn_argv(&SpawnCtx {
                inject: Some(&paths),
                command: Some("claude --resume abc")
            }),
            vec![
                "/bin/bash",
                "--rcfile",
                rc,
                "-i",
                "-c",
                "exec claude --resume abc"
            ],
            "injected command pane: -i still sources the rcfile under -c"
        );
        assert_eq!(
            p.spawn_argv(&SpawnCtx {
                inject: None,
                command: None
            }),
            vec!["/bin/bash", "-il"],
            "non-injected: a genuine login shell, no rc of ours"
        );
        assert_eq!(
            p.spawn_argv(&SpawnCtx {
                inject: None,
                command: Some("vim")
            }),
            vec!["/bin/bash", "-il", "-c", "exec vim"]
        );
    }

    /// The command string is spliced verbatim after `exec` — no quoting, no
    /// tilde expansion — exactly like the zsh shapes.
    #[test]
    fn bash_spawn_argv_splices_the_command_verbatim() {
        let paths = inject_paths("/managed/rc/bash");
        let command = r#"claude --settings '~/a b/ptr.json' --resume xyz"#;
        let argv = profile().spawn_argv(&SpawnCtx {
            inject: Some(&paths),
            command: Some(command),
        });
        assert_eq!(argv.last().unwrap(), &format!("exec {command}"));
    }

    /// A homebrew bash keeps its own path everywhere argv is built — in argv[0]
    /// of every spawn shape and of the probe.
    #[test]
    fn bash_profile_carries_its_resolved_path() {
        let p = BashProfile::new("/opt/homebrew/bin/bash");
        let paths = inject_paths("/managed/rc/bash");
        assert_eq!(p.program(), "/opt/homebrew/bin/bash");
        for inject in [None, Some(&paths)] {
            for command in [None, Some("vim")] {
                assert_eq!(
                    p.spawn_argv(&SpawnCtx { inject, command })[0],
                    "/opt/homebrew/bin/bash"
                );
            }
        }
        assert_eq!(
            p.probe_argv("command -v -- claude"),
            vec![
                "/opt/homebrew/bin/bash".to_string(),
                "-ilc".to_string(),
                "command -v -- claude".to_string()
            ]
        );
        // comm is the exec path's basename, so it is `bash` at any path.
        assert_eq!(p.comm_name(), "bash");
    }

    #[test]
    fn bash_probe_argv_is_an_interactive_login_shell() {
        assert_eq!(
            profile().probe_argv("command -v -- claude"),
            vec![
                "/bin/bash".to_string(),
                "-ilc".to_string(),
                "command -v -- claude".to_string()
            ]
        );
    }

    /// Nothing bash-specific rides the environment — not even when Nice has a
    /// `ZDOTDIR` of its own to pass along. A stray `NICE_USER_ZDOTDIR` in a bash
    /// pane is exactly the cross-shell leakage the abstraction prevents.
    #[test]
    fn bash_inject_env_is_always_empty() {
        let paths = inject_paths("/managed/rc/bash");
        assert_eq!(
            profile().inject_env(
                &paths,
                &UserShellEnv {
                    user_zdotdir: Some("/user/z".to_string())
                }
            ),
            Vec::<(String, String)>::new()
        );
        assert_eq!(
            profile().inject_env(&paths, &UserShellEnv { user_zdotdir: None }),
            Vec::<(String, String)>::new()
        );
    }

    // ---- the rc writer -----------------------------------------------------

    /// One file, reported as the `rcfile` argv points at — and rewritten from
    /// the frozen const on every call, so a removed rc self-heals.
    #[test]
    fn bash_write_rc_files_reports_paths_and_self_heals() {
        let dir = Scratch(unique("nice-bash-rc-test"));
        assert!(!dir.0.exists(), "the writer must create its own directory");

        let paths = profile().write_rc_files(&dir.0).expect("write rc files");
        assert_eq!(
            paths,
            InjectPaths {
                dir: dir.0.clone(),
                rcfile: Some(dir.0.join(RC_FILE_NAME)),
            }
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join(RC_FILE_NAME)).expect("read"),
            NICE_BASHRC_BODY,
            "the writer must round-trip the script byte-for-byte"
        );

        // The file set is `nice.bashrc` alone — no zsh-style four-file dance,
        // and no temp sibling left behind by the atomic write.
        let entries: Vec<String> = std::fs::read_dir(&dir.0)
            .expect("read dir")
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![RC_FILE_NAME.to_string()]);

        std::fs::remove_file(dir.0.join(RC_FILE_NAME)).expect("remove the rc");
        profile().write_rc_files(&dir.0).expect("rewrite rc files");
        assert_eq!(
            std::fs::read_to_string(dir.0.join(RC_FILE_NAME)).expect("read"),
            NICE_BASHRC_BODY,
            "a removed rc must be restored byte-for-byte on the next write"
        );
    }

    // ---- script structural tests (the pre-byte-pin net, design §10) --------

    /// The real bash 3.2 syntax gate, run against the file the writer actually
    /// produces. `/bin/bash` 3.2 ships on every macOS, so this is unconditional —
    /// a skip here would be a failure, not an environment gap.
    #[test]
    fn nice_bashrc_passes_real_bin_bash_syntax_check() {
        let dir = Scratch(unique("nice-bash-syntax"));
        let paths = profile().write_rc_files(&dir.0).expect("write rc files");
        let rcfile = paths.rcfile.expect("bash always reports an rcfile");
        let out = Command::new("/bin/bash")
            .arg("-n")
            .arg(&rcfile)
            .output()
            .expect("spawn /bin/bash -n");
        assert!(
            out.status.success(),
            "`/bin/bash -n {}` failed: {}",
            rcfile.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The login emulation runs `/etc/profile` first, then the first existing of
    /// `.bash_profile` → `.bash_login` → `.profile`, in bash's documented order.
    #[test]
    fn nice_bashrc_sources_the_login_chain_in_documented_order() {
        let etc = at(". /etc/profile");
        let bash_profile = at(r#". "$HOME/.bash_profile""#);
        let bash_login = at(r#". "$HOME/.bash_login""#);
        let profile_file = at(r#". "$HOME/.profile""#);
        assert!(
            etc < bash_profile && bash_profile < bash_login && bash_login < profile_file,
            "login chain out of order: /etc/profile, .bash_profile, .bash_login, .profile"
        );
        // First-match, not all-three: the second and third are `elif` arms.
        let code = code_only();
        assert!(
            code.contains(r#"elif [ -f "$HOME/.bash_login" ]; then"#)
                && code.contains(r#"elif [ -f "$HOME/.profile" ]; then"#),
            "the profile chain must be a first-match if/elif, not three ifs"
        );
    }

    /// Nice's hooks are defined AFTER the login block, so they win over anything
    /// the user's own config defined — the same ordering rule as the zsh stubs.
    #[test]
    fn nice_bashrc_defines_hooks_after_the_login_block() {
        let login_end = at(r#". "$HOME/.profile""#);
        for name in [
            "_nice_json_escape() {",
            "_nice_claude_exited() {",
            "claude() {",
            "_nice_emit_cwd_osc7() {",
            "_nice_osc7_prompt_command() {",
        ] {
            assert!(
                at(name) > login_end,
                "`{name}` must be defined after the login emulation"
            );
        }
    }

    /// The handshake payload and its dispatch, byte-shape-identical to the zsh
    /// stub's (the socket server is dialect-agnostic).
    #[test]
    fn nice_bashrc_handshake_payload_and_reply_modes() {
        let body = NICE_BASHRC_BODY;
        assert!(body.contains(r#"\"action\":\"claude\""#));
        for field in ["cwd", "args", "tabId", "paneId"] {
            assert!(body.contains(field), "payload must include {field}");
        }
        assert!(
            body.contains(r#"nc -U "$NICE_SOCKET""#),
            "must speak AF_UNIX to Nice's control socket via nc -U"
        );
        for mode in ["newtab)", "inplace)", "attach)", "resume)"] {
            assert!(body.contains(mode), "wrapper must handle the `{mode}` reply");
        }
        assert!(
            body.contains(r#""{\"action\":\"claude_exited\",\"paneId\":${pane_id_json}}""#),
            "a returned attach must report the pane back to Nice"
        );
        assert!(
            body.contains("control socket unreachable"),
            "must warn when the socket is gone"
        );
    }

    /// An unset `NICE_SOCKET` bypasses the wrapper entirely — a bash pane
    /// outside Nice runs the real binary.
    #[test]
    fn nice_bashrc_no_handshake_when_socket_unset() {
        assert!(
            code_only().contains(r#"if [[ -z "$NICE_SOCKET" ]]; then"#),
            "missing NICE_SOCKET must bypass the wrapper"
        );
    }

    /// The passthrough set matches zsh's, verbatim.
    #[test]
    fn nice_bashrc_passthrough_flags_and_subcommands() {
        let body = NICE_BASHRC_BODY;
        for flag in ["-p", "--print", "-h", "--help", "--version", "--output-format"] {
            assert!(body.contains(flag), "flag {flag} must short-circuit");
        }
        for sub in ["mcp", "config", "migrate-installer", "update", "doctor"] {
            assert!(body.contains(sub), "subcommand {sub} must short-circuit");
        }
    }

    /// `${sid:0:8}` — bash substring. zsh's `${sid[1,8]}` expands to the WHOLE
    /// string in bash, which was inventory finding 3's concrete bug.
    #[test]
    fn nice_bashrc_attach_uses_bash_substring_not_zsh_subscript() {
        let code = code_only();
        assert!(
            code.contains(r#"command claude attach "${sid:0:8}""#),
            "attach must prefix-match with the bash substring form"
        );
        assert!(
            !code.contains("${sid[1,8]}"),
            "the zsh subscript form expands to the whole string in bash"
        );
    }

    /// `${#arr[@]}` is the ARRAY length; `${#arr}` (the zsh spelling) is the
    /// length of element 0. And `local -a` is split from the `=()` assignment,
    /// which bash 3.2 handles unambiguously only in the two-line form.
    #[test]
    fn nice_bashrc_uses_bash_array_length_and_split_declaration() {
        let code = code_only();
        assert!(
            code.contains("if (( ${#pre[@]} )); then"),
            "array emptiness must be tested with ${{#pre[@]}}"
        );
        assert!(
            !code.contains("${#pre}"),
            "${{#pre}} is the length of element 0 in bash"
        );
        for array in ["pre", "post"] {
            assert!(
                code.contains(&format!("local -a {array}\n")),
                "`{array}` must be declared with a bare `local -a`"
            );
            assert!(
                !code.contains(&format!("local -a {array}=(")),
                "bash 3.2's `local name=(...)` initialization is quirky — \
                 declare `{array}` and assign it on separate lines"
            );
        }
    }

    /// The OSC 7 emitter: bare-`%` substitution (the zsh `\%` escape is zsh-only
    /// arcana), octal `\033`/`\007`, and `$HOSTNAME` (bash's spelling of zsh's
    /// `$HOST`).
    #[test]
    fn nice_bashrc_osc7_emitter_encoding_and_spelling() {
        let assign = NICE_BASHRC_BODY
            .lines()
            .find(|l| l.contains("local p=") && l.contains("PWD"))
            .unwrap_or("");
        assert!(
            assign.contains(r#"${PWD//%/%25}"#),
            "bare `%` is the correct bash spelling. Got: <{assign}>"
        );
        assert!(
            !assign.contains(r#"${PWD//\%/%25}"#),
            "the zsh `\\%` escape does not carry over. Got: <{assign}>"
        );
        assert!(
            code_only().contains("p=${p// /%20}"),
            "spaces must percent-encode after the `%` pass"
        );
        // The chosen spelling of both bytes is octal — POSIX-portable and
        // consistent. (bash 3.2's printf accepts `\e` too; this is style, not a
        // 3.2 workaround, so nothing here asserts `\e` is unsupported.)
        assert!(
            code_only().contains(r#"printf '\033]7;file://%s%s\007' "${HOSTNAME}" "$p""#),
            "emitter must produce an octal-spelled OSC 7 file:// URL for $HOSTNAME"
        );
    }

    /// bash has no `chpwd` hook, so the emitter rides `PROMPT_COMMAND` with a
    /// `$PWD` change-dedup — appended cooperatively in BOTH forms (bash ≥ 5.1
    /// may hold it as an array; 3.2 is always a string).
    #[test]
    fn nice_bashrc_prompt_command_dedups_and_appends_cooperatively() {
        let code = code_only();
        assert!(
            code.contains(r#"if [[ "$PWD" != "$_nice_last_osc7_pwd" ]]; then"#),
            "PROMPT_COMMAND fires every prompt — the wrapper must dedup on $PWD"
        );
        assert!(
            code.contains(r#"case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in"#),
            "the array/string form must be sniffed with declare -p"
        );
        assert!(
            code.contains("PROMPT_COMMAND+=(_nice_osc7_prompt_command)"),
            "the >= 5.1 array arm must append as an array element"
        );
        assert!(
            code.contains(
                r#"PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }_nice_osc7_prompt_command""#
            ),
            "the string arm must preserve whatever the user's profile registered"
        );
    }

    /// The unconditional startup fire is the LAST statement of the file: it is
    /// the app-typed-prefill readiness signal, and the compose section lands
    /// above it.
    #[test]
    fn nice_bashrc_startup_osc7_fire_is_the_final_statement() {
        let statements: Vec<&str> = NICE_BASHRC_BODY
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(
            statements.last().copied(),
            Some("_nice_emit_cwd_osc7"),
            "the startup OSC 7 fire must be the file's final statement"
        );
        assert_eq!(
            statements[statements.len() - 2],
            "_nice_last_osc7_pwd=$PWD",
            "the startup fire must seed the dedup so the first prompt does not repeat it"
        );
    }

    /// The dialect table as a pinned NEGATIVE set: zsh spellings that would be
    /// wrong or inert in bash, plus the two features this slice deliberately
    /// leaves out (prefill env var, compose).
    #[test]
    fn nice_bashrc_carries_no_zsh_isms_prefill_or_compose() {
        let code = code_only();
        for (needle, why) in [
            ("print -z", "bash has no editor-buffer push; prefill is app-typed"),
            ("print -u2", "bash has no `print` builtin; errors go to `printf … >&2`"),
            (
                "NICE_PREFILL_COMMAND",
                "the app types the prefill; the rc must never reference the var",
            ),
            ("emulate", "`emulate -L zsh` has no bash equivalent and no purpose here"),
            ("chpwd_functions", "bash has no chpwd hook; the emitter rides PROMPT_COMMAND"),
            (
                "exec command",
                "bash's exec never resolves functions, and `command` is not a \
                 precommand modifier after it — `exec command claude` would exec \
                 the /usr/bin/command shim",
            ),
            (
                "$HOME/.bashrc",
                "the user's profile sources it; doing it again double-sources",
            ),
            (".bashrc", "no spelling of the user's ~/.bashrc may be sourced here"),
            ("bind -x", "compose is not in this slice"),
            ("READLINE_LINE", "compose is not in this slice"),
            ("5099", "no compose trigger may be bound by a bash pane"),
        ] {
            assert!(
                !code.contains(needle),
                "nice.bashrc must not contain `{needle}` — {why}"
            );
        }
        // The prefill var is absent from the COMMENTS too: nothing in this file
        // may suggest a shell-side prefill contract exists for bash.
        assert!(
            !NICE_BASHRC_BODY.contains("NICE_PREFILL_COMMAND"),
            "not even a comment may reference NICE_PREFILL_COMMAND"
        );
    }

    // ======================================================================
    // Real-bash end-to-end (design §10)
    //
    // Unconditional: `/bin/bash` 3.2.57 ships on every macOS, so a skip here
    // would be a failure, not an environment gap. Every leg runs the script the
    // writer actually produces, under a scratch `$HOME` (see [`hermetic`]).
    // ======================================================================

    /// Write `nice.bashrc` into a `shellrc` dir under the scratch home — the
    /// same writer production uses — and return the path argv points at.
    fn nice_bashrc_in(home: &ScratchHome) -> PathBuf {
        profile()
            .write_rc_files(&home.path().join("shellrc"))
            .expect("write nice.bashrc")
            .rcfile
            .expect("bash always reports an rcfile")
    }

    /// The production injected argv for `rc`, straight out of
    /// [`BashProfile::spawn_argv`] — so the e2e legs exercise the real shape,
    /// `exec` wrapper and all.
    fn injected_argv(rc: &Path, command: Option<&str>) -> Vec<String> {
        let paths = InjectPaths {
            dir: rc.parent().expect("rc has a parent").to_path_buf(),
            rcfile: Some(rc.to_path_buf()),
        };
        profile().spawn_argv(&SpawnCtx {
            inject: Some(&paths),
            command,
        })
    }

    /// `bash --rcfile <rc> -i -c <script>` WITHOUT the production `exec`
    /// wrapper, for legs whose script is several statements (`exec` would try to
    /// PATH-search the first word). The rc-sourcing behavior under test is
    /// identical — `-i` is what makes bash read the rcfile under `-c`.
    fn unwrapped_argv(rc: &Path, script: &str) -> Vec<String> {
        vec![
            SYSTEM_BASH.to_string(),
            "--rcfile".to_string(),
            rc.to_string_lossy().into_owned(),
            "-i".to_string(),
            "-c".to_string(),
            script.to_string(),
        ]
    }

    fn run_bash(home: &ScratchHome, argv: &[String], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = home.command(&argv[0]);
        cmd.args(&argv[1..]);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn /bin/bash")
    }

    fn stdout_of(out: &Output) -> String {
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.len() > haystack.len() {
            return None;
        }
        (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
    }

    /// Every OSC 7 payload in `bytes`, in order — the bytes between `ESC ] 7 ;`
    /// and its BEL terminator.
    fn osc7_payloads(bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while let Some(start) = find_subsequence(rest, &[0x1b, 0x5d, 0x37, 0x3b]) {
            let payload = &rest[start + 4..];
            let bel = payload
                .iter()
                .position(|&b| b == 0x07)
                .expect("OSC 7 emission missing BEL terminator");
            out.push(String::from_utf8_lossy(&payload[..bel]).into_owned());
            rest = &payload[bel + 1..];
        }
        out
    }

    // ---- the `-i -c` rcfile-sourcing pin -----------------------------------

    /// THE bash behavior the whole injected-command-pane shape rests on: under
    /// `-i`, bash sources `--rcfile` even when `-c` supplies a command — and
    /// sources it BEFORE running that command. If this ever stopped holding,
    /// command panes would silently lose the `claude()` shadow and OSC 7 while
    /// still looking fine.
    ///
    /// Deliberately driven with a marker fixture rc rather than `nice.bashrc`:
    /// the assertion is about bash, not about our script.
    #[test]
    fn bash_i_c_sources_the_rcfile_before_the_command() {
        let home = ScratchHome::new("nice-bash-rcfile-pin");
        let rc = home.write_file("fixture.bashrc", "echo NICE-RCFILE-SOURCED\n");

        let argv = injected_argv(&rc, Some("echo NICE-COMMAND-RAN"));
        assert_eq!(
            argv.last().map(String::as_str),
            Some("exec echo NICE-COMMAND-RAN"),
            "the leg must run the production argv shape"
        );
        let out = stdout_of(&run_bash(&home, &argv, &[]));

        let sourced = out
            .find("NICE-RCFILE-SOURCED")
            .unwrap_or_else(|| panic!("`bash --rcfile … -i -c` did not source the rc. Got: <{out}>"));
        let ran = out
            .find("NICE-COMMAND-RAN")
            .unwrap_or_else(|| panic!("the -c command did not run. Got: <{out}>"));
        assert!(
            sourced < ran,
            "the rcfile must be sourced BEFORE the -c command. Got: <{out}>"
        );
    }

    /// The hermetic-quiet argv reads neither the user's config nor ours — the
    /// bash analogue of the zsh suite's empty `ZDOTDIR`.
    #[test]
    fn quiet_argv_reads_no_startup_files() {
        let home = ScratchHome::new("nice-bash-quiet");
        home.write_file(".bash_profile", "echo NICE-PROFILE-RAN\n");
        home.write_file(".bashrc", "echo NICE-BASHRC-RAN\n");

        let mut argv = quiet_argv(SYSTEM_BASH);
        argv.push("-ic".to_string());
        argv.push("echo NICE-QUIET".to_string());
        let out = stdout_of(&run_bash(&home, &argv, &[]));

        assert!(out.contains("NICE-QUIET"), "the command must run. Got: <{out}>");
        assert!(
            !out.contains("NICE-PROFILE-RAN") && !out.contains("NICE-BASHRC-RAN"),
            "--norc --noprofile must read no startup files. Got: <{out}>"
        );
    }

    // ---- login emulation ----------------------------------------------------

    /// Run the real `nice.bashrc` over a scratch `$HOME` seeded with `files`,
    /// returning stdout. The pane shape is the production one (`exec true` after
    /// the rc), so what we observe is exactly what a real command pane sources.
    fn run_login_emulation(prefix: &str, files: &[(&str, &str)]) -> String {
        let home = ScratchHome::new(prefix);
        for (name, body) in files {
            home.write_file(name, body);
        }
        let rc = nice_bashrc_in(&home);
        stdout_of(&run_bash(&home, &injected_argv(&rc, Some("true")), &[]))
    }

    fn marker_count(haystack: &str, marker: &str) -> usize {
        haystack.matches(marker).count()
    }

    /// bash's documented login sequence is a FIRST-MATCH chain, and the rc
    /// reproduces it: `.bash_profile` wins outright, then `.bash_login`, then
    /// `.profile`. Sourcing more than one is the classic re-implementation bug
    /// (a user's `.profile` re-running under an already-loaded `.bash_profile`).
    #[test]
    fn login_emulation_takes_the_first_matching_profile_file() {
        let all = [
            (".bash_profile", "echo NICE-BASH-PROFILE\n"),
            (".bash_login", "echo NICE-BASH-LOGIN\n"),
            (".profile", "echo NICE-PROFILE\n"),
        ];

        let out = run_login_emulation("nice-bash-login-all", &all);
        assert!(
            out.contains("NICE-BASH-PROFILE")
                && !out.contains("NICE-BASH-LOGIN")
                && !out.contains("NICE-PROFILE"),
            ".bash_profile must win outright when all three exist. Got: <{out}>"
        );

        let out = run_login_emulation("nice-bash-login-nobp", &all[1..]);
        assert!(
            out.contains("NICE-BASH-LOGIN") && !out.contains("NICE-PROFILE"),
            "without .bash_profile, .bash_login is next. Got: <{out}>"
        );

        let out = run_login_emulation("nice-bash-login-profile", &all[2..]);
        assert!(
            out.contains("NICE-PROFILE"),
            "with only .profile, .profile runs. Got: <{out}>"
        );
    }

    /// The convention-honored leg: a `.bash_profile` that sources `~/.bashrc`
    /// gets BOTH — exactly once each. The rc deliberately never sources
    /// `~/.bashrc` itself, so a user whose profile does it stays single-sourced
    /// (double-sourcing re-runs PATH prepends, duplicate completions, and
    /// anything else a `.bashrc` does non-idempotently).
    #[test]
    fn login_emulation_never_double_sources_a_profile_sourced_bashrc() {
        let out = run_login_emulation(
            "nice-bash-login-chain",
            &[
                (
                    ".bash_profile",
                    "echo NICE-BASH-PROFILE\n. \"$HOME/.bashrc\"\n",
                ),
                (".bashrc", "echo NICE-USER-BASHRC\n"),
            ],
        );
        assert_eq!(
            marker_count(&out, "NICE-BASH-PROFILE"),
            1,
            ".bash_profile must run exactly once. Got: <{out}>"
        );
        assert_eq!(
            marker_count(&out, "NICE-USER-BASHRC"),
            1,
            "a profile-sourced ~/.bashrc must run exactly once. Got: <{out}>"
        );
    }

    /// A `~/.bashrc` the user's profile does NOT source stays unread — the same
    /// thing a real login bash does. This is the documented PATH limitation as a
    /// test: discovery (`probe_argv`, same emulation) and panes agree.
    #[test]
    fn login_emulation_leaves_an_unsourced_bashrc_alone() {
        let out = run_login_emulation(
            "nice-bash-login-orphan-rc",
            &[
                (".bash_profile", "echo NICE-BASH-PROFILE\n"),
                (".bashrc", "echo NICE-USER-BASHRC\n"),
            ],
        );
        assert!(
            out.contains("NICE-BASH-PROFILE") && !out.contains("NICE-USER-BASHRC"),
            "an unsourced ~/.bashrc must stay unread, exactly as in a login bash. Got: <{out}>"
        );
    }

    /// Nice's hooks are installed after the user's config, so a profile that
    /// defines its own `claude` (or leaves PATH pointing at one) still ends up
    /// with the shadow — the ordering rule, observed at runtime rather than by
    /// reading the file.
    #[test]
    fn nice_hooks_win_over_user_config() {
        let out = run_login_emulation(
            "nice-bash-hook-order",
            &[(
                ".bash_profile",
                "claude() { echo NICE-USER-CLAUDE; }\n\
                 _nice_emit_cwd_osc7() { echo NICE-USER-EMITTER; }\n",
            )],
        );
        assert!(
            !out.contains("NICE-USER-EMITTER"),
            "the startup fire must run OUR emitter, not the user's stand-in. Got: <{out}>"
        );

        let home = ScratchHome::new("nice-bash-hook-order-fn");
        home.write_file(".bash_profile", "claude() { echo NICE-USER-CLAUDE; }\n");
        let rc = nice_bashrc_in(&home);
        let out = stdout_of(&run_bash(
            &home,
            &unwrapped_argv(&rc, "type -t claude; claude --version"),
            &[],
        ));
        assert!(
            !out.contains("NICE-USER-CLAUDE"),
            "Nice's claude() must be defined after the user's. Got: <{out}>"
        );
    }

    // ---- the claude() shadow, end to end in a real pty ----------------------

    /// What [`run_claude_shadow_e2e`] observed: the fake `claude`'s recorded argv
    /// lines (one per exec), every payload the wrapper wrote to the socket, and
    /// the pty transcript, which the assertions quote so a failure shows what the
    /// shell actually did.
    struct ShadowRun {
        execs: Vec<String>,
        payloads: Vec<String>,
        transcript: String,
    }

    /// Drive the injected `claude()` shadow END-TO-END through real `/bin/bash`
    /// under the real generated `nice.bashrc`: a fake `nc` answers the handshake
    /// with `reply`, and a fake `claude` appends its argv to a record file
    /// (exiting `attach_exit` when invoked as `attach …`, 0 otherwise).
    ///
    /// The pty is what makes the dispatch reachable at all — the wrapper passes
    /// straight through to the real binary when stdin is not a tty, so no `-ic`
    /// helper can enter it. The driver is zsh's `zpty` module: test
    /// infrastructure that happens to be written in the other shell (always
    /// present on macOS), driving a bash child. Same shape as the zsh suite's
    /// harness, with the inner shell swapped.
    fn run_claude_shadow_e2e(reply: &str, attach_exit: i32, command: &str) -> ShadowRun {
        use std::process::Stdio;

        let home = ScratchHome::new("nice-bash-shadow-home");
        let sent = home.path().join("payloads");
        let record = home.path().join("argv");

        // The handshake partner: record the payload, print Nice's one-line reply.
        // An empty `reply` prints a bare newline, which command substitution
        // strips — the socket-unreachable leg.
        home.install_executable(
            "nc",
            &format!(
                "#!/bin/bash\ncat >> {sent}\nprintf '%s\\n' {reply:?}\n",
                sent = sent.display()
            ),
        );
        // The exec target: record argv, then honor the requested attach outcome.
        home.install_executable(
            "claude",
            &format!(
                "#!/bin/bash\nprintf '%s\\n' \"$*\" >> {rec}\n\
                 [ \"$1\" = attach ] && exit {attach_exit}\nexit 0\n",
                rec = record.display()
            ),
        );
        // Load-bearing: `/etc/profile`'s `path_helper` demotes the fixture bin
        // behind `/usr/bin`, where a REAL `nc` lives. The login chain has to put
        // it back — which is also how this leg proves the chain ran.
        home.write_path_restoring_bash_profile("");

        let rc = nice_bashrc_in(&home);
        let capture = home.path().join("pty.bin");
        let driver = home.path().join("driver.zsh");
        // Both waits are READINESS-driven, not sleep-driven: the whole suite's
        // pty tests run in parallel, and a fixed "the shell is surely up by now"
        // sleep turns into a flake the moment the box is loaded.
        //
        //   * ready to type — the rc's startup OSC 7 has landed. It is the file's
        //     final statement, so seeing it means the login emulation and every
        //     hook definition are done.
        //   * done — either the follow-up marker echoed back as OUTPUT (the
        //     wrapper returned and we are at a prompt again) or the child is gone
        //     (the wrapper exec'd). The marker is typed with quotes inside the
        //     word so the tty's echo of the line can never be mistaken for the
        //     line's output.
        std::fs::write(
            &driver,
            r#"emulate -L zsh
zmodload zsh/zpty || exit 2
cmd=$1; out=$2; rc=$3
: > $out
drain() { local c; while zpty -rt n c 2>/dev/null; do print -rn -- "$c" >> $out; done }
zpty n /bin/bash --rcfile "$rc" -i
integer i
for (( i = 0; i < 400; i++ )); do
    drain
    grep -q $'\e]7;' $out && break
    sleep 0.05
done
zpty -w n "$cmd"
zpty -w n 'printf "%s\n" NICE-E2E-"DONE"' 2>/dev/null
for (( i = 0; i < 400; i++ )); do
    drain
    grep -q 'NICE-E2E-DONE' $out && break
    zpty -t n 2>/dev/null || break
    sleep 0.05
done
drain
zpty -d n 2>/dev/null
"#,
        )
        .unwrap();

        let status = home
            .command("/bin/zsh")
            .arg(&driver)
            .arg(command)
            .arg(&capture)
            .arg(&rc)
            // Non-empty socket + pane ids: what a real Nice pane injects.
            .env("NICE_SOCKET", home.path().join("nice.sock"))
            .env("NICE_TAB_ID", "t1")
            .env("NICE_PANE_ID", "t1-claude")
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
            transcript: String::from_utf8_lossy(&std::fs::read(&capture).unwrap_or_default())
                .escape_debug()
                .to_string(),
        }
    }

    /// The handshake payload the bash wrapper puts on the wire, field for field.
    /// Nice's socket server is dialect-agnostic, so this shape is a compatibility
    /// contract with the zsh stub, not a bash detail.
    #[test]
    fn claude_shadow_handshake_payload_shape_e2e() {
        let run = run_claude_shadow_e2e("newtab", 0, "claude --resume abc");
        let payload = run
            .payloads
            .first()
            .unwrap_or_else(|| panic!("no handshake reached the socket. pty: <{}>", run.transcript));
        for needle in [
            r#""action":"claude""#,
            r#""args":["--resume","abc"]"#,
            r#""tabId":"t1""#,
            r#""paneId":"t1-claude""#,
            r#""cwd":"/"#,
        ] {
            assert!(
                payload.contains(needle),
                "handshake payload missing {needle}. Got: <{payload}>"
            );
        }
    }

    /// `newtab`: Nice opened the session elsewhere, so the wrapper returns
    /// without running anything. The handshake assertion is what keeps this from
    /// passing vacuously — "no execs" is also what a wrapper that never ran at
    /// all would produce.
    #[test]
    fn claude_shadow_newtab_mode_runs_nothing_e2e() {
        let run = run_claude_shadow_e2e("newtab", 0, "claude");
        assert_eq!(
            run.payloads.len(),
            1,
            "the wrapper must have handshaken exactly once. pty: <{}>",
            run.transcript
        );
        assert!(
            run.execs.is_empty(),
            "newtab must exec nothing. execs: {:?}, pty: <{}>",
            run.execs,
            run.transcript
        );
    }

    /// `inplace`: this pane becomes the session. The reply's settings pointer and
    /// session id are PREPENDED to whatever the user typed.
    #[test]
    fn claude_shadow_inplace_mode_prepends_settings_and_sid_e2e() {
        let sid = "b8c8244b-e94e-4c38-95fb-31be9a28187e";

        let bare = run_claude_shadow_e2e("inplace - ", 0, "claude --dangerously-skip-permissions");
        assert_eq!(
            bare.execs,
            vec!["--dangerously-skip-permissions".to_string()],
            "a `-` sid and no settings must add no flags. pty: <{}>",
            bare.transcript
        );

        let full = run_claude_shadow_e2e(&format!("inplace {sid} /ptr.json"), 0, "claude");
        assert_eq!(
            full.execs,
            vec![format!("--settings /ptr.json --session-id {sid}")],
            "settings then session-id, both ahead of the user's args. pty: <{}>",
            full.transcript
        );
    }

    /// The `attach` reply execs `claude attach <first 8 of the uuid>` — proving
    /// `${sid:0:8}` really is a substring in bash, where the zsh subscript form
    /// would have passed the whole uuid. When attach fails (a jobs entry the
    /// daemon left behind), it degrades to `--resume <full uuid>` rather than
    /// stranding the user at attach's error.
    #[test]
    fn claude_shadow_attach_mode_attaches_then_falls_back_e2e() {
        let sid = "b8c8244b-e94e-4c38-95fb-31be9a28187e";

        let ok = run_claude_shadow_e2e(
            &format!("attach {sid} /ptr.json"),
            0,
            &format!("claude --resume {sid}"),
        );
        assert_eq!(
            ok.execs,
            vec!["attach b8c8244b".to_string()],
            "a successful attach must be the only exec — no resume behind it. pty: <{}>",
            ok.transcript
        );
        // The attached Claude ran as a CHILD, so this shell outlived it: Nice
        // must be told the pane is a prompt again, or its promotion flag stays
        // set and every later `claude` here opens a new session.
        assert_eq!(
            ok.payloads.last().map(String::as_str),
            Some(r#"{"action":"claude_exited","paneId":"t1-claude"}"#),
            "a returned attach must report the pane back to Nice. payloads: {:?}",
            ok.payloads
        );

        let fell_back = run_claude_shadow_e2e(
            &format!("attach {sid} /ptr.json"),
            1,
            &format!("claude --resume {sid}"),
        );
        assert_eq!(
            fell_back.execs,
            vec![
                "attach b8c8244b".to_string(),
                format!("--settings /ptr.json --resume {sid}"),
            ],
            "a failed attach must degrade to the durable --resume. pty: <{}>",
            fell_back.transcript
        );
        assert!(
            !fell_back
                .payloads
                .iter()
                .any(|p| p.contains("claude_exited")),
            "the fallback EXECS claude — the pty's own exit reports that pane. payloads: {:?}",
            fell_back.payloads
        );
    }

    /// The `resume` reply execs `--resume <uuid>` and DROPS the user's original
    /// `attach <id>` args — they name a session the daemon no longer hosts.
    #[test]
    fn claude_shadow_resume_mode_replaces_the_attach_args_e2e() {
        let sid = "b8c8244b-e94e-4c38-95fb-31be9a28187e";
        let run = run_claude_shadow_e2e(&format!("resume {sid}"), 0, "claude attach b8c8244b");
        assert_eq!(
            run.execs,
            vec![format!("--resume {sid}")],
            "pty: <{}>",
            run.transcript
        );
    }

    /// Nice unreachable (empty reply) or speaking a reply this shell does not
    /// know: never strand the user — exec the real binary with their own args.
    /// `exec claude` and not `exec command claude`: bash's `exec` PATH-searches
    /// and never resolves functions, so the shadow is already bypassed, while
    /// `exec command claude` would have exec'd the `/usr/bin/command` shim.
    #[test]
    fn claude_shadow_unreachable_or_unknown_reply_runs_claude_directly_e2e() {
        let empty = run_claude_shadow_e2e("", 0, "claude --resume abc");
        assert_eq!(
            empty.execs,
            vec!["--resume abc".to_string()],
            "an unreachable socket must exec claude with the user's args. pty: <{}>",
            empty.transcript
        );
        assert!(
            empty.transcript.contains("control socket unreachable"),
            "the user must be told why. pty: <{}>",
            empty.transcript
        );

        let junk = run_claude_shadow_e2e("kaboom", 0, "claude --resume abc");
        assert_eq!(
            junk.execs,
            vec!["--resume abc".to_string()],
            "an unknown reply must exec claude with the user's args. pty: <{}>",
            junk.transcript
        );
        assert!(
            junk.transcript.contains("unexpected response"),
            "the user must be told why. pty: <{}>",
            junk.transcript
        );
    }

    // ---- OSC 7 cwd reporting ------------------------------------------------

    /// The startup fire, in a real bash: one clean `file://` payload for the
    /// spawn cwd, before the pane's command runs. This is what Nice's app-typed
    /// prefill keys its readiness on, so "it happens at all" is load-bearing —
    /// and the `%`-free payload is the encoding regression sentinel.
    #[test]
    fn nice_bashrc_emits_a_clean_startup_osc7_at_runtime() {
        let home = ScratchHome::new("nice-bash-osc7-home");
        let rc = nice_bashrc_in(&home);
        let work = scratch("nice-bash-osc7-work");

        let mut cmd = home.command(SYSTEM_BASH);
        let argv = injected_argv(&rc, Some("true"));
        let out = cmd
            .args(&argv[1..])
            .current_dir(&work.0)
            .output()
            .expect("spawn /bin/bash");

        let payloads = osc7_payloads(&out.stdout);
        assert_eq!(
            payloads.len(),
            1,
            "startup must fire exactly one OSC 7. Captured: {:?}",
            String::from_utf8_lossy(&out.stdout).escape_debug().to_string()
        );
        let payload = &payloads[0];
        assert!(
            payload.starts_with("file://"),
            "OSC 7 payload must be a file:// URL. Got: <{payload}>"
        );
        let last = work.0.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(
            payload.contains(last),
            "payload path must contain the cwd's last component ({last}). Got: <{payload}>"
        );
        assert!(
            !payload.contains('%'),
            "a clean path must percent-encode nothing. Got: <{payload}>"
        );
    }

    /// The encoding order (`%` before space) over real paths: a `%` becomes
    /// `%25` and is not re-encoded, a space becomes `%20` and is not left raw.
    #[test]
    fn nice_bashrc_percent_encodes_spaces_and_percents_at_runtime() {
        let home = ScratchHome::new("nice-bash-osc7-encode");
        let rc = nice_bashrc_in(&home);

        for (dir, want, unwanted) in [
            ("with space", "with%20space", " "),
            ("with%percent", "with%25percent", "%percent"),
        ] {
            let work = home.path().join(dir);
            std::fs::create_dir_all(&work).unwrap();
            let argv = injected_argv(&rc, Some("true"));
            let out = home
                .command(SYSTEM_BASH)
                .args(&argv[1..])
                .current_dir(&work)
                .output()
                .expect("spawn /bin/bash");

            let payloads = osc7_payloads(&out.stdout);
            assert_eq!(payloads.len(), 1, "one startup OSC 7 per pane");
            assert!(
                payloads[0].contains(want),
                "`{dir}` must encode as `{want}`. Got: <{}>",
                payloads[0]
            );
            assert!(
                !payloads[0].contains(unwanted),
                "`{dir}` must not leave `{unwanted}` in the payload. Got: <{}>",
                payloads[0]
            );
        }
    }

    /// `PROMPT_COMMAND` fires before EVERY prompt, so the emitter dedups on
    /// `$PWD`: two prompts at the same cwd report once. Driven by calling the
    /// hook directly (the functions are callable from the `-c` script because the
    /// rcfile was sourced first), which is what the prompt loop does.
    #[test]
    fn nice_bashrc_prompt_command_emits_only_on_cwd_change_at_runtime() {
        let home = ScratchHome::new("nice-bash-osc7-dedup");
        let rc = nice_bashrc_in(&home);
        let w2 = home.path().join("w2");
        let w3 = home.path().join("w3");
        std::fs::create_dir_all(&w2).unwrap();
        std::fs::create_dir_all(&w3).unwrap();

        let out = run_bash(
            &home,
            &unwrapped_argv(
                &rc,
                r#"cd "$W2"; _nice_osc7_prompt_command; _nice_osc7_prompt_command; cd "$W3"; _nice_osc7_prompt_command"#,
            ),
            &[
                ("W2", w2.to_str().unwrap()),
                ("W3", w3.to_str().unwrap()),
            ],
        );

        let payloads = osc7_payloads(&out.stdout);
        assert_eq!(
            payloads.len(),
            3,
            "startup + w2 + w3, with the repeat prompt at w2 suppressed. Got: {payloads:?}"
        );
        assert!(
            payloads[1].ends_with("/w2") && payloads[2].ends_with("/w3"),
            "the two reported changes must be the two real cd's. Got: {payloads:?}"
        );
    }

    /// A user profile that registers its own `PROMPT_COMMAND` keeps it: ours is
    /// appended, not assigned over.
    #[test]
    fn nice_bashrc_appends_to_a_user_prompt_command_at_runtime() {
        let home = ScratchHome::new("nice-bash-prompt-append");
        home.write_file(
            ".bash_profile",
            "PROMPT_COMMAND='echo NICE-USER-PROMPT-COMMAND'\n",
        );
        let rc = nice_bashrc_in(&home);
        let w2 = home.path().join("w2");
        std::fs::create_dir_all(&w2).unwrap();

        let out = run_bash(
            &home,
            &unwrapped_argv(&rc, r#"cd "$W2"; eval "$PROMPT_COMMAND""#),
            &[("W2", w2.to_str().unwrap())],
        );
        let text = stdout_of(&out);

        assert!(
            text.contains("NICE-USER-PROMPT-COMMAND"),
            "the user's PROMPT_COMMAND must survive. Got: <{}>",
            text.escape_debug()
        );
        let payloads = osc7_payloads(&out.stdout);
        assert_eq!(
            payloads.len(),
            2,
            "startup + the cd, both ours. Got: {payloads:?}"
        );
        assert!(payloads[1].ends_with("/w2"), "Got: {payloads:?}");
    }
}
