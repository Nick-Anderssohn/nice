//! `persistence-restore` self-test scenario — the R18 session persistence +
//! restore gate (Validation §3).
//!
//! Drives the **shipped window** restore path with a **temp session store**
//! (injected via `NICE_APPLICATION_SUPPORT_ROOT`) seeded with a hand-authored
//! v3-shaped `sessions.json`: a Claude tab with a deliberately stale cwd + a
//! planted fake `~/.claude/projects` bucket/transcript (the cwd-heal target), a
//! terminal `Main` tab, a `parentTabId` pair, a saved frame, `sidebarCollapsed:
//! true`. One `Application::run`; the restore fan-out fns are called EXPLICITLY
//! (the `shell-socket` precedent — no relaunch).
//!
//! The legs:
//! * **(a)** restore round-trip on the shipped window (`open_managed_window_with`
//!   + `build_window_root`): the model tree matches the fixture, lineage intact,
//!   sidebar collapsed, the frame applied (read back via `window_screen_frame`
//!   within tolerance), the cwd-heal corrected the stale Claude cwd, and a bounded
//!   grid-poll shows the pre-typed `claude --resume <sid>` with NOTHING executed;
//! * **(b)** a raw-socket mutation polls the store file for the debounced
//!   coalesced write;
//! * **(c)** the **W5 veto**: with live panes, the REAL close action
//!   (`-[NSWindow performClose:]` — the exact action the red traffic-light button's
//!   target invokes, routed through the window delegate's `windowShouldClose:`
//!   gate, NOT the should-close closure directly; the traffic-light frame helper is
//!   asserted to locate the close button, but a synthetic CGEvent click does not
//!   hit-test to the native button under gpui's full-size-content window — verified
//!   on-device) leaves the window OPEN, the modal shows (AX role+label), Cancel is a
//!   total no-op (file byte-identical), Confirm closes + the slot disappears;
//! * **(d)** re-running the restore fan-out functions against a store yields
//!   exactly the restorable slots (seed id/parts match; ghosts dropped);
//! * **(e)** a unit-level quit-cascade disposition (both snapshots survive + a
//!   close after `AppQuitting` is inert — the wipe regression);
//! * **(f)** migration: a Swift-shaped fixture ⇒ lossless adopt, `branch` ignored,
//!   own file written, source bytes untouched.
//!
//! ## Hermeticity
//!
//! Every path is a per-test temp tree: `NICE_APPLICATION_SUPPORT_ROOT` (the
//! store), `NICE_CLAUDE_PROJECTS_ROOT` (the heal bucket), a sandbox `HOME`, a
//! `ZDOTDIR` fixture, and `NICE_CLAUDE_OVERRIDE` at a stub — the real `claude` and
//! the real `~/Library/Application Support` / `~/.claude` are NEVER touched. The
//! store Global + the scenario `ShellInjectConfig` + every env var are cleared at
//! teardown so the later `multiwindow` scenario runs clean. **Registers the
//! `WindowRegistry` WITHOUT `install`** (its `build_window_root` only `register`s;
//! quit-when-empty would kill the suite), registered BEFORE `multiwindow` (the
//! sole installer, last).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_view::TerminalSessionHandle;

use crate::app_shell::AppShellView;
use crate::cwd_heal::encode_claude_bucket;
use crate::session_store::{self, PersistedFrame, PersistedWindow, SessionStore};
use crate::window_state::WindowState;
use crate::window_registry::WindowRegistry;
use crate::{platform, restore};

const POLL_MS: u64 = 100;
const READY_POLLS: usize = 60;
const SID: &str = "sid-fixture";
const WIN_ID: &str = "win-fixture";
const FIXTURE_FRAME: PersistedFrame = PersistedFrame {
    x: 160.0,
    y: 160.0,
    width: 900.0,
    height: 600.0,
};

