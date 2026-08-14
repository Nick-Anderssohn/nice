//! `TerminalView` — the gpui view that owns a [`FocusHandle`] and paints a
//! [`TerminalSessionHandle`] through a [`TerminalElement`] each frame.
//!
//! It observes the session handle (repaint on the handle's `notify`) and owns a
//! `FocusHandle` (needed for R5 key dispatch + DECSET-1004 focus reporting).
//! The caret's solid/hollow state is **computed** from
//! `focus_handle.is_focused(window) && window.is_window_active()` every frame —
//! there is deliberately **no separately-maintained focus flag** (that is
//! pain-catalog mechanism #5, remembered-not-computed state).
//!
//! Focus routing (M2 Item D): the view grabs key focus exactly **once**, on its
//! first render — a fresh pane starts focused with no app wiring — and never
//! again, so app chrome (an inline-rename field, a context menu) can hold focus
//! without the terminal yanking it back the next frame. Every later move is
//! explicit: the app calls [`TerminalView::focus`] (pane/tab activation, rename
//! commit/cancel, menu dismissal), and a mouse-down on the view re-focuses it
//! via an explicit `window.focus` in [`TerminalView::on_mouse_down`]. (gpui's
//! `track_focus` mouse-down auto-transfer can't carry this: it runs after
//! `on_mouse_down` in the reversed bubble order, so the `stop_propagation` on
//! the app-mouse-reporting path would suppress it.)
//!
//! ## R5 input path
//!
//! The view is the terminal's input edge. It owns the pure [`ImeState`]
//! marked-text machine and drives it through the platform [`TermInputHandler`]
//! (registered every frame in the element's paint). Three gpui listeners feed the
//! keyboard encoder:
//!
//! * [`on_key_down`](TerminalView::on_key_down) / [`on_key_up`](TerminalView::on_key_up)
//!   translate gpui `Keystroke`s (plus the injected macOS keyCode side-channel)
//!   into `nice-term-input` [`KeyInput`]s and write the encoded bytes to the pty
//!   — but **only** for keys the terminal owns (functional keys, ctrl/⌘/Meta
//!   chords, and — in full kitty mode — every key). Plain and shift printables
//!   fall through to the platform IME `insertText` path (so CJK composition and
//!   dead keys work); their committed text is written by
//!   [`ime_commit`](TerminalView::ime_commit) as data, never through the encoder.
//! * [`on_modifiers_changed`](TerminalView::on_modifiers_changed) is the kitty
//!   modifiers-as-functional-keys report (bare Shift/Ctrl/Alt/⌘ press+release):
//!   active only under REPORT_ALL_KEYS, resolving the left/right key from the
//!   flagsChanged keyCode side-channel.
//! * The five G1 IME gating behaviours live in [`ImeState`]; this view is the
//!   thin adapter (marked-text updates, the Enter-commit swallow, the
//!   never-`None` candidate anchor at the grid cursor cell).
//!
//! ## R5 mouse / paste / copy / focus (slice 3)
//!
//! The remaining VT input surface is wired here too, on top of the same handle:
//!
//! * **VT mouse reporting** — when the app requests it (the core `Term`'s
//!   `MOUSE_MODE` bits), `on_mouse_*` hit-test the pixel position to a cell
//!   ([`mouse::cell_from_offset`], reusing R4's [`grid_top_y`] metrics) and
//!   encode X10/SGR/UTF-8 reports through slice-1's
//!   [`encode_mouse`](nice_term_input::encode_mouse). A held **Shift** is the
//!   local override: it forces selection/scroll even while the app reports.
//! * **Local drag selection** — a bare left drag (or any drag with Shift) drives
//!   R4's [`TerminalSessionHandle::set_selection`] in buffer coordinates.
//! * **⌘+click opens a URL** — a ⌘+left-press over a link (matched by
//!   [`crate::hyperlink`]) is consumed here and opened on the matching release
//!   via the injected [`UrlOpener`], starting no selection and sending no mouse
//!   report. Like Ghostty, ⌘ therefore overrides app mouse reporting for links —
//!   but only for them: a ⌘+click anywhere else reports/selects as it always did.
//! * **⌘-hover underlines the link** — while ⌘ is held, the URL under the
//!   pointer is tracked in `hovered_hyperlink` and its range handed to the
//!   [`TerminalElement`], which underlines exactly those cells; the pointer
//!   turns into a hand. The hover follows the ⌘ key itself (press/release
//!   recompute from the last pointer position) and clears when the pointer
//!   leaves the pane. It is passive — motion reports still reach the app.
//! * **⌘V paste** — the clipboard text is framed by
//!   [`wrap_bracketed_paste`](nice_term_input::wrap_bracketed_paste) gated on the
//!   core's `bracketed_paste_active()`, then written to the pty.
//! * **⌘C copy** — a live selection is rendered to a string and written to the
//!   pasteboard (only while kitty is off; under kitty ⌘C forwards `ESC[99;9u`).
//! * **Shift+PageUp/PageDown/Home/End scrollback** (Phase 0) — consumed before
//!   the key encoder and driven on the viewport
//!   ([`crate::input::scrollback_key_action`]); plain variants still encode,
//!   and on the alternate screen even the Shift chords go to the app. Works on
//!   a held pane too (the read-gesture carve-out beside held ⌘C).
//! * **Copy mode** (Phase 3) — while the pane is in copy mode (alacritty's
//!   `TermMode::VI`, read through the handle) the view stops being an input
//!   edge at all: [`on_key_down`](TerminalView::on_key_down) consumes EVERY key
//!   through [`crate::input::copy_mode_key_action`] ahead of the held/IME
//!   gates, [`on_key_up`](TerminalView::on_key_up) drops the matching release
//!   reports, the three IME callbacks drop their pty writes (still running
//!   their state transitions so a composition can clear itself), and mouse
//!   reporting is suspended exactly like the Shift override. The mode toggles
//!   themselves are app-level actions, never key-listener business.
//! * **DECSET-1004 focus in/out** — a change in the combined focus predicate
//!   (`is_focused && window active`, the same value the caret uses) emits
//!   `ESC[I` / `ESC[O` when the app enabled focus reporting.
//!
//! [`KeyInput`]: nice_term_input::KeyInput

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
// gpui has a `Point` of its own (pixels), so the grid's is aliased.
use alacritty_terminal::index::{Column, Line, Point as GridPoint};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;
use gpui::{
    div, point, prelude::*, px, rgb, size, App, Bounds, ClipboardItem, Context, CursorStyle,
    Entity, ExternalPaths, FocusHandle, Focusable, Font, FontFeatures, FontStyle, FontWeight,
    KeyDownEvent, KeyUpEvent, Keystroke, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba, ScrollWheelEvent, SharedString,
    Subscription, TextRun, Window,
};

use nice_term_core::ExitStatus;
use nice_term_input::{
    encode_mouse, utf16_to_byte, wrap_bracketed_paste, ImeState, KeyEventType, MouseAction,
    MouseButton as VtButton, MouseInput, OptionAsMeta, OptionSide,
};
use nice_theme::Srgba;

use crate::drop::{drop_bytes, ImageDropProvider};
use crate::element::{fit_grid, grid_top_y, GridCache, ImeInput, TerminalElement, TerminalMetrics};
use crate::font::{snap_metrics_to_scale, FontSettings};
use crate::hyperlink::{hover_changed, UrlOpener, UrlRegexCache};
use crate::mouse::{link_click_verdict, LinkClickVerdict};
use crate::input::{
    build_key_input, build_modifier_input, copy_mode_key_action, encoder_config, ime_gate,
    key_gate, kitty_forwards_super, mouse_reports_to_app, named_key_for, scrollback_key_action,
    CopyModeAction, ImeCallback, KeyCodeProbe, KeyGate, ScrollbackAction,
};
use crate::mouse;
use crate::overlay::{
    copy_mode_badge_label, held_exit_footer, HeldPane, LaunchDeadline, LaunchOverlay,
    DEFAULT_LAUNCH_OVERLAY_GRACE, HELD_FOOTER_LABEL,
};
use crate::session_handle::{TerminalEvent, TerminalSessionHandle};
use crate::theme::TerminalTheme;

/// Default coalescing window for bounds-driven pty refits. Swift parity: the
/// SwiftTerm fork ships `resizeDebounceMs = 200` and Nice leaves it at the
/// default (disabling it only for the one pre-fork bootstrap apply — mirrored
/// here by applying the FIRST fit synchronously, see
/// [`TerminalView::schedule_refit`]).
pub const RESIZE_DEBOUNCE_DEFAULT: Duration = Duration::from_millis(200);

