# Manual IME pre-check — Zed's built-in terminal

**Zed IS installed** — `/Applications/Zed.app`, bundle `dev.zed.Zed`, **version 1.8.2**
(build `20260624.160235`, ~1 week old as of 2026-07-01). That build is recent enough to reflect
current-Zed-main IME behavior, so this pre-check tells us empirically whether upstream GPUI's
terminal already gets IME right today — the cheapest possible signal before writing any
`InputHandler`.

**RUN 2026-07-02 (Nick at the keyboard): A–F ALL PASS** — per-item RESULT lines inline
below; E3 skipped (no second display). **Actual Zed version at run time: 1.9.0** — the
app auto-updated after this doc pinned 1.8.2 (version read from the installed
Info.plist at write-up). Bonus finding (while diagnosing the spike bin's item 8):
⌃⌘Space Character Viewer and native dictation both WORK in Zed's terminal — the
existence proof that the spike bin's summoning gap is app/bundle-side. Full write-up:
`RESULTS-spike2-20260702.md`.

**Who runs this:** the MAIN Claude session, by hand, WITH a human at the keyboard (IME
composition, dead keys, and press-and-hold cannot be driven headlessly / by a subagent). This is
a checklist, not a script. ~10 minutes.

**Why it matters:** if Zed 1.8.2's terminal passes B, C, D, E below, that is a live existence
proof that a from-scratch GPUI-native terminal on current gpui can get IME right at the app level
(no gpui fork) — directly closing audit gap G1. If it fails, note *which* item and whether the
failure looks app-side (bad bounds / bad commit policy) or platform-side (routing) — that decides
whether the real spike must budget a `gpui_macos` patch.

---

## Setup (one-time)

1. Enable a CJK input source: System Settings → Keyboard → Text Input → Input Sources → **＋** →
   add **Chinese, Simplified → Pinyin – Simplified** (and/or Japanese → Romaji). Learn the
   input-source switch shortcut (default **⌃Space** or **⌘Space**).
2. Open Zed → open the built-in terminal (**⌃`** or Terminal: Toggle from the command palette).
3. Confirm the running Zed is 1.8.2: `zed --version` in any shell → `Zed 1.8.2`.
4. For items that need a full-screen TUI, have a `claude`-style TUI ready to run **inside Zed's
   terminal** (claude-code if available; otherwise any full-screen alt-buffer TUI such as
   `htop`, `vim`, or `python -c "import curses; ..."` — the point is a full-screen app that owns
   its own input box and parks/hides the hardware cursor). Note in results which TUI was used.

Record each item as PASS / FAIL / N/A with a one-line observation.

---

## Checklist

### A. Baseline sanity (no IME)
- Switch to the ABC/US input source. Type `echo hello` + Return. **Expect:** normal echo, Return
  submits. Confirms the terminal + keybindings work before IME is involved.

Result: PASS

### B. CJK compose + commit, and **Enter mid-composition must COMMIT, not newline** (zed#23003)
1. Switch to Pinyin. In the shell prompt, type `nihao`. **Expect:** an underlined preedit
   `nihao` with a candidate window showing 你好 etc. (preedit should NOT be sent to the shell yet).
2. Press **Return/Enter to confirm the first candidate.** **Expect (PASS):** 你好 is committed to
   the prompt as text, and **NO newline / command submission happens.** **FAIL:** the line
   submits / a newline appears (this is the zed#23003 regression). Record exactly what happened.
   RESULT: hitting enter results in the text showing up as nihao. it does not show up as 你好.
   However, that matches the global behavior. No matter what app I am in, hitting enter shows up
   as "nihao", not as "你好". However, if I hit SPACE instead of ENTER, then it does show up as
   "你好", which matches global behavior. so looks like it works correctly.
3. Press Return again (now not composing). **Expect:** the line submits normally. 
   RESULT: Pass
4. Preedit editing: type `nihao`, press **←/→** and **Backspace** while composing. **Expect:**
   arrows/backspace edit the *preedit / candidate selection*, they do NOT move the shell cursor or
   delete prompt chars. (Tests that non-printing keys route to the IME while composing.)
   RESULT: Pass

### C. Dead keys (option+e, then e → é)
1. Switch to **ABC** (or U.S.) input source — dead keys need a layout that has them (ABC does;
   "U.S." classic may not — if é doesn't appear, switch to "ABC" or "U.S. International – PC").
2. Type **⌥e** (Option+e). **Expect:** an underlined `´` preedit (dead-key accent pending).
   RESULT: PASS
3. Type **e**. **Expect (PASS):** `é` committed. **FAIL:** you get `¥`/`ee`/`´e`, or an ESC
   sequence (would indicate Option is being consumed as Meta instead of as compose).
   RESULT: PASS
4. Note: if Zed's terminal has an "Option as Meta" setting enabled, ⌥e may intentionally send
   ESC-e instead — check Zed terminal settings and record the setting state alongside the result.

### D. Press-and-hold accent popover (hold e → è é ê ë ē)
1. ABC input source. **Press and hold `e`.** **Expect one of two acceptable behaviors — record
   which:** (i) an accent popover appears and a number/arrow selects an accent (press-and-hold ON);
   or (ii) the key auto-repeats `eeee…` (press-and-hold OFF, the terminal convention). Either can
   be "correct" for a terminal; the point is to see which policy Zed's terminal uses so we match
   it. A FAIL is a hang, a stuck popover that eats subsequent keys, or a crash.
RESULT: PASS

### E. Candidate-window ANCHOR position (zed#46055) — the load-bearing one
1. At the **shell prompt** (no TUI): switch to Pinyin, type `nihao`. **Observe where the
   candidate window appears.** **Expect (PASS):** anchored at/just below the text cursor.
   RESULT: PASS
2. Now run the full-screen TUI (claude-code / vim / htop) **inside Zed's terminal**. If it has a
   text input box (claude-code does; in vim use insert mode `i`), switch to Pinyin and type
   `nihao` **into that input box.** **Observe the candidate window.**
   - **PASS:** anchored at the TUI's input caret.
   - **FAIL (the reported bug):** candidate window jumps to the **screen's bottom-left corner**
     (and, on a multi-monitor setup, possibly onto the built-in display even if Zed is on an
     external monitor). Record which TUI triggered it.
   RESULT: PASS
3. If you have a second display, drag the Zed window to the external monitor and repeat step 2 —
   note whether the candidate window follows the window or lands on the built-in display.
   RESULT: Not hooked up to a second display, so skipping this test.

### F. Keybindings still fire (no regression from IME plumbing)
1. With an IME (Pinyin) active but **not composing**, exercise terminal keybindings that were the
   historical casualties: **⌘← / ⌘→** (move to line start/end), **⌥← / ⌥→** (word motion),
   **⌘K** / **⌘C** / **⌘V** as bound. **Expect:** all fire normally — the IME being *active* must
   not swallow them when you're not composing.
   RESULT: PASS
2. Type a normal command with the IME active-but-idle (ASCII passes straight through): `ls` +
   Return. **Expect:** normal execution.
   RESULT: PASS

---

## Interpreting results

- **B, C, E all PASS** → strong evidence the from-scratch terminal InputHandler is app-solvable on
  current gpui with no fork. Closes G1's cheap pre-check; proceed to the real spike per
  `IMPLEMENTATION-PLAN.md`, on a pinned zed main rev.
- **E FAILs (bottom-left)** → confirms the anchor is terminal-side and that even Zed hasn't fixed
  it; our terminal must get `bounds_for_range` right (never `None` while composing; anchor to the
  grid cursor). Does not block us — it's our code to write.
- **B FAILs (Enter → newline)** → confirms zed#23003 is still live in Zed's own terminal; our
  terminal must implement the commit-swallow flag (plan §Enter). Still app-level; not a blocker.
- **F FAILs** → the more serious signal: it would mean the arbitration itself eats keybindings
  when an IME is active. Given main's `!modifiers.platform` guard this is *not expected*; if it
  happens, capture the exact key and flag that the real spike may need the `gpui_macos`
  `handle_key_event` routing patch (contingency in SCOPE §5).

Record the Zed build (`zed --version`), macOS version, the input sources used, and the TUI used
for E alongside the PASS/FAIL grid so the result is reproducible.
