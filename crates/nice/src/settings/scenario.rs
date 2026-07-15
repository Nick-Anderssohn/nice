//! The `settings-window` self-test scenario (R23 What-to-build item 11).
//!
//! ## Legs
//! * **(a) ⌘, opens the singleton.** Driving [`open_or_focus_settings`] opens one
//!   Settings window (the `SettingsWindow` Global handle becomes `Some`, the real
//!   `App::windows()` count steps up by one, and the handle is a NEW window, not
//!   the host); a second drive focuses the SAME window (no second window, the
//!   handle unchanged); closing it clears the singleton Global.
//! * **(b) a live Appearance change repaints the real main window.** The host
//!   window's background paints the LIVE chrome accent
//!   ([`active_chrome_accent`](crate::theme_settings::active_chrome_accent) — the
//!   exact read the shipped toolbar/sidebar chrome uses). Driving the Appearance
//!   pane's `apply_accent` (R21) flips the resolved [`ThemeState`] the chrome
//!   paints from, and a [`sample_window_pixels`](nice_harness::capture::sample_window_pixels)
//!   read of the host window before/after the change shows a real per-channel
//!   recolor (±8/255) — proving the pane wired R21's `apply_*` and the fan-out
//!   reaches a shipped, painted window at the pixel level, not just the store
//!   state. (The COMPOSED pixel assert across the whole board — chrome + a
//!   terminal cell on the launch window under a rebound chord — remains R24's,
//!   per the plan's Validation split.)
//! * **(c) a Font slider fans out.** Driving the Font pane's terminal-size handler
//!   ([`apply_terminal_px`](crate::settings::font_pane::apply_terminal_px)) changes
//!   the shared [`FontSettings`](nice_term_view::FontSettings) px + re-metrics; a
//!   subsequent ⌘= (`zoom_by`) continues from the slider value on the SAME entity
//!   (no desync), and the `fonts` section on the temp `ui_settings.json` reflects
//!   the change (persistence).
//! * **(d) Import through the fake picker.** Scripting the injected
//!   [`RecordingFilePicker`](crate::settings::file_picker) to a temp fixture and
//!   driving the Import… handler runs R22's `import_theme` through the seam
//!   (never a real `NSOpenPanel`): the theme enters `imported_entries()` /
//!   `themes(for:)`; a malformed fixture surfaces the exact mapped §ImportError
//!   string (R23's copy).
//! * **(e) the rail exposes the six slugs.** `settings_rail_sections()` carries
//!   `appearance … about`, incl. the `shortcuts` row (the R24 pane).
//! * **(s1–s3) the Shortcuts recorder (R24).** Over a dedicated host window
//!   rendering the shipped `shortcuts_pane` body: **s1** enters recording on
//!   `newTerminalPane` and a REAL ⌘Y chord (`post_key_tap`, own pid) rebinds it;
//!   **s2** captures ⌘B (ToggleSidebar's default) → the "Already used by <label>"
//!   conflict, then Replace clears the loser + binds this action; **s3** Reset
//!   restores the default and `is_at_default` flips (the Reset button hides). A
//!   `SavedInputSource` is held for the whole leg (IME restore).
//! * **(§6) the tranche-5 final composition (Milestone 6).** Over the REAL
//!   registered launch window (`open_managed_window` / `build_window_root`, the
//!   exact path `run` takes — NOT a scenario host), composes the whole tranche's
//!   board by CGEvent to nice's own pid: **(6a)** a REBOUND chord dispatches on
//!   the shipped window (rebind `newTerminalPane` → ⌘Y, post it → a pane appears;
//!   the old default ⌘T no longer does), **(6b)** the PROTECTED non-rebindable set
//!   (⌃⌘F, ⌘N, ⌘Q, ⌘W, Esc@SidebarShell, ⌘,) survives the rebuild (a
//!   `key_bindings()` presence audit), **(6c)** ⌘, opens R23's settings window (the
//!   `OpenSettings` non-rebindable firing LIVE) and a live theme change
//!   (`apply_accent` + `apply_scheme`) repaints the shipped chrome AND a terminal
//!   cell (`sample_window_pixels`, max channel delta > 8/255), and **(6d)** a busy
//!   pane close (the `synthetic_foreground_child` seam) presents R20.5's
//!   `ConfirmationModal` (`pending_modal()` + the AX confirm id), cancelled. A
//!   hermetic ZDOTDIR/HOME/`NICE_CLAUDE_OVERRIDE` fixture; the launch window is
//!   reaped + the rebind reset at teardown. No earlier cycle asserted this board
//!   together — it is the Milestone-6 claim (TRANCHE-2-NOTES §6).
//!
//! The open path is driven explicitly (the plan's "drive the open fns explicitly,
//! no relaunch"); the ⌘, binding + action handler are wired by
//! [`install_open_settings_command`], exactly as the shipped `run` does.
//!
//! Registered BEFORE `multiwindow` in [`crate::app::selftest_scenarios`]: the
//! settings window is UNREGISTERED (D7) and this scenario installs no
//! `WindowRegistry` close observer, so nothing here trips the quit-when-empty
//! terminus `multiwindow` relies on being last. Self-reported (the pass criterion
//! is window count + fan-out state + the import outcome, not cadence).
//!
//! ## Hermeticity (tranche-5 rule)
//! Fully sandboxed: an injected [`OsSchemeSource`] stub (no leg reads the real
//! system appearance), a temp import-fixture dir + the `RecordingFilePicker`
//! (no real panel, fixtures are temp files), and the `run_selftest`-installed
//! defaults+temp theme store / catalog. No leg writes the real CFPrefs domain
//! (leg (b) drives R21's `apply_accent`, not any CFPref write).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, prelude::*, Action, AnyWindowHandle, AsyncApp, Context, Entity, Keystroke,
    Render, Window, WindowHandle,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_theme::palette::ColorScheme;
use nice_theme::AccentPreset;

use nice_model::shortcuts::{OwnedCombo, ShortcutAction};

use crate::app_shell::{AppShellView, PaneHostView};
use crate::platform;
use crate::settings::appearance_pane::{last_import_feedback, perform_import};
use crate::settings::file_picker::selftest_fake;
use crate::settings::root::{settings_rail_sections, shortcuts_pane};
use crate::settings::shortcuts_pane::{
    cancel_capture, conflict_action, conflict_message, enter_record, recording_action,
    reset_action, resolve_replace,
};
use crate::shortcuts_store::ShortcutBindings;
use crate::settings::window::{
    current_settings_window, force_settings_handle_for_scenario, install_open_settings_command,
    open_or_focus_settings,
};
use crate::terminal_theme_catalog::TerminalThemeCatalog;
use crate::theme_settings::{
    self, OsSchemeSource, SharedThemeState, ThemeSettingsStore, ThemeState,
};
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

