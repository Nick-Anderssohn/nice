//! `app-shell` self-test scenario — the R13.5 app-shell composition gate
//! (the plan's What-to-build #3), the shipped-surface sibling of the component
//! scenarios (`chrome` / `sidebar` / `pane-strip` / `session-lifecycle`).
//!
//! Where those scenarios each mount ONE component over a hand-seeded window, this
//! one opens through the **shipped builder** — `crate::app::open_managed_window` →
//! `build_window_root` → [`AppShellView`](crate::app_shell::AppShellView), the exact
//! path `crate::app::run` and every ⌘N take — and asserts the composition the launched
//! app actually shows. A scenario that mounted its own composition would re-create
//! the blind spot R13.5 exists to close (the launched app showed only the R9 chrome
//! band over one terminal because no plan owned the shipped window's composition), so
//! going through the real builder is load-bearing, not a convenience.
//!
//! ## What it drives (against the one shipped shell window)
//!
//! 1. **The AX anchors are exposed.** An AX-tree walk of this process
//!    (`crate::platform::ax_find_titled_role`, the `ax-probe` pattern) finds the
//!    sidebar-card root (`nice-sidebar-root`) and the pane-strip root
//!    (`nice-pane-strip-root`) each exposed as an `AXGroup` — the exported
//!    shipped-surface assertion hooks (§6). The shipped shell does not drive
//!    continuous frames, so the poll forces a repaint per tick (a `WindowState`
//!    notify) to keep AccessKit's lazily-activated tree current.
//! 2. **⌘T adds a visible pill AND switches pane content.** A real ⌘T CGEvent
//!    (`CGEventPostToPid`, own pid — the same edge `multiwindow` drives) routes
//!    through the shipped keymap to the key window: the toolbar gains one laid-out
//!    pill (a *visible* pill, not just a model row), the new pane becomes active,
//!    and the [`PaneHostView`](crate::app_shell::PaneHostView) follows the switch and
//!    spawns+hosts its pty — proving the slice-2 `cx.notify()` wiring makes a
//!    window-scoped chord produce a visible result in the shipped shell.
//! 3. **The strip `+` spawns a real pty whose output renders.** Driving the real
//!    toolbar `+` seam adds a terminal pane; the pane host spawns its login shell,
//!    and a marker echoed into that pty renders back in the pane's live grid.
//! 4. **Closing the extra pane refocuses a neighbor.** The real pill-× close path
//!    removes the active extra pane from the model, the active pane refocuses to a
//!    surviving neighbor, and the pane host re-hosts that neighbor (the departed
//!    pane's view is dissolved from the composition; the neighbor stays live).
//!    Then two M2 feel-check behaviour gates ride the same window: **inline-rename
//!    focus routing** (Item D — real typed keys land in the rename field, Return
//!    commits + Escape cancels, and key focus returns to the active terminal each
//!    time) and **window resize → pty grid refit** (Item E — a real vertical frame
//!    resize re-fits the active pane's pty rows and restores them on un-resize).
//! 5. **⌘B collapses / expands the card.** A real ⌘B CGEvent — the R12 shortcut table
//!    binds *toggle-sidebar* to `cmd-b` (the plan's "⌘S" predates that table) —
//!    collapses the card, and the shell's intended leading-column width drops
//!    240 → 0 (the M2 collapsed design reserves no leading column — one
//!    full-width band; [`SidebarShellView::scenario_leading_column_width`], which
//!    re-derives that width from the collapse flag — not a laid-out `Bounds` read),
//!    and a second ⌘B restores it.
//! 6. **Teardown releases every session; the closed pane's pty is reaped.**
//!    `WindowState::teardown` clears the SessionManager's session map (asserted:
//!    every session released). It SIGHUP→SIGKILLs (via `PtyProcess::drop`, which
//!    joins the reaper — no zombie) any pane whose handle it held the *last* ref to:
//!    the closed pane, whose cached `TerminalView` the pane host already dropped, is
//!    reaped here (asserted: `kill(pid, 0)` → ESRCH). The still-*hosted* panes keep a
//!    `TerminalView` ref in the mounted `PaneHostView`, so their pty's final reap
//!    lands on window close (dropping the shell view tree) — confirmed by the external
//!    `ps` sweep (Validation), per the R3 teardown contract. Reaping a view-hosted
//!    pane inside the still-open scenario window is not possible, and the honest
//!    assertion says so.
//!
//! Self-reported ([`Gate::SelfReported`](nice_harness::selftest)): the criterion is
//! composition/model/session state, not cadence. Accessibility (TCC) is preflighted
//! and a missing grant FAILs loudly (a silently-dropped CGEvent would make ⌘T / ⌘B
//! no-ops). Registered **before** `multiwindow`: it does NOT install the
//! `WindowRegistry` close observer (`build_window_root`'s `register` uses
//! `default_global`), so closing its window never trips the quit-when-empty terminus
//! that `multiwindow` — which DOES install it — relies on being last.

use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_view::TerminalSessionHandle;
use nice_theme::chrome_geometry::SIDEBAR_DEFAULT_WIDTH;

use crate::app_shell::{AppShellView, PaneHostView, PANE_STRIP_ROOT_LABEL, SIDEBAR_ROOT_LABEL};
use crate::platform;
use crate::sidebar_shell::SidebarShellView;
use crate::toolbar::WindowToolbarView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- fixed geometry / timing -------------------------------------------------

/// ⌘T — NewTerminalPane (`CGKeyCode` for `t`).
const KC_T: u16 = 17;
/// ⌘B — ToggleSidebar (`CGKeyCode` for `b`). The R12 table binds *toggle-sidebar*
/// to `cmd-b`; the plan's "⌘S" for this step predates that binding table.
const KC_B: u16 = 11;
/// `x` — the bare printable typed into the inline-rename field (M2 Item D).
const KC_X: u16 = 7;
/// Return — commits an inline rename.
const KC_RETURN: u16 = 36;
/// Escape — cancels an inline rename (the `SidebarShell` Esc action).
const KC_ESC: u16 = 53;

