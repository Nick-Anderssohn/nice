//! `theme-fanout` self-test scenario — the R21 live theme-system gate.
//!
//! Drives the **shipped window** (`open_managed_window` / `build_window_root`, the
//! exact path `crate::app::run` takes) with the live theme globals installed, then
//! exercises the store `apply_*` mutators + the OS-sync reconcile and asserts the
//! fan-out reaches BOTH halves — chrome (the active `Slots`) and a live terminal
//! pane (the pushed `TerminalTheme` + a ground-truth pixel recolor) — plus the
//! R17-live Claude mirror.
//!
//! ## Legs (all fail-loud, state-poll bounded — never sleep-and-hope)
//!
//! * **(a/d) OS-sync scheme flip fans chrome + terminal.** With `sync_with_os` ON,
//!   flipping the injected [`OsSchemeSource`](crate::theme_settings::OsSchemeSource)
//!   stub and reconciling flips `scheme` (the adapter's job): the active chrome
//!   `Slots` change, the Main pane's `TerminalView` swaps its render theme, and a
//!   pixel sample on the live terminal recolors. With sync OFF, driving the stub is
//!   a no-op (`reconcile_with_os` gates on `sync_with_os`).
//! * **(d) manual contradiction turns sync off.** Re-enabling sync pins the scheme
//!   to the stub OS; a manual `apply_scheme` to the OTHER scheme turns
//!   `sync_with_os` off (the `userPicked` analog).
//! * **(b) accent recolors the caret.** `apply_accent` pushes a new accent into the
//!   pane; on the cursor-None Nice theme the accent IS the caret color, so the
//!   view's accent updates.
//! * **(c) terminal-theme-id: inactive is latent, the flip applies it.** Setting the
//!   INACTIVE scheme's terminal-id does not recolor the pane (persisted, latent);
//!   the next scheme flip makes that slot active and the pane picks it up. (The R21
//!   stub catalog resolves every id to the Nice default for a scheme, so a
//!   same-scheme active-id change is a visual no-op by construction — the active
//!   path is exercised through the scheme flip, which changes the active slot's
//!   resolution; R22's distinct themes make a same-scheme active recolor visible.)
//! * **(e) R17-live Claude mirror.** With the gate ON a theme change rewrites the
//!   sandbox `nice.json` colors file (byte-diff via the landed only-if-changed
//!   writer); `apply_sync_claude_theme` re-sources every window's `--settings`
//!   provider so a subsequently-spawned pane would get / lose the flag.
//! * **(f) R22 Ghostty import end-to-end.** A fixture `.ghostty` written under the
//!   sandbox support root is `import_theme`d through the Global catalog (parse →
//!   persist verbatim as `<slug>.ghostty` → enter the imported list), resolves by
//!   id, and — once `apply_terminal_theme_id` (R21) makes it live for the active
//!   scheme — recolors the live terminal pane (render theme swap + a pixel sample),
//!   proving parse → persist → catalog → resolve → fan-out on a real window.
//!
//! ## Hermeticity (tranche-5 rule)
//!
//! Fully sandboxed: an explicit temp theme-store path (never the real
//! `ui_settings.json`), a fake `$HOME` (so the Claude colors + pointer files land
//! under the sandbox, never `~/.claude` / `~/.nice`), a blanked `ZDOTDIR` rc chain
//! for the Main pane's shell, a `NICE_CLAUDE_OVERRIDE` stub, and an INJECTED
//! `OsSchemeSource` stub (no leg reads the real system appearance). It mints its OWN
//! `SharedThemeState` + `OsSchemeSource` (`run_selftest` installs neither), and
//! installs no `WindowRegistry` close observer (its `build_window_root` only
//! `register`s), so it is registered BEFORE `multiwindow` (which owns the
//! quit-when-empty terminus and must be last).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AppContext as _, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_theme::palette::{ColorScheme, Slots};
use nice_theme::AccentPreset;
use nice_term_view::{TerminalColor, TerminalTheme};

