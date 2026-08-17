//! `detach-adopt` self-test scenario — the tmux-port Phase 4 detach / adopt /
//! tear-off gate (plan § Slice 5).
//!
//! The whole phase's premise is that a session can outlive its OS window: closing
//! a window moves its running sessions into an app-global
//! [`DetachedPool`](crate::detached_pool::DetachedPool) instead of killing them,
//! every window's sidebar renders that pool, and adopting a row moves the live pty
//! back into a window with **no respawn**. None of that is observable from a unit
//! test — the pty has to be real, the windows have to be real OS windows, and the
//! move has to be driven through the shipped doors. This scenario does that, over
//! one real `zsh -il` whose grid carries a marker across every move.
//!
//! ## The legs
//!
//! 1. **A real marker in a real pty.** Window A opens through the SHIPPED builder
//!    (`crate::app::open_managed_window` → `build_window_root`), its eager Main
//!    shell comes up, and `echo <marker>` renders back into the live grid.
//! 2. **Close detaches (D1).** The window closes through the REAL close action
//!    (`-[NSWindow performClose:]` → `on_window_should_close` →
//!    `request_window_close`): no confirmation is presented (D1's no-confirm
//!    branch — nothing is destroyed, so nothing is confirmed), the window
//!    deregisters, and the session lands on the pool head. On disk, the ONE flush
//!    in `route_close_disk_fate` has already put it in `detached[]` and taken it
//!    out of `windows[]` — the §P5 single-batch ordering, observed at the file.
//! 3. **A pooled child is still alive and its dead window is inert** (plan N4).
//!    A second marker echoed into the POOLED pty renders — the child never
//!    noticed the window go away — and the stale present-kick aimed at the closed
//!    window no-ops instead of aborting the process.
//! 4. **The pool renders in a fresh window's sidebar (§P12).** Window B opens and
//!    its Detached section + row surface in this process's own macOS AX tree
//!    (`sidebar.detached.section` / `sidebar.detached.row`, the frozen ids) — a
//!    render assertion, not a model read.
//! 5. **Click-adopt moves the live pty (D3).** The shipped sidebar row handler
//!    adopts the row into window B: the pool empties, window B owns the session,
//!    and the pane's `Entity<TerminalSessionHandle>` is the **same entity** it was
//!    in window A with both markers still in its grid — the no-respawn proof.
//! 6. **The explicit detach chord (§P7/§P8).** `DetachSession` (⌃⌘⇧D) dispatched
//!    through gpui's real action machinery puts the session back on the pool while
//!    window B — which still owns its Main — stays open.
//! 7. **The adopt-latest chord (§P8).** `AdoptDetachedSession` (⌃⌘A) takes the
//!    pool head back into window B, same entity again.
//! 8. **Tear-off (§P9/D4).** The adopted session's pill is split, focus returns to
//!    the marker pane, and `TearOffPane` (⌃⌘N) moves that pane into an OS window of
//!    its own: a new window is registered, it hosts the SAME handle with the marker
//!    intact, and the source pill survives in window B healed to its remaining
//!    pane (the multi-leaf branch).
//!
//! ## Hermeticity
//!
//! A per-run temp tree: a sandbox `HOME` (so the login shells source no user rc), a
//! temp `NICE_APPLICATION_SUPPORT_ROOT` with its own `SessionStore` Global, and a
//! `DetachedPool` Global installed HERE — `run_selftest` installs none (the
//! hermeticity rule the `persistence-restore` scenario asserts), so this scenario
//! is the only place in the suite where a close detaches. Both Globals and every
//! env var are cleared at teardown, and every window this scenario opened is closed
//! before it reports, so the next scenario runs exactly as it did pre-Phase-4.
//!
//! **Registers the `WindowRegistry` WITHOUT `install`** (its `build_window_root`
//! only `register`s) and drives the disk fate through a SCOPED `on_window_closed`
//! observer calling `route_close_disk_fate` — the `persistence-restore` precedent.
//! Quit-when-empty would kill the suite the moment window A closed. Registered
//! BEFORE `multiwindow`, the sole installer, last.
//!
//! Needs **no** Accessibility grant: the close is an AppKit `performClose:`, the
//! chords go through gpui's own action dispatch, and the AX reads are of this
//! process's own tree (the `ax-probe` pattern). `Gate::SelfReported`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_view::TerminalSessionHandle;

