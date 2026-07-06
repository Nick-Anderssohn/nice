//! `WorkspaceOps` — the single injectable seam for every OS-integration call the
//! file browser makes: open with the OS default, open with a chosen application,
//! reveal in Finder, enumerate the apps that can open a file (+ the default), and
//! the "Other…" chooser. **Frozen decision (hermeticity):** no test or scenario
//! ever launches a real app, reveals in the real Finder, or queries live Launch
//! Services — everything routes through this trait.
//!
//! * The **production** impl ([`ProductionWorkspaceOps`]) forwards to the objc2
//!   primitives in [`crate::platform`] (the only module that touches the OS
//!   workspace APIs). `app::run` installs it as the gpui `Global` — the
//!   `SharedFontSettings` pattern.
//! * The **recording fake** ([`RecordingWorkspaceOps`]) logs every call and
//!   returns injected fixtures. `run_selftest` installs it process-wide BEFORE
//!   any scenario runs (via [`install_recording_fake`]); the scenario reads the
//!   log back through [`selftest_fake`] and preloads the app enumeration /
//!   chooser answer.
//!
//! The pure Open-With **ordering / dedup / synthesized-default** logic is NOT
//! re-implemented here — it lives in [`nice_model::file_browser::open_with`].
//! [`open_with_entries`] just adapts a [`OpenWithApps`] enumeration (from either
//! impl) into that function's injected-lookup shape.

use std::sync::{Arc, Mutex};

use gpui::{App, AppContext, Global};
use nice_model::file_browser::open_with::{self, OpenWithEntry, OpenWithLookups};

/// The applications that can open a target, as the `WorkspaceOps` seam reports
/// them: `(standardized_app_path, display_name)` in Launch Services order, plus
/// the user's default app path (if any). Fed into [`open_with_entries`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenWithApps {
    /// `(standardized_app_path, display_name)`, Launch Services order.
    pub apps: Vec<(String, String)>,
    /// The user's default app path for the target, if any.
    pub default_app: Option<String>,
}

/// One recorded workspace call — the fake's log entries, read back by scenarios
/// to prove "exactly one open, nothing launched".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceCall {
    Open(String),
    OpenWith { path: String, app_path: String },
    Reveal(String),
    AppsFor(String),
    ChooseApplication,
}

/// Every OS-integration call the file browser makes. Object-safe (installed as a
/// boxed trait object in the [`WorkspaceOpsGlobal`]).
pub trait WorkspaceOps {
    /// Open with the OS default handler.
    fn open(&self, path: &str);
    /// Open with the application at `app_path`.
    fn open_with(&self, path: &str, app_path: &str);
    /// Reveal in Finder.
    fn reveal(&self, path: &str);
    /// Enumerate the apps that can open `path` (+ the default).
    fn apps_for(&self, path: &str) -> OpenWithApps;
    /// The "Other…" chooser — the chosen app path, or `None` if cancelled.
    fn choose_application(&self) -> Option<String>;
}

/// Adapt an [`OpenWithApps`] enumeration into the ordered "Open With ▸" entries,
/// delegating to the pure [`nice_model::file_browser::open_with::entries`]
/// (default first, remainder alphabetized case-insensitively, deduped by path,
/// synthesized default if missing from the list).
pub fn open_with_entries(apps: &OpenWithApps) -> Vec<OpenWithEntry> {
    let all_apps: Vec<String> = apps.apps.iter().map(|(p, _)| p.clone()).collect();
    let display = |query: &str| -> String {
        apps.apps
            .iter()
            .find(|(p, _)| p == query)
            .map(|(_, name)| name.clone())
            // A synthesized default (not in the enumeration) has no display name
            // here — fall back to its file name without the `.app` extension.
            .unwrap_or_else(|| {
                let base = std::path::Path::new(query)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| query.to_string());
                base
            })
    };
    let lookups = OpenWithLookups {
        all_apps,
        default_app: apps.default_app.clone(),
        display_name: &display,
    };
    open_with::entries(&lookups)
}