/// Insert `ch` immediately before the last char of `s` (the expected result of a
/// caret-left + type in the rename cursor-editing legs). An empty string yields
/// just `ch`.
fn insert_before_last(s: &str, ch: char) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let pos = chars.len().saturating_sub(1);
    chars.insert(pos, ch);
    chars.into_iter().collect()
}

/// Vertical shrink applied for the resize→refit check (pt). Big enough to lose
/// several grid rows at any sane cell height.
const RESIZE_DY: f64 = -160.0;
/// Poll cap for the pty grid to refit after a window resize (the paint-driven
/// defer → `schedule_refit` → `resize_pty_to_fit` → `Session::resize` path,
/// M2 Item E). The budget (30 × 100 ms) comfortably covers the Swift-parity
/// 200 ms resize-debounce window the coalescer holds the apply behind.
const REFIT_POLLS: usize = 30;

/// The macOS `AXRole` a `gpui::Role::Group` maps to (accesskit_macos →
/// `NSAccessibilityGroupRole`), i.e. what the two anchors must expose as — the same
/// expectation the `ax-probe` canary asserts.
const AX_EXPECTED_ROLE: &str = "AXGroup";
/// How long to poll the AX tree for the two anchors before failing. AccessKit
/// activates lazily on the first query and the node appears a frame later; this is
/// generous headroom over that latency (matching the `ax-probe` timeout).
const AX_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll cap for a model mutation to produce a hosted+spawned pty (the pane host's
/// activate-on-next-render → deferred spawn), on the real pty clock.
const SPAWN_POLLS: usize = 40;
/// Poll cap for a login shell to echo the strip-`+` marker back into its grid — a
/// real `zsh -il` sourcing the user's rc, so generous.
const GRID_POLLS: usize = 80;
/// Interval between polls (real wall-clock; the pty child runs on OS threads).
const POLL_MS: u64 = 100;
/// Tolerance (pt) for the ⌘B leading-column geometry comparisons.
const GEOM_EPS: f32 = 4.0;
/// The marker echoed into the strip-`+` pane's pty; distinctive enough that a login
/// shell's own rc output can't spoof it.
const STRIP_MARKER: &str = "NICERS__APPSHELL__STRIP__OK";

/// Accessibility-grant remediation, shared verbatim with the other CGEvent
/// scenarios: without the TCC grant `CGEventPostToPid` is silently dropped, so the
/// injected ⌘T / ⌘B are no-ops and the scenario can never pass.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected chord can reach the window. \
Fix: System Settings → Privacy & Security → Accessibility → enable the process \
hosting this run. If it shows ON but this persists, the grant is STALE — remove \
it with '-' and re-add it, then re-run. Verify: swift -e 'import \
ApplicationServices; print(AXIsProcessTrusted())'";

// ===========================================================================
// scenario wiring
// ===========================================================================

/// Open the `app-shell` scenario window through the SHIPPED builder and spawn its
/// driver (self-reported gate). Installs the shipped shortcut keymap first (so the
/// ⌘T / ⌘B CGEvents route through the real action system — idempotent, an earlier
/// suite scenario may already have installed it), then opens exactly as
/// `crate::app::run` does. It does **not** install the `WindowRegistry` close
/// observer (see the module docs).
pub fn open_app_shell_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        crate::keymap::install_shortcuts(app);
        crate::app::open_managed_window(app)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_app_shell(acx, whandle).await;
        eprintln!("[selftest] scenario 'app-shell': {}", report.detail);
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

/// Post one key tap (with `flags`) to our own pid, then yield so AppKit dispatches
/// it into the key window before the next event.
async fn tap(cx: &mut AsyncApp, pid: i32, keycode: u16, flags: u64) {
    platform::post_key_tap(pid, keycode, flags, None);
    settle(cx, 120).await;
}

/// Re-assert frontmost/key right before a chord so a posted CGEvent routes to this
/// window's gpui action dispatch (the `multiwindow` re-key pattern).
async fn rekey(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>) {
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_app_shell(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>) -> CadenceReport {
    // Frontmost/key + painted once (registers input handlers, first AccessKit-eligible
    // frame) before any event.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Accessibility preflight — FAIL loudly (never silently skip the CGEvent half).
    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    rekey(cx, whandle).await;

    // Resolve the shipped shell + its per-window state (registered by
    // `build_window_root`). The shell hands back the SAME sidebar/toolbar it renders.
    let shell = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => return CadenceReport::error(format!("app-shell: could not read the shell view: {e}")),
    };
    let (sidebar, toolbar) =
        shell.update(cx, |s, _| (s.scenario_sidebar(), s.scenario_toolbar()));
    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "app-shell: the shipped builder did not register the window's WindowState".to_string(),
        );
    };

    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();

    let Some(main_tab) = active_tab_id(cx, &state) else {
        return CadenceReport::error("app-shell: the shipped window has no active tab".to_string());
    };

    // 1. The two shipped-surface AX anchors are exposed as AXGroup.
    ax_anchor_checks(cx, &state, pid, &mut failures).await;

    // 2. ⌘T adds a visible pill AND switches pane content (real chord).
    cmd_t_checks(cx, whandle, &toolbar, &state, &main_tab, pid, &mut failures).await;

    // 3. The strip `+` spawns a real pty whose output renders.
    strip_add_checks(cx, &toolbar, &state, &main_tab, &mut failures).await;

    // 4. Closing the extra pane refocuses a neighbor (the pane host re-hosts it) —
    //    returns the closed pane's pty pid: the pane host drops its view on close, so
    //    that pty's only remaining ref is the SessionManager's, which teardown reaps.
    let closed_pid =
        close_pane_checks(cx, whandle, &toolbar, &state, &main_tab, &mut failures).await;

    // 4.5 Inline-rename focus routing (M2 Item D): keys land in the rename
    //     field, Enter commits + returns focus to the terminal, Escape cancels
    //     the sidebar tab rename + returns focus.
    rename_focus_checks(cx, whandle, &shell, &toolbar, &sidebar, &state, pid, &mut failures).await;

    // 4.75 Window resize → pty grid refit (M2 Item E): a vertical window resize
    //      re-fits the active pane's pty grid (rows track the window; cols hold).
    resize_refit_checks(cx, whandle, &state, &main_tab, &mut failures).await;

    // 5. ⌘B collapses / expands the card (geometry read) — last, so the AX
    //    assertions above ran while the card (and its anchor) was expanded.
    cmd_b_checks(cx, whandle, &sidebar, pid, &mut failures).await;

    // 6. Teardown releases every session; the closed pane's pty is reaped.
    teardown_checks(cx, &state, &main_tab, closed_pid, &mut failures).await;

    build_report(failures)
}

