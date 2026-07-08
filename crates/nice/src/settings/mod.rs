//! The R23 preferences window (Milestone 6, minus the CUT Editors pane).
//!
//! GPUI has no `Settings` scene, so R23 mints the window itself: the
//! [`OpenSettings`](window::OpenSettings) action, the ⌘, accelerator, the
//! "Settings…" app-menu item, a fresh [`settings_window_options`](window::settings_window_options)
//! ([`WindowOptions`](gpui::WindowOptions) with STANDARD macOS chrome — a real
//! title bar + traffic lights, NOT Nice's hidden-titlebar band), and a singleton
//! `SettingsWindow` Global (⌘, focuses the one open window if present, else opens
//! + stores; the window's close clears the Global).
//!
//! ## Module layout
//! * [`window`] — the shell: the action + ⌘, binding + menu item, the fresh
//!   window options, the singleton Global (focus-else-open / close-clears), and
//!   the `app::run` bootstrap install.
//! * [`root`] — [`SettingsRootView`](root::SettingsRootView): the 160pt section
//!   rail (from [`settings_rail_sections`](root::settings_rail_sections)) over a
//!   scrollable content area that dispatches per slug through
//!   [`render_section`](root::render_section); the shared
//!   [`setting_title`](root::setting_title) /
//!   [`setting_subtitle`](root::setting_subtitle) /
//!   [`setting_row`](root::setting_row) building blocks; and the
//!   [`shortcuts_pane`](root::shortcuts_pane) placeholder R24 fills.
//! * [`scenario`] — the `settings-window` self-test scenario.
//!
//! ## Unregistered by design (D7)
//! The settings window is NOT registered in the [`WindowRegistry`](crate::window_registry::WindowRegistry):
//! shortcut dispatch ignores it for free and it never hits the `WindowState`-shaped
//! teardown path (it has no such state). Quit-when-empty counts REGISTERED windows
//! only, so closing the last MAIN window while settings is open still quits the app
//! — the documented Swift divergence.
//!
//! The individual pane bodies (Appearance / Font / Claude / Advanced / About) land
//! in later slices of this cycle; slice 1 ships the shell, the rail/dispatch, the
//! shared building blocks, the Shortcuts placeholder seam, and the scenario
//! skeleton (leg a).

pub(crate) mod about_pane;
pub(crate) mod advanced_pane;
pub(crate) mod appearance_pane;
pub(crate) mod claude_pane;
pub(crate) mod file_picker;
pub(crate) mod font_pane;
pub(crate) mod prefs_store;
pub(crate) mod root;
pub(crate) mod scenario;
pub(crate) mod shortcuts_pane;
pub(crate) mod sidebar_font;
pub(crate) mod window;
