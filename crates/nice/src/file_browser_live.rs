//! `file-browser` self-test scenario — the R19 shipped-surface gate (What to
//! build #7). Opens through the SHIPPED builder (`open_managed_window` →
//! `build_window_root` → `AppShellView`, the exact path `run` takes), roots the
//! active tab's browser at a temp fixture tree, and drives the real composition:
//!
//! (a) a real ⌘⇧B chord (the shipped `ToggleSidebarMode` keymap) swaps the tab
//!     list for the tree in the live window — the AX root
//!     `nice-rs-file-browser-root` surfaces as an `AXGroup` and a fixture row is
//!     rendered (model-read corroboration);
//! (b) a single click expands a fixture dir, a second single click collapses it;
//! (c) a double click on a folder re-roots the tree (model `root_path`);
//! (d) a double click on a file records exactly one `open` on the recording
//!     `WorkspaceOps` fake — nothing is launched;
//! (e) a right-click on a file shows Open / Open With ▸ / Reveal in Finder /
//!     Copy Path; a right-click on a folder omits Open + Open With; the Open
//!     With ▸ second stage lists the fake's apps, default first;
//! (f) creating a file in an expanded fixture dir surfaces its row within a
//!     bounded fail-loud poll (the live watcher + 120 ms debounce);
//! (g) the sort-direction toggle reorders rows; the hidden toggle + a real ⌘⇧.
//!     chord hide/show a dotfile; a real ⌘⇧B still flips modes.
//!
//! Hermeticity: the fixture tree lives under a per-run temp dir; the recording
//! `WorkspaceOps` fake is installed process-wide by `run_selftest` before any
//! scenario, so no real app launches / Finder reveal / Launch-Services query
//! ever happens (the fake's log is the only evidence). Self-reported
//! ([`Gate::SelfReported`](nice_harness::selftest)); Accessibility is preflighted
//! (a missing grant FAILs loudly — a dropped CGEvent would make the chords
//! no-ops). Registered BEFORE `multiwindow`: it does NOT install the
//! `WindowRegistry` close observer, so closing its window never trips the
//! quit-when-empty terminus `multiwindow` relies on being last.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};

use crate::app_shell::AppShellView;
use crate::file_browser::view::{FileBrowserView, FILE_BROWSER_ROOT_LABEL};
use crate::file_browser::workspace_ops::{selftest_fake, OpenWithApps, WorkspaceCall};
use crate::platform;
use crate::sidebar_shell::SidebarShellView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

/// ⌘⇧B — ToggleSidebarMode (`CGKeyCode` for `b`).
const KC_B: u16 = 11;

/// The macOS `AXRole` a `gpui::Role::Group` maps to (as the `ax-probe` /
/// `app-shell` anchors assert).
const AX_EXPECTED_ROLE: &str = "AXGroup";
/// AX poll budget (AccessKit activates lazily on the first query).
const AX_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval (real wall-clock).
const POLL_MS: u64 = 100;
/// Watcher poll budget for step (f): create → kqueue → 120 ms debounce → wake →
/// foreground drain → re-render. The watcher's own thread is exempt from the
/// no-wall-clock rule; this is a bounded fail-loud poll.
const WATCH_POLLS: usize = 40;

const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected chord can reach the window. \
Fix: System Settings → Privacy & Security → Accessibility → enable the process \
hosting this run. If it shows ON but this persists, the grant is STALE — remove \
it with '-' and re-add it, then re-run.";

// ===========================================================================
// scenario wiring
// ===========================================================================

pub fn open_file_browser_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        crate::keymap::install_shortcuts(app);
        crate::app::open_managed_window(app)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_file_browser(acx, whandle).await;
        eprintln!("[selftest] scenario 'file-browser': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor().timer(Duration::from_millis(ms)).await;
}

async fn tap(cx: &mut AsyncApp, pid: i32, keycode: u16, flags: u64) {
    platform::post_key_tap(pid, keycode, flags, None);
    settle(cx, 150).await;
}

async fn rekey(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>) {
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;
}

