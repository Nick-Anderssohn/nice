//! `WindowRegistry` — the process-wide `WindowId → Entity<WindowState>` map, the
//! Rust mirror of Swift's `WindowRegistry` (`Sources/Nice/State/WindowRegistry.swift`).
//!
//! gpui entities are already app-global, so the registry is thin: its job is the
//! **lookup contract** the dossier (G5) names four consumers for, all of which
//! must exist even though only the first is live this cycle:
//!
//!   a. **Focused-window shortcut routing** (R12 keymap slice) —
//!      [`WindowRegistry::active_state`]: the key window, else the most-recently
//!      keyed window, else the first registered.
//!   b. **Close / quit confirmation** (W5 / R18) — registration bakes in **no**
//!      close-confirm behavior; R18 attaches it via `on_window_should_close`
//!      later. R12's only close behavior is deregister + [`WindowState::teardown`]
//!      + quit-when-empty.
//!   c. **Per-session-id lookup** (Stage 5 undo routing) —
//!      [`WindowRegistry::state_for_window_session_id`].
//!   d. **R25 cross-window migration** — served by
//!      [`WindowRegistry::state_for_window`] / the id-keyed map.
//!
//! MRU is tracked with our own ordered list (the pin's `window_stack()` is only a
//! z-order *assist* and may return `None`), updated from each window's
//! `observe_window_activation` — exactly the role Swift's `didBecomeKey` observer
//! played. The DO-NOT-PORT list holds: no `WindowClaimLedger`, no token channel,
//! no `NSEvent` monitor — ⌘N calls `open_window` directly (see `crate::app`).
//!
//! The registry holds a **strong** `Entity<WindowState>` per window, so a window's
//! state lives exactly as long as its registration; [`WindowRegistry::handle_window_closed`]
//! drops it (after teardown) on close.

// The read side of the lookup contract — `active_state` (consumer a),
// `state_for_window` / `state_for_window_session_id` (consumers c/d), `count`, and the
// pure `mru::select` they lean on — has no in-crate caller until R12's keymap
// slice routes shortcuts through `active_state` and the multi-window scenario /
// itests assert `count`. The plan requires all four consumers to EXIST now
// (dossier G5); the write side (register / note_active / close hook) is live.
// Same "seam ahead of its caller" pattern as `sidebar_actions` / `window_strip_actions`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use gpui::{App, Entity, Global, WindowId};

use crate::window_state::WindowState;

/// Pure MRU-ordering + active-window-selection logic, generic over the window
/// key so it is unit-testable without a gpui `App`. `order[0]` is the best
/// candidate: a window moves to the front when it becomes active
/// ([`touch`]); a freshly-registered window is appended to the back
/// ([`append`]) so, before anything is keyed, the *first* registered window is
/// still front-most (Swift's "default to the first registered" fallback).
mod mru {
    use std::collections::HashSet;
    use std::hash::Hash;

    /// Move `id` to the front (it just became the active window). Removes any
    /// existing occurrence first so the list never carries duplicates.
    pub(super) fn touch<K: Copy + PartialEq>(order: &mut Vec<K>, id: K) {
        order.retain(|x| *x != id);
        order.insert(0, id);
    }

    /// Append `id` to the back on registration, if not already present. Keeps a
    /// never-activated window behind already-known ones so the front stays the
    /// most-recently-keyed (or, absent any activation, the first registered).
    pub(super) fn append<K: Copy + PartialEq>(order: &mut Vec<K>, id: K) {
        if !order.iter().any(|x| *x == id) {
            order.push(id);
        }
    }

    /// Drop `id` from the order (deregistration).
    pub(super) fn remove<K: Copy + PartialEq>(order: &mut Vec<K>, id: K) {
        order.retain(|x| *x != id);
    }

    /// Choose the active window: the key window when `prefer_key` and it is still
    /// live, else the front-most live entry in `order`, else `None`. `live` is the
    /// set of registered ids (a stale key or a dropped MRU entry can never win).
    pub(super) fn select<K: Copy + Eq + Hash>(
        prefer_key: bool,
        key: Option<K>,
        order: &[K],
        live: &HashSet<K>,
    ) -> Option<K> {
        if prefer_key {
            if let Some(k) = key {
                if live.contains(&k) {
                    return Some(k);
                }
            }
        }
        order.iter().copied().find(|id| live.contains(id))
    }
}

