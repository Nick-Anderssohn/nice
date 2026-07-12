//! The Settings window shell (R23 What-to-build item 1): the [`OpenSettings`]
//! action, its ⌘, accelerator, the fresh [`settings_window_options`], and the
//! singleton [`SettingsWindow`] Global.
//!
//! GPUI has no `Settings` scene, so R23 mints all of it. The "Settings…" app-menu
//! item lives in [`crate::app::app_menus`] (it dispatches this same action); the
//! ⌘, binding + the action handler + the close observer are installed by
//! [`install_open_settings_command`] from `app::run` (and, in the suite, from the
//! `settings-window` scenario).
//!
//! ## Singleton semantics (D7)
//! [`open_or_focus_settings`] focuses the stored window when the handle is still
//! live, else opens a fresh window and stores its handle. The window is opened via
//! a bare [`App::open_window`] and is **never** registered in the
//! [`WindowRegistry`](crate::window_registry::WindowRegistry) — so shortcut
//! dispatch ignores it and it has no `WindowState`-shaped teardown. The close
//! observer clears the [`SettingsWindow`] Global so the next ⌘, opens fresh. The
//! non-rebindable ⌘, accelerator follows the ⌘N / ⌘Q / ⌘W idiom
//! (`app.rs:510-551`): a fixed `KeyBinding::new` with a `None` context, so it
//! survives R24's `clear_key_bindings()` rebuild (it is not in the 13-action
//! rebindable table).

use gpui::{
    point, px, size, AnyWindowHandle, App, AppContext, Bounds, Global, KeyBinding, Size,
    TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use crate::settings::root::SettingsRootView;

// The ⌘, action. Like `NewWindow` / `Quit` / `CloseWindow` (`app.rs:417-424`) this
// is a fixed window-management action, NOT one of the 13 rebindable shortcuts
// (`nice_model::shortcuts`), so it lives here rather than in the defaults table.
gpui::actions!(nice, [OpenSettings]);

/// Ideal opening size (Swift `NiceApp.swift:204-210` mirror).
const IDEAL_WIDTH: f32 = 640.0;
const IDEAL_HEIGHT: f32 = 440.0;
/// Minimum size — the rail (160pt) + a legible pane; a resize drag cannot shrink
/// below it.
const MIN_WIDTH: f32 = 560.0;
const MIN_HEIGHT: f32 = 380.0;

/// The singleton handle to the one open Settings window, or `None` when closed.
/// ⌘, focuses the stored handle when it is still live, else opens + stores; the
/// window's close clears it (see [`install_open_settings_command`]).
#[derive(Default)]
struct SettingsWindow(Option<AnyWindowHandle>);

impl Global for SettingsWindow {}

/// One-shot install guard (mirrors [`crate::keymap`]'s `ShortcutsInstalled`): the
/// shipped app installs once, but the self-test suite runs every scenario in ONE
/// process and several install the command; a second install would double-register
/// the action handler + the close observer.
struct OpenSettingsInstalled;

impl Global for OpenSettingsInstalled {}

/// A FRESH [`WindowOptions`] for the Settings window — deliberately NOT
/// [`crate::app::window_options_with`] (the MAIN window's hidden-titlebar band +
/// custom drag + repositioned traffic lights). Settings keeps STANDARD macOS
/// chrome: a real, movable title bar with the native traffic lights, resizable,
/// minimizable, min 560×380 / ideal 640×440.
pub(crate) fn settings_window_options() -> WindowOptions {
    let bounds = Bounds {
        origin: point(px(200.0), px(200.0)),
        size: size(px(IDEAL_WIDTH), px(IDEAL_HEIGHT)),
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Standard chrome: `appears_transparent: false` + no custom traffic-light
        // position ⇒ AppKit draws its own opaque titlebar (the Settings scene look,
        // not the main window's band).
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        kind: WindowKind::Normal,
        // A standard window the user drags by the real titlebar (unlike the main
        // window, which draws its own drag band and sets `is_movable: false`).
        is_movable: true,
        is_resizable: true,
        is_minimizable: true,
        focus: true,
        show: true,
        window_min_size: Some(Size {
            width: px(MIN_WIDTH),
            height: px(MIN_HEIGHT),
        }),
        ..Default::default()
    }
}

/// The current singleton settings-window handle, or `None` when closed. Exposed
/// for the `settings-window` scenario's leg (a) assertions.
pub(crate) fn current_settings_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<SettingsWindow>().and_then(|g| g.0)
}

