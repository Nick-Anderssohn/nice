# IME / text-input gate — scope & verdict (spike #2, rank-2)

READ-ONLY research. Scopes the from-scratch GPUI-native terminal's `InputHandler`.
Does NOT implement it. Companion files: `IMPLEMENTATION-PLAN.md`, `MANUAL-PRECHECK.md`.

Charter: `.claude/handoff/handoff-20260702-034100.md` ("IME gate RE-OPENED"),
audit `notes/rewrite-research-audit-20260701.md` G1 (lines 151-153),
report §13 spike 2.

---

## TL;DR verdict

**IME is app-solvable on *current zed main* without patching gpui — but only on main, not on 0.2.2.**
Between 0.2.2 and main, gpui's mac input layer grew exactly the hooks the audit said were
missing, and fixed the regression that killed the historical Zed fix. Concretely:

1. **Candidate-window anchor (zed#46055) is 100% terminal-side.** gpui's
   `first_rect_for_character_range` just linearly maps whatever `bounds_for_range` our
   `InputHandler` returns into screen coords; when it returns `None` gpui emits
   `NSRect(0,0,0,0)` = the screen's bottom-left — which is *exactly* the reported symptom.
   Fix = return `Some(cursor-cell rect)` always while composing. No gpui change.

2. **Enter-during-composition commit (zed#23003) is app-solvable** via a "just committed"
   flag checked in the terminal's key-down callback, because `do_command_by_selector`
   re-dispatches the Enter keystroke back to our callback and *we* decide `propagate`.

3. **The keybinding regression that reverted the historical Zed fix (PR #27572, cmd-left/right)
   is already fixed on main** by a `!modifiers.platform` guard in the routing predicate, plus a
   new app-overridable hook `prefers_ime_for_printable_keys()`. So the audit's "app code CANNOT
   override the arbitration" is **true for 0.2.2 but no longer true for main.**

**A gpui patch is contingency, not baseline.** Hold one small (~10-30 line) patch to
`crates/gpui_macos/src/window.rs::handle_key_event` (the routing predicate, ~line 2152) *only
if* the live spike surfaces a key-routing case that the `do_command_by_selector` re-dispatch
can't cover. Baseline plan needs no fork.

**InputHandler size:** ~400-500 LOC for a first correct pass (11 trait methods, mostly small +
a preedit buffer + inline preedit render + the commit-swallow flag + grid-cursor→pixel bounds).
This EXCLUDES the kitty key encoder (~800 LOC, tracked separately as audit G7 — that encodes
*keys*, not composed text, and is a different work package).

**Run the spike on a pinned current zed git rev, NOT crates.io gpui 0.2.2.** 0.2.2 lacks
`accepts_text_input`, `prefers_ime_for_printable_keys`, and the `!platform` routing fix; on 0.2.2
the gate would look artificially worse and might appear to need a fork.

---

## 1. The GPUI InputHandler contract

### On-disk 0.2.2 (spike-only baseline)

The IME bridge is a near-1:1 exposure of `NSTextInputClient`. The public trait an app
implements:

- `trait InputHandler` — `/Users/nick/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-0.2.2/src/platform.rs:995-1085`
- `trait EntityInputHandler` (the ergonomic per-view form, wrapped by `ElementInputHandler`) — `.../gpui-0.2.2/src/input.rs:10-73`
- `pub(crate) struct PlatformInputHandler` (the bridge the platform calls) — `.../gpui-0.2.2/src/platform.rs:853-940`

Methods (0.2.2), each mapping to an NSTextInputClient selector:

| Trait method | NSTextInputClient selector | Terminal semantics |
|---|---|---|
| `selected_text_range` | `selectedRange` | caret in the preedit buffer |
| `marked_text_range` | `markedRange` / drives `hasMarkedText` | `Some` iff composing |
| `text_for_range` | `attributedSubstringForProposedRange` | substring of preedit |
| `replace_text_in_range` | `insertText:replacementRange:` | **COMMIT** → bytes to pty |
| `replace_and_mark_text_in_range` | `setMarkedText:selectedRange:replacementRange:` | update preedit (underlined) |
| `unmark_text` | `unmarkText` | clear preedit |
| `bounds_for_range` | `firstRectForCharacterRange:actualRange:` | **candidate-window anchor** |
| `character_index_for_point` | `characterIndexForPoint:` | point→preedit index (low value for a terminal) |
| `apple_press_and_hold_enabled` | (governs press-and-hold vs key-repeat) | default `true` |

The mac bridge that calls these lives in `.../gpui-0.2.2/src/platform/mac/window.rs`:
NSTextInputClient protocol registration at `:196-250`; `insert_text` `:2234`;
`set_marked_text` `:2252`; `unmark_text` `:2275`; `first_rect_for_character_range` `:2193-2218`;
`do_command_by_selector` `:2310-2325`.

### Current zed main (the real target)

The trait **moved** and **grew two methods**. The tree split `gpui` into per-platform crates
(`gpui`, `gpui_platform`, `gpui_macos`, `gpui_macros`, `gpui_linux`, …).

- `trait InputHandler` is now in `crates/gpui_platform/src/platform.rs:1369-1476` (fetched from
  `zed-industries/zed` main, 2026-07-01). All eight 0.2.2 methods are unchanged in signature.
  **New on main:**
  - `accepts_text_input(&mut self, …) -> bool` (default `true`) — `platform.rs:1461`
  - `prefers_ime_for_printable_keys(&mut self, …) -> bool` (default `false`) — `platform.rs:1473`,
    with the load-bearing docstring at `:1470-1472`: *"The editor overrides this based on whether
    it expects character input (e.g. Vim insert mode returns `true`, normal mode returns `false`).
    **The terminal keeps the default `false` so that raw keys reach the terminal process.**"*
- The mac bridge moved to `crates/gpui_macos/src/window.rs`: `first_rect_for_character_range`
  `:2688-2716`, `do_command_by_selector` `:2808-2825`.

### The platform key-dispatch arbitration (the audit's window.rs:1660-1765)

This is the un-overridable-from-app-code claim. Current real locations:

- **0.2.2:** `handle_key_event` at `.../gpui-0.2.2/src/platform/mac/window.rs:1660-1775`.
  `is_composing` computed at `:1696-1699`; the routing predicate at `:1709-1713`:
  ```
  if is_composing
     || (key_char.is_none() && !modifiers.control && !modifiers.function)
  ```
  → route the native event to `inputContext handleEvent:` FIRST; only if the IME neither
  handled it nor set `do_command_handled` does the app's keybinding callback run.
  **On 0.2.2 this predicate is fixed policy — app code cannot alter which keys go to the IME.**
  This is what the audit correctly described.

- **Current main:** `handle_key_event` at `crates/gpui_macos/src/window.rs:2081-2211`.
  `is_composing` at `:2117`. The predicate gained (a) a **new `is_ime_printable_key`** branch
  (`:2137-2150`) that calls the app hook `query_prefers_ime_for_printable_keys()` (wrapper at
  `gpui_platform/src/platform.rs:1346`, trait method `prefers_ime_for_printable_keys` at `:1473`)
  gated on `is_ime_input_source_active()` (`window.rs:2054`); and (b) a **`!modifiers.platform`
  term** added to the non-printing-key branch (`:2152-2158`). Comment at `:2130-2131` states the
  `!platform` term exists so *"Cmd+key events (e.g. Cmd+`) are not consumed by the IME."*

**Interpretation:** the `!modifiers.platform` addition is the structural fix for the exact
regression (cmd-left / cmd-right) that got PR #27572 reverted — those are `platform`-modified
keys and are now excluded from IME-first routing. And `prefers_ime_for_printable_keys()` is a
genuine app-level lever over the arbitration that did not exist in 0.2.2. **So on main the app
DOES have a bounded override**, refuting the "app code cannot override" framing for current main
(it held for 0.2.2). What the app still cannot change on main: the *fixed* choice to route
`key_char.is_none()` non-printing keys (arrows/Enter/Esc) to the IME first when not composing —
but the app still gets the final `propagate` decision via `do_command_by_selector`'s re-dispatch
(`window.rs:2808-2821`), which is enough for terminal correctness.

---

## 2. Is the Enter-during-composition bug still live on main?

**Issue trail:**
- **zed#23003** (github.com/zed-industries/zed/issues/23003) — *terminal* (labels
  `area:integrations/terminal`, `area:internationalization`, `state:reproducible`). Chinese IME:
  pressing Enter to confirm a candidate produces a terminal newline instead of committing.
  Reported 2025-01-11. **Closed**, linking PR #27572.
- **PR #27572** (github.com/zed-industries/zed/pull/27572) — made Tab/Enter work during
  composition. **Merged 2025-03-28, reverted 2025-03-29 via PR #27719** because it "broke other
  bindings in the terminal like cmd-left and cmd-right" (ConradIrwin). So the *first* fix
  regressed keybindings — the same failure mode the audit flagged.

**Current-main state (from source):** the arbitration on main now excludes `platform`-modified
keys (`!modifiers.platform`, `window.rs:2157`) — i.e. cmd-left/right are no longer sent to the
IME — and added `accepts_text_input` + `prefers_ime_for_printable_keys` hooks. This is consistent
with a *later, more comprehensive* fix having landed after the #27572 revert. **Whether Zed's
terminal today commits-not-newlines on Enter is best settled empirically** — that is precisely
what `MANUAL-PRECHECK.md` item B does in the installed Zed 1.8.2.

**Does this block a from-scratch GPUI-native terminal?** No. Mechanism: while composing, Enter
goes to `inputContext handleEvent:` first (`window.rs:2166-2169`). The IME commits via
`insert_text` → our `replace_text_in_range`, then *may* also emit `doCommandBySelector(insertNewline:)`,
which gpui converts back into a re-dispatched Enter `KeyDown` to our callback
(`do_command_by_selector`, `:2808-2821`) with our callback deciding `propagate`. So our terminal
can set a `committed_this_key_cycle` flag in `replace_text_in_range` and, in the key-down callback,
**swallow an Enter that arrives immediately after a same-cycle commit** (send nothing to the pty),
otherwise send `\r`. This is app-level and needs no gpui change. (See `IMPLEMENTATION-PLAN.md` §Enter.)

---

## 3. Candidate-window anchoring (zed#46055)

**Issue** (github.com/zed-industries/zed/issues/46055): with Chinese IME in Zed's terminal, the
candidate window appears at the **screen's bottom-left** instead of at the cursor — specifically
when running `claude` and `droid`; it works correctly with `codex` and a plain shell prompt, and
works in VS Code and Terminal.app. Multi-monitor: shows on the built-in display even when Zed is
on an external monitor. Closed **not-planned**, P3.

**Terminal-side or platform-side? Terminal-side.** The anchor is entirely `bounds_for_range`:
`first_rect_for_character_range` (main `window.rs:2688-2716`; 0.2.2 `:2193-2218`) calls
`input_handler.bounds_for_range(range)` and, on `None`, returns
`NSRect(NSPoint(0,0), NSSize(0,0))` (`:2699-2700` on main; `:2204-2205` on 0.2.2). In screen
coords that `(0,0)` origin lands at the **bottom-left** — an exact match for the reported symptom.
The `claude`/`droid`-only trigger fits: those are full-screen alt-buffer TUIs that manage their
own input box and park/hide the terminal's hardware cursor, so Zed's terminal InputHandler returns
`None` (or a stale/off-grid rect) for the marked range while they run, whereas `codex`/shell keep
the real grid cursor in the input line. **Fix (our terminal): never return `None` from
`bounds_for_range` while composing — anchor to the current grid cursor cell (row,col → pixel
rect), matching Terminal.app.** No gpui change. The multi-monitor "wrong display" facet also
falls out once a correct non-zero cursor-cell rect is returned (gpui adds `frame.origin` of the
window's screen at `:2704-2707`).

---

## 4. Reference encoders (Apache/MIT — safe to template) + the GPL boundary

**What an InputHandler must NOT do:** encode keys. Committed IME text is written to the pty as
raw UTF-8 bytes. The kitty/legacy *key* encoder is a separate work package (audit G7, ~800-950
LOC) that lives in the key-down callback, not in `InputHandler`. The reference encoders below are
for that key path; the InputHandler only bridges composition → the same pty byte stream.

- **alacritty frontend `alacritty/src/input/keyboard.rs`** — Apache-2.0. The canonical ~800-line
  kitty CSI-u encoder (`SequenceBuilder` / `try_from_textual`, named-key handling, progressive
  flags). **NOT on this disk** — only `alacritty_terminal` (the *core* crate, no key encoder) is
  present at `/Users/nick/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/alacritty_terminal-0.26.0/`.
  Source: `github.com/alacritty/alacritty` → `alacritty/src/input/keyboard.rs` (frontend crate,
  introduced by PR #7125 "Implement kitty's keyboard protocol").
- **termwiz** — MIT. `KeyboardEncoding::Kitty`; the InputParser parses kitty events; the encode
  side is `encode_kitty_input()` in `wezterm-gui/src/termwindow/keyevent.rs`; `apc::KittyImage` is
  in `termwiz/src/escape/apc.rs`. Source: `github.com/wezterm/wezterm` (`termwiz` crate).
- **Xuanwo/gpui-ghostty** — GPUI+terminal integration reference (per audit G1/report §13); use for
  architecture, verify license before templating.

**GPL boundary (audit finding, handoff line 56):** Zed's `terminal` and `terminal_view` crates
are **GPL-3.0-or-later**; Nice is MIT. **Never read or template that source into an implementing
agent.** Architecture-level understanding via issues/PRs/this doc is fine. Everything templated
must come from Apache-2.0 (alacritty) / MIT (termwiz) / permissively-licensed sources.

### What the InputHandler must do to bridge NSTextInputClient → pty byte stream

- **CJK compose/commit** (Pinyin/Kana/Hangul): keystrokes during composition arrive as
  `setMarkedText:` → `replace_and_mark_text_in_range` (update underlined preedit, no pty write);
  selecting a candidate + confirm arrives as `insertText:` → `replace_text_in_range` (write the
  committed UTF-8 to the pty, clear preedit).
- **Dead keys (option+e, then e → é):** the OS sends `setMarkedText("´")` then `insertText("é")`.
  Works *iff* the terminal is NOT treating Option as Meta for that key. Option-as-Meta (ESC prefix)
  vs dead-key composition is a genuine conflict and must be config-driven (macOS "Use Option as
  Meta key"). This is the sharpest real-world edge (see plan §DeadKeys).
- **Press-and-hold accent popover (hold e → è é ê ë):** governed by `apple_press_and_hold_enabled`.
  Terminals conventionally return **`false`** (like iTerm2) so a held key becomes key-repeat to the
  pty rather than an accent popover. Return `false` unless a live test argues otherwise.
- **Dictation & Character Viewer:** ride the same `insertText:`/`setMarkedText:` plumbing; if
  `replace_text_in_range`/`replace_and_mark_text_in_range` are correct they work for free. Zed
  shipped this broken for ~11 months (zed#30446) — include them in the test matrix.

---

## 5. InputHandler sizing

Trait methods to implement (11; * = has a usable default we should still override for a terminal):

1. `marked_text_range` — trivial: `Some(0..preedit.utf16_len)` iff composing.
2. `selected_text_range` — caret within preedit; `Some(collapsed)` (never `None`).
3. `text_for_range` — substring of preedit buffer (UTF-16 ↔ UTF-8 conversion).
4. `replace_and_mark_text_in_range` — set preedit + request repaint (underlined inline preedit).
5. `replace_text_in_range` — **commit**: write UTF-8 to pty, clear preedit, set `committed_this_cycle`.
6. `unmark_text` — clear preedit.
7. `bounds_for_range` — grid-cursor cell → element-local `Bounds<Pixels>`; never `None` while composing.
8. `character_index_for_point` — map point→preedit index (can be minimal).
9. `apple_press_and_hold_enabled`* → `false` (key-repeat, terminal convention).
10. `accepts_text_input`* → `true`.
11. `prefers_ime_for_printable_keys`* → `false` (matches Zed's terminal; raw keys reach the pty).

Plus outside the trait:
- preedit buffer + inline underline rendering over the grid at the cursor: ~100-150 LOC;
- commit-swallow-newline flag in the key-down callback: ~30 LOC;
- `window.handle_input(ElementInputHandler)` wiring during paint + focus handle: ~30 LOC.

**Total ~400-500 LOC** first correct pass. Hard cases (ranked): (1) Enter-commit-not-newline flag
dance; (2) `bounds_for_range` correctness for full-screen TUIs / hidden cursor (#46055);
(3) inline preedit render over a grid that has no text document; (4) UTF-16↔UTF-8 offset
bookkeeping; (5) Option-as-Meta vs dead-key arbitration; (6) dictation/press-and-hold parity.

**If a gpui patch turns out necessary** (contingency only): file `crates/gpui_macos/src/window.rs`,
fn `handle_key_event`, the routing predicate at ~line 2152-2158 — a ~10-30 line change to which
keys are offered to the IME first. Baseline plan assumes this is NOT needed on main.

---

## Citations (load-bearing)

- gpui 0.2.2 `InputHandler` trait — `.../gpui-0.2.2/src/platform.rs:995-1085`; mac arbitration
  `handle_key_event` — `.../gpui-0.2.2/src/platform/mac/window.rs:1660-1775` (predicate
  `:1709-1713`); `first_rect_for_character_range` None→(0,0) — `:2193-2218` (`:2204-2205`).
- zed main `InputHandler` (moved + grew) — `crates/gpui_platform/src/platform.rs:1369-1476`
  (`prefers_ime_for_printable_keys` `:1473`, docstring `:1470-1472`; `accepts_text_input` `:1461`).
- zed main arbitration — `crates/gpui_macos/src/window.rs:2081-2211` (`is_ime_printable_key`
  `:2137-2150`; `!modifiers.platform` `:2157`); `first_rect` `:2688-2716`; `do_command_by_selector`
  `:2808-2825`.
- zed#23003 (terminal Enter-during-composition, closed); PR #27572 (merged 2025-03-28, reverted
  2025-03-29 via #27719, broke cmd-left/right); zed#46055 (candidate window bottom-left under
  claude/droid, terminal-side, closed not-planned).
- Encoders: alacritty `alacritty/src/input/keyboard.rs` (Apache-2.0, upstream only); termwiz
  `KeyboardEncoding::Kitty` + `termwiz/src/escape/apc.rs` (MIT). GPL boundary: Zed
  `terminal`/`terminal_view` = GPL-3.0-or-later, reference-only.
