//! W5 quit / window-close lifecycle — the Rust twin of Swift's
//! `AppDelegate` + `SessionLifecycleController` + `WindowRegistry`
//! termination ordering (`AppDelegate.swift`, `SessionLifecycleController.swift`,
//! `WindowRegistryTerminationOrderingTests.swift`).
//!
//! Nice owns quit: gpui cannot veto macOS terminate
//! (`gpui_macos/src/platform.rs`; `on_app_quit` is non-cancelable), so the
//! confirmation lives on a `Quit` action + `cmd-q` + an app-menu item, and on a
//! `CloseWindow` action + `cmd-w` + `Window::on_window_should_close` for the red
//! traffic light. The gpui-side wiring (actions, menus, modal presentation,
//! `quit_cascade`) lives in [`crate::app`]; this module owns the two pure,
//! table-tested cores those callers lean on:
//!
//!   * [`close_disposition`] — the reason routing (Swift's `TearDownReason`): a
//!     window's disk fate on close, given the process `AppQuitting` state and its
//!     per-window `user_initiated_close` flag.
//!   * the alert-copy builders ([`describe_live_panes`] / [`quit_dialog_copy`] /
//!     [`close_dialog_copy`]) — the verbatim wording from
//!     `AppDelegate.QuitConfirmation` (`:112-153`), which Swift never unit-pinned
//!     and Rust does.
//!
//! plus the process [`AppQuitting`] gpui global (the "close events are inert once
//! quit begins" latch — Swift's detach-observers-before-teardown invariant).

// `close_disposition` + the copy builders are consumed by `crate::app` (the
// quit/close handlers) and `crate::window_registry` (`handle_window_closed`
// routing); `AppQuitting` is set by `quit_cascade`. The pure cores below are
// exercised by this module's `#[test]`s.
#![allow(dead_code)]

use gpui::Global;

/// The process-wide "quit has begun" latch. Set FIRST by `quit_cascade` (before
/// any window is snapshotted / torn down): from then on
/// `Window::on_window_should_close` returns `true` unconditionally and every
/// window close is treated as app-terminating (preserve), never user-closed
/// (remove). This is Swift's detach-observers-before-teardown invariant
/// (`SessionLifecycleController.swift:42-50` — getting it wrong once wiped a
/// window's tabs on quit in production). A marker global — presence is the
/// signal.
pub(crate) struct AppQuitting;

impl Global for AppQuitting {}

/// A closing window's fate on disk — the Rust twin of Swift's
/// `WindowSession.TearDownReason`. The two close paths must diverge: a window
/// the user explicitly closed should not reappear next launch
/// ([`Remove`](CloseDisposition::Remove)), while a window merely open at quit
/// time should ([`Preserve`](CloseDisposition::Preserve)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseDisposition {
    /// Drop the slot from `sessions.json` — a genuine red-button / ⌘W close.
    Remove,
    /// Persist the latest snapshot — app terminating, OR a close that never set
    /// the intent flag (the safer failure mode: a forgotten flag preserves the
    /// window rather than silently dropping it).
    Preserve,
}

/// Route a closing window to its disk disposition (Swift's
/// `SessionLifecycleController.handleWindowWillClose` reason routing).
///
/// `app_quitting` (the [`AppQuitting`] global) forces [`Preserve`] unconditionally
/// — once quit begins, every close is app-terminating, never user-closed. This is
/// THE motivating regression: a `willClose` burst landing after `quit_cascade`
/// snapshotted a window must not `remove` the snapshot we just upserted
/// (`WindowRegistryTerminationOrderingTests.test_willTerminate_thenWillClose…`).
///
/// Otherwise the per-window `user_initiated_close` flag — set ONLY by the
/// confirmed red-button / ⌘W path — selects [`Remove`]; the default is
/// [`Preserve`].
///
/// [`Preserve`]: CloseDisposition::Preserve
/// [`Remove`]: CloseDisposition::Remove
pub(crate) fn close_disposition(app_quitting: bool, user_initiated_close: bool) -> CloseDisposition {
    if app_quitting {
        return CloseDisposition::Preserve;
    }
    if user_initiated_close {
        CloseDisposition::Remove
    } else {
        CloseDisposition::Preserve
    }
}

/// One confirmation dialog's copy — the generic modal's `(title, message,
/// confirm_label)` for a given live-pane count. R18's quit/close callers pass
/// `cancel_label = "Cancel"`, `destructive_confirm = false` alongside these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DialogCopy {
    /// The modal's title (Swift's `NSAlert.messageText`).
    pub title: String,
    /// The modal's body (Swift's `informativeText`).
    pub message: String,
    /// The confirm button's label.
    pub confirm_label: String,
}

