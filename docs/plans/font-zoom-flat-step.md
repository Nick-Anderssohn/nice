# Font zoom: flat 1pt step, no sidebar ratio

Status: SHIPPED 2026-09-12 — Fable-reviewed (no blockers; findings folded in); targeted tests + `NICE_SELFTEST=settings-window` (with `--features selftest`) pass; Nick's manual feel-check passed.

## Problem

The Settings Font pane's terminal-size "+" / "−" also changes the sidebar size.

- The pane stepper calls `font_pane::apply_terminal_px` → `FontSettings::set_px`.
- `set_px` emits `FontZoom` on every size change (`crates/nice-term-view/src/font.rs:219`).
- `SharedSidebarFontSettings` subscribes to `FontZoom` and rescales:
  `round(sidebar × new / last_terminal_px)` (`crates/nice/src/settings/sidebar_font.rs:104`).
  At 13/12, one "+" gives `round(12 × 14/13) = 13`.
- "Reset to defaults" never updates `last_terminal_px`, because `reset_to_defaults`
  deliberately emits no `FontZoom`. So the first "+" after a reset behaves differently
  depending on the pre-reset terminal size: it grows, holds, or even shrinks the sidebar.

Separately, keyboard zoom (⌘= / ⌘− / ⌘0) persists nothing. `zoom_shared_font` and
`reset_shared_font` (`crates/nice/src/keymap.rs:1074-1086`) mutate the entity only.
The only persist calls are in `font_pane.rs`. A relaunch reverts keyboard zoom.

## Decisions (Nick, 2026-09-12)

1. Remove the ratio system entirely.
2. ⌘= adds 1pt to BOTH terminal and sidebar. ⌘− subtracts 1pt from both.
3. Each size clamps independently to 8–32. At a bound the gap between them may close
   (terminal 32 / sidebar 31 → ⌘= → 32 / 32). Accepted.
4. The pane's terminal stepper changes only the terminal. The sidebar stepper changes
   only the sidebar. (Already true for the sidebar stepper.)
5. Keyboard zoom persists both sizes.

⌘0 resets both sizes (terminal 13, sidebar 12) and persists them. It does NOT touch
family or line height. That stays the Font pane's "Reset to defaults" job. (Today ⌘0
resets the terminal, and the sidebar follows via the ratio. The new behavior is the
flat equivalent.)

## Changes

### 1. Delete `FontZoom` (`crates/nice-term-view`)

The sidebar is its only subscriber. Everything else referencing it is a test, comment, or doc.

- `src/font.rs`: delete the `FontZoom` struct and `impl EventEmitter<FontZoom>`. Remove the
  `cx.emit(FontZoom …)` in `set_px` and `set_family`. Drop the `EventEmitter` import if it
  becomes unused. Update doc comments that mention `FontZoom`:
  - module docs (~line 10, "Stage 2's proportional sidebar scale…")
  - `zoom_by`, `set_px`, `set_line_height`, `set_family`, `reset_to_defaults` docs.
