# IME live spike — runbook (§13 spike 2, live half)

The from-scratch GPUI-native terminal `InputHandler` from `IMPLEMENTATION-PLAN.md`,
built and wired to a minimal live terminal-ish view. Build + headless verification are
DONE (below); the composition checklist needs a display and a human at the keyboard.

**Code:**
- `spikes/phase0-poc/aa-gamma/gpui-term-main/src/bin/ime-spike/main.rs` — the live spike
  bin: platform `InputHandler` impl, key-down policy, preedit overlay, alacritty
  local-echo grid, HUD.
- `spikes/phase0-poc/aa-gamma/gpui-term-main/src/bin/ime-spike/ime_state.rs` — the pure
  marked-text state machine (no gpui types) + 16 unit tests.

Builds against the SAME pinned checkout as the AA/gamma matrix bin:
`spikes/phase0-poc/aa-gamma/zed-main-patched/` (zed main
`10b07951838e422722e34641f4a9c0bfec9037ff` + bg-luminance patch, path deps).
**No zed-side changes were needed** — the gpui patch remains contingency-only.

---

## Run it (main session / Nick only — opens a real window)

```sh
cd spikes/phase0-poc/aa-gamma/gpui-term-main
NICE_IME_SPIKE_RUN=1 cargo run --bin ime-spike            # Option-as-Meta OFF (default; dead keys compose)
NICE_IME_SPIKE_RUN=1 cargo run --bin ime-spike -- --option-as-meta   # ⌥key → ESC key (dead keys bypassed)
```

Without `NICE_IME_SPIKE_RUN=1` the binary prints instructions and exits — a stray
`cargo run` can never open a window. `cmd-q` quits.

**What you see:** an alacritty-backed grid (no shell, no pty — committed bytes are
local-echoed straight into the vte parser), a block cursor, and below it a HUD that
live-logs every NSTextInputClient call, every pty write (with hex), the preedit +
selection state, the commit-swallow flag, and the last `bounds_for_range` anchor rect.
While composing, the preedit is drawn at the cursor as cyan underlined text on a
translucent strip (thick underline = the IME's selected sub-range, yellow caret at the
composition caret).

Setup for the checklist: add Pinyin (and/or Japanese Romaji) under System Settings →
Keyboard → Input Sources; switch with ⌃Space/⌘Space. Keep the "ABC" source for the
dead-key item.

---

## Verified WITHOUT a display (done, 2026-07-01)

- [x] `cargo build --bin ime-spike` succeeds against zed-main-patched (0 warnings from
      spike code).
- [x] `cargo build --bin gpui-term-main` (the existing AA/gamma matrix bin) still
      succeeds — the addition is purely additive (new `[[bin]]` + new files).
- [x] `cargo test --bin ime-spike`: **16/16 pass**, headless (the state machine is
      gpui-free). Covers: compose-without-commit, commit-clears-and-arms-swallow,
      plain-ASCII commit does NOT arm, bare-Enter-after-disarm sends CR, the Japanese
      Enter-commit same-cycle swallow sequence, partial (clause) commit splicing,
      dead-key ´→é, replacement-range clamping/rebasing, the UNHONORABLE replacement
      range (target already at the pty) flagged, unmark-commits-pending,
      UTF-16↔UTF-8 offset math incl. surrogate pairs and mid-surrogate snapping.