use crate::app_shell::{AppShellView, PaneHostView};
use crate::terminal_theme_catalog::TerminalThemeCatalog;
use crate::theme_settings::{
    self, OsSchemeSource, SharedThemeState, ThemeSettingsStore, ThemeState,
};
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- fixture -----------------------------------------------------------------

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    zdotdir: PathBuf,
    theme_store_path: PathBuf,
    /// The imported terminal-theme storage dir (R22) — the catalog enumerates it
    /// at boot and `import_theme` persists into it. Under the temp base, never the
    /// real `terminal-themes/` (hermeticity).
    terminal_themes_dir: PathBuf,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-theme-fanout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let zdotdir = base.join("zdotdir");
        for d in [&home, &zdotdir] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }
        // The R14 ZDOTDIR blanked stub chain (a spec-wins rc chain — no user rc).
        crate::shell_inject::write_stubs(&zdotdir).context("write ZDOTDIR stubs")?;

        // A stub `claude` that idles — this scenario never spawns Claude, but
        // NICE_CLAUDE_OVERRIDE must be set so no leg can reach the real binary.
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

        let theme_store_path = base.join("ui_settings.json");
        let terminal_themes_dir = base.join("terminal-themes");
        Ok(Fixture {
            base,
            home,
            zdotdir,
            theme_store_path,
            terminal_themes_dir,
        })
    }

    /// The sandbox Claude colors file the R17-live write lands at
    /// (`<home>/.claude/themes/<slug>.json`).
    fn claude_colors_file(&self) -> PathBuf {
        self.home
            .join(".claude")
            .join("themes")
            .join(format!("{}.json", crate::claude_theme_sync::SLUG))
    }
}

// -- OS-scheme stub ----------------------------------------------------------

const OS_LIGHT: u8 = 0;
const OS_DARK: u8 = 1;

