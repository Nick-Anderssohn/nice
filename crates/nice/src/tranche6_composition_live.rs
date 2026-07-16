//! `tranche6-composition` self-test scenario — the R27 §6 tranche-6 close-out
//! composition leg (Validation §6), the last plan of tranche 6.
//!
//! Where each earlier t6 scenario proves ONE surface in isolation, this one
//! composes the WHOLE tranche's board on the **REAL shipped launch window**
//! (opened through `crate::app::open_managed_window` / `build_window_root` →
//! [`AppShellView`](crate::app_shell::AppShellView), the exact path
//! `crate::app::run` and every ⌘N take — the `app_shell_live` / `settings::scenario`
//! §6 mount precedent, NEVER a scenario-only toolbar). No earlier cycle asserted
//! R25 + R26 + R27 together on the shipped window; this is the Milestone-6-plus-
//! tranche-6 claim (TRANCHE-2-NOTES §5/§6).
//!
//! ## The three composed legs
//!
//!   * **(a) R27 — the update pill on the shipped toolbar.** With the injected
//!     recording `ReleaseFetcher` scripted to a newer tag, drive the foreground
//!     `check_now` on the shipped window → the trailing pill appears on the SHIPPED
//!     toolbar (AX `"toolbar.updateAvailable"`, an `AXButton`); a real guarded-HID
//!     click on it (behind the mandatory preflight) opens the popover showing
//!     `brew update && brew upgrade --cask nice`.
//!   * **(b) R25 — pill drag-reorder on the shipped strip.** With ≥2 panes in the
//!     active tab, a real guarded-HID drag of the trailing pill leftward past the
//!     leading pill's midpoint should reorder it BEFORE the leader — a COMMITTED
//!     reorder read back off the shipped strip (`pane_ids()` flips), hard-asserted
//!     when the drag commits, and hard-FAILED if it commits to the WRONG slot. But
//!     a synthetic global-HID drag lands only the PRESS: AppKit does not deliver
//!     `mouseDragged:` for an injected press (no implicit mouse-grab), so R25's
//!     gpui `on_drag`/`on_drop` never arm and no reorder commits. A landed-press-
//!     with-order-untouched therefore DEFERS (the honest-deferral discipline
//!     below) rather than failing — the deterministic reorder is hard-asserted
//!     in-process by `nice-itests`.
//!   * **(c) R26 — handoff nested tab + settings toggle.** A raw-`UnixStream`
//!     `handoff` naming a seeded originating Claude tab replies `ok` and opens a
//!     nested `[HANDOFF] <title>` tab on the shipped window (parented under the
//!     originating tab), with the stub `claude` (never the machine's real one); and
//!     ⌘, opens R23's shipped settings window whose rail exposes the Claude section
//!     (an `AXButton`) — the home of R26's `settings.claude.installHandoffSkill`
//!     handoff toggle (whose click behaviour is pinned by R26's own `claude_pane`
//!     unit test + the `handoff` installer scenario).
//!
//! ## The guarded global-HID seam (SELFTEST-ONLY; the `platform.rs` invariant carve-out)
//!
//! The R25 drag and the R27 pill click post through the NEW guarded global-HID
//! seams ([`platform::post_global_left_click`] / [`platform::post_global_left_drag`])
//! — NOT `CGEventPostToPid` — because pid-posted mouse events silently drop (hover
//! paints, `mouseDown` never fires — the M6 record, MEMORY: nice synthetic
//! mouse). Every global post is fenced by the mandatory preflight: (1) **activate
//! the app + raise the window**, then (2) **verify frontmost-at-point** — the
//! shipped window MUST own the click coordinate per
//! [`platform::frontmost_window_owns_point`] ([`CGWindowListCopyWindowInfo`] z-order
//! check). Only then does the post proceed and the leg HARD-ASSERTS the outcome; a
//! failed preflight (another window on top / the point off ours) **DEFERS LOUDLY**
//! — an explicit `DEFER` with remediation, no post — so an unattended
//! `NICE_SELFTEST=all` run can never send clicks into another app. A synthetic
//! gesture that passes the preflight yet does not drive the real behaviour also
//! DEFERS (the `pane_strip_live` honest-deferral discipline: a synthetic mouse
//! event need not land on a gpui hitbox). This covers two shapes for the R25 drag:
//! the press not landing at all, AND — the shape observed here — the press landing
//! (it selects the mover) while the DRAG never arms, because AppKit does not
//! deliver `mouseDragged:` for an injected press (no implicit mouse-grab), so
//! gpui's `on_drag`/`on_drop` never fire. Either way the deterministic reorder is
//! hard-asserted in-process by `nice-itests`, and the leg never reads a vacuous
//! "order unchanged" as a pass — it DEFERS, and still hard-FAILS a reorder that
//! commits to the wrong slot. Keyboard events (⌘,) stay `CGEventPostToPid` to
//! nice's own pid.
//!
//! ## Hermeticity
//!
//! Everything is injected. The fetch is the `run_selftest`-installed recording
//! `ReleaseFetcher` (never the network / `github.com`, never the launch timer — the
//! worker is `app::run`-only + gated OFF). The handoff stub `claude` is seeded via
//! the `ResolvedClaudePath` Global with `NICE_CLAUDE_OVERRIDE` UNSET; the machine's
//! real `claude` is NEVER spawned. `HOME` is a sandbox with no rc for the driver's
//! lifetime (restored at teardown), so the shipped Main pane's login shell + the
//! handoff pane's stub source nothing. A `SavedInputSource` is held for the whole
//! leg (Pinyin is enabled on this machine; a mid-leg failure must not strand it).
//! `Gate::SelfReported`; registered BEFORE `multiwindow` — its `build_window_root`
//! only `register`s (no `WindowRegistry` close observer), so its window never trips
//! the quit-when-empty terminus `multiwindow` owns as the last gate. Teardown drops
//! every session so no zsh / stub outlives the window.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{Pane, PaneKind, Tab};

