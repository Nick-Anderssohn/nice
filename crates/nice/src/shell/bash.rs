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
//! is gated on a **version probe**: [`BashProfile::probed`] runs the resolved
//! binary once at resolve time ([`probe_version`], `--norc --noprofile`, ~ms) and
//! only a bash ≥ 4.3 reports [`ComposeSupport::Trigger`]. Stock 3.2 — and any
//! probe that fails to spawn, exits non-zero, or prints something unparseable —
//! reports [`ComposeSupport::None`], so a bash pane's
//! [`PaneShell`](super::PaneShell) snapshot keeps the trigger bytes away from a
//! prompt that could not consume them (inventory finding F6).
//!
//! Prefill is [`PrefillStrategy::AppTyped`]: bash has no `print -z`, so Nice
//! types the deferred-resume line into the pty itself once the pane's first OSC 7
//! reports readiness (design §6.4). The script therefore never references
//! `NICE_PREFILL_COMMAND` — pinned as a negative by the structural tests.
//!
//! Like the zsh stubs, the script body is a static file pulled in with
//! `include_str!` (design §8): no templating, ever — every dynamic value reaches
//! the shell through an env var. And like them it is now **byte-pinned**: the
//! contract froze when Command Compose landed (design §10), so a digest over
//! this body plus a `write_rc_files` round-trip turn any edit into a deliberate,
//! reviewed digest bump. The structural positive/negative sets below stay the
//! readable half of the contract — they say what the script must do; the digest
//! only says nothing changed silently.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{
    ComposeSupport, InjectPaths, PrefillStrategy, ShellKind, ShellProfile, SpawnCtx, UserShellEnv,
};

/// The generated `nice.bashrc`: login emulation, the `claude()` shadow, and the
/// `PROMPT_COMMAND` OSC 7 emitter. Written into the profile's rc directory by
/// [`ShellProfile::write_rc_files`] and referenced by `--rcfile`.
pub const NICE_BASHRC_BODY: &str = include_str!("scripts/bash/nice.bashrc");

/// The rc file's name inside the profile's rc directory.
const RC_FILE_NAME: &str = "nice.bashrc";

/// The lowest bash that can honor the Command Compose trigger. `READLINE_LINE`
/// became writable from a `bind -x` handler in 4.0, but a `bind -x` binding
/// whose key sequence is longer than two characters silently never fires before
/// **4.3** — and the trigger is the 8-byte `\e[5099~`. Gating at 4.0 would
/// recreate finding F6 on 4.0–4.2: Nice would write bytes a dead binding could
/// not consume, and `[5099~` would land in the user's line. The rc script's own
/// `BASH_VERSINFO` guard checks this same pair, so the two sides cannot disagree
/// in the dangerous direction. (Correction to design §6.3's "≥ 4.0"; there is no
/// real macOS population on 4.0–4.2 — stock is 3.2.57, homebrew is 5.x.)
const COMPOSE_MIN_VERSION: (u32, u32) = (4, 3);

/// The version probe's shell command: `major.minor` on stdout and nothing else.
/// Run under `--norc --noprofile` so no user config can slow it down or pollute
/// stdout — the whole probe is a sub-millisecond fork of a shell that reads no
/// files.
const VERSION_PROBE_CMD: &str = r#"printf %s.%s "${BASH_VERSINFO[0]}" "${BASH_VERSINFO[1]}""#;

/// Parse the probe's `major.minor` output. Strict on purpose — exactly two
/// dot-separated non-negative integers — because every rejection falls to
/// [`ComposeSupport::None`], the safe direction: a bash whose version Nice
/// cannot read is treated as one that cannot honor the trigger.
fn parse_version(out: &str) -> Option<(u32, u32)> {
    let (major, minor) = out.trim().split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Run `<path> --norc --noprofile -c 'printf …'` and read back `(major, minor)`.
///
/// Fails toward `None` at every step — the binary is missing or not executable,
/// it is not a bash (an `sh` chokes on `BASH_VERSINFO` and exits non-zero), it
/// exits non-zero for any other reason, or its output does not parse. This is
/// the seam the tests point at stub executables printing canned versions; it
/// takes a plain path, so nothing about it is tied to a real bash.
pub(crate) fn probe_version(path: &str) -> Option<(u32, u32)> {
    let out = Command::new(path)
        .args(["--norc", "--noprofile", "-c", VERSION_PROBE_CMD])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// Where a modern bash lands on macOS when it is not the stock one: the two
/// homebrew prefixes (Apple silicon, then Intel), walked in that order.
const HOMEBREW_BASH_PATHS: [&str; 2] = ["/opt/homebrew/bin/bash", "/usr/local/bin/bash"];

/// The env override that points the compose harness at an arbitrary bash build
/// — the first candidate [`find_compose_bash`] tries.
const TEST_BASH_ENV: &str = "NICE_TEST_BASH";

/// Find a bash that can actually honor the Command Compose trigger — one
/// [`probe_version`] reports at ≥ [`COMPOSE_MIN_VERSION`]. Search order:
/// `NICE_TEST_BASH` → `/opt/homebrew/bin/bash` → `/usr/local/bin/bash` → every
/// `bash` on this process's `PATH`, first match wins. **Every** candidate goes
/// through the same probe, the env override included, so a `NICE_TEST_BASH`
/// aimed at stock 3.2 is rejected rather than trusted.
///
/// `None` means "this machine has no bash ≥ 4.3", which is the normal state of
/// a stock macOS: both consumers — the compose real-pty e2e and the
/// `compose-live-bash` self-test scenario — then self-skip with a printed
/// notice (design §10's environment-gated-test policy).
///
/// Deliberately **not** `#[cfg(test)]`: `compose_live.rs` is compiled into the
/// shipping binary, and one shared helper is what keeps the scenario and the
/// e2e from drifting into two different notions of "a bash that can compose".
/// Nothing on a production code path calls it — panes get their bash from
/// `resolve()`, never from this search.
pub(crate) fn find_compose_bash() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(over) = std::env::var_os(TEST_BASH_ENV) {
        candidates.push(PathBuf::from(over));
    }
    candidates.extend(HOMEBREW_BASH_PATHS.iter().map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("bash")));
    }
    candidates.into_iter().find(|cand| {
        probe_version(&cand.to_string_lossy()).is_some_and(|v| v >= COMPOSE_MIN_VERSION)
    })
}

