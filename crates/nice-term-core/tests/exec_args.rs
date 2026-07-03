//! Exec-argv byte-assert, ported from
//! `Tests/NiceUnitTests/TabPtySessionExecArgsTests.swift`, plus the
//! quoting-table → exec-argv byte-compare the plan calls for (paths with
//! spaces, quotes, `$`, unicode). The `exec` form is load-bearing — without it,
//! quitting vim drops back to a zsh prompt instead of closing the pane — so
//! these pins are cheap insurance the carve-out is not rewritten back inline.

use nice_term_core::{build_argv, build_exec_args, shell_single_quote, ZSH_PATH};

#[test]
fn build_exec_args_none_returns_login_shell() {
    assert_eq!(build_exec_args(None), vec!["-il".to_string()]);
}

#[test]
fn build_exec_args_simple_command_wraps_in_exec() {
    assert_eq!(
        build_exec_args(Some("vim '/tmp/x.md'")),
        vec!["-ilc".to_string(), "exec vim '/tmp/x.md'".to_string()]
    );
}

#[test]
fn build_exec_args_command_with_editor_args_passes_through_verbatim() {
    // `nvim -p` opens files in tabs. The args must reach nvim unchanged — no
    // extra quoting, no shell wrapping that re-tokenises them.
    assert_eq!(
        build_exec_args(Some("nvim -p '/Users/me/foo.swift'")),
        vec![
            "-ilc".to_string(),
            "exec nvim -p '/Users/me/foo.swift'".to_string()
        ]
    );
}

#[test]
fn build_exec_args_empty_command_still_wraps_in_exec() {
    // Empty-string command means "exec nothing", which zsh rejects at runtime.
    // Pin the structural contract — we do not silently switch to `-il`.
    assert_eq!(
        build_exec_args(Some("")),
        vec!["-ilc".to_string(), "exec ".to_string()]
    );
}

#[test]
fn build_argv_prepends_zsh_path() {
    // The full argv handed to execve is argv[0] = /bin/zsh followed by the exec
    // args. argv[0] is the plain path (login-ness comes from `-l`, not a
    // leading-dash argv[0]).
    assert_eq!(build_argv(None), vec![ZSH_PATH.to_string(), "-il".to_string()]);
    assert_eq!(
        build_argv(Some("vim")),
        vec![ZSH_PATH.to_string(), "-ilc".to_string(), "exec vim".to_string()]
    );
}

/// The quoting-table → exec-argv byte-compare: a path is single-quoted, spliced
/// into a command, and the resulting exec argv is asserted verbatim. Covers
/// spaces, embedded single quotes, `$`, and unicode.
#[test]
fn quoting_table_produces_expected_exec_argv() {
    struct Case {
        raw_path: &'static str,
        expected_argv: Vec<String>,
    }

    let cases = vec![
        Case {
            // space in path
            raw_path: "/tmp/a b.md",
            expected_argv: vec![
                "-ilc".to_string(),
                "exec vim '/tmp/a b.md'".to_string(),
            ],
        },
        Case {
            // embedded single quote → '\'' close-open-escape
            raw_path: "/tmp/it's.md",
            expected_argv: vec![
                "-ilc".to_string(),
                r#"exec vim '/tmp/it'\''s.md'"#.to_string(),
            ],
        },
        Case {
            // `$` and backtick are literal inside single quotes
            raw_path: "/tmp/$HOME `x`.md",
            expected_argv: vec![
                "-ilc".to_string(),
                "exec vim '/tmp/$HOME `x`.md'".to_string(),
            ],
        },
        Case {
            // unicode passes through untouched
            raw_path: "/tmp/café 🫠.md",
            expected_argv: vec![
                "-ilc".to_string(),
                "exec vim '/tmp/café 🫠.md'".to_string(),
            ],
        },
    ];

    for case in cases {
        let command = format!("vim {}", shell_single_quote(case.raw_path));
        assert_eq!(
            build_exec_args(Some(&command)),
            case.expected_argv,
            "exec argv mismatch for path {:?}",
            case.raw_path
        );
    }
}
