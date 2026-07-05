//! Shell injection — the synthetic `ZDOTDIR` rc chain (R14).
//!
//! Ports Swift `MainTerminalShellInject`
//! (`Sources/Nice/Process/MainTerminalShellInject.swift`). We write a `ZDOTDIR`
//! directory the Main Terminal's zsh picks up. It contains stub `.zshenv` /
//! `.zprofile` / `.zlogin` / `.zshrc` that chain back to the user's real startup
//! files (resolved from `$NICE_USER_ZDOTDIR` if set, else by sourcing
//! `~/.zshenv` to discover the user's intended `ZDOTDIR`), then — in `.zshrc` —
//! restore `ZDOTDIR` to that intended value BEFORE sourcing the user's `.zshrc`
//! and define a `claude()` function that intercepts *interactive* invocations
//! and forwards them to Nice's control socket so a new tab opens instead of the
//! shell exec'ing claude in place.
//!
//! The "restore `ZDOTDIR` *before sourcing user's .zshrc*" dance is what stops
//! shell tools (Powerlevel10k, oh-my-zsh, nvm, asdf, starship init…) from
//! scribbling on our temp dir when they probe `${ZDOTDIR:-$HOME}/...` — both at
//! the interactive prompt AND during the user's `.zshrc` init (oh-my-zsh sets
//! `ZSH_COMPDUMP="${ZDOTDIR:-$HOME}/.zcompdump-..."` at load time, p10k sources
//! `${ZDOTDIR:-$HOME}/.p10k.zsh`, etc.). Ordering "restore → source user's
//! .zshrc → install our hooks" gives correctness for those init-time probes and
//! lets our `claude()` shadow / OSC 7 hook layer on top of (and survive)
//! anything the user defines.
//!
//! Documented limitations (kept as-is, NOT fixed — see the Swift header):
//!   * `exec zsh` inside a Nice pane drops the injection (the new zsh runs with
//!     the user's restored `ZDOTDIR`, not our temp value).
//!   * `/etc/zshenv` setting `ZDOTDIR` bypasses the injection entirely (zsh
//!     re-resolves `$ZDOTDIR/.zshenv` from the new value before reading our
//!     stub). macOS ships no `/etc/zshenv`, so this is documented, not fixed.
//!
//! Storage location: the stubs live in a fixed, per-variant directory under
//! Application Support (`…/<CFBundleName>/zdotdir`) — NOT `$TMPDIR`. macOS's
//! `com.apple.bsd.dirhelper` sweeps `$TMPDIR` files older than 3 days; when Nice
//! ran longer than that, the sweep deleted the stubs out from under the live
//! process and every new pane's zsh then sourced nothing. Application Support is
//! never swept. Because the stub contents are static, one shared directory
//! serves every window and every process of a variant; `Nice RS Dev` stays
//! isolated from the Swift `Nice` / `Nice Dev` via `CFBundleName`.
//! [`write_stubs`] rewrites the stubs on every launch, so the directory
//! self-heals if anything ever removes a file.
//!
//! **The four rc-stub bodies below are a FROZEN compatibility contract.**
//! Installed helpers already on users' disks (`~/.nice/nice-claude-hook.sh`,
//! `~/.nice/nice-handoff.sh`) and the shadow function's muscle-memory behavior
//! must keep working byte-for-byte against the app. They are ported
//! character-for-character from the Swift source and pinned by both the
//! static-text tests and the real-zsh end-to-end tests below. Do not "clean
//! them up" — the `\%` OSC 7 escape is load-bearing zsh arcana (a bare `%` is a
//! parameter pattern anchor), and the `_nice_json_escape` dialect (backslash,
//! double-quote, LF, CR, tab — nothing else) is exactly what Nice's JSON decoder
//! expects.
//!
//! The `$NICE_SOCKET` env var the `claude()` function reads, and the per-pane
//! `NICE_TAB_ID` / `NICE_PANE_ID` / `NICE_USER_ZDOTDIR` / `NICE_PREFILL_COMMAND`
//! values, are injected separately (R14 slice 3 / slice 4); these stubs only
//! reference them. This module owns the stub text, the writer, and the
//! per-variant location; the `app::run` bootstrap wiring is R14 slice 3.

#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};

/// Stub `.zshenv`: discover + stash the user's intended `ZDOTDIR`, then restore
/// our temp dir so zsh keeps reading the other stubs.
pub const ZSHENV_BODY: &str = r#"# Nice: discover and stash the user's intended ZDOTDIR, then
# restore our temp dir so zsh keeps reading our other stubs
# (.zprofile / .zshrc). See file header for the cooperation contract.
NICE_TEMP_ZDOTDIR="$ZDOTDIR"
if [[ -n "$NICE_USER_ZDOTDIR" ]]; then
    # Inherited from Nice's launch env (launchctl / parent process).
    USER_ZDOTDIR="$NICE_USER_ZDOTDIR"
