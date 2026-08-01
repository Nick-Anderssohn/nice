//! `niceties-link` self-test scenario — the ⌘+click / ⌘-hover terminal link
//! path (the "⌘+click a printed URL does nothing" bug).
//!
//! The unit tests cover the matching (`nice-term-view`'s `hyperlink` module) and
//! the underline's route through the row cache (`element.rs`); what neither can
//! reach is the **gpui event wiring** — that a real press/release pair over a
//! link arms, opens, starts no selection, and writes nothing to the pty, and
//! that the hover follows the ⌘ key. So this scenario drives the shipped
//! listeners through gpui's own dispatch (`Window::dispatch_event`, the same
//! seam `multiwindow` uses for its modifiers-release) over a **real pty**:
//! constructed `MouseMove` / `MouseDown` / `MouseUp` / `ModifiersChanged` events
//! land in the window's rendered-frame hit test exactly as the platform's would.
//! Nothing here calls a view method the app doesn't call.
//!
//! Two things are ground truth rather than assertion helpers:
//!
//! 1. **The opener** is the injected [`UrlOpener`] seam
//!    ([`TerminalView::set_url_opener`]) with a *recording* closure, so "did the
//!    click open the URL" is answered exactly, with no browser launched (the
//!    real `NSWorkspace openURL:` is the one manual-adjacent black-box check).
//! 2. **The pty** is the `input-live` capture-tee child
//!    (`sh -c 'stty raw -echo; exec tee <cap>'`): everything the view writes is
//!    copied verbatim into a capture file *and* echoed back, so the printed URL
//!    reaches the grid and the "⌘+click sent nothing" claim is a byte count, not
//!    an inference.
//!
//! The cases:
//!
//! 1. ⌘+move over the URL sets the hover; ⌘ **release** clears it; ⌘ **press**
//!    with the pointer parked there sets it again (no pointer twitch needed);
//!    ⌘+move off the URL clears it.
//! 2. ⌘+click on the URL — the opener records exactly that URL, no selection is
//!    created, and **zero** bytes reach the pty.
//! 3. The same ⌘+click with the app's mouse reporting ON (the reported bug's
//!    real setting — a URL printed inside Claude Code): still opens, still
//!    writes nothing, i.e. ⌘ bypasses the report like Ghostty.
//! 4. ⌘+click **off** any link with reporting on — the opener is NOT called and
//!    the ordinary SGR press/release report IS sent, byte-exact.
//!
//! Self-reported gate ([`Gate::SelfReported`]): the pass criterion is recorded
//! state + byte-exact pty receipt, not frame cadence. Like `niceties-drop` it
//! synthesizes no CGEvents, so it needs **no** Accessibility grant.
//!
//! [`Gate::SelfReported`]: nice_harness::selftest::Gate::SelfReported
//! [`UrlOpener`]: nice_term_view::UrlOpener

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use gpui::{
    div, point, prelude::*, px, AnyWindowHandle, AsyncApp, Capslock, Context, Entity, IntoElement,
    Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, PlatformInput, Point, Render, SharedString, Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    grid_top_y, FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::AccentPreset;

// -- fixed geometry (font resolution / zoom is covered by niceties-zoom) -----

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FONT_FAMILY: &str = "Menlo";
const FONT_PX: f32 = 13.0;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// The URL printed into the grid — the reported case verbatim (an npm-serve
/// style `http://localhost:5173/`). Its trailing `/` is not trimmable
/// punctuation, so the opened text must equal this string exactly.
const URL: &str = "http://localhost:5173/";

/// Printed after the URL so the line has non-link content too (and so a stray
/// match past the URL's end would show up as a text mismatch).
const TAIL: &str = " ok";

/// A column well past the printed line — blank cells, so a click there is
/// unambiguously "not on a link".
const OFF_LINK_COL: usize = 60;

/// The animated container hosting the live [`TerminalView`] (RAF each render so
/// it keeps painting; frame stamp for the harness's per-scenario reset).
struct LinkTermView {
    terminal: Entity<TerminalView>,
}

impl Render for LinkTermView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().child(self.terminal.clone())
    }
}

// -- small io / assertion helpers (self-contained, like niceties_drop) -------

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
            "{label}: pty bytes mismatch\n    want: \"{}\"\n    got:  \"{}\"",
            esc(want),
            esc(got)
        ));
    }
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

