# Sidebar Gap Analysis — Nice full-height sidebar vs. macOS best practices

Audits the **as-built** sidebar + top-bar overlap against
`docs/research/macos-full-height-sidebar-best-practices.md` (and cross-references
`custom-macos-toolbar-best-practices.md` / `toolbar-gap-analysis.md`). Analysis
only — no code changed.

All file:line references are to the worktree at
`/Users/nick/Projects/nice/.claude/worktrees/refactor-top-bar`.

---

## Executive summary

Nice implements the **floating, inset, rounded sidebar *card*** pattern — the
**same** modern macOS style used by Apple Music, Finder, and Xcode (and codified
in macOS 26 Tahoe's floating-glass-panel sidebar). This corrects an earlier
framing in this doc: Apple Music is **not** a flush full-height column; like
Nice, it floats an inset card with rounded corners and a drop shadow over the
window background. So Nice is **matching** the canonical reference, not diverging
from it. The card is inset 6pt on all sides (`AppShellView.swift:448-450`), has
rounded corners + a stroke + a drop shadow (`AppShellView.swift:434-447`), and
uses the correct sidebar material in the `.macOS` palette (`NSVisualEffectView`,
`.sidebar` material, `.behindWindow` blending — `SidebarBackground.swift:29-33`).
Because the floating-card pattern is *defined* by an inset card rather than a
flush surface that the title bar sits transparently over, several "flush
full-height" best-practice items in the research doc (e.g. `allowsFullHeightLayout`,
`NSTrackingSeparatorToolbarItem`, scroll-under) are **N/A by design** — they
belong to the flush pattern, which is a different (older) style, not a better one.
The top band is handled by a custom 52pt strip reserved *inside* the card
(`AppShellView.swift:389-390`).

Within this floating-card paradigm the overlap is handled deliberately and looks
mostly correct: the card reserves a 52pt traffic-light safe zone at its top
(`AppShellView.swift:389-390`), the first sidebar row therefore starts below the
traffic lights, and the toolbar to the right is a sibling column (not overlaid),
so there is **no sidebar/content seam** of the kind the doc warns about
(`AppShellView.swift:352-359`). The two real correctness risks the research doc
flags both apply: (1) **traffic lights are hand-positioned** via
`TrafficLightNudger` (`TrafficLightNudger.swift`), which re-applies only on
`didBecomeKey` + `didResize` and **not on full-screen transitions**
(`TrafficLightNudger.swift:89-109`) — the exact revert pitfall in the doc; and
(2) **there is no full-screen handling anywhere** for the custom band
(confirmed by search — no `willEnterFullScreen`/`didEnterFullScreen` observers in
`Sources/`), so band height, the 52pt safe zone, and traffic-light offsets are
not recomputed entering/exiting full screen. Divider-tracking is **N/A** (no
native toolbar split; the toolbar is a separate column, not overlaid on the
sidebar), and collapse reflow is correct-by-construction but **unanimated**
(`SidebarModel.swift:44-46`, `AppShellView.swift:340-347`).

Net: as a *floating-card* sidebar it matches the Apple Music / Finder / Xcode
reference and the seam/material/inset items pass or are N/A; the failures are the
**hand-managed-traffic-light + no-full-screen** correctness cluster, which is the
same cluster the toolbar gap analysis already flagged. The sidebar/overlap
*design* is correct; the gaps are all in window-chrome correctness.

---

## Current architecture (as-built)

**Window chrome.** Single `WindowGroup(id: "main")` with
`.windowStyle(.hiddenTitleBar)` + `.windowResizability(.contentSize)`
(`NiceApp.swift:100-101`). No `NavigationSplitView`, no `NSSplitViewController`,
no `HSplitView`, no `NSToolbar`, no `NSTrackingSeparatorToolbarItem` anywhere in
`Sources/` (confirmed by search). The shell root ignores the top safe area
entirely (`AppShellView.swift:174`, `.ignoresSafeArea(edges: .top)`).

**Two layout modes** are chosen by `appState.sidebar.sidebarCollapsed`
(`AppShellView.swift:340-347`):