use crate::app_shell::AppShellView;
use crate::detached_pool;
use crate::platform;
use crate::session_store::{self, SessionStore};
use crate::sidebar_shell::{
    SidebarShellView, SIDEBAR_DETACHED_ROW_ID, SIDEBAR_DETACHED_ROW_ROLE,
    SIDEBAR_DETACHED_SECTION_ID, SIDEBAR_DETACHED_SECTION_ROLE,
};
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

const POLL_MS: u64 = 100;
/// Poll cap for a real login shell to come up and echo a marker back.
const GRID_POLLS: usize = 80;
/// Poll cap for the AX tree to surface a node (AccessKit builds lazily, one frame
/// after the first query).
const AX_POLLS: usize = 60;
/// Poll cap for model-level settling after an action dispatch.
const SETTLE_POLLS: usize = 40;

/// Echoed into window A's Main pty before anything moves. Distinctive enough that
/// a login shell's own output cannot spoof it.
const MARKER: &str = "NICERS__DETACH__ADOPT__OK";
/// Echoed into the SAME pty while it sits in the pool with no window at all.
const POOLED_MARKER: &str = "NICERS__DETACH__POOLED__OK";
/// The macOS `AXRole` string a `gpui::Role` surfaces as, so the AX assertions
/// below check the role the ELEMENT declares
/// ([`SIDEBAR_DETACHED_SECTION_ROLE`] / [`SIDEBAR_DETACHED_ROW_ROLE`]) rather
/// than a second copy of it that can drift. Only the two roles the Detached
/// section uses are mapped; anything else is a bug in the caller, not a case to
/// guess at.
fn ax_role_name(role: gpui::Role) -> &'static str {
    match role {
        gpui::Role::Group => "AXGroup",
        gpui::Role::Button => "AXButton",
        _ => "AXUnknown",
    }
}

// ===========================================================================
// fixture
// ===========================================================================

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// The temp store's `sessions.json`, resolved once while
    /// `NICE_APPLICATION_SUPPORT_ROOT` points at the fixture.
    store_path: PathBuf,
    /// The developer's own `ZDOTDIR`, restored at teardown.
    prev_zdotdir: Option<String>,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-detach-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;
        let home = base.join("home");
        let support_root = base.join("app-support");
        for d in [&home, &support_root] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }
        let prev_zdotdir = std::env::var("ZDOTDIR").ok();
        // SAFETY: single-threaded scenario setup, before any window forks.
        unsafe {
            std::env::set_var(
                "NICE_APPLICATION_SUPPORT_ROOT",
                support_root.to_string_lossy().as_ref(),
            );
            // No shell-inject config is installed under `run_selftest`, so the
            // login shells below inherit the PROCESS `ZDOTDIR`. Point it at the
            // empty sandbox home so no developer's `.zshrc` output can land in a
            // grid this scenario greps for its markers (the live-run rule in
            // `docs/testing.md`).
            std::env::set_var("ZDOTDIR", home.to_string_lossy().as_ref());
        };
        let store_path = session_store::default_store_path();
        Ok(Fixture {
            base,
            home,
            store_path,
            prev_zdotdir,
        })
    }
}

// ===========================================================================
// scenario wiring
// ===========================================================================