// -- fixture -----------------------------------------------------------------

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    support_root: PathBuf,
    projects_root: PathBuf,
    zdotdir: PathBuf,
    work: PathBuf,
    recovered: PathBuf,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-rs-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let support_root = base.join("app-support");
        let projects_root = base.join("claude-projects");
        let zdotdir = base.join("zdotdir");
        let work = base.join("work");
        let recovered = base.join("recovered-worktree");
        for d in [
            &home,
            &support_root,
            &projects_root,
            &zdotdir,
            &work,
            &recovered,
        ] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }

        // The R14 ZDOTDIR stub chain (the `print -z` prefill tail) so a restored
        // deferred-resume Claude pane pre-types NICE_PREFILL_COMMAND.
        crate::shell_inject::write_stubs(&zdotdir).context("write ZDOTDIR stubs")?;

        // The stub `claude`: idle forever (a restored pane never RUNS it — the
        // prefill is only pre-typed). NEVER the machine's real claude.
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin)?;
        let stub = bin.join("claude");
        std::fs::write(&stub, "#!/bin/sh\nwhile IFS= read -r _l; do : ; done\n")?;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
        // SAFETY: single-threaded scenario setup before any pane forks.
        unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };

        // Plant the heal bucket: `<projects_root>/<bucket(recovered)>/sid.jsonl`
        // whose transcript head carries a top-level cwd == the recovered dir. The
        // fixture Claude tab's persisted cwd points at a NON-existent stale path, so
        // the heal must scan, find this bucket, and adopt `recovered`.
        let recovered_str = recovered.to_string_lossy().into_owned();
        let bucket = projects_root.join(encode_claude_bucket(&recovered_str));
        std::fs::create_dir_all(&bucket)?;
        std::fs::write(
            bucket.join(format!("{SID}.jsonl")),
            format!("{{\"type\":\"user\",\"cwd\":\"{recovered_str}\",\"sessionId\":\"{SID}\"}}\n"),
        )?;

        // The hand-authored v3 store file (a parentTabId pair, a Claude tab with a
        // stale cwd, a Main terminal tab, a frame, sidebarCollapsed true).
        let store_dir = support_root.join(session_store::STORE_FOLDER);
        std::fs::create_dir_all(&store_dir)?;
        let home_s = home.to_string_lossy();
        let work_s = work.to_string_lossy();
        let stale = "/tmp/nice-persist-stale-does-not-exist";
        let json = format!(
            r#"{{
  "version": 3,
  "windows": [
    {{
      "id": "{WIN_ID}",
      "activeTabId": "claude-tab",
      "sidebarCollapsed": true,
      "frame": {{ "x": {fx}, "y": {fy}, "width": {fw}, "height": {fh} }},
      "projects": [
        {{
          "id": "terminals",
          "name": "Terminals",
          "path": "{home_s}",
          "tabs": [
            {{ "id": "terminals-main", "title": "Main", "cwd": "{home_s}",
               "nextTerminalIndex": 2,
               "panes": [ {{ "id": "terminals-main-p", "title": "Terminal 1", "kind": "terminal", "cwd": "{home_s}" }} ] }}
          ]
        }},
        {{
          "id": "proj",
          "name": "Proj",
          "path": "{work_s}",
          "tabs": [
            {{ "id": "claude-tab", "title": "Claude", "cwd": "{stale}",
               "claudeSessionId": "{SID}",
               "activePaneId": "claude-tab-claude",
               "panes": [
                 {{ "id": "claude-tab-claude", "title": "Claude", "kind": "claude" }},
                 {{ "id": "claude-tab-t1", "title": "Terminal 1", "kind": "terminal" }}
               ] }},
            {{ "id": "child-tab", "title": "Child", "cwd": "{work_s}",
               "parentTabId": "claude-tab",
               "panes": [ {{ "id": "child-tab-p", "title": "Terminal 1", "kind": "terminal", "cwd": "{work_s}" }} ] }}
          ]
        }}
      ]
    }}
  ]
}}"#,
            fx = FIXTURE_FRAME.x,
            fy = FIXTURE_FRAME.y,
            fw = FIXTURE_FRAME.width,
            fh = FIXTURE_FRAME.height,
        );
        std::fs::write(store_dir.join("sessions.json"), json)?;

        Ok(Fixture {
            base,
            home,
            support_root,
            projects_root,
            zdotdir,
            work: work.clone(),
            recovered,
        })
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the restored `persistence-restore` window through the shipped restore
/// path and spawn its driver. Installs the temp store Global, sets the injectable
/// paths, and opens the fixture's one saved window via `open_managed_window_with`.
pub fn open_persistence_restore_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home.to_string_lossy().into_owned();
    let support = fixture.support_root.to_string_lossy().into_owned();
    let projects = fixture.projects_root.to_string_lossy().into_owned();
    let zdotdir = fixture.zdotdir.to_string_lossy().into_owned();

    let whandle: WindowHandle<AppShellView> = cx.update(|app| -> Result<_> {
        crate::keymap::install_shortcuts(app);
        // Injectable paths (resolved by app::run in production; the scenario is the
        // hermetic injection point).
        // SAFETY: single-threaded scenario setup.
        unsafe {
            std::env::set_var("NICE_APPLICATION_SUPPORT_ROOT", &support);
            std::env::set_var("NICE_CLAUDE_PROJECTS_ROOT", &projects);
        }
        // The restored deferred-resume Claude pane forks with the synthetic ZDOTDIR
        // rc chain so its `print -z` tail pre-types NICE_PREFILL_COMMAND.
        crate::app::set_scenario_shell_inject_config(app, Some(zdotdir.clone()), None);

        // Install the temp store Global (own path from the injected support root; no
        // Swift migration source in this leg).
        let own = session_store::default_store_path();
        session_store::install_global(SessionStore::open(own, None));

        // Hydrate the one saved window + open it through the SHIPPED restore path.
        let saved = session_store::load();
        let seed = restore::hydrate_seed(&saved.windows[0]);
        let root = PathBuf::from(&projects);

        let prev = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", &home) };
        let opened = crate::app::open_managed_window_with(app, Some(seed), Some(root));
        match prev {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        Ok(opened?)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_persistence_restore(acx, whandle, fixture).await;
        // Clean up so the later `multiwindow` scenario runs with no store, no
        // scenario shell-inject config, and no leaked env.
        let _ = acx.update(|app| {
            session_store::clear_global();
            crate::app::set_scenario_shell_inject_config(app, None, None);
        });
        // SAFETY: teardown, single-threaded.
        unsafe {
            std::env::remove_var("NICE_APPLICATION_SUPPORT_ROOT");
            std::env::remove_var("NICE_CLAUDE_PROJECTS_ROOT");
            std::env::remove_var("NICE_CLAUDE_OVERRIDE");
        }
        eprintln!("[selftest] scenario 'persistence-restore': {}", report.detail);
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

// -- driver ------------------------------------------------------------------

async fn run_persistence_restore(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    let any = AnyWindowHandle::from(whandle);
    let id = any.window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "persistence-restore: the shipped builder did not register the restored window",
        );
    };

    // Install a SCOPED window-close observer that routes the real disk fate + reaps
    // the window's ptys WITHOUT the quit-when-empty (this scenario registers the
    // registry WITHOUT `install`, so leg (c)'s Confirm close must still remove the
    // slot + reap the shells, but must NOT quit the suite). Held for the driver's
    // lifetime; dropped when the driver returns, so it never touches `multiwindow`.
    let _close_sub = cx.update(|app| {
        app.on_window_closed(|cx, closed_id| WindowRegistry::route_close_disk_fate(cx, closed_id))
    });

    let mut failures: Vec<String> = Vec::new();

    // === (a) restore round-trip ============================================
    // Model tree: the saved grouping is trusted (proj + Terminals), lineage intact.
    let tab_ids = state.update(cx, |s, _| {
        s.model
            .projects
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect::<Vec<_>>()
    });
    for expect in ["terminals-main", "claude-tab", "child-tab"] {
        if !tab_ids.iter().any(|t| t == expect) {
            failures.push(format!("(a) restored tree is missing tab '{expect}'"));
        }
    }
    // Lineage: child-tab's parent survived → link intact.
    let child_parent =
        state.update(cx, |s, _| s.model.tab_for("child-tab").and_then(|t| t.parent_tab_id.clone()));
    if child_parent.as_deref() != Some("claude-tab") {
        failures.push(format!(
            "(a) lineage lost: child-tab parent = {child_parent:?}, expected Some(\"claude-tab\")"
        ));
    }
    // Sidebar collapsed (restored FROM THE STORE), leading column width 0.
    if !state.update(cx, |s, _| s.sidebar.collapsed()) {
        failures.push("(a) restored sidebar is not collapsed (sidebarCollapsed: true)".into());
    }
    // Frame applied: the read-back Cocoa frame matches the fixture within tolerance
    // (width/height preserved; origin may shift a little for the menu bar / Dock).
    settle(cx, 300).await;
    match whandle.update(cx, |_v, w, _a| platform::window_screen_frame(w)).ok().flatten() {
        Some([_x, _y, w, h]) => {
            if (w - FIXTURE_FRAME.width).abs() > 6.0 || (h - FIXTURE_FRAME.height).abs() > 6.0 {
                failures.push(format!(
                    "(a) restored frame size {}x{} != fixture {}x{}",
                    w, h, FIXTURE_FRAME.width, FIXTURE_FRAME.height
                ));
            }
        }
        None => failures.push("(a) window_screen_frame returned None (no AppKit handle)".into()),
    }
    // cwd-heal corrected the stale Claude cwd to the recovered worktree path.
    let recovered = fixture.recovered.to_string_lossy().into_owned();
    let healed_cwd = state.update(cx, |s, _| s.model.tab_for("claude-tab").map(|t| t.cwd.clone()));
    if healed_cwd.as_deref() != Some(recovered.as_str()) {
        failures.push(format!(
            "(a) cwd-heal did not adopt the recovered cwd: claude-tab.cwd = {healed_cwd:?}, \
             expected {recovered:?}"
        ));
    }
    // The deferred-resume Claude pane lazy-spawns on activation and pre-types
    // `claude --resume <sid>` with NOTHING executed (the stub never runs).
    let mut spawned = None;
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        if let Some(h) =
            state.update(cx, |s, _| s.session.pane_handle("claude-tab", "claude-tab-claude"))
        {
            spawned = Some(h);
            break;
        }
    }
    match spawned {
        Some(h) => {
            let needle = format!("claude --resume {SID}");
            if !poll_grid_contains(cx, &h, &needle).await {
                failures.push(format!(
                    "(a) the restored Claude pane never pre-typed '{needle}' (deferred-resume \
                     prefill / ZDOTDIR chain did not land)"
                ));
            }
        }
        None => failures
            .push("(a) the restored Claude active pane never lazy-spawned its deferred shell".into()),
    }

    // === (b) socket mutation → debounced store write ========================
    if let Some(socket_path) = state.update(cx, |s, _| s.control_socket_path()) {
        let own = cx.update(|_| session_store::default_store_path());
        let tabs_before = store_file_tab_count(&own);
        let work = fixture.work.to_string_lossy().into_owned();
        // A socket `claude` newtab with an empty tabId opens a new tab (a model
        // mutation) — which fires the post-gate save trigger.
        let _ = send_claude_newtab(cx, &socket_path, &work).await;
        let mut wrote = false;
        for _ in 0..READY_POLLS {
            settle(cx, POLL_MS).await;
            if store_file_tab_count(&own) > tabs_before {
                wrote = true;
                break;
            }
        }
        if !wrote {
            failures.push(
                "(b) a socket mutation did not produce a debounced store write (tab count on disk \
                 never grew)"
                    .into(),
            );
        }
    } else {
        failures.push("(b) the restored window armed no control socket to drive".into());
    }

    // === (c) W5 veto via the REAL close button ==============================
    if !cx.update(|_| platform::accessibility_trusted()) {
        failures.push(
            "(c) Accessibility not trusted — the real-close-button CGEvent cannot be posted; grant \
             nice-rs Accessibility and re-run"
                .into(),
        );
    } else if let Err(e) = veto_leg(cx, whandle, &state, &mut failures).await {
        failures.push(format!("(c) veto leg error: {e}"));
    }

    // === (d) re-run the fan-out functions against a fresh store =============
    fan_out_selection_leg(cx, &fixture, &mut failures);

    // === (e) quit-cascade disposition (the wipe regression) =================
    quit_cascade_leg(&mut failures);

    // === (f) Swift migration ================================================
    if let Err(e) = migration_leg(&fixture) {
        failures.push(format!("(f) migration leg error: {e}"));
    }

    build_report(failures)
}

