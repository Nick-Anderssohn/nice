//! `ReleaseFetcher` — the single injectable seam for the one GitHub Releases GET
//! the update checker makes (What-to-build item 2, Binding decision D1),
//! modeled exactly on the landed
//! [`WorkspaceOps`](crate::file_browser::workspace_ops) /
//! [`FilePickerOps`](crate::settings::file_picker) recording-fake pattern.
//! **Frozen decision (hermeticity):** no test or scenario ever hits the real
//! network or `github.com` — every fetch routes through this trait, and the
//! recording fake returns canned JSON.
//!
//! * The **production** impl ([`ProductionReleaseFetcher`]) forwards to the D1
//!   objc2 `NSURLSession` GET in [`crate::platform::http_get`] (the only module
//!   that touches the OS networking APIs), requires a `200..300`, and decodes
//!   `{ "tag_name": String }` only. `app::run` installs it as the gpui `Global`.
//! * The **recording fake** ([`RecordingReleaseFetcher`]) returns a scripted tag
//!   or a scripted error and logs every call. `run_selftest` installs one
//!   process-wide before any scenario runs (via [`install_recording_fake`]); the
//!   `update-check` scenario drives `set_tag` / `set_error` and asserts the call
//!   log through the shared handle.
//!
//! ## Why an `Arc`, not a `Box`
//! The Global holds `Arc<dyn ReleaseFetcher>`, NOT a `Box`: gpui Globals are
//! foreground-only and the blocking fetch runs on a background thread that has no
//! `cx`, so [`super::check_now`] / [`super::ReleaseCheckerGlobal::start`] clone
//! the `Arc` ON THE FOREGROUND and move the clone into the background work,
//! marshalling only the `Result<String, FetchError>` back. The `Send + Sync`
//! bound on the trait is therefore load-bearing — the trait object must cross the
//! thread boundary. A bare `Box<dyn ReleaseFetcher>` neither is `Send + Sync`-
//! bounded nor clones out of the Global, so it could not reach the worker.

use std::sync::{Arc, Mutex};

use gpui::{App, Global};
use serde::Deserialize;

/// Why a release fetch failed. Every variant is **swallowed** by the checker
/// (`ReleaseChecker.swift:140-142`) — the pill only ever appears, never errors —
/// so these are informational (they drive no UI). Mirrors Swift's
/// `GitHubReleaseFetcher.FetchError` (`httpStatus`) plus the Rust production impl's
/// transport + decode failure modes; the Swift `invalidResponse` case (a non-HTTP
/// response) is folded into [`Transport`](FetchError::Transport) by
/// [`crate::platform::http_get`], which returns a message for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The OS networking call failed outright (offline / DNS / timeout / TLS / a
    /// non-HTTP response). Carries the platform error text.
    Transport(String),
    /// The response was HTTP but outside `200..300`.
    HttpStatus(u16),
    /// The body was not decodable as `{ "tag_name": String }`. Carries the serde
    /// error text.
    Decode(String),
}

/// The one GitHub Releases lookup, hidden behind a seam so tests inject a fake and
/// never touch the network. Object-safe and `Send + Sync` (installed as an `Arc`
/// trait object in [`ReleaseFetcherGlobal`], cloned onto a background thread).
pub trait ReleaseFetcher: Send + Sync {
    /// Return the latest release's `tag_name` exactly as GitHub stores it
    /// (typically `v0.1.5`). The caller parses it into `SemanticVersion`. Any
    /// transport error, non-2xx, or decode failure is a [`FetchError`] — callers
    /// treat every error as "no update info available".
    fn fetch_latest_tag(&self) -> Result<String, FetchError>;
}

/// The `tag_name`-only decode target (`ReleaseFetcher.swift:65-67`) — every other
/// field GitHub returns is ignored.
#[derive(Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
}

// MARK: - Production impl (objc2 NSURLSession via platform.rs) -------------------

/// The shipped implementation — one synchronous `NSURLSession` GET behind
/// [`crate::platform::http_get`], with the frozen request literals (D1). Zero
/// state; installed once by `app::run`.
pub struct ProductionReleaseFetcher;

impl ReleaseFetcher for ProductionReleaseFetcher {
    fn fetch_latest_tag(&self) -> Result<String, FetchError> {
        let user_agent = super::user_agent();
        // The two headers are BOTH mandatory — GitHub rejects unauthenticated
        // requests with no User-Agent (the frozen request block). 10 s timeout.
        let headers = [
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", user_agent.as_str()),
        ];
        let resp = crate::platform::http_get(&super::releases_latest_endpoint(), &headers, 10.0)
            .map_err(FetchError::Transport)?;
        if !(200..300).contains(&resp.status) {
            return Err(FetchError::HttpStatus(resp.status));
        }
        let decoded: LatestRelease = serde_json::from_slice(&resp.body)
            .map_err(|e| FetchError::Decode(e.to_string()))?;
        Ok(decoded.tag_name)
    }
}

// MARK: - Recording fake --------------------------------------------------------

#[derive(Default)]
struct RecordingState {
    /// The number of `fetch_latest_tag` calls — the checker's seed-from-cache
    /// test asserts this stays 0 (no network on construction).
    calls: usize,
    /// The scripted result the next `fetch_latest_tag` returns. `None` ⇒ the fake
    /// has not been scripted yet; treated as a transport error so an unscripted
    /// call can't masquerade as a successful empty tag.
    scripted: Option<Result<String, FetchError>>,
}

