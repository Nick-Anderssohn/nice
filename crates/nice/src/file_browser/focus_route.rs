//! `focus_route` — the production focus-follow closure for the app-wide file-op
//! undo/redo history (F6, the cross-window undo half). Slice 2 stood the history
//! `Entity` + `Global` up with a `None` focus-follow (drift still published, but
//! no window-activation routing); the final-composition slice fills that seam
//! here so ⌘Z in window B undoes window A's op AND routes focus back to window A
//! — the Milestone-5 claim.
//!
//! ## Why a two-phase bridge (the cx-less closure constraint)
//!
//! [`FocusFollow`](super::history::FocusFollow) is `FnMut(&FileOperationOrigin)
//! -> FocusResult` — it runs *inside* [`FileOperationHistory::undo`]/`redo`, which
//! take no `cx`, so the closure cannot touch the `App` to activate a window. The
//! frozen slice-1 signature (pinned by `CrossWindowUndoTests`) must not change.
//! So the work splits across the [`FocusRouter`] shared cell:
//!
//!   1. **`refresh_live`** (dispatcher, has `&App`) snapshots the live windows'
//!      session ids into the router BEFORE the undo/redo.
//!   2. The **focus-follow closure** (cx-less) looks the origin up in that
//!      snapshot: live ⇒ record the origin + return [`FocusResult::Routed`], gone
//!      ⇒ [`FocusResult::OriginGone`] (the inverse still applies, plus the
//!      closed-window banner).
//!   3. **`drive_pending`** (dispatcher, has `&mut App`) drains the recorded
//!      origins AFTER the undo/redo and drives each real window: `activate_window`
//!      + sidebar → Files + `select_tab(origin.tab_id)` — Swift's
//!      `FileOperationFocusRouter` behaviour, native shape (the documented
//!      divergence from its 2-method protocol).
//!
//! Both `refresh_live` and `drive_pending` are no-ops when no [`FocusRouterGlobal`]
//! is installed (a scenario / preview that never stood the router up), the same
//! "no Global ⇒ inert" discipline the other seams use. [`install`] is called by
//! `app::run`'s `install_file_operations` (production) and by the `file-browser`
//! composition leg (both over a `WindowRegistry`-registered window set).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::{AnyWindowHandle, App, Entity, Global};

use nice_model::SidebarMode;

use crate::file_browser::history::{FileOperationHistory, FocusFollow, FocusResult};
use crate::file_browser::ops::FileOperationOrigin;
use crate::window_registry::WindowRegistry;

/// The shared bridge between the cx-less focus-follow closure and the dispatcher
/// (see the module docs). Not `Send` — it lives on the main-thread `App` only, the
/// same as every gpui entity/global here.
#[derive(Default)]
struct FocusRouteState {
    /// Session ids of the live windows, refreshed by [`refresh_live`] from the
    /// [`WindowRegistry`] before each undo/redo so the closure can decide
    /// Routed-vs-Gone without a `cx`.
    live: HashSet<String>,
    /// Origins the closure resolved to a live window this pass; [`drive_pending`]
    /// drains them and drives the real windows after the undo/redo lands.
    resolved: Vec<FileOperationOrigin>,
}

/// A clonable handle on the [`FocusRouteState`] cell (one `Rc` in the closure, one
/// in the [`FocusRouterGlobal`]).
#[derive(Clone, Default)]
pub struct FocusRouter(Rc<RefCell<FocusRouteState>>);

/// Process Global holding the router so the dispatcher ([`refresh_live`] /
/// [`drive_pending`]) can reach the same cell the installed focus-follow closure
/// captured.
pub struct FocusRouterGlobal(pub FocusRouter);
impl Global for FocusRouterGlobal {}

impl FocusRouter {
    /// The [`FocusFollow`] closure to install on the history: look the origin up in
    /// the live snapshot, record it for [`drive_pending`] when live, and report
    /// Routed / OriginGone accordingly.
    fn follow(&self) -> FocusFollow {
        let inner = self.0.clone();
        Box::new(move |origin: &FileOperationOrigin| {
            let mut st = inner.borrow_mut();
            if st.live.contains(&origin.window_session_id) {
                st.resolved.push(origin.clone());
                FocusResult::Routed
            } else {
                FocusResult::OriginGone
            }
        })
    }
}

