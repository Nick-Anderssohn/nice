# Manual test walkthrough

Each phase builds on the last. Set up a junk test directory you don't mind churning, e.g. `~/Desktop/nice-rcm-test/`. Reset between phases as noted.

## Setup

1. `scripts/install.sh` (dev variant — installs `Nice Dev.app`).
2. Open `Nice Dev`, open a project rooted at the test dir, switch sidebar to file browser (⌘⇧B or the files icon).
3. Seed a few files and folders so you have something to right-click: `a.txt`, `b.txt`, a folder `subA/` with `c.txt` inside, an empty folder `subB/`.

## Phase 1 — Menu visibility

1. **Right-click `a.txt`**: menu shows Open / Open With› / Reveal in Finder | Copy / Copy Path / Cut / Move to Trash. **Paste hidden** (pasteboard empty).
2. **Right-click `subA/` (folder)**: Open and Open With **hidden**, everything else present.
3. **Right-click the root row** (project name at top): Cut / Copy / Move to Trash **hidden**. Reveal / Copy Path remain.
4. **Click somewhere else then right-click `a.txt` again**: same items, no leftover state.

## Phase 2 — Single-select Copy + Paste

1. Right-click `a.txt` → **Copy**. Now right-click `subB/` → menu shows **Paste**. Click it.
2. Expand `subB/`: `a.txt` is inside. Original `a.txt` still at root.
3. Right-click `a.txt` → **Copy** again. Right-click any *file* inside `subB/` (not the folder itself) → **Paste**. The new copy should land in `subB/`, not next to that file's row at the root. (Paste-into-file resolves to parent.)
4. **Collision rename**: with `subB/a.txt` already there, right-click `a.txt` → Copy → right-click `subB/` → Paste. Confirm `subB/a copy.txt` appears. Repeat once more → `subB/a copy 2.txt`.
5. **Same-parent paste**: right-click root `a.txt` → Copy → right-click root → Paste. Confirm `a copy.txt` lands at root.

## Phase 3 — Cut + Paste

1. Reset: clear out `subB/` so it's empty.
2. Right-click `a.txt` → **Cut**. Confirm the row visibly **dims to ~half opacity** (cut ghost).
3. Right-click `subB/` → **Paste**. `a.txt` moves into `subB/` and the ghost clears.
4. **Cut intent invalidation**: cut `b.txt`, then in Finder copy any file. Come back to Nice and right-click `subB/` → the menu still shows Paste, but it pastes as a **copy** (not a move), and the original `b.txt` is unchanged. Ghost on `b.txt` clears.
5. **Cut a folder**: right-click `subA/` → Cut → Paste into `subB/`. Whole tree (including `subA/c.txt`) moves.

## Phase 4 — Multi-select

