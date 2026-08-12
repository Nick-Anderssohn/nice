# Shell abstraction — the `ShellProfile` framework

Status: design (no code changed yet).
Input: the verified zsh-assumption inventory (12 findings; scratchpad `zsh-inventory-opus.md`).
Audience: planning agents writing per-feature bash-support plans. Everything a plan needs to
depend on is in the **Contract** sections; anything marked *implementation latitude* may be
adjusted by implementers without breaking sibling plans.

---

## 1. Problem and shape of the solution

Nice hardcodes `/bin/zsh` at one spawn choke point and then rides a zsh-only delivery channel
(the synthetic `ZDOTDIR` rc chain) for every shell-integration feature: the `claude()` shadow,
Command Compose, OSC 7 cwd reporting, deferred-resume prefill. Binary discovery, the orphan
reaper, and user-facing copy also assume zsh.

The fix is **one trait, one resolution point, one per-pane snapshot**:

- `ShellProfile` — a trait each supported shell implements. It owns everything shell-dialect-
  shaped: spawn argv, rc-injection strategy, rc-file bodies (handshake, compose, cwd hook,
  prefill tail), probe argv, kernel comm name, display copy.
- Resolution happens **once at app bootstrap** (`install_shell_inject_bootstrap`), producing a
  process-global active profile. Panes never re-resolve.
- Each spawned pane captures a tiny **`PaneShell` snapshot** (kind + compose capability) at
  spawn time, so routing decisions (finding 6) key on what that pane actually runs even if the
  setting changes mid-run.

Scope is deliberately "zsh today, bash next, fish someday": no dynamic loading, no config-file-
defined shells, no plugin system. Adding a shell = one new module implementing the trait + one
enum arm + rc-script files.

---

## 2. The trait (Contract)

Lives in a new module `crates/nice/src/shell/` (see §8 for layout). Sketch — signatures are the
contract; doc comments here are abbreviated.

