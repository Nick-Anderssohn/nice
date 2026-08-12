//! Spawn spec + the pure projections that feed the pty layer: the argv handed
//! to `execve`, tilde expansion of the cwd, and the injected environment. Ports
//! the PROTECTED spawn contract from
//! `Sources/Nice/Process/TabPtySession.swift` (`buildExecArgs`, `buildEnv`,
//! `expandTilde`) — behavior, not structure.
//!
//! This crate is shell-policy-free (`docs/design/shell-abstraction.md` §8): a
//! spec carries a **prebuilt argv** and the pty layer execs it verbatim. The
//! convenience constructors default that argv to the historical zsh login shell
//! so hermetic term-core tests, itest fixtures, and scenarios keep spawning
//! exactly what they always did; production Nice overrides it via
//! [`SpawnSpec::with_argv`] from the active shell profile.

/// The login shell the default argv runs. Retained as the shell-agnostic
/// crate's *default* (not its policy): the `nice` crate's shell profiles decide
/// what production panes exec, and every caller that has not been routed through
/// a profile — hermetic tests, itest fixtures — still gets this.
pub const ZSH_PATH: &str = "/bin/zsh";

/// How a pane's child process is launched.
///
/// - `command == None` → a plain **login + interactive** shell (zsh `-il` by
///   default): the user's PATH/rc are honored and the pane renders a prompt.
/// - `command == Some(cmd)` → the login shell replaces itself with `cmd` via
///   `zsh -ilc "exec <cmd>"`, so rc files still run first (PATH parity) but
///   quitting the command closes the pty (matching the pane lifecycle) and
///   signals/resize forward to the command, not an intermediate shell.
///
/// The `cwd` is tilde-expanded before spawn; the `command` string itself is
/// **never** tilde-expanded (`TabPtySession.swift:1007-1013`). `env` is the
/// caller-supplied injection, applied on top of the base terminal env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    /// `None` = shell-only (`-il`); `Some(cmd)` = `-ilc "exec <cmd>"`.
    ///
    /// Display/launch-overlay source of truth. It does NOT drive the exec —
    /// [`argv`](Self::argv) does. A caller that overrides the argv must build it
    /// from this same string (see [`with_argv`](Self::with_argv)).
    pub command: Option<String>,
    /// Working directory. Tilde-expanded (`~`, `~/…`) at spawn time.
    pub cwd: String,
    /// Caller-injected env pairs, applied over the base terminal env. A pair
    /// whose key already exists in the base overrides it.
    pub env: Vec<(String, String)>,
    /// Initial pty height in character cells.
    pub rows: u16,
    /// Initial pty width in character cells.
    pub cols: u16,
    /// The full argv (argv\[0\] = the program) `execve`d for this spec — the
    /// exec truth. Defaults to [`build_argv`] over
    /// [`command`](Self::command) (the zsh login shape); production Nice
    /// replaces it with the active shell profile's argv via
    /// [`with_argv`](Self::with_argv). Must never be empty.
    pub argv: Vec<String>,
}

/// Parity default terminal size (SwiftTerm's classic 80x24). Callers that care
/// about size set it explicitly; perf/memory validations override.
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;

impl SpawnSpec {
    /// A shell-only pane rooted at `cwd`. The default argv is a zsh login shell
    /// (`/bin/zsh -il`); production Nice overrides it via
    /// [`with_argv`](Self::with_argv) from the active shell profile.
    pub fn shell(cwd: impl Into<String>) -> Self {
        Self {
            command: None,
            cwd: cwd.into(),
            env: Vec::new(),
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            argv: build_argv(None),
        }
    }

    /// A command pane rooted at `cwd`. The default argv is a zsh login shell
    /// running the command (`/bin/zsh -ilc "exec <command>"`); production Nice
    /// overrides it via [`with_argv`](Self::with_argv).
    pub fn command(command: impl Into<String>, cwd: impl Into<String>) -> Self {
        let command = command.into();
        let argv = build_argv(Some(&command));
        Self {
            command: Some(command),
            cwd: cwd.into(),
            env: Vec::new(),
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            argv,
        }
    }

    /// Set the injected env pairs (builder style).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Replace the argv `execve`s for this spec (builder style) — how the `nice`
    /// crate hands term-core a shell profile's argv without term-core knowing
    /// any shell's dialect.
    ///
    /// **Invariant:** build `argv` from the spec's own
    /// [`command`](Self::command). The `command` stays the display/launch-overlay
    /// source of truth while the argv is the exec truth, and a pane whose overlay
    /// advertises one command while its child runs another is a bug with no
    /// visible symptom until the pane exits.
    ///
    /// An empty `argv` is rejected at spawn time
    /// ([`PtyProcess::spawn`](crate::PtyProcess::spawn) returns `InvalidInput`),
    /// never at exec.
    pub fn with_argv(mut self, argv: Vec<String>) -> Self {
        self.argv = argv;
        self
    }

    /// Set the initial pty size (builder style).
    pub fn with_size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }
}

