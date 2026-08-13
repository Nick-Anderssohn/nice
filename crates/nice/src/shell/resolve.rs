//! Which shell Nice runs — the resolution point (design §4).
//!
//! The chain is `NICE_SHELL` → `advanced.shell` → `$SHELL` →
//! `getpwuid(getuid()).pw_shell` → `/bin/zsh`. Structured as a pure precedence
//! walk ([`resolve_path`]) over injectable inputs ([`ResolveInputs`]) so every
//! hop is table-testable without touching the environment or the passwd
//! database, plus a thin live wrapper ([`resolve`]) that reads the real env,
//! `$SHELL`, and `getpwuid`, and maps the winning path to a profile by
//! executable basename.

use std::ffi::CStr;
use std::path::Path;

use super::bash::BashProfile;
use super::fallback::FallbackProfile;
use super::zsh::ZshProfile;
use super::ShellProfile;

/// The persisted user choice (`advanced.shell`). `Automatic` is the default and
/// means "walk the rest of the chain".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellSetting {
    Automatic,
    Path(String),
}

/// Injectable inputs to [`resolve_path`] — every source the §4 chain consults,
/// plus the usability probe, so the precedence walk is pure and table-testable.
pub struct ResolveInputs {
    /// `$NICE_SHELL` — the dev/test override seam.
    pub nice_shell: Option<String>,
    /// `advanced.shell` from `ui_settings.json`.
    pub setting: ShellSetting,
    /// `$SHELL` — Nice's inherited env.
    pub env_shell: Option<String>,
    /// `getpwuid(getuid()).pw_shell` — the account's login shell.
    pub pwuid_shell: Option<String>,
    /// Exists + executable probe (injectable so tests never touch the real
    /// filesystem).
    pub is_usable: fn(&str) -> bool,
}

/// A candidate wins a hop only if it is non-empty, absolute, and usable — every
/// hop is checked the same way, including `NICE_SHELL` (design §4: "Absolute
/// path expected; non-absolute or empty ⇒ ignored").
fn candidate_ok(path: &str, is_usable: fn(&str) -> bool) -> bool {
    !path.is_empty() && Path::new(path).is_absolute() && is_usable(path)
}

/// The pure precedence walk (design §4, steps 1-5). Each hop that fails
/// [`candidate_ok`] falls through to the next; `/bin/zsh` is the floor, so this
/// always returns a usable-looking absolute path.
fn resolve_path(inputs: &ResolveInputs) -> String {
    if let Some(p) = &inputs.nice_shell {
        if candidate_ok(p, inputs.is_usable) {
            return p.clone();
        }
    }
    if let ShellSetting::Path(p) = &inputs.setting {
        if candidate_ok(p, inputs.is_usable) {
            return p.clone();
        }
    }
    if let Some(p) = &inputs.env_shell {
        if candidate_ok(p, inputs.is_usable) {
            return p.clone();
        }
    }
    if let Some(p) = &inputs.pwuid_shell {
        if candidate_ok(p, inputs.is_usable) {
            return p.clone();
        }
    }
    "/bin/zsh".to_string()
}

/// The live `is_usable`: exists, is a regular file, and has any execute bit
/// set. No `dscl`, no subprocess.
fn is_usable_live(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// The account's login shell via `getpwuid(getuid())` — copies the C string to
/// an owned `String` immediately (the struct's `pw_shell` points into a static
/// buffer libc may reuse on the next passwd-database call). Called once at
/// bootstrap, on the main thread.
fn pwuid_shell_live() -> Option<String> {
    // SAFETY: `getpwuid` returns either null or a pointer to a `libc::passwd`
    // valid until the next passwd-database call on this thread; we read
    // `pw_shell` and copy it to an owned `String` before returning, so nothing
    // borrows the static buffer past this call.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        let shell_ptr = (*pw).pw_shell;
        if shell_ptr.is_null() {
            return None;
        }
        CStr::from_ptr(shell_ptr).to_str().ok().map(str::to_string)
    }
}