```rust
// crates/nice/src/shell/mod.rs

use std::io;
use std::path::{Path, PathBuf};

/// Which shell family a pane runs. Copy key for per-pane storage and match sites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    Zsh,
    Bash,
    /// Resolved to a shell Nice has no profile for (fish, tcsh, dash, …).
    /// Plain shell, all integration features off. Carries no path — the
    /// FallbackProfile instance holds the resolved path.
    Other,
}

/// Paths produced by writing a profile's rc files. What `spawn_argv` /
/// `inject_env` need to reference them.
pub struct InjectPaths {
    /// Directory holding the rc files (zsh: the synthetic ZDOTDIR;
    /// bash: the directory containing `nice.bashrc`).
    pub dir: PathBuf,
    /// The file argv must point at, for argv-injected shells (bash `--rcfile`).
    /// `None` for env-injected shells (zsh) and the fallback.
    pub rcfile: Option<PathBuf>,
}

/// User-side env captured at bootstrap, before any pty inherits Nice's
/// overrides. Extend per-shell as needed; zsh uses `user_zdotdir`.
pub struct UserShellEnv {
    /// Nice's own inherited `ZDOTDIR` (XDG-style zsh layouts).
    pub user_zdotdir: Option<String>,
}

/// How a pane's child is launched (mirrors today's SpawnSpec split).
pub struct SpawnCtx<'a> {
    /// `Some` ⇒ spawn WITH Nice's rc injection (normal panes, deferred-resume
    /// Claude panes). `None` ⇒ spawn the user's genuine login shell with no
    /// injection (non-deferred Claude windows, hermetic tests).
    pub inject: Option<&'a InjectPaths>,
    /// `None` ⇒ interactive shell pane. `Some(cmd)` ⇒ command pane: the shell
    /// must end up running `exec <cmd>` after rc files (PATH parity), so the
    /// command owns the pty and its exit closes the pane.
    pub command: Option<&'a str>,
}

/// Whether Nice may write COMPOSE_TRIGGER_SEQ to an idle prompt of this shell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComposeSupport {
    /// The rc files bind COMPOSE_TRIGGER_SEQ to a compose implementation
    /// (zsh ZLE widget; bash ≥ 4.3 `bind -x`). Trigger bytes are safe to send.
    Trigger,
    /// No shell-side binding exists. NEVER send trigger bytes (finding 6).
    None,
}

/// How a deferred-resume prefill line reaches the prompt (finding 8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefillStrategy {
    /// The rc tail consumes `NICE_PREFILL_COMMAND` shell-side (zsh `print -z`).
    /// Nice sets the env var and does nothing else. FROZEN for zsh.
    ShellSide,
    /// Nice types the prefill into the pty master itself, once the pane
    /// signals readiness (first OSC 7 report — the rc files fire one at
    /// startup). No env var needed; Nice holds the string app-side.
    AppTyped,
    /// No prefill. The pane opens at a bare prompt (fallback shells).
    Off,
}

pub trait ShellProfile: Send + Sync {
    fn kind(&self) -> ShellKind;

    /// Absolute path of the resolved shell binary (e.g. "/bin/zsh").
    fn program(&self) -> &str;

    /// Full argv (argv[0] included) for a pane spawn — finding 1.
    /// Must honor both SpawnCtx axes (inject × command).
    fn spawn_argv(&self, ctx: &SpawnCtx) -> Vec<String>;

    /// Shell-specific env pairs for an INJECTED spawn — finding 2.
    /// zsh: `ZDOTDIR` + `NICE_USER_ZDOTDIR`. bash: empty (injection rides
    /// argv). Generic pairs (NICE_SOCKET/NICE_TAB_ID/NICE_PANE_ID/
    /// NICE_COMPOSE_CONF) are NOT this method's job — they stay
    /// shell-agnostic in pty_manager.
    fn inject_env(&self, inject: &InjectPaths, user: &UserShellEnv) -> Vec<(String, String)>;

    /// Write this profile's rc files into `dir` (overwrite-always self-heal,
    /// same policy as today's write_stubs) — findings 2, 3, 4, 7, 8.
    /// Returns the paths spawn_argv / inject_env need.
    fn write_rc_files(&self, dir: &Path) -> io::Result<InjectPaths>;

    /// Compose capability of panes spawned from this profile — findings 4, 6.
    fn compose_support(&self) -> ComposeSupport;

    /// Prefill mechanism — finding 8.
    fn prefill(&self) -> PrefillStrategy;

    /// argv (argv[0] included) for one-shot PATH-honoring probes, e.g.
    /// `command -v -- claude` — finding 9. Must load the user's PATH the way
    /// a real login pane of this shell would.
    fn probe_argv(&self, probe_cmd: &str) -> Vec<String>;

    /// This shell's kernel comm name (MAXCOMLEN-truncated) for the orphan
    /// reaper prefilter — finding 10. ("zsh", "bash", …)
    /// Borrowed from `self`, not `&'static str` (amended per review-fable.md I3):
    /// `FallbackProfile`'s comm is the basename of a runtime-resolved path, which
    /// cannot be `'static` without leaking. Callers collect owned `String`s.
    fn comm_name(&self) -> &str;

    /// Human name for settings/help copy — findings 11, 12.
    /// Same non-`'static` treatment as `comm_name` for the fallback's runtime
    /// basename (amended per review-fable.md I3); copy builders clone as needed
    /// (plan 04's `active_display_name` returns `String` for this reason).
    fn display_name(&self) -> &str;
}
```

### Per-method rationale mapped to the inventory

| Method | Finding(s) | Replaces / feeds |
|---|---|---|
| `spawn_argv` | 1 | `spawn.rs` `ZSH_PATH` + `build_exec_args`/`build_argv`; `pty.rs` execve site is untouched (it just gets a prebuilt argv) |
| `inject_env` | 2 | the `ZDOTDIR`/`NICE_USER_ZDOTDIR` pairs in `pty_manager.rs session_window_env_pairs` (:1308-1314) and `build_claude_extra_env` (:1965-1974) |
| `write_rc_files` | 2, 3, 4, 5, 7, 8 | `shell_inject.rs write_stubs`; each profile owns its script bodies, including the compose LLM instruction's dialect word (finding 5 — the instruction lives inside the script, so no separate trait method is needed) |
| `compose_support` | 4, 6 | new capability bit; consumed by `compose_route` via the per-pane snapshot |
| `prefill` | 8 | strategy switch; zsh keeps the frozen `print -z` contract, bash gets app-typed injection |
| `probe_argv` | 9 | `app.rs run_which_claude`'s hardcoded `/bin/zsh -ilc` |
| `comm_name` | 10 | `orphan_reaper.rs:177`'s literal `"zsh"` (see §7 — the reaper matches the union across the registry, not one profile) |
| `display_name` | 11, 12 | help/tooltip copy in `shortcuts.rs` / `claude_pane.rs`, README wording |

### Module-level functions (Contract)

```rust
/// The registry: every shell Nice ships a profile for. Adding a shell = add here.
pub fn all_known_comm_names() -> &'static [&'static str];   // ["zsh", "bash"]

