//! `file-browser` self-test scenario — the R19 shipped-surface gate (What to
//! build #7). Opens through the SHIPPED builder (`open_managed_window` →
//! `build_window_root` → `AppShellView`, the exact path `run` takes), roots the
//! active tab's browser at a temp fixture tree, and drives the real composition:
//!
//! (a) a real ⌘⇧B chord (the shipped `ToggleSidebarMode` keymap) swaps the tab
//!     list for the tree in the live window — the AX root
//!     `nice-file-browser-root` surfaces as an `AXGroup` and a fixture row is
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
//! Plan-2 leg: (e′) with TWO files selected, a REAL left press on one of them
//! must not collapse the selection and a REAL release in place must — the
//! select-then-drag contract the multi-selection drag payload rides on; the full
//! real drag onto a folder row is attempted and defers loudly when it does not
//! arm (see [`multi_select_press_leg`]).
//!
//! Inline-rename legs (on top of R20's (d) / (d′)): (d-word) real ⌥/⌘ editing
//! chords (⌥←/⌥→/⌥⇧←/⌘←/⌘→/⌘⇧←/⌥⌫) walk a fixed caret/selection table in the
//! open field; (d-drag) a press at one painted char boundary and a move to
//! another selects that range and typing replaces it — attempted as a REAL
//! guarded global-HID gesture and, either way, hard-asserted through the
//! production hit-test over the geometry the field painted; (d-clip) real
//! ⌘A/⌘C/⌘V chords round-trip the field text through the REAL system clipboard
//! and a seeded multi-line paste lands as one sanitized line (the leg saves and
//! restores whatever the clipboard held before it ran).
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
use gpui::{AnyWindowHandle, AppContext, AsyncApp, ClipboardItem, Entity, WindowHandle};

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
/// ← / → / ⌫ (`kVK_LeftArrow` / `kVK_RightArrow` / `kVK_Delete`) — the (d-word)
/// leg's real ⌥/⌘ editing chords. Arrows and Backspace are FUNCTIONAL keys: their
/// meaning comes from the keycode alone, so they are posted with no unicode
/// override (layout-independent, unlike the character-matched chords).
const KC_LEFT: u16 = 123;
const KC_RIGHT: u16 = 124;
const KC_BACKSPACE: u16 = 51;
/// a / c / v (`kVK_ANSI_A` / `_C` / `_V`) — the (d-clip) leg's ⌘A/⌘C/⌘V chords,
/// posted by keycode like the ⌘⇧B / ⌘Z / ⌘N chords above (letter keycodes carry
/// their own `charactersIgnoringModifiers`, so no unicode override is needed).
/// The bare `c` tap doubles as the leg's "type a suffix" keystroke.
const KC_A: u16 = 0;
const KC_C: u16 = 8;
const KC_V: u16 = 9;

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
            "nice-file-browser-{}-{}",
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
        // Plan-1 inline-rename legs: a MULTI-WORD name for the (d-word) ⌥/⌘
        // motion table (word runs "alpha" / "beta_gamma" / "txt" separated by a
        // space and a dot — the two separator classes motion treats alike), and a
        // single-run name for the (d-drag) gesture. Both are cancelled, never
        // committed, so they stay on disk for the leg's own untouched-check.
        std::fs::write(root.join("alpha beta_gamma.txt"), b"w\n")?;
        std::fs::write(root.join("dragselect.txt"), b"d\n")?;
        // (d-clip): its own rename target, cancelled like the two above, so the
        // clipboard round trip can assert an exact field text with no other leg's
        // edits in it.
        std::fs::write(root.join("clipme.txt"), b"c\n")?;
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
        // Plan-2 (e′) real-event multi-selection leg: two files pressed with REAL
        // CGEvents plus their own drop target, all disjoint from every other leg's
        // fixtures so the leg's fs outcome is unambiguous.
        std::fs::create_dir_all(root.join("multidrag"))?;
        std::fs::write(root.join("multi1.txt"), b"1\n")?;
        std::fs::write(root.join("multi2.txt"), b"2\n")?;
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
    // Items the PLATFORM refused to let us drive for real (a synthetic press the
    // OS never delivered), reported loudly and handed to a human pass — never a
    // silent pass, and never in place of a hard assertion: every deferring leg
    // still pins its behaviour deterministically (the `update-check` /
    // `tranche6-composition` discipline).
    let mut deferred: Vec<String> = Vec::new();

    // (a) ⌘⇧B → files mode; the tree replaces the tab list.
    let Some(fb) = enter_files_mode(cx, whandle, &sidebar, pid, &mut failures).await else {
        return build_report(failures, deferred); // nothing else can run without the view
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

    // Plan-1 rename-field legs (Bugs A + B), on the SAME shared field the R20
    // rename legs above drive: (d-word) real ⌥/⌘ editing chords, (d-drag) a real
    // press-drag selection. Both run while window A is still the only window and
    // the tree is still rooted at the fixture (before the §6 second window and
    // the re-root below).
    rename_word_keys_leg(cx, whandle, &fb, &fixture, pid, &mut failures).await;
    rename_drag_select_leg(cx, whandle, &fb, &fixture, &mut failures, &mut deferred).await;
    // (d-clip) the clipboard chords on the same shared field — real ⌘A/⌘C/⌘V
    // through the REAL system pasteboard (saved and restored by the leg).
    rename_clipboard_leg(cx, whandle, &fb, &fixture, pid, &mut failures).await;

    // R20 headline: the extension-change confirmation modal END TO END (the
    // `run_rename_modals` present → confirm → apply and present → cancel → abort
    // wiring the extension-preserving `r20_legs` renames never reach).
    rename_confirm_modal_leg(cx, whandle, &fb, &state, &fixture, &mut failures).await;

    // Validation step 6 — the §6 shipped-surface composition leg (the Milestone-5
    // claim): two REAL windows, a CGEvent ⌘Z in window B undoing window A's op with
    // focus routed back. Runs here while window A's root is still the fixture tree
    // (before `reroot_check` re-roots it) and its sidebar is still in Files mode.
    composition_leg(cx, whandle, &fb, &fixture, &state, &main_tab, pid, &mut failures).await;

    // (e′) plan-2: REAL press / release inside a 2-file selection (the press must
    // not collapse it; the release must), plus an attempted real end-to-end drag.
    // Runs after the composition leg (which leaves window A frontmost) and before
    // the re-root below, so the tree is still the fixture and its rows are painted.
    multi_select_press_leg(cx, whandle, &fb, &fixture, &mut failures, &mut deferred).await;

    // (c) double-click a folder re-roots (done late — it changes the root).
    reroot_check(cx, &fb, &fixture.path("src"), &mut failures).await;

    // (g cont.) ⌘⇧B still flips modes.
    mode_flip_check(cx, whandle, &state, pid, &mut failures).await;

    build_report(failures, deferred)
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

    // Hidden: dotfiles default to HIDDEN everywhere (the 2026-07-07 deviation
    // from Swift's cwd heuristic), so .env is absent until the user opts in; a
    // real ⌘⇧. chord reveals it, and the control-strip toggle hides it again.
    let dotfile = fixture.path(".env");
    let shown = |cx: &mut AsyncApp| fb.update(cx, |v, _| v.scenario_rendered_paths()).iter().any(|p| p == &dotfile);
    if shown(cx) {
        failures.push(format!(
            "hidden: the dotfile {dotfile} must be HIDDEN by default (the 2026-07-07 deviation)"
        ));
    }
    // ⌘⇧. reveal: dispatch the SHIPPED `ToggleHiddenFiles` action (the ⌘⇧.
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
            "hidden: the shipped ToggleHiddenFiles action (⌘⇧.) did not reveal the dotfile — \
             the files-mode/state-exists double gate did not fire"
                .to_string(),
        );
    }
    // The control-strip toggle hides it again (restoring the hidden default).
    fb.update(cx, |v, cx| v.drive_toggle_hidden(cx));
    settle(cx, 200).await;
    if shown(cx) {
        failures.push("hidden: the control-strip toggle did not re-hide the dotfile".to_string());
    } else {
        eprintln!(
            "[selftest] file-browser: dotfiles hidden by default; ⌘⇧. revealed .env, \
             the control-strip toggle hid it again"
        );
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

    // (d') click-to-position: a click INSIDE the open field repositions the caret
    //      without restarting the edit. Bias-proofed against the probe-offset bug
    //      class (the field-box-vs-text-run off-by-one): (1) the two live layout
    //      probes are cross-checked against each other — the text-run probe must
    //      sit exactly the field's 6px horizontal padding right of the field-box
    //      probe, so a probe regression cannot cancel out of the click math; and
    //      (2) the clicks pin the half-glyph convention at REAL window
    //      coordinates through the production probe path: the text's left pixel
    //      edge → caret 0, the left half of glyph 3 → caret 3 (before it), just
    //      right of glyph 3's midpoint → caret 4 (after it).
    // The target must be a row the `uniform_list` actually RENDERS: only a
    // painted field publishes a boundary table, and this leg clicks at painted
    // geometry. `alpha.txt` sorts near the top of the fixture (dirs first, then
    // README.md / "alpha beta_gamma.txt" / this), so it is inside the viewport;
    // the leg used to name `slashme.txt`, ~20 rows down and never rendered, and
    // "passed" only because the per-view probe still held the LAST painted
    // field's table — the click math and the hit-test were then the same stale
    // numbers, i.e. circular. Rename-begin now resets that cell, so an
    // off-screen target fails loudly instead.
    const CLICK_NAME: &str = "alpha.txt"; // base "alpha" [0,5)
    let clickme = fixture.path(CLICK_NAME);
    // An inactive window does not redraw at all — raise before waiting on a paint.
    rekey(cx, whandle).await;
    begin_rename(cx, whandle, fb, &clickme).await;
    if !await_rename_paint(cx, fb, CLICK_NAME.chars().count()).await {
        failures.push(format!(
            "rename click: the open field never painted a boundary table describing \
             '{CLICK_NAME}' — there is no drawn geometry to click at"
        ));
    } else {
        let sel0 = fb.update(cx, |v, _| v.scenario_rename_selection());
        let (field_left, text_left) = fb.update(cx, |v, _| v.scenario_rename_probe());
        // (1) probe cross-check: the text run sits at field box + 6px padding (the
        // taffy absolute-inset origin excludes the 1px border). A ~0 delta is the
        // recorded-the-field-box bug that biased every click one glyph right.
        let delta = text_left - field_left;
        if !(5.0..=8.0).contains(&delta) {
            failures.push(format!(
                "rename click: probe cross-check failed — text_left({text_left}) - field_left({field_left}) \
                 = {delta}, expected ≈6px (the field padding); the text probe is not on the text run"
            ));
        }
        // (2) half-glyph convention at window coordinates, through the real probe
        //     math — the boundary xs now come from the field's own last paint (no
        //     window needed), so the click targets are the glyphs that were drawn.
        let checks: Vec<(String, f32, usize)> = fb.update(cx, |v, _| {
            let b3 = v.scenario_rename_x_for_index(3).unwrap_or(0.0);
            let b4 = v.scenario_rename_x_for_index(4).unwrap_or(0.0);
            vec![
                ("text left edge → caret 0".to_string(), text_left + 0.5, 0),
                ("left half of glyph 3 → caret 3".to_string(), text_left + b3 + 1.0, 3),
                (
                    "right of glyph 3's midpoint → caret 4".to_string(),
                    text_left + (b3 + b4) / 2.0 + 1.0,
                    4,
                ),
            ]
        });
        for (what, x, want) in checks {
            let (mapped, sel_after, still) = whandle
                .update(cx, |_r, window, app| {
                    fb.update(app, |v, cx| {
                        let placed = v.drive_rename_click_at_window_x(x, window, cx);
                        (placed, v.scenario_rename_selection(), v.scenario_is_renaming())
                    })
                })
                .unwrap_or((None, None, false));
            if !still {
                failures.push(format!(
                    "rename click ({what}): the click ended / restarted the edit (edit mode dropped)"
                ));
                break;
            }
            if mapped != Some(want) || sel_after != Some((want, want)) {
                failures.push(format!(
                    "rename click ({what}): clicked window-x {x:.1}, expected caret {want}; \
                     got mapped={mapped:?} sel={sel_after:?}"
                ));
            }
        }
        let sel_final = fb.update(cx, |v, _| v.scenario_rename_selection());
        if sel_final == sel0 {
            failures.push(format!(
                "rename click: the caret never moved — selection stayed at the preselection {sel0:?} \
                 (clicks likely re-tripped begin-rename instead of repositioning)"
            ));
        } else {
            eprintln!(
                "[selftest] file-browser R20: rename-field clicks repositioned the caret at real \
                 window coordinates (probe delta {delta:.1}px; left-edge→0, glyph-3 left half→3, \
                 past midpoint→4) without restarting the edit"
            );
        }
    }
    cancel_rename(cx, whandle, fb).await;

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
        .update(cx, |_r, _window, app| {
            fb.update(app, |v, cx| v.drive_drag_drop(&drag_a, &src, cx))
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

/// Wait (up to ~2s) for the OPEN rename field to publish a boundary table that
/// describes a name of `chars` chars — i.e. for THIS field to have painted.
///
/// Every leg that aims a click at painted geometry has to call this. Rename-begin
/// resets the per-view probe cell, so an unpainted field reads as an empty table
/// instead of quietly handing back the PREVIOUS field's numbers (which is how the
/// click leg used to "pass": it re-renamed the same file, so the stale table
/// happened to match). The table is pinned, not merely non-empty: boundary
/// `chars` must exist and `chars + 1` must not, so a table left by a field with a
/// different name can never satisfy it.
async fn await_rename_paint(cx: &mut AsyncApp, fb: &Entity<FileBrowserView>, chars: usize) -> bool {
    for _ in 0..40 {
        let (last, past_end) = fb.update(cx, |v, _| {
            (
                v.scenario_rename_x_for_index(chars),
                v.scenario_rename_x_for_index(chars + 1),
            )
        });
        if last.is_some() && past_end.is_none() {
            return true;
        }
        settle(cx, 50).await;
    }
    false
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

/// Snapshot the live window while the rename field is OPEN, for the visual
/// spot-check.
///
/// The driver's `NICE_CAPTURE` shot is taken after the scenario reports its
/// verdict, by which point the window has been torn down — a rename field, which
/// exists only for a few hundred ms mid-run, cannot appear in it. This writes
/// `<NICE_CAPTURE base>-<stage>.png` at the moment the state is on screen
/// instead, and is a no-op when no capture was requested (the standing gate run
/// takes no screenshots). A capture that was ASKED for and then failed is a
/// scenario failure, exactly as it is in the driver — never a silent skip.
async fn capture_rename_stage(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    stage: &str,
    failures: &mut Vec<String>,
) {
    if nice_harness::capture::requested_path().is_none() {
        return;
    }
    settle(cx, 200).await; // let the state paint before the drawable read-back
    match nice_harness::capture::capture_stage(whandle.into(), cx, stage) {
        Ok(Some(path)) => eprintln!(
            "[selftest] file-browser: wrote a mid-rename capture ({stage}) -> {}",
            path.display()
        ),
        Ok(None) => {}
        Err(e) => failures.push(format!(
            "rename capture ({stage}): NICE_CAPTURE was requested but the mid-scenario \
             snapshot failed: {e}"
        )),
    }
}

// ---- (d-word) real ⌥/⌘ editing chords in the rename field ------------------

/// Bug A's live gate. With a rename open on a MULTI-WORD fixture name, post the
/// real ⌥/⌘ editing chords as CGEvents and assert the model's caret/selection
/// after each one against a fixed table.
///
/// The mapping itself is unit-tested (`inline_rename`'s dispatch tests) and the
/// motion semantics are table-tested in the model; what only a live run can
/// prove is the wiring the bug actually was — the chords reaching the FOCUSED
/// field as `alt`/`platform` modifiers on a real keystroke (`dispatch_rename_key`
/// had no `alt` parameter at all, so ⌥← arrived as a bare ←). Arrows and ⌫ are
/// functional keys, so `post_key_tap` posts them by keycode with no unicode
/// override — no `SavedInputSource` and none of the character-matching
/// divergence that keeps `⌘⇧.` out of the CGEvent path elsewhere in this file.
///
/// The rename is CANCELLED at the end: this leg asserts the field model, never a
/// filesystem outcome, so the fixture must be untouched afterwards.
async fn rename_word_keys_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    pid: i32,
    failures: &mut Vec<String>,
) {
    // "alpha beta_gamma.txt" — word runs [0,5) "alpha", [6,16) "beta_gamma"
    // (`_` is a word char), [17,20) "txt"; len 20.
    const NAME: &str = "alpha beta_gamma.txt";
    let path = fixture.path(NAME);
    let start = failures.len();

    rekey(cx, whandle).await;
    begin_rename(cx, whandle, fb, &path).await;
    let opened = fb.update(cx, |v, _| v.scenario_rename_text());
    if opened.as_deref() != Some(NAME) {
        failures.push(format!(
            "rename ⌥/⌘ keys: the rename never opened on '{NAME}' (field text {opened:?})"
        ));
        // A wrong-row edit may be open; close it so this failure can't cascade
        // into the later legs (matches the other early-outs in this function).
        cancel_rename(cx, whandle, fb).await;
        return;
    }

    // ⌘→ first: it parks the caret at the end (the table's deterministic start)
    // AND doubles as the reach-the-field gate. If a real CGEvent never lands on
    // the focused field, every row below would report the same thing — so fail
    // once, with the actionable reason, and stop.
    tap(cx, pid, KC_RIGHT, platform::FLAG_COMMAND).await;
    let after_cmd_right = fb.update(cx, |v, _| v.scenario_rename_selection());
    if after_cmd_right != Some((20, 20)) {
        failures.push(format!(
            "rename ⌥/⌘ keys: a real ⌘→ did not move the caret to the end of the open field \
             (selection {after_cmd_right:?}, expected (20,20)) — the chord never reached the \
             focused rename field (window not key, focus elsewhere, or the arrow chords are not \
             dispatched to it)"
        ));
        cancel_rename(cx, whandle, fb).await;
        return;
    }

    // (what, keycode, flags, expected selection after the chord).
    let table: [(&str, u16, u64, (usize, usize)); 8] = [
        ("⌥← from the end → start of \"txt\"", KC_LEFT, platform::FLAG_OPTION, (17, 17)),
        ("⌥← → start of \"beta_gamma\"", KC_LEFT, platform::FLAG_OPTION, (6, 6)),
        (
            "⌥⇧← extends a word left (anchor fixed)",
            KC_LEFT,
            platform::FLAG_OPTION | platform::FLAG_SHIFT,
            (0, 6),
        ),
        (
            "⌥→ out of the selection's end → end of \"beta_gamma\"",
            KC_RIGHT,
            platform::FLAG_OPTION,
            (16, 16),
        ),
        (
            "⌘⇧← extends to the text start",
            KC_LEFT,
            platform::FLAG_COMMAND | platform::FLAG_SHIFT,
            (0, 16),
        ),
        ("⌘← collapses to the text start", KC_LEFT, platform::FLAG_COMMAND, (0, 0)),
        ("⌥→ → end of \"alpha\"", KC_RIGHT, platform::FLAG_OPTION, (5, 5)),
        ("⌥→ → end of \"beta_gamma\"", KC_RIGHT, platform::FLAG_OPTION, (16, 16)),
    ];
    for (what, keycode, flags, want) in table {
        tap(cx, pid, keycode, flags).await;
        let got = fb.update(cx, |v, _| v.scenario_rename_selection());
        if got != Some(want) {
            failures.push(format!(
                "rename ⌥/⌘ keys ({what}): expected selection {want:?}, got {got:?}"
            ));
        }
    }

    // Visual spot-check material (Validation step 4): the field is open on a
    // row that is ON SCREEN, with a bare caret parked mid-text at the end of
    // "beta_gamma" — the state the driver's own capture can never show, since it
    // runs after the scenario has torn the window down.
    capture_rename_stage(cx, whandle, "rename-caret", failures).await;

    // ⌥⌫ deletes the word before the caret (caret at 16 = the end of
    // "beta_gamma") — the delete half of Bug A.
    tap(cx, pid, KC_BACKSPACE, platform::FLAG_OPTION).await;
    let text = fb.update(cx, |v, _| v.scenario_rename_text());
    let sel = fb.update(cx, |v, _| v.scenario_rename_selection());
    if text.as_deref() != Some("alpha .txt") || sel != Some((6, 6)) {
        failures.push(format!(
            "rename ⌥/⌘ keys (⌥⌫ deletes the previous word): expected 'alpha .txt' with the caret \
             at 6, got text {text:?} selection {sel:?}"
        ));
    }

    cancel_rename(cx, whandle, fb).await;
    if !exists(&path) || exists(&fixture.path("alpha .txt")) {
        failures.push(format!(
            "rename ⌥/⌘ keys: Esc must leave '{NAME}' untouched on disk (no 'alpha .txt')"
        ));
    }
    if failures.len() == start {
        eprintln!(
            "[selftest] file-browser (d-word): real ⌥←/⌥→/⌥⇧←/⌘←/⌘→/⌘⇧←/⌥⌫ CGEvents walked the \
             word table in the open rename field, and Esc left the file untouched"
        );
    }
}

// ---- (d-clip) real ⌘A/⌘C/⌘V clipboard chords in the rename field -----------

/// The clipboard bug's live gate: with a rename open, real ⌘A/⌘C/⌘V CGEvents
/// must round-trip the field text through the REAL system clipboard.
///
/// The chord→outcome rule is unit-tested against an in-memory fake
/// (`inline_rename`'s dispatch tests); what only a live run can prove is that a
/// REAL OS key event carrying the ⌘ chord reaches this process's focused rename
/// field and comes back out on the REAL system pasteboard — the ground-truth
/// half of the wiring the bug actually was.
///
/// This leg posts CGEvents, so like every other leg in this scenario it cannot
/// run without the Accessibility (TCC) grant, and it is therefore NOT the only
/// gate on the fix: the grant-free half of the same wiring claim (the chord
/// reaching the focused field rather than falling through as `Ignored`, the
/// production `RenameClipboard for App` impl reading/writing the same clipboard
/// the rest of the app does, and the call sites' `&mut **cx` deref surviving the
/// entity update it runs inside) lives in
/// `crates/nice-itests/tests/behavior_rename_clipboard.rs`, which drives the
/// SHIPPED field on the mocked context and runs on any host under `cargo test`.
///
/// Four steps: ⌘A ⌘C copies the whole name out; ⌘V pastes it back over the
/// still-selected field (text unchanged — a paste that dropped or doubled
/// characters shows up here); a bare `c` tap proves the caret sits where the
/// paste left it; and a driver-seeded MULTI-LINE clipboard pastes as one
/// sanitized line.
///
/// The leg saves the pre-existing clipboard and restores it at the end — a
/// self-test must not eat whatever the person running it had copied. The rename
/// is CANCELLED, so the fixture must be untouched on disk afterwards.
async fn rename_clipboard_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    pid: i32,
    failures: &mut Vec<String>,
) {
    const NAME: &str = "clipme.txt";
    // Seeded for the sanitization step: two runs of control characters between
    // the segments, and an ordinary space that must survive verbatim.
    const MULTILINE: &str = "pa\r\n\tste me";
    const SANITIZED: &str = "pa ste me";
    let path = fixture.path(NAME);
    // The char offset of the end of the field text — the caret a paste over the
    // whole selection must leave behind.
    let end = NAME.chars().count();
    let start = failures.len();

    let saved = cx.update(|app| app.read_from_clipboard().and_then(|item| item.text()));

    rekey(cx, whandle).await;
    begin_rename(cx, whandle, fb, &path).await;
    let opened = fb.update(cx, |v, _| v.scenario_rename_text());
    if opened.as_deref() != Some(NAME) {
        failures.push(format!(
            "rename clipboard: the rename never opened on '{NAME}' (field text {opened:?})"
        ));
        cancel_rename(cx, whandle, fb).await;
        // A no-op today (nothing has been written yet), but every exit from
        // this leg restores symmetrically so a future edit that writes earlier
        // cannot leak the leg's clipboard content past a failure.
        restore_clipboard(cx, saved);
        return;
    }

    // ⌘A doubles as the reach-the-field gate (as ⌘→ does in the (d-word) leg):
    // if a real chord never lands on the focused field, every assertion below
    // would report the same thing, so fail once with the actionable reason.
    tap(cx, pid, KC_A, platform::FLAG_COMMAND).await;
    let after_cmd_a = fb.update(cx, |v, _| v.scenario_rename_selection());
    if after_cmd_a != Some((0, end)) {
        failures.push(format!(
            "rename clipboard: a real ⌘A did not select the whole open field (selection \
             {after_cmd_a:?}) — the chord never reached the focused rename field (window not key, \
             focus elsewhere, or ⌘-chords are not dispatched to it)"
        ));
        cancel_rename(cx, whandle, fb).await;
        restore_clipboard(cx, saved);
        return;
    }

    // ⌘C: the selection lands on the real pasteboard.
    tap(cx, pid, KC_C, platform::FLAG_COMMAND).await;
    let copied = cx.update(|app| app.read_from_clipboard().and_then(|item| item.text()));
    if copied.as_deref() != Some(NAME) {
        failures.push(format!(
            "rename clipboard (⌘C): the system clipboard must hold the copied field text \
             {NAME:?}, got {copied:?}"
        ));
    }

    // ⌘V over the still-selected field: replacing a selection with its own text
    // is a no-op only if the paste is exact (a dropped or doubled character
    // shows up immediately), and it parks the caret at the end.
    tap(cx, pid, KC_V, platform::FLAG_COMMAND).await;
    let pasted = fb.update(cx, |v, _| v.scenario_rename_text());
    let pasted_sel = fb.update(cx, |v, _| v.scenario_rename_selection());
    if pasted.as_deref() != Some(NAME) || pasted_sel != Some((end, end)) {
        failures.push(format!(
            "rename clipboard (⌘V over the selection): expected the field text {NAME:?} with a \
             collapsed caret at its end, got text {pasted:?} selection {pasted_sel:?}"
        ));
    }

    // A bare `c` types at that caret — the paste left an ordinary editing state
    // behind, not a stuck selection.
    tap(cx, pid, KC_C, 0).await;
    let typed = format!("{NAME}c");
    let after_type = fb.update(cx, |v, _| v.scenario_rename_text());
    if after_type.as_deref() != Some(typed.as_str()) {
        failures.push(format!(
            "rename clipboard (type after paste): expected {typed:?}, got {after_type:?}"
        ));
    }

    // A multi-line clipboard seeded by the driver pastes as ONE line at the
    // caret — the sanitizer, end to end through the real chord.
    cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string(MULTILINE.to_string())));
    tap(cx, pid, KC_V, platform::FLAG_COMMAND).await;
    let want = format!("{typed}{SANITIZED}");
    let after_multi = fb.update(cx, |v, _| v.scenario_rename_text());
    if after_multi.as_deref() != Some(want.as_str()) {
        failures.push(format!(
            "rename clipboard (⌘V of {MULTILINE:?}): a multi-line paste must flatten to one line \
             at the caret — expected {want:?}, got {after_multi:?}"
        ));
    }

    cancel_rename(cx, whandle, fb).await;
    if !exists(&path) {
        failures.push(format!(
            "rename clipboard: Esc must leave '{NAME}' untouched on disk"
        ));
    }
    restore_clipboard(cx, saved);
    if failures.len() == start {
        eprintln!(
            "[selftest] file-browser (d-clip): real ⌘A/⌘C/⌘V CGEvents round-tripped the open \
             rename field through the system clipboard, a multi-line paste flattened to one line, \
             and Esc left the file untouched"
        );
    }
}

