//! `history` — the app-wide undo/redo stack for file operations (F6). Ported
//! from `FileOperationHistory.swift`. ONE process-wide instance drives ⌘Z / ⌘⇧Z
//! for file ops: ⌘Z in window B can undo a trash performed in window A, with
//! focus routed back to the originator so the user sees the change land.
//!
//! This is the gpui-free **model half**: the stacks, the drift handling, the
//! frozen strings, and the injectable focus-follow seam. The gpui `Entity`
//! wrapper + process `Global` handle (so per-window banner views can
//! `cx.observe` it) and the production focus-follow closure (which drives the
//! `WindowRegistry` / `AnyWindowHandle`) land in a later slice — this half is
//! what the ported `FileOperationHistoryTests` + `CrossWindowUndoTests` pin.
//!
//! ## Focus routing — native shape (documented divergence)
//!
//! Swift's 2-method `FileOperationFocusRouter` protocol becomes a single
//! injectable [`FocusFollow`] closure (`FnMut(&FileOperationOrigin) ->
//! FocusResult`). The production closure resolves the origin via the
//! `WindowRegistry`, activates the window, flips sidebar mode → Files, and
//! selects the origin tab; on a live origin it returns [`FocusResult::Routed`],
//! on a gone origin [`FocusResult::OriginGone`] (the op still applies,
//! headlessly, plus the closed-window banner). Seam absent (tests without a
//! recording fake) ⇒ [`FocusResult::NoRouter`], no banner.
//!
//! ## Drift handling
//!
//! Between push and undo the user may move / delete files via Finder or the
//! terminal. The service throws on missing inputs; this layer catches, DROPS
//! the offending op (never re-pushes it), and surfaces a transient
//! [`FileOperationHistory::last_drift_message`] — also the one failure channel
//! for paste/DnD/trash/rename errors (a later slice).

use super::ops::{FileOperation, FileOperationError, FileOperationOrigin, FileOperationsService};

/// Result of attempting to route focus to an originating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusResult {
    /// Focus follow ran (window activated + sidebar/tab updated).
    Routed,
    /// No seam configured — a test/preview that doesn't route. No banner.
    NoRouter,
    /// A seam exists but the originating window is gone — the inverse still
    /// applies, but the user is told it landed somewhere they can't see.
    OriginGone,
}

/// The injectable focus-follow seam. `FnMut` so a recording fake can accumulate
/// calls; the production closure captures the `WindowRegistry` handle.
pub type FocusFollow = Box<dyn FnMut(&FileOperationOrigin) -> FocusResult>;

/// App-wide file-operation undo/redo history (the model half — see module docs).
pub struct FileOperationHistory {
    service: FileOperationsService,
    focus_follow: Option<FocusFollow>,
    /// Most recent ops on top. `push` appends; `undo` pops and pushes onto
    /// `redo_stack`.
    undo_stack: Vec<FileOperation>,
    redo_stack: Vec<FileOperation>,
    /// One-shot transient message the per-window banner surfaces when an op
    /// can't be undone/redone cleanly. Cleared by callers after display.
    last_drift_message: Option<String>,
}

impl FileOperationHistory {
    /// Construct over the shared [`FileOperationsService`]. `focus_follow` is the
    /// injectable seam — `None` in tests that don't route (the Swift `registry:
    /// nil`), a recording fake in cross-window tests, the production closure in
    /// `app::run`.
    pub fn new(service: FileOperationsService, focus_follow: Option<FocusFollow>) -> Self {
        Self {
            service,
            focus_follow,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_drift_message: None,
        }
    }

    /// The shared pure FS worker — the orchestration layer performs copy/cut/
    /// trash through the SAME service instance so an injected fake reaches those
    /// paths too, not just undo/redo.
    pub fn service(&self) -> &FileOperationsService {
        &self.service
    }

    /// Install / replace the focus-follow seam (the production closure is set
    /// after the `WindowRegistry` exists).
    pub fn set_focus_follow(&mut self, focus_follow: FocusFollow) {
        self.focus_follow = Some(focus_follow);
    }

