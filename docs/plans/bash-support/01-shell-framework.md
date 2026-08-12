# Shell framework — extraction + resolution (migration steps 1–2)

Implements **steps 1 and 2** of `docs/design/shell-abstraction.md` (read it first — its §2/§4/§5
Contract sections are binding; this plan does not restate rationale it already carries). Inventory
findings referenced as F1–F12 are from the verified zsh-assumption inventory the design doc was
built on; the finding→code map below is self-sufficient.

## Goal

- **Step 1 (pure extraction, byte-frozen):** a new `crates/nice/src/shell/` module owning the
  `ShellProfile` trait, `ZshProfile`, and supporting types; the four rc-stub bodies moved to
  `include_str!` files **byte-identically**; every current zsh call site (spawn argv, inject env,
  rc writing, compose trigger constants, prefill env, claude probe, reaper comm filter) routed
  through the profile. Zero observable behavior change — provable, see "Byte-freeze proof".
- **Step 2 (resolution + fallback + routing hygiene):** the §4 resolution chain
  (`NICE_SHELL` → `advanced.shell` setting → `$SHELL` → `getpwuid` → `/bin/zsh`), the
  `ShellRuntime` global, `FallbackProfile` for unknown shells, the per-pane `PaneShell` snapshot,
  compose routing gated on the pane's `ComposeSupport` (fixes F6), and the reaper matching the
  registry comm-name union (fixes F10). After step 2 a fish (or, for now, bash) user gets their
  own shell as a correct, quiet, plain terminal.

## Non-goals

- **No `BashProfile`** (steps 3–5). In this slice a resolved bash maps to `FallbackProfile` — a
  genuine plain login bash with all integration features off. Step 3 upgrades it.
- **No Settings UI** (step 6). Step 2 lands the persisted `advanced.shell` key + read plumbing
  only; no Settings ▸ Advanced row.
- **No user-facing copy changes** beyond the bootstrap reaper stderr line (design §7). The
  `shortcuts.rs` / `claude_pane.rs` "zsh prompt" strings and README wording are step 6 —
  `display_name()` exists on the trait from step 1 but nothing in `nice-model` consumes it yet
  (crate layering; see Open questions).
- **No bash-side hermetic test helper** (`--norc --noprofile`) — design §10 assigns it to step 3.
- **No persistence of `PaneShell`** — runtime-only by contract (design §2).

## Design contract recap (what implementers code against)

Everything in design §2 (trait + types), §3 (dispatch), §4 (resolution), §5 (fallback table),
§6.1 (`ZshProfile`), §7 (reaper), §8 (layout) is Contract. Signatures come from §2 verbatim.
Where this plan says "implementation latitude" the design doc says so too.

## Current call-site map (verified against the worktree)

| Concern | Site |
|---|---|
| Spawn argv (F1) | `crates/nice-term-core/src/spawn.rs:10` `ZSH_PATH`, `:90-106` `build_exec_args`/`build_argv`; consumed at `crates/nice-term-core/src/pty.rs:191-192` |
| Rc stubs + writer (F2) | `crates/nice/src/shell_inject.rs` — `ZSHENV_BODY` :84, `ZPROFILE_BODY` :105, `ZLOGIN_BODY` :112, `ZSHRC_BODY` :123-579, `write_stubs` :587, `write_atomic` :600, `default_location` :619 |
| Compose trigger constants (F4/F6) | `shell_inject.rs:75` `COMPOSE_TRIGGER_SEQ`, `:80` `COMPOSE_TRIGGER_BINDKEY`; consumers `window_state.rs:2387`, `compose_live.rs:45` |
| Window env injection (F2) | `pty_manager.rs:220` `WindowShellEnv`, `:1300-1321` `session_window_env_pairs`, `:1611-1618` (spawn_claude_window reads the fields) |
| Claude env + prefill (F8) | `pty_manager.rs:1947-1985` `build_claude_extra_env`, `:2000-2005` `build_claude_prefill_command` (FROZEN wire string, untouched) |
| Probe (F9) | `app.rs:1433-1449` `kickoff_claude_probe`, `:1456-1471` `run_which_claude` (hardcoded `/bin/zsh -ilc`) |
| Reaper (F10) | `orphan_reaper.rs:143-183` `live_candidate_pids` (comm == `"zsh"` at :177), `ReaperEnv::live` :128; call site + stderr line `app.rs:1399-1402` |
| Bootstrap + global | `app.rs:1356-1363` `ShellInjectConfig`, `:1376-1385` `set_scenario_shell_inject_config`, `:1390-1423` `install_shell_inject_bootstrap`, `:1649-1655` window construction reads the global |
| Production `SpawnSpec` constructors in `nice` | `pty_manager.rs:1636` (ResumeDeferred shell), `:1656` (claude command), `:1659` (probe-unresolved plain shell), `:1742` (deferred terminal); `app.rs:1658-1663` (main window, incl. `NICE_COMMAND`) |
| Settings store | `settings/prefs_store.rs:42-46` `AdvancedSection` (currently only `smooth_scroll`); `SettingsPrefsStore` global is set at `app.rs:1155`, **before** the bootstrap at `:1186` — the ordering step 2 needs |