/// Put the system clipboard back the way [`rename_clipboard_leg`] found it. An
/// empty `saved` means the clipboard held no text to begin with, in which case
/// the leg's own copy is left in place rather than fabricating an empty item —
/// there is no "clear it" in the gpui API, and an empty string is not the same
/// thing as an empty pasteboard.
fn restore_clipboard(cx: &mut AsyncApp, saved: Option<String>) {
    if let Some(text) = saved {
        cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string(text)));
    }
}

// ---- (d-drag) press-and-drag selection in the rename field -----------------

/// Bug B's live gate: with a rename open, drag from char boundary 2 to boundary
/// 6 and assert the field selected `(2, 6)` — then type one char and assert it
/// REPLACED that range.
///
/// The gesture is attempted for real first (guarded global-HID DOWN → DRAG steps
/// → UP, behind the mandatory activate + raise + frontmost-at-point preflight —
/// never a blind post; pid-posted mouse events silently drop, hence the R27
/// carve-out seams). macOS does not establish the implicit mouse grab for a
/// synthetic press, so `mouseDragged:` delivery is not guaranteed (the
/// `tranche6-composition` reorder leg's recorded finding); when the moves do not
/// arrive the real half DEFERS LOUDLY to a human pass and the leg still HARD
/// asserts the same gesture through the production hit-test + `extend_to` path
/// over the very geometry the field PAINTED. Nothing is weakened by a deferral:
/// the assertions below run either way.
async fn rename_drag_select_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    const NAME: &str = "dragselect.txt";
    const FROM: usize = 2;
    const TO: usize = 6;
    let path = fixture.path(NAME);
    let start = failures.len();

    rekey(cx, whandle).await;
    begin_rename(cx, whandle, fb, &path).await;
    // Wait for THIS field's paint to record the boundary table + field box. The
    // wait is pinned to the name, so a table left by an earlier rename (the probe
    // cell is per-view; begin resets it, but a leg must still not race the paint)
    // can never stand in for it.
    let painted = await_rename_paint(cx, fb, NAME.chars().count()).await;
    let opened = fb.update(cx, |v, _| v.scenario_rename_text());
    if opened.as_deref() != Some(NAME) {
        failures.push(format!(
            "rename drag-select: the rename never opened on '{NAME}' (field text {opened:?})"
        ));
        // A wrong-row edit may be open; close it so this failure can't cascade
        // into the later legs (matches the other early-outs in this function).
        cancel_rename(cx, whandle, fb).await;
        return;
    }

    // Window coordinates of the two boundaries, from the field's OWN last paint:
    // the text run's left edge plus the painted boundary offset, nudged 1px right
    // so the half-glyph rounding lands on the boundary itself (the (d′) leg's
    // convention). The y is the painted field box's vertical centre.
    let (_field_left, text_left) = fb.update(cx, |v, _| v.scenario_rename_probe());
    let geom = fb.update(cx, |v, _| {
        (
            v.scenario_rename_x_for_index(FROM),
            v.scenario_rename_x_for_index(TO),
            v.scenario_rename_field_center_y(),
        )
    });
    let (Some(b_from), Some(b_to), Some(y)) = geom else {
        failures.push(format!(
            "rename drag-select: the open field never painted its boundary table / box \
             (boundaries {:?}/{:?}, box centre {:?}) — nothing to aim at",
            geom.0, geom.1, geom.2
        ));
        cancel_rename(cx, whandle, fb).await;
        return;
    };
    // The wait above is what pins the table to THIS name (one boundary per char
    // plus the trailing one, no more); re-read it here so the failure message
    // names the numbers the clicks would otherwise have been measured against.
    let n = NAME.chars().count();
    let (last, past_end) = fb.update(cx, |v, _| {
        (
            v.scenario_rename_x_for_index(n),
            v.scenario_rename_x_for_index(n + 1),
        )
    });
    if !painted || last.is_none() || past_end.is_some() {
        failures.push(format!(
            "rename drag-select: the painted boundary table does not describe '{NAME}' ({n} chars) \
             — boundary {n} is {last:?} and boundary {} is {past_end:?} (expected Some / None), so \
             the field's last paint was a DIFFERENT field and every click target below would be \
             measured against stale geometry",
            n + 1
        ));
        cancel_rename(cx, whandle, fb).await;
        return;
    }
    let x_from = text_left + b_from + 1.0;
    let x_to = text_left + b_to + 1.0;

    // --- the real gesture (guarded; DEFERS rather than failing) ---------------
    match real_drag_gesture(cx, whandle, fb, x_from, x_to, y).await {
        Ok((pressed, dragged)) if pressed == Some((FROM, FROM)) && dragged == Some((FROM, TO)) => {
            eprintln!(
                "[selftest] file-browser (d-drag): a real guarded-HID press-drag selected \
                 [{FROM},{TO}) in the rename field"
            );
        }
        Ok((pressed, dragged)) if pressed == Some((FROM, FROM)) => {
            deferred.push(format!(
                "rename drag-select: the real guarded-HID press LANDED (caret at boundary {FROM}), \
                 but the held mouse-moves did not extend the selection (got {dragged:?}, want \
                 {:?}) — macOS does not establish the implicit mouse grab for a synthetic press, \
                 so `mouseDragged:` is not delivered (the tranche6 reorder leg's recorded finding). \
                 DEFERRED to a human drag; the same gesture is HARD-asserted below through the \
                 production hit-test over the painted geometry.",
                Some((FROM, TO))
            ));
        }
        Ok((pressed, _)) => {
            deferred.push(format!(
                "rename drag-select: the real guarded-HID press at window x {x_from:.1}, y {y:.1} \
                 did not place the caret at boundary {FROM} (selection {pressed:?}) — the \
                 synthetic press did not land on the field. DEFERRED to a human drag; the gesture \
                 is HARD-asserted below through the production hit-test."
            ));
        }
        Err(reason) => deferred.push(format!("rename drag-select: {reason}")),
    }

    // --- the hard assertion: press → move → selection, through production -----
    let pressed = whandle
        .update(cx, |_r, window, app| {
            fb.update(app, |v, cx| {
                v.drive_rename_click_at_window_x(x_from, window, cx);
                v.scenario_rename_selection()
            })
        })
        .unwrap_or(None);
    let dragged = fb.update(cx, |v, cx| {
        v.drive_rename_drag_to_window_x(x_to, cx);
        v.scenario_rename_selection()
    });
    if pressed != Some((FROM, FROM)) || dragged != Some((FROM, TO)) {
        failures.push(format!(
            "rename drag-select: a press at the painted x of boundary {FROM} then a move to \
             boundary {TO} must select [{FROM},{TO}) — got press {pressed:?}, drag {dragged:?}"
        ));
    }
    // Visual spot-check material (Validation step 4), taken BEFORE the typing
    // below collapses it: the selection highlight over [FROM,TO) of the live
    // field.
    capture_rename_stage(cx, whandle, "rename-selection", failures).await;
    // Typing replaces the dragged range ("dragselect.txt" minus [2,6) = "agse").
    fb.update(cx, |v, cx| v.drive_rename_type('z', cx));
    settle(cx, 60).await;
    let text = fb.update(cx, |v, _| v.scenario_rename_text());
    if text.as_deref() != Some("drzlect.txt") {
        failures.push(format!(
            "rename drag-select: typing over the dragged selection should yield 'drzlect.txt'; \
             got {text:?}"
        ));
    }

    cancel_rename(cx, whandle, fb).await;
    if !exists(&path) {
        failures.push(format!(
            "rename drag-select: Esc must leave '{NAME}' untouched on disk"
        ));
    }
    if failures.len() == start {
        eprintln!(
            "[selftest] file-browser (d-drag): press at boundary {FROM} + move to boundary {TO} \
             selected the range over the PAINTED geometry, and typing replaced it"
        );
    }
}

