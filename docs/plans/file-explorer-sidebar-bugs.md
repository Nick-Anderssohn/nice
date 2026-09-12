# File explorer sidebar: font size + post-drag hover flicker

Status: IMPLEMENTED 2026-09-12 — Fable-reviewed plan and code; new gpui tests red before and green after; `file_browser::view` tests + workspace build pass. Awaiting Nick's hands-on check.

Two bugs in the sidebar's file explorer (Files) mode.

1. Changing the sidebar font size does nothing in Files mode.
2. After dragging a file from the file explorer into a Claude pane, hover highlighting
   in the sidebar flickers, and the dragged row keeps a hover highlight. It clears only
   after the next click in the window.

## Decisions (Nick, 2026-09-12)

- After a drop, the dragged row **stays selected**. This matches Finder. The fix for
  bug 2 is only the stuck hover and the flicker.

## Bug 1: file explorer ignores the sidebar font size

### Cause

`crates/nice/src/file_browser/view.rs` draws with fixed sizes and never reads the
sidebar font setting.

- Constants at `view.rs:83-93`: `ROW_HEIGHT` 22, `INDENT_PER_LEVEL` 16,
  `DISCLOSURE_SLOT` 12, `ICON_FRAME` 16, `NAME_SIZE` 13, `ICON_SIZE` 13.
- More fixed sizes: the header text (`NAME_SIZE`, `view.rs:1558`), the row chevron
  (10pt, `view.rs:2355`), and the empty/missing-folder text (22 / 12 / 10 / 12,
  `view.rs:1784-1812`).
- The rename field gets `NAME_SIZE` (`view.rs:2545`).

Sessions mode scales through `SidebarShellView::sidebar_pt(base)`
(`sidebar_shell.rs:743`), which calls
`settings::sidebar_font::sidebar_size(sidebar_px, base)`. The shell re-reads the size
every render (`sidebar_shell.rs:2481`).

No subscription is needed. `font_pane::apply_sidebar_px` already calls
`refresh_windows()`, and gpui re-renders the whole window tree, `FileBrowserView`
included. Reading the size in `render` is enough.

### Change

In `FileBrowserView`:

1. Add a field `sidebar_px: f32`, initialised to `DEFAULT_SIDEBAR_FONT_PX`.
   At the top of `render`, next to `self.window_scale = …`, set
   `self.sidebar_px = crate::settings::sidebar_font::current_sidebar_px(cx)`.
2. Add a helper `fn pt(&self, base: f32) -> f32 { sidebar_size(self.sidebar_px, base) }`,
   the same shape as the shell's `sidebar_pt`.
3. `render_row` (`view.rs:2299`) and `render_rename_field` (`view.rs:2524`) are free
   functions and can't borrow the view. Give each one extra `sidebar_px: f32`
   parameter. Inside, a local `let pt = |b: f32| sidebar_size(sidebar_px, b);` keeps
   the use sites short. No new struct. At the 12pt default every scaled value equals
   today's constant.
4. Use the scaled values:
   - rows: `pt(ROW_HEIGHT)`, `pt(INDENT_PER_LEVEL)`, `pt(DISCLOSURE_SLOT)`,
     `pt(ICON_FRAME)`, `pt(ICON_SIZE)`, chevron `pt(10.0)`, name text `pt(NAME_SIZE)`;
   - rename field: `pt(NAME_SIZE)`;
   - header: text size `pt(NAME_SIZE)`;
   - empty / missing-folder states: `pt(22.0)`, `pt(12.0)`, `pt(10.0)`, `pt(12.0)`.
5. `scenario_row_center` (`view.rs:2114`) has no `cx`. Have it use
   `sidebar_size(self.sidebar_px, ROW_HEIGHT)` (the last-rendered size) in place of
   `ROW_HEIGHT`, and `sidebar_size(self.sidebar_px, ROW_PRESS_INSET)` in place of
   `ROW_PRESS_INSET`, so the aim point stays on the name run as indent and icons grow.
   Update its doc comment.
6. Update the "size stays `NAME_SIZE`" comments at `view.rs:1548-1550` and `:1737-1739`.

**Not scaled (deliberate):** paddings and gaps, and the control strip's 20px buttons and
11pt icons. Sessions mode keeps paddings and toolbar-style buttons fixed too. The
drift banner is drawn by the app shell, not the sidebar, so it is out of scope.