/// The recording fake: returns the scripted tag/error and counts every call.
/// Cheaply clonable (`Arc<Mutex<..>>`-backed) — `run_selftest` installs one clone
/// as the Global and stashes another in the process static so the scenario shares
/// the same log / scripted state.
#[derive(Clone, Default)]
pub struct RecordingReleaseFetcher {
    state: Arc<Mutex<RecordingState>>,
}

// `set_tag` / `set_error` / `call_count` are driven by the `release_check` unit
// tests (this crate) and the `update-check` scenario + §6 composition leg
// (slices 3/4) — the pending-consumer `wake_main_runloop` precedent.
#[allow(dead_code)]
impl RecordingReleaseFetcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the tag the next `fetch_latest_tag` returns (a successful fetch).
    pub fn set_tag(&self, tag: impl Into<String>) {
        self.state.lock().unwrap().scripted = Some(Ok(tag.into()));
    }

    /// Script an error the next `fetch_latest_tag` returns (the silent-failure
    /// leg: the flag must stay put, no pill).
    pub fn set_error(&self, error: FetchError) {
        self.state.lock().unwrap().scripted = Some(Err(error));
    }

    /// How many times the fetcher was invoked — proof of "seeded from cache with
    /// NO network call" (call log empty) or "the check reached the seam".
    pub fn call_count(&self) -> usize {
        self.state.lock().unwrap().calls
    }
}

impl ReleaseFetcher for RecordingReleaseFetcher {
    fn fetch_latest_tag(&self) -> Result<String, FetchError> {
        let mut state = self.state.lock().unwrap();
        state.calls += 1;
        state
            .scripted
            .clone()
            .unwrap_or_else(|| Err(FetchError::Transport("unscripted recording fetcher".into())))
    }
}

// MARK: - The process Global (WorkspaceOps pattern) -----------------------------

/// The installed `ReleaseFetcher` — an `Arc` trait object (see the module doc for
/// why `Arc`, not `Box`). `app::run` installs the production impl; `run_selftest`
/// installs the recording fake. Absent ⇒ [`super::check_now`] is a no-op (the
/// `FilePickerOps` no-op-when-absent discipline).
pub struct ReleaseFetcherGlobal(pub Arc<dyn ReleaseFetcher>);

impl Global for ReleaseFetcherGlobal {}

/// Clone the installed fetcher `Arc` off the Global, or `None` when none is
/// installed (the no-op-when-absent discipline). Cloned ON THE FOREGROUND so the
/// clone can be moved into background work.
pub(crate) fn try_global(cx: &App) -> Option<Arc<dyn ReleaseFetcher>> {
    cx.try_global::<ReleaseFetcherGlobal>().map(|g| g.0.clone())
}

/// Install the production impl as the Global — `app::run` ONLY.
pub fn install_production(cx: &mut App) {
    cx.set_global(ReleaseFetcherGlobal(Arc::new(ProductionReleaseFetcher)));
}

/// Install a fresh recording fake as the Global AND stash a shared clone in the
/// process static (so a scenario can script the tag/error + read the call count) —
/// the `run_selftest` seam, called before any scenario runs. Returns the fake
/// handle.
pub fn install_recording_fake(cx: &mut App) -> RecordingReleaseFetcher {
    let fake = RecordingReleaseFetcher::new();
    cx.set_global(ReleaseFetcherGlobal(Arc::new(fake.clone())));
    *selftest_slot().lock().unwrap() = Some(fake.clone());
    fake
}

fn selftest_slot() -> &'static Mutex<Option<RecordingReleaseFetcher>> {
    static SLOT: Mutex<Option<RecordingReleaseFetcher>> = Mutex::new(None);
    &SLOT
}

/// The recording fake installed by [`install_recording_fake`], if any — the
/// scenario's handle onto the same call log / scripted state as the Global.
/// Consumed by the `update-check` scenario (slice 3).
#[allow(dead_code)]
pub fn selftest_fake() -> Option<RecordingReleaseFetcher> {
    selftest_slot().lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recording fake returns the scripted tag and counts calls.
    #[test]
    fn recording_fake_returns_scripted_tag_and_counts_calls() {
        let fake = RecordingReleaseFetcher::new();
        // Unscripted ⇒ a transport error (never a bogus empty success).
        assert!(matches!(fake.fetch_latest_tag(), Err(FetchError::Transport(_))));
        fake.set_tag("v9.9.9");
        assert_eq!(fake.fetch_latest_tag(), Ok("v9.9.9".to_string()));
        assert_eq!(fake.call_count(), 2, "both invocations were logged");
    }

    /// A scripted error is returned verbatim.
    #[test]
    fn recording_fake_returns_scripted_error() {
        let fake = RecordingReleaseFetcher::new();
        fake.set_error(FetchError::HttpStatus(503));
        assert_eq!(fake.fetch_latest_tag(), Err(FetchError::HttpStatus(503)));
    }

    /// Clones share one state (the Global's copy and the scenario's copy are the
    /// same fake).
    #[test]
    fn clones_share_the_state() {
        let a = RecordingReleaseFetcher::new();
        let b = a.clone();
        a.set_tag("v1.2.3");
        assert_eq!(b.fetch_latest_tag(), Ok("v1.2.3".to_string()));
        assert_eq!(a.call_count(), 1, "the clone's call is visible on the original");
    }
}
