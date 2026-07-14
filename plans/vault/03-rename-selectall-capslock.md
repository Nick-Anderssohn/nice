# Fix plan: inline-rename select-all-on-entry + caps-lock

## Bugs

- **BUG A — no preselection on tab / pane-pill rename.** Entering rename mode on a
  claude/session tab (sidebar) or a pane pill (title bar) drops a *collapsed caret at
  the end* of the title instead of selecting the whole text. The user must triple-click
  to replace the whole name. The file explorer already preselects correctly (base name,
  minus the `.ext`) and that file-explorer behavior is the desired reference — it must
  NOT change.

- **BUG B — rename fields ignore Caps Lock.** With Caps Lock ON, typing a letter into
  any inline rename (session tab, file-explorer entry, pane pill) inserts the lowercase
  letter. Applies to all three because they share one key→char path.

## Root causes

### BUG A — the two tab/pill call sites seed the editor with no selection

All three renames mount the same field ([`crates/nice/src/inline_rename.rs`]) backed by
the same pure model [`nice_model::file_browser::TextFieldEditor`]. The model has two
seed constructors:

- `TextFieldEditor::new(text)` — collapsed caret at end, no selection
  (`crates/nice-model/src/file_browser/text_field.rs:58-66`).
- `TextFieldEditor::with_selection(text, len)` — initial selection `[0, len)`, `len`
  clamped to text length (`text_field.rs:70-78`).

The **file explorer** seeds with a selection:
`TextFieldEditor::with_selection(&name, preselect_len(&name, is_dir))`
(`crates/nice/src/file_browser/view.rs:948`), where `preselect_len`
(`text_field.rs:272-279`) returns the base-name length for a file-with-extension, else
the whole length — this is the correct reference behavior.

The **sidebar tab** and **pane pill** seed with `new` — no selection:
- `crates/nice/src/sidebar_shell.rs:846` — `self.rename_editor = Some(TextFieldEditor::new(&title));`
  (comment at :845 "Cursor at the end (typing appends) — the prior char-append behaviour.")
- `crates/nice/src/toolbar.rs:647` — `self.rename_editor = Some(TextFieldEditor::new(&title));`
  (comment at :646 same).

So the tab/pill fields open with nothing selected → the first keystroke inserts instead
of replacing. That is BUG A. (A tab/pane title is not a filename, so the correct target
is *select the whole title*, not base-minus-extension.)

### BUG B — GPUI's macOS backend builds `key_char` without Caps Lock, and the field inserts `key_char` verbatim

The field inserts the character from `keystroke.key_char`. All three call sites pass
`ks.key_char.as_deref()` into the shared `dispatch_rename_key`, which inserts it when no
⌘/⌃ is held:
- `crates/nice/src/inline_rename.rs:98-106` (`dispatch_rename_key` — the `key_char` insert branch)
- call sites: `crates/nice/src/sidebar_shell.rs:916-923`,
  `crates/nice/src/toolbar.rs:728-735`, `crates/nice/src/file_browser/view.rs:1011-1018`.

`key_char` itself is wrong. GPUI's macOS backend computes it in
`vendor/zed/crates/gpui_macos/src/events.rs`, `parse_keystroke`:

```
// events.rs:458-468
if !control && !command && !function {
    let mut mods = NO_MOD;
    if shift  { mods |= SHIFT_MOD; }
    if alt    { mods |= OPTION_MOD; }
    key_char = Some(chars_for_modified_key(native_event.keyCode(), mods));
}
```

`chars_for_modified_key` (`events.rs:512+`) calls `UCKeyTranslate` with only the
shift/option bits — the **alphaLock (Caps Lock) bit is never included**, so for a letter
key it always returns the un-capslocked (lowercase, unless Shift) character. Caps-Lock
state IS read by the backend, but only for the separate `ModifiersChanged`/`Capslock`
event (`events.rs:120-131`), never folded into `key_char`. Hence `key_char` is lowercase
with Caps Lock on. That is BUG B.