/// A view over one terminal session. Construct with [`TerminalView::new`] from a
/// session handle + theme value + accent (R2) + cell metrics.
pub struct TerminalView {
    handle: Entity<TerminalSessionHandle>,
    theme: TerminalTheme,
    accent: Srgba,
    /// The surface-fill alpha (0.55–1.0) the grid paints its DEFAULT background at
    /// (restyle plan 3 transparency). `1.0` (the default) paints the whole-viewport
    /// default-bg fill fully opaque, exactly as before; below `1.0` the grid SKIPS
    /// that fill so the translucent window-body backing behind it shows through as
    /// the single surface (see [`crate::element::TerminalElement`]'s paint). Cells
    /// with an explicit background, selection, cursor, and glyphs stay opaque on
    /// top regardless. The app pushes this from `SharedThemeState`; this crate
    /// never observes an app entity.
    background_opacity: f32,
    /// The shared, app-level terminal font state (family chain + size + cell
    /// metrics) this view observes (T11). Owned at the app root in `crates/nice`;
    /// every pane shares one entity, so a ⌘+/⌘−/⌘0 zoom fans out to all of them.
    /// The three fields below are a **cache** of `font.read(cx)`, refreshed on
    /// construction and whenever the entity notifies (see [`on_font_changed`]),
    /// so `render` / the mouse + IME handlers read them synchronously without
    /// re-borrowing the entity every frame.
    ///
    /// [`on_font_changed`]: TerminalView::on_font_changed
    font: Entity<FontSettings>,
    font_family: SharedString,
    font_px: f32,
    metrics: TerminalMetrics,
    /// The window backing-scale factor observed on the last render. Seeded at
    /// the derive-time snap scale (2× — Retina), where the snap below is a
    /// no-op; corrected on the first render, before the first paint/fit.
    display_scale: f32,
    /// [`metrics`](Self::metrics) with the cell WIDTH re-snapped to
    /// `display_scale`'s device-pixel grid ([`snap_metrics_to_scale`]) — THE
    /// cell box every geometry consumer uses: the painted element, the pty
    /// fit, mouse hit-testing, and the IME anchor. On a 2× display it equals
    /// `metrics` exactly; on a 1× display it widens a half-px cell to whole
    /// device px, so the text / background / box-drawing grids share one
    /// device-aligned pitch instead of drifting 0.5 px per column apart.
    effective_metrics: TerminalMetrics,
    focus_handle: FocusHandle,
    /// Whether the first-render focus grab has run (M2 Item D focus-once). Set
    /// on the first [`Render::render`]; never cleared. All later focus moves are
    /// explicit ([`focus`](Self::focus), click-to-focus) so app chrome can hold
    /// focus without the terminal stealing it back per frame.
    focused_once: bool,
    /// Whether that first-render grab actually takes focus (see
    /// [`set_focus_on_first_render`](Self::set_focus_on_first_render)). Default
    /// `true`; the split host clears it on the panes that must NOT steal focus
    /// when a whole pane tree mounts in one pass.
    focus_on_first_render: bool,
    /// The pure marked-text (preedit) state machine driven by the platform IME.
    ime: ImeState,
    /// Option-as-Meta policy (SwiftTerm-parity default `Both`). Consulted per
    /// keystroke to decide whether a ⌥-modified printable is a Meta chord (ESC
    /// prefix, bypasses the IME) or is left for the OS to compose.
    option_as_meta: OptionAsMeta,
    /// The injected macOS keyCode side-channel (built in `crates/nice/src/platform`).
    /// `None` until the app wires it; the encoder then falls back to gpui's key
    /// names alone (no layout-independent alternate-key recovery).
    keycode_probe: Option<KeyCodeProbe>,
    /// The injected raw-image drop provider (T7): reads the drag pasteboard for
    /// image data and returns a temp PNG path. `None` until the app wires it (the
    /// sole objc2 home is `crates/nice/src/platform`); a drop with no file URLs
    /// then simply types nothing (the file-URL path is unaffected).
    image_drop_provider: Option<ImageDropProvider>,
    /// The injected URL opener (see [`UrlOpener`]) ⌘+click hands its match to.
    /// `None` until the app wires it, in which case the open falls back to
    /// `cx.open_url` — enough for a standalone embedding, but production must
    /// inject the main-queue-deferred opener (see the type's docs).
    url_opener: Option<UrlOpener>,
    /// The compiled [`URL_REGEX`](crate::URL_REGEX) matcher, built on first use
    /// and reused by every link lookup — a ⌘+click today, a search per ⌘-held
    /// mouse-move once hover lands; the DFAs are far too expensive to rebuild
    /// per event.
    url_regex: UrlRegexCache,
    /// The `(vrow, col)` a ⌘+left-press landed on **over a link**, pending its
    /// release: ⌘+click opens on mouse-UP, and only when the release lands on
    /// that same cell, so a ⌘+drag off the link cancels (Ghostty parity).
    /// `Some` only between such a press and the next left-up (in or out of the
    /// pane). A ⌘+press that was NOT over a link never arms and falls through to
    /// the normal reporting / selection paths.
    link_click_armed: Option<(usize, usize)>,
    /// The link the pointer is over **while ⌘ is held** — its URL text plus the
    /// match range in buffer coordinates. This is the whole ⌘-hover affordance:
    /// the range is handed to the [`TerminalElement`] (which underlines exactly
    /// its cells) and its mere presence switches the pointer to the hand cursor.
    /// `None` whenever ⌘ is not held, the pointer is not over a match, or the
    /// pointer has left the pane. Every change to it notifies, so paint always
    /// sees the current state (see [`set_hovered_hyperlink`](
    /// Self::set_hovered_hyperlink)).
    hovered_hyperlink: Option<(String, Match)>,
    /// The last pointer position seen by [`on_mouse_move`](Self::on_mouse_move),
    /// in window coordinates. Kept so a ⌘ press or release — which carries no
    /// position of its own — can recompute the hover in place: pressing ⌘ while
    /// already pointing at a URL must underline it without a pointer twitch.
    /// `None` until the pointer first moves over this pane, and cleared again
    /// when it leaves: gpui delivers mouse-moves only while the pointer is
    /// inside the element, so a position kept past the exit would be a stale
    /// place for a later ⌘ press to "hover".
    last_mouse_pos: Option<Point<Pixels>>,
    /// This frame's grid bounds, published by the element during paint and read
    /// by the mouse handlers on the next event for pixel→cell hit-testing. Shared
    /// so paint writes it without re-entering this entity (see [`TerminalElement`]).
    paint_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The cross-frame damage-gated row cache (fix round r5b), reconciled and
    /// painted by the [`TerminalElement`] built each frame. Shared the same way
    /// as `paint_bounds` (`Rc`, element-side mutation only) so paint never
    /// re-enters this entity; the view itself never reads it — it only keeps it
    /// alive across frames.
    grid_cache: Rc<RefCell<GridCache>>,
    /// Whether the element schedules a pty grid refit when its painted bounds
    /// change (M2 Item E — window resize → `resize_pty_to_fit`). Off by default:
    /// fixed-grid embeddings (the pixel-assertion self-tests spawn at an exact
    /// `rows × cols` and key their sample points on it) must never have their
    /// grid silently re-fitted. The shipped app's pane host opts in via
    /// [`set_auto_refit`](Self::set_auto_refit).
    auto_refit: bool,
    /// The last `(rows, cols)` successfully pushed to the pty by
    /// [`resize_pty_to_fit`](Self::resize_pty_to_fit) — the resize feedback-loop
    /// guard: a refit that computes the same fit is skipped, so
    /// resize → SIGWINCH → repaint can never re-trigger itself. `None` until the
    /// first successful push (and reset on a held-pane respawn, whose fresh
    /// shell spawns at the spec size and must be refit unconditionally).
    last_pty_fit: Option<(u16, u16)>,
    /// Coalescing window for bounds-driven refits — the Swift-parity resize
    /// debounce (the SwiftTerm fork's `resizeDebounceMs`, default 200 ms in
    /// `AppleTerminalView.processSizeChange`): a live-resize burst lands ONE
    /// `TIOCSWINSZ`/SIGWINCH per window instead of one per row crossing, so the
    /// child isn't redraw-thrashed mid-drag. Zero applies synchronously.
    resize_debounce: Duration,
    /// A bounds change arrived while (or since) the coalescing timer was armed.
    /// Latest-wins: the fire re-reads `paint_bounds` rather than a size stored
    /// at arrival, so the apply uses whatever the newest paint published.
    pending_refit_arrived: bool,
    /// The coalescing timer is armed. Deliberately NOT re-armed by new arrivals
    /// (the fork's semantics): a sustained drag lands once per window rather
    /// than never.
    pending_refit_scheduled: bool,
    /// Whether a local selection drag is in progress: `true` between the left
    /// mouse-down that anchored a selection and the ending mouse-up (in or out
    /// of the pane). The anchor itself is NOT here — it lives in the `Term`'s
    /// own `Selection`, which `extend_selection` never rebuilds and which
    /// alacritty rotates with the grid as output streams (`Term::scroll_up` →
    /// `Selection::rotate`). That keeps the anchor **content-locked** through
    /// both motions the old viewport-row anchor could not reconcile: streaming
    /// while parked in scrollback (grid rotation moves selection and display
    /// offset together) and the user scrolling mid-drag (grid coordinates
    /// don't move at all). The drag END is **screen-locked** instead: every
    /// mouse-move and every mid-drag wheel step re-resolves the pointer
    /// against the current display offset and extends to that cell (see
    /// `docs/plans/selection-scroll-anchor.md`).
    ///
    /// This is the GESTURE flag, deliberately not tied to the selection's
    /// liveness: if the Term drops the selection mid-drag (a clear/erase
    /// intersecting it, alt-screen swap, column resize), extends become
    /// no-ops but the flag stays set until a real release — matching
    /// alacritty — so the mouse-up/move guards keep swallowing the gesture's
    /// events instead of leaking VT reports for a press the app never saw.
    drag_selecting: bool,
    /// The last cell a mouse **report** was emitted for, to de-duplicate motion
    /// reports (an app gets one report per cell crossed, not per pixel moved).
    last_report_cell: Option<(usize, usize)>,
    /// Fractional wheel travel not yet emitted as a wheel report, in cells. Whole
    /// steps become button-64/65 reports; the remainder is kept so a slow
    /// trackpad still eventually reports (mirrors the scrollback accumulator).
    wheel_accum: f32,
    /// The last combined focus state (`is_focused && window active`) reported via
    /// DECSET-1004. `None` until the first render seeds it (so startup never emits
    /// a spurious `ESC[I`); thereafter a change edge-triggers a focus report.
    last_focus_reported: Option<bool>,
    /// The "Launching…" overlay timing machine (T9), driven off the R3
    /// [`TerminalEvent`] stream + the grace deadline (see [`crate::overlay`]).
    overlay: LaunchOverlay,
    /// Whether this launch's grace deadline has been armed yet (armed once, on the
    /// first paint of a pending pane — see [`arm_overlay_deadline`]).
    ///
    /// [`arm_overlay_deadline`]: TerminalView::arm_overlay_deadline
    overlay_armed: bool,
    /// The grace window before the overlay shows — a test-settable seam (Swift's
    /// `launchOverlayGraceSeconds`). Defaults to [`DEFAULT_LAUNCH_OVERLAY_GRACE`].
    overlay_grace: Duration,
    /// The injected App-Nap-safe grace-deadline factory (T9). `None` falls back to
    /// a gpui timer (fine for a frontmost window); the shipped app installs the
    /// real spike-6 watchdog-pattern deadline from `crates/nice/src/platform`.
    launch_deadline: Option<LaunchDeadline>,
    /// The command string shown (dimmed) under the "Launching…" title, if the app
    /// set one. Purely cosmetic — the timing is the overlay's job.
    overlay_command: Option<SharedString>,
    /// The held-pane machine (T10): latches a non-clean exit so the view stays
    /// mounted with a dismiss affordance (see [`crate::overlay`]).
    held: HeldPane,
    /// Whether the dim in-buffer exit footer has been written for the current hold
    /// (written exactly once, on the `Exited { held: true }` edge).
    held_footer_written: bool,
    /// Repaint subscription to the session handle. Held so it stays live for the
    /// view's lifetime.
    _handle_sub: Subscription,
    /// Typed-event subscription to the session handle's [`TerminalEvent`] stream
    /// (`OutputStarted` / `Exited`) — the R3 events that drive the overlay + held
    /// machines. Held for the view's lifetime.
    _event_sub: Subscription,
    /// Observation of the shared [`FontSettings`]. Held for the view's lifetime;
    /// fires [`on_font_changed`](TerminalView::on_font_changed) on every zoom.
    _font_sub: Subscription,
}

/// A hit-tested grid cell: viewport coordinates (what a VT report carries) plus
/// the buffer line (what [`TerminalSessionHandle::set_selection`] wants —
/// negative in scrollback).
#[derive(Clone, Copy)]
struct Hit {
    col: usize,
    vrow: usize,
    buffer_line: i32,
}

/// Cap on wheel reports emitted for a single scroll event, so a hard trackpad
/// fling under app mouse reporting can't flood the pty with button-64/65 reports.
const WHEEL_REPORT_MAX: i32 = 8;

impl TerminalView {
    /// Build a view over `handle`, painting with `theme` (caret in `accent`
    /// unless the theme overrides the cursor) using the shared [`FontSettings`]
    /// `font` for the family + size + cell metrics. The view observes `font`: a
    /// ⌘+/⌘−/⌘0 zoom re-metrics it and resizes the pty (see
    /// [`on_font_changed`](Self::on_font_changed)), no view rebuild.
    pub fn new(
        handle: Entity<TerminalSessionHandle>,
        theme: TerminalTheme,
        accent: Srgba,
        font: Entity<FontSettings>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Repaint whenever the session handle notifies (new output / events).
        let sub = cx.observe(&handle, |_this, _handle, cx| cx.notify());
        // Subscribe to the handle's typed R3 lifecycle events (OutputStarted /
        // Exited): they drive the T9 launch overlay + the T10 held-pane machine.
        // The handle is a view-independent entity that keeps draining while a pane
        // is hidden, so these fire even off-screen.
        let event_sub = cx.subscribe(&handle, |this, _handle, event: &TerminalEvent, cx| {
            this.on_terminal_event(event, cx);
        });
        // Observe the shared font state: a zoom re-metrics this view + resizes its
        // pty. The entity persists across changes, so nothing here is rebuilt.
        let font_sub = cx.observe(&font, |this, _font, cx| this.on_font_changed(cx));
        // Seed the metric cache from the shared state (the observe callback only
        // fires on later changes, never at subscription time).
        let (font_family, font_px, metrics) = {
            let f = font.read(cx);
            (f.family(), f.px(), f.metrics())
        };
        // A view built AFTER its pane already produced output — a deferred pane
        // spawned while its tab was inactive, first visited now — must not flash the
        // "Launching…" overlay: that pane's one-shot `OutputStarted` fired to zero
        // subscribers, so no event will ever clear the overlay for it. Reconstruct
        // the cleared state from the session's latched `output_started` fact, so the
        // first-paint arm gate (`overlay.is_pending()`) never arms. A view mounted at
        // spawn sees `output_started == false` and arms the grace normally.
        let mut overlay = LaunchOverlay::new();
        if handle.read(cx).output_started() {
            overlay.clear();
        }
        Self {
            handle,
            theme,
            accent,
            background_opacity: 1.0,
            font,
            font_family,
            font_px,
            metrics,
            // 2× (Retina) until the first render reads the real window scale;
            // the snap is exactly idempotent there, so this equals `metrics`.
            display_scale: 2.0,
            effective_metrics: snap_metrics_to_scale(metrics, 2.0),
            focus_handle: cx.focus_handle(),
            focused_once: false,
            focus_on_first_render: true,
            ime: ImeState::new(),
            option_as_meta: OptionAsMeta::default(),
            keycode_probe: None,
            image_drop_provider: None,
            url_opener: None,
            url_regex: UrlRegexCache::new(),
            link_click_armed: None,
            hovered_hyperlink: None,
            last_mouse_pos: None,
            paint_bounds: Rc::new(Cell::new(None)),
            grid_cache: Rc::new(RefCell::new(GridCache::default())),
            auto_refit: false,
            last_pty_fit: None,
            resize_debounce: RESIZE_DEBOUNCE_DEFAULT,
            pending_refit_arrived: false,
            pending_refit_scheduled: false,
            drag_selecting: false,
            last_report_cell: None,
            wheel_accum: 0.0,
            last_focus_reported: None,
            overlay,
            overlay_armed: false,
            overlay_grace: DEFAULT_LAUNCH_OVERLAY_GRACE,
            launch_deadline: None,
            overlay_command: None,
            held: HeldPane::new(),
            held_footer_written: false,
            _handle_sub: sub,
            _event_sub: event_sub,
            _font_sub: font_sub,
        }
    }

    /// The shared font state this view observes (T11). Exposed so the app /
    /// self-tests can read the current size + metrics and drive zoom.
    pub fn font(&self) -> &Entity<FontSettings> {
        &self.font
    }

    /// The cell metrics this view is currently painting at: the shared
    /// [`FontSettings`] box with the cell width re-snapped to the live window
    /// backing scale (equal to it on 2× displays). This is what the element,
    /// pty fit, hit-testing, and IME anchor all use.
    pub fn metrics(&self) -> TerminalMetrics {
        self.effective_metrics
    }