/// The scenario's host window root: a frame-stamping view whose background paints
/// the LIVE chrome accent
/// ([`active_chrome_accent`](crate::theme_settings::active_chrome_accent) — the
/// same read the shipped toolbar/sidebar chrome uses). It re-reads the accent on
/// every frame (the continuous `request_animation_frame` loop), so a live
/// `apply_accent` fan-out recolors its pixels; leg (b) pixel-samples this window
/// before/after the accent change to prove the fan-out reaches a shipped, painted
/// window — not just the store state.
struct SettingsHostRoot;

impl Render for SettingsHostRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        let accent = crate::theme::srgba_to_rgba(theme_settings::active_chrome_accent(cx));
        div().size_full().bg(accent)
    }
}

/// A scenario host window rendering R24's real Shortcuts pane (the `shortcuts_pane`
/// seam) so the recorder legs (s1–s3) drive the SHIPPED recorder + a real CGEvent.
/// A dedicated host (not the live settings window) because R23's `SettingsRootView`
/// selects its active pane through PRIVATE state R24 must not touch — this renders
/// the identical pane body, focus-routes to it, and receives real chords when key.
struct ShortcutsHostRoot;

impl Render for ShortcutsHostRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        let panel =
            crate::theme::slot_to_rgba(crate::theme_settings::active_chrome_slots(cx).panel);
        div()
            .size_full()
            .bg(panel)
            .child(shortcuts_pane(window, cx))
    }
}

// macOS virtual keycodes (`CGKeyCode`) for the recorder legs' chords.
const KC_Y: u16 = 16; // kVK_ANSI_Y → ⌘Y (s1: a free chord)
const KC_B: u16 = 11; // kVK_ANSI_B → ⌘B (s2: ToggleSidebar's default → a conflict)
const KC_T: u16 = 17; // kVK_ANSI_T → ⌘T (§6a: NewTerminalPane's DEFAULT combo)
const KC_COMMA: u16 = 43; // kVK_ANSI_Comma → ⌘, (§6c: the OpenSettings non-rebindable)

/// Accessibility-grant remediation (shared wording with the `niceties-zoom` /
/// `multiwindow` scenarios): without the TCC grant `CGEventPostToPid` is silently
/// dropped, so the recorder never sees the injected chord.
const SHORTCUTS_ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected recorder chord can reach the \
window. Fix: System Settings → Privacy & Security → Accessibility → enable the \
process hosting this run (remove + re-add if it shows ON but is stale), then re-run.";

/// Points inside the 960×640 host window (whose background paints the live chrome
/// accent) — several interior points so anti-alias jitter at any one cannot mask a
/// real recolor.
const HOST_SAMPLE_POINTS: &[(f32, f32)] = &[(200.0, 200.0), (480.0, 320.0), (760.0, 500.0)];

fn sample_host(cx: &mut AsyncApp, host: AnyWindowHandle) -> Result<Vec<[u8; 4]>> {
    nice_harness::capture::sample_window_pixels(host, cx, HOST_SAMPLE_POINTS)
}

/// The largest per-channel delta across a sampled point set — the ground-truth
/// "did the window repaint" signal, tolerant of anti-aliasing (`>8/255` is a real
/// recolor, not sampling jitter). Returns 0 on empty input.
fn max_channel_delta(a: &[[u8; 4]], b: &[[u8; 4]]) -> u16 {
    a.iter()
        .zip(b.iter())
        .flat_map(|(x, y)| (0..3).map(move |i| (x[i] as i16 - y[i] as i16).unsigned_abs()))
        .max()
        .unwrap_or(0)
}

/// A minimal, well-formed Ghostty theme source with the given background hex + a
/// full 16-entry palette (so it parses). `bg` is `rrggbb` (no `#`).
fn ghostty_source(bg: &str) -> String {
    let mut s = format!("background = #{bg}\nforeground = #ffffff\n");
    for i in 0..16u8 {
        s.push_str(&format!("palette = {i}=#0000{i:02x}\n"));
    }
    s
}

