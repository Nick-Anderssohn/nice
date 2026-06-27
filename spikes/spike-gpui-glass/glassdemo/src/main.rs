// SPIKE: GPUI liquid-glass / macOS vibrancy probe.
//
// Goal: window + true macOS vibrancy (NSVisualEffectView-equivalent) behind
// content, rounded corners, a translucent styled panel, native traffic lights.
//
// Mechanism discovered by reading gpui-0.2.2 source
// (src/platform/mac/window.rs::set_background_appearance):
//   - WindowBackgroundAppearance::Blurred makes gpui insert an
//     NSVisualEffectView subclass ("BlurredView", material = .Selection,
//     state = .Active) BELOW the content view on macOS >= Monterey, or fall
//     back to the private CGSSetWindowBackgroundBlurRadius on older systems.
//   - So genuine system vibrancy is BUILT IN; no custom objc/Metal needed for
//     the basic blur. (The material is hardcoded to .Selection though.)

use gpui::{
    div, prelude::*, px, rgb, rgba, size, App, Application, Bounds, Context, Point,
    TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

struct GlassDemo;

impl Render for GlassDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Root is left transparent so the window's NSVisualEffectView vibrancy
        // shows through everywhere there is no opaque content.
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_6()
            // A translucent "glass" panel: rounded corners + semi-transparent
            // fill layered on top of the system vibrancy + hairline border.
            .child(
                div()
                    .w(px(440.0))
                    .h(px(280.0))
                    .rounded(px(22.0))
                    // 0xRRGGBBAA: white at ~18% alpha -> tints the vibrancy.
                    .bg(rgba(0xFFFFFF2E))
                    .border_1()
                    .border_color(rgba(0xFFFFFF66))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(rgb(0xFFFFFF))
                    .text_2xl()
                    .child("Liquid glass panel")
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgba(0xFFFFFFCC))
                            .child("NSVisualEffectView vibrancy via gpui Blurred"),
                    ),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(760.0), px(520.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // *** the key line: real macOS vibrancy behind the window ***
                window_background: WindowBackgroundAppearance::Blurred,
                // Native traffic lights, but transparent titlebar so the
                // vibrancy reaches the top edge. Custom traffic-light position.
                titlebar: Some(TitlebarOptions {
                    title: Some("GPUI Glass".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(Point {
                        x: px(14.0),
                        y: px(14.0),
                    }),
                }),
                kind: WindowKind::Normal,
                is_resizable: true,
                ..Default::default()
            },
            |_window, cx| cx.new(|_cx| GlassDemo),
        )
        .unwrap();
    });
}
