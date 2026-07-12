//! `pasteboard` — the file-browser Copy / Cut / Paste pasteboard adapter (F7).
//! Ported from `FilePasteboardAdapter.swift`. Reads and writes file URLs over
//! the standard `public.file-url` type so Nice interoperates with Finder both
//! ways.
//!
//! This is the gpui-free **model half**: the [`FilePasteboard`] trait (the raw
//! system-pasteboard surface), its recording [`FakeFilePasteboard`], and the
//! [`FilePasteboardAdapter`] cut-intent logic. The production objc2
//! `FilePasteboard` (gpui's own write path silently drops `ExternalPaths`, so
//! we own the write) and the named-pasteboard integration tests land in a later
//! slice.
//!
//! ## Cut semantics (frozen — an in-process fiction)
//!
//! macOS has no native "cut files" concept. Writing file URLs with [`Intent::Cut`]
//! records a [`CutCompanion`] (change_count + urls) BESIDE the real pasteboard
//! write; external pasters ALWAYS see a plain copy. A read reports `Cut` intent
//! ONLY if a companion exists AND its change_count equals the live pasteboard's
//! AND its URL list equals the read list — any pasteboard mutation by anyone
//! silently degrades cut→copy and un-ghosts the rows. Cut intent is cleared on
//! ANY text write (Copy Path included).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The raw pasteboard surface — the system-pasteboard operations the adapter
/// needs. `write_file_urls` = `clearContents` + `writeObjects:[NSURL]`;
/// `read_file_urls` = `readObjectsForClasses:[NSURL]` filtered to file URLs;
/// `write_text` = `clearContents` + `setString:forType:`; `change_count` =
/// `changeCount`. The production impl binds the general system pasteboard in
/// `app::run`; tests use [`FakeFilePasteboard`].
pub trait FilePasteboard {
    fn write_file_urls(&mut self, urls: &[PathBuf]);
    fn read_file_urls(&self) -> Vec<PathBuf>;
    fn write_text(&mut self, text: &str);
    fn change_count(&self) -> i64;
}

/// Recording fake pasteboard over an in-memory item list. Each write bumps
/// `change_count` exactly like the system pasteboard, so an "external mutation" in tests
/// is just another write through the same handle (the
/// `test_externalChangeCount_invalidatesCutIntent` precedent). Also stores
/// non-file content (plain text, web URLs) so the adapter's file-URL filtering
/// is exercised.
pub struct FakeFilePasteboard {
    items: Vec<PbItem>,
    change_count: i64,
}

enum PbItem {
    File(PathBuf),
    Text(String),
    Web(String),
}

impl Default for FakeFilePasteboard {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            change_count: 0,
        }
    }
}

impl FakeFilePasteboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The plain-text content currently on the pasteboard, if any (the Swift
    /// `pasteboard.string(forType: .string)` read-back for the Copy-Path test).
    pub fn text(&self) -> Option<String> {
        self.items.iter().find_map(|i| match i {
            PbItem::Text(s) => Some(s.clone()),
            _ => None,
        })
    }

    /// Test-only: write a mix of file and web URLs (another app's clipboard)
    /// so the adapter's file-URL filtering is exercised. Bumps `change_count`.
    pub fn write_mixed_file_and_web(&mut self, files: &[PathBuf], webs: &[&str]) {
        self.items = files
            .iter()
            .map(|f| PbItem::File(f.clone()))
            .chain(webs.iter().map(|w| PbItem::Web((*w).to_string())))
            .collect();
        self.change_count += 1;
    }
}

impl FilePasteboard for FakeFilePasteboard {
    fn write_file_urls(&mut self, urls: &[PathBuf]) {
        self.items = urls.iter().map(|u| PbItem::File(u.clone())).collect();
        self.change_count += 1;
    }

