//! R27 — the update-checker backend (U1). A **nudge, not an updater**
//! (`ReleaseChecker.swift:5-8`): poll GitHub Releases on a slow cadence, compare
//! the latest tag to the running version, and — if newer — light a process-wide
//! `update_available` flag the trailing toolbar pill (a later slice) binds to. No
//! download, no Sparkle, no self-update, no settings toggle (strict Swift parity).
//!
//! ## The pieces
//! * [`release_fetch`] — the injectable [`ReleaseFetcher`] seam + the production
//!   objc2 `NSURLSession` GET (behind `platform.rs`) + the recording fake. No
//!   test or scenario ever hits the real network.
//! * [`update_check_store`] — the cached last-seen tag in a NEW `update_check`
//!   section of `ui_settings.json`, read at construction for the frame-1 pill.
//! * This module — the [`ReleaseCheckerGlobal`] (process-wide `update_available`
//!   / `latest_version`), [`check_now`] (one fetch, marshalled to the foreground),
//!   [`start`] (the gated 6 h loop), and the [`update_available`] free fn the
//!   toolbar reads exactly as it reads `active_chrome_accent`.
//!
//! ## Where the flag lives (D2)
//! A single process-wide gpui `Global`, NOT a per-`WindowState` value: Swift's
//! `ReleaseChecker` is a `NiceServices` singleton, and one shared checker keeps
//! the request rate at 4/day regardless of window count. The toolbar reads it via
//! the [`update_available`] free fn; the foreground fetch handler repaints every
//! window via `cx.refresh_windows()` (a Global, not an entity — no `cx.notify()`).
//!
//! ## Hermeticity
//! The worker [`start`]s only from `app::run` and only when
//! [`LAUNCH_CHECK_ENABLED`] (D6: false for the dev build). `run_selftest` installs
//! the recording fetcher + a `with_defaults` temp cache store and NEVER starts the
//! worker; a scenario drives [`check_now`] explicitly against the fake. No
//! launch-time network, no launch-time real-file write.

use std::time::Duration;

use gpui::{App, AsyncApp, Global};
use nice_model::SemanticVersion;

pub mod release_fetch;
pub mod update_check_store;

// The pinned Exported-contract surface. `FetchError` is used internally here;
// the fetcher impls + Global are consumed by the pill/popover + `update-check`
// scenario + §6 composition leg (slices 3/4) and by `app::run`'s installs —
// re-exported now so those land against a stable path (the `wake_main_runloop`
// pending-consumer precedent).
#[allow(unused_imports)]
pub use release_fetch::{
    FetchError, ProductionReleaseFetcher, RecordingReleaseFetcher, ReleaseFetcher,
    ReleaseFetcherGlobal,
};
pub use update_check_store::UpdateCheckStore;

// MARK: - Frozen constants (D6) --------------------------------------------------

/// The GitHub `owner/repo` slug every request is built from (D6). The single point
/// (with [`CASK_NAME`] / [`LAUNCH_CHECK_ENABLED`]) the eventual production Rust
/// release flips.
pub const REPO_SLUG: &str = "Nick-Anderssohn/nice";

/// The Homebrew cask name the popover's `brew upgrade --cask <name>` command uses
/// (D6). Consumed by the pill's popover (slice 3).
#[allow(dead_code)]
pub const CASK_NAME: &str = "nice";

/// Whether the periodic launch check runs. **Gated OFF for the not-yet-released
/// Rust dev build (D6):** `Nice RS Dev.app` ships at crate version `0.1.0` with no
/// published GitHub release or cask of its own, so comparing `0.1.0` against Swift
/// Nice's `vX.Y.Z` tags would light the pill spuriously on every launch. The
/// feature stays fully built, wired, and tested; a scenario exercises the pill via
/// the injected seam (never the launch timer). When the Rust app ships as a real
/// release with its own cask + tags aligned to `CARGO_PKG_VERSION`, flip this to
/// `true` and repoint [`REPO_SLUG`] / [`CASK_NAME`] with no logic change.
pub const LAUNCH_CHECK_ENABLED: bool = false;

/// Delay before the first check after [`start`] — keeps launch quiet (parity,
/// `ReleaseChecker.swift:46`).
const INITIAL_DELAY: Duration = Duration::from_secs(3);