/// Post a REAL left press-drag-release across the open rename field, behind the
/// mandatory guarded-HID preflight (activate + raise + `CGWindowListCopyWindowInfo`
/// frontmost-at-point). Returns the field's selection right after the press and
/// right before the release, or `Err(reason)` when the preflight refused (NO post
/// was made). The release is always posted once the press is — the button must
/// never be left held.
async fn real_drag_gesture(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    x_from: f32,
    x_to: f32,
    y: f32,
) -> Result<(Option<(usize, usize)>, Option<(usize, usize)>), String> {
    let (gx0, gy) = guarded_global_point(cx, whandle, x_from, y, "the field's press point").await?;
    let Some((gx1, _)) = content_to_global(cx, whandle, x_to, y) else {
        return Err(
            "could not convert the field's release point to CG-global coords — DEFERRED, \
             no global post was made"
                .to_string(),
        );
    };

    platform::post_global_left_down(gx0, gy);
    settle(cx, 150).await;
    let pressed = fb.update(cx, |v, _| v.scenario_rename_selection());
    let steps = 8;
    for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        platform::post_global_left_drag(gx0 + (gx1 - gx0) * t, gy);
        settle(cx, 40).await;
    }
    settle(cx, 100).await;
    let dragged = fb.update(cx, |v, _| v.scenario_rename_selection());
    platform::post_global_left_up(gx1, gy);
    settle(cx, 150).await;
    Ok((pressed, dragged))
}

