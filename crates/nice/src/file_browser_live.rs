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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{AnyWindowHandle, AppContext, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};

use crate::app_shell::AppShellView;
use crate::file_browser::history::{FileOperationHistory, FileOperationHistoryGlobal};
use crate::file_browser::ops::{FakeTrasher, FileOperationsService};
use crate::file_browser::pasteboard::{
    FakeFilePasteboard, FilePasteboard, FilePasteboardGlobal, Intent,
};
use crate::file_browser::view::{FileBrowserView, FILE_BROWSER_ROOT_LABEL};
use crate::file_browser::workspace_ops::{selftest_fake, OpenWithApps, WorkspaceCall};
use crate::keymap::{RedoFileOperation, UndoFileOperation};
use crate::platform;
use crate::sidebar_shell::SidebarShellView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

/// ⌘⇧B — ToggleSidebarMode (`CGKeyCode` for `b`).
const KC_B: u16 = 11;
/// ⌘N — New Window (`CGKeyCode` for `n`; the §6 composition leg's second window).
const KC_N: u16 = 45;
/// ⌘Z — UndoFileOperation (`CGKeyCode` for `z`; the §6 cross-window undo chord).
const KC_Z: u16 = 6;

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
        // The §6 composition leg opens a SECOND real window via a ⌘N CGEvent (the
        // `multiwindow` precedent) — wire the New Window command here. Its
        // `build_window_root` only `register`s the window (no `WindowRegistry`
        // close observer), so opening/closing window B never trips quit-when-empty.
        crate::app::install_new_window_command(app);
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
        // R20 fixtures: copy/cut/paste, trash+undo, rename, and drag targets.
        std::fs::write(root.join("copyme.txt"), b"c\n")?;
        std::fs::write(root.join("cutme.txt"), b"x\n")?;
        std::fs::write(root.join("degrade.txt"), b"d\n")?;
        std::fs::write(root.join("other.txt"), b"o\n")?;
        std::fs::create_dir_all(root.join("restoredir"))?;
        std::fs::write(root.join("restoredir/gone.txt"), b"g\n")?;
        std::fs::write(root.join("renameme.txt"), b"r\n")?;
        std::fs::write(root.join("escme.txt"), b"e\n")?;
        std::fs::write(root.join("slashme.txt"), b"s\n")?;
        // Extension-change confirmation-modal targets (disjoint from every other
        // rename target so the modal orchestration leg's fs outcome is unambiguous).
        std::fs::write(root.join("extchange.txt"), b"x\n")?;
        std::fs::write(root.join("extcancel.txt"), b"x\n")?;
        std::fs::write(root.join("dragA.txt"), b"A\n")?;
        std::fs::write(root.join("dragB.txt"), b"B\n")?;
        std::fs::write(root.join("driftme.txt"), b"D\n")?;
        // §6 final-composition leg: two rows copy→pasted into a folder + a
        // slow-second-click rename target (kept disjoint from the R20-leg files so
        // the composition leg's op stack is unambiguous).
        std::fs::create_dir_all(root.join("compdir"))?;
        std::fs::write(root.join("comp1.txt"), b"1\n")?;
        std::fs::write(root.join("comp2.txt"), b"2\n")?;
        std::fs::write(root.join("comprename.txt"), b"n\n")?;
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

    // R20 (F5–F8): install the file-op globals HERE (never the production Trash /
    // general pasteboard — hermeticity): a fresh history over a temp-dir
    // `FakeTrasher`, and the pasteboard adapter over a recording fake. No
    // production focus-follow closure ⇒ cross-window undo isn't exercised here
    // (single-window legs); undo/redo apply their inverses regardless.
    let trash_root = fixture.root.join(".fake-trash");
    if let Err(e) = std::fs::create_dir_all(&trash_root) {
        return CadenceReport::error(format!("file-browser: could not make the fake trash dir: {e}"));
    }
    cx.update(|app| {
        let service = FileOperationsService::new(Box::new(FakeTrasher::new(trash_root.clone())));
        let history = app.new(|_| FileOperationHistory::new(service, None));
        // Install the production focus-follow closure (the §6 composition leg's
        // cross-window ⌘Z routes focus back to window A). Windows opened through the
        // shipped builder are registered in the `WindowRegistry` (lazily created by
        // `register`, no `install`), so the router resolves origins over them. Inert
        // for the single-window R20 legs above (routing to the sole live window A).
        crate::file_browser::focus_route::install(app, &history);
        app.set_global(FileOperationHistoryGlobal(history));
        let pb: Box<dyn FilePasteboard> = Box::new(FakeFilePasteboard::new());
        app.set_global(FilePasteboardGlobal::new(pb));
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

    // R20 legs (Validation step 4 a–f): copy/paste, cut/ghost/move, trash+⌘Z,
    // rename, in-tree drag, and undo drift — NOT the CGEvent composition leg
    // (that Milestone-5 leg is the close-out slice's).
    r20_legs(cx, whandle, &fb, &fixture, &mut failures).await;

    // R20 headline: the extension-change confirmation modal END TO END (the
    // `run_rename_modals` present → confirm → apply and present → cancel → abort
    // wiring the extension-preserving `r20_legs` renames never reach).
    rename_confirm_modal_leg(cx, whandle, &fb, &state, &fixture, &mut failures).await;

    // Validation step 6 — the §6 shipped-surface composition leg (the Milestone-5
    // claim): two REAL windows, a CGEvent ⌘Z in window B undoing window A's op with
    // focus routed back. Runs here while window A's root is still the fixture tree
    // (before `reroot_check` re-roots it) and its sidebar is still in Files mode.
    composition_leg(cx, whandle, &fb, &fixture, &state, &main_tab, pid, &mut failures).await;

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

// ---- R20 legs (Validation step 4 a–f) --------------------------------------

fn exists(p: &str) -> bool {
    Path::new(p).exists()
}

async fn r20_legs(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    let src = fixture.path("src");

    // (a) copy → paste into a folder; a second paste lands `copyme copy.txt`.
    let copyme = fixture.path("copyme.txt");
    fb.update(cx, |v, cx| v.drive_copy(&copyme, cx));
    settle(cx, 120).await;
    fb.update(cx, |v, cx| v.drive_paste(&src, cx));
    settle(cx, 150).await;
    fb.update(cx, |v, cx| v.drive_paste(&src, cx));
    settle(cx, 150).await;
    let first = fixture.path("src/copyme.txt");
    let second = fixture.path("src/copyme copy.txt");
    if !exists(&first) || !exists(&second) {
        failures.push(format!(
            "copy/paste: two pastes into src should land {first} then {second}"
        ));
    } else {
        eprintln!("[selftest] file-browser R20: copy → paste twice landed 'copyme.txt' then 'copyme copy.txt'");
    }

    // (b1) cut ghosts the row; paste moves the tree.
    let cutme = fixture.path("cutme.txt");
    fb.update(cx, |v, cx| v.drive_cut(&cutme, cx));
    settle(cx, 100).await;
    let ghosted = fb.update(cx, |v, cx| v.scenario_cut_paths(cx));
    if !ghosted.iter().any(|p| p == &cutme) {
        failures.push("cut: the cut row must be ghosted (in the observable cut set)".to_string());
    }
    fb.update(cx, |v, cx| v.drive_paste(&src, cx));
    settle(cx, 150).await;
    if exists(&cutme) || !exists(&fixture.path("src/cutme.txt")) {
        failures.push("cut/paste: a cut then paste must MOVE cutme.txt into src".to_string());
    }

    // (b2) an external-style pasteboard mutation degrades cut → copy (un-ghosts).
    let degrade = fixture.path("degrade.txt");
    let other = fixture.path("other.txt");
    fb.update(cx, |v, cx| v.drive_cut(&degrade, cx));
    settle(cx, 100).await;
    cx.update(|app| {
        if app.has_global::<FilePasteboardGlobal>() {
            // Another app grabs the pasteboard (a write with different URLs bumps
            // the changeCount under our cut companion).
            app.global_mut::<FilePasteboardGlobal>()
                .0
                .write(&[PathBuf::from(&other)], Intent::Copy);
        }
    });
    let ghosted2 = fb.update(cx, |v, cx| v.scenario_cut_paths(cx));
    if ghosted2.iter().any(|p| p == &degrade) {
        failures.push(
            "cut degrade: an external pasteboard mutation must invalidate the cut (un-ghost)"
                .to_string(),
        );
    } else if failures.is_empty() {
        eprintln!("[selftest] file-browser R20: cut ghosted the row, paste moved the tree, an external mutation degraded the cut to a copy");
    }

    // (c) trash (FakeTrasher) → ⌘Z restores into a COLLAPSED dir → ⌘⇧Z re-trashes.
    let gone = fixture.path("restoredir/gone.txt");
    fb.update(cx, |v, cx| v.drive_trash(&gone, cx));
    settle(cx, 150).await;
    if exists(&gone) {
        failures.push("trash: gone.txt should have been recycled".to_string());
    }
    cx.update(|app| app.dispatch_action(&UndoFileOperation));
    settle(cx, 200).await;
    if !exists(&gone) {
        failures.push(
            "undo trash: ⌘Z must restore gone.txt (into the still-collapsed restoredir)".to_string(),
        );
    }
    cx.update(|app| app.dispatch_action(&RedoFileOperation));
    settle(cx, 200).await;
    if exists(&gone) {
        failures.push("redo trash: ⌘⇧Z must re-trash gone.txt with a fresh trash URL".to_string());
    } else {
        eprintln!("[selftest] file-browser R20: trash → ⌘Z restored into a collapsed dir → ⌘⇧Z re-trashed");
    }

    // (d) menu-rename: typed edit + Return commits (basename preselected); Esc
    //     reverts; a `/` draft STAYS in edit mode.
    let renameme = fixture.path("renameme.txt");
    begin_rename(cx, whandle, fb, &renameme).await;
    let renaming = fb.update(cx, |v, _| v.scenario_is_renaming());
    let sel = fb.update(cx, |v, _| v.scenario_rename_selection());
    if !renaming {
        failures.push("rename: begin did not enter edit mode".to_string());
    }
    if sel != Some((0, 8)) {
        failures.push(format!(
            "rename: the basename 'renameme' must be preselected [0,8); got {sel:?}"
        ));
    }
    fb.update(cx, |v, cx| v.drive_rename_type('x', cx));
    settle(cx, 60).await;
    let text = fb.update(cx, |v, _| v.scenario_rename_text());
    if text.as_deref() != Some("x.txt") {
        failures.push(format!(
            "rename: typing over the preselected base should yield 'x.txt'; got {text:?}"
        ));
    }
    commit_rename(cx, whandle, fb).await;
    if exists(&renameme) || !exists(&fixture.path("x.txt")) {
        failures.push("rename commit: Return must rename renameme.txt → x.txt".to_string());
    }

    let escme = fixture.path("escme.txt");
    begin_rename(cx, whandle, fb, &escme).await;
    fb.update(cx, |v, cx| v.drive_rename_type('y', cx));
    cancel_rename(cx, whandle, fb).await;
    if !exists(&escme) || exists(&fixture.path("y.txt")) {
        failures.push("rename cancel: Esc must revert (escme.txt intact, no y.txt)".to_string());
    }

    let slashme = fixture.path("slashme.txt");
    begin_rename(cx, whandle, fb, &slashme).await;
    fb.update(cx, |v, cx| v.drive_rename_type('/', cx));
    commit_rename(cx, whandle, fb).await;
    if !fb.update(cx, |v, _| v.scenario_is_renaming()) {
        failures.push("rename: a '/' draft must STAY in edit mode, never commit".to_string());
    } else {
        eprintln!("[selftest] file-browser R20: menu-rename typed+committed (base preselected), Esc reverted, '/' stayed in edit mode");
    }
    cancel_rename(cx, whandle, fb).await; // clean up the open field

    // (e) in-tree drag of a multi-selection onto a folder row moves it; the accent
    //     hover-highlight predicate (can_drop) is asserted.
    let drag_a = fixture.path("dragA.txt");
    let drag_b = fixture.path("dragB.txt");
    fb.update(cx, |v, cx| v.drive_select(&drag_a, cx));
    fb.update(cx, |v, cx| v.drive_add_to_selection(&drag_b, cx));
    settle(cx, 60).await;
    let target_ok = fb.update(cx, |v, cx| v.scenario_can_drop(&drag_a, &src, cx));
    let self_drop = fb.update(cx, |v, cx| v.scenario_can_drop(&drag_a, &drag_a, cx));
    if !target_ok || self_drop {
        failures.push(
            "drag highlight: can_drop must accept the folder target and reject a self-drop"
                .to_string(),
        );
    }
    whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| v.drive_drag_drop(&drag_a, &src, window, cx))
        })
        .ok();
    settle(cx, 150).await;
    if !exists(&fixture.path("src/dragA.txt"))
        || !exists(&fixture.path("src/dragB.txt"))
        || exists(&drag_a)
        || exists(&drag_b)
    {
        failures.push(
            "drag/drop: a multi-selection drag onto src must move BOTH files".to_string(),
        );
    } else {
        eprintln!("[selftest] file-browser R20: an in-tree multi-selection drag moved both files onto the folder (hover-highlight predicate asserted)");
    }

    // (f) drift: a move whose target vanishes → ⌘Z shows the frozen banner, op dropped.
    let driftme = fixture.path("driftme.txt");
    fb.update(cx, |v, cx| v.drive_cut(&driftme, cx));
    settle(cx, 80).await;
    fb.update(cx, |v, cx| v.drive_paste(&src, cx));
    settle(cx, 150).await;
    let moved = fixture.path("src/driftme.txt");
    if !exists(&moved) {
        failures.push("drift setup: driftme.txt should have moved into src".to_string());
    }
    let _ = std::fs::remove_file(&moved); // user deletes it out from under the history
    cx.update(|app| app.dispatch_action(&UndoFileOperation));
    settle(cx, 150).await;
    let (msg, redo_len) = cx.update(|app| match app.try_global::<FileOperationHistoryGlobal>() {
        Some(g) => {
            let h = g.0.read(app);
            (
                h.last_drift_message().map(str::to_string),
                h.redo_stack().len(),
            )
        }
        None => (None, 0),
    });
    let expected = "Couldn't undo: 'driftme.txt' is no longer there.";
    if msg.as_deref() != Some(expected) {
        failures.push(format!(
            "drift: ⌘Z on a vanished move target must show the frozen banner '{expected}'; got {msg:?}"
        ));
    }
    if redo_len != 0 {
        failures.push("drift: a drifted undo must DROP the op (redo stack stays empty)".to_string());
    }
    if msg.as_deref() == Some(expected) && redo_len == 0 {
        eprintln!("[selftest] file-browser R20: undo drift showed the frozen banner and dropped the op");
    }
}