- [x] `bounds_for_range` **cannot return None** (zed#46055 fix): the method
      unconditionally wraps a total function (`ime_anchor_bounds`) in `Some`; the
      anchor is the grid-cursor cell, which exists even when a TUI parks/hides the
      hardware cursor. Code-inspectable — there is no `None` code path.
- [x] `prefers_ime_for_printable_keys` → `false` while `accepts_text_input` → `true`:
      the handler implements the **platform** `InputHandler` trait directly because
      gpui's `ElementInputHandler` adapter forwards `prefers_ime_for_printable_keys`
      to `accepts_text_input` (crates/gpui/src/input.rs:191-194 in the pinned rev),
      which would wrongly send printable keys IME-first for a terminal.
- [x] `apple_press_and_hold_enabled` → `false` (terminal/iTerm2 convention).
- [x] Committed IME text bypasses key encoding: `replace_text_in_range` writes raw
      UTF-8 to the parser. No kitty encoder anywhere in the bin (audit G7 stays a
      separate package).
- [x] Gate refusal: running without `NICE_IME_SPIKE_RUN=1` prints instructions,
      exit 0, no window.

## Left for the human (MANUAL-PRECHECK items, now against OUR handler)

Record PASS/FAIL + macOS version + input sources used. The HUD shows the ground truth
for every item (pty writes in hex, swallow flag, anchor rect).

1. **CJK compose + commit** (matrix #1): Pinyin, type `nihao`. Expect underlined cyan
   preedit at the cursor, candidate window anchored at the cursor cell, **no pty write**
   until commit; commit shows `pty ← "你好" [e4 bd a0 e5 a5 bd]` and 你好 appears in
   the grid.
2. **Enter mid-composition** (matrix #2, zed#23003): confirm a candidate with Enter.
   Expect the HUD `insertText(...) → COMMIT; swallow armed` and either NO enter key
   event or `key enter SWALLOWED`; **no `0d 0a` pty write**. Then a bare Enter → new
   prompt line (`pty ← "\r\n"`). Tab-commit variant: same, swallow message for `tab`.
3. **Preedit editing** (matrix #3): while composing press ←/→/Backspace. Expect
   `setMarkedText` updates only (preedit/sel change), no pty writes, grid cursor
   does not move.
4. **Dead keys** (matrix #4): ABC source, ⌥e then e → `pty ← "é"`, with the `´`
   stage visible as a preedit. Re-run with `--option-as-meta`: ⌥e →
   `pty ← "\x1be"` (ESC e), no composition.
5. **Press-and-hold** (matrix #5): hold `e`. Expect key-repeat (`pty ← "e"` many
   times), no accent popover, nothing stuck.
6. **Candidate anchor** (matrix #6, zed#46055): move the cursor with arrows and
   press **F2** (parks the cursor mid-grid, TUI-style), then compose. The candidate
   window must appear at the cursor cell — never the screen's bottom-left. On a
   second display, drag the window there and repeat (anchor must follow the window).
7. **Keybinding non-regression** (matrix #7): with Pinyin active but NOT composing,
   press **⌘← / ⌘→ / ⌘K**. Expect `KEYBINDING cmd-… fired` in the HUD (these are the
   PR #27572-revert casualties). Type `ls` — ASCII passes straight through to the grid.
8. **Dictation + Character Viewer** (matrix #8): insert an emoji via ⌃⌘Space and a
   phrase via dictation. Both must arrive as `insertText` commits.
9. Emoji/surrogates live (matrix #9): compose/commit an emoji-producing sequence;
   offsets already unit-tested, this is the visual confirmation.

**Gate (plan):** 1-3, 6, 7 must PASS to close audit G1 with "no gpui fork". If a
routing case fails app-side, the contingency is the ~10-30 LOC `gpui_macos`
`handle_key_event` predicate patch (SCOPE §5) — record it as a maintained-fork cost.

---

## Design notes / deviations from the plan

- **Direct platform-trait impl instead of `EntityInputHandler`.** The plan suggested
  the `ElementInputHandler` path; at this rev that adapter ties
  `prefers_ime_for_printable_keys` to `accepts_text_input`, so the spike implements
  `gpui::InputHandler` directly on a small wrapper (`TermInputHandler`, same
  element-bounds capture as `ElementInputHandler::new`). The real terminal should do
  the same (or upstream a fix).
- **Commit-swallow disarm is a foreground task**, not "start of next cycle" only:
  `take_commit_swallow()` at the top of every key-down PLUS an async disarm scheduled
  by each composition commit. The re-dispatched Enter of the same native cycle is
  synchronous (`doCommandBySelector` → `keystroke_for_do_command` re-dispatch), so it
  runs before the task; a commit with no same-cycle re-dispatch (e.g. Pinyin
  Space-commit) is disarmed before the next keypress. This closes the
  "Space-commit then bare Enter must still send CR" hole the plan's flag alone has.
- **`unmarkText` commits the pending preedit** (Terminal.app behavior) instead of
  dropping it — dropping loses user text on focus-loss/input-source switch.
- **Partial commits** (`insertText` with a replacement range inside the marked text,
  Japanese clause 確定) splice the committed clause out and keep the rest marked.
- Local echo is a stand-in for a pty: Enter → CR LF, Backspace → BS SP BS, arrows →
  CSI cursor moves, F2 → CUP (cursor-park probe). These are NOT key encodings — the
  kitty/legacy encoder (audit G7, ~800-950 LOC) is intentionally absent.
- Wide-char cell math (你 = 2 columns) comes from alacritty's grid; the spacer cells
  are skipped at paint. Backspace over a wide char is knowingly naive (1 column).
- Font: Menlo 13px (system-present; CJK via CoreText fallback), cell probed at
  runtime from advance('W') with a half-point snap; cell height fixed 16pt.