    /// The grid bounds this view's last paint published — the frame
    /// [`hit_cell`](Self::hit_cell) measures every mouse position in. Read-only,
    /// and `None` before the first paint. Exposed so a self-test can aim a
    /// synthetic mouse event at a known cell through the same geometry the
    /// hit-test uses (bounds origin + [`grid_top_y`] + [`metrics`](Self::metrics))
    /// instead of re-deriving the layout and drifting from it.
    pub fn paint_bounds(&self) -> Option<Bounds<Pixels>> {
        self.paint_bounds.get()
    }

    /// Refresh the cached font from the shared [`FontSettings`] and **re-metric**:
    /// recompute the grid so it fills the current view at the new cell size and
    /// push the new `(rows, cols)` to the pty via the R3/R4 resize path (which
    /// drives SIGWINCH so the child reflows). No view rebuild — this runs on the
    /// existing entity, from `cx.observe`.
    ///
    /// The fit uses this frame's element bounds (published by the last paint via
    /// `paint_bounds`); before the first paint there are no bounds, so the resize
    /// is skipped (the next paint already picks up the new metrics anyway).
    fn on_font_changed(&mut self, cx: &mut Context<Self>) {
        let (family, px_size, metrics) = {
            let f = self.font.read(cx);
            (f.family(), f.px(), f.metrics())
        };
        self.font_family = family;
        self.font_px = px_size;
        self.metrics = metrics;
        self.effective_metrics = snap_metrics_to_scale(metrics, self.display_scale);
        self.resize_pty_to_fit(cx);
        cx.notify();
    }

    /// Re-fit the pty to the current window at the current metrics: recompute the
    /// grid that fills this frame's element bounds and push `(rows, cols)` to the
    /// pty over the R3/R4 resize path. Shared by the zoom re-metric
    /// ([`on_font_changed`](Self::on_font_changed)) and the T10 dismiss respawn
    /// (the fresh shell must fill the window, not stay at the spec's spawn size).
    ///
    /// Best-effort: before the first paint there are no bounds (skip — the next
    /// paint picks up the size anyway), and a not-yet-spawned / exited session
    /// errors, which is dropped (nothing to reflow; the fit is then NOT recorded,
    /// so a later spawn still gets its refit).
    ///
    /// M2 Item E adds the third caller — a deferred callback scheduled by the
    /// element when its painted bounds change — and the feedback-loop guard: the
    /// computed fit is compared against [`last_pty_fit`](Self::last_pty_fit) and
    /// a no-delta refit is skipped, so resize → SIGWINCH → output → repaint can
    /// never re-trigger itself.
    pub(crate) fn resize_pty_to_fit(&mut self, cx: &App) {
        if let Some(bounds) = self.paint_bounds.get() {
            let (rows, cols) = fit_grid(
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
                // The device-snapped box the element actually paints at — the
                // fit must count the columns that box tiles into the window.
                self.effective_metrics,
            );
            if self.last_pty_fit == Some((rows, cols)) {
                return; // no rows/cols delta — nothing to push (loop guard)
            }
            if self.handle.read(cx).session().resize(rows, cols).is_ok() {
                self.last_pty_fit = Some((rows, cols));
            }
        }
    }

