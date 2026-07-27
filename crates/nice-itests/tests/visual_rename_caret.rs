//! Bug C regression gate — **execution model: real-MacPlatform
//! [`gpui::VisualTestAppContext`], run in a `harness = false` main-thread
//! binary, serially** (see `docs/testing.md` rule 3: it reads back a rendered
//! pixel, needs no real OS event and no cadence claim).
//!
//! ## What this pins, and why the old live leg could not
//!
//! The inline-rename field's click hit-test used to shape the text with the
//! WINDOW BASE text style inside a mouse handler, while the field painted the
//! text with whatever style the element tree resolved at that spot (the toolbar
//! pill and the sidebar tab both set the terminal font family on an ancestor)
//! and split it across three sibling divs. Two measurements, so the caret drew
//! further and further right of the click the further right you clicked.
//!
//! The `file-browser` live leg (d′) never caught it: it synthesizes the click x
//! from `char_boundary_x`, i.e. from the same `shape_line` math the hit-test
//! uses, so hit-test and expectation moved together while the *rendered* caret
//! was visibly wrong. This test judges the RENDERED PIXELS instead: it clicks at
//! a chosen window x near the right end of a long mixed-width name, captures the
//! frame, finds the caret bar's pixel column by its (unique, full-alpha) colour,
//! and asserts the bar sits within one average glyph advance of the x that was
//! clicked. Nothing in the assertion path reuses the hit-test's own arithmetic.
//!
//! Two cases mirror the two style configurations the three call sites are drawn
//! from:
//!
//! * `ancestor-mono-family` — an ancestor sets the terminal font family, as the
//!   toolbar pill (`toolbar.rs`) and the sidebar tab (`sidebar_shell.rs`) do.
//! * `window-base-family` — nothing overrides the family, as the file-browser
//!   row (`file_browser/view.rs`) does.
//!
//! ## Why the module is `#[path]`-included, not imported
//!
//! `rename_field` lives in the `nice` BINARY crate, which a dev/test crate
//! cannot depend on (the same constraint `chrome_band` / `sidebar_multiselect`
//! call out). Mirroring the element here would gate a copy, which for this bug
//! is worthless — the whole claim is that the SHIPPED element's paint and
//! hit-test agree. So this binary compiles the real `inline_rename.rs` source
//! file directly. It is dependency-clean (gpui + `nice-model` + `nice-theme`
//! only), and `cfg(test)` is off in a `harness = false` target, so the module's
//! own unit tests stay where they belong (`cargo test -p nice`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use gpui::{
    div, point, prelude::*, px, rgb, size, AnyWindowHandle, Context, FocusHandle, KeyDownEvent,
    Modifiers, MouseButton, MouseDownEvent, SharedString, VisualTestAppContext, Window,
};

use nice_model::file_browser::TextFieldEditor;

#[allow(dead_code)]
#[path = "../../nice/src/inline_rename.rs"]
mod inline_rename;

use inline_rename::{
    apply_rename_click, field_probe_cell, field_text, rename_field, FieldColors, FieldProbeCell,
};

/// A long name whose glyph advances differ a lot (wide `W`, narrow `i`, wide
/// `m`, digits) so any per-glyph measurement error accumulates visibly by the
/// right end — the shape of the reported drift.
const TEXT: &str = "WWWWiiiimmmm-1234567890.txt";
const TEXT_SIZE: f32 = 14.0;
/// The family an ancestor sets in the pill / tab configuration. Menlo ships with
/// macOS and is the same family the terminal exemplar uses.
const MONO_FAMILY: &str = "Menlo";

const WIN_W: f32 = 720.0;
const WIN_H: f32 = 80.0;
/// Horizontal inset of the field inside the window, so `text_left` is nowhere
/// near 0 and a stale/zero probe cannot accidentally pass.
const FIELD_INSET_X: f32 = 24.0;

/// Caret fill: full-alpha pure red; selection fill: full-alpha pure green. Two
/// colours nothing else in the frame paints (background, border, and glyphs are
/// greys/blues), so a colour match inside the field is unambiguously that
/// overlay.
const CARET_HEX: u32 = 0xff0000;
const CARET_RGB: (u8, u8, u8) = (255, 0, 0);
const SELECTION_HEX: u32 = 0x00ff00;
const SELECTION_RGB: (u8, u8, u8) = (0, 255, 0);
/// Per-channel tolerance for "this pixel is that overlay" — both are opaque
/// quads, so only sRGB round-tripping moves them.
const OVERLAY_TOL: u8 = 12;

