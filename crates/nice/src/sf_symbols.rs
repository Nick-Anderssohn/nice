//! Runtime SF Symbol icons (M2 feel-check Item A) — the Rust half of the
//! symbol pipeline whose AppKit half is [`crate::platform::rasterize_sf_symbol`].
//!
//! Every chrome icon used to be a Unicode stand-in glyph; this module replaces
//! them with real SF Symbols, rasterized once per `(name, size, weight, colour,
//! scale)` and presented through gpui's `img()`:
//!
//!   * `platform` resolves `NSImage(systemSymbolName:)` +
//!     `NSImageSymbolConfiguration` (point size, weight) and hands back a
//!     straight coverage mask at the window's backing scale;
//!   * [`sf_symbol_icon`] tints that mask with the caller's palette colour into
//!     a [`gpui::RenderImage`] — **BGRA, straight (non-premultiplied) alpha**,
//!     the frame format gpui's own loaders produce (`gpui/src/elements/img.rs`
//!     decodes straight-alpha RGBA and swaps R↔B; `svg_renderer.rs`
//!     un-premultiplies via `swap_rgba_pa_to_bgra`) — and caches the bitmap in
//!     a process [`Global`] (the `keymap` global pattern), so a render pass
//!     after the first is a HashMap hit;
//!   * the element sets its own point size (`device px / scale`) explicitly,
//!     because `RenderImage::new` fixes `scale_factor = 1.0` (crate-private),
//!     so an unsized `img()` would lay the bitmap out at device-pixel size;
//!   * active / inactive / hover tints are just different colours → different
//!     cache entries; button boxes and hover fills stay with the callers.
//!
//! Fallback: if a symbol name fails to resolve (or any AppKit step fails), the
//! caller's original Unicode glyph renders instead — styled with the same
//! size / weight / colour — so nothing ever goes blank. The failure is
//! negative-cached too, so a missing symbol costs one AppKit round-trip per
//! key, not one per frame.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{div, img, prelude::*, px, AnyElement, App, FontWeight, Global, RenderImage, Rgba, SharedString};
use image::Frame;

use crate::platform::{self, SymbolBitmap};

/// The symbol weights the chrome uses (the subset of `NSFontWeight` the icon
/// table needs today, resolved through the linked AppKit constants — see
/// `crate::platform`; add variants as call sites need them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SymbolWeight {
    Regular,
    Semibold,
}

impl SymbolWeight {
    /// The AppKit `NSFontWeight` value fed to `NSImageSymbolConfiguration`.
    fn ns_weight(self) -> f64 {
        match self {
            SymbolWeight::Regular => platform::ns_font_weight_regular(),
            SymbolWeight::Semibold => platform::ns_font_weight_semibold(),
        }
    }

    /// The matching gpui text weight — used only by the Unicode fallback so it
    /// keeps the stand-in's original look.
    fn font_weight(self) -> FontWeight {
        match self {
            SymbolWeight::Regular => FontWeight::NORMAL,
            SymbolWeight::Semibold => FontWeight::SEMIBOLD,
        }
    }
}

/// One cached, tinted symbol bitmap plus the logical point box the `img()`
/// element must claim (`device px / scale` — see the module docs on
/// `RenderImage::scale_factor`).
#[derive(Clone)]
struct SymbolImage {
    image: Arc<RenderImage>,
    width_pt: f32,
    height_pt: f32,
}

/// Cache key: symbol name + quantized point size (quarter-points), weight,
/// RGBA8 tint, and quantized backing scale. Quantization only canonicalizes
/// float noise — the app feeds a handful of exact sizes / two scales.
#[derive(Clone, PartialEq, Eq, Hash)]
struct SymbolKey {
    name: &'static str,
    size_q: u16,
    weight: SymbolWeight,
    color: [u8; 4],
    scale_q: u16,
}

