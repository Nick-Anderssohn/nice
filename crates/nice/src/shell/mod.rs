//! The shell abstraction — one trait, one resolution point, one per-pane
//! snapshot (`docs/design/shell-abstraction.md`).
//!
//! Nice used to hardcode `/bin/zsh` at the spawn choke point and ride a zsh-only
//! delivery channel (the synthetic `ZDOTDIR` rc chain) for every shell-integration
//! feature: the `claude()` shadow, Command Compose, OSC 7 cwd reporting, deferred
//! resume prefill. [`ShellProfile`] is the seam that ends that: everything
//! shell-dialect-shaped — spawn argv, injection strategy, rc-file bodies, probe
//! argv, kernel comm name, display copy — lives behind it, one implementation per
//! shell ([`zsh::ZshProfile`] today).
//!
//! Migration state (design §9): **steps 1 and 2 are complete.** Step 1 was the
//! pure extraction — the trait and `ZshProfile`, byte-frozen against the old
//! inline `shell_inject` module, with every call site routed through the
//! profile. Step 2 turned the abstraction on: [`resolve`](resolve::resolve) runs
//! the real §4 chain (`NICE_SHELL` → `advanced.shell` → `$SHELL` → `getpwuid` →
//! `/bin/zsh`) and maps an unrecognized basename to
//! [`fallback::FallbackProfile`], panes carry a [`PaneShell`] snapshot that
//! gates Command Compose, the orphan reaper prefilters on the comm-name union
//! ([`reaper_comm_names`]), rc files live under `shellrc/<shell>/`, and the
//! deferred-resume prefill var is gated on the profile's
//! [`PrefillStrategy`].
//!
//! Next (design §9 step 3): `BashProfile` — `--rcfile` spawn shapes and a
//! `nice.bashrc`, which also adds `"bash"` to [`all_known_comm_names`].

// Carried over from the `shell_inject` module this replaces, for the same
// reason and one more. The trait surface is a CONTRACT (design §2) implemented
// in full by every profile, but its consumers arrive across migration steps
// 1–6 — and some items never get a production consumer at all
// (`COMPOSE_TRIGGER_BINDKEY` is the shell-side spelling, asserted only by the
// script-pinning tests). Warning on the gap would train the eye to ignore
// dead-code warnings during exactly the migration where they are noise.
#![allow(dead_code)]

pub mod fallback;
pub mod resolve;
pub mod zsh;

use std::io;
use std::path::{Path, PathBuf};

/// The private pty trigger for Command Compose. Nice's `commandCompose` action
/// handler writes exactly these bytes to the window's pty (only when the shell
/// sits at an idle interactive prompt, and only when the pane's shell bound
/// them); the zsh `.zshrc` widget binds exactly this sequence. CSI 5099~
/// (DECFNK style): no terminal emits tilde-coded numbers anywhere near 5099 from
/// real keys (the real ones top out in the 30s), and it is deliberately far from
/// bracketed paste's 200/201. Written in a single `write_input` call so ZLE's
/// keymap trie matches it atomically against the `\e[5~` (PageUp) shared prefix.
///
/// Shared across shells on purpose: every profile that supports compose binds
/// these same bytes, so the app side never branches on dialect.
pub const COMPOSE_TRIGGER_SEQ: &[u8] = b"\x1b[5099~";

/// The shell-side `bindkey` spelling of [`COMPOSE_TRIGGER_SEQ`] (`\e` = ESC).
/// The static tests pin that the zsh `.zshrc` binds exactly this string in all
/// three keymaps, and that its bytes agree with the Rust constant.
pub const COMPOSE_TRIGGER_BINDKEY: &str = r"\e[5099~";

/// Which shell family a pane runs. Copy key for per-pane storage and match
/// sites — it is a key, not a dispatcher (design §3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    Zsh,
    Bash,
    /// Resolved to a shell Nice has no profile for (fish, tcsh, dash, …).
    /// Plain shell, all integration features off. Carries no path — the
    /// fallback profile instance holds the resolved path.
    Other,
}

/// Paths produced by writing a profile's rc files: what
/// [`ShellProfile::spawn_argv`] / [`ShellProfile::inject_env`] need to
/// reference them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectPaths {
    /// Directory holding the rc files (zsh: the synthetic `ZDOTDIR`;
    /// bash: the directory containing `nice.bashrc`).
    pub dir: PathBuf,
    /// The file argv must point at, for argv-injected shells (bash `--rcfile`).
    /// `None` for env-injected shells (zsh) and the fallback.
    pub rcfile: Option<PathBuf>,
}

