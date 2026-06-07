# Handoff: in-place top-bar / sidebar correctness refactor

**Purpose of the new conversation:** implement the in-place chrome-correctness
refactor for Nice's custom top bar + sidebar. This is the agreed path after a long
investigation; the architecture decision is already made — **do not** re-open it.

**Paste this whole file into the fresh conversation as the brief.**

---

## TL;DR for the new session

Nice draws its own 52pt top bar (`.hiddenTitleBar` + custom SwiftUI) and a floating
inset sidebar card. That architecture is correct and stays. There is a small,
well-defined set of **window-chrome correctness bugs + structural fragility** to
fix in place. No foundational rewrite. No native `NSToolbar` / window-tabbing /
`NSTitlebarAccessoryViewController` migration — that was spiked and **proven
unviable** (see below). Drag-to-reorder pills is **out of scope**.

---

## How we got here (context, do not redo)

A full investigation lives in `docs/research/`:
- `custom-macos-toolbar-best-practices.md`, `macos-full-height-sidebar-best-practices.md` — best-practices research
- `toolbar-gap-analysis.md`, `sidebar-gap-analysis.md` — audits of the current code
- `divergence-justifications.md` — adversarial defense of the custom design
- `refactor-recommendation.md` — **the synthesis + the SPIKE RESULTS; read this first**

**Settled conclusions (don't relitigate):**
- The custom pane pills, floating sidebar card, custom source list, and custom
  palette/materials are **justified or on-pattern** (the sidebar is the same
  floating-card pattern as Apple Music / Finder / Xcode). Keep them.
- The native `NSTitlebarAccessoryViewController` migration is **rejected on spike
  evidence**: accessory height is capped (`.top`=32pt, `.bottom`=36pt, invariant to
  request), it forces a two-tier layout, the system title area can't be themed
  (seams against non-system palettes), and it breaks the full-height sidebar. The
  throwaway spike + screenshots are preserved at git tag **`spike/native-titlebar`**
  (`git show spike/native-titlebar`); screenshots in `docs/research/spike/`.

---

## ⚠️ Build / test rules (from CLAUDE.md — read before running anything)

- This work happens in the git worktree at
  `/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`, branch
  `worktree-refactor-top-bar`. Run all commands from there; do **not** `cd` to the
  main checkout.
- **Only ever touch the `Nice Dev` build**, never the user's prod `/Applications/Nice.app`
  (it hosts the live Claude session). Install with `scripts/install.sh` (defaults to
  dev — safe). Never run bare `xcodebuild`/`xcodebuild test` against the `Nice`
  scheme (that hits the prod bundle id).
- **Acquire the worktree lock** around any install/test/xcodebuild:
  `scripts/worktree-lock.sh acquire <op>` … `scripts/worktree-lock.sh release`
  (there are `worktree-lock` and `nice-install` skills for this). The build is
  slow; run install in the foreground with a long timeout, and release the lock
  even on failure.
- Tests: `scripts/test.sh` (forwards `-only-testing:` args). UITests drive the dev
  app bundle.
- **SourceKit caveat:** after adding a new file, the editor will spuriously report
  "cannot find type X in scope" for same-module types until `xcodegen` regenerates
  the project (which `scripts/install.sh` does). The build is the source of truth —
  don't chase those phantom diagnostics.

---

## The work items (prioritized)

All paths under `Sources/Nice/`. Line numbers are current as of this handoff
(branch `worktree-refactor-top-bar`); re-grep if they've drifted.

### High — correctness bugs (no requirements defense; just wrong)

1. **Honor `AppleActionOnDoubleClick` in double-click-to-zoom.**
   `TitleBarZoomMonitor` unconditionally calls `window.performZoom(nil)` at
   `Views/WindowDragRegion.swift:99`. macOS users can set the title-bar
   double-click action to Maximize / Minimize / Do Nothing. Read it live from
   `NSGlobalDomain` (`UserDefaults.standard.string(forKey: "AppleActionOnDoubleClick")`,
   values `Maximize` / `Minimize` / `None`; absent ⇒ treat as Maximize/zoom, the
   system default) and branch to `performZoom` / `performMiniaturize` / no-op.

2. **Re-apply traffic-light offsets on full-screen transitions.**
   `TrafficLightNudger.nudge` installs observers for `didBecomeKeyNotification`
   (`Views/TrafficLightNudger.swift:91`) and `didResizeNotification` (`:101`) only.
   macOS also resets custom button positions entering/exiting full screen. Add
   observers for `willEnterFullScreenNotification` / `didEnterFullScreenNotification`
   / `willExitFullScreenNotification` / `didExitFullScreenNotification`, re-applying
   the offset. **Smallest, highest-value fix — one file.**

3. **Handle the custom band in full screen at all.**
   There is no full-screen handling anywhere (`grep -rn FullScreen Sources/` is
   empty). The hard-coded 52pt band / safe zone / zoom gate don't track the
   title-bar band changing in full screen. **Manually test enter/exit full screen
   first** to see the actual misbehavior, then branch to recompute or hide the band.
   Depends on #4. (The spike confirmed the native bar would handle this for free —
   but we're staying custom, so it's ours to handle.)

