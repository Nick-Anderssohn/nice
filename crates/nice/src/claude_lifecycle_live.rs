//! `claude-lifecycle` self-test scenario — the R15 end-to-end Claude gate.
//!
//! Where the ported unit suites pin the pure decision/parse/exec composers and
//! `session-lifecycle` drives the manager headless, this scenario drives the
//! **whole R15 flow** over the **SHIPPED window** (opened through
//! `crate::app::open_managed_window` / `build_window_root`, the exact path
//! `crate::app::run` uses) with a **real control socket**, **real ptys**, and the
//! live `route_terminal_event` subscription lift. It exercises the five legs the
//! plan pins:
//!
//! * **(a) socket newtab + T5 status** — a raw-`UnixStream` `claude` message with
//!   an empty `tabId` replies `newtab`; a fresh Claude tab appears with a minted
//!   v4 session UUID, its Claude pane SPAWNED (the stub runs) and
//!   `is_claude_running == true` FROM CREATION; the stub's braille-prefixed then
//!   ✳-prefixed OSC titles drive the tab's sidebar-dot status Thinking → Waiting
//!   through the SHIPPED window's subscription.
//! * **(b) ≤1-running-Claude refusal** — a second `claude` from that tab's real
//!   pane ids replies `newtab` (Swift's `test_existingClaudeRunning_repliesNewtab`).
//! * **(c) in-place promotion** — a plain terminal pane in a non-Terminals project
//!   promoting on a `claude` message: reply begins `inplace <uuid>` (a valid v4
//!   uuid as field 2, an optional R17 settings 3rd field TOLERATED) and the pane
//!   flips (kind → Claude, `is_claude_running` false→true).
//! * **(d) worktree split** — `claude -w foo` buckets the new tab under the
//!   invocation cwd while its `Tab.cwd` carries `.claude/worktrees/foo`.
//! * **(e) exit routes in the shipped window** — a real `exit` in a live terminal
//!   pane removes it from the SHIPPED window (the subscription-lift proof).
//! * **(f) session_update rotation (R16)** — a raw-`UnixStream` `session_update`
//!   with `source:"resume"` + a new id + a cwd move materializes a sibling parent
//!   tab pinned to the OLD id, `is_claude_running == false`, at ROOT
//!   (`parent_tab_id == None`) with the PRE-rotation cwd, while the originating tab
//!   is re-parented UNDER it (indented) and moves into the post-rotation worktree;
//!   a `source:"clear"` update rotates the id in place with NO new tab; a
//!   cwd-bearing update adopts onto `Tab.cwd`.
//!
//! ## Hermeticity
//!
//! `NICE_CLAUDE_OVERRIDE` points `claude` at a stub script (emits the OSC titles,
//! then idles) — the machine's real `claude` is NEVER spawned. `HOME` is a
//! sandbox for the Main pane's login shell; every Claude pane spawns in a sandbox
//! work dir carried by the socket message. Grid/model polls are bounded and
//! fail-loud (never sleep-and-hope). `Gate::SelfReported`; registered BEFORE
//! `multiwindow` (its `build_window_root` only `register`s — it installs no
//! `WindowRegistry` close observer, so its window never trips the quit-when-empty
//! terminus that `multiwindow` relies on being last).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{Pane, PaneKind, Tab, TabStatus};
use nice_term_view::TerminalSessionHandle;

use crate::app_shell::AppShellView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- timing ------------------------------------------------------------------

/// Poll cap for a routed model mutation (tab creation, status transition, pane
/// removal) to land after its socket / pty event — the drain task + entity
/// subscription hop, on the real clock.
const ROUTE_POLLS: usize = 60;
/// Poll cap for a fixture shell to print its `READY` marker.
const READY_POLLS: usize = 60;
/// Interval between polls (real wall-clock; the pty children run on OS threads).
const POLL_MS: u64 = 100;
/// A fixture shell prints this once it is blocked reading input, so leg (e) polls
/// the grid for readiness rather than sleeping.
const READY_MARKER: &str = "NICERS__CLAUDE__EXIT__READY";

// -- fixture -----------------------------------------------------------------