/// Force the singleton Global to a (possibly dead) handle. Scenario-only support
/// for driving the stale-handle fallthrough of [`open_or_focus_settings`] — the
/// shipped app only ever stores handles through `open_or_focus_settings` itself.
pub(crate) fn force_settings_handle_for_scenario(cx: &mut App, handle: AnyWindowHandle) {
    cx.set_global(SettingsWindow(Some(handle)));
}

/// ⌘, / "Settings…" behaviour: focus the one open Settings window when its handle
/// is still live, else open a fresh window and store its handle (the singleton).
/// The window is opened UNREGISTERED (D7) — never added to the
/// [`WindowRegistry`](crate::window_registry::WindowRegistry).
pub(crate) fn open_or_focus_settings(cx: &mut App) {
    // Focus the existing window when its stored handle is still among the live
    // windows (a stale handle — window already closed — falls through to reopen;
    // the close observer normally clears it first).
    if let Some(handle) = current_settings_window(cx) {
        let still_open = cx
            .windows()
            .iter()
            .any(|w| w.window_id() == handle.window_id());
        if still_open {
            let _ = handle.update(cx, |_root, window, _cx| window.activate_window());
            return;
        }
    }

    // Else open a fresh Settings window (bare `open_window`, no registry) and store
    // its handle as the singleton.
    match cx.open_window(settings_window_options(), |_window, cx| {
        cx.new(|_cx| SettingsRootView::new())
    }) {
        Ok(handle) => cx.set_global(SettingsWindow(Some(handle.into()))),
        Err(e) => eprintln!("nice: failed to open the settings window: {e:#}"),
    }
}

/// Wire the Settings command, once, from [`crate::app::run`] before the first
/// window opens (and, in the suite, from the `settings-window` scenario): the
/// global [`OpenSettings`] handler, its ⌘, key binding, and the close observer
/// that clears the singleton Global. The "Settings…" menu item ([`crate::app::app_menus`])
/// dispatches the same action.
///
/// Idempotent via [`OpenSettingsInstalled`]: the shipped app calls it once, the
/// suite has several scenarios install it in one process.
pub(crate) fn install_open_settings_command(cx: &mut App) {
    if cx.try_global::<OpenSettingsInstalled>().is_some() {
        return;
    }
    cx.set_global(OpenSettingsInstalled);
    cx.set_global(SettingsWindow::default());

    cx.on_action(|_: &OpenSettings, cx: &mut App| open_or_focus_settings(cx));
    // ⌘, — a non-rebindable window-management accelerator (like ⌘N / ⌘Q / ⌘W).
    // `None` context = active in every dispatch context.
    cx.bind_keys([KeyBinding::new("cmd-,", OpenSettings, None)]);

    // Clear the singleton Global when the Settings window closes, so the next ⌘,
    // opens fresh. The Settings window is unregistered (D7), so this is the ONLY
    // close plumbing it has. Coexists with the `WindowRegistry`'s own
    // `on_window_closed` observer (both fire; this one only touches the settings
    // Global, never quits).
    cx.on_window_closed(|cx, id| {
        let is_settings =
            current_settings_window(cx).is_some_and(|h| h.window_id() == id);
        if is_settings {
            cx.set_global(SettingsWindow::default());
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_window_options_use_standard_movable_chrome() {
        let opts = settings_window_options();
        // Standard macOS chrome: a real (opaque) titlebar, user-movable, resizable
        // — the deliberate divergence from the main window's hidden band.
        let titlebar = opts.titlebar.expect("settings window has a titlebar");
        assert!(
            !titlebar.appears_transparent,
            "settings keeps standard opaque chrome, not the hidden band"
        );
        assert!(
            titlebar.traffic_light_position.is_none(),
            "standard chrome ⇒ AppKit places the traffic lights"
        );
        assert_eq!(titlebar.title.as_deref(), Some("Settings"));
        assert!(opts.is_movable, "the real titlebar drags the window");
        assert!(opts.is_resizable);
    }

    #[test]
    fn settings_window_has_the_min_and_ideal_sizes() {
        let opts = settings_window_options();
        let min = opts.window_min_size.expect("settings window has a min size");
        assert_eq!(f32::from(min.width), MIN_WIDTH);
        assert_eq!(f32::from(min.height), MIN_HEIGHT);
        match opts.window_bounds {
            Some(WindowBounds::Windowed(bounds)) => {
                assert_eq!(f32::from(bounds.size.width), IDEAL_WIDTH);
                assert_eq!(f32::from(bounds.size.height), IDEAL_HEIGHT);
            }
            other => panic!("expected windowed ideal bounds, got {other:?}"),
        }
    }
}
