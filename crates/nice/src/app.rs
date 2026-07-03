//! App module — owns window creation and the root view.
//!
//! Two entry points share one window layout:
//!   * [`run`] — the normal app: one static "Nice RS Dev" window painting a
//!     solid background + the version line.
//!   * [`run_selftest`] — the `NICE_RS_SELFTEST` harness path: the same window,
//!     but the root view animates (stamps a frame + repaints every tick) so the
//!     harness can measure frame cadence. Scenario orchestration, the cadence
//!     gate, capture, and the watchdog all live in `nice_harness::selftest`;
//!     this module only supplies the concrete gpui view + window.
//!
//! Later cycles slot richer chrome into `RootView` (real title bar is R9) and
//! register more scenarios in [`selftest_scenarios`].

use anyhow::Result;
use gpui::{
    div, point, prelude::*, px, rgb, size, AnyWindowHandle, App, AppContext, AsyncApp, Bounds,
    Context, IntoElement, Render, TitlebarOptions, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};

use nice_harness::selftest::Scenario;

/// The application's root view: a solid background with one line of text (the
/// version string). In self-test mode it also drives a continuous animated
/// repaint and stamps each frame for the cadence gate.
struct RootView {
    /// When true, stamp a frame + request the next animation frame on every
    /// render (the self-test measurement loop). When false (the shipped app),
    /// paint once and stay static.
    animated: bool,
    frame: u64,
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Self-test mode: bracket the frame with an os_signpost interval, stamp
        // the frame clock, and keep the composite running via RAF. The interval
        // covers element construction (paint happens later in the pipeline);
        // later cycles wanting present-complete intervals hook the renderer.
        let signpost = if self.animated {
            let id = nice_harness::signpost::frame_begin();
            nice_harness::frame::stamp();
            self.frame += 1;
            window.request_animation_frame();
            Some(id)
        } else {
            None
        };

        // A moving accent bar so each animated frame genuinely differs (real
        // per-frame compositing work, and a non-uniform screenshot capture).
        let accent_x = 40.0 + ((self.frame % 200) as f64) * 1.5;
        let version = concat!("Nice RS Dev v", env!("CARGO_PKG_VERSION"));

        let element = div()
            .size_full()
            .bg(rgb(0x11141b))
            .text_color(rgb(0xe6e9ef))
            .font_family("Helvetica")
            .child(
                div()
                    .absolute()
                    .top(px(80.0))
                    .left(px(accent_x as f32))
                    .w(px(120.0))
                    .h(px(6.0))
                    .rounded(px(3.0))
                    .bg(rgb(0x6e59f5)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(140.0))
                    .left(px(40.0))
                    .text_xl()
                    .child(version),
            );

        if let Some(id) = signpost {
            nice_harness::signpost::frame_end(id);
        }
        element
    }
}

/// Fixed, sensible default window geometry + chrome defaults (real chrome is
/// R9). Shared by the shipped window and every self-test scenario window.
fn window_options() -> WindowOptions {
    let bounds = Bounds {
        origin: point(px(160.0), px(160.0)),
        size: size(px(960.0), px(640.0)),
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_background: WindowBackgroundAppearance::Opaque,
        titlebar: Some(TitlebarOptions {
            title: Some("Nice RS Dev".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        kind: WindowKind::Normal,
        is_resizable: true,
        focus: true,
        show: true,
        ..Default::default()
    }
}

/// Run the shipped application: one static window, quit on window close.
pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.activate(true);
        cx.on_window_closed(|cx, _id| cx.quit()).detach();
        if let Err(e) = cx
            .open_window(window_options(), |_window, cx| {
                cx.new(|_cx| RootView {
                    animated: false,
                    frame: 0,
                })
            })
        {
            eprintln!("nice-rs: failed to open window: {e:#}");
            std::process::exit(1);
        }
    });
}

/// Open the self-test scenario window (animated root view). Handed to the
/// harness as a [`Scenario`] opener.
fn open_selftest_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let handle = cx.open_window(window_options(), |_window, cx| {
        cx.new(|_cx| RootView {
            animated: true,
            frame: 0,
        })
    })?;
    Ok(handle.into())
}

/// The scenario registry the harness iterates. Later cycles push more
/// [`Scenario`]s here (terminal streaming, input latency, …); the `smoke`
/// scenario is the minimal "the window opens and paints at a sane cadence" gate.
pub fn selftest_scenarios() -> Vec<Scenario> {
    vec![Scenario {
        name: "smoke",
        open: open_selftest_window,
    }]
}

/// Run the `NICE_RS_SELFTEST` harness path inside one `Application::run`.
pub fn run_selftest(selector: String) {
    let scenarios = selftest_scenarios();
    gpui_platform::application().run(move |cx: &mut App| {
        nice_harness::selftest::drive(cx, &selector, scenarios);
    });
}
