//! Typography aliases, ported verbatim from
//! `Sources/Nice/Theme/Typography.swift`.
//!
//! This is three NAMED FONT ALIASES as data — not a full type scale and not
//! font resolution. Swift's `Font.system(_:design:)` picks a concrete size from
//! the OS text style at draw time; resolving those (the SF Mono → JetBrains
//! Mono NL → system chain, and pixel sizes) is R7's job, not R2's. Here we only
//! record the `(text-style, design)` pair each alias names.

/// The Swift `Font.TextStyle` an alias is built on — only the two cases
/// `Typography.swift` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextStyle {
    /// SwiftUI `.body`.
    Body,
    /// SwiftUI `.caption`.
    Caption,
}

/// The Swift `Font.Design` an alias requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontDesign {
    /// SwiftUI default design (no `design:` argument).
    Default,
    /// SwiftUI `.monospaced`.
    Monospaced,
}

/// A named font alias as pure data: the text style and design it selects. Font
/// *resolution* (family chain, point size) is deferred to R7.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypographyAlias {
    /// The OS text style the size is taken from.
    pub text_style: TextStyle,
    /// The font design (default or monospaced).
    pub design: FontDesign,
}

/// `Font.niceUI` — `Font.system(.body)` (`Typography.swift:12`).
pub const NICE_UI: TypographyAlias = TypographyAlias {
    text_style: TextStyle::Body,
    design: FontDesign::Default,
};

/// `Font.niceMono` — `Font.system(.body, design: .monospaced)`
/// (`Typography.swift:13`).
pub const NICE_MONO: TypographyAlias = TypographyAlias {
    text_style: TextStyle::Body,
    design: FontDesign::Monospaced,
};

/// `Font.niceMonoSmall` — `Font.system(.caption, design: .monospaced)`
/// (`Typography.swift:14`).
pub const NICE_MONO_SMALL: TypographyAlias = TypographyAlias {
    text_style: TextStyle::Caption,
    design: FontDesign::Monospaced,
};

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures for the typography aliases. See
    //! crates/README.md "Fixture-provenance convention".
    use super::*;

    #[test]
    fn aliases_match_swift() {
        // Typography.swift:12-14.
        assert_eq!(
            NICE_UI,
            TypographyAlias {
                text_style: TextStyle::Body,
                design: FontDesign::Default
            }
        ); // Typography.swift:12
        assert_eq!(
            NICE_MONO,
            TypographyAlias {
                text_style: TextStyle::Body,
                design: FontDesign::Monospaced
            }
        ); // Typography.swift:13
        assert_eq!(
            NICE_MONO_SMALL,
            TypographyAlias {
                text_style: TextStyle::Caption,
                design: FontDesign::Monospaced
            }
        ); // Typography.swift:14
    }
}