/// The MANDATORY preflight before any guarded global-HID post: activate the app +
/// raise the window, convert the content point `(x, y)` to CG-global, then verify
/// our window owns that point per `CGWindowListCopyWindowInfo`. `Ok` carries the
/// coordinates to post at; `Err(reason)` means the caller must DEFER LOUDLY — NO
/// post was made. `what` names the point in that reason.
async fn guarded_global_point(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    x: f32,
    y: f32,
    what: &str,
) -> Result<(f64, f64), String> {
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;
    let Some((gx, gy)) = content_to_global(cx, whandle, x, y) else {
        return Err(format!(
            "could not convert {what} to CG-global coords — DEFERRED, no global post was made"
        ));
    };
    if !platform::frontmost_window_owns_point(gx, gy) {
        return Err(format!(
            "the frontmost-at-point preflight FAILED — our window does not own {what} \
             ({gx:.0},{gy:.0}) per CGWindowListCopyWindowInfo (another window is on top, or the \
             point is off our window). DEFERRED LOUDLY; NO global post was made. Bring the nice \
             window frontmost and re-run for the real-gesture assertion."
        ));
    }
    Ok((gx, gy))
}

/// Convert a window content point to CG-global coordinates (no preflight — the
/// caller fences the gesture at its press point).
fn content_to_global(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    x: f32,
    y: f32,
) -> Option<(f64, f64)> {
    whandle
        .update(cx, |_v, w, _a| {
            platform::content_point_to_cg_global(w, x as f64, y as f64)
        })
        .ok()
        .flatten()
}