    fn read_file_urls(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter_map(|i| match i {
                PbItem::File(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    fn write_text(&mut self, text: &str) {
        self.items = vec![PbItem::Text(text.to_string())];
        self.change_count += 1;
    }

    fn change_count(&self) -> i64 {
        self.change_count
    }
}

/// Forward the trait through a boxed object so the process-wide adapter can hold
/// either the production pasteboard or a fake behind one `Box<dyn FilePasteboard>`
/// (the [`FilePasteboardGlobal`] type).
impl FilePasteboard for Box<dyn FilePasteboard> {
    fn write_file_urls(&mut self, urls: &[PathBuf]) {
        (**self).write_file_urls(urls);
    }
    fn read_file_urls(&self) -> Vec<PathBuf> {
        (**self).read_file_urls()
    }
    fn write_text(&mut self, text: &str) {
        (**self).write_text(text);
    }
    fn change_count(&self) -> i64 {
        (**self).change_count()
    }
}

/// The shipped [`FilePasteboard`]: forwards to the objc2 pasteboard handle in
/// [`crate::platform`] (the only module that touches AppKit's pasteboard). Holds a
/// retained [`crate::platform::PasteboardRef`] — the general system pasteboard when
/// `app::run` binds it, or (in the round-trip integration tests) an isolated named
/// pasteboard. gpui's own write path silently drops `ExternalPaths`
/// (`gpui_macos/src/pasteboard.rs:178`), so the browser owns this write.
pub struct ProductionFilePasteboard {
    pasteboard: crate::platform::PasteboardRef,
}

impl ProductionFilePasteboard {
    /// Over a bound [`crate::platform::PasteboardRef`] (general in `app::run`;
    /// named in tests). The `PasteboardRef` construction — which reaches AppKit —
    /// stays with the caller so this type is just the trait adapter.
    pub fn new(pasteboard: crate::platform::PasteboardRef) -> Self {
        Self { pasteboard }
    }
}

impl FilePasteboard for ProductionFilePasteboard {
    fn write_file_urls(&mut self, urls: &[PathBuf]) {
        self.pasteboard.write_file_urls(urls);
    }
    fn read_file_urls(&self) -> Vec<PathBuf> {
        self.pasteboard.read_file_urls()
    }
    fn write_text(&mut self, text: &str) {
        self.pasteboard.write_text(text);
    }
    fn change_count(&self) -> i64 {
        self.pasteboard.change_count()
    }
}

/// Copy vs cut intent recorded by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Copy,
    Cut,
}

/// Result of a successful read: the file URLs and the (in-process) intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteboardRead {
    pub urls: Vec<PathBuf>,
    pub intent: Intent,
}

/// The in-process cut companion — stamped on every cut write, keyed by
/// `change_count` so any unrelated mutation invalidates the cut.
#[derive(Clone)]
struct CutCompanion {
    change_count: i64,
    urls: Vec<PathBuf>,
}

/// The adapter: cut-intent + Copy-Path text over an injectable
/// [`FilePasteboard`].
pub struct FilePasteboardAdapter<P: FilePasteboard> {
    pasteboard: P,
    cut_companion: Option<CutCompanion>,
    last_written_change_count: Option<i64>,
}

impl<P: FilePasteboard> FilePasteboardAdapter<P> {
    pub fn new(pasteboard: P) -> Self {
        Self {
            pasteboard,
            cut_companion: None,
            last_written_change_count: None,
        }
    }

    /// The `change_count` immediately after our last write, or `None` before any
    /// write. Lets observers tell whether our content is still the latest.
    pub fn last_written_change_count(&self) -> Option<i64> {
        self.last_written_change_count
    }

    // MARK: - Read

    /// Read the current pasteboard as file URLs. `None` if there are no file
    /// URLs. Cut intent is reported only when the companion's change_count
    /// matches the live pasteboard's AND its URL list equals the read list.
    pub fn read(&self) -> Option<PasteboardRead> {
        let file_urls = self.pasteboard.read_file_urls();
        if file_urls.is_empty() {
            return None;
        }
        let intent = match &self.cut_companion {
            Some(c)
                if c.change_count == self.pasteboard.change_count() && c.urls == file_urls =>
            {
                Intent::Cut
            }
            _ => Intent::Copy,
        };
        Some(PasteboardRead {
            urls: file_urls,
            intent,
        })
    }

    // MARK: - Write

    /// Replace the pasteboard with `urls`. For [`Intent::Cut`], stamp the
    /// in-process companion so the next in-process read sees `Cut`; external
    /// pasters always see copies.
    pub fn write(&mut self, urls: &[PathBuf], intent: Intent) {
        self.pasteboard.write_file_urls(urls);
        let count = self.pasteboard.change_count();
        self.last_written_change_count = Some(count);
        self.cut_companion = match intent {
            Intent::Copy => None,
            Intent::Cut => Some(CutCompanion {
                change_count: count,
                urls: urls.to_vec(),
            }),
        };
    }

    /// Replace the pasteboard with plain text (Copy Path — the adapter owns
    /// every file-browser pasteboard mutation). Clears any cut companion since
    /// the previous file URLs are no longer current.
    pub fn write_text(&mut self, text: &str) {
        self.pasteboard.write_text(text);
        self.cut_companion = None;
        self.last_written_change_count = Some(self.pasteboard.change_count());
    }

    /// Forget any cut companion so the next read reports `Copy`. Called after a
    /// paste-from-cut completes — the sources have moved, so the cut highlight
    /// clears.
    pub fn clear_cut_intent(&mut self) {
        self.cut_companion = None;
    }

    /// True iff the cut companion is still pointed at the live pasteboard and
    /// contains `url` — drives the 0.45 "ghost" opacity on cut rows.
    pub fn is_cut(&self, url: &Path) -> bool {
        match &self.cut_companion {
            Some(c) if c.change_count == self.pasteboard.change_count() => {
                c.urls.iter().any(|u| u == url)
            }
            _ => false,
        }
    }

    /// Snapshot of paths currently in the cut companion (empty if cut intent
    /// isn't current) — the observable set the R19 rows read for ghosting.
    pub fn cut_paths(&self) -> HashSet<PathBuf> {
        match &self.cut_companion {
            Some(c) if c.change_count == self.pasteboard.change_count() => {
                c.urls.iter().cloned().collect()
            }
            _ => HashSet::new(),
        }
    }
}

// MARK: - The process Global (SharedFontSettings / WorkspaceOpsGlobal pattern) --

/// The ONE process-wide pasteboard adapter — the cut companion is in-process
/// state that must persist across menu invocations and be observable for row
/// ghosting, so there is exactly one, in a gpui `Global`. `app::run` installs it
/// over the production general pasteboard; a scenario/test installs it over a
/// fake. Absent ⇒ the browser's Copy / Cut / Paste / Copy-Path are no-ops (and
/// `can_paste` reads false) — never a fallback to the real general pasteboard.
pub struct FilePasteboardGlobal(pub FilePasteboardAdapter<Box<dyn FilePasteboard>>);

impl gpui::Global for FilePasteboardGlobal {}

impl FilePasteboardGlobal {
    /// Wrap a boxed [`FilePasteboard`] in the adapter + the Global newtype.
    pub fn new(pasteboard: Box<dyn FilePasteboard>) -> Self {
        Self(FilePasteboardAdapter::new(pasteboard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn adapter() -> FilePasteboardAdapter<FakeFilePasteboard> {
        FilePasteboardAdapter::new(FakeFilePasteboard::new())
    }

    // MARK: - Round-trip

    /// `FilePasteboardAdapterTests.test_writeCopy_thenRead_returnsCopyIntent`
    #[test]
    fn write_copy_then_read_returns_copy_intent() {
        let mut a = adapter();
        let url = path("/tmp/file.txt");
        a.write(&[url.clone()], Intent::Copy);
        let read = a.read().unwrap();
        assert_eq!(read.urls, vec![url]);
        assert_eq!(read.intent, Intent::Copy);
    }

    /// `FilePasteboardAdapterTests.test_writeCut_thenRead_returnsCutIntent`
    #[test]
    fn write_cut_then_read_returns_cut_intent() {
        let mut a = adapter();
        let url = path("/tmp/file.txt");
        a.write(&[url.clone()], Intent::Cut);
        let read = a.read().unwrap();
        assert_eq!(read.urls, vec![url]);
        assert_eq!(read.intent, Intent::Cut);
    }

    /// `FilePasteboardAdapterTests.test_externalChangeCount_invalidatesCutIntent`
    /// — the changeCount cut-invalidation spec case.
    #[test]
    fn external_change_count_invalidates_cut_intent() {
        let mut a = adapter();
        a.write(&[path("/tmp/file.txt")], Intent::Cut);
        // Another app bumps the pasteboard (a write through the same handle).
        a.pasteboard.write_file_urls(&[path("/tmp/other.txt")]);
        let read = a.read().unwrap();
        assert_eq!(
            read.intent,
            Intent::Copy,
            "cut intent must invalidate when the change count moves under us"
        );
    }

    /// `FilePasteboardAdapterTests.test_writeMultipleURLs_roundtrips`
    #[test]
    fn write_multiple_urls_roundtrips() {
        let mut a = adapter();
        let urls = vec![path("/tmp/a.txt"), path("/tmp/b.txt")];
        a.write(&urls, Intent::Copy);
        assert_eq!(a.read().unwrap().urls, urls);
    }

    /// `FilePasteboardAdapterTests.test_read_emptyPasteboard_returnsNil`
    #[test]
    fn read_empty_pasteboard_returns_none() {
        let a = adapter();
        assert!(a.read().is_none());
    }

    /// `FilePasteboardAdapterTests.test_read_nonFileURLContent_returnsNil`
    #[test]
    fn read_non_file_url_content_returns_none() {
        let mut a = adapter();
        a.pasteboard.write_text("hello");
        assert!(a.read().is_none());
    }

    // MARK: - Cut companion

    /// `FilePasteboardAdapterTests.test_clearCutIntent_removesCutHighlight`
    #[test]
    fn clear_cut_intent_removes_cut_highlight() {
        let mut a = adapter();
        let url = path("/tmp/file.txt");
        a.write(&[url.clone()], Intent::Cut);
        assert!(a.is_cut(&url));
        a.clear_cut_intent();
        assert!(!a.is_cut(&url));
    }

    /// `FilePasteboardAdapterTests.test_isCut_reflectsCurrentCompanion`
    #[test]
    fn is_cut_reflects_current_companion() {
        let mut a = adapter();
        let cut = path("/tmp/cut.txt");
        let other = path("/tmp/other.txt");
        a.write(&[cut.clone()], Intent::Cut);
        assert!(a.is_cut(&cut));
        assert!(!a.is_cut(&other));
    }

    /// `FilePasteboardAdapterTests.test_overwriteWithCopy_clearsCutCompanion`
    #[test]
    fn overwrite_with_copy_clears_cut_companion() {
        let mut a = adapter();
        let url = path("/tmp/file.txt");
        a.write(&[url.clone()], Intent::Cut);
        assert_eq!(a.read().unwrap().intent, Intent::Cut);
        a.write(&[url.clone()], Intent::Copy);
        assert_eq!(a.read().unwrap().intent, Intent::Copy);
        assert!(!a.is_cut(&url));
    }

    /// `cut_paths()` mirrors the companion — the observable ghost set.
    #[test]
    fn cut_paths_reflects_companion() {
        let mut a = adapter();
        let url = path("/tmp/cut.txt");
        a.write(&[url.clone()], Intent::Cut);
        assert_eq!(a.cut_paths(), [url.clone()].into_iter().collect());
        a.clear_cut_intent();
        assert!(a.cut_paths().is_empty());
    }

    // MARK: - Mixed and edge content

    /// `FilePasteboardAdapterTests.test_read_mixedFileAndHTTPURLs_returnsOnlyFileURLs`
    #[test]
    fn read_mixed_file_and_http_urls_returns_only_file_urls() {
        let mut a = adapter();
        a.pasteboard
            .write_mixed_file_and_web(&[path("/tmp/file.txt")], &["https://example.com"]);
        let read = a.read().unwrap();
        assert_eq!(read.urls.len(), 1);
        assert_eq!(read.urls[0].file_name().unwrap(), "file.txt");
    }

    /// `FilePasteboardAdapterTests.test_externalClear_afterCutWrite_readReturnsNil`
    #[test]
    fn external_clear_after_cut_write_read_returns_none() {
        let mut a = adapter();
        a.write(&[path("/tmp/cut.txt")], Intent::Cut);
        // Another app replaces the content with plain text.
        a.pasteboard.write_text("hello");
        assert!(
            a.read().is_none(),
            "plain-text content with no file URLs reads as none regardless of stale cut companion"
        );
    }

    // MARK: - writeText (Copy Path)

    /// `FilePasteboardAdapterTests.test_writeText_writesNewlineSeparatedString_andClearsCutIntent`
    #[test]
    fn write_text_writes_newline_separated_string_and_clears_cut_intent() {
        let mut a = adapter();
        let url = path("/tmp/cut.txt");
        a.write(&[url.clone()], Intent::Cut);
        assert!(a.is_cut(&url));

        a.write_text("/tmp/a.txt\n/tmp/b.txt");
        assert_eq!(a.pasteboard.text().as_deref(), Some("/tmp/a.txt\n/tmp/b.txt"));
        assert!(a.read().is_none(), "write_text replaces file URLs with text");
        assert!(!a.is_cut(&url), "write_text clears the cut companion");
    }
}

// MARK: - Named-pasteboard objc2 round-trip integration tests -------------------
//
// The isolated-pasteboard cases from `FilePasteboardAdapterTests.swift:23-34`
// re-cast onto the REAL objc2 write/read path: a NAMED AppKit pasteboard (invisible
// to Finder / other apps — the named-pasteboard + releaseGlobally precedent)
// backs a `ProductionFilePasteboard`, so this exercises the SAME `writeObjects:`
// / `readObjectsForClasses:` / `setString:forType:` / `changeCount` objc2 code the
// shipped general pasteboard runs, without ever mutating the general pasteboard
// (mutating it is a blocking hermeticity finding). Each test owns a uniquely-named
// pasteboard and destroys it with `releaseGlobally` before returning.
#[cfg(test)]
mod named_pasteboard_integration_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A per-test uniquely-named pasteboard, released globally on drop so no named
    /// pasteboard leaks between cases.
    struct NamedPasteboard {
        name: String,
    }

    impl NamedPasteboard {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let name = format!(
                "nice-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            );
            Self { name }
        }

        /// A production adapter over a fresh handle onto THIS named pasteboard.
        fn adapter(&self) -> FilePasteboardAdapter<ProductionFilePasteboard> {
            // SAFETY: a `#[test]` provides no AppKit autorelease pool by default, so
            // the caller wraps each body in `autoreleasepool`; a named pasteboard is
            // independent of app state and needs no running NSApplication.
            let pb = unsafe { crate::platform::PasteboardRef::named(&self.name) };
            FilePasteboardAdapter::new(ProductionFilePasteboard::new(pb))
        }

        /// Destroy the named pasteboard globally (called at the end of each case).
        fn release(&self) {
            // SAFETY: a named pasteboard we own; never the general one.
            let pb = unsafe { crate::platform::PasteboardRef::named(&self.name) };
            unsafe { pb.release_globally() };
        }
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    /// `test_writeCopy_thenRead_returnsCopyIntent` over the real objc2 path.
    #[test]
    fn objc2_write_copy_then_read_returns_copy_intent() {
        objc2::rc::autoreleasepool(|_| {
            let named = NamedPasteboard::new();
            let mut a = named.adapter();
            let url = tmp("nice-pb-copy.txt");
            a.write(&[url.clone()], Intent::Copy);
            let read = a.read().expect("file URLs round-trip through the named pasteboard");
            assert_eq!(read.urls, vec![url]);
            assert_eq!(read.intent, Intent::Copy);
            named.release();
        });
    }

    /// `test_writeCut_thenRead_returnsCutIntent` — cut is an in-process fiction; the
    /// objc2 write still round-trips the URLs, and the companion reports Cut.
    #[test]
    fn objc2_write_cut_then_read_returns_cut_intent() {
        objc2::rc::autoreleasepool(|_| {
            let named = NamedPasteboard::new();
            let mut a = named.adapter();
            let url = tmp("nice-pb-cut.txt");
            a.write(&[url.clone()], Intent::Cut);
            let read = a.read().expect("file URLs round-trip");
            assert_eq!(read.urls, vec![url.clone()]);
            assert_eq!(read.intent, Intent::Cut);
            assert!(a.is_cut(&url));
            named.release();
        });
    }

    /// `test_writeMultipleURLs_roundtrips` over the real `writeObjects:` path.
    #[test]
    fn objc2_write_multiple_urls_roundtrips() {
        objc2::rc::autoreleasepool(|_| {
            let named = NamedPasteboard::new();
            let mut a = named.adapter();
            let urls = vec![tmp("nice-pb-a.txt"), tmp("nice-pb-b.txt")];
            a.write(&urls, Intent::Copy);
            assert_eq!(a.read().expect("round-trip").urls, urls);
            named.release();
        });
    }

    /// `test_externalChangeCount_invalidatesCutIntent` — a real second write bumps
    /// the live `changeCount`, degrading the stale cut companion to a copy.
    #[test]
    fn objc2_external_change_count_invalidates_cut_intent() {
        objc2::rc::autoreleasepool(|_| {
            let named = NamedPasteboard::new();
            let mut a = named.adapter();
            a.write(&[tmp("nice-pb-cut.txt")], Intent::Cut);
            // Simulate another writer bumping the SAME real pasteboard.
            let other = unsafe { crate::platform::PasteboardRef::named(&named.name) };
            other.write_file_urls(&[tmp("nice-pb-other.txt")]);
            let read = a.read().expect("round-trip");
            assert_eq!(
                read.intent,
                Intent::Copy,
                "cut intent invalidates when the real changeCount moves"
            );
            named.release();
        });
    }

    /// `test_writeText_writesNewlineSeparatedString_andClearsCutIntent` — Copy Path
    /// over the real `setString:forType:` path; the file URLs are gone afterward.
    #[test]
    fn objc2_write_text_clears_file_urls_and_cut() {
        objc2::rc::autoreleasepool(|_| {
            let named = NamedPasteboard::new();
            let mut a = named.adapter();
            let url = tmp("nice-pb-cut.txt");
            a.write(&[url.clone()], Intent::Cut);
            assert!(a.is_cut(&url));
            a.write_text("/tmp/a.txt\n/tmp/b.txt");
            assert!(a.read().is_none(), "text write replaces the file URLs");
            assert!(!a.is_cut(&url), "text write clears the cut companion");
            named.release();
        });
    }

    /// `test_read_emptyPasteboard_returnsNil` over the real read path.
    #[test]
    fn objc2_read_empty_pasteboard_returns_none() {
        objc2::rc::autoreleasepool(|_| {
            let named = NamedPasteboard::new();
            let a = named.adapter();
            assert!(a.read().is_none());
            named.release();
        });
    }
}