/// The child's visible grid, line by line.
fn grid_lines(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> Vec<String> {
    handle.update(cx, |h, _| h.session().grid_lines())
}

/// Where [`URL`] currently sits in the visible grid, as `(vrow, col)` of its
/// first cell. Re-located before every phase rather than assumed: the echoed
/// mouse reports of the reporting phases are themselves grid input, so the line
/// is free to move.
fn find_url_cell(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> Option<(usize, usize)> {
    grid_lines(cx, handle)
        .iter()
        .enumerate()
        .find_map(|(row, line)| line.find(URL).map(|col| (row, col)))
}

/// Whatever text is currently selected (the ⌘+click contract: none).
fn selection_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> Option<String> {
    handle.update(cx, |h, _| h.selection_text())
}

// -- synthetic input through gpui's real dispatch ----------------------------

/// Window-space centre of grid cell `(vrow, col)`, derived from the bounds the
/// view's last paint published — the very frame its `hit_cell` measures in, so
/// this cannot drift from the hit-test. `None` before the first paint.
fn cell_center(
    cx: &mut AsyncApp,
    terminal: &Entity<TerminalView>,
    vrow: usize,
    col: usize,
) -> Option<Point<Pixels>> {
    terminal.update(cx, |view, _| {
        let bounds = view.paint_bounds()?;
        let m = view.metrics();
        Some(point(
            px(f32::from(bounds.origin.x) + (col as f32 + 0.5) * m.cell_w),
            px(grid_top_y(bounds) + (vrow as f32 + 0.5) * m.cell_h),
        ))
    })
}

/// Feed one platform event into the window through gpui's own dispatch, which
/// hit-tests it against the rendered frame and runs the real listeners.
fn dispatch(cx: &mut AsyncApp, window: AnyWindowHandle, input: PlatformInput) {
    let _ = window.update(cx, |_root, win, app| {
        win.dispatch_event(input, app);
    });
}

fn mouse_move(cx: &mut AsyncApp, window: AnyWindowHandle, pos: Point<Pixels>, modifiers: Modifiers) {
    dispatch(
        cx,
        window,
        PlatformInput::MouseMove(MouseMoveEvent {
            position: pos,
            pressed_button: None,
            modifiers,
        }),
    );
}

/// A left press + release at the same point — the shape a ⌘+click has to have
/// to open (the view arms on the press and opens on the matching release).
fn left_click(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    pos: Point<Pixels>,
    modifiers: Modifiers,
) {
    dispatch(
        cx,
        window,
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: pos,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
    );
    dispatch(
        cx,
        window,
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: pos,
            modifiers,
            click_count: 1,
        }),
    );
}

/// A modifiers-changed edge (⌘ down / ⌘ up). Routed to the focused element, so
/// the scenario focuses the terminal before using it.
fn set_modifiers(cx: &mut AsyncApp, window: AnyWindowHandle, modifiers: Modifiers) {
    dispatch(
        cx,
        window,
        PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers,
            capslock: Capslock { on: false },
        }),
    );
}

fn hovered(cx: &mut AsyncApp, terminal: &Entity<TerminalView>) -> Option<String> {
    terminal.update(cx, |view, _| {
        view.hovered_hyperlink_url().map(str::to_string)
    })
}

fn expect_hover(
    failures: &mut Vec<String>,
    label: &str,
    want: Option<&str>,
    got: Option<String>,
) {
    if got.as_deref() != want {
        failures.push(format!(
            "{label}: hovered link was {got:?}, expected {want:?}"
        ));
    }
}

// -- scenario ----------------------------------------------------------------