// -- leg (c): the W5 veto via the real close button --------------------------

async fn veto_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    failures: &mut Vec<String>,
) -> Result<()> {
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;
    let pid = std::process::id() as i32;
    let win_id = AnyWindowHandle::from(whandle).window_id();

    // Confirm the traffic-light frame helper locates the REAL close button (the
    // coordinate the plan's CGEvent path targets) — its center is a live, on-screen
    // point.
    let frames = whandle
        .update(cx, |_v, w, _a| platform::standard_window_button_frames(w))
        .ok()
        .flatten()
        .context("standard_window_button_frames returned None")?;
    if frames[0].width <= 0.0 || frames[0].height <= 0.0 {
        failures.push("(c) the close button (index 0) has a degenerate frame".into());
    }

    // Drive the REAL close action via `-[NSWindow performClose:]` — the exact
    // action the red traffic-light button's target invokes. It routes through the
    // window delegate's `windowShouldClose:` (our `on_window_should_close` gate),
    // NOT by invoking the should-close closure directly. (A synthetic CGEvent click
    // on the native button does not hit-test to it under gpui's full-size-content
    // window — verified on-device — so `performClose:` is the real close-button
    // action through the same delegate path.)
    // Capture the content-view pointer, then drive the close from the task with NO
    // outstanding gpui borrow (performClose: re-enters gpui synchronously to
    // present the veto modal — calling it inside `update` would panic).
    let view_ptr = whandle.update(cx, |_v, w, _a| platform::ns_view_of(w)).unwrap_or(std::ptr::null_mut());
    platform::perform_window_close_ptr(view_ptr);
    settle(cx, 400).await;

    // The window stays OPEN (the veto) and the modal is presented.
    if cx.update(|app| WindowRegistry::state_for_window(app, win_id)).is_none() {
        failures.push("(c) the close action closed the window with live panes (no veto)".into());
        return Ok(());
    }
    if !state.update(cx, |s, _| s.pending_modal().is_some()) {
        failures.push("(c) the close action did not present the confirmation modal".into());
    }
    // The modal's confirm button is exposed to the AX tree (role + label).
    // AccessKit builds its tree lazily one frame AFTER the first AX query, so poll
    // — forcing a repaint each tick (the modal doesn't RAF) so the node
    // materializes and stays current (the `app-shell` AX-anchor pattern).
    let mut ax = Err("never queried".to_string());
    for _ in 0..READY_POLLS {
        let _ = state.update(cx, |_s, c| c.notify());
        settle(cx, POLL_MS).await;
        ax = platform::ax_find_titled_role(pid, crate::confirmation_modal::CONFIRM_ACCEPT_ID);
        if matches!(&ax, Ok(role) if role == "AXButton") {
            break;
        }
    }
    match ax {
        Ok(role) if role == "AXButton" => {}
        Ok(role) => failures.push(format!("(c) modal confirm button role = '{role}', want AXButton")),
        Err(e) => failures.push(format!("(c) modal confirm button not in the AX tree: {e}")),
    }

    // Cancel is a total no-op: the store file is byte-identical across it.
    let own = cx.update(|_| session_store::default_store_path());
    session_store::flush();
    let before = std::fs::read(&own).unwrap_or_default();
    resolve_modal(cx, whandle, state, false);
    settle(cx, 300).await;
    if cx.update(|app| WindowRegistry::state_for_window(app, win_id)).is_none() {
        failures.push("(c) Cancel closed the window (must be a total no-op)".into());
    }
    session_store::flush();
    let after = std::fs::read(&own).unwrap_or_default();
    if before != after {
        failures.push("(c) Cancel changed the store file (must be byte-identical)".into());
    }

    // Re-open the modal via a second real close action, then Confirm → the window
    // closes and its slot disappears from the store.
    // Capture the content-view pointer, then drive the close from the task with NO
    // outstanding gpui borrow (performClose: re-enters gpui synchronously to
    // present the veto modal — calling it inside `update` would panic).
    let view_ptr = whandle.update(cx, |_v, w, _a| platform::ns_view_of(w)).unwrap_or(std::ptr::null_mut());
    platform::perform_window_close_ptr(view_ptr);
    settle(cx, 400).await;
    resolve_modal(cx, whandle, state, true);
    settle(cx, 500).await;
    if cx.update(|app| WindowRegistry::state_for_window(app, win_id)).is_some() {
        failures.push("(c) Confirm did not close the window".into());
    }
    session_store::flush();
    let final_state = session_store::read_state(&own);
    if final_state.windows.iter().any(|w| w.id == WIN_ID) {
        failures.push("(c) Confirm did not remove the window slot from the store file".into());
    }
    Ok(())
}