/// App-global map of live windows to their per-window state, plus the MRU order.
#[derive(Default)]
pub(crate) struct WindowRegistry {
    /// One strong handle per live window. Keyed by gpui `WindowId`.
    entries: HashMap<WindowId, Entity<WindowState>>,
    /// MRU order; `order[0]` is the active-window fallback (see [`mru`]).
    order: Vec<WindowId>,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    /// Install the registry and its single close observer. Call once at app
    /// startup, before the first window opens. The one `on_window_closed`
    /// observer routes every window's close through [`Self::handle_window_closed`]
    /// (it carries the `WindowId`), so per-window close plumbing stays out of the
    /// views.
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(WindowRegistry::default());
        cx.on_window_closed(|cx, id| WindowRegistry::handle_window_closed(cx, id))
            .detach();
    }

    /// Register `state` as window `id`'s per-window state. Idempotent on the id.
    /// Deliberately installs no close-confirm behavior (that is R18).
    pub(crate) fn register(cx: &mut App, id: WindowId, state: Entity<WindowState>) {
        let reg = cx.default_global::<WindowRegistry>();
        reg.entries.insert(id, state);
        mru::append(&mut reg.order, id);
    }

    /// Note that window `id` became the active (key) window — moves it to the
    /// MRU front. Driven by each window's `observe_window_activation`. A no-op for
    /// an unregistered id.
    pub(crate) fn note_active(cx: &mut App, id: WindowId) {
        let reg = cx.default_global::<WindowRegistry>();
        if reg.entries.contains_key(&id) {
            mru::touch(&mut reg.order, id);
        }
    }

    /// Consumer (a): the per-window state shortcut dispatch should route to.
    /// `prefer_key` returns the current key window's state when it is registered;
    /// otherwise (and always as a fallback) the most-recently-keyed live window,
    /// else the first registered. `None` only when no window is registered.
    pub(crate) fn active_state(cx: &App, prefer_key: bool) -> Option<Entity<WindowState>> {
        Self::active_state_with_id(cx, prefer_key).map(|(_id, state)| state)
    }

    /// Like [`active_state`](Self::active_state) but also returns the chosen
    /// window's [`WindowId`], so a caller — the ⌘Q MRU fallback in
    /// [`crate::app::request_quit`] (#4 / D1) — can resolve that window's
    /// `AnyWindowHandle` via `cx.windows()` to activate it and host the quit
    /// confirmation there. Same MRU selection as [`active_state`](Self::active_state).
    pub(crate) fn active_state_with_id(
        cx: &App,
        prefer_key: bool,
    ) -> Option<(WindowId, Entity<WindowState>)> {
        let key = cx.active_window().map(|w| w.window_id());
        let reg = cx.try_global::<WindowRegistry>()?;
        let live: HashSet<WindowId> = reg.entries.keys().copied().collect();
        let chosen = mru::select(prefer_key, key, &reg.order, &live)?;
        reg.entries
            .get(&chosen)
            .cloned()
            .map(|state| (chosen, state))
    }

    /// Consumer (d): the state for a specific window id (R25 migration / direct
    /// routing).
    pub(crate) fn state_for_window(cx: &App, id: WindowId) -> Option<Entity<WindowState>> {
        cx.try_global::<WindowRegistry>()?.entries.get(&id).cloned()
    }

    /// Consumer (c): the state owning `window_session_id` (undo routing, Stage 5).
    /// Returns `None` when no live window carries that session.
    pub(crate) fn state_for_window_session_id(
        cx: &App,
        window_session_id: &str,
    ) -> Option<Entity<WindowState>> {
        // Clone the handles out first so the global borrow ends before we read
        // each entity (`Entity::read` also borrows the app).
        let handles: Vec<Entity<WindowState>> = cx
            .try_global::<WindowRegistry>()?
            .entries
            .values()
            .cloned()
            .collect();
        handles
            .into_iter()
            .find(|h| h.read(cx).window_session_id() == window_session_id)
    }

    /// The window holding the session pinned to Claude conversation
    /// `claude_session_id` — the cross-window half of the background-`/fork`
    /// parent lookup (Fix B). `session_update` is routed to the window owning the
    /// relayed window id, but a daemon-spawned fork child relays whichever window
    /// id first spawned the Claude daemon, which says nothing about where the
    /// forked conversation is actually open — so the fork path falls back to
    /// searching every live window. Modelled on
    /// [`state_for_session_id`](Self::state_for_session_id), which does the same
    /// for a WINDOW session id (this one keys on a CLAUDE session id, a
    /// per-session value). `None` when no live window carries that conversation.
    ///
    /// Must not be called while any window's [`WindowState`] is being updated:
    /// reading an entity that is currently leased panics in gpui. The fork path
    /// calls it from a spawned task, where no lease is held.
    pub(crate) fn state_for_claude_session(
        cx: &App,
        claude_session_id: &str,
    ) -> Option<Entity<WindowState>> {
        // Clone the handles out first so the global borrow ends before we read
        // each entity (`Entity::read` also borrows the app).
        let handles: Vec<Entity<WindowState>> = cx
            .try_global::<WindowRegistry>()?
            .entries
            .values()
            .cloned()
            .collect();
        handles.into_iter().find(|h| {
            h.read(cx)
                .workspace
                .session_id_for_claude_session(claude_session_id)
                .is_some()
        })
    }

    /// The number of live registered windows (used by the multi-window scenario /
    /// itests to assert open/close deltas against the real `NSWindow` count).
    pub(crate) fn count(cx: &App) -> usize {
        cx.try_global::<WindowRegistry>()
            .map_or(0, |r| r.entries.len())
    }

    /// Every live window's per-window state (W5: the ⌘Q window-count sum + the
    /// quit-cascade snapshot/teardown loop — Swift's `registry.allAppStates`).
    /// Cloned handles so the global borrow ends before the caller reads/updates
    /// each entity.
    pub(crate) fn all_states(cx: &App) -> Vec<Entity<WindowState>> {
        cx.try_global::<WindowRegistry>()
            .map(|r| r.entries.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Remove window `id` from the map + MRU, returning its state handle.
    fn deregister(cx: &mut App, id: WindowId) -> Option<Entity<WindowState>> {
        let reg = cx.default_global::<WindowRegistry>();
        mru::remove(&mut reg.order, id);
        reg.entries.remove(&id)
    }

    /// Window-close handler: route the window's disk fate (W5), tear its state
    /// down (session teardown), and quit when the last window closes.
    ///
    /// R18: the disk reason routes on [`crate::lifecycle::close_disposition`] —
    /// `AppQuitting` (quit began) or a default close ⇒ preserve the snapshot
    /// (upsert); only a confirmed red-button / ⌘W close (`user_initiated_close`)
    /// ⇒ remove the slot. `remove` MUST flush so a quit right after can't
    /// resurrect the slot from a stale debounce (both branches flush). All a
    /// no-op when no store Global is installed. Quit-when-empty keeps the
    /// single-window "close quits the app" behavior while a multi-window app
    /// survives closing one of several windows.
    pub(crate) fn handle_window_closed(cx: &mut App, id: WindowId) {
        Self::route_close_disk_fate(cx, id);
        if Self::should_quit_after_close(cx, id) {
            cx.quit();
        }
    }

    /// The quit-when-empty decision for [`handle_window_closed`](Self::handle_window_closed),
    /// split out as a testable seam (the test platform's `cx.quit()` is an
    /// unobservable no-op). Quits only when the registry holds no live window AND no
    /// Settings window OTHER than the one being closed is still live (#13 / D4).
    ///
    /// Order-independence (BUGS.md #13): two `on_window_closed` observers fire on a
    /// Settings close — this registry's and the settings module's own
    /// (`settings/window.rs`, which clears the `SettingsWindow` Global) — and their
    /// install order is not a contract. So when the closing window IS the Settings
    /// window, [`current_settings_window`](crate::settings::window::current_settings_window)
    /// may still return its (now-closing) handle; the `window_id() != closed_id`
    /// guard treats that as "not live" so the last window closing (terminal OR
    /// Settings) still quits, exactly as before.
    fn should_quit_after_close(cx: &App, closed_id: WindowId) -> bool {
        let registry_empty = cx
            .try_global::<WindowRegistry>()
            .map_or(true, |r| r.entries.is_empty());
        let settings_live = crate::settings::window::current_settings_window(cx)
            .is_some_and(|h| h.window_id() != closed_id);
        // tmux-port Phase 4 (D5): a detached session with a live child keeps the
        // app alive window-less, the way a tmux server outlives its clients.
        // Quitting here would SIGHUP exactly what detach promised to preserve.
        let pool_live = crate::detached_pool::pool_has_live(cx);
        should_quit_on_window_close(registry_empty, settings_live, pool_live)
    }

    /// The disk-fate + teardown half of [`handle_window_closed`], WITHOUT the
    /// quit-when-empty: deregister the window, route its disk fate
    /// ([`crate::lifecycle::close_disposition`] — remove+flush on a confirmed user
    /// close, else preserve+flush), and tear its sessions down (pty reap). The
    /// `persistence-restore` scenario installs a SCOPED `on_window_closed` observer
    /// calling THIS (never [`handle_window_closed`]) so its window close routes the
    /// real disk fate + reaps its ptys without the quit-when-empty that would kill
    /// the suite (it registers the `WindowRegistry` WITHOUT `install`).
    pub(crate) fn route_close_disk_fate(cx: &mut App, id: WindowId) {
        if let Some(state) = Self::deregister(cx, id) {
            let app_quitting = cx.has_global::<crate::lifecycle::AppQuitting>();
            // tmux-port Phase 4 (D1/§P5): move this window's detach-eligible
            // sessions into the app-global pool BEFORE the snapshot is read, so
            // the snapshot describes only what is left behind. The pool push
            // writes the bucket into the store CACHE on the spot (write-through),
            // and the window slot's Remove/Preserve lands in the same batch — the
            // ONE existing flush below then covers both. That ordering is
            // crash-consistency load-bearing: a second flush after the pool write
            // would open a window in which a hard kill leaves the detached
            // sessions in NEITHER `windows[]` NOR `detached[]`.
            Self::detach_eligible_sessions_into_pool(cx, &state, app_quitting);
            let (user_initiated, snapshot) = {
                let s = state.read(cx);
                (s.user_initiated_close(), s.persisted_snapshot())
            };
            match crate::lifecycle::close_disposition(app_quitting, user_initiated) {
                crate::lifecycle::CloseDisposition::Remove => {
                    crate::session_store::remove(&snapshot.id)
                }
                crate::lifecycle::CloseDisposition::Preserve => {
                    crate::session_store::upsert(snapshot)
                }
            }
            crate::session_store::flush();
            state.update(cx, |s, _cx| s.teardown());
        }
    }

    /// The close-path detach partition (plan §D1/§P2/§P5): move every
    /// detach-eligible session of the closing window into the app-global pool,
    /// leaving the model-dead ones behind for today's teardown.
    ///
    /// Three gates, each a deliberate no-op rather than a special case:
    ///
    /// * **Quit** (`app_quitting`) is untouched. `AppQuitting` still `Preserve`s
    ///   whole windows, so sessions attached at quit stay in their windows and
    ///   restore there; the pool persists only what was already detached.
    /// * **No pool installed** ⇒ nothing happens. `run_selftest` and the
    ///   scenarios build no pool (the hermeticity rule), so every landed close
    ///   test keeps its pre-Phase-4 behavior byte-for-byte — including the
    ///   `persistence-restore` scenario, which drives this function directly.
    /// * **The setting is OFF** ⇒ nothing happens: the user asked for the
    ///   pre-Phase-4 confirm-then-kill flow, and `request_window_close` has
    ///   already presented the confirmation that goes with it.
    ///
    /// Sessions are taken in REVERSE sidebar order and each pushed onto the pool
    /// head, so the pool ends up carrying this window's sessions in the order the
    /// sidebar showed them.
    fn detach_eligible_sessions_into_pool(
        cx: &mut App,
        state: &Entity<WindowState>,
        app_quitting: bool,
    ) {
        if app_quitting || !crate::detached_pool::detach_on_close_enabled(cx) {
            return;
        }
        let mut eligible =
            crate::detached_pool::detach_eligible_session_ids(&state.read(cx).workspace);
        eligible.reverse();
        for session_id in eligible {
            crate::detached_pool::detach_into_pool(cx, state, &session_id);
        }
    }
}

/// Pure quit-when-empty predicate (BUGS.md #13, D4; tmux-port Phase 4 D5): the
/// app quits on a window close only when the [`WindowRegistry`] holds no
/// remaining live window, no Settings window is still live, AND the detached pool
/// holds no live session. Split from
/// [`WindowRegistry::should_quit_after_close`] (which reads the app to derive all
/// three inputs) so the decision is unit-testable without a window.
///
/// `pool_live` is the D5 term: detach must never kill, so closing the last window
/// while a pooled child is still running leaves Nice running dock-only rather
/// than SIGHUPing it. A pool holding only STRUCTURAL rows (nothing running)
/// contributes nothing — those cost no process, and hanging the app on them would
/// make a stale persisted row unquittable.
pub(crate) fn should_quit_on_window_close(
    registry_empty: bool,
    settings_live: bool,
    pool_live: bool,
) -> bool {
    registry_empty && !settings_live && !pool_live
}

#[cfg(test)]
mod tests {
    use super::mru;
    use std::collections::HashSet;

    fn live(ids: &[u64]) -> HashSet<u64> {
        ids.iter().copied().collect()
    }

    #[test]
    fn append_preserves_registration_order_and_dedups() {
        let mut order: Vec<u64> = Vec::new();
        mru::append(&mut order, 1);
        mru::append(&mut order, 2);
        mru::append(&mut order, 3);
        mru::append(&mut order, 2); // already present — no-op
        assert_eq!(order, vec![1, 2, 3], "appended to the back, no duplicate");
    }

    #[test]
    fn touch_moves_to_front_without_duplicating() {
        let mut order = vec![1u64, 2, 3];
        mru::touch(&mut order, 3);
        assert_eq!(order, vec![3, 1, 2], "the activated window is now front-most");
        mru::touch(&mut order, 3); // already front — stays, no dup
        assert_eq!(order, vec![3, 1, 2]);
        mru::touch(&mut order, 9); // never-seen id is inserted at the front
        assert_eq!(order, vec![9, 3, 1, 2]);
    }

    #[test]
    fn remove_drops_the_id() {
        let mut order = vec![3u64, 1, 2];
        mru::remove(&mut order, 1);
        assert_eq!(order, vec![3, 2]);
        mru::remove(&mut order, 42); // absent — no-op
        assert_eq!(order, vec![3, 2]);
    }

    #[test]
    fn select_prefers_the_key_window_when_registered() {
        let order = vec![3u64, 1, 2];
        let l = live(&[1, 2, 3]);
        assert_eq!(mru::select(true, Some(2), &order, &l), Some(2));
    }

    #[test]
    fn select_falls_back_to_mru_when_key_is_unregistered() {
        // A key window that isn't ours (e.g. a future Settings window, or none)
        // must not win — dispatch routes to the most-recently-keyed Nice window.
        let order = vec![3u64, 1, 2];
        let l = live(&[1, 2, 3]);
        assert_eq!(mru::select(true, Some(99), &order, &l), Some(3));
        assert_eq!(mru::select(true, None, &order, &l), Some(3));
    }

    #[test]
    fn select_without_prefer_key_uses_mru_front() {
        let order = vec![3u64, 1, 2];
        let l = live(&[1, 2, 3]);
        assert_eq!(mru::select(false, Some(2), &order, &l), Some(3));
    }

    #[test]
    fn select_skips_stale_mru_entries_and_returns_none_when_empty() {
        // MRU front points at a window that has since deregistered → skip it.
        let order = vec![5u64, 3, 1];
        let l = live(&[1, 3]);
        assert_eq!(mru::select(false, None, &order, &l), Some(3));
        // No live windows at all.
        assert_eq!(mru::select(true, Some(1), &[], &live(&[])), None);
    }

    #[test]
    fn select_models_the_close_fallback_to_most_recently_keyed() {
        // Validation §3: B is key, then B closes; dispatch falls back to the most
        // recently keyed *surviving* window (A).
        let mut order = vec![1u64]; // A registered first
        mru::append(&mut order, 2); // B registered
        mru::touch(&mut order, 2); // B keyed → front
        assert_eq!(order, vec![2, 1]);
        // B closes.
        mru::remove(&mut order, 2);
        let l = live(&[1]);
        assert_eq!(mru::select(true, None, &order, &l), Some(1));
    }

    // ===================================================================
    // #13 / D4 — gated quit-when-empty
    // ===================================================================

    #[test]
    fn should_quit_only_when_registry_empty_and_no_settings_live() {
        use super::should_quit_on_window_close as q;
        // The only quitting combination: no registered window left, no Settings
        // window still open, and no live pooled session — today's single-window
        // "close quits the app".
        assert!(q(true, false, false), "empty registry, no Settings ⇒ quit");
        // Settings still open ⇒ never quit, even with an empty registry (#13: closing
        // the last terminal window must not take Settings down mid-use).
        assert!(!q(true, true, false), "empty registry but Settings live ⇒ stay");
        // Another registered window survives ⇒ never quit, Settings or not.
        assert!(!q(false, false, false), "registered windows remain ⇒ stay");
        assert!(!q(false, true, false), "registered windows remain + Settings ⇒ stay");
    }

    /// tmux-port Phase 4 (D5): a live detached session keeps the app alive
    /// window-less. It is an independent veto — it holds with no windows and no
    /// Settings, which is exactly the state closing the last window lands in.
    #[test]
    fn a_live_detached_pool_keeps_the_app_alive_window_less() {
        use super::should_quit_on_window_close as q;
        assert!(
            !q(true, false, true),
            "last window closed but a pooled child is still running ⇒ stay alive"
        );
        assert!(
            !q(false, false, true),
            "a live pool never quits, windows or not"
        );
        // An EMPTY-or-structural-only pool changes nothing: no process is at
        // risk, so the app must still quit rather than hang on a persisted row.
        assert!(
            q(true, false, false),
            "a pool with nothing running does not hold the app open"
        );
    }

    /// The decision seam [`super::WindowRegistry::should_quit_after_close`] derives
    /// `registry_empty` + `settings_live` from the app and gates on the pure
    /// predicate. Driven on a `TestAppContext` because the test platform's
    /// `cx.quit()` is an unobservable no-op (per the plan: assert the seam, don't
    /// build a quit-spy). Covers #13's order-independence: closing the last terminal
    /// window while Settings is live must NOT quit, yet closing Settings itself as
    /// the last live window still quits.
    #[gpui::test]
    fn should_quit_after_close_gates_on_a_live_settings_window(cx: &mut gpui::TestAppContext) {
        use super::WindowRegistry;

        // Two bare windows: one stands in for the Settings window, one for a just-
        // closed terminal window. The registry stays EMPTY throughout (both the
        // "last terminal closed" and "Settings closed last" cases start from empty).
        let settings_win = cx.add_window(|_w, _cx| gpui::Empty);
        let other_win = cx.add_window(|_w, _cx| gpui::Empty);

        cx.update(|app| {
            app.set_global(WindowRegistry::default());

            // No Settings window open: closing the last window quits, as it always
            // has (registry empty + no Settings live).
            assert!(
                WindowRegistry::should_quit_after_close(app, other_win.window_id()),
                "empty registry with no Settings open still quits on the last close"
            );

            // Settings window now live (its handle stored in the SettingsWindow Global).
            crate::settings::window::force_settings_handle_for_scenario(
                app,
                gpui::AnyWindowHandle::from(settings_win),
            );

            // Closing the last TERMINAL window while Settings is open must NOT quit
            // (#13) — Settings' id differs from the closing window's id ⇒ settings_live.
            assert!(
                !WindowRegistry::should_quit_after_close(app, other_win.window_id()),
                "an empty registry with Settings still open does not quit"
            );

            // Closing SETTINGS itself as the last live window still quits: the id-guard
            // treats the window being closed as not-live regardless of whether the
            // settings-close observer has cleared the Global yet (order-independent).
            assert!(
                WindowRegistry::should_quit_after_close(app, settings_win.window_id()),
                "closing Settings as the last live window still quits"
            );
        });
    }
}

// ===========================================================================
// tmux-port Phase 4 — the close-path detach partition (plan §D1/§P2/§P5)
// ===========================================================================

#[cfg(test)]
mod detach_close_tests {
    use std::path::PathBuf;

    use gpui::AppContext;
    use nice_model::{Session, TermWindow, TermWindowKind, WorkspaceModel};
    use nice_term_core::SpawnSpec;

    use super::WindowRegistry;
    use crate::detached_pool::DetachedPoolGlobal;
    use crate::session_store::{self, SessionStore};
    use crate::settings::prefs_store::SettingsPrefsStore;
    use crate::window_state::WindowState;

    /// A scratch dir for one case's `sessions.json` + `ui_settings.json`.
    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("nice-p4-close-{name}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// A cheap hermetic child that sits on its pty until its `PaneState` drops —
    /// so "the pool got it before `teardown` ran" is observable as a process that
    /// is still running.
    fn fixture_spec() -> SpawnSpec {
        SpawnSpec::shell(std::env::temp_dir().to_string_lossy().to_string()).with_argv(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ])
    }

    /// A window model with the seeded Terminals/Main session (model-ALIVE) plus a
    /// second project holding one HELD session (model-dead) — the two sides of
    /// the §P2 partition in one window.
    fn model_with_a_live_and_a_held_session() -> WorkspaceModel {
        let mut model = WorkspaceModel::new("/tmp");
        let pi = model.ensure_project("proj", "Project", "/tmp/proj");
        let mut held = Session::new("held", "Held", "/tmp/proj");
        let mut window = TermWindow::new("held-w", "Terminal 1", TermWindowKind::Terminal);
        window.is_alive = false;
        held.active_window_id = Some(window.id.clone());
        held.windows = vec![window];
        model.projects[pi].sessions.push(held);
        model
    }

    /// The `sessions.json` actually on disk (never the cache) — the whole point
    /// of the ordering pin is that the flush already carried the pool.
    fn on_disk(path: &PathBuf) -> serde_json::Value {
        let text = std::fs::read_to_string(path).expect("sessions.json was flushed");
        serde_json::from_str(&text).expect("sessions.json is valid JSON")
    }

    fn detached_ids(doc: &serde_json::Value) -> Vec<String> {
        doc["detached"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|r| r["session"]["id"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// §P5's pinned ordering, asserted through its three observable
    /// consequences at once:
    ///
    /// 1. the eligible session is in the POOL and out of the window's model;
    /// 2. its child is STILL RUNNING after the close — so the extraction beat
    ///    `WindowState::teardown`, which would have SIGHUPed it;
    /// 3. the flushed `sessions.json` already carries the `detached` bucket AND
    ///    has dropped the window slot — so the pool write landed in the cache
    ///    before the ONE flush, not after it. A second flush would open the
    ///    crash window in which the session is in neither bucket.
    ///
    /// The model-dead `held` session is left behind for today's `Remove` fate.
    #[gpui::test]
    fn close_path_detaches_eligible_sessions_before_the_single_flush(cx: &mut gpui::TestAppContext) {
        let _serialize = session_store::global_test_lock();
        let dir = scratch("flush-order");
        let store_path = dir.join("sessions.json");
        session_store::clear_global();
        let _store = session_store::install_global(SessionStore::open(store_path.clone()));

        let win = cx.add_window(|_w, _cx| gpui::Empty);
        cx.update(|app| {
            app.set_global(WindowRegistry::default());
            crate::detached_pool::install(app);
            app.set_global(SettingsPrefsStore::with_defaults(dir.join("ui_settings.json")));

            let state = app.new(|_cx| WindowState::with_model(model_with_a_live_and_a_held_session()));
            let main_window_id = state
                .read(app)
                .workspace
                .session_for("terminals-main")
                .and_then(|s| s.active_window_id.clone())
                .expect("the seeded Main session has a window");
            let handle = state
                .update(app, |ws, cx| {
                    ws.ptys.set_event_wakes_enabled_for_test(false);
                    ws.ptys
                        .spawn_window("terminals-main", &main_window_id, fixture_spec(), cx)
                        .unwrap();
                    ws.ptys.pane_handle("terminals-main", &main_window_id, &main_window_id)
                })
                .expect("the Main pane spawned");
            // The D1 no-confirm close branch sets this before the window goes.
            state.update(app, |ws, _| ws.set_user_initiated_close(true));
            WindowRegistry::register(app, win.window_id(), state.clone());

            WindowRegistry::route_close_disk_fate(app, win.window_id());

            // (1) pooled, and gone from the window; the held session stayed.
            let pool = app.global::<DetachedPoolGlobal>().0.clone();
            let pooled: Vec<String> = pool
                .read(app)
                .entries()
                .iter()
                .map(|e| e.session.id.clone())
                .collect();
            // Re-keyed on the way out (`WindowState::detach_session`): the pool is
            // app-global, so a row cannot keep the `terminals-main` that EVERY
            // window seeds — it would collide with the next window to adopt it.
            assert_eq!(pooled.len(), 1, "exactly one detached row: {pooled:?}");
            assert_ne!(pooled[0], "terminals-main", "re-keyed on the way out");
            let detached_id = pooled[0].clone();
            assert!(
                state.read(app).workspace.session_for("terminals-main").is_none(),
                "the detached session left the window's model"
            );
            assert!(
                state.read(app).workspace.session_for("held").is_some(),
                "a model-dead session is NOT detached — it keeps today's Remove fate"
            );

            // (2) the child outlived teardown.
            assert!(
                handle.read(app).session().try_status().is_none(),
                "the pooled child must still be running after the window closed"
            );

            // (3) the ONE flush already carried both writes.
            let doc = on_disk(&store_path);
            assert_eq!(
                detached_ids(&doc),
                vec![detached_id],
                "the detached bucket was in the cache before the flush"
            );
            assert!(
                doc["windows"].as_array().map_or(true, |w| w.is_empty()),
                "the confirmed close removed the window slot in the same batch"
            );

            pool.update(app, |p, cx| p.clear_live(cx));
        });
        session_store::clear_global();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Setting OFF ⇒ the pre-Phase-4 path, byte for byte: nothing is pooled, the
    /// window slot is still removed by the confirmed close, and the file carries
    /// no `detached` bucket at all.
    #[gpui::test]
    fn close_path_with_detach_off_keeps_todays_behavior(cx: &mut gpui::TestAppContext) {
        let _serialize = session_store::global_test_lock();
        let dir = scratch("setting-off");
        let store_path = dir.join("sessions.json");
        session_store::clear_global();
        let _store = session_store::install_global(SessionStore::open(store_path.clone()));

        let win = cx.add_window(|_w, _cx| gpui::Empty);
        cx.update(|app| {
            app.set_global(WindowRegistry::default());
            crate::detached_pool::install(app);
            let mut prefs = SettingsPrefsStore::with_defaults(dir.join("ui_settings.json"));
            let _ = prefs.set_close_window_detaches(false);
            app.set_global(prefs);

            let state = app.new(|_cx| WindowState::with_model(model_with_a_live_and_a_held_session()));
            // Persist the window first, so "the confirmed close DROPS the slot"
            // is a visible transition rather than a slot that was never there.
            state.update(app, |ws, _| ws.save_to_store());
            session_store::flush();
            assert!(
                !on_disk(&store_path)["windows"].as_array().unwrap().is_empty(),
                "precondition: the window has a persisted slot"
            );
            state.update(app, |ws, _| ws.set_user_initiated_close(true));
            WindowRegistry::register(app, win.window_id(), state.clone());

            WindowRegistry::route_close_disk_fate(app, win.window_id());

            let pool = app.global::<DetachedPoolGlobal>().0.clone();
            assert!(
                pool.read(app).is_empty(),
                "with the setting OFF the close kills as it always did"
            );
            assert!(
                state.read(app).workspace.session_for("terminals-main").is_some(),
                "no session was extracted from the model"
            );
            let doc = on_disk(&store_path);
            assert!(detached_ids(&doc).is_empty(), "no detached bucket is written");
            assert!(
                doc["windows"].as_array().map_or(true, |w| w.is_empty()),
                "the confirmed close still drops the window slot"
            );
        });
        session_store::clear_global();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Quit is untouched (§P5): with `AppQuitting` latched the close `Preserve`s
    /// the WHOLE window, so sessions attached at quit stay in `windows[]` and
    /// restore there — the pool persists only what was already detached.
    #[gpui::test]
    fn quit_preserves_attached_sessions_and_never_detaches(cx: &mut gpui::TestAppContext) {
        let _serialize = session_store::global_test_lock();
        let dir = scratch("quit-parity");
        let store_path = dir.join("sessions.json");
        session_store::clear_global();
        let _store = session_store::install_global(SessionStore::open(store_path.clone()));

        let win = cx.add_window(|_w, _cx| gpui::Empty);
        cx.update(|app| {
            app.set_global(WindowRegistry::default());
            crate::detached_pool::install(app);
            app.set_global(SettingsPrefsStore::with_defaults(dir.join("ui_settings.json")));
            app.set_global(crate::lifecycle::AppQuitting);

            // One row was ALREADY detached before the quit began.
            let pool = app.global::<DetachedPoolGlobal>().0.clone();
            let mut pooled = Session::new("pre-detached", "Pooled", "/tmp/work");
            pooled.windows = vec![TermWindow::new(
                "pre-detached-w",
                "Terminal 1",
                TermWindowKind::Terminal,
            )];
            pooled.active_window_id = Some("pre-detached-w".into());
            pool.update(app, |p, cx| {
                p.push_front(
                    crate::detached_pool::DetachedEntry::structural(
                        pooled,
                        crate::detached_pool::DetachedProject {
                            id: "proj".into(),
                            name: "Project".into(),
                            path: "/tmp/proj".into(),
                        },
                    ),
                    cx,
                )
            });

            let state = app.new(|_cx| WindowState::with_model(model_with_a_live_and_a_held_session()));
            WindowRegistry::register(app, win.window_id(), state.clone());
            WindowRegistry::route_close_disk_fate(app, win.window_id());

            assert_eq!(
                pool.read(app).len(),
                1,
                "quit adds nothing to the pool — attached sessions stay attached"
            );
            assert!(
                state.read(app).workspace.session_for("terminals-main").is_some(),
                "the quit close extracts nothing from the model"
            );
            let doc = on_disk(&store_path);
            assert_eq!(
                detached_ids(&doc),
                vec!["pre-detached".to_string()],
                "the pool persists exactly what was already detached"
            );
            let window_sessions: Vec<String> = doc["windows"][0]["projects"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|p| p["tabs"].as_array().cloned().unwrap_or_default())
                .map(|s| s["id"].as_str().unwrap_or_default().to_string())
                .collect();
            assert!(
                window_sessions.contains(&"terminals-main".to_string()),
                "an attached session is preserved under windows[]: {window_sessions:?}"
            );
        });
        session_store::clear_global();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