/// Mint the live theme globals leg (b) needs (`run_selftest` installs the store +
/// catalog but NOT `SharedThemeState` / `OsSchemeSource` — a scenario opting into
/// live theming mints them itself over the already-installed defaults store). Then
/// install the ⌘, command. No pty / HOME / Claude spawn (the gate stays OFF).
pub fn open_settings_window_scenario(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    cx.update(|app| {
        // Live theme state over the run_selftest defaults store + catalog, so the
        // Appearance pane's `apply_accent` fans out to a resolved `ThemeState`.
        let entity = {
            let store = app.global::<ThemeSettingsStore>();
            let catalog = app.global::<TerminalThemeCatalog>();
            let state = ThemeState::from_stores(store, catalog);
            app.new(|_| state)
        };
        app.set_global(SharedThemeState(entity));
        // An injected OS-scheme stub (Dark, matching the store default) — no leg
        // reads the real system appearance.
        app.set_global(OsSchemeSource::new(|_| ColorScheme::Dark));
        // The ⌘, / OpenSettings command exactly as the shipped `run` (idempotent).
        install_open_settings_command(app);
        // Leg (c) drives the Font pane's terminal-size handler, which reads the
        // shared `FontSettings` + `SharedSidebarFont` entities `install_shortcuts`
        // mints (seeded from the `run_selftest` defaults+temp `SettingsPrefsStore`).
        // Idempotent — the shipped `run` installs the same keymap.
        crate::keymap::install_shortcuts(app);
    });

    let window = cx.open_window(crate::app::window_options(), |_window, cx| {
        cx.new(|_cx| SettingsHostRoot)
    })?;
    let window: AnyWindowHandle = window.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_settings_window(acx, window).await;
        eprintln!("[selftest] scenario 'settings-window': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn windows_len(cx: &mut AsyncApp) -> usize {
    cx.update(|app| app.windows().len())
}

fn settings_handle(cx: &mut AsyncApp) -> Option<AnyWindowHandle> {
    cx.update(|app| current_settings_window(app))
}

async fn run_settings_window(cx: &mut AsyncApp, host: AnyWindowHandle) -> CadenceReport {
    // Frontmost/key + painted once before we drive anything.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    let host_id = host.window_id();
    let mut failures: Vec<String> = Vec::new();

    leg_a_singleton(cx, host_id, &mut failures).await;
    leg_b_accent_fanout(cx, host, &mut failures).await;
    leg_c_font_slider(cx, &mut failures).await;
    leg_d_import(cx, &mut failures).await;
    leg_e_rail(cx, &mut failures);
    leg_shortcuts(cx, &mut failures).await;
    leg_composition(cx, &mut failures).await;

    build_report(failures)
}

// === leg (c) — a Font slider fans out to the shared FontSettings ============
async fn leg_c_font_slider(cx: &mut AsyncApp, failures: &mut Vec<String>) {
    let Some(font) = cx.update(|app| crate::keymap::try_shared_font_settings(app)) else {
        failures
            .push("SharedFontSettings was not installed (install_shortcuts) for leg c".to_string());
        return;
    };
    let before_px = cx.update(|app| font.read(app).px());
    let before_metrics = cx.update(|app| font.read(app).metrics());
    // A distinct in-range target so the change is observable.
    let target = if before_px >= 20.0 {
        before_px - 3.0
    } else {
        before_px + 3.0
    };

    // Drive the Font-pane terminal-size handler (the stepper/slider wire).
    cx.update(|app| crate::settings::font_pane::apply_terminal_px(app, target));
    settle(cx, 150).await;

    let after_px = cx.update(|app| font.read(app).px());
    if after_px != target {
        failures.push(format!(
            "the terminal-size slider did not fan out: FontSettings.px() {after_px} != {target}"
        ));
    }
    // The cell metrics re-derive off the new size (a main-window terminal cell
    // re-metrics on the same entity every pane observes).
    let after_metrics = cx.update(|app| font.read(app).metrics());
    if after_metrics == before_metrics {
        failures.push("the size change did not re-metric the shared FontSettings".to_string());
    }

    // A subsequent ⌘= observes the SAME entity — it continues from the slider
    // value (target + 1), proving no desync between the slider and the zoom action.
    cx.update(|app| font.update(app, |f, cx| f.zoom_by(1, cx)));
    settle(cx, 100).await;
    let after_zoom = cx.update(|app| font.read(app).px());
    if after_zoom != target + 1.0 {
        failures.push(format!(
            "⌘= did not continue from the slider value (desync): px {after_zoom} != {}",
            target + 1.0
        ));
    }

    // Persistence (plan leg e): the `fonts` section on disk reflects the slider
    // change — poll the temp `ui_settings.json` the run_selftest store points at.
    let path = cx.update(|app| {
        app.try_global::<crate::settings::prefs_store::SettingsPrefsStore>()
            .map(|s| s.path().to_path_buf())
    });
    match path {
        Some(path) => {
            let on_disk = std::fs::read(&path).ok().and_then(|bytes| {
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|v| v["fonts"]["terminal_font_size"].as_f64())
            });
            if on_disk != Some(target as f64) {
                failures.push(format!(
                    "the fonts section on disk did not reflect the slider change: {on_disk:?} != {target}"
                ));
            }
        }
        None => failures
            .push("the SettingsPrefsStore was not installed for the persistence check".to_string()),
    }

    // Restore the baseline so no font state leaks to a later scenario (the
    // `multiwindow` discipline; `niceties-zoom` asserts a 13pt baseline).
    cx.update(|app| {
        font.update(app, |f, cx| f.reset_to_defaults(cx));
        if let Some(sb) = crate::settings::sidebar_font::shared_sidebar_font(app) {
            sb.update(app, |s, cx| s.reset(cx));
        }
    });
}

// === leg (a) — ⌘, opens the singleton =======================================
async fn leg_a_singleton(cx: &mut AsyncApp, host_id: gpui::WindowId, failures: &mut Vec<String>) {
    let windows_before = windows_len(cx);
    if settings_handle(cx).is_some() {
        failures.push("the SettingsWindow global was already Some before any ⌘,".to_string());
    }

    // First ⌘, — opens the settings window and stores it as the singleton.
    cx.update(|app| open_or_focus_settings(app));
    settle(cx, 400).await;

    let windows_after_open = windows_len(cx);
    if windows_after_open != windows_before + 1 {
        failures.push(format!(
            "⌘, did not open a settings window: App::windows() {windows_before} → {windows_after_open}"
        ));
    }
    let settings_id = match settings_handle(cx) {
        Some(h) => {
            if h.window_id() == host_id {
                failures.push(
                    "the SettingsWindow global points at the host window, not a new one".to_string(),
                );
            }
            Some(h.window_id())
        }
        None => {
            failures
                .push("⌘, opened no settings window (the SettingsWindow global is None)".to_string());
            None
        }
    };

    // Second ⌘, — focuses the SAME window; opens no second one (the singleton).
    cx.update(|app| open_or_focus_settings(app));
    settle(cx, 400).await;
    let windows_after_second = windows_len(cx);
    if windows_after_second != windows_after_open {
        failures.push(format!(
            "a second ⌘, opened another window (singleton broken): App::windows() \
             {windows_after_open} → {windows_after_second}"
        ));
    }
    if let (Some(first), Some(second)) = (settings_id, settings_handle(cx).map(|h| h.window_id())) {
        if first != second {
            failures.push(
                "a second ⌘, replaced the settings-window handle (singleton broken)".to_string(),
            );
        }
    }

    // Close the settings window: the on_window_closed observer must clear the
    // singleton Global so the next ⌘, opens fresh. (`remove_window` is a
    // programmatic close — it bypasses the close-confirmation gate but does fire
    // the close observers, exactly like the multiwindow leg's close of B.)
    let Some(first_handle) = settings_handle(cx) else {
        return; // already reported above; the close/reopen legs need a live handle
    };
    let _ = first_handle.update(cx, |_root, window, _cx| window.remove_window());
    settle(cx, 400).await;
    let windows_after_close = windows_len(cx);
    if windows_after_close != windows_before {
        failures.push(format!(
            "closing the settings window did not drop App::windows() back: \
             {windows_after_open} → {windows_after_close} (expected {windows_before})"
        ));
    }
    if settings_handle(cx).is_some() {
        failures.push(
            "closing the settings window did not clear the SettingsWindow global \
             (the on_window_closed observer did not fire or matched the wrong id)"
                .to_string(),
        );
    }

    // Stale-handle fallthrough: force the Global back to the now-DEAD handle; ⌘,
    // must see it is no longer among the live windows and reopen fresh rather
    // than trying to focus a closed window.
    cx.update(|app| force_settings_handle_for_scenario(app, first_handle));
    cx.update(|app| open_or_focus_settings(app));
    settle(cx, 400).await;
    let windows_after_reopen = windows_len(cx);
    if windows_after_reopen != windows_before + 1 {
        failures.push(format!(
            "⌘, with a stale handle did not reopen a settings window: App::windows() \
             {windows_after_close} → {windows_after_reopen}"
        ));
    }
    match settings_handle(cx) {
        Some(h) if h.window_id() == first_handle.window_id() => failures.push(
            "⌘, with a stale handle kept the dead handle instead of storing a fresh one"
                .to_string(),
        ),
        None => failures.push(
            "⌘, with a stale handle stored no new settings-window handle".to_string(),
        ),
        Some(_) => {}
    }
    // Leave the reopened settings window OPEN — legs (b)/(d) drive the pane's
    // globals while it is up.
}