Scenario/test call sites that stay untouched through this slice (design §10): the empty-`ZDOTDIR`
helpers at `nice-term-core/src/pty.rs:557-566`, `nice-itests/src/session.rs` (`cat_fixture_spec`
etc.), and the ~10 scenario `ZDOTDIR`-blanking spawns. `shell_inject::write_stubs` scenario
callers (`claude_e2e_live.rs:172`, `shell_socket_live.rs:159`, `compose_live.rs:111`,
`theme_fanout_live.rs:104`, `close_confirm_live.rs:95`, `persistence_restore_live.rs:115`,
`settings/scenario.rs:765`) keep compiling via the step-1 shim and are re-pointed in step 2.

---

## Step 1 — pure zsh extraction (byte-frozen)

Ordered work items. Land as one commit series; W1.0 must be its own commit BEFORE any move.

### W1.0 — pin the frozen bytes first (the proof baseline)

In `shell_inject.rs`'s test module, add `stub_bodies_and_argv_sha256_frozen`:

- SHA-256 each of `ZSHENV_BODY`, `ZPROFILE_BODY`, `ZLOGIN_BODY`, `ZSHRC_BODY` and assert against
  hex literals (compute once via a `--nocapture` run, paste, re-run green).
- Also assert the argv goldens inline (cheap, total): `build_argv(None) == ["/bin/zsh","-il"]`,
  `build_argv(Some("exec x")) shape`, and the two constants
  `COMPOSE_TRIGGER_SEQ == b"\x1b[5099~"` / `COMPOSE_TRIGGER_BINDKEY == r"\e[5099~"`.

Commit this test on the unmodified code. It then rides through every later commit **unchanged**
(only its `use` path may move) — that is the byte-freeze proof: if the extraction perturbs one
byte of any body, this test fails.

### W1.1 — `crates/nice/src/shell/` module skeleton

New files per design §8:

- `shell/mod.rs` — `ShellKind`, `ComposeSupport`, `PrefillStrategy`, `InjectPaths`,
  `UserShellEnv`, `SpawnCtx`, `PaneShell`, the `ShellProfile` trait (signatures exactly as design
  §2), `all_known_comm_names()` (step 1: `&["zsh"]`; plan 02 flips it to `&["zsh", "bash"]` with
  the `BashProfile` — see Open questions), and the relocated
  `COMPOSE_TRIGGER_SEQ` / `COMPOSE_TRIGGER_BINDKEY` constants with their doc comments moved
  verbatim.
- `shell/resolve.rs` — `ShellSetting { Automatic, Path(String) }` + `pub fn resolve(setting:
  &ShellSetting) -> Box<dyn ShellProfile>`. **Step 1 pins it**: ignore inputs, always return
  `ZshProfile` at `/bin/zsh`, with a `// step 2 turns on the §4 chain` comment. Structure the
  file so step 2 only fills in the chain (keep an internal `resolve_path(inputs) -> String`
  stub).
- `shell/zsh.rs` — `ZshProfile { path: String }` implementing the trait (W1.2), plus the moved
  writer machinery: `write_atomic`, the stub writer (now `ZshProfile::write_rc_files`),
  `default_location()` + `application_support_root()` + `bundle_folder_name()` move here
  unchanged (still returning `…/<CFBundleName>/zdotdir` in step 1 — the rename is step 2, W2.6).
- `shell/scripts/zsh/{zshenv.zsh,zprofile.zsh,zlogin.zsh,zshrc.zsh}` — the four bodies as files;
  `zsh.rs` declares `pub const ZSHENV_BODY: &str = include_str!("scripts/zsh/zshenv.zsh");` etc.
  **Byte-exactness gotchas:** the current constants do NOT end with a trailing newline (raw
  strings end at the closing quote), so the files must not either — disable any editor/format
  final-newline insertion for these files (they are `.zsh`, rustfmt never touches them; verify
  with `tail -c 1 | xxd`). W1.0's SHA test is the enforcement.
- `shell/fallback.rs` — created in step 2; not in step 1.
- `crates/nice/src/main.rs` (or `lib.rs` module root — wherever `mod shell_inject;` is declared):
  add `mod shell;`.

`ZshProfile` per design §6.1:

- `kind()` → `Zsh`; `program()` → the stored path; `comm_name()` → `"zsh"`; `display_name()` →
  `"zsh"`; `compose_support()` → `Trigger`; `prefill()` → `ShellSide`.
- `spawn_argv(ctx)` → `[path, "-il"]` / `[path, "-ilc", "exec <cmd>"]`; ignores `ctx.inject`
  (env decides injection for zsh). Reuse `nice_term_core::build_exec_args` for the tail so the
  clustered spellings stay pinned by one implementation.
- `inject_env(inject, user)` → `[("ZDOTDIR", inject.dir), ("NICE_USER_ZDOTDIR",
  user.user_zdotdir.clone().unwrap_or_default())]` — the always-set empty-string semantics
  preserved verbatim.
- `write_rc_files(dir)` → creates `dir`, writes the four stubs atomically (moved code), returns
  `InjectPaths { dir, rcfile: None }`.
- `probe_argv(cmd)` → `[path, "-ilc", cmd]`.

### W1.2 — move the `shell_inject.rs` content; leave a shim

