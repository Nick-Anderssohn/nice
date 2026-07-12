//! `DriftBannerView` — the transient bottom overlay that surfaces the app-wide
//! file-operation history's one failure channel (F6). Ported from
//! `FileOperationDriftBanner.swift`.
//!
//! Mounted ONCE PER WINDOW in the shipped `build_window_root` composition (via
//! [`crate::app_shell::AppShellView`]), regardless of sidebar mode — every window
//! shows the same message simultaneously (Swift parity), because it observes the
//! ONE process-wide [`FileOperationHistory`] entity (the
//! [`FileOperationHistoryGlobal`]). It renders iff
//! [`FileOperationHistory::last_drift_message`] is `Some`: an undo glyph + the
//! message + a manual ✕, auto-dismissing after 3.5 s via the **App-Nap-safe**
//! delay ([`crate::platform::nap_safe_delay`]) — never a bare gpui timer, which App
//! Nap would defer on the idle window a stale banner lives on. The auto-dismiss is
//! guarded by message identity so a newer message isn't clobbered by an older
//! timer.
//!
//! AX: role Group + label `nice-drift-banner` (the shipped-surface anchor the
//! composition leg walks for).

use std::time::Duration;

use gpui::{
    div, prelude::*, px, App, Context, Entity, MouseButton, MouseDownEvent, Render, SharedString,
    Subscription, Task, Window,
};

use nice_theme::palette::{slots, ColorScheme, Palette, Slots};

use super::history::{FileOperationHistory, FileOperationHistoryGlobal};
use crate::theme::{slot_to_rgba, srgba_to_rgba, srgba_with_alpha};

/// The shipped-surface AX anchor label for the drift banner (role Group + this
/// label; no title — the `app-shell` AX convention).
pub(crate) const DRIFT_BANNER_LABEL: &str = "nice-drift-banner";

/// How long a banner shows before it auto-dismisses (Swift's 3.5 s).
const DISMISS_AFTER: Duration = Duration::from_millis(3500);

/// Max banner width (Swift's 480 pt).
const MAX_WIDTH: f32 = 480.0;

/// The per-window drift banner. Observes the process-wide history entity and
/// renders its transient message; a window without a history Global installed
/// (a scenario that never stood one up) renders nothing.
pub(crate) struct DriftBannerView {
    /// The process-wide history entity, if a Global is installed. Held so the
    /// manual-✕ / auto-dismiss paths can clear the message.
    history: Option<Entity<FileOperationHistory>>,
    /// Re-render (and re-arm the auto-dismiss) when the history publishes a
    /// message.
    _history_sub: Option<Subscription>,
    /// The message currently shown + timed — the identity guard so a stale
    /// auto-dismiss never clears a newer message.
    armed: Option<String>,
    /// The in-flight auto-dismiss task; dropped (cancelled) when a new message
    /// re-arms it.
    _dismiss: Option<Task<()>>,
}

impl DriftBannerView {
    /// Build over the process-wide [`FileOperationHistoryGlobal`], if installed.
    /// Called by [`crate::app_shell::AppShellView::new`] (the shipped composition).
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let history = cx
            .try_global::<FileOperationHistoryGlobal>()
            .map(|g| g.0.clone());
        let sub = history.as_ref().map(|h| {
            cx.observe(h, |this, history, cx| {
                this.on_history_changed(history, cx);
            })
        });
        Self {
            history,
            _history_sub: sub,
            armed: None,
            _dismiss: None,
        }
    }

    /// The message to render, if any.
    fn current_message(&self, cx: &App) -> Option<String> {
        self.history
            .as_ref()
            .and_then(|h| h.read(cx).last_drift_message().map(str::to_owned))
    }

    /// The history published (or cleared) a message: re-arm the identity-guarded
    /// auto-dismiss for a fresh non-empty message, then re-render.
    fn on_history_changed(
        &mut self,
        history: Entity<FileOperationHistory>,
        cx: &mut Context<Self>,
    ) {
        let msg = history.read(cx).last_drift_message().map(str::to_owned);
        if msg == self.armed {
            return;
        }
        self.armed = msg.clone();
        // Dropping the prior task cancels its pending wake, so an older timer can
        // never clear the newer message.
        self._dismiss = None;
        if let Some(m) = msg {
            let history = history.clone();
            self._dismiss = Some(cx.spawn(async move |this, acx| {
                // App-Nap-safe: a dedicated-OS-thread sleep + main-runloop wake, not
                // a coalescable timer the idle banner window would let App Nap defer.
                crate::platform::nap_safe_delay(DISMISS_AFTER).await;
                let _ = this.update(acx, |this, cx| {
                    // Identity guard: only clear if the SAME message is still up.
                    if this.armed.as_deref() == Some(m.as_str()) {
                        history.update(cx, |h, hcx| {
                            h.clear_drift_message();
                            hcx.notify();
                        });
                        this.armed = None;
                    }
                });
            }));
        }
        cx.notify();
    }

    /// The manual ✕: clear the message now (and cancel the pending auto-dismiss).
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(history) = self.history.clone() {
            history.update(cx, |h, hcx| {
                h.clear_drift_message();
                hcx.notify();
            });
        }
        self.armed = None;
        self._dismiss = None;
        cx.notify();
    }
}

impl Render for DriftBannerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(message) = self.current_message(cx) else {
            // Nothing to show — an empty, zero-impact overlay layer.
            return div().id("drift-banner.layer");
        };
        let s: Slots =
            slots(Palette::Nice, ColorScheme::Dark).expect("Nice + Dark is a valid palette/scheme");
        let panel_bg = srgba_to_rgba(srgba_with_alpha(
            nice_theme::color::Srgba {
                r: 0.10,
                g: 0.11,
                b: 0.14,
                a: 1.0,
            },
            0.96,
        ));
        let ink = slot_to_rgba(s.ink);
        let ink2 = slot_to_rgba(s.ink2);

        // Bottom-centered floating card over the whole shell. `absolute` takes the
        // layer out of flow so it never displaces the sidebar/pane content.
        div()
            .id("drift-banner.layer")
            .absolute()
            .bottom(px(16.0))
            .left_0()
            .right_0()
            .flex()
            .flex_row()
            .justify_center()
            .child(
                div()
                    .id(DRIFT_BANNER_LABEL)
                    .role(gpui::Role::Group)
                    .aria_label(DRIFT_BANNER_LABEL)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .max_w(px(MAX_WIDTH))
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(panel_bg)
                    // undo glyph
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(ink2)
                            .child(SharedString::from("\u{21BA}")),
                    )
                    // message
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.0))
                            .text_color(ink)
                            .child(SharedString::from(message)),
                    )
                    // manual ✕
                    .child(
                        div()
                            .id("drift-banner.dismiss")
                            .cursor_pointer()
                            .px(px(4.0))
                            .text_size(px(13.0))
                            .text_color(ink2)
                            .child(SharedString::from("\u{2715}"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e: &MouseDownEvent, _w, cx| {
                                    this.dismiss(cx);
                                    cx.stop_propagation();
                                }),
                            ),
                    ),
            )
    }
}
