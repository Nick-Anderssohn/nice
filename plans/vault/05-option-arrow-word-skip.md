# 05 — Option+Left/Right should skip words, not print `D`/`C`

## Bug

Verbatim: "Hitting option+leftArrow prints a 'D' in the terminal and
option+rightArrow prints a 'C' in the terminal. That is not right. on mac,
option+leftOrRightArrow is used to skip over words - that behavior should apply
to our terminal too. Not sure why we are printing a char. Weird."

On macOS, Option+Left / Option+Right are the universal "jump backward/forward one
word" shortcut. In Nice's terminal they instead leak a bare `D` / `C` into the
shell line.

## Current behavior

Nice is in **legacy VT mode** at a normal shell prompt (default `KeyEncoder`,
`KittyFlags::empty()` — the kitty protocol is only enabled when the app requests
it via `CSI > flags u`). Option = the `alt` modifier
(`crates/nice-term-input/src/key.rs:63` maps Option→`alt`).

Trace for **Option+Left** (`alt` + `NamedKey::ArrowLeft`), legacy mode:

- `KeyEncoder::encode` routes a `Key::Named` to `encode_functional(.., disambiguate = false, ..)`
  — `crates/nice-term-input/src/keyboard.rs:190`.
- In `encode_functional`, `mod_bits = modifiers.kitty_bits()` = `2` (alt),
  so `wants_mod_field = true` — `keyboard.rs:205`, `keyboard.rs:216`.
- `functional_encoding(ArrowLeft)` = `FnEncoding::CsiLetter(b'D')` —
  `keyboard.rs:564`.
- Because `wants_mod_field` is true, the CsiLetter arm skips the plain
  `ESC [ D` / SS3 branch and calls
  `build_csi_with_modifier(1, mod_bits = 2, None, &[b'D'], true)` —
  `keyboard.rs:246`.
- `build_csi_with_modifier` (`keyboard.rs:339`) produces payload `"1"`, then
  appends `;` + `mod_value` where `mod_value = mod_bits + 1 = 3` → `"1;3"`, and
  the terminator `D`.

**Emitted bytes: `ESC [ 1 ; 3 D` = `\x1b[1;3D`.**
Option+Right is identical with terminator `C` → **`\x1b[1;3C`**.

These are the *technically-correct* xterm "alt-modified arrow" sequences — but
**default zsh/bash bind nothing to them.** When zle / GNU readline reads an
escape sequence that is neither a complete binding nor the prefix of one, it
discards the unmatched escape/CSI-introducer/parameter bytes and the trailing
final byte (`D` / `C`) falls through to self-insert. So the user sees a lone
`D` (Left) or `C` (Right) typed into the line. (Same family as the well-known
"Ctrl+Arrow inserts `;5A`" reports on shells without the binding — see Sources.)

Note the inconsistency the fix closes: Nice's legacy path **already treats
Option as Meta for printables** — `legacy_char_sequence` prefixes `ESC` when
`alt` is held (`keyboard.rs:395`), so **Option+b already emits `\x1b b` =
backward-word today.** Only the arrow keys, which take the functional path,
diverge to the unbound `CSI 1;3D` form.

## Target behavior

### Legacy mode (kitty protocol off — the default shell prompt)

- **Option+Left → `ESC b` (`\x1b b`, 0x1b 0x62)** = Meta-b = readline/zle
  `backward-word`.
- **Option+Right → `ESC f` (`\x1b f`, 0x1b 0x66)** = Meta-f = `forward-word`.