/// Interval between subsequent checks. 6 h keeps a single instance at 4 req/day,
/// far under GitHub's 60 req/hr/IP unauthenticated limit (parity,
/// `ReleaseChecker.swift:52`).
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The `/releases/latest` endpoint, derived from [`REPO_SLUG`] (D6). Hits
/// `/releases/latest`, NOT `/releases`, so drafts + prereleases are excluded for
/// free (there is no prerelease logic anywhere). This is the ONLY `github.com`
/// reference and it is unreachable from any test (all fetch goes through the
/// injected Global).
pub(crate) fn releases_latest_endpoint() -> String {
    format!("https://api.github.com/repos/{REPO_SLUG}/releases/latest")
}

/// The running app's version — the About-pane idiom (D4): the bundle
/// `CFBundleShortVersionString`, else `CARGO_PKG_VERSION` for an unbundled
/// `cargo run` / test binary. In a shipped bundle both equal the plist version.
fn current_version_string() -> String {
    crate::platform::main_bundle_short_version()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// The mandatory `User-Agent` (GitHub rejects unauthenticated requests without
/// one): `Nice/<version> (github.com/<slug>)`, falling back to `dev` only if the
/// version string is empty (the frozen request block).
pub(crate) fn user_agent() -> String {
    let version = current_version_string();
    let version = if version.is_empty() {
        "dev".to_string()
    } else {
        version
    };
    format!("Nice/{version} (github.com/{REPO_SLUG})")
}

// MARK: - The process-wide checker Global (D2) ----------------------------------

/// The process-wide release checker: the two flags the toolbar binds to plus the
/// running version and the started-latch. One per process (D2), installed by
/// `app::run` (or `run_selftest`); the toolbar reads it through [`update_available`].
pub struct ReleaseCheckerGlobal {
    /// The running app's version, the compare baseline (D4).
    current_version: String,
    /// The latest tag we've seen (from cache or a fetch), verbatim. `None` until
    /// the first seed/fetch.
    latest_version: Option<String>,
    /// Whether [`latest_version`](Self::latest_version) is strictly newer than
    /// [`current_version`](Self::current_version) — the pill's visibility.
    update_available: bool,
    /// [`start`] idempotency latch (parity: a second `start()` is a no-op).
    started: bool,
}

impl Global for ReleaseCheckerGlobal {}

impl ReleaseCheckerGlobal {
    fn new(current_version: String) -> Self {
        Self {
            current_version,
            latest_version: None,
            update_available: false,
            started: false,
        }
    }

    /// Adopt `raw` as the latest tag and recompute the flag: `update_available` is
    /// true iff `raw` parses to a strictly-newer version than `current_version`
    /// (`ReleaseChecker.swift:147-158`). A parse failure on EITHER side leaves the
    /// flag false (no pill) — but `latest_version` is still updated to `raw`.
    fn apply_latest(&mut self, raw: &str) {
        self.latest_version = Some(raw.to_string());
        self.update_available = SemanticVersion::is_newer(raw, &self.current_version);
    }

    /// Whether a newer release is available (the pill's render gate). Consumed by
    /// the toolbar read + tests (slice 3).
    #[allow(dead_code)]
    pub fn update_available(&self) -> bool {
        self.update_available
    }

    /// The latest seen tag verbatim (`v0.1.5`), for the popover title (slice 3).
    #[allow(dead_code)]
    pub fn latest_version(&self) -> Option<&str> {
        self.latest_version.as_deref()
    }

    /// The running app's version (the compare baseline). Read by the popover /
    /// diagnostics (slice 3).
    #[allow(dead_code)]
    pub fn current_version(&self) -> &str {
        &self.current_version
    }
}

/// Construct + install the checker Global, seeding it from the cached tag (D3) so
/// the pill can render on frame 1 after relaunch. Reads the [`UpdateCheckStore`]
/// Global (installed before this); an absent store ⇒ no seed. Called from both
/// `app::run` and `run_selftest` (the worker [`start`] is what stays gated to
/// `run`). Under `run_selftest` the store is `with_defaults` (empty), so the seed
/// is inherently a no-op and the pill stays hidden — layout stability.
pub fn install(cx: &mut App) {
    let current = current_version_string();
    let cached = cx
        .try_global::<UpdateCheckStore>()
        .and_then(|s| s.last_known_latest());
    let mut checker = ReleaseCheckerGlobal::new(current);
    if let Some(tag) = cached {
        checker.apply_latest(&tag);
    }
    cx.set_global(checker);
}

/// The toolbar's read (D2): the latest version when a newer release exists, else
/// `None`. Read from `WindowToolbarView::render` exactly as `active_chrome_accent`
/// is (slice 3). Absent Global ⇒ `None` (no pill).
#[allow(dead_code)]
pub fn update_available(cx: &App) -> Option<String> {
    cx.try_global::<ReleaseCheckerGlobal>().and_then(|c| {
        if c.update_available {
            c.latest_version.clone()
        } else {
            None
        }
    })
}

/// Apply one fetch result on the FOREGROUND (`ReleaseChecker.checkNow` body,
/// `ReleaseChecker.swift:135-143`). On success: `apply_latest` (recomputes the
/// flag) then cache the tag **unconditionally** — even an unparseable tag
/// (`:139`) — so a bad response can't re-nag; then `refresh_windows()` to repaint
/// the pill. On error: **swallow it** — `update_available` / `latest_version` keep
/// their prior values, nothing surfaces, no repaint (`:140-142`).
pub(crate) fn apply_fetch_result(cx: &mut App, result: Result<String, FetchError>) {
    let tag = match result {
        Ok(tag) => tag,
        // Intentional: don't surface network errors to the UI; prior state stays.
        Err(_) => return,
    };
    if cx.try_global::<ReleaseCheckerGlobal>().is_some() {
        cx.global_mut::<ReleaseCheckerGlobal>().apply_latest(&tag);
    }
    // Cache on EVERY successful fetch, unconditionally (even garbage) — the
    // only-if-changed guard inside just skips a redundant identical rewrite.
    if cx.try_global::<UpdateCheckStore>().is_some() {
        let _ = cx
            .global_mut::<UpdateCheckStore>()
            .set_last_known_latest(&tag);
    }
    // The checker is a process-wide Global, not an entity, so repaint via
    // refresh_windows (the settings-toggle precedent), NOT cx.notify().
    cx.refresh_windows();
}

/// Run one check immediately (`ReleaseChecker.checkNow`). Exposed for tests, the
/// `update-check` scenario, and a hypothetical future "check now" menu item —
/// wired to no production UI. Clones the fetcher `Arc` ON THE FOREGROUND, runs the
/// blocking fetch on the background executor (the `kickoff_claude_probe` template),
/// and marshals the `Result` back to the foreground [`apply_fetch_result`]. A
/// no-op when no fetcher is installed. Driven by the `update-check` scenario + the
/// §6 composition leg + tests (slices 3/4); wired to no production UI.
#[allow(dead_code)]
pub fn check_now(cx: &mut App) {
    let Some(fetcher) = release_fetch::try_global(cx) else {
        return;
    };
    cx.spawn(async move |acx: &mut AsyncApp| {
        let result = acx
            .background_executor()
            .spawn(async move { fetcher.fetch_latest_tag() })
            .await;
        acx.update(|app| apply_fetch_result(app, result));
    })
    .detach();
}

/// Schedule the first check (after [`INITIAL_DELAY`]) and the repeating
/// [`INTERVAL`] (`ReleaseChecker.start`). **Idempotent** — a second call after the
/// first latches to a no-op. Called from `app::run` ONLY and only when
/// [`LAUNCH_CHECK_ENABLED`] (D6). App-Nap-safe: the loop sleeps on
/// [`crate::platform::nap_safe_delay`] (a dedicated OS-thread sleep that kicks
/// `CFRunLoopWakeUp` — the exact worker-thread-marshal shape the control socket
/// uses, D5), the blocking fetch runs on the background executor, and each result
/// is applied on the foreground. There is no `stop()`: the task is detached and
/// process exit suffices (nothing wires teardown — YAGNI).
pub fn start(cx: &mut App) {
    {
        let checker = cx.global_mut::<ReleaseCheckerGlobal>();
        if checker.started {
            return;
        }
        checker.started = true;
    }
    cx.spawn(async move |acx: &mut AsyncApp| {
        let mut delay = INITIAL_DELAY;
        loop {
            crate::platform::nap_safe_delay(delay).await;
            // No fetcher installed ⇒ nothing to do this cycle; retry next interval.
            let Some(fetcher) = acx.update(|app| release_fetch::try_global(app)) else {
                delay = INTERVAL;
                continue;
            };
            let result = acx
                .background_executor()
                .spawn(async move { fetcher.fetch_latest_tag() })
                .await;
            acx.update(|app| apply_fetch_result(app, result));
            delay = INTERVAL;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use release_fetch::{FetchError, RecordingReleaseFetcher};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nice-release-check-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui_settings.json")
    }

    /// Install the recording fetcher (as the Global), a `with_defaults` temp cache
    /// store, and a checker over `current_version` seeded from whatever the store
    /// holds (empty here). Returns the fake handle so a test can script it.
    fn setup(cx: &mut App, current: &str, cache_path: PathBuf) -> RecordingReleaseFetcher {
        let fake = release_fetch::install_recording_fake(cx);
        cx.set_global(UpdateCheckStore::with_defaults(cache_path));
        cx.set_global(ReleaseCheckerGlobal::new(current.to_string()));
        fake
    }

    /// Drive one check SYNCHRONOUSLY (the deterministic gate-probe path): fetch off
    /// the recording fake (instant, no blocking) then apply on the foreground —
    /// exactly what [`check_now`] does, minus the background-thread hop. Keeps the
    /// gate probes free of executor-pump timing.
    fn drive_check_sync(cx: &mut App) {
        let fetcher = release_fetch::try_global(cx).expect("a fetcher is installed");
        let result = fetcher.fetch_latest_tag();
        apply_fetch_result(cx, result);
    }

    fn checker(cx: &App) -> &ReleaseCheckerGlobal {
        cx.global::<ReleaseCheckerGlobal>()
    }

    // MARK: fetcher → state (ReleaseCheckerTests.swift:93-181)

    /// Gate probe 2(a). A newer tag flips `update_available` on and
    /// `update_available(cx)` returns the tag (`test_newerTag_flipsUpdateAvailable`,
    /// `ReleaseCheckerTests.swift:93-103`).
    #[gpui::test]
    fn newer_tag_flips_update_available(cx: &mut TestAppContext) {
        cx.update(|app| {
            let fake = setup(app, "0.1.4", temp_path("newer"));
            fake.set_tag("v9.9.9");
            assert!(!checker(app).update_available(), "clean start: flag off");
            drive_check_sync(app);
            assert!(checker(app).update_available());
            assert_eq!(checker(app).latest_version(), Some("v9.9.9"));
            assert_eq!(update_available(app), Some("v9.9.9".to_string()));
        });
    }

    /// Gate probe 2(b). An equal tag leaves the flag false
    /// (`test_equalTag_leavesUpdateAvailableFalse`, `ReleaseCheckerTests.swift:105-113`).
    #[gpui::test]
    fn equal_tag_leaves_flag_false(cx: &mut TestAppContext) {
        cx.update(|app| {
            let fake = setup(app, "0.1.5", temp_path("equal"));
            fake.set_tag("v0.1.5");
            drive_check_sync(app);
            assert!(!checker(app).update_available());
            assert_eq!(update_available(app), None);
        });
    }

    /// An older tag leaves the flag false
    /// (`test_olderTag_leavesUpdateAvailableFalse`, `ReleaseCheckerTests.swift:115-123`).
    #[gpui::test]
    fn older_tag_leaves_flag_false(cx: &mut TestAppContext) {
        cx.update(|app| {
            let fake = setup(app, "0.2.0", temp_path("older"));
            fake.set_tag("v0.1.9");
            drive_check_sync(app);
            assert!(!checker(app).update_available());
        });
    }

    /// Gate probe 2(c). A thrown fetch leaves the prior state intact AND writes
    /// nothing new to the cache (`test_fetcherThrow_leavesPreviousStateIntact`,
    /// `ReleaseCheckerTests.swift:125-138`). Seed a known-good state via the cache,
    /// then fail the fetch: the seeded flag/version must survive and the cache file
    /// must be unchanged.
    #[gpui::test]
    fn fetch_error_leaves_previous_state_and_cache_intact(cx: &mut TestAppContext) {
        cx.update(|app| {
            let path = temp_path("error-preserve");
            // Seed the cache with a known-newer tag, then install a checker seeded
            // from it (the frame-1 path) — the flag lights immediately.
            let mut seed = UpdateCheckStore::with_defaults(path.clone());
            seed.set_last_known_latest("v0.2.0").unwrap();
            let bytes_before = std::fs::read(&path).unwrap();

            let fake = release_fetch::install_recording_fake(app);
            app.set_global(UpdateCheckStore::load(path.clone()));
            crate::release_check::install(app); // seeds from the cache
            assert!(
                checker(app).update_available(),
                "cache seed lights the flag immediately"
            );

            fake.set_error(FetchError::Transport("offline".into()));
            drive_check_sync(app);

            // The fetch failed; the cached state must not be wiped.
            assert!(checker(app).update_available());
            assert_eq!(checker(app).latest_version(), Some("v0.2.0"));
            // And no new cache write happened (bytes identical).
            let bytes_after = std::fs::read(&path).unwrap();
            assert_eq!(bytes_before, bytes_after, "an error must not rewrite the cache");
        });
    }

    /// A successful fetch writes `update_check.last_known_latest`
    /// (`test_successfulFetch_writesCacheKey`, `ReleaseCheckerTests.swift:140-151`).
    #[gpui::test]
    fn successful_fetch_writes_cache_key(cx: &mut TestAppContext) {
        cx.update(|app| {
            let path = temp_path("writes-cache");
            let fake = setup(app, "0.1.4", path.clone());
            fake.set_tag("v0.1.5");
            drive_check_sync(app);

            let reloaded = UpdateCheckStore::load(path);
            assert_eq!(reloaded.last_known_latest(), Some("v0.1.5".to_string()));
        });
    }

    /// Gate probe 2(d). A fresh checker constructed over a cache holding `v9.9.9`
    /// reports the pill on frame 1 with the fetcher's call log EMPTY — everything
    /// the pill needs comes from the cached tag alone, NO network
    /// (`test_freshInstanceSeedsFromCache_beforeAnyNetworkCall`,
    /// `ReleaseCheckerTests.swift:153-164`).
    #[gpui::test]
    fn fresh_instance_seeds_from_cache_before_any_network_call(cx: &mut TestAppContext) {
        cx.update(|app| {
            let path = temp_path("seed-no-net");
            let mut seed = UpdateCheckStore::with_defaults(path.clone());
            seed.set_last_known_latest("v9.9.9").unwrap();

            let fake = release_fetch::install_recording_fake(app);
            app.set_global(UpdateCheckStore::load(path));
            // Install a checker over current 0.1.4 — construction seeds from cache.
            {
                let current = "0.1.4".to_string();
                let cached = app
                    .try_global::<UpdateCheckStore>()
                    .and_then(|s| s.last_known_latest());
                let mut c = ReleaseCheckerGlobal::new(current);
                if let Some(tag) = cached {
                    c.apply_latest(&tag);
                }
                app.set_global(c);
            }
            // No check_now — the pill state must come from the seed alone.
            assert!(checker(app).update_available());
            assert_eq!(checker(app).latest_version(), Some("v9.9.9"));
            assert_eq!(fake.call_count(), 0, "the seed made NO fetch call");
        });
    }

    /// Gate probe 2(e). An unparseable tag doesn't crash and leaves the flag false,
    /// but is STILL cached so the same bad response can't re-nag next run
    /// (`test_unparseableTag_doesNotCrashAndLeavesFlagFalse`,
    /// `ReleaseCheckerTests.swift:166-181`).
    #[gpui::test]
    fn unparseable_tag_leaves_flag_false_but_is_still_cached(cx: &mut TestAppContext) {
        cx.update(|app| {
            let path = temp_path("unparseable");
            let fake = setup(app, "0.1.4", path.clone());
            fake.set_tag("not-a-version");
            drive_check_sync(app);

            assert!(!checker(app).update_available(), "garbage ⇒ no pill");
            // But the raw string is still cached (unconditional write).
            let reloaded = UpdateCheckStore::load(path);
            assert_eq!(
                reloaded.last_known_latest(),
                Some("not-a-version".to_string())
            );
        });
    }

    /// The async [`check_now`] wiring end-to-end: clone the `Arc`, hop the
    /// background executor, marshal back — proves the production path (not just the
    /// synchronous core) flips the flag. Pumped via `run_until_parked`.
    #[gpui::test]
    async fn check_now_async_marshals_back_and_flips_flag(cx: &mut TestAppContext) {
        let fake = cx.update(|app| setup(app, "0.1.4", temp_path("async")));
        fake.set_tag("v9.9.9");
        cx.update(|app| check_now(app));
        cx.run_until_parked();
        cx.update(|app| {
            assert!(checker(app).update_available());
            assert_eq!(update_available(app), Some("v9.9.9".to_string()));
            assert_eq!(fake.call_count(), 1, "exactly one fetch reached the seam");
        });
    }

    /// [`update_available`] returns `None` when no checker Global is installed (the
    /// no-pill-when-absent discipline the toolbar relies on).
    #[gpui::test]
    fn update_available_none_without_global(cx: &mut TestAppContext) {
        cx.update(|app| {
            assert_eq!(update_available(app), None);
        });
    }
}