- Move the module doc header, bodies, writer, location fns, and the ENTIRE test module
  (~48 tests, `shell_inject.rs:700-1851`) into `shell/zsh.rs` (tests may live in
  `shell/zsh/tests.rs` if size warrants — latitude). **No assertion text changes**; only `use`
  paths. W1.0's SHA test moves along with them.
- Shrink `shell_inject.rs` to a re-export shim: `pub(crate) use crate::shell::{COMPOSE_TRIGGER_SEQ,
  COMPOSE_TRIGGER_BINDKEY};` + `pub(crate) use crate::shell::zsh::{write_stubs, default_location, ZSHRC_BODY, …}`
  (keep `write_stubs(dir) -> io::Result<PathBuf>` as a thin wrapper over
  `ZshProfile::write_rc_files` returning `paths.dir`, so the 7 scenario callers and `app.rs:1405`
  compile untouched in step 1). Drop the `#![allow(dead_code)]` in the new module; keep it on the
  shim only if needed.
- Update the two production consumers of the compose constants to the new path
  (`window_state.rs:2360,2387`, `compose_live.rs:45`) — or leave them on the shim; latitude.
  The shim dies in step 2 (W2.7).

### W1.3 — `SpawnSpec` carries a prebuilt argv (term-core stays policy-free)

`crates/nice-term-core/src/spawn.rs`:

- Add field `pub argv: Vec<String>` to `SpawnSpec`. `SpawnSpec::shell` / `SpawnSpec::command`
  populate it via `build_argv(command)` — the zsh-shaped default, so every existing term-core
  test, itest, and scenario compiles and behaves identically. Add builder
  `pub fn with_argv(mut self, argv: Vec<String>) -> Self` (caller override; must be non-empty).
- `ZSH_PATH`, `build_exec_args`, `build_argv` stay public and unchanged (they ARE the default).
- Doc-comment update: the spawn contract is "argv[0] is the program"; the zsh wording moves to
  the constructors ("default argv is a zsh login shell; production Nice overrides via profile").

`crates/nice-term-core/src/pty.rs:188-208`: `PtyProcess::spawn` uses `spec.argv` — `program =
cstr(&spec.argv[0])`, `argv_owned` from `spec.argv` — instead of `ZSH_PATH` + `build_argv(spec.command)`.
Guard: empty `argv` ⇒ `io::Error` (InvalidInput), never a panic.

**Invariant to document at both ends:** callers of `with_argv` must build the argv from the same
`command` string stored on the spec (the spec's `command` remains the display/launch-overlay
source of truth; the argv is the exec truth).

### W1.4 — `ShellRuntime` global replaces `ShellInjectConfig`

`app.rs`:

- Define (in `shell/mod.rs`, `impl gpui::Global` in `app.rs` or via a `Global` impl in the module
  — latitude):

  ```rust
  pub struct ShellRuntime {
      pub profile: Box<dyn ShellProfile>,
      pub inject: Option<InjectPaths>,   // None ⇒ rc write failed (non-fatal)
      pub user_env: UserShellEnv,
  }
  ```

