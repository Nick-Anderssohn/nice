//! [`FallbackProfile`] — the unknown-shell degrade (design §5).
//!
//! For any resolved shell Nice has no dedicated profile for (fish, tcsh, dash,
//! and — until migration step 3 — bash): degrade to a working plain terminal.
//! Never block, never garble. No rc injection, no `claude()` shadow, no
//! Command Compose, no prefill — the pane runs the user's actual shell with
//! their actual config and PATH, minus Nice's integration features.
//!
//! Spawn argv uses separate short flags (`-i`, `-l`, `-c`), never the clustered
//! `-ilc` spelling zsh/bash accept: clustered short-option parsing is not
//! universal, but `-i` `-l` `-c` as distinct arguments is accepted by fish,
//! tcsh, and every POSIX-ish shell this profile might resolve to.
//!
//! Known, accepted wrinkle: tcsh takes `-l` only when it is the *sole*
//! argument, so a tcsh pane is really a non-login tcsh. Harmless in practice —
//! tcsh reads `~/.tcshrc` for interactive shells either way, so PATH still
//! resolves — and not worth a per-shell argv special case in a profile whose
//! whole job is to degrade politely. fish honors `-l` normally.

use std::io;
use std::path::Path;

use super::{
    ComposeSupport, InjectPaths, PrefillStrategy, ShellKind, ShellProfile, SpawnCtx, UserShellEnv,
};

/// `MAXCOMLEN` (BSD/Darwin) is 16 bytes including the NUL terminator, so a
/// kernel `comm` string tops out at 15 visible bytes.
const MAX_COMM_BYTES: usize = 15;

/// Truncate `s` to at most `max` bytes, backing off to the nearest earlier
/// UTF-8 character boundary so the result is always valid `str`.
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// A shell Nice has no profile for, at the resolved `path`. Carries the
/// basename twice: once truncated for [`ShellProfile::comm_name`] (the kernel
/// `comm` string), once whole for [`ShellProfile::display_name`].
pub struct FallbackProfile {
    path: String,
    comm_name: String,
    display_name: String,
}

impl FallbackProfile {
    pub fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        let basename = Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path.as_str())
            .to_string();
        let comm_name = truncate_bytes(&basename, MAX_COMM_BYTES);
        Self {
            path,
            comm_name,
            display_name: basename,
        }
    }
}

impl ShellProfile for FallbackProfile {
    fn kind(&self) -> ShellKind {
        ShellKind::Other
    }

    fn program(&self) -> &str {
        &self.path
    }

    /// `[path, "-i", "-l"]` / `[path, "-i", "-l", "-c", "exec <cmd>"]` — see
    /// the module doc for why these stay separate flags.
    fn spawn_argv(&self, ctx: &SpawnCtx) -> Vec<String> {
        let mut argv = vec![self.path.clone(), "-i".to_string(), "-l".to_string()];
        if let Some(cmd) = ctx.command {
            argv.push("-c".to_string());
            argv.push(format!("exec {cmd}"));
        }
        argv
    }

    /// No env-based injection channel for an unknown shell.
    fn inject_env(&self, _inject: &InjectPaths, _user: &UserShellEnv) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Writes nothing — there is no rc dialect to write for an unknown shell.
    /// Returns the directory unchanged so callers still have somewhere to
    /// point `InjectPaths::dir`, matching the design §5 table.
    fn write_rc_files(&self, dir: &Path) -> io::Result<InjectPaths> {
        Ok(InjectPaths {
            dir: dir.to_path_buf(),
            rcfile: None,
        })
    }

    /// No shell-side binding exists — never send Compose trigger bytes.
    fn compose_support(&self) -> ComposeSupport {
        ComposeSupport::None
    }

    /// No prefill mechanism — the deferred-resume pane opens at a bare prompt.
    fn prefill(&self) -> PrefillStrategy {
        PrefillStrategy::Off
    }

    /// `[path, "-i", "-l", "-c", probe_cmd]` — `command -v` is POSIX and
    /// fish/tcsh support it.
    fn probe_argv(&self, probe_cmd: &str) -> Vec<String> {
        vec![
            self.path.clone(),
            "-i".to_string(),
            "-l".to_string(),
            "-c".to_string(),
            probe_cmd.to_string(),
        ]
    }

    fn comm_name(&self) -> &str {
        &self.comm_name
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_pane_argv_is_separate_flags() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        let ctx = SpawnCtx {
            inject: None,
            command: None,
        };
        assert_eq!(
            profile.spawn_argv(&ctx),
            vec!["/usr/local/bin/fish", "-i", "-l"]
        );
    }