/// The sandboxed fixture: a fake `$HOME`, a stub `claude` (exported as
/// `NICE_CLAUDE_OVERRIDE`), and two invocation work dirs (one per bucketing leg).
struct Fixture {
    home: PathBuf,
    work_a: PathBuf,
    work_d: PathBuf,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-rs-claude-lifecycle-{}", std::process::id()));
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let work_a = base.join("work-a");
        let work_d = base.join("work-d");
        for d in [&home, &work_a, &work_d] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }

        // The stub `claude`: BURST a braille-prefixed ("thinking") OSC title a few
        // times (so at least one lands AFTER the shipped subscription is established
        // — guarding the socket-spawn → notify → render → subscribe race; a
        // Thinking→Thinking re-report is a no-op), then block on one line of input,
        // then emit a ✳-prefixed ("waiting") OSC title, then idle. NEVER the machine's
        // real claude (hermeticity). `\u{2801}` (⠁) is inside the braille spinner
        // range 0x2800..=0x28FF; `\u{2733}` (✳) is the sparkle.
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).context("create stub bin")?;
        let stub = bin.join("claude");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             n=0\n\
             while [ \"$n\" -lt 15 ]; do\n\
             \x20 printf '\\033]2;\u{2801} build-thing\\007'\n\
             \x20 n=$((n + 1))\n\
             \x20 sleep 0.1\n\
             done\n\
             IFS= read -r _line\n\
             printf '\\033]2;\u{2733} needs-input\\007'\n\
             while IFS= read -r _l; do : ; done\n",
        )
        .context("write stub claude")?;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .context("chmod stub claude")?;

        // Point the spawn path's `resolve_claude_binary` at the stub (process env —
        // re-read every spawn). Overwrite-always so a prior scenario's override is
        // replaced by this emitting stub.
        // SAFETY: single-threaded scenario setup, before any Claude pane forks;
        // matches the existing `std::env::set_var` seams (spawn.rs, selftest).
        unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };

        Ok(Fixture { home, work_a, work_d })
    }

    fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the `claude-lifecycle` window through the SHIPPED builder and spawn its