use crate::app_shell::AppShellView;
use crate::platform;
use crate::release_check::{self, release_fetch};
use crate::session_manager::ResolvedClaudePath;
use crate::settings::window::{current_settings_window, install_open_settings_command};
use crate::toolbar::WindowToolbarView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- fixed values ------------------------------------------------------------

/// The newer tag the recording fetcher reports for leg (a) (clearly `> 0.1.0`, the
/// unbundled `CARGO_PKG_VERSION` the checker compares against under `cargo run`).
const NEWER_TAG: &str = "v9.9.9";
/// The exact combined brew command the popover must show (leg a).
const BREW_COMMAND: &str = "brew update && brew upgrade --cask nice";
/// The pill's AX title (`aria_label`) + expected role.
const PILL_AX_TITLE: &str = "Update available";
const PILL_AX_ROLE: &str = "AXButton";
/// The settings rail's Claude section (leg c) — the R26 handoff toggle's home. Its
/// rail button carries `.id("settings.section.claude")` + `Role::Button` +
/// `aria_label("Claude")`, so it surfaces as an `AXButton` titled "Claude".
const CLAUDE_SECTION_AX_TITLE: &str = "Claude";
const CLAUDE_SECTION_AX_ROLE: &str = "AXButton";
/// The shipped settings window's title (`settings_window_options`) — the AX search
/// for the Claude section is scoped to THIS window's subtree so another scenario's
/// window (in the serial suite these all share one process) or a lingering menu
/// cannot surface a same-titled node first.
const SETTINGS_WINDOW_AX_TITLE: &str = "Settings";
/// R26's handoff-toggle a11y id — pinned here for the report; its click behaviour
/// is covered by R26's `claude_pane` unit test + the `handoff` installer scenario.
const HANDOFF_TOGGLE_AX_ID: &str = "settings.claude.installHandoffSkill";
/// ⌘, — OpenSettings (`CGKeyCode` for `,`).
const KC_COMMA: u16 = 43;

/// How far LEFT of the leading pill's centre the reorder release lands. A pill's
/// centre IS its midpoint and the resolver flips on `x > mid_x`, so releasing a few
/// pt left of centre resolves to the before-leader slot (`place_after == false`)
/// while staying inside the leader's frame (the `pane_strip_live` reorder margin).
const REORDER_BEFORE_MARGIN: f64 = 15.0;

/// General poll timeout for a state / AX / popover transition.
const POLL_TIMEOUT: Duration = Duration::from_secs(4);
/// Poll cap + interval for the socket-routed tab creation (the drain-task hop, on
/// the real clock) — mirrors `handoff_live`.
const ROUTE_POLLS: usize = 60;
const POLL_MS: u64 = 100;

/// Accessibility-grant remediation, shared verbatim with the other CGEvent
/// scenarios: without the TCC grant every synthetic event is silently dropped.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so synthetic \
events are SILENTLY DROPPED and no injected click/drag/chord can reach the window. \
Fix: System Settings → Privacy & Security → Accessibility → enable the process \
hosting this run. If it shows ON but this persists, the grant is STALE — remove it \
with '-' and re-add it, then re-run. Verify: swift -e 'import ApplicationServices; \
print(AXIsProcessTrusted())'";

// -- fixture -----------------------------------------------------------------

