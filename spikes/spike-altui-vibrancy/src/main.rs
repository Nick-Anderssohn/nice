//! SPIKE: non-GPUI Rust UI path for macOS vibrancy / liquid glass.
//!
//! Path: raw winit (window + run loop) + objc2 (bridge AppKit directly).
//! We create a winit window, drop down to its underlying NSView/NSWindow via
//! raw-window-handle, and attach a real NSVisualEffectView (classic vibrancy)
//! plus attempt macOS 26's new NSGlassEffectView ("liquid glass") — exactly the
//! technique Tauri/Electron use. This is the most direct route to TRUE native
//! vibrancy from a non-GPUI Rust stack.

use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindow, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

// NSView autoresizing flags (avoid guessing objc2 enum spelling).
const NS_VIEW_WIDTH_SIZABLE: u64 = 2;
const NS_VIEW_HEIGHT_SIZABLE: u64 = 16;

#[derive(Default)]
struct App {
    window: Option<Window>,
    // Keep AppKit views alive for the window's lifetime.
    _effect: Option<Retained<NSVisualEffectView>>,
    _glass: Option<Retained<NSView>>,
    start: Option<Instant>,
    glass_present: bool,
    effect_attached: bool,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Rust altui vibrancy spike")
            .with_inner_size(LogicalSize::new(760.0, 520.0))
            .with_transparent(true);
        let window = event_loop
            .create_window(attrs)
            .expect("failed to create winit window");

        self.attach_native_effects(&window);

        self.start = Some(Instant::now());
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Auto-exit so the spike never lingers if no one captures/kills it.
        if let Some(start) = self.start {
            if start.elapsed() >= Duration::from_secs(30) {
                event_loop.exit();
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(400),
        ));
    }
}

impl App {
    fn attach_native_effects(&mut self, window: &Window) {
        // 1. Reach the underlying NSView via raw-window-handle.
        let handle = window.window_handle().expect("no window handle");
        let ns_view_ptr = match handle.as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr(),
            other => {
                eprintln!("unexpected window handle (not AppKit): {other:?}");
                return;
            }
        };

        let mtm = MainThreadMarker::new().expect("attach must run on main thread");

        // SAFETY: winit guarantees this is a live NSView for the window's lifetime.
        let view: &NSView = unsafe { &*(ns_view_ptr.cast::<NSView>()) };
        let ns_window: Retained<NSWindow> = view.window().expect("NSView has no NSWindow");

        let frame: NSRect = view.bounds();

        // 2. Make the NSWindow itself non-opaque + clear so vibrancy shows through,
        //    with a transparent full-size-content titlebar (liquid-glass framing).
        unsafe {
            ns_window.setOpaque(false);
            let clear = NSColor::clearColor();
            ns_window.setBackgroundColor(Some(&clear));
            ns_window.setTitlebarAppearsTransparent(true);
            ns_window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
            let mask = ns_window.styleMask();
            ns_window.setStyleMask(mask | NSWindowStyleMask::FullSizeContentView);
            ns_window.setHasShadow(true);
        }

        // 3. Attach a REAL NSVisualEffectView (classic vibrancy / translucency).
        let effect: Retained<NSVisualEffectView> =
            unsafe { NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame) };
        unsafe {
            effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
            effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            effect.setState(NSVisualEffectState::Active);
            effect.setWantsLayer(true);
            let _: () = msg_send![&*effect, setAutoresizingMask: NS_VIEW_WIDTH_SIZABLE | NS_VIEW_HEIGHT_SIZABLE];

            // Rounded corners via the backing CALayer (msg_send avoids a QuartzCore dep).
            let layer: *mut AnyObject = msg_send![&*effect, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: 18.0f64];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }

            // Insert below any winit content so the blur is the background.
            view.addSubview(&effect);
            let _: () = msg_send![&*effect, setFrameOrigin: NSPoint::new(0.0, 0.0)];
        }
        self.effect_attached = true;
        eprintln!("EFFECT_ATTACHED=1 (NSVisualEffectView material=HUDWindow, cornerRadius=18)");

        // 4. Attempt macOS 26 "Liquid Glass": NSGlassEffectView (runtime lookup so
        //    we don't depend on the objc2 binding existing for this brand-new class).
        match AnyClass::get(c"NSGlassEffectView") {
            Some(cls) => {
                self.glass_present = true;
                let inset = NSRect::new(
                    NSPoint::new(60.0, 60.0),
                    NSSize::new(
                        (frame.size.width - 120.0).max(80.0),
                        (frame.size.height - 120.0).max(80.0),
                    ),
                );
                unsafe {
                    let alloc: *mut AnyObject = msg_send![cls, alloc];
                    let glass_raw: *mut AnyObject = msg_send![alloc, initWithFrame: inset];
                    if !glass_raw.is_null() {
                        let _: () = msg_send![glass_raw, setCornerRadius: 28.0f64];
                        let _: () = msg_send![glass_raw, setWantsLayer: true];
                        let glass_view: &NSView = &*(glass_raw.cast::<NSView>());
                        view.addSubview(glass_view);
                        // Take ownership of the +1 from alloc/init.
                        self._glass = Retained::from_raw(glass_raw.cast::<NSView>());
                    }
                }
                eprintln!(
                    "GLASS_PRESENT=1 (NSGlassEffectView found; instantiated + cornerRadius=28)"
                );
            }
            None => {
                self.glass_present = false;
                eprintln!("GLASS_PRESENT=0 (NSGlassEffectView not in this AppKit runtime)");
            }
        }

        let win_num: isize = unsafe { ns_window.windowNumber() };
        eprintln!("WINDOW_NUMBER={win_num}");

        self._effect = Some(effect);
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("failed to build event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("event loop error");
    println!(
        "EXIT effect_attached={} glass_present={}",
        app.effect_attached, app.glass_present
    );
}
