//! Embedded stroke-SVG chrome icons for the 2026-07 restyle (plan
//! `docs/plans/restyle/01-titlebar-restyle.md`).
//!
//! The restyle replaces the SF-Symbol chrome controls the titlebar (and, in
//! plan 2, the sidebar) draws with the exact stroke icons from
//! `docs/design/restyle-mocks.html`. gpui's [`gpui::svg`] element renders an SVG
//! by rasterizing it to an alpha coverage mask and tinting the mask with the
//! element's `text` colour — so each icon is authored as a 1px-stroke shape with
//! an explicit (colour-irrelevant) stroke paint; only the coverage matters, the
//! caller's `.text_color(..)` sets the visible tint (`ink3` at rest, `ink` on
//! hover, per the mock's `.tb-btn`).
//!
//! The bytes are served to gpui through a tiny embedded [`AssetSource`]
//! ([`ChromeIconAssets`]) registered at app startup
//! (`gpui::Application::with_assets`). It answers only for the `chrome/…` paths
//! below; every other path (including the SVG font-loader's probes) resolves to
//! `None`, exactly as gpui's default no-op source would, so registering it
//! changes nothing else.
//!
//! SF-symbol rendering (`crate::sf_symbols`) stays for every surface the restyle
//! does not cover.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// The sidebar-collapse toggle icon (Finder/Safari "toggle sidebar"): a rounded
/// rectangle with a vertical divider marking the sidebar column. Verbatim from
/// `docs/design/restyle-mocks.html` (`.tb-btn`): 15×12 viewBox, 1px stroke,
/// `rect x=.5 y=.5 w=14 h=11 rx=2.5` + a vertical line at x=5.5. The stroke
/// paint is a fixed black — gpui uses only the alpha mask and re-tints it — so
/// the caller's `text_color` is what shows.
pub(crate) const SIDEBAR_TOGGLE: &str = "chrome/sidebar-toggle.svg";

/// The icon's authored aspect box (viewBox), in points — the caller sizes the
/// `svg()` element to these so it lays out at the mock's dimensions.
pub(crate) const SIDEBAR_TOGGLE_W: f32 = 15.0;
pub(crate) const SIDEBAR_TOGGLE_H: f32 = 12.0;

const SIDEBAR_TOGGLE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="15" height="12" viewBox="0 0 15 12" fill="none" stroke="#000" stroke-width="1"><rect x="0.5" y="0.5" width="14" height="11" rx="2.5"/><line x1="5.5" y1="0.5" x2="5.5" y2="11.5"/></svg>"##;

/// The embedded [`AssetSource`] serving the restyle's chrome icons. Stateless;
/// registered once via `gpui::Application::with_assets` in `crate::app`.
pub(crate) struct ChromeIconAssets;

impl AssetSource for ChromeIconAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            SIDEBAR_TOGGLE => Some(Cow::Borrowed(SIDEBAR_TOGGLE_SVG)),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![SharedString::from(SIDEBAR_TOGGLE)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_the_sidebar_toggle_and_nothing_else() {
        let assets = ChromeIconAssets;
        let bytes = assets.load(SIDEBAR_TOGGLE).unwrap().expect("icon present");
        // The mock's exact geometry markers (15×12 viewBox, rect rx=2.5, the
        // x=5.5 divider line).
        let svg = std::str::from_utf8(&bytes).unwrap();
        assert!(svg.contains("viewBox=\"0 0 15 12\""));
        assert!(svg.contains("rx=\"2.5\""));
        assert!(svg.contains("x1=\"5.5\""));
        assert!(assets.load("chrome/does-not-exist.svg").unwrap().is_none());
    }
}
