# InputHandler implementation plan — gpui-native terminal (for a later builder agent)

Concrete plan for the eventual `InputHandler` on the from-scratch GPUI-native terminal
(`gpui_term`). This is a PLAN — do not treat prior sections as implemented. Read `SCOPE.md`
first for the evidence and the verdict; run `MANUAL-PRECHECK.md` first for the cheap signal.

**Non-negotiables before you start:**
- Build against a **pinned current zed git rev** (gpui + gpui_platform + gpui_macos + gpui_macros
  from git), NOT crates.io gpui 0.2.2. The needed hooks (`accepts_text_input`,
  `prefers_ime_for_printable_keys`) and the `!modifiers.platform` routing fix exist **only on
  main**. On 0.2.2 this plan is not fully implementable without a gpui fork.
- **Never** read/template Zed's `terminal` / `terminal_view` crates (GPL-3.0-or-later; Nice is
  MIT). Template only from alacritty (Apache-2.0) and termwiz (MIT). See SCOPE §4.
- The `InputHandler` bridges **composition only**. Normal key encoding (kitty/legacy CSI-u) is a
  separate work package (audit G7, ~800-950 LOC) in the key-down callback — out of scope here
  except at the two seams called out below (Enter-commit, Option-as-Meta).

---

## Data model

Add to the terminal view state:

```
struct ImeState {
    preedit: String,              // current marked/composing text (UTF-8); empty = not composing
    preedit_sel: Range<usize>,    // caret/selection within preedit, UTF-16 offsets
    committed_this_cycle: bool,    // set by replace_text_in_range, read+cleared in key-down callback
}
```

Key facts that shape the design:
- A terminal has **no editable document** — the "document" the IME sees is *only the preedit
  buffer*. Committed text leaves immediately (written to the pty), so document length is
  `preedit.utf16_len()` (0 when idle). This is the standard trick that makes NSTextInputClient
  work for a non-document surface.
- NSTextInputClient offsets are **UTF-16**; the terminal/Rust world is UTF-8. Every range crossing
  the boundary needs conversion. Keep one helper pair (`utf16_to_byte`, `byte_to_utf16`) over the
  preedit buffer.

Wire it in `paint`: call `window.handle_input(focus_handle, ElementInputHandler::new(bounds, view), cx)`
(the `EntityInputHandler` → `ElementInputHandler` path; `gpui_platform::input` /
`gpui::input`). The terminal view implements `EntityInputHandler` (the per-view ergonomic trait,
`input.rs`), which gpui adapts to the platform `InputHandler`.

---

## Trait methods — implementation notes

`EntityInputHandler` (per-view) method-by-method. Mapping to NSTextInputClient in SCOPE §1.

- **`marked_text_range`** → `if preedit.is_empty() { None } else { Some(0..preedit.utf16_len()) }`.
  This is what makes gpui's `is_composing` true (`window.rs` arbitration). Trivial.
- **`selected_text_range`** → `Some(UTF16Selection { range: preedit_sel, reversed: false })` while
  composing (caret at end of preedit); when idle return `Some(0..0)`. **Never `None`** — some IMEs
  misbehave on `None`.
- **`text_for_range`** → substring of `preedit` for the requested UTF-16 range (clamp; convert).
- **`replace_and_mark_text_in_range`** (setMarkedText) → replace `preedit` (in the given range, or
  whole) with `new_text`, set `preedit_sel = new_selected_range`, request repaint. **No pty write.**
  This is the preedit-update path (every keystroke while composing CJK, and the dead-key `´` stage).
- **`replace_text_in_range`** (insertText) → **COMMIT.** Write `text` as UTF-8 bytes to the pty
  (raw bytes — NOT through the kitty key encoder; committed IME text is data, not keys). Then clear
  `preedit`, clear `preedit_sel`, and set `committed_this_cycle = true`. Request repaint.
- **`unmark_text`** → clear `preedit` / `preedit_sel` (drop composing state). No pty write.
- **`bounds_for_range`** → **the candidate-window anchor; the #46055 fix.** Map the current
  terminal **grid cursor** (row, col) to an element-local `Bounds<Pixels>` using the cell metrics
  (cell_width, line_height, origin). Return `Some(rect)` for the cursor cell. **Never return `None`
  while composing** (None → gpui emits `NSRect(0,0,0,0)` → screen bottom-left; SCOPE §3). If the
  running app hid the cursor (DECTCEM off) or parked it, still return the last known / current grid
  cursor cell rect — matching Terminal.app's behavior.
- **`character_index_for_point`** → map a window point into a preedit index (or `Some(0)`); minimal
  for a terminal but implement rather than panic.
- **`apple_press_and_hold_enabled`** → return **`false`** (terminal convention, like iTerm2: held
  key → key-repeat to the pty, not an accent popover). Revisit only if MANUAL-PRECHECK item D
  argues the popover is wanted.