    /// Coalesced entry point for bounds-driven refits — the port of the Swift
    /// resize debounce (`AppleTerminalView.processSizeChange` in the SwiftTerm
    /// fork). Semantics, matching the fork exactly:
    ///
    /// - **Bootstrap applies synchronously.** The first fit after a spawn
    ///   (`last_pty_fit == None`, which a held-pane respawn resets) skips the
    ///   coalescer, so the shell starts at the real geometry — the same reason
    ///   Nice's Swift host zeroes `resizeDebounceMs` around its one pre-fork
    ///   `setFrameSize` apply.
    /// - **Zero debounce applies synchronously** (test/consumer escape hatch).
    /// - Otherwise **latest-wins coalescing**: mark an arrival, arm ONE timer
    ///   per burst (never re-armed by later arrivals, so a sustained drag lands
    ///   once per window rather than never), and at fire time re-read the live
    ///   `paint_bounds` instead of any size captured at arrival.
    pub(crate) fn schedule_refit(&mut self, cx: &mut Context<Self>) {
        if self.last_pty_fit.is_none() || self.resize_debounce.is_zero() {
            self.resize_pty_to_fit(cx);
            return;
        }
        self.pending_refit_arrived = true;
        if self.pending_refit_scheduled {
            return;
        }
        self.pending_refit_scheduled = true;
        let delay = self.resize_debounce;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |view, cx| view.fire_coalesced_refit(cx));
        })
        .detach();
    }

    /// The coalescing timer fired: apply the pending refit at the LIVE painted
    /// bounds (latest-wins). A fire with nothing pending no-ops — the arrived
    /// flag is the cancellation token (the timer itself can't be cancelled).
    fn fire_coalesced_refit(&mut self, cx: &App) {
        self.pending_refit_scheduled = false;
        if !self.pending_refit_arrived {
            return;
        }
        self.pending_refit_arrived = false;
        self.resize_pty_to_fit(cx);
    }

    /// Set the coalescing window for bounds-driven refits (0 = synchronous).
    /// Parity with the SwiftTerm fork's `resizeDebounceMs` knob; the default is
    /// [`RESIZE_DEBOUNCE_DEFAULT`].
    pub fn set_resize_debounce_ms(&mut self, ms: u64) {
        self.resize_debounce = Duration::from_millis(ms);
    }

    /// Opt in to bounds-driven pty refits (M2 Item E): when set, a change in the
    /// element's painted bounds schedules [`schedule_refit`](Self::schedule_refit)
    /// via `cx.defer` (outside the paint pass), so the grid tracks a live window
    /// resize — coalesced behind the Swift-parity resize debounce. The shipped
    /// pane host sets this; fixed-grid scenario embeddings leave it off (their
    /// pixel assertions key on the exact spawn grid).
    pub fn set_auto_refit(&mut self, on: bool) {
        self.auto_refit = on;
    }

    /// Opt this view OUT of the first-render focus grab (the grab in
    /// [`Render::render`], see [`focused_once`](Self::focused_once)).
    ///
    /// Single-view hosting made "a fresh terminal takes key focus on its first
    /// render" free. Splits break it: a pill whose whole pane tree mounts in
    /// one pass would hand focus to whichever leaf rendered LAST rather than to
    /// the pane the model says is focused. The host clears this on every
    /// non-focused pane it mounts, then focuses the focused one explicitly
    /// ([`focus`](Self::focus)).
    ///
    /// Default `true`, so an embedding that never calls it behaves exactly as
    /// before. Only consulted on the first render; later calls change nothing.
    pub fn set_focus_on_first_render(&mut self, on: bool) {
        self.focus_on_first_render = on;
    }

    /// Live-recolor this pane (R21 theme fan-out): replace the render `theme` +
    /// caret `accent` and repaint, no view rebuild — the same field-update +
    /// `cx.notify()` shape as [`on_font_changed`](Self::on_font_changed). The paint
    /// path already follows `accent` for the caret when the theme's cursor is unset
    /// (see [`accent_rgba`](Self::accent_rgba)), so a scheme / terminal-theme change
    /// carries its own accent through here. **Boundary-legal** (TRANCHE-2-NOTES §4):
    /// plain color values in — the app pushes these from `SharedThemeState`; this
    /// view crate never observes an app entity.
    pub fn set_theme(&mut self, theme: TerminalTheme, accent: Srgba, cx: &mut Context<Self>) {
        self.theme = theme;
        self.accent = accent;
        cx.notify();
    }

    /// Live-set the surface-fill opacity (restyle plan 3 transparency fan-out):
    /// the alpha the grid paints its DEFAULT background at. Clamped to `[0, 1]`.
    /// `1.0` restores the fully-opaque whole-viewport fill; below `1.0` the grid
    /// skips it so the translucent window-body backing shows through. Repaints
    /// without a rebuild; boundary-legal (plain `f32`), a companion to
    /// [`set_theme`](Self::set_theme).
    pub fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        self.background_opacity = opacity.clamp(0.0, 1.0);
        cx.notify();
    }

    /// Live-recolor only the accent (R21 accent fan-out): the caret / launch
    /// overlay tint, leaving the terminal `theme` untouched. Repaints without a
    /// rebuild. Boundary-legal (plain `Srgba` in), the accent-only companion to
    /// [`set_theme`](Self::set_theme).
    pub fn set_accent(&mut self, accent: Srgba, cx: &mut Context<Self>) {
        self.accent = accent;
        cx.notify();
    }

    /// The current render theme (read accessor). Lets the R21 fan-out probe
    /// (`nice-itests`) assert [`set_theme`](Self::set_theme) mutated the field and
    /// inspect `theme.cursor` (the `None` ⇒ caret-follows-accent precondition).
    pub fn theme(&self) -> &TerminalTheme {
        &self.theme
    }

    /// The current caret / launch-overlay accent (read accessor). When the render
    /// theme's `cursor` is unset the block caret paints in exactly this color
    /// (`element.rs`), so an [`set_accent`](Self::set_accent) that changes this
    /// value recolors the caret on a `cursor: None` theme.
    pub fn accent(&self) -> Srgba {
        self.accent
    }

    // R12: `zoom_font` / `reset_font` / `try_zoom_chord` were removed here. The
    // ⌘=/⌘−/⌘0 zoom chords are app-level keyboard shortcuts now (`crate::keymap`
    // in `crates/nice`), which drive the shared `FontSettings` entity directly;
    // this view keeps observing that entity (see `on_font_changed`) and re-metrics
    // on every zoom, but no longer intercepts the chords in its key path.

    /// The view's focus handle (R5 drives key input through it; the app's focus
    /// routing reads it — see [`focus`](Self::focus)).
    pub fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Move key focus to this terminal — the explicit focus-routing seam (M2
    /// Item D). The app calls it on pane/tab activation and when handing focus
    /// back after a chrome interaction (inline-rename commit/cancel, context-menu
    /// dismissal). Idempotent: `Window::focus` early-returns if this handle
    /// already holds focus.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    /// Install the macOS keyCode side-channel (see [`KeyCodeProbe`]). The app
    /// calls this once with a closure that reads `[NSApp currentEvent].keyCode`
    /// (built in `crates/nice/src/platform` — the sole objc2 home). Without it
    /// the encoder still works from gpui's key names; it just cannot recover the
    /// layout-independent base-layout codepoint for kitty alternate reporting.
    pub fn set_keycode_probe(&mut self, probe: KeyCodeProbe) {
        self.keycode_probe = Some(probe);
    }

    /// Install the raw-image drop provider (T7). The app calls this once with a
    /// closure that reads the drag pasteboard for image data, transcodes it to a
    /// temp PNG, and returns that path (built in `crates/nice/src/platform` — the
    /// sole objc2 home, so this crate stays objc2-free). It is consulted only for
    /// a drop that carried no file URLs (the Swift browser / Messages / Preview
    /// raw-image fallback); without it such a drop types nothing.
    pub fn set_image_drop_provider(&mut self, provider: ImageDropProvider) {
        self.image_drop_provider = Some(provider);
    }

    /// Install the URL opener ⌘+click uses (see [`UrlOpener`]). The app calls
    /// this once with `crates/nice/src/platform`'s deferred `NSWorkspace
    /// openURL:` wrapper; self-tests inject a recorder. Without it the view falls
    /// back to `cx.open_url`, which works but re-enters AppKit from inside the
    /// mouse-up listener's `App` borrow — production must not rely on it.
    pub fn set_url_opener(&mut self, opener: UrlOpener) {
        self.url_opener = Some(opener);
    }

    /// The URL of the link currently ⌘-hovered, if any — a read-only probe of
    /// `hovered_hyperlink`, whose only other observable is an underline in
    /// painted pixels. The `niceties-link` self-test reads it to assert the
    /// hover follows the pointer and the ⌘ key; nothing in the app calls it.
    pub fn hovered_hyperlink_url(&self) -> Option<&str> {
        self.hovered_hyperlink.as_ref().map(|(url, _)| url.as_str())
    }

    // -- launch overlay + held panes (T9 / T10) --------------------------------

    /// Install the App-Nap-safe grace-deadline factory (T9 — see
    /// [`LaunchDeadline`]). The app calls this once with a closure built in
    /// `crates/nice/src/platform` (the sole foreign-code home); without it the
    /// overlay falls back to a plain gpui timer, adequate for a frontmost window.
    pub fn set_launch_deadline(&mut self, deadline: LaunchDeadline) {
        self.launch_deadline = Some(deadline);
    }

    /// Set the "Launching…" grace window (Swift's `launchOverlayGraceSeconds`
    /// seam). The self-tests set a short window so the overlay shows promptly.
    pub fn set_overlay_grace(&mut self, grace: Duration) {
        self.overlay_grace = grace;
    }

    /// Set the command string shown (dimmed) under the "Launching…" title.
    pub fn set_overlay_command(&mut self, command: impl Into<SharedString>) {
        self.overlay_command = Some(command.into());
    }

    /// Whether the "Launching…" overlay is currently painting (grace elapsed with
    /// no output). Exposed for the `niceties-overlay` self-test's state assertion.
    pub fn overlay_visible(&self) -> bool {
        self.overlay.is_visible()
    }

    /// Whether the overlay has EVER been visible for the current launch — the
    /// state-machine counter the `niceties-overlay` fast-path case asserts stays
    /// `false` (an instant-prompt pane never flashes the overlay).
    pub fn overlay_ever_visible(&self) -> bool {
        self.overlay.ever_visible()
    }

    /// Whether the pane is held open after a non-clean exit (T10). Exposed for the
    /// `niceties-held` self-test.
    pub fn is_held(&self) -> bool {
        self.held.is_held()
    }

    /// Dispatch a session lifecycle [`TerminalEvent`] into the overlay + held
    /// machines. `OutputStarted` clears the launch overlay; `Exited` clears it too
    /// (a pane that never output leaves no orphan overlay) and, when the R3
    /// classification says held, latches the held state + writes the dim in-buffer
    /// footer once.
    fn on_terminal_event(&mut self, event: &TerminalEvent, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::OutputStarted => {
                if self.overlay.clear() {
                    cx.notify();
                }
            }
            TerminalEvent::Exited { status, held } => {
                let mut changed = self.overlay.clear();
                if *held && self.held.on_exited(*status, *held) {
                    self.write_held_footer(*status, cx);
                    changed = true;
                }
                if changed {
                    cx.notify();
                }
            }
            // OSC title/cwd ride this same entity (R13) but are **app-level**
            // concerns — the pane pill label, the tab auto-title, per-pane cwd
            // persistence — routed by the session manager's own subscription on
            // this entity, not the view. The view holds no title/cwd state, so it
            // ignores them (a hidden pane has no view at all, which is exactly why
            // these events live on the entity).
            // `SearchRequested` is the same shape in reverse: the view *emits*
            // it (in-mode `/`/`?`) for the app's search bar to consume. Seeing
            // it come back round through this subscription means nothing here.
            TerminalEvent::TitleChanged(_)
            | TerminalEvent::TitleReset
            | TerminalEvent::CwdChanged(_)
            | TerminalEvent::SearchRequested { .. } => {}
            // `TerminalEvent` is `#[non_exhaustive]` for cross-crate consumers, but
            // it is defined in THIS crate, so this match is exhaustive here — a
            // future lifecycle variant will (rightly) force the view to handle it.
        }
    }

    /// Arm this launch's grace deadline exactly once (T9). The overlay-worthy case
    /// is a **silent** pane — no output means no damage, so nothing else would
    /// wake the UI to show the overlay — so the deadline is self-driving. Per
    /// spike-6 it must be App-Nap-safe: the injected [`LaunchDeadline`] uses the
    /// watchdog pattern (a dedicated OS-thread sleep that wakes the main runloop),
    /// not a coalescable timer. The fallback gpui timer is only used when no
    /// factory is injected (a frontmost window, the only self-testable case).
    fn arm_overlay_deadline(&mut self, cx: &mut Context<Self>) {
        self.overlay_armed = true;
        let grace = self.overlay_grace;
        match &self.launch_deadline {
            Some(factory) => {
                let fut = factory(grace);
                cx.spawn(async move |this, cx| {
                    fut.await;
                    let _ = this.update(cx, |view, cx| view.on_grace_elapsed(cx));
                })
                .detach();
            }
            None => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(grace).await;
                    let _ = this.update(cx, |view, cx| view.on_grace_elapsed(cx));
                })
                .detach();
            }
        }
    }

    /// The grace deadline fired: promote the overlay `Pending → Visible` (unless
    /// output already cleared it) and repaint.
    fn on_grace_elapsed(&mut self, cx: &mut Context<Self>) {
        if self.overlay.on_grace_elapsed() {
            cx.notify();
        }
    }

    /// Write the dim exit footer INTO the terminal buffer for a held pane (T10) —
    /// the exact `TabPtySession.paneExitFooter` line, parsed straight into the
    /// shared `Term` under a brief lock (the child is dead, so this is a synthetic
    /// feed, not a pty echo). Idempotent per hold. The held session keeps its
    /// scrollback alive, so the footer lands below whatever the process last
    /// printed and stays readable.
    fn write_held_footer(&mut self, status: ExitStatus, cx: &App) {
        if self.held_footer_written {
            return;
        }
        let footer = held_exit_footer(HELD_FOOTER_LABEL, status);
        if let Some(term_arc) = self.handle.read(cx).term() {
            let mut guard = term_arc.lock();
            // A fresh parser feeds the complete, self-contained footer sequence
            // (SGR + text + CR/LF — no OSC/DA, so the EventProxy never writes to the
            // now-closed pty). The FairMutex serialises this against the feeder,
            // which has already EOF'd by exit time.
            let mut parser: Processor = Processor::new();
            parser.advance(&mut *guard, footer.as_bytes());
        }
        self.held_footer_written = true;
    }

    /// Dismiss a held pane by respawning a fresh login shell in the same window
    /// (T10). **NEW single-pane-era UI**, a temporary stand-in until Stage 2's
    /// tab-dissolve owns pane teardown — deliberately minimal. This is the only
    /// path that frees the held term: [`TerminalSessionHandle::respawn_shell`]
    /// drops the held session (releasing its scrollback) and installs a fresh one
    /// in place, keeping this view's subscriptions + the app's present kick. A
    /// no-op if the pane is not held; also the `niceties-held` self-test seam.
    pub fn dismiss_held(&mut self, cx: &mut Context<Self>) {
        if !self.held.is_held() {
            return;
        }
        match self.handle.update(cx, |handle, hcx| handle.respawn_shell(hcx)) {
            Ok(()) => {
                self.held.dismiss();
                self.held_footer_written = false;
                // A fresh launch gets a fresh overlay grace (re-armed next paint).
                self.overlay.reset();
                self.overlay_armed = false;
                // The fresh shell spawns at the spec size; refit it to the window
                // unconditionally (the guard would otherwise skip a fit equal to
                // the OLD session's last push — but this is a NEW pty).
                self.last_pty_fit = None;
                self.resize_pty_to_fit(cx);
                cx.notify();
            }
            // Respawn failed (catastrophic fork/openpty) — keep the held pane so
            // its output stays readable rather than blanking to a dead view.
            Err(e) => eprintln!("nice: dismiss respawn failed: {e:#}"),
        }
    }

    // -- drag-drop (T7) --------------------------------------------------------

    /// Handle a file / image drop: type the dropped paths at the prompt as a
    /// space-joined run of backslash-escaped POSIX paths (drop order), framed in
    /// bracketed-paste markers when the app enabled DECSET 2004, else space-padded
    /// — never a trailing newline. Port of `NiceTerminalView.performDragOperation`
    /// (`NiceTerminalView.swift:399-428`).
    ///
    /// This is both the gpui `on_drop::<ExternalPaths>` target and the
    /// `niceties-drop` self-test seam (it accepts a constructed [`ExternalPaths`]).
    /// `ExternalPaths` carries only file URLs (gpui's macOS backend registers just
    /// `NSFilenamesPboardType`); a drop with none falls back to the injected
    /// image-drop provider (the raw-image path).
    pub fn handle_external_paths_drop(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let mut posix: Vec<String> = paths
            .paths()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        // Raw-image fallback: only when no file URLs were present (the explicit
        // file drop always wins), mirroring Swift's `extractDroppedPaths`.
        if posix.is_empty() {
            if let Some(provider) = &self.image_drop_provider {
                if let Some(temp) = provider() {
                    posix.push(temp.to_string_lossy().into_owned());
                }
            }
        }
        if posix.is_empty() {
            return;
        }
        let active = self.handle.read(cx).session().bracketed_paste_active();
        // `drop_bytes` filters unsafe paths (C0 / DEL) and returns `None` when none
        // survive — the caller sends nothing (Swift's `guard !paths.isEmpty`).
        if let Some(bytes) = drop_bytes(&posix, active) {
            self.write_pty(&bytes, cx);
            cx.notify();
        }
    }

    /// Set the Option-as-Meta policy (the R5 config surface; the settings UI is a
    /// later cycle). Defaults to [`OptionAsMeta::Both`] (SwiftTerm parity).
    pub fn set_option_as_meta(&mut self, policy: OptionAsMeta) {
        self.option_as_meta = policy;
    }

    // -- key input -------------------------------------------------------------

    /// The terminal's currently-tracked VT mode (kitty flags + DECCKM), read
    /// under a brief `Term` lock. `NONE` before the session is spawned.
    fn current_mode(&self, cx: &App) -> TermMode {
        self.handle
            .read(cx)
            .term()
            // `Term::mode()` returns `&TermMode`; copy it out (TermMode is `Copy`)
            // before the brief lock guard drops.
            .map(|term_arc| *term_arc.lock().mode())
            .unwrap_or(TermMode::NONE)
    }

    /// Whether this pane is in copy mode (Phase 3, P1: copy mode IS
    /// `TermMode::VI`). Read fresh from the handle at every gate — there is no
    /// view-side mirror of it to drift out of sync, and the mode outlives this
    /// view (it lives on the per-pane handle).
    fn copy_mode_active(&self, cx: &App) -> bool {
        self.handle.read(cx).copy_mode_active()
    }

    /// Write raw bytes to the child. Best-effort: a not-yet-spawned session
    /// errors, which is dropped (there is nowhere to send the keystroke yet).
    fn write_pty(&self, bytes: &[u8], cx: &App) {
        if !bytes.is_empty() {
            let _ = self.handle.read(cx).session().write_input(bytes);
        }
    }

    /// Typed input snaps a scrolled-up viewport back to the bottom — the
    /// standard terminal behavior: keystrokes headed for the pty first jump
    /// the view back to the live screen so the user can see what they type.
    /// Callers are the typed-input paths only ([`dispatch_key`], the IME
    /// preedit/commit, ⌘V paste) — NOT ⌘C copy (not pty input, and it must
    /// not yank a scrolled selection out of view), key-up release reports, or
    /// bare-modifier reports (holding Shift is not typing).
    ///
    /// Notify discipline (r5c lever B): only an actual snap notifies — via the
    /// session handle's context, the same repaint path wheel scrolling uses —
    /// so the common parked-at-bottom case stays a pure pty write with no
    /// per-key redraw.
    ///
    /// [`dispatch_key`]: Self::dispatch_key
    fn snap_to_bottom_on_input(&mut self, cx: &mut Context<Self>) {
        self.handle.update(cx, |handle, hcx| {
            if !handle.is_at_bottom() {
                handle.scroll_to_bottom();
                hcx.notify();
            }
        });
    }

    /// Drive one keyboard-scrollback navigation on the viewport (Phase 0).
    /// Navigation, not typing: deliberately NO `snap_to_bottom_on_input` (same
    /// carve-out as ⌘C), and the repaint notifies through the session handle's
    /// context — the wheel path's discipline.
    fn perform_scrollback(&mut self, action: ScrollbackAction, cx: &mut Context<Self>) {
        self.handle.update(cx, |handle, hcx| {
            match action {
                ScrollbackAction::PageUp => handle.scroll_page_up(),
                ScrollbackAction::PageDown => handle.scroll_page_down(),
                ScrollbackAction::Top => handle.scroll_to_top(),
                ScrollbackAction::Bottom => handle.scroll_to_bottom(),
            }
            hcx.notify();
        });
    }

    /// Perform one copy-mode key action (Phase 3) against the session handle.
    ///
    /// Every arm is an app gesture — none of them touches the pty, and none
    /// snaps the viewport to the bottom (copy mode is *reading* scrollback; a
    /// snap would throw away the thing being read). `Swallow`/`SwallowPaste`
    /// deliberately do nothing at all: consuming the key IS the behaviour (P4).
    fn perform_copy_mode(&mut self, action: CopyModeAction, cx: &mut Context<Self>) {
        match action {
            CopyModeAction::Motion(motion) => self.handle.update(cx, |handle, hcx| {
                handle.vi_motion(motion);
                hcx.notify();
            }),
            CopyModeAction::Top => self.handle.update(cx, |handle, hcx| {
                handle.vi_top();
                hcx.notify();
            }),
            CopyModeAction::Bottom => self.handle.update(cx, |handle, hcx| {
                handle.vi_bottom();
                hcx.notify();
            }),
            CopyModeAction::Page { toward_history, half } => {
                self.handle.update(cx, |handle, hcx| {
                    handle.vi_page(toward_history, half);
                    hcx.notify();
                })
            }
            CopyModeAction::ToggleSelection(ty) => self.handle.update(cx, |handle, hcx| {
                handle.toggle_copy_selection(ty);
                hcx.notify();
            }),
            // `y` / Enter: copy-and-cancel. With nothing selected there is
            // nothing to copy, so the mode stays (P4) — `copy_selection` reports
            // exactly that, and it is the same clipboard write ⌘C uses.
            CopyModeAction::Yank => {
                if self.copy_selection(cx) {
                    self.handle.update(cx, |handle, hcx| {
                        handle.exit_copy_mode();
                        hcx.notify();
                    });
                }
            }
            // ⌘C: today's copy, unchanged — including staying in the mode.
            CopyModeAction::YankStay => {
                self.copy_selection(cx);
            }
            // The query field lives in the app crate (P2), so the view can only
            // ask for it. `begin_search` is the app's call as it opens the bar.
            CopyModeAction::OpenSearch { backward } => self.handle.update(cx, |_handle, hcx| {
                hcx.emit(TerminalEvent::SearchRequested { backward });
            }),
            CopyModeAction::NextMatch => self.handle.update(cx, |handle, hcx| {
                if handle.next_match() {
                    hcx.notify();
                }
            }),
            CopyModeAction::PrevMatch => self.handle.update(cx, |handle, hcx| {
                if handle.prev_match() {
                    hcx.notify();
                }
            }),
            CopyModeAction::Exit => self.handle.update(cx, |handle, hcx| {
                handle.exit_copy_mode();
                hcx.notify();
            }),
            // Nothing to do: the key is consumed by the caller either way.
            CopyModeAction::SwallowPaste | CopyModeAction::Swallow => {}
        }
    }

    /// gpui key-down: the terminal's typed-input entry point.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;

        // Copy mode (Phase 3): the FIRST gate — ahead of the held gate (P10:
        // keyboard-selecting a dead pane's output is that pane's whole remaining
        // purpose) and ahead of the IME gates. EVERY key is consumed here, bound
        // or not (the table's default arm is `Swallow`), so nothing reaches the
        // ⌘V/⌘C handling or `dispatch_key` while VI is on — P4's no-leak
        // guarantee. Nice's own ⌃⌘ chords never arrive: gpui matches actions
        // before view key listeners, which is why the mode toggles are actions
        // and the in-mode keys are intercepted here.
        if matches!(
            key_gate(self.copy_mode_active(cx), self.held.is_held()),
            KeyGate::CopyMode
        ) {
            self.perform_copy_mode(copy_mode_key_action(&keystroke.key, m), cx);
            cx.stop_propagation();
            return;
        }

        // Held pane (T10): the child is dead, so **pty-bound** input is inert — the
        // key is consumed (never reaching the encoder / a closed pty, and never
        // falling through to AppKit's unhandled-key beep). But app gestures that
        // never touch the pty stay live: the whole point of a held pane is reading
        // its failed output (T10), so ⌘C must still copy a mouse selection.
        // Without this the output is readable but not copyable, and this app has
        // no menu-bar Edit>Copy fallback (unlike the Swift app, where copy is
        // app-level and survives a held pane). No kitty ⌘C-forward gate here
        // (there is no live child to forward `ESC[99;9u` to); ⌘V is intentionally
        // left inert (nothing to paste into a dead shell).
        //
        // R12: the ⌘=/⌘−/⌘0 zoom chords are NO LONGER intercepted here — they are
        // app-level keyboard shortcuts now (`crate::keymap` in `crates/nice`),
        // matched by the keymap before this key listener ever runs. A held pane
        // still enlarges: the app action mutates the shared `FontSettings` this
        // view observes, which re-metrics it (the dead pty's resize error is just
        // dropped) without the keystroke reaching this handler at all.
        if self.held.is_held() {
            if m.platform && !m.control && !m.alt && keystroke.key == "c" && self.copy_selection(cx)
            {
                cx.stop_propagation();
                return;
            }
            // Phase 0: keyboard scrollback stays live on a held pane — same
            // class as ⌘C above (a read gesture over the dead child's output,
            // which is the pane's whole purpose; wheel scrolling already
            // works). The mode still gates alt-screen via the term's frozen
            // final state.
            if let Some(action) = scrollback_key_action(&keystroke.key, m, self.current_mode(cx)) {
                self.perform_scrollback(action, cx);
                cx.stop_propagation();
                return;
            }
            // The one non-gesture key a held pane honours: the dismiss affordance —
            // a bare Enter respawns a fresh shell (like clicking the pill), the only
            // path that frees the held term. `dismiss_held` issues its own
            // `cx.notify()` on success; every other consumed key changes nothing
            // paint reads, so none notifies (r5c lever B — see `dispatch_key`).
            if keystroke.key == "enter" && !m.control && !m.platform && !m.alt {
                self.dismiss_held(cx);
            }
            cx.stop_propagation();
            return;
        }

        // Read+clear the Enter-swallow at the START of every key cycle — only an
        // Enter/Tab re-dispatched in the SAME native cycle as a composition commit
        // (the `doCommandBySelector(insertNewline:)` path) observes `true`.
        let swallow = self.ime.take_commit_swallow();

        // (G1 item 2) An Enter/Tab that just confirmed a composition this cycle is
        // swallowed — no CR/HT reaches the pty (the commit already wrote the text).
        // No notify: the commit's visible effect was already painted by
        // `ime_commit`'s own notify; consuming this key changes nothing further.
        let commit_confirm_key =
            (keystroke.key == "enter" || keystroke.key == "tab") && !m.control && !m.platform;
        if swallow && commit_confirm_key {
            cx.stop_propagation();
            return;
        }

        // (G1 items 1 & 3) While composing, all key handling belongs to the IME
        // (preedit edits, candidate navigation, commit): the pty stays silent.
        // gpui routes keys to the IME because `marked_text_range` is `Some`, so a
        // key that still lands here must not encode anything. No notify either:
        // every preedit mutation arrives through the platform input handler
        // (`ime_set_marked` / `ime_commit` / `ime_unmark`), each of which
        // notifies itself — this handler mutated nothing paint reads.
        if self.ime.is_composing() {
            return;
        }

        // ⌘V paste / ⌘C copy — the macOS-standard editing shortcuts, handled
        // before the key encoder. ⌘V always pastes (bracketed-wrapped per the
        // core's DECSET-2004 state); ⌘C copies a live selection UNLESS the encoder
        // would actually forward ⌘C as `ESC[99;9u` — i.e. `kitty_forwards_super`
        // (DISAMBIGUATE / REPORT_ALL_KEYS). Gating on plain `kitty_active` would
        // strand ⌘C under e.g. REPORT_EVENT_TYPES-only: copy skipped AND the
        // encoder emitting nothing. (Under real kitty, the "⌘C doesn't copy" quirk
        // is Claude-side and deliberately not fixed here.)
        if m.platform && !m.control && !m.alt {
            if keystroke.key == "v" {
                // Paste is typed input: snap a scrolled-up viewport first.
                // Otherwise no notify: the paste is a pty write — its echo comes
                // back through damage → drain → throttled notify (see
                // `dispatch_key`).
                self.snap_to_bottom_on_input(cx);
                self.paste_clipboard(cx);
                cx.stop_propagation();
                return;
            }
            if keystroke.key == "c"
                && !kitty_forwards_super(self.current_mode(cx))
                && self.copy_selection(cx)
            {
                cx.stop_propagation();
                return;
            }
            // R12: ⌘=/⌘−/⌘0 zoom is no longer handled here — it is an app-level
            // keyboard shortcut (`crate::keymap` in `crates/nice`), matched by the
            // GPUI keymap before this key listener runs (dispatch order: actions →
            // key listeners → input handler). The action mutates the shared,
            // process-level `FontSettings` this view observes, so every open
            // window re-metrics; the keystroke never reaches this handler, so it
            // also never encodes to the pty. ⌘V/⌘C above stay LOCAL (they are not
            // in the rebindable shortcut table and depend on this view's selection
            // / kitty state).
        }

        let event_type = if event.is_held {
            KeyEventType::Repeat
        } else {
            KeyEventType::Press
        };
        self.dispatch_key(keystroke, event_type, cx);
    }

    /// gpui key-up: only relevant to the kitty event-type ladder (press/repeat/
    /// release). In legacy and plain-kitty modes releases encode to nothing.
    /// Pty-write only — no `cx.notify()`, same contract as [`dispatch_key`]
    /// (r5c lever B): the release report's echo (if the app paints anything)
    /// returns through the damage → drain → throttled-notify path.
    ///
    /// [`dispatch_key`]: Self::dispatch_key
    fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Copy mode (P4, gate 2): the press was swallowed, so its release must be
        // too — otherwise an app that asked for `REPORT_EVENT_TYPES` would still
        // see the release half of every in-mode keystroke. The one asymmetry this
        // accepts is a press/release pair that straddles entry or exit (the Esc
        // that leaves the mode releases with VI already off), which is the same
        // class as today's intercepted Shift+PageUp.
        if self.copy_mode_active(cx) {
            return;
        }
        if self.ime.is_composing() {
            return;
        }
        let mode = self.current_mode(cx);
        if !mode.contains(TermMode::REPORT_EVENT_TYPES) {
            return; // the app did not ask for release reporting
        }
        let keycode = self.keycode_probe.as_ref().and_then(|probe| probe());
        let Some(input) = build_key_input(&event.keystroke, KeyEventType::Release, keycode, false)
        else {
            return;
        };
        if let Some(bytes) = encoder_config(mode, false).encode(&input) {
            self.write_pty(&bytes, cx);
        }
    }

    /// Decide whether a (non-composing) keystroke is terminal-owned — encode it
    /// and write the bytes, consuming the event — or should fall through to the
    /// platform IME / app keybindings.
    fn dispatch_key(&mut self, keystroke: &Keystroke, event: KeyEventType, cx: &mut Context<Self>) {
        let mode = self.current_mode(cx);
        let m = keystroke.modifiers;

        // Phase 0 keyboard scrollback: Shift+PageUp/PageDown/Home/End drive the
        // viewport through scrollback instead of encoding (on the alternate
        // screen the policy declines and the keys encode to the TUI as before).
        // Applies to Press AND Repeat, so holding Shift+PageUp keeps paging.
        // The held-pane gate in `on_key_down` performs the same interception
        // for a dead child (keys never reach here in that state).
        if let Some(action) = scrollback_key_action(&keystroke.key, m, mode) {
            self.perform_scrollback(action, cx);
            cx.stop_propagation();
            return;
        }

        // ⌥-as-Meta: gpui does not report which Option key is held, so the
        // per-side policy is resolved best-effort (Both/Off are side-independent;
        // Left/RightOnly assume the left key — a settings-UI-era refinement).
        let alt_meta = m.alt && self.option_as_meta.treats_as_meta(OptionSide::Left);

        let named = named_key_for(&keystroke.key).is_some();
        let should_encode = if named {
            // Functional keys (arrows, F-keys, Enter/Tab/Backspace/…) never reach
            // the IME — always terminal input.
            true
        } else {
            let report_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
            (m.control && !m.platform)   // ctrl chords are terminal input in every mode
                || (m.platform && kitty_forwards_super(mode)) // ⌘ only when it'd be CSI-u-forwarded
                || alt_meta               // ⌥-as-Meta prefixes ESC and bypasses the IME
                || report_all             // full kitty encodes every key as CSI-u
        };

        if !should_encode {
            // Plain / shift printable, a ⌘ shortcut in legacy mode, or an
            // ⌥-composing key: let it propagate to the platform (NSTextInputClient
            // → IME compose or `insertText` commit) or to app keybindings. The pty
            // is not touched here — a printable that commits snaps at `ime_commit`
            // instead, and an app shortcut must not snap at all.
            return;
        }

        // Terminal-owned typed input: snap a scrolled-up viewport back to the
        // bottom. Deliberately not gated on the encoder producing bytes — a
        // legacy modified-key *repeat* encodes nothing but is still typing.
        self.snap_to_bottom_on_input(cx);

        let keycode = self.keycode_probe.as_ref().and_then(|probe| probe());
        let Some(mut input) = build_key_input(keystroke, event, keycode, false) else {
            return;
        };
        // For a Meta chord the inserted text is the OS-composed glyph (e.g. ⌥a →
        // "å"); Meta must send `ESC` + the *base* key, so drop that text and let
        // the encoder use the base char.
        if alt_meta {
            input.text = None;
        }
        if let Some(bytes) = encoder_config(mode, false).encode(&input) {
            self.write_pty(&bytes, cx);
        }
        // Terminal-owned: consume it even when the encoder produced nothing (a
        // legacy modified-key *repeat*, which encodes only on the initial press).
        // Consuming still is deliberate — letting such a key propagate would reach
        // AppKit's unhandled-key path and beep. Chords that *should* yield bytes
        // do (e.g. Ctrl+Shift+C degrades to 0x03 in `legacy_char_sequence`).
        //
        // Deliberately NO `cx.notify()` (fix round r5c, lever B). A keystroke's
        // only effect here is the pty write, which paint cannot see — the echo
        // mutates the grid via the feeder and comes back through the
        // damage → drain → throttled-notify path (r5 lever 2), which presents
        // it. Notifying here instead re-dirtied the window on EVERY key, and
        // gpui's `dispatch_key_event` force-draws a dirty window before
        // dispatching each key (vendor/zed/crates/gpui/src/window.rs:4724 — it
        // needs a fresh dispatch tree), so every keystroke paid a full
        // immediate-mode draw ON TOP of the echo's own throttled frame: the
        // 2026-07-10 5 s sample during a 120 cps typing flood counted ~335
        // main-thread samples inside that pre-dispatch `Window::draw`. Nothing
        // else in this handler mutates visual state: the one key side effect
        // that can change what paint reads — the snap-to-bottom above — notifies
        // at its own site (via the session handle, like wheel scrolling) and
        // only when it actually moves the viewport, composing/preedit
        // transitions happen only in the input-handler callbacks (which notify
        // themselves), and the caret/focus visuals are driven by focus +
        // window-activation edges, not keystrokes. Any future key side effect
        // that DOES change what paint reads must notify at its own site, like
        // `dismiss_held` and the snap do.
        cx.stop_propagation();
    }

    /// gpui flagsChanged: a bare modifier key (Shift/Ctrl/Alt/⌘) went down or up.
    /// This is the kitty **modifiers-as-functional-keys** report — under
    /// REPORT_ALL_KEYS the app sees each bare modifier as `CSI 57441 u` (left
    /// shift) etc., press and (with event reporting) release. Every other mode
    /// ignores it. The specific left/right key comes from the flagsChanged keyCode
    /// side-channel; press vs release is computed from the new aggregate modifier
    /// state (see [`build_modifier_input`]). While composing, the encoder still
    /// reports bare modifiers (kitty's composition rule) — the composing flag is
    /// threaded through so it can. Pty-write only — no `cx.notify()`, same
    /// contract as [`dispatch_key`](Self::dispatch_key) (r5c lever B).
    ///
    /// It is also the ⌘-hover edge: ⌘ down over a URL underlines it without the
    /// pointer moving, ⌘ up clears the underline. That runs FIRST, because the
    /// bare-modifier report below returns early in every mode but kitty's — the
    /// affordance must not depend on which app is running.
    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // ⌘ pressed (or released) with the pointer somewhere in this pane:
        // recompute from where it actually is. No position remembered means the
        // pointer is outside the pane — and the exit already cleared any hover
        // (the `on_hover(false)` listener drops both together), so there is
        // nothing to do for that side.
        if let Some(pos) = self.last_mouse_pos {
            self.update_hover(pos, event.modifiers, cx);
        }

        // Copy mode: P4's no-leak guarantee reaches this writer too. The plan
        // enumerates the four gates around `on_key_down`; bare-modifier reports
        // are the fifth pty writer a keystroke can reach, and Shift is held
        // constantly inside the mode (`V`, `G`, `?`, `N`), so under
        // report-all-keys every one of those would otherwise report to the app.
        // The ⌘-hover edge above stays live — it writes nothing.
        if self.copy_mode_active(cx) {
            return;
        }

        let mode = self.current_mode(cx);
        if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
            return; // bare-modifier reports are a report-all-keys feature only
        }
        let Some(keycode) = self.keycode_probe.as_ref().and_then(|probe| probe()) else {
            return; // no keyCode side-channel -> cannot resolve which modifier key
        };
        let composing = self.ime.is_composing();
        let Some(input) = build_modifier_input(keycode, &event.modifiers, composing) else {
            return; // not a bare modifier key (or unmapped keyCode)
        };
        if let Some(bytes) = encoder_config(mode, false).encode(&input) {
            self.write_pty(&bytes, cx);
        }
    }

    // -- mouse, selection, paste/copy, focus reporting (slice 3) ---------------

    /// Hit-test a window pixel position to a grid cell, using the bounds the
    /// element published on its last paint (`paint_bounds`). Returns viewport +
    /// buffer coordinates, or `None` before the first paint / spawn.
    fn hit_cell(&self, pos: Point<Pixels>, cx: &App) -> Option<Hit> {
        let bounds = self.paint_bounds.get()?;
        let term_arc = self.handle.read(cx).term()?;
        let (rows, cols, display_offset) = {
            let term = term_arc.lock();
            (
                term.screen_lines(),
                term.columns(),
                term.grid().display_offset(),
            )
        };
        let grid_top = grid_top_y(bounds);
        let rel_x = f32::from(pos.x) - f32::from(bounds.origin.x);
        let rel_y = f32::from(pos.y) - grid_top;
        let (col, vrow) = mouse::cell_from_offset(
            rel_x,
            rel_y,
            self.effective_metrics.cell_w,
            self.effective_metrics.cell_h,
            cols,
            rows,
        );
        Some(Hit {
            col,
            vrow,
            buffer_line: vrow as i32 - display_offset as i32,
        })
    }

    /// The URL under `hit` and its match range, if any (see
    /// [`TerminalSessionHandle::hyperlink_at`]). Callers must gate this on ⌘
    /// being held: it locks the `Term` and runs a regex scan, so it must never
    /// run on ordinary pointer traffic.
    fn hyperlink_at(&self, hit: &Hit, cx: &App) -> Option<(String, Match)> {
        let handle = self.handle.read(cx);
        self.url_regex
            .with(|regex| handle.hyperlink_at(hit.buffer_line, hit.col, regex))
            .flatten()
    }

    /// Recompute the ⌘-hover from a pointer position: the link under it while ⌘
    /// is held, nothing otherwise. Called by every mouse-move and by the ⌘
    /// press/release edge (which has no position of its own — hence
    /// `last_mouse_pos`).
    ///
    /// The `!platform` fast path is what keeps ordinary pointer traffic free:
    /// with ⌘ up and no hover live this returns before hit-testing, so no `Term`
    /// lock and no regex scan happen on a plain mouse-move.
    fn update_hover(
        &mut self,
        pos: Point<Pixels>,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if !modifiers.platform && self.hovered_hyperlink.is_none() {
            return;
        }
        let found = modifiers
            .platform
            .then(|| {
                self.hit_cell(pos, cx)
                    .and_then(|hit| self.hyperlink_at(&hit, cx))
            })
            .flatten();
        self.set_hovered_hyperlink(found, cx);
    }

    /// Store the hover state, notifying **only when the underlined range
    /// changes**. The notify is mandatory on a real change (the underline rides
    /// the element's [`SnapshotKey`], so paint must re-run to add or drop it) and
    /// must be skipped otherwise — the rule itself is the pure, unit-tested
    /// [`hover_changed`].
    fn set_hovered_hyperlink(&mut self, next: Option<(String, Match)>, cx: &mut Context<Self>) {
        let changed = hover_changed(&self.hovered_hyperlink, &next);
        self.hovered_hyperlink = next;
        if changed {
            cx.notify();
        }
    }

    /// Open `url` with the injected [`UrlOpener`], or gpui's own `open_url` when
    /// the app never wired one (see [`set_url_opener`](Self::set_url_opener)).
    fn open_url(&self, url: &str, cx: &App) {
        match &self.url_opener {
            Some(open) => open(url),
            None => cx.open_url(url),
        }
    }

    /// Encode + write one VT mouse report for `action` on `button` at `hit`.
    fn send_mouse_report(
        &self,
        mode: TermMode,
        button: VtButton,
        action: MouseAction,
        hit: &Hit,
        m: gpui::Modifiers,
        cx: &App,
    ) {
        let input = MouseInput {
            button,
            action,
            col: hit.col,
            line: hit.vrow,
            modifiers: mouse::report_modifiers(m),
        };
        if let Some(bytes) = encode_mouse(mouse::protocol(mode), &input) {
            self.write_pty(&bytes, cx);
        }
    }

    /// gpui mouse-down: a VT press report (app reporting, no Shift override) or
    /// the start of a local selection drag.
    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Clicking a terminal always grabs key focus (standard terminal
        // behaviour), independent of whatever the click does below. We cannot
        // lean on gpui's `track_focus` mouse-down auto-focus here: that listener
        // runs *after* this one in the reversed bubble order, so the
        // `cx.stop_propagation()` on the app-mouse-reporting path (Claude Code,
        // vim, …) would suppress it and leave a terminal that had lost focus —
        // e.g. to the file browser — unable to regain it on click. Focus first,
        // explicitly, before any early return.
        window.focus(&self.focus_handle, cx);

        let mode = self.current_mode(cx);
        let m = event.modifiers;

        // ⌘+left-press over a URL arms a link click and consumes the press: no
        // selection starts and NO press report is sent, so ⌘+click keeps working
        // inside an app that grabbed the mouse (Claude Code, vim) exactly as it
        // does in Ghostty. The open itself waits for the matching release (see
        // `on_mouse_up`). A ⌘+press that is not over a link arms nothing and
        // falls through to the unchanged behaviour below.
        if event.button == MouseButton::Left && m.platform {
            if let Some(hit) = self.hit_cell(event.position, cx) {
                if self.hyperlink_at(&hit, cx).is_some() {
                    self.link_click_armed = Some((hit.vrow, hit.col));
                    cx.stop_propagation();
                    return;
                }
            }
        }

        // App mouse reporting, unless Shift — or copy mode (P10) — forces the
        // local branch below.
        let copy_mode = self.copy_mode_active(cx);
        if mouse_reports_to_app(mode, m.shift, copy_mode) {
            if let (Some(button), Some(hit)) =
                (mouse::vt_button(event.button), self.hit_cell(event.position, cx))
            {
                self.send_mouse_report(mode, button, MouseAction::Press, &hit, m, cx);
                self.last_report_cell = Some((hit.col, hit.vrow));
            }
            cx.stop_propagation();
            return;
        }

        // Local selection: only the left button starts one. The click count picks
        // the granularity — single-click collapses any prior selection and arms a
        // cell-wise drag from this anchor; double-click selects the word under
        // the pointer (alacritty `Semantic`, expanded by the Term's
        // semantic-escape chars); triple-click selects the whole line. A drag
        // that follows a multi-click extends by that same granularity.
        if event.button == MouseButton::Left {
            if let Some(hit) = self.hit_cell(event.position, cx) {
                let kind = match event.click_count {
                    0 | 1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                self.drag_selecting = true;
                // In copy mode the click also MOVES the vi cursor there (P10).
                // `scroll_display` recomputes a live selection's end to the vi
                // cursor while VI is on, so without this a later wheel scroll
                // would drag the mouse selection back to wherever the cursor was
                // parked; pointing the cursor at the click makes that recompute
                // target where the drag already is. The point is in view by
                // construction, so `vi_goto` scrolls nothing.
                if copy_mode {
                    self.handle.update(cx, |handle, _| {
                        handle.vi_goto(GridPoint::new(Line(hit.buffer_line), Column(hit.col)));
                    });
                }
                // The Term owns the anchor from here (content-locked; see the
                // `drag_selecting` field docs). A `Simple` selection starts
                // empty, so a single click still collapses any prior highlight
                // without a separate clear; `Semantic`/`Lines` paint the
                // word/line at once via `to_range` expansion.
                self.handle
                    .read(cx)
                    .start_selection(kind, (hit.buffer_line, hit.col));
                cx.notify();
            }
        }
    }

    /// gpui mouse-move: track the ⌘-hover, extend an active local selection, or
    /// emit a motion report.
    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Remembered for the ⌘ press/release edge, which carries no position.
        self.last_mouse_pos = Some(event.position);

        if self.drag_selecting {
            // The button was released (possibly outside the pane, so no mouse-up
            // reached us) — stop extending the selection.
            if event.pressed_button != Some(MouseButton::Left) {
                self.drag_selecting = false;
                return;
            }
            if let Some(hit) = self.hit_cell(event.position, cx) {
                // Screen-locked end: `hit.buffer_line` is the pointer resolved
                // against the current display offset. The anchor needs no
                // algebra — it lives in the Term, content-locked. A `false`
                // (the Term dropped the selection mid-drag) is a no-op: the
                // gesture idles until release but keeps owning the button
                // events (see the `drag_selecting` field docs).
                if self.handle.read(cx).extend_selection((hit.buffer_line, hit.col)) {
                    cx.notify();
                }
            }
            return;
        }

        // ⌘-hover, updated only outside a drag (the branch above returns): the
        // underline is an affordance for a click that is not already happening.
        self.update_hover(event.position, event.modifiers, cx);

        // Hover is passive — motion reports are NOT suppressed while ⌘ is held,
        // so an app that tracks the pointer keeps tracking it under ⌘.
        let mode = self.current_mode(cx);
        if !mouse_reports_to_app(mode, event.modifiers.shift, self.copy_mode_active(cx)) {
            return;
        }
        if !mouse::reports_motion(mode, event.pressed_button.is_some()) {
            return;
        }
        if let Some(hit) = self.hit_cell(event.position, cx) {
            // One report per cell crossed, not per pixel of travel.
            if self.last_report_cell == Some((hit.col, hit.vrow)) {
                return;
            }
            self.last_report_cell = Some((hit.col, hit.vrow));
            let button = event
                .pressed_button
                .and_then(mouse::vt_button)
                .unwrap_or(VtButton::None);
            self.send_mouse_report(mode, button, MouseAction::Motion, &hit, event.modifiers, cx);
        }
    }

    /// gpui mouse-up: end a local selection drag (keeping the selection) or emit a
    /// release report.
    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // The release of an armed ⌘+link click (see `on_mouse_down`). It opens
        // only if the pointer is still on the pressed cell, ⌘ is still held, and
        // a URL is still there — a ⌘+drag away, a released ⌘, or output that
        // scrolled the link out all cancel harmlessly. Either way the up is
        // consumed: an armed click sent no press report, so it must not send a
        // release report either. The decision itself is
        // [`mouse::link_click_verdict`] (pure, unit-tested); only the lookup
        // and the open live here. Hit-testing is skipped entirely when nothing is
        // armed, which is every ordinary release.
        let armed_hit = self
            .link_click_armed
            .is_some()
            .then(|| self.hit_cell(event.position, cx))
            .flatten();
        match link_click_verdict(
            self.link_click_armed,
            event.button,
            event.modifiers.platform,
            armed_hit.as_ref().map(|hit| (hit.vrow, hit.col)),
        ) {
            LinkClickVerdict::Open => {
                self.link_click_armed = None;
                if let Some((url, _range)) =
                    armed_hit.as_ref().and_then(|hit| self.hyperlink_at(hit, cx))
                {
                    self.open_url(&url, cx);
                }
                cx.stop_propagation();
                return;
            }
            LinkClickVerdict::Cancel => {
                self.link_click_armed = None;
                cx.stop_propagation();
                return;
            }
            // Not an armed left-release — fall through unchanged.
            LinkClickVerdict::NotOurs => {}
        }

        if self.drag_selecting && event.button == MouseButton::Left {
            // Selection persists (for ⌘C); nothing is sent to the pty.
            self.drag_selecting = false;
            return;
        }
        let mode = self.current_mode(cx);
        if !mouse_reports_to_app(mode, event.modifiers.shift, self.copy_mode_active(cx)) {
            return;
        }
        if let (Some(button), Some(hit)) =
            (mouse::vt_button(event.button), self.hit_cell(event.position, cx))
        {
            self.send_mouse_report(mode, button, MouseAction::Release, &hit, event.modifiers, cx);
        }
        // Consume the up while the app is reporting, matching the press.
        cx.stop_propagation();
    }

    /// A left button-up that landed outside the pane still ends a drag cleanly
    /// (the in-bounds `on_mouse_up` never fired for it) — and disarms a ⌘+link
    /// click for the same reason: a release outside the pane is never on the
    /// pressed cell, so it can only cancel.
    fn on_mouse_up_out(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.drag_selecting = false;
        self.link_click_armed = None;
    }

    /// ⌘V: paste the clipboard, bracketed-wrapped when the app enabled DECSET
    /// 2004 (`bracketed_paste_active`), else passed through raw. R7's drag-drop
    /// reuses this same wrap seam.
    fn paste_clipboard(&self, cx: &App) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let active = self.handle.read(cx).session().bracketed_paste_active();
        let bytes = wrap_bracketed_paste(text.as_bytes(), active);
        self.write_pty(&bytes, cx);
    }

    /// ⌘C: copy a live selection to the pasteboard. Returns `true` iff there was
    /// a non-empty selection to copy (the caller then consumes the key).
    fn copy_selection(&self, cx: &App) -> bool {
        match self.handle.read(cx).selection_text() {
            Some(text) if !text.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            _ => false,
        }
    }

    /// Emit a DECSET-1004 focus report when the combined focus predicate changes.
    /// Called from `render` with the same value the caret uses. Edge-triggered:
    /// the first call seeds the state without emitting (so startup never sends a
    /// spurious `ESC[I`); later transitions send `ESC[I` (gained) / `ESC[O`
    /// (lost) when the app has focus reporting enabled.
    fn report_focus_change(&mut self, focused: bool, cx: &App) {
        if self.last_focus_reported == Some(focused) {
            return;
        }
        let seed = self.last_focus_reported.is_none();
        self.last_focus_reported = Some(focused);
        if seed {
            return;
        }
        if self.current_mode(cx).contains(TermMode::FOCUS_IN_OUT) {
            let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
            self.write_pty(seq, cx);
        }
    }

    // -- IME adapter (called by `TermInputHandler`) ----------------------------

    /// `selectedRange` — always a valid (possibly collapsed) range.
    pub(crate) fn ime_selected_range_utf16(&self) -> Range<usize> {
        self.ime.selected_range_utf16()
    }

    /// `markedRange` — `Some` iff composing (what routes keys to the IME first).
    pub(crate) fn ime_marked_range_utf16(&self) -> Option<Range<usize>> {
        self.ime.marked_range_utf16()
    }

    /// `attributedSubstringForProposedRange` — clamped preedit substring + range.
    pub(crate) fn ime_text_for_range(&self, range: Range<usize>) -> Option<(String, Range<usize>)> {
        self.ime.text_for_range_utf16(range)
    }

    /// `setMarkedText:` — update the preedit (no pty write) and repaint.
    ///
    /// Copy mode declines the whole callback ([`ime_gate`]): the composition is
    /// never learned, so `is_composing` never arms and no preedit paints over
    /// the scrollback the user is reading.
    pub(crate) fn ime_set_marked(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        sel: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let gate = ime_gate(ImeCallback::SetMarked, self.copy_mode_active(cx));
        if !gate.run_transition {
            return;
        }
        // Starting/updating a composition is typing: snap so the preedit
        // overlay (anchored at the grid cursor) is actually on screen.
        if gate.snap_to_bottom {
            self.snap_to_bottom_on_input(cx);
        }
        self.ime.set_marked_text(range, text, sel);
        cx.notify();
    }

    /// `insertText:` — commit. Committed IME text is **data**: write it straight
    /// to the pty (never through the key encoder). If it ended a composition,
    /// schedule the end-of-cycle disarm so a later bare Enter still sends CR.
    ///
    /// In copy mode the write and the snap are dropped but the transition still
    /// RUNS ([`ime_gate`]): a composition already in flight when `⌃⌘c` fired has
    /// to be able to clear itself here, or marked state would stay `Some` and
    /// gpui would keep routing every key into the input context — leaving the
    /// pane keyboard-dead after the mode exits.
    pub(crate) fn ime_commit(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let gate = ime_gate(ImeCallback::Commit, self.copy_mode_active(cx));
        let outcome = self.ime.commit_text(range, text);
        // Plain printables arrive here (via `insertText:`), not `dispatch_key` —
        // this is the snap-to-bottom site for ordinary typing.
        if gate.snap_to_bottom {
            self.snap_to_bottom_on_input(cx);
        }
        if gate.write_pty {
            self.write_pty(outcome.pty_text.as_bytes(), cx);
        }
        if outcome.was_composing {
            // End-of-native-key-cycle disarm: runs after any synchronous
            // `doCommandBySelector` re-dispatch, before the next keypress, so a
            // commit with no same-cycle Enter re-dispatch (e.g. Pinyin
            // Space-commit) cannot swallow a LATER bare Enter.
            cx.spawn(async move |this, cx| {
                this.update(cx, |view, _| view.ime.disarm_commit_swallow())
                    .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// `unmarkText` — accept the pending composition as typed (focus loss /
    /// input-source switch). Does not arm the Enter swallow.
    ///
    /// In copy mode the pending text is discarded instead of written, but the
    /// transition still runs so the preedit clears (same reasoning as
    /// [`ime_commit`](Self::ime_commit)).
    pub(crate) fn ime_unmark(&mut self, cx: &mut Context<Self>) {
        let gate = ime_gate(ImeCallback::Unmark, self.copy_mode_active(cx));
        if let Some(pending) = self.ime.unmark() {
            if gate.write_pty {
                self.write_pty(pending.as_bytes(), cx);
            }
        }
        cx.notify();
    }

    /// `firstRectForCharacterRange` — the candidate-window anchor. **Never `None`**
    /// (the zed#46055 fix): always a rect at the grid cursor cell, in window px.
    /// For a sub-range query while composing it advances into the rendered preedit
    /// overlay (Terminal.app parity), so a multi-clause candidate list tracks the
    /// caret. `element_bounds` is the grid element's bounds this frame.
    pub(crate) fn ime_anchor_bounds(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Bounds<Pixels> {
        let m = self.effective_metrics;
        // The grid cursor cell in viewport coordinates (row honours the scroll/
        // display offset, clamped on-screen). A full-screen TUI that parks or
        // hides the hardware cursor still has a grid cursor point, so this is
        // total by construction — there is no path that yields "no cursor".
        let cursor = self.handle.read(cx).term().map(|term_arc| {
            let term = term_arc.lock();
            let content = term.renderable_content();
            let display_offset = content.display_offset as i32;
            let screen_rows = term.screen_lines();
            let cols = term.columns();
            let cp = content.cursor.point;
            let vr = (cp.line.0 + display_offset).clamp(0, screen_rows.saturating_sub(1) as i32);
            (vr as usize, cp.column.0.min(cols.saturating_sub(1)))
        });
        let (row, col) = cursor.unwrap_or((0, 0));
        let grid_top = grid_top_y(element_bounds);
        let mut x = f32::from(element_bounds.origin.x) + col as f32 * m.cell_w;
        let y = grid_top + row as f32 * m.cell_h;

        // Sub-range queries anchor within the rendered preedit overlay; range
        // start 0 (or idle) is exactly the cursor cell.
        if self.ime.is_composing() && range_utf16.start > 0 {
            let preedit = self.ime.preedit().to_string();
            let byte = utf16_to_byte(&preedit, range_utf16.start);
            let run = TextRun {
                len: preedit.len(),
                font: term_font(self.font_family.clone()),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(preedit),
                px(self.font_px),
                &[run],
                None,
            );
            x += f32::from(shaped.x_for_index(byte));
        }

        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(m.cell_w), px(m.cell_h)),
        }
    }
}

/// A plain monospace [`Font`] for the given family (preedit shaping / anchor
/// measurement). Attributes are irrelevant to the metrics the anchor needs.
fn term_font(family: SharedString) -> Font {
    Font {
        family,
        features: FontFeatures::default(),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
        fallbacks: None,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus-once (M2 Item D): grab key focus on this view's FIRST render
        // only, so a fresh pane starts focused without app wiring. The grab
        // never recurs — an inline-rename field or context menu that takes
        // focus keeps it (the pre-M2 per-frame grab yanked it back the next
        // frame, killing rename typing). Later moves are explicit: the app's
        // focus routing calls [`TerminalView::focus`], and a click on the view
        // re-focuses it via the explicit `window.focus` in `on_mouse_down`
        // (gpui's tracked-focus mouse-down transfer can't be relied on — the
        // app-mouse-reporting path's `stop_propagation` suppresses it).
        // The grab is skipped entirely for a pane the host mounted un-focused
        // (`set_focus_on_first_render(false)`) — with several panes mounting in
        // one pass, the last-rendered one would otherwise win the focus race.
        if !self.focused_once {
            self.focused_once = true;
            if self.focus_on_first_render {
                window.focus(&self.focus_handle, cx);
            }
        }

        // Re-snap the cell box to THIS window's backing scale when it changes
        // (first render, or the window migrated displays — 2× Retina ↔ 1×
        // external): the painted grid pitch must be whole device px on the
        // live display, or the box-drawing sprite grid drifts off the exact
        // text grid (up to 0.5 logical px PER COLUMN at 1×). A real change
        // re-fits the pty too — the new cell box tiles the same window into a
        // different (rows, cols).
        let scale = window.scale_factor();
        if scale != self.display_scale {
            self.display_scale = scale;
            let snapped = snap_metrics_to_scale(self.metrics, scale);
            if snapped != self.effective_metrics {
                self.effective_metrics = snapped;
                if self.auto_refit {
                    self.schedule_refit(cx);
                }
            }
        }

        let caret_solid = self.focus_handle.is_focused(window) && window.is_window_active();

        // DECSET-1004 focus in/out rides the same predicate as the caret: emit a
        // report on the change edge (window activation calls `refresh()`, so this
        // render re-runs when activation flips, not just on focus-handle changes).
        self.report_focus_change(caret_solid, cx);

        // Arm the T9 launch-overlay grace deadline once, on the first paint of a
        // still-pending pane. It cannot be armed at construction (the App-Nap-safe
        // factory is injected afterwards, like the keyCode probe), and the silent
        // pane it exists for produces no other wake — so this self-driving deadline
        // is what promotes the overlay to visible.
        if !self.overlay_armed && self.overlay.is_pending() && !self.held.is_held() {
            self.arm_overlay_deadline(cx);
        }

        // T9/T10 overlays, built before the div chain (they read `self` + register
        // a listener via `cx`), painted as children ON TOP of the terminal element.
        let show_overlay = self.overlay.is_visible();
        let launch_overlay = show_overlay.then(|| self.render_launch_overlay());
        let show_held = self.held.is_held();
        let held_affordance = show_held.then(|| self.render_held_affordance(cx));

        // The copy-mode badge (P9): the mode has no other standing signal — the
        // grid still looks live, the caret has just stopped following the shell
        // — so a pane in copy mode says so, and names the search whose matches
        // are tinting its cells.
        let copy_badge = self.copy_mode_active(cx).then(|| self.render_copy_badge(cx));

        // Snapshot the preedit for this frame's inline overlay (byte range for the
        // shaped runs). The IME wiring (input-handler registration + preedit
        // paint) is threaded into the element so it shares the grid geometry.
        let preedit = if self.ime.is_composing() {
            let text = self.ime.preedit().to_string();
            let sel16 = self.ime.selected_range_utf16();
            let sel_bytes =
                utf16_to_byte(&text, sel16.start)..utf16_to_byte(&text, sel16.end);
            Some((SharedString::from(text), sel_bytes))
        } else {
            None
        };
        let ime = ImeInput {
            focus_handle: self.focus_handle.clone(),
            view: cx.entity(),
            preedit,
        };

        // Native full screen forces the grid opaque: macOS draws a plain black
        // backdrop (not the wallpaper) behind a full-screen Space, so at < 1.0
        // the skip-own-fill rule would reveal a black-composited backing instead
        // of the desktop. The enter/exit resize redraws the tree, so this
        // reverts to the stored translucency on exit without an observer.
        let background_opacity = if window.is_fullscreen() {
            1.0
        } else {
            self.background_opacity
        };
        let element = TerminalElement::new(
            self.handle.read(cx),
            &self.theme,
            self.accent,
            self.font_family.clone(),
            self.font_px,
            self.effective_metrics,
            caret_solid,
            ime,
            self.paint_bounds.clone(),
            self.auto_refit,
            self.grid_cache.clone(),
            background_opacity,
            // The ⌘-hover underline: only the range travels to paint (the URL
            // text is the click's business), and it rides the element's
            // snapshot key, so hover on/off invalidates the row cache.
            self.hovered_hyperlink.as_ref().map(|(_, m)| m.clone()),
        );

        div()
            .track_focus(&self.focus_handle)
            .id("terminal.grid")
            .size_full()
            // Text-style I-beam pointer over the grid (standard terminal
            // behaviour, matching Terminal.app / iTerm even while the app has
            // mouse reporting on) — except over a ⌘-hovered link, where the
            // hand is the other half of the affordance the underline starts.
            .cursor(if self.hovered_hyperlink.is_some() {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            })
            // The pointer leaving the pane stops mouse-moves, so the hover would
            // otherwise stick underlined with ⌘ still held (and the remembered
            // position would go stale — see `last_mouse_pos`).
            .on_hover(cx.listener(|this, hovering: &bool, _window, cx| {
                if !*hovering {
                    this.last_mouse_pos = None;
                    this.set_hovered_hyperlink(None, cx);
                }
            }))
            // File / image drag-drop (T7): a dropped set of file URLs (or a
            // raw-image fallback) is typed as escaped paths at the prompt. gpui
            // delivers an OS file drop as an `ExternalPaths` active-drag.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                // Dropping files onto the terminal grabs key focus so the user
                // can type immediately. The drag originates in the sidebar file
                // browser (which parks focus in itself on the initiating click),
                // and a drop is not a mouse-down, so neither `track_focus` nor
                // the `on_mouse_down` focus grab fires — focus explicitly here.
                window.focus(&this.focus_handle, cx);
                this.handle_external_paths_drop(paths, cx);
            }))
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .child(element)
            // T9 "Launching…" overlay + T10 held-pane dismiss affordance, painted
            // over the grid when active (children paint after the element).
            .when_some(launch_overlay, |root, overlay| root.child(overlay))
            .when_some(held_affordance, |root, pill| root.child(pill))
            .when_some(copy_badge, |root, badge| root.child(badge))
    }
}