/// Drive the pending modal's Cancel / Confirm answer directly (the plan's
/// hermeticity rule requires only the close button be a real CGEvent).
fn resolve_modal(
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
}

// -- leg (d): fan-out selection over a fresh store ---------------------------

fn fan_out_selection_leg(cx: &mut AsyncApp, fixture: &Fixture, failures: &mut Vec<String>) {
    // A fresh store with one restorable window + one projectless ghost.
    let dir = fixture.base.join("fanout-store");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("sessions.json");
    let good = PersistedWindow {
        id: "keep-me".into(),
        active_tab_id: None,
        sidebar_collapsed: false,
        projects: sample_projects(),
        frame: Some(FIXTURE_FRAME),
    };
    let ghost = PersistedWindow {
        id: "ghost".into(),
        active_tab_id: None,
        sidebar_collapsed: false,
        projects: vec![],
        frame: None,
    };
    let state = session_store::PersistedState {
        version: session_store::CURRENT_VERSION,
        windows: vec![good.clone(), ghost],
    };
    let _ = std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap());

    let loaded = session_store::read_state(&path);
    let restorable: Vec<_> = loaded.windows.iter().filter(|w| restore::is_restorable(w)).collect();
    let _ = cx; // keep the signature uniform with the GUI legs
    if restorable.len() != 1 {
        failures.push(format!(
            "(d) fan-out filter kept {} windows, expected exactly 1 (the ghost must drop)",
            restorable.len()
        ));
        return;
    }
    let seed = restore::hydrate_seed(restorable[0]);
    if seed.window_id != "keep-me" {
        failures.push(format!("(d) surviving seed id = {:?}, expected \"keep-me\"", seed.window_id));
    }
    if seed.frame.as_ref() != Some(&FIXTURE_FRAME) {
        failures.push("(d) surviving seed frame does not match the saved record".into());
    }
    if seed.projects.iter().flat_map(|p| p.tabs.iter()).count() == 0 {
        failures.push("(d) surviving seed hydrated no tabs".into());
    }
}