/// Open the `detach-adopt` scenario's first window through the shipped builder and
/// spawn its driver (self-reported gate). Installs the temp store + the pool
/// Global first — the pool is what makes a close detach, and nothing else in the
/// suite installs one.
pub fn open_detach_adopt_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home.to_string_lossy().into_owned();

    let whandle: WindowHandle<AppShellView> = cx.update(|app| -> Result<_> {
        // The chords below go through gpui's real action dispatch, so the shipped
        // keymap must be installed (idempotent — an earlier scenario may have).
        crate::keymap::install_shortcuts(app);
        session_store::install_global(SessionStore::open(session_store::default_store_path()));
        // The pool boots exactly as `app::run` boots it: after the store, before
        // any window exists.
        detached_pool::install(app);
        open_sandboxed_window(app, &home)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_detach_adopt(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'detach-adopt': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

/// Open a fresh managed window with `HOME` pointed at the sandbox for the duration
/// of the call: the window's cwd, its Terminals project path and — load-bearing —
/// the env its eager Main shell forks with all read the process `HOME`.
fn open_sandboxed_window(
    app: &mut gpui::App,
    home: &str,
) -> Result<WindowHandle<AppShellView>> {
    let prev = std::env::var("HOME").ok();
    // SAFETY: single-threaded; restored immediately below, before any await.
    unsafe { std::env::set_var("HOME", home) };
    let opened = crate::app::open_managed_window(app);
    match prev {
        Some(h) => unsafe { std::env::set_var("HOME", h) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    opened
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_detach_adopt(
    cx: &mut AsyncApp,
    whandle_a: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    cx.update(|app| app.activate(true));
    // Wait for the driver's OWN activation preamble to finish before anything can
    // close this window: it polls `is_window_active` on the handle we are about to
    // close in leg 2, and a handle that dies mid-preamble fails the scenario with
    // the (misleading) "never became frontmost" message. Best-effort — the
    // preamble, not this, is the activation authority.
    for _ in 0..20 {
        settle(cx, POLL_MS).await;
        if whandle_a
            .update(cx, |_v, w, _a| w.is_window_active())
            .unwrap_or(false)
        {
            break;
        }
    }
    settle(cx, 400).await;

    let id_a = AnyWindowHandle::from(whandle_a).window_id();
    let Some(state_a) = cx.update(|app| WindowRegistry::state_for_window(app, id_a)) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "detach-adopt: the shipped builder did not register window A".to_string(),
            ),
        )
        .await;
    };

    // The SCOPED close observer (the `persistence-restore` precedent): route the
    // real disk fate + the Phase-4 detach partition + the pty teardown, WITHOUT
    // the quit-when-empty that would kill the suite the moment window A closes.
    // Held for the driver's lifetime — it must outlive the teardown closes below,
    // or their ptys leak into the next scenario.
    let _close_sub = cx.update(|app| {
        app.on_window_closed(WindowRegistry::route_close_disk_fate)
    });

    let mut failures: Vec<String> = Vec::new();

    // === 1. a real marker in a real pty ====================================
    let Some((session_id, term_window_id, pane_id)) = active_pane_keys(cx, &state_a) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "detach-adopt: window A opened with no active session/window/pane".to_string(),
            ),
        )
        .await;
    };
    let Some(handle) = cx
        .update(|app| state_a.read(app).ptys.pane_handle(&session_id, &term_window_id, &pane_id))
    else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "detach-adopt: window A's Main pane never got a pty handle (the eager fresh-window \
                 spawn did not run)"
                    .to_string(),
            ),
        )
        .await;
    };
    let handle_id = handle.entity_id();
    write_line(cx, &handle, &format!("echo {MARKER}"));
    if !poll_grid_contains(cx, &handle, MARKER).await {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(format!(
                "detach-adopt: window A's login shell never echoed '{MARKER}' — no live pty to \
                 detach"
            )),
        )
        .await;
    }

    // === 2. closing the window detaches (D1 / §P5) =========================
    let pooled_before = cx.update(|app| detached_pool::pool_rows(app));
    if !pooled_before.is_empty() {
        failures.push(format!(
            "(pre) the pool was not empty before the first detach: {pooled_before:?}"
        ));
    }

    let view_ptr = whandle_a
        .update(cx, |_v, w, _a| platform::ns_view_of(w))
        .unwrap_or(std::ptr::null_mut());
    platform::perform_window_close_ptr(view_ptr);
    settle(cx, 600).await;

    // D1: nothing was destroyed, so nothing was confirmed.
    if cx.update(|app| state_a.read(app).pending_modal().is_some()) {
        failures.push(
            "(2) closing a window with live sessions presented a confirmation — with detach ON the \
             close is non-destructive and D1 skips it"
                .to_string(),
        );
    }
    if cx.update(|app| WindowRegistry::state_for_window(app, id_a).is_some()) {
        failures.push("(2) the real close action did not close window A".to_string());
    }
    // A detached session is RE-KEYED on the way out (the pool is app-global; a
    // window's ids are only window-unique), so the pool row is found by BEING the
    // window's one session, not by the id it had while attached. Its identity is
    // proved where it counts — leg 5 checks the very same pty entity comes back.
    let rows = cx.update(|app| detached_pool::pool_rows(app));
    let Some(pooled_id) = rows.first().map(|(id, _, _)| id.clone()) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "detach-adopt: the pool is empty after closing window A — the close-path detach \
                 partition did not run"
                    .to_string(),
            ),
        )
        .await;
    };
    if rows.len() != 1 {
        failures.push(format!(
            "(2) closing a one-session window put {} rows on the pool, expected 1 (rows: {rows:?})",
            rows.len()
        ));
    }
    if pooled_id == session_id {
        failures.push(format!(
            "(2) the pooled row kept its in-window id '{session_id}' — a detached session must take \
             a process-unique one, or it collides with the seeded Main of whatever window adopts it"
        ));
    }
    // The ONE flush in `route_close_disk_fate` already covered both halves.
    check_disk(&fixture, &pooled_id, true, &mut failures, "2");

    // === 3. the pooled child is alive; its dead window is inert (N4) =======
    write_line(cx, &handle, &format!("echo {POOLED_MARKER}"));
    if !poll_grid_contains(cx, &handle, POOLED_MARKER).await {
        failures.push(format!(
            "(3) the POOLED pty never echoed '{POOLED_MARKER}' — detaching killed the child (or \
             its feeder stopped once the window went away)"
        ));
    }

    // === 4. the pool renders in a fresh window's sidebar (§P12) ============
    let home = fixture.home.to_string_lossy().into_owned();
    let whandle_b = match cx.update(|app| open_sandboxed_window(app, &home)) {
        Ok(h) => h,
        Err(e) => {
            return teardown(
                cx,
                fixture,
                CadenceReport::error(format!("detach-adopt: could not open window B: {e:#}")),
            )
            .await
        }
    };
    cx.update(|app| app.activate(true));
    let _ = whandle_b.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 500).await;

    let id_b = AnyWindowHandle::from(whandle_b).window_id();
    let Some(state_b) = cx.update(|app| WindowRegistry::state_for_window(app, id_b)) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error("detach-adopt: window B did not register".to_string()),
        )
        .await;
    };
    let Ok(shell_b) = whandle_b.entity(cx) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error("detach-adopt: could not read window B's shell view".to_string()),
        )
        .await;
    };
    let sidebar_b = shell_b.update(cx, |s, _| s.scenario_sidebar());
    ax_detached_section_checks(cx, &state_b, &mut failures).await;

    // Window B seeded its own "Main" exactly as window A did, so this is where an
    // id shared across two windows would bite: the adopt below would be refused
    // by the duplicate-id guard and the click would do nothing at all.
    if cx.update(|app| state_b.read(app).workspace.session_for(&pooled_id).is_some()) {
        failures.push(format!(
            "(4) window B already owns the pooled id '{pooled_id}' — the pool's app-global \
             namespace collided with a freshly-seeded window's own session"
        ));
    }

    // === 5. click-adopt moves the live pty (D3) ============================
    adopt_click_checks(
        cx,
        &sidebar_b,
        &state_b,
        &pooled_id,
        handle_id,
        &handle,
        &fixture,
        &mut failures,
    )
    .await;

    // === 6/7. the explicit-detach and adopt-latest chords (§P7/§P8) ========
    chord_checks(
        cx,
        whandle_b,
        &state_b,
        &pooled_id,
        handle_id,
        &handle,
        &mut failures,
    )
    .await;

    // === 8. tear-off into an OS window of its own (§P9/D4) =================
    tear_off_checks(cx, whandle_b, &state_b, handle_id, &mut failures).await;

    teardown(cx, fixture, build_report(failures)).await
}