// ---- 1. AX anchors ---------------------------------------------------------

/// Poll the process AX tree until BOTH shipped anchors surface as `AXGroup`, or time
/// out. The shipped shell doesn't RAF, so each tick forces a repaint (a `WindowState`
/// notify → the observing shell views re-render → the frontmost window presents) so
/// AccessKit — lazily activated by the first query here — can build/refresh its tree.
async fn ax_anchor_checks(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    pid: i32,
    failures: &mut Vec<String>,
) {
    let deadline = Instant::now() + AX_TIMEOUT;
    let mut found_sidebar = false;
    let mut found_strip = false;
    let mut last_sidebar = "AX tree never exposed it".to_string();
    let mut last_strip = "AX tree never exposed it".to_string();

    while Instant::now() < deadline && !(found_sidebar && found_strip) {
        // Force a fresh frame (the AX walk below lazily activates AccessKit; the node
        // then materializes on a later frame). The query MUST run on this main-thread
        // task — a same-process AX query dispatches inline, but a background query
        // would race gpui's per-frame RefCell borrow (the `ax-probe` finding).
        let _ = state.update(cx, |_s, cx| cx.notify());
        settle(cx, 150).await;

        if !found_sidebar {
            match platform::ax_find_titled_role(pid, SIDEBAR_ROOT_LABEL) {
                Ok(role) if role == AX_EXPECTED_ROLE => found_sidebar = true,
                Ok(role) => last_sidebar = format!("exposed but role '{role}' != '{AX_EXPECTED_ROLE}'"),
                Err(e) => last_sidebar = e,
            }
        }
        if !found_strip {
            match platform::ax_find_titled_role(pid, PANE_STRIP_ROOT_LABEL) {
                Ok(role) if role == AX_EXPECTED_ROLE => found_strip = true,
                Ok(role) => last_strip = format!("exposed but role '{role}' != '{AX_EXPECTED_ROLE}'"),
                Err(e) => last_strip = e,
            }
        }
    }

    if found_sidebar {
        eprintln!("[selftest] app-shell AX: sidebar root '{SIDEBAR_ROOT_LABEL}' exposed as {AX_EXPECTED_ROLE}");
    } else {
        failures.push(format!(
            "AX: sidebar-card root anchor '{SIDEBAR_ROOT_LABEL}' not exposed as {AX_EXPECTED_ROLE}: {last_sidebar}"
        ));
    }
    if found_strip {
        eprintln!("[selftest] app-shell AX: pane-strip root '{PANE_STRIP_ROOT_LABEL}' exposed as {AX_EXPECTED_ROLE}");
    } else {
        failures.push(format!(
            "AX: pane-strip root anchor '{PANE_STRIP_ROOT_LABEL}' not exposed as {AX_EXPECTED_ROLE}: {last_strip}"
        ));
    }
}

// ---- 2. ⌘T adds a visible pill + switches content --------------------------

async fn cmd_t_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    state: &Entity<WindowState>,
    tab: &str,
    pid: i32,
    failures: &mut Vec<String>,
) {
    rekey(cx, whandle).await;

    let pills_before = toolbar_pane_ids(cx, toolbar);
    let active_before = toolbar_active(cx, toolbar);

    tap(cx, pid, KC_T, platform::FLAG_COMMAND).await;
    settle(cx, 400).await;

    let pills_after = toolbar_pane_ids(cx, toolbar);
    let active_after = toolbar_active(cx, toolbar);

    let Some(new_pill) = pills_after.iter().find(|p| !pills_before.contains(p)).cloned() else {
        failures.push(format!(
            "⌘T: pill count {}→{} — no new pane pill (did the chord route to the shipped key window?)",
            pills_before.len(),
            pills_after.len()
        ));
        return;
    };
    if pills_after.len() != pills_before.len() + 1 {
        failures.push(format!(
            "⌘T: pill count {}→{} (expected exactly one new pill)",
            pills_before.len(),
            pills_after.len()
        ));
    }
    // Content switched: the new pane is active (and it changed).
    if active_after.as_deref() != Some(new_pill.as_str()) || active_after == active_before {
        failures.push(format!(
            "⌘T: added pill {new_pill} but active pane is {active_after:?} (was {active_before:?}) — \
             pane content did not switch to the new pane"
        ));
    }
    // A VISIBLE pill: laid out on screen, not just a model row.
    let visible = toolbar.update(cx, |v, cx| v.scenario_pill_bounds(&new_pill, cx).is_some());
    if !visible {
        failures.push(format!(
            "⌘T: new pane {new_pill} has no laid-out pill bounds — present in the model but not rendered as a visible pill"
        ));
    }
    // The pane host followed the switch and spawned+hosted the new pane's pty.
    if poll_pane_spawned(cx, state, tab, &new_pill).await {
        eprintln!("[selftest] app-shell ⌘T: added visible pill {new_pill}, active + hosted by the pane host");
    } else {
        failures.push(format!(
            "⌘T: the pane host did not spawn+host the new active pane {new_pill} — the composition did not follow the active-pane switch"
        ));
    }
}

