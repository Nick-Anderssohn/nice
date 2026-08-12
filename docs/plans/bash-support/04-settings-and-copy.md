# Bash support 04 — settings UI + user-facing copy

Migration step 6 of `docs/design/shell-abstraction.md` (§9.6). Closes inventory findings
**F11** (help text says "zsh prompt") and **F12** (README promises a plain `zsh`), and puts the
`advanced.shell` setting in front of the user.

## Goal

1. A **Shell** picker in Settings ▸ Advanced — Automatic (default) plus every shell binary on
   this Mac that Nice has a profile for (stock Mac: zsh and bash) — persisted to
   `advanced.shell` in `ui_settings.json`.
2. Picking a shell takes effect **for terminals opened after the change**; panes already
   running keep the shell they started with. That promise is stated in the row's ⓘ tooltip and
   is made literally true by the apply path (W4), not by asking for a relaunch.
3. No user-visible string says "zsh" any more unless the user's shell actually is zsh: the
   Command Compose tooltip and settings copy go shell-neutral or read the active profile's
   `display_name()`; the README stops promising zsh.

## Non-goals

- **No per-window or per-pane shell override.** One process-wide setting, one active profile
  (design §1). A window keeps whatever it started with.
- **No custom shell arguments.** Design §6.2 calls this out as the one thing that would force
  the `ENV`+`--posix` injection channel; it stays unbuilt.
- **No "Custom path…" file picker.** The discovery list (W2) covers real installs. A hand-edited
  `advanced.shell` pointing somewhere exotic is *preserved and shown* (W2's passthrough item),
  so nothing is silently clobbered — but there is no browse-for-a-binary UI.
- **No UI treatment of `NICE_SHELL`.** It is a dev/test seam of the same family as `NICE_COMMAND`
  and `NICE_CLAUDE_OVERRIDE` — undocumented, unsurfaced. When it is set the picker still shows
  and writes the persisted setting; the override simply wins at resolve time.
- **No relaunch prompt, no respawn of live panes, no fish/tcsh profile.**
- The bootstrap stderr line (`"reaped N orphan zsh shell(s)"`, `app.rs:1401`) belongs to plan 01
  W2.5 (migration step 2 — reaper comm-union, design §7). This plan only *verifies* it no longer
  says zsh.

## Dependencies

**Plan 01 (framework + resolution + setting storage)** — hard dependency. This plan consumes:

- `crate::shell::ShellSetting { Automatic, Path(String) }` and `crate::shell::resolve(&ShellSetting)`.
- The `ShellRuntime` global (`profile: Box<dyn ShellProfile>`, `inject`, `user_env`), plan 01
  W1.4's reusable `install_shell_runtime(cx, &ShellSetting)` (resolve → `write_rc_files` →
  set-global), and the profile methods `display_name()`, `program()`, `kind()`,
  `compose_support()`.
- The persisted key `advanced.shell` (absent ⇒ `Automatic`).

If plan 01 landed the resolution chain but **not** the persistence, add it here (W1); it is ~20
lines in an existing file. If plan 01's names differ, adapt — the shapes above are the contract
this plan was written against, not new API.

**Plan 02 (`BashProfile`)** — soft. The picker works without it: a `/bin/bash` selection resolves
to `FallbackProfile` and the user gets a plain, quiet bash with no Nice integration (design §5).
Landing 04 before 02 is safe and useful; landing it after is nicer.

**Plan 03 (bash compose)** — soft. W6's "Compose isn't available in this shell" sentence reads
`compose_support()`, which is `None` for every non-zsh profile until 03 lands and stays `None`
for stock bash 3.2 forever (design §6.3).

---

## Work items

### W1 — persist `advanced.shell` (only if plan 01 didn't)