fn sample_projects() -> Vec<nice_model::PersistedProject> {
    vec![nice_model::PersistedProject {
        id: "proj".into(),
        name: "Proj".into(),
        path: "/work".into(),
        tabs: vec![nice_model::PersistedTab {
            id: "t1".into(),
            title: "A".into(),
            cwd: "/work".into(),
            claude_session_id: None,
            active_pane_id: None,
            panes: vec![],
            title_manually_set: None,
            parent_tab_id: None,
            next_terminal_index: None,
        }],
    }]
}

// -- leg (e): quit-cascade disposition (the wipe regression) -----------------

fn quit_cascade_leg(failures: &mut Vec<String>) {
    use crate::lifecycle::{close_disposition, CloseDisposition};
    // Once quit has begun (AppQuitting), EVERY window close is inert — it PRESERVES
    // the snapshot, never removes it (the production wipe regression: a
    // willClose firing during teardown must not wipe the window's saved tabs).
    let confirmed_user_close = true;
    if close_disposition(true, confirmed_user_close) != CloseDisposition::Preserve {
        failures.push(
            "(e) a close after AppQuitting began removed the slot (the wipe regression) — it must \
             preserve"
                .into(),
        );
    }
    // A default (not user-initiated) close outside quit also preserves.
    if close_disposition(false, false) != CloseDisposition::Preserve {
        failures.push("(e) a non-user close removed the slot — preserve is the safe failure mode".into());
    }
    // Only a confirmed user close outside quit removes.
    if close_disposition(false, true) != CloseDisposition::Remove {
        failures.push("(e) a confirmed user close did not remove the slot".into());
    }
}

