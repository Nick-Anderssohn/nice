//! The Claude pane (R23 What-to-build item 5, The spec §Claude) — a single
//! "Sync Claude Code theme" toggle. The handoff-skill row is OMITTED this cycle
//! (Binding decision D1: R26 owns the toggle AND its file-installing side effect).
//!
//! The toggle both **persists** to the `syncClaudeTheme` CFPreferences key R17
//! reads at boot (via the new [`crate::platform::write_bool_pref`], D4 — the single
//! source of truth) AND drives R21's [`apply_sync_claude_theme`](crate::theme_settings::apply_sync_claude_theme)
//! for the live effect. The two are split so the LIVE arm is exercised in isolation
//! by the in-crate `#[gpui::test]` below (the CFPref write is confined to
//! [`perform_toggle_sync_claude`] and is NEVER reached from that test or from
//! `run_selftest` — hermeticity). The test installs no `SharedThemeState`, so
//! [`sync_claude_live_arm`]'s off→on colors-file write no-ops — it never touches
//! the real `~/.claude` — leaving the gate flip as the clean assertion.

use gpui::{px, div, prelude::*, AnyElement, App, Context, Window};

use crate::settings::controls::{dropdown, toggle_switch, DropdownItem};
use crate::settings::root::{setting_row_info, SettingsRootView};
use crate::theme_settings;

/// The full toggle handler (the shipped click path): persist the new value to the
/// `syncClaudeTheme` CFPref (D4 — the R17 boot gate's single source of truth), then
/// apply the live effect. Reaches the REAL CFPrefs domain, so it is called ONLY
/// from the live UI handler in an `app::run`-installed window — never from
/// `run_selftest` (the in-crate test below drives [`sync_claude_live_arm`] instead).
pub(crate) fn perform_toggle_sync_claude(cx: &mut App, on: bool) {
    crate::platform::write_bool_pref("syncClaudeTheme", on);
    sync_claude_live_arm(cx, on);
}

/// The LIVE arm only — R21's `apply_sync_claude_theme` (flip the gate + re-source
/// every window's `--settings` provider + rewrite the colors file on `off→on`),
/// with NO CFPref write. The scenario drives THIS so the suite never touches the
/// real preferences domain (hermeticity, D4).
pub(crate) fn sync_claude_live_arm(cx: &mut App, on: bool) {
    theme_settings::apply_sync_claude_theme(cx, on);
    // Repaint so the toggle reflects the new gate state (apply_sync_claude_theme
    // re-sources providers but does not itself refresh chrome).
    cx.refresh_windows();
}

/// The Claude-skills toggle handler (R26, D10) — the shipped click path. Persist
/// the new value to the `installHandoffSkill` CFPref (the boot-reconcile gate's
/// single source of truth — the key keeps its handoff-era name now that the
/// dispatch pair rides the same toggle), update the in-memory `HandoffSkillGate`
/// so the re-render reflects it, sync EVERY installed skill/helper pair on disk
/// to the new state, then repaint. Unlike the sync-theme toggle there is NO
/// unit-observable live-arm split: the toggle's ONLY effect is real filesystem
/// writes (tested hermetically at the installer's `sync_with` seam against
/// scratch dirs), so a split would buy no hermetic coverage (YAGNI). Reaches
/// the REAL CFPrefs domain
/// AND the real `~/.claude` / `~/.nice`, so it is called ONLY from the live UI
/// handler in an `app::run`-installed window — NEVER `run_selftest`, never a test.
pub(crate) fn perform_toggle_install_handoff(cx: &mut App, on: bool) {
    crate::platform::write_bool_pref("installHandoffSkill", on);
    crate::app::set_handoff_skill_gate(cx, on);
    crate::skill_installer::sync(on);
    cx.refresh_windows();
}

/// One Command Compose dropdown (model or effort): the current selection's
/// label on the trigger, one checkmarked item per choice, each item routing to
/// its `compose_conf` setter. `current` is compared by VALUE (an unknown
/// persisted token shows the raw token so the user sees what is set).
fn compose_dropdown(
    trigger_id: &'static str,
    choices: &'static [(&'static str, &'static str)],
    current: String,
    apply: fn(&mut App, &str),
    window: &mut Window,
    cx: &mut Context<SettingsRootView>,
) -> AnyElement {
    let current_label = choices
        .iter()
        .find(|(value, _)| *value == current)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| current.clone());
    let items = choices
        .iter()
        .map(|(value, label)| {
            DropdownItem::new(
                format!("{trigger_id}.{}", if value.is_empty() { "default" } else { value }),
                *label,
                *value == current,
                move |cx| apply(cx, value),
            )
        })
        .collect();
    dropdown(trigger_id, current_label, items, window, cx)
}