// ---- 4. the Detached section renders ---------------------------------------

/// Poll this process's own macOS AX tree until the Detached section AND one
/// detached row surface with the roles their build sites declare (`AXGroup` for
/// the container, `AXButton` for the clickable row). The shipped shell does not
/// drive continuous frames, so each tick forces a repaint (the `app-shell` anchor
/// pattern) to keep AccessKit's lazily-built tree current.
async fn ax_detached_section_checks(
    cx: &mut AsyncApp,
    state_b: &Entity<WindowState>,
    failures: &mut Vec<String>,
) {
    let want_section = ax_role_name(SIDEBAR_DETACHED_SECTION_ROLE);
    let want_row = ax_role_name(SIDEBAR_DETACHED_ROW_ROLE);
    let pid = std::process::id() as i32;
    let mut section = Err("never queried".to_string());
    let mut row = Err("never queried".to_string());
    for _ in 0..AX_POLLS {
        state_b.update(cx, |_s, c| c.notify());
        settle(cx, POLL_MS).await;
        section = platform::ax_find_titled_role(pid, SIDEBAR_DETACHED_SECTION_ID);
        row = platform::ax_find_titled_role(pid, SIDEBAR_DETACHED_ROW_ID);
        if matches!(&section, Ok(r) if r == want_section) && matches!(&row, Ok(r) if r == want_row)
        {
            break;
        }
    }
    match section {
        Ok(role) if role == want_section => {}
        Ok(role) => failures.push(format!(
            "(4) '{SIDEBAR_DETACHED_SECTION_ID}' exposes role '{role}', want {want_section}"
        )),
        Err(e) => failures.push(format!(
            "(4) the Detached section never rendered — '{SIDEBAR_DETACHED_SECTION_ID}' is not in \
             the AX tree: {e}"
        )),
    }
    match row {
        Ok(role) if role == want_row => {}
        Ok(role) => failures.push(format!(
            "(4) '{SIDEBAR_DETACHED_ROW_ID}' exposes role '{role}', want {want_row}"
        )),
        Err(e) => failures.push(format!(
            "(4) the pooled session rendered no Detached row — '{SIDEBAR_DETACHED_ROW_ID}' is not \
             in the AX tree: {e}"
        )),
    }
}