1. Re-seed files at root: `m1.txt`, `m2.txt`, `m3.txt`, `m4.txt`.
2. **Cmd-click** `m1.txt`, then **Cmd-click** `m3.txt`. Both have the accent background; `m2.txt` doesn't.
3. **Shift-click** `m4.txt`. Range from anchor (`m3.txt`, the last plain/cmd click) through `m4.txt` is selected. (Anchor doesn't move on shift-click.)
4. **Cmd-click** `m1.txt` again to **toggle off**. It deselects.
5. Right-click `m2.txt` (which is **outside** the selection): selection visibly snaps to just `m2.txt`, menu acts on it only.
6. Re-select `m1.txt`, `m3.txt`, `m4.txt` via Cmd-click. Right-click `m3.txt` (**inside** the selection): menu acts on all three. Click **Move to Trash**: all three disappear.
7. ⌘Z: all three return.

## Phase 5 — Trash + Undo / Redo

1. Right-click `a.txt` → **Move to Trash**. Confirm it disappears from the tree and **appears in macOS Trash** (open Trash app).
2. **⌘Z**: file reappears at original location, also gone from Trash.
3. **⌘⇧Z**: re-trashed.
4. Trash a folder. ⌘Z restores it with all children intact.
5. Trash three files. ⌘Z three times restores them in reverse order.
6. Trash a file, undo, then **trash a different file**. Try ⌘⇧Z: should be a no-op (the new op cleared the redo stack).

## Phase 6 — Drift handling

1. Right-click `a.txt` → Cut → Paste into `subB/`.
2. Without using Nice, open Finder and **delete `subB/a.txt`** (move it to Trash directly in Finder).
3. Back in Nice, ⌘Z. **Drift banner appears at the bottom** with text like "Couldn't undo: 'a.txt' is no longer there." Auto-dismisses after ~3.5s, or click the X to dismiss now.
4. Trash a file. Empty the macOS Trash. ⌘Z. Banner shows "was emptied from Trash."

## Phase 7 — Cross-window undo focus follow ⭐

This is the headline of the feature.

1. Open a **second window** (⌘N). Both windows now show file browsers.
2. In **Window B**, switch sidebar to a tab whose CWD is a different directory.
3. In **Window A**, trash `a.txt`.
4. Switch focus to **Window B** (click on it).
5. Press **⌘Z** in Window B. **Window A snaps to the front**, file browser is showing, the tab where the trash happened is selected, and `a.txt` reappears.
6. ⌘⇧Z (now still in Window A): trash happens again.
7. **Closed-window heads-up**: trash a file in Window B. Close Window B (⌘W). Bring Window A to front. ⌘Z. Banner says "Undid Move to Trash — change landed in a closed window." File came back on disk (Reveal in Finder to confirm).

## Phase 8 — Open With

1. Right-click a `.swift` file → **Open With ›**. Submenu lists detected apps. **Default app** (e.g. Xcode) appears first marked "(default)". Others alphabetised.
2. Click any one — it opens the file there.
3. Right-click → Open With › → **Other…**. NSOpenPanel rooted at `/Applications` opens. Pick anything (e.g. TextEdit) → file opens with it.
4. Right-click a binary / unfamiliar extension. Submenu may say "No applications found"; "Other…" still works.

## Phase 9 — Pasteboard interop with Finder

1. In Finder, copy a file (⌘C). Switch to Nice → right-click any folder → Paste. File lands in that folder.
2. In Nice, copy a file. Switch to Finder, navigate to a folder, paste (⌘V). File lands there.
3. Cut in Nice → switch to Finder → paste. Should paste as a **copy** (macOS pasteboard has no native cut-files concept; the cut intent is in-app only).

## Phase 10 — Copy Path

1. Right-click `a.txt` → **Copy Path**. Open a terminal, paste — the absolute path appears.
2. Multi-select two files. Right-click → Copy Path → paste in terminal: two newline-separated absolute paths.
3. Confirm Cut highlight (if any) clears after Copy Path (different content on the pasteboard now).

## Phase 11 — Keybinding integration

1. Settings → Shortcuts: confirm two new rows appear: **Undo file operation** (⌘Z) and **Redo file operation** (⌘⇧Z).
2. Click the recorder for Undo, press a different combo (e.g. ⌘⌥Z). Trash a file → press the new combo → restored.
3. Reset Undo to default. Hit ⌘Z in a text field (e.g. tab title rename) — text-field undo still works (the global monitor doesn't steal it).

## Phase 12 — Edge cases

1. **Empty selection right-click**: click empty space in the file tree (not on a row), then right-click any row. Menu acts on just that row.
2. **Hidden files toggle (⌘⇧.)**: toggle dotfiles on, right-click a `.gitignore` → menu appears as on any file.
3. **Show hidden + cut**: with hidden visible, cut `.gitignore`, paste into another folder. Move works; ghost on the source clears.
4. **Trash a folder, then trash another folder with the same name elsewhere, then ⌘Z twice**: both restore to their distinct original locations.
5. **Restart Nice** mid-flow (after a trash, before undo): the undo stack is in-memory only — confirm `⌘Z` after relaunch is a no-op (no banner, nothing happens). The trashed file is still in macOS Trash; nothing is silently restored.

## Quick smoke (if pressed for time)

Phases 1, 2, 3, 5, 7. That covers the menu, copy, cut+ghost, trash+undo, and the cross-window focus follow — the highest-risk parts.