// === leg (b) — a live Appearance change repaints the real main window =======
async fn leg_b_accent_fanout(
    cx: &mut AsyncApp,
    host: AnyWindowHandle,
    failures: &mut Vec<String>,
) {
    // The store's fresh-install accent is Ocean; pick a DIFFERENT accent so the
    // change is observable (Ocean ↔ Terracotta differ by >100/255 per channel).
    let before = cx.update(|app| theme_settings::active_chrome_accent(app));
    let target = if before == AccentPreset::Terracotta.color() {
        AccentPreset::Ocean
    } else {
        AccentPreset::Terracotta
    };

    // Ground truth: the host window's background paints the live chrome accent, so
    // capture its pixels BEFORE the change. Both samples are read under identical
    // (post-leg-a) window conditions, so the accent-driven per-channel DELTA is the
    // signal — robust to any focus/occlusion dimming that affects both equally.
    let pixels_before = match sample_host(cx, host) {
        Ok(p) => Some(p),
        Err(e) => {
            failures.push(format!("(b) baseline pixel capture failed: {e}"));
            None
        }
    };

    cx.update(|app| theme_settings::apply_accent(app, target));
    settle(cx, 300).await;

    let after = cx.update(|app| theme_settings::active_chrome_accent(app));
    if after != target.color() {
        failures.push(format!(
            "apply_accent did not fan out to the live theme state: active accent \
             {after:?} != {:?}",
            target.color()
        ));
    }
    if after == before {
        failures.push("apply_accent left the active accent unchanged (no fan-out)".to_string());
    }

    // The fan-out must actually REPAINT the shipped window at the pixel level (the
    // plan's Validation §3 / §Scenario leg (b), ±8/255) — not merely flip the store
    // state.
    if let Some(before_px) = pixels_before {
        match sample_host(cx, host) {
            Ok(after_px) => {
                let delta = max_channel_delta(&before_px, &after_px);
                if delta <= 8 {
                    failures.push(format!(
                        "(b) pixel fan-out: apply_accent did not repaint the shipped window \
                         (max channel delta {delta} <= 8)"
                    ));
                } else {
                    eprintln!(
                        "[selftest] settings-window (b): the accent fan-out repainted the host \
                         window (max channel delta {delta})"
                    );
                }
            }
            Err(e) => failures.push(format!("(b) post-accent pixel capture failed: {e}")),
        }
    }

    // Restore the baseline accent so no theme state leaks to a later scenario (the
    // `multiwindow`/font-leg discipline). Re-apply whichever preset the baseline
    // pixel color came from, falling back to the fresh-install default (Terracotta).
    let baseline = AccentPreset::ALL
        .iter()
        .copied()
        .find(|p| p.color() == before)
        .unwrap_or(AccentPreset::Terracotta);
    cx.update(|app| theme_settings::apply_accent(app, baseline));
    settle(cx, 100).await;
}