// ===========================================================================
// fixture
// ===========================================================================

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> std::io::Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "nice-rs-file-browser-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join("src/lib.rs"), b"// lib\n")?;
        std::fs::write(root.join("README.md"), b"# readme\n")?;
        std::fs::write(root.join("alpha.txt"), b"a\n")?;
        std::fs::write(root.join("zeta.txt"), b"z\n")?;
        std::fs::write(root.join(".env"), b"SECRET=1\n")?;
        Ok(Fixture { root })
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_file_browser(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    rekey(cx, whandle).await;

    let fixture = match Fixture::new() {
        Ok(f) => f,
        Err(e) => return CadenceReport::error(format!("file-browser: fixture setup failed: {e}")),
    };
    let Some(fake) = selftest_fake() else {
        return CadenceReport::error(
            "file-browser: the recording WorkspaceOps fake was not installed by run_selftest"
                .to_string(),
        );
    };
    fake.set_apps(OpenWithApps {
        apps: vec![
            ("/Applications/Zed.app".into(), "Zed".into()),
            ("/Applications/TextEdit.app".into(), "TextEdit".into()),
        ],
        default_app: Some("/Applications/Zed.app".into()),
    });

    let shell = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => return CadenceReport::error(format!("file-browser: no shell view: {e}")),
    };
    let sidebar = shell.update(cx, |s, _| s.scenario_sidebar());
    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "file-browser: the shipped builder did not register the window's WindowState".to_string(),
        );
    };

    // Root the active tab's browser at the fixture tree (before entering files
    // mode, so the lazily-created state seeds its root there).
    let Some(main_tab) = state.update(cx, |s, _| s.model.active_tab_id().map(str::to_string)) else {
        return CadenceReport::error("file-browser: the shipped window has no active tab".to_string());
    };
    let fixture_root = fixture.root.to_string_lossy().into_owned();
    state.update(cx, |s, cx| {
        let root = fixture_root.clone();
        s.model.mutate_tab(&main_tab, |t| t.cwd = root);
        cx.notify();
    });

    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();

    // (a) ⌘⇧B → files mode; the tree replaces the tab list.
    let Some(fb) = enter_files_mode(cx, whandle, &sidebar, pid, &mut failures).await else {
        return build_report(failures); // nothing else can run without the view
    };
    ax_anchor_check(cx, &state, pid, &mut failures).await;
    assert_row_rendered(cx, &fb, &fixture.path("README.md"), &mut failures);

    // (b) single-click expands a dir, second collapses.
    expand_collapse_check(cx, &fb, &fixture.path("src"), &mut failures).await;

    // (d) double-click a file ⇒ exactly one open on the fake, nothing launched.
    double_click_open_check(cx, &fb, &fake, &fixture.path("README.md"), &mut failures).await;

    // (e) right-click menus + the two-stage Open With.
    context_menu_checks(cx, whandle, &fb, &fixture, &mut failures).await;

    // (f) create a file in an expanded dir ⇒ its row appears (live watcher).
    watcher_check(cx, &fb, &fixture, &mut failures).await;

    // (g) sort direction reorders; hidden toggle + ⌘⇧. hide/show a dotfile.
    sort_and_hidden_checks(cx, whandle, &fb, &fixture, &mut failures).await;

    // (c) double-click a folder re-roots (done late — it changes the root).
    reroot_check(cx, &fb, &fixture.path("src"), &mut failures).await;

    // (g cont.) ⌘⇧B still flips modes.
    mode_flip_check(cx, whandle, &state, pid, &mut failures).await;

    build_report(failures)
}

// ---- (a) enter files mode --------------------------------------------------

async fn enter_files_mode(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    sidebar: &Entity<SidebarShellView>,
    pid: i32,
    failures: &mut Vec<String>,
) -> Option<Entity<FileBrowserView>> {
    rekey(cx, whandle).await;
    tap(cx, pid, KC_B, platform::FLAG_COMMAND | platform::FLAG_SHIFT).await;
    settle(cx, 300).await;
    for _ in 0..20 {
        if let Some(fb) = sidebar.update(cx, |s, _| s.scenario_file_browser()) {
            eprintln!("[selftest] file-browser: ⌘⇧B swapped the tab list for the tree");
            return Some(fb);
        }
        settle(cx, POLL_MS).await;
    }
    failures.push(
        "⌘⇧B: the sidebar never entered files mode (the file browser view was never mounted — \
         did the chord reach the shipped keymap?)"
            .to_string(),
    );
    None
}