/// Resolve the active profile once at bootstrap (§4 order). Infallible:
/// always returns SOME profile (worst case ZshProfile at /bin/zsh).
pub fn resolve(setting: &ShellSetting) -> Box<dyn ShellProfile>;

/// The persisted user choice (§4). `Automatic` is the default.
pub enum ShellSetting { Automatic, Path(String) }
```

### The per-pane snapshot (Contract)

```rust
/// Captured at spawn, stored beside the pty handle (pty_manager's WindowPty).
/// Everything routing needs after spawn — panes never touch the live profile.
#[derive(Clone, Copy)]
pub struct PaneShell {
    pub kind: ShellKind,
    pub compose: ComposeSupport,
}
```

`PtyManager` grows an accessor `pane_shell(session_id, term_window_id) -> Option<PaneShell>`.
`compose_route` (window_state.rs:2403) gains a `compose: ComposeSupport` parameter and returns
`Trigger` only when it is `ComposeSupport::Trigger` — otherwise the existing
`ForwardCmdEnter`/`Noop` legs apply unchanged. This is the whole fix for finding 6: no trigger
bytes ever reach a shell that didn't bind them.

`PaneShell` is runtime-only. It is NOT persisted in `sessions.json`; restored panes respawn
under the then-current profile.

---

## 3. Dispatch: trait objects, keyed by a plain enum

**Decision: `Box<dyn ShellProfile>` for the single active profile; `ShellKind` (a `Copy` enum)
everywhere state is stored or matched.**

Rationale:

- The user asked for "interfaces that can be implemented", not if-ladders. A trait with one
  impl per shell (`ZshProfile`, `BashProfile`, `FallbackProfile`) puts each shell's knowledge
  in one file. Call sites hold `&dyn ShellProfile` and never match on kind for behavior.
- Trait objects beat enum-dispatch here because the fallback profile carries data (an arbitrary
  resolved path like `/opt/homebrew/bin/fish`), and the bash profile carries probed data (its
  major version, §6.3). A pure enum would force that data into variants and reintroduce
  match-everywhere.
- Exactly one profile is active per app run, resolved once — so there is no trait-object
  proliferation, no `'static` gymnastics, and dyn overhead is irrelevant (spawn-frequency
  calls).
- `ShellKind` still exists because per-pane state wants a `Copy` value and a couple of genuinely
  kind-shaped decisions (icon, copy) are fine as small matches. It is a key, not a dispatcher.
- No dynamic loading, no registration macros, no `inventory` crate: the registry is a literal
  match in `resolve()` and a literal slice in `all_known_comm_names()`. YAGNI.

---

## 4. Detection / resolution (Contract)

Resolution runs once, synchronously, at the top of `install_shell_inject_bootstrap`
(app.rs:1390) — before the reaper, before rc writing, before the claude probe (both consume the
result). Order:

1. **`NICE_SHELL` env override** — dev/test seam, same family as `NICE_COMMAND` /
   `NICE_CLAUDE_OVERRIDE`. Absolute path expected; non-absolute or empty ⇒ ignored.
2. **User setting** — `ShellSetting::Path(p)` from `ui_settings.json` (new optional key
   `advanced.shell`, absent ⇒ `Automatic`). Surfaced later as a Settings ▸ Advanced row
   (migration step 6); the plumbing lands before the UI does.
3. **`$SHELL`** — Nice's inherited env (launchd sets it for GUI apps from the account record;
   still absent in some contexts, hence step 4).
4. **`getpwuid(getuid()).pw_shell`** via libc — the account's login shell. (Not `dscl`: no
   subprocess, no parsing.)