### Medium — structural fragility (and prerequisites for the High items)

4. **One shared constant for the 52pt band height.** `52` is duplicated
   independently at `Views/AppShellView.swift:390` (card spacer),
   `Views/AppShellView.swift:608` (window-background band),
   `Views/WindowToolbarView.swift:57` (toolbar height), and
   `Views/WindowDragRegion.swift:88` (zoom-gate `yFromTop <= 52`). Fold into one
   constant. **Do this first — it unblocks #3 and #6.**

5. **Couple the collapsed-cap geometry to the traffic-light offset.** The collapsed
   cap reserves a leading `WindowDragRegion().frame(width: 82)`
   (`Views/AppShellView.swift:549`) and the nudge offset is `dx: 8, dy: -10`
   (`Views/AppShellView.swift:189`) — independently tuned magic numbers that
   silently drift. Derive both from one geometry source.

6. **Make the zoom region derive from the shared constant.** The zoom monitor is a
   process-wide `leftMouseDown` hook gated on the magic `52`
   (`Views/WindowDragRegion.swift:87-88`). Source the gate from #4 so it can't
   desync. ⚠️ **Do NOT naively replace the process-wide monitor with a `mouseDown`
   override** — the file's own header comment (`WindowDragRegion.swift:26-38`)
   explains why that fails under SwiftUI hosting + `mouseDownCanMoveWindow`, and
   `UITests/WindowDragUITests.swift:131` (`testEmptyToolbarDoubleClickZoomsWindow`)
   guards the behavior. Keep the monitor; just de-magic the constant. See
   `divergence-justifications.md` §3.

### Low — polish

7. **Animate sidebar collapse/expand.** `SidebarModel.toggleSidebar()` flips a Bool
   (`State/SidebarModel.swift:44-45`) and the shell swaps with a bare `if`
   (`Views/AppShellView.swift:342`) — the sidebar snaps. Wrap in `withAnimation` /
   add `.animation(value:)`, gated on `accessibilityReduceMotion`.

---

## What NOT to touch (settled — changing these re-opens closed decisions)

- The `.hiddenTitleBar` + custom-band architecture (native accessory rejected).
- Custom pane pills (native window tabbing is structurally inapplicable — panes
  aren't `NSWindow`s; each pane has its own pty, only the active one is mounted).
- The floating inset sidebar card pattern and the custom `ScrollView`/`VStack`
  source list.
- The custom palette/materials system (it already uses native `.sidebar` vibrancy
  where correct — the `.macOS` palette — and must paint non-system themes itself).
- The window-drag region itself (`mouseDownCanMoveWindow = true`,
  `WindowDragRegion.swift:57`) — already correct and on best-practice.
- Drag-to-reorder pills — explicitly out of scope for this refactor.

---

## Suggested sequencing

1. **#4** (shared 52pt constant) — small; unblocks #3 and #6.
2. **#2** (full-screen traffic-light observers) — isolated one-file win.
3. **#1** (`AppleActionOnDoubleClick`) — isolated, same file region as #6.
4. **#3** (full-screen band) — manual full-screen test pass first to scope it.
5. **#5, #6** — fragility cleanup once the constant exists.
6. **#7** (collapse animation) — independent, any time.

---

## Verification

- **Build/install:** `scripts/worktree-lock.sh acquire refactor && scripts/install.sh; scripts/worktree-lock.sh release`
  → expect `** BUILD SUCCEEDED **` and `installed Nice Dev`.
- **Existing guards to keep green:**
  - `Tests/NiceUnitTests/WindowToolbarDragRegionTests.swift`
    (`testDragViewOptsIntoCooperativeDrag`)
  - `UITests/WindowDragUITests.swift` (`testEmptyToolbarDragMovesWindow`,
    `testEmptyToolbarDoubleClickZoomsWindow`) — the double-click test is the
    backstop for #1/#6.
  - Run via `scripts/test.sh` (under the lock).
- **Manual checks** (the `verify` / `run` skills can drive `Nice Dev`):
  - #1: in System Settings set Desktop & Dock → "double-click a window's title bar
    to" = Minimize, then Do Nothing; double-click the band each time and confirm it
    matches (and Maximize for zoom).
  - #2/#3: enter/exit full screen (⌃⌘F); confirm traffic lights stay correctly
    placed and the band reflows sanely on both transitions and on resize.
  - #7: ⌘B collapse/expand animates and respects Reduce Motion.
- Note: screenshots can't be captured from inside the agent's pty (no Screen
  Recording permission) — ask the user for visual confirmation if needed, as we did
  during the spike.

---

## Done = 

#1–#7 implemented, dev build green, the three existing drag/zoom tests still pass,
and manual full-screen + double-click-action behavior verified. Commit on
`worktree-refactor-top-bar` (branch protection bypass on pushes is fine per the
user's standing preference; this is a feature branch anyway).