### Test

One `#[gpui::test]` in `view.rs`'s `mod tests`:
`rows_are_laid_out_at_the_sidebar_font_size`. It proves the scaled row height reaches
layout.

- Install `SharedSidebarFont` at 18px with `cx.set_global` (the idiom from
  `keymap.rs` tests, `keymap.rs:2306-2313`). Do this **before** `mount`: test windows
  draw inside `add_window` (`vendor/zed/crates/gpui/src/app.rs:1169-1174`).
- Mount with the existing `mount` helper. No extra notify or `run_until_parked` is
  needed.
- Take `scenario_row_center` for two adjacent rendered rows (e.g. `A.txt` and
  `B.txt`). Assert their y values differ by `sidebar_size(18.0, ROW_HEIGHT)` = 33.
  Today's code gives 22, so the test is red before the fix.
- Secondary check: `view.sidebar_px == 18.0`.
- If the list's tracked scroll bounds are zero in the test window (so
  `scenario_row_center` returns `None`), fall back to the field assertion only and
  note that in the implementation notes.

No pure unit test for the scaling. `sidebar_size` is already covered by
`sidebar_size_scales_against_the_12pt_anchor` (`settings/sidebar_font.rs:101-110`).

## Bug 2: hover flicker after dragging a file out

### Cause

The stock gpui macOS `synthetic_drag` loop keeps replaying a stale mouse event.

1. Every mouse-move with a button held (`handle_view_event`,
   `vendor/zed/crates/gpui_macos/src/window.rs:2415-2436`) bumps
   `synthetic_drag_counter` and spawns `synthetic_drag` (`window.rs:3165`). That task
   re-sends the same `MouseMove { pressed_button: Left, position }` every 16ms while
   the counter is unchanged. Its purpose is scroll-while-selecting.
2. The counter changes only on a newer held-button move or a `MouseUp` delivered to
   the view (`window.rs:2425`, `:2439`).
3. Arming a row drag calls `Window::begin_external_paths_drag`
   (`file_browser/view.rs:2439`). That starts an `NSDraggingSession`
   (`zed-external-drag-out` patch). From then on AppKit tracks the mouse itself. The
   view gets no more dragged events and never gets the mouse-up.
   `external_files_dragged` only prevents *new* loops; it doesn't stop the running one.
4. The loop from the arming move keeps running after the drop. Every 16ms it sets
   gpui's mouse position back to the arm point, which is over the dragged row.

Result: the dragged row reads as hovered. Real mouse moves light the row under the
cursor, then the replay moves the position back, so hover flips between the two.
The next click's mouse-up bumps the counter and ends the loop.

### Change

Stop the loop when the OS drag session actually starts. In the
`zed-external-drag-out` patch, `MacWindow::begin_external_paths_drag`
(`gpui_macos/src/window.rs`) becomes:

```rust
fn begin_external_paths_drag(&self, paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let native_view = self.0.as_ref().lock().native_view.as_ptr();
    let began = unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let began = begin_external_paths_drag_on_view(native_view, paths);
        let _: () = msg_send![pool, release];
        began
    };
    if began {
        // AppKit now owns mouse tracking until the session ends, so the view
        // never receives this gesture's mouse-up. Without this bump the
        // synthetic_drag loop spawned by the arming mouse-move replays that
        // stale held-button move every 16ms after the drop, fighting real
        // hover until the next click.
        self.0.as_ref().lock().synthetic_drag_counter += 1;
    }
    began
}
```

- Bump only when a session began. If it didn't, gpui's in-app drag is the only
  mechanism and the normal mouse-up still arrives.
- The window-state lock is not held across the AppKit call (it is taken in its own
  statement before and after).
- No new loop can start during the session. Drag-destination callbacks go through
  `send_file_drop_event`, not `handle_view_event`.

### Applying the patch change

`vendor/zed` is generated from `patches/*.patch` by `scripts/vendor-zed.sh`, which
tracks applied patches by marker files and will not re-apply an edited patch.
**`vendor/` in a worktree is a symlink to the main checkout's `vendor/`**, so editing
it affects every worktree's build. The change is small and harmless on its own, but
announce it.