5. **`/bin/zsh`** — last resort (matches today's behavior exactly).

The winning **path** is then mapped to a profile by executable basename: `zsh` ⇒ `ZshProfile`,
`bash` ⇒ `BashProfile`, anything else ⇒ `FallbackProfile { path }` (§5). The path is kept as
resolved — a homebrew `/opt/homebrew/bin/bash` gets the bash profile *at that path*, not
`/bin/bash`. A resolved path that doesn't exist/isn't executable falls through to the next step.

**Where the result lives:** a gpui global, extending today's `ShellInjectConfig`:

```rust
/// Installed by install_shell_inject_bootstrap, and replaced only by an explicit
/// settings pick (migration step 6). Panes still never re-resolve — they keep
/// their spawn-time PaneShell. (Amended per review-fable.md B1.)
pub struct ShellRuntime {
    pub profile: Box<dyn ShellProfile>,          // the active profile
    pub inject: Option<InjectPaths>,             // None if write_rc_files failed (non-fatal, same as today)
    pub user_env: UserShellEnv,
}
```

Window construction reads it to build `WindowShellEnv` (which becomes shell-agnostic: it keeps
`socket_path`/`compose_conf` and gains the profile-produced `inject_env` pairs instead of
hardcoded zdotdir fields). Pane spawn reads it to build argv and the `PaneShell` snapshot.

**Setting changes mid-run** apply to newly spawned panes only. Existing panes keep their
snapshot; no relaunch, no respawn. (Cheap, honest, and the Settings row copy says so.)
Mechanically (amended per review-fable.md B1): the resolve → `write_rc_files` → install-global
sequence is one reusable installer (`install_shell_runtime(cx, &ShellSetting)`), called by the
bootstrap and again by the settings pick, which replaces the `ShellRuntime` global. Nothing
re-resolves per pane.

---

## 5. Unknown-shell fallback (Contract)

`FallbackProfile { path }` — for fish/tcsh/anything unrecognized. Degrade to a working plain
terminal; never block, never garble.

| Surface | Fallback behavior |
|---|---|
| `spawn_argv` | shell pane: `[path, "-i", "-l"]`; command pane: `[path, "-i", "-l", "-c", "exec <cmd>"]`. Separate short flags (not clustered `-ilc`) — clustered parsing is not universal; separate `-i` `-l` `-c` is accepted by fish/tcsh/etc. |
| `write_rc_files` | writes nothing; returns `InjectPaths { dir, rcfile: None }` |
| `inject_env` | empty |
| `compose_support` | `None` — ⌘↩ falls through to `ForwardCmdEnter`/`Noop`, exactly the pre-feature behavior; no stray bytes (finding 6) |
| `prefill` | `Off` — deferred-resume pane opens at a bare prompt. Acceptable: strictly better than today (today the user gets a *zsh* they didn't ask for) |
| `probe_argv` | `[path, "-i", "-l", "-c", probe_cmd]` — `command -v` is POSIX and fish/tcsh support it; if the probe fails, the existing "Claude not installed ⇒ plain shell" fallback applies |
| `comm_name` | basename of `path`, truncated to 15 bytes (MAXCOMLEN) |
| `display_name` | the basename (e.g. "fish") |

Net effect for a fish user: their actual shell with their actual config, correct PATH, working
panes — minus the `claude()` shadow, compose, cwd tracking (file browser/restore keep the spawn
cwd), and prefill. No feature emits garbage or errors into their terminal.

---

## 6. Per-shell profile specifics

### 6.1 `ZshProfile` — pure extraction, byte-frozen

Everything moves; nothing changes observably.

- `program`: resolved path (normally `/bin/zsh`).
- `spawn_argv`: `["/bin/zsh", "-il"]` / `["/bin/zsh", "-ilc", "exec <cmd>"]` — the clustered
  spellings stay byte-identical (pinned by `exec_args.rs`).
- Injection stays **env-based**: `inject_env` returns `ZDOTDIR=<dir>` +
  `NICE_USER_ZDOTDIR=<user value or "">` (the always-set empty-string semantics are load-bearing
  for the `.zshenv` stub — preserved verbatim). `spawn_argv` ignores `ctx.inject` (zsh argv is
  identical with and without injection; env decides).
- `write_rc_files`: writes the four stubs. Bodies move from inline `r##` strings to
  `include_str!` files (§8) with **zero byte changes** — the frozen-contract tests keep pinning
  them.
- `compose_support`: `Trigger`. `prefill`: `ShellSide` (`print -z`, frozen).
- `probe_argv`: `["/bin/zsh", "-ilc", probe_cmd]`. `comm_name`: `"zsh"`.

### 6.2 Bash injection mechanism — the researched decision

bash has no `ZDOTDIR` analogue. Candidate channels:

| Channel | Verdict |
|---|---|
| **`--rcfile <file>`** (alias `--init-file`) | **Chosen.** Read *instead of* `~/.bashrc` for interactive shells. Caveat: it is **ignored for login shells** (`-l` makes bash read the profile chain and skip rcfile processing) — so Nice must spawn bash **non-login** and emulate the login chain inside the rcfile (below). We fully control argv, which is the one precondition this approach needs. |
| `BASH_ENV` | Rejected: sourced only by **non-interactive** shells. Nice's panes are interactive (`-i`); `BASH_ENV` would never fire for them. |
| `ENV` + `--posix` trick (ghostty/kitty style: set `ENV` to our script, start `bash --posix`, un-POSIX inside) | Rejected for YAGNI: it exists to inject when you *cannot* control argv cleanly (user-supplied shell args must pass through). Nice owns the full argv, so `--rcfile` achieves the same result with none of the POSIX-mode entry/exit subtlety. Note for the future: if Nice ever grows "custom shell arguments", revisit. |
| Writing into `$HOME` dotfiles | Rejected outright: mutating user config. |

**Spawn shapes** (`BashProfile::spawn_argv`):

- injected shell pane: `[bash, "--rcfile", <nice.bashrc>, "-i"]`
- injected command pane: `[bash, "--rcfile", <nice.bashrc>, "-i", "-c", "exec <cmd>"]`
  (bash with `-i` sources the rcfile even under `-c` — it honors the interactive flag; the
  implementation plan must include a real-bash test pinning this)
- non-injected (`ctx.inject == None`, i.e. non-deferred Claude windows and hermetic tests):
  `[bash, "-il"]` / `[bash, "-il", "-c", "exec <cmd>"]` — a *genuine* login bash reading the
  user's own profile chain, mirroring how the zsh path deliberately omits `ZDOTDIR` there.

**`nice.bashrc` structure** (single file; bash has no four-file dance because there is no
`ZDOTDIR`-restore problem — nothing needs un-redirecting):

1. **Login emulation** — because `--rcfile` forced non-login: source `/etc/profile` if present,
   then the **first existing** of `~/.bash_profile`, `~/.bash_login`, `~/.profile` (bash's
   documented login order). The user's profile conventionally sources `~/.bashrc` itself; we do
   NOT source `~/.bashrc` ourselves on top (double-source risk). A user whose PATH lives only in
   an unsourced `~/.bashrc` sees the same result they'd see in a real login bash — their
   convention, honored.
2. **Nice hooks**, defined after user config so they win (same ordering rule as zsh):
   - `claude()` shadow + `_nice_claude_exited` — bash port of finding 3's function: `${sid:0:8}`
     for the prefix, `>&2` instead of `print -u2`, `${#arr[@]}` lengths. Talks to the same
     control socket; the server side is dialect-agnostic (inventory, "already shell-agnostic").
   - OSC 7 emitter via `PROMPT_COMMAND` (finding 7): fires each prompt, so it keeps
     `_nice_last_pwd` and emits only on change (plus one unconditional startup fire — which
     doubles as the AppTyped-prefill readiness signal, §6.4). Percent-encoding uses bash-safe
     substitution (the zsh `\%` arcana does not carry over). Append cooperatively: handle both
     string `PROMPT_COMMAND` and the bash ≥ 5.1 array form.
   - Compose binding — only when supported (§6.3).
3. **Baseline dialect: bash 3.2** (macOS ships `/bin/bash` 3.2.57 forever, GPLv2). Everything in
   (1)-(2) except compose must run on 3.2: no associative arrays, no `${var^^}`, no `;&`.

**Known limitation** (documented, not fixed — same class as the existing "`exec zsh` drops the
injection" zsh limitation): under `--rcfile` the shell is not a login shell, so
`shopt -q login_shell` reports false and `logout` is unavailable. Prompt frameworks and
profile scripts that branch on it will take their non-login path. Also `exec bash` inside a
pane drops the injection, exactly like `exec zsh` does today.

### 6.3 Bash Command Compose (findings 4, 5, 6)

- Mechanism: `bind -x '"\e[5099~": _nice_command_compose'` — same `COMPOSE_TRIGGER_SEQ` bytes
  as zsh (shared constant, unchanged). The handler reads/writes `READLINE_LINE` /
  `READLINE_POINT`.
- **Version gate:** **bash ≥ 4.3**. Writable `READLINE_LINE` arrives in 4.0, but binding
  `bind -x` to a *multi-character* key sequence (our `\e[5099~` trigger) only works from 4.3 —
  a 4.0 gate would send trigger bytes to a 4.0–4.2 pane that can't consume them, recreating
  finding F6. Stock `/bin/bash` (3.2) ⇒ `ComposeSupport::None`; homebrew bash 5 ⇒ `Trigger`.
  `BashProfile` probes once at resolve time —
  `bash --norc --noprofile -c 'echo ${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}'` (no rc files ⇒
  ~ms, acceptable synchronously in bootstrap) — and stores major.minor. The rc file
  independently guards its binding with the same major.minor check so the two gates can never
  disagree in the dangerous direction. (Correction from plan 03; supersedes any "≥ 4.0"
  wording elsewhere in this doc.)
- UX degradation vs zsh, accepted: readline has no `zle -F` async fds, no
  `POSTDISPLAY`/`region_highlight`. The bash handler runs `claude -p` **synchronously** (no
  ghost-text spinner; optionally a one-line stderr notice) and replaces `READLINE_LINE` with
  the result. Enter remains the user's own keypress — the no-newline rule is unchanged.
- Dialect (finding 5): the bash script's instruction string says "bash command line" (and the
  implementation plan should say "bash 3.2-compatible… " only if compose is ever enabled below
  4 — it isn't, so plain "bash" is right). The instruction lives inside each profile's script;
  no Rust-side dialect plumbing.
- Conf parsing: the `${blob#*…}`/`${rest%%…}` surgery in the zsh widget is POSIX-parameter-
  expansion and ports to bash nearly verbatim; `NICE_COMPOSE_CONF` env key and JSON shape are
  unchanged (they are app↔shell interchange, dialect-free).

### 6.4 Bash prefill (finding 8) — `PrefillStrategy::AppTyped`

bash has no `print -z`. Rejected alternatives: `history -s` (user must press ↑ — discoverability
failure), TIOCSTI (root-gated on modern kernels), readline macro binds (contorted, still needs
app cooperation). Chosen: **Nice types the line itself** — writing bytes to the pty master is
exactly "the user typed it": editable, submitted only by the user's Enter. This is established
terminal-app practice (iTerm2 "send text at start", tmux `send-keys`).

Mechanics (Contract for the planner):

- `build_claude_prefill_command` stays the single composer (frozen wire string).
- For `ShellSide` profiles nothing changes: `NICE_PREFILL_COMMAND` env, rc tail consumes it.
- For `AppTyped` profiles, `build_claude_extra_env` does NOT set `NICE_PREFILL_COMMAND`;
  instead the spawn path records `pending_prefill: String` on the pane entry. When the pane's
  **first OSC 7** report arrives (the rc file's unconditional startup fire — guaranteed to be
  post-rc, i.e. the prompt is up or imminent), Nice writes the prefill bytes (no trailing
  newline, ever) and clears the pending slot. The OSC 7 receiver already flows through Nice's
  cwd tracking; this adds one hook at that site.
- Panes without injection, or fallback profiles, never have a pending prefill (`Off`).

### 6.5 Binary discovery (finding 9)

`run_which_claude` takes the argv from `ShellRuntime.profile.probe_argv("command -v -- claude")`
instead of hardcoding `/bin/zsh -ilc`. For bash: `[bash, "-ilc", "command -v -- claude"]` — an
interactive login bash reads the profile chain, which is where bash users' PATH lives (directly
or via their sourced `~/.bashrc`). Everything else about the probe (async, exit-0 +
absolute-path validation, `NICE_CLAUDE_OVERRIDE` seam) is untouched.

---

## 7. Orphan reaper (finding 10, Contract)

The reaper runs at bootstrap and must catch shells orphaned by **prior** runs — possibly under
a different shell setting than the current one. So it does not consult the active profile alone:

- comm prefilter accepts `all_known_comm_names()` (registry union: `["zsh", "bash"]`) **plus**
  the currently-resolved profile's `comm_name()` (covers a fallback shell like `"fish"`).
- The decisive criterion is unchanged and already shell-agnostic: PPID == 1, uid == ours, and
  the env carries `NICE_TAB_ID=`.

Accepted edge: a user who ran Nice under fallback-fish, crashed, then switched the setting to
zsh before relaunching leaves fish orphans unmatched by the prefilter. Rare (crash + setting
change between runs), bounded (the pty cap bites only after hundreds of accumulated orphans),
and self-healing the next time the fish setting is active. Not worth widening the prefilter to
env-scanning every PPID==1 process.

The bootstrap stderr line drops "zsh": `"reaped {n} orphan shell(s) from prior runs"`.

---

## 8. Code layout and script storage

```
crates/nice/src/shell/
    mod.rs          — ShellKind, ShellProfile, ComposeSupport, PrefillStrategy,
                      InjectPaths, UserShellEnv, SpawnCtx, PaneShell, registry fns
    resolve.rs      — ShellSetting + the §4 resolution chain (pure, injectable
                      inputs for tests: env snapshot, pwuid fn, setting)
    zsh.rs          — ZshProfile (absorbs today's shell_inject.rs writer/paths)
    bash.rs         — BashProfile (+ version probe)
    fallback.rs     — FallbackProfile
    scripts/
        zsh/zshenv.zsh  zprofile.zsh  zlogin.zsh  zshrc.zsh   — byte-identical moves
        bash/nice.bashrc
```

- **Scripts are static files pulled in with `include_str!`** — replacing the ~580-line
  `ZSHRC_BODY` inline string. **No templating**: every dynamic value already flows through env
  vars (`NICE_SOCKET`, `NICE_COMPOSE_CONF`, `NICE_PREFILL_COMMAND`, …) and that stays the rule.
  A profile that ever needs a baked-in value should add an env var instead. This keeps the
  frozen-contract byte-pinning tests trivial (`assert_eq!(include_str!(…), <pinned>)` shape
  survives).
- `COMPOSE_TRIGGER_SEQ` / `COMPOSE_TRIGGER_BINDKEY` move to `shell/mod.rs` (shared by both
  script sets and the Rust trigger writer).
- `shell_inject.rs` shrinks to a re-export shim during migration, then disappears.
- **`nice-term-core` stays shell-policy-free.** `SpawnSpec` changes from implying zsh to
  carrying a prebuilt argv: `spawn.rs` loses `ZSH_PATH`/`build_argv`'s hardcoding and instead
  accepts the argv the `nice` crate built via the profile. The existing convenience
  constructors (`SpawnSpec::shell/command`) keep a zsh-shaped default so term-core's hermetic
  tests and nice-itests compile unchanged; production call sites in `nice` always pass
  profile-built argv. `base_env`/`build_env`/`expand_tilde` are already shell-agnostic and stay.
- Rc-file destination directory: same per-variant Application Support location
  (`…/<CFBundleName>/zdotdir` today) — renamed `…/<CFBundleName>/shellrc` with per-shell
  subdirs (`shellrc/zsh/`, `shellrc/bash/`), written for the **active profile only** at
  bootstrap. The never-swept / overwrite-always / self-heal properties carry over verbatim.

---

## 9. Migration order

Each step lands green and shippable on its own; steps 3-5 are the natural per-feature plan
boundaries for the bash planning agents.

1. **Extract (pure refactor, zsh-only).** Introduce `shell/` with the trait, `ZshProfile`,
   `SpawnCtx`, `ShellRuntime`; move stub bodies to `scripts/zsh/*` via `include_str!`
   (byte-identical); route spawn argv, inject env, probe argv, and the rc writer through the
   profile. No behavior change; all frozen-contract and exec-args tests still pin the same
   bytes. `resolve()` exists but is pinned to zsh.
2. **Resolution + fallback + routing hygiene.** Turn on the §4 chain (`NICE_SHELL` → setting
   plumbing (no UI yet) → `$SHELL` → `getpwuid` → `/bin/zsh`); add `FallbackProfile`;
   per-pane `PaneShell` snapshot + `compose_route` gating (fixes finding 6 for every non-zsh
   shell); reaper comm-union (fixes finding 10). After this step a fish user gets a correct,
   quiet, plain fish.
3. **`BashProfile` core.** `--rcfile` spawn shapes, `nice.bashrc` with login emulation,
   `claude()` shadow port, OSC 7 `PROMPT_COMMAND` emitter. (Findings 1, 2, 3, 7 for bash.)
4. **Bash prefill + discovery.** `AppTyped` pending-prefill hook on first-OSC 7; probe via
   `probe_argv`. (Findings 8, 9.)
5. **Bash compose.** Version probe, `bind -x` handler, bash dialect instruction line.
   (Findings 4, 5.)
6. **Copy + UI.** Settings ▸ Advanced "Shell" row (Automatic / explicit path) writing
   `advanced.shell`; de-zsh the tooltip/help strings via `display_name`; README wording.
   (Findings 11, 12.)

---

## 10. Testing strategy

**Unit (pure, no pty):**
- Per-profile argv matrices: `spawn_argv` over the full `SpawnCtx` grid (inject × command),
  `probe_argv`, `inject_env` — table tests like today's `exec_args.rs`/env-matrix tests.
- Resolution chain: `resolve()` with injected env-snapshot/pwuid/setting inputs covering every
  precedence hop and the missing-binary fall-through.
- Script pinning: zsh bodies stay byte-frozen (existing static-text tests, now reading the
  `include_str!` files). Bash script gets the same treatment once its contract freezes:
  structural assertions first (binds `COMPOSE_TRIGGER_BINDKEY`, guards on `BASH_VERSINFO`,
  sources the login chain in documented order), byte-pin after step 5.
- `write_rc_files` into a tempdir: file set, `InjectPaths` shape, overwrite-self-heal.
- `compose_route` truth table gains the `ComposeSupport` axis.
- Reaper: comm-union matching (existing unit-test harness parametrizes comm).

**Hermetic pty tests (the ZDOTDIR-blanking generalization):** today's tests blank `ZDOTDIR` to
keep the user's rc out of the grid. That trick is zsh-shaped. The structural replacement is
`SpawnCtx { inject: None }` — hermeticism by *not injecting*, which works identically for every
profile — plus per-shell env quieting where the user's own rc must also be excluded:
- zsh: keep the existing empty-`ZDOTDIR` helpers (they work and pin current behavior);
- bash: hermetic spawns use `--norc --noprofile` argv (a test-only `SpawnCtx` flag or helper —
  *implementation latitude*), since bash reads `$HOME` paths that can't be redirected away.
Existing fixtures (`pty.rs:557`, `nice-itests/src/session.rs`, scenario blanking) keep working
untouched through step 2; step 3's plan adds the bash helper.

**Real-shell end-to-end (mirroring the existing real-zsh tests):**
- `/bin/bash` 3.2 is guaranteed present on every macOS — the step 3/4 e2e suite runs against
  it unconditionally: login-emulation sourcing order, `claude()` handshake against a fixture
  socket, OSC 7 emission + change-dedup, `-i -c` rcfile-sourcing pin, prefill-type-in after
  first OSC 7.
- Compose e2e requires bash ≥ 4.3: probe for one (`command -v bash` outside `/bin`) and skip
  with a notice when absent — same policy shape as other environment-gated scenarios.
- The existing zsh e2e/zpty suites run unchanged throughout (they are the frozen-contract
  regression net for step 1's extraction).

**Live scenarios:** `compose_live` and the `term-render`-family scenarios stay zsh-pinned via
`NICE_SHELL=/bin/zsh` (deterministic), with one added bash smoke scenario in step 5.

---

## 11. Summary of contract points for planners

- Implement against `ShellProfile` exactly as in §2; `ZshProfile` behavior is byte-frozen.
- One active profile, resolved once (§4), living in `ShellRuntime`; panes carry `PaneShell`.
- Compose trigger bytes are gated on the pane's `ComposeSupport` — never send on `None`.
- Bash injection = `--rcfile` + non-login spawn + in-script login emulation (§6.2);
  non-injected bash spawns are genuine `-il` login shells.
- Bash prefill = app-typed on first OSC 7 (§6.4); zsh keeps `print -z`.
- Unknown shells run plain with all integration features off (§5) — degradation, never garbage.
- Scripts are static `include_str!` files parameterized only by env vars (§8).
- Migration steps 3-5 are the intended per-feature plan boundaries (§9).