Rationale: `\eb` / `\ef` are bound to backward-word / forward-word out of the
box in **both** GNU readline (bash) and the zsh emacs keymap, so word-skip works
with no user `.zshrc`/`.inputrc` config. This is exactly what **Terminal.app**
sends by default and what **Ghostty** ships as its default macOS keybind
(`alt+left=esc:b`, `alt+right=esc:f`, chosen precisely "because Terminal.app does
it"). iTerm2 ships this under its "Natural Text Editing" preset. It also makes
Option+Left match the bytes Nice **already** sends for Option+b (Meta-b),
finishing the Option-as-Meta contract the legacy printable path started.

### Kitty mode (DISAMBIGUATE / REPORT_ALL_KEYS active — protocol-aware apps)

**No change** — keep emitting `CSI 1;3D` / `CSI 1;3C`. Kitty-keyboard-aware
programs decode `1;3` as the alt modifier on Left/Right and perform word motion
themselves; that is the protocol's contract and downgrading to `ESC b`/`ESC f`
would lose modifier fidelity. The bug only exists on the legacy path.

## Fix design

Add a narrow legacy-only special case in `encode_functional`
(`crates/nice-term-input/src/keyboard.rs`), gated on `!disambiguate` so it can
never affect kitty output. Only pure Option (alt with no shift/ctrl/super) on
Left/Right is redirected; everything else is untouched.

At the top of `encode_functional`, before the `match key`:

```rust
// macOS Option-as-Meta word motion. In legacy mode, Option+Left / Option+Right
// map to Meta-b / Meta-f (ESC b / ESC f) — the backward-word / forward-word
// bindings default zsh & bash honor — matching Terminal.app / Ghostty and Nice's
// own Option-as-Meta printable path (Option+b already emits ESC b). The kitty
// CSI 1;3D / 1;3C form is reserved for protocol-aware apps (disambiguate on).
if !disambiguate {
    if let Some(byte) = legacy_alt_word_motion(key, input.modifiers) {
        return Some(vec![cc::ESC, byte]);
    }
}
```

with a small free helper:

```rust
/// Option-only (no shift/ctrl/super) Left/Right → Meta-b / Meta-f byte, the
/// macOS word-skip motion for the legacy line editor. `None` otherwise.
fn legacy_alt_word_motion(key: NamedKey, mods: Modifiers) -> Option<u8> {
    if !(mods.alt && !mods.shift && !mods.ctrl && !mods.super_) {
        return None;
    }
    match key {
        NamedKey::ArrowLeft => Some(b'b'),
        NamedKey::ArrowRight => Some(b'f'),
        _ => None,
    }
}
```

Details / rationale:

- Emitting on press **and repeat** (no event-type gate) is correct — holding
  Option+Left should repeat the word jump. Releases are already dropped upstream
  in `encode` (`keyboard.rs:117`) when event reporting is off, i.e. always in
  legacy mode.
- `app_cursor` (DECCKM) is intentionally ignored: Meta-b/Meta-f are line-editor
  bindings, not cursor-mode sequences; Terminal.app/Ghostty send `\eb`/`\ef`
  regardless of application-cursor mode.
- Requiring alt to be the *only* modifier keeps Shift+Option (selection) and
  Ctrl+Option combos on their existing `CSI 1;<n>D` path — see Risks.

## Files touched

- `crates/nice-term-input/src/keyboard.rs` — add the `!disambiguate` guard block
  in `encode_functional` and the `legacy_alt_word_motion` helper; extend the
  `#[cfg(test)] mod tests`. No other file changes; `input.rs` already maps
  `"left"`/`"right"` → `ArrowLeft`/`ArrowRight` and Option→`alt` correctly, so
  the input edge needs nothing.

## Tests

All in `keyboard.rs`'s existing `mod tests`, alongside
`legacy_alt_a_is_esc_prefixed` and `legacy_arrow_up_plain_and_app_cursor`:

- `legacy_option_left_is_esc_b` — `KeyEncoder::default().encode(alt + ArrowLeft)`
  == `Some(b"\x1bb".to_vec())`.
- `legacy_option_right_is_esc_f` — alt + ArrowRight == `Some(b"\x1bf".to_vec())`.
- `legacy_option_left_repeats` — same input with `event = Repeat` still emits
  `\x1bb` (word-skip repeats while held).
- `legacy_option_arrow_ignores_app_cursor` — with `app_cursor: true`,
  alt + ArrowLeft is still `\x1bb` (not an SS3 `ESC O` form).
- `kitty_option_left_stays_csi_1_3d` — `disamb().encode(alt + ArrowLeft)` is
  unchanged at `Some(b"\x1b[1;3D".to_vec())`; alt + ArrowRight == `\x1b[1;3C`.
  (Guards the "legacy only" boundary.)
- `legacy_shift_option_left_stays_csi` — shift+alt + ArrowLeft in legacy stays
  `Some(b"\x1b[1;4D".to_vec())` (documents that only pure Option is redirected).
- `legacy_option_up_down_unchanged` — alt + ArrowUp stays `\x1b[1;3A`, alt +
  ArrowDown stays `\x1b[1;3B` (documents the deliberate vertical scope-out).

## Risks & interactions

- **Scope is deliberately narrow.** Only pure Option + Left/Right changes. This
  matches the exact user complaint and the Terminal.app / Ghostty defaults, and
  avoids inventing behavior for combos macOS has no word convention for.
- **Option+Up / Option+Down (`\x1b[1;3A` / `\x1b[1;3B`)**: left as-is. They will
  still leak `A`/`B` in a default shell, but there is **no** de-facto macOS
  "word-vertical" motion and neither Terminal.app nor Ghostty remaps them, so
  there is nothing correct to send in legacy mode — a Meta-`p`/`n`
  (history) mapping would be a surprising invention. Explicitly out of scope;
  the test `legacy_option_up_down_unchanged` pins this.
- **Shift+Option+Left/Right, Ctrl+Option+Left/Right**: left on the existing
  `CSI 1;4D` / `CSI 1;7D` legacy path (selection / extended combos with no
  default zsh/bash binding). Redirecting them to a Meta byte would drop the
  extra modifier and could collide with other bindings; no user request covers
  them. `legacy_shift_option_left_stays_csi` pins the boundary.
- **Kitty path untouched** — the `!disambiguate` guard means DISAMBIGUATE /
  REPORT_ALL_KEYS apps keep the `CSI 1;3D`/`1;3C` fidelity they expect; the
  `kitty_option_left_stays_csi_1_3d` test enforces it. No interaction with the
  ⌘-as-super `ESC[99;9u` contract (super is excluded by the alt-only guard).
- **In-flight branch on the same crate**: another branch is editing
  `crates/nice-term-input/src/paste.rs`. This fix touches only `keyboard.rs`
  (encoder + its `mod tests`) — a **different file in the same crate**. No
  overlapping lines; the only shared surface is `lib.rs` re-exports, which this
  change does not modify. Expect a clean merge; if `Cargo`/lib wiring conflicts
  arise they will be trivial.

## Sources

- Ghostty default `alt+left=esc:b` / `alt+right=esc:f` (matches Terminal.app):
  https://github.com/ghostty-org/ghostty/discussions/7740 ,
  https://ghostty.org/docs/config/keybind
- iTerm2 Natural Text Editing / Option-arrow word motion:
  https://brettterpstra.com/2011/08/12/option-arrow-navigation-in-iterm2/
- Unbound `CSI 1;<n>` arrow sequences self-insert their tail in shells without
  the binding: https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=536459