// MARK: - Production impl (objc2 via platform.rs) --------------------------------

/// The shipped implementation — every call forwards to [`crate::platform`]. Zero
/// state; installed once by `app::run`.
pub struct ProductionWorkspaceOps;

impl WorkspaceOps for ProductionWorkspaceOps {
    fn open(&self, path: &str) {
        crate::platform::workspace_open(path);
    }
    fn open_with(&self, path: &str, app_path: &str) {
        crate::platform::workspace_open_with(path, app_path);
    }
    fn reveal(&self, path: &str) {
        crate::platform::workspace_reveal(path);
    }
    fn apps_for(&self, path: &str) -> OpenWithApps {
        let (apps, default_app) = crate::platform::workspace_apps_for(path);
        OpenWithApps { apps, default_app }
    }
    fn choose_application(&self) -> Option<String> {
        crate::platform::workspace_choose_application()
    }
}

// MARK: - Recording fake --------------------------------------------------------

#[derive(Default)]
struct RecordingState {
    calls: Vec<WorkspaceCall>,
    /// Canned `apps_for` response the scenario preloads.
    apps: OpenWithApps,
    /// Canned `choose_application` response.
    chosen: Option<String>,
}

/// The recording fake: logs every call and returns injected fixtures. Cheaply
/// clonable (`Arc`-backed) — `run_selftest` installs one clone as the Global and
/// stashes another in the process static so the scenario shares the same log.
#[derive(Clone, Default)]
pub struct RecordingWorkspaceOps {
    state: Arc<Mutex<RecordingState>>,
}

impl RecordingWorkspaceOps {
    pub fn new() -> Self {
        Self::default()
    }

    /// Preload the `apps_for` enumeration a scenario will assert on.
    pub fn set_apps(&self, apps: OpenWithApps) {
        self.state.lock().unwrap().apps = apps;
    }

    /// Preload the `choose_application` answer.
    pub fn set_chosen(&self, chosen: Option<String>) {
        self.state.lock().unwrap().chosen = chosen;
    }

    /// A snapshot of the recorded call log (in call order).
    pub fn calls(&self) -> Vec<WorkspaceCall> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Clear the recorded log (scenario re-use between legs).
    pub fn clear(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    fn record(&self, call: WorkspaceCall) {
        self.state.lock().unwrap().calls.push(call);
    }
}

impl WorkspaceOps for RecordingWorkspaceOps {
    fn open(&self, path: &str) {
        self.record(WorkspaceCall::Open(path.to_string()));
    }
    fn open_with(&self, path: &str, app_path: &str) {
        self.record(WorkspaceCall::OpenWith {
            path: path.to_string(),
            app_path: app_path.to_string(),
        });
    }
    fn reveal(&self, path: &str) {
        self.record(WorkspaceCall::Reveal(path.to_string()));
    }
    fn apps_for(&self, path: &str) -> OpenWithApps {
        self.record(WorkspaceCall::AppsFor(path.to_string()));
        self.state.lock().unwrap().apps.clone()
    }
    fn choose_application(&self) -> Option<String> {
        self.record(WorkspaceCall::ChooseApplication);
        self.state.lock().unwrap().chosen.clone()
    }
}

// MARK: - The process Global (SharedFontSettings pattern) -----------------------

/// The installed `WorkspaceOps` — a boxed trait object. `app::run` installs the
/// production impl; `run_selftest` installs the recording fake. Absent ⇒ the
/// caller (the view / menu actions) treats every OS action as unavailable.
pub struct WorkspaceOpsGlobal(pub Box<dyn WorkspaceOps>);

impl Global for WorkspaceOpsGlobal {}

/// Install the production impl as the Global — `app::run` ONLY.
pub fn install_production(cx: &mut App) {
    cx.set_global(WorkspaceOpsGlobal(Box::new(ProductionWorkspaceOps)));
}

/// Install a fresh recording fake as the Global AND stash a shared clone in the
/// process static (so a scenario can read its log + preload fixtures) — the
/// `run_selftest` seam, called before any scenario runs. Returns the fake handle.
pub fn install_recording_fake(cx: &mut App) -> RecordingWorkspaceOps {
    let fake = RecordingWorkspaceOps::new();
    cx.set_global(WorkspaceOpsGlobal(Box::new(fake.clone())));
    *selftest_slot().lock().unwrap() = Some(fake.clone());
    fake
}

fn selftest_slot() -> &'static Mutex<Option<RecordingWorkspaceOps>> {
    static SLOT: Mutex<Option<RecordingWorkspaceOps>> = Mutex::new(None);
    &SLOT
}