// ---- 5. click-adopt ---------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn adopt_click_checks(
    cx: &mut AsyncApp,
    sidebar_b: &Entity<SidebarShellView>,
    state_b: &Entity<WindowState>,
    session_id: &str,
    handle_id: gpui::EntityId,
    handle: &Entity<TerminalSessionHandle>,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) {
    // The shipped row handler — the same call the row's click listener makes.
    sidebar_b.update(cx, |v, cx| v.drive_adopt_detached_row(session_id, cx));
    settle(cx, 400).await;

    let rows = cx.update(|app| detached_pool::pool_rows(app));
    if rows.iter().any(|(id, _, _)| id == session_id) {
        failures.push(format!(
            "(5) '{session_id}' is still pooled after the adopt click (rows: {rows:?})"
        ));
    }
    if !cx.update(|app| state_b.read(app).workspace.session_for(session_id).is_some()) {
        failures.push(format!(
            "(5) window B does not own '{session_id}' after adopting it"
        ));
        return;
    }
    match adopted_pane_handle(cx, state_b, session_id) {
        Some(h) if h.entity_id() == handle_id => {}
        Some(h) => failures.push(format!(
            "(5) the adopted pane carries a DIFFERENT session handle ({:?} != {handle_id:?}) — the \
             pty was respawned instead of moved",
            h.entity_id()
        )),
        None => failures.push(
            "(5) the adopted session has no pane handle in window B — the live payload did not \
             move with it"
                .to_string(),
        ),
    }
    // The scrollback moved with the child: BOTH markers are still in the grid, the
    // second of which was written while the session had no window at all.
    let grid = grid_text(cx, handle);
    for needle in [MARKER, POOLED_MARKER] {
        if !grid.contains(needle) {
            failures.push(format!(
                "(5) the adopted pane's grid lost '{needle}' — scrollback did not survive the move"
            ));
        }
    }
    // The §P6/B3b single batch: after the flush the session is in `windows[]` and
    // NOT in `detached[]` — never in both, never in neither.
    session_store::flush();
    check_disk(fixture, session_id, false, failures, "5");
}

// ---- 6/7. the two chords ----------------------------------------------------