// === leg (d) — Import through the fake picker ================================
async fn leg_d_import(cx: &mut AsyncApp, failures: &mut Vec<String>) {
    let Some(fake) = selftest_fake() else {
        failures.push("the RecordingFilePicker fake was not installed by run_selftest".to_string());
        return;
    };
    // A temp fixture dir (never the real terminal-themes dir).
    let dir = std::env::temp_dir().join(format!("nice-settings-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if std::fs::create_dir_all(&dir).is_err() {
        failures.push("could not create the temp import-fixture dir".to_string());
        return;
    }

    // --- success: a valid `.ghostty` imports through the seam ---------------
    let good = dir.join("Cool Import.ghostty");
    let _ = std::fs::write(&good, ghostty_source("abcdef"));
    fake.set_next(Some(good.clone()));
    let calls_before = fake.call_count();
    cx.update(|app| perform_import(app));
    settle(cx, 150).await;

    if fake.call_count() <= calls_before {
        failures.push("Import… did not reach the FilePickerOps seam".to_string());
    }
    let imported_ok = cx.update(|app| {
        app.try_global::<TerminalThemeCatalog>()
            .map(|c| {
                let ids: Vec<String> = c.imported_entries().into_iter().map(|e| e.id).collect();
                let in_picker = c
                    .themes(ColorScheme::Dark)
                    .into_iter()
                    .any(|e| e.id == "cool-import");
                ids.contains(&"cool-import".to_string()) && in_picker
            })
            .unwrap_or(false)
    });
    if !imported_ok {
        failures.push(
            "a successful import did not enter imported_entries() / themes(for:)".to_string(),
        );
    }
    // A success clears the inline import feedback.
    if cx.update(|app| last_import_feedback(app)).is_some() {
        failures.push("a successful import left a stale error feedback".to_string());
    }

    // --- failure: a malformed fixture surfaces the mapped §ImportError copy --
    let bad = dir.join("Broken.ghostty");
    let _ = std::fs::write(&bad, "background = nothex\n");
    fake.set_next(Some(bad.clone()));
    cx.update(|app| perform_import(app));
    settle(cx, 150).await;
    match cx.update(|app| last_import_feedback(app)) {
        Some(copy) => {
            if copy.title != "The theme file is invalid" {
                failures.push(format!(
                    "a malformed import mapped to the wrong title: {:?}",
                    copy.title
                ));
            }
            // `background = nothex` fails hex-decode → InvalidHex on line 1.
            if copy.message != "Line 1 contains an invalid color value: `nothex`." {
                failures.push(format!(
                    "a malformed import mapped to the wrong message: {:?}",
                    copy.message
                ));
            }
        }
        None => failures
            .push("a malformed import surfaced no §ImportError feedback".to_string()),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// === legs (s1–s3) — the recorder: rebind, conflict + Replace, Reset ==========
//
// Drives R24's shipped Shortcuts pane recorder over a dedicated host window (the
// live settings window's active-pane state is R23-private; this renders the same
// `shortcuts_pane` body). s1 captures a REAL chord posted to nice's own pid
// (`post_key_tap`), holding a `SavedInputSource` for the whole leg (IME restore).
async fn leg_shortcuts(cx: &mut AsyncApp, failures: &mut Vec<String>) {
    // Real CGEvents ⇒ the TCC grant must be live, else every injected chord is a
    // silently-dropped no-op (fail loudly, never skip).
    if !platform::accessibility_trusted() {
        failures.push(format!("(shortcuts) {SHORTCUTS_ACCESSIBILITY_REMEDIATION}"));
        return;
    }
    // Hold the user's input source for the whole leg (Pinyin is enabled on this
    // machine; a mid-leg failure must not strand it) — restored on drop.
    let _saved = platform::current_input_source();

    // Open the recorder host window (the shipped Shortcuts pane body) and make it
    // key/frontmost so the injected chord routes to its focused recorder.
    let host = match cx.open_window(crate::app::window_options(), |_window, cx| {
        cx.new(|_cx| ShortcutsHostRoot)
    }) {
        Ok(h) => {
            let h: AnyWindowHandle = h.into();
            h
        }
        Err(e) => {
            failures.push(format!("(shortcuts) could not open the recorder host window: {e}"));
            return;
        }
    };
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;
    let _ = host.update(cx, |_root, window, _cx| window.activate_window());
    settle(cx, 300).await;

    let pid = std::process::id() as i32;

    // --- s1: enter recording on newTerminalPane, post a free ⌘Y, assert rebind ---
    let _ = host.update(cx, |_root, window, cx| {
        enter_record(window, cx, ShortcutAction::NewTerminalPane)
    });
    settle(cx, 200).await;
    if cx.update(|app| recording_action(app)) != Some(ShortcutAction::NewTerminalPane) {
        failures.push("(s1) enter_record did not put newTerminalPane into capture mode".to_string());
    }
    // Re-assert frontmost right before the chord so the CGEvent routes to the host.
    let _ = host.update(cx, |_root, window, _cx| window.activate_window());
    settle(cx, 150).await;
    platform::post_key_tap(pid, KC_Y, platform::FLAG_COMMAND, None);
    settle(cx, 300).await;

    let cmd_y = OwnedCombo::from_token("cmd-y");
    let bound = cx.update(|app| app.try_global::<ShortcutBindings>().and_then(|s| s.binding(ShortcutAction::NewTerminalPane)));
    if bound != cmd_y {
        failures.push(format!(
            "(s1) the real ⌘Y chord did not rebind newTerminalPane: binding is {bound:?}, expected cmd-y \
             (recorder did not capture — is the host key/focused?)"
        ));
    }
    // A committed capture returns to rest (the new pills would render from the store).
    if cx.update(|app| recording_action(app)).is_some() {
        failures.push("(s1) the recorder did not tear down after committing the capture".to_string());
    }

    // --- s2: capture a conflicting chord (⌘B = ToggleSidebar), then Replace -------
    let _ = host.update(cx, |_root, window, cx| {
        enter_record(window, cx, ShortcutAction::NewTerminalPane)
    });
    settle(cx, 200).await;
    let _ = host.update(cx, |_root, window, _cx| window.activate_window());
    settle(cx, 150).await;
    platform::post_key_tap(pid, KC_B, platform::FLAG_COMMAND, None);
    settle(cx, 300).await;

    match cx.update(|app| conflict_action(app)) {
        Some(ShortcutAction::ToggleSidebar) => {
            // The frozen "Already used by <label>" copy.
            let msg = conflict_message(ShortcutAction::ToggleSidebar);
            if msg != "Already used by Toggle sidebar" {
                failures.push(format!("(s2) the conflict copy is wrong: {msg:?}"));
            }
            // The conflict blocked commit — still recording, not yet bound.
            if cx.update(|app| recording_action(app)) != Some(ShortcutAction::NewTerminalPane) {
                failures.push("(s2) a conflict did not keep the recorder in capture mode".to_string());
            }
        }
        other => failures.push(format!(
            "(s2) the ⌘B conflict did not surface ToggleSidebar as the loser: conflict is {other:?}"
        )),
    }

    // Drive Replace: the loser (ToggleSidebar) is unbound AND newTerminalPane takes ⌘B.
    cx.update(|app| resolve_replace(app));
    settle(cx, 200).await;
    if cx.update(|app| app.try_global::<ShortcutBindings>().and_then(|s| s.binding(ShortcutAction::ToggleSidebar))).is_some() {
        failures.push("(s2) Replace did not clear the losing action (ToggleSidebar still bound)".to_string());
    }
    if cx.update(|app| app.try_global::<ShortcutBindings>().and_then(|s| s.binding(ShortcutAction::NewTerminalPane))) != OwnedCombo::from_token("cmd-b") {
        failures.push("(s2) Replace did not bind newTerminalPane to the conflicting combo (cmd-b)".to_string());
    }
    if cx.update(|app| recording_action(app)).is_some() {
        failures.push("(s2) the recorder did not tear down after Replace".to_string());
    }

    // --- s3: Reset newTerminalPane → the default (⌘T), and the Reset button hides -
    cx.update(|app| reset_action(app, ShortcutAction::NewTerminalPane));
    settle(cx, 150).await;
    if cx.update(|app| app.try_global::<ShortcutBindings>().and_then(|s| s.binding(ShortcutAction::NewTerminalPane))) != OwnedCombo::from_token("cmd-t") {
        failures.push("(s3) Reset did not restore newTerminalPane to its default (cmd-t)".to_string());
    }
    // `is_at_default` drives the Reset button's visibility — true ⇒ the button hides.
    if !cx.update(|app| app.try_global::<ShortcutBindings>().map(|s| s.is_at_default(ShortcutAction::NewTerminalPane)).unwrap_or(false)) {
        failures.push("(s3) newTerminalPane is not is_at_default after Reset (the Reset button would still show)".to_string());
    }

    // Cleanup: force any stranded capture down (restores the keymap), reset every
    // action this leg touched to its default so nothing leaks to `multiwindow`, and
    // close the host window.
    cx.update(|app| {
        cancel_capture(app);
        reset_action(app, ShortcutAction::NewTerminalPane);
        reset_action(app, ShortcutAction::ToggleSidebar);
    });
    let _ = host.update(cx, |_root, window, _cx| window.remove_window());
    settle(cx, 200).await;
}

// === §6 final composition — the shipped-surface Milestone-6 board ===========
//
// Drives the REAL registered launch window (`open_managed_window` /
// `build_window_root`, the exact path `run` takes) — NOT a scenario host — to
// compose the whole tranche's board (Validation §6a–d): a REBOUND chord dispatches
// on the shipped window, the PROTECTED non-rebindable set survives the rebuild, ⌘,
// opens R23's settings window and a live theme change repaints shipped chrome + a
// terminal cell (R21 fan-out through the store `apply_*`), and a busy pane close
// presents R20.5's ConfirmationModal. Real CGEvents (⌘Y / ⌘T / ⌘,) post to
// nice's OWN pid; a `SavedInputSource` is held for the whole leg (IME restore).

/// A hermetic fixture for the composition window's shell (a spec-wins ZDOTDIR rc
/// chain + a sandbox HOME + a `NICE_CLAUDE_OVERRIDE` idle stub) — never the real
/// `~` / `claude`. Mirrors the `close-confirmation` / `theme-fanout` fixtures.
struct CompFixture {
    base: PathBuf,
    home: PathBuf,
    zdotdir: PathBuf,
}

impl CompFixture {
    fn build() -> Result<Self> {
        let base =
            std::env::temp_dir().join(format!("nice-settings-comp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base)?;
        let base = base.canonicalize()?;
        let home = base.join("home");
        let zdotdir = base.join("zdotdir");
        for d in [&home, &zdotdir] {
            std::fs::create_dir_all(d)?;
        }
        // The R14 ZDOTDIR blanked stub chain (a spec-wins rc chain — no user rc).
        crate::shell_inject::write_stubs(&zdotdir)?;
        // A stub `claude` that idles — this leg never spawns Claude, but
        // NICE_CLAUDE_OVERRIDE must be set so nothing can reach the real binary.
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin)?;
        let stub = bin.join("claude");
        std::fs::write(&stub, "#!/bin/sh\nwhile IFS= read -r _l; do : ; done\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
        }
        // SAFETY: single-threaded scenario setup before any pane forks.
        unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };
        Ok(CompFixture { base, home, zdotdir })
    }
}

// Points over the shipped 960×640 window's CHROME (the ~52pt top bar + the left
// sidebar card) — a scheme flip repaints the whole chrome palette, so several
// interior points make the recolor unmistakable and dodge anti-alias jitter.
const COMP_CHROME_POINTS: &[(f32, f32)] = &[(480.0, 26.0), (60.0, 520.0), (140.0, 580.0)];
// Points inside the terminal region (right of the ~240pt sidebar card, below the
// ~52pt top bar) — sampled low/right to dodge the prompt at the grid's top-left.
const COMP_TERMINAL_POINTS: &[(f32, f32)] = &[(600.0, 320.0), (720.0, 440.0), (820.0, 560.0)];

fn comp_sample(cx: &mut AsyncApp, window: AnyWindowHandle, points: &[(f32, f32)]) -> Result<Vec<[u8; 4]>> {
    nice_harness::capture::sample_window_pixels(window, cx, points)
}

fn comp_active_pane_count(cx: &mut AsyncApp, state: &Entity<WindowState>) -> usize {
    state.update(cx, |s, _| {
        s.model
            .active_tab_id()
            .and_then(|id| s.model.tab_for(id))
            .map_or(0, |t| t.panes.len())
    })
}

fn comp_active_tab_and_pane(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
) -> Option<(String, String)> {
    state.update(cx, |s, _| {
        let tab = s.model.active_tab_id()?.to_string();
        let pane = s.model.tab_for(&tab).and_then(|t| t.active_pane_id.clone())?;
        Some((tab, pane))
    })
}

/// Post one key tap (with `flags`) to our own pid, then settle so AppKit dispatches
/// it into the key window before the next event.
async fn comp_tap(cx: &mut AsyncApp, pid: i32, keycode: u16, flags: u64) {
    platform::post_key_tap(pid, keycode, flags, None);
    settle(cx, 150).await;
}

/// The PROTECTED non-rebindable set still present in the LIVE keymap after the
/// rebind — the biggest-regression audit (`rebuild_keymap`'s total clear must have
/// re-installed each one). Returns the labels of any that are MISSING. Mirrors the
/// `keymap::rebuild_keeps_non_rebindables_and_swaps_live_combo` probe.
fn comp_missing_non_rebindables(cx: &mut AsyncApp) -> Vec<&'static str> {
    cx.update(|app| {
        let keymap = app.key_bindings();
        let keymap = keymap.borrow();
        let bound = |action: &dyn Action, chord: &str| -> bool {
            match Keystroke::parse(chord) {
                Ok(ks) => keymap
                    .bindings_for_action(action)
                    .any(|b| matches!(b.match_keystrokes(std::slice::from_ref(&ks)), Some(false))),
                Err(_) => false,
            }
        };
        let mut missing = Vec::new();
        if !bound(&crate::app::ToggleFullScreen, "ctrl-cmd-f") {
            missing.push("⌃⌘F");
        }
        if !bound(&crate::app::NewWindow, "cmd-n") {
            missing.push("⌘N");
        }
        if !bound(&crate::app::Quit, "cmd-q") {
            missing.push("⌘Q");
        }
        if !bound(&crate::app::CloseWindow, "cmd-w") {
            missing.push("⌘W");
        }
        if !bound(&crate::sidebar_shell::CollapseSidebarSelection, "escape") {
            missing.push("Esc@SidebarShell");
        }
        if !bound(&crate::settings::window::OpenSettings, "cmd-,") {
            missing.push("⌘,");
        }
        missing
    })
}

/// Reap the launch window's shells, reset the rebound action to its default (so
/// nothing leaks to `multiwindow`), remove the window, and restore HOME/env +
/// remove the fixture.
async fn comp_finish(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    win_id: gpui::WindowId,
    prev_home: &Option<String>,
    fixture: &CompFixture,
) {
    let _ = cx.update(|app| {
        // Restore the rebound action so the later `multiwindow` scenario's ⌘T works.
        ShortcutBindings::reset(app, ShortcutAction::NewTerminalPane);
        if let Some(state) = WindowRegistry::state_for_window(app, win_id) {
            state.update(app, |s, _| s.teardown());
        }
        crate::app::set_scenario_shell_inject_config(app, None, None);
    });
    let _ = whandle.update(cx, |_root, window, _cx| window.remove_window());
    settle(cx, 200).await;
    // SAFETY: teardown, single-threaded.
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        std::env::remove_var("NICE_CLAUDE_OVERRIDE");
    }
    let _ = std::fs::remove_dir_all(&fixture.base);
}

async fn leg_composition(cx: &mut AsyncApp, failures: &mut Vec<String>) {
    // Real CGEvents (⌘Y / ⌘T / ⌘,) ⇒ the TCC grant must be live, else every
    // injected chord is a silently-dropped no-op (fail loudly, never skip).
    if !platform::accessibility_trusted() {
        failures.push(format!("(6) {SHORTCUTS_ACCESSIBILITY_REMEDIATION}"));
        return;
    }
    // Hold the user's input source for the whole leg (Pinyin is enabled on this
    // machine; a mid-leg failure must not strand it) — restored on drop.
    let _saved = platform::current_input_source();
    let pid = std::process::id() as i32;

    // Close any settings window a prior leg (leg a) left open so §6c's ⌘, opens a
    // FRESH one (a clean `App::windows()` step-up).
    if let Some(h) = settings_handle(cx) {
        let _ = h.update(cx, |_root, window, _cx| window.remove_window());
        settle(cx, 200).await;
    }

    // --- open the REAL shipped launch window (hermetic shell) ----------------
    let fixture = match CompFixture::build() {
        Ok(f) => f,
        Err(e) => {
            failures.push(format!("(6) could not build the composition fixture: {e}"));
            return;
        }
    };
    let home = fixture.home.to_string_lossy().into_owned();
    let zdotdir = fixture.zdotdir.to_string_lossy().into_owned();
    let prev_home = std::env::var("HOME").ok();

    let opened: Result<WindowHandle<AppShellView>> = cx.update(|app| {
        crate::app::set_scenario_shell_inject_config(app, Some(zdotdir.clone()), None);
        // SAFETY: single-threaded scenario setup; HOME set only across the open, the
        // ZDOTDIR chain (spec-wins) is what keeps the shell hermetic.
        unsafe { std::env::set_var("HOME", &home) };
        let opened = crate::app::open_managed_window(app);
        // SAFETY: restore HOME immediately.
        unsafe {
            match &prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
        opened.map_err(anyhow::Error::from)
    });
    let whandle = match opened {
        Ok(h) => h,
        Err(e) => {
            failures.push(format!("(6) open_managed_window failed: {e}"));
            let _ = cx.update(|app| crate::app::set_scenario_shell_inject_config(app, None, None));
            // SAFETY: teardown, single-threaded.
            unsafe {
                match &prev_home {
                    Some(h) => std::env::set_var("HOME", h),
                    None => std::env::remove_var("HOME"),
                }
                std::env::remove_var("NICE_CLAUDE_OVERRIDE");
            }
            let _ = std::fs::remove_dir_all(&fixture.base);
            return;
        }
    };
    let any: AnyWindowHandle = whandle.into();
    let win_id = any.window_id();

    // Frontmost/key + painted before we drive anything.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, win_id)) else {
        failures
            .push("(6) the shipped builder did not register the launch window's WindowState".into());
        comp_finish(cx, whandle, win_id, &prev_home, &fixture).await;
        return;
    };
    let shell = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => {
            failures.push(format!("(6) could not read the launch window's shell view: {e}"));
            comp_finish(cx, whandle, win_id, &prev_home, &fixture).await;
            return;
        }
    };
    let toolbar = shell.update(cx, |s, _| s.scenario_toolbar());
    let pane_host: Entity<PaneHostView> = shell.update(cx, |s, _| s.scenario_pane_host());
    let Some((main_tab, main_pane)) = comp_active_tab_and_pane(cx, &state) else {
        failures.push("(6) the shipped launch window has no active tab/pane".into());
        comp_finish(cx, whandle, win_id, &prev_home, &fixture).await;
        return;
    };

    // Poll the Main pane's TerminalView mounted (the fan-out target for §6c). §6a/
    // §6b/§6d do not need it painted; only §6c's pixel sample does.
    let mut mounted = false;
    for _ in 0..40 {
        if pane_host
            .update(cx, |h, _| h.scenario_terminal_for(&main_pane))
            .is_some()
        {
            mounted = true;
            break;
        }
        settle(cx, 100).await;
    }

    // === §6a — a REBOUND chord dispatches on the shipped window ==============
    cx.update(|app| {
        ShortcutBindings::set_binding(
            app,
            ShortcutAction::NewTerminalPane,
            OwnedCombo::from_token("cmd-y"),
        )
    });
    settle(cx, 150).await;
    let _ = whandle.update(cx, |_root, window, _cx| window.activate_window());
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;

    let panes_before = comp_active_pane_count(cx, &state);
    comp_tap(cx, pid, KC_Y, platform::FLAG_COMMAND).await;
    settle(cx, 300).await;
    let panes_after_y = comp_active_pane_count(cx, &state);
    if panes_after_y != panes_before + 1 {
        failures.push(format!(
            "(6a) the rebound ⌘Y did not add a pane on the shipped launch window: active-tab pane \
             count {panes_before} → {panes_after_y} (is the window key/focused?)"
        ));
    } else {
        eprintln!("[selftest] settings-window (6a): the rebound ⌘Y dispatched NewTerminalPane on the shipped window");
    }
    // The OLD default ⌘T no longer dispatches (rebuild_keymap dropped it).
    let panes_before_t = comp_active_pane_count(cx, &state);
    comp_tap(cx, pid, KC_T, platform::FLAG_COMMAND).await;
    settle(cx, 250).await;
    let panes_after_t = comp_active_pane_count(cx, &state);
    if panes_after_t != panes_before_t {
        failures.push(format!(
            "(6a) the old default ⌘T still added a pane after the rebind: {panes_before_t} → \
             {panes_after_t} (rebuild_keymap did not drop the old combo)"
        ));
    }

    // === §6b — the PROTECTED non-rebindable set survives the rebuild =========
    let missing = comp_missing_non_rebindables(cx);
    if !missing.is_empty() {
        failures.push(format!(
            "(6b) non-rebindable(s) missing from the live keymap after the rebind: {missing:?} — \
             rebuild_keymap's total clear did not re-install them"
        ));
    }

    // === §6c — ⌘, opens R23's settings window (a non-rebindable firing live) =
    let win_before = windows_len(cx);
    let _ = whandle.update(cx, |_root, window, _cx| window.activate_window());
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;
    comp_tap(cx, pid, KC_COMMA, platform::FLAG_COMMAND).await;
    settle(cx, 400).await;
    let settings_opened = settings_handle(cx).is_some();
    let win_after = windows_len(cx);
    if !settings_opened || win_after != win_before + 1 {
        failures.push(format!(
            "(6c) ⌘, did not open R23's settings window on the shipped surface (the OpenSettings \
             non-rebindable did not fire live): settings_handle={settings_opened}, App::windows() \
             {win_before} → {win_after}"
        ));
    } else {
        eprintln!("[selftest] settings-window (6c): ⌘, opened R23's settings window on the shipped surface");
    }
    // Close it so the pixel sample reads the frontmost launch window, not settings.
    if let Some(h) = settings_handle(cx) {
        let _ = h.update(cx, |_root, window, _cx| window.remove_window());
        settle(cx, 300).await;
    }

    // === §6c — a live theme change repaints shipped chrome + a terminal cell =
    if mounted {
        let _ = whandle.update(cx, |_root, window, _cx| window.activate_window());
        let _ = cx.update(|app| app.activate(true));
        settle(cx, 400).await;

        let chrome_before = comp_sample(cx, any, COMP_CHROME_POINTS);
        let term_before = comp_sample(cx, any, COMP_TERMINAL_POINTS);
        let baseline_accent = cx.update(|app| theme_settings::active_chrome_accent(app));
        let target_accent = if baseline_accent == AccentPreset::Terracotta.color() {
            AccentPreset::Ocean
        } else {
            AccentPreset::Terracotta
        };
        let baseline_scheme = cx.update(|app| app.global::<ThemeSettingsStore>().appearance().scheme);
        let target_scheme = match baseline_scheme {
            ColorScheme::Dark => ColorScheme::Light,
            ColorScheme::Light => ColorScheme::Dark,
        };

        // Exercise R21's accent picker (chrome tint), then flip the scheme — the
        // live theme change that repaints BOTH chrome and the terminal. (The stub
        // selftest catalog resolves every id to the scheme's Nice default, so
        // apply_terminal_theme_id is a latent no-op here — the terminal-cell recolor
        // comes from the scheme flip, exactly as `theme-fanout` leg (a) proves;
        // R22's distinct themes make apply_terminal_theme_id visible there.)
        cx.update(|app| theme_settings::apply_accent(app, target_accent));
        settle(cx, 200).await;
        cx.update(|app| theme_settings::apply_scheme(app, target_scheme));
        settle(cx, 400).await;

        match (chrome_before, comp_sample(cx, any, COMP_CHROME_POINTS)) {
            (Ok(before), Ok(after)) => {
                let delta = max_channel_delta(&before, &after);
                if delta <= 8 {
                    failures.push(format!(
                        "(6c) the live theme change did not repaint the shipped chrome (max channel \
                         delta {delta} <= 8)"
                    ));
                } else {
                    eprintln!("[selftest] settings-window (6c): the theme change repainted the shipped chrome (max channel delta {delta})");
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                failures.push(format!("(6c) chrome pixel capture failed: {e}"))
            }
        }
        match (term_before, comp_sample(cx, any, COMP_TERMINAL_POINTS)) {
            (Ok(before), Ok(after)) => {
                let delta = max_channel_delta(&before, &after);
                if delta <= 8 {
                    failures.push(format!(
                        "(6c) the live theme change did not recolor a terminal cell on the shipped \
                         window (max channel delta {delta} <= 8)"
                    ));
                } else {
                    eprintln!("[selftest] settings-window (6c): the theme change recolored a live terminal cell (max channel delta {delta})");
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                failures.push(format!("(6c) terminal pixel capture failed: {e}"))
            }
        }

        // Restore the theme baseline so nothing leaks to `multiwindow`.
        let baseline_preset = AccentPreset::ALL
            .iter()
            .copied()
            .find(|p| p.color() == baseline_accent)
            .unwrap_or(AccentPreset::Ocean);
        cx.update(|app| {
            theme_settings::apply_scheme(app, baseline_scheme);
            theme_settings::apply_accent(app, baseline_preset);
        });
        settle(cx, 200).await;
    } else {
        failures.push(
            "(6c) the launch window's Main TerminalView never mounted — no terminal to assert the \
             theme fan-out reaches"
                .into(),
        );
    }

    // === §6d — R20.5 gates a busy pane close ================================
    // Add a fresh terminal pane, mark it busy through the `synthetic_foreground_child`
    // seam (the TRUE tcgetpgrp read is covered once in `close-confirmation`), then
    // drive its close: R20.5's ConfirmationModal must interpose (a veto, no close).
    let before_ids = toolbar.update(cx, |v, cx| v.pane_ids(cx));
    let _ = toolbar.update(cx, |v, cx| v.drive_add_terminal_pane(cx));
    settle(cx, 300).await;
    let after_ids = toolbar.update(cx, |v, cx| v.pane_ids(cx));
    match after_ids.into_iter().find(|p| !before_ids.contains(p)) {
        Some(busy_pane) => {
            cx.update(|app| {
                state.update(app, |s, _| {
                    s.session
                        .mark_synthetic_foreground_child(&main_tab, &busy_pane)
                })
            });
            let _ = whandle.update(cx, |_root, window, app| {
                toolbar.update(app, |v, cx| v.drive_close_pane(&busy_pane, window, cx));
            });
            settle(cx, 300).await;

            let mut modal_up = false;
            for _ in 0..30 {
                if state.update(cx, |s, _| s.pending_modal().is_some()) {
                    modal_up = true;
                    break;
                }
                settle(cx, 100).await;
            }
            if !modal_up {
                failures.push(format!(
                    "(6d) closing the busy pane {busy_pane} presented NO confirmation modal (R20.5's \
                     busy gate did not interpose)"
                ));
            } else if !toolbar.update(cx, |v, cx| v.pane_ids(cx)).contains(&busy_pane) {
                failures.push(format!(
                    "(6d) the busy close removed pane {busy_pane} without confirmation (no veto)"
                ));
            } else {
                // The confirm ("Force quit") button surfaces as a live AXButton.
                let mut ax_ok = false;
                for _ in 0..30 {
                    let _ = state.update(cx, |_s, c| c.notify());
                    settle(cx, 100).await;
                    if matches!(
                        platform::ax_find_titled_role(pid, crate::confirmation_modal::CONFIRM_ACCEPT_ID),
                        Ok(role) if role == "AXButton"
                    ) {
                        ax_ok = true;
                        break;
                    }
                }
                if !ax_ok {
                    failures.push(
                        "(6d) R20.5's confirm button never surfaced as an AXButton in the AX tree"
                            .into(),
                    );
                } else {
                    eprintln!("[selftest] settings-window (6d): a busy pane close presented R20.5's confirmation (live AXButton)");
                }
            }
            // Cancel — the composed leg makes no destructive close.
            let modal = state.update(cx, |s, _| s.pending_modal());
            if let Some(modal) = modal {
                let _ = whandle.update(cx, |_root, window, app| {
                    modal.update(app, |m, mcx| m.resolve(false, window, mcx))
                });
            }
            settle(cx, 200).await;
        }
        None => failures.push(
            "(6d) could not add a terminal pane to mark busy for the close-gate assert".into(),
        ),
    }

    comp_finish(cx, whandle, win_id, &prev_home, &fixture).await;
}

// === leg (e) — the rail exposes the six slugs ===============================
fn leg_e_rail(_cx: &mut AsyncApp, failures: &mut Vec<String>) {
    let slugs: Vec<&str> = settings_rail_sections().iter().map(|(s, _)| *s).collect();
    let expected = ["appearance", "shortcuts", "font", "claude", "advanced", "about"];
    if slugs != expected {
        failures.push(format!(
            "the settings rail is not the six-slug order minus Editors: {slugs:?}"
        ));
    }
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "settings-window OK: ⌘, opened one settings window (singleton), a live \
                     Appearance accent change repainted the shipped host window (pixel recolor \
                     >8/255) + fanned out to the theme state, a Font-slider change \
                     fanned out to the shared FontSettings + persisted to disk (⌘= stayed in sync), \
                     Import ran through the fake picker (success + a mapped §ImportError), the \
                     rail exposes the six slugs, and the Shortcuts recorder rebound newTerminalPane \
                     via a real ⌘Y chord (s1), surfaced + Replace-resolved a ⌘B conflict with \
                     ToggleSidebar (s2), and Reset restored the default (s3); and the §6 \
                     final-composition board held on the REAL launch window — a rebound ⌘Y \
                     dispatched (old ⌘T dead) (6a), the non-rebindable set survived the rebuild \
                     (6b), ⌘, opened R23's settings + a live theme change repainted shipped chrome \
                     AND a terminal cell (6c), and a busy pane close presented R20.5's confirmation \
                     (6d)"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} settings-window assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
