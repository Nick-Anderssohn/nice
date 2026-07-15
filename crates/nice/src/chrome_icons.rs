//! Embedded stroke-SVG chrome icons for the 2026-07 restyle (plans
//! `docs/plans/restyle/01-titlebar-restyle.md` and
//! `docs/plans/restyle/02-sidebar-flatten.md`).
//!
//! The restyle replaces the SF-Symbol chrome controls the titlebar and the
//! sidebar draw with the exact stroke icons from
//! `docs/design/restyle-mocks.html`. gpui's [`gpui::svg`] element renders an SVG
//! by rasterizing it to an alpha coverage mask and tinting the mask with the
//! element's `text` colour — so each icon is authored as a 1px-stroke shape with
//! an explicit (colour-irrelevant) stroke paint; only the coverage matters, the
//! caller's `.text_color(..)` sets the visible tint (`ink3` at rest, `ink` on
//! hover, per the mock's `.tb-btn` / `.sb-ico`).
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

/// The sidebar footer's "Claude tabs" mode-switcher icon: three horizontal
/// lines. Verbatim from `docs/design/restyle-mocks.html` (`.sb-footer`
/// `.sb-ico` "Claude tabs"): 14×12 viewBox, 1.4 stroke, round caps, `path d="M1
/// 2h12M1 6h12M1 10h8"`.
pub(crate) const MODE_TABS: &str = "chrome/mode-tabs.svg";
pub(crate) const MODE_TABS_W: f32 = 14.0;
pub(crate) const MODE_TABS_H: f32 = 12.0;

const MODE_TABS_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="12" viewBox="0 0 14 12" fill="none" stroke="#000" stroke-width="1.4" stroke-linecap="round"><path d="M1 2h12M1 6h12M1 10h8"/></svg>"##;

/// The sidebar footer's "File explorer" mode-switcher icon: an outline
/// folder. Verbatim from `docs/design/restyle-mocks.html` (`.sb-footer`
/// `.sb-ico` "File explorer"): 14×12 viewBox, 1px stroke (no explicit
/// `stroke-width`, matching the mock's default), `path d="M1
/// 3.5A1.5 1.5 0 0 1 2.5 2h2.6l1.4 1.8h5A1.5 1.5 0 0 1 13 5.3v4.2a1.5 1.5 0 0
/// 1-1.5 1.5h-9A1.5 1.5 0 0 1 1 9.5v-6z"`.
pub(crate) const MODE_FILES: &str = "chrome/mode-files.svg";
pub(crate) const MODE_FILES_W: f32 = 14.0;
pub(crate) const MODE_FILES_H: f32 = 12.0;

const MODE_FILES_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="12" viewBox="0 0 14 12" fill="none" stroke="#000" stroke-width="1"><path d="M1 3.5A1.5 1.5 0 0 1 2.5 2h2.6l1.4 1.8h5A1.5 1.5 0 0 1 13 5.3v4.2a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 1 9.5v-6z"/></svg>"##;

/// The sidebar footer's settings-gear icon. The mock renders this slot with a
/// Unicode font glyph (`⚙︎`) at authoring time, but round-2 restyle plan 4
/// promotes the footer to the mock's real stroke cog (superseding the shipped
/// sun-like radial-ray gear). Verbatim geometry from
/// `docs/design/restyle-mocks.html` (`.sb-ico.gear`): a 24×24 viewBox, 1.7
/// stroke, round caps + joins, `<circle cx=12 cy=12 r=3.2>` plus the classic
/// toothed-cog outline path. The element size stays 14×14 (the mock's own
/// `width`/`height`), so the 1.7 stroke over the 24-unit viewBox lands ≈1px at
/// render — matching the mode icons' weight.
pub(crate) const MODE_GEAR: &str = "chrome/mode-gear.svg";
pub(crate) const MODE_GEAR_W: f32 = 14.0;
pub(crate) const MODE_GEAR_H: f32 = 14.0;

const MODE_GEAR_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3.2"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>"##;

/// The embedded [`AssetSource`] serving the restyle's chrome icons. Stateless;
/// registered once via `gpui::Application::with_assets` in `crate::app`.
pub(crate) struct ChromeIconAssets;

impl AssetSource for ChromeIconAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            SIDEBAR_TOGGLE => Some(Cow::Borrowed(SIDEBAR_TOGGLE_SVG)),
            MODE_TABS => Some(Cow::Borrowed(MODE_TABS_SVG)),
            MODE_FILES => Some(Cow::Borrowed(MODE_FILES_SVG)),
            MODE_GEAR => Some(Cow::Borrowed(MODE_GEAR_SVG)),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![
            SharedString::from(SIDEBAR_TOGGLE),
            SharedString::from(MODE_TABS),
            SharedString::from(MODE_FILES),
            SharedString::from(MODE_GEAR),
        ])
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

    #[test]
    fn serves_the_mode_tabs_icon() {
        let assets = ChromeIconAssets;
        let bytes = assets.load(MODE_TABS).unwrap().expect("icon present");
        let svg = std::str::from_utf8(&bytes).unwrap();
        // docs/design/restyle-mocks.html .sb-footer "Claude tabs": 14×12
        // viewBox, 1.4 stroke, round caps, three horizontal lines.
        assert!(svg.contains("viewBox=\"0 0 14 12\""));
        assert!(svg.contains("stroke-width=\"1.4\""));
        assert!(svg.contains("stroke-linecap=\"round\""));
        assert!(svg.contains("M1 2h12M1 6h12M1 10h8"));
    }

    #[test]
    fn serves_the_mode_files_icon() {
        let assets = ChromeIconAssets;
        let bytes = assets.load(MODE_FILES).unwrap().expect("icon present");
        let svg = std::str::from_utf8(&bytes).unwrap();
        // docs/design/restyle-mocks.html .sb-footer "File explorer": 14×12
        // viewBox, 1px stroke, outline-folder path.
        assert!(svg.contains("viewBox=\"0 0 14 12\""));
        assert!(svg.contains("stroke-width=\"1\""));
        assert!(svg.contains("M1 3.5A1.5 1.5 0 0 1 2.5 2h2.6l1.4 1.8h5"));
    }

    #[test]
    fn serves_a_new_thin_stroke_gear_not_a_glyph() {
        let assets = ChromeIconAssets;
        let bytes = assets.load(MODE_GEAR).unwrap().expect("icon present");
        let svg = std::str::from_utf8(&bytes).unwrap();
        // The mock's real stroke cog (docs/design/restyle-mocks.html .sb-ico.gear):
        // 24×24 viewBox, 1.7 stroke, round caps + joins, a center ring
        // (circle r=3.2) plus the toothed-cog outline path — not the Unicode glyph
        // the mock uses as a placeholder, and not SF_GEAR.
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        assert!(svg.contains("stroke-width=\"1.7\""));
        assert!(svg.contains("stroke-linejoin=\"round\""));
        assert!(svg.contains("<circle cx=\"12\" cy=\"12\" r=\"3.2\""));
        assert!(svg.contains("M19.4 15a1.65 1.65 0 0 0 .33 1.82"));
        assert!(!svg.contains('\u{2699}')); // ⚙ — must not embed the font glyph.
    }

    #[test]
    fn list_enumerates_every_served_icon() {
        let assets = ChromeIconAssets;
        let listed = assets.list("chrome").unwrap();
        for path in [SIDEBAR_TOGGLE, MODE_TABS, MODE_FILES, MODE_GEAR] {
            assert!(listed.iter().any(|s| s.as_ref() == path), "{path} missing from list()");
        }
    }
}