async fn chord_checks(
    cx: &mut AsyncApp,
    whandle_b: WindowHandle<AppShellView>,
    state_b: &Entity<WindowState>,
    session_id: &str,
    handle_id: gpui::EntityId,
    handle: &Entity<TerminalSessionHandle>,
    failures: &mut Vec<String>,
) {
    if !rekey(cx, whandle_b).await {
        failures.push(
            "(6) window B is not frontmost — the chord legs cannot be dispatched safely (the \
             handlers route through WindowRegistry::active_state); free the display Space and \
             re-run"
                .to_string(),
        );
        return;
    }

    // 6. ⌃⌘⇧D — the explicit detach of the active (adopted) session. It leaves
    // the window under a fresh id, exactly as the close-path detach did.
    dispatch(cx, whandle_b, Box::new(crate::keymap::DetachSession));
    settle(cx, 400).await;
    let pooled = poll_until(cx, |cx| {
        !cx.update(|app| detached_pool::pool_rows(app)).is_empty()
    })
    .await;
    if !pooled {
        failures.push(format!(
            "(6) the DetachSession chord left '{session_id}' attached — the pool never took it"
        ));
    }
    if cx.update(|app| state_b.read(app).workspace.session_for(session_id).is_some()) {
        failures.push(format!(
            "(6) window B still owns '{session_id}' after the explicit detach"
        ));
    }
    // §P7's terminus: window B still owns its Main, so it must NOT have closed.
    if cx.update(|app| WindowRegistry::state_for_window(app, AnyWindowHandle::from(whandle_b).window_id()).is_none()) {
        failures.push(
            "(6) detaching one of window B's two sessions closed the window — only an EMPTIED \
             window closes (§P7)"
                .to_string(),
        );
        return;
    }

    // 7. ⌃⌘A — adopt the pool head straight back, under the id it now carries.
    let Some(redetached_id) = cx
        .update(|app| detached_pool::pool_rows(app))
        .first()
        .map(|(id, _, _)| id.clone())
    else {
        failures.push("(7) nothing on the pool to adopt back".to_string());
        return;
    };
    if !rekey(cx, whandle_b).await {
        failures.push("(7) window B lost frontmost before the adopt-latest chord".to_string());
        return;
    }
    dispatch(cx, whandle_b, Box::new(crate::keymap::AdoptDetachedSession));
    settle(cx, 400).await;
    let adopted = poll_until(cx, |cx| {
        cx.update(|app| {
            state_b
                .read(app)
                .workspace
                .session_for(&redetached_id)
                .is_some()
        })
    })
    .await;
    if !adopted {
        failures.push(format!(
            "(7) the AdoptDetachedSession chord did not bring '{redetached_id}' back into window B"
        ));
        return;
    }
    if !cx.update(|app| detached_pool::pool_rows(app)).is_empty() {
        failures.push("(7) the pool is not empty after adopting its only row".to_string());
    }
    match adopted_pane_handle(cx, state_b, &redetached_id) {
        Some(h) if h.entity_id() == handle_id => {}
        Some(h) => failures.push(format!(
            "(7) the re-adopted pane carries a DIFFERENT handle ({:?} != {handle_id:?}) — the \
             second round trip respawned it",
            h.entity_id()
        )),
        None => failures.push("(7) the re-adopted session has no pane handle".to_string()),
    }
    if !grid_text(cx, handle).contains(MARKER) {
        failures.push(format!(
            "(7) '{MARKER}' is gone from the grid after the detach/adopt round trip"
        ));
    }
}

// ---- 8. tear-off ------------------------------------------------------------