- **Expanded** (`expandedShell`, `AppShellView.swift:351-360`): an
  `HStack(spacing: 0)` of `[ floatingSidebarCard(resizable: true) ,
  VStack { WindowToolbarView(); mainContent } ]`. So the sidebar is a **sibling
  column to the left of the toolbar**, not a surface the toolbar overlays. The
  card is a **floating inset card** (the Apple Music / Finder / Xcode style): inset
  6pt top/leading/bottom (`AppShellView.swift:448-450`) with rounded corners — it
  spans the window top-to-bottom but as an inset card, not a flush edge-to-edge
  column.
- **Collapsed** (`collapsedShell`, `AppShellView.swift:509-533`): no sidebar
  column. A small `collapsedCap` card (124×40pt, `AppShellView.swift:559`) sits
  in the upper-left to host the traffic lights + a restore button; the toolbar
  and content fill the full width.

**The sidebar card.** `floatingSidebarCard(...)` (`AppShellView.swift:376-455`)
wraps `VStack { WindowDragRegion().frame(height: 52) ...; SidebarView() }` in a
`SidebarBackground`. The 52pt top region is a reserved traffic-light safe zone
(comment `AppShellView.swift:382-388`) that doubles as a drag/zoom title-bar
surface (`WindowDragRegion`). A trailing `HStack` overlays mode-toggle icons +
the collapse button at the top-trailing of that 52pt strip
(`AppShellView.swift:391-429`). The card has rounded corners
(`AppShellView.swift:434`), a 0.5pt stroke (`AppShellView.swift:435-441`), a
trailing resize handle (`AppShellView.swift:442-446`, `462-496`), a drop shadow
(`AppShellView.swift:447`), and `.zIndex(1)` to keep its shadow above the
neighboring opaque chrome (`AppShellView.swift:451-454`).

**Sidebar material / background.** Owned upstream by `SidebarBackground`
(`SidebarBackground.swift`): `.nice` palette → flat `niceBg2`; `.macOS` →
`VisualEffectView(material: .sidebar, blendingMode: .behindWindow, state:
.active)` (`SidebarBackground.swift:29-33`, `VisualEffectView.swift:21-40`);
catppuccin → vibrancy + a 0.68-opacity tint (`SidebarBackground.swift:34-42`).
`SidebarView` itself paints **no** background so vibrancy shows through
(`SidebarView.swift:9-13`).

**Sidebar content.** `SidebarView` (`SidebarView.swift:19-186`) is the expanded
column only (collapsed is handled upstream as the cap, `SidebarView.swift:4-7`).
It shows a `tabList` (`ScrollView` of `ProjectGroup`s,
`SidebarView.swift:142-163`) or `FileBrowserView` depending on `sidebarMode`,
plus a footer (`SidebarView.swift:167-185`). The list has `.padding(.vertical,
10)` (`SidebarView.swift:149`) but **no top safe-area inset of its own** — the
52pt clearance comes from the sibling `WindowDragRegion` spacer above it in the
card's VStack (`AppShellView.swift:389-390, 430`).

**Traffic lights.** Hand-positioned by `TrafficLightNudger.nudge(window:, dx: 8,
dy: -10)` called from the `WindowAccessor` callback
(`AppShellView.swift:189`). It offsets the three `standardWindowButton`s
(`TrafficLightNudger.swift:112-129`) and re-applies on `didBecomeKey` +
`didResize` (`TrafficLightNudger.swift:89-109`), guarding to
`.fullSizeContentView` windows only (`TrafficLightNudger.swift:78`). The `dy:-10`
aligns their centers with the collapse icon at window-y≈26
(`AppShellView.swift:180-182`).

**Top-bar drag/zoom.** `WindowDragRegion` (`mouseDownCanMoveWindow = true`,
`WindowDragRegion.swift:56-58`) + the process-wide `TitleBarZoomMonitor` gated on
a hard-coded `yFromTop <= 52` (`WindowDragRegion.swift:87-88`), installed from
`AppShellView.swift:190`. (Covered in depth by `toolbar-gap-analysis.md`.)