else
    # Source ~/.zshenv ourselves to discover any ZDOTDIR set there
    # (XDG-style). This is the FIRST source of ~/.zshenv this
    # session — zsh read OUR stub, not the user's, because ZDOTDIR
    # was overridden — so no double-source / non-idempotency risk.
    unset ZDOTDIR
    [[ -f "$HOME/.zshenv" ]] && source "$HOME/.zshenv"
    USER_ZDOTDIR="${ZDOTDIR:-$HOME}"
fi
export ZDOTDIR="$NICE_TEMP_ZDOTDIR"
export NICE_USER_ZDOTDIR="$USER_ZDOTDIR"
unset NICE_TEMP_ZDOTDIR USER_ZDOTDIR"#;

/// Stub `.zprofile`: chain back to the user's real `.zprofile` (login shells).
pub const ZPROFILE_BODY: &str = r#"# Nice: source the user's real .zprofile from the location resolved
# in our .zshenv. (Without this, login-shell users silently lose
# .zprofile because zsh's $ZDOTDIR/.zprofile lookup hits our stub.)
[[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zprofile" ]] \
    && source "$NICE_USER_ZDOTDIR/.zprofile""#;

/// Stub `.zlogin`: defensive chain-back to the user's real `.zlogin`.
pub const ZLOGIN_BODY: &str = r#"# Nice: defensive — if our .zshrc somehow exited before restoring
# ZDOTDIR (user .zshrc errored out, etc.), source the user's real
# .zlogin from where they actually keep it. In the success path
# ZDOTDIR has already been restored to the user's value by our
# .zshrc, so zsh reads the user's .zlogin directly and this stub
# is never reached.
[[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zlogin" ]] \
    && source "$NICE_USER_ZDOTDIR/.zlogin""#;

/// Stub `.zshrc`: restore `ZDOTDIR` before sourcing the user's `.zshrc`, then
/// install the `claude()` shadow, the OSC 7 cwd emitter, and the prefill tail.
pub const ZSHRC_BODY: &str = r#"# Stash the resolved user-side ZDOTDIR before we drop NICE_USER_ZDOTDIR.
# Trim trailing slashes so an accidental "/Users/nick/" (from launchctl
# or weird shells) compares equal to "/Users/nick" for the unset branch.
NICE_RESOLVED_USER_ZDOTDIR="${NICE_USER_ZDOTDIR%/}"

# Restore ZDOTDIR to the user's intended value BEFORE sourcing
# their .zshrc — so anything they pull in during init (oh-my-zsh
# `ZSH_COMPDUMP="${ZDOTDIR:-$HOME}/.zcompdump-..."`, p10k's
# `source "${ZDOTDIR:-$HOME}/.p10k.zsh"`, plugin-manager caches,
# etc.) probes the user's real config path instead of our temp
# dir. The whole point of this PR is closing that gap; restoring
# after the source would only fix tools the user runs at the
# interactive prompt, not the much larger surface of init-time
# tooling.
if [[ "$NICE_RESOLVED_USER_ZDOTDIR" == "${HOME%/}" ]]; then
    unset ZDOTDIR    # match standard convention when $HOME resolves
else
    export ZDOTDIR="$NICE_RESOLVED_USER_ZDOTDIR"
fi
unset NICE_USER_ZDOTDIR

# Source the user's real .zshrc from where they actually keep it
# (handles XDG-style ZDOTDIR layouts under e.g. ~/.config/zsh).
[[ -n "$NICE_RESOLVED_USER_ZDOTDIR" && -f "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc" ]] \
    && source "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc"
unset NICE_RESOLVED_USER_ZDOTDIR

# Now shadow `claude` so running it handshakes with Nice over
# NICE_SOCKET. The socket either tells us to exit (Nice is opening
# a new tab) or to exec claude in place (Nice is promoting this
# pane to Claude). Defining the function AFTER user's .zshrc
# ensures we win over anything they may have defined themselves —
# if a user wants to opt out, they can still `unfunction claude`
# in a precmd hook.
_nice_json_escape() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    printf '"%s"' "$s"
}

claude() {
    # Passthrough to the real binary (no handshake) when:
    #   1. Not inside a Nice pty ($NICE_SOCKET unset).
    #   2. stdin is piped — caller is streaming input to claude.
    #   3. User passed a flag that makes claude non-interactive.
    #   4. User invoked a non-interactive subcommand.
    if [[ -z "$NICE_SOCKET" ]]; then
        command claude "$@"
        return
    fi
    if [[ ! -t 0 ]]; then
        command claude "$@"
        return
    fi
    local a
    for a in "$@"; do
        case "$a" in
            -p|--print|-h|--help|--version|--output-format|--output-format=*)
                command claude "$@"
                return
                ;;
        esac
    done
    case "${1-}" in
        mcp|config|migrate-installer|update|doctor)
            command claude "$@"
            return
            ;;
    esac

    local args_json="["
    local first=1
    for a in "$@"; do
        [[ $first -eq 1 ]] || args_json+=","
        args_json+=$(_nice_json_escape "$a")
        first=0
    done
    args_json+="]"

    # Send {cwd, args, tabId, paneId} and read a single-line reply.
    # NICE_TAB_ID / NICE_PANE_ID are empty in the Main Terminal —
    # Nice uses empty tabId as the signal for "always open a new
    # sidebar tab."
    local cwd_json tab_id_json pane_id_json
    cwd_json=$(_nice_json_escape "$PWD")
    tab_id_json=$(_nice_json_escape "${NICE_TAB_ID:-}")
    pane_id_json=$(_nice_json_escape "${NICE_PANE_ID:-}")
    local payload="{\"action\":\"claude\",\"cwd\":${cwd_json},\"args\":${args_json},\"tabId\":${tab_id_json},\"paneId\":${pane_id_json}}"

    local response
    response=$(printf '%s\n' "$payload" | nc -U "$NICE_SOCKET" -w 2 2>/dev/null)
    if [[ -z "$response" ]]; then
        print -u2 "nice: control socket unreachable; running claude directly"
        exec command claude "$@"
    fi

    local mode sid settings
    read -r mode sid settings <<< "$response"
    case "$mode" in
        newtab)
            # Nice is opening a new sidebar tab; nothing to do here.
            return 0
            ;;
        inplace)
            # Nice promoted this pane to Claude. Build the exec line:
            #   --settings <path>  when Nice's theme sync is on (the
            #     3rd reply field), so this in-place Claude matches
            #     the Nice theme like a from-scratch Nice Claude pane;
            #   --session-id <sid> when Nice minted an id so it can
            #     resume later. A sid of "-" (or empty) means the
            #     user's own args (e.g. --resume <uuid>) already
            #     identify the session, so no --session-id is added.
            local -a pre=()
            [[ -n "$settings" ]] && pre+=(--settings "$settings")
            [[ -n "$sid" && "$sid" != "-" ]] && pre+=(--session-id "$sid")
            # Guard the expansion so an empty `pre` never trips the
            # user's `setopt nounset` (and never injects an empty arg).
            if (( ${#pre} )); then
                exec command claude "${pre[@]}" "$@"
            else
                exec command claude "$@"
            fi
            ;;
        *)
            print -u2 "nice: unexpected response '$response'; running claude directly"
            exec command claude "$@"
            ;;
    esac
}

# Nice: emit OSC 7 (current working directory) on every cd so the
# host terminal can capture and persist it. Format:
#   ESC ] 7 ; file://hostname/path BEL
# The injected hook appends to chpwd_functions rather than replacing
# chpwd directly so anything the user already registered (in their
# real .zshrc, sourced above) keeps firing.
_nice_emit_cwd_osc7() {
    # Minimal URL encoding: % first (so we don't double-encode the
    # %20 we're about to emit), then space. macOS paths almost
    # never need more; anything exotic (?, #, non-ASCII) flows
    # through unencoded and SwiftTerm tolerates the raw bytes.
    # The `\%` escape is load-bearing — a bare `%` in a zsh
    # parameter pattern is the "anchor at end of string" matcher,
    # which makes `${PWD//%/%25}` append `%25` to every path.
    local p=${PWD//\%/%25}
    p=${p// /%20}
    printf '\e]7;file://%s%s\a' "${HOST}" "$p"
}
typeset -ga chpwd_functions
chpwd_functions+=(_nice_emit_cwd_osc7)
# Fire once at shell startup so the initial cwd is reported even
# if the user never cd's.
_nice_emit_cwd_osc7

# Nice: if the app asked us to pre-type a command at the next
# prompt (set when a restored Claude tab boots), push it onto zsh's
# line-editor buffer. The user sees the command typed and ready;
# nothing runs until they hit Enter.
if [[ -n "$NICE_PREFILL_COMMAND" ]]; then
    print -z "$NICE_PREFILL_COMMAND"
fi"#;

/// Write the four `ZDOTDIR` stubs into `dir`, creating it (and any missing
/// parents) if needed, and return `dir`. Ports Swift
/// `MainTerminalShellInject.make(at:)`. Every file is (over)written every call
/// so the directory self-heals if a stub was ever removed; each write is atomic
/// (temp sibling + rename) so a pty child mid-`source` never reads a half-written
/// stub when a second window/process of the same variant rewrites the shared dir.
pub fn write_stubs(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    write_atomic(&dir.join(".zshenv"), ZSHENV_BODY)?;
    write_atomic(&dir.join(".zprofile"), ZPROFILE_BODY)?;
    write_atomic(&dir.join(".zlogin"), ZLOGIN_BODY)?;
    write_atomic(&dir.join(".zshrc"), ZSHRC_BODY)?;
    Ok(dir.to_path_buf())
}

/// Atomically replace `path` with `contents`: write to a pid-suffixed sibling in
/// the same directory, then rename over the target (rename is atomic within a
/// filesystem). The pid suffix keeps two concurrent same-variant processes from
/// colliding on the temp name.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stub");
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// The fixed, per-variant `ZDOTDIR` location:
/// `<app support root>/<CFBundleName>/zdotdir`. Ports Swift
/// `MainTerminalShellInject.defaultLocation()`. Honors the
/// `NICE_APPLICATION_SUPPORT_ROOT` override seam (tests redirect it into a
/// sandbox; production leaves it unset). The folder name tracks `CFBundleName`
/// so `Nice RS Dev` gets its own directory, never shared with the Swift builds.
/// Pure — creates nothing (unlike the Swift `FileManager.url(create:true)`,
/// which is unnecessary here because [`write_stubs`] creates the dir).
pub fn default_location() -> PathBuf {
    let override_value = std::env::var("NICE_APPLICATION_SUPPORT_ROOT").ok();
    let home = std::env::var("HOME").ok();
    application_support_root(override_value.as_deref(), home.as_deref())
        .join(bundle_folder_name())
        .join("zdotdir")
}

/// Resolve the Application Support root. The `NICE_APPLICATION_SUPPORT_ROOT`
/// override wins when present and non-empty (the test seam); otherwise
/// `<home>/Library/Application Support`. Factored out of [`default_location`] so
/// the override seam is unit-tested without mutating the process environment.
fn application_support_root(override_value: Option<&str>, home: Option<&str>) -> PathBuf {
    if let Some(root) = override_value {
        if !root.is_empty() {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(home.unwrap_or("/")).join("Library/Application Support")
}

/// The per-variant folder name: the running app's `CFBundleName`
/// (`"Nice RS Dev"` for the shipped bundle), falling back to `"Nice RS Dev"`
/// when unbundled — never `"Nice"` / `"Nice Dev"`, so an unbundled `cargo run`
/// can never collide with the Swift builds' Application Support.
fn bundle_folder_name() -> String {
    crate::platform::main_bundle_name().unwrap_or_else(|| "Nice RS Dev".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---- temp-dir plumbing -------------------------------------------------

    /// A throwaway directory removed on drop (mirrors Swift's
    /// `addTeardownBlock { removeItem }`). Its `Drop` runs on normal test exit;
    /// a panicking assertion leaves the temp dir behind, which is harmless.
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unique(prefix: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()))
    }

    /// A fresh empty scratch directory.
    fn scratch(prefix: &str) -> Scratch {
        let dir = unique(prefix);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Write the four stubs into a throwaway `ZDOTDIR` (auto-removed) and return
    /// it — the twin of Swift `makeIsolated()`.
    fn make_isolated() -> Scratch {
        let dir = unique("nice-rs-zdotdir-test");
        write_stubs(&dir).expect("write stubs");
        Scratch(dir)
    }

    /// Read one stub file's contents after writing the stubs to disk (exercises
    /// the writer round-trip, like Swift's read-after-`make`).
    fn read_stub(name: &str) -> String {
        let dir = make_isolated();
        std::fs::read_to_string(dir.0.join(name)).expect("read stub")
    }

    fn zshrc() -> String {
        read_stub(".zshrc")
    }

    // ---- file layout -------------------------------------------------------

    #[test]
    fn make_creates_all_four_stubs() {
        let dir = make_isolated();
        for name in [".zshenv", ".zprofile", ".zlogin", ".zshrc"] {
            assert!(
                dir.0.join(name).is_file(),
                "expected ZDOTDIR to contain {name}"
            );
        }
    }

    /// The writer must round-trip the FROZEN constants byte-for-byte — a writer
    /// bug that mangled the stub text would silently break the socket handshake.
    #[test]
    fn writer_round_trips_frozen_bytes() {
        let dir = make_isolated();
        for (name, body) in [
            (".zshenv", ZSHENV_BODY),
            (".zprofile", ZPROFILE_BODY),
            (".zlogin", ZLOGIN_BODY),
            (".zshrc", ZSHRC_BODY),
        ] {
            let on_disk = std::fs::read_to_string(dir.0.join(name)).expect("read");
            assert_eq!(on_disk, body, "{name} on disk must equal the frozen const");
        }
    }

    /// The ZDOTDIR must live under Application Support, NOT `$TMPDIR` (which
    /// macOS sweeps after 3 days), and be stable across calls so the dir is
    /// reused rather than re-namespaced per launch.
    #[test]
    fn default_location_is_under_app_support_not_temp() {
        let dir = default_location();
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("zdotdir"),
            "ZDOTDIR directory should be named `zdotdir`"
        );
        assert!(
            !dir.starts_with(std::env::temp_dir()),
            "ZDOTDIR must not live in $TMPDIR (macOS dirhelper sweeps it after 3 days). Got: {dir:?}"
        );
        let parent = dir.parent().expect("has parent");
        assert!(
            parent.to_string_lossy().contains("Application Support"),
            "ZDOTDIR should live under Application Support. Got: {dir:?}"
        );
        assert_eq!(
            dir,
            default_location(),
            "ZDOTDIR location must be stable across calls (one shared, reused dir)"
        );
    }

    // ---- the NICE_APPLICATION_SUPPORT_ROOT override seam -------------------
    //
    // Driven through the pure `application_support_root` so no process env is
    // mutated (which would race parallel tests).

    #[test]
    fn override_root_wins_when_set() {
        assert_eq!(
            application_support_root(Some("/sandbox/appsup"), Some("/home/u")),
            PathBuf::from("/sandbox/appsup")
        );
    }

    #[test]
    fn empty_override_falls_back_to_home() {
        assert_eq!(
            application_support_root(Some(""), Some("/home/u")),
            PathBuf::from("/home/u/Library/Application Support")
        );
    }

    #[test]
    fn default_root_uses_home_app_support() {
        assert_eq!(
            application_support_root(None, Some("/home/u")),
            PathBuf::from("/home/u/Library/Application Support")
        );
    }

    // ---- chain-back stubs --------------------------------------------------

    /// `.zprofile`, `.zlogin`, `.zshrc` chain back through the resolved user-side
    /// ZDOTDIR var so XDG-style layouts and `~/.zshenv`-set values are honored.
    #[test]
    fn chain_backs_source_from_user_zdotdir() {
        for (filename, var) in [
            (".zprofile", "NICE_USER_ZDOTDIR"),
            (".zlogin", "NICE_USER_ZDOTDIR"),
            (".zshrc", "NICE_RESOLVED_USER_ZDOTDIR"),
        ] {
            let body = read_stub(filename);
            let needle = format!(r#"source "${var}/{filename}""#);
            assert!(
                body.contains(&needle),
                "{filename} must source ${var}/{filename}"
            );
        }
    }

    /// `.zshenv` discovers the user's intended ZDOTDIR (preferring
    /// `$NICE_USER_ZDOTDIR`, falling back to sourcing `~/.zshenv`), then restores
    /// `$ZDOTDIR` to our temp dir so zsh keeps reading our other stubs.
    #[test]
    fn zshenv_discovers_user_zdotdir() {
        let body = read_stub(".zshenv");
        assert!(
            body.contains(r#"if [[ -n "$NICE_USER_ZDOTDIR" ]]; then"#),
            ".zshenv must branch on NICE_USER_ZDOTDIR"
        );
        assert!(
            body.contains(r#"source "$HOME/.zshenv""#),
            ".zshenv must source ~/.zshenv as the fallback discovery path"
        );
        assert!(
            body.contains(r#"export ZDOTDIR="$NICE_TEMP_ZDOTDIR""#),
            ".zshenv must restore ZDOTDIR to our temp value"
        );
        assert!(
            body.contains(r#"export NICE_USER_ZDOTDIR="$USER_ZDOTDIR""#),
            ".zshenv must persist the resolved value back into NICE_USER_ZDOTDIR"
        );
    }

    /// `.zshrc` restores `$ZDOTDIR` to the user's value BEFORE sourcing their
    /// `.zshrc`, and installs `claude()` AFTER, so our hooks win.
    #[test]
    fn zshrc_restores_user_zdotdir_before_sourcing() {
        let body = zshrc();
        let restore = body
            .find(r#"export ZDOTDIR="$NICE_RESOLVED_USER_ZDOTDIR""#)
            .expect("restore marker present");
        let source = body
            .find(r#"source "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc""#)
            .expect("source marker present");
        let claude = body.find("claude() {").expect("claude marker present");
        assert!(
            restore < source,
            ".zshrc must restore ZDOTDIR BEFORE sourcing user's .zshrc"
        );
        assert!(
            source < claude,
            ".zshrc must source user's .zshrc BEFORE installing claude()"
        );
        assert!(
            body.contains("unset NICE_USER_ZDOTDIR"),
            ".zshrc must clear NICE_USER_ZDOTDIR"
        );
        assert!(
            body.contains(r#"if [[ "$NICE_RESOLVED_USER_ZDOTDIR" == "${HOME%/}" ]]; then"#)
                && body.contains("unset ZDOTDIR"),
            ".zshrc must unset (not export) ZDOTDIR when the resolved value matches $HOME"
        );
    }

    // ---- .zshrc shell wrapper contract ------------------------------------

    #[test]
    fn zshrc_defines_claude_function() {
        assert!(
            zshrc().contains("claude() {"),
            "zshrc must shadow `claude` with a function"
        );
    }

    #[test]
    fn zshrc_defines_json_escape_helper() {
        let body = zshrc();
        assert!(body.contains("_nice_json_escape()"), "JSON escape helper required");
        assert!(
            body.contains(r#"s=${s//\\/\\\\}"#),
            "escape must replace backslashes first"
        );
        assert!(
            body.contains(r#"s=${s//\"/\\\"}"#),
            "escape must replace double quotes"
        );
        assert!(body.contains(r#"$'\n'"#), "escape must handle embedded newlines");
    }

    #[test]
    fn zshrc_handshake_payload_shape() {
        let body = zshrc();
        assert!(
            body.contains(r#""action":"claude""#) || body.contains(r#"\"action\":\"claude\""#),
            "payload must label itself as the claude action"
        );
        assert!(body.contains("cwd"), "payload must include cwd");
        assert!(body.contains("args"), "payload must include args");
        assert!(body.contains("tabId"), "payload must include tabId");
        assert!(body.contains("paneId"), "payload must include paneId");
    }

    #[test]
    fn zshrc_uses_nc_with_socket_path() {
        assert!(
            zshrc().contains(r#"nc -U "$NICE_SOCKET""#),
            "must speak AF_UNIX to Nice's control socket via nc -U"
        );
    }

    #[test]
    fn zshrc_dispatches_newtab_and_inplace_modes() {
        let body = zshrc();
        assert!(body.contains("newtab)"), "wrapper must handle the `newtab` mode");
        assert!(body.contains("inplace)"), "wrapper must handle the `inplace` mode");
        assert!(
            body.contains(r#"pre+=(--session-id "$sid")"#),
            "inplace must splice --session-id"
        );
        assert!(
            body.contains(r#"pre+=(--settings "$settings")"#),
            "inplace must splice --settings"
        );
    }

    #[test]
    fn zshrc_socket_unreachable_falls_back_to_command() {
        let body = zshrc();
        assert!(
            body.contains("control socket unreachable"),
            "must warn when the socket is gone"
        );
        assert!(
            body.contains(r#"exec command claude "$@""#),
            "unreachable socket must fall back to running claude directly"
        );
    }

    #[test]
    fn zshrc_non_interactive_flags_short_circuit_to_command() {
        let body = zshrc();
        for flag in ["-p", "--print", "-h", "--help", "--version", "--output-format"] {
            assert!(
                body.contains(flag),
                "non-interactive flag {flag} must be short-circuited"
            );
        }
    }

    #[test]
    fn zshrc_non_interactive_subcommands_short_circuit() {
        let body = zshrc();
        for sub in ["mcp", "config", "migrate-installer", "update", "doctor"] {
            assert!(
                body.contains(sub),
                "non-interactive subcommand {sub} must be short-circuited"
            );
        }
    }

    #[test]
    fn zshrc_prefill_command_uses_print_z() {
        assert!(
            zshrc().contains(r#"print -z "$NICE_PREFILL_COMMAND""#),
            "restored Claude tabs rely on print -z to pre-type the resume command"
        );
    }

    #[test]
    fn zshrc_no_handshake_when_socket_unset() {
        assert!(
            zshrc().contains(r#"if [[ -z "$NICE_SOCKET" ]]"#),
            "missing NICE_SOCKET must bypass the wrapper entirely"
        );
    }

    // ---- OSC 7 cwd-update emitter -----------------------------------------

    #[test]
    fn zshrc_defines_osc7_emitter() {
        assert!(
            zshrc().contains("_nice_emit_cwd_osc7()"),
            "zshrc must define the OSC 7 emitter"
        );
    }

    #[test]
    fn zshrc_emitter_hooks_into_chpwd_functions() {
        assert!(
            zshrc().contains("chpwd_functions+=(_nice_emit_cwd_osc7)"),
            "emitter must append to chpwd_functions to fire on every cd"
        );
    }

    #[test]
    fn zshrc_emitter_fires_once_at_shell_start() {
        let body = zshrc();
        let plain_call = body.lines().any(|l| l.trim() == "_nice_emit_cwd_osc7");
        assert!(
            plain_call,
            "emitter must be invoked as a bare statement to capture spawn cwd"
        );
    }

    /// A bare `%` in zsh's `${var//pattern/repl}` is the end-of-string anchor
    /// matcher — it would append `%25` to every path. The backslash escape forces
    /// literal interpretation. Assert on the actual substitution line so comments
    /// mentioning the bare form don't trip the negative check.
    #[test]
    fn zshrc_percent_escape_is_literal_pattern() {
        let body = zshrc();
        let assign = body
            .lines()
            .find(|l| l.contains("local p=") && l.contains("PWD"))
            .unwrap_or("");
        assert!(
            assign.contains(r#"${PWD//\%/%25}"#),
            "% in the substitution pattern must be backslash-escaped. Got: <{assign}>"
        );
        assert!(
            !assign.contains(r#"${PWD//%/%25}"#),
            "bare `%` in the substitution line is the end-of-string anchor. Got: <{assign}>"
        );
    }

    #[test]
    fn zshrc_emitter_format_is_osc7_file_url() {
        assert!(
            zshrc().contains(r#"printf '\e]7;file://%s%s\a'"#),
            "emitter must produce a well-formed OSC 7 file:// URL terminated with BEL"
        );
    }

    // ---- real-zsh end-to-end ----------------------------------------------

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.len() > haystack.len() {
            return None;
        }
        (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
    }

    /// Launch a real `/bin/zsh` under the synthetic ZDOTDIR with a controlled
    /// `$HOME` and env, returning its stdout. `login_shell` runs `-ilc` so
    /// `.zprofile` / `.zlogin` lookups fire. `NICE_USER_ZDOTDIR` is always set —
    /// empty string when `None` — to match production (which always sets it).
    /// The env is fully replaced (`env_clear`), mirroring Swift's
    /// `proc.environment = [...]`, so no ambient `ZDOTDIR` / `NICE_SOCKET` leaks
    /// into the child. Never touches the real `$HOME`.
    fn run_zsh_under_injection(
        home: &Path,
        nice_user_zdotdir: Option<&str>,
        commands: &str,
        login_shell: bool,
    ) -> String {
        let zdotdir = make_isolated();
        let out = Command::new("/bin/zsh")
            .arg(if login_shell { "-ilc" } else { "-ic" })
            .arg(commands)
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOST", "test.local")
            .env("NICE_USER_ZDOTDIR", nice_user_zdotdir.unwrap_or(""))
            .current_dir(home)
            .output()
            .expect("spawn /bin/zsh");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// End-to-end: launch real zsh and confirm the OSC 7 payload contains the
    /// actual cwd without spurious bytes (the `%` regression sentinel). Uses an
    /// empty sandbox `$HOME` so no user dotfiles are sourced — the emitter still
    /// fires from our stub.
    #[test]
    fn zshrc_emitter_produces_clean_osc7_at_runtime() {
        let zdotdir = make_isolated();
        let home = scratch("nice-rs-osc7-home");
        let workcwd = scratch("nice-rs-osc7-work");

        let out = Command::new("/bin/zsh")
            .arg("-ic")
            .arg("exit")
            .env_clear()
            .env("ZDOTDIR", &zdotdir.0)
            .env("HOME", &home.0)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOST", "test.local")
            .current_dir(&workcwd.0)
            .output()
            .expect("spawn /bin/zsh");
        let bytes = out.stdout;

        let osc_start = find_subsequence(&bytes, &[0x1b, 0x5d, 0x37, 0x3b])
            .unwrap_or_else(|| panic!("zsh did not emit OSC 7. Captured: {:?}", &bytes));
        let payload_start = osc_start + 4;
        let bel_rel = bytes[payload_start..]
            .iter()
            .position(|&b| b == 0x07)
            .expect("OSC 7 emission missing BEL terminator");
        let payload =
            String::from_utf8_lossy(&bytes[payload_start..payload_start + bel_rel]).into_owned();

        assert!(
            payload.starts_with("file://"),
            "OSC 7 payload must be a file:// URL. Got: <{payload}>"
        );
        // The workdir has no space/percent, so a clean encoding leaves the last
        // component intact and the whole payload percent-free.
        let last = workcwd.0.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(
            payload.contains(last),
            "payload path must contain the cwd's last component ({last}). Got: <{payload}>"
        );
        assert!(
            !payload.contains('%'),
            "decoded path must not contain `%`. Got: <{payload}>"
        );
    }

    /// THE bug: oh-my-zsh / p10k probe `${ZDOTDIR:-$HOME}/...` while the user's
    /// `.zshrc` is being sourced. ZDOTDIR must already be the user's value at
    /// that point (restored BEFORE the source), not our temp dir.
    #[test]
    fn end_to_end_user_zshrc_sees_restored_zdotdir_during_init() {
        let home = scratch("nice-rs-e2e-home");
        std::fs::write(
            home.0.join(".zshrc"),
            "touch \"${ZDOTDIR:-$HOME}/.during-zshrc-marker\"\n\
             print -r -- \"DURING_ZSHRC_ZDOTDIR=${ZDOTDIR-<unset>}\"\n",
        )
        .unwrap();

        let out = run_zsh_under_injection(&home.0, None, "true", false);

        assert!(
            out.contains("DURING_ZSHRC_ZDOTDIR=<unset>"),
            "ZDOTDIR must be restored BEFORE sourcing user's .zshrc. Output: <{out}>"
        );
        assert!(
            home.0.join(".during-zshrc-marker").is_file(),
            "files written via ${{ZDOTDIR:-$HOME}}/... during user's .zshrc must land in real $HOME"
        );
    }

    /// Default case: no NICE_USER_ZDOTDIR, no custom ZDOTDIR in `~/.zshenv`. The
    /// injection resolves ZDOTDIR to $HOME (unset), so tooling writes to real home.
    #[test]
    fn end_to_end_default_user_zdotdir_resolves_to_home() {
        let home = scratch("nice-rs-e2e-home");

        let out = run_zsh_under_injection(
            &home.0,
            None,
            "touch \"${ZDOTDIR:-$HOME}/.p10k.zsh\"\n\
             print -r -- \"FINAL_ZDOTDIR=${ZDOTDIR-<unset>}\"",
            false,
        );

        assert!(
            out.contains("FINAL_ZDOTDIR=<unset>"),
            "default user: expected ZDOTDIR unset by .zshrc restore. Output: <{out}>"
        );
        assert!(
            home.0.join(".p10k.zsh").is_file(),
            "expected .p10k.zsh to land in the real home, not our temp dir"
        );
    }

    /// XDG-style: user sets `export ZDOTDIR=~/.config/zsh` in `~/.zshenv`. The
    /// injection sources that during discovery and resolves to the custom path.
    #[test]
    fn end_to_end_xdg_style_zdotdir_honored_from_zshenv() {
        let home = scratch("nice-rs-e2e-home");
        let custom = home.0.join(".config/zsh");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(home.0.join(".zshenv"), r#"export ZDOTDIR="$HOME/.config/zsh""#).unwrap();
        std::fs::write(custom.join(".zshrc"), "echo NICE-XDG-ZSHRC-LOADED").unwrap();

        let out = run_zsh_under_injection(
            &home.0,
            None,
            r#"print -r -- "FINAL_ZDOTDIR=$ZDOTDIR""#,
            false,
        );

        assert!(
            out.contains("NICE-XDG-ZSHRC-LOADED"),
            "custom ZDOTDIR's .zshrc must be sourced. Output: <{out}>"
        );
        assert!(
            out.contains(&format!("FINAL_ZDOTDIR={}", custom.display())),
            "ZDOTDIR must be restored to the user's intended XDG path. Output: <{out}>"
        );
    }

    /// Login-shell bonus fix: our `.zprofile` chains through
    /// `$NICE_USER_ZDOTDIR/.zprofile` so login-shell users keep their `~/.zprofile`.
    #[test]
    fn end_to_end_login_shell_sources_user_zprofile() {
        let home = scratch("nice-rs-e2e-home");
        std::fs::write(home.0.join(".zprofile"), "echo NICE-ZPROFILE-LOADED").unwrap();

        let out = run_zsh_under_injection(&home.0, None, "true", true);

        assert!(
            out.contains("NICE-ZPROFILE-LOADED"),
            "login shells must source ~/.zprofile through the synthetic stub. Output: <{out}>"
        );
    }

    /// launchctl-style: Nice inherited a ZDOTDIR from its launch env, passed as
    /// NICE_USER_ZDOTDIR; the shell restores that value verbatim.
    #[test]
    fn end_to_end_launchctl_style_zdotdir_honored_from_env() {
        let home = scratch("nice-rs-e2e-home");
        let custom = home.0.join("launchctl-zsh");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join(".zshrc"), "echo NICE-LAUNCHCTL-ZSHRC-LOADED").unwrap();

        let out = run_zsh_under_injection(
            &home.0,
            Some(custom.to_str().unwrap()),
            r#"print -r -- "FINAL_ZDOTDIR=$ZDOTDIR""#,
            false,
        );

        assert!(
            out.contains("NICE-LAUNCHCTL-ZSHRC-LOADED"),
            "launchctl-style: custom ZDOTDIR's .zshrc must be sourced. Output: <{out}>"
        );
        assert!(
            out.contains(&format!("FINAL_ZDOTDIR={}", custom.display())),
            "launchctl-style: ZDOTDIR must be restored from NICE_USER_ZDOTDIR. Output: <{out}>"
        );
    }
}