- **`accepts_text_input`** → `true`.
- **`prefers_ime_for_printable_keys`** → **`false`** — matches Zed's own terminal
  (`gpui_platform/src/platform.rs:1470-1472` docstring: "The terminal keeps the default `false` so
  that raw keys reach the terminal process"). Returning `true` would send printable keys to the IME
  first to protect multi-stroke keybindings — but a terminal has no `jj`-style keybindings and wants
  raw keys, so keep `false`.

---

## §Enter — Enter-during-composition must commit, not newline (zed#23003)

The arbitration (`gpui_macos/src/window.rs::handle_key_event`, ~2081-2211) routes Enter to the IME
first while composing. The IME commits via `insert_text` → our `replace_text_in_range`, then may
also emit `doCommandBySelector(insertNewline:)`, which gpui re-dispatches as an Enter `KeyDown` to
our key-down callback (`do_command_by_selector`, `:2808-2821`) — and **our callback decides
`propagate`.**

Policy in the key-down callback:
```
on key_down(Enter):
    if ime.committed_this_cycle {      // a commit happened in this same handleEvent cycle
        ime.committed_this_cycle = false;
        return handled (propagate = false);   // swallow — do NOT send \r
    }
    send "\r" (or CR/LF per mode) to pty;
    return handled;
```
Clear `committed_this_cycle` at the *start* of each native key cycle that is NOT a commit, so a
plain Enter (no preceding commit) always sends `\r`. This is entirely app-level; no gpui change.

This is also the seam where the historical Zed fix (PR #27572) regressed cmd-left/right — **on main
that is already prevented** by the `!modifiers.platform` guard, so scoping the swallow to Enter
(and not to `platform`-modified keys) is safe.

---

## §DeadKeys — Option+e then e → é vs Option-as-Meta

Dead keys arrive as `setMarkedText("´")` then `insertText("é")` — they Just Work if the
InputHandler is wired AND the terminal is not intercepting Option as Meta for that key. But a
terminal commonly maps Option→Meta (ESC prefix). **These conflict.** Resolution:
- Make Option-as-Meta a config toggle (mirror macOS "Use Option as Meta key" / the per-profile
  setting terminals expose).
- When Option-as-Meta is OFF (default for IME users): let ⌥e reach the IME (the arbitration already
  routes it — ⌥ is `alt`, not `platform`, and produces a `key_char`), so dead keys compose.
- When Option-as-Meta is ON: the key-down callback encodes ESC+e and should NOT feed the IME. Decide
  precedence explicitly and test both with MANUAL-PRECHECK item C.

---

## §Preedit rendering

The grid has no text document, so the preedit must be drawn as an overlay: render `preedit` inline
starting at the grid cursor cell with an **underline** (and the candidate/selection sub-range styled
per `preedit_sel`), pushing nothing into the grid model. On commit it's replaced by whatever the pty
echoes back. Keep it a paint-time overlay keyed off `ImeState`, invalidated on every
setMarkedText/commit/unmark.

---

## Kitty-encoder integration point

Not part of InputHandler. Two seams only:
1. **Committed IME text** (`replace_text_in_range`) bypasses the key encoder → raw UTF-8 to pty.
2. **Normal keys** flow through the key-down callback → the kitty/legacy encoder → pty. Template
   that encoder from **alacritty `alacritty/src/input/keyboard.rs`** (Apache-2.0) or **termwiz
   `KeyboardEncoding::Kitty`** (MIT) — see SCOPE §4 for URLs. During composition the encoder must be
   **suppressed** (keys belong to the IME); gate it on `!ime.preedit.is_empty()` plus the
   arbitration already routing composing keys to the IME.

---

## Test matrix (the real spike's gate)

Run headed on a pinned zed main rev; where possible automate, but IME composition needs a human
(same items as MANUAL-PRECHECK, now against *our* terminal):

| # | Case | Pass criterion |
|---|---|---|
| 1 | CJK compose + commit (Pinyin `nihao`→你好) | preedit underlined, no pty write until commit; commit writes UTF-8 你好 |
| 2 | **Enter mid-composition** | commits candidate, **no `\r`** to pty; a following bare Enter does send `\r` |
| 3 | Preedit editing (←/→/Backspace while composing) | edits preedit, not the pty/grid |
| 4 | Dead keys ⌥e e → é (Option-as-Meta OFF) | é committed; with Option-as-Meta ON → ESC-e |
| 5 | Press-and-hold `e` | key-repeat to pty (`apple_press_and_hold_enabled=false`); no stuck popover |
| 6 | **Candidate anchor over a claude-code-style TUI** | window at the cursor cell, never screen bottom-left; correct on multi-monitor |
| 7 | Keybinding non-regression (⌘←/→, ⌥←/→, ⌘C/V) with IME active-not-composing | all fire; ASCII passes through |
| 8 | Dictation + Character Viewer insert | text committed via insertText path (zed#30446 class) |
| 9 | UTF-16/UTF-8 boundary (emoji, surrogate pairs in preedit) | offsets correct, no panic/truncation |

**Gate:** 1-3, 6, 7 must PASS to declare IME app-solvable with no gpui fork (closes audit G1).
If any *routing* case (7, or an Enter/arrow edge in 2) can't be fixed app-side, escalate to the
contingency gpui patch: `crates/gpui_macos/src/window.rs::handle_key_event`, routing predicate
~line 2152-2158, ~10-30 LOC — and record it as a maintained-fork cost against Path B.

**Size reminder:** ~400-500 LOC for the InputHandler + preedit + commit-flag + bounds mapping;
the kitty key encoder (~800-950 LOC) is the separate G7 package.