impl SymbolKey {
    fn new(name: &'static str, point_size: f32, weight: SymbolWeight, color: Rgba, scale: f32) -> Self {
        let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self {
            name,
            size_q: (point_size * 4.0).round() as u16,
            weight,
            color: [q(color.r), q(color.g), q(color.b), q(color.a)],
            scale_q: (scale * 4.0).round() as u16,
        }
    }
}

/// The process-level rendered-symbol cache (`None` = the symbol failed to
/// resolve; the Unicode fallback renders and no further AppKit attempts are
/// made for that key). A gpui [`Global`], following `keymap`'s
/// `SharedFontSettings` pattern.
#[derive(Default)]
struct SfSymbolCache(HashMap<SymbolKey, Option<SymbolImage>>);

impl Global for SfSymbolCache {}

/// The icon element: the SF Symbol `name` rasterized at `point_size` /
/// `weight`, tinted `color`, at `scale` device pixels per point (pass the
/// window's `scale_factor()`), or the `fallback_glyph` styled identically when
/// the symbol cannot be resolved. The returned element is exactly the glyph
/// box — callers keep their own button frames, hover fills, and press
/// handlers.
pub(crate) fn sf_symbol_icon(
    name: &'static str,
    fallback_glyph: &'static str,
    point_size: f32,
    weight: SymbolWeight,
    color: Rgba,
    scale: f32,
    cx: &mut App,
) -> AnyElement {
    let key = SymbolKey::new(name, point_size, weight, color, scale);
    let cached = cx.default_global::<SfSymbolCache>().0.get(&key).cloned();
    let entry = match cached {
        Some(entry) => entry,
        None => {
            let rendered = platform::rasterize_sf_symbol(name, point_size, weight.ns_weight(), scale)
                .map(|bitmap| tint_symbol(&bitmap, color, scale));
            cx.default_global::<SfSymbolCache>()
                .0
                .insert(key, rendered.clone());
            rendered
        }
    };

    match entry {
        // `flex_none()` is load-bearing (fix round r6): a symbol's canvas is
        // routinely wider than the caller's icon frame (SF "terminal" at 12pt
        // is a 17×13pt canvas; prod's `.frame(width: 12, height: 12)` lets it
        // overflow, `WindowToolbarView.swift:903-906`). Without it the img is
        // a shrinkable flex item, and gpui's default `ObjectFit::Contain`
        // turned the squeeze into a uniform downscale — the pill icons
        // rendered visibly smaller and fainter than prod.
        Some(icon) => img(icon.image)
            .w(px(icon.width_pt))
            .h(px(icon.height_pt))
            .flex_none()
            .into_any_element(),
        None => div()
            .flex_none()
            .text_size(px(point_size))
            .font_weight(weight.font_weight())
            .text_color(color)
            .child(SharedString::from(fallback_glyph))
            .into_any_element(),
    }
}

/// Cache key name for the brand logo mark — an impossible SF Symbol name, so
/// it can never collide with a real symbol's cache entries.
const LOGO_MARK_KEY: &str = "nice.logo.mark";

/// The brand logo mark (fix round r6): the white chevron + underline stroke
/// from `Logo.swift`, rasterized by [`platform::rasterize_logo_mark`] on the
/// exact prod path geometry and cached/tinted through the same pipeline as the
/// SF Symbols above. The element is the full 22×22pt logo canvas
/// ([`platform::LOGO_MARK_CANVAS_PT`]) — the caller overlays it on its accent
/// square, which occupies the same viewBox (rect x=1 y=1 20×20).
///
/// `fallback_glyph` renders 11pt-bold in `color` when rasterization fails (the
/// toolbar's pre-r6 `❯` stand-in look), so the brand slot never goes blank.
pub(crate) fn logo_mark_icon(
    fallback_glyph: &'static str,
    color: Rgba,
    scale: f32,
    cx: &mut App,
) -> AnyElement {
    let key = SymbolKey::new(
        LOGO_MARK_KEY,
        platform::LOGO_MARK_CANVAS_PT,
        SymbolWeight::Regular,
        color,
        scale,
    );
    let cached = cx.default_global::<SfSymbolCache>().0.get(&key).cloned();
    let entry = match cached {
        Some(entry) => entry,
        None => {
            let rendered =
                platform::rasterize_logo_mark(scale).map(|bitmap| tint_symbol(&bitmap, color, scale));
            cx.default_global::<SfSymbolCache>()
                .0
                .insert(key, rendered.clone());
            rendered
        }
    };

    match entry {
        Some(icon) => img(icon.image)
            .w(px(icon.width_pt))
            .h(px(icon.height_pt))
            .flex_none()
            .into_any_element(),
        None => div()
            .flex_none()
            .text_size(px(11.0))
            .font_weight(FontWeight::BOLD)
            .text_color(color)
            .child(SharedString::from(fallback_glyph))
            .into_any_element(),
    }
}