async fn begin_rename(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    path: &str,
) {
    whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| v.drive_begin_rename(path, window, cx))
        })
        .ok();
    settle(cx, 100).await;
}

async fn commit_rename(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
) {
    whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| v.drive_rename_commit(window, cx))
        })
        .ok();
    settle(cx, 150).await;
}

async fn cancel_rename(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
) {
    whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| v.drive_rename_cancel(window, cx))
        })
        .ok();
    settle(cx, 100).await;
}

// ---- R20 extension-change confirmation-modal orchestration -----------------

/// Drive the extension-change confirmation modal END TO END — the R20 headline
/// the extension-preserving `r20_legs` renames never exercise. A `.txt → .md`
/// rename presents the modal (`modals_for` → `present_confirmation`); confirming
/// applies it on disk and refocuses, a separate cancel aborts (file untouched)
/// and still refocuses. This is the ONLY coverage of the `run_rename_modals`
/// present → confirm → apply and present → cancel → abort wiring (`modals_for`
/// itself is table-tested in `rename.rs`).
async fn rename_confirm_modal_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    state: &Entity<WindowState>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    const EXT_TITLE: &str = "Are you sure you want to change the extension?";
    let start = failures.len();

    // (confirm) extchange.txt → extchange.md: the modal is presented; confirm
    // applies the rename on disk and bumps the terminal-refocus counter.
    let src = fixture.path("extchange.txt");
    let dst = fixture.path("extchange.md");
    retype_rename(cx, whandle, fb, &src, "extchange.md").await;
    let refocus_before = fb.update(cx, |v, _| v.scenario_refocus_count());
    commit_rename(cx, whandle, fb).await;
    let title = fb.update(cx, |v, cx| v.scenario_pending_modal_title(cx));
    if title.as_deref() != Some(EXT_TITLE) {
        failures.push(format!(
            "rename ext-modal: committing a .txt→.md rename must present the extension-change modal; got title {title:?}"
        ));
    }
    answer_modal(cx, whandle, state, true).await;
    if exists(&src) || !exists(&dst) {
        failures.push(
            "rename ext-modal confirm: confirming the extension modal must rename extchange.txt → extchange.md".to_string(),
        );
    }
    if fb.update(cx, |v, _| v.scenario_refocus_count()) <= refocus_before {
        failures.push(
            "rename ext-modal confirm: applying the rename must refocus the terminal".to_string(),
        );
    }

    // (cancel) extcancel.txt → extcancel.md: the modal is presented; cancel aborts
    // (the fs stays untouched) and STILL refocuses the terminal.
    let src = fixture.path("extcancel.txt");
    let dst = fixture.path("extcancel.md");
    retype_rename(cx, whandle, fb, &src, "extcancel.md").await;
    let refocus_before = fb.update(cx, |v, _| v.scenario_refocus_count());
    commit_rename(cx, whandle, fb).await;
    if fb.update(cx, |v, cx| v.scenario_pending_modal_title(cx)).as_deref() != Some(EXT_TITLE) {
        failures
            .push("rename ext-modal cancel: the extension-change modal was not presented".to_string());
    }
    answer_modal(cx, whandle, state, false).await;
    if !exists(&src) || exists(&dst) {
        failures.push(
            "rename ext-modal cancel: cancelling the extension modal must leave extcancel.txt untouched (no extcancel.md)".to_string(),
        );
    }
    if fb.update(cx, |v, _| v.scenario_refocus_count()) <= refocus_before {
        failures.push(
            "rename ext-modal cancel: aborting on cancel must still refocus the terminal".to_string(),
        );
    }

    if failures.len() == start {
        eprintln!("[selftest] file-browser R20: extension-change modal — confirm applied .txt→.md on disk, cancel left the file untouched (both refocused the terminal)");
    }
}

