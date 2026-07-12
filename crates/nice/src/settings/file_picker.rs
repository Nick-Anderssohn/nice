//! `FilePickerOps` — the single injectable seam for the Settings "Import theme…"
//! file chooser (Binding decision D5), modeled exactly on the landed
//! [`WorkspaceOps`](crate::file_browser::workspace_ops) recording-fake pattern.
//! **Frozen decision (hermeticity):** no test or scenario ever opens a real
//! `NSOpenPanel` — every import goes through this trait, and the fixture path is a
//! temp file.
//!
//! * The **production** impl ([`ProductionFilePicker`]) forwards to the objc2
//!   [`crate::platform::choose_theme_file`] panel (filtered to `.ghostty`/`.conf`).
//!   `app::run` installs it as the gpui `Global` — the `WorkspaceOps` pattern.
//! * The **recording fake** ([`RecordingFilePicker`]) logs each call and returns a
//!   scripted path. `run_selftest` installs one process-wide before any scenario
//!   runs (via [`install_recording_fake`]); the `settings-window` scenario reads
//!   the log back through [`selftest_fake`] and scripts the next chosen path.
//!
//! The Import… button reads the seam to get an `Option<PathBuf>` (`None` ⇒ the user
//! cancelled), then calls R22's `import_theme(cx, &path)` — see
//! [`crate::settings::appearance_pane::perform_import`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{App, Global};

/// The file-choose seam every Settings Import… invocation routes through.
/// Object-safe (installed as a boxed trait object in [`FilePickerOpsGlobal`]).
pub trait FilePickerOps {
    /// Present the theme-file chooser. Returns the chosen file's path, or `None`
    /// when the user cancels.
    fn pick_theme_file(&self) -> Option<PathBuf>;
}

// MARK: - Production impl (objc2 via platform.rs) --------------------------------

/// The shipped implementation — forwards to the objc2 `NSOpenPanel` in
/// [`crate::platform`]. Zero state; installed once by `app::run`.
pub struct ProductionFilePicker;

impl FilePickerOps for ProductionFilePicker {
    fn pick_theme_file(&self) -> Option<PathBuf> {
        crate::platform::choose_theme_file().map(PathBuf::from)
    }
}

// MARK: - Recording fake --------------------------------------------------------

#[derive(Default)]
struct RecordingState {
    /// The number of `pick_theme_file` calls — the scenario asserts the Import…
    /// button reached the seam (and NEVER a real panel).
    calls: usize,
    /// The path the NEXT `pick_theme_file` returns (`None` ⇒ simulate a cancel).
    scripted: Option<PathBuf>,
}

/// The recording fake: logs each call and returns the scripted path. Cheaply
/// clonable (`Arc`-backed) — `run_selftest` installs one clone as the Global and
/// stashes another in the process static so the scenario shares the same log.
#[derive(Clone, Default)]
pub struct RecordingFilePicker {
    state: Arc<Mutex<RecordingState>>,
}

impl RecordingFilePicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the path the next `pick_theme_file` returns (`None` = the user
    /// cancels). The scenario points this at a temp fixture `.ghostty`.
    pub fn set_next(&self, path: Option<PathBuf>) {
        self.state.lock().unwrap().scripted = path;
    }

    /// How many times the picker was invoked — proof the Import… handler reached
    /// the seam.
    pub fn call_count(&self) -> usize {
        self.state.lock().unwrap().calls
    }
}

impl FilePickerOps for RecordingFilePicker {
    fn pick_theme_file(&self) -> Option<PathBuf> {
        let mut state = self.state.lock().unwrap();
        state.calls += 1;
        state.scripted.clone()
    }
}

// MARK: - The process Global (WorkspaceOps pattern) -----------------------------

/// The installed `FilePickerOps` — a boxed trait object. `app::run` installs the
/// production impl; `run_selftest` installs the recording fake. Absent ⇒ the
/// Import… handler treats the choose as cancelled (no-op-when-absent).
pub struct FilePickerOpsGlobal(pub Box<dyn FilePickerOps>);

impl Global for FilePickerOpsGlobal {}

/// Read the seam and present the chooser — `None` when no picker is installed
/// (the `WorkspaceOps` no-op-when-absent discipline) or the user cancelled.
pub(crate) fn pick_theme_file(cx: &App) -> Option<PathBuf> {
    cx.try_global::<FilePickerOpsGlobal>()
        .and_then(|g| g.0.pick_theme_file())
}

/// Install the production impl as the Global — `app::run` ONLY.
pub fn install_production(cx: &mut App) {
    cx.set_global(FilePickerOpsGlobal(Box::new(ProductionFilePicker)));
}

/// Install a fresh recording fake as the Global AND stash a shared clone in the
/// process static (so a scenario can script the chosen path + read the call
/// count) — the `run_selftest` seam, called before any scenario runs. Returns the
/// fake handle.
pub fn install_recording_fake(cx: &mut App) -> RecordingFilePicker {
    let fake = RecordingFilePicker::new();
    cx.set_global(FilePickerOpsGlobal(Box::new(fake.clone())));
    *selftest_slot().lock().unwrap() = Some(fake.clone());
    fake
}

fn selftest_slot() -> &'static Mutex<Option<RecordingFilePicker>> {
    static SLOT: Mutex<Option<RecordingFilePicker>> = Mutex::new(None);
    &SLOT
}

/// The recording fake installed by [`install_recording_fake`], if any — the
/// scenario's handle onto the same call log / scripted state as the Global.
pub fn selftest_fake() -> Option<RecordingFilePicker> {
    selftest_slot().lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_fake_returns_scripted_path_and_counts_calls() {
        let fake = RecordingFilePicker::new();
        // Unset ⇒ a cancel.
        assert_eq!(fake.pick_theme_file(), None);
        // Scripted ⇒ that path, once.
        let p = PathBuf::from("/tmp/nice-fixture.ghostty");
        fake.set_next(Some(p.clone()));
        assert_eq!(fake.pick_theme_file(), Some(p));
        assert_eq!(fake.call_count(), 2, "both invocations were logged");
    }
}
