//! `TerminalView` — the gpui view that owns a [`FocusHandle`] and paints a
//! [`TerminalSessionHandle`] through a [`TerminalElement`] each frame.
//!
//! It observes the session handle (repaint on the handle's `notify`) and owns a
//! `FocusHandle` (needed for R5 key dispatch + DECSET-1004 focus reporting).
//! The caret's solid/hollow state is **computed** from
//! `focus_handle.is_focused(window) && window.is_window_active()` every frame —
//! there is deliberately **no separately-maintained focus flag** (that is
//! pain-catalog mechanism #5, remembered-not-computed state). R13 later directs
//! focus here via `focus_handle.focus()`.

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Render, ScrollWheelEvent,
    SharedString, Subscription, Window,
};

use nice_theme::Srgba;

use crate::element::{TerminalElement, TerminalMetrics};
use crate::session_handle::TerminalSessionHandle;
use crate::theme::TerminalTheme;

/// A view over one terminal session. Construct with [`TerminalView::new`] from a
/// session handle + theme value + accent (R2) + cell metrics.
pub struct TerminalView {
    handle: Entity<TerminalSessionHandle>,
    theme: TerminalTheme,
    accent: Srgba,
    font_family: SharedString,
    font_px: f32,
    metrics: TerminalMetrics,
    focus_handle: FocusHandle,
    /// Repaint subscription to the session handle. Held so it stays live for the
    /// view's lifetime.
    _handle_sub: Subscription,
}

impl TerminalView {
    /// Build a view over `handle`, painting with `theme` (caret in `accent`
    /// unless the theme overrides the cursor) at `font_family` / `font_px` and
    /// the given cell `metrics`.
    pub fn new(
        handle: Entity<TerminalSessionHandle>,
        theme: TerminalTheme,
        accent: Srgba,
        font_family: SharedString,
        font_px: f32,
        metrics: TerminalMetrics,
        cx: &mut Context<Self>,
    ) -> Self {
        // Repaint whenever the session handle notifies (new output / events).
        let sub = cx.observe(&handle, |_this, _handle, cx| cx.notify());
        Self {
            handle,
            theme,
            accent,
            font_family,
            font_px,
            metrics,
            focus_handle: cx.focus_handle(),
            _handle_sub: sub,
        }
    }

    /// The view's focus handle (R5 drives key input through it; R13 focuses it).
    pub fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus_handle
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Take focus once (idempotent — `Window::focus` early-returns if this
        // handle already holds it) so the caret's computed focus state is live
        // without a stored flag. R13 will own focus routing across panes.
        window.focus(&self.focus_handle, cx);

        let caret_solid = self.focus_handle.is_focused(window) && window.is_window_active();
        let element = TerminalElement::new(
            self.handle.read(cx),
            &self.theme,
            self.accent,
            self.font_family.clone(),
            self.font_px,
            self.metrics,
            caret_solid,
        );

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(element)
    }
}

impl TerminalView {
    /// Wheel / trackpad → line-stepped scrollback scroll. gpui's convention is
    /// that a **positive** `delta.y` reveals earlier content, which for a terminal
    /// means scrolling **into history** — so the fractional line count derived
    /// from the delta is passed straight through to
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
        if lines != 0.0 {
            self.handle.update(cx, |handle, hcx| {
                handle.scroll_lines(lines);
                hcx.notify();
            });
        }
    }
}