    /// Most recent ops on top.
    pub fn undo_stack(&self) -> &[FileOperation] {
        &self.undo_stack
    }

    pub fn redo_stack(&self) -> &[FileOperation] {
        &self.redo_stack
    }

    /// The one-shot drift message (banner input); `None` until an op drifts or
    /// lands headlessly.
    pub fn last_drift_message(&self) -> Option<&str> {
        self.last_drift_message.as_deref()
    }

    /// Clear the drift message (the banner calls this after displaying it).
    pub fn clear_drift_message(&mut self) {
        self.last_drift_message = None;
    }

    /// Publish a transient failure message on the banner channel. `last_drift_message`
    /// is the ONE failure channel for paste / DnD / trash / rename errors too (plan
    /// §History), not only undo/redo drift, so the menu/DnD callers route their
    /// service errors here.
    pub fn set_drift_message(&mut self, message: String) {
        self.last_drift_message = Some(message);
    }

    // MARK: - Push

    /// Record a successful op for later undo. Pushing always clears the redo
    /// stack — a new op makes re-applying previously-undone ops diverge from a
    /// linear history.
    pub fn push(&mut self, op: FileOperation) {
        self.undo_stack.push(op);
        self.redo_stack.clear();
    }

    // MARK: - Undo / Redo

    /// Undo the most recent op. No-op on an empty stack. Routes focus back to
    /// the origin; if the origin is gone, applies headlessly + a heads-up
    /// message. Real drift (file moved/deleted between op and undo) DROPS the
    /// offending op rather than re-pushing to redo.
    pub fn undo(&mut self) {
        let Some(op) = self.undo_stack.pop() else {
            return;
        };
        let origin = op.origin().clone();
        let result = self.follow_focus(&origin);
        match self.service.undo(&op) {
            Ok(()) => {
                if result == FocusResult::OriginGone {
                    self.last_drift_message = Some(headless_message(&op, true));
                }
                self.redo_stack.push(op);
            }
            Err(err) => {
                self.last_drift_message = Some(drift_message(&err, false));
            }
        }
    }

    /// Redo the most recently undone op. No-op on an empty redo stack. Mirrors
    /// `undo`'s focus-follow + headless-message behaviour. Re-apply returns the
    /// op that was actually performed — for trash this carries FRESH trash
    /// paths — and THAT is pushed back onto the undo stack.
    pub fn redo(&mut self) {
        let Some(op) = self.redo_stack.pop() else {
            return;
        };
        let origin = op.origin().clone();
        let result = self.follow_focus(&origin);
        match self.service.apply(op) {
            Ok(result_op) => {
                if result == FocusResult::OriginGone {
                    self.last_drift_message = Some(headless_message(&result_op, false));
                }
                self.undo_stack.push(result_op);
            }
            Err(err) => {
                self.last_drift_message = Some(drift_message(&err, true));
            }
        }
    }

    /// Invoke the focus-follow seam, or [`FocusResult::NoRouter`] when absent.
    fn follow_focus(&mut self, origin: &FileOperationOrigin) -> FocusResult {
        match self.focus_follow.as_mut() {
            Some(f) => f(origin),
            None => FocusResult::NoRouter,
        }
    }
}

// MARK: - The process Global (SharedFontSettings / WorkspaceOpsGlobal pattern) --

/// The ONE process-wide undo/redo history, as a gpui `Entity` in a `Global`
/// handle: an entity (not a bare value) so per-window drift-banner views can
/// `cx.observe` it and re-render when a message publishes. `app::run` creates it
/// (over the production `Trasher`); ⌘Z / ⌘⇧Z (`crate::keymap`) and the menu
/// handlers (`crate::file_browser::view`) drive it. Absent ⇒ those actions are
/// no-ops — the same "no Global ⇒ inert" discipline the other seams use.
pub struct FileOperationHistoryGlobal(pub gpui::Entity<FileOperationHistory>);

impl gpui::Global for FileOperationHistoryGlobal {}