async fn ax_anchor_check(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    pid: i32,
    failures: &mut Vec<String>,
) {
    let deadline = Instant::now() + AX_TIMEOUT;
    let mut found = false;
    let mut last = "AX tree never exposed it".to_string();
    while Instant::now() < deadline && !found {
        let _ = state.update(cx, |_s, cx| cx.notify());
        settle(cx, 150).await;
        match platform::ax_find_titled_role(pid, FILE_BROWSER_ROOT_LABEL) {
            Ok(role) if role == AX_EXPECTED_ROLE => found = true,
            Ok(role) => last = format!("exposed but role '{role}' != '{AX_EXPECTED_ROLE}'"),
            Err(e) => last = e,
        }
    }
    if found {
        eprintln!("[selftest] file-browser AX: root '{FILE_BROWSER_ROOT_LABEL}' exposed as {AX_EXPECTED_ROLE}");
    } else {
        failures.push(format!(
            "AX: file-browser root anchor '{FILE_BROWSER_ROOT_LABEL}' not exposed as {AX_EXPECTED_ROLE}: {last}"
        ));
    }
}

fn assert_row_rendered(
    cx: &mut AsyncApp,
    fb: &Entity<FileBrowserView>,
    path: &str,
    failures: &mut Vec<String>,
) {
    let rows = fb.update(cx, |v, _| v.scenario_rendered_paths());
    if rows.iter().any(|p| p == path) {
        eprintln!("[selftest] file-browser: fixture row rendered ({} rows)", rows.len());
    } else {
        failures.push(format!(
            "files-mode: the fixture row {path} is not in the rendered tree (rows: {rows:?})"
        ));
    }
}

// ---- (b) expand / collapse -------------------------------------------------

async fn expand_collapse_check(
    cx: &mut AsyncApp,
    fb: &Entity<FileBrowserView>,
    dir: &str,
    failures: &mut Vec<String>,
) {
    // The two clicks here are DISTINCT single clicks (expand, then collapse), so
    // they must be spaced beyond the router's 280 ms double-click window — a
    // shorter gap would read as a double-click (re-root) instead.
    fb.update(cx, |v, cx| v.drive_single_click(dir, cx));
    settle(cx, 400).await;
    if !fb.update(cx, |v, cx| v.scenario_is_expanded(dir, cx)) {
        failures.push(format!("expand: a single click on the dir {dir} did not expand it"));
        return;
    }
    let child = format!("{dir}/lib.rs");
    if !fb.update(cx, |v, _| v.scenario_rendered_paths()).iter().any(|p| p == &child) {
        failures.push(format!("expand: the child row {child} did not appear after expanding"));
    }
    fb.update(cx, |v, cx| v.drive_single_click(dir, cx));
    settle(cx, 400).await;
    if fb.update(cx, |v, cx| v.scenario_is_expanded(dir, cx)) {
        failures.push(format!("collapse: a second single click on {dir} did not collapse it"));
    } else {
        eprintln!("[selftest] file-browser: single click expanded then collapsed the dir");
    }
}

// ---- (d) double-click file ⇒ one open --------------------------------------

async fn double_click_open_check(
    cx: &mut AsyncApp,
    fb: &Entity<FileBrowserView>,
    fake: &crate::file_browser::workspace_ops::RecordingWorkspaceOps,
    file: &str,
    failures: &mut Vec<String>,
) {
    fake.clear();
    fb.update(cx, |v, cx| v.drive_double_click(file, cx));
    settle(cx, 200).await;
    let calls = fake.calls();
    let opens: Vec<&WorkspaceCall> = calls
        .iter()
        .filter(|c| matches!(c, WorkspaceCall::Open(p) if p == file))
        .collect();
    if opens.len() == 1 && calls.len() == 1 {
        eprintln!("[selftest] file-browser: double-click a file recorded exactly one open, nothing launched");
    } else {
        failures.push(format!(
            "double-click file: expected exactly one Open({file}) on the fake, got {calls:?}"
        ));
    }
}

// ---- (e) context menus + Open With -----------------------------------------