/// Create the router, set its production focus-follow closure on `history`, and
/// install the [`FocusRouterGlobal`]. Idempotent enough to call per scenario (a
/// fresh router replaces any prior one, matching the fresh-history-per-scenario
/// discipline).
pub fn install(cx: &mut App, history: &Entity<FileOperationHistory>) {
    let router = FocusRouter::default();
    history.update(cx, |h, _| h.set_focus_follow(router.follow()));
    cx.set_global(FocusRouterGlobal(router));
}

/// Snapshot the live windows' session ids into the router so the cx-less
/// focus-follow closure can decide Routed vs OriginGone, and clear any stale
/// resolved list. No-op when no router is installed.
pub fn refresh_live(cx: &App) {
    let Some(router) = cx.try_global::<FocusRouterGlobal>().map(|g| g.0.clone()) else {
        return;
    };
    let live: HashSet<String> = WindowRegistry::all_states(cx)
        .iter()
        .map(|s| s.read(cx).session_id().to_string())
        .collect();
    let mut st = router.0.borrow_mut();
    st.live = live;
    st.resolved.clear();
}

/// Drive every route the focus-follow closure resolved this pass: bring the
/// originating window frontmost, flip its sidebar to Files, and select the origin
/// tab. No-op when no router is installed / nothing resolved.
pub fn drive_pending(cx: &mut App) {
    let Some(router) = cx.try_global::<FocusRouterGlobal>().map(|g| g.0.clone()) else {
        return;
    };
    let resolved: Vec<FileOperationOrigin> = std::mem::take(&mut router.0.borrow_mut().resolved);
    for origin in resolved {
        route_to_origin(cx, &origin);
    }
}

/// Flip the origin window to Files + select its origin tab, then activate it. The
/// restored entry becomes visible through R19's watcher hub — no extra refresh.
fn route_to_origin(cx: &mut App, origin: &FileOperationOrigin) {
    let Some(state) = WindowRegistry::state_for_session_id(cx, &origin.window_session_id) else {
        return;
    };
    state.update(cx, |s, cx| {
        // Only two modes — flip to Files when not already there (`select_tab`
        // handles a stale/absent id gracefully).
        if s.sidebar.mode() != SidebarMode::Files {
            s.sidebar.toggle_sidebar_mode();
        }
        if let Some(tab_id) = &origin.tab_id {
            s.model.select_tab(tab_id);
        }
        cx.notify();
    });
    if let Some(handle) = window_handle_for_session(cx, &origin.window_session_id) {
        let _ = handle.update(cx, |_root, window, _app| window.activate_window());
    }
}

/// The live window handle whose `WindowState` carries `session_id` (matched via
/// the registry's id-keyed lookup over `cx.windows()`). `None` when the origin
/// window is gone — the caller then leaves the change headless.
fn window_handle_for_session(cx: &App, session_id: &str) -> Option<AnyWindowHandle> {
    cx.windows().into_iter().find(|w| {
        WindowRegistry::state_for_window(cx, w.window_id())
            .is_some_and(|s| s.read(cx).session_id() == session_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The closure records a live origin and returns `Routed`; an unknown origin
    /// returns `OriginGone` and records nothing — the decision the cx-less half
    /// makes over the snapshot `refresh_live` fills (the gpui-driven half is
    /// exercised by the `file-browser` composition leg).
    #[test]
    fn follow_routes_live_origins_and_records_them() {
        let router = FocusRouter::default();
        router.0.borrow_mut().live.insert("win-A".to_string());
        let mut follow = router.follow();

        let live = FileOperationOrigin::new("win-A", Some("tab-A".into()));
        let gone = FileOperationOrigin::new("win-Z", Some("tab-Z".into()));
        assert_eq!(follow(&live), FocusResult::Routed);
        assert_eq!(follow(&gone), FocusResult::OriginGone);

        let resolved = &router.0.borrow().resolved;
        assert_eq!(resolved.len(), 1, "only the live origin is recorded");
        assert_eq!(resolved[0].window_session_id, "win-A");
        assert_eq!(resolved[0].tab_id.as_deref(), Some("tab-A"));
    }
}