Note this defect is shared: the terminal input path also takes its inserted text from
`key_char` (`crates/nice-term-view/src/input.rs:220` — `text = keystroke.key_char.clone()`),
so in principle the terminal has the same latent issue. The reported bug is the rename
fields; the fix below is scoped to them (see Risks for why we do not patch the vendored
backend).

GPUI already exposes the live Caps-Lock state to the app: `Window::capslock() -> Capslock { on }`
(`vendor/zed/crates/gpui/src/window.rs:2615-2617`), kept current from the platform
(`window.rs:1346`, `:1593`, `:4586`). The `Modifiers` struct has NO caps-lock field
(`vendor/zed/crates/gpui/src/platform/keystroke.rs:447-471`); Caps Lock lives only in the
separate `Capslock` struct (`keystroke.rs:663-669`). So the app must read it from
`window.capslock()`, not from `keystroke.modifiers`.

## Fix design

Both fixes are small and both land in shared code so all three renames are covered.

### BUG A — preselect the whole title on tab/pill rename entry

Change the two `new(&title)` seeds to select the entire title. `with_selection` clamps an
overlong `len`, so passing the char count selects all:

- `crates/nice/src/sidebar_shell.rs:846`:
  `TextFieldEditor::new(&title)` → `TextFieldEditor::with_selection(&title, title.chars().count())`
- `crates/nice/src/toolbar.rs:647`: same change.
- Update the now-stale "Cursor at the end (typing appends)" comment at each site
  (:845 / :646) to describe select-all-on-entry.

Do **not** touch the file-explorer seed (`view.rs:948`) — it must keep base-minus-`.ext`.

Optional clarity nicety (YAGNI-bounded, do only if it reads cleaner): add a
`TextFieldEditor::with_all_selected(text)` convenience on the model that forwards to
`with_selection(text, len)`, and call it from the two sites. Not required.

### BUG B — fold Caps Lock into the inserted char, in the shared dispatch

Thread the live Caps-Lock state into `dispatch_rename_key` and apply it to the char that
would be inserted. Caps Lock affects only alphabetic characters and toggles their case
*on top of* the Shift already reflected in `key_char`:

1. Add a `capslock: bool` parameter to
   `dispatch_rename_key` (`crates/nice/src/inline_rename.rs:60-110`). In the printable-insert
   branch (currently :100-104), before `editor.apply_key(TextFieldKey::Char(ch))`, apply:
   ```
   let ch = if capslock && ch.is_ascii_alphabetic() {
       // Caps Lock toggles letter case on top of Shift (which key_char already applied):
       // no-shift lowercase -> upper; shift upper -> lower.
       if ch.is_ascii_lowercase() { ch.to_ascii_uppercase() } else { ch.to_ascii_lowercase() }
   } else { ch };
   ```
   Scope to `is_ascii_alphabetic` — Caps Lock does not affect digits/punctuation, and the
   reported bug is ASCII letters. (Unicode letters are out of scope; see Risks.)

2. At each of the three call sites, read `window.capslock().on` and pass it as the new
   argument (the `window` is in scope in every `on_rename_key`):
   - `crates/nice/src/sidebar_shell.rs:916-923`
   - `crates/nice/src/toolbar.rs:728-735`
   - `crates/nice/src/file_browser/view.rs:1011-1018`

Because the case-flip lives in `dispatch_rename_key`, one edit fixes all three renames.

## Files touched

- `crates/nice/src/inline_rename.rs` — add `capslock: bool` param to `dispatch_rename_key`;
  apply the case flip in the insert branch; update the unit tests to pass the new arg.
- `crates/nice/src/sidebar_shell.rs` — BUG A seed change (:846) + comment; BUG B pass
  `window.capslock().on` (:916).
- `crates/nice/src/toolbar.rs` — BUG A seed change (:647) + comment; BUG B pass
  `window.capslock().on` (:728).
- `crates/nice/src/file_browser/view.rs` — BUG B pass `window.capslock().on` (:1011). No
  BUG A change here (file-explorer preselection is already correct).
- (Optional) `crates/nice-model/src/file_browser/text_field.rs` — only if adding the
  `with_all_selected` convenience.

No changes to `vendor/zed` and none to `crates/nice-term-input`.