/// Begin an inline rename on `path`, ⌘A-select the whole field, then type the
/// full `new_name` (an extension change needs the whole field — the basename
/// preselection alone keeps the old extension).
async fn retype_rename(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    path: &str,
    new_name: &str,
) {
    begin_rename(cx, whandle, fb, path).await;
    fb.update(cx, |v, cx| v.drive_rename_select_all(cx));
    for ch in new_name.chars() {
        fb.update(cx, |v, cx| v.drive_rename_type(ch, cx));
    }
    settle(cx, 60).await;
}

/// Answer the pending confirmation modal (confirm / cancel) directly, from the
/// raw app context via the `WindowState` entity (hermeticity: the modal answer is
/// driven, not real-clicked — the `persistence-restore` precedent). Resolved
/// OUTSIDE any `FileBrowserView` update: the modal's completion re-enters the view
/// to recurse/refocus, which would double-borrow it inside `fb.update`.
async fn answer_modal(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    confirmed: bool,
) {
    let modal = state.update(cx, |s, _| s.pending_modal());
    if let Some(modal) = modal {
        let _ = whandle.update(cx, |_root, window, app| {
            modal.update(app, |m, mcx| m.resolve(confirmed, window, mcx));
        });
    }
    settle(cx, 150).await;
}

// ---- Validation step 6: the §6 final-composition leg -----------------------

