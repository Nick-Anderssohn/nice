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
//!   `appearance … about`, incl. the `shortcuts` placeholder (the R24 seam).
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

use std::time::Duration;

use anyhow::Result;
use gpui::{div, prelude::*, AnyWindowHandle, AppContext as _, AsyncApp, Context, Render, Window};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_theme::palette::ColorScheme;
use nice_theme::AccentPreset;

use crate::settings::appearance_pane::{last_import_feedback, perform_import};
use crate::settings::file_picker::selftest_fake;
use crate::settings::root::settings_rail_sections;
use crate::settings::window::{
    current_settings_window, force_settings_handle_for_scenario, install_open_settings_command,
    open_or_focus_settings,
};
use crate::terminal_theme_catalog::TerminalThemeCatalog;
use crate::theme_settings::{
    self, OsSchemeSource, SharedThemeState, ThemeSettingsStore, ThemeState,
};

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
    // pixel color came from, falling back to the fresh-install default (Ocean).
    let baseline = AccentPreset::ALL
        .iter()
        .copied()
        .find(|p| p.color() == before)
        .unwrap_or(AccentPreset::Ocean);
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
    let dir = std::env::temp_dir().join(format!("nice-rs-settings-import-{}", std::process::id()));
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
                     Import ran through the fake picker (success + a mapped §ImportError), and the \
                     rail exposes the six slugs incl. the shortcuts placeholder"
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