impl TerminalView {
    /// The accent as a gpui [`Rgba`] — the "Launching…" status-dot / held-pill
    /// tint. Uses the raw R2 accent (not the theme cursor override) so the
    /// `niceties-overlay` self-test can key its pixel probe on the known preset.
    fn accent_rgba(&self) -> Rgba {
        Rgba {
            r: self.accent.r,
            g: self.accent.g,
            b: self.accent.b,
            a: 1.0,
        }
    }

    /// Chrome slots derived from this view's terminal theme (the app's
    /// `derive_chrome` idiom) — the overlays' card colors. This crate never
    /// observes the app's `SharedThemeState`; deriving from the theme it was
    /// handed keeps the overlay consistent with the chrome the app derives from
    /// the same theme.
    fn overlay_slots(&self) -> nice_theme::Slots {
        let to_srgba = |c: crate::theme::TerminalColor| {
            Srgba::rgb(
                c.r as f32 / 255.0,
                c.g as f32 / 255.0,
                c.b as f32 / 255.0,
            )
        };
        nice_theme::derive_chrome(to_srgba(self.theme.foreground), to_srgba(self.theme.background))
    }

    /// The centred "Launching…" overlay (T9): a status dot + title, plus the
    /// dimmed command line when the app set one. Non-interactive (no listeners),
    /// so mouse events pass through to the terminal below. The dot sits on the
    /// window's vertical centre line (a single centred flex row), which the
    /// self-test's pixel probe keys on. Text renders in the terminal font at the
    /// chrome point sizes (the restyle look — the old Helvetica was Swift
    /// parity, retired).
    fn render_launch_overlay(&self) -> impl IntoElement {
        let ink = self.theme.foreground.to_u32();
        // A dimmed subtitle colour: the theme's bright-black (ANSI 8), a muted grey
        // (mirrors the chrome's `ink3` under the command line).
        let ink3 = self.theme.ansi[8].to_u32();
        let title: SharedString = match &self.overlay_command {
            Some(cmd) => format!("Launching {cmd}…").into(),
            None => "Launching…".into(),
        };

        let dot = div()
            .w(px(11.0))
            .h(px(11.0))
            .rounded(px(5.5))
            .bg(self.accent_rgba());
        let heading = div()
            .flex()
            .items_center()
            .child(dot)
            .child(div().w(px(8.0)))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(ink))
                    .child(title),
            );

        let card = div()
            .flex()
            .flex_col()
            .items_center()
            .font_family(self.font_family.clone())
            .child(heading)
            .when_some(self.overlay_command.clone(), |card, cmd| {
                card.child(div().h(px(6.0))).child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(ink3))
                        .child(cmd),
                )
            });

        // Fill the view and centre the card. An `.absolute()` element must be
        // sized by explicit insets — `.size_full()` (percentage size) resolves to
        // ZERO on an absolutely-positioned element in gpui/taffy, so `inset: 0`
        // (all four sides) is what stretches it over the terminal.
        overlay_fill().flex().items_center().justify_center().child(card)
    }

    /// The NEW single-pane-era dismiss affordance (T10) — a minimal Stage-2
    /// stand-in pill: click it or press ⏎ to respawn a fresh shell (the only path
    /// that frees the held term). Deliberately unobtrusive; Stage 2's tab-dissolve
    /// replaces it.
    fn render_held_affordance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Chrome-card colors derived from the theme (the hardcoded warm-dark
        // Swift fill read wrong on light themes) + the terminal font at the
        // chrome point size — the restyle card idiom.
        let slots = self.overlay_slots();
        let pill = div()
            .w(px(240.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .bg(slot_rgba(slots.panel))
            .border_1()
            .border_color(slot_rgba(slots.line))
            .text_size(px(12.0))
            .text_color(slot_rgba(slots.ink))
            .font_family(self.font_family.clone())
            .child("press \u{23ce} or click to start a new shell")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    this.dismiss_held(cx);
                    cx.stop_propagation();
                }),
            );
        // Anchored a little above the bottom edge, centred horizontally. Fills via
        // `inset: 0` (see [`overlay_fill`] — `.size_full()` is zero on an absolute
        // element).
        overlay_fill()
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .child(pill)
            .child(div().h(px(24.0)))
    }

    /// The copy-mode badge (P9): a small pill in the pane's top-right corner
    /// reading `COPY`, plus the live search query when one is running (the
    /// wording and its eliding are [`copy_mode_badge_label`], unit-tested).
    ///
    /// Top-RIGHT because the bottom of a pane is where the prompt and the app
    /// crate's search bar live, and because copy mode is usually entered to read
    /// something that scrolled off the top. Non-interactive (no listeners), so
    /// clicks and drags pass straight through to the grid underneath — in copy
    /// mode those move the vi cursor and select.
    fn render_copy_badge(&self, cx: &App) -> impl IntoElement {
        let slots = self.overlay_slots();
        let label: SharedString =
            copy_mode_badge_label(self.handle.read(cx).active_search_query()).into();

        let pill = div()
            .px(px(7.0))
            .py(px(2.0))
            .rounded(px(6.0))
            .bg(slot_rgba(slots.panel))
            .border_1()
            .border_color(slot_rgba(slots.line))
            .text_size(px(11.0))
            .text_color(slot_rgba(slots.ink))
            .font_family(self.font_family.clone())
            .child(label);

        // Inset from the pane's own edges, inside the Phase-2 content inset, so
        // the badge never sits on a split divider. Fills via `inset: 0` (see
        // [`overlay_fill`] — `.size_full()` is zero on an absolute element).
        overlay_fill()
            .flex()
            .justify_end()
            .items_start()
            .pt(px(4.0))
            .pr(px(6.0))
            .child(pill)
    }
}