// ---- 3. strip + spawns a real pty whose output renders ---------------------

async fn strip_add_checks(
    cx: &mut AsyncApp,
    toolbar: &Entity<WindowToolbarView>,
    state: &Entity<WindowState>,
    tab: &str,
    failures: &mut Vec<String>,
) {
    let pills_before = toolbar_pane_ids(cx, toolbar);
    // Drive the real toolbar `+` seam (not a shortcut) — the shipped strip add path.
    let _ = toolbar.update(cx, |v, cx| v.drive_add_terminal_pane(cx));
    settle(cx, 400).await;

    let pills_after = toolbar_pane_ids(cx, toolbar);
    let Some(new_pill) = pills_after.iter().find(|p| !pills_before.contains(p)).cloned() else {
        failures.push("strip-+: no new pane pill after the toolbar + add".to_string());
        return;
    };

    // The pane host spawns the new active pane's pty (deferred-spawn on activation).
    if !poll_pane_spawned(cx, state, tab, &new_pill).await {
        failures.push(format!(
            "strip-+: the pane host did not spawn+host pane {new_pill} — a real pty did not fork behind the strip +"
        ));
        return;
    }
    let Some(handle) = pane_handle(cx, state, tab, &new_pill) else {
        failures.push(format!("strip-+: pane {new_pill} spawned but its session handle vanished"));
        return;
    };

    // Its output renders: echo a marker into the pty and poll the live grid for it.
    let echo = format!("echo {STRIP_MARKER}\n");
    let _ = handle.update(cx, |h, _| {
        let _ = h.session().write_input(echo.as_bytes());
    });
    let mut rendered = false;
    for _ in 0..GRID_POLLS {
        settle(cx, POLL_MS).await;
        let grid = handle.update(cx, |h, _| h.session().grid_lines().join("\n"));
        if grid.contains(STRIP_MARKER) {
            rendered = true;
            break;
        }
    }
    if rendered {
        eprintln!("[selftest] app-shell strip-+: pane {new_pill} spawned a real pty and its output rendered in the grid");
    } else {
        failures.push(format!(
            "strip-+: pane {new_pill}'s pty never rendered the '{STRIP_MARKER}' marker into its grid (login shell did not come up / echo)"
        ));
    }
}

// ---- 4. closing the extra pane refocuses a neighbor ------------------------

async fn close_pane_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    state: &Entity<WindowState>,
    tab: &str,
    failures: &mut Vec<String>,
) -> Option<i32> {
    let pills = toolbar_pane_ids(cx, toolbar);
    if pills.len() < 2 {
        failures.push(format!(
            "close-pane: only {} pane(s) on the active tab — need ≥2 to test neighbor refocus",
            pills.len()
        ));
        return None;
    }
    let Some(closed) = toolbar_active(cx, toolbar) else {
        failures.push("close-pane: no active pane to close".to_string());
        return None;
    };
    // The closed pane's pty pid, read while its session is live — the teardown reap
    // check verifies this one (the pane host drops its cached view on close, so after
    // teardown the SessionManager's is the pty's LAST ref and drop reaps it).
    let closed_pid = pane_handle(cx, state, tab, &closed).and_then(|h| handle_pid(cx, &h));

    // Close the active extra pane through the real pill-× path.
    let closed_c = closed.clone();
    let _ = whandle.update(cx, |_root, window, app| {
        toolbar.update(app, |v, cx| v.drive_close_pane(&closed_c, window, cx))
    });
    settle(cx, 400).await;

    let pills_after = toolbar_pane_ids(cx, toolbar);
    if pills_after.contains(&closed) {
        failures.push(format!("close-pane: {closed} is still in the strip after close"));
        return closed_pid;
    }
    if pills_after.len() != pills.len() - 1 {
        failures.push(format!(
            "close-pane: pill count {}→{} (expected -1)",
            pills.len(),
            pills_after.len()
        ));
    }
    // Refocus landed on a surviving neighbor, and the pane host re-hosts it.
    match toolbar_active(cx, toolbar) {
        Some(a) if a != closed && pills_after.contains(&a) => {
            if poll_pane_spawned(cx, state, tab, &a).await {
                eprintln!("[selftest] app-shell close-pane: closed {closed}, refocused neighbor {a}, still hosted");
            } else {
                failures.push(format!(
                    "close-pane: refocused to {a} but the pane host holds no live session for it"
                ));
            }
        }
        other => failures.push(format!(
            "close-pane: after closing {closed} the active pane is {other:?} — expected a surviving neighbor"
        )),
    }
    closed_pid
}

// ---- 4.5 inline-rename focus routing (M2 Item D) ----------------------------

