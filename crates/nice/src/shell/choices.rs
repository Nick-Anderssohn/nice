//! What the Settings ▸ Advanced ▸ Shell picker offers (design §9.6, migration
//! step 6).
//!
//! Pure on purpose: [`shell_choices`] takes its candidate list as an argument,
//! so the whole picker — filtering, ordering, labels, the passthrough row — is
//! unit-testable without a window and without depending on which shells the test
//! machine happens to have. [`discover_candidates`] is the one live half, and it
//! only *collects* paths; every screen is applied in [`shell_choices`].

use std::path::{Path, PathBuf};

/// The shell families Nice has a profile for, in the order the picker lists
/// them. Anything else is dropped from the list: offering fish here would
/// advertise an integration-free terminal as a feature, when what a fish user
/// gets is [`super::fallback::FallbackProfile`] (design §5). This mirrors
/// [`super::resolve`]'s basename mapping — a family added there gets a row here.
const KNOWN_FAMILIES: [&str; 2] = ["zsh", "bash"];

/// Candidates appended after `/etc/shells`. Homebrew's bash (Apple Silicon and
/// Intel prefixes) is the one that matters: `/etc/shells` lists only the system
/// shells, and a homebrew bash 5 is exactly the install where Command Compose
/// works while stock `/bin/bash` 3.2 cannot (design §6.3).
const EXTRA_CANDIDATES: [&str; 4] = [
    "/bin/zsh",
    "/bin/bash",
    "/opt/homebrew/bin/bash",
    "/usr/local/bin/bash",
];

/// One row of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellChoice {
    /// `None` ⇒ Automatic; `Some(path)` ⇒
    /// [`super::resolve::ShellSetting::Path`].
    pub(crate) path: Option<String>,
    /// What the row reads.
    pub(crate) label: String,
    /// a11y-id suffix (`"automatic"`, `"bin_zsh"`, `"opt_homebrew_bin_bash"`).
    pub(crate) id_suffix: String,
}

/// Every shell binary worth screening: each non-comment, non-empty line of
/// `/etc/shells`, then [`EXTRA_CANDIDATES`]. Deduped, order-preserving — so the
/// system paths `/etc/shells` lists come before a homebrew install of the same
/// family. Deliberately unfiltered: existence, the executable bit and the family
/// screen all live in [`shell_choices`], which keeps them testable against
/// injected fixtures.
///
/// A missing or unreadable `/etc/shells` is not an error — the extras alone
/// still describe a stock Mac.
pub(crate) fn discover_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/etc/shells") {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            push_unique(&mut out, PathBuf::from(line));
        }
    }
    for extra in EXTRA_CANDIDATES {
        push_unique(&mut out, PathBuf::from(extra));
    }
    out
}

fn push_unique(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !out.contains(&path) {
        out.push(path);
    }
}

/// The picker's rows, in order.
///
/// `persisted` is the raw `advanced.shell` value (`None` ⇒ Automatic; never
/// `Some("")` — [`crate::settings::prefs_store::SettingsPrefsStore::shell`]
/// normalizes an empty string away). `automatic_name` is the ALREADY-resolved
/// active profile's display name, used only to label row 0 when Automatic is
/// what is selected — the setting being Automatic is exactly what makes the
/// active profile the thing Automatic picked, so naming it costs no extra
/// resolution. `candidates` is [`discover_candidates`] live, injected in tests.
pub(crate) fn shell_choices(
    persisted: Option<&str>,
    automatic_name: &str,
    candidates: &[PathBuf],
) -> Vec<ShellChoice> {
    let automatic_label = match persisted {
        None => format!("Automatic ({automatic_name})"),
        Some(_) => "Automatic".to_string(),
    };
    let mut rows = vec![ShellChoice {
        path: None,
        label: automatic_label,
        id_suffix: "automatic".to_string(),
    }];

    // Screen once, keeping each survivor's family so the ordering and labeling
    // passes below don't re-derive it.
    let mut kept: Vec<(&'static str, String)> = Vec::new();
    for candidate in candidates {
        let Some(family) = known_family(candidate) else {
            continue;
        };
        if !is_usable(candidate) {
            continue;
        }
        let path = candidate.to_string_lossy().into_owned();
        if kept.iter().any(|(_, kept_path)| *kept_path == path) {
            continue;
        }
        kept.push((family, path));
    }

    // All zsh paths, then all bash paths, candidate order within a family. A
    // family with exactly one survivor is labeled by the family name alone; two
    // installs of the same family are disambiguated by path, which is
    // load-bearing rather than cosmetic — Command Compose works on a homebrew
    // bash 5 and not on stock `/bin/bash` 3.2.
    for family in KNOWN_FAMILIES {
        let paths: Vec<&String> = kept
            .iter()
            .filter(|(f, _)| *f == family)
            .map(|(_, path)| path)
            .collect();
        let disambiguate = paths.len() > 1;
        for path in paths {
            let label = if disambiguate {
                format!("{family} ({path})")
            } else {
                family.to_string()
            };
            rows.push(ShellChoice {
                path: Some(path.clone()),
                label,
                id_suffix: id_suffix_for(path),
            });
        }
    }

    // A persisted path Nice can't offer — a hand-edited setting, a shell that
    // was uninstalled, an exotic prefix — rides along as its own trailing row so
    // the user SEES what is set and can change it. Never silently reset (the
    // compose-dropdown precedent: an unknown persisted token shows the raw
    // token).
    if let Some(persisted) = persisted {
        if !rows.iter().any(|row| row.path.as_deref() == Some(persisted)) {
            rows.push(ShellChoice {
                path: Some(persisted.to_string()),
                label: persisted.to_string(),
                id_suffix: id_suffix_for(persisted),
            });
        }
    }

    rows
}

/// The candidate's family, or `None` when Nice has no profile for it.
fn known_family(path: &Path) -> Option<&'static str> {
    let basename = path.file_name()?.to_str()?;
    KNOWN_FAMILIES.into_iter().find(|f| *f == basename)
}