/// User-side env captured at bootstrap, before any pty inherits Nice's
/// overrides. Extend per-shell as needed; zsh uses `user_zdotdir`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserShellEnv {
    /// Nice's own inherited `ZDOTDIR` (XDG-style zsh layouts).
    pub user_zdotdir: Option<String>,
}

/// How a pane's child is launched (mirrors today's `SpawnSpec` split).
pub struct SpawnCtx<'a> {
    /// `Some` ⇒ spawn WITH Nice's rc injection (normal panes, deferred-resume
    /// Claude panes). `None` ⇒ spawn the user's genuine login shell with no
    /// injection (non-deferred Claude windows, hermetic tests).
    pub inject: Option<&'a InjectPaths>,
    /// `None` ⇒ interactive shell pane. `Some(cmd)` ⇒ command pane: the shell
    /// must end up running `exec <cmd>` after rc files (PATH parity), so the
    /// command owns the pty and its exit closes the pane.
    pub command: Option<&'a str>,
}

/// Whether Nice may write [`COMPOSE_TRIGGER_SEQ`] to an idle prompt of this
/// shell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComposeSupport {
    /// The rc files bind [`COMPOSE_TRIGGER_SEQ`] to a compose implementation
    /// (zsh ZLE widget; bash ≥ 4.3 `bind -x`). Trigger bytes are safe to send.
    Trigger,
    /// No shell-side binding exists. NEVER send trigger bytes — they would land
    /// on the prompt as garbage.
    None,
}

/// How a deferred-resume prefill line reaches the prompt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefillStrategy {
    /// The rc tail consumes `NICE_PREFILL_COMMAND` shell-side (zsh `print -z`).
    /// Nice sets the env var and does nothing else. FROZEN for zsh.
    ShellSide,
    /// Nice types the prefill into the pty master itself, once the pane signals
    /// readiness (first OSC 7 report). No env var needed.
    AppTyped,
    /// No prefill. The pane opens at a bare prompt (fallback shells).
    Off,
}

/// Everything routing needs about a pane's shell after spawn. Captured at spawn
/// time and stored beside the pty handle, so a mid-run setting change can never
/// make Nice talk to a live pane in a dialect it does not speak. Runtime-only —
/// never persisted to `sessions.json`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaneShell {
    pub kind: ShellKind,
    pub compose: ComposeSupport,
}

/// One shell's dialect knowledge. Exactly one profile is active per app run
/// (resolved once at bootstrap, design §4), so trait-object dispatch costs
/// nothing that matters and each shell's knowledge stays in one file.
pub trait ShellProfile: Send + Sync {
    fn kind(&self) -> ShellKind;

    /// Absolute path of the resolved shell binary (e.g. `/bin/zsh`).
    fn program(&self) -> &str;

    /// Full argv (argv\[0\] included) for a pane spawn. Must honor both
    /// [`SpawnCtx`] axes (inject × command).
    fn spawn_argv(&self, ctx: &SpawnCtx) -> Vec<String>;

    /// Shell-specific env pairs for an INJECTED spawn. zsh: `ZDOTDIR` +
    /// `NICE_USER_ZDOTDIR`. bash: empty (injection rides argv). Generic pairs
    /// (`NICE_SOCKET` / `NICE_TAB_ID` / `NICE_PANE_ID` / `NICE_COMPOSE_CONF`)
    /// are NOT this method's job — they stay shell-agnostic in `pty_manager`.
    fn inject_env(&self, inject: &InjectPaths, user: &UserShellEnv) -> Vec<(String, String)>;

    /// Write this profile's rc files into `dir` (overwrite-always self-heal),
    /// returning the paths `spawn_argv` / `inject_env` need.
    fn write_rc_files(&self, dir: &Path) -> io::Result<InjectPaths>;

    /// Compose capability of panes spawned from this profile.
    fn compose_support(&self) -> ComposeSupport;

    /// Prefill mechanism for deferred-resume panes.
    fn prefill(&self) -> PrefillStrategy;

    /// argv (argv\[0\] included) for one-shot PATH-honoring probes, e.g.
    /// `command -v -- claude`. Must load the user's PATH the way a real login
    /// pane of this shell would.
    fn probe_argv(&self, probe_cmd: &str) -> Vec<String>;