/// The informative-text body describing the live panes — Swift's
/// `QuitConfirmation.describe(claude:terminal:)` (`AppDelegate.swift:132-152`),
/// verbatim. Parts are `"N Claude session(s)"` / `"N terminal(s)"`, joined with
/// `" and "`; the trailing sentence depends on which kinds are present (Claude
/// sessions are saved for next launch, terminals are closed).
///
/// Precondition (the callers guarantee it): `claude + terminal > 0` — the
/// zero-pane path never opens a dialog.
pub(crate) fn describe_live_panes(claude: usize, terminal: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if claude > 0 {
        parts.push(format!(
            "{claude} Claude session{}",
            if claude == 1 { "" } else { "s" }
        ));
    }
    if terminal > 0 {
        parts.push(format!(
            "{terminal} terminal{}",
            if terminal == 1 { "" } else { "s" }
        ));
    }
    let list = parts.join(" and ");
    if claude > 0 && terminal > 0 {
        format!(
            "You still have {list} open. Claude sessions will be saved for next \
             launch; terminals will be closed."
        )
    } else if claude > 0 {
        format!("You still have {list} open. They will be saved for next launch.")
    } else {
        format!("You still have {list} open. They will be closed.")
    }
}

/// The ⌘Q / Quit-menu confirmation copy (`AppDelegate.swift:43-48`): title
/// `"Quit NICE?"`, confirm `"Quit"`.
pub(crate) fn quit_dialog_copy(claude: usize, terminal: usize) -> DialogCopy {
    DialogCopy {
        title: "Quit NICE?".to_string(),
        message: describe_live_panes(claude, terminal),
        confirm_label: "Quit".to_string(),
    }
}

