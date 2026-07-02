# Spike 2 results — IME live typing checklist (gate G1), 2026-07-02

The live half of §13 spike 2: the from-scratch platform `InputHandler` in the
`ime-spike` bin (built 2026-07-01 against pinned zed main `10b0795` +
bg-luminance patch; 16/16 headless unit tests) driven by a human at the
keyboard, plus the by-hand Zed pre-check from `MANUAL-PRECHECK.md`.

## Verdict: FULL PASS — gate G1 CLOSED, no gpui fork

All five gating items (matrix 1, 2, 3, 6, 7) PASS live on the pinned zed main
rev. Non-gating items 4, 5, 9 PASS. Item 8 splits: the `insertText` commit
path PASSes; only the *summoning* of system text services fails in the bare
spike bin (⌃⌘Space palette, dictation engagement) — and both work in Zed's
terminal on the same framework, so the gap is app/bundle-side, an ordinary
Phase-1 work item, not a fork risk. The ~10–30 LOC `gpui_macos` contingency
patch (SCOPE §5) was **not needed — retired unused**.

## Environment

- macOS 26.5.1 (25F80), built-in display, single-display setup (both
  second-display variants skipped — none connected).
- Input sources: **Pinyin – Simplified + ABC**. Japanese Romaji NOT used → the
  Tab-commit variant of item 2 and the clause (partial-commit) flow were not
  exercised live; both are covered by the headless unit tests.
- Binary: `ime-spike` (debug) from `aa-gamma/gpui-term-main`, path-deps on
  `zed-main-patched` (`10b0795` + bg-luminance patch). One run in default
  mode, one with `--option-as-meta`.
- Zed pre-check ran against **Zed 1.9.0** — the app auto-updated since
  `MANUAL-PRECHECK.md` was written against 1.8.2 (2026-07-01). Fresher build,
  same conclusion; version read from the installed app's Info.plist at
  write-up time.
- Human at the keyboard: Nick. HUD (pty-write hex log + event log) = ground
  truth for every item.

## Part A — Zed pre-check (existence proof)

Per-item RESULT lines are recorded inline in `MANUAL-PRECHECK.md`. Summary:
**A–F all PASS** (E3 skipped — no second display). Notable observations:

- **B2:** with Pinyin, **Enter commits the raw preedit** (`nihao` as literal
  text); it does not select the candidate — Space/digit does. This matches
  system-wide Pinyin behavior in every app tested, so it is correct IME
  semantics — and critically **no newline/submission occurs**, so the
  zed#23003 hazard is absent in current Zed.
- **E2:** candidate window stays anchored at the TUI caret — zed#46055 does
  not reproduce on Zed 1.9.0 in this setup.
- **Bonus** (established while diagnosing item 8 below): **⌃⌘Space and native
  dictation both work inside Zed's terminal** — the existence proof covering
  the spike bin's only failing surface.

## Part B — `ime-spike` matrix

| # | Item | Result | Evidence (HUD) |
|---|---|---|---|
| 1 | CJK compose + commit | **PASS** | preedit at cursor, candidate window at cursor cell, no pty write until commit; `pty ← "你好" [e4 bd a0 e5 a5 bd]`; 你好 rendered |
| 2 | Enter mid-composition | **PASS** | Pinyin semantics — Enter commits the RAW preedit (note below); **no `0d 0a` while composing**; bare Enter after → `pty ← "\r\n"` |
| 3 | Preedit editing | **PASS** | ←/→/Backspace while composing → `setMarkedText` updates only, zero pty writes, grid cursor unmoved |
| 4 | Dead keys, both modes | **PASS** | default: `´` preedit → `pty ← "é"`; `--option-as-meta`: `key ⌥e → ESC e (option-as-meta ON; IME bypassed)` + `pty ← "\u{1b}e" [1b 65]` |
| 5 | Press-and-hold | **PASS** | key auto-repeat, no accent popover, nothing stuck |
| 6 | Candidate anchor, parked cursor | **PASS** | arrows + F2 (CUP 6;30) then compose → candidate window at the parked cursor cell, never bottom-left. Second-display variant skipped |
| 7 | Keybinding non-regression | **PASS** | `KEYBINDING cmd-… fired` for ⌘←/⌘→/⌘K with Pinyin active-idle; `ls` ASCII passthrough |
| 8 | Character Viewer + dictation | **PASS (commit path) / FAIL (summoning — bare bin only)** | diagnosis below |
| 9 | Emoji/surrogates live | **PASS** | emoji inserted via Character Viewer arrived as an `insertText` commit and rendered correctly |

**Gate (1, 2, 3, 6, 7): 5/5 PASS → G1 closes "no gpui fork".**

**Item 2 note (runbook expectation corrected):** RUN-IME.md's "confirm a
candidate with Enter" describes the Japanese Romaji flow. With Pinyin, Enter
commits the raw latin preedit and Space selects the candidate — verified to
match system-wide behavior in other apps. The assertion that actually gates
(no CR to the pty while composing = commit-swallow working; bare Enter
afterwards sends CR) held exactly.

**Item 4 note (runbook formatting corrected):** the expected write is logged
Rust-debug-style as `pty ← "\u{1b}e" [1b 65]` in the HUD's pty-write section
(`feed_pty`, main.rs:165) — NOT the runbook's `pty ← "\x1be"` — while the
event log shows `key ⌥e → ESC e (option-as-meta ON; IME bypassed)`
(main.rs:384). Both lines observed (screenshot-confirmed). RUN-IME.md updated.

### Item 8 diagnosis — summoning is app/bundle-side, not framework-side

- ⌃⌘Space never shows the emoji palette while the spike window is focused.
- Native dictation does not engage (mic key does nothing; works in other
  apps at the same moment).
- WisprFlow (third-party dictation; inserts via AX/synthetic paths) inserts
  nothing — expected for a bin with no ⌘V handler and a minimal AX tree, and
  NOT evidence about the NSTextInputClient path.
- **The commit path itself is proven:** opening the same Character Viewer via
  the input menu ("Show Emoji & Symbols") and double-clicking an emoji
  delivered a normal `insertText` commit, rendered in the grid (= item 9).
- **Existence proof:** both ⌃⌘Space and dictation work inside Zed's terminal
  (1.9.0) on the same gpui — nothing here needs a fork. The bare unbundled
  cargo bin simply doesn't summon system text services (hotkey never
  forwarded; no dictation-target presentation). Phase-1 work item for the
  real, bundled terminal app: forward the palette summon and present as a
  dictation target, then re-verify both in the bundled app.

## Interpretation for the report

- G1 (IME composition / candidate-window arbitration) — the §13 program's
  last gate with a live component — is **CLOSED fork-free** on the
  production-candidate stack. zed#23003-class and zed#46055-class behaviors
  verified correct in OUR handler, live.
- The `gpui_macos` `handle_key_event` contingency (SCOPE §5) is retired
  unused.
- Non-gating Phase-1 backlog: ⌃⌘Space summon + dictation-target presentation
  in the bundled app (Zed proves both possible on unforked gpui).

## Provenance

- Run 2026-07-02 afternoon, main session: window launched with
  `NICE_IME_SPIKE_RUN=1 cargo run --bin ime-spike` (then `-- --option-as-meta`)
  from `aa-gamma/gpui-term-main/`.
- Results called out live by Nick; the item-4 ESC-e pty line
  (`pty ← "\u{1b}e" [1b 65]`) confirmed by screenshot.
- Zed pre-check results recorded inline by Nick in `MANUAL-PRECHECK.md`
  (same day, actual Zed 1.9.0).
