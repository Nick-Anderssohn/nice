//! Ported test-for-test from `Tests/NiceUnitTests/ShellQuotingTests.swift`.
//! Byte-compares both quoting helpers against the exact expectations the Swift
//! suite pins — a bad encoding here silently corrupts every shell command Nice
//! builds.

use nice_term_core::{shell_backslash_escape, shell_single_quote};

// ---- shell_single_quote ---------------------------------------------------

#[test]
fn empty_string_produces_empty_quoted_pair() {
    assert_eq!(shell_single_quote(""), "''");
}

#[test]
fn simple_string_wrapped_in_single_quotes() {
    assert_eq!(shell_single_quote("hello"), "'hello'");
}

#[test]
fn embedded_single_quote_uses_close_open_escape() {
    // The classic '\'' trick: close the quote, emit a literal backslash-quote,
    // reopen the quote. Must survive verbatim.
    assert_eq!(shell_single_quote("it's"), r#"'it'\''s'"#);
}

#[test]
fn only_single_quotes_escaped_correctly() {
    assert_eq!(shell_single_quote("'"), r#"''\'''"#);
    assert_eq!(shell_single_quote("''"), r#"''\'''\'''"#);
}

#[test]
fn shell_metacharacters_pass_through_inside_quotes() {
    // `$`, backtick, `\`, `"`, `*`, `?`, space — all literal inside single
    // quotes, must not be expanded or escaped.
    let weird = r#"$HOME `date` \n "x" * ? ~"#;
    assert_eq!(shell_single_quote(weird), format!("'{weird}'"));
}

#[test]
fn newline_passes_through_inside_quotes() {
    assert_eq!(shell_single_quote("a\nb"), "'a\nb'");
}

#[test]
fn unicode_passes_through_inside_quotes() {
    assert_eq!(shell_single_quote("café 🫠"), "'café 🫠'");
}

#[test]
fn single_quote_round_trip_invariant() {
    // The invariant the quoting promises: the quoted form starts and ends with
    // `'`, and its interior contains no raw single quote outside a `'\''`
    // escape sequence.
    let inputs = ["", "plain", "with space", "it's", "''''", "$(rm -rf /)", "\\\"`$"];
    for input in inputs {
        let quoted = shell_single_quote(input);
        assert!(quoted.starts_with('\''), "missing leading quote: {quoted}");
        assert!(quoted.ends_with('\''), "missing trailing quote: {quoted}");
        let inner = &quoted[1..quoted.len() - 1];
        let sanitized = inner.replace(r#"'\''"#, "\u{FFFD}");
        assert!(
            !sanitized.contains('\''),
            "raw single quote leaked through quoting for input {input:?}: {quoted}"
        );
    }
}

// ---- shell_backslash_escape -----------------------------------------------

#[test]
fn backslash_escape_empty_string_returns_empty() {
    // Key behavioral difference from single-quote, which returns `''`.
    assert_eq!(shell_backslash_escape(""), "");
}

#[test]
fn backslash_escape_pure_alnum_unchanged() {
    assert_eq!(shell_backslash_escape("hello"), "hello");
    assert_eq!(shell_backslash_escape("HELLO123"), "HELLO123");
}

#[test]
fn backslash_escape_plain_path_unchanged() {
    assert_eq!(
        shell_backslash_escape("/Users/nick/file.txt"),
        "/Users/nick/file.txt"
    );
}

#[test]
fn backslash_escape_safe_set_passes_through() {
    for ch in [
        "a", "Z", "0", "9", ".", "_", "/", "+", ":", "=", "@", ",", "-",
    ] {
        assert_eq!(
            shell_backslash_escape(ch),
            ch,
            "expected {ch:?} to pass through"
        );
    }
}

#[test]
fn backslash_escape_unsafe_set_each_char_prefixed() {
    // Every other printable ASCII metachar gets one backslash. Listed verbatim
    // to match the helper's doc comment 1:1.
    let unsafe_chars = [
        " ", "!", "\"", "#", "$", "%", "&", "'", "(", ")", "*", ";", "<", ">", "?", "[", "\\",
        "]", "^", "`", "{", "|", "}", "~",
    ];
    for ch in unsafe_chars {
        assert_eq!(
            shell_backslash_escape(ch),
            format!("\\{ch}"),
            "expected {ch:?} to be backslash-escaped"
        );
    }
}

#[test]
fn backslash_escape_non_ascii_unchanged() {
    assert_eq!(shell_backslash_escape("café🫠.png"), "café🫠.png");
}

#[test]
fn backslash_escape_non_ascii_with_space_only_space_escaped() {
    assert_eq!(shell_backslash_escape("café 🫠.png"), r#"café\ 🫠.png"#);
}

#[test]
fn backslash_escape_real_world_mac_path() {
    assert_eq!(
        shell_backslash_escape("/Users/nick/Documents/My File (final).txt"),
        r#"/Users/nick/Documents/My\ File\ \(final\).txt"#
    );
}

#[test]
fn backslash_escape_single_backslash_escaped_to_double() {
    assert_eq!(shell_backslash_escape(r#"\"#), r#"\\"#);
}

#[test]
fn backslash_escape_single_quote_prefixed() {
    assert_eq!(shell_backslash_escape("it's.txt"), r#"it\'s.txt"#);
}

#[test]
fn backslash_escape_mixed_sequence_ordering_preserved() {
    assert_eq!(shell_backslash_escape("a b'c(d)e"), r#"a\ b\'c\(d\)e"#);
}

#[test]
fn backslash_escape_temp_image_path_unchanged() {
    let path =
        "/private/var/folders/ab/cd/T/Nice/dropped-images/DEAD-BEEF-1234-5678.png";
    assert_eq!(shell_backslash_escape(path), path);
}