/// Drives the SHIPPED rename paths with real CGEvent keys against the real key
/// window:
///
/// * pill rename: `drive_begin_rename` (the gate-passed title-tap /
///   context-menu entry path) → a typed `x` lands in the FIELD (not the pty) →
///   Return commits the model title → key focus returns to the active terminal;
/// * sidebar tab rename: `drive_begin_tab_rename` → typed `x` lands in the
///   field → Escape (the `SidebarShell` Esc action) cancels, title unchanged →
///   key focus returns to the active terminal.
///
/// Pre-M2 the terminal re-grabbed focus every frame, so the field lost focus the
/// same frame it was focused and every key hit the pty — this check pins the
/// focus-once + explicit-routing fix.
async fn rename_focus_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    shell: &Entity<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    sidebar: &Entity<SidebarShellView>,
    state: &Entity<WindowState>,
    pid: i32,
    failures: &mut Vec<String>,
) {
    rekey(cx, whandle).await;
    let pane_host = shell.update(cx, |s, _| s.scenario_pane_host());
    let Some(tab) = active_tab_id(cx, state) else {
        failures.push("rename-focus: no active tab".to_string());
        return;
    };

    // Baseline: the pane host routed key focus to the active terminal.
    if active_terminal_focused(cx, whandle, &pane_host) != Some(true) {
        failures.push(
            "rename-focus: the active terminal does not hold key focus before the rename — \
             activation focus routing (PaneHostView) is not working"
                .to_string(),
        );
        return;
    }

    // --- pill rename: type + Enter-commit + refocus ---
    let _ = whandle.update(cx, |_r, window, app| {
        toolbar.update(app, |v, cx| v.drive_begin_rename(window, cx))
    });
    settle(cx, 200).await;
    let (editing, field_focused) = whandle
        .update(cx, |_r, window, app| {
            toolbar.update(app, |v, _| {
                (v.scenario_rename_editing(), v.scenario_rename_focused(window))
            })
        })
        .unwrap_or((false, false));
    if !editing || !field_focused {
        failures.push(format!(
            "rename-focus: after begin-rename the pill field is editing={editing} focused={field_focused} \
             (expected both true — the terminal must not steal focus back)"
        ));
        return;
    }

    let draft_before = toolbar.update(cx, |v, _| v.scenario_rename_draft());
    // BUG A: the whole title must be preselected on entry (a pane title is not a
    // filename — the entire name is the replace target), so the first keystroke
    // replaces it rather than appending.
    let sel_on_entry = toolbar.update(cx, |v, _| v.scenario_rename_selection());
    let want_sel = Some((0, draft_before.chars().count()));
    if sel_on_entry != want_sel {
        failures.push(format!(
            "rename-focus: pill rename must preselect the whole title {want_sel:?} on entry, got \
             {sel_on_entry:?} — the first keystroke would append instead of replace (BUG A)"
        ));
    }
    tap(cx, pid, KC_X, 0).await;
    let draft_after = toolbar.update(cx, |v, _| v.scenario_rename_draft());
    // With the whole title preselected, the typed 'x' REPLACES it (draft becomes
    // "x"), not appends.
    if draft_after != "x" {
        failures.push(format!(
            "rename-focus: typed 'x' over the preselected title '{draft_before}' should replace it \
             ('x'), got '{draft_after}' — the key did not land / the preselection was lost"
        ));
    }

    // Cursor editing: move the caret one char left, then type via a real key
    // event — it must INSERT at the caret (mid-string), not append. Proves the
    // pill's field is the cursor-capable editor (arrows + mid-string insert), not
    // the old append-only field.
    let mid_before = draft_after.clone();
    let _ = whandle.update(cx, |_r, _w, app| {
        toolbar.update(app, |v, cx| v.drive_rename_arrow(false, cx))
    });
    tap(cx, pid, KC_X, 0).await;
    let draft_after = toolbar.update(cx, |v, _| v.scenario_rename_draft());
    let expected_mid = insert_before_last(&mid_before, 'x');
    if draft_after != expected_mid {
        failures.push(format!(
            "rename cursor: after ←+type 'x' the pill draft should insert mid-string ('{expected_mid}'), \
             got '{draft_after}' — the caret did not move / insert at position"
        ));
    }

    tap(cx, pid, KC_RETURN, 0).await;
    settle(cx, 200).await;
    if toolbar.update(cx, |v, _| v.scenario_rename_editing()) {
        failures.push("rename-focus: Return did not commit the pill rename".to_string());
    }
    let committed = state.update(cx, |s, _| {
        s.model.tab_for(&tab).and_then(|t| {
            let pid = t.active_pane_id.as_deref()?;
            t.panes.iter().find(|p| p.id == pid).map(|p| p.title.clone())
        })
    });
    if committed.as_deref() != Some(draft_after.as_str()) {
        failures.push(format!(
            "rename-focus: committed title is {committed:?}, expected '{draft_after}'"
        ));
    }
    if active_terminal_focused(cx, whandle, &pane_host) != Some(true) {
        failures.push(
            "rename-focus: key focus did not return to the active terminal after the Enter commit"
                .to_string(),
        );
    } else {
        eprintln!(
            "[selftest] app-shell rename-focus: pill rename — typed key landed in the field, \
             Return committed '{draft_after}', focus returned to the terminal"
        );
    }

    // --- pill rename: Escape-cancel + refocus (the toolbar's own owner
    //     binding — the sidebar's Esc is the shell action, tested below) ---
    let title_before = state.update(cx, |s, _| {
        s.model.tab_for(&tab).and_then(|t| {
            let pid = t.active_pane_id.as_deref()?;
            t.panes.iter().find(|p| p.id == pid).map(|p| p.title.clone())
        })
    });
    let _ = whandle.update(cx, |_r, window, app| {
        toolbar.update(app, |v, cx| v.drive_begin_rename(window, cx))
    });
    settle(cx, 200).await;
    tap(cx, pid, KC_X, 0).await;
    tap(cx, pid, KC_ESC, 0).await;
    settle(cx, 200).await;
    if toolbar.update(cx, |v, _| v.scenario_rename_editing()) {
        failures.push("rename-focus: Escape did not cancel the pill rename".to_string());
    }
    let title_now = state.update(cx, |s, _| {
        s.model.tab_for(&tab).and_then(|t| {
            let pid = t.active_pane_id.as_deref()?;
            t.panes.iter().find(|p| p.id == pid).map(|p| p.title.clone())
        })
    });
    if title_now != title_before {
        failures.push(format!(
            "rename-focus: pill Escape cancel changed the title {title_before:?} → {title_now:?}"
        ));
    }
    if active_terminal_focused(cx, whandle, &pane_host) != Some(true) {
        failures.push(
            "rename-focus: key focus did not return to the terminal after the pill Escape cancel"
                .to_string(),
        );
    } else {
        eprintln!(
            "[selftest] app-shell rename-focus: pill rename — Escape cancelled (title unchanged), \
             focus returned to the terminal"
        );
    }

    // --- sidebar tab rename: type + Escape-cancel + refocus ---
    let title_before = state.update(cx, |s, _| s.model.tab_for(&tab).map(|t| t.title.clone()));
    let _ = whandle.update(cx, |_r, window, app| {
        sidebar.update(app, |v, cx| v.drive_begin_tab_rename(window, cx))
    });
    settle(cx, 200).await;
    let (editing, field_focused) = whandle
        .update(cx, |_r, window, app| {
            sidebar.update(app, |v, _| {
                (v.scenario_tab_rename_editing(), v.scenario_tab_rename_focused(window))
            })
        })
        .unwrap_or((false, false));
    if !editing || !field_focused {
        failures.push(format!(
            "rename-focus: after begin-tab-rename the sidebar field is editing={editing} \
             focused={field_focused} (expected both true)"
        ));
        return;
    }
    let draft_before = sidebar.update(cx, |v, _| v.scenario_tab_rename_draft());
    // BUG A: the whole tab title must be preselected on entry, so the first
    // keystroke replaces it (a tab title is not a filename).
    let sel_on_entry = sidebar.update(cx, |v, _| v.scenario_tab_rename_selection());
    let want_sel = Some((0, draft_before.chars().count()));
    if sel_on_entry != want_sel {
        failures.push(format!(
            "rename-focus: sidebar tab rename must preselect the whole title {want_sel:?} on entry, \
             got {sel_on_entry:?} — the first keystroke would append instead of replace (BUG A)"
        ));
    }
    tap(cx, pid, KC_X, 0).await;
    let draft_after = sidebar.update(cx, |v, _| v.scenario_tab_rename_draft());
    // Preselected whole title → the typed 'x' REPLACES it ("x"), not appends.
    if draft_after != "x" {
        failures.push(format!(
            "rename-focus: typed 'x' over the preselected tab title '{draft_before}' should replace it \
             ('x'), got '{draft_after}'"
        ));
    }

    // Cursor editing: ←, then type via a real key event — insert at the caret
    // (mid-string), proving the sidebar tab field is cursor-capable too.
    let mid_before = draft_after.clone();
    let _ = whandle.update(cx, |_r, _w, app| {
        sidebar.update(app, |v, cx| v.drive_tab_rename_arrow(false, cx))
    });
    tap(cx, pid, KC_X, 0).await;
    let mid_after = sidebar.update(cx, |v, _| v.scenario_tab_rename_draft());
    let expected_mid = insert_before_last(&mid_before, 'x');
    if mid_after != expected_mid {
        failures.push(format!(
            "rename cursor: after ←+type 'x' the tab draft should insert mid-string ('{expected_mid}'), \
             got '{mid_after}'"
        ));
    } else {
        eprintln!(
            "[selftest] app-shell rename cursor: pill + sidebar-tab fields insert mid-string after ← \
             (cursor-capable editor)"
        );
    }

    tap(cx, pid, KC_ESC, 0).await;
    settle(cx, 200).await;
    if sidebar.update(cx, |v, _| v.scenario_tab_rename_editing()) {
        failures.push(
            "rename-focus: Escape did not cancel the sidebar tab rename (is the SidebarShell Esc \
             binding installed?)"
                .to_string(),
        );
    }
    let title_after = state.update(cx, |s, _| s.model.tab_for(&tab).map(|t| t.title.clone()));
    if title_after != title_before {
        failures.push(format!(
            "rename-focus: Escape cancel changed the tab title {title_before:?} → {title_after:?}"
        ));
    }
    if active_terminal_focused(cx, whandle, &pane_host) != Some(true) {
        failures.push(
            "rename-focus: key focus did not return to the active terminal after the Escape cancel"
                .to_string(),
        );
    } else {
        eprintln!(
            "[selftest] app-shell rename-focus: sidebar tab rename — typed key landed in the field, \
             Escape cancelled (title unchanged), focus returned to the terminal"
        );
    }
}