- **Factor the install as a reusable fn** (design §4, amended per `review-fable.md` B1):

  ```rust
  /// Resolve → write rc files → install the ShellRuntime global.
  /// Called by the bootstrap, and again by the Settings ▸ Advanced shell pick (step 6).
  pub(crate) fn install_shell_runtime(cx: &mut App, setting: &ShellSetting);
  ```

  It does resolve (design §4), captures `UserShellEnv`, writes the profile's rc files
  (`Err` ⇒ today's stderr line + `inject: None`, non-fatal), and `cx.set_global(ShellRuntime …)`.
  In step 1 this is a few lines and changes no behavior; it exists so step 6 does not have to
  restructure the bootstrap after the fact. **Do not pin set-once semantics** anywhere (no doc
  comment or test asserting `resolve()` runs exactly once) — the global is replaced by an
  explicit settings pick; only panes never re-resolve.

- `install_shell_inject_bootstrap` (`app.rs:1390`) becomes:
  1. `install_shell_runtime(cx, &ShellSetting::Automatic)` (step 1: `resolve` is pinned to zsh) —
     resolution at the TOP, per design §4 ordering. Internally it captures
     `user_env = UserShellEnv { user_zdotdir: std::env::var("ZDOTDIR").ok() }` **before** rc
     writing (same ordering guarantee as today's step 3 comment) and writes into
     `crate::shell::zsh::default_location()` (step 2 generalizes the directory, W2.6).
  2. `tmp_sweep` unchanged.
  3. Reaper: pass `&[comm_name]` read from the installed global
     (`cx.global::<ShellRuntime>().profile.comm_name()`, W1.7) — behavior identical (`["zsh"]`).
     Stderr line byte-identical in step 1.
  4. `kickoff_claude_probe(cx)` unchanged in position.

  (The rc write now happens inside the installer at the top rather than after the reaper. It
  touches Application Support only, so its order relative to `tmp_sweep`/the reaper is not
  load-bearing.)
- Delete `ShellInjectConfig`; `set_scenario_shell_inject_config(cx, zdotdir, user_zdotdir)`
  keeps its signature but now installs a `ShellRuntime` (`ZshProfile` at `/bin/zsh`,
  `inject: zdotdir.map(|d| InjectPaths { dir: d.into(), rcfile: None })`,
  `user_env.user_zdotdir`) — all 11 scenario callers compile untouched. "Reset to `(None, None)`"
  now means a runtime with `inject: None` (window env degrades to socket-only exactly as today).
- Window construction (`app.rs:1649-1655`): read `try_global::<ShellRuntime>()`; compute the
  window inject pairs (W1.5) and pass them to `arm_window_control_socket` in place of
  `(zdotdir, user_zdotdir)`.

**Failed-rc-write parity (byte-freeze detail):** today, when the stub write fails, panes still
get `NICE_USER_ZDOTDIR` (always-set, `session_window_env_pairs:1311-1314`) and ResumeDeferred
panes still get `NICE_USER_ZDOTDIR` + `NICE_PREFILL_COMMAND` — only `ZDOTDIR` is dropped. The
trait's `inject_env` is only callable with a live `InjectPaths`, so the bootstrap composes the
degraded pairs explicitly:

```rust
let window_inject_pairs = match &runtime.inject {
    Some(p) => runtime.profile.inject_env(p, &runtime.user_env),
    // Legacy degraded path, kept byte-identical — a ZSH-ONLY quirk that predates
    // the trait, so it is gated on the profile kind (review-fable.md I2). Non-zsh
    // profiles with `inject: None` emit nothing; a stray NICE_USER_ZDOTDIR in a
    // bash/fish pane is exactly the cross-shell leakage this abstraction exists to
    // prevent.
    None if runtime.profile.kind() == ShellKind::Zsh => {
        vec![("NICE_USER_ZDOTDIR".into(), runtime.user_env.user_zdotdir.clone().unwrap_or_default())]
    }
    None => vec![],
};
```

Put this in one place (a free fn in `shell/mod.rs`, e.g. `window_inject_pairs(&ShellRuntime)`)
with a comment naming it a preserved zsh quirk.

### W1.5 — `WindowShellEnv` goes shell-agnostic

`pty_manager.rs`:

- `WindowShellEnv` (`:220`): replace `zdotdir: Option<String>` + `user_zdotdir: Option<String>`
  with `inject_pairs: Vec<(String, String)>` (profile-produced, possibly empty). `socket_path`
  and `compose_conf` stay.
- `session_window_env_pairs` (`:1300`): emit `NICE_SOCKET` (when set), then splice
  `inject_pairs`, then `NICE_COMPOSE_CONF` / `NICE_TAB_ID` / `NICE_PANE_ID` as today. **Pair
  order note:** today's order is `NICE_SOCKET, ZDOTDIR, NICE_USER_ZDOTDIR, NICE_COMPOSE_CONF,
  NICE_TAB_ID, NICE_PANE_ID`; `ZshProfile::inject_env` returns `[ZDOTDIR, NICE_USER_ZDOTDIR]`,
  so splicing preserves it. Order is not semantically load-bearing (env is a map to the child)
  but keep it anyway — free, and it keeps any incidental test/logging diffs at zero.
- `spawn_claude_window` (`:1611-1618`): read `(socket_path, inject_pairs)` from the env;
  `build_claude_extra_env` signature changes from `(…, zdotdir_path, user_zdotdir, …)` to
  `(…, inject_pairs: &[(String, String)], …)`. Its ResumeDeferred arm splices `inject_pairs`
  where it pushed ZDOTDIR/NICE_USER_ZDOTDIR (`:1964-1974`) and keeps the `NICE_PREFILL_COMMAND`
  push (`:1979-1982`) — same output pairs for every input in the matrix.
  `build_claude_prefill_command` untouched (FROZEN).
- `arm_window_control_socket` (`app.rs:1490`): parameter change `(zdotdir, user_zdotdir)` →
  `inject_pairs: Vec<(String, String)>`.

### W1.6 — route the probe through the profile

`app.rs`: `kickoff_claude_probe` computes
`let argv = cx.global::<ShellRuntime>().profile.probe_argv("command -v -- claude");` on the
foreground, then the background task runs `run_which_claude(&argv)` (signature gains the argv;
`Command::new(&argv[0]).args(&argv[1..])`). Everything else (override seam, validation,
async delivery) unchanged. For zsh the spawned command is byte-identical to today.

### W1.7 — parametrize the reaper comm filter (no behavior change yet)

`orphan_reaper.rs`: `live_candidate_pids` takes `accepted: &[&str]`; the `:177` check becomes
`if !accepted.contains(&comm_name(&info.pbi_comm).as_str()) { continue; }`.
`ReaperEnv::live(accepted: Vec<String>)` threads it. Call site (`app.rs:1399`) passes
`vec![profile.comm_name().to_string()]` — `["zsh"]`, identical behavior, stderr line untouched
in step 1. Extract the acceptance check as a pure `fn comm_accepted(accepted: &[String], comm: &str) -> bool`
for unit tests.

### W1.8 — production spawn sites pass profile argv

All five production `SpawnSpec` sites in `nice` gain
`.with_argv(profile.spawn_argv(&SpawnCtx { inject, command }))`, where `profile`/`inject` come
from `ShellRuntime` (via a small helper — latitude — e.g.
`fn profile_argv(cx: &App, inject: bool, command: Option<&str>) -> Vec<String>` that falls back
to the zsh default when the global is absent, i.e. under `run_selftest`):

- `pty_manager.rs:1636` ResumeDeferred shell: `inject: Some`, `command: None`.
- `pty_manager.rs:1656` claude command pane: `inject: None` (design §2: non-deferred Claude
  windows spawn WITHOUT injection — matches today: `build_claude_extra_env` adds no ZDOTDIR
  outside ResumeDeferred), `command: Some(<post-exec remainder>)`.
- `pty_manager.rs:1659` probe-unresolved plain shell: `inject: None`, `command: None`.
- `pty_manager.rs:1742` deferred terminal: `inject: Some`, `command: None`.
- `app.rs:1658-1663` main window: `inject: Some`, `command` from `NICE_COMMAND` presence.

Note the `inject` axis changes NOTHING for zsh (its `spawn_argv` ignores it; injection rides
env) — it exists so step 2's fallback and step 3's bash get the right argv through the same
call sites. `PtyManager` needs access to the profile: pass the argv (and later `PaneShell`) in
from callers, or give `PtyManager` a `cx`-read helper — spawn methods already take `cx`;
reading `try_global::<ShellRuntime>()` inside `spawn_claude_window` / `ensure_active_window_spawned`
/ `spawn_window` is the smallest change (latitude).

**Step 1 exit:** `cargo test --workspace` green with zero assertion changes to any frozen test;
W1.0's SHA test passing; app behavior identical.

---

## Step 2 — resolution, fallback, routing hygiene

### W2.1 — the resolution chain (`shell/resolve.rs`)

Implement design §4 as a pure core + thin live wrapper:

```rust
pub struct ResolveInputs {
    pub nice_shell: Option<String>,      // $NICE_SHELL
    pub setting: ShellSetting,           // advanced.shell (Automatic when absent)
    pub env_shell: Option<String>,       // $SHELL
    pub pwuid_shell: Option<String>,     // getpwuid(getuid()).pw_shell
    pub is_usable: fn(&str) -> bool,     // exists + executable (injectable; test seam)
}
fn resolve_path(inputs: &ResolveInputs) -> String;   // pure precedence walk
pub fn resolve(setting: &ShellSetting) -> Box<dyn ShellProfile>;  // live wrapper
```

- Precedence: `NICE_SHELL` (absolute + usable, else ignored) → `Path(p)` from the setting
  (usable, else fall through) → `$SHELL` (usable) → `pwuid_shell` (usable) → `/bin/zsh`.
  Non-absolute or empty values are ignored at every hop.
- `is_usable` live impl: `Path::new(p).is_absolute()` + metadata is a file + mode has any
  execute bit (`std::os::unix::fs::PermissionsExt`).
- `pw_shell` via `libc::getpwuid(libc::getuid())` — copy the C string to an owned `String`
  immediately (static buffer); called once at bootstrap on the main thread. No `dscl`.
- Basename mapping: `zsh` ⇒ `ZshProfile { path }` (the resolved path, e.g.
  `/opt/homebrew/bin/zsh` — kept as resolved per §4); **anything else, including `bash`,** ⇒
  `FallbackProfile { path }` in this slice (step 3 adds the `bash` arm; leave a
  `// step 3: BashProfile` comment on the match).
- `resolve()` is infallible — worst case `ZshProfile` at `/bin/zsh`.

Bootstrap wiring (`install_shell_inject_bootstrap`): read the setting from
`cx.global::<SettingsPrefsStore>().shell_setting()` (the store global is installed at
`app.rs:1155`, before the bootstrap at `:1186` — verified). Pass it to `resolve`.

### W2.2 — `advanced.shell` setting plumbing (no UI)

`settings/prefs_store.rs`:

- `AdvancedSection` gains `#[serde(default, skip_serializing_if = "Option::is_none")] shell:
  Option<String>` — absent ⇒ `Automatic`; a non-empty string ⇒ `ShellSetting::Path`.
  Empty-string value ⇒ treat as absent (defensive).
- Accessor `pub fn shell_setting(&self) -> ShellSetting`. **No setter** in this slice (the UI is
  step 6; the read-merge-write writer already preserves unknown keys, and `AdvancedSection`
  round-trips the field when other advanced settings are written — add a test for that
  round-trip).

### W2.3 — `FallbackProfile` (`shell/fallback.rs`)

Exactly the design §5 table. Key spellings:

- `spawn_argv`: `[path, "-i", "-l"]` / `[path, "-i", "-l", "-c", "exec <cmd>"]` — separate
  flags, never clustered.
- `write_rc_files`: `Ok(InjectPaths { dir: dir.to_path_buf(), rcfile: None })` without touching
  the filesystem.
- `inject_env`: `vec![]`. `compose_support`: `None`. `prefill`: `Off`.
- `probe_argv`: `[path, "-i", "-l", "-c", cmd]`.
- `comm_name`: basename of `path` truncated to **15 bytes** (MAXCOMLEN is 16 incl. NUL). Store
  the truncated basename on the profile at construction and borrow it — the trait method is
  `fn comm_name(&self) -> &str`, not `&'static str` (design §2 amended per `review-fable.md`
  I3; `display_name` likewise). Every profile satisfies it and no caller cares (the reaper
  collects owned `String`s).
- `display_name`: same borrowed basename (pre-truncation).

### W2.4 — `PaneShell` snapshot + compose gating (fixes F6)

- `shell/mod.rs` already defines `PaneShell { kind, compose }` (step 1). `pty_manager.rs`:
  `WindowPty` (`:198`) gains `shell: PaneShell`; `spawn_session_raw` captures it at spawn from
  `ShellRuntime` (absent global ⇒ `PaneShell { kind: Zsh, compose: Trigger }` — preserves
  scenario behavior under `run_selftest`; comment it). Accessor
  `pub(crate) fn pane_shell(&self, session_id: &str, term_window_id: &str) -> Option<PaneShell>`.
- `window_state.rs`: `compose_route` (`:2403`) gains a `compose: ComposeSupport` parameter; the
  Trigger leg becomes `kind == Terminal && alive && !fg_child && compose == ComposeSupport::Trigger`;
  the ForwardCmdEnter/Noop legs are unchanged (a non-Trigger pane falls through to them — the
  pre-feature ⌘↩ behavior, no stray bytes). `dispatch_command_compose` (`:2366`) reads
  `self.ptys.pane_shell(…)` (defaulting to the zsh snapshot when `None`, same rationale) and
  passes `.compose`.
- Mid-run setting changes: nothing to do **in this slice** — panes keep their snapshot by
  construction, so a later `ShellRuntime` replacement can only affect newly spawned panes. Note
  the global is replaceable, not set-once (design §4, amended per `review-fable.md` B1): step 6
  re-runs `install_shell_runtime` on a settings pick. Do not add doc comments or tests asserting
  a single resolve. (The Settings-row copy about "new panes only" is step 6.)

### W2.5 — reaper comm-name union (fixes F10)

- `app.rs:1399`: accepted set = `all_known_comm_names()` ∪ `profile.comm_name()`, deduped. In
  this slice the registry is `["zsh"]`, so the union is `["zsh"]` for zsh users and
  `["zsh", "<fallback comm>"]` (e.g. `"fish"`, `"bash"`) for fallback users.
- Stderr line drops "zsh": `"nice: reaped {reaped} orphan shell(s) from prior runs"` (design §7).
- The decisive PPID==1 + uid + `NICE_TAB_ID=` criteria are untouched.

### W2.6 — rc directory rename: `zdotdir` → `shellrc/<shell>/`

- `default_location()` (now in `shell/zsh.rs`; hoist the variant-root part to `shell/mod.rs`) —
  `…/<CFBundleName>/shellrc/zsh` for zsh. Written for the active profile only at bootstrap;
  overwrite-always/self-heal semantics carry over verbatim (they live in `write_rc_files`).
- The legacy `…/<CFBundleName>/zdotdir` directory is LEFT IN PLACE (a few KB of static stubs;
  sweeping shared Application Support state from new code risks racing an older running variant
  build — YAGNI). Note it in the module doc.
- Update the location test (`default_location_is_under_app_support_not_temp`) for the new
  leaf path.

Done in step 2, not step 1, deliberately: step 1 is byte-frozen **including the injected
`ZDOTDIR` env value**; step 2 already owns "same-behavior-different-values" changes.

### W2.7 — prefill gating + retire the shim

- `build_claude_extra_env` consults the active profile's `prefill()` (thread it as a parameter,
  same style as `inject_pairs`): `ShellSide` ⇒ set `NICE_PREFILL_COMMAND` exactly as today;
  `Off` ⇒ skip it (fallback panes open at a bare prompt, design §5); `AppTyped` ⇒ also skip the
  env var and do nothing more — **unreachable in this slice** (no profile returns it); leave a
  `// step 4: record pending_prefill` comment rather than dead machinery.
- Re-point the 7 scenario `shell_inject::write_stubs` callers and any remaining shim users to
  `crate::shell::zsh` (scenarios deliberately write ZSH fixture stubs — they are zsh-pinned by
  design §10, so calling the zsh writer directly is correct, not a smell). Delete
  `shell_inject.rs`.
- `NICE_SHELL` is now honored: live scenarios that launch the real app binary and depend on zsh
  behavior should export `NICE_SHELL=/bin/zsh` in their launch env for determinism (grep the
  scenario harness launch sites; in-process `run_selftest` scenarios never resolve and are
  unaffected).

---

## Test plan

### Existing tests that MOVE (assertions unchanged)

- The entire `shell_inject.rs` test module (`:648-1851`: stub layout, frozen-byte round-trip,
  static-text pins, real-`/bin/zsh` e2e chains, zpty compose visual tests, attach/resume e2e) →
  `shell/zsh.rs` (or `shell/zsh/tests.rs`). These are the frozen-contract regression net for the
  extraction; not one assertion string changes in step 1.
- `orphan_reaper.rs` reap-seam tests: unchanged (the seam still injects `list_candidates`).

### Existing tests that CHANGE (and how)

| Test | Step | Change |
|---|---|---|
| `pty_manager/tests.rs:1610` `manager_with_shell_env` + env-matrix tests (`:1659-1810`) | 1 | Helper builds `WindowShellEnv { inject_pairs: vec![("ZDOTDIR", …), ("NICE_USER_ZDOTDIR", …)], … }`; every ASSERTED pair stays identical, including present-but-empty `NICE_USER_ZDOTDIR` (`:1696`) and spec-wins `ZDOTDIR` (`:1630`). `build_claude_extra_env` tests pass `inject_pairs` + `PrefillStrategy::ShellSide`; asserted output unchanged. |
| `window_state.rs:5888` `compose_route_truth_table` | 2 | Gains the `ComposeSupport` axis: every existing row passes `Trigger` and keeps its expected route; new rows pin `compose: None` ⇒ never `Trigger` (falls to `ForwardCmdEnter` when `kitty_super`, else `Noop`). |
| `shell/zsh.rs` `default_location_is_under_app_support_not_temp` | 2 | Leaf assertions become `shellrc/zsh`. |
| Reaper: new pure `comm_accepted` tests | 1–2 | Step 1: `["zsh"]` accepts `zsh`, rejects `bash`. Step 2: union accepts registry + fallback comm; 15-byte truncation case (e.g. a long basename). |

### Existing tests that must NOT change (hermeticism, design §10)

`nice-term-core/src/pty.rs:557` `test_env`, `nice-term-core/tests/*` (incl. `exec_args.rs` —
`build_argv`/`build_exec_args` still exist and still pin `/bin/zsh -il`/`-ilc`),
`nice-itests/src/session.rs` fixtures, and every scenario `ZDOTDIR`-blanking spawn. The
structural replacement (`SpawnCtx { inject: None }`) exists from step 1 for production spawns,
but the zsh empty-`ZDOTDIR` trick stays for tests because it ALSO excludes the user's own rc —
per design, both mechanisms coexist; the bash `--norc --noprofile` helper is step 3.

### New tests

Step 1:
- `stub_bodies_and_argv_sha256_frozen` (W1.0 — the byte-freeze proof).
- `ZshProfile` argv matrix over the full `SpawnCtx` grid (inject Some/None × command Some/None):
  four rows, all `-il`/`-ilc "exec …"` — pins that zsh ignores the inject axis.
- `ZshProfile::inject_env` pairs incl. `user_zdotdir: None` ⇒ `NICE_USER_ZDOTDIR=""`.
- `ZshProfile::probe_argv` = `["/bin/zsh", "-ilc", "command -v -- claude"]`.
- `write_rc_files` into a tempdir: four files, `InjectPaths { rcfile: None }`, overwrite
  self-heal (delete one stub, rewrite, present again).
- `resolve()` step-1 pin: returns Zsh at `/bin/zsh` for arbitrary inputs.
- `SpawnSpec` argv default: `shell()`/`command()` argv equals `build_argv(...)`; `with_argv`
  override survives to `PtyProcess::spawn` (extend `exec_args.rs` or a pty_process test with a
  real spawn using an overridden argv, e.g. `["/bin/sh", "-c", "true"]`).
- Degraded-path parity: `window_inject_pairs` with a zsh profile and `inject: None` yields
  exactly `[("NICE_USER_ZDOTDIR", "")]` / the inherited value.

Step 2:
- `resolve_path` precedence table: every hop wins over the next; non-absolute/empty `NICE_SHELL`
  ignored; unusable path falls through; final `/bin/zsh` floor; basename mapping (`zsh` path
  preserved as-resolved, e.g. homebrew zsh; `bash`/`fish`/garbage ⇒ fallback carrying the path).
- `FallbackProfile` table tests: argv shapes (separate flags), empty `inject_env`,
  no-op `write_rc_files` (tempdir stays empty), `compose_support() == None`,
  `prefill() == Off`, `probe_argv`, comm/display names incl. truncation.
- `prefs_store`: `advanced.shell` absent ⇒ `Automatic`; set ⇒ `Path`; empty string ⇒
  `Automatic`; field survives a `set_smooth_scroll` write round-trip.
- `pane_shell` snapshot: spawned window reports the runtime profile's kind/compose; absent
  runtime ⇒ zsh default.
- `build_claude_extra_env` with `PrefillStrategy::Off`: no `NICE_PREFILL_COMMAND`, no inject
  pairs; `ShellSide` row unchanged.
- Degraded path is zsh-gated (`review-fable.md` I2): `window_inject_pairs` with a
  `FallbackProfile` and `inject: None` yields an **empty** vec — no `NICE_USER_ZDOTDIR`.
- Real-shell smoke (cheap, unconditional — `/bin/bash` always exists): spawn
  `FallbackProfile { path: "/bin/bash" }.spawn_argv(command: Some("echo ok"))` through
  `PtyProcess` and assert `ok` arrives + clean exit — pins that the separate-flag spelling is
  accepted by a real non-zsh shell. (Full bash integration e2e is step 3.)

### Suite commands

`cargo test --workspace` after each work item; the shell_inject move (W1.2) and the
`WindowShellEnv` change (W1.5) are the two commits most likely to break compile — keep them
small. Fix rounds: targeted tests for touched modules only (repo rule).

## Byte-freeze proof (step 1 acceptance)

1. W1.0's SHA-256 + argv-golden test lands BEFORE any extraction commit and passes unchanged
   after the last step-1 commit. Reviewers verify the test file's assertions are untouched by
   the series (`git log -p -- '*zsh*'` shows only `use`-path/moves).
2. The moved frozen-contract suite (static-text pins, `writer_round_trips_frozen_bytes`, real-zsh
   e2e) passes with zero assertion edits.
3. `exec_args.rs` and the pty_manager env-matrix assertions pass with zero expected-value edits.
4. Manual spot-check: `shasum -a 256 crates/nice/src/shell/scripts/zsh/*.zsh` matches the
   literals in W1.0's test (guards against `include_str!` pointing at the wrong file).

## Acceptance criteria

Step 1:
- All of "Byte-freeze proof". `cargo test --workspace` green.
- `crates/nice/src/shell/` exists with trait + `ZshProfile` + pinned `resolve()`;
  `shell_inject.rs` is a shim; no production call site constructs zsh argv/env/rc paths outside
  the profile.
- Scratch-env `Nice Dev` launch (per CLAUDE.md recipe): `claude` shadow handshake opens a
  session, ⌘↩ compose works at a zsh prompt, a restored Claude session prefills
  `claude --resume …`, file-browser cwd tracks `cd`.

Step 2:
- `NICE_SHELL=/bin/zsh` (or unset, on a zsh account): behavior indistinguishable from step 1.
- Scratch-env launch with `NICE_SHELL=/bin/bash`: panes run a real login bash (user's PATH
  present via profile chain), ⌘↩ at the bash prompt produces NO garbage bytes (and still
  forwards to kitty TUIs), no `NICE_PREFILL_COMMAND` in `env` output, restored Claude panes open
  at a bare prompt, no rc files written under `shellrc/` for bash.
- Reaper: with a fallback profile active, `all_known_comm_names ∪ comm_name` drives the
  prefilter (unit-proven); stderr line says "orphan shell(s)".
- `advanced.shell` in `ui_settings.json` overrides `$SHELL`; `NICE_SHELL` overrides both.
- `pane_shell` snapshots are runtime-only (nothing new in `sessions.json`).

## Validation

All headless, run from the worktree. See "Test plan" for what each suite covers.

1. `cargo build --workspace` — must stay green through the series (W1.2 and W1.5 are the two
   likely break points; keep those commits small).
2. `cargo test -p nice shell::` — the moved zsh suite plus the new profile/resolve/fallback
   tests. Green **with `stub_bodies_and_argv_sha256_frozen` passing** is the byte-freeze proof.
3. `shasum -a 256 crates/nice/src/shell/scripts/zsh/*.zsh` — the four hashes must match W1.0's
   hex literals (catches an `include_str!` aimed at the wrong file); and
   `tail -c 1 crates/nice/src/shell/scripts/zsh/zshrc.zsh | xxd` must NOT show `0a`.
4. `git log -p <base>..HEAD -- crates/nice/src/shell` — reviewer check: no changed assertion
   text in the moved tests, only `use`-path/move noise.
5. `cargo test -p nice-term-core` (incl. `exec_args.rs`) and
   `cargo test -p nice compose_route` — pass with expected-value edits only where the
   "Existing tests that CHANGE" table says so.
6. `cargo test --workspace` — green at step-1 exit and again at step-2 exit.

Deferred to the user's feel-check on the merged branch: both scratch-env `Nice Dev` launches in
Acceptance criteria (zsh parity; `NICE_SHELL=/bin/bash` fallback panes). No install, app launch,
or GUI automation is part of this plan's runnable validation.

## Open questions (for the design owner — do not silently deviate)

1. **`comm_name(&self) -> &'static str` can't be satisfied by `FallbackProfile`** (its comm is
   derived from a runtime path). **Resolved by `review-fable.md` I3: design §2 amended** — the
   trait method is `fn comm_name(&self) -> &str` (and `display_name` the same way, for the
   fallback's runtime basename). Zero call-site impact (the reaper collects owned `String`s
   anyway).