/// Tint a coverage mask into a BGRA straight-alpha [`RenderImage`] frame. The
/// colour channels carry the tint everywhere (also under zero coverage) so
/// bilinear sampling at the glyph edge never pulls a foreign colour in; the
/// alpha channel is `coverage × tint alpha`.
fn tint_symbol(bitmap: &SymbolBitmap, color: Rgba, scale: f32) -> SymbolImage {
    let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (b, g, r) = (q(color.b), q(color.g), q(color.r));
    let tint_a = color.a.clamp(0.0, 1.0);

    let mut data = Vec::with_capacity(bitmap.coverage.len() * 4);
    for &coverage in &bitmap.coverage {
        let a = (f32::from(coverage) * tint_a).round() as u8;
        // BGRA byte order, straight alpha (see the module docs).
        data.extend_from_slice(&[b, g, r, a]);
    }
    let buffer = image::RgbaImage::from_raw(bitmap.px_width as u32, bitmap.px_height as u32, data)
        .expect("buffer is exactly px_width * px_height * 4 bytes");

    let scale = scale.max(1.0);
    SymbolImage {
        image: Arc::new(RenderImage::new(vec![Frame::new(buffer)])),
        width_pt: bitmap.px_width as f32 / scale,
        height_pt: bitmap.px_height as f32 / scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_key_quantizes_size_scale_and_color() {
        let color = Rgba {
            r: 0.5,
            g: 0.25,
            b: 1.0,
            a: 1.0,
        };
        let a = SymbolKey::new("plus", 10.0, SymbolWeight::Semibold, color, 2.0);
        let b = SymbolKey::new("plus", 10.0000001, SymbolWeight::Semibold, color, 2.0);
        assert!(a == b, "float noise must not split cache entries");
        let c = SymbolKey::new("plus", 10.0, SymbolWeight::Regular, color, 2.0);
        assert!(a != c, "weight is part of the key");
        let d = SymbolKey::new("plus", 10.0, SymbolWeight::Semibold, color, 1.0);
        assert!(a != d, "backing scale is part of the key");
    }

    #[test]
    fn tint_fills_bgra_straight_alpha() {
        // A 2×1 mask: transparent, fully inked. Tint = pure red at full alpha.
        let bitmap = SymbolBitmap {
            coverage: vec![0, 255],
            px_width: 2,
            px_height: 1,
        };
        let red = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let icon = tint_symbol(&bitmap, red, 2.0);
        let bytes = icon.image.as_bytes(0).expect("one frame");
        // BGRA: colour channels carry the tint even at zero coverage.
        assert_eq!(bytes, &[0, 0, 255, 0, 0, 0, 255, 255]);
        // The element box is device px / scale.
        assert_eq!(icon.width_pt, 1.0);
        assert_eq!(icon.height_pt, 0.5);
    }

    #[test]
    fn tint_scales_alpha_by_tint_alpha() {
        let bitmap = SymbolBitmap {
            coverage: vec![200],
            px_width: 1,
            px_height: 1,
        };
        let half = Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.5,
        };
        let icon = tint_symbol(&bitmap, half, 1.0);
        let bytes = icon.image.as_bytes(0).expect("one frame");
        assert_eq!(bytes[3], 100); // 200 × 0.5
    }
}