/// Whether the pane host's active terminal holds key focus right now.
fn active_terminal_focused(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    pane_host: &Entity<PaneHostView>,
) -> Option<bool> {
    whandle
        .update(cx, |_r, window, app| {
            pane_host
                .read(app)
                .active_terminal_focus_handle(app)
                .map(|fh| fh.is_focused(window))
        })
        .ok()
        .flatten()
}

// ---- 4.75 window resize → pty grid refit (M2 Item E) ------------------------

/// Shrinks the shipped window 160pt vertically and asserts the ACTIVE pane's
/// pty grid loses rows (cols hold — the width didn't change), then restores the
/// frame and asserts the rows come back. This pins the paint-driven refit
/// wiring (`TerminalElement` bounds delta → deferred `schedule_refit`, which
/// coalesces behind the Swift-parity 200 ms resize debounce →
/// `resize_pty_to_fit` → `Session::resize` → TIOCSWINSZ/SIGWINCH); pre-M2 the
/// grid stayed at its spawn size forever.
async fn resize_refit_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    tab: &str,
    failures: &mut Vec<String>,
) {
    // The raw content-view pointer: the resize must be issued OUTSIDE any gpui
    // update (no App borrow outstanding) or gpui never processes the new
    // viewport on this pin — see `platform::resize_window_by`'s note. The
    // CGEvent posts follow the same pattern.
    let Some(ns_view) = whandle
        .update(cx, |_r, w, _a| platform::ns_view_of(w) as usize)
        .ok()
        .filter(|p| *p != 0)
    else {
        failures.push("resize-refit: could not resolve the window's NSView".to_string());
        return;
    };
    let Some(pane) = state.update(cx, |s, _| {
        s.model.tab_for(tab).and_then(|t| t.active_pane_id.clone())
    }) else {
        failures.push("resize-refit: no active pane".to_string());
        return;
    };
    let Some(handle) = pane_handle(cx, state, tab, &pane) else {
        failures.push("resize-refit: the active pane has no live session".to_string());
        return;
    };
    let dims = |cx: &mut AsyncApp, h: &Entity<TerminalSessionHandle>| {
        h.update(cx, |h, _| h.session().dimensions())
    };
    let Some((rows0, cols0)) = dims(cx, &handle) else {
        failures.push("resize-refit: the active pane's pty has no dimensions".to_string());
        return;
    };

    // Shrink 160pt vertically → the refit grid must LOSE rows. Each
    // poll forces a fresh frame (a `WindowState` notify → the shell re-renders →
    // the frontmost window presents — the same nudge the AX poll uses): the
    // refit trigger is paint-driven, and a programmatic `setFrame` on this
    // otherwise-idle, non-RAF window does not by itself present a frame (an
    // interactive user resize paints continuously, so the live app needs no
    // such nudge).
    platform::resize_window_ptr_by(ns_view, 0.0, RESIZE_DY);
    let mut shrunk: Option<(u16, u16)> = None;
    for _ in 0..REFIT_POLLS {
        let _ = state.update(cx, |_s, cx| cx.notify());
        settle(cx, POLL_MS).await;
        if let Some((r, c)) = dims(cx, &handle) {
            if r < rows0 {
                shrunk = Some((r, c));
                break;
            }
        }
    }
    match shrunk {
        Some((r, c)) => {
            if c != cols0 {
                failures.push(format!(
                    "resize-refit: a pure vertical shrink changed cols {cols0}→{c} (rows {rows0}→{r})"
                ));
            } else {
                eprintln!(
                    "[selftest] app-shell resize-refit: 160pt vertical shrink refit the pty \
                     {rows0}×{cols0} → {r}×{c}"
                );
            }
        }
        None => {
            failures.push(format!(
                "resize-refit: pty grid stayed {rows0}×{cols0} after a 160pt vertical shrink — \
                 the resize→refit wiring (M2 Item E) did not fire"
            ));
            // Still restore the frame below so later checks see the original size.
        }
    }

    // Restore the frame → the rows must come back to the original fit (same
    // per-poll repaint nudge as the shrink leg).
    platform::resize_window_ptr_by(ns_view, 0.0, -RESIZE_DY);
    let mut restored = false;
    for _ in 0..REFIT_POLLS {
        let _ = state.update(cx, |_s, cx| cx.notify());
        settle(cx, POLL_MS).await;
        if dims(cx, &handle) == Some((rows0, cols0)) {
            restored = true;
            break;
        }
    }
    if restored {
        eprintln!("[selftest] app-shell resize-refit: frame restored, pty refit back to {rows0}×{cols0}");
    } else {
        failures.push(format!(
            "resize-refit: after restoring the frame the pty did not return to {rows0}×{cols0} \
             (got {:?})",
            dims(cx, &handle)
        ));
    }
}

