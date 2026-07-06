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
//!      [`WindowRegistry::state_for_session_id`].
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
// `state_for_window` / `state_for_session_id` (consumers c/d), `count`, and the
// pure `mru::select` they lean on — has no in-crate caller until R12's keymap
// slice routes shortcuts through `active_state` and the multi-window scenario /
// itests assert `count`. The plan requires all four consumers to EXIST now
// (dossier G5); the write side (register / note_active / close hook) is live.
// Same "seam ahead of its caller" pattern as `sidebar_actions` / `pane_strip_actions`.
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
        let key = cx.active_window().map(|w| w.window_id());
        let reg = cx.try_global::<WindowRegistry>()?;
        let live: HashSet<WindowId> = reg.entries.keys().copied().collect();
        let chosen = mru::select(prefer_key, key, &reg.order, &live)?;
        reg.entries.get(&chosen).cloned()
    }

    /// Consumer (d): the state for a specific window id (R25 migration / direct
    /// routing).
    pub(crate) fn state_for_window(cx: &App, id: WindowId) -> Option<Entity<WindowState>> {
        cx.try_global::<WindowRegistry>()?.entries.get(&id).cloned()
    }

    /// Consumer (c): the state owning `session_id` (undo routing, Stage 5).
    /// Returns `None` when no live window carries that session.
    pub(crate) fn state_for_session_id(
        cx: &App,
        session_id: &str,
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
            .find(|h| h.read(cx).session_id() == session_id)
    }

    /// The number of live registered windows (used by the multi-window scenario /
    /// itests to assert open/close deltas against the real `NSWindow` count).
    pub(crate) fn count(cx: &App) -> usize {
        cx.try_global::<WindowRegistry>()
            .map_or(0, |r| r.entries.len())
    }

    /// Every live window's per-window state (W5: the ⌘Q pane-count sum + the
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
        let empty = cx
            .try_global::<WindowRegistry>()
            .map_or(true, |r| r.entries.is_empty());
        if empty {
            cx.quit();
        }
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
}