/// bash — the `--rcfile`-injected profile (design §6.2). `path` is the resolved
/// binary, kept as resolved: a homebrew `/opt/homebrew/bin/bash` gets the bash
/// profile *at that path*, never rewritten to `/bin/bash`.
pub struct BashProfile {
    path: String,
    /// `(major, minor)` of the binary at [`Self::path`], or `None` when unprobed
    /// or unreadable. The only input to [`ShellProfile::compose_support`].
    version: Option<(u32, u32)>,
}

impl BashProfile {
    /// A profile that has NOT probed its binary — `compose_support` is
    /// [`ComposeSupport::None`]. Every path-shaped question (`spawn_argv`,
    /// `rc_dir_for`, `probe_argv`, `comm_name`) is answered from the path alone,
    /// so callers that only ask those — the unit tests, in practice — get them
    /// without forking a shell. Production resolution uses [`Self::probed`].
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            version: None,
        }
    }

    /// Probe the binary's version once, here at construction (design §6.3 allows
    /// the synchronous probe in bootstrap: `--norc --noprofile` reads no files).
    /// The one production constructor — `resolve()` builds every live
    /// `BashProfile` this way, so a pane's compose capability is decided before
    /// the first spawn and never re-probed.
    pub fn probed(path: impl Into<String>) -> Self {
        let path = path.into();
        let version = probe_version(&path);
        Self { path, version }
    }

    /// Construct with an already-known version — the injectable seam for
    /// [`ShellProfile::compose_support`]'s truth table, with no subprocess.
    #[cfg(test)]
    pub(crate) fn with_version(path: impl Into<String>, version: Option<(u32, u32)>) -> Self {
        Self {
            path: path.into(),
            version,
        }
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

    /// [`ComposeSupport::Trigger`] iff the probed version is at least
    /// [`COMPOSE_MIN_VERSION`] — a homebrew bash 5 gets compose, stock
    /// `/bin/bash` 3.2 does not, and an unprobed or unreadable version
    /// (`None`) does not either. `None` is the safe direction: it is the pane
    /// snapshot that keeps trigger bytes off a prompt with no binding for them.
    fn compose_support(&self) -> ComposeSupport {
        match self.version {
            Some(v) if v >= COMPOSE_MIN_VERSION => ComposeSupport::Trigger,
            _ => ComposeSupport::None,
        }
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
    use std::process::{Command, Output, Stdio};

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
        // Compose is off for an UNPROBED profile (this fixture) exactly as it is
        // for a probed stock /bin/bash 3.2 — the version gate owns that answer,
        // and the tests below pin both halves. Prefill is app-typed because bash
        // has no `print -z`.
        assert_eq!(p.compose_support(), ComposeSupport::None);
        assert_eq!(p.prefill(), PrefillStrategy::AppTyped);
    }

    // ---- the >= 4.3 compose version gate (work items 1 + 6) ----------------

    /// The parser is deliberately strict: anything that is not exactly two
    /// dot-separated integers is `None`, which routes to `ComposeSupport::None`
    /// — the direction that cannot put `[5099~` in a user's line.
    #[test]
    fn bash_version_parse_reads_major_minor_and_fails_toward_none() {
        assert_eq!(parse_version("3.2"), Some((3, 2)));
        assert_eq!(parse_version("4.2"), Some((4, 2)));
        assert_eq!(parse_version("4.3"), Some((4, 3)));
        assert_eq!(parse_version("5.3"), Some((5, 3)));
        // The probe prints no trailing newline, but a stub (or a future spelling)
        // may; surrounding whitespace is trimmed, not rejected.
        assert_eq!(parse_version("  5.2\n"), Some((5, 2)));
        for bad in [
            "",           // probe produced nothing
            "\n",         //
            "5",          // no minor
            "5.",         //
            ".3",         //
            "5.x",        // non-numeric minor
            "x.3",        // non-numeric major
            "-1.2",       // negative major
            "5.3.1",      // a full version string is NOT the probe's contract
            "GNU bash 5", // someone else's --version banner
        ] {
            assert_eq!(parse_version(bad), None, "{bad:?} must not parse");
        }
    }

    /// The probe against real executables, through the same seam production
    /// uses: it takes a path, so a stub script printing a canned version stands
    /// in for any bash Nice might resolve.
    #[test]
    fn bash_version_probe_reads_stub_executables_and_fails_toward_none() {
        let home = ScratchHome::new("nice-bash-probe");
        let stub = |name: &str, body: &str| {
            home.install_executable(name, body).display().to_string()
        };

        let modern = stub("bash-5", "#!/bin/sh\nprintf %s 5.3\n");
        assert_eq!(probe_version(&modern), Some((5, 3)), "canned modern bash");

        let gate = stub("bash-43", "#!/bin/sh\nprintf %s 4.3\n");
        assert_eq!(probe_version(&gate), Some((4, 3)));

        let silent = stub("bash-silent", "#!/bin/sh\nexit 0\n");
        assert_eq!(probe_version(&silent), None, "no output");

        let garbage = stub("bash-garbage", "#!/bin/sh\nprintf %s 'not a version'\n");
        assert_eq!(probe_version(&garbage), None, "unparseable output");

        let failing = stub("bash-fails", "#!/bin/sh\nprintf %s 5.2\nexit 1\n");
        assert_eq!(
            probe_version(&failing),
            None,
            "a non-zero exit is not trusted even when stdout parses"
        );

        assert_eq!(
            probe_version(&home.bin().join("nope").display().to_string()),
            None,
            "a binary that cannot be spawned probes as None"
        );
    }

    /// The real macOS baseline: `/bin/bash` is 3.2.57 forever, so a probed stock
    /// bash reports its version and still gets no compose. Unconditional —
    /// `/bin/bash` is present on every machine.
    #[test]
    fn bash_version_probe_reports_stock_bin_bash_as_3_2() {
        assert_eq!(
            probe_version(SYSTEM_BASH),
            Some((3, 2)),
            "macOS ships /bin/bash 3.2.57"
        );
        assert_eq!(
            BashProfile::probed(SYSTEM_BASH).compose_support(),
            ComposeSupport::None,
            "stock bash cannot bind a multi-character `bind -x` sequence"
        );
    }

    /// The gate itself: `Trigger` iff the probed version is >= 4.3.
    #[test]
    fn bash_compose_support_needs_bash_4_3() {
        let support = |version| BashProfile::with_version("/bin/bash", version).compose_support();
        for off in [None, Some((3, 2)), Some((4, 0)), Some((4, 2))] {
            assert_eq!(
                support(off),
                ComposeSupport::None,
                "{off:?}: below the gate (or unprobed) — no trigger bytes"
            );
        }
        for on in [Some((4, 3)), Some((4, 10)), Some((5, 3)), Some((6, 0))] {
            assert_eq!(
                support(on),
                ComposeSupport::Trigger,
                "{on:?}: at or above the gate"
            );
        }
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

    // ---- script structural tests (what the contract SAYS, design §10) ------
    //
    // The readable half of the frozen contract: the digest pin at the bottom of
    // this module catches any silent change, these say which parts matter and
    // why. Neither replaces the other.

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
    /// wrong or inert in bash, plus the prefill env var bash deliberately has
    /// no shell-side contract for.
    ///
    /// The compose negatives this set once carried (`bind -x`, `READLINE_LINE`,
    /// `5099`) are gone on purpose: the compose section is now IN the script,
    /// and the positive pins below own that surface.
    #[test]
    fn nice_bashrc_carries_no_zsh_isms_or_shell_side_prefill() {
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

    // ---- Command Compose: static pins (work item 3) ------------------------
    //
    // The bash half of the zsh static-text suite in `shell/zsh.rs`
    // (`zshrc_compose_defines_widget_and_binds_trigger_in_all_keymaps` and
    // friends), minus the precmd pin — the synchronous handler has nothing in
    // flight across prompts to abandon — plus two pins with no zsh analogue:
    // the `BASH_VERSINFO` guard the `bind` calls must sit inside, and the
    // dialect word in the instruction.

    /// The handler is bound to the trigger in ALL THREE keymaps that can hold
    /// an idle prompt, with the shell-side trigger text derived from the Rust
    /// constant so the two sides can never drift — and every `bind` sits INSIDE
    /// the `BASH_VERSINFO` ≥ 4.3 guard, since a `bind -x` on an 8-byte sequence
    /// silently never fires before 4.3.
    #[test]
    fn nice_bashrc_compose_binds_the_trigger_in_all_keymaps_under_the_version_guard() {
        use crate::shell::{COMPOSE_TRIGGER_BINDKEY, COMPOSE_TRIGGER_SEQ};

        assert!(NICE_BASHRC_BODY.contains("_nice_command_compose() {"));

        // The guard is spelled against BASH_VERSINFO's major AND minor: a
        // majors-only check would open compose on 4.0–4.2, where the binding
        // is dead and the trigger bytes would land as `[5099~` in the line.
        let guard =
            "if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 3) )); then";
        let guard_at = at(guard);
        // The guard's own `fi` — the first one after it.
        let fi_at = guard_at
            + NICE_BASHRC_BODY[guard_at..]
                .find("\nfi\n")
                .expect("the version guard must be closed by a top-level `fi`");

        for bind in [
            format!(r#"bind -x '"{COMPOSE_TRIGGER_BINDKEY}": _nice_command_compose'"#),
            format!(r#"bind -m vi-insert -x '"{COMPOSE_TRIGGER_BINDKEY}": _nice_command_compose'"#),
            format!(r#"bind -m vi-command -x '"{COMPOSE_TRIGGER_BINDKEY}": _nice_command_compose'"#),
        ] {
            let bind_at = at(&bind);
            assert!(
                bind_at > guard_at && bind_at < fi_at,
                "`{bind}` must sit inside the BASH_VERSINFO >= 4.3 guard"
            );
        }
        // No `bind` may escape the guard: 3.2 must source this file silently.
        // Counted over the CODE — the comments discuss `bind -x` at length.
        assert_eq!(
            code_only().matches("bind ").count(),
            3,
            "the only `bind` calls in the script are the three guarded ones"
        );

        // Byte agreement: `\e` + the rest of the bind text == the pty bytes.
        let mut expected = vec![0x1b_u8];
        expected.extend_from_slice(
            COMPOSE_TRIGGER_BINDKEY
                .strip_prefix(r"\e")
                .expect("the bindkey spelling starts with \\e")
                .as_bytes(),
        );
        assert_eq!(COMPOSE_TRIGGER_SEQ, expected.as_slice());
    }

    /// The dialect line (inventory finding F5): the instruction asks for a
    /// *bash* command line, and no code in the script mentions zsh. The
    /// instruction is otherwise byte-identical to the zsh widget's — the single
    /// word is the whole delta.
    #[test]
    fn nice_bashrc_compose_instruction_asks_for_bash_not_zsh() {
        let code = code_only();
        assert!(
            code.contains(
                "_nice_compose_instruction='Convert this plain-English request into a \
                 single bash command line for macOS."
            ),
            "the instruction must ask for `a single bash command line`"
        );
        assert!(
            !code.contains("zsh"),
            "no code in nice.bashrc may mention zsh — least of all the instruction"
        );
        // The dialect word is the ONLY delta from the zsh instruction.
        let bash_instruction = instruction_of(&code_only());
        let zsh_instruction = instruction_of(crate::shell::zsh::ZSHRC_BODY);
        assert_eq!(
            bash_instruction.replacen("single bash command", "single zsh command", 1),
            zsh_instruction,
            "the two instructions differ in the dialect word and nothing else"
        );
    }

    /// The `_nice_compose_instruction='…'` value of a script body.
    fn instruction_of(body: &str) -> String {
        let start = body
            .find("_nice_compose_instruction='")
            .expect("script defines _nice_compose_instruction");
        let rest = &body[start + "_nice_compose_instruction='".len()..];
        rest[..rest.find('\'').expect("instruction is single-quoted")].to_string()
    }

    /// The never-auto-execute invariant as a pinned NEGATIVE: no code path may
    /// accept the line. readline's spelling of "run it" is the `accept-line`
    /// function; the handler's only writes are to `READLINE_LINE` /
    /// `READLINE_POINT`, which is editing state, not execution.
    #[test]
    fn nice_bashrc_compose_never_accepts_the_line() {
        let code = code_only();
        assert!(
            !code.contains("accept-line"),
            "the injected rc must never bind or invoke readline's accept-line"
        );
        let writes: Vec<&str> = code
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("READLINE_"))
            .collect();
        assert_eq!(
            writes,
            vec![r#"READLINE_LINE="$out""#, "READLINE_POINT=${#out}"],
            "the handler's only mutations are the composed line and the cursor"
        );
    }

    /// The user's text reaches claude on STDIN (a herestring off a
    /// `$READLINE_LINE` copy), never argv — so no quoting of user text can ever
    /// be wrong — and the flags ride `command claude -p`, which cannot re-enter
    /// the `claude()` shadow defined above.
    #[test]
    fn nice_bashrc_compose_pipes_the_line_via_stdin() {
        let code = code_only();
        assert!(code.contains(r#"local request="$READLINE_LINE" sgr out rc"#));
        assert!(code.contains(r#"_nice_compose_translate <<< "$request""#));
        assert!(code.contains(r#"command claude -p "$_nice_compose_instruction""#));
        for forbidden in [
            r#"claude -p "$request""#,
            r#"claude -p "$READLINE_LINE""#,
            r#"claude "$request""#,
        ] {
            assert!(
                !code.contains(forbidden),
                "user text must never reach claude's argv (`{forbidden}`)"
            );
        }
    }

    /// An empty line is a no-op — no subprocess, no status line.
    #[test]
    fn nice_bashrc_compose_empty_line_is_a_noop() {
        assert!(code_only().contains(r#"[[ -z "$READLINE_LINE" ]] && return 0"#));
    }

    /// The handler reads its runtime knobs from `$NICE_COMPOSE_CONF` — accent
    /// for the status line's color, model/effort for the claude flags.
    #[test]
    fn nice_bashrc_compose_reads_the_conf_for_accent_model_and_effort() {
        let code = code_only();
        assert!(code.contains("NICE_COMPOSE_CONF"));
        for key in ["accent", "model", "effort"] {
            assert!(
                code.contains(&format!("_nice_compose_conf_get {key}")),
                "the conf key `{key}` must be read"
            );
        }
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

    // ---- Command Compose: real-bash 3.2 end-to-end (work item 4) -----------
    //
    // The bash port of `shell/zsh.rs`'s `compose_*_e2e` quartet. Unconditional
    // and CI-safe: the compose helpers are defined OUTSIDE the ≥ 4.3 `bind`
    // guard precisely so stock `/bin/bash` 3.2 — on every macOS — can run
    // them. Only the `bind` calls need a modern bash, and those are the pty
    // tier's business.

    /// Install a fake `claude` into the scratch home's `bin/` recording its
    /// argv (one per line) and stdin under `rec`, then printing `reply` — or,
    /// with `fail`, exiting 1 with no output at all.
    ///
    /// The caller must also have written a PATH-restoring profile
    /// ([`ScratchHome::write_path_restoring_bash_profile`]): the rc's login
    /// emulation sources `/etc/profile`, whose `path_helper` would otherwise
    /// demote the fixture `bin/` behind `/usr/bin`.
    fn write_fake_claude(home: &ScratchHome, rec: &Path, reply: &str, fail: bool) {
        std::fs::create_dir_all(rec).expect("create claude record dir");
        let body = if fail {
            "#!/bin/bash\nexit 1\n".to_string()
        } else {
            format!(
                "#!/bin/bash\nprintf '%s\\n' \"$@\" > {rec}/argv\ncat > {rec}/stdin\nprintf '%s' {reply:?}\n",
                rec = rec.display(),
            )
        };
        home.install_executable("claude", &body);
    }

    /// Source the real `nice.bashrc` in `/bin/bash --rcfile … -i -c` under the
    /// scratch home and run `script`, returning its stdout with the rc's
    /// startup OSC 7 stripped (everything through the last BEL) — the bash
    /// flavor of zsh's `run_zsh_compose`.
    fn run_bash_compose(home: &ScratchHome, extra_env: &[(&str, &str)], script: &str) -> String {
        let rc = nice_bashrc_in(home);
        let out = run_bash(home, &unwrapped_argv(&rc, script), extra_env);
        let text = stdout_of(&out);
        match text.rfind('\u{07}') {
            Some(i) => text[i + 1..].to_string(),
            None => text,
        }
    }

    /// `_nice_compose_translate` pipes the request through the fake claude's
    /// STDIN, passes `-p` + the instruction, appends the conf file's
    /// `--model`/`--effort`, and prints the reply verbatim.
    #[test]
    fn bash_compose_translate_pipes_stdin_and_conf_flags_e2e() {
        let home = ScratchHome::new("nice-bash-compose-conf");
        home.write_path_restoring_bash_profile("");
        let rec = scratch("nice-bash-compose-conf-rec");
        write_fake_claude(&home, &rec.0, "ls -la", false);
        let conf = home.write_file(
            "compose.json",
            r##"{"accent":"#7a94db","model":"opus","effort":"high"}"##,
        );

        let out = run_bash_compose(
            &home,
            &[("NICE_COMPOSE_CONF", conf.to_str().unwrap())],
            r#"_nice_compose_translate <<< "list files with details""#,
        );

        assert_eq!(out, "ls -la", "translate prints the model reply verbatim");
        let argv = std::fs::read_to_string(rec.0.join("argv")).expect("fake claude ran");
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec![
                "-p",
                instruction_of(&code_only()).as_str(),
                "--model",
                "opus",
                "--effort",
                "high",
            ],
            "argv is `-p <instruction>` plus the conf-driven flags"
        );
        assert_eq!(
            std::fs::read_to_string(rec.0.join("stdin")).unwrap(),
            "list files with details\n",
            "the user text reaches claude on stdin, never argv"
        );
        assert!(
            !argv.contains("list files"),
            "the user text must NOT appear on the command line. Got: <{argv}>"
        );
    }

    /// Without a conf file, translate still runs — bare `claude -p`, no flags
    /// (and no `set -u` trip on the empty array); a failing claude yields empty
    /// output, which is what makes the handler print its failure line and keep
    /// the typed English.
    #[test]
    fn bash_compose_translate_no_conf_and_failure_e2e() {
        let home = ScratchHome::new("nice-bash-compose-noconf");
        home.write_path_restoring_bash_profile("");
        let rec = scratch("nice-bash-compose-noconf-rec");
        write_fake_claude(&home, &rec.0, "echo hi", false);

        let out = run_bash_compose(&home, &[], r#"_nice_compose_translate <<< "say hi""#);
        assert_eq!(out, "echo hi");
        let argv = std::fs::read_to_string(rec.0.join("argv")).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec!["-p", instruction_of(&code_only()).as_str()],
            "no conf ⇒ no --model/--effort. Got: <{argv}>"
        );

        let failing = ScratchHome::new("nice-bash-compose-fail");
        failing.write_path_restoring_bash_profile("");
        write_fake_claude(&failing, &scratch("nice-bash-compose-fail-rec").0, "", true);
        let out = run_bash_compose(&failing, &[], r#"_nice_compose_translate <<< "x""#);
        assert_eq!(out, "", "a failing claude yields empty translate output");
    }

    /// `_nice_compose_strip` trims and unwraps fences/backticks — driven
    /// through real bash 3.2, because the 3.2-safe parameter-expansion arcana
    /// (`${out#"${out%%[![:space:]]*}"}`, not zsh's extendedglob) is the thing
    /// under test.
    #[test]
    fn bash_compose_strip_unwraps_fences_e2e() {
        let home = ScratchHome::new("nice-bash-compose-strip");
        home.write_path_restoring_bash_profile("");

        let script = concat!(
            "printf '%s|' \"$(_nice_compose_strip $'```bash\\nls -la\\n```')\"\n",
            "printf '%s|' \"$(_nice_compose_strip $'  \\tfind . -name \"*.rs\"\\n')\"\n",
            "printf '%s|' \"$(_nice_compose_strip '`echo hi`')\"\n",
            // A multi-line command inside a fence survives with its newlines.
            "printf '%s|' \"$(_nice_compose_strip $'```\\nfor f in *; do\\n  echo $f\\ndone\\n```')\"\n",
        );
        let out = run_bash_compose(&home, &[], script);
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

    /// The bash conf reader and the Rust writer's parser agree key-for-key on a
    /// production-shaped blob — the app↔shell interchange pin, ported from zsh.
    #[test]
    fn bash_compose_conf_get_matches_rust_parser_e2e() {
        let home = ScratchHome::new("nice-bash-compose-confget");
        home.write_path_restoring_bash_profile("");
        let blob = r##"{"accent":"#c96442","model":"sonnet","effort":"medium"}"##;
        let conf = home.write_file("compose.json", blob);

        let out = run_bash_compose(
            &home,
            &[("NICE_COMPOSE_CONF", conf.to_str().unwrap())],
            r#"printf '%s|%s|%s' "$(_nice_compose_conf_get accent)" "$(_nice_compose_conf_get model)" "$(_nice_compose_conf_get effort)""#,
        );
        let bash_values: Vec<&str> = out.split('|').collect();
        for (i, key) in ["accent", "model", "effort"].iter().enumerate() {
            assert_eq!(
                Some(bash_values[i].to_string()),
                crate::compose_conf::parse_value(blob, key),
                "bash and Rust parsers agree on {key}"
            );
        }
    }

    /// The guard-closed half of acceptance 3, on the bash every macOS actually
    /// ships: stock 3.2 sources the whole rc — compose section included —
    /// cleanly. The helpers are defined (they live outside the guard, which is
    /// what makes the e2e quartet above unconditional), the three `bind` calls
    /// are skipped, and NOTHING about `bind` reaches the user's terminal. A
    /// `bind -x` warning leaking into every stock-bash pane's first screen is
    /// exactly the visible-breakage class the version gate exists to prevent.
    #[test]
    fn nice_bashrc_sources_cleanly_on_stock_bash_3_2_with_the_guard_closed() {
        let home = ScratchHome::new("nice-bash-compose-guard");
        home.write_path_restoring_bash_profile("");
        let rc = nice_bashrc_in(&home);

        let script = "type -t _nice_command_compose _nice_compose_translate \
                      _nice_compose_strip _nice_compose_sgr _nice_compose_conf_get; \
                      bind -p 2>/dev/null | grep -c 5099";
        let out = run_bash(&home, &unwrapped_argv(&rc, script), &[]);

        let stdout = stdout_of(&out);
        let tail = match stdout.rfind('\u{07}') {
            Some(i) => stdout[i + 1..].to_string(),
            None => stdout.clone(),
        };
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(
            lines,
            vec!["function", "function", "function", "function", "function", "0"],
            "3.2 must define every compose helper and bind the trigger in NONE of its \
             keymaps. Got: <{tail}>"
        );

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("bind"),
            "sourcing the rc on stock 3.2 must emit no `bind` warning. Stderr: <{stderr}>"
        );
        assert!(
            !stderr.contains("5099"),
            "no trigger spelling may reach the terminal on 3.2. Stderr: <{stderr}>"
        );
    }

    // ---- Command Compose: real-pty e2e on a modern bash (work item 5) ------
    //
    // The one tier that needs a bash >= 4.3: `bind -x` on an 8-byte sequence is
    // the whole point, and readline only paints under a real pty. Found via
    // [`find_compose_bash`] (`NICE_TEST_BASH` -> homebrew -> PATH); a machine
    // with only stock 3.2 gets a printed skip, which is a PASS — the guard-closed
    // leg above is what covers that machine's actual behavior.

    /// Whatever the discovery returns really is a bash that can compose — the
    /// helper's contract, asserted without mutating the process environment (a
    /// racy thing to do under a threaded test runner).
    #[test]
    fn find_compose_bash_only_yields_a_bash_that_can_compose() {
        let Some(found) = find_compose_bash() else {
            return;
        };
        assert!(
            found.is_file(),
            "discovery returned a path that is not a file: {found:?}"
        );
        assert!(
            probe_version(&found.to_string_lossy()).expect("a discovered bash probes")
                >= COMPOSE_MIN_VERSION,
            "discovery must reject anything below {COMPOSE_MIN_VERSION:?}"
        );
    }

    /// Drain everything currently readable from a non-blocking pty master into
    /// `sink`. Returns `false` once the child's side is gone (EOF or `EIO`,
    /// which is how macOS reports the last slave fd closing).
    fn drain_pty(fd: std::os::unix::io::RawFd, sink: &mut Vec<u8>) -> bool {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe {
                libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n > 0 {
                sink.extend_from_slice(&buf[..n as usize]);
                continue;
            }
            if n == 0 {
                return false;
            }
            let err = std::io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(libc::EAGAIN) => true,
                Some(libc::EINTR) => continue,
                _ => false,
            };
        }
    }

    /// Poll the accumulated pty output until `pred` holds or `budget_ms`
    /// elapses. Time-based rather than a fixed sleep so a slow machine does not
    /// turn a passing compose into a flake.
    fn poll_pty(
        pty: &nice_term_core::PtyProcess,
        sink: &mut Vec<u8>,
        budget_ms: u64,
        pred: impl Fn(&str) -> bool,
    ) -> bool {
        let mut waited = 0;
        loop {
            drain_pty(pty.master_fd(), sink);
            if pred(&String::from_utf8_lossy(sink)) {
                return true;
            }
            if waited >= budget_ms {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited += 50;
        }
    }

    /// Full-visual e2e in a REAL pty on a REAL modern bash: type English, fire
    /// the trigger bytes Nice writes, and read the wire.
    ///
    /// Pins the fragile half of the design — the redisplay choreography. A
    /// `bind -x` handler that prints a status row and then clears its way back
    /// up has no API telling it what readline will redraw; the contract is
    /// "composed line rendered, no fragments of the status line or the old
    /// English left behind", and only a real terminal can show it. The Enter leg
    /// proves the redrawn text is genuine EDITING state (`READLINE_LINE`) rather
    /// than paint on the screen.
    ///
    /// **Driven through [`nice_term_core::PtyProcess`], not zsh's `zpty`** (the
    /// zsh spinner test's harness). `zpty` opens its pty with a 0×0 winsize —
    /// zsh only copies a size when the driving shell has a real tty, which a
    /// test runner does not — and readline paints NOTHING into a zero-width
    /// screen: with `zpty`, typed text never echoes and the post-handler
    /// redisplay emits no bytes, so the whole assertion set is vacuous. ZLE
    /// falls back to `$COLUMNS` and never noticed. `PtyProcess` opens the pty
    /// *with* the winsize, and is what production actually spawns panes
    /// through — the argv under test is `BashProfile::spawn_argv`'s own.
    #[test]
    fn bash_compose_replaces_the_line_in_a_real_pty_e2e() {
        use crate::shell::COMPOSE_TRIGGER_SEQ;
        use nice_term_core::{PtyProcess, SpawnSpec};

        let Some(bash) = find_compose_bash() else {
            eprintln!(
                "[skip] bash_compose_replaces_the_line_in_a_real_pty_e2e: no bash >= {}.{} \
                 (looked at ${TEST_BASH_ENV}, {}, {}, then PATH). The stock-3.2 legs still ran.",
                COMPOSE_MIN_VERSION.0,
                COMPOSE_MIN_VERSION.1,
                HOMEBREW_BASH_PATHS[0],
                HOMEBREW_BASH_PATHS[1],
            );
            return;
        };

        const REQUEST: &str = "list all files with details";

        let home = ScratchHome::new("nice-bash-compose-pty");
        home.write_path_restoring_bash_profile("");
        let rec = scratch("nice-bash-compose-pty-rec");
        std::fs::create_dir_all(&rec.0).expect("create record dir");
        // Thinks ~0.8s so the status line is captured mid-flight, then replies.
        home.install_executable(
            "claude",
            &format!(
                "#!/bin/bash\ncat > {rec}/stdin\nsleep 0.8\nprintf '%s' 'ls -la'\n",
                rec = rec.0.display()
            ),
        );
        let conf = home.write_file(
            "compose.json",
            r##"{"accent":"#c96442","model":"sonnet","effort":"medium"}"##,
        );
        let modern = BashProfile::new(bash.to_string_lossy().into_owned());
        let paths = modern
            .write_rc_files(&home.path().join("shellrc"))
            .expect("write nice.bashrc");

        let mut env = home.env();
        env.push((
            "NICE_COMPOSE_CONF".to_string(),
            conf.to_string_lossy().into_owned(),
        ));
        let spec = SpawnSpec::shell(home.path().to_string_lossy().into_owned())
            .with_argv(modern.spawn_argv(&SpawnCtx {
                inject: Some(&paths),
                command: None,
            }))
            .with_env(env)
            .with_size(24, 80);
        let pty = PtyProcess::spawn(&spec).expect("spawn the bash pane");
        // Non-blocking master: the drain loop polls rather than parking on a
        // shell that has nothing more to say.
        unsafe { libc::fcntl(pty.master_fd(), libc::F_SETFL, libc::O_NONBLOCK) };

        let mut raw: Vec<u8> = Vec::new();
        let dump = |t: &str| t.escape_debug().to_string();

        assert!(
            poll_pty(&pty, &mut raw, 8000, |t| t.contains('$')),
            "the injected bash never printed a prompt. Captured: <{}>",
            dump(&String::from_utf8_lossy(&raw))
        );
        pty.write_input(REQUEST.as_bytes()).expect("type the request");
        assert!(
            poll_pty(&pty, &mut raw, 4000, |t| t.contains(REQUEST)),
            "readline never echoed the typed request. Captured: <{}>",
            dump(&String::from_utf8_lossy(&raw))
        );

        // The exact bytes `dispatch_command_compose`'s `Trigger` route writes.
        pty.write_input(COMPOSE_TRIGGER_SEQ).expect("fire the trigger");
        let composed = poll_pty(&pty, &mut raw, 15000, |t| {
            t.contains("Composing") && t.rfind("ls -la").is_some()
        });
        let split = raw.len();
        let before = String::from_utf8_lossy(&raw[..split]).into_owned();
        assert!(
            composed,
            "the compose never completed. Captured: <{}>",
            dump(&before)
        );

        // Nothing ran: `ls -la` prints a `total` header, and it must not appear
        // until the Enter leg — asserted BEFORE Enter is sent so the two halves
        // of the capture can never be confused.
        assert!(
            !before.contains("total "),
            "the composed command EXECUTED without the user's Enter. Captured: <{}>",
            dump(&before)
        );

        pty.write_input(b"\r").expect("press Enter");
        let ran = poll_pty(&pty, &mut raw, 8000, |t| t.contains("total "));
        let after = String::from_utf8_lossy(&raw[split..]).into_owned();
        pty.teardown();

        assert!(
            before.contains("Composing"),
            "the status line never painted. Captured: <{}>",
            dump(&before)
        );
        // #c96442 -> SGR 38;2;201;100;66, the same truecolor bytes the zsh
        // spinner test asserts: one accent, one wire spelling, both shells.
        assert!(
            before.contains("38;2;201;100;66"),
            "the status line must be painted in the conf accent as truecolor. Captured: <{}>",
            dump(&before)
        );
        assert!(
            before.contains("ls -la"),
            "the composed command never landed in the redrawn line. Captured: <{}>",
            dump(&before)
        );
        // The trigger was consumed by the binding, not echoed as text — the
        // finding-F6 symptom, asserted on the wire.
        assert!(
            !before.contains("5099~"),
            "trigger fragments leaked into the line. Captured: <{}>",
            dump(&before)
        );
        // The redraw is CLEAN: everything after the handler's last erase is the
        // prompt plus the composed command, with no surviving fragment of the
        // status line or of the English that was typed.
        let redrawn = before
            .rsplit("\u{1b}[2K")
            .next()
            .expect("the cleanup erases before readline redraws");
        assert!(
            redrawn.contains("ls -la") && !redrawn.contains(REQUEST)
                && !redrawn.contains("Composing"),
            "the redrawn line must be the composed command alone. Redrawn: <{}> of <{}>",
            dump(redrawn),
            dump(&before)
        );
        assert_eq!(
            std::fs::read_to_string(rec.0.join("stdin")).expect("fake claude ran"),
            format!("{REQUEST}\n"),
            "the request reached claude on stdin, so the reply arrived by compose"
        );
        assert!(
            ran && after.contains("total "),
            "the user's Enter did not run the composed command — the redrawn line was \
             paint, not READLINE_LINE. Captured: <{}>",
            dump(&after)
        );
    }

    // ---- the frozen script contract (work item 8) --------------------------

    /// SHA-256 of `bytes` via the system `shasum`, so a reviewer's hand-run
    /// `shasum -a 256 nice.bashrc` cannot disagree with this test. Mirrors the
    /// zsh suite's helper of the same name deliberately — each script module
    /// owns its own freeze.
    fn sha256_hex(bytes: &[u8]) -> String {
        use std::io::Write;
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

    /// The script's contract froze when compose landed (design §10): from here
    /// on `nice.bashrc` is a compatibility surface against rc files already
    /// written to users' disks, so no byte of it may change by accident.
    ///
    /// A failure here means one of two things: the script text changed (if that
    /// was deliberate and reviewed, re-pin the digest in the SAME commit), or
    /// `include_str!` points at the wrong file. The structural tests above stay
    /// the readable "what the contract says" layer — this is only the "nothing
    /// changed silently" layer, and neither replaces the other.
    #[test]
    fn nice_bashrc_body_sha256_frozen() {
        assert_eq!(
            sha256_hex(NICE_BASHRC_BODY.as_bytes()),
            "823a1b33b10fa511a055c0d4accc9de31acad92c6078efa857cf99726ce9c935",
            "nice.bashrc bytes changed"
        );
    }

    /// What [`ShellProfile::write_rc_files`] puts on disk is the frozen body,
    /// byte for byte — the writer half of the freeze. A writer that mangled the
    /// script (a stray newline, a truncated atomic write) would break panes
    /// while the digest test above stayed green.
    #[test]
    fn nice_bashrc_writer_round_trips_the_frozen_bytes() {
        let dir = scratch("nice-bashrc-roundtrip");
        let rcfile = profile()
            .write_rc_files(&dir.0)
            .expect("write nice.bashrc")
            .rcfile
            .expect("bash always reports an rcfile");
        assert_eq!(
            std::fs::read_to_string(&rcfile).expect("read the written rc"),
            NICE_BASHRC_BODY,
            "the rc on disk must equal the frozen const"
        );
    }
}