// ---- 5. ⌘B collapses / expands the card (geometry read) --------------------

async fn cmd_b_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    sidebar: &Entity<SidebarShellView>,
    pid: i32,
    failures: &mut Vec<String>,
) {
    rekey(cx, whandle).await;

    let (collapsed0, width0) = sidebar_geom(cx, sidebar);
    if collapsed0 {
        failures.push("⌘B: sidebar started collapsed (expected the expanded default)".to_string());
        return;
    }
    if (width0 - SIDEBAR_DEFAULT_WIDTH).abs() > GEOM_EPS {
        failures.push(format!(
            "⌘B: expanded card leading column is {width0:.1}pt, expected the {SIDEBAR_DEFAULT_WIDTH} default"
        ));
    }

    // ⌘B collapses.
    tap(cx, pid, KC_B, platform::FLAG_COMMAND).await;
    settle(cx, 350).await;
    let (collapsed1, width1) = sidebar_geom(cx, sidebar);
    if !collapsed1 {
        failures.push(
            "⌘B: the card did not collapse — the toggle-sidebar chord did not reach the shipped shell (or nothing re-rendered)".to_string(),
        );
    } else if width1.abs() > GEOM_EPS {
        failures.push(format!(
            "⌘B: collapsed flag set but the intended leading-column width is {width1:.1}pt — the M2 \
             collapsed design reserves NO leading column (expected 0)"
        ));
    } else {
        eprintln!("[selftest] app-shell ⌘B: card collapsed, leading column {width0:.1}→{width1:.1}pt");
    }

    // ⌘B again expands.
    tap(cx, pid, KC_B, platform::FLAG_COMMAND).await;
    settle(cx, 350).await;
    let (collapsed2, width2) = sidebar_geom(cx, sidebar);
    if collapsed2 {
        failures.push("⌘B: the card did not expand on the second toggle".to_string());
    } else if (width2 - width0).abs() > GEOM_EPS {
        failures.push(format!(
            "⌘B: expanded, but the leading column restored to {width2:.1}pt, not the {width0:.1}pt it collapsed from"
        ));
    } else {
        eprintln!("[selftest] app-shell ⌘B: card expanded back to {width2:.1}pt");
    }
}

