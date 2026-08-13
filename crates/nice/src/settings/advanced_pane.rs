//! The Advanced pane (R23 What-to-build item 6, The spec §Advanced) — the
//! **Shell** picker (design §9.6, migration step 6) over the "Smooth scrolling"
//! toggle, which stays **persisted but INERT** (Binding decision D2: the
//! scroll-animation backend is deferred `nice-term-*` feel work).
//!
//! The Shell row is split the way the Claude pane splits
//! `perform_toggle_sync_claude` / `sync_claude_live_arm`: [`persist_shell_setting`]
//! is the pure-persistence half (the settings file and nothing else, unit-tested
//! against a temp-path store), and [`perform_pick_shell`] is the shipped click
//! path that additionally writes rc files under Application Support, replaces the
//! process-wide `ShellRuntime`, refreshes every live window's inject env and
//! re-probes `claude`. Nothing in a test or `run_selftest` can reach that half.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, AnyElement, App, Context, Window};

use crate::settings::controls::{dropdown, toggle_switch, DropdownItem};
use crate::settings::prefs_store::SettingsPrefsStore;
use crate::settings::root::{setting_row, setting_row_info, SettingsRootView};
use crate::shell::choices::{discover_candidates, shell_choices, shell_row_info};
use crate::shell::resolve::ShellSetting;

/// The Shell dropdown's a11y trigger id; each option row is
/// `<trigger>.<id_suffix>` (`settings.advanced.shell.bin_bash`).
const SHELL_DROPDOWN_ID: &str = "settings.advanced.shell";

/// The persisted smooth-scroll value (default OFF; absent store ⇒ OFF).
fn smooth_scroll_on(cx: &App) -> bool {
    cx.try_global::<SettingsPrefsStore>()
        .map(|s| s.smooth_scroll())
        .unwrap_or(false)
}

/// Persist the toggle (INERT — no live effect, D2). No-op when the store is absent.
pub(crate) fn toggle_smooth_scroll(cx: &mut App, on: bool) {
    if cx.try_global::<SettingsPrefsStore>().is_some() {
        let _ = cx.global_mut::<SettingsPrefsStore>().set_smooth_scroll(on);
    }
    cx.refresh_windows();
}

/// The raw persisted `advanced.shell` path (`None` ⇒ Automatic, which is also
/// what an absent store reads as).
fn persisted_shell(cx: &App) -> Option<String> {
    cx.try_global::<SettingsPrefsStore>().and_then(|s| s.shell())
}

/// The Shell dropdown's options, one per [`shell_choices`] row: the row's a11y
/// id, its label, whether it is the current selection, and the
/// [`perform_pick_shell`] call the click performs.
///
/// Pure (`candidates` is injected — live callers pass [`discover_candidates`]),
/// so ids, labels and the exactly-one-selected invariant are unit-tested without
/// a window. Selection compares by VALUE against `persisted`, and
/// `shell_choices` guarantees a matching row exists — an unofferable persisted
/// path rides along as its own passthrough row — so exactly one option is ever
/// selected.
pub(crate) fn shell_dropdown_items(
    persisted: Option<&str>,
    automatic_name: &str,
    candidates: &[PathBuf],
) -> Vec<DropdownItem> {
    shell_choices(persisted, automatic_name, candidates)
        .into_iter()
        .map(|choice| {
            let selected = choice.path.as_deref() == persisted;
            let path = choice.path;
            DropdownItem::new(
                format!("{SHELL_DROPDOWN_ID}.{}", choice.id_suffix),
                choice.label,
                selected,
                move |cx| perform_pick_shell(cx, path.clone()),
            )
        })
        .collect()
}

/// The persistence half of a shell pick: write `advanced.shell` through the
/// [`SettingsPrefsStore`], returning whether the value actually changed. No rc
/// files, no `ShellRuntime` swap, no process spawn — this is the half a
/// `#[gpui::test]` drives.
///
/// Absent store ⇒ a no-op that reports "no change" (the `compose_conf` setter's
/// fail-soft precedent), so `run_selftest` and scenarios pick nothing up. A
/// failed *write* still reports a change: the store's in-memory value has already
/// moved, so the pick must take effect this run — it just won't survive a
/// relaunch.
pub(crate) fn persist_shell_setting(cx: &mut App, path: Option<String>) -> bool {
    if cx.try_global::<SettingsPrefsStore>().is_none() {
        return false;
    }
    match cx.global_mut::<SettingsPrefsStore>().set_shell(path) {
        Ok(changed) => changed,
        Err(e) => {
            eprintln!("nice: could not persist the shell setting: {e}");
            true
        }
    }
}