/// Project an optional command to the arg list passed to `/bin/zsh` (argv[0]
/// excluded). `None` → `["-il"]` (login-interactive shell); `Some(cmd)` →
/// `["-ilc", "exec <cmd>"]`.
///
/// The `exec` form is load-bearing: it replaces the login shell with the
/// command so quitting the command closes the pty, and signals/resize forward
/// to the command instead of being eaten by an intermediate shell. Port of
/// `TabPtySession.buildExecArgs`; the command is spliced verbatim (no tilde
/// expansion, no re-quoting — callers pre-quote via [`crate::shell_single_quote`]).
pub fn build_exec_args(command: Option<&str>) -> Vec<String> {
    match command {
        Some(cmd) => vec!["-ilc".to_string(), format!("exec {cmd}")],
        None => vec!["-il".to_string()],
    }
}

/// The DEFAULT argv for a spec: `[ZSH_PATH, <exec args…>]`. argv[0] is the plain
/// executable path (no leading-dash login convention — login-ness comes from the
/// `-l` flag, matching `execName: nil` in `TabPtySession.addTerminalPane`).
///
/// This is what [`SpawnSpec::shell`] / [`SpawnSpec::command`] store on the spec;
/// [`SpawnSpec::with_argv`] replaces it for callers that resolve their own shell.
pub fn build_argv(command: Option<&str>) -> Vec<String> {
    let mut argv = Vec::with_capacity(3);
    argv.push(ZSH_PATH.to_string());
    argv.extend(build_exec_args(command));
    argv
}

/// Expand a leading `~` to `$HOME` so the spawn's working directory resolves
/// cleanly. Paths without a leading `~` pass through unchanged. Port of
/// `TabPtySession.expandTilde`.
pub fn expand_tilde(path: &str) -> String {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        // No HOME → cannot expand; hand the path through untouched.
        Err(_) => return path.to_string(),
    };
    if path == "~" {
        return home;
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// The base terminal env, mirroring `SwiftTerm.Terminal.getEnvironmentVariables`
/// (the default `TabPtySession.buildEnv` forwards on top of): a fixed
/// `TERM`/`COLORTERM`/`LANG` plus a curated pass-through of identity vars from
/// the parent env. PATH is deliberately **not** forwarded — the login shell
/// rebuilds it from the user's rc files, which is the whole point of `-l`.
fn base_env() -> Vec<(String, String)> {
    let mut env = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
        // Without this, tools like vi produce non-UTF-8-friendly sequences.
        ("LANG".to_string(), "en_US.UTF-8".to_string()),
    ];
    // Curated pass-through set from SwiftTerm's getEnvironmentVariables.
    for key in ["LOGNAME", "USER", "DISPLAY", "LC_TYPE", "HOME"] {
        if let Ok(val) = std::env::var(key) {
            upsert(&mut env, key, val);
        }
    }
    env
}

/// The final env for a spawn: [`base_env`] with the caller's injected pairs
/// applied on top (a caller key that already exists overrides the base value;
/// a new key is appended). Insertion order is stable; keys are unique so the
/// child's `getenv` is unambiguous.
pub fn build_env(extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = base_env();
    for (k, v) in extra {
        upsert(&mut env, k, v.clone());
    }
    env
}

/// Insert `key=value`, replacing any existing entry for `key` in place so the
/// result never carries duplicate keys.
fn upsert(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        env.push((key.to_string(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_only_touches_leading_tilde() {
        // SAFETY: single-threaded test; we restore HOME before returning.
        let saved = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", "/Users/tester") };

        assert_eq!(expand_tilde("~"), "/Users/tester");
        assert_eq!(expand_tilde("~/foo/bar"), "/Users/tester/foo/bar");
        // A mid-string tilde is not a home reference — untouched.
        assert_eq!(expand_tilde("/tmp/~/x"), "/tmp/~/x");
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        // `~user` is not expanded (only `~` and `~/…`), matching the Swift port.
        assert_eq!(expand_tilde("~other/x"), "~other/x");

        match saved {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn command_is_never_tilde_expanded() {
        // The cwd is tilde-expanded; the command string is spliced verbatim.
        // A `~` inside the command must survive into the exec arg untouched so
        // the login shell (not us) decides whether to expand it.
        let args = build_exec_args(Some("vim ~/notes.md"));
        assert_eq!(args, vec!["-ilc".to_string(), "exec vim ~/notes.md".to_string()]);
    }

    #[test]
    fn build_env_caller_overrides_base_and_dedupes() {
        let extra = vec![
            ("NICE_TEST_VAR".to_string(), "abc".to_string()),
            ("TERM".to_string(), "dumb".to_string()),
        ];
        let env = build_env(&extra);
        // Caller override wins and there is exactly one TERM entry.
        let terms: Vec<&String> = env
            .iter()
            .filter(|(k, _)| k == "TERM")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(terms, vec![&"dumb".to_string()]);
        // Injected key is present.
        assert!(env.iter().any(|(k, v)| k == "NICE_TEST_VAR" && v == "abc"));
        // No duplicate keys anywhere.
        let mut keys: Vec<&String> = env.iter().map(|(k, _)| k).collect();
        let before = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), before, "env carries duplicate keys");
    }
}