/// The recording fake installed by [`install_recording_fake`], if any — the
/// scenario's handle onto the same call log / fixture state as the Global.
pub fn selftest_fake() -> Option<RecordingWorkspaceOps> {
    selftest_slot().lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recording fake logs each call in order and returns injected fixtures.
    #[test]
    fn recording_fake_logs_and_returns_fixtures() {
        let fake = RecordingWorkspaceOps::new();
        fake.set_apps(OpenWithApps {
            apps: vec![("/Applications/Xcode.app".into(), "Xcode".into())],
            default_app: Some("/Applications/Xcode.app".into()),
        });
        fake.set_chosen(Some("/Applications/BBEdit.app".into()));

        fake.open("/tmp/a.txt");
        fake.open_with("/tmp/a.txt", "/Applications/Xcode.app");
        fake.reveal("/tmp/a.txt");
        let apps = fake.apps_for("/tmp/a.txt");
        let chosen = fake.choose_application();

        assert_eq!(apps.default_app.as_deref(), Some("/Applications/Xcode.app"));
        assert_eq!(chosen.as_deref(), Some("/Applications/BBEdit.app"));
        assert_eq!(
            fake.calls(),
            vec![
                WorkspaceCall::Open("/tmp/a.txt".into()),
                WorkspaceCall::OpenWith {
                    path: "/tmp/a.txt".into(),
                    app_path: "/Applications/Xcode.app".into(),
                },
                WorkspaceCall::Reveal("/tmp/a.txt".into()),
                WorkspaceCall::AppsFor("/tmp/a.txt".into()),
                WorkspaceCall::ChooseApplication,
            ]
        );
    }

    /// Clones share one log (the Global's copy and the scenario's copy are the
    /// same fake).
    #[test]
    fn clones_share_the_log() {
        let a = RecordingWorkspaceOps::new();
        let b = a.clone();
        a.open("/tmp/x");
        assert_eq!(b.calls(), vec![WorkspaceCall::Open("/tmp/x".into())]);
    }

    /// [`open_with_entries`] delegates to the pure ordering: default first, rest
    /// alphabetized, synthesized default gets a filename fallback.
    #[test]
    fn open_with_entries_orders_via_pure_function() {
        let apps = OpenWithApps {
            apps: vec![
                ("/Applications/TextEdit.app".into(), "TextEdit".into()),
                ("/Applications/BBEdit.app".into(), "BBEdit".into()),
            ],
            default_app: Some("/Applications/Xcode.app".into()),
        };
        let entries = open_with_entries(&apps);
        // Default (synthesized — not in the list) first, name from the filename.
        assert_eq!(entries[0].app_path, "/Applications/Xcode.app");
        assert!(entries[0].is_default);
        assert_eq!(entries[0].display_name, "Xcode");
        // Remainder alphabetized: BBEdit before TextEdit.
        let rest: Vec<&str> = entries.iter().skip(1).map(|e| e.app_path.as_str()).collect();
        assert_eq!(rest, ["/Applications/BBEdit.app", "/Applications/TextEdit.app"]);
    }
}