/// How far the painted caret may sit from the clicked x. The plan's floor is
/// "within one glyph advance" (~8.4px here); this is deliberately much tighter,
/// because the whole point is that paint and hit-test now share ONE measurement
/// — the residue is the pixel scan's 1/scale quantization plus sRGB rounding.
/// Measured after the fix: +0.14px and -0.09px. A ±4px perturbation of the
/// caret overlay must fail this test, and does.
const MAX_DRIFT_PX: f32 = 2.0;

/// Bounded readiness wait for the first painted frame (real Metal + a forced
/// repaint), polled — never a bare sleep, and never a timing assertion.
const RENDER_TIMEOUT: Duration = Duration::from_secs(8);

/// One captured frame. Kept as raw RGBA + dimensions so this binary needs no
/// dependency on the `image` crate just to name `capture_screenshot`'s return.
struct Frame {
    raw: Vec<u8>,
    width: u32,
    height: u32,
}

/// The root: one rename field, laid out like a call site's title slot.
struct FieldRoot {
    focus: FocusHandle,
    editor: TextFieldEditor,
    probe: FieldProbeCell,
    family: Option<SharedString>,
    /// Where the production hit-test put the caret, published for the report
    /// (the assertion itself reads pixels, not this).
    landed: Rc<Cell<usize>>,
    /// The editor's selection after the last click — what the selection-overlay
    /// case expects to see highlighted.
    selection: Rc<Cell<(usize, usize)>>,
}

impl Render for FieldRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = FieldColors {
            bg: rgb(0x101820),
            border: rgb(0x30404f),
            text: rgb(0xe8eef5),
            caret: rgb(CARET_HEX),
            selection: rgb(SELECTION_HEX),
        };
        let text = field_text(&self.editor);
        let weak = cx.weak_entity();
        let weak_drag = cx.weak_entity();
        let landed = self.landed.clone();
        let selection = self.selection.clone();
        let drag_selection = self.selection.clone();
        div()
            .size_full()
            .bg(rgb(0x000000))
            .flex()
            .flex_row()
            .items_center()
            .px(px(FIELD_INSET_X))
            .text_size(px(TEXT_SIZE))
            .when_some(self.family.clone(), |d, fam| d.font_family(fam))
            .child(rename_field(
                &self.focus,
                &text,
                "TestRename",
                colors,
                TEXT_SIZE,
                self.probe.clone(),
                |_e: &KeyDownEvent, _window, _app| {},
                move |index, click_count, _window, app| {
                    landed.set(index);
                    let _ = weak.update(app, |this, cx| {
                        apply_rename_click(&mut this.editor, index, click_count);
                        selection.set(this.editor.selection());
                        cx.notify();
                    });
                },
                move |index, _window, app| {
                    let _ = weak_drag.update(app, |this, cx| {
                        this.editor.extend_to(index);
                        drag_selection.set(this.editor.selection());
                        cx.notify();
                    });
                },
            ))
    }
}

