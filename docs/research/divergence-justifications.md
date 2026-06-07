# Divergence Justifications — the defense of Nice's custom top bar & sidebar

> Role: devil's advocate / defense counsel. Three prior docs
> (`custom-macos-toolbar-best-practices.md`, `macos-full-height-sidebar-best-practices.md`,
> `toolbar-gap-analysis.md`, `sidebar-gap-analysis.md`) argue Nice diverges from
> macOS best practice in eight ways. This document builds the strongest *legitimate*
> case for each divergence grounded in Nice's real requirements — and concedes,
> plainly, the ones with no good defense. No code was changed.
>
> All file:line references are to the worktree at
> `/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`.

---

## Executive summary

Nice is a **multi-theme terminal emulator that hosts long-lived ptys** (shells
and Claude Code sessions). Once you hold that requirement set in your head, the
picture splits cleanly into two halves, and the two halves have opposite verdicts.

**The architectural / pattern divergences are largely justified.** Custom pane
pills, the custom floating sidebar card, the custom source list, and the custom
palette/material system are not laziness — they are forced (or strongly favored)
by three hard requirements the native machinery actively fights: (1) panes are
**not** `NSWindow`s (only the active one is mounted in a single window), so
`NSWindowTabGroup` — which requires one `NSWindow` per tab — would invert the app
into N windows-per-pane and is structurally inapplicable; (2) Nice ships
**non-system themes** (Catppuccin, Dracula, Nord, …), so native chrome that
follows `NSApp.appearance` cannot render them; and (3) the multi-pane,
single-window session model has no native control that maps onto it. On these,
the "move toward best practices" recommendations (`NSTitlebarAccessoryViewController`,
`NavigationSplitView`, `.listStyle(.sidebar)`, native vibrancy only) would cost
Nice something concrete and in several cases are simply incompatible.

**The window-chrome *correctness* items are mostly indefensible, and I concede
them.** Hand-rolled traffic-light positioning that isn't re-applied on full-screen,
the complete absence of any full-screen handling, and double-click-to-zoom that
ignores `AppleActionOnDoubleClick` are bugs, not trade-offs. A credible defense
doesn't dress a bug as a design decision. The one nuance: hand-rolling the
*window-move* hook is correct and well-supported, and the process-wide zoom
monitor — while ugly — has a documented, test-backed justification that the
"obvious" native fix does not actually work in SwiftUI's hosting configuration
(`WindowDragRegion.swift:25-43`).

**Honest bottom line:** Of the 8 divergences, I rate **4 Strong, 1 Moderate, 1
Weak, and 2 None (conceded)**. The plan to "move toward best practices" should be
*narrowed, not abandoned*: keep every custom visual/architectural choice (they are
on-pattern or required), and spend the effort exclusively on the window-chrome
correctness cluster — full-screen handling and the `AppleActionOnDoubleClick`
read. Notably, the gap docs' headline recommendation —
`NSTitlebarAccessoryViewController` — is the *one* refactor I'd push back on
hardest, because the fixed native title-bar height (~28pt) cannot host Nice's 52pt
pill strip without clipping, and adopting it re-introduces system-appearance
coupling that breaks the themes.

---

## What Nice actually is (the requirements that matter)

These are the load-bearing facts. Every verdict below traces back to one of them.

1. **It is a terminal host for long-lived ptys, not a document app.** README:
   "A native macOS terminal that organizes your Claude Code sessions… Nice spawns
   it in a fresh pty." Sessions must survive across UI churn; CLAUDE.md treats
   killing the app as "los[ing] the user's live session work." Anything that
   tears down and re-creates windows/views (e.g. native window tabbing, which
   swaps `NSWindow`s) is dangerous to a live pty.

2. **A tab hosts *multiple* panes, and a pane is not a window.** `Tab.panes:
   [Pane]` is "Ordered panes shown as pills in the toolbar" (`Models.swift:127-129`).
   A single tab routinely owns a `.claude` pane *and* one or more `.terminal`
   panes (`Models.swift:34-79`, `PaneKind`). Each pane is its **own**
   `NiceTerminalView` with its **own pty and child process** — they do *not* share
   a pty. They are merely grouped per-tab by a `TabPtySession` container
   (`addPane` appends to `tab.panes`, reuses the tab's `TabPtySession` from
   `ptySessions[tabId]`, and adds a *new* terminal view to it,
   `SessionsModel.swift:588-594`). Only the `activePaneId` pane is mounted at a
   time — selecting a pill swaps the visible terminal NSView in `mainContent`
   (`AppShellView.swift:639-643`). The pills are an intra-window strip, not a
   window group.