// -- leg (f): Swift migration ------------------------------------------------

fn migration_leg(fixture: &Fixture) -> Result<()> {
    // A Swift-shaped fixture: the source `Nice/sessions.json` carries `branch`
    // keys the Rust schema drops. The own store is absent, so opening with the
    // Swift source triggers the one-time migration.
    let root = fixture.base.join("migration");
    let own_dir = root.join(session_store::STORE_FOLDER);
    let swift_dir = root.join(session_store::SWIFT_STORE_FOLDER);
    std::fs::create_dir_all(&own_dir)?;
    std::fs::create_dir_all(&swift_dir)?;
    let own = own_dir.join("sessions.json");
    let swift = swift_dir.join("sessions.json");
    let swift_bytes = br#"{
  "version": 3,
  "windows": [
    { "id": "swift-win", "sidebarCollapsed": false,
      "projects": [
        { "id": "proj", "name": "Proj", "path": "/work", "branch": "main",
          "tabs": [
            { "id": "t1", "title": "A", "cwd": "/work", "branch": "feature",
              "panes": [ { "id": "p1", "title": "Claude", "kind": "claude" } ] }
          ] }
      ] }
  ]
}"#;
    std::fs::write(&swift, swift_bytes)?;

    {
        // Open with migration; drop the store to flush + join before asserting.
        let _store = SessionStore::open(own.clone(), Some(swift.clone()));
    }

    if !own.exists() {
        anyhow::bail!("migration did not write the own store");
    }
    let adopted = session_store::read_state(&own);
    let win = adopted
        .windows
        .iter()
        .find(|w| w.id == "swift-win")
        .context("own store did not adopt the Swift window")?;
    if win.projects.is_empty() || win.projects[0].tabs.is_empty() {
        anyhow::bail!("migration lost the Swift window's projects/tabs");
    }
    // `branch` is ignored (the Rust schema has no such field) — the round-trip is
    // lossless minus branch, and re-serializing carries no branch key.
    let reser = serde_json::to_string(&adopted).unwrap();
    if reser.contains("\"branch\"") {
        anyhow::bail!("the migrated store leaked a `branch` key (M5 drop failed)");
    }
    // The source bytes are untouched (migration only READS the Swift file).
    if std::fs::read(&swift)? != swift_bytes {
        anyhow::bail!("migration mutated the Swift source file (must be read-only)");
    }
    Ok(())
}