// ---- 6. teardown leaves no orphaned shells ---------------------------------

async fn teardown_checks(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    main_tab: &str,
    closed_pid: Option<i32>,
    failures: &mut Vec<String>,
) {
    // The (tab, pane) set the SessionManager holds sessions for right now.
    let live_before: Vec<(String, String)> = state.update(cx, |s, _| {
        let mut v = Vec::new();
        for project in &s.model.projects {
            for tab in &project.tabs {
                for pane in &tab.panes {
                    if s.session.has_pane(&tab.id, &pane.id) {
                        v.push((tab.id.clone(), pane.id.clone()));
                    }
                }
            }
        }
        v
    });

    // Drop every session. `SessionManager::teardown` clears the whole session map,
    // releasing the manager's ref to each pane's `TerminalSessionHandle` and
    // SIGHUP→SIGKILLing (via `PtyProcess::drop`) any pane whose handle it held the
    // LAST ref to — i.e. a pane the mounted `PaneHostView` is no longer hosting (the
    // closed pane, whose cached `TerminalView` the host already dropped). The panes
    // the host still hosts keep a `TerminalView` ref, so teardown releases the
    // manager's ref but the pty's final reap lands on window close (dropping the
    // shell view tree) — verified by the external `ps` sweep, per the R3 teardown
    // contract. So: assert the manager released every session (in-scenario), and that
    // the closed pane's pty — teardown's to reap — is gone at the OS level.
    let _ = state.update(cx, |s, _| s.teardown());

    // (a) The manager released every session it held.
    let leftover = state.update(cx, |s, _| {
        live_before
            .iter()
            .filter(|(t, p)| s.session.has_pane(t, p))
            .count()
    });
    if leftover > 0 {
        failures.push(format!(
            "teardown: {leftover} SessionManager session(s) survived WindowState::teardown"
        ));
    }

    // (b) The closed pane's pty — whose view the host dropped on close, so teardown
    //     held its last ref — is OS-reaped. `PtyProcess::drop` joins the reaper (no
    //     zombie), so checking immediately (no settle) keeps the pid-reuse window at
    //     microseconds. This is the genuine teardown→reap OS proof.
    if let Some(cp) = closed_pid {
        if !process_gone(cp) {
            failures.push(format!(
                "teardown: the closed pane's pty (pid {cp}) is still alive after WindowState::teardown — teardown did not reap a released shell"
            ));
        }
    }

    // (c) Teardown drops sessions, never the model tree — the Main tab still exists.
    let main_present = state.update(cx, |s, _| s.model.tab_for(main_tab).is_some());
    if !main_present {
        failures.push("teardown: the Main tab vanished from the model (teardown must not touch the tree)".to_string());
    }

    if failures.is_empty() {
        eprintln!(
            "[selftest] app-shell teardown: released all {n} session(s); closed pane pty reaped \
             (the {n} still-hosted pane pty(ies) reap on window close — external ps sweep)",
            n = live_before.len()
        );
    }
}

/// `kill(pid, 0)` probes existence without signalling: 0 → alive (incl. a zombie,
/// which our reaper-joining drop precludes), any error (ESRCH) → gone.
fn process_gone(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) != 0 }
}

// ---- shared reads ----------------------------------------------------------

// `Entity::update` under `AsyncApp` returns the closure's value directly (it panics
// only if the entity is gone — impossible here, the shell/state own live windows).

fn active_tab_id(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Option<String> {
    state.update(cx, |s, _| s.model.active_tab_id().map(str::to_string))
}

fn pane_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab: &str,
    pane: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    state.update(cx, |s, _| s.session.pane_handle(tab, pane))
}

fn handle_pid(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> Option<i32> {
    handle.update(cx, |h, _| h.session().child_pid())
}

/// Poll until `pane` on `tab` has a live pty session (the pane host's activate →
/// deferred-spawn), or the cap elapses.
async fn poll_pane_spawned(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab: &str,
    pane: &str,
) -> bool {
    for _ in 0..SPAWN_POLLS {
        if state.update(cx, |s, _| s.session.has_pane(tab, pane)) {
            return true;
        }
        settle(cx, POLL_MS).await;
    }
    false
}

fn toolbar_pane_ids(cx: &mut AsyncApp, toolbar: &Entity<WindowToolbarView>) -> Vec<String> {
    toolbar.update(cx, |v, cx| v.pane_ids(cx))
}

fn toolbar_active(cx: &mut AsyncApp, toolbar: &Entity<WindowToolbarView>) -> Option<String> {
    toolbar.update(cx, |v, cx| v.active_pane_id(cx))
}

fn sidebar_geom(cx: &mut AsyncApp, sidebar: &Entity<SidebarShellView>) -> (bool, f32) {
    sidebar.update(cx, |v, cx| (v.is_collapsed(cx), v.scenario_leading_column_width(cx)))
}

// ---- verdict ---------------------------------------------------------------

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "app-shell OK (through the shipped builder): both AX anchors exposed as AXGroup, \
                     ⌘T added a visible pill + switched pane content, the strip + spawned a real pty \
                     whose output rendered, closing the extra pane refocused a live neighbor, inline \
                     rename routed real keys to the field (Return committed / Escape cancelled, focus \
                     returned to the terminal both times), a real vertical resize refit the pty grid \
                     and back, ⌘B collapsed + expanded the card (geometry read), and teardown released \
                     every session + reaped the closed pane's pty (still-hosted panes reap on window \
                     close — external ps sweep)."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} app-shell assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
