//! The pure option-as-meta config value.
//!
//! On macOS the Option key can either compose special characters (its OS
//! default — `⌥o` → `œ`) or act as the terminal Meta key, prefixing the
//! keystroke with `ESC` so applications see `M-x` etc. The SwiftTerm fork ships
//! this as a single boolean (`optionAsMetaKey`, default on / "both options are
//! Meta"). This value type keeps that parity default while also modelling the
//! macOS-idiomatic per-side choice (the same shape alacritty's `OptionAsAlt`
//! uses), so a later settings UI can offer left-only / right-only without a
//! data-model change. It is pure config: the event-edge slice consults it to
//! decide whether to set `Modifiers::alt` (and thus whether the keyboard
//! encoder emits the Meta `ESC` prefix); the encoder itself never sees it.

/// Which physical Option/Alt key an event came from, for the per-side policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionSide {
    /// The left Option/Alt key.
    Left,
    /// The right Option/Alt key.
    Right,
}

/// Whether the Option/Alt key acts as Meta (sends `ESC`) or is left to the OS
/// to compose characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OptionAsMeta {
    /// Option never acts as Meta — the OS composes special characters. Equals
    /// SwiftTerm's `optionAsMetaKey = false`.
    Off,
    /// Both Option keys act as Meta. The SwiftTerm shipping default
    /// (`optionAsMetaKey = true`), and this type's [`Default`].
    #[default]
    Both,
    /// Only the left Option key acts as Meta; the right composes.
    LeftOnly,
    /// Only the right Option key acts as Meta; the left composes.
    RightOnly,
}

impl OptionAsMeta {
    /// Whether an Option/Alt press on `side` should be treated as Meta (and thus
    /// have `ESC` prefixed / `Modifiers::alt` set), given this policy.
    pub fn treats_as_meta(self, side: OptionSide) -> bool {
        match self {
            OptionAsMeta::Off => false,
            OptionAsMeta::Both => true,
            OptionAsMeta::LeftOnly => side == OptionSide::Left,
            OptionAsMeta::RightOnly => side == OptionSide::Right,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_swiftterm_both() {
        assert_eq!(OptionAsMeta::default(), OptionAsMeta::Both);
        assert!(OptionAsMeta::default().treats_as_meta(OptionSide::Left));
        assert!(OptionAsMeta::default().treats_as_meta(OptionSide::Right));
    }

    #[test]
    fn off_never_meta() {
        assert!(!OptionAsMeta::Off.treats_as_meta(OptionSide::Left));
        assert!(!OptionAsMeta::Off.treats_as_meta(OptionSide::Right));
    }

    #[test]
    fn per_side_policies() {
        assert!(OptionAsMeta::LeftOnly.treats_as_meta(OptionSide::Left));
        assert!(!OptionAsMeta::LeftOnly.treats_as_meta(OptionSide::Right));
        assert!(!OptionAsMeta::RightOnly.treats_as_meta(OptionSide::Left));
        assert!(OptionAsMeta::RightOnly.treats_as_meta(OptionSide::Right));
    }
}