**Collapse / toggle.** `SidebarModel.toggleSidebar()` flips a Bool
(`SidebarModel.swift:44-46`); bound to ⌘B (`KeyboardShortcuts.swift:192`), the
expanded card's collapse button (`AppShellView.swift:420-425`), and the
collapsed cap's expand button (`AppShellView.swift:550-555`). The shell switches
modes via a plain `if` with **no `withAnimation`/`.animation` on the mode swap**
(`AppShellView.swift:340-347`); the only animations are on the *peek* overlay
inside `collapsedShell` (`AppShellView.swift:523-532`).

**Window background fill.** `windowBackground` paints a full-width 52pt
`niceChrome` band + 1pt `niceLine` under it, then the terminal bg below
(`AppShellView.swift:589-612`), so the 6pt gap around the card top/leading
reveals the same chrome band the toolbar shows (comment
`AppShellView.swift:590-600`).

---

## Audit table

Checklist items are drawn from the research doc's "The overlap zone … how to do
it right" list plus the task's required coverage areas.

| Checklist item | Verdict | Evidence (file:line) | Notes |
|---|---|---|---|
| Content goes *under* the band (full-size content view, transparent title bar, split extends to top, `allowsFullHeightLayout`) | Partial / N/A | `NiceApp.swift:100`; `AppShellView.swift:174,389-390` | `.hiddenTitleBar` gives full-size content. Nice uses the **floating inset card** pattern (same as Apple Music / Finder / Xcode), so the *flush*-pattern machinery (`NSSplitView`/`allowsFullHeightLayout`) is N/A by design — the "extends to top" is a manual 52pt reserved spacer inside the card. Correct for the floating pattern, not a divergence. |
| Band shows the **sidebar material** (no opaque seam over the sidebar) | Pass | `SidebarBackground.swift:29-33`; `AppShellView.swift:389-390,430,352-359` | The 52pt top strip is *inside* the card over the sidebar material; the toolbar is a separate column to the right, so there is no opaque bar painted across the sidebar. Correct `.sidebar` material in `.macOS`. No seam by construction. |
| Sidebar background blends into the top band | Pass | `AppShellView.swift:377-432` | The card's `SidebarBackground` runs under the full card including the 52pt strip, so the top band over the sidebar is the same material. |
| Sidebar runs full height / overlaps the top band | Pass | `AppShellView.swift:448-450,389-390` | Card spans top-to-bottom of the window as a 6pt-inset rounded floating card — the Apple Music / Finder / Xcode style. It reserves the top 52pt for the traffic lights inside the card. Correct floating-pattern overlap. |
| Divide the toolbar at the sidebar divider (`NSTrackingSeparatorToolbarItem` / `.sidebarTrackingSeparator` or hand-rolled divider math) | N/A | `AppShellView.swift:352-359`; no `trackingSeparator` in `Sources/` | The toolbar is a **sibling column**, not overlaid on the sidebar, so there is no toolbar-over-sidebar boundary to track. The bar's left edge is glued to the card's trailing edge automatically by the `HStack` layout — no divider math needed. The doc's tracking-separator concern does not arise. |
| Top safe-area inset == band height so first row clears the traffic lights | Pass | `AppShellView.swift:389-390,430`; `SidebarView.swift:142-149` | The 52pt `WindowDragRegion` spacer sits above `SidebarView()` in the card VStack, so the first sidebar row starts at ~52pt and clears the (nudged) traffic lights. Inset is structural, not a `safeAreaInset`. Root `ignoresSafeArea(edges:.top)` (`:174`) is intentional since the inset is hand-built. |
| Content scrolls *under* the band (scroll-under shadow) | Fail / N/A | `SidebarView.swift:142-152` | The `tabList` ScrollView starts *below* the 52pt spacer, so content does **not** scroll under the band and there is no scroll-under shadow. For a floating card this is the expected look (Xcode doesn't scroll under either), so it's effectively N/A — but it is a hard "no" against the literal best-practice item. |
| Don't move the traffic lights (prefer system placement) | Fail | `TrafficLightNudger.swift:112-129`; `AppShellView.swift:189` | Buttons are hand-offset (`dx:8, dy:-10`). This is exactly the pattern the doc warns against; it requires re-applying on every relevant window event. |
| If repositioning, re-apply on **resize** and **full-screen** transitions | Partial | `TrafficLightNudger.swift:89-109` | Re-applies on `didBecomeKey` + `didResize` (good), but **no full-screen observers** (`willEnter/ExitFullScreen`). The doc explicitly calls out full-screen as a reset trigger; this is unhandled. |
| Re-home / reflow on collapse | Partial | `SidebarModel.swift:44-46`; `AppShellView.swift:340-347,542-574` | Collapse swaps to `collapsedShell` and the traffic lights re-home into the `collapsedCap` correctly-by-construction (cap reserves leading 82pt, `AppShellView.swift:549`). But the nudger's canonical origins are window-level, and the cap relies on the *same* nudge offset — works, but is implicit. No animation on the reflow (see below). |
| Collapse/expand is animated | Fail | `AppShellView.swift:340-347` | The mode switch is a bare `if`; no `withAnimation` on `toggleSidebar`, no `.animation(value: sidebarCollapsed)` on the shell. The sidebar appears/disappears instantly. Only the *peek* overlay animates (`:523-532`). |
| Sidebar toggle exists and is conventionally placed | Pass | `AppShellView.swift:420-425,550-555`; `KeyboardShortcuts.swift:192` | Collapse button at the card's top-trailing; expand button in the collapsed cap; ⌘B shortcut. Placement is reasonable (trailing rather than leading, by design comment `:382-388`). |
| Full-screen transition handling (band height change) | Fail | (no matches for `FullScreen` in `Sources/`) | No full-screen observers anywhere. Band height, 52pt safe zone, and traffic-light offsets are not recomputed in full screen. Behavior on entering full screen is unverified and likely wrong (buttons revert; band assumptions stale). |
| Accessibility / hit-testing in the overlap zone | Partial | `WindowDragRegion.swift:56-58,87-104`; `AppShellView.swift:401-429` | Drag region is behind interactive controls so pills/buttons keep their clicks; traffic lights are native NSButtons layered above and keep their own hit-testing. But the zoom monitor is a process-wide `leftMouseDown` hook gated on a magic `52` (`WindowDragRegion.swift:87`) duplicated from the bar height — fragile, and full-screen changes the band so the gate desyncs. No VoiceOver labels audited here (out of overlap scope). |
| Use `.listStyle(.sidebar)` for native source-list styling | Unknown / N/A | `SidebarView.swift:142-163` | The sidebar is a hand-built `ScrollView`+`VStack` of custom `ProjectGroup`/`TabRow`, not a `List`, so `.listStyle(.sidebar)` doesn't apply. Custom styling is intentional. Not a defect for this design. |
| Don't over-apply `ignoresSafeArea` | Pass | `AppShellView.swift:174,194` | `ignoresSafeArea(edges:.top)` is scoped to the shell root, paired with the manual 52pt inset, which is the correct trade for a hand-built band. No evidence of content clipped under the buttons. |

**Tally (excluding pure N/A):** Pass = 7, Partial = 4, Fail = 4, N/A = 2,
Unknown = 1. (The "sidebar runs full height / overlaps the top band" item moved
Partial → Pass after correcting the pattern framing — see note below.) The four
Fails cluster on **traffic-light management + full-screen + collapse animation +
scroll-under**; the first three are the doc's named correctness pitfalls, the last
is cosmetic-and-arguably-N/A for a floating card.

---

## Detailed findings (Partial / Fail / Unknown)

### F1 — Hand-managed traffic lights with no full-screen re-apply (Fail, correctness)
`TrafficLightNudger` reaches into the `NSWindow` and offsets the standard buttons
(`TrafficLightNudger.swift:112-129`), re-applying on `didBecomeKeyNotification`
and `didResizeNotification` only (`TrafficLightNudger.swift:89-109`). The
research doc is explicit (§3, overlap-zone #5, §8): macOS **resets custom button
positions on full-screen enter/exit** as well as resize, and the fix is to
observe the full-screen notifications and re-apply. Those observers are absent
(confirmed: no `FullScreen` references in `Sources/`). Consequence: entering or
exiting full screen will revert the buttons to their default origin until the
next key/resize event nudges them back — a visible jump, and possibly buttons
landing under/over the card edge. This is the single highest-value gap and
matches the toolbar gap analysis's "full-screen behavior … not handled anywhere
I found" uncertainty (`toolbar-gap-analysis.md:160-161,326-328`).

### F2 — No full-screen handling for the custom band at all (Fail, correctness)
Beyond the traffic lights, nothing recomputes the band in full screen: the 52pt
constant is hard-coded in three independent places — the card spacer
(`AppShellView.swift:390`), the `windowBackground` band
(`AppShellView.swift:608`), and the zoom monitor gate
(`WindowDragRegion.swift:88`) — with no shared source of truth and no full-screen
branch. In native full screen the title-bar band auto-hides/changes height; a
hand-built band that assumes a constant 52pt will not track that. Behavior is
**unverified in code** and should be tested manually (the doc flags this as the
same event you must hook for repositioning, §8).

### F3 — Collapse/expand is not animated (Fail, cosmetic)
`toggleSidebar()` flips a Bool (`SidebarModel.swift:44-46`) and the shell chooses
`collapsedShell` vs `expandedShell` with a plain `if`
(`AppShellView.swift:340-347`) carrying no animation. Native
`toggleSidebar(_:)` animates the collapse; here the sidebar snaps in/out. The
peek overlay *does* animate (`AppShellView.swift:523-532`), which makes the
unanimated primary toggle more noticeable. Low correctness impact, but it is the
doc's "collapse reflow" item and a polish gap vs. the Xcode/Mail reference. (Also
note: no Reduce-Motion gating exists, but with no animation there is nothing to
gate — `accessibilityReduceMotion` is absent from `Sources/`.)

### F4 — Sidebar content does not scroll under the band (Fail vs. literal item; N/A for the chosen design)
The `tabList` ScrollView begins below the 52pt reserved spacer
(`SidebarView.swift:142-152`, `AppShellView.swift:389-390`), so there is no
scroll-under and no scroll-under shadow (research doc §6). For the **flush
full-height** pattern this is a defect; for Nice's **floating inset card**
(Xcode-style, where the list also starts below the header) it is the intended
look. Recorded as Fail-against-checklist but effectively a deliberate
design choice, not a bug.

### P1 — Floating card is the correct modern pattern (not a divergence)
**Correction to an earlier framing in this doc.** The floating, 6pt-inset rounded
card (`AppShellView.swift:434-450`) is **the** modern macOS sidebar pattern, used
by Apple Music, Finder, Xcode, and modern Mail, and formalized as the default in
macOS 26 Tahoe. Apple Music — the developer's reference example — is itself a
*floating* card (visible rounded corners + drop shadow over the wallpaper), not a
flush full-height column. So Nice matches the reference; it does not diverge from
it.

The practical consequence: the research doc's *flush*-pattern items
(`allowsFullHeightLayout`, `NSTrackingSeparatorToolbarItem` divider-tracking,
scroll-under) are **N/A by design** — they're the machinery of the older flush
style, not requirements the floating style fails to meet. The floating pattern
deliberately sidesteps those problems (no seam, no tracking separator needed).
Read the rest of this audit with that in mind: the sidebar/overlap **design** is
on-pattern; every real gap below is in window-chrome **correctness**
(traffic-light reposition, full-screen, animation), independent of the pattern
choice.

### P2 — Collapse reflow relies on implicit traffic-light geometry (Partial)
On collapse, the lights must re-home over the `collapsedCap`'s reserved 82pt
(`AppShellView.swift:549`). This works because the nudger applies the same window-
level offset regardless of mode, and the cap is sized/positioned to match
(`AppShellView.swift:540-559`). But there is no explicit coupling — the cap's
82pt and the nudger's `dx:8` are independently tuned magic numbers
(`AppShellView.swift:189,549`), so a change to one silently breaks alignment.
Functionally correct today; structurally fragile.

### P3 — Zoom monitor magic `52` desyncs from the band (Partial, overlap hit-testing)
`TitleBarZoomMonitor` gates double-click-zoom on `yFromTop <= 52`
(`WindowDragRegion.swift:87-88`), duplicating the bar height with no shared
constant. In full screen (where the band changes) or if the bar height ever
changes, the zoom region desyncs from the visible chrome. Same fragility called
out in `toolbar-gap-analysis.md:178-199`.

### U1 — `.listStyle(.sidebar)` not used (Unknown / N/A)
The sidebar is a custom `ScrollView`+`VStack` (`SidebarView.swift:142-163`), not
a SwiftUI `List`, so the native source-list selection/styling from
`.listStyle(.sidebar)` is intentionally not in play. Whether the custom styling
fully matches the native source-list appearance (selection highlight insets,
section spacing) was not evaluated against a native baseline — flagged Unknown
rather than asserting Pass/Fail.

---

## Prioritized fixes

### High — correctness
1. **Re-apply traffic-light offsets on full-screen transitions** (F1). Add
   `NSWindow.willEnterFullScreenNotification` / `didEnterFullScreenNotification` /
   `willExitFullScreenNotification` / `didExitFullScreenNotification` observers in
   `TrafficLightNudger.nudge` (`TrafficLightNudger.swift:89-109`) alongside the
   existing key/resize ones, calling `applyOffset`. **Quick win**, isolated to one
   file, directly closes the doc's named pitfall.
2. **Verify and handle full-screen band behavior** (F2). Manually test
   enter/exit full screen; if the 52pt band assumptions break, recompute the band
   height / safe zone there. Likely **structural** (the 52pt is hard-coded in
   three places — fold into one shared constant first). The doc and the toolbar
   gap analysis both leave this explicitly unverified, so it needs a real-app pass
   before deciding scope.

### Medium — robustness
3. **Single source of truth for the 52pt band height** (F2/P3). Replace the
   magic `52` in `AppShellView.swift:390`, `:608`, and `WindowDragRegion.swift:88`
   with one shared constant so the band, safe zone, and zoom region can't desync —
   prerequisite for any full-screen band fix. **Quick win.**
4. **Make collapse/expand explicit and coupled** (P2). Tie the collapsed-cap's
   leading reserve and the nudge offset to one geometry source so they can't drift
   (`AppShellView.swift:189,549`). Medium effort.

### Low — cosmetic / polish
5. **Animate the sidebar collapse/expand** (F3). Wrap `toggleSidebar()` call
   sites in `withAnimation` or add `.animation(value: sidebar.sidebarCollapsed)`
   to the shell (`AppShellView.swift:340-347`), gated on
   `accessibilityReduceMotion`. Quick win, brings it in line with native
   `toggleSidebar` feel.
6. **Scroll-under shadow** (F4) — only if moving toward the flush full-height
   look. For the current floating-card design, leave as-is (intentional).

**Cosmetic vs. correctness split:** the seam/material/inset items — the things
the doc worries most about visually — are **already correct (Pass)**. The real
debt is the **correctness** cluster around hand-managed traffic lights and the
total absence of full-screen handling (High #1–#2), which is the same conclusion
the toolbar gap analysis reached from the toolbar side. The single clearest
quick win is adding the full-screen observers to `TrafficLightNudger`.

### Uncertainties
- **Full-screen behavior** is unverified from code (no observers exist); needs a
  manual pass to confirm the predicted button-revert + stale-band behavior.
- **Custom-list vs. native source-list fidelity** (U1) was not benchmarked
  against a native `.listStyle(.sidebar)`.
- **Tahoe / Liquid Glass** adoption (doc §5, §7) is entirely absent; not assessed
  as a gap because the app deliberately runs a custom palette/material system.