/// The shipped Shell-pick click path (live UI only): persist, then — when the
/// pick actually resolves to a different binary — install the new shell runtime,
/// refresh every live window's inject env, and re-probe `claude`.
///
/// Writes rc files under Application Support and may spawn the probe, so it is
/// reached ONLY from the dropdown handler in an `app::run`-installed window;
/// tests drive [`persist_shell_setting`] instead.
///
/// Panes already running are deliberately left alone (design §4): they keep the
/// shell — and the `PaneShell` snapshot — they started with. The Shell row's ⓘ
/// says exactly that.
pub(crate) fn perform_pick_shell(cx: &mut App, path: Option<String>) {
    if !persist_shell_setting(cx, path.clone()) {
        cx.refresh_windows();
        return;
    }
    let setting = match path {
        Some(p) => ShellSetting::Path(p),
        None => ShellSetting::Automatic,
    };
    // Automatic and an explicit pick of the binary Automatic already resolved to
    // are the same shell: re-writing rc files, re-arming every window and
    // re-probing `claude` would all be busywork. The persisted value still
    // changed (that write happened above), so only the live half is skipped.
    let resolved_program = crate::shell::resolve::resolve(&setting).program().to_string();
    let unchanged = cx
        .try_global::<crate::shell::ShellRuntime>()
        .map(|rt| rt.profile.program() == resolved_program)
        .unwrap_or(false);
    if unchanged {
        cx.refresh_windows();
        return;
    }
    // Resolve → write the new profile's rc files → replace the ShellRuntime
    // global. From here on every NEW pane spawn (in any window, including
    // already-open ones) builds its argv and its PaneShell from the new profile.
    // A failed rc write stays non-fatal: it logs, leaves `inject: None`, and
    // panes still get NICE_SOCKET.
    crate::shell::install_shell_runtime(cx, &setting);
    // What does NOT follow from the global swap: each window's inject-env pairs,
    // frozen when its control socket was armed. Leaving them stale is a
    // correctness bug in both directions — a zsh→bash window would keep handing
    // Nice's zsh ZDOTDIR to any zsh started inside a bash pane, and a bash→zsh
    // window would spawn zsh panes with NO ZDOTDIR (no `claude()` shadow, no
    // OSC 7, no compose binding) while their snapshot still advertises compose.
    crate::app::refresh_window_inject_env(cx);
    // PATH lives in the shell's own rc files, so the new shell can resolve
    // `claude` somewhere else — or, transiently, not at all. Re-probe with
    // replace-only-on-success so a failed probe can never downgrade a working
    // install to "Claude not installed".
    crate::app::kickoff_claude_probe(cx, true);
    cx.refresh_windows();
}