fn u8_to_scheme(v: u8) -> ColorScheme {
    if v == OS_LIGHT {
        ColorScheme::Light
    } else {
        ColorScheme::Dark
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the shipped `theme-fanout` window and spawn its driver. Installs the live
/// theme globals (store at a temp path, catalog stub, a freshly-minted
/// `SharedThemeState`, an injected `OsSchemeSource` stub) BEFORE opening — so the
/// Main pane seeds from the live theme — then opens exactly as `crate::app::run`
/// does (no `WindowRegistry` install). `$HOME` is sandboxed for the WHOLE driver
/// (the R17-live Claude writes read it) and restored at teardown.
pub fn open_theme_fanout_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home.to_string_lossy().into_owned();
    let zdotdir = fixture.zdotdir.to_string_lossy().into_owned();
    let theme_path = fixture.theme_store_path.clone();
    let terminal_themes_dir = fixture.terminal_themes_dir.clone();
    // The injected OS scheme stub, shared with the driver so it can flip it. Seeded
    // Dark (matches the store's fresh-install placeholder, so the boot reconcile is
    // a no-op and the baseline is deterministic).
    let os_cell = Arc::new(AtomicU8::new(OS_DARK));

    let prev_home = std::env::var("HOME").ok();
    let whandle: WindowHandle<AppShellView> = cx.update(|app| -> Result<_> {
        crate::keymap::install_shortcuts(app);
        crate::app::set_scenario_shell_inject_config(app, Some(zdotdir.clone()), None);

        // Mint the live theme globals the shipped builder reads (run_selftest
        // installs a defaults store + catalog but NO SharedThemeState / OsSchemeSource
        // — a scenario opting into live theming mints them itself). Override the
        // defaults store with one at the sandbox path so apply_* persists hermetically.
        let store = ThemeSettingsStore::load(theme_path.clone());
        // R22: the catalog over the sandbox terminal-themes dir (empty at boot),
        // so the import leg's `import_theme` persists + resolves hermetically.
        let catalog = TerminalThemeCatalog::new(terminal_themes_dir.clone());
        let entity = {
            let state = ThemeState::from_stores(&store, &catalog);
            app.new(|_| state)
        };
        app.set_global(store);
        app.set_global(catalog);
        app.set_global(SharedThemeState(entity));
        let cell = os_cell.clone();
        app.set_global(OsSchemeSource::new(move |_| u8_to_scheme(cell.load(Ordering::SeqCst))));
        // The Claude theme-sync gate starts OFF; leg (e) flips it ON via the live
        // toggle (never from run_selftest — hermeticity).
        crate::app::set_claude_theme_sync_gate(app, false);

        // SAFETY: single-threaded scenario setup; HOME held for the driver (the
        // R17-live Claude writes read it), restored only on the failure path here or
        // at driver teardown.
        unsafe { std::env::set_var("HOME", &home) };
        let opened = crate::app::open_managed_window(app);
        if opened.is_err() {
            // SAFETY: single-threaded; the driver never runs, so restore HOME now.
            unsafe {
                match &prev_home {
                    Some(h) => std::env::set_var("HOME", h),
                    None => std::env::remove_var("HOME"),
                }
                std::env::remove_var("NICE_CLAUDE_OVERRIDE");
            }
        }
        opened.map_err(anyhow::Error::from)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_theme_fanout(acx, whandle, &fixture, os_cell).await;
        // Reap the Main pane's shell, reset the scenario config, restore HOME/env.
        let id = AnyWindowHandle::from(whandle).window_id();
        let _ = acx.update(|app| {
            if let Some(state) = WindowRegistry::state_for_window(app, id) {
                state.update(app, |s, _| s.teardown());
            }
            crate::app::set_scenario_shell_inject_config(app, None, None);
        });
        // SAFETY: teardown, single-threaded.
        unsafe {
            match &prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            std::env::remove_var("NICE_CLAUDE_OVERRIDE");
        }
        let _ = std::fs::remove_dir_all(&fixture.base);
        eprintln!("[selftest] scenario 'theme-fanout': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

// -- reads -------------------------------------------------------------------

fn active_slots(cx: &mut AsyncApp) -> Slots {
    cx.update(|app| theme_settings::active_chrome_slots(app))
}

fn store_scheme(cx: &mut AsyncApp) -> ColorScheme {
    cx.update(|app| app.global::<ThemeSettingsStore>().appearance().scheme)
}

fn store_sync_on(cx: &mut AsyncApp) -> bool {
    cx.update(|app| app.global::<ThemeSettingsStore>().appearance().sync_with_os)
}

/// The Main pane's live [`TerminalView`] render theme + accent — the terminal
/// half of the fan-out (the pushed colors). `None` until the pane host has cached
/// the view.
fn terminal_theme_accent(
    cx: &mut AsyncApp,
    pane_host: &Entity<PaneHostView>,
    pane_id: &str,
) -> Option<(TerminalTheme, nice_theme::color::Srgba)> {
    let view = pane_host.update(cx, |h, _| h.scenario_terminal_for(pane_id))?;
    Some(view.update(cx, |v, _| (v.theme().clone(), v.accent())))
}

/// Poll until the Main pane's `TerminalView` is cached in the pane host (the
/// activate-on-render → deferred spawn path), so the terminal-half reads land.
async fn await_terminal_view(
    cx: &mut AsyncApp,
    pane_host: &Entity<PaneHostView>,
    pane_id: &str,
) -> bool {
    for _ in 0..40 {
        if terminal_theme_accent(cx, pane_host, pane_id).is_some() {
            return true;
        }
        settle(cx, 100).await;
    }
    false
}

/// The largest per-channel delta across a sampled point set — the ground-truth
/// "did the terminal repaint" signal, tolerant of anti-aliasing (`>8/255` is a
/// real recolor, not sampling jitter). Returns 0 on a capture error so the caller
/// reports the capture failure explicitly rather than a false pass.
fn max_channel_delta(a: &[[u8; 4]], b: &[[u8; 4]]) -> u16 {
    a.iter()
        .zip(b.iter())
        .flat_map(|(x, y)| (0..3).map(move |i| (x[i] as i16 - y[i] as i16).unsigned_abs()))
        .max()
        .unwrap_or(0)
}

/// Points inside the terminal region of the 960×640 shipped window (right of the
/// ~240pt sidebar card, below the ~52pt top bar) — sampled low/right to dodge the
/// shell prompt at the grid's top-left.
const TERMINAL_SAMPLE_POINTS: &[(f32, f32)] = &[(600.0, 320.0), (720.0, 440.0), (820.0, 560.0)];

fn sample_terminal(cx: &mut AsyncApp, handle: AnyWindowHandle) -> Result<Vec<[u8; 4]>> {
    nice_harness::capture::sample_window_pixels(handle, cx, TERMINAL_SAMPLE_POINTS)
}

// -- driver ------------------------------------------------------------------

async fn run_theme_fanout(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: &Fixture,
    os_cell: Arc<AtomicU8>,
) -> CadenceReport {
    let any: AnyWindowHandle = whandle.into();
    // Frontmost/key + painted (capture reads the drawable; a frontmost, painted
    // window makes the sample deterministic).
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    let shell = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => return CadenceReport::error(format!("theme-fanout: could not read the shell view: {e}")),
    };
    let pane_host = shell.update(cx, |s, _| s.scenario_pane_host());
    let id = any.window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "theme-fanout: the shipped builder did not register the window's WindowState".to_string(),
        );
    };
    let Some((main_tab, main_pane)) = active_tab_and_pane(cx, &state) else {
        return CadenceReport::error("theme-fanout: the shipped window has no active tab/pane".to_string());
    };
    let _ = main_tab;

    if !await_terminal_view(cx, &pane_host, &main_pane).await {
        return CadenceReport::error(
            "theme-fanout: the Main pane's TerminalView never mounted (fan-out has no terminal to reach)"
                .to_string(),
        );
    }
    settle(cx, 300).await;

    let mut failures: Vec<String> = Vec::new();

    scheme_flip_leg(cx, any, &pane_host, &main_pane, &os_cell, &mut failures).await;
    manual_contradiction_leg(cx, &os_cell, &mut failures).await;
    accent_leg(cx, &pane_host, &main_pane, &mut failures).await;
    terminal_id_latency_leg(cx, any, &pane_host, &main_pane, &mut failures).await;
    claude_sync_leg(cx, &state, fixture, &mut failures).await;
    imported_theme_leg(cx, any, &pane_host, &main_pane, fixture, &mut failures).await;

    build_report(failures)
}

// -- leg (a/d): OS-sync scheme flip fans chrome + terminal -------------------

async fn scheme_flip_leg(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    pane_host: &Entity<PaneHostView>,
    pane_id: &str,
    os_cell: &Arc<AtomicU8>,
    failures: &mut Vec<String>,
) {
    // Baseline (Dark): chrome slots, terminal theme, pixels.
    let slots_before = active_slots(cx);
    let Some((theme_before, _)) = terminal_theme_accent(cx, pane_host, pane_id) else {
        failures.push("(a) baseline: Main pane TerminalView vanished".into());
        return;
    };
    let pixels_before = match sample_terminal(cx, handle) {
        Ok(p) => Some(p),
        Err(e) => {
            failures.push(format!("(a) baseline pixel capture failed: {e}"));
            None
        }
    };

    // OS switches to Light; the appearance adapter's job is reconcile_with_os.
    os_cell.store(OS_LIGHT, Ordering::SeqCst);
    cx.update(|app| theme_settings::reconcile_with_os(app, ColorScheme::Light));
    settle(cx, 400).await;

    if store_scheme(cx) != ColorScheme::Light {
        failures.push("(a/d) sync_with_os ON: an OS Light switch did NOT flip the scheme".into());
    }
    // Chrome half: the active Slots changed (Mocha dark table → Latte light table).
    let slots_after = active_slots(cx);
    if slots_after == slots_before {
        failures.push("(a) chrome fan-out: the active Slots did not change on the scheme flip".into());
    }
    // Terminal half: the pushed render theme changed (nice_default_dark → light).
    match terminal_theme_accent(cx, pane_host, pane_id) {
        Some((theme_after, _)) if theme_after != theme_before => {
            eprintln!("[selftest] theme-fanout (a): chrome Slots + terminal theme both recolored on the scheme flip");
        }
        Some(_) => failures
            .push("(a) terminal fan-out: the Main pane's render theme did not change on the scheme flip".into()),
        None => failures.push("(a) terminal fan-out: the Main pane TerminalView vanished".into()),
    }
    // Ground-truth pixel recolor on the live terminal cell.
    if let Some(before) = pixels_before {
        match sample_terminal(cx, handle) {
            Ok(after) => {
                let delta = max_channel_delta(&before, &after);
                if delta <= 8 {
                    failures.push(format!(
                        "(a) pixel fan-out: the live terminal did not recolor on the scheme flip (max channel delta {delta} <= 8)"
                    ));
                } else {
                    eprintln!("[selftest] theme-fanout (a): live terminal pixel recolored (max channel delta {delta})");
                }
            }
            Err(e) => failures.push(format!("(a) post-flip pixel capture failed: {e}")),
        }
    }

    // sync OFF ⇒ driving the stub is a no-op (reconcile gates on sync_with_os).
    cx.update(|app| theme_settings::apply_sync_with_os(app, false));
    let scheme_now = store_scheme(cx);
    os_cell.store(OS_DARK, Ordering::SeqCst);
    cx.update(|app| theme_settings::reconcile_with_os(app, ColorScheme::Dark));
    settle(cx, 200).await;
    if store_scheme(cx) != scheme_now {
        failures.push("(d) sync_with_os OFF: an OS switch STILL flipped the scheme (reconcile did not gate)".into());
    } else {
        eprintln!("[selftest] theme-fanout (d): with sync off, an OS switch is a no-op");
    }
}

// -- leg (d): manual contradiction turns sync off ----------------------------

async fn manual_contradiction_leg(
    cx: &mut AsyncApp,
    os_cell: &Arc<AtomicU8>,
    failures: &mut Vec<String>,
) {
    // Re-enable sync: it pins the scheme to the stub OS (Dark).
    os_cell.store(OS_DARK, Ordering::SeqCst);
    cx.update(|app| theme_settings::apply_sync_with_os(app, true));
    if store_scheme(cx) != ColorScheme::Dark || !store_sync_on(cx) {
        failures.push("(d) turning sync ON did not pin the scheme to the OS (Dark)".into());
        return;
    }
    // A manual pick to the OTHER scheme contradicts the OS ⇒ sync turns off.
    cx.update(|app| theme_settings::apply_scheme(app, ColorScheme::Light));
    settle(cx, 200).await;
    if store_sync_on(cx) {
        failures.push("(d) a manual scheme pick contradicting the OS did NOT turn sync_with_os off".into());
    } else if store_scheme(cx) != ColorScheme::Light {
        failures.push("(d) the manual scheme pick did not take effect".into());
    } else {
        eprintln!("[selftest] theme-fanout (d): a manual contradicting pick turned sync off");
    }
}

// -- leg (b): accent recolors the caret --------------------------------------

async fn accent_leg(
    cx: &mut AsyncApp,
    pane_host: &Entity<PaneHostView>,
    pane_id: &str,
    failures: &mut Vec<String>,
) {
    let Some((_, accent_before)) = terminal_theme_accent(cx, pane_host, pane_id) else {
        failures.push("(b) baseline: Main pane TerminalView vanished".into());
        return;
    };
    // Pick an accent that differs from the current one (default is Ocean).
    let target = AccentPreset::Fern;
    cx.update(|app| theme_settings::apply_accent(app, target));
    settle(cx, 200).await;
    match terminal_theme_accent(cx, pane_host, pane_id) {
        Some((_, accent_after)) if accent_after != accent_before && accent_after == target.color() => {
            eprintln!("[selftest] theme-fanout (b): apply_accent recolored the pane's accent (the cursor-None caret)");
        }
        Some(_) => failures.push("(b) accent fan-out: the pane's accent did not update on apply_accent".into()),
        None => failures.push("(b) accent fan-out: the Main pane TerminalView vanished".into()),
    }
}

// -- leg (c): terminal-theme-id — inactive latent, flip applies --------------

async fn terminal_id_latency_leg(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    pane_host: &Entity<PaneHostView>,
    pane_id: &str,
    failures: &mut Vec<String>,
) {
    // Active scheme is Light here (leg d left it Light, sync off). Setting the
    // INACTIVE (Dark) slot's terminal id must NOT recolor the pane now.
    let Some((theme_before, _)) = terminal_theme_accent(cx, pane_host, pane_id) else {
        failures.push("(c) baseline: Main pane TerminalView vanished".into());
        return;
    };
    let pixels_before = sample_terminal(cx, handle).ok();
    cx.update(|app| theme_settings::apply_terminal_theme_id(app, ColorScheme::Dark, "nice-default-dark"));
    settle(cx, 200).await;

    match terminal_theme_accent(cx, pane_host, pane_id) {
        Some((theme_now, _)) if theme_now == theme_before => {
            eprintln!("[selftest] theme-fanout (c): an inactive-scheme terminal-id change is latent (no recolor)");
        }
        Some(_) => failures.push("(c) latency: an INACTIVE-scheme terminal-id change recolored the pane (should be latent)".into()),
        None => failures.push("(c) latency: the Main pane TerminalView vanished".into()),
    }
    if let (Some(before), Some(after)) = (pixels_before, sample_terminal(cx, handle).ok()) {
        let delta = max_channel_delta(&before, &after);
        if delta > 8 {
            failures.push(format!(
                "(c) latency: the terminal repainted on an inactive-slot id change (max channel delta {delta})"
            ));
        }
    }
    // Confirm it PERSISTED to the inactive slot.
    let dark_id = cx.update(|app| app.global::<ThemeSettingsStore>().appearance().terminal_theme_dark_id.clone());
    if dark_id != "nice-default-dark" {
        failures.push(format!("(c) the inactive-slot id did not persist (dark id = {dark_id})"));
    }

    // Flip to Dark: the now-active Dark slot applies, so the pane picks up the Dark
    // resolution (the active-slot path — R21's stub resolves it to nice_default_dark).
    cx.update(|app| theme_settings::apply_scheme(app, ColorScheme::Dark));
    settle(cx, 300).await;
    match terminal_theme_accent(cx, pane_host, pane_id) {
        Some((theme_now, _)) if theme_now != theme_before => {
            eprintln!("[selftest] theme-fanout (c): the scheme flip made the latent Dark slot active and the pane applied it");
        }
        Some(_) => failures.push("(c) the scheme flip did not apply the now-active Dark terminal slot".into()),
        None => failures.push("(c) the Main pane TerminalView vanished after the flip".into()),
    }
}

// -- leg (e): R17-live Claude mirror + provider re-source --------------------

async fn claude_sync_leg(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    // Gate starts OFF: the window's provider is None (no --settings for new panes).
    let provider_off = cx.update(|app| state.read(app).claude_settings_path_provider());
    if provider_off.is_some() {
        failures.push("(e) precondition: the window carried a --settings provider while the gate was OFF".into());
    }

    // Toggle ON: re-source providers + rewrite the colors file (off→on).
    cx.update(|app| theme_settings::apply_sync_claude_theme(app, true));
    settle(cx, 150).await;
    let provider_on = cx.update(|app| state.read(app).claude_settings_path_provider());
    if provider_on.is_none() {
        failures.push("(e) apply_sync_claude_theme(on) did not re-source the window's --settings provider".into());
    } else {
        eprintln!("[selftest] theme-fanout (e): the gate ON re-sourced the window's --settings provider");
    }
    let colors_file = fixture.claude_colors_file();
    let bytes_after_on = match std::fs::read(&colors_file) {
        Ok(b) => b,
        Err(e) => {
            failures.push(format!("(e) the gate ON did not write the Claude colors file {}: {e}", colors_file.display()));
            Vec::new()
        }
    };

    // A theme change with the gate ON rewrites the colors file to the new colors.
    if !bytes_after_on.is_empty() {
        cx.update(|app| theme_settings::apply_accent(app, AccentPreset::Iris));
        settle(cx, 150).await;
        match std::fs::read(&colors_file) {
            Ok(bytes_after_change) if bytes_after_change != bytes_after_on => {
                eprintln!("[selftest] theme-fanout (e): a gate-ON theme change rewrote the Claude colors file (byte-diff)");
            }
            Ok(_) => failures.push("(e) a gate-ON accent change did NOT rewrite the Claude colors file (byte-identical)".into()),
            Err(e) => failures.push(format!("(e) re-reading the colors file failed: {e}")),
        }
    }

    // Toggle OFF: re-source providers back to None (new panes get no flag).
    cx.update(|app| theme_settings::apply_sync_claude_theme(app, false));
    settle(cx, 100).await;
    let provider_reoff = cx.update(|app| state.read(app).claude_settings_path_provider());
    if provider_reoff.is_some() {
        failures.push("(e) apply_sync_claude_theme(off) did not clear the window's --settings provider".into());
    } else {
        eprintln!("[selftest] theme-fanout (e): the gate OFF cleared the window's --settings provider");
    }
}

// -- leg (f): R22 Ghostty import — parse → persist → catalog → resolve → paint -

/// A well-formed Ghostty theme fixture with a vivid magenta background (so the
/// recolor is unmistakable against the dark baseline) and a full 16-entry palette.
fn imported_theme_source() -> String {
    let mut s = String::from(
        "# imported neon fixture\n\
         background = #ff00ff\n\
         foreground = #ffffff\n\
         cursor-color = #ffff00\n\
         selection-background = #202020\n",
    );
    for i in 0..16u8 {
        s.push_str(&format!("palette = {i}=#00{i:02x}00\n"));
    }
    s
}

/// End-to-end R22 leg: write a fixture `.ghostty` under the sandbox support root,
/// `import_theme` it through the Global catalog, confirm it persisted as
/// `<slug>.ghostty` + entered the catalog + resolves, then `apply_terminal_theme_id`
/// (R21) to its id for the active scheme and assert the live terminal pane recolors
/// to the imported background — proving parse → persist → catalog → resolve → R21
/// fan-out on a real window.
async fn imported_theme_leg(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    pane_host: &Entity<PaneHostView>,
    pane_id: &str,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    const NEON: TerminalColor = TerminalColor::new(0xff, 0x00, 0xff);

    let Some((theme_before, _)) = terminal_theme_accent(cx, pane_host, pane_id) else {
        failures.push("(f) baseline: Main pane TerminalView vanished".into());
        return;
    };
    let pixels_before = sample_terminal(cx, handle).ok();

    // Write the fixture into the sandbox terminal-themes dir (create-on-demand).
    if let Err(e) = std::fs::create_dir_all(&fixture.terminal_themes_dir) {
        failures.push(format!("(f) could not create the sandbox terminal-themes dir: {e}"));
        return;
    }
    let fixture_path = fixture.terminal_themes_dir.join("Imported Neon.ghostty");
    if let Err(e) = std::fs::write(&fixture_path, imported_theme_source()) {
        failures.push(format!("(f) could not write the fixture theme file: {e}"));
        return;
    }

    // Import through the Global catalog (the R22 Exported surface).
    let import = cx.update(|app| {
        app.global_mut::<TerminalThemeCatalog>()
            .import_theme(&fixture_path)
    });
    let entry = match import {
        Ok(e) => e,
        Err(e) => {
            failures.push(format!("(f) import_theme rejected a well-formed fixture: {e:?}"));
            return;
        }
    };
    if entry.id != "imported-neon" {
        failures.push(format!("(f) import slug mismatch: got '{}', want 'imported-neon'", entry.id));
    }
    // Persisted under the temp support root as `<slug>.ghostty`.
    if !fixture.terminal_themes_dir.join("imported-neon.ghostty").exists() {
        failures.push("(f) import did not persist <slug>.ghostty under the temp support root".into());
    }
    // Entered the catalog: resolves to its background for the active scheme.
    let active = store_scheme(cx);
    let resolved_bg = cx.update(|app| {
        app.global::<TerminalThemeCatalog>()
            .resolve(&entry.id, active)
            .background
    });
    if resolved_bg != NEON {
        failures.push("(f) the imported theme did not resolve to its background through the catalog".into());
    }

    // Make it live for the active scheme (R21 mutator) — the pane recolors.
    cx.update(|app| theme_settings::apply_terminal_theme_id(app, active, &entry.id));
    settle(cx, 300).await;

    match terminal_theme_accent(cx, pane_host, pane_id) {
        Some((theme_now, _)) if theme_now != theme_before && theme_now.background == NEON => {
            eprintln!("[selftest] theme-fanout (f): an imported Ghostty theme parsed → persisted → entered the catalog → resolved → recolored the live pane");
        }
        Some(_) => failures
            .push("(f) the imported theme did not recolor the Main pane's render theme".into()),
        None => failures.push("(f) the Main pane TerminalView vanished after applying the import".into()),
    }
    if let (Some(before), Some(after)) = (pixels_before, sample_terminal(cx, handle).ok()) {
        let delta = max_channel_delta(&before, &after);
        if delta <= 8 {
            failures.push(format!(
                "(f) the live terminal did not recolor to the imported theme (max channel delta {delta} <= 8)"
            ));
        } else {
            eprintln!("[selftest] theme-fanout (f): live terminal pixel recolored to the imported theme (max channel delta {delta})");
        }
    }
}

// -- reads / report ----------------------------------------------------------

fn active_tab_and_pane(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Option<(String, String)> {
    state.update(cx, |s, _| {
        let tab = s.model.active_tab_id()?.to_string();
        let pane = s.model.tab_for(&tab).and_then(|t| t.active_pane_id.clone())?;
        Some((tab, pane))
    })
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "theme-fanout OK: (a/d) an OS Light switch (sync on) flipped the scheme — chrome Slots \
                     + terminal render theme + a live terminal pixel all recolored, and with sync off an OS \
                     switch was a no-op; (d) a manual contradicting pick turned sync off; (b) apply_accent \
                     recolored the pane accent (the cursor-None caret); (c) an inactive-slot terminal-id \
                     change was latent (no recolor, persisted) and the next scheme flip applied it; \
                     (e) the gate ON re-sourced the --settings provider + wrote the colors file, a gate-ON \
                     theme change rewrote it (byte-diff), and the gate OFF cleared the provider; \
                     (f) an imported Ghostty theme parsed → persisted as <slug>.ghostty under the temp \
                     support root → entered the catalog → resolved → recolored the live terminal pane"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} theme-fanout assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