/// The last path component of a path, for the drift strings.
fn last_component(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The FROZEN drift strings (verbatim, test-pinned — plan §History). `redo`
/// mirrors `undo` with the verb swapped.
fn drift_message(err: &FileOperationError, redo: bool) -> String {
    let verb = if redo { "redo" } else { "undo" };
    let verb_cap = if redo { "Redo" } else { "Undo" };
    match err {
        FileOperationError::SourceMissing(p) => {
            format!("Couldn't {verb}: '{}' is no longer there.", last_component(p))
        }
        FileOperationError::TrashedItemMissing(p) => {
            format!(
                "Couldn't {verb}: '{}' was emptied from Trash.",
                last_component(p)
            )
        }
        FileOperationError::Underlying(msg) => format!("{verb_cap} failed: {msg}"),
    }
}

/// The FROZEN headless heads-up string when an op landed without a live
/// originating window.
fn headless_message(op: &FileOperation, undo: bool) -> String {
    let verb = if undo { "Undid" } else { "Redid" };
    format!("{verb} {} — change landed in a closed window.", op.label())
}

#[cfg(test)]
mod tests {
    use super::super::ops::{FakeTrasher, FileOperation, FileOperationsService};
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    // A per-test temp tree, removed on drop.
    struct TempTree {
        root: PathBuf,
    }
    impl TempTree {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "nice-history-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn make_file(&self, name: &str, body: &str) -> PathBuf {
            let url = self.root.join(name);
            fs::write(&url, body).unwrap();
            url
        }
        fn make_dir(&self, name: &str) -> PathBuf {
            let url = self.root.join(name);
            fs::create_dir_all(&url).unwrap();
            url
        }
        fn trash(&self) -> PathBuf {
            self.make_dir("Trash")
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn origin() -> FileOperationOrigin {
        FileOperationOrigin::new("win-1", Some("tab-1".into()))
    }

    fn service(trash: &std::path::Path) -> FileOperationsService {
        FileOperationsService::new(Box::new(FakeTrasher::new(trash)))
    }

    /// History with no focus-follow seam (the Swift `registry: nil`).
    fn make_history(t: &TempTree) -> FileOperationHistory {
        FileOperationHistory::new(service(&t.trash()), None)
    }

    fn copy_op(t: &TempTree, name: &str) -> FileOperation {
        let src = t.make_file(name, "");
        let dest = t.make_dir(&format!("dest-{name}"));
        service(&t.trash())
            .copy(&[src], &dest, origin())
            .expect("copy")
    }

    // MARK: - Push

    /// `FileOperationHistoryTests.test_push_pushesOntoUndoStack_clearsRedo`
    #[test]
    fn push_pushes_onto_undo_stack_clears_redo() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        history.push(copy_op(&t, "a.txt"));
        history.undo(); // redo stack now has an entry
        assert_eq!(history.redo_stack().len(), 1);

        history.push(copy_op(&t, "second.txt"));
        assert_eq!(history.redo_stack().len(), 0);
        assert_eq!(history.undo_stack().len(), 1);
    }

    // MARK: - Undo / Redo

    /// `FileOperationHistoryTests.test_undo_appliesInverse_pushesToRedo`
    #[test]
    fn undo_applies_inverse_pushes_to_redo() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "");
        let dest = t.make_dir("dest");
        let op = service(&t.trash()).copy(&[src], &dest, origin()).unwrap();
        history.push(op);
        assert!(dest.join("a.txt").exists());

        history.undo();
        assert!(!dest.join("a.txt").exists());
        assert_eq!(history.undo_stack().len(), 0);
        assert_eq!(history.redo_stack().len(), 1);
    }

    /// `FileOperationHistoryTests.test_redo_reappliesOriginal_pushesToUndo`
    #[test]
    fn redo_reapplies_original_pushes_to_undo() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "hi");
        let dest = t.make_dir("dest");
        let op = service(&t.trash()).move_(&[src.clone()], &dest, origin()).unwrap();
        history.push(op);
        history.undo();
        assert!(src.exists());
        assert!(!dest.join("a.txt").exists());

        history.redo();
        assert!(!src.exists());
        assert!(dest.join("a.txt").exists());
    }

    /// `FileOperationHistoryTests.test_undo_emptyStack_isNoOp`
    #[test]
    fn undo_empty_stack_is_noop() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        history.undo();
        assert!(history.last_drift_message().is_none());
    }

    /// `FileOperationHistoryTests.test_redo_emptyStack_isNoOp`
    #[test]
    fn redo_empty_stack_is_noop() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        history.redo();
        assert!(history.last_drift_message().is_none());
    }

    // MARK: - Drift

    /// `FileOperationHistoryTests.test_drift_undoCopy_destinationGone_silent`
    #[test]
    fn drift_undo_copy_destination_gone_silent() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "");
        let dest = t.make_dir("dest");
        let op = service(&t.trash()).copy(&[src], &dest, origin()).unwrap();
        history.push(op);
        // User deleted the copied file via Finder before ⌘Z.
        fs::remove_file(dest.join("a.txt")).unwrap();

        history.undo();
        assert!(history.last_drift_message().is_none());
        assert_eq!(history.redo_stack().len(), 1);
    }

    /// `FileOperationHistoryTests.test_drift_undoMove_sourceMissing_publishesMessage`
    #[test]
    fn drift_undo_move_source_missing_publishes_message() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "hi");
        let dest = t.make_dir("dest");
        let op = service(&t.trash()).move_(&[src], &dest, origin()).unwrap();
        history.push(op);
        fs::remove_file(dest.join("a.txt")).unwrap();

        history.undo();
        let msg = history.last_drift_message().expect("drift message");
        assert!(msg.contains("a.txt"));
        assert_eq!(history.redo_stack().len(), 0);
    }

    /// `FileOperationHistoryTests.test_drift_undoTrash_emptied_publishesMessage`
    #[test]
    fn drift_undo_trash_emptied_publishes_message() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "");
        let op = service(&t.trash()).trash(&[src], origin()).unwrap();
        if let FileOperation::Trash { items, .. } = &op {
            fs::remove_file(&items[0].trashed).unwrap();
        }
        history.push(op);

        history.undo();
        let msg = history.last_drift_message().expect("drift message");
        assert!(msg.contains("emptied"));
        assert_eq!(history.redo_stack().len(), 0);
    }

    /// `FileOperationHistoryTests.test_lastDriftMessage_overwritesAcrossSuccessiveDrifts`
    #[test]
    fn last_drift_message_overwrites_across_successive_drifts() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let s1 = t.make_file("a.txt", "1");
        let d1 = t.make_dir("dest");
        let op1 = service(&t.trash()).move_(&[s1], &d1, origin()).unwrap();
        history.push(op1);
        fs::remove_file(d1.join("a.txt")).unwrap();
        history.undo();
        let first = history.last_drift_message().unwrap().to_string();

        let s2 = t.make_file("b.txt", "2");
        let op2 = service(&t.trash()).move_(&[s2], &d1, origin()).unwrap();
        history.push(op2);
        fs::remove_file(d1.join("b.txt")).unwrap();
        history.undo();
        let second = history.last_drift_message().unwrap().to_string();

        assert_ne!(first, second);
        assert!(second.contains("b.txt"));
    }

    /// `FileOperationHistoryTests.test_originPreservedThroughUndoRedo`
    #[test]
    fn origin_preserved_through_undo_redo() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "hi");
        let dest = t.make_dir("dest");
        let origin = FileOperationOrigin::new("win-7", Some("tab-99".into()));
        let op = service(&t.trash()).move_(&[src], &dest, origin.clone()).unwrap();
        history.push(op);

        history.undo();
        assert_eq!(history.redo_stack().last().unwrap().origin(), &origin);

        history.redo();
        assert_eq!(history.undo_stack().last().unwrap().origin(), &origin);
    }

    // MARK: - Cross-window focus routing (CrossWindowUndoTests)

    /// A recording focus-follow fake: registered session ids are "live"
    /// (routed); unregistered ones are gone. Records every followed origin and
    /// every window it was asked to bring to front — the native-shape stand-in
    /// for Swift's `FakeFocusRouter` + the `AppState` sidebar/tab mutations.
    #[derive(Default)]
    struct FocusRecorderInner {
        live: HashSet<String>,
        followed: Vec<FileOperationOrigin>,
        brought_to_front: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct FocusRecorder(Rc<RefCell<FocusRecorderInner>>);

    impl FocusRecorder {
        fn register(&self, session_id: &str) {
            self.0.borrow_mut().live.insert(session_id.to_string());
        }
        fn follower(&self) -> FocusFollow {
            let inner = self.0.clone();
            Box::new(move |origin: &FileOperationOrigin| {
                let mut i = inner.borrow_mut();
                i.followed.push(origin.clone());
                if i.live.contains(&origin.window_session_id) {
                    i.brought_to_front.push(origin.window_session_id.clone());
                    FocusResult::Routed
                } else {
                    FocusResult::OriginGone
                }
            })
        }
        fn followed(&self) -> Vec<FileOperationOrigin> {
            self.0.borrow().followed.clone()
        }
        fn brought_to_front(&self) -> Vec<String> {
            self.0.borrow().brought_to_front.clone()
        }
    }

    fn history_with_recorder(t: &TempTree, recorder: &FocusRecorder) -> FileOperationHistory {
        FileOperationHistory::new(service(&t.trash()), Some(recorder.follower()))
    }

    /// `CrossWindowUndoTests.test_undo_routesFocusToOriginatingAppState_whenDifferent`
    /// — the sidebar-mode flip is done inside the production closure; the model
    /// half asserts the recorder was asked to route to the origin.
    #[test]
    fn undo_routes_focus_to_originating_state_when_different() {
        let t = TempTree::new();
        let recorder = FocusRecorder::default();
        recorder.register("win-A");
        recorder.register("win-B");
        let mut history = history_with_recorder(&t, &recorder);

        let src = t.make_file("a.txt", "");
        let op = service(&t.trash())
            .trash(&[src], FileOperationOrigin::new("win-A", Some("tab-A".into())))
            .unwrap();
        history.push(op);

        history.undo();
        let followed = recorder.followed();
        assert_eq!(followed.len(), 1);
        assert_eq!(followed[0].window_session_id, "win-A");
    }

    /// `CrossWindowUndoTests.test_undo_setsSidebarToFiles_andSelectsOriginatingTab`
    /// — the tab selection happens in the production closure; assert the
    /// recorder routed to the origin tab.
    #[test]
    fn undo_routes_to_originating_tab() {
        let t = TempTree::new();
        let recorder = FocusRecorder::default();
        recorder.register("win-A");
        let mut history = history_with_recorder(&t, &recorder);

        let src = t.make_file("file.txt", "data");
        let op = service(&t.trash())
            .trash(&[src], FileOperationOrigin::new("win-A", Some("tab-XYZ".into())))
            .unwrap();
        history.push(op);

        history.undo();
        let followed = recorder.followed();
        assert_eq!(followed[0].tab_id.as_deref(), Some("tab-XYZ"));
    }

    /// `CrossWindowUndoTests.test_undo_originatingWindowGone_appliesHeadless_publishesMessage_pushesToRedo`
    #[test]
    fn undo_originating_window_gone_applies_headless_publishes_message() {
        let t = TempTree::new();
        let recorder = FocusRecorder::default(); // win-A NOT registered
        let mut history = history_with_recorder(&t, &recorder);

        let src = t.make_file("file.txt", "");
        let op = service(&t.trash())
            .trash(&[src.clone()], FileOperationOrigin::new("win-A", Some("tab-A".into())))
            .unwrap();
        history.push(op);

        history.undo();
        assert!(src.exists(), "fs inverse applies even when origin window is gone");
        assert_eq!(history.redo_stack().len(), 1);
        let msg = history.last_drift_message().expect("headless message");
        assert!(msg.contains("closed window"));
    }

    /// `CrossWindowUndoTests.test_undo_routedToLiveAppState_doesNotPublishHeadlessMessage`
    #[test]
    fn undo_routed_to_live_state_does_not_publish_headless_message() {
        let t = TempTree::new();
        let recorder = FocusRecorder::default();
        recorder.register("win-A");
        let mut history = history_with_recorder(&t, &recorder);

        let src = t.make_file("file.txt", "");
        let op = service(&t.trash())
            .trash(&[src], FileOperationOrigin::new("win-A", Some("tab-A".into())))
            .unwrap();
        history.push(op);

        history.undo();
        assert!(history.last_drift_message().is_none());
        assert_eq!(recorder.brought_to_front(), vec!["win-A".to_string()]);
    }

    /// `CrossWindowUndoTests.test_redo_routesFocusToOriginatingAppState`
    #[test]
    fn redo_routes_focus_to_originating_state() {
        let t = TempTree::new();
        let recorder = FocusRecorder::default();
        recorder.register("win-A");
        let mut history = history_with_recorder(&t, &recorder);

        let src = t.make_file("file.txt", "");
        let op = service(&t.trash())
            .trash(&[src], FileOperationOrigin::new("win-A", Some("tab-A".into())))
            .unwrap();
        history.push(op);
        history.undo();

        history.redo();
        // Two follows (undo + redo), both routed to win-A.
        assert_eq!(recorder.brought_to_front(), vec!["win-A".to_string(), "win-A".to_string()]);
    }

    // MARK: - Redo message coverage (the redo twins of the undo drift/headless cases)

    /// The redo twin of `drift_undo_move_source_missing_publishes_message`: a
    /// redo whose re-apply drifts (the source is deleted after the undo put it
    /// back) DROPS the op and publishes the FROZEN "Couldn't redo: …" banner via
    /// `drift_message(_, redo = true)` (history.rs:172).
    #[test]
    fn drift_redo_move_source_missing_publishes_message() {
        let t = TempTree::new();
        let mut history = make_history(&t);
        let src = t.make_file("a.txt", "hi");
        let dest = t.make_dir("dest");
        let op = service(&t.trash())
            .move_(&[src.clone()], &dest, origin())
            .unwrap();
        history.push(op);
        history.undo();
        // Undo moved the file back to `src`; the user deletes it before ⌘⇧Z.
        assert!(src.exists());
        fs::remove_file(&src).unwrap();

        history.redo();
        let msg = history.last_drift_message().expect("redo drift message");
        assert!(msg.contains("redo"), "message names the redo verb: {msg}");
        assert!(msg.contains("a.txt"), "message names the affected file: {msg}");
        // The drifted op is dropped, never re-pushed onto either stack.
        assert_eq!(history.undo_stack().len(), 0);
        assert_eq!(history.redo_stack().len(), 0);
    }

    /// The redo twin of
    /// `undo_originating_window_gone_applies_headless_publishes_message`: a redo
    /// routed to a GONE origin still applies the op, then publishes the FROZEN
    /// "Redid … closed window." banner via `headless_message(result_op, undo =
    /// false)` (history.rs:167). The `result_op` (fresh trash paths) is what
    /// lands on the undo stack.
    #[test]
    fn redo_originating_window_gone_applies_headless_publishes_message() {
        let t = TempTree::new();
        let recorder = FocusRecorder::default(); // win-A NOT registered
        let mut history = history_with_recorder(&t, &recorder);

        let src = t.make_file("file.txt", "");
        let op = service(&t.trash())
            .trash(&[src.clone()], FileOperationOrigin::new("win-A", Some("tab-A".into())))
            .unwrap();
        history.push(op);
        history.undo();
        assert!(src.exists(), "undo restored the file");

        history.redo();
        assert!(
            !src.exists(),
            "redo re-applied the trash even though the origin window is gone"
        );
        assert_eq!(history.undo_stack().len(), 1);
        assert_eq!(history.redo_stack().len(), 0);
        let msg = history.last_drift_message().expect("headless redo message");
        assert!(msg.contains("Redid"), "message uses the redo verb: {msg}");
        assert!(
            msg.contains("closed window"),
            "message names the closed window: {msg}"
        );
    }
}
