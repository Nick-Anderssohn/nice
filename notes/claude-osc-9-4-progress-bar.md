# Claude Code's OSC 9;4 progress bar is disabled in Nice

## Summary

Claude Code can emit **OSC 9;4 progress sequences** during long-running
operations — the escape codes that drive a progress indicator in the
terminal tab / Dock / taskbar (the ConEmu / Windows-Terminal / iTerm2 /
Ghostty progress protocol). In Nice this is **silently disabled**, not
because of anything in our terminal emulation, but because Claude gates
the feature on a non-empty `TERM_PROGRAM_VERSION`, and Nice's Ghostty
spoof leaves that variable empty.

This is **not worth fixing right now** — it's a minor cosmetic feature and
SwiftTerm/Nice doesn't currently render an OSC 9;4 progress bar anyway. This
note exists so that if we ever *do* want it, we know exactly what to flip
and what the (non-)tradeoffs are. **No code change has been made.**

## Why it's off

Nice advertises itself to Claude as Ghostty for OSC-title purposes —
`TabPtySession.buildClaudeExtraEnv` (`Sources/Nice/Process/TabPtySession.swift`)
sets `TERM_PROGRAM=ghostty` but **not** `TERM_PROGRAM_VERSION`, and the pty
inherits `TERM=xterm-256color` + `COLORTERM=truecolor` from SwiftTerm's
defaults. So the env Claude actually sees is:

```
TERM=xterm-256color
TERM_PROGRAM=ghostty
TERM_PROGRAM_VERSION=        # empty
COLORTERM=truecolor
```

Claude's progress-reporting gate (function `ASH` in the bundled CLI, which
backs the `progressReporting` setting documented as *"Emit OSC 9;4 progress
sequences during long-running operations"*) is, in essence:

```js
if (settingOverride !== undefined) return settingOverride;
if (!process.stdout.isTTY) return false;
if (WT_SESSION) return false;
if (ConEmu*) return true;
const v = coerce(TERM_PROGRAM_VERSION);
if (!v) return false;                       // ← empty version → OFF (this is us)
if (TERM_PROGRAM === "ghostty")  return v >= 1.2.0;
if (TERM_PROGRAM === "iTerm.app") return v >= 3.6.6;
return false;
```

Because `TERM_PROGRAM_VERSION` is empty, `coerce("")` is falsy and the gate
returns `false` before it ever looks at the Ghostty branch.

This was found by reverse-engineering the `claude` binary
(`~/.local/share/claude/versions/<v>`, a Bun-compiled Mach-O with embedded
JS) while diagnosing the unrelated Cmd+C bug. It is the **only** Claude
feature gated on the Ghostty *version*: truecolor rides on `COLORTERM`,
hyperlinks (OSC 8) ride on Claude's own terminal allowlist that includes
`"ghostty"`, and notifications / terminal-name / etc. ride on
`isGhostty()` (`TERM==="xterm-ghostty" || TERM_PROGRAM==="ghostty"`), all of
which Nice already satisfies via `TERM_PROGRAM=ghostty`.

## How to enable it later (if we decide to)

Set `TERM_PROGRAM_VERSION` to a value `>= 1.2.0` alongside the existing
`TERM_PROGRAM=ghostty` in `buildClaudeExtraEnv`. That alone satisfies the
gate. We'd also need SwiftTerm to actually *do something* with the inbound
OSC 9;4 sequences (show a progress bar / dock badge), which today it
ignores — so enabling the env without renderer support just makes Claude
emit codes that go nowhere. Treat the env flip and the SwiftTerm renderer
work as one unit if we pick this up.

## The spinner question — investigated, NOT a blocker

There was a worry that "completing" the Ghostty identity might break Nice's
animated status dot, because **Nice infers pane status by sniffing Claude's
output**. Worth recording the finding so we don't re-chase it:

- Nice's status detection is **OSC-title based**, not content-based.
  `SessionsModel.paneTitleChanged` (`Sources/Nice/State/SessionsModel.swift`,
  ~line 442) reads the *leading character* of the window title Claude sets
  via OSC 0/1/2: a **braille char (U+2800–U+28FF) ⇒ `.thinking`**, a
  **sparkle `✳` (U+2733) ⇒ `.waiting`**.
- In the Claude bundle, that **title** status prefix is *not* TERM-gated:
  the thinking spinner is a fixed braille array `Lr4 = ["⠂","⠐"]`
  (U+2802/U+2810) and the waiting glyph is `Zr4 = "✳"` (U+2733), both plain
  consts with no `TERM` branch. (`O2H` dispatches `SET_TITLE_AND_ICON`.)
- The **only** TERM-gated spinner is a *separate, in-content* animation
  (`qzH`): on `TERM === "xterm-ghostty"` its frames are `· ✢ ✳ ✶ ✻ *`, vs.
  `· ✢ ✳ ✶ ✻ ✽…` otherwise. Nice never reads this one.

Conclusion: **the TERM-gated spinner is the in-content one Nice ignores; the
title spinner Nice depends on is TERM-independent.** So even setting
`TERM=xterm-ghostty` wouldn't break the status dot.

Empirical clincher: Nice's status dot already works today with
`TERM=xterm-256color`, and `qzH`'s *default* (non-ghostty) frames
(`· ✢ ✳ ✶ ✻ ✽`) aren't braille either — so if the title used `qzH` the dot
would already be broken on our current identity. It isn't, which proves the
title is fed by the braille `Lr4`, not `qzH`. (Verified by tracing the title
builder to `O2H → SET_TITLE_AND_ICON`; `qzH`'s only consumers are in-content
animation builders.)

And it's moot regardless: the progress-bar gate keys on
`TERM_PROGRAM_VERSION` (+ `TERM_PROGRAM`), **not** on `TERM`. So the only
change needed to enable progress reporting is adding `TERM_PROGRAM_VERSION`
— we never need to touch `TERM`, and there's no reason to (changing
`TERM` to `xterm-ghostty` would only swap that cosmetic in-content spinner
and risk Ghostty-specific terminfo assumptions for zero gain here).

## See also

- Cmd+C-doesn't-copy in fullscreen is a separate, confirmed **Claude** bug
  (not Nice): upstream
  [anthropics/claude-code#65844](https://github.com/anthropics/claude-code/issues/65844).
  Nice forwards `ESC[99;9u` correctly; it's not version-gated.