// ---- (e′) real-event press / drag inside a multi-selection ------------------

/// Plan-2's live gate. With TWO files selected, a REAL left press on one of them
/// must NOT collapse the selection — the bug was that the collapse happened at
/// mouse-DOWN, so by the time gpui armed the drag the pressed row's payload was a
/// single path — and a REAL release in place (no movement) must then collapse it
/// to that row alone (Finder's select-then-drag).
///
/// How the 2-selection is BUILT is immaterial to the bug (which is a plain,
/// unmodified press on an already-multi-selected row), so it is built through the
/// scenario drive seams: the sanctioned global-HID mouse seams are modifier-less
/// by construction (they stamp only the click-state field; nothing in the platform
/// layer puts ⌘/⇧ on a global mouse post), and a keyboard synthetic cannot supply
/// flags to a separately-posted mouse event. The real events are spent where the
/// bug lives — the press and the release.
///
/// The full drag (press → held moves onto a folder row → release) is ATTEMPTED and
/// DEFERS LOUDLY when it does not arm: macOS does not establish AppKit's implicit
/// mouse grab for a synthetic press, so the trailing moves are never delivered as
/// `mouseDragged:` and gpui's `on_drag` / `on_drop` never arm (established twice
/// in-tree — the `tranche6-composition` reorder leg and the (d-drag) leg above). A
/// drag that DOES arm and commits the WRONG outcome hard-FAILS. The non-deferrable
/// gate for "the whole selection travels" is r20 leg (e)'s `drive_drag_drop` seam
/// plus the in-process `view.rs` behaviour tests — never this leg's drag half.
async fn multi_select_press_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fb: &Entity<FileBrowserView>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    const A_NAME: &str = "multi1.txt";
    const B_NAME: &str = "multi2.txt";
    const DIR_NAME: &str = "multidrag";
    let a = fixture.path(A_NAME);
    let b = fixture.path(B_NAME);
    let dir = fixture.path(DIR_NAME);
    let start = failures.len();

    rekey(cx, whandle).await;
    // Collapse `src` (the watcher leg expanded it) so the two rows and the drop
    // target share one viewport — the drag half needs both on screen at once.
    if fb.update(cx, |v, cx| v.scenario_is_expanded(&fixture.path("src"), cx)) {
        fb.update(cx, |v, cx| v.drive_single_click(&fixture.path("src"), cx));
        settle(cx, 400).await;
    }

    let both = vec![a.clone(), b.clone()];
    let select_both = |cx: &mut AsyncApp| {
        fb.update(cx, |v, cx| v.drive_select(&a, cx));
        fb.update(cx, |v, cx| v.drive_add_to_selection(&b, cx));
    };
    let selection = |cx: &mut AsyncApp| fb.update(cx, |v, cx| v.scenario_selected_paths(cx));

    select_both(cx);
    settle(cx, 120).await;
    let selected = selection(cx);
    if selected != both {
        failures.push(format!(
            "(e′) setup: the two fixture rows did not become the selection (got {selected:?}, \
             want {both:?})"
        ));
        return;
    }

    // --- press / release on an already-selected row, for REAL -----------------
    let Some((ax, ay)) = row_center(cx, fb, &a).await else {
        failures.push(format!(
            "(e′) the tree never painted a row box for {A_NAME} inside the list viewport — there \
             is no drawn geometry to aim a real press at"
        ));
        return;
    };
    match guarded_global_point(cx, whandle, ax, ay, &format!("the {A_NAME} row press point")).await {
        Err(reason) => deferred.push(format!("(e′) real press on a selected row: {reason}")),
        Ok((gx, gy)) => {
            platform::post_global_left_down(gx, gy);
            settle(cx, 250).await;
            let after_press = selection(cx);
            platform::post_global_left_up(gx, gy);
            settle(cx, 300).await;
            let after_release = selection(cx);
            if after_release != vec![a.clone()] {
                // The release is also the landed-gate: a gesture that never reached
                // the row leaves the selection untouched, and that must not read as
                // a green press assertion.
                failures.push(format!(
                    "(e′) a real press+release in place on the already-selected row {A_NAME} must \
                     collapse the selection to it alone at the RELEASE; selection after the \
                     release is {after_release:?} (aimed at window ({ax:.0},{ay:.0}) / CG-global \
                     ({gx:.0},{gy:.0}); if the selection is still both files, the synthetic \
                     press/release never reached the row at all)"
                ));
            } else if after_press != both {
                failures.push(format!(
                    "(e′) the real left-press on the already-selected row {A_NAME} COLLAPSED the \
                     multi-selection at MOUSE-DOWN (selection right after the press: \
                     {after_press:?}, want {both:?}) — exactly the regression the deferred-collapse \
                     fix exists for: the row's drag payload would be a single path by the time \
                     gpui armed the drag"
                ));
            } else {
                eprintln!(
                    "[selftest] file-browser (e′): a real press on a row inside a 2-selection kept \
                     the whole selection, and the real release in place collapsed it to that row"
                );
            }
        }
    }

    // --- the full real drag onto a folder row (attempt; defers if it never arms) -
    select_both(cx);
    // Beyond the router's 280 ms double-click window, so the drag's press reads as
    // a fresh first click and not a double-click on the row above.
    settle(cx, 400).await;
    if !fb.update(cx, |v, cx| v.scenario_can_drop(&a, &dir, cx))
        || fb.update(cx, |v, cx| v.scenario_can_drop(&a, &a, cx))
    {
        failures.push(format!(
            "(e′) drag highlight: can_drop must accept the folder target {DIR_NAME} and reject a \
             self-drop (this is the predicate that paints the accent hover highlight)"
        ));
    }
    let from = fb.update(cx, |v, _| v.scenario_row_center(&a));
    let to = fb.update(cx, |v, _| v.scenario_row_center(&dir));
    let (Some((sx, sy)), Some((tx, ty))) = (from, to) else {
        deferred.push(format!(
            "(e′) real multi-selection drag: {A_NAME} and the folder {DIR_NAME} are not \
             simultaneously inside the painted list viewport (row boxes {from:?} / {to:?}), so \
             there is no drawn geometry to drag between. DEFERRED; the multi-path move stays \
             pinned by the `drive_drag_drop` seam leg and the in-process tests."
        ));
        return;
    };
    match guarded_global_point(cx, whandle, sx, sy, "the multi-selection drag's press point").await {
        Err(reason) => deferred.push(format!("(e′) real multi-selection drag: {reason}")),
        Ok((gx0, gy0)) => {
            let Some((gx1, gy1)) = content_to_global(cx, whandle, tx, ty) else {
                deferred.push(
                    "(e′) real multi-selection drag: could not convert the folder row's drop point \
                     to CG-global coords — DEFERRED, NO global post was made"
                        .to_string(),
                );
                return;
            };
            let arms_before = fb.update(cx, |v, _| v.scenario_drag_arm_count());
            platform::post_global_left_down(gx0, gy0);
            settle(cx, 150).await;
            let steps = 10;
            for i in 1..=steps {
                let t = f64::from(i) / f64::from(steps);
                platform::post_global_left_drag(gx0 + (gx1 - gx0) * t, gy0 + (gy1 - gy0) * t);
                settle(cx, 40).await;
            }
            settle(cx, 120).await;
            platform::post_global_left_up(gx1, gy1);
            settle(cx, 400).await;

            let moved_a = exists(&fixture.path("multidrag/multi1.txt"));
            let moved_b = exists(&fixture.path("multidrag/multi2.txt"));
            let src_a = exists(&a);
            let src_b = exists(&b);
            // Four readings: the drag armed and moved the whole selection (pass);
            // nothing moved AND the drag never armed (defer — the platform
            // limitation); nothing moved but the drag DID arm (hard fail — the
            // armed OS session swallowed the gesture and the in-app drop was
            // lost, exactly the regression shape the drag-out change could
            // introduce); or something else committed (hard fail — a wrong
            // outcome is never a deferral, e.g. only the pressed row travelled).
            let armed = fb.update(cx, |v, _| v.scenario_drag_arm_count()) > arms_before;
            let both_moved = moved_a && !src_a && moved_b && !src_b;
            let nothing_happened = !moved_a && !moved_b && src_a && src_b;
            if nothing_happened && armed {
                failures.push(
                    "(e′) the drag ARMED (on_drag ran — the OS drag session started) but nothing \
                     moved and both sources are still in place: the drop back onto our own window \
                     was lost. This is NOT the never-armed platform limitation — suspect the \
                     NSDraggingSession in-app destination path (enter/exit/perform plumbing)."
                        .to_string(),
                );
                return;
            }
            match (both_moved, nothing_happened) {
                (true, _) => {
                    // The model must agree with the disk: both source rows are gone
                    // from the tree's projection.
                    let mut gone = false;
                    for _ in 0..20 {
                        settle(cx, 100).await;
                        let rows = fb.update(cx, |v, _| v.scenario_rendered_paths());
                        gone = !rows.iter().any(|p| p == &a) && !rows.iter().any(|p| p == &b);
                        if gone {
                            break;
                        }
                    }
                    if gone {
                        eprintln!(
                            "[selftest] file-browser (e′): a REAL end-to-end drag armed and moved \
                             BOTH selected files into {DIR_NAME} (disk + model)"
                        );
                    } else {
                        failures.push(format!(
                            "(e′) the real drag moved both files on disk but the tree still renders \
                             {A_NAME}/{B_NAME} at the old location"
                        ));
                    }
                }
                (_, true) => deferred.push(
                    "(e′) the real end-to-end drag did not arm (corroborated: the drag-arm counter \
                     never moved): nothing moved and both sources are still in place. macOS does \
                     not establish AppKit's implicit mouse grab for a synthetic press, so the held \
                     moves are never delivered as `mouseDragged:` and gpui's on_drag/on_drop never \
                     arm (the recorded in-tree finding). DEFERRED to a human drag of a 2-file \
                     selection onto a folder; the multi-path move itself stays hard-pinned by r20 \
                     leg (e)'s `drive_drag_drop` seam and the in-process view tests."
                        .to_string(),
                ),
                _ => failures.push(format!(
                    "(e′) the real drag COMMITTED A WRONG OUTCOME: multi1 in {DIR_NAME}={moved_a} \
                     (source left={src_a}), multi2 in {DIR_NAME}={moved_b} (source left={src_b}) — \
                     a drag that arms must carry the WHOLE selection, so a single-file move means \
                     the multi-selection was collapsed before the drag armed"
                )),
            }
        }
    }

    if failures.len() == start {
        eprintln!("[selftest] file-browser (e′): the real-event multi-selection press leg held");
    }
}

