//! Command Compose runtime config — the flat-JSON file the injected ZLE widget
//! reads on every compose (`$NICE_COMPOSE_CONF`), and the process store behind
//! the Settings ▸ Claude model/effort dropdowns.
//!
//! The file is the app→shell channel for values that can change while a window's
//! env is already frozen (env is inherited at fork): the current accent color
//! (the spinner tint) and the `claude -p` model/effort flags. Nice rewrites it
//! at boot, on every appearance commit (accent may have changed), and on every
//! dropdown change; the widget re-reads it per compose, so a mid-session accent
//! or model switch takes effect on the next ⌘↩ with no relaunch.
//!
//! Shape (all keys flat, Nice-controlled, no escapes — the zsh side parses with
//! plain parameter surgery, see `shell/scripts/zsh/zshrc.zsh`'s `_nice_compose_conf_get`):
//!
//! ```json
//! {"accent":"#c96442","model":"sonnet","effort":"medium"}
//! ```
//!
//! An empty `model`/`effort` value means "pass no flag" (the CLI's own default).
//! The file doubles as the model/effort persistence — there is no second store.

use std::io;
use std::path::PathBuf;

use gpui::{App, Global};

/// Default `--model` for the compose call (Nick-picked).
pub(crate) const DEFAULT_MODEL: &str = "sonnet";
/// Default `--effort` for the compose call (Nick-picked).
pub(crate) const DEFAULT_EFFORT: &str = "medium";

/// The model dropdown's `(value, label)` choices. An empty value omits the
/// `--model` flag (the CLI resolves its own configured default).
pub(crate) const MODEL_CHOICES: [(&str, &str); 4] = [
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("haiku", "Haiku"),
    ("", "CLI default"),
];

/// The effort dropdown's `(value, label)` choices.
pub(crate) const EFFORT_CHOICES: [(&str, &str); 3] =
    [("low", "Low"), ("medium", "Medium"), ("high", "High")];

/// The process-wide compose config: the conf-file path plus the persisted
/// model/effort selections. Installed once by [`install`]; absent in scenarios
/// that never opt in (every accessor fails soft to the defaults).
pub(crate) struct ComposeConfStore {
    path: PathBuf,
    model: String,
    effort: String,
}

impl Global for ComposeConfStore {}

/// The fixed conf-file location: `<app support root>/<CFBundleName>/compose.json`
/// — a sibling of the `zdotdir` stub directory, honoring the same
/// `NICE_APPLICATION_SUPPORT_ROOT` test seam.
pub(crate) fn default_path() -> PathBuf {
    let root = match std::env::var("NICE_APPLICATION_SUPPORT_ROOT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
            .join("Library/Application Support"),
    };
    root.join(crate::platform::support_folder_name())
        .join("compose.json")
}