/// The Advanced pane body (The spec §Advanced). **Shell** first — the dropdown
/// over [`shell_dropdown_items`], with the "new terminals only" promise in its ⓘ
/// ([`shell_row_info`]) — then the "Smooth scrolling" [`toggle_switch`] (a11y
/// `settings.advanced.smoothScrolling`; click → [`toggle_smooth_scroll`] with the
/// flipped value).
///
/// Takes `&mut Context<SettingsRootView>` rather than `&mut App` because the
/// dropdown's open-menu state lives on the root view — the same shape the
/// Claude / Font / Appearance panes already have.
pub(crate) fn advanced_pane(window: &mut Window, cx: &mut Context<SettingsRootView>) -> AnyElement {
    let on = smooth_scroll_on(cx);
    // The ALREADY-resolved active profile's name: with Automatic selected it is
    // by definition what Automatic picked, which is what row 0 names.
    let automatic_name = crate::shell::active_display_name(cx);
    let persisted = persisted_shell(cx);
    let items = shell_dropdown_items(
        persisted.as_deref(),
        &automatic_name,
        &discover_candidates(),
    );
    let current_label = items
        .iter()
        .find(|item| item.selected)
        .map(|item| item.label.to_string())
        .unwrap_or_else(|| "Automatic".to_string());

    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .child(setting_row_info(
            "Shell",
            shell_row_info(&automatic_name),
            dropdown(SHELL_DROPDOWN_ID, current_label, items, window, cx),
            cx,
        ))
        .child(setting_row(
            "Smooth scrolling",
            toggle_switch("settings.advanced.smoothScrolling", on, cx, move |cx| {
                toggle_smooth_scroll(cx, !on);
            }),
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::bash::hermetic::ScratchHome;
    use gpui::TestAppContext;

    fn ids(items: &[DropdownItem]) -> Vec<String> {
        items.iter().map(|i| i.id.to_string()).collect()
    }

    fn labels(items: &[DropdownItem]) -> Vec<String> {
        items.iter().map(|i| i.label.to_string()).collect()
    }

    fn selected_label(items: &[DropdownItem]) -> String {
        let selected: Vec<&DropdownItem> = items.iter().filter(|i| i.selected).collect();
        assert_eq!(
            selected.len(),
            1,
            "exactly one option is ever checkmarked: {:?}",
            labels(items)
        );
        selected[0].label.to_string()
    }

    fn temp_settings_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-advanced-pane-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// Every row carries its `settings.advanced.shell.<id_suffix>` id and the
    /// choice's label, and Automatic is the selection when nothing is persisted.
    #[test]
    fn dropdown_items_carry_the_row_ids_and_select_automatic_by_default() {
        let home = ScratchHome::new("nice-advanced-items");
        let zsh = home.install_executable("zsh", "#!/bin/sh\n");
        let bash = home.install_executable("bash", "#!/bin/sh\n");

        let expected_id = |path: &PathBuf| {
            format!(
                "settings.advanced.shell.{}",
                path.to_string_lossy()
                    .replace(['/', '.'], "_")
                    .trim_start_matches('_')
            )
        };
        let items = shell_dropdown_items(None, "zsh", &[zsh.clone(), bash.clone()]);
        assert_eq!(
            ids(&items),
            vec![
                "settings.advanced.shell.automatic".to_string(),
                expected_id(&zsh),
                expected_id(&bash),
            ]
        );
        assert_eq!(labels(&items), vec!["Automatic (zsh)", "zsh", "bash"]);
        assert_eq!(selected_label(&items), "Automatic (zsh)");
    }

    /// A persisted path selects its own row — including the passthrough row a
    /// path Nice can't offer gets, so a hand-edited setting is visible and
    /// selected rather than silently reset.
    #[test]
    fn the_persisted_path_is_the_selected_row() {
        let home = ScratchHome::new("nice-advanced-selected");
        let bash = home.install_executable("bash", "#!/bin/sh\n");
        let bash_path = bash.to_string_lossy().into_owned();

        let items = shell_dropdown_items(Some(&bash_path), "zsh", &[bash.clone()]);
        assert_eq!(selected_label(&items), "bash");
        assert_eq!(labels(&items)[0], "Automatic", "row 0 drops the shell name");

        let items = shell_dropdown_items(Some("/opt/pkg/bin/bash"), "zsh", &[bash]);
        assert_eq!(selected_label(&items), "/opt/pkg/bin/bash");
        assert_eq!(
            ids(&items).last().unwrap(),
            "settings.advanced.shell.opt_pkg_bin_bash"
        );
    }

    /// The persistence half round-trips through the settings FILE and reports
    /// only real changes. This is the whole of what a test may reach: the rc
    /// write, the `ShellRuntime` swap, the window fan-out and the claude probe
    /// all live in `perform_pick_shell`, which nothing here calls — the same
    /// split (and the same by-construction argument) as `claude_pane`'s
    /// CFPref-free live arm.
    #[gpui::test]
    fn persist_shell_setting_round_trips_and_only_reports_changes(cx: &mut TestAppContext) {
        let path = temp_settings_path("round-trip");
        cx.update(|app| {
            // Absent store: a no-op that neither panics nor claims a change.
            assert!(
                !persist_shell_setting(app, Some("/bin/bash".to_string())),
                "no store ⇒ no change"
            );

            app.set_global(SettingsPrefsStore::load(path.clone()));
            assert!(persist_shell_setting(app, Some("/bin/bash".to_string())));
            assert!(
                !persist_shell_setting(app, Some("/bin/bash".to_string())),
                "re-picking the same shell is not a change"
            );
        });
        assert_eq!(
            SettingsPrefsStore::load(path.clone()).shell(),
            Some("/bin/bash".to_string()),
            "the pick reached the file"
        );

        cx.update(|app| {
            assert!(persist_shell_setting(app, None), "back to Automatic writes");
            assert!(
                !persist_shell_setting(app, None),
                "re-picking Automatic is not a change"
            );
        });
        let reloaded = SettingsPrefsStore::load(path);
        assert_eq!(reloaded.shell(), None, "Automatic reads as absent");
        assert_eq!(
            reloaded.shell_setting(),
            crate::shell::resolve::ShellSetting::Automatic
        );
    }
}