/// The sandboxed fixture: a fake `$HOME` (no rc), an argv-idle stub `claude`, and
/// its scratch base. Mirrors the `handoff` scenario's hermeticity.
struct Fixture {
    base: PathBuf,
    home: PathBuf,
    work: PathBuf,
    prev_home: Option<String>,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-t6-comp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;
        let home = base.join("home");
        let work = base.join("work");
        let bin = base.join("bin");
        for d in [&home, &work, &bin] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }
        // The stub `claude`: idle reading stdin so its pane stays alive until
        // teardown. NEVER the machine's real claude (hermeticity). It records no
        // argv — this leg asserts the [HANDOFF] tab + `ok` reply, not the flags
        // (the `handoff` scenario pins the argv).
        let stub = bin.join("claude");
        std::fs::write(&stub, "#!/bin/sh\nwhile IFS= read -r _l; do : ; done\n")
            .context("write stub claude")?;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .context("chmod stub claude")?;
        // Remove any prior scenario's NICE_CLAUDE_OVERRIDE so the ResolvedClaudePath
        // Global (seeded in `open`) is what resolves the stub.
        // SAFETY: single-threaded scenario setup, before any Claude pane forks.
        unsafe { std::env::remove_var("NICE_CLAUDE_OVERRIDE") };
        let prev_home = std::env::var("HOME").ok();
        Ok(Fixture {
            base,
            home,
            work,
            prev_home,
        })
    }

    fn stub_path(&self) -> String {
        self.base.join("bin").join("claude").to_string_lossy().into_owned()
    }

    fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }

    fn work_str(&self) -> String {
        self.work.to_string_lossy().into_owned()
    }

    /// Restore `$HOME` and drop the whole scratch tree.
    fn teardown(&self) {
        // SAFETY: single-threaded teardown after the driver's last spawn.
        match &self.prev_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the composition window through the SHIPPED builder and spawn its driver
/// (self-reported gate). Seeds the stub-`claude` `ResolvedClaudePath` global,
/// sandboxes `HOME` (no rc) for the driver's lifetime (restored at teardown), and
/// installs the ⌘, / OpenSettings command exactly as the shipped `run` does.
pub fn open_tranche6_composition_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let stub = fixture.stub_path();
    let home = fixture.home_str();
    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        // The shipped builder reads process-globals (fonts, keymap); seed them
        // (idempotent — an earlier suite scenario may already have).
        crate::keymap::install_shortcuts(app);
        // ⌘, / OpenSettings, exactly as the shipped `run` (leg c).
        install_open_settings_command(app);
        // Seed the stub as the resolved claude binary (env override unset ⇒
        // is_override false ⇒ the handoff pane spawns the stub, never real claude).
        app.set_global(ResolvedClaudePath(Some(stub.clone())));
        // Sandbox HOME (no rc) for the whole driver; restored at teardown.
        // SAFETY: single-threaded setup; the Main pane forks synchronously inside
        // `open_managed_window` under this HOME.
        unsafe { std::env::set_var("HOME", &home) };
        crate::app::open_managed_window(app)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_composition(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'tranche6-composition': {}", report.detail);
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

async fn run_composition(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    // Frontmost/key + painted once (registers handlers, first AccessKit-eligible
    // frame) before any event.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // The mouse legs (a/b) + the ⌘, chord (c) all need the TCC grant; without it
    // every synthetic event is a silently-dropped no-op. The pixel-free content
    // assertions still run, but the shipped-surface legs would be vacuous — surface
    // it as a hard failure (the shipped composition IS the point of this scenario).
    if !platform::accessibility_trusted() {
        fixture.teardown();
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    // Hold the user's input source for the whole leg (IME restore on drop).
    let _saved = platform::current_input_source();

    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        fixture.teardown();
        return CadenceReport::error(
            "tranche6-composition: the shipped builder did not register the window's WindowState"
                .to_string(),
        );
    };
    let shell = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => {
            fixture.teardown();
            return CadenceReport::error(format!(
                "tranche6-composition: could not read the shipped shell view: {e}"
            ));
        }
    };
    let toolbar = shell.update(cx, |s, _| s.scenario_toolbar());
    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // (a) R27 — the update pill + popover on the SHIPPED toolbar.
    leg_a_update_pill(cx, whandle, &toolbar, pid, &mut failures, &mut deferred).await;

    // (b) R25 — a real committed pill reorder on the SHIPPED strip.
    leg_b_reorder(cx, whandle, &toolbar, &mut failures, &mut deferred).await;

    // (c) R26 — the handoff nested tab + the shipped settings toggle's home.
    leg_c_handoff(cx, whandle, &state, &fixture, pid, &mut failures).await;

    // Teardown: drop every session so no zsh / stub outlives the window, clear the
    // stub global, restore HOME + drop the scratch tree.
    let _ = state.update(cx, |s, _cx| s.teardown());
    settle(cx, 200).await;
    let _ = cx.update(|app| app.set_global(ResolvedClaudePath(None)));
    fixture.teardown();

    build_report(failures, deferred)
}

// ---- (a) R27 update pill ---------------------------------------------------

async fn leg_a_update_pill(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    let Some(fake) = release_fetch::selftest_fake() else {
        failures.push(
            "(a) the recording ReleaseFetcher was not installed by run_selftest \
             (release_fetch::selftest_fake() is None) — the hermetic fetch seam is missing"
                .to_string(),
        );
        return;
    };
    // Script a newer tag + drive the foreground check_now on the shipped window.
    fake.set_tag(NEWER_TAG);
    cx.update(|app| release_check::check_now(app));
    let flipped = poll_app(cx, POLL_TIMEOUT, |app| {
        release_check::update_available(app).as_deref() == Some(NEWER_TAG)
    })
    .await;
    if !flipped {
        failures.push(format!(
            "(a) after check_now with the fetcher set to {NEWER_TAG}, \
             release_check::update_available did not become Some(\"{NEWER_TAG}\") on the shipped window"
        ));
        return;
    }
    // The pill surfaces on the REAL AX tree as an AXButton titled "Update available".
    match poll_ax(cx, toolbar, pid, PILL_AX_TITLE).await {
        Some(role) if role == PILL_AX_ROLE => {
            eprintln!("[selftest] tranche6-composition (a): the update pill is exposed as an {PILL_AX_ROLE} titled '{PILL_AX_TITLE}' on the shipped toolbar");
        }
        Some(role) => failures.push(format!(
            "(a) the shipped pill's AX element is titled '{PILL_AX_TITLE}' but its role is '{role}', not '{PILL_AX_ROLE}'"
        )),
        None => {
            failures.push(format!(
                "(a) no AX element titled '{PILL_AX_TITLE}' surfaced within {POLL_TIMEOUT:?} on the \
                 shipped toolbar — the update pill did not expose on the AX tree"
            ));
            return;
        }
    }

    // A real guarded-HID click on the pill → the popover opens (hard when the
    // preflight passes; DEFER LOUDLY otherwise — never a blind global post).
    // Ensure it starts closed so the click's job is unambiguous.
    let _ = whandle.update(cx, |_r, _w, app| {
        toolbar.update(app, |v, cx| v.drive_dismiss_update_popover(cx))
    });
    settle(cx, 150).await;
    let Some((cx_pt, cy_pt)) = toolbar.update(cx, |v, _| v.scenario_update_pill_center()) else {
        deferred.push(
            "(a) real click: the pill's painted bounds were not recorded — cannot target the pill \
             for a guarded-HID click. DEFERRED; the popover contents are asserted in-process below."
                .to_string(),
        );
        // Still assert the contents in-process so the leg is not vacuous.
        assert_popover_contents_in_process(cx, whandle, toolbar, failures).await;
        return;
    };
    match guarded_preflight(cx, whandle, cx_pt, cy_pt).await {
        PreflightOutcome::Owned((gx, gy)) => {
            platform::post_global_left_click(gx, gy, 1);
            let opened = poll_view(cx, toolbar, POLL_TIMEOUT, |v| v.scenario_update_popover_open()).await;
            if opened {
                let cmd = toolbar.update(cx, |v, cx| v.scenario_update_popover_command(cx));
                assert_command(cmd, failures, "a real guarded-HID click");
                eprintln!("[selftest] tranche6-composition (a): a real guarded-HID click on the shipped pill opened the popover ({BREW_COMMAND})");
                let _ = whandle.update(cx, |_r, _w, app| {
                    toolbar.update(app, |v, cx| v.drive_dismiss_update_popover(cx))
                });
                settle(cx, 150).await;
            } else {
                failures.push(
                    "(a) real click: the preflight passed and a global-HID click was posted at the \
                     pill centre on the shipped toolbar, but the popover did not open"
                        .to_string(),
                );
            }
        }
        PreflightOutcome::Deferred(msg) => {
            deferred.push(format!("(a) real click: {msg}"));
            // The popover contents are still asserted in-process (never vacuous).
            assert_popover_contents_in_process(cx, whandle, toolbar, failures).await;
        }
    }
}

/// Open the popover in-process and assert its exact combined brew command (the
/// deterministic fallback when the real click DEFERs — the `update_check_live`
/// content-pin precedent).
async fn assert_popover_contents_in_process(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    failures: &mut Vec<String>,
) {
    let _ = whandle.update(cx, |_r, window, app| {
        toolbar.update(app, |v, cx| v.drive_open_update_popover(window, cx))
    });
    settle(cx, 200).await;
    let cmd = toolbar.update(cx, |v, cx| v.scenario_update_popover_command(cx));
    assert_command(cmd, failures, "the in-process popover");
    let _ = whandle.update(cx, |_r, _w, app| {
        toolbar.update(app, |v, cx| v.drive_dismiss_update_popover(cx))
    });
    settle(cx, 150).await;
}

fn assert_command(cmd: Option<String>, failures: &mut Vec<String>, via: &str) {
    match cmd {
        Some(cmd) if cmd == BREW_COMMAND => {}
        Some(cmd) => failures.push(format!(
            "(a) {via}: the popover command was {cmd:?}, not exactly '{BREW_COMMAND}'"
        )),
        None => failures.push(format!("(a) {via}: the popover was not open when reading its command")),
    }
}

// ---- (b) R25 pill reorder --------------------------------------------------

async fn leg_b_reorder(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    // Ensure ≥2 panes on the active tab (the shipped Main tab starts with one).
    let mut ids = toolbar.update(cx, |v, cx| v.pane_ids(cx));
    if ids.len() < 2 {
        let _ = whandle.update(cx, |_r, _w, app| {
            toolbar.update(app, |v, cx| v.drive_add_terminal_pane(cx))
        });
        settle(cx, 400).await;
        ids = toolbar.update(cx, |v, cx| v.pane_ids(cx));
    }
    if ids.len() < 2 {
        failures.push("(b) could not get ≥2 panes on the shipped strip to reorder".to_string());
        return;
    }
    let leader = ids[0].clone();
    let mover = ids[1].clone();

    // Select the LEADER active, so a landed press on the mover flips active
    // leader→mover — the delivery signal (`move_pane` never touches
    // `active_pane_id`, so the flip evidences the PRESS landing, not the reorder).
    let _ = whandle.update(cx, |_r, window, app| {
        toolbar.update(app, |v, cx| v.drive_select_pane(&leader, window, cx))
    });
    settle(cx, 250).await;

    let (Some((sx, sy)), Some((tx, ty))) = (
        read_pill_center(cx, toolbar, &mover),
        read_pill_center(cx, toolbar, &leader),
    ) else {
        failures.push("(b) the leader/mover pills were not laid out (no bounds) — cannot post the reorder drag".to_string());
        return;
    };
    // Release LEFT of the leader's midpoint (its centre IS the midpoint) so the
    // resolver yields the before-leader slot (`place_after == false`).
    let end_x = tx - REORDER_BEFORE_MARGIN;

    let active_before = read_active(cx, toolbar);
    match guarded_drag(cx, whandle, sx, sy, end_x, ty).await {
        PreflightOutcome::Deferred(msg) => {
            deferred.push(format!("(b) reorder: {msg}"));
            return;
        }
        PreflightOutcome::Owned(_) => {}
    }
    settle(cx, 400).await;
    let active_after = read_active(cx, toolbar);
    let order_after = toolbar.update(cx, |v, cx| v.pane_ids(cx));

    // The press must be shown to have LANDED (it selected the mover) before we can
    // hard-assert the reorder — a synthetic press need not land on a gpui hitbox
    // (the `pane_strip_live` honest-deferral; the deterministic reorder is
    // hard-asserted in-process by nice-itests). A non-landing press DEFERS rather
    // than reading a vacuous "order unchanged" as a pass.
    let landed = active_before.as_deref() != Some(mover.as_str())
        && active_after.as_deref() == Some(mover.as_str());
    if !landed {
        deferred.push(format!(
            "(b) reorder: the guarded global-HID press did not register on the mover pill (active \
             {active_after:?}, was {active_before:?}) — a synthetic mouse event need not land on a \
             gpui hitbox. DEFERRED to a human drag; the deterministic reorder (move past B's \
             midpoint → order changes) is hard-asserted in-process (nice-itests)."
        ));
        return;
    }
    let reordered = order_after.len() >= 2
        && order_after[0] == mover
        && order_after[1] == leader;
    // The order the press LANDED but before any reorder (leader still ahead of the
    // mover): the drag did not engage R25's gpui `on_drag`/`on_drop`.
    let unchanged = order_after.len() >= 2
        && order_after[0] == leader
        && order_after[1] == mover;
    if reordered {
        eprintln!(
            "[selftest] tranche6-composition (b): a real guarded-HID drag reordered the mover before \
             the leader on the shipped strip — pane_ids now leads [{mover}, {leader}]"
        );
    } else if unchanged {
        // The press landed (it selected the mover) yet the strip order is untouched:
        // the synthetic global-HID DRAG did not arm gpui's drag-and-drop. AppKit
        // does not deliver `mouseDragged:` for an injected press (no implicit
        // mouse-grab — cursor-warp + re-cadenced posts were both tried and neither
        // drives it), so `on_drag`/`on_drag_move`/`on_drop` never fire. This is the
        // SAME synthetic-mouse limitation `pane_strip_live` DEFERS (its press does
        // not even land); the deterministic reorder — move past the leader's
        // midpoint → order flips + `save_to_store` persists — is hard-asserted
        // in-process by `nice-itests`. DEFER (never read a vacuous "order unchanged"
        // as a pass); a real committed reorder is the human-pass item.
        deferred.push(format!(
            "(b) reorder: the guarded global-HID press LANDED (mover {mover} selected) but the drag \
             committed no reorder — the shipped strip still leads [{leader}, {mover}]. AppKit does \
             not deliver `mouseDragged:` for a synthetic press, so R25's gpui drag-and-drop never \
             arms (cursor-warp + re-cadenced posts both tried; neither drives it). DEFERRED to a \
             human drag; the deterministic reorder is hard-asserted in-process (nice-itests)."
        ));
    } else {
        // The order changed to something OTHER than the expected before-leader slot
        // — a genuine reorder-to-wrong-slot regression, hard-failed.
        failures.push(format!(
            "(b) reorder: the press landed (mover {mover} selected) and the strip order changed to \
             {order_after:?}, but not to [{mover}, {leader}, …] — dragging the mover left past the \
             leader's midpoint must reorder it BEFORE the leader"
        ));
    }
}

// ---- (c) R26 handoff nested tab + settings toggle home ---------------------

async fn leg_c_handoff(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    fixture: &Fixture,
    pid: i32,
    failures: &mut Vec<String>,
) {
    // --- the socket handoff round-trip on the shipped window ---
    let Some(socket_path) = state.update(cx, |s, _cx| s.control_socket_path()) else {
        failures.push("(c) the shipped window armed no control socket (no path to drive the handoff)".to_string());
        return;
    };
    let work = fixture.work_str();

    // Seed a model-only originating Claude tab so the handoff NESTS under it.
    seed_originating_claude_tab(cx, state, &work, "comp-orig-tab", "comp-orig-pane", "my-project");
    let before = all_tab_ids(cx, state);
    let handoff_file = format!("{work}/comp-handoff.md");
    let reply = send_handoff(
        cx,
        &socket_path,
        &work,
        &handoff_file,
        "comp-orig-tab",
        "comp-orig-pane",
        "please continue the migration",
        "claude-opus-4-8",
        "xhigh",
    )
    .await;
    if reply.as_deref().map(str::trim_end) != Some("ok") {
        failures.push(format!("(c) handoff: expected reply 'ok', got {reply:?}"));
    }
    match poll_new_tab(cx, state, &before).await {
        None => failures.push("(c) handoff: the socket `handoff` produced no new tab on the shipped window".to_string()),
        Some(new_tab) => {
            let snap = state.update(cx, |s, _cx| {
                s.model
                    .tab_for(&new_tab)
                    .map(|t| (t.title.clone(), t.title_manually_set, t.parent_tab_id.clone()))
            });
            match snap {
                None => failures.push("(c) handoff: the new tab vanished before assertion".to_string()),
                Some((title, locked, parent)) => {
                    if title != "[HANDOFF] my-project" {
                        failures.push(format!(
                            "(c) handoff: the nested tab title must be '[HANDOFF] my-project', got {title:?}"
                        ));
                    }
                    if !locked {
                        failures.push("(c) handoff: the handoff tab's title must be LOCKED (title_manually_set == true)".to_string());
                    }
                    if parent.as_deref() != Some("comp-orig-tab") {
                        failures.push(format!(
                            "(c) handoff: the handoff tab must nest under the originating tab \
                             (parent_tab_id == 'comp-orig-tab'), got {parent:?}"
                        ));
                    } else {
                        eprintln!("[selftest] tranche6-composition (c): a socket `handoff` opened a nested [HANDOFF]-titled tab on the shipped window (reply ok, parented under the originating tab)");
                    }
                }
            }
        }
    }

    // --- the handoff toggle's home in the shipped settings window ---
    // ⌘, opens R23's shipped settings window (the OpenSettings non-rebindable firing
    // live); its rail exposes the Claude section (an AXButton) — the home of R26's
    // `settings.claude.installHandoffSkill` toggle. The toggle's click behaviour is
    // pinned by R26's `claude_pane` unit test + the `handoff` installer scenario; a
    // gpui toggle exposes its aria_label ("On"/"Off"), not its a11y id, as AXTitle,
    // so the shipped-surface assertion is the AX-discoverable Claude section that
    // hosts it.
    // Close any settings window a prior scenario left open so this ⌘, opens a FRESH
    // one (a clean App::windows() step-up) — the settings §6 precedent.
    if let Some(h) = cx.update(|app| current_settings_window(app)) {
        let _ = h.update(cx, |_root, window, _cx| window.remove_window());
        settle(cx, 200).await;
    }
    let _ = whandle.update(cx, |_r, window, _a| window.activate_window());
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;
    let win_before = cx.update(|app| app.windows().len());
    platform::post_key_tap(pid, KC_COMMA, platform::FLAG_COMMAND, None);
    settle(cx, 400).await;
    let settings = cx.update(|app| current_settings_window(app));
    let win_after = cx.update(|app| app.windows().len());
    if settings.is_none() || win_after != win_before + 1 {
        failures.push(format!(
            "(c) settings: ⌘, did not open R23's settings window on the shipped surface (the \
             OpenSettings non-rebindable did not fire live): settings_handle={}, App::windows() \
             {win_before} → {win_after}",
            settings.is_some()
        ));
    } else {
        // The Claude section — R26's handoff toggle ('{HANDOFF_TOGGLE_AX_ID}') home —
        // surfaces on the shipped settings rail's AX tree as an AXButton. The search
        // is scoped to the "Settings" window subtree: in the serial suite the same
        // process hosts every scenario's windows, and a "Claude"-titled node in one
        // of those (or a lingering menu) is an AXMenuItem — a whole-app walk could
        // return it first, so scope to the window actually under test.
        match poll_ax_pid_in_window(cx, pid, SETTINGS_WINDOW_AX_TITLE, CLAUDE_SECTION_AX_TITLE).await {
            Some(role) if role == CLAUDE_SECTION_AX_ROLE => {
                eprintln!(
                    "[selftest] tranche6-composition (c): ⌘, opened the shipped settings window; its \
                     rail exposes the Claude section (AXButton) — the home of the R26 handoff toggle \
                     '{HANDOFF_TOGGLE_AX_ID}'"
                );
            }
            Some(role) => failures.push(format!(
                "(c) settings: the shipped settings rail's Claude section surfaced as '{role}', not \
                 '{CLAUDE_SECTION_AX_ROLE}'"
            )),
            None => failures.push(format!(
                "(c) settings: the shipped settings window opened but its rail never exposed the \
                 Claude section (AXButton titled '{CLAUDE_SECTION_AX_TITLE}') within {POLL_TIMEOUT:?} \
                 — the R26 handoff toggle's home is not on the shipped settings surface"
            )),
        }
    }
    // Close the settings window so nothing leaks to `multiwindow`.
    if let Some(h) = cx.update(|app| current_settings_window(app)) {
        let _ = h.update(cx, |_root, window, _cx| window.remove_window());
        settle(cx, 300).await;
    }
}

// -- guarded global-HID preflight + posts ------------------------------------

/// The outcome of the guarded preflight: either our window OWNS the point (with its
/// CG-global coords, cleared to post) or the post must be DEFERRED with a reason.
enum PreflightOutcome {
    Owned((f64, f64)),
    Deferred(String),
}

/// The mandatory preflight before a guarded global-HID post: activate the app +
/// raise the window, convert the content point to CG-global, then verify our window
/// owns that point per `CGWindowListCopyWindowInfo`. Returns the global coords to
/// post at, or a DEFER reason (never a blind post).
async fn guarded_preflight(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    cx_pt: f64,
    cy_pt: f64,
) -> PreflightOutcome {
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;
    let Some((gx, gy)) = whandle
        .update(cx, |_v, w, _a| platform::content_point_to_cg_global(w, cx_pt, cy_pt))
        .ok()
        .flatten()
    else {
        return PreflightOutcome::Deferred(
            "could not convert the content point to CG-global coords — DEFERRED".to_string(),
        );
    };
    if !platform::frontmost_window_owns_point(gx, gy) {
        return PreflightOutcome::Deferred(format!(
            "the frontmost-at-point preflight FAILED — our window does not own the point \
             ({gx:.0},{gy:.0}) per CGWindowListCopyWindowInfo (another window is on top, or the \
             point is off our window). DEFERRED LOUDLY; NO global post was made. Bring the nice \
             window frontmost and re-run for the real assertion."
        ));
    }
    PreflightOutcome::Owned((gx, gy))
}

/// A guarded global-HID left drag from content `(sx, sy)` to `(ex, ey)`: preflight
/// at the start point (activate + raise + frontmost-at-point), then post DOWN →
/// interpolated DRAG steps → UP via the global HID tap. The window stays frontmost
/// through the sub-second sequence, so the start-point preflight fences the whole
/// gesture (the `update_check_live` single-preflight discipline). Returns `Owned`
/// once posted, or `Deferred` when the preflight failed (no post).
async fn guarded_drag(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    sx: f64,
    sy: f64,
    ex: f64,
    ey: f64,
) -> PreflightOutcome {
    let (gx0, gy0) = match guarded_preflight(cx, whandle, sx, sy).await {
        PreflightOutcome::Owned(g) => g,
        d @ PreflightOutcome::Deferred(_) => return d,
    };
    // The end point in CG-global (no separate preflight — the same window owns the
    // strip band; the start-point ownership fences the gesture).
    let Some((gx1, gy1)) = whandle
        .update(cx, |_v, w, _a| platform::content_point_to_cg_global(w, ex, ey))
        .ok()
        .flatten()
    else {
        return PreflightOutcome::Deferred(
            "could not convert the drag end point to CG-global coords — DEFERRED".to_string(),
        );
    };
    // NOTE (synthetic-drag limitation): a synthetic global-HID `LeftMouseDown`
    // lands (it reaches the gpui view's `mouseDown:` — the leg's landed-gate proves
    // this), but AppKit does NOT establish the implicit mouse-grab a real hardware
    // press does, so the trailing `LeftMouseDragged` events are never delivered as
    // `mouseDragged:` and R25's gpui `on_drag`/`on_drop` never arm. (Cursor-warp +
    // re-cadenced posts were both tried and neither drives the arming.) The leg
    // therefore treats a landed-press-with-no-committed-reorder as a DEFER, not a
    // failure (see `leg_b_reorder`): the deterministic reorder is hard-asserted
    // in-process by `nice-itests`. This drive is kept so that if a future
    // gpui/macOS delivers `mouseDragged:` for synthetic drags, the leg promotes to
    // a hard commit assertion with no code change.
    platform::post_global_left_down(gx0, gy0);
    settle(cx, 90).await;
    let steps = 8;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        platform::post_global_left_drag(gx0 + (gx1 - gx0) * t, gy0 + (gy1 - gy0) * t);
    }
    platform::post_global_left_up(gx1, gy1);
    PreflightOutcome::Owned((gx1, gy1))
}