async fn tear_off_checks(
    cx: &mut AsyncApp,
    whandle_b: WindowHandle<AppShellView>,
    state_b: &Entity<WindowState>,
    handle_id: gpui::EntityId,
    failures: &mut Vec<String>,
) {
    // The re-adopted session is the selected one, and detach re-keys on every
    // trip out of a window — so read the ids off the live model rather than
    // carrying leg 5's copy down here.
    let Some((session_id, term_window_id, marker_pane)) = active_pane_keys(cx, state_b) else {
        failures.push("(8) window B has no active pane to split".to_string());
        return;
    };
    let session_id = session_id.as_str();

    // Split, so the tear-off exercises the MULTI-LEAF branch (the source pill
    // survives, healed and refocused) rather than the whole-pill move.
    if !rekey(cx, whandle_b).await {
        failures.push("(8) window B lost frontmost before the split".to_string());
        return;
    }
    dispatch(cx, whandle_b, Box::new(crate::keymap::SplitRight));
    let split = poll_until(cx, |cx| {
        leaf_count(cx, state_b, session_id, &term_window_id) == Some(2)
    })
    .await;
    if !split {
        failures.push(
            "(8) the SplitRight chord did not split the adopted pill — the tear-off leg needs two \
             leaves to exercise the multi-leaf branch"
                .to_string(),
        );
        return;
    }

    // Focus back onto the marker pane (the split focused the NEW one).
    dispatch(cx, whandle_b, Box::new(crate::keymap::FocusPaneLeft));
    let refocused = poll_until(cx, |cx| {
        active_pane_keys(cx, state_b).map(|(_, _, p)| p) == Some(marker_pane.clone())
    })
    .await;
    if !refocused {
        failures.push(format!(
            "(8) FocusPaneLeft did not return focus to the marker pane '{marker_pane}'"
        ));
        return;
    }

    let windows_before: Vec<_> = cx.update(|app| WindowRegistry::all_states(app));
    dispatch(cx, whandle_b, Box::new(crate::keymap::TearOffPane));
    let grew = poll_until(cx, |cx| {
        cx.update(|app| WindowRegistry::count(app)) > windows_before.len()
    })
    .await;
    if !grew {
        failures.push(
            "(8) the TearOffPane chord opened no new OS window — the pane never left window B"
                .to_string(),
        );
        return;
    }

    // The new window hosts the SAME live handle, marker intact.
    let torn = cx.update(|app| {
        WindowRegistry::all_states(app)
            .into_iter()
            .find(|s| s != state_b && pane_handle_ids(s, app).contains(&handle_id))
    });
    match torn {
        Some(state_c) => {
            let handle = cx.update(|app| {
                pane_handles(&state_c, app)
                    .into_iter()
                    .find(|h| h.entity_id() == handle_id)
            });
            match handle {
                Some(h) if grid_text(cx, &h).contains(MARKER) => {}
                Some(_) => failures.push(format!(
                    "(8) the torn-off window hosts the moved pane but its grid lost '{MARKER}'"
                )),
                None => failures.push(
                    "(8) the torn-off window's pane handle vanished between the two reads"
                        .to_string(),
                ),
            }
        }
        None => failures.push(format!(
            "(8) no newly-opened window hosts the moved pane handle ({handle_id:?}) — tear-off \
             respawned instead of moving it"
        )),
    }

    // The multi-leaf branch heals the source: window B keeps the pill, now a
    // single leaf, and still owns the session.
    match leaf_count(cx, state_b, session_id, &term_window_id) {
        Some(1) => {}
        Some(n) => failures.push(format!(
            "(8) the source pill has {n} leaves after tear-off, expected the sole survivor"
        )),
        None => failures.push(
            "(8) the source pill is gone from window B — the multi-leaf branch must leave it \
             standing"
                .to_string(),
        ),
    }
}

// ===========================================================================
// helpers
// ===========================================================================

/// `(session_id, term_window_id, pane_id)` of the window's focused pane.
fn active_pane_keys(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
) -> Option<(String, String, String)> {
    cx.update(|app| {
        let ws = state.read(app);
        let session_id = ws.workspace.active_session_id()?.to_string();
        let session = ws.workspace.session_for(&session_id)?;
        let term_window_id = session.active_window_id.clone()?;
        let pane_id = session
            .windows
            .iter()
            .find(|w| w.id == term_window_id)?
            .effective_pane_id();
        Some((session_id, term_window_id, pane_id))
    })
}

/// The session's focused-pane handle in `state`'s pty manager.
fn adopted_pane_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    cx.update(|app| {
        let ws = state.read(app);
        let session = ws.workspace.session_for(session_id)?;
        let term_window_id = session.active_window_id.clone()?;
        let pane_id = session
            .windows
            .iter()
            .find(|w| w.id == term_window_id)?
            .effective_pane_id();
        ws.ptys.pane_handle(session_id, &term_window_id, &pane_id)
    })
}

/// Every live pane handle a window currently holds.
fn pane_handles(state: &Entity<WindowState>, app: &gpui::App) -> Vec<Entity<TerminalSessionHandle>> {
    let ws = state.read(app);
    ws.ptys
        .live_pane_keys()
        .into_iter()
        .filter_map(|(s, w, p)| ws.ptys.pane_handle(&s, &w, &p))
        .collect()
}

fn pane_handle_ids(state: &Entity<WindowState>, app: &gpui::App) -> Vec<gpui::EntityId> {
    pane_handles(state, app)
        .into_iter()
        .map(|h| h.entity_id())
        .collect()
}

/// Leaf count of one pill's split tree, or `None` when the pill is gone.
fn leaf_count(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
) -> Option<usize> {
    cx.update(|app| {
        state
            .read(app)
            .workspace
            .session_for(session_id)?
            .windows
            .iter()
            .find(|w| w.id == term_window_id)
            .map(|w| w.layout.leaf_count())
    })
}

fn grid_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _| h.session().grid_lines().join("\n"))
}

fn write_line(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>, line: &str) {
    let payload = format!("{line}\n");
    handle.update(cx, |h, _| {
        let _ = h.session().write_input(payload.as_bytes());
    });
}