/// Map the winning path to a profile by executable basename (design §4): `zsh`
/// ⇒ [`ZshProfile`], `bash` ⇒ [`BashProfile`], anything else ⇒
/// [`FallbackProfile`]. The resolved path is kept as-resolved in every arm — a
/// homebrew `/opt/homebrew/bin/bash` gets the bash profile *at that path*, not
/// `/bin/bash`, so its own rc/probe/spawn argv all name the binary the user
/// actually runs.
fn profile_for_path(path: String) -> Box<dyn ShellProfile> {
    let basename = Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path.as_str());
    match basename {
        "zsh" => Box::new(ZshProfile::new(path)),
        "bash" => Box::new(BashProfile::new(path)),
        _ => Box::new(FallbackProfile::new(path)),
    }
}

/// Resolve the active profile. Infallible: worst case a [`ZshProfile`] at
/// `/bin/zsh`.
pub fn resolve(setting: &ShellSetting) -> Box<dyn ShellProfile> {
    let inputs = ResolveInputs {
        nice_shell: std::env::var("NICE_SHELL").ok(),
        setting: setting.clone(),
        env_shell: std::env::var("SHELL").ok(),
        pwuid_shell: pwuid_shell_live(),
        is_usable: is_usable_live,
    };
    profile_for_path(resolve_path(&inputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::ShellKind;

    // `is_usable` is a plain `fn` pointer (no captures), per the contract, so
    // each precedence fixture is a top-level-shaped `fn` item rather than a
    // closure.
    fn always_usable(_p: &str) -> bool {
        true
    }
    fn never_usable(_p: &str) -> bool {
        false
    }
    fn usable_except_setting(p: &str) -> bool {
        p != "/opt/bad/setting-shell"
    }
    fn usable_except_env(p: &str) -> bool {
        p != "/opt/bad/env-shell"
    }
    fn usable_except_nice_shell(p: &str) -> bool {
        p != "/opt/bad/nice-shell"
    }

    fn inputs(
        nice_shell: Option<&str>,
        setting: ShellSetting,
        env_shell: Option<&str>,
        pwuid_shell: Option<&str>,
        is_usable: fn(&str) -> bool,
    ) -> ResolveInputs {
        ResolveInputs {
            nice_shell: nice_shell.map(str::to_string),
            setting,
            env_shell: env_shell.map(str::to_string),
            pwuid_shell: pwuid_shell.map(str::to_string),
            is_usable,
        }
    }

    #[test]
    fn nice_shell_wins_over_everything() {
        let got = resolve_path(&inputs(
            Some("/opt/nice/shell"),
            ShellSetting::Path("/opt/setting/shell".to_string()),
            Some("/opt/env/shell"),
            Some("/opt/pwuid/shell"),
            always_usable,
        ));
        assert_eq!(got, "/opt/nice/shell");
    }

    #[test]
    fn nice_shell_non_absolute_is_ignored() {
        let got = resolve_path(&inputs(
            Some("relative/shell"),
            ShellSetting::Path("/opt/setting/shell".to_string()),
            None,
            None,
            always_usable,
        ));
        assert_eq!(got, "/opt/setting/shell");
    }

    #[test]
    fn nice_shell_empty_is_ignored() {
        let got = resolve_path(&inputs(
            Some(""),
            ShellSetting::Path("/opt/setting/shell".to_string()),
            None,
            None,
            always_usable,
        ));
        assert_eq!(got, "/opt/setting/shell");
    }

    #[test]
    fn nice_shell_unusable_falls_through() {
        let got = resolve_path(&inputs(
            Some("/opt/bad/nice-shell"),
            ShellSetting::Path("/opt/setting/shell".to_string()),
            None,
            None,
            usable_except_nice_shell,
        ));
        assert_eq!(got, "/opt/setting/shell");
    }

    #[test]
    fn setting_wins_over_env_and_pwuid() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Path("/opt/setting/shell".to_string()),
            Some("/opt/env/shell"),
            Some("/opt/pwuid/shell"),
            always_usable,
        ));
        assert_eq!(got, "/opt/setting/shell");
    }

    #[test]
    fn automatic_setting_falls_through_to_env() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Automatic,
            Some("/opt/env/shell"),
            Some("/opt/pwuid/shell"),
            always_usable,
        ));
        assert_eq!(got, "/opt/env/shell");
    }

    #[test]
    fn setting_unusable_falls_through_to_env() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Path("/opt/bad/setting-shell".to_string()),
            Some("/opt/env/shell"),
            Some("/opt/pwuid/shell"),
            usable_except_setting,
        ));
        assert_eq!(got, "/opt/env/shell");
    }

    #[test]
    fn env_shell_wins_over_pwuid() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Automatic,
            Some("/opt/env/shell"),
            Some("/opt/pwuid/shell"),
            always_usable,
        ));
        assert_eq!(got, "/opt/env/shell");
    }

    #[test]
    fn env_shell_unusable_falls_through_to_pwuid() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Automatic,
            Some("/opt/bad/env-shell"),
            Some("/opt/pwuid/shell"),
            usable_except_env,
        ));
        assert_eq!(got, "/opt/pwuid/shell");
    }

    #[test]
    fn pwuid_shell_used_when_nothing_else_present() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Automatic,
            None,
            Some("/opt/pwuid/shell"),
            always_usable,
        ));
        assert_eq!(got, "/opt/pwuid/shell");
    }

    #[test]
    fn floor_is_bin_zsh_when_nothing_is_usable() {
        let got = resolve_path(&inputs(
            Some("/opt/nice/shell"),
            ShellSetting::Path("/opt/setting/shell".to_string()),
            Some("/opt/env/shell"),
            Some("/opt/pwuid/shell"),
            never_usable,
        ));
        assert_eq!(got, "/bin/zsh");
    }

    #[test]
    fn floor_is_bin_zsh_when_nothing_present_at_all() {
        let got = resolve_path(&inputs(
            None,
            ShellSetting::Automatic,
            None,
            None,
            always_usable,
        ));
        assert_eq!(got, "/bin/zsh");
    }

    #[test]
    fn basename_mapping_zsh_keeps_resolved_path() {
        let profile = profile_for_path("/opt/homebrew/bin/zsh".to_string());
        assert_eq!(profile.kind(), ShellKind::Zsh);
        assert_eq!(profile.program(), "/opt/homebrew/bin/zsh");
    }

    /// Basename `bash` now resolves to the real bash profile — at `/bin/bash`
    /// and at a homebrew path alike, each keeping its own binary — rather than
    /// falling through to the fallback.
    #[test]
    fn basename_mapping_bash_keeps_resolved_path() {
        for path in ["/bin/bash", "/opt/homebrew/bin/bash"] {
            let profile = profile_for_path(path.to_string());
            assert_eq!(profile.kind(), ShellKind::Bash, "{path}");
            assert_eq!(profile.program(), path);
            assert_eq!(profile.comm_name(), "bash", "{path}");
        }
    }

    #[test]
    fn basename_mapping_unknown_shell_is_fallback() {
        let profile = profile_for_path("/opt/homebrew/bin/fish".to_string());
        assert_eq!(profile.kind(), ShellKind::Other);
        assert_eq!(profile.program(), "/opt/homebrew/bin/fish");
    }

    #[test]
    fn resolve_is_infallible_end_to_end() {
        // The live wrapper always returns SOME profile; on a normal dev machine
        // this resolves to whatever `$SHELL` / pwuid says, but it must never
        // panic regardless of the account's configuration.
        let _ = resolve(&ShellSetting::Automatic);
    }
}