    /// This shell's kernel comm name (MAXCOMLEN-truncated) for the orphan
    /// reaper's prefilter (`"zsh"`, `"bash"`, …). Borrowed from `self` rather
    /// than `&'static str`: the fallback profile's comm is the basename of a
    /// runtime-resolved path. Callers collect owned `String`s.
    fn comm_name(&self) -> &str;

    /// Human name for settings/help copy. Same borrowed treatment as
    /// [`Self::comm_name`], for the same reason.
    fn display_name(&self) -> &str;
}

/// The fixed, per-variant root every profile's rc files live under:
/// `<app support root>/<CFBundleName>/shellrc`, one subdirectory per shell
/// ([`rc_dir_for`]). Application Support, never `$TMPDIR` — macOS's
/// `com.apple.bsd.dirhelper` sweeps `$TMPDIR` files older than 3 days, which
/// used to delete the stubs out from under a long-running Nice.
///
/// Honors the `NICE_APPLICATION_SUPPORT_ROOT` override seam (tests redirect it
/// into a sandbox; production leaves it unset), and the folder name tracks
/// `CFBundleName` so each variant (`Nice` / `Nice Dev`) stays isolated. Pure —
/// creates nothing; the profile's `write_rc_files` creates what it needs.
///
/// **Legacy directory:** before the per-shell split, zsh's stubs lived directly
/// in `<CFBundleName>/zdotdir`. That directory is deliberately LEFT IN PLACE
/// (a few KB of static stubs): sweeping shared Application Support state from
/// new code would race an older running build of the same variant that is still
/// reading it.
pub fn shellrc_root() -> PathBuf {
    let override_value = std::env::var("NICE_APPLICATION_SUPPORT_ROOT").ok();
    let home = std::env::var("HOME").ok();
    application_support_root(override_value.as_deref(), home.as_deref())
        .join(bundle_folder_name())
        .join("shellrc")
}

/// Where `profile`'s rc files go: `shellrc/<shell>/`, keyed by the profile's own
/// comm name (`shellrc/zsh`, `shellrc/bash`, …). Only the ACTIVE profile's
/// directory is written, at bootstrap.
pub fn rc_dir_for(profile: &dyn ShellProfile) -> PathBuf {
    shellrc_root().join(profile.comm_name())
}

