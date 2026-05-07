# Stop the per-launch ZDOTDIR from eating shell-tool config

## Context

`MainTerminalShellInject.make()` writes a fresh `ZDOTDIR` to
`$TMPDIR/nice-zdotdir-<pid>/` on every launch and points zsh at it.
The synthetic `.zshrc` chains back to `$HOME/.zshrc` (line 79) and
then layers on Nice's `claude()` shadow function and OSC 7 cwd hook.
Clean idea, but it traps any shell tool that mutates "the user's
zshrc" or stores config under `${ZDOTDIR:-$HOME}`:

- **Powerlevel10k** writes the instant-prompt block + `source
  ~/.p10k.zsh` line to `$ZDOTDIR/.zshrc` and dumps its config to
  `$ZDOTDIR/.p10k.zsh`. `NiceServices.cleanupStaleArtifacts` (and the
  per-launch teardown at `NiceServices.swift:177-178`) deletes the
  temp dir on exit, so every launch the wizard re-runs from scratch.
  Confirmed in this session — the user ran `p10k configure`
  successfully four times and `~/.p10k.zsh` never appeared.
- **oh-my-zsh's installer**, **nvm install script**, **asdf**,
  **fnm**, **rbenv**, **starship init zsh** — all the same pattern.
  Anything that does `echo '...' >> ~/.zshrc` while running under our
  ZDOTDIR appends to the temp file instead.

`$ZDOTDIR` is correct and standard from zsh's perspective; the
problem is purely that we're hosting a shell session in a directory
the user thinks is ephemeral but tooling thinks is durable.

This bites first-run UX badly: the user followed standard
oh-my-zsh + p10k instructions and saw the wizard re-prompt every
launch with no obvious cause.

## Approach

Three options, rough order of effort vs. payoff. Pick one (or layer
1 on top of 2/3 as cheap insurance).

### Option 1 — Warn the user when the temp `.zshrc` is dirty (cheap)

On Nice exit, before deleting the temp dir, diff the synthetic
`.zshrc` against the body Nice originally wrote (we have it as a
constant in `MainTerminalShellInject.swift`). If they differ, post a
notification: *"A shell tool wrote to Nice's per-launch `.zshrc`.
These changes will be lost. Likely culprit: powerlevel10k / nvm /
oh-my-zsh / starship. Move them to `~/.zshrc` to persist."* Same diff
check for `$ZDOTDIR/.p10k.zsh` and any other extension files that
appear there (it's a temp dir we own — ls is fine).

Doesn't fix the root cause, but turns a silent loss into a clear
signal. Useful even if we ship one of the other options below,
since some tools would still surprise users.

### Option 2 — Don't synthesize a `.zshrc`; inject via `precmd_functions` (medium)

Instead of redirecting `ZDOTDIR`, leave it alone and inject Nice's
shell functions through a different channel:

- Set `ZDOTDIR=$HOME` (or unset entirely) for the pty.
- Set `NICE_SHELL_INIT=$NICE_RESOURCES/nice-shell-init.zsh` in the
  pty environment.
- Add a one-liner to the user's real `~/.zshrc` (with permission, or
  via a "Set up shell integration" button in Settings, the same way
  iTerm2 and Ghostty do): `[[ -n "$NICE_SHELL_INIT" ]] && source
  "$NICE_SHELL_INIT"`.

Now p10k / nvm / et al. write to the actual `~/.zshrc`, the user's
real config is the source of truth, and Nice's hooks load
unconditionally when running inside Nice. The shell-integration
opt-in is a one-time prompt; users who decline get a working terminal
without the `claude()` shadow + OSC 7, exactly the behavior every
other terminal app falls back to.

Risks: requires user consent to touch `~/.zshrc` (we shouldn't write
silently), and the OSC 7 + `claude()` hooks need to be idempotent
under double-source (someone running tmux inside Nice, etc.). Both
solvable; today's hooks are already idempotent in practice.

### Option 3 — Keep the synthetic `.zshrc`, but persist it across launches (medium)

Move the temp dir from `$TMPDIR/nice-zdotdir-<pid>` to a stable
location like `~/Library/Application Support/Nice/zdotdir/`, drop the
per-pid suffix, and stop deleting it on exit. p10k's config now
survives across launches without any user-visible weirdness — the
wizard runs once, writes to `~/Library/Application
Support/Nice/zdotdir/.zshrc` + `.p10k.zsh`, and stays there.

Trade-off: this still hides the user's terminal config in a place
they can't easily find or version-control, and it splits config
between two locations (real `~/.zshrc` for non-Nice shells + the
Nice-only one). It also means `NICE_SOCKET` writes from past runs are
permanent in `.zsh_history` we no longer own. But it's the
lowest-friction fix from the user's perspective: no consent prompts,
no breakage of existing behavior, just "wizard runs once and sticks."

## Recommendation

Ship **1 immediately** (low risk, high signal — covers tools we
haven't thought of yet). Then plan **2** as the proper fix: it's the
pattern every modern terminal converged on, makes shell integration
opt-in, and gets us out of the business of synthesizing rc files.
Skip **3** unless 2 turns out to be more invasive than expected — it
trades one footgun for another.

## Files to touch (Option 2 sketch)

- `Sources/Nice/Process/MainTerminalShellInject.swift` — repurpose to
  emit the static init script to bundle resources at build time
  rather than a per-launch temp dir. Or move the script body to a
  Resources/.zsh file so it's plain text.
- `Sources/Nice/State/NiceServices.swift` — drop the `zdotdirPath`
  field, drop the cleanup pass, set `NICE_SHELL_INIT` in the env
  override path instead.
- `Sources/Nice/State/SessionsModel.swift:208-209` — replace
  `extraEnv["ZDOTDIR"] = zdotdirPath` with
  `extraEnv["NICE_SHELL_INIT"] = ...`.
- `Sources/Nice/Process/TabPtySession.swift:106-108, 282-283` —
  same, swap `ZDOTDIR` for `NICE_SHELL_INIT`.
- New: a Settings pane row "Shell integration: install / uninstall"
  that idempotently inserts/removes a single line into `~/.zshrc` and
  `~/.bashrc` (and surfaces the line so power users can copy it
  themselves).

## Repro

1. Fresh user, follows oh-my-zsh + powerlevel10k install docs in a
   Nice terminal.
2. Runs `p10k configure`, picks a style, completes the wizard.
3. Quits and relaunches Nice. Wizard runs again from scratch.
4. `ls ~/.p10k.zsh` shows no file. `cat /var/folders/.../T/nice-
   zdotdir-<pid>/.zshrc` and `.p10k.zsh` show the wizard's output —
   in a directory that's about to be deleted.

Found while setting up oh-my-zsh + p10k for the user's daily-driver
terminal.