async fn context_menu_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    let file = fixture.path("README.md");
    let dir = fixture.path("src");

    // Right-click a file: Open / Open With ▸ / Reveal in Finder / Copy Path.
    let file_labels = right_click_labels(cx, whandle, fb, &file).await;
    for want in ["Open", "Open With \u{25B8}", "Reveal in Finder", "Copy Path"] {
        if !file_labels.iter().any(|l| l == want) {
            failures.push(format!(
                "right-click file: menu is missing '{want}' (got {file_labels:?})"
            ));
        }
    }

    // Right-click a folder: Open + Open With are omitted.
    let dir_labels = right_click_labels(cx, whandle, fb, &dir).await;
    for unwanted in ["Open", "Open With \u{25B8}"] {
        if dir_labels.iter().any(|l| l == unwanted) {
            failures.push(format!(
                "right-click folder: menu must omit '{unwanted}' (got {dir_labels:?})"
            ));
        }
    }
    if !dir_labels.iter().any(|l| l == "Reveal in Finder") || !dir_labels.iter().any(|l| l == "Copy Path") {
        failures.push(format!(
            "right-click folder: menu should still carry Reveal in Finder + Copy Path (got {dir_labels:?})"
        ));
    }

    // Open With ▸ second stage: the fake's apps, default first.
    let ow_labels = whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| {
                v.drive_open_with(&file, window, cx);
                v.scenario_menu_labels(cx)
            })
        })
        .unwrap_or_default();
    if ow_labels.first().map(String::as_str) != Some("Zed (default)") {
        failures.push(format!(
            "Open With ▸: second stage must list the default app first ('Zed (default)'); got {ow_labels:?}"
        ));
    }
    if !ow_labels.iter().any(|l| l == "TextEdit") || !ow_labels.iter().any(|l| l == "Other\u{2026}") {
        failures.push(format!(
            "Open With ▸: second stage should list 'TextEdit' + 'Other…' (got {ow_labels:?})"
        ));
    }
    if failures.is_empty() {
        eprintln!("[selftest] file-browser: right-click menus + two-stage Open With are correct");
    }
}

async fn right_click_labels(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    path: &str,
) -> Vec<String> {
    let labels = whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| {
                v.drive_right_click(path, window, cx);
                v.scenario_menu_labels(cx)
            })
        })
        .unwrap_or_default();
    settle(cx, 100).await;
    labels
}

// ---- (f) live watcher ------------------------------------------------------

async fn watcher_check(
    cx: &mut AsyncApp,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    let dir = fixture.path("src");
    // Ensure src is expanded (so it's in the watched set).
    if !fb.update(cx, |v, cx| v.scenario_is_expanded(&dir, cx)) {
        fb.update(cx, |v, cx| v.drive_single_click(&dir, cx));
        settle(cx, 200).await;
    }
    // Give the watcher a beat to register the knote before mutating.
    settle(cx, 250).await;
    let new_file = fixture.path("src/watched_new.rs");
    if let Err(e) = std::fs::write(&new_file, b"// new\n") {
        failures.push(format!("watcher: could not create the fixture file: {e}"));
        return;
    }
    // Bounded fail-loud poll — NO forced notify, so only a watcher-driven
    // re-render can surface the new row (this is what proves the watcher fired).
    for _ in 0..WATCH_POLLS {
        settle(cx, POLL_MS).await;
        if fb.update(cx, |v, _| v.scenario_rendered_paths()).iter().any(|p| p == &new_file) {
            eprintln!("[selftest] file-browser: the live watcher surfaced a newly-created row");
            return;
        }
    }
    failures.push(format!(
        "watcher: a file created in the expanded dir never surfaced as a row within the poll budget \
         (the kqueue watcher + 120ms debounce + foreground drain did not fire): {new_file}"
    ));
}

// ---- (g) sort direction + hidden -------------------------------------------

