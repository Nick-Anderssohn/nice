//! `niceties-drop` self-test scenario — the T7 file/image drag-drop escaped-path
//! path (R7 Validation §3).
//!
//! Synthesizing a real OS drag is impractical from a headless driver (and gpui's
//! macOS backend only accepts *filename* drags anyway), so — exactly as the plan
//! permits — this drives the view's drop handler through its test seam
//! ([`TerminalView::handle_external_paths_drop`]) with **constructed**
//! [`ExternalPaths`] events over a real pty, and asserts the exact bytes the view
//! types into the child:
//!
//! 1. one path — escaped, space-padded (DECSET 2004 off);
//! 2. multiple paths — space-joined in drop order;
//! 3. a path with spaces / shell metacharacters — backslash-escaped;
//! 4. the **raw-image fallback** — a drop with no file URLs consults the injected
//!    image-drop provider (here a stub returning a fixed temp path) and types that
//!    path (proving the fallback wiring; the real objc2 pasteboard read is a human
//!    real-drag pass at Nick's next manual session — it is not blocked on here);
//! 5. with DECSET 2004 **on** — the run is framed in `ESC[200~ … ESC[201~`;
//! 6. and never a trailing newline.
//!
//! Capture mechanism is the `input-live` capture-tee child
//! (`sh -c 'stty raw -echo; exec tee <cap>'`): it copies everything the view
//! writes to the pty verbatim into a capture file **and** echoes it back so the
//! core parser still sees an injected DECSET. Unlike the CGEvent scenarios this
//! drives the handler directly, so it needs **no** Accessibility grant.
//!
//! Self-reported gate ([`Gate::SelfReported`]): the pass criterion is byte-exact
//! pty receipt, not frame cadence.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use gpui::{
    div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, ExternalPaths, IntoElement, Render,
    SharedString, Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView};
use nice_theme::AccentPreset;

// -- fixed geometry (font resolution / zoom is covered by niceties-zoom) -----

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FONT_FAMILY: &str = "Menlo";
const FONT_PX: f32 = 13.0;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// The fixed path the stub image-drop provider returns — a plausible temp PNG
/// path with a space, so the raw-image fallback also exercises escaping. Assert
/// on its escaped, space-padded form.
const STUB_IMAGE_PATH: &str = "/private/tmp/nice-rs-drop-image/pasted image.png";

/// The animated container hosting the live [`TerminalView`] (RAF each render so
/// it keeps painting; frame stamp for the harness's per-scenario reset).
struct DropTermView {
    terminal: Entity<TerminalView>,
}

impl Render for DropTermView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().child(self.terminal.clone())
    }
}

// -- small io / assertion helpers (self-contained, like niceties_zoom) -------

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn cap_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Bytes appended to the capture file since offset `start`.
fn cap_since(path: &Path, start: u64) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(all) if (all.len() as u64) >= start => all[start as usize..].to_vec(),
        Ok(all) => all,
        Err(_) => Vec::new(),
    }
}