/// The window-coordinate centre of `path`'s PAINTED row, scrolling it into view
/// once if the tree has not drawn it inside the list viewport. `None` when the row
/// never lands in the viewport within the poll budget — a row that was not drawn is
/// never aimed at.
async fn row_center(
    cx: &mut AsyncApp,
    fb: &Entity<FileBrowserView>,
    path: &str,
) -> Option<(f32, f32)> {
    if let Some(c) = fb.update(cx, |v, _| v.scenario_row_center(path)) {
        return Some(c);
    }
    if !fb.update(cx, |v, cx| v.drive_scroll_row_into_view(path, cx)) {
        return None; // not in the projection at all
    }
    for _ in 0..20 {
        settle(cx, 60).await;
        if let Some(c) = fb.update(cx, |v, _| v.scenario_row_center(path)) {
            return Some(c);
        }
    }
    None
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

fn build_report(failures: Vec<String>, deferred: Vec<String>) -> CadenceReport {
    if !deferred.is_empty() {
        eprintln!("[selftest] file-browser DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "file-browser OK (through the shipped builder): ⌘⇧B swapped in the tree \
                 (AX root exposed + fixture row rendered), single-click expand/collapse, \
                 double-click re-root, double-click file recorded one open (nothing launched), \
                 right-click menus (file vs folder) + two-stage Open With default-first, the \
                 live watcher surfaced a created row, sort-direction + hidden toggle + ⌘⇧. \
                 worked, ⌘⇧B still flips modes, the R20 legs (copy/paste, cut-ghost-move, \
                 trash+⌘Z, rename, drag, drift) passed, the rename field's real ⌥/⌘ editing \
                 chords walked the word table and a press-drag selected a range, the §6 \
                 composition leg (⌘N second window, CGEvent ⌘Z in B undoing A's op with focus \
                 routed back) held, and a REAL press inside a 2-file selection kept it (the \
                 release collapsed it); {} item(s) DEFERRED to a human pass",
                deferred.len()
            ),
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