/// driver (self-reported gate). Sandboxes `HOME` around `open_managed_window` so
/// the Main pane's login shell reads no real user rc, then restores it (every
/// Claude pane spawns in a socket-supplied work dir, not `HOME`).
pub fn open_claude_lifecycle_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home_str();
    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        // The shipped builder reads the process-global `SharedFontSettings` (via the
        // pane host); `install_shortcuts` seeds it. Idempotent — an earlier suite
        // scenario may already have installed it.
        crate::keymap::install_shortcuts(app);
        let prev = std::env::var("HOME").ok();
        // SAFETY: single-threaded setup; restored immediately after the (synchronous)
        // Main-pane spawn inside `open_managed_window`.
        unsafe { std::env::set_var("HOME", &home) };
        let opened = crate::app::open_managed_window(app);
        match prev {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        opened
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_claude_lifecycle(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'claude-lifecycle': {}", report.detail);
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

async fn run_claude_lifecycle(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    // Resolve the shipped window's per-window state (registered by build_window_root).
    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "claude-lifecycle: the shipped builder did not register the window's WindowState",
        );
    };
    // The control socket armed inside `open_managed_window` (path discarded there;
    // read it back here to drive raw-socket `claude` requests).
    let Some(socket_path) = state.update(cx, |s, _cx| s.control_socket_path()) else {
        return CadenceReport::error(
            "claude-lifecycle: the shipped window armed no control socket (no path to drive)",
        );
    };

    let mut failures: Vec<String> = Vec::new();
    let work_a = fixture.work_a.to_string_lossy().into_owned();
    let work_d = fixture.work_d.to_string_lossy().into_owned();

    // === (a) socket newtab + minted uuid + spawned running Claude + T5 status ===
    let tabs_before = all_tab_ids(cx, &state);
    let reply_a = match send_claude(cx, &socket_path, &work_a, &[], "", "").await {
        Some(r) => r,
        None => {
            return CadenceReport::error(
                "claude-lifecycle (a): no reply to the newtab `claude` request over the socket",
            )
        }
    };
    if reply_a.trim_end() != "newtab" {
        failures.push(format!("(a) newtab: expected reply 'newtab', got {reply_a:?}"));
    }
    // The new Claude tab: the one tab id that appeared (it is also now the active tab).
    let claude_tab = match poll_new_tab(cx, &state, &tabs_before).await {
        Some(t) => t,
        None => {
            return CadenceReport::error(
                "claude-lifecycle (a): the newtab reply produced no new tab in the model",
            )
        }
    };
    let (claude_pane, companion_pane) = match tab_pane_ids(cx, &state, &claude_tab) {
        Some(p) => p,
        None => {
            return CadenceReport::error(
                "claude-lifecycle (a): the new Claude tab has no [Claude, Terminal 1] panes",
            )
        }
    };
    // The Claude pane is running FROM CREATION + its pty SPAWNED (the stub runs).
    if !pane_is_claude_running(cx, &state, &claude_tab, &claude_pane) {
        failures.push("(a) the new Claude pane is not is_claude_running from creation".into());
    }
    if !poll_has_pane(cx, &state, &claude_tab, &claude_pane).await {
        failures.push("(a) the new Claude pane never spawned its pty (the stub did not run)".into());
    }
    // A minted, valid v4 session UUID persists on the tab.
    match tab_session_id(cx, &state, &claude_tab) {
        Some(sid) if is_v4_uuid(&sid) => {}
        Some(sid) => failures.push(format!("(a) tab session id {sid:?} is not a valid v4 UUID")),
        None => failures.push("(a) the new Claude tab carries no minted session id".into()),
    }
    // T5: the stub's braille OSC drives the tab's sidebar-dot status → Thinking,
    // then (after a line of input) its ✳ OSC → Waiting — over the SHIPPED window's
    // subscription.
    if !poll_tab_status(cx, &state, &claude_tab, TabStatus::Thinking).await {
        failures.push(
            "(a) T5: the Claude pane's braille OSC title did not drive the tab status to Thinking \
             (the shipped subscription did not route the title)"
                .into(),
        );
    } else {
        // Unblock the stub's `read` so it emits the ✳ (waiting) title.
        write_pane_line(cx, &state, &claude_tab, &claude_pane, b"go\n");
        if !poll_tab_status(cx, &state, &claude_tab, TabStatus::Waiting).await {
            failures.push(
                "(a) T5: the Claude pane's ✳ OSC title did not drive the tab status to Waiting"
                    .into(),
            );
        }
    }

    // === (b) ≤1-running-Claude refusal: a second `claude` from that tab ⇒ newtab ==
    let reply_b = send_claude(cx, &socket_path, &work_a, &[], &claude_tab, &companion_pane).await;
    match reply_b {
        Some(r) if r.trim_end() == "newtab" => {}
        Some(r) => failures.push(format!(
            "(b) refusal: a second `claude` in a running-Claude tab must reply 'newtab', got {r:?}"
        )),
        None => failures.push("(b) refusal: no reply to the second `claude` request".into()),
    }

    // === (c) in-place promotion of a terminal pane in a non-Terminals project =====
    seed_promotable_terminal_tab(cx, &state, &work_a);
    let reply_c = send_claude(cx, &socket_path, &work_a, &[], "promote-tab", "promote-pane").await;
    match reply_c {
        Some(r) => {
            let parts: Vec<&str> = r.trim_end().split(' ').collect();
            if parts.first() != Some(&"inplace") {
                failures.push(format!("(c) promotion: reply must begin 'inplace', got {r:?}"));
            } else if parts.len() < 2 || !is_v4_uuid(parts[1]) {
                failures.push(format!(
                    "(c) promotion: field 2 must be a valid v4 uuid (mint-new), got {r:?}"
                ));
            }
            // Tolerate an optional 3rd field (R17's default-ON settings pointer).
            if parts.len() > 3 {
                failures.push(format!("(c) promotion: reply carries too many fields: {r:?}"));
            }
        }
        None => failures.push("(c) promotion: no reply to the promoting `claude` request".into()),
    }
    // The model flipped: the terminal pane is now a running Claude pane.
    if !poll_pane_promoted(cx, &state, "promote-tab", "promote-pane").await {
        failures.push(
            "(c) promotion: the terminal pane did not flip to a running Claude pane in the model"
                .into(),
        );
    }

    // === (d) worktree split: `claude -w foo` ⇒ Tab.cwd + bucket anchor ============
    let tabs_before_d = all_tab_ids(cx, &state);
    let reply_d = send_claude(cx, &socket_path, &work_d, &["-w".into(), "foo".into()], "", "").await;
    if reply_d.as_deref().map(str::trim_end) != Some("newtab") {
        failures.push(format!("(d) worktree: expected reply 'newtab', got {reply_d:?}"));
    }
    match poll_new_tab(cx, &state, &tabs_before_d).await {
        Some(wt_tab) => {
            let (cwd, proj_path) = tab_cwd_and_project_path(cx, &state, &wt_tab);
            let want_suffix = "/.claude/worktrees/foo";
            if !cwd.as_deref().is_some_and(|c| c.ends_with(want_suffix)) {
                failures.push(format!(
                    "(d) worktree: Tab.cwd {cwd:?} must end with {want_suffix:?}"
                ));
            }
            // The bucket project anchors at the invocation cwd (not the worktree).
            if proj_path.as_deref() != Some(work_d.as_str()) {
                failures.push(format!(
                    "(d) worktree: bucket project path {proj_path:?} must anchor at the invocation \
                     cwd {work_d:?}"
                ));
            }
        }
        None => failures.push("(d) worktree: the `claude -w foo` request produced no new tab".into()),
    }

    // === (e) typed `exit` removes a live terminal pane from the SHIPPED window =====
    // Add a deterministic read-then-exit pane to the Main tab (which keeps its Main
    // login shell, so the tab survives the exit — no dissolve, no quit-terminus),
    // spawn it, let the shipped `PaneHostView` sweep subscribe it, poll READY, then
    // write a line so it exits cleanly and assert the routed removal.
    let main_tab = nice_model::TabModel::MAIN_TERMINAL_TAB_ID.to_string();
    match spawn_exit_fixture_pane(cx, &state, &main_tab, &work_a) {
        Some(exit_pane) => {
            let handle = pane_handle(cx, &state, &main_tab, &exit_pane);
            let ready = match &handle {
                Some(h) => poll_grid_contains(cx, h, READY_MARKER).await,
                None => false,
            };
            if !ready {
                failures.push("(e) exit: the read-then-exit fixture pane never became ready".into());
            } else {
                write_pane_line(cx, &state, &main_tab, &exit_pane, b"go\n");
                if !poll_pane_gone(cx, &state, &main_tab, &exit_pane).await {
                    failures.push(
                        "(e) exit: the cleanly-exited pane was never removed from the shipped \
                         window (the subscription lift did not route Exited{held:false})"
                            .into(),
                    );
                }
            }
        }
        None => failures.push("(e) exit: could not add + spawn the read-then-exit fixture pane".into()),
    }

    // === (f) session_update rotation: /branch parent + /clear + cwd adopt =========
    // Reuse the tab promoted in leg (c): "promote-tab" (in non-Terminals project
    // "promote-proj"), whose claude pane "promote-pane" is now a running Claude with
    // a minted session id. That minted id is the pre-rotation OLD id.
    let old_sid = state.update(cx, |s, _cx| {
        s.model.tab_for("promote-tab").and_then(|t| t.claude_session_id.clone())
    });
    match old_sid {
        None => failures.push("(f) rotation: the promoted tab carries no session id to rotate".into()),
        Some(old_sid) => {
            // -- f1 /branch: source=resume + a NEW id + a cwd move. The pre-rotation
            //    cwd is `work_a` (seed_promotable_terminal_tab set promote-tab.cwd).
            let branch_wt = format!("{work_a}/.claude/worktrees/branch-wt");
            send_session_update(cx, &socket_path, "promote-pane", "branch-new-id", Some("resume"), Some(&branch_wt)).await;
            match poll_branch_parent(cx, &state, "promote-proj", &old_sid, "promote-tab").await {
                None => failures.push(
                    "(f) branch: no sibling parent tab pinned to the OLD id materialized in promote-proj".into(),
                ),
                Some(parent_id) => {
                    let snap = state.update(cx, |s, _cx| {
                        let parent = s.model.tab_for(&parent_id).cloned();
                        let orig = s.model.tab_for("promote-tab").cloned();
                        (parent, orig)
                    });
                    let (parent, orig) = snap;
                    let parent = parent.expect("parent id just polled must resolve");
                    let orig = orig.expect("originating promote-tab must still exist");
                    // Sibling parent: deferred (not running), pinned to OLD id, at ROOT.
                    let parent_claude = parent.panes.iter().find(|p| p.kind == PaneKind::Claude);
                    if parent_claude.map(|p| p.is_claude_running) != Some(false) {
                        failures.push("(f) branch: sibling parent's claude pane must be is_claude_running == false (deferred)".into());
                    }
                    if parent.parent_tab_id.is_some() {
                        failures.push(format!(
                            "(f) branch: ROOT PROMOTION — the new parent must render at root (parent_tab_id == None), got {:?}",
                            parent.parent_tab_id
                        ));
                    }
                    // The sibling inherits the PRE-rotation cwd (its old-id transcript
                    // lives in the pre-rotation bucket) — the ordering pin, live.
                    if parent.cwd != work_a {
                        failures.push(format!(
                            "(f) branch: sibling parent must inherit the pre-rotation cwd {work_a:?}, got {:?}",
                            parent.cwd
                        ));
                    }
                    // Originating tab: re-parented UNDER the new root (renders indented —
                    // the landed row_indent contract keys off parent_tab_id.is_some()),
                    // moved into the post-rotation worktree, carrying the NEW id.
                    if orig.parent_tab_id.as_deref() != Some(parent_id.as_str()) {
                        failures.push(format!(
                            "(f) branch: originating tab must be re-parented under the new root (indented), got parent_tab_id {:?}",
                            orig.parent_tab_id
                        ));
                    }
                    if orig.cwd != branch_wt {
                        failures.push(format!(
                            "(f) branch: originating tab must move to the post-rotation worktree {branch_wt:?}, got {:?}",
                            orig.cwd
                        ));
                    }
                    if orig.claude_session_id.as_deref() != Some("branch-new-id") {
                        failures.push(format!(
                            "(f) branch: originating tab must carry the NEW session id, got {:?}",
                            orig.claude_session_id
                        ));
                    }

                    // -- f1-overlay (Bug 1): activating the deferred branch parent
                    //    must NOT flash the stray "Launching…" overlay. Its
                    //    ResumeDeferred shell already spawned + printed its prompt
                    //    while the parent tab was INACTIVE, so the pane's one-shot
                    //    OutputStarted fired to zero view subscribers; the FRESH
                    //    TerminalView mounted on first visit must read the latched
                    //    output_started and start its overlay cleared (never arming
                    //    the grace). NOTE: activating only mounts + focuses the view —
                    //    it never presses Enter, so the prefilled `claude --resume`
                    //    stays un-run (hermeticity: the stub claude is never spawned).
                    let parent_claude = parent
                        .panes
                        .iter()
                        .find(|p| p.kind == PaneKind::Claude)
                        .map(|p| p.id.clone());
                    match parent_claude {
                        None => failures
                            .push("(f) branch-overlay: the sibling parent has no Claude pane".into()),
                        Some(parent_pane) => {
                            let printed = match pane_handle(cx, &state, &parent_id, &parent_pane) {
                                Some(h) => poll_grid_nonempty(cx, &h).await,
                                None => false,
                            };
                            if !printed {
                                failures.push(
                                    "(f) branch-overlay: the branch parent's deferred shell never \
                                     printed a prompt (cannot conclude the overlay was suppressed)"
                                        .into(),
                                );
                            } else {
                                // Activate the parent tab: the shipped host builds a
                                // fresh TerminalView for its already-output pane.
                                let _ = state.update(cx, |s, cx| {
                                    s.model.select_tab(&parent_id);
                                    cx.notify();
                                });
                                // Well past the 750 ms launch grace: a buggy overlay
                                // would have armed on first paint and shown by now.
                                settle(cx, 1200).await;
                                let view = whandle
                                    .update(cx, |shell, _w, _a| shell.scenario_pane_host())
                                    .ok()
                                    .and_then(|ph| {
                                        ph.update(cx, |ph, _| ph.scenario_terminal_for(&parent_pane))
                                    });
                                match view {
                                    None => failures.push(
                                        "(f) branch-overlay: activating the branch parent mounted no \
                                         TerminalView for its pane".into(),
                                    ),
                                    Some(view) => {
                                        let (visible, ever) = view.update(cx, |v, _| {
                                            (v.overlay_visible(), v.overlay_ever_visible())
                                        });
                                        if visible {
                                            failures.push(
                                                "(f) branch-overlay: the \"Launching…\" overlay is \
                                                 VISIBLE on the branch parent (Bug 1 regressed)".into(),
                                            );
                                        }
                                        if ever {
                                            failures.push(
                                                "(f) branch-overlay: the overlay FLASHED on the branch \
                                                 parent (overlay_ever_visible latched — Bug 1 regressed)"
                                                    .into(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // -- f2 /clear: source=clear + a new id ⇒ id updates in place, NO new tab.
            let count_before_clear = project_tab_count(cx, &state, "promote-proj");
            send_session_update(cx, &socket_path, "promote-pane", "cleared-id", Some("clear"), None).await;
            if !poll_tab_session_id(cx, &state, "promote-tab", "cleared-id").await {
                failures.push("(f) clear: /clear must update the originating tab's session id in place".into());
            }
            if project_tab_count(cx, &state, "promote-proj") != count_before_clear {
                failures.push("(f) clear: /clear must NOT materialize a new tab".into());
            }

            // -- f3 cwd adopt: a same-id update carrying a fresh cwd ⇒ Tab.cwd adopts.
            let adopt_cwd = format!("{work_a}/.claude/worktrees/adopt-wt");
            send_session_update(cx, &socket_path, "promote-pane", "cleared-id", Some("clear"), Some(&adopt_cwd)).await;
            if !poll_tab_cwd(cx, &state, "promote-tab", &adopt_cwd).await {
                failures.push("(f) cwd adopt: a cwd-bearing update must adopt onto Tab.cwd".into());
            }
        }
    }

    // === teardown: drop every session so no zsh / stub outlives the window ========
    let _ = state.update(cx, |s, _cx| s.teardown());
    settle(cx, 200).await;

    build_report(failures)
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "claude lifecycle OK (shipped window): a socket `claude` newtab spawned a \
                     running Claude tab with a minted v4 uuid whose stub OSC titles drove the \
                     sidebar-dot status Thinking → Waiting; a second `claude` in that tab was \
                     refused (newtab); a terminal pane promoted in place (inplace <uuid> + model \
                     flip); `claude -w foo` split Tab.cwd into .claude/worktrees/foo anchored at \
                     the invocation cwd; a typed `exit` removed a live terminal pane via the \
                     shipped subscription lift; and a `session_update` /branch rotation \
                     (source=resume + new id + cwd move) materialized a deferred sibling parent \
                     pinned to the OLD id at root with the pre-rotation cwd while the originating \
                     tab re-parented under it into the post-rotation worktree, a /clear rotated \
                     the id in place with no new tab, and a cwd-bearing update adopted Tab.cwd."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} claude-lifecycle assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}

// -- raw-socket `claude` drive -----------------------------------------------

/// Drive a raw `claude` request over the control socket on a DEDICATED thread (so
/// the blocking read never wedges the foreground drain that answers it), then poll
/// its reply channel between settles. Returns the trimmed reply line (`None` on
/// timeout / no reply).
async fn send_claude(
    cx: &mut AsyncApp,
    socket_path: &str,
    cwd: &str,
    args: &[String],
    tab_id: &str,
    pane_id: &str,
) -> Option<String> {
    let payload = claude_json(cwd, args, tab_id, pane_id);
    let rx = raw_request(socket_path.to_string(), payload);
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        match rx.try_recv() {
            Ok(Some(bytes)) => return Some(String::from_utf8_lossy(&bytes).into_owned()),
            Ok(None) => return None,
            Err(std::sync::mpsc::TryRecvError::Empty) => continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

/// Connect, write one newline-terminated JSON payload, read the reply to EOF (the
/// handler drops the server end after replying). Retries the connect until a
/// newline-terminated reply arrives or a deadline elapses.
fn raw_request(path: String, payload: String) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut result: Option<Vec<u8>> = None;
        while Instant::now() < deadline {
            if let Ok(mut s) = UnixStream::connect(&path) {
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

/// Build the frozen `claude` NDJSON request line.
fn claude_json(cwd: &str, args: &[String], tab_id: &str, pane_id: &str) -> String {
    let args_json = args
        .iter()
        .map(|a| format!("\"{}\"", json_escape(a)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"action\":\"claude\",\"cwd\":\"{}\",\"args\":[{}],\"tabId\":\"{}\",\"paneId\":\"{}\"}}",
        json_escape(cwd),
        args_json,
        json_escape(tab_id),
        json_escape(pane_id),
    )
}

/// Minimal JSON string escaping (temp paths + kebab args carry no control chars).
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Fire-and-forget a `session_update` over the control socket. It carries no reply
/// (the parser drops the client fd BEFORE dispatch — fire-and-forget), so connect
/// ONCE (retrying only the connect until the socket answers or a short deadline),
/// write the framed line exactly once, and return; the caller polls the model for
/// the routed mutation. Writing exactly once matters: a re-sent line would
/// materialize a second phantom branch parent.
async fn send_session_update(
    cx: &mut AsyncApp,
    socket_path: &str,
    pane_id: &str,
    session_id: &str,
    source: Option<&str>,
    cwd: Option<&str>,
) {
    let payload = session_update_json(pane_id, session_id, source, cwd);
    let path = socket_path.to_string();
    let done = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            match UnixStream::connect(&path) {
                Ok(mut s) => {
                    let _ = s.write_all(payload.as_bytes());
                    let _ = s.write_all(b"\n");
                    let _ = s.flush();
                    // The server reads the buffered line, then hits EOF on our drop.
                    return;
                }
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(_) => return,
            }
        }
    });
    let _ = done.join();
    // Let the foreground drain route the fire-and-forget message before we poll.
    settle(cx, POLL_MS).await;
}

/// Build a frozen `session_update` NDJSON request line (absent `source`/`cwd` are
/// omitted, exactly as the hook script does when the field is empty).
fn session_update_json(pane_id: &str, session_id: &str, source: Option<&str>, cwd: Option<&str>) -> String {
    let mut fields = format!(
        "\"action\":\"session_update\",\"paneId\":\"{}\",\"sessionId\":\"{}\"",
        json_escape(pane_id),
        json_escape(session_id),
    );
    if let Some(src) = source {
        fields.push_str(&format!(",\"source\":\"{}\"", json_escape(src)));
    }
    if let Some(c) = cwd {
        fields.push_str(&format!(",\"cwd\":\"{}\"", json_escape(c)));
    }
    format!("{{{fields}}}")
}

/// Whether `s` is a lowercase RFC-4122 v4 UUID (`8-4-4-4-12`, version nibble `4`,
/// variant nibble in `[89ab]`) — the shape `mint_session_uuid` produces.
fn is_v4_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => c == b'-',
            14 => c == b'4',
            19 => matches!(c, b'8' | b'9' | b'a' | b'b'),
            _ => c.is_ascii_hexdigit() && !c.is_ascii_uppercase(),
        };
        if !ok {
            return false;
        }
    }
    true
}

// -- model / session readers -------------------------------------------------

fn all_tab_ids(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Vec<String> {
    state.update(cx, |s, _cx| {
        s.model
            .projects
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect()
    })
}

/// Poll until exactly one tab id appears that was not in `before`, returning it.
async fn poll_new_tab(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    before: &[String],
) -> Option<String> {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let now = all_tab_ids(cx, state);
        if let Some(new) = now.iter().find(|t| !before.contains(t)) {
            return Some(new.clone());
        }
    }
    None
}

/// The `(claude_pane_id, companion_pane_id)` of a `[Claude, Terminal 1]` tab.
fn tab_pane_ids(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
) -> Option<(String, String)> {
    state.update(cx, |s, _cx| {
        let tab = s.model.tab_for(tab_id)?;
        let claude = tab.panes.first()?.id.clone();
        let companion = tab.panes.get(1)?.id.clone();
        Some((claude, companion))
    })
}

fn pane_is_claude_running(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    state.update(cx, |s, _cx| {
        s.model
            .tab_for(tab_id)
            .and_then(|t| t.panes.iter().find(|p| p.id == pane_id))
            .map(|p| p.is_claude_running)
            .unwrap_or(false)
    })
}

fn tab_session_id(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str) -> Option<String> {
    state.update(cx, |s, _cx| {
        s.model.tab_for(tab_id).and_then(|t| t.claude_session_id.clone())
    })
}

fn tab_cwd_and_project_path(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
) -> (Option<String>, Option<String>) {
    state.update(cx, |s, _cx| {
        let cwd = s.model.tab_for(tab_id).map(|t| t.cwd.clone());
        let proj = s
            .model
            .project_tab_index(tab_id)
            .map(|(pi, _)| s.model.projects[pi].path.clone());
        (cwd, proj)
    })
}

/// Poll until a tab in `project_id` (other than `exclude_tab`) is pinned to
/// `session_id`, returning its id — the materialized branch parent, located by its
/// pinned OLD session id (the originating tab excluded, since it briefly held the
/// same id pre-rotation).
async fn poll_branch_parent(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    project_id: &str,
    session_id: &str,
    exclude_tab: &str,
) -> Option<String> {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let found = state.update(cx, |s, _cx| {
            s.model.projects.iter().find(|p| p.id == project_id).and_then(|p| {
                p.tabs
                    .iter()
                    .find(|t| t.id != exclude_tab && t.claude_session_id.as_deref() == Some(session_id))
                    .map(|t| t.id.clone())
            })
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The number of tabs currently in `project_id` (0 if the project is gone).
fn project_tab_count(cx: &mut AsyncApp, state: &Entity<WindowState>, project_id: &str) -> usize {
    state.update(cx, |s, _cx| {
        s.model
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.tabs.len())
            .unwrap_or(0)
    })
}

async fn poll_tab_session_id(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    want: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        if tab_session_id(cx, state, tab_id).as_deref() == Some(want) {
            return true;
        }
    }
    false
}

async fn poll_tab_cwd(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    want: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let got = state.update(cx, |s, _cx| s.model.tab_for(tab_id).map(|t| t.cwd.clone()));
        if got.as_deref() == Some(want) {
            return true;
        }
    }
    false
}

async fn poll_tab_status(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    want: TabStatus,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let got = state.update(cx, |s, _cx| s.model.tab_for(tab_id).map(|t| t.status()));
        if got == Some(want) {
            return true;
        }
    }
    false
}

async fn poll_has_pane(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        if state.update(cx, |s, _cx| s.session.has_pane(tab_id, pane_id)) {
            return true;
        }
    }
    false
}

async fn poll_pane_promoted(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let ok = state.update(cx, |s, _cx| {
            s.model
                .tab_for(tab_id)
                .and_then(|t| t.panes.iter().find(|p| p.id == pane_id))
                .map(|p| p.kind == PaneKind::Claude && p.is_claude_running)
                .unwrap_or(false)
        });
        if ok {
            return true;
        }
    }
    false
}

async fn poll_pane_gone(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let gone = state.update(cx, |s, _cx| {
            s.model
                .tab_for(tab_id)
                .map(|t| !t.panes.iter().any(|p| p.id == pane_id))
                .unwrap_or(true)
        });
        if gone {
            return true;
        }
    }
    false
}

/// Seed a plain terminal-only tab into a fresh non-Terminals project — the
/// promotable target leg (c) needs (a terminal pane in a non-Terminals tab with no
/// running Claude; the pane needs no live pty — promotion is model-only). Selects
/// it so the shipped shell renders it.
fn seed_promotable_terminal_tab(cx: &mut AsyncApp, state: &Entity<WindowState>, cwd: &str) {
    let _ = state.update(cx, |s, _cx| {
        s.model.ensure_project("promote-proj", "Promote", cwd);
        let mut tab = Tab::new("promote-tab", "term", cwd);
        tab.panes = vec![Pane::new("promote-pane", "Terminal 1", PaneKind::Terminal)];
        tab.active_pane_id = Some("promote-pane".to_string());
        tab.next_terminal_index = 2;
        if let Some(pi) = s.model.projects.iter().position(|p| p.id == "promote-proj") {
            s.model.projects[pi].tabs.push(tab);
        }
        s.model.select_tab("promote-tab");
    });
}

/// Add a `sh -c 'echo READY; read; exit 0'` pane to `tab_id` and spawn it through
/// the manager; the shipped `PaneHostView` sweep subscribes it. Returns the new
/// pane id.
fn spawn_exit_fixture_pane(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    cwd: &str,
) -> Option<String> {
    state.update(cx, |s, cx| {
        let pane_id = s.session.add_pane(&mut s.model, tab_id, None)?;
        let spec = nice_term_core::SpawnSpec::command(
            format!("sh -c 'echo {READY_MARKER}; IFS= read -r _l; exit 0'"),
            cwd.to_string(),
        )
        // Blank ZDOTDIR so no rc sourcing races the marker (spec-wins injection).
        .with_env(vec![("ZDOTDIR".to_string(), cwd.to_string())])
        .with_size(24, 80);
        s.session.spawn_pane(tab_id, &pane_id, spec, cx).ok()?;
        // Re-render so the host's sweep subscribes the fresh pane before it exits.
        cx.notify();
        Some(pane_id)
    })
}

fn pane_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    state.update(cx, |s, _cx| s.session.pane_handle(tab_id, pane_id))
}

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

/// Poll until the pane's grid holds any non-whitespace content — the deferred
/// branch-parent shell's prompt. By the time it prints, that pane's one-shot
/// `OutputStarted` has fired (and, since the pane has no view yet, drained to zero
/// subscribers) — the exact precondition the /branch-overlay leg reproduces.
async fn poll_grid_nonempty(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> bool {
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        let grid = handle.update(cx, |h, _cx| h.session().grid_lines().join("\n"));
        if grid.chars().any(|c| !c.is_whitespace()) {
            return true;
        }
    }
    false
}

fn write_pane_line(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
    bytes: &[u8],
) {
    if let Some(handle) = pane_handle(cx, state, tab_id, pane_id) {
        let bytes = bytes.to_vec();
        let _ = handle.update(cx, |h, _cx| {
            let _ = h.session().write_input(&bytes);
        });
    }
}