/// Exists, is a regular file, has an execute bit. Restates
/// [`super::resolve`]'s private `is_usable_live` — the same screen the
/// resolution chain applies, so the picker can only offer paths resolution
/// would actually accept.
fn is_usable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// A path flattened into an a11y-id fragment: `/` and `.` become `_`, and the
/// leading separator's underscore is trimmed (`/bin/zsh` ⇒ `bin_zsh`). Ids are
/// matched by test harnesses, so they must carry no characters an id selector
/// treats specially.
fn id_suffix_for(path: &str) -> String {
    path.replace(['/', '.'], "_")
        .trim_start_matches('_')
        .to_string()
}

/// The Settings ▸ Advanced ▸ Shell row's ⓘ text.
///
/// This is where "a change affects new terminals only" is communicated: an ⓘ
/// tooltip on a `setting_row_info`, which is the pane convention (rows carry no
/// hint text). No inline caption, no alert, no relaunch banner — the promise is
/// made true by the apply path, not by asking the user to restart.
pub(crate) fn shell_row_info(automatic_name: &str) -> String {
    format!(
        "Which shell new terminals run. Automatic uses your login shell \
         ({automatic_name}). Terminals already open keep the shell they started with."
    )
}

/// The Settings ▸ Claude ▸ Command Compose model row's ⓘ text.
///
/// `supported` is the active profile's [`super::ComposeSupport`]. When compose
/// is unavailable the row still renders two live dropdowns, so the reason leads
/// the tooltip — without it a stock-`/bin/bash` user configures a feature that
/// silently does nothing. The version floor is **4.3**, not 4: bash 4.0–4.2
/// cannot bind `bind -x` to a multi-character sequence (design §6.3).
pub(crate) fn compose_model_info(shell_name: &str, supported: bool) -> String {
    let body = format!(
        "The Claude model that turns plain English at a {shell_name} prompt into a real \
         command (the Compose command shortcut). Sonnet balances speed and quality; \
         CLI default uses whatever your claude is configured with."
    );
    if supported {
        body
    } else {
        format!(
            "Command Compose isn't available in {shell_name} — it needs zsh, or bash 4.3 \
             or newer. {body}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::bash::hermetic::ScratchHome;

    fn paths(choices: &[ShellChoice]) -> Vec<Option<&str>> {
        choices.iter().map(|c| c.path.as_deref()).collect()
    }

    fn labels(choices: &[ShellChoice]) -> Vec<&str> {
        choices.iter().map(|c| c.label.as_str()).collect()
    }

    /// Row 0 is Automatic whatever else survives, and it names the resolved
    /// shell only while Automatic is the selected setting — with another shell
    /// picked, re-resolving just to label a menu row would be wasted work, and
    /// the name would be wrong anyway.
    #[test]
    fn automatic_is_row_zero_and_names_the_shell_only_when_selected() {
        let choices = shell_choices(None, "zsh", &[]);
        assert_eq!(paths(&choices), vec![None]);
        assert_eq!(labels(&choices), vec!["Automatic (zsh)"]);

        let choices = shell_choices(Some("/bin/bash"), "zsh", &[]);
        assert_eq!(choices[0].path, None);
        assert_eq!(choices[0].label, "Automatic");
        assert_eq!(choices[0].id_suffix, "automatic");
    }

    /// Nonexistent, non-executable and not-a-file candidates are all dropped:
    /// the picker only offers what resolution would accept.
    #[test]
    fn unusable_candidates_are_dropped() {
        let home = ScratchHome::new("nice-choices-unusable");
        let real = home.install_executable("zsh", "#!/bin/sh\n");
        let not_executable = home.write_file("noexec/bash", "not a program");
        let missing = home.path().join("nowhere/zsh");
        let a_directory = home.path().join("dir/bash");
        std::fs::create_dir_all(&a_directory).unwrap();

        let choices = shell_choices(
            None,
            "zsh",
            &[real.clone(), not_executable, missing, a_directory],
        );
        assert_eq!(
            paths(&choices),
            vec![None, Some(real.to_string_lossy().as_ref())]
        );
    }

    /// A shell Nice has no profile for is not offered — a fish row would
    /// advertise a terminal with none of Nice's integration as a feature.
    #[test]
    fn unknown_families_are_dropped() {
        let home = ScratchHome::new("nice-choices-family");
        let fish = home.install_executable("fish", "#!/bin/sh\n");
        let tcsh = home.install_executable("tcsh", "#!/bin/sh\n");
        let bash = home.install_executable("bash", "#!/bin/sh\n");

        let choices = shell_choices(None, "zsh", &[fish, tcsh, bash.clone()]);
        assert_eq!(
            paths(&choices),
            vec![None, Some(bash.to_string_lossy().as_ref())]
        );
    }

    /// The same path listed twice (`/etc/shells` and the extras both name
    /// `/bin/bash`) yields one row.
    #[test]
    fn duplicate_candidates_yield_one_row() {
        let home = ScratchHome::new("nice-choices-dupe");
        let bash = home.install_executable("bash", "#!/bin/sh\n");
        let choices = shell_choices(None, "zsh", &[bash.clone(), bash.clone(), bash.clone()]);
        assert_eq!(choices.len(), 2, "Automatic plus one bash: {:?}", choices);
    }

    /// zsh rows before bash rows, candidate order within a family — regardless
    /// of the order the candidate list arrived in.
    #[test]
    fn zsh_rows_come_before_bash_rows() {
        let system = ScratchHome::new("nice-choices-order-sys");
        let brew = ScratchHome::new("nice-choices-order-brew");
        let system_bash = system.install_executable("bash", "#!/bin/sh\n");
        let system_zsh = system.install_executable("zsh", "#!/bin/sh\n");
        let brew_bash = brew.install_executable("bash", "#!/bin/sh\n");

        let choices = shell_choices(
            None,
            "zsh",
            &[system_bash.clone(), brew_bash.clone(), system_zsh.clone()],
        );
        assert_eq!(
            paths(&choices),
            vec![
                None,
                Some(system_zsh.to_string_lossy().as_ref()),
                Some(system_bash.to_string_lossy().as_ref()),
                Some(brew_bash.to_string_lossy().as_ref()),
            ]
        );
    }

    /// One install of a family reads as the bare family name; two read as
    /// `family (path)` so the user can tell a compose-capable homebrew bash from
    /// stock `/bin/bash`.
    #[test]
    fn labels_disambiguate_only_when_a_family_has_two_installs() {
        let system = ScratchHome::new("nice-choices-label-sys");
        let brew = ScratchHome::new("nice-choices-label-brew");
        let system_zsh = system.install_executable("zsh", "#!/bin/sh\n");
        let system_bash = system.install_executable("bash", "#!/bin/sh\n");
        let brew_bash = brew.install_executable("bash", "#!/bin/sh\n");

        // One bash: the bare family name.
        let choices = shell_choices(None, "zsh", &[system_zsh.clone(), system_bash.clone()]);
        assert_eq!(labels(&choices), vec!["Automatic (zsh)", "zsh", "bash"]);

        // Two bashes: both carry their path, and zsh (still alone) does not.
        let choices = shell_choices(
            None,
            "zsh",
            &[system_zsh, system_bash.clone(), brew_bash.clone()],
        );
        assert_eq!(
            labels(&choices),
            vec![
                "Automatic (zsh)".to_string(),
                "zsh".to_string(),
                format!("bash ({})", system_bash.display()),
                format!("bash ({})", brew_bash.display()),
            ]
        );
    }

    /// A persisted path the machine can't offer is appended verbatim rather than
    /// silently dropped, so the user sees what is actually set.
    #[test]
    fn an_unofferable_persisted_path_gets_a_passthrough_row() {
        let home = ScratchHome::new("nice-choices-passthrough");
        let bash = home.install_executable("bash", "#!/bin/sh\n");

        let choices = shell_choices(Some("/opt/pkg/bin/bash"), "zsh", &[bash.clone()]);
        assert_eq!(
            paths(&choices),
            vec![
                None,
                Some(bash.to_string_lossy().as_ref()),
                Some("/opt/pkg/bin/bash"),
            ]
        );
        let passthrough = choices.last().unwrap();
        assert_eq!(passthrough.label, "/opt/pkg/bin/bash");
        assert_eq!(passthrough.id_suffix, "opt_pkg_bin_bash");
    }

    /// A persisted path that IS among the offered rows adds no duplicate.
    #[test]
    fn a_persisted_path_that_is_offered_adds_no_extra_row() {
        let home = ScratchHome::new("nice-choices-no-dupe");
        let bash = home.install_executable("bash", "#!/bin/sh\n");
        let persisted = bash.to_string_lossy().into_owned();

        let choices = shell_choices(Some(&persisted), "zsh", &[bash]);
        assert_eq!(paths(&choices), vec![None, Some(persisted.as_str())]);
    }

    /// Ids carry no `/` or `.` — they are matched by harnesses as plain
    /// identifiers.
    #[test]
    fn id_suffix_is_sanitized_and_stable() {
        assert_eq!(id_suffix_for("/bin/zsh"), "bin_zsh");
        assert_eq!(
            id_suffix_for("/opt/homebrew/bin/bash"),
            "opt_homebrew_bin_bash"
        );
        assert_eq!(id_suffix_for("/opt/shells/bash-5.2"), "opt_shells_bash-5_2");
        for suffix in [
            id_suffix_for("/bin/zsh"),
            id_suffix_for("/opt/homebrew/bin/bash"),
        ] {
            assert!(!suffix.contains('/') && !suffix.contains('.'), "{suffix}");
            assert!(!suffix.starts_with('_'), "{suffix}");
        }
    }

    /// The Shell row's ⓘ names the shell Automatic resolved to and states the
    /// already-open promise, which is the only place that promise is made.
    #[test]
    fn shell_row_info_names_the_shell_and_the_already_open_promise() {
        let info = shell_row_info("bash");
        assert!(info.contains("bash"), "{info}");
        assert!(info.contains("Automatic uses your login shell (bash)"), "{info}");
        assert!(
            info.contains("already open keep the shell they started with"),
            "{info}"
        );
        // And it follows the active profile rather than hardcoding a shell.
        assert!(shell_row_info("zsh").contains("(zsh)"));
        assert!(!shell_row_info("bash").contains("zsh"), "no stray zsh");
    }

    /// A shell that can't compose leads with WHY, naming the shell and the real
    /// 4.3 floor — the literal is pinned so the gate can't regress to "4".
    #[test]
    fn compose_model_info_leads_with_the_unavailable_sentence() {
        let unsupported = compose_model_info("bash", false);
        assert!(
            unsupported.starts_with("Command Compose isn't available in bash"),
            "{unsupported}"
        );
        assert!(
            unsupported.contains("it needs zsh, or bash 4.3 or newer"),
            "{unsupported}"
        );
        assert!(
            unsupported.contains("plain English at a bash prompt"),
            "the model explanation still follows: {unsupported}"
        );

        let supported = compose_model_info("zsh", true);
        assert!(
            !supported.contains("isn't available"),
            "a compose-capable shell gets no warning: {supported}"
        );
        assert!(!supported.contains("4.3"), "{supported}");
        assert!(supported.starts_with("The Claude model"), "{supported}");
    }

    /// F11's regression pin on this crate's side: the Compose tooltip names
    /// whatever shell is active, never a hardcoded zsh.
    #[test]
    fn compose_model_info_says_zsh_only_when_zsh_is_the_shell() {
        assert!(!compose_model_info("bash", true).contains("zsh"));
        assert!(
            compose_model_info("fish", false).contains("zsh"),
            "the unavailable sentence names zsh as a SUPPORTED shell, which is fine"
        );
    }

    /// F12's regression pin: the README no longer promises the terminal beside a
    /// Claude session is zsh. A blanket "no `zsh` in the README" grep cannot
    /// hold — the Requirements list names zsh and bash as the two shells Nice
    /// integrates with — so this pins the two claims that were wrong, and
    /// requires any surviving mention to name bash alongside.
    #[test]
    fn readme_does_not_promise_a_zsh_terminal() {
        let readme = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
        let text = std::fs::read_to_string(&readme)
            .unwrap_or_else(|e| panic!("read {}: {e}", readme.display()));
        assert!(
            !text.contains("plain `zsh`"),
            "the README still promises a plain zsh"
        );
        for line in text.lines().filter(|l| l.contains("zsh")) {
            assert!(
                line.contains("bash"),
                "a README line names zsh without naming bash beside it: {line}"
            );
        }
    }

    /// The live collector always describes a stock Mac even with no
    /// `/etc/shells`, and never repeats a path.
    #[test]
    fn discover_candidates_covers_the_extras_without_duplicates() {
        let candidates = discover_candidates();
        for extra in EXTRA_CANDIDATES {
            assert!(
                candidates.contains(&PathBuf::from(extra)),
                "{extra} missing from {candidates:?}"
            );
        }
        let mut seen = candidates.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), candidates.len(), "duplicate candidate path");
    }
}