/// A chrome slot as a gpui [`Rgba`] — this crate has no dependency on the app's
/// slot adapter, so the derive→Rgba hop lives here (overlay use only).
fn slot_rgba(slot: nice_theme::SlotColor) -> Rgba {
    let nice_theme::SlotColor::Srgb(s) = slot;
    Rgba {
        r: s.r,
        g: s.g,
        b: s.b,
        a: s.a,
    }
}

/// A full-view absolute overlay container (`position: absolute; inset: 0`). The
/// four explicit zero insets are load-bearing: a `.size_full()` (percentage size)
/// resolves to ZERO on an absolutely-positioned element in gpui/taffy, so it would
/// never paint — the insets are what stretch the overlay over the terminal.
fn overlay_fill() -> gpui::Div {
    div()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
}

impl TerminalView {
    /// Wheel / trackpad → line-stepped scrollback scroll, or VT wheel reports when
    /// the app requests mouse reporting (and Shift, the local override, is not
    /// held). gpui's convention is that a **positive** `delta.y` reveals earlier
    /// content, which for a terminal means scrolling **into history** — so the
    /// fractional line count derived from the delta is passed straight through to
    /// [`TerminalSessionHandle::scroll_lines`] (positive = toward history). The
    /// handle keeps the sub-line remainder as the deferred smooth-scroll seam;
    /// GPUI main pixel-snaps, so what actually paints is line-stepped.
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `pixel_delta` resolves both the precise (pixels) and coarse (lines)
        // wheel variants against the cell height; dividing back out yields a
        // fractional line count either way.
        let cell_h = self.metrics.cell_h;
        let dy: f32 = event.delta.pixel_delta(px(cell_h)).y.into();
        let lines = dy / cell_h;