// -- socket handoff drive (mirrors handoff_live) -----------------------------

#[allow(clippy::too_many_arguments)]
async fn send_handoff(
    cx: &mut AsyncApp,
    socket_path: &str,
    cwd: &str,
    handoff_file: &str,
    tab_id: &str,
    pane_id: &str,
    instructions: &str,
    model: &str,
    effort: &str,
) -> Option<String> {
    let payload = handoff_json(cwd, handoff_file, tab_id, pane_id, instructions, model, effort);
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

/// Connect, write one newline-terminated JSON payload, read the reply to EOF on a
/// dedicated thread (so the blocking read never wedges the foreground drain that
/// answers it). Mirrors `handoff_live::raw_request`.
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

#[allow(clippy::too_many_arguments)]
fn handoff_json(
    cwd: &str,
    handoff_file: &str,
    tab_id: &str,
    pane_id: &str,
    instructions: &str,
    model: &str,
    effort: &str,
) -> String {
    format!(
        "{{\"action\":\"handoff\",\"cwd\":\"{}\",\"handoffFile\":\"{}\",\"tabId\":\"{}\",\"paneId\":\"{}\",\"instructions\":\"{}\",\"model\":\"{}\",\"effort\":\"{}\"}}",
        json_escape(cwd),
        json_escape(handoff_file),
        json_escape(tab_id),
        json_escape(pane_id),
        json_escape(instructions),
        json_escape(model),
        json_escape(effort),
    )
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// -- model / poll helpers ----------------------------------------------------

fn all_tab_ids(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Vec<String> {
    state.update(cx, |s, _cx| {
        s.model
            .projects
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect()
    })
}

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

/// Seed a model-only originating Claude tab (present, non-Terminals, owning
/// `pane_id`) so the handoff resolves + nests. Mirrors `handoff_live`.
fn seed_originating_claude_tab(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
    tab_id: &str,
    pane_id: &str,
    title: &str,
) {
    let _ = state.update(cx, |s, _cx| {
        s.model.ensure_project("comp-orig-proj", "Orig", cwd);
        let mut claude = Pane::new(pane_id, "Claude", PaneKind::Claude);
        claude.is_claude_running = true;
        let mut tab = Tab::new(tab_id, title, cwd);
        tab.panes = vec![
            claude,
            Pane::new(&format!("{tab_id}-t1"), "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some(pane_id.to_string());
        tab.next_terminal_index = 2;
        if let Some(pi) = s.model.projects.iter().position(|p| p.id == "comp-orig-proj") {
            s.model.projects[pi].tabs.push(tab);
        }
        s.model.select_tab(tab_id);
    });
}

fn read_active(cx: &mut AsyncApp, toolbar: &Entity<WindowToolbarView>) -> Option<String> {
    toolbar.update(cx, |v, cx| v.active_pane_id(cx))
}

/// The on-screen content-view centre of a pill (offset-free bounds + the current
/// scroll offset), as `(x, y_from_top)` — the guarded-drag target. Mirrors
/// `pane_strip_live::read_pill_center`; in the shipped window the toolbar's pill
/// bounds are the same window-content coords `close-confirmation` clicks through.
fn read_pill_center(
    cx: &mut AsyncApp,
    toolbar: &Entity<WindowToolbarView>,
    pane_id: &str,
) -> Option<(f64, f64)> {
    toolbar.update(cx, |v, cx| {
        let b = v.scenario_pill_bounds(pane_id, cx)?;
        let off = v.scenario_scroll_offset_x();
        let x = f32::from(b.origin.x) + off + f32::from(b.size.width) / 2.0;
        let y = f32::from(b.origin.y) + f32::from(b.size.height) / 2.0;
        Some((x as f64, y as f64))
    })
}

/// Poll `pred` against the foreground `App` until true or `timeout`.
async fn poll_app(
    cx: &mut AsyncApp,
    timeout: Duration,
    pred: impl Fn(&gpui::App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cx.update(|app| pred(app)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        settle(cx, 80).await;
    }
}

/// Poll `pred` against the toolbar view until true or `timeout`.
async fn poll_view(
    cx: &mut AsyncApp,
    toolbar: &Entity<WindowToolbarView>,
    timeout: Duration,
    pred: impl Fn(&WindowToolbarView) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if toolbar.update(cx, |v, _| pred(v)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        settle(cx, 80).await;
    }
}

/// Poll the process AX tree for an element titled `title`, forcing a repaint of the
/// toolbar each tick (AccessKit lazily activates on the first query then
/// materializes a frame later — the `app-shell` precedent). Returns its role.
async fn poll_ax(
    cx: &mut AsyncApp,
    toolbar: &Entity<WindowToolbarView>,
    pid: i32,
    title: &str,
) -> Option<String> {
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        let _ = toolbar.update(cx, |_v, cx| cx.notify());
        settle(cx, 120).await;
        if let Ok(role) = platform::ax_find_titled_role(pid, title) {
            return Some(role);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

/// Poll the AX subtree of the process's window titled `window_title` for an element
/// titled `title` (no view handle to repaint — the settings window drives its own
/// frames). Scoping to one window keeps a same-titled node in another scenario's
/// window (serial suite, one process) or a lingering menu from matching first.
/// Returns its role.
async fn poll_ax_pid_in_window(
    cx: &mut AsyncApp,
    pid: i32,
    window_title: &str,
    title: &str,
) -> Option<String> {
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        settle(cx, 120).await;
        if let Ok(role) = platform::ax_find_titled_role_in_window(pid, window_title, title) {
            return Some(role);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

// -- verdict -----------------------------------------------------------------

fn build_report(failures: Vec<String>, deferred: Vec<String>) -> CadenceReport {
    if !deferred.is_empty() {
        eprintln!("[selftest] tranche6-composition DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "tranche6-composition OK (on the REAL shipped launch window): (a) a newer tag flips \
                 update_available + exposes the trailing pill as an AXButton on the shipped toolbar, \
                 and a real guarded-HID click (or the in-process fallback) shows the exact combined \
                 brew command; (b) a real guarded-HID drag committed a pill reorder on the shipped strip \
                 (hard-asserted when the drag armed, else DEFERRED — a synthetic press does not \
                 arm gpui's drag-and-drop; the deterministic reorder is pinned by nice-itests); \
                 (c) a socket `handoff` opened a nested \
                 [HANDOFF]-titled tab (reply ok) and ⌘, opened the shipped settings window exposing \
                 the Claude section (AXButton) — the R26 handoff toggle's home; {} item(s) DEFERRED \
                 to a human pass",
                deferred.len()
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} tranche6-composition assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