// -- shared helpers ----------------------------------------------------------

async fn poll_grid_contains(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    needle: &str,
) -> bool {
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        let grid = handle.update(cx, |h, _cx| h.session().grid_lines().join("\n"));
        if grid.contains(needle) {
            return true;
        }
    }
    false
}

fn store_file_tab_count(path: &Path) -> usize {
    session_store::read_state(path)
        .windows
        .iter()
        .flat_map(|w| w.projects.iter())
        .map(|p| p.tabs.len())
        .sum()
}

async fn send_claude_newtab(cx: &mut AsyncApp, socket_path: &str, cwd: &str) -> Option<String> {
    let payload = format!(
        "{{\"action\":\"claude\",\"cwd\":\"{}\",\"args\":[],\"tabId\":\"\",\"paneId\":\"\"}}",
        cwd.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let rx = raw_request(socket_path.to_string(), payload);
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        match rx.try_recv() {
            Ok(v) => return v.map(|b| String::from_utf8_lossy(&b).into_owned()),
            Err(std::sync::mpsc::TryRecvError::Empty) => continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

fn raw_request(path: String, payload: String) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut result: Option<Vec<u8>> = None;
        while Instant::now() < deadline {
            if let Ok(mut s) = std::os::unix::net::UnixStream::connect(&path) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
                if s.write_all(payload.as_bytes()).is_ok() && s.write_all(b"\n").is_ok() {
                    let mut buf = Vec::new();
                    let _ = s.read_to_end(&mut buf);
                    if buf.contains(&b'\n') {
                        result = Some(buf);
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = tx.send(result);
    });
    rx
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "persistence-restore OK: restore round-trip (tree/lineage/collapse/frame/heal/\
                     prefill), socket-mutation debounced write, W5 veto via the real close button \
                     (modal shown, Cancel no-op, Confirm removed the slot), fan-out selection, \
                     quit-cascade disposition, and Swift migration"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} persistence-restore assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