`crates/nice/src/settings/prefs_store.rs` — `AdvancedSection` gains

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
shell: Option<String>,
```

plus `pub fn shell(&self) -> Option<String>` and
`pub fn set_shell(&mut self, path: Option<String>) -> io::Result<bool>` following
`set_terminal_font_family` exactly: only-if-changed, write through
`write_ui_settings_merged` (co-owner sections untouched). `None` ⇒ Automatic; the key is
omitted from the JSON entirely, not written as `null`.

Absent store ⇒ Automatic everywhere (the `run_selftest` / scenario default). Every accessor and
mutator in this plan fails soft when `SettingsPrefsStore` is missing — the `compose_conf` setter
precedent (`compose_conf.rs:137`).

### W2 — the choice list (`crates/nice/src/shell/choices.rs`, new)

A pure module so the whole picker is unit-testable without a window.

```rust
pub(crate) struct ShellChoice {
    /// `None` ⇒ Automatic; `Some(path)` ⇒ ShellSetting::Path(path).
    pub(crate) path: Option<String>,
    pub(crate) label: String,
    /// a11y-id suffix ("automatic", "bin_zsh", "opt_homebrew_bin_bash").
    pub(crate) id_suffix: String,
}

/// The picker's rows, in order. `persisted` is the current `advanced.shell`
/// value; `automatic_name` is the active profile's display name (only used
/// when `persisted.is_none()`).
pub(crate) fn shell_choices(
    persisted: Option<&str>,
    automatic_name: &str,
    candidates: &[PathBuf],   // injected for tests; live: discover_candidates()
) -> Vec<ShellChoice>;