/// Open the `niceties-link` scenario window (capture-tee session, recording URL
/// opener) and spawn the ⌘+click / ⌘-hover assertions (self-reported gate).
pub fn open_niceties_link_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = std::env::temp_dir().join(format!("nice-niceties-link-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();

    // Capture-tee child (see the module header): raw mode, tee copies stdin
    // verbatim into the capture file AND echoes it, so the URL we write reaches
    // the grid and every byte the view writes is countable.
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;

    // The recording opener stands in for `platform::workspace_open_url`, so a
    // ⌘+click is observable without launching a browser.
    let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let terminal = {
        let handle = handle.clone();
        let opened = opened.clone();
        cx.new(move |cx| {
            // Fixed-metrics font (Menlo/13px/8×16): this scenario asserts state +
            // pty bytes, not font geometry.
            let font = cx.new(|_cx| {
                FontSettings::fixed(
                    SharedString::from(FONT_FAMILY),
                    FONT_PX,
                    TerminalMetrics::new(CELL_W, CELL_H),
                )
            });
            let mut v = TerminalView::new(handle, theme, accent, font, cx);
            v.set_url_opener(Arc::new(move |url: &str| {
                opened.lock().expect("opener log").push(url.to_string());
            }));
            v
        })
    };

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| LinkTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_niceties_link(acx, handle, terminal, window, cap_path, opened).await;
        eprintln!("[selftest] scenario 'niceties-link': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn run_niceties_link(
    cx: &mut AsyncApp,
    handle: Entity<TerminalSessionHandle>,
    terminal: Entity<TerminalView>,
    window: AnyWindowHandle,
    cap_path: PathBuf,
    opened: Arc<Mutex<Vec<String>>>,
) -> CadenceReport {
    // Frontmost + painted once (the hit test runs against the rendered frame, and
    // `cell_center` needs published bounds), and long enough for
    // `stty raw -echo; exec tee` to be live before the first write.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Modifiers-changed events route to the focused element; in the app a click
    // focuses the pane, but the hover phase runs before any click.
    let _ = window.update(cx, |_root, win, app| {
        let focus = terminal.read(app).focus_handle_ref().clone();
        win.focus(&focus, app);
    });

    let mut failures: Vec<String> = Vec::new();

    // Print the URL (tee echoes it back, so it lands in the grid) and wait for it.
    if let Err(e) = write_child(cx, &handle, format!("{URL}{TAIL}\r\n").as_bytes()) {
        failures.push(format!("could not print the URL: {e}"));
    }
    let mut url_cell = None;
    for _ in 0..40 {
        url_cell = find_url_cell(cx, &handle);
        if url_cell.is_some() {
            break;
        }
        settle(cx, 50).await;
    }
    let Some((url_row, url_col)) = url_cell else {
        return fail(vec![format!(
            "the printed URL never appeared in the grid; visible: {:?}",
            grid_lines(cx, &handle)
        )]);
    };
    // A cell in the middle of the URL — a link lookup must succeed from any of
    // its cells, and the middle is the least accidental of them.
    let mid_col = url_col + URL.len() / 2;
    let Some(link_pos) = cell_center(cx, &terminal, url_row, mid_col) else {
        return fail(vec![
            "the view published no paint bounds — it never painted, so no synthetic \
             mouse event can be aimed at a cell"
                .to_string(),
        ]);
    };
    let Some(off_pos) = cell_center(cx, &terminal, url_row, OFF_LINK_COL) else {
        return fail(vec!["no paint bounds for the off-link cell".to_string()]);
    };

    // --- Phase 1: the ⌘-hover follows the pointer AND the ⌘ key ---------------
    {
        mouse_move(cx, window, link_pos, Modifiers::command());
        settle(cx, 80).await;
        expect_hover(
            &mut failures,
            "hover-cmd-move-on-url",
            Some(URL),
            hovered(cx, &terminal),
        );

        // ⌘ released with the pointer parked on the URL: the underline must go.
        set_modifiers(cx, window, Modifiers::none());
        settle(cx, 80).await;
        expect_hover(
            &mut failures,
            "hover-cmd-release",
            None,
            hovered(cx, &terminal),
        );

        // ⌘ pressed again without moving the pointer: it must come back.
        set_modifiers(cx, window, Modifiers::command());
        settle(cx, 80).await;
        expect_hover(
            &mut failures,
            "hover-cmd-press-parked",
            Some(URL),
            hovered(cx, &terminal),
        );

        // ⌘ still held, pointer moved off the link: gone again.
        mouse_move(cx, window, off_pos, Modifiers::command());
        settle(cx, 80).await;
        expect_hover(
            &mut failures,
            "hover-cmd-move-off-url",
            None,
            hovered(cx, &terminal),
        );
    }

    // --- Phase 2: ⌘+click on the URL opens it, silently, without selecting ----
    {
        let start = cap_len(&cap_path);
        mouse_move(cx, window, link_pos, Modifiers::command());
        left_click(cx, window, link_pos, Modifiers::command());
        settle(cx, 250).await;

        expect_opened(&mut failures, "cmd-click-on-url", &opened, &[URL]);
        expect_bytes(
            &mut failures,
            "cmd-click-on-url-pty",
            b"",
            &cap_since(&cap_path, start),
        );
        if let Some(text) = selection_text(cx, &handle).filter(|t| !t.is_empty()) {
            failures.push(format!(
                "cmd-click-on-url: the click created a selection ({text:?}); a ⌘+click must \
                 never select"
            ));
        }
    }

    // --- Enable app mouse reporting (SGR), proven by a plain click reporting ---
    // The reported bug's real setting: a URL printed by a program that grabbed
    // the mouse (Claude Code, vim). The echoed DECSET reaches the parser via tee.
    if let Err(e) = write_child(cx, &handle, b"\x1b[?1000h\x1b[?1006h") {
        failures.push(format!("could not enable mouse reporting: {e}"));
    }
    let mut reporting = false;
    for _ in 0..20 {
        let start = cap_len(&cap_path);
        mouse_move(cx, window, off_pos, Modifiers::none());
        left_click(cx, window, off_pos, Modifiers::none());
        settle(cx, 100).await;
        if !cap_since(&cap_path, start).is_empty() {
            reporting = true;
            break;
        }
    }
    if !reporting {
        failures.push(
            "mouse reporting never engaged (a plain click reported nothing after ESC[?1000h \
             ESC[?1006h), so the ⌘-bypass cases below cannot be judged"
                .to_string(),
        );
    }
    // The plain click above selected/reported at the off-link cell; start clean.
    handle.update(cx, |h, _| h.clear_selection());
    settle(cx, 150).await;

    // Re-locate: the echoed reports are themselves grid input.
    let Some((url_row, url_col)) = find_url_cell(cx, &handle) else {
        return fail_with(
            failures,
            format!(
                "the URL left the grid once reporting was on; visible: {:?}",
                grid_lines(cx, &handle)
            ),
        );
    };
    let mid_col = url_col + URL.len() / 2;
    let (Some(link_pos), Some(off_pos)) = (
        cell_center(cx, &terminal, url_row, mid_col),
        cell_center(cx, &terminal, url_row, OFF_LINK_COL),
    ) else {
        return fail_with(failures, "the view stopped publishing paint bounds".to_string());
    };

    // --- Phase 3: with reporting ON, ⌘+click on the URL still opens, silently --
    {
        let start = cap_len(&cap_path);
        mouse_move(cx, window, link_pos, Modifiers::command());
        left_click(cx, window, link_pos, Modifiers::command());
        settle(cx, 250).await;

        expect_opened(
            &mut failures,
            "cmd-click-on-url-reporting",
            &opened,
            &[URL, URL],
        );
        expect_bytes(
            &mut failures,
            "cmd-click-on-url-reporting-pty",
            b"",
            &cap_since(&cap_path, start),
        );
    }

    // --- Phase 4: ⌘+click OFF the link opens nothing and reports normally ------
    {
        let start = cap_len(&cap_path);
        mouse_move(cx, window, off_pos, Modifiers::command());
        left_click(cx, window, off_pos, Modifiers::command());
        settle(cx, 250).await;

        // Unchanged from phase 3 — no third open.
        expect_opened(
            &mut failures,
            "cmd-click-off-link",
            &opened,
            &[URL, URL],
        );
        // ⌘ is not part of the SGR modifier set, so this is the ordinary
        // left press/release pair at that cell.
        let want = format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            OFF_LINK_COL + 1,
            url_row + 1,
            OFF_LINK_COL + 1,
            url_row + 1
        );
        expect_bytes(
            &mut failures,
            "cmd-click-off-link-pty",
            want.as_bytes(),
            &cap_since(&cap_path, start),
        );
    }

    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "terminal links OK: ⌘-hover on {URL} set/cleared with the pointer AND the ⌘ key; \
                 ⌘+click opened exactly that URL with no selection and zero pty bytes — including \
                 with app mouse reporting ON (⌘ bypasses the report); a ⌘+click off the link \
                 opened nothing and sent the ordinary SGR press/release pair."
            ),
        }
    } else {
        fail(failures)
    }
}

/// Assert the recording opener's full log (order included) — the opener is the
/// only place "the click opened something" is observable.
fn expect_opened(
    failures: &mut Vec<String>,
    label: &str,
    opened: &Arc<Mutex<Vec<String>>>,
    want: &[&str],
) {
    let got = opened.lock().expect("opener log").clone();
    if got != want {
        failures.push(format!(
            "{label}: opened URLs were {got:?}, expected {want:?}"
        ));
    }
}

fn fail(failures: Vec<String>) -> CadenceReport {
    CadenceReport {
        passed: false,
        stats: IntervalStats::default(),
        detail: format!(
            "{} niceties-link assertion(s) failed:\n  {}",
            failures.len(),
            failures.join("\n  ")
        ),
    }
}

/// A fatal setup failure that aborts the run, reported alongside whatever had
/// already failed.
fn fail_with(mut failures: Vec<String>, last: String) -> CadenceReport {
    failures.push(last);
    fail(failures)
}