/// The red-button / ⌘W confirmation copy (`AppDelegate.swift:80-84`): title
/// `"Close this window?"`, confirm `"Close"`.
pub(crate) fn close_dialog_copy(claude: usize, terminal: usize) -> DialogCopy {
    DialogCopy {
        title: "Close this window?".to_string(),
        message: describe_live_panes(claude, terminal),
        confirm_label: "Close".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use nice_model::{PersistedProject, PersistedTab};

    use crate::session_store::{read_state, PersistedWindow, SessionStore, DiskIo, DEBOUNCE};

    // MARK: - close_disposition (the reason-routing core)
    // Ports SessionLifecycleControllerTests + WindowRegistryTerminationOrderingTests,
    // which collapse onto this single decision + the store sequence below.

    #[test]
    fn user_intent_without_quit_removes() {
        // `windowShouldClose` flipped the flag on confirm → drop the slot.
        assert_eq!(
            close_disposition(false, true),
            CloseDisposition::Remove,
            "userInitiatedClose=true (not quitting) routes to .userClosedWindow → remove"
        );
    }

    #[test]
    fn default_without_intent_preserves() {
        // No flag, not quitting — the safer failure mode is preserve.
        assert_eq!(
            close_disposition(false, false),
            CloseDisposition::Preserve,
            "without the intent flag the snapshot survives (safer failure mode)"
        );
    }

    #[test]
    fn quitting_always_preserves_even_with_intent() {
        // THE regression: a willClose burst after quit began must NOT remove the
        // snapshot quit_cascade just upserted — AppQuitting forces preserve even
        // if the per-window intent flag happens to be set.
        assert_eq!(
            close_disposition(true, true),
            CloseDisposition::Preserve,
            "once quit begins every close is app-terminating (preserve), never remove"
        );
        assert_eq!(close_disposition(true, false), CloseDisposition::Preserve);
    }

    // MARK: - the store sequence (LifecycleController / TerminationOrdering)
    //
    // The Swift suites drive NSWindow willClose/willTerminate notifications; the
    // Rust collapse tests the disposition applied to a real SessionStore (no gpui,
    // no global — a locally-owned store, race-free under libtest).

    /// A minimal one-tab window snapshot with id `id`.
    fn window(id: &str) -> PersistedWindow {
        PersistedWindow {
            id: id.to_string(),
            active_tab_id: Some("t".to_string()),
            sidebar_collapsed: false,
            sidebar_mode: None,
            projects: vec![PersistedProject {
                id: "terminals".to_string(),
                name: "Terminals".to_string(),
                path: "/tmp".to_string(),
                tabs: vec![PersistedTab {
                    id: "t".to_string(),
                    title: "t".to_string(),
                    cwd: "/tmp".to_string(),
                    claude_session_id: None,
                    active_pane_id: None,
                    panes: vec![],
                    title_manually_set: None,
                    parent_tab_id: None,
                    next_terminal_index: None,
                }],
            }],
            frame: None,
        }
    }

    /// Apply a close disposition to the store the way `handle_window_closed`
    /// does: remove or upsert, then flush (remove MUST flush so a quit right
    /// after can't resurrect the slot from a stale debounce).
    fn apply_close(store: &SessionStore, snapshot: PersistedWindow, disposition: CloseDisposition) {
        match disposition {
            CloseDisposition::Remove => store.remove(&snapshot.id),
            CloseDisposition::Preserve => store.upsert(snapshot),
        }
        store.flush();
    }

    fn disk_store(path: &Path) -> SessionStore {
        SessionStore::open_with(path.to_path_buf(), None, Box::new(DiskIo), DEBOUNCE)
    }

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nice-lifecycle-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// The quit cascade: AppQuitting is set, both windows snapshotted+flushed,
    /// then a willClose burst arrives. With `app_quitting=true` the burst
    /// preserves — both snapshots must survive (the prod wipe regression).
    #[test]
    fn quit_then_close_burst_preserves_every_snapshot() {
        let dir = scratch();
        let path = dir.0.join("sessions.json");
        let store = disk_store(&path);

        // quit_cascade's snapshot+flush half.
        store.upsert(window("win-A"));
        store.upsert(window("win-B"));
        store.flush();

        // The scene-teardown willClose burst, now that AppQuitting is set.
        for id in ["win-A", "win-B"] {
            apply_close(&store, window(id), close_disposition(true, true));
        }

        let state = read_state(&path);
        let mut ids: Vec<String> = state.windows.iter().map(|w| w.id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["win-A", "win-B"],
            "both windows survive the ⌘Q dance — next launch sees both snapshots"
        );
    }

    /// A genuine user close removes the slot; a subsequent app-quit preserves the
    /// survivor (Swift's `test_userCloseThenAppQuit_keepsOnlyTheSurvivor`).
    #[test]
    fn user_close_then_quit_keeps_only_the_survivor() {
        let dir = scratch();
        let path = dir.0.join("sessions.json");
        let store = disk_store(&path);

        // Both snapshots pre-seeded (a tab mutation pushed each).
        store.upsert(window("win-closed"));
        store.upsert(window("win-surviving"));
        store.flush();

        // User clicks the red traffic light on `win-closed` (intent flag set).
        apply_close(&store, window("win-closed"), close_disposition(false, true));
        assert_eq!(
            read_state(&path).windows.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
            vec!["win-surviving"],
            "after the user close only the survivor remains on disk"
        );

        // ⌘Q with the survivor still open: quit snapshots+flushes it.
        store.upsert(window("win-surviving"));
        store.flush();
        // The willClose burst for the survivor, AppQuitting set → preserve.
        apply_close(&store, window("win-surviving"), close_disposition(true, false));
        assert_eq!(
            read_state(&path).windows.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
            vec!["win-surviving"],
            "survivor's snapshot remains after the terminate cascade — the prod regression"
        );
    }

    /// A willClose without the intent flag (the belt-and-suspenders path) must
    /// preserve — never remove.
    #[test]
    fn close_without_intent_preserves_snapshot() {
        let dir = scratch();
        let path = dir.0.join("sessions.json");
        let store = disk_store(&path);
        store.upsert(window("win-no-intent"));
        store.flush();

        apply_close(&store, window("win-no-intent"), close_disposition(false, false));

        assert!(
            read_state(&path).windows.iter().any(|w| w.id == "win-no-intent"),
            "willClose without userInitiatedClose preserves (routes via appTerminating)"
        );
    }

    // MARK: - alert copy (NEW — Swift never unit-pinned these strings)

    #[test]
    fn describe_pluralizes_and_joins() {
        assert_eq!(
            describe_live_panes(1, 0),
            "You still have 1 Claude session open. They will be saved for next launch."
        );
        assert_eq!(
            describe_live_panes(2, 0),
            "You still have 2 Claude sessions open. They will be saved for next launch."
        );
        assert_eq!(
            describe_live_panes(0, 1),
            "You still have 1 terminal open. They will be closed."
        );
        assert_eq!(
            describe_live_panes(0, 3),
            "You still have 3 terminals open. They will be closed."
        );
        assert_eq!(
            describe_live_panes(1, 1),
            "You still have 1 Claude session and 1 terminal open. Claude sessions \
             will be saved for next launch; terminals will be closed."
        );
        assert_eq!(
            describe_live_panes(2, 3),
            "You still have 2 Claude sessions and 3 terminals open. Claude sessions \
             will be saved for next launch; terminals will be closed."
        );
    }

    #[test]
    fn quit_and_close_copy_titles_and_confirm_labels() {
        let q = quit_dialog_copy(1, 2);
        assert_eq!(q.title, "Quit NICE?");
        assert_eq!(q.confirm_label, "Quit");
        assert_eq!(q.message, describe_live_panes(1, 2));

        let c = close_dialog_copy(1, 2);
        assert_eq!(c.title, "Close this window?");
        assert_eq!(c.confirm_label, "Close");
        assert_eq!(c.message, describe_live_panes(1, 2));
    }
}