3. **There is exactly one window class for the main UI, and many panes inside it.**
   `WindowGroup(id: "main")` (`NiceApp.swift:82`) is the only main scene; multiple
   *windows* are siblings (⌘N / restore), but panes never become windows. So the
   "one tab == one `NSWindow`" model that `NSWindowTabGroup` requires
   (best-practices doc §4) is the wrong shape twice over: Nice's *pills* are panes
   (sub-window) and Nice's *sidebar tabs* are sessions (also sub-window).

4. **Nice ships non-system themes.** README: "Twelve built-in terminal themes —
   Catppuccin (all four), Dracula, Nord, Gruvbox, Tokyo Night, Solarized, Atom
   One… plus five native-chrome accents." The chrome palette is a first-class axis
   (`Palette` enum: `.nice`, `.macOS`, `.catppuccinLatte`, `.catppuccinMocha` —
   `Tweaks.swift:33`), and only the `.macOS` palette delegates to system semantic
   colors (`Palette.swift:76-88`, every `niceX` helper switches `.macOS →
   NSColor.windowBackgroundColor`). The Catppuccin palettes paint *literal* sRGB
   values that have nothing to do with `NSApp.appearance`. **Native chrome can only
   render the `.macOS` palette** — by construction, `NSToolbar`/native title bar
   vibrancy follows the system appearance, which Tweaks deliberately pins to
   `.aqua`/`.darkAqua` (`Tweaks.swift:223-228`) regardless of the chosen theme.

5. **The whole window must repaint instantly on a live theme switch.** README:
   "Switch live from Settings; the whole window repaints instantly." This is a
   SwiftUI environment-driven repaint (`palette` env key, `Palette.swift:32-46`;
   `.environment(\.palette, …)` at `AppShellView.swift:201`). Native title-bar
   chrome is not in that reactive graph.

6. **Deployment target is macOS 14 (Sonoma).** README "Requirements." So Tahoe /
   Liquid Glass and the macOS-15-only `Transferable` insertion-index APIs are not
   available as a baseline.

7. **The team can and does build correct custom DnD when it matters.** The sidebar
   already implements live drag-reorder with the *exact* community-recommended
   pattern: built-in `String` payload not a custom UTI (`SidebarView.swift:656`),
   a pure resolver, a `DropDelegate`, and the deferred-mutation fix
   (`DispatchQueue.main.async` in `performDrop`, `SidebarView.swift:907`). This
   matters for the defense: when Nice hand-rolls something, it is generally because
   native didn't fit, not because the team can't use native APIs.

---

## Defense table