/// The Milestone-5 shipped-surface claim, end-to-end on the REAL composition:
/// enter files mode, click-select two rows, context-menu Copy → Paste (recorded
/// on the fakes + applied on disk), a slow-second-click rename + commit, then open
/// a SECOND real window via a ⌘N CGEvent and press ⌘Z THERE — the op undoes AND
/// focus routes back to window A (active + sidebar Files + origin tab). The chords
/// that gpui matches by character (⌘N, ⌘Z) are REAL CGEvents to our own pid; the
/// row-level interactions use the same real router seams the rest of this scenario
/// drives (pixel-accurate row clicks aren't synthesizable via `CGEventPostToPid`).
#[allow(clippy::too_many_arguments)]
async fn composition_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    state: &Entity<WindowState>,
    main_tab: &str,
    pid: i32,
    failures: &mut Vec<String>,
) {
    use std::collections::HashSet;

    use gpui::WindowId;
    use nice_model::SidebarMode;

    // Window A frontmost/key + confirmed in files mode (the whole leg drives it).
    rekey(cx, whandle).await;
    if state.update(cx, |s, _| s.sidebar.mode()) != SidebarMode::Files {
        tap(cx, pid, KC_B, platform::FLAG_COMMAND | platform::FLAG_SHIFT).await;
        settle(cx, 250).await;
    }
    if state.update(cx, |s, _| s.sidebar.mode()) != SidebarMode::Files {
        failures.push("composition: window A never settled into files mode".to_string());
        return;
    }
    let a_id = AnyWindowHandle::from(whandle).window_id();

    // --- click-select two rows, then context-menu Copy → Paste into a folder ---
    let comp1 = fixture.path("comp1.txt");
    let comp2 = fixture.path("comp2.txt");
    let compdir = fixture.path("compdir");
    fb.update(cx, |v, cx| v.drive_select(&comp1, cx));
    fb.update(cx, |v, cx| v.drive_add_to_selection(&comp2, cx));
    settle(cx, 80).await;
    fb.update(cx, |v, cx| v.drive_copy(&comp1, cx)); // copies the whole selection
    settle(cx, 120).await;
    fb.update(cx, |v, cx| v.drive_paste(&compdir, cx));
    settle(cx, 180).await;
    let pasted1 = fixture.path("compdir/comp1.txt");
    let pasted2 = fixture.path("compdir/comp2.txt");
    if !exists(&pasted1) || !exists(&pasted2) {
        failures.push(
            "composition: context-menu Copy → Paste did not land both rows in compdir on disk"
                .to_string(),
        );
    }

    // --- rename one row via slow-second-click, then commit ---------------------
    let renameme = fixture.path("comprename.txt");
    // Two distinct single clicks spaced beyond the 280 ms double-click window: the
    // first sole-selects the file, the second (on the already-sole file) arms the
    // deferred slow-second-click rename (the real router path, files-only).
    fb.update(cx, |v, cx| v.drive_single_click(&renameme, cx));
    settle(cx, 400).await;
    fb.update(cx, |v, cx| v.drive_single_click(&renameme, cx));
    // Poll for the armed deferral (280 ms timer) + the render that consumes it.
    let mut renaming = false;
    for _ in 0..20 {
        settle(cx, 60).await;
        if fb.update(cx, |v, _| v.scenario_is_renaming()) {
            renaming = true;
            break;
        }
    }
    if !renaming {
        failures.push(
            "composition: a slow-second-click did not enter inline rename on comprename.txt"
                .to_string(),
        );
    } else {
        // Type over the preselected basename → "cr.txt" (a target disjoint from
        // every fixture name AND from leg d's `x.txt` output — the source files are
        // disjoint, but the rename TARGET must be too or the raw single-pair move
        // hits leg d's `x.txt` and surfaces the frozen "already exists" refusal),
        // then commit (Return path).
        fb.update(cx, |v, cx| v.drive_rename_type('c', cx)); // replaces the base
        fb.update(cx, |v, cx| v.drive_rename_type('r', cx)); // appends → "cr"
        settle(cx, 60).await;
        commit_rename(cx, whandle, fb).await;
    }
    let renamed = fixture.path("cr.txt");
    if !renamed_committed(&renameme, &renamed) {
        failures.push(
            "composition: the slow-second-click rename did not commit comprename.txt → cr.txt"
                .to_string(),
        );
    }

    // --- open a SECOND real window via a ⌘N CGEvent ----------------------------
    let before: HashSet<WindowId> =
        cx.update(|app| app.windows().iter().map(|w| w.window_id()).collect());
    rekey(cx, whandle).await;
    tap(cx, pid, KC_N, platform::FLAG_COMMAND).await;
    settle(cx, 500).await;
    let b_handle = cx.update(|app| {
        app.windows()
            .into_iter()
            .find(|w| !before.contains(&w.window_id()))
    });
    let Some(b_handle) = b_handle else {
        failures.push("composition: ⌘N did not open a second real window".to_string());
        return;
    };

    // Drive B frontmost/key and confirm it keyed before posting the cross-window
    // ⌘Z (a routing miss then reports as "B never keyed", not a confusing verdict).
    let _ = b_handle.update(cx, |_v, window, _app| window.activate_window());
    settle(cx, 400).await;
    let b_is_key =
        cx.update(|app| app.active_window().map(|w| w.window_id())) == Some(b_handle.window_id());
    if !b_is_key {
        failures.push(
            "composition: the ⌘N window B never became key, so ⌘Z could not be routed to it"
                .to_string(),
        );
        close_and_reap(cx, b_handle).await;
        return;
    }

    // --- ⌘Z in window B undoes window A's op AND routes focus back to A ---------
    tap(cx, pid, KC_Z, platform::FLAG_COMMAND).await;
    // Poll: the undo (rename Move inverse) restores comprename.txt, and the focus
    // route brings window A frontmost.
    let mut undone = false;
    let mut a_active = false;
    for _ in 0..20 {
        settle(cx, 100).await;
        undone = exists(&renameme) && !exists(&renamed);
        a_active = cx.update(|app| app.active_window().map(|w| w.window_id())) == Some(a_id);
        if undone && a_active {
            break;
        }
    }
    if !undone {
        failures.push(
            "composition: ⌘Z in window B did not undo window A's rename (comprename.txt not restored)"
                .to_string(),
        );
    }
    if !a_active {
        failures.push(
            "composition: cross-window ⌘Z did not route focus back to window A (A never became active)"
                .to_string(),
        );
    }
    let a_mode = state.update(cx, |s, _| s.sidebar.mode());
    let a_tab = state.update(cx, |s, _| s.model.active_tab_id().map(str::to_string));
    if a_mode != SidebarMode::Files {
        failures.push(format!(
            "composition: focus route left window A in {a_mode:?}, expected Files mode"
        ));
    }
    if a_tab.as_deref() != Some(main_tab) {
        failures.push(format!(
            "composition: focus route did not select window A's origin tab (got {a_tab:?}, want {main_tab:?})"
        ));
    }
    if undone && a_active && a_mode == SidebarMode::Files && a_tab.as_deref() == Some(main_tab) {
        eprintln!(
            "[selftest] file-browser §6: Copy→Paste + slow-second-click rename on window A, ⌘N opened window B, \
             CGEvent ⌘Z in B undid A's op and routed focus back (A active + Files + origin tab)"
        );
    }

    // Close + reap window B, then hand focus back to window A for the later legs.
    close_and_reap(cx, b_handle).await;
    rekey(cx, whandle).await;
}