## Tests

### Unit (pure, no GUI) — feasible and preferred

- **`inline_rename.rs` (`dispatch_rename_key`)** — extend the existing `#[cfg(test)] mod tests`:
  - Caps Lock ON + `key_char == "a"` (no shift) inserts `"A"`.
  - Caps Lock ON + Shift + `key_char == "A"` inserts `"a"` (shift+caps cancels for letters).
  - Caps Lock ON + a digit/punct `key_char` (e.g. `"7"`, `"-"`) inserts it unchanged.
  - Caps Lock OFF leaves every existing case identical (regression guard — update the
    existing calls that now take the extra `false`).
- **`text_field.rs`** — existing `with_selection_*` tests already cover select-all via
  `with_selection(text, len)`; add one asserting `with_selection(title, title.chars().count())`
  selects `(0, len)` for a tab-like title (and, if the convenience is added, a
  `with_all_selected` test).

### In-process scenario (self-test) — assert preselection on entry

The scenario harness already reads begin-state selection:
`scenario_tab_rename_selection` (`sidebar_shell.rs:2384`),
`scenario_rename_selection` (`toolbar.rs:1980`, `file_browser/view.rs:1716`). Add / extend
scenarios so that immediately after `drive_begin_rename`:
- sidebar tab rename selection == `(0, title_len)` (BUG A).
- pane-pill rename selection == `(0, title_len)` (BUG A).
- file-explorer rename selection stays base-minus-`.ext` (reference unchanged — guard).

Caps Lock is set by the real macOS backend, so an end-to-end caps-lock assertion isn't
practical in the pure scenario harness; the unit tests above are the coverage for BUG B.

### Manual validation (dev build, per CLAUDE.md — scratch env, never prod)

1. Rename a claude/session tab (sidebar): whole title highlighted on entry; first
   keystroke replaces it.
2. Rename a pane pill (title bar): same.
3. Rename a file-explorer entry: still selects base minus `.ext` (unchanged).
4. With Caps Lock ON, type letters in each of the three renames → uppercase; Shift+letter
   with Caps Lock ON → lowercase; digits/symbols unaffected.

## Risks & interactions

- **Shared rename code, not shared with the terminal fix.** Both fixes live in
  `crates/nice` (the field/model + the three view call sites). They do **not** touch
  `crates/nice-term-input`, so there is **no overlap with the in-flight option+arrow key
  fix** in that crate. The Caps-Lock case-flip is confined to `dispatch_rename_key`, which
  only the three renames call.
- **Why not fix the root defect in `gpui_macos::parse_keystroke`.** Adding the alphaLock
  bit to the `UCKeyTranslate` mask would fix `key_char` for the whole app (terminal
  included) in one place, but: (a) it edits vendored zed, which `scripts/vendor-zed.sh`
  regenerates — it would have to be carried as a `patches/` patch; (b) it broadens blast
  radius into terminal key encoding and the kitty shifted-key logic that compares
  `key_char` to the primary key (`crates/nice-term-view/src/input.rs:220-229`). For the
  reported (rename-only) bug the scoped app-layer fix is lower-risk and YAGNI-aligned. If
  a future caps-lock-in-terminal bug is filed, revisit the backend patch.
- **ASCII-only case flip.** macOS Caps Lock also uppercases non-ASCII letters; we scope
  to `is_ascii_alphabetic` to match the reported bug and avoid Unicode case-mapping
  subtleties (multi-char upper/lowercase). Note this as a deliberate bound.
- **Caps-Lock source.** Must read `window.capslock().on`, not `keystroke.modifiers` —
  `Modifiers` has no caps-lock field; caps lock is only in the separate `Capslock` struct.
- **BUG A target differs by call site by design.** Tabs/pills select the WHOLE title;
  the file explorer selects base-minus-extension. Keep them different — do not unify onto
  `preselect_len` (a tab title is not a filename and has no meaningful extension).
- **Signature churn.** Adding a param to `dispatch_rename_key` touches its unit tests and
  all three call sites; mechanical, but all four must be updated together or it won't
  compile.
