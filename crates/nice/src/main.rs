//! nice-rs — the Nice rewrite's GPUI application binary (Path B, all-Rust
//! single Metal stack). Process/binary name `nice-rs`, distinct from the Swift
//! `Nice` / `Nice Dev` builds.
//!
//! Structure (grows over later cycles):
//!   * [`app`] — owns window creation + the root view (shipped window and the
//!     self-test scenario window).
//!   * [`app_shell`] — the R13.5 per-window composition root (`AppShellView`):
//!     mounts the R11 pane strip + R10 floating sidebar card + the pane-content
//!     host (`PaneHostView`, active pane → session handle → per-pane
//!     `TerminalView`) that the shipped window and every ⌘N window use.
//!   * [`platform`] — the single home for foreign AppKit / objc2 / CoreGraphics
//!     access (all-Rust rule): the demand-present kick + present-timing facts
//!     (R1), the keyCode side-channel (R5), and the CGEvent/AX/TIS FFI the live
//!     input scenarios drive (R5).
//!   * [`input_live`] — the R5 live input self-test scenarios (`input-live` /
//!     `input-shell`): real CGEvents posted to our own pid, byte-exact pty
//!     receipt, the item-4 candidate anchor, and the IME go/no-go probe.
//!   * [`chrome_live`] — the R9 live window-chrome self-test scenario
//!     (`chrome`): real mouse CGEvents drive the shipped band + repositioned
//!     traffic lights + full-screen wiring, judged against AppKit frame/state
//!     reads (geometry, drag differential, double-click, full-screen toggle).
//!   * [`niceties_zoom`] — the R7/T11 live zoom + pty re-metric self-test
//!     (`niceties-zoom`): real ⌘+/⌘0 CGEvents grow the shared font, and the
//!     grid re-fits + the pty winsize follows.
//!   * [`niceties_drop`] — the R7/T7 file/image drag-drop self-test
//!     (`niceties-drop`): the drop handler is driven with constructed
//!     `ExternalPaths` events and asserts byte-exact escaped-path typing.
//!   * [`niceties_overlay`] — the R7/T9 "Launching…" overlay self-test
//!     (`niceties-overlay`): a slow silent pane shows the overlay past a short
//!     grace window and clears it on first output, while an instant-prompt pane
//!     never flashes it (the state-machine counter).
//!   * [`niceties_held`] — the R7/T10 held-pane self-test (`niceties-held`): a
//!     non-zero exit stays mounted with the dim in-buffer footer + the dismiss
//!     affordance, typing is inert, and dismiss respawns a fresh shell.
//!   * [`theme`] — the token → `gpui::Rgba` colour adapter shared by the R10/R11
//!     chrome components.
//!   * [`status_dot`] — the R10 `StatusDot` component (per-status colour + the
//!     ring/breathe pulse), reused by R11's toolbar pills.
//!   * [`context_menu`] — the in-house context-menu popup (anchored + deferred +
//!     click-away/Esc), reused by R11.
//!   * [`inline_rename`] — the shared inline-rename field (char-by-char editor +
//!     caret) the R10 sidebar row and the R11 toolbar pill both mount.
//!   * [`sidebar_actions`] — the `SidebarActions` create/close/select seam
//!     (R10 model-only; R13 rewires it to real sessions).
//!   * [`pane_strip_actions`] — the `PaneStripActions` pane select/close/add
//!     seam the R11 toolbar drives (model-only; R13 rewires it too).
//!   * [`sidebar_shell`] — the R10 sessions-mode sidebar: the shell layout
//!     (floating card / collapsed cap / peek / resize) and the sidebar card
//!     (project groups, tab rows, footer, mode/collapse toggles, multi-select
//!     routing, inline rename, Esc collapse), driving the R8 model through the
//!     `SidebarActions` seam.
//!   * [`toolbar`] — the R11 window toolbar pane strip: the brand block, the
//!     scroll-tracked pill row with its overflow chevron + attention badge and
//!     edge fades, inline pill rename, per-kind context menus, and the trailing
//!     `+`, driving the R8 model through the `PaneStripActions` seam.
//!   * [`pane_strip_live`] — the R11 live pane-strip self-test scenario
//!     (`pane-strip`): real CGEvents drive the shipped `WindowToolbarView`'s
//!     pill-vs-band drag differential + overflow chevron + auto-center, judged
//!     against AppKit frame reads (the in-process real-layout differentials live
//!     in `nice-itests`).
//!   * [`sidebar_live`] — the R10 live sidebar self-test scenario (`sidebar`):
//!     real CGEvents drive the shipped `SidebarShellView`'s resize clamp +
//!     double-click reset and the top-strip-vs-body drag differential, judged
//!     against AppKit frame reads, plus the collapse-cap geometry drift guards
//!     (R9 button-frame re-assert) and the per-state dot colour/pulse checks.
//!   * [`window_state`] — the R12 per-window composition root (`WindowState`),
//!     the Rust mirror of Swift's `AppState`: the R8 document + R10 sidebar /
//!     selection + the R10/R11 action seams + the R13 session slot. Handed to
//!     each window as a constructor argument by `app::build_window_root`.
//!   * [`window_registry`] — the R12 process-wide `WindowId → WindowState` map:
//!     register/deregister on open/close, MRU via `observe_window_activation`,
//!     the four-consumer lookup contract (`active_state`, id / session-id
//!     lookup), and the close→teardown hook.
//!   * [`keymap`] — the R12 shortcut dispatch: the 13 rebindable actions +
//!     ⌃⌘F generated from `nice_model::shortcuts`, the app-level (font/undo) vs
//!     window-level (sidebar/pane, through the registry's `active_state`)
//!     handler split, the process-level `FontSettings` fan-out, and the peek
//!     modifier-release observer.
//!
//! Entry dispatch: `NICE_RS_SELFTEST=<scenario>` runs the measurement harness
//! (see `nice_harness::selftest`); otherwise the normal app opens its window.

mod app;
mod app_shell;
mod app_shell_live;
mod chrome_live;
mod context_menu;
mod inline_rename;
mod input_live;
mod keymap;
mod multiwindow;
mod niceties_drop;
mod niceties_held;
mod niceties_overlay;
mod niceties_zoom;
mod pane_strip_actions;
mod pane_strip_live;
mod platform;
mod session_lifecycle;
mod session_manager;
mod sidebar_actions;
mod sidebar_live;
mod sidebar_shell;
mod status_dot;
mod theme;
mod toolbar;
mod window_registry;
mod window_state;

fn main() {
    match std::env::var("NICE_RS_SELFTEST") {
        Ok(selector) if !selector.trim().is_empty() => app::run_selftest(selector),
        _ => app::run(),
    }
}