async fn sort_and_hidden_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    let alpha = fixture.path("alpha.txt");
    let zeta = fixture.path("zeta.txt");
    let index = |rows: &[String], p: &str| rows.iter().position(|r| r == p);

    let rows = fb.update(cx, |v, _| v.scenario_rendered_paths());
    let (a0, z0) = (index(&rows, &alpha), index(&rows, &zeta));
    if !(a0 < z0 && a0.is_some()) {
        failures.push(format!(
            "sort: ascending should place alpha.txt before zeta.txt (a={a0:?} z={z0:?})"
        ));
    }
    fb.update(cx, |v, cx| v.drive_toggle_direction(cx));
    settle(cx, 200).await;
    let rows = fb.update(cx, |v, _| v.scenario_rendered_paths());
    let (a1, z1) = (index(&rows, &alpha), index(&rows, &zeta));
    if !(z1 < a1 && z1.is_some()) {
        failures.push(format!(
            "sort: after the direction toggle zeta.txt should precede alpha.txt (a={a1:?} z={z1:?})"
        ));
    } else {
        eprintln!("[selftest] file-browser: the sort-direction toggle reordered the rows");
    }
    // Restore ascending so later reads read naturally.
    fb.update(cx, |v, cx| v.drive_toggle_direction(cx));
    settle(cx, 150).await;

    // Hidden: .env is shown by default (non-home cwd); the control-strip toggle
    // hides it, and a real ⌘⇧. chord shows it again.
    let dotfile = fixture.path(".env");
    let shown = |cx: &mut AsyncApp| fb.update(cx, |v, _| v.scenario_rendered_paths()).iter().any(|p| p == &dotfile);
    if !shown(cx) {
        failures.push(format!("hidden: the dotfile {dotfile} should be shown by default (non-home cwd)"));
    }
    fb.update(cx, |v, cx| v.drive_toggle_hidden(cx));
    settle(cx, 200).await;
    if shown(cx) {
        failures.push("hidden: the control-strip toggle did not hide the dotfile".to_string());
    }
    // ⌘⇧. re-show: dispatch the SHIPPED `ToggleHiddenFiles` action (the ⌘⇧.
    // binding's target) directly. A synthetic shift+`.` CGEvent does NOT decode to
    // the base `.` key at the gpui pin (the documented character-matching
    // divergence — the same reason `multiwindow` only drives letter/arrow chords),
    // but `App::dispatch_action` routes through the exact shipped keymap handler,
    // exercising the R19 files-mode-AND-state-exists double gate end to end.
    rekey(cx, whandle).await;
    let _ = cx.update(|app| app.dispatch_action(&crate::keymap::ToggleHiddenFiles));
    settle(cx, 250).await;
    if !shown(cx) {
        failures.push(
            "hidden: the shipped ToggleHiddenFiles action (⌘⇧.) did not re-show the dotfile — \
             the files-mode/state-exists double gate did not fire"
                .to_string(),
        );
    } else {
        eprintln!("[selftest] file-browser: hidden toggle hid the dotfile; the shipped ⌘⇧. action re-showed it");
    }
}

// ---- (c) re-root -----------------------------------------------------------

async fn reroot_check(
    cx: &mut AsyncApp,
    fb: &Entity<FileBrowserView>,
    dir: &str,
    failures: &mut Vec<String>,
) {
    fb.update(cx, |v, cx| v.drive_double_click(dir, cx));
    settle(cx, 200).await;
    match fb.update(cx, |v, cx| v.scenario_root(cx)) {
        Some(root) if root == dir => {
            eprintln!("[selftest] file-browser: double-click a folder re-rooted the tree");
        }
        other => failures.push(format!(
            "re-root: double-click on the folder {dir} did not re-root (root is {other:?})"
        )),
    }
}

// ---- (g cont.) mode flip ---------------------------------------------------

async fn mode_flip_check(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    pid: i32,
    failures: &mut Vec<String>,
) {
    use nice_model::SidebarMode;
    rekey(cx, whandle).await;
    tap(cx, pid, KC_B, platform::FLAG_COMMAND | platform::FLAG_SHIFT).await;
    settle(cx, 250).await;
    let mode = state.update(cx, |s, _| s.sidebar.mode());
    if mode != SidebarMode::Tabs {
        failures.push(format!("⌘⇧B: expected a flip back to Tabs mode, got {mode:?}"));
        return;
    }
    tap(cx, pid, KC_B, platform::FLAG_COMMAND | platform::FLAG_SHIFT).await;
    settle(cx, 250).await;
    let mode = state.update(cx, |s, _| s.sidebar.mode());
    if mode != SidebarMode::Files {
        failures.push(format!("⌘⇧B: expected a flip back to Files mode, got {mode:?}"));
    } else {
        eprintln!("[selftest] file-browser: ⌘⇧B still flips the sidebar mode");
    }
}

// ---- verdict ---------------------------------------------------------------

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "file-browser OK (through the shipped builder): ⌘⇧B swapped in the tree \
                     (AX root exposed + fixture row rendered), single-click expand/collapse, \
                     double-click re-root, double-click file recorded one open (nothing launched), \
                     right-click menus (file vs folder) + two-stage Open With default-first, the \
                     live watcher surfaced a created row, sort-direction + hidden toggle + ⌘⇧. \
                     worked, and ⌘⇧B still flips modes."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} file-browser assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