/// Close a scenario-opened window AND reap its state. `remove_window` closes the
/// NSWindow (programmatic — no confirm gate; no close observer is installed here,
/// so it never quits), but the `WindowRegistry`'s strong `WindowState` handle
/// would otherwise keep the window's Main-pane pty alive; `route_close_disk_fate`
/// deregisters it and tears its sessions down (reaping the pty). Store calls
/// inside are no-ops — the scenario installs no session store.
async fn close_and_reap(cx: &mut AsyncApp, handle: AnyWindowHandle) {
    let id = handle.window_id();
    let _ = handle.update(cx, |_v, window, _app| window.remove_window());
    let _ = cx.update(|app| WindowRegistry::route_close_disk_fate(app, id));
    settle(cx, 250).await;
}

/// A rename committed iff the original is gone and the new name landed (used so a
/// failure to enter rename mode doesn't spuriously pass this check).
fn renamed_committed(original: &str, renamed: &str) -> bool {
    !exists(original) && exists(renamed)
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
                     worked, ⌘⇧B still flips modes, the R20 legs (copy/paste, cut-ghost-move, \
                     trash+⌘Z, rename, drag, drift) passed, and the §6 composition leg (⌘N second \
                     window, CGEvent ⌘Z in B undoing A's op with focus routed back) held."
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