| # | Divergence | Best-practice expectation | Nice's approach | Strength | One-line verdict |
|---|---|---|---|---|---|
| 1 | `.hiddenTitleBar` + custom 52pt bar | `NSTitlebarAccessoryViewController`; let AppKit own the title bar | Fully custom SwiftUI bar (`NiceApp.swift:100`) | **Moderate** | Justified by theming + 52pt height + repaint; but the accessory path is partly viable, so not airtight. |
| 2 | Custom pane pills vs. native window tabbing | `NSWindowTabGroup` for free reorder/+/overflow/a11y | `ForEach(tab.panes)` pill strip (`WindowToolbarView`) | **Strong** | Justified — native tabbing needs one `NSWindow` per tab; mapping pills to it would invert a single-window app into N windows-per-pane and forfeit theming + custom pill affordances. (Not because panes "share a pty" — they don't.) |
| 3 | Hand-rolled drag + double-click-zoom | `mouseDownCanMoveWindow` regions + read `AppleActionOnDoubleClick` | `DragView` (correct) + `TitleBarZoomMonitor` (always zooms) | **Weak / split** | Move-region is correct & justified; the *monitor* is a defensible workaround but it **ignores the user preference** — that part is conceded. |
| 4 | Hand-positioned traffic lights | Don't move them; if you must, re-apply on resize **and** full-screen | `TrafficLightNudger`, re-applies on key/resize **only** (`TrafficLightNudger.swift:89-109`) | **None (concede)** | Repositioning is defensible to match Xcode inset; the missing full-screen re-apply is a bug, not a trade-off. |
| 5 | Floating card vs. `NavigationSplitView` | Native split view / `NSSplitViewController` | Hand-built inset card (`AppShellView.swift:376-455`) | **Strong** | Justified — the floating card *is* the modern pattern (Music/Finder/Xcode), and native split view can't carry non-system themes. |
| 6 | Custom `ScrollView`+`VStack` vs. `List(.sidebar)` | `List` + `.listStyle(.sidebar)` for native source list | Custom `ScrollView`/`VStack` of `ProjectGroup`/`TabRow` | **Strong** | Justified — branch-lineage tree, per-row pty status dots, custom DnD, and theme tints exceed what `.listStyle(.sidebar)` cleanly supports. |
| 7 | Custom materials/palette vs. native `.sidebar` vibrancy | System vibrancy only | Palette-switched: flat / vibrancy / tinted-vibrancy (`SidebarBackground.swift`) | **Strong** | Justified — multi-theme requirement makes "native vibrancy only" impossible; and `.macOS` palette *does* use exactly the native `.sidebar` material. |
| 8 | Custom 52pt band / manual safe-area | Toolbar-driven insets, native safe area | `ignoresSafeArea(.top)` + manual 52pt spacer (`AppShellView.swift:174,389`) | **None (concede) for full-screen; Strong for the design** | The structural 52pt inset is correct for a floating card; the missing full-screen recompute (52 hard-coded in 3 places) is the conceded bug. |

---

## Per-divergence defense

### 1. Fully custom bar instead of `NSTitlebarAccessoryViewController` — **Moderate**

**Best-practice expectation.** Keep the pill strip custom but host it in an
`NSTitlebarAccessoryViewController` so AppKit retains the title bar and you inherit
correct traffic-light layout, full-screen transitions, and native move/zoom over
the title region (toolbar best-practices doc §3, Rec #1).

**Nice's approach.** `.windowStyle(.hiddenTitleBar)` (`NiceApp.swift:100`) + a 52pt
SwiftUI `HStack` stacked above content (`AppShellView.swift:355-358`).

**Strongest defense.**
- **The 52pt height is the killer detail for the accessory.** The best-practices
  doc itself flags (and lists as an open uncertainty, §3 / Uncertainties) that the
  native title-bar height is fixed (~28pt) and a `.bottom` accessory is *clipped*
  by the title row; it explicitly cannot promise a 52pt strip renders un-clipped.
  Nice's bar is 52pt by design and the height is load-bearing — it is the
  traffic-light safe zone, the drag region, and the alignment datum for the
  collapsed cap and the sidebar card top (`AppShellView.swift:379-390`,
  `windowBackground` band at `:601-608`). The accessory path would force either a
  clipped strip or a second sub-title-bar band, i.e. *more* custom layout, not
  less.
- **Theming.** An accessory view is wrapped by AppKit in a system visual-effect
  view tied to `NSApp.appearance`. Nice's chrome is painted by the SwiftUI
  `palette` graph (`niceChrome`, `Palette.swift:245-257`) and must repaint live on
  theme switch (req. 4–5). A SwiftUI `NSHostingView` inside the accessory *can*
  paint custom colors, but you fight the surrounding system material to keep
  Catppuccin/Solarized from showing a system-tinted seam.
- **Reorder is independent.** The toolbar gap analysis concedes the accessory
  migration is "not a prerequisite" for the actual goal (drag-to-reorder pills) and
  "would make a small feature ride on a risky rewrite" (`toolbar-gap-analysis.md:38,310`).
  So the divergence buys simplicity for the feature the team actually wants.

**Honest weakness.** The accessory path is *not impossible* — Nice could put the
title hidden and use a tall `.bottom` accessory, and it would genuinely recover
full-screen handling and traffic-light layout for free, which Nice is currently
missing (see #4, #8). So this isn't airtight: the custom bar is *defensible* and
lower-friction today, but "we couldn't use the accessory" overstates it. The
truthful claim is "the accessory has a real height/theming cost and doesn't
advance the actual roadmap goal," which is Moderate, not Strong.

**Verdict: Partially justified (Moderate).** Reasonable engineering trade-off given
the 52pt height and live-theming constraints; weakened by the fact the accessory
would also fix the conceded chrome bugs.

---

### 2. Custom pane pills instead of native window tabbing — **Strong**

**Best-practice expectation.** `NSWindowTabGroup` / `addTabbedWindow(_:ordered:)`
gives reorderable tabs, a "+", overflow, and full a11y for free (toolbar
best-practices doc §4).

**Nice's approach.** A horizontal `ForEach(tab.panes)` pill strip inside the bar
(`WindowToolbarView`), ordered purely by the `Tab.panes` array.

**Correction.** An earlier draft of this entry justified the divergence by claiming
panes "share a single pty session." **That is false** — each pane has its own pty
and child process; `TabPtySession` is only a per-tab *container* of pane views
(`SessionsModel.swift:588-594`). The conclusion below stands, but on the real
reasons, not that one.

**Strongest defense (this one is decisive).**
- **Native window tabbing requires one `NSWindow` per tab.** The tab bar is
  system-drawn and a "tab" *is* a whole `NSWindow`; a tab group is an emergent
  property of which windows are tabbed together, not a swappable container. Nice's
  pills are panes inside a *single* window — only the `activePaneId` pane is
  mounted, and selecting a pill swaps an NSView in `mainContent`
  (`AppShellView.swift:639-643`). Adopting native tabbing would invert this into
  **N `NSWindow`s per tab**, hosting each terminal in its own window.
- **The "swap a tab group per sidebar tab" idea doesn't rescue it.** There is no
  hidden/inactive tab group to toggle; switching the visible set would require
  programmatically *untabbing* every window of the old sidebar tab and *retabbing*
  the new one on **every sidebar selection** — heavyweight, animated window
  regrouping (built for user-driven tab drags), on each click — while the
  system-drawn tab bar becomes a second, authoritative source of truth that must be
  kept in sync with the data model *and* the sidebar list.
- **The best-practices doc reaches the same conclusion.** §4: if Nice's panes
  aren't 1:1 with `NSWindow`s (they aren't), native window tabbing is a poor fit;
  Chrome, iTerm2, and Warp all roll their own for exactly this reason.
- **pty lifecycle risk.** Native tabs are full windows with their own
  close/minimize/zoom/state-restoration/menu behavior; panes are deliberately
  *held after their process exits* so scrollback survives (`PaneEntry.isHeld`).
  Handing pane lifecycle to AppKit's window/tab machinery is risk with no upside
  for an app whose value proposition is "never lose a live session" (README,
  CLAUDE.md).
- **Theming + per-pill semantics native tabbing can't express.** The system tab
  bar follows system appearance and can't render Nice's non-system themes
  (Catppuccin/Nord/etc.). Pills also carry per-pane attention state (`StatusDot`,
  `Pane.needsAttention`), an overflow chevron that badges off-screen attention
  (`Tab.hasOffscreenAttention`, `Models.swift:225`), inline rename, and per-pane
  close — none of which native window tabs model.

**Verdict: Justified (Strong).** Not because panes "share a pty," but because
native window tabbing forces one-`NSWindow`-per-pane and a system-drawn, unstylable
tab bar — inverting a single-window SwiftUI app and discarding the custom pill UX
and theming. The custom strip is the correct choice, not a divergence to "fix."

---

### 3. Hand-rolled window drag + double-click-to-zoom — **Weak / split verdict**

**Best-practice expectation.** Use `mouseDownCanMoveWindow` per region for move;
for zoom, read `AppleActionOnDoubleClick` from `NSGlobalDomain` and branch
Maximize/Minimize/None, live (toolbar best-practices doc §6).

**Nice's approach.** Two pieces (`WindowDragRegion.swift`): a `DragView`
overriding `mouseDownCanMoveWindow → true` (`:56-58`), and a process-wide
`TitleBarZoomMonitor` that on any in-region double-click calls
`window.performZoom(nil)` unconditionally (`:72-104`).

**Defense — the move region (Strong, fully justified).** The `DragView` is
*exactly* the API the doc recommends (`mouseDownCanMoveWindow`, doc §6
`[MouseDown]`). The gap analysis agrees this is "correct" (gap table row 4). No
concession needed here — and crucially it sits *behind* the pills so a pill drag
isn't stolen by window-move (`WindowToolbarView` ZStack ordering;
`toolbar-gap-analysis.md:253-257`), which is precisely the future-proofing the
reorder feature needs.

**Defense — the zoom *monitor as a mechanism* (Moderate).** The natural objection
is "why a process-wide `NSEvent` monitor instead of a `mouseDown` override?" The
file header answers it with receipts (`WindowDragRegion.swift:25-43`): a prior
"Phase A" tried folding zoom into a `mouseDown(_:)` override and it **provably
fails in both drag configurations** — with `mouseDownCanMoveWindow = true` AppKit's
title-bar tracker eats the gesture and `mouseDown` is never delivered for
stationary clicks; with it `false`, `performDrag` for the single-click case
absorbs the second click so `clickCount` never reaches 2. Both failures are
**covered by a UITest** (`WindowDragUITests.testEmptyToolbarDoubleClickZoomsWindow`).
That is a documented, test-backed reason the "obvious" native approach doesn't work
through SwiftUI's hosting machinery — a legitimate engineering justification for an
ugly tool, not cargo-culting.

**Concession — the zoom *behavior*.** The monitor calls `performZoom(nil)`
unconditionally (`:99`). It does **not** read `AppleActionOnDoubleClick`. A user
who set "Minimize" or "Do Nothing" in System Settings gets the wrong behavior. This
is the single most-cited custom-bar regression in the doc (§6) and there is no
requirements-based defense for it — it's a straightforward bug. Worse, it's a
*cheap* fix (read one global default, branch three ways) that doesn't depend on any
architecture change, so "low priority" is the only mitigation, not "justified."

**Verdict: split.** Move region — Justified (Strong). Zoom monitor mechanism —
Partially justified (Moderate, with a test-backed rationale). Zoom *behavior*
ignoring the user preference — **Not justified (concede)**. Net rating **Weak**
because the user-visible behavior is wrong.

---

### 4. Hand-positioned traffic lights (`TrafficLightNudger`) — **None (concede)**

**Best-practice expectation.** Prefer system placement; if you must reposition,
re-apply on **every** resize *and* full-screen transition (both docs: toolbar §6,
sidebar §3 / overlap #5).

**Nice's approach.** `TrafficLightNudger.nudge(window:, dx:8, dy:-10)` offsets the
three standard buttons (`TrafficLightNudger.swift:112-129`) and re-applies on
`didBecomeKey` + `didResize` **only** (`:89-109`). No full-screen observers exist
anywhere (`grep FullScreen Sources/` → nothing).

**The most I can say in defense.** The *decision to inset the buttons at all* is
defensible: with the sidebar card pulled to a 6pt inset, the default button
position lands flush against the card's rounded corner, and Xcode itself insets
its traffic lights into the sidebar for breathing room (`TrafficLightNudger.swift:5-11`).
Matching the Xcode look is a legitimate aesthetic goal, and the nudger is written
carefully — it captures canonical origins so the offset is idempotent and doesn't
compound across focus events (`:63-72,119-127`), a real bug they already fixed. So
the *existence* of the nudger has a rationale.

**Why I concede anyway.** The doc's specific charge is not "you repositioned" —
it's "you repositioned and didn't re-apply on full-screen." That charge is correct
and undefended: entering/exiting full screen resets button positions, and with no
full-screen observer the buttons revert to their default origin until the next
key/resize event nudges them back — a visible jump
(`sidebar-gap-analysis.md:178-190`). There is no requirement that *wants* this;
it's an omission. A defense attorney who argued "the live-pty model justifies not
handling full-screen" would lose all credibility. It's a bug.

**Verdict: Not justified (concede).** The repositioning is defensible; the missing
full-screen re-apply is a bug. Fix is small and isolated (add the four
`NSWindow.*FullScreen*Notification` observers in `nudge`).

---

### 5. Floating inset card instead of `NavigationSplitView` / `NSSplitViewController` — **Strong**

**Best-practice expectation.** Use `NavigationSplitView` (SwiftUI) /
`NSSplitViewController` for a full-height sidebar; you inherit full-height layout,
divider tracking, and toggle for free (sidebar best-practices doc §1).

**Nice's approach.** A hand-built `HStack` of `[floatingSidebarCard | VStack {
toolbar; content }]` (`AppShellView.swift:351-360`); the card is a 6pt-inset,
rounded, shadowed, resizable panel (`:434-450`).

**Strongest defense.**
- **The floating card *is* the canonical modern pattern, not a divergence from
  it.** The sidebar gap analysis explicitly corrects the earlier framing:
  "Apple Music is **not** a flush full-height column; like Nice, it floats an inset
  card with rounded corners and a drop shadow… So Nice is **matching** the canonical
  reference" (`sidebar-gap-analysis.md:14-31,223-240`). Several "flush-pattern"
  best-practice items (`allowsFullHeightLayout`, `NSTrackingSeparatorToolbarItem`,
  scroll-under) are therefore **N/A by design** — they belong to a different,
  older style.
- **Native split view re-introduces system-appearance coupling that breaks the
  themes.** `NavigationSplitView`/`.listStyle(.sidebar)` styling and vibrancy
  follow `NSApp.appearance`, which Tweaks pins to aqua/darkAqua
  (`Tweaks.swift:223-228`). The Catppuccin/Nice palettes paint literal sRGB
  (`Palette.swift`), so a native split view would render the chrome wrong for every
  non-`.macOS` theme — a concrete loss of the app's headline feature (req. 4).
- **Divider tracking is a non-problem here.** Because the toolbar is a *sibling
  column* to the right of the card (not overlaid on it), there is no
  toolbar-over-sidebar boundary to track; the `HStack` glues the bar's left edge to
  the card's trailing edge for free (`sidebar-gap-analysis.md:154`). The single
  hardest thing native gives you (the tracking separator) is something Nice's
  layout doesn't even need.
- **The resize handle and peek-overlay behavior** (`AppShellView.swift:457-496`,
  `509-533`) are custom interactions the floating card supports cleanly; bolting
  them onto `NSSplitViewController` collapse semantics would be more friction.

**Honest weakness.** Native `toggleSidebar(_:)` animates collapse; Nice's mode swap
is an un-animated `if` (`AppShellView.swift:340-347`, `sidebar-gap-analysis.md:203-212`).
That's a real polish gap — but it's a missing `withAnimation`, not a reason to
adopt the whole native stack, and it's orthogonal to the pattern choice.

**Verdict: Justified (Strong).** The floating card matches the modern reference and
is the *only* way to carry non-system themes; native split view would cost the
themes for benefits (divider tracking) Nice's layout doesn't need.

---

### 6. Custom `ScrollView`+`VStack` source list instead of `List` + `.listStyle(.sidebar)` — **Strong**

**Best-practice expectation.** Use SwiftUI `List` + `.listStyle(.sidebar)` for the
native source-list selection highlight and section styling (sidebar best-practices
doc §5).

**Nice's approach.** A hand-built `ScrollView` of `ProjectGroup`s containing
`TabRow`s (`SidebarView.swift:142-163`), painting no background so vibrancy shows
through (`:9-13`).

**Strongest defense.**
- **The rows carry semantics a stock sidebar `List` doesn't model.** Each `TabRow`
  shows a live pty status dot derived from the tab's Claude panes (`Tab.status`,
  `Models.swift:238`; `StatusDot`), a branch-lineage indent (depth-1 tree via
  `Tab.parentTabId`, `Models.swift:146-184`), inline rename, and per-row context
  menus. `.listStyle(.sidebar)` gives you selection chrome, not a custom
  parent/child lineage renderer with per-row live status.
- **Live drag-reorder is already implemented the *recommended* way on this custom
  list.** `TabRow.onDrag` returns a built-in `String` payload, not a custom UTI
  (`SidebarView.swift:656`, avoiding the macOS custom-UTI drop-target pitfall the
  doc flags), driven by `ProjectGroupDropDelegate` + a pure resolver + the
  deferred-mutation fix (`:868-913`). SwiftUI `List` `.onMove` is the *vertical*
  affordance, but it doesn't compose with the cross-group / lineage-aware drop
  logic Nice needs. The custom list is what *enables* correct DnD here, not what
  obstructs it (req. 7).
- **Theme fidelity again.** A `List(.sidebar)` paints its own selection material
  tied to system appearance; Nice tints selection with the *user's accent*
  regardless of palette (`niceSel(_:accent:)`, `Palette.swift:224-227`). That's a
  feature the native list fights.

**Honest weakness.** The doc's U1 is fair: nobody benchmarked the custom row
metrics (selection inset, section spacing) against a native source list, so "fully
matches native" is unproven. But "didn't pixel-match the baseline" is a QA gap, not
a reason the architecture is wrong.

**Verdict: Justified (Strong).** The lineage tree, per-row pty status, accent-tinted
selection, and cross-group DnD exceed what `.listStyle(.sidebar)` cleanly delivers.

---

### 7. Custom materials/palette instead of native `.sidebar` vibrancy only — **Strong**

**Best-practice expectation.** The sidebar background should be an
`NSVisualEffectView` with the `.sidebar` material, behind-window blending — native
vibrancy (sidebar best-practices doc §5).

**Nice's approach.** `SidebarBackground` switches on palette
(`SidebarBackground.swift:21-46`): `.nice` → flat `niceBg2`; `.macOS` → exactly the
native `VisualEffectView(.sidebar, .behindWindow, .active)`; Catppuccin → vibrancy
*plus* a 0.68-opacity tint.

**Strongest defense (this one nearly defends itself).**
- **"Native vibrancy only" is mathematically incompatible with a multi-theme app.**
  A behind-window `.sidebar` vibrancy view pulls the desktop wallpaper through and
  tints toward the system appearance. It cannot render "Catppuccin Mocha" or
  "Solarized" — those are fixed color identities (req. 4). Demanding native-only
  vibrancy is demanding the app drop its headline feature.
- **Where native vibrancy *is* the right answer, Nice already uses it verbatim.**
  The `.macOS` palette path is *literally* the doc's recommendation —
  `.sidebar` material, `.behindWindow`, `.active` (`SidebarBackground.swift:29-33`,
  `VisualEffectView.swift:21-40`), with the wallpaper-tinting comment matching
  Xcode/Finder/Mail. So Nice isn't rejecting native vibrancy; it's *scoping* it to
  the one palette where it's correct and substituting deliberate alternatives where
  it isn't.
- **The hybrid for Catppuccin is a thoughtful compromise, not a hack.** It keeps
  the vibrancy blur (so the card still reads as translucent glass) while overlaying
  a theme tint at 0.68 so the identity survives — the best of both within the
  constraint. The `.nice` palette deliberately goes flat because, per the code
  comment, "the nice palette's custom oklch values don't blend with vibrancy in a
  visually coherent way" (`VisualEffectView.swift:12-15`) — an informed visual
  decision, not laziness.

**Verdict: Justified (Strong).** "Native vibrancy only" is impossible for a
multi-theme app; Nice uses native vibrancy exactly where it applies and substitutes
considered alternatives elsewhere.

---

### 8. Custom 52pt band + manual safe-area instead of toolbar-driven insets — **Strong (design) / None (full-screen, concede)**

**Best-practice expectation.** Let the toolbar contribute the top safe-area inset;
respect native safe area so the first row clears the traffic lights, and recompute
on full-screen (sidebar best-practices doc §6).

**Nice's approach.** Shell root `ignoresSafeArea(edges:.top)` (`AppShellView.swift:174`)
paired with a structural 52pt `WindowDragRegion` spacer inside the card above
`SidebarView()` (`:389-390,430`); the window-background band is also 52pt
(`:601-608`); the zoom monitor gates on `yFromTop <= 52` (`WindowDragRegion.swift:88`).

**Defense — the manual inset itself (Strong).**
- With a hand-drawn bar there *is* no toolbar to contribute a safe-area inset, so
  owning the inset is the correct trade for a custom bar (the doc concedes exactly
  this, §7: "set the top content inset to your bar height (52pt) explicitly"). The
  sidebar gap analysis marks the inset items **Pass**: the first sidebar row clears
  the (nudged) lights, there's no double-gap, and `ignoresSafeArea` is correctly
  scoped (`sidebar-gap-analysis.md:155,165`). Notably, going manual here *avoids*
  the Apple-confirmed `NavigationSplitView` double-safe-area bug (rdar://122947424,
  doc §6) — a concrete correctness *win* for the custom approach.
- The 52pt also serves the seam-free overlap: the band over the sidebar is the
  sidebar's own material (the strip is *inside* the card), so there's no opaque
  seam at the sidebar/content boundary — the exact failure mode the doc warns about
  (`sidebar-gap-analysis.md:151`). Custom layout achieves the native look here.

**Concession — full-screen and the duplicated constant.** The 52 is hard-coded in
three independent places with no shared source of truth
(`AppShellView.swift:390`, `:608`, `WindowDragRegion.swift:88`), and nothing
recomputes the band when full screen changes its height. In native full screen the
title-bar band auto-hides/resizes; a band that assumes a constant 52pt will not
track it (`sidebar-gap-analysis.md:192-201`). This is the same omission as #4 —
undefended, a bug. The duplicated constant is also pure debt (one `enum` constant
would fix it).

**Verdict: design is Justified (Strong) — manual inset is correct for a custom bar
and dodges the native double-inset bug; full-screen handling is Not justified
(concede).**

---

## Concessions (stated plainly)

These have no requirements-based defense. They are bugs or debt, and pretending
otherwise would burn the defense's credibility:

1. **Double-click-to-zoom ignores `AppleActionOnDoubleClick`** (`WindowDragRegion.swift:99`).
   Always zooms; wrong for users who set Minimize / Do Nothing. Cheap fix,
   independent of any architecture change. *(part of #3)*

2. **Traffic-light offset is never re-applied on full-screen transitions**
   (`TrafficLightNudger.swift:89-109` — only key/resize). Buttons revert on
   enter/exit full screen. *(#4)*

3. **No full-screen handling anywhere for the custom band** (`grep FullScreen
   Sources/` → empty). Band height, 52pt safe zone, and traffic-light offsets are
   not recomputed in full screen. Behavior is unverified and likely wrong. *(#8)*

4. **The 52pt band height is duplicated across three files** with no shared
   constant — silent desync risk on any future change. Pure debt. *(#8)*

Minor, non-correctness debt I won't dress up either: the collapse/expand mode swap
is un-animated (`AppShellView.swift:340-347`), and the collapsed-cap geometry is
coupled to the nudge offset only by independently-tuned magic numbers
(`:189,549`) — fragile, though functionally correct today.

---

## Bottom line

**Does the requirements-based defense change the conclusion that Nice should move
toward best practices? Partly — it should be *narrowed*, not dropped.**

- **Keep the custom approach (do NOT "fix" toward native):** the pane pills (#2),
  the floating sidebar card (#5), the custom source list (#6), and the
  palette/material system (#7). These are required by — or are the correct modern
  expression of — Nice's real constraints (non-`NSWindow` multi-pane model, live
  ptys, non-system themes, live repaint). Adopting the native equivalents would
  cost the themes and/or break the pane/session model for benefits Nice's layout
  doesn't need. The window-move region (#3a) and the manual 52pt inset (#8 design)
  are likewise correct and should stay.

- **Push back hardest on the gap docs' headline recommendation:** migrating to
  `NSTitlebarAccessoryViewController` (#1). The fixed native title-bar height
  cannot host the load-bearing 52pt strip without clipping (the doc's own open
  uncertainty), and it re-introduces system-appearance coupling that fights the
  themes. It is the *least* compelling of the "go native" moves for this app.

- **Spend the best-practices effort exclusively on the window-chrome correctness
  cluster:** read `AppleActionOnDoubleClick` and branch (concession 1), add the
  four full-screen observers to `TrafficLightNudger` and recompute the band on
  full-screen (concessions 2–3), and fold the 52pt constant into one source of
  truth (concession 4). All four are small, isolated, and independent of every
  architectural decision above.

In one line: **Nice's *patterns* are right for what Nice is; its *window-chrome
correctness* is not. Defend the architecture, fix the chrome bugs, and leave the
`NSTitlebarAccessoryViewController` migration on the shelf.**