fn main() {
    let cases: [(&str, Option<&str>); 2] = [
        ("ancestor-mono-family", Some(MONO_FAMILY)),
        ("window-base-family", None),
    ];
    let mut cx = VisualTestAppContext::new(gpui_platform::current_platform(false));
    let mut failed = false;
    for (label, family) in cases {
        match run_case(&mut cx, label, family) {
            Ok(report) => println!("VISUAL PASS visual_rename_caret[{label}]: {report}"),
            Err(e) => {
                eprintln!("VISUAL FAIL visual_rename_caret[{label}]: {e:#}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

/// Mount the field, then judge two painted overlays against the geometry they
/// claim: the caret bar after a click near the right end, and the selection
/// highlight after a double-click mid-name.
fn run_case(cx: &mut VisualTestAppContext, label: &str, family: Option<&str>) -> Result<String> {
    let probe = field_probe_cell();
    let landed = Rc::new(Cell::new(usize::MAX));
    let selection = Rc::new(Cell::new((0, 0)));
    let window = {
        let probe = probe.clone();
        let landed = landed.clone();
        let selection = selection.clone();
        let family = family.map(SharedString::from);
        cx.open_offscreen_window(size(px(WIN_W), px(WIN_H)), move |_window, app| {
            app.new(|c: &mut Context<FieldRoot>| FieldRoot {
                focus: c.focus_handle(),
                editor: TextFieldEditor::new(TEXT),
                probe,
                family,
                landed,
                selection,
            })
        })?
    };
    let handle: AnyWindowHandle = window.into();

    // First paint: the probes and the paint-time boundary table are written by
    // the field's own paint pass, so nothing below may read them earlier.
    render_until(cx, handle, CARET_RGB)?;
    let scale = cx.update_window(handle, |_, w, _| w.scale_factor())?;

    let chars = TEXT.chars().count();
    let boundaries = probe.borrow().boundaries.clone();
    if boundaries.len() != chars + 1 {
        bail!(
            "the field's paint recorded {} boundaries for a {chars}-char name",
            boundaries.len()
        );
    }
    let text_left = probe.borrow().text_left;
    if text_left <= 0.0 {
        bail!("layout probe never recorded a text_left (got {text_left})");
    }
    let line_width = boundaries[chars];
    let avg_advance = line_width / chars as f32;

    // (1) Caret: click at a boundary near the RIGHT end, where a per-glyph
    //     measurement error has had the whole string to accumulate.
    let index = chars - 4;
    let click_x = text_left + boundaries[index];
    let click_y = WIN_H / 2.0;
    cx.simulate_mouse_down(
        handle,
        point(px(click_x), px(click_y)),
        MouseButton::Left,
        Modifiers::default(),
    );
    let landed = landed.get();
    let image = render_until(cx, handle, CARET_RGB)?;

    // Judge the frame, not the arithmetic.
    let (caret_left, _) = color_span_px(&image, scale, text_left, line_width, CARET_RGB)?;
    let drift = caret_left - click_x;
    let caret_report = format!(
        "caret: clicked x={click_x:.2} (boundary {index}), hit-test landed at index {landed}, \
         painted at x={caret_left:.2}, drift={drift:+.2}px"
    );
    if drift.abs() > MAX_DRIFT_PX {
        bail!("caret drifted more than {MAX_DRIFT_PX}px from the click: {caret_report}");
    }

    // (2) Selection highlight: a double-click mid-name selects a word, and the
    //     highlight is drawn from the same boundary table. Both of its edges must
    //     land on the glyph boundaries the model selected — the "offset
    //     highlight" the three-div split used to be able to produce.
    let word_x = text_left + boundaries[chars / 2];
    cx.simulate_event(
        handle,
        MouseDownEvent {
            position: point(px(word_x), px(click_y)),
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        },
    );
    let (sel_start, sel_end) = selection.get();
    if sel_start == sel_end {
        bail!("the double-click selected nothing (selection {sel_start}..{sel_end})");
    }
    let image = render_until(cx, handle, SELECTION_RGB)?;
    let (paint_left, paint_right) =
        color_span_px(&image, scale, text_left, line_width, SELECTION_RGB)?;
    let want_left = text_left + boundaries[sel_start];
    let want_right = text_left + boundaries[sel_end];
    let sel_report = format!(
        "selection {sel_start}..{sel_end}: expected x∈[{want_left:.2},{want_right:.2}], \
         painted x∈[{paint_left:.2},{paint_right:.2}]"
    );
    if (paint_left - want_left).abs() > MAX_DRIFT_PX
        || (paint_right - want_right).abs() > MAX_DRIFT_PX
    {
        bail!("the selection highlight is offset from the selected glyphs: {sel_report}");
    }
    // …and the glyphs are still painted ON TOP of it: a band that is uniformly
    // the highlight colour means the text vanished behind the overlay.
    let glyph_pixels = non_matching_pixels_in_band(&image, scale, want_left, want_right, SELECTION_RGB);
    if glyph_pixels < 20 {
        bail!(
            "only {glyph_pixels} non-highlight pixels inside the selection band — \
             the selected text is not painted over the highlight"
        );
    }

    Ok(format!(
        "[{label}] {caret_report}; {sel_report}; {glyph_pixels} glyph px over the highlight \
         (avg glyph advance {avg_advance:.2}px, scale {scale})"
    ))
}

/// Force a repaint and wait (bounded, polled) for a frame that actually contains
/// `want` — the field only paints an overlay once the window has drawn.
fn render_until(
    cx: &mut VisualTestAppContext,
    window: AnyWindowHandle,
    want: (u8, u8, u8),
) -> Result<Frame> {
    let deadline = Instant::now() + RENDER_TIMEOUT;
    loop {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, w, _| w.refresh());
        cx.run_until_parked();
        if let Ok(image) = cx.capture_screenshot(window) {
            if image
                .as_raw()
                .chunks_exact(4)
                .any(|p| is_color([p[0], p[1], p[2], p[3]], want))
            {
                return Ok(Frame {
                    width: image.width(),
                    height: image.height(),
                    raw: image.into_raw(),
                });
            }
        }
        if Instant::now() >= deadline {
            bail!("no frame containing the {want:?} overlay was rendered within {RENDER_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn is_color(p: [u8; 4], want: (u8, u8, u8)) -> bool {
    p[3] > 200
        && p[0].abs_diff(want.0) <= OVERLAY_TOL
        && p[1].abs_diff(want.1) <= OVERLAY_TOL
        && p[2].abs_diff(want.2) <= OVERLAY_TOL
}

/// The `[leftmost, rightmost)` extent of `want`-coloured pixels inside the text
/// run's horizontal span, in LOGICAL px (window coordinates).
fn color_span_px(
    image: &Frame,
    scale: f32,
    text_left: f32,
    line_width: f32,
    want: (u8, u8, u8),
) -> Result<(f32, f32)> {
    // Search the whole text run plus a generous margin either side, so an overlay
    // that drifted off the end of the text is still found and REPORTED (rather
    // than silently "not found").
    let (width, height) = (image.width, image.height);
    let raw = &image.raw;
    let x0 = ((text_left - 40.0) * scale).floor().max(0.0) as u32;
    let x1 = (((text_left + line_width + 80.0) * scale).ceil() as u32).min(width);
    let mut lo: Option<u32> = None;
    let mut hi: Option<u32> = None;
    for y in 0..height {
        for x in x0..x1 {
            let i = ((y * width + x) * 4) as usize;
            if i + 3 >= raw.len() {
                continue;
            }
            if is_color([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]], want) {
                lo = Some(lo.map_or(x, |v| v.min(x)));
                hi = Some(hi.map_or(x, |v| v.max(x)));
            }
        }
    }
    match (lo, hi) {
        (Some(lo), Some(hi)) => Ok((lo as f32 / scale, (hi + 1) as f32 / scale)),
        _ => Err(anyhow!(
            "no {want:?} pixel found in x∈[{x0},{x1}) of the {width}×{height} frame — \
             that overlay was not painted, or not where the field says it is"
        )),
    }
}

/// How many pixels inside the highlight band are NOT the highlight colour — the
/// glyphs (and their antialiasing) drawn over it.
///
/// Counting is confined to the highlight's OWN rows (the min..=max device y that
/// actually contains a `want`-coloured pixel in the band). The field paints its
/// box background and border above and below the one-line-height highlight quad,
/// and those chrome pixels clear any brightness filter — scanning every row would
/// count them and let the "text vanished behind the overlay" assertion pass with
/// zero glyphs painted.
fn non_matching_pixels_in_band(
    image: &Frame,
    scale: f32,
    left: f32,
    right: f32,
    want: (u8, u8, u8),
) -> usize {
    let (width, height) = (image.width, image.height);
    let raw = &image.raw;
    let x0 = (left * scale).ceil() as u32 + 1;
    let x1 = ((right * scale).floor() as u32).min(width).saturating_sub(1);
    // Pass 1: the highlight's vertical extent within the band.
    let mut y_lo: Option<u32> = None;
    let mut y_hi: Option<u32> = None;
    for y in 0..height {
        for x in x0..x1 {
            let i = ((y * width + x) * 4) as usize;
            if i + 3 >= raw.len() {
                continue;
            }
            if is_color([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]], want) {
                y_lo = Some(y_lo.map_or(y, |v| v.min(y)));
                y_hi = Some(y_hi.map_or(y, |v| v.max(y)));
            }
        }
    }
    let (Some(y_lo), Some(y_hi)) = (y_lo, y_hi) else {
        return 0;
    };
    // Pass 2: non-highlight pixels on those rows only. The dark-pixel filter
    // still excludes the window background that shows through antialiased
    // highlight edges on the extreme rows.
    let mut n = 0usize;
    for y in y_lo..=y_hi {
        for x in x0..x1 {
            let i = ((y * width + x) * 4) as usize;
            if i + 3 >= raw.len() {
                continue;
            }
            let p = [raw[i], raw[i + 1], raw[i + 2], raw[i + 3]];
            if !is_color(p, want) && (p[0] > 24 || p[1] > 24 || p[2] > 24) {
                n += 1;
            }
        }
    }
    n
}