Regenerate the patch with `git diff` rather than hand-editing hunk counts. Do not
delete the marker and rerun `scripts/vendor-zed.sh`: that does `reset --hard` on the
pin and rewrites every patched file (`scripts/vendor-zed.sh:161-166`), forcing a full
gpui rebuild in the shared target dir.

1. In the scratchpad:
   `git clone --local --no-checkout ~/.cache/nice/zed-mirror.git <scratch>/zed`,
   then check out `ZED_PIN` (from `scripts/vendor-zed.sh`).
2. `git apply` patches 1–5 in the `PATCHES` order, then `git add -A && git commit`.
3. `git apply patches/zed-external-drag-out.patch`, make the code edit above, then
   `git diff > patches/zed-external-drag-out.patch`. Keep the patch's existing header
   comment if it has one (check the first lines before overwriting).
4. `git apply` `zed-1x-crisp-text` on top to confirm the full set still applies. (It
   touches `gpui/src/scene.rs`, `gpui/src/window.rs` and `gpui_macos/src/shaders.metal`,
   not `gpui_macos/src/window.rs`, so no conflict is expected.)
5. Make the identical edit by hand in `vendor/zed/crates/gpui_macos/src/window.rs`.
   Confirm with `diff` that the scratch file and the vendor file match.
6. Update the patch's description in `scripts/vendor-zed.sh` (lines ~50-62) with one
   sentence about ending the synthetic drag loop.

### Test

No automated test. `gpui_macos` has no test harness, and the bug needs a real
`NSDraggingSession`, which synthetic in-process events can't create. Validate
black-box (below).

## Bug 3 (found in feel-check): dragging a selected file entered rename

Not in the original plan. Pressing a file that was already the sole selection and
dragging it entered inline rename.

### Cause

A plain press on the sole selection routes at mouse-down (`press_disposition`), so
`on_row_click` armed the 280ms slow-second-click rename timer (`arm_slow_rename`) at
mouse-down. Arming a drag never cancelled it.

### Change

- `on_row_click` records the candidate in `pending_slow_rename` instead of arming.
- `on_row_release` (gpui `on_click`, which never fires once a drag arms) arms it.
- `clear_pending_press` (drag arm, right press) and each new press clear it.
- `drive_single_click` / `drive_double_click` also call `on_row_release`, like a
  real click.

Chosen over deferring the press: that would move select and folder expand/collapse
to mouse-up. Arming on release also covers a press held past 280ms before the drag,
which cancelling the timer at drag arm would miss. Matches Finder.

### Tests

- `a_press_that_becomes_a_drag_never_enters_rename`: red before, green after.
- `a_slow_second_click_release_enters_rename`: regression guard.

## Validation

- `cargo build --workspace`.
- Targeted tests: `cargo test -p nice file_browser::view`.
- Black-box in `Nice Dev` (scratch env per CLAUDE.md, under `scripts/worktree-lock.sh`,
  `caffeinate -d`):
  1. **Font.** Settings → Font → sidebar "+" ×4. In Files mode, row text, icons,
     chevrons, row height and indent grow together. The header grows too. Nested
     children stay aligned under their parent's name. "−" back to 12 matches the
     current look. Then ⌘= and ⌘− once each: the file explorer follows.
  2. **Drag.** Drag an image file from Files mode into a terminal/Claude pane. After
     the drop, move the mouse over other sidebar rows without clicking. Hover follows
     the cursor with no flicker. The dragged row stays selected (steady fill). A
     screen recording or two screenshots a second apart confirm it.
  3. **Drag, other paths.**
     - Drag a file to Finder and drop. Move back over the sidebar without clicking:
       hover follows, no stuck row. Finder still gets a copy.
     - Drag a file onto a folder row in the tree. The accent drop highlight is
       steady during the drag (today the replay loop fights it too), and the move
       still works.
     - Click-drag to select text in a terminal. Scroll-while-selecting still works,
       since only the drag-out path changes.

The `file-browser` live scenario always runs at sidebar 12 (`app.rs:4455-4466` seeds
defaults), so it can't check scaled geometry. The gpui test above covers that.
- Nick's feel-check before landing on main.

## Out of scope

- Selection after a drop (kept, per decision).
- Scaling the control strip, paddings, or the drift banner.