/// The Claude pane body (The spec §Claude). The "Sync Claude Code theme" control
/// is the shared [`toggle_switch`] (a11y `settings.claude.syncClaudeTheme`);
/// click → [`perform_toggle_sync_claude`] with the flipped value. The two
/// Command Compose dropdowns route to [`crate::compose_conf`] (persist +
/// compose.json rewrite; the ZLE widget reads the file on the next ⌘↩).
pub(crate) fn claude_pane(window: &mut Window, cx: &mut Context<SettingsRootView>) -> AnyElement {
    let on = crate::app::claude_theme_sync_gate_on(cx);
    // R26: the handoff-skill toggle RENDERS from the in-memory gate (seeded once
    // from the CFPref in `app::run`), NOT `read_bool_pref` — so a scenario /
    // `run_selftest` render never reads the real CFPrefs domain (D7). Absent
    // gate ⇒ OFF.
    let handoff_on = crate::app::handoff_skill_gate_on(cx);

    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .child(setting_row_info(
            "Sync Claude Code theme",
            "Matches Claude Code's colors to Nice's current terminal theme, and updates \
             them live when you change it.",
            toggle_switch("settings.claude.syncClaudeTheme", on, cx, move |cx| {
                perform_toggle_sync_claude(cx, !on);
            }),
            cx,
        ))
        .child(setting_row_info(
            "Install the Nice Claude skills",
            "Adds the global /nice-handoff and /nice-dispatch Claude Code skills and \
             installs their helpers in ~/.nice. Run inside a Claude window, /nice-handoff \
             writes a handoff file capturing the current work and opens a new session \
             where a fresh Claude continues from where this one left off; /nice-dispatch \
             writes a task brief and opens a background session running it in its own \
             worktree.",
            toggle_switch(
                "settings.claude.installHandoffSkill",
                handoff_on,
                cx,
                move |cx| {
                    perform_toggle_install_handoff(cx, !handoff_on);
                },
            ),
            cx,
        ))
        .child(setting_row_info(
            "Command Compose model",
            "The Claude model that turns plain English at a zsh prompt into a real \
             command (the Compose command shortcut). Sonnet balances speed and \
             quality; CLI default uses whatever your claude is configured with.",
            compose_dropdown(
                "settings.claude.composeModel",
                &crate::compose_conf::MODEL_CHOICES,
                crate::compose_conf::model(cx),
                |cx, v| crate::compose_conf::set_model(cx, v),
                window,
                cx,
            ),
            cx,
        ))
        .child(setting_row_info(
            "Command Compose effort",
            "How hard the model thinks per compose. Medium is a good default; low \
             is snappier for simple commands.",
            compose_dropdown(
                "settings.claude.composeEffort",
                &crate::compose_conf::EFFORT_CHOICES,
                crate::compose_conf::effort(cx),
                |cx, v| crate::compose_conf::set_effort(cx, v),
                window,
                cx,
            ),
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    // The live arm on the MOCKED `TestAppContext` (no Metal, no pixels;
    // parallel-safe), living IN THIS CRATE — `sync_claude_live_arm` takes
    // `&mut App` and drives the process gate, which a dev/test crate cannot reach
    // (it cannot import this binary crate). This is the coverage the CFPref split
    // was built for: it exercises the LIVE arm without ever touching the real
    // CFPreferences domain (that write stays in `perform_toggle_sync_claude`).
    //
    // No `SharedThemeState` is installed, so `apply_sync_claude_theme`'s off→on
    // `claude_sync_if_gated` colors-file write no-ops (it gates on that entity) —
    // the suite never writes the real `~/.claude`. The gate flip is the assertion.

    #[gpui::test]
    fn sync_claude_live_arm_flips_the_gate_both_ways(cx: &mut TestAppContext) {
        cx.update(|app| {
            // Absent global ⇒ OFF (the `run_selftest` default).
            assert!(
                !crate::app::claude_theme_sync_gate_on(app),
                "the gate starts OFF (no ClaudeThemeSyncGate global installed)"
            );

            // off→on flips the gate ON (and, with no SharedThemeState, writes nothing).
            sync_claude_live_arm(app, true);
            assert!(
                crate::app::claude_theme_sync_gate_on(app),
                "sync_claude_live_arm(_, true) turns the gate ON"
            );

            // on→off flips it back OFF.
            sync_claude_live_arm(app, false);
            assert!(
                !crate::app::claude_theme_sync_gate_on(app),
                "sync_claude_live_arm(_, false) turns the gate OFF"
            );
        });
    }
}
