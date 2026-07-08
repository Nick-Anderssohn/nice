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

use gpui::{px, div, prelude::*, AnyElement, App, Window};

use crate::settings::controls::toggle_switch;
use crate::settings::root::{setting_row, setting_title};
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

/// The Claude pane body (The spec §Claude). The "Sync Claude Code theme" control
/// is the shared [`toggle_switch`] (a11y `settings.claude.syncClaudeTheme`);
/// click → [`perform_toggle_sync_claude`] with the flipped value.
pub(crate) fn claude_pane(_window: &mut Window, cx: &mut App) -> AnyElement {
    let on = crate::app::claude_theme_sync_gate_on(cx);

    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .child(setting_title("Claude", cx))
        .child(setting_row(
            "Sync Claude Code theme",
            Some(
                "Match Claude Code's colors to Nice's current terminal theme, and update \
                 them live when you change it."
                    .into(),
            ),
            toggle_switch("settings.claude.syncClaudeTheme", on, cx, move |cx| {
                perform_toggle_sync_claude(cx, !on);
            }),
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