/// Render bytes with non-printables escaped, for readable byte diffs.
fn esc(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            0x0d => out.push_str("\\r"),
            0x0a => out.push_str("\\n"),
            0x09 => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

fn expect_bytes(failures: &mut Vec<String>, label: &str, want: &[u8], got: &[u8]) {
    if got != want {
        failures.push(format!(
            "{label}: dropped bytes mismatch\n    want: \"{}\"\n    got:  \"{}\"",
            esc(want),
            esc(got)
        ));
    }
}

fn bracketed_active(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> bool {
    handle.update(cx, |h, _| h.session().bracketed_paste_active())
}

fn write_child(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    bytes: &[u8],
) -> Result<()> {
    handle
        .update(cx, |h, _| h.session().write_input(bytes))
        .map_err(|e| anyhow!("pty write failed: {e}"))
}

/// Drive the drop handler with a constructed [`ExternalPaths`] (the test seam).
fn perform_drop(
    cx: &mut AsyncApp,
    terminal: &Entity<TerminalView>,
    paths: &[&str],
) {
    let ep = ExternalPaths(paths.iter().map(|p| PathBuf::from(*p)).collect::<Vec<_>>().into());
    terminal.update(cx, |view, cx| view.handle_external_paths_drop(&ep, cx));
}

// -- scenario ----------------------------------------------------------------

/// Open the `niceties-drop` scenario window (capture-tee session) and spawn the
/// drop-handler assertions (self-reported gate).
pub fn open_niceties_drop_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = std::env::temp_dir().join(format!("nice-rs-niceties-drop-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();

    // Capture-tee child (see the module header): raw mode, tee copies stdin
    // verbatim into the capture file AND echoes it so an injected DECSET reaches
    // the parser.
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;

    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let terminal = {
        let handle = handle.clone();
        cx.new(move |cx| {
            // Fixed-metrics font (Menlo/13px/8×16): this scenario asserts byte-exact
            // pty receipt, not font geometry.
            let font = cx.new(|_cx| {
                FontSettings::fixed(
                    SharedString::from(FONT_FAMILY),
                    FONT_PX,
                    TerminalMetrics::new(CELL_W, CELL_H),
                )
            });
            let mut v = TerminalView::new(handle, theme, accent, font, cx);
            // Stub the raw-image fallback so a no-file-URL drop is exercisable
            // without real pasteboard image data (the real read is human-verified).
            v.set_image_drop_provider(Arc::new(|| Some(PathBuf::from(STUB_IMAGE_PATH))));
            v
        })
    };

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| DropTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_niceties_drop(acx, handle, terminal, cap_path).await;
        eprintln!("[selftest] scenario 'niceties-drop': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn run_niceties_drop(
    cx: &mut AsyncApp,
    handle: Entity<TerminalSessionHandle>,
    terminal: Entity<TerminalView>,
    cap_path: PathBuf,
) -> CadenceReport {
    // Frontmost + painted once (registers the input handler / present kick), and
    // long enough for `stty raw -echo; exec tee` to be live before the first drop.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    let mut failures: Vec<String> = Vec::new();

    // DECSET 2004 must be off at session start (no app enabled it yet).
    if bracketed_active(cx, &handle) {
        failures.push("DECSET 2004 unexpectedly active at session start".into());
    }

    // --- Phase 1 (off): one path with spaces + metacharacters, space-padded ---
    {
        let start = cap_len(&cap_path);
        perform_drop(cx, &terminal, &["/Users/nick/Documents/My File (final).txt"]);
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "one-path-off",
            br#" /Users/nick/Documents/My\ File\ \(final\).txt "#,
            &cap_since(&cap_path, start),
        );
    }

    // --- Phase 2 (off): multiple paths, space-joined in drop order ------------
    {
        let start = cap_len(&cap_path);
        perform_drop(cx, &terminal, &["/a/one", "/b/two three", "/c/four"]);
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "multi-path-off",
            br#" /a/one /b/two\ three /c/four "#,
            &cap_since(&cap_path, start),
        );
    }

    // --- Phase 3 (off): raw-image fallback (no file URLs → stub provider) ------
    {
        let start = cap_len(&cap_path);
        perform_drop(cx, &terminal, &[]); // empty ExternalPaths
        settle(cx, 200).await;
        // The stub path, escaped + space-padded (it contains a space).
        let want = format!(
            " {} ",
            STUB_IMAGE_PATH.replace(' ', "\\ ")
        );
        expect_bytes(
            &mut failures,
            "image-fallback-off",
            want.as_bytes(),
            &cap_since(&cap_path, start),
        );
    }

    // --- Enable DECSET 2004 (echoed back by tee → parser sets the mode bit) ---
    if let Err(e) = write_child(cx, &handle, b"\x1b[?2004h") {
        failures.push(format!("could not enable DECSET 2004: {e}"));
    }
    let mut on = false;
    for _ in 0..40 {
        if bracketed_active(cx, &handle) {
            on = true;
            break;
        }
        settle(cx, 25).await;
    }
    if !on {
        failures.push("DECSET 2004 never became active after ESC[?2004h".into());
    }
    // Let tee flush the echoed DECSET bytes so the next offset excludes them.
    settle(cx, 150).await;

    // --- Phase 4 (on): one path, framed in bracketed-paste markers ------------
    {
        let start = cap_len(&cap_path);
        perform_drop(cx, &terminal, &["/x/plain.txt"]);
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "one-path-on",
            b"\x1b[200~/x/plain.txt\x1b[201~",
            &cap_since(&cap_path, start),
        );
    }

    // --- Phase 5 (on): multiple paths, joined then framed (no padding) --------
    {
        let start = cap_len(&cap_path);
        perform_drop(cx, &terminal, &["/a/one", "/b/two three"]);
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "multi-path-on",
            b"\x1b[200~/a/one /b/two\\ three\x1b[201~",
            &cap_since(&cap_path, start),
        );
    }

    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail:
                "drop handler byte-exact: escaped + space-joined paths (padded when DECSET 2004 \
                 off, ESC[200~…ESC[201~ framed when on), raw-image fallback types the provider's \
                 temp path; no trailing newline. (Real OS image drag is a deferred human pass.)"
                    .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} niceties-drop assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