    #[test]
    fn command_pane_argv_execs_and_stays_separate_flags() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        let ctx = SpawnCtx {
            inject: None,
            command: Some("vim"),
        };
        assert_eq!(
            profile.spawn_argv(&ctx),
            vec!["/usr/local/bin/fish", "-i", "-l", "-c", "exec vim"]
        );
    }

    /// `inject` is ignored — the fallback never injects, on either axis.
    #[test]
    fn spawn_argv_ignores_the_inject_axis() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        let paths = InjectPaths {
            dir: "/tmp/whatever".into(),
            rcfile: None,
        };
        let with_inject = SpawnCtx {
            inject: Some(&paths),
            command: None,
        };
        let without_inject = SpawnCtx {
            inject: None,
            command: None,
        };
        assert_eq!(
            profile.spawn_argv(&with_inject),
            profile.spawn_argv(&without_inject)
        );
    }

    #[test]
    fn inject_env_is_always_empty() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        let paths = InjectPaths {
            dir: "/tmp/whatever".into(),
            rcfile: None,
        };
        let user = UserShellEnv {
            user_zdotdir: Some("/user/z".to_string()),
        };
        assert_eq!(profile.inject_env(&paths, &user), Vec::new());
    }

    #[test]
    fn write_rc_files_touches_nothing_and_leaves_the_dir_empty() {
        let dir = std::env::temp_dir().join(format!(
            "nice-fallback-write-rc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!dir.exists());
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        let paths = profile.write_rc_files(&dir).unwrap();
        assert_eq!(paths.dir, dir);
        assert_eq!(paths.rcfile, None);
        assert!(!dir.exists(), "write_rc_files must not touch the filesystem");
    }

    #[test]
    fn compose_and_prefill_are_off() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        assert_eq!(profile.compose_support(), ComposeSupport::None);
        assert_eq!(profile.prefill(), PrefillStrategy::Off);
    }

    #[test]
    fn probe_argv_is_separate_flags_with_the_probe_command() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        assert_eq!(
            profile.probe_argv("command -v -- claude"),
            vec![
                "/usr/local/bin/fish",
                "-i",
                "-l",
                "-c",
                "command -v -- claude"
            ]
        );
    }

    #[test]
    fn comm_and_display_name_are_the_basename() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        assert_eq!(profile.comm_name(), "fish");
        assert_eq!(profile.display_name(), "fish");
    }

    #[test]
    fn comm_name_truncates_to_fifteen_bytes_display_name_does_not() {
        // 20-byte basename; comm_name must be at most 15 bytes.
        let long_name = "abcdefghijklmnopqrst";
        assert_eq!(long_name.len(), 20);
        let profile = FallbackProfile::new(format!("/opt/weird/{long_name}"));
        assert_eq!(profile.comm_name(), &long_name[..15]);
        assert_eq!(profile.comm_name().len(), 15);
        assert_eq!(profile.display_name(), long_name);
    }

    #[test]
    fn program_returns_the_resolved_path() {
        let profile = FallbackProfile::new("/opt/homebrew/bin/fish");
        assert_eq!(profile.program(), "/opt/homebrew/bin/fish");
    }

    #[test]
    fn kind_is_other() {
        let profile = FallbackProfile::new("/usr/local/bin/fish");
        assert_eq!(profile.kind(), ShellKind::Other);
    }

    /// Real-shell smoke: the separate-flag argv this profile builds must be
    /// accepted by an actual non-zsh shell, and the command must own the pty and
    /// close the pane when it exits. `/bin/bash` is on every macOS, so this runs
    /// unconditionally — it is the cheapest available proof that
    /// `-i -l -c "exec …"` is a real spawn shape and not just a table entry.
    /// (Bash's own integration lands in migration step 3; this only pins argv.)
    #[test]
    fn separate_flag_argv_runs_a_command_under_real_bash() {
        use nice_term_core::{PtyProcess, SpawnSpec};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let marker = format!("NICE_FALLBACKOK_{}", std::process::id());
        let command = format!("echo {marker}");
        let argv = FallbackProfile::new("/bin/bash").spawn_argv(&SpawnCtx {
            inject: None,
            command: Some(&command),
        });
        assert_eq!(
            argv,
            vec![
                "/bin/bash",
                "-i",
                "-l",
                "-c",
                &format!("exec {command}")
            ]
        );

        // A `$HOME` with no dotfiles: the login chain finds nothing to source, so
        // the shell under test stays a real login+interactive bash without
        // dragging in whatever this machine's bash profile does.
        let home = std::env::temp_dir().join(format!("nice-fallback-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("create scratch HOME");
        let spec = SpawnSpec::command(command.clone(), "/tmp")
            .with_env(vec![(
                "HOME".to_string(),
                home.to_string_lossy().into_owned(),
            )])
            .with_argv(argv);
        let pty = PtyProcess::spawn(&spec).expect("spawn a real bash pane");

        // Drain the master continuously — an undrained pty buffer blocks the
        // child's writes and it never becomes reapable.
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let fd = pty.master_fd();
        let drain = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n > 0 {
                    sink.lock().unwrap().extend_from_slice(&buf[..n as usize]);
                } else if n == 0 {
                    break;
                } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
        });

        let status = pty
            .wait_timeout(Duration::from_secs(20))
            .expect("the bash pane did not exit within the timeout");
        let _ = drain.join();
        let text = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(status.code(), Some(0), "clean exit; got {status:?}, output: {text:?}");
        assert!(
            text.contains(&marker),
            "the command never ran under `-i -l -c`; got: {text:?}"
        );
    }
}