async fn poll_grid_contains(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    needle: &str,
) -> bool {
    for _ in 0..GRID_POLLS {
        settle(cx, POLL_MS).await;
        if grid_text(cx, handle).contains(needle) {
            return true;
        }
    }
    false
}

/// Poll a model-level predicate until it holds (or the cap runs out).
async fn poll_until(cx: &mut AsyncApp, mut pred: impl FnMut(&mut AsyncApp) -> bool) -> bool {
    for _ in 0..SETTLE_POLLS {
        settle(cx, POLL_MS).await;
        if pred(cx) {
            return true;
        }
    }
    false
}

/// Re-assert frontmost/key before dispatching an action, and report whether this
/// window actually IS the active one — the handlers resolve their target through
/// `WindowRegistry::active_state`, so a dispatch on a non-frontmost window would
/// silently act on the wrong one.
async fn rekey(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>) -> bool {
    cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;
    let want = AnyWindowHandle::from(whandle).window_id();
    cx.update(|app| app.active_window().map(|w| w.window_id()) == Some(want))
}

/// Dispatch a shipped action through gpui's real dispatch machinery — the same
/// route a keystroke takes (`persistence-restore`'s Bug-A pattern), with no
/// CGEvent and therefore no Accessibility grant.
fn dispatch(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>, action: Box<dyn gpui::Action>) {
    let _ = whandle.update(cx, |_v, window, app| {
        window.dispatch_action(action, app);
    });
}

/// Assert where `session_id` lives ON DISK: exactly one of the two buckets, never
/// both and never neither. `want_detached` picks which.
fn check_disk(
    fixture: &Fixture,
    session_id: &str,
    want_detached: bool,
    failures: &mut Vec<String>,
    leg: &str,
) {
    let state = session_store::read_state(&fixture.store_path);
    let in_detached = state.detached.iter().any(|d| d.session.id == session_id);
    let in_windows = state
        .windows
        .iter()
        .flat_map(|w| w.projects.iter())
        .flat_map(|p| p.sessions.iter())
        .any(|s| s.id == session_id);
    if in_detached != want_detached {
        failures.push(format!(
            "({leg}) on disk, detached[] {} '{session_id}' (expected it {})",
            if in_detached { "holds" } else { "is missing" },
            if want_detached { "there" } else { "gone" }
        ));
    }
    if in_windows == want_detached {
        failures.push(format!(
            "({leg}) on disk, windows[] {} '{session_id}' — a session must live in exactly one \
             bucket (§P5/§P6 single-batch ordering)",
            if in_windows { "also holds" } else { "is missing" }
        ));
    }
}

// ===========================================================================
// teardown + report
// ===========================================================================

/// Close every window this scenario opened (while the scoped close observer is
/// still alive, so their ptys are reaped), then clear both Globals and every env
/// var so the next scenario runs exactly as it did pre-Phase-4.
async fn teardown(cx: &mut AsyncApp, fixture: Fixture, report: CadenceReport) -> CadenceReport {
    let handles = cx.update(|app| {
        WindowRegistry::all_states(app)
            .into_iter()
            .filter_map(|s| s.read(app).window_handle())
            .collect::<Vec<_>>()
    });
    for handle in handles {
        let _ = handle.update(cx, |_root, window, _app| window.remove_window());
    }
    settle(cx, 400).await;
    cx.update(|app| {
        // Dropping the pool drops any payload still in it — a clean SIGHUP for the
        // detached children, the same semantics the quit cascade gives them.
        detached_pool::clear_global(app);
        session_store::clear_global();
    });
    // SAFETY: teardown, single-threaded.
    unsafe {
        std::env::remove_var("NICE_APPLICATION_SUPPORT_ROOT");
        match &fixture.prev_zdotdir {
            Some(z) => std::env::set_var("ZDOTDIR", z),
            None => std::env::remove_var("ZDOTDIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&fixture.base);
    report
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "detach-adopt OK: a real close detached a live session into the pool with no \
                     confirmation and one disk batch, the pooled child kept running with its dead \
                     window inert, a fresh window rendered the Detached section + row, click-adopt \
                     / ⌃⌘⇧D / ⌃⌘A moved the SAME pty handle in and out with scrollback intact, and \
                     ⌃⌘N tore the marker pane into an OS window of its own leaving the source pill \
                     healed"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} detach-adopt assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