2. **Failed-rc-write env parity** (W1.4): the design's `inject_env` contract has no arm for
   `inject: None`, but today's behavior still injects always-set `NICE_USER_ZDOTDIR` (and the
   ResumeDeferred prefill) on that path. **Resolved by `review-fable.md` I2: keep the quirk,
   zsh-only** — the legacy branch in `window_inject_pairs` is gated on
   `profile.kind() == ShellKind::Zsh`; non-zsh profiles with `inject: None` emit nothing.
3. **`all_known_comm_names()`** is specified as `["zsh", "bash"]`, but the registry principle
   ("every shell Nice ships a profile for") yields `["zsh"]` until step 3 lands `BashProfile`.
   **Resolved by `review-fable.md` B2: owned by plan 02** — this plan ships `["zsh"]`, and plan
   02 (work item 3, alongside the `resolve()` bash arm) flips it to `["zsh", "bash"]`. The
   fallback-union still covers bash users meanwhile.
4. **Legacy `…/zdotdir` directory** is left on disk after the W2.6 rename (not swept). Confirm
   that's acceptable (a one-time best-effort `remove_dir_all` is the alternative; rejected here
   as a race with concurrently-running older builds of the same variant).
5. **`display_name()` consumers**: the "zsh prompt" copy lives in `nice-model`
   (`shortcuts.rs:123-128`), which cannot depend on the `nice` crate's profile. Step 6 needs a
   plumbing decision (thread the string into `nice-model` at install time, or move the copy);
   nothing in this slice blocks on it.