- `src/lib.rs:107`: drop `FontZoom` from the `pub use`.
- `FontSettings::zoom_by` and `FontSettings::reset`: after change 3 nothing in the shipped
  path calls them. Delete them if the compiler confirms no other callers (the
  `settings-window` scenario's `zoom_by` call is rewritten in change 5).

### 2. Strip the ratio from `SharedSidebarFontSettings` (`crates/nice/src/settings/sidebar_font.rs`)

- Remove fields `last_terminal_px` and `_zoom_sub`, and method `on_terminal_zoom`.
- `new(px, terminal_px, font, cx)` → `new(px)`. It no longer needs a `Context` or the font
  entity. Callers become `cx.new(|_| SharedSidebarFontSettings::new(px))`.
- Keep `px`, `set_px` (clamped, notify-only-on-change), `reset`, `sidebar_size`,
  `clamp_sidebar_px`, the Global and its accessors.
- Rewrite the module docs: sidebar size is independent. Keyboard zoom steps it by 1pt
  alongside the terminal (see `keymap.rs`).
- Imports: drop `Subscription`, `FontSettings`, `FontZoom`, `Entity` if unused.
- Tests: delete `terminal_zoom_rescales_the_sidebar_proportionally`,
  `terminal_zoom_clamps_the_rescaled_sidebar`, and
  `zero_reference_guard_advances_without_rescaling`. Keep
  `set_px_clamps_no_ops_and_reset_restores_the_default`, with the constructor updated.
  It no longer needs the `FontSettings` entity (line ~321), so drop that. Also drop the
  `use nice_term_view::DEFAULT_TERMINAL_FONT_PX` import (line ~181) if unused.

`crates/nice/src/keymap.rs:227-241`: construct with `new(sidebar_px)`. Drop the
`terminal_px` read and the comment about the `FontZoom` coupling. Also drop the
`FontZoom` sentence in the seed comment at ~line 199.

### 3. Keyboard zoom steps both sizes and persists (`crates/nice/src/keymap.rs`)

Reuse the Font pane's apply helpers. They already clamp, persist, and `refresh_windows()`.
The refresh also fixes a small side issue: an open Settings window's `<n> pt` readouts now
update on a keyboard zoom.

```rust
fn zoom_shared_font(cx: &mut App, delta: i32) {
    let delta = delta as f32;
    if let Some(font) = try_shared_font_settings(cx) {
        let px = font.read(cx).px();
        crate::settings::font_pane::apply_terminal_px(cx, px + delta);
    }
    if let Some(sidebar) = crate::settings::sidebar_font::shared_sidebar_font(cx) {
        let px = sidebar.read(cx).px();
        crate::settings::font_pane::apply_sidebar_px(cx, px + delta);
    }
}

fn reset_shared_font(cx: &mut App) {
    crate::settings::font_pane::apply_terminal_px(cx, DEFAULT_TERMINAL_FONT_PX);
    crate::settings::font_pane::apply_sidebar_px(cx, sidebar_font::DEFAULT_SIDEBAR_FONT_PX);
}
```

Independent clamping falls out of the two setters, with no extra logic. Make both
functions `pub(crate)` so the tests and the scenario can call the shipped path.

Store writes are only-if-changed (`prefs_store.rs:159`, `:180`), so a zoom pinned at a
bound does not rewrite the file.

Known cost, accepted: each ⌘= / ⌘− does two read-merge-write passes on `ui_settings.json`
(one per helper), including under key autorepeat. The pane stepper does one. A combined
setter would save the second write but isn't worth the extra code now. The two
`refresh_windows()` calls are free: gpui only queues the effect, and applying it twice
is idempotent.

Reentrancy is fine. Action handlers run with `&mut App` and no entity borrow, and these
same helpers already run from a mouse-down handler.

### 4. Font pane (`crates/nice/src/settings/font_pane.rs`)

No logic change. Once `FontZoom` is gone, `apply_terminal_px` touches only the terminal.
Update the stale docs:
- `apply_terminal_px` (line 62-64): remove "The sidebar rescales proportionally via its own
  `FontZoom` subscription".
- `reset_fonts` (line 118-121): remove the explanation about `reset_to_defaults` not
  emitting `FontZoom`.

### 5. Scenario + itest + README

- `crates/nice/src/settings/scenario.rs` leg (c) (lines ~271-347, module doc lines 21-26):
  - Replace `font.update(… f.zoom_by(1, cx))` with `crate::keymap::zoom_shared_font(app, 1)`.
    The "continues from the slider value" assertion stays.
  - Add: the sidebar px is unchanged after `apply_terminal_px`. This is the regression the
    bug report describes, asserted on the shipped entities.
  - **Fix the on-disk assertion (lines ~316-333).** It reads `terminal_font_size` AFTER
    the zoom and expects `target`. That only passes today because `zoom_by` never saves.
    With the zoom now saving, the file holds `target + 1`. Assert `target + 1.0`, and add
    `sidebar_font_size == sidebar_before + 1`. That makes this leg the scenario-level
    proof that keyboard zoom saves both sizes.
  - Teardown restore: keep `reset_to_defaults` + sidebar `reset`.
- `crates/nice-itests/src/font_mutators.rs` + `crates/nice-itests/src/lib.rs:107-110`:
  remove the `FontZoom` subscription, the "emits a FontZoom" assertion, and the
  "reset_to_defaults emits NO FontZoom" assertion. Keep the clamp / notify / family /
  reset assertions. Update the module doc.
- `crates/README.md` ~line 485 (`sidebar_font` entry) and ~line 545 (`set_px` "emits
  `FontZoom`"): describe the new independent sizes + flat keyboard step. Also ~line 2316
  (the `settings-window` scenario table row mentions "⌘= (`zoom_by`)"): point it at
  `zoom_shared_font`.
- `docs/plans/restyle/02-sidebar-flatten.md:60` ("proportional zoom intact") is a
  historical plan. Leave it alone.

## Tests (new)

Put these as `#[gpui::test]`s in `keymap.rs`'s existing `mod tests`. That module already
uses `TestAppContext`. Each test installs a `SettingsPrefsStore::load(<temp path>)` global,
the `SharedFontSettings` global (`FontSettings::resolved_default`), and the
`SharedSidebarFont` global, set by hand with `cx.set_global`. Don't use
`install_shortcuts`; no existing keymap test calls it. For the temp file, use the
module's `unique_temp_ui_settings` helper (`keymap.rs:~1410`). For the reload-from-disk
check, follow the pattern in `advanced_pane.rs:296-326`.

**Flush rule (all tests).** Mutate in one `cx.update(...)`, call `cx.run_until_parked()`,
then assert in a separate `cx.update(...)`. Subscription events are delivered only when
the outermost update flushes. An assertion inside the same closure would pass even with
the old ratio coupling still in place.

**Prove test 1 catches the bug.** Write test 1 first and run it against the current code,
before removing `FontZoom`. It must fail: 13/12, then pane "+", gives sidebar 13. Record
that in the implementation notes. Then make the change and watch it pass.

1. **Pane terminal stepper leaves the sidebar alone.** Start at 13/12. Call
   `apply_terminal_px(14)`, then `apply_terminal_px(15)`. Sidebar stays 12. Then
   `reset_fonts` followed by `apply_terminal_px(14)`: sidebar still 12. The second half
   covers the stale-reference case.
2. **⌘= / ⌘− step both by 1 and persist.** From 13/12: `zoom_shared_font(+1)` gives 14/13.
   `zoom_shared_font(-1)` ×2 gives 12/11. After each step, the store's
   `terminal_font_px()` / `sidebar_font_px()` match. Reload the JSON from disk once to
   prove the write landed.
3. **Independent clamping.** Set terminal 32, sidebar 31. `zoom_shared_font(+1)` gives
   32/32. Mirror at the floor: terminal 9, sidebar 8, `zoom_shared_font(-1)` gives 8/8.
4. **⌘0 resets both sizes only.** Set terminal 20, sidebar 17, a family override, and a
   non-default line height. `reset_shared_font` gives 13/12, persisted. Family and line
   height are unchanged.

## Validation

- `cargo build --workspace`.
- Targeted tests: `cargo test -p nice keymap::tests`, `cargo test -p nice sidebar_font`,
  `cargo test -p nice-itests font_mutators`.
- Black-box in `Nice Dev` (scratch env per CLAUDE.md, under `scripts/worktree-lock.sh`):
  1. Settings → Font → Reset to defaults → terminal "+" ×3. Readouts go 14/15/16 terminal,
     sidebar stays 12.
  2. ⌘= ×2 → 18 / 14 (readouts update live while Settings is open). ⌘0 → 13 / 12.
  3. ⌘= ×2, quit, relaunch the same scratch env → sizes are 15 / 14.
- The `settings-window` and `niceties-zoom` self-test scenarios still pass. `niceties-zoom`
  only checks terminal px + metrics and needs no change.

## Out of scope

- No settings migration. Stored `terminal_font_size` / `sidebar_font_size` values are read
  unchanged; only the runtime coupling changes.
- The Swift-parity rationale in the old docs is dropped on purpose.