        // Under app mouse reporting (and without the local-override Shift), the
        // wheel is a VT event, not local scrollback: emit button-64/65 reports at
        // the pointer cell. Positive `lines` reveals earlier content, i.e. wheel
        // **up** (button 64). Whole cells are reported; the remainder is kept so a
        // slow trackpad still eventually reports (like the scrollback accumulator).
        let mode = self.current_mode(cx);
        if mouse_reports_to_app(mode, event.modifiers.shift, self.copy_mode_active(cx)) {
            self.wheel_accum += lines;
            let steps = self.wheel_accum.trunc();
            self.wheel_accum -= steps;
            let count = (steps.abs() as i32).min(WHEEL_REPORT_MAX);
            if count > 0 {
                if let Some(hit) = self.hit_cell(event.position, cx) {
                    let button = if steps > 0.0 {
                        VtButton::WheelUp
                    } else {
                        VtButton::WheelDown
                    };
                    for _ in 0..count {
                        self.send_mouse_report(
                            mode,
                            button,
                            MouseAction::Press,
                            &hit,
                            event.modifiers,
                            cx,
                        );
                    }
                }
            }
            cx.stop_propagation();
            return;
        }

        if lines != 0.0 {
            self.handle.update(cx, |handle, hcx| {
                handle.scroll_lines(lines);
                hcx.notify();
            });
            // A wheel mid-drag extends the selection NOW, not on the next
            // mouse-move: the pointer pixel hasn't moved, but the content
            // under it has. Scroll first, then re-resolve the pointer against
            // the new display offset (`hit_cell` reads it) — the ordering
            // kitty (#7453) and alacritty (#1598) both pin; reversed it lags a
            // row per event. The anchor needs nothing: it is content-locked in
            // the Term (see the `drag_selecting` field docs).
            if self.drag_selecting {
                if let Some(hit) = self.hit_cell(event.position, cx) {
                    // A dead selection makes this a no-op; the gesture flag is
                    // only cleared by a real release (field docs). The repaint
                    // rides the `hcx.notify()` above via the handle observer.
                    self.handle
                        .read(cx)
                        .extend_selection((hit.buffer_line, hit.col));
                }
            }
        }
    }
}