/// Resolve the Application Support root. The `NICE_APPLICATION_SUPPORT_ROOT`
/// override wins when present and non-empty (the test seam); otherwise
/// `<home>/Library/Application Support`. Factored out of [`shellrc_root`] so the
/// override seam is unit-tested without mutating the process environment.
pub(crate) fn application_support_root(override_value: Option<&str>, home: Option<&str>) -> PathBuf {
    if let Some(root) = override_value {
        if !root.is_empty() {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(home.unwrap_or("/")).join("Library/Application Support")
}

/// The per-variant folder name: the running app's `CFBundleName` (`"Nice"` /
/// `"Nice Dev"` for the shipped bundles), falling back to `"Nice (unbundled)"`
/// when unbundled — so an unbundled `cargo run` gets its own directory. Delegates
/// to [`crate::platform::support_folder_name`], the single source of truth.
fn bundle_folder_name() -> String {
    crate::platform::support_folder_name()
}

/// The active shell, process-wide: the resolved profile, the rc files written
/// for it, and the user-side env captured before any pty inherited Nice's
/// overrides. Installed by [`install_shell_runtime`] at bootstrap, and replaced
/// by an explicit settings pick (migration step 6) — panes never re-resolve, they
/// carry their spawn-time [`PaneShell`] instead.
///
/// Absent under `run_selftest`: the launch-time writers never run there
/// (hermeticity), so a scenario window gets a socket-only pty env and its shells
/// read the user's real rc directly, exactly as before the abstraction.
pub struct ShellRuntime {
    pub profile: Box<dyn ShellProfile>,
    /// `None` ⇒ the rc write failed (non-fatal): panes still get `NICE_SOCKET`,
    /// they just source the user's own rc.
    pub inject: Option<InjectPaths>,
    pub user_env: UserShellEnv,
}

impl gpui::Global for ShellRuntime {}

/// The rc-injection env pairs every window of this run injects into its panes.
///
/// The live path is the profile's own [`ShellProfile::inject_env`]. The
/// `inject: None` arm is a preserved **zsh-only quirk**: when the stub write
/// fails, today's Nice still injects the always-set `NICE_USER_ZDOTDIR` (only
/// `ZDOTDIR` is dropped), and the `.zshenv` stub's empty/absent distinction
/// makes that observable. It predates the trait, so it stays gated on the
/// profile kind — a stray `NICE_USER_ZDOTDIR` in a bash or fish pane is exactly
/// the cross-shell leakage this abstraction exists to prevent, so non-zsh
/// profiles with no inject paths emit nothing.
pub fn window_inject_pairs(runtime: &ShellRuntime) -> Vec<(String, String)> {
    match &runtime.inject {
        Some(paths) => runtime.profile.inject_env(paths, &runtime.user_env),
        None if runtime.profile.kind() == ShellKind::Zsh => vec![(
            "NICE_USER_ZDOTDIR".to_string(),
            runtime.user_env.user_zdotdir.clone().unwrap_or_default(),
        )],
        None => Vec::new(),
    }
}

/// Resolve the active shell → write its rc files → install the [`ShellRuntime`]
/// global. The one place a profile is chosen, called by the bootstrap and again
/// by the Settings ▸ Advanced shell pick (migration step 6) — which is why it is
/// a function and not inline bootstrap code, and why nothing here is set-once.
///
/// Ordering is design §4's: resolution first, then the user-side env capture
/// (**before** anything of ours can perturb it), then the rc write. A failed rc
/// write is non-fatal — it logs and leaves `inject: None`, and windows still get
/// `NICE_SOCKET`.
pub fn install_shell_runtime(cx: &mut gpui::App, setting: &resolve::ShellSetting) {
    let profile = resolve::resolve(setting);
    // Nice's own inherited ZDOTDIR, read straight from the process env so it is
    // captured even when the rc write below fails (a window still benefits from
    // NICE_USER_ZDOTDIR).
    let user_env = UserShellEnv {
        user_zdotdir: std::env::var("ZDOTDIR").ok(),
    };
    // One subdirectory per shell under the per-variant `shellrc` root, written
    // for the ACTIVE profile only.
    let inject = match profile.write_rc_files(&rc_dir_for(profile.as_ref())) {
        Ok(paths) => Some(paths),
        Err(e) => {
            eprintln!("nice: ZDOTDIR inject failed: {e} (windows still get NICE_SOCKET)");
            None
        }
    };
    cx.set_global(ShellRuntime {
        profile,
        inject,
        user_env,
    });
}

/// The argv a production pane spawn should exec, from the active profile.
///
/// `inject` says whether this pane wants Nice's rc injection (normal panes and
/// deferred-resume Claude panes do; non-deferred Claude windows spawn the user's
/// genuine login shell, matching what Nice has always done). It changes nothing
/// for zsh — injection rides `ZDOTDIR`, not argv — but it is the axis a shell
/// like bash injects along, so every call site declares it now.
///
/// With no [`ShellRuntime`] installed (`run_selftest`, which never bootstraps)
/// this is the historical zsh default, so scenarios spawn exactly what they
/// always spawned.
pub fn spawn_argv(cx: &gpui::App, inject: bool, command: Option<&str>) -> Vec<String> {
    let Some(runtime) = cx.try_global::<ShellRuntime>() else {
        return nice_term_core::build_argv(command);
    };
    let ctx = SpawnCtx {
        inject: if inject {
            runtime.inject.as_ref()
        } else {
            None
        },
        command,
    };
    runtime.profile.spawn_argv(&ctx)
}

/// The [`PaneShell`] snapshot a pane spawned right now should carry: the active
/// profile's kind + compose capability, frozen at spawn so routing never asks
/// the live profile what a running pane speaks.
///
/// With no [`ShellRuntime`] installed (`run_selftest`, which never bootstraps)
/// this is the historical zsh snapshot, so scenario panes route exactly as they
/// always did — their fixture shells ARE zsh (design §10).
pub fn pane_shell(cx: &gpui::App) -> PaneShell {
    match cx.try_global::<ShellRuntime>() {
        Some(runtime) => PaneShell {
            kind: runtime.profile.kind(),
            compose: runtime.profile.compose_support(),
        },
        None => PaneShell {
            kind: ShellKind::Zsh,
            compose: ComposeSupport::Trigger,
        },
    }
}

/// How a deferred-resume pane of the active profile receives its prefill line.
///
/// Same absent-runtime rule as [`pane_shell`]: no runtime ⇒ zsh's shell-side
/// `print -z` contract, which is what scenarios' fixture stubs implement.
pub fn prefill_strategy(cx: &gpui::App) -> PrefillStrategy {
    match cx.try_global::<ShellRuntime>() {
        Some(runtime) => runtime.profile.prefill(),
        None => PrefillStrategy::ShellSide,
    }
}

/// The registry: every shell Nice ships a profile for. Adding a shell = add it
/// here. The orphan reaper prefilters on this union (plus the active profile's
/// own comm name) so it still recognizes shells orphaned by a PRIOR run under a
/// different setting.
///
/// `"bash"` joins the list with `BashProfile` (migration step 3) — until then a
/// bash user runs under the fallback profile, and the active-profile arm of the
/// union already covers them.
pub fn all_known_comm_names() -> &'static [&'static str] {
    &["zsh"]
}

/// The kernel comm names the orphan reaper's prefilter admits: the registry
/// ([`all_known_comm_names`]) plus `active` — the currently-resolved profile's
/// own comm, which covers a fallback shell (`"fish"`, and `"bash"` until step 3)
/// that is in no registry. Deduped, registry first.
///
/// The union is what makes the reaper see shells orphaned by a PRIOR run under a
/// DIFFERENT setting (design §7) — the decisive PPID==1 / uid / `NICE_TAB_ID=`
/// criteria are unchanged and already shell-agnostic. Accepted edge, also §7: a
/// user who crashed under fallback-fish and then switched to zsh leaves fish
/// orphans unmatched until the fish setting is active again.
pub fn reaper_comm_names(active: &str) -> Vec<String> {
    let mut names: Vec<String> = all_known_comm_names().iter().map(|s| s.to_string()).collect();
    if !names.iter().any(|n| n == active) {
        names.push(active.to_string());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use zsh::ZshProfile;

    fn zsh_runtime(inject: Option<&str>, user_zdotdir: Option<&str>) -> ShellRuntime {
        ShellRuntime {
            profile: Box::new(ZshProfile::new("/bin/zsh")),
            inject: inject.map(|d| InjectPaths {
                dir: d.into(),
                rcfile: None,
            }),
            user_env: UserShellEnv {
                user_zdotdir: user_zdotdir.map(str::to_string),
            },
        }
    }

    /// The live path is just the profile's `inject_env`.
    #[test]
    fn window_inject_pairs_uses_the_profile_when_rc_files_exist() {
        assert_eq!(
            window_inject_pairs(&zsh_runtime(Some("/managed/z"), Some("/user/z"))),
            vec![
                ("ZDOTDIR".to_string(), "/managed/z".to_string()),
                ("NICE_USER_ZDOTDIR".to_string(), "/user/z".to_string()),
            ]
        );
    }

    /// Failed-rc-write parity: a zsh pane still gets the always-set
    /// `NICE_USER_ZDOTDIR` (only `ZDOTDIR` drops out) — the pre-abstraction
    /// behavior, preserved verbatim.
    #[test]
    fn window_inject_pairs_keeps_the_zsh_degraded_quirk() {
        assert_eq!(
            window_inject_pairs(&zsh_runtime(None, None)),
            vec![("NICE_USER_ZDOTDIR".to_string(), String::new())],
            "an absent ZDOTDIR must still ship an EMPTY NICE_USER_ZDOTDIR"
        );
        assert_eq!(
            window_inject_pairs(&zsh_runtime(None, Some("/user/z"))),
            vec![("NICE_USER_ZDOTDIR".to_string(), "/user/z".to_string())]
        );
    }

    // ---- the NICE_APPLICATION_SUPPORT_ROOT override seam -------------------
    //
    // Driven through the pure `application_support_root` so no process env is
    // mutated (which would race parallel tests). Moved here from `shell/zsh.rs`
    // with the function, when the rc root became shared across profiles.

    #[test]
    fn override_root_wins_when_set() {
        assert_eq!(
            application_support_root(Some("/sandbox/appsup"), Some("/home/u")),
            PathBuf::from("/sandbox/appsup")
        );
    }

    #[test]
    fn empty_override_falls_back_to_home() {
        assert_eq!(
            application_support_root(Some(""), Some("/home/u")),
            PathBuf::from("/home/u/Library/Application Support")
        );
    }

    #[test]
    fn default_root_uses_home_app_support() {
        assert_eq!(
            application_support_root(None, Some("/home/u")),
            PathBuf::from("/home/u/Library/Application Support")
        );
    }

    /// Each profile writes into its OWN subdirectory of the shared per-variant
    /// root, so two shells' rc files can never collide — and zsh's directory is
    /// exactly the `ZDOTDIR` its module hands out.
    #[test]
    fn rc_dir_is_a_per_shell_subdirectory_of_the_shellrc_root() {
        let root = shellrc_root();
        assert_eq!(
            root.file_name().and_then(|n| n.to_str()),
            Some("shellrc"),
            "the shared root is named `shellrc`"
        );
        let zsh_dir = rc_dir_for(&ZshProfile::new("/bin/zsh"));
        assert_eq!(zsh_dir, root.join("zsh"));
        assert_eq!(
            zsh_dir,
            zsh::default_location(),
            "the bootstrap must write zsh's stubs exactly where the zsh module reads them"
        );
        assert_eq!(
            rc_dir_for(&fallback::FallbackProfile::new("/usr/local/bin/fish")),
            root.join("fish")
        );
    }

    /// The reaper's accepted set is the registry plus whatever the active
    /// profile is — never one or the other, so a crashed run's orphans are
    /// matched whether they predate or follow a shell-setting change.
    #[test]
    fn reaper_comm_names_union_covers_registry_and_active_profile() {
        // A zsh run: the registry already carries it, so no duplicate appears.
        assert_eq!(reaper_comm_names("zsh"), vec!["zsh".to_string()]);
        // A fallback run: the registry entries still ride along, so zsh orphans
        // from a PRIOR zsh-configured run are reaped by a fish-configured launch.
        assert_eq!(
            reaper_comm_names("fish"),
            vec!["zsh".to_string(), "fish".to_string()]
        );
        // Until step 3's BashProfile joins the registry, a bash user's own comm
        // reaches the prefilter through the active-profile arm.
        assert_eq!(
            reaper_comm_names("bash"),
            vec!["zsh".to_string(), "bash".to_string()]
        );
        // Every registry entry is always present, whatever is active.
        for name in all_known_comm_names() {
            assert!(reaper_comm_names("tcsh").iter().any(|n| n == name));
        }
    }

    /// The degraded arm is zsh's alone. A non-zsh profile whose rc write failed
    /// emits NOTHING — a stray `NICE_USER_ZDOTDIR` in a bash or fish pane is
    /// exactly the cross-shell leakage the abstraction exists to prevent.
    #[test]
    fn window_inject_pairs_degraded_arm_is_zsh_only() {
        let fallback = ShellRuntime {
            profile: Box::new(fallback::FallbackProfile::new("/usr/local/bin/fish")),
            inject: None,
            user_env: UserShellEnv {
                user_zdotdir: Some("/user/z".to_string()),
            },
        };
        assert_eq!(
            window_inject_pairs(&fallback),
            Vec::<(String, String)>::new(),
            "a non-zsh profile must not inherit zsh's always-set NICE_USER_ZDOTDIR"
        );
        // And with live rc paths it is still the profile's own (empty) answer.
        let fallback = ShellRuntime {
            inject: Some(InjectPaths {
                dir: "/managed/rc".into(),
                rcfile: None,
            }),
            ..fallback
        };
        assert_eq!(
            window_inject_pairs(&fallback),
            Vec::<(String, String)>::new()
        );
    }

    /// The production spawn sites all read their argv from here. With a zsh
    /// runtime installed — and with none at all, the `run_selftest` shape — the
    /// argv is the historical zsh login shell on both axes.
    #[gpui::test]
    fn spawn_argv_is_the_zsh_default_with_and_without_a_runtime(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let login = vec!["/bin/zsh".to_string(), "-il".to_string()];
            let command = vec![
                "/bin/zsh".to_string(),
                "-ilc".to_string(),
                "exec vim".to_string(),
            ];

            // No runtime (scenarios never bootstrap one): the term-core default.
            assert_eq!(spawn_argv(cx, true, None), login);
            assert_eq!(spawn_argv(cx, false, Some("vim")), command);

            cx.set_global(zsh_runtime(Some("/managed/z"), None));
            // zsh ignores the inject axis — injection rides ZDOTDIR — so all four
            // combinations of (inject × command) collapse to the same two argvs.
            assert_eq!(spawn_argv(cx, true, None), login);
            assert_eq!(spawn_argv(cx, false, None), login);
            assert_eq!(spawn_argv(cx, true, Some("vim")), command);
            assert_eq!(spawn_argv(cx, false, Some("vim")), command);
        });
    }
}