/// Candidate binaries: every non-comment, non-empty line of `/etc/shells`,
/// then the fixed fallbacks `/bin/zsh`, `/bin/bash`, `/opt/homebrew/bin/bash`,
/// `/usr/local/bin/bash`. Deduped, order-preserving. No filtering here —
/// `shell_choices` does the existence/family screen so tests can inject.
pub(crate) fn discover_candidates() -> Vec<PathBuf>;
```

Rules `shell_choices` applies, in order:

1. Row 0 is always **Automatic** (`path: None`). Label `"Automatic"`, or
   `"Automatic ({automatic_name})"` when `persisted.is_none()` — the setting *is* Automatic, so
   the already-resolved active profile is exactly what Automatic picked, and naming it costs
   nothing. (When some other shell is selected we don't re-resolve just to label a menu row.)
2. Keep candidates that **exist, are executable** (`std::fs::metadata` + mode `& 0o111`), and
   whose file-name is a family Nice has a profile for — `"zsh"` or `"bash"`. Anything else is
   dropped: offering fish here would advertise an integration-free terminal as a feature.
3. Order: all `zsh` paths, then all `bash` paths, each in candidate order (so `/bin/*` beats
   homebrew, since `/etc/shells` lists the system paths).
4. Label = the family name (`"zsh"`, `"bash"`) when it is the only surviving path of that
   family, else `"{family} ({path})"` — a Mac with both `/bin/bash` and homebrew bash 5 shows
   both, distinguishably. (Compose only works on the ≥ 4.3 one, so this distinction is
   load-bearing.)
5. **Passthrough:** if `persisted` is `Some(p)` and `p` is not among the surviving rows, append a
   final row `{ path: Some(p), label: p.clone() }`. The compose-dropdown precedent
   ("an unknown persisted token shows the raw token so the user sees what is set",
   `claude_pane.rs:64`). Never silently drop a user's setting.
6. `id_suffix`: `"automatic"`, else the path with `/` and `.` replaced by `_`, leading `_` trimmed.

### W3 — the Advanced pane row

`crates/nice/src/settings/advanced_pane.rs`. The pane currently takes `&mut App`
(`root.rs:570`); dropdowns need `&mut Context<SettingsRootView>` (the open-menu state lives on
the root view). **Change the signature to `advanced_pane(window: &mut Window, cx: &mut Context<SettingsRootView>)`
and update its `root.rs` wrapper** — the Claude/Font/Appearance panes already have this shape, so
this is a one-line-each alignment, not a new pattern.

Row order: **Shell first**, then the existing Smooth scrolling toggle.

```rust
setting_row_info(
    "Shell",
    shell_row_info(&display_name),        // W6
    dropdown("settings.advanced.shell", current_label, items, window, cx),
    cx,
)
```

- Items are built from `shell_choices(...)` exactly like `compose_dropdown`
  (`claude_pane.rs:65-90`): one `DropdownItem` per choice, id
  `format!("settings.advanced.shell.{id_suffix}")`, `selected` = the row matching the persisted
  value (Automatic when absent), `on_select` → `perform_pick_shell(cx, choice.path.clone())`.
- `current_label` = the selected row's label.
- Absent `SettingsPrefsStore` ⇒ Automatic selected, picks no-op (W1's fail-soft).

### W4 — applying a pick

Two functions in `advanced_pane.rs`, split the way `claude_pane.rs` splits
`perform_toggle_sync_claude` / `sync_claude_live_arm` — so the disk-touching half is never
reachable from `run_selftest` or a unit test:

```rust
/// The shipped click path (live UI only — writes rc files under Application
/// Support and may spawn the claude probe).
pub(crate) fn perform_pick_shell(cx: &mut App, path: Option<String>);

/// The persistence half: write `advanced.shell` through SettingsPrefsStore.
/// No disk beyond the settings file, no process spawn. Unit-testable.
pub(crate) fn persist_shell_setting(cx: &mut App, path: Option<String>) -> bool; // changed?
```

`perform_pick_shell` = `persist_shell_setting`, and when it reports a change:

1. `let setting = path.map(ShellSetting::Path).unwrap_or(ShellSetting::Automatic);`
   `let profile = crate::shell::resolve(&setting);`
2. If `profile.program()` equals the active `ShellRuntime`'s program, stop (Automatic → the same
   binary the user just picked explicitly is a no-op). `cx.refresh_windows()` and return.
3. Write the new profile's rc files (`write_rc_files` into its `shellrc/<kind>/` dir, design §8)
   and install the new `ShellRuntime` global — i.e. **call plan 01's
   `install_shell_runtime(cx, &setting)`**, which plan 01 W1.4 factors out of
   `install_shell_inject_bootstrap` (`app.rs:1390`) for exactly this consumer, and which design
   §4 (amended per `review-fable.md` B1) blesses as the sanctioned way the global is replaced.
   Steps 1–2 above collapse into it if the installer also returns/records the resolved program.
   If plan 01's as-built code did not factor it, do it here and have the bootstrap call it. A
   `write_rc_files` failure stays non-fatal exactly as today (`app.rs:1407`): log, leave
   `inject: None`, panes still get `NICE_SOCKET`.
4. **Refresh every live window's shell env** so "new panes get the new shell" is literally true.
   `WindowShellEnv` (`pty_manager.rs:220`) is frozen per window at
   `arm_window_control_socket` (`app.rs:1508`), so a fresh profile would otherwise only reach
   *new windows*. Fan out over `WindowRegistry::all_states(cx)` (the
   `total_live_window_counts` precedent, `app.rs:656`) and call `ptys.set_window_shell_env(...)`
   with the new profile's inject-env pairs, **preserving each window's existing `socket_path`
   and `compose_conf`** (they are per-window and must not be rebuilt). Add a
   `PtyManager::window_shell_env()` reader if one doesn't exist.
   *Fallback if plan 01 restructured `WindowShellEnv` such that this is not a small change:*
   skip the fan-out, and narrow W6's tooltip to "new windows" instead of "new terminals". Say
   which one shipped in the commit message.
5. **Re-probe `claude`.** The whole point of F9 is that PATH lives in the shell's own rc files,
   so a zsh→bash switch can change where `claude` is. Re-run `kickoff_claude_probe`, but guard
   the delivery: only overwrite `ResolvedClaudePath` when the new probe returns `Some` — a
   transient probe failure must not downgrade a working install to "Claude not installed".
   (`kickoff_claude_probe` currently sets the global unconditionally, `app.rs:1446`; add the
   guard behind a `replace_only_on_success: bool` parameter or a sibling fn.)
6. `cx.refresh_windows()`.

### W5 — F11: the Command Compose strings

- `crates/nice-model/src/shortcuts.rs:123-128` — `ShortcutAction::info(CommandCompose)`:
  **shell-neutral**, not dynamic. `nice-model` has no dependency on `nice` and must not gain one
  for a tooltip; `info()` is a `&'static str` on a plain enum. New text:

  > "Turns plain English typed at a shell prompt into a real command using Claude Code. The
  > command is placed at the prompt for review — press Enter yourself to run it. Does nothing
  > while a program is running in the window."

  Also fix the doc comment at `:69-71` ("zsh's line buffer" → "the shell's line buffer").
- `crates/nice/src/settings/claude_pane.rs:138-141` — the "Command Compose model" ⓘ text is built
  in the `nice` crate, which *can* read the active profile, so it goes **dynamic**: "…at a bash
  prompt…" via W6's helper.

### W6 — the copy helpers (`crates/nice/src/shell/mod.rs` or `choices.rs`)

Pure string builders, so the wording is pinned by unit tests instead of read off a screenshot:

```rust
/// The active profile's display name; "zsh" when no ShellRuntime is installed
/// (scenarios / run_selftest — today's behavior).
pub(crate) fn active_display_name(cx: &App) -> String;

/// Settings ▸ Advanced ▸ Shell ⓘ text.
pub(crate) fn shell_row_info(automatic_name: &str) -> String;

/// Settings ▸ Claude ▸ Command Compose model ⓘ text.
pub(crate) fn compose_model_info(shell_name: &str, supported: bool) -> String;
```

- `shell_row_info`: *"Which shell new terminals run. Automatic uses your login shell
  ({automatic_name}). Terminals already open keep the shell they started with."* This is where
  "changes affect new panes only" is communicated — an ⓘ tooltip on a `setting_row_info`, which
  is the pane convention ("rows carry NO hint text… the ⓘ tooltip carries the setting's
  non-obvious info", `root.rs:439-441`, `claude_pane.rs:110`). No inline caption, no alert, no
  relaunch banner.
- `compose_model_info`: *"The Claude model that turns plain English at a {shell} prompt into a
  real command (the Compose command shortcut). Sonnet balances speed and quality; CLI default
  uses whatever your claude is configured with."* When `!supported`, prepend: *"Command Compose
  isn't available in {shell} — it needs zsh, or bash 4.3 or newer."* (**4.3**, not 4: bash 4.0–4.2
  can't bind `bind -x` to a multi-character sequence, so design §6.3 / plan 03 gate at 4.3.)
  Without this, a stock-bash user
  sees two live dropdowns for a feature that silently does nothing (design §6.3 makes stock
  `/bin/bash` 3.2 permanently `ComposeSupport::None`).
- `active_display_name` is a `String`, not the trait's borrowed `&str` (design §2, amended per
  `review-fable.md` I3): `FallbackProfile`'s name is a runtime basename, and cloning at
  tooltip-build time is free.

### W7 — F12: README

- Line 19: "…running in its own long-lived pty with a plain **shell** window alongside."
- Line 57: "…sessions fall back to a plain **shell** if it's missing."
- Requirements list gains one line: *"— zsh or bash; Nice runs your login shell (pick it in
  Settings ▸ Advanced). Other shells run fine as plain terminals, without Nice's shell
  integration."* Accurate for the design's fallback behavior (§5) whether or not plan 02 has
  landed.

Do **not** touch `docs/testing.md`'s real-zsh fixture policy or the internal doc comments in
inventory finding 13 — those are dev-facing and owned by plans 01-03.

---

## Test plan

Unit only, plus a scripted real-app check. No new live GUI scenario: the settings scenario
(`settings/scenario.rs`) is display-bound and expensive, and every decision here is pure.

**`shell/choices.rs`**
- non-executable / nonexistent candidates are dropped; `/bin/fish` is dropped (unknown family).
- dedupe: the same path in `/etc/shells` and the fallback list yields one row.
- ordering: zsh rows before bash rows, candidate order within a family.
- labels: single-bash Mac ⇒ `"bash"`; `/bin/bash` + `/opt/homebrew/bin/bash` ⇒ both labeled with
  their paths.
- Automatic is always row 0; label carries the resolved name only when `persisted.is_none()`.
- passthrough: `persisted = Some("/opt/pkg/bin/bash")` not among candidates ⇒ a trailing row with
  that raw path as its label, and it is the selected one.
- `id_suffix` is stable and contains no `/` or `.`.

**`settings/prefs_store.rs`** (if W1 applies)
- round-trip `advanced.shell`; absent key ⇒ `None`; `set_shell(None)` omits the key; only-if-changed
  returns `false` on a repeat; a planted `appearance` / `file_browser_sort` survives a shell write.

**`settings/advanced_pane.rs`**
- a pure `shell_dropdown_items(persisted, automatic_name, candidates)` returns `DropdownItem`s with
  the expected ids, labels and exactly one `selected` — the `controls.rs` test precedent
  (`DropdownItem::select` exists for exactly this, `controls.rs:258`).
- `persist_shell_setting` against a temp-path `SettingsPrefsStore` in a `#[gpui::test]`:
  Automatic → `/bin/bash` → Automatic round-trips through the file, and returns `false` on a
  repeat pick. It must **not** be able to reach `perform_pick_shell`'s rc-file write — assert by
  construction (the split), the way `claude_pane`'s test asserts the CFPref write is unreachable.
- absent store ⇒ `persist_shell_setting` is a no-op that returns `false` and does not panic.

**Copy**
- `shell_row_info("bash")` mentions bash and says already-open terminals keep their shell.
- `compose_model_info("bash", false)` contains the unavailable sentence, names bash, and pins the
  literal `"4.3"` (so the corrected gate can't regress to "4");
  `compose_model_info("zsh", true)` does not.
- Regression pin for F11: `ShortcutAction::info(CommandCompose).unwrap()` does **not** contain
  `"zsh"`, and neither does any `label()`/`info()` across `ShortcutAction::ALL`.

**Repo-level**
- A test (or the acceptance grep) asserting no user-facing `"zsh"` literal survives — scoped to
  the actual **UI-string surfaces**: `ShortcutAction::label()`/`info()` output across
  `ALL` (`crates/nice-model/src/shortcuts.rs`), this plan's pane copy helpers
  (`shell_row_info`, `compose_model_info`, and the `claude_pane.rs` ⓘ text they feed), and
  `README.md`. A directory-wide grep over `crates/nice/src/settings/` cannot work:
  `settings/scenario.rs:765` calls the zsh stub writer and stays zsh-pinned by design (plan 01
  W2.7 re-points it to `crate::shell::zsh` — the literal moves, it does not vanish). If a
  directory grep is used anyway, exclude `scenario.rs` explicitly.

## Verification (real app)

Scratch-env `Nice Dev` per CLAUDE.md (seeded HOME, keychain symlink, own settings domain):

1. Settings ▸ Advanced shows **Shell** above Smooth scrolling, reading `Automatic (zsh)` on a
   stock Mac. Hovering ⓘ shows the "already open keep their shell" text.
2. Pick **bash**. Open a new terminal window ⇒ `echo "$0"` / `ps -o comm= -p $$` reports bash.
   The terminal that was already open still reports zsh.
3. Open a new pane in the *pre-existing* window ⇒ bash (this is W4.4; if the fallback shipped,
   this is expected to still be zsh and the tooltip must say "new windows").
4. Quit and relaunch ⇒ the picker still reads bash and new windows are bash
   (`ui_settings.json` carries `"advanced":{"shell":"/bin/bash"}`).
5. Switch back to Automatic ⇒ new windows are zsh again; `advanced.shell` is gone from the file.
6. Hand-edit `advanced.shell` to a path that doesn't exist, relaunch ⇒ Nice still starts, panes
   work (resolution falls through, design §4), and the picker shows the raw path as the selected
   row so the user can see and change it.
7. With bash selected on a stock Mac, Settings ▸ Claude ▸ Command Compose model ⓘ leads with the
   "isn't available in bash" sentence.

## Acceptance criteria

- Settings ▸ Advanced has a **Shell** dropdown: Automatic + every profile-backed shell binary
  found on the machine, persisted to `advanced.shell`, surviving relaunch, defaulting to
  Automatic when the key is absent.
- A persisted path Nice can't offer is shown verbatim and selected — never silently reset.
- Picking a shell changes what the *next* terminal runs, with no relaunch and no respawn of
  running panes; the ⓘ tooltip states exactly that, and the statement is true of the shipped
  behavior (panes if W4.4 landed, windows if the documented fallback did).
- `grep -rn zsh` over `README.md`, `crates/nice-model/src/shortcuts.rs` and this plan's copy
  helpers returns nothing user-facing (`settings/scenario.rs`'s zsh-stub-writer call is
  dev-facing and stays — see the Test plan's Repo-level note).
- A stock-bash user is told in-product why Command Compose does nothing for them.
- `cargo test --workspace` green; no new live-scenario runtime.