/// Render the conf-file body. Values are Nice-controlled tokens (accent hex,
/// model/effort words) — no escaping needed, pinned by the round-trip test.
fn render(accent_hex: &str, model: &str, effort: &str) -> String {
    format!(r##"{{"accent":"{accent_hex}","model":"{model}","effort":"{effort}"}}"##)
}

/// Extract `key`'s string value from a conf blob — the Rust mirror of the zsh
/// `_nice_compose_conf_get` parser (the two must agree; the `shell::zsh` static
/// test pins the zsh side against this shape). `None` when the key is absent.
pub(crate) fn parse_value(blob: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let rest = blob.split_once(&marker)?.1;
    Some(rest.split_once('"')?.0.to_string())
}

/// Install the store at `path`: seed model/effort from an existing conf file
/// (fail-soft to the defaults — fresh install, unreadable file, missing keys),
/// then write the file back with the CURRENT accent. Called from `app::run`
/// after the live theme store exists; scenarios may call it with a temp path.
pub(crate) fn install(cx: &mut App, path: PathBuf) {
    let blob = std::fs::read_to_string(&path).unwrap_or_default();
    let model = parse_value(&blob, "model").unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let effort = parse_value(&blob, "effort").unwrap_or_else(|| DEFAULT_EFFORT.to_string());
    cx.set_global(ComposeConfStore {
        path,
        model,
        effort,
    });
    rewrite(cx);
}

/// Rewrite the conf file from the store + the live accent. No-op when the
/// store was never installed (a scenario that didn't opt in). A write failure
/// is logged and otherwise ignored — the widget falls back to its defaults.
pub(crate) fn rewrite(cx: &mut App) {
    if cx.try_global::<ComposeConfStore>().is_none() {
        return;
    }
    // The RESOLVED accent (a preset's color, or the active scheme's
    // theme-derived hue under "From theme") — so the spinner tint tracks both
    // selection kinds. Falls back to Terracotta without the theme globals.
    let accent = crate::claude_theme_sync::Rgb8::from_srgba(
        crate::theme_settings::active_accent_color(cx),
    )
    .hex();
    let store = cx.global::<ComposeConfStore>();
    let body = render(&accent, &store.model, &store.effort);
    if let Err(e) = write_atomic(&store.path, &body) {
        eprintln!("nice: compose conf write failed: {e}");
    }
}

/// The current model selection ([`DEFAULT_MODEL`] when the store is absent).
pub(crate) fn model(cx: &App) -> String {
    cx.try_global::<ComposeConfStore>()
        .map(|s| s.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// The current effort selection ([`DEFAULT_EFFORT`] when the store is absent).
pub(crate) fn effort(cx: &App) -> String {
    cx.try_global::<ComposeConfStore>()
        .map(|s| s.effort.clone())
        .unwrap_or_else(|| DEFAULT_EFFORT.to_string())
}

/// Set + persist the model selection (a dropdown pick). No-op when the store
/// is absent.
pub(crate) fn set_model(cx: &mut App, model: &str) {
    if cx.try_global::<ComposeConfStore>().is_none() {
        return;
    }
    cx.global_mut::<ComposeConfStore>().model = model.to_string();
    rewrite(cx);
}

/// Set + persist the effort selection (a dropdown pick). No-op when the store
/// is absent.
pub(crate) fn set_effort(cx: &mut App, effort: &str) {
    if cx.try_global::<ComposeConfStore>().is_none() {
        return;
    }
    cx.global_mut::<ComposeConfStore>().effort = effort.to_string();
    rewrite(cx);
}

/// Atomic write: temp sibling + rename (same discipline as
/// `shell::zsh::write_atomic` — a pty child mid-read never sees a torn file).
fn write_atomic(path: &std::path::Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("conf");
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_parse_round_trip() {
        let body = render("#7a94db", "sonnet", "medium");
        assert_eq!(
            body,
            r##"{"accent":"#7a94db","model":"sonnet","effort":"medium"}"##
        );
        assert_eq!(parse_value(&body, "accent").as_deref(), Some("#7a94db"));
        assert_eq!(parse_value(&body, "model").as_deref(), Some("sonnet"));
        assert_eq!(parse_value(&body, "effort").as_deref(), Some("medium"));
        assert_eq!(parse_value(&body, "missing"), None);
    }

    #[test]
    fn empty_value_round_trips_as_no_flag() {
        // "" = pass no flag; the zsh side's `[[ -n "$v" ]]` skips it the same way.
        let body = render("#c96442", "", "medium");
        assert_eq!(parse_value(&body, "model").as_deref(), Some(""));
    }

    #[test]
    fn write_atomic_creates_parents_and_replaces() {
        let dir = std::env::temp_dir().join(format!("compose-conf-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/compose.json");
        write_atomic(&path, "one").expect("first write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
        write_atomic(&path, "two").expect("replace");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_are_the_locked_product_decision() {
        assert_eq!(DEFAULT_MODEL, "sonnet");
        assert_eq!(DEFAULT_EFFORT, "medium");
        // Every dropdown choice value is a bare CLI token (no whitespace/quotes
        // — the zsh side splices it unquoted-safe via "$v").
        for (value, _) in MODEL_CHOICES.iter().chain(EFFORT_CHOICES.iter()) {
            assert!(!value.contains(char::is_whitespace) && !value.contains('"'));
        }
    }
}
