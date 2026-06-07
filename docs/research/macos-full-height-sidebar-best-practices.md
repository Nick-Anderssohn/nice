# macOS full-height sidebar: how the pattern is really built

Research brief for the **Nice** app (SwiftUI + AppKit interop, `.hiddenTitleBar`
window, custom 52pt top bar). Focus: the "Apple Music" pattern where the
sidebar runs the full height of the window, the traffic lights sit on top of the
sidebar, and the content toolbar starts to the right of the sidebar — with
extra attention to **the overlap zone (sidebar drawn over the toolbar band)**
and to **doing this with a custom, non-native top bar**.

> Terminology note. Apple does not call this a "title-bar sidebar." The
> idiomatic name is a **full-height sidebar** (WWDC20), produced by a
> **full-size content view** window whose **split view dividers reach the top of
> the window**. The toolbar is split at the sidebar's trailing edge by an
> **`NSTrackingSeparatorToolbarItem`** (the "sidebar tracking separator"). Lead
> with those terms.

Uncertainty is flagged inline. Every nontrivial claim is cited; URLs are in
**Sources** at the end.

---

## Executive summary

- The pattern is **first-class and largely automatic in native AppKit/SwiftUI**.
  An `NSSplitViewController` whose sidebar item has `allowsFullHeightLayout =
  true`, hosted in a window with `.fullSizeContentView`, gives you a sidebar
  that extends up into the title-bar band; the traffic lights end up overlaid on
  it for free. WWDC20: "Your app may get this for free just by building on Big
  Sur" [WWDC20-10104].
- The toolbar is divided at the sidebar's trailing edge by an
  **`NSTrackingSeparatorToolbarItem`**, which tracks a specific split-view
  divider so sidebar-side toolbar items stay over the sidebar and content-side
  items stay over the detail pane. AppKit ships a standard identifier
  (`.sidebarTrackingSeparator` / `NSToolbarSidebarTrackingSeparatorItemIdentifier`)
  that "discovers the full-height sidebar automatically" [Apple-TrackingSep,
  WWDC20-10104, Apple-SidebarTrackingSep].
- The **unified toolbar style** is the prerequisite that makes the toolbar live
  in the same band as the full-height sidebar and lets the title-bar separator
  align to the split divider [WWDC20-10104, Giacomi].
- **Traffic-light placement is managed by the system** when you use the native
  stack. You do *not* position the buttons; the window does. Manually moving
  `standardWindowButton(...)` is the classic source of bugs — the buttons
  **revert on resize and on full-screen transitions** unless you re-apply on
  every relevant window event [ClyappTrafficLights(cached), Tauri-4789].
- **SwiftUI** produces the same look via `NavigationSplitView` (sidebar +
  detail) plus `.toolbar` items and `.windowToolbarStyle(.unified...)`; the
  sidebar toggle and tracking separator are injected for you. The known wart is
  **incorrect top safe-area propagation** from the toolbar into the detail
  column — an Apple-confirmed bug (rdar://122947424) [Apple-Forum-746611].
- **macOS 26 "Tahoe" / Liquid Glass** restyles the sidebar as a *floating,
  inset, translucent glass* panel and introduces `backgroundExtensionEffect` so
  content can extend under/behind it without clipping; window corners grow to
  wrap concentrically around the glass toolbar [WWDC25-323, DevTo-LiquidGlass,
  Lapcat-Corners].
- **The cost of a custom top bar** (`.hiddenTitleBar` + your own SwiftUI bar) is
  that you give up exactly the conveniences that make this pattern correct for
  free: divider-tracking toolbar separation, automatic traffic-light
  repositioning, and toolbar-driven safe-area insets. You must reimplement the
  overlap zone by hand and keep it correct across resize, collapse, and
  full-screen.

---

## 1. The idiomatic native implementation

**AppKit (the ground truth).** The full-height sidebar is an
`NSSplitViewController`-based layout in a window that lets content sit under the
title bar:

- Window: include **`.fullSizeContentView`** in `styleMask` so content (the
  split view) extends into the title-bar region [Bancarel-Medium,
  NSWindowStyles]. WWDC20 lists this plus "`NSView` safe area APIs to manage
  layout" as the requirement [WWDC20-10104].
- Split view: build the sidebar item with the sidebar behavior and set
  **`allowsFullHeightLayout = true`** so the sidebar column draws up behind the
  title bar rather than starting below it [Bancarel-Medium, WWDC20-10104].
- Toolbar: use the **unified** style and add the sidebar tracking separator
  (next section).

WWDC20 frames this as mostly free: "Perhaps one of the most eye-catching updates
is the beautiful new full-height sidebar, seen here in Mail. Your app may get
this for free just by building on Big Sur, and that's because it takes advantage
of existing APIs" [WWDC20-10104].

**SwiftUI.** Use `NavigationSplitView { sidebar } detail: { ... }`. On macOS
this is backed by an `NSSplitViewController` under the hood, so you inherit the
full-height behavior; SwiftUI injects the sidebar toggle and the tracking
separator for you. Column behavior is controlled with:

- `NavigationSplitViewVisibility` (`.automatic` / `.all` / `.doubleColumn` /
  `.detailOnly`) bound via `NavigationSplitView(columnVisibility:)`
  [UseYourLoaf-SplitView].
- `.navigationSplitViewColumnWidth(min:ideal:max:)` for column sizing (widths
  are *preferred*, not guaranteed) [UseYourLoaf-SplitView].
- `.navigationSplitViewStyle(.balanced / .prominentDetail / .automatic)`
  [UseYourLoaf-SplitView].
- Window toolbar style via `.windowToolbarStyle(.unified(showsTitle:))`
  [Apple-Forum-746611].

One developer notes the SwiftUI app-lifecycle path "works seamlessly … without
extra configuration," whereas hosting via `NSHostingController` in an AppKit
lifecycle requires you to set `.fullSizeContentView` and the tracking separator
yourself or "the sidebar behaves inconsistently" / "jumps unexpectedly" during
toggling [Bancarel-Medium]. (Single source; treat as a practitioner report.)

---

## 2. Toolbar / sidebar separation mechanics

The visual division of the toolbar at the sidebar's trailing edge is produced by
**`NSTrackingSeparatorToolbarItem`** — "a toolbar separator that aligns with the
vertical split view in the same window" [Apple-TrackingSep].

- You construct it bound to a split view and a divider index:
  ```swift
  let trackingItem = NSTrackingSeparatorToolbarItem(
      itemIdentifier: identifier,
      splitView: splitView,
      dividerIndex: 1)
  ```
  "NSTrackingSeparatorToolbarItem items are configured to track a specific
  divider, and the toolbar does the rest" [WWDC20-10104, Apple-TrackingSep-Init].
- Pass the split view **at creation time, before the window is visible**
  [Giacomi].
- AppKit also provides a **standard identifier** so you don't have to wire the
  split view manually in the common case:
  **`NSToolbarItem.Identifier.sidebarTrackingSeparator`** (ObjC
  `NSToolbarSidebarTrackingSeparatorItemIdentifier`). Include it in the
  toolbar's default item identifiers and "AppKit uses `sidebarTrackingSeparator`
  to know where to place the separator inside the title bar"; the item
  "discover[s] the full-height sidebar automatically" [Apple-SidebarTrackingSep,
  WWDC20-10104, Bancarel-Medium].
- WWDC20 describes the intended result directly: "Notice how there are items in
  the toolbar that align with sections of content in the window. The dividers of
  the split view reach up to the top of the window, creating these beautiful
  full-height sections" [WWDC20-10104]. During toolbar customization "these
  sections are not removable" [WWDC20-10104].

**Unified toolbar style** is the carrier for all of this: "the new standard look
of toolbars … the new inline title … placed at the leading edge of the toolbar,
next to the sidebar" [WWDC20-10104]. Setup recipe from a focused tutorial: set
the toolbar style to unified, `titlebarAppearsTransparent = false`, and add the
tracking separator [Giacomi].

**The title-bar separator line.** With the unified style the toolbar no longer
draws a permanent bottom divider; instead "a shadow will automatically appear
between the toolbar and scrolled content to create a visual pocket where the
content trails off" — i.e. a scroll-under separator. This is tunable per split
item / per window via **`titlebarSeparatorStyle`** [WWDC20-10104]. The tracking
separator is what keeps that line aligned to the split divider across the band.

---

## 3. Traffic-light / window-control handling in the overlap region

**With the native stack, you do not place the traffic lights.** Because the
window uses `.fullSizeContentView` and the sidebar draws full height, the system
lays the close/minimize/zoom buttons in the title-bar band, which now sits over
the top of the sidebar — that is what produces the Apple Music look. The buttons
remain system-managed.

Things to know:

- **Disabling title visibility vertically centers the traffic lights** in the
  available title-bar height, and accessory `NSView`s anchored to the traffic
  lights move with them [WebSearch-NSWindow]. This matters if your top band is a
  non-standard height.
- **When the sidebar collapses, the traffic lights re-home over the content.**
  In the native stack this is automatic: the buttons stay pinned to the window's
  top-leading corner, so as the sidebar slides away the content simply moves
  under them. (No first-party doc states this in one line; it is the observed
  behavior of Music/Mail and the consequence of system-managed button layout.
  Flagged as inference, not a cited Apple statement.)
- **Top inset / safe area.** The window exposes the title-bar band as a top
  **safe-area inset**; first-party guidance is to "use `NSView` safe area APIs
  to manage layout" so sidebar content is inset below the buttons rather than
  clipped by them [WWDC20-10104].

**What goes wrong if you manage the buttons yourself.** The moment you call
`window.standardWindowButton(.closeButton)?.setFrameOrigin(...)` (or hide/show
them), you own a fragile invariant: **macOS resets custom button positions on
resize and on full-screen enter/exit.** Practitioner write-ups all converge on
the same fix — observe the relevant `NSWindow` events
(`didResize`/`didEndLiveResize`, full-screen notifications) and **re-apply the
position every time**:

- "Traffic light buttons change their position back when you resize the window …
  you need to monitor the NSWindow resize event and set the position back"
  [ClyappTrafficLights(cached)].
- Cross-platform shells hit the identical issue: "When windows exit fullscreen
  mode … detect the transition and call `repositionTrafficLights()` to restore
  the custom position" [Tauri-RoundedCorners, Tauri-4789].

So: **prefer letting the system place the buttons.** Only reposition if a custom
band height demands it, and then handle every resize and full-screen transition.

---

## 4. Sidebar toggle / collapse behavior

- **Native AppKit** ships `NSSplitViewController.toggleSidebar(_:)`, which
  collapses/expands the sidebar `NSSplitViewItem` with the standard animation.
  `NSSplitViewItem` exposes `canCollapse` / `isCollapsed` and collapse behavior.
- **SwiftUI** has no dedicated toggle API historically, so two routes exist:
  1. `SidebarCommands()` in `.commands` adds a "Toggle Sidebar" menu item
     (⌥⌘S) automatically [Apple-Forum-651807, Sarunw-Toggle].
  2. Drive AppKit through the responder chain:
     ```swift
     NSApp.keyWindow?.firstResponder?.tryToPerform(
         #selector(NSSplitViewController.toggleSidebar(_:)), with: nil)
     ```
     This "relies on the assumption that the sidebar is backed by
     `NSSplitViewController`, which might change" and caused responder-chain
     memory leaks from scroll views in early betas — use with care
     [Apple-Forum-651807, Sarunw-Toggle].
  3. Modern, preferred: bind `NavigationSplitView(columnVisibility:)` to
     `NavigationSplitViewVisibility` state [Sarunw-Toggle, UseYourLoaf-SplitView].
- **Toolbar item placement.** The sidebar toggle is a toolbar item that, in
  unified style, sits at the leading edge over the sidebar. In SwiftUI it is
  injected automatically; you can remove it with **`.toolbar(removing:
  .sidebarToggle)`** and re-add your own [WebSearch-Toggle].
- **The chevron / back control** (as in the reference image) is conventionally a
  leading toolbar item placed *before* the tracking separator, so it lives in
  the sidebar-side region; navigation/back chevrons for the detail pane go after
  the separator on the content side.
- **Toolbar reflow on collapse.** With the tracking separator, when the sidebar
  collapses the separator's tracked divider disappears and the system reflows
  the toolbar so the formerly-sidebar items move left and the traffic lights
  re-home over the content. This is automatic *only* with the native toolbar +
  tracking separator.

---

## 5. Sidebar visual style

- **List style.** Use SwiftUI `.listStyle(.sidebar)` (AppKit: an
  `NSOutlineView`/`NSTableView` in "Source List" mode) for the standard sidebar
  selection highlight and section styling [Mackuba-DarkSide].
- **Material / vibrancy.** The sidebar's translucent background is an
  **`NSVisualEffectView`** with the **`.sidebar` material**; vibrancy is "a
  subtle blending of foreground and background colors … blurring … to improve
  legibility," and `NSVisualEffectBlendingMode` distinguishes
  **behind-window** (desktop shows through) from **within-window** blending
  [Apple-NSVisualEffectView, Mackuba-DarkSide, Apple-HIG-Materials]. The
  classic sidebar uses behind-window blending so the desktop/wallpaper tints it.
- **Big Sur restyle (macOS 11).** Sidebars gained colorful, accent-tinted icons,
  customizable per item via
  `outlineView(_:tintConfigurationForItem:)` returning an `NSTintConfiguration`
  (`.default` / `.monochrome` / `.preferredColor` / `.fixedColor`)
  [WWDC20-10104]. Big Sur also added more spacing and "a clearer sense of
  structure" [WebSearch-Materials].
- **macOS 26 Tahoe / Liquid Glass.** The sidebar becomes a **floating, inset,
  translucent glass** panel that "floats above your content," with content
  refracting behind it; concentricity means the window's corner radius grows to
  "wrap concentrically around the glass toolbar elements." Toolbars, sidebar,
  menu bar, and Dock "receive Liquid Glass automatically" [WWDC25-323,
  DevTo-LiquidGlass, Lapcat-Corners, MacStories-Tahoe]. (Note: critics report
  reduced consistency / legibility tradeoffs — design context, not a bug
  [Cloudship-Tahoe, MacObserver-26].)
- **Blending with the title-bar region in the overlap zone.** Because the
  sidebar material runs full height and the title bar is transparent over it,
  the overlap band shows the *same* sidebar material — there is no seam. That
  seamlessness is the whole point of the pattern and is why a custom opaque top
  bar over the sidebar tends to look wrong (see §"Implications").

---

## 6. Top safe-area / content inset in the overlap zone (extra-focus)

This is the subtle part: the *top* of the sidebar overlaps the title-bar band,
so the first sidebar item must not hide under the traffic lights / toolbar.

- **The mechanism is the top safe-area inset.** A full-size-content-view window
  reports the title-bar band as a top safe-area inset; sidebar/detail content
  should respect it (`NSView` safe-area APIs in AppKit;
  `safeAreaInsets`/`.safeAreaPadding` in SwiftUI) so the first row starts below
  the buttons [WWDC20-10104, SwiftWithMajid-SafeArea].
- **Scroll-under behavior.** Sidebar content scrolls *beneath* the title-bar
  band; the system's automatic toolbar/title-bar **shadow appears only once
  content scrolls under it**, creating the "visual pocket"
  [WWDC20-10104]. With the `.sidebar` material this reads as content fading
  under translucent glass.
- **The SwiftUI safe-area bug (important).** On macOS, `NavigationSplitView` +
  `.toolbar` has been observed to **double-count the toolbar height** as safe
  area in the detail column, leaving mysterious vertical gaps equal to the
  toolbar height. An Apple Frameworks Engineer confirmed it: "this appears to be
  a bug … proportional to some safe area being propagated incorrectly"
  (rdar://122947424) [Apple-Forum-746611]. Workarounds reported:
  `.ignoresSafeArea(.all)` on the affected view, wrapping in a `VStack(spacing:
  0)` that fills, or subclassing `NSHostingView` to ignore the toolbar safe area
  — each with caveats (you can re-clip content under the buttons if you over-
  apply `ignoresSafeArea`) [Apple-Forum-746611, Apple-Forum-746611-followups].
  Treat `ignoresSafeArea` as a scalpel, not a default.
- **Tahoe.** `backgroundExtensionEffect` is the modern answer for hero/banner
  content that should bleed under the floating sidebar: "views can extend
  outside the safe area, without clipping their content" — the artwork is
  mirrored and blurred outside the safe area while keeping its content visible
  [WWDC25-323].

**Failure modes if you get the inset wrong:** first sidebar row sits under the
traffic lights (unclickable / hidden); or, over-correcting, a dead gap the
height of the toolbar at the top of the detail pane; or content that never
scrolls under the band, so the scroll-under shadow never appears and the overlap
looks flat/seamed.

---

## 7. Doing it with a CUSTOM top bar (not a native NSToolbar) — Nice's situation

Nice uses `.hiddenTitleBar` (transparent, full-size content) and draws its own
52pt SwiftUI top bar (pane pills, +, overflow) instead of an `NSToolbar`. The
window-styling techniques are well documented:

```swift
window.titlebarAppearsTransparent = true
window.styleMask.insert(.fullSizeContentView)
window.titleVisibility = .hidden
```
[NSWindowStyles]. But this trades away the native conveniences. **What you lose
and must reimplement:**

| Native convenience | Provided by | What you must do with a custom bar |
| --- | --- | --- |
| Toolbar split at sidebar trailing edge | `NSTrackingSeparatorToolbarItem` / `.sidebarTrackingSeparator` [Apple-TrackingSep, WWDC20-10104] | Manually align your bar's "sidebar region vs content region" boundary to the live split divider X, and update it as the divider moves/collapses |
| Title-bar separator aligned to divider, scroll-under shadow | unified toolbar + `titlebarSeparatorStyle` [WWDC20-10104] | Draw (or omit) your own separators; detect scroll-under yourself if you want the shadow |
| Traffic-light placement & re-home on collapse | system-managed full-size-content window | Either let the system place them (don't touch the buttons) **or** reposition + re-apply on every resize and full-screen transition [ClyappTrafficLights(cached), Tauri-4789] |
| Top safe-area inset for the band | toolbar contributes safe area | Set/own the top inset so the first sidebar item and content clear the buttons; with a 52pt bar your inset is 52pt, not the system default |
| Sidebar toggle item & reflow | `.sidebarToggle` toolbar item [WebSearch-Toggle] | Build your own toggle and animate your bar's region split to match |
| Version restyles (Big Sur icons, Tahoe glass) | automatic [WWDC20-10104, WWDC25-323] | You opt out of automatic Liquid Glass for the bar; you must restyle by hand |

**Key pitfalls specific to a custom bar over a full-height sidebar:**

1. **Keep the traffic lights system-placed if at all possible.** The single
   biggest source of breakage in custom-titlebar apps is hand-positioned traffic
   lights that snap back on resize/full-screen [ClyappTrafficLights(cached),
   Tauri-4789]. If your 52pt bar's left region simply *leaves room* for the
   system buttons (a leading inset equal to the buttons' width + margins) and
   you never move the buttons, you avoid the whole class of bugs.
2. **The overlap must use the sidebar material, not an opaque bar.** Natively the
   top band over the sidebar shows the sidebar's own material seamlessly (§5). A
   custom opaque 52pt bar painted across the full width will visibly seam at the
   sidebar/content boundary and break the Apple Music illusion. Either (a) let
   the sidebar's `NSVisualEffectView` show through the top band on the sidebar
   side and only paint your bar on the content side, or (b) match the bar's
   material to `.sidebar` over the sidebar region.
3. **You own the divider-tracking math.** Without the tracking separator,
   nothing keeps "where the bar's content region begins" glued to the live split
   divider. You must read the divider position and update on drag, on collapse,
   and on window resize, or the pills/`+`/overflow will drift relative to the
   sidebar edge.
4. **Collapse reflow is manual.** When the sidebar collapses, natively the
   traffic lights re-home over content and toolbar items reflow. With a custom
   bar you must animate your bar's left inset back down (and, if you positioned
   the buttons, move them too) so the content region reclaims the freed space.
5. **Safe-area inset is yours to set.** SwiftUI's automatic toolbar safe area
   does not exist for a hand-drawn bar; set the top content inset to your bar
   height (52pt) explicitly, and beware the inverse of the rdar bug — *under*-
   insetting clips the first row under the buttons.

There is no clean first-party API to "give me the native toolbar division and
traffic-light management while drawing my own bar." The honest tradeoff: a
custom bar buys you full visual control at the cost of reimplementing the
overlap correctness yourself, per macOS version.

---

## 8. Accessibility & correctness pitfalls in the overlap region

- **Hit-testing under the traffic lights.** Anything you draw in the band over
  the buttons must not steal clicks from close/minimize/zoom, and conversely the
  buttons must not cover an interactive sidebar control. With system-placed
  buttons this is handled; with a custom bar you must ensure your bar's leading
  region is non-interactive (or correctly z-ordered/hit-test-transparent) over
  the button area. (General correctness consequence of custom button/bar
  layout; see the repositioning-bug reports for how brittle manual layout gets
  [ClyappTrafficLights(cached), Tauri-4789].)
- **Full-screen transitions change the band height.** Entering full screen
  removes/!changes the title-bar band; custom button positions are reset on the
  transition [Tauri-4789]. A custom top bar must recompute its top inset and the
  traffic-light allowance when the band height changes, or content jumps /
  overlaps. This is the same event you must hook for repositioning (§3, §7).
- **Focus order / responder chain.** The AppKit `toggleSidebar` responder-chain
  trick can leak and depends on `NSSplitViewController` backing; prefer the
  `NavigationSplitViewVisibility` binding for predictable focus/behavior
  [Apple-Forum-651807, Sarunw-Toggle].
- **Don't over-apply `ignoresSafeArea`.** Using it to fix the rdar gap (§6) can
  re-introduce content hidden under the traffic lights if applied to the wrong
  view; scope it tightly [Apple-Forum-746611].
- **VoiceOver / labeling.** A custom bar must supply accessibility labels for
  its controls that the native toolbar would otherwise provide; the pills, `+`,
  and overflow each need labels and the sidebar toggle needs the conventional
  "Toggle Sidebar" semantics. (General HIG correctness; no overlap-specific
  first-party cite.)

---

## The overlap zone (sidebar over toolbar): how to do it right

Consolidated checklist for the band where the sidebar sits under the title bar.

1. **Make content go under the band, not start below it.** Window must be
   `.fullSizeContentView` + transparent title bar; the split view extends to the
   top; the sidebar item has `allowsFullHeightLayout = true` (native) so the
   sidebar paints behind the buttons [WWDC20-10104, Bancarel-Medium].
2. **Let the band show the sidebar material.** The overlap reads as one
   continuous translucent surface because the title bar is transparent over the
   sidebar's `.sidebar`-material `NSVisualEffectView` — no opaque seam
   [Apple-NSVisualEffectView, Mackuba-DarkSide, §5].
3. **Divide the toolbar at the divider.** Use `NSTrackingSeparatorToolbarItem` /
   `.sidebarTrackingSeparator` so sidebar-side and content-side items split
   exactly at the divider and the separator line aligns to it
   [Apple-TrackingSep, WWDC20-10104]. With a custom bar, replicate this with
   live divider-position math (§7.3).
4. **Inset the top by the band height as safe area.** First sidebar row and
   detail content respect the top safe-area inset so nothing hides under the
   traffic lights; let content scroll *under* the band so the scroll-under
   shadow appears [WWDC20-10104, §6]. Watch the SwiftUI double-inset rdar bug
   [Apple-Forum-746611].
5. **Don't move the traffic lights.** Let the system place them over the
   sidebar; only reposition if forced, and then re-apply on every resize and
   full-screen transition [ClyappTrafficLights(cached), Tauri-4789].
6. **Re-home on collapse.** When the sidebar collapses, the buttons move over
   content and the toolbar/bar regions reflow; native does this automatically,
   custom bars must animate it (§4, §7.4).
7. **Tahoe:** if adopting Liquid Glass, the sidebar floats inset and content can
   extend behind it via `backgroundExtensionEffect`; corners go concentric
   [WWDC25-323, DevTo-LiquidGlass].

---

## Implications for a custom (non-native) top bar — the Nice project

Tying the above to Nice's `.hiddenTitleBar` + custom 52pt SwiftUI bar:

- **The pattern is achievable with a custom bar, but every overlap convenience
  is now manual.** You already have the hardest precondition (full-size content
  window). What native gives free and you must own: divider-aligned region split
  in the bar, traffic-light allowance/re-home, top safe-area inset of 52pt, and
  per-version restyling (you've opted out of automatic Liquid Glass for the bar).
- **Lowest-risk traffic-light strategy:** keep them system-placed. Reserve a
  leading inset in the bar equal to the buttons' width + standard margins and
  never call `standardWindowButton(...).setFrameOrigin`. This sidesteps the
  resize/full-screen revert bug entirely [ClyappTrafficLights(cached),
  Tauri-4789].
- **Audit the seam.** Confirm the 52pt bar does not paint an opaque fill across
  the sidebar region; over the sidebar it should be the sidebar material (or
  transparent so the sidebar's `NSVisualEffectView` shows through). A visible
  vertical seam at the sidebar/content boundary in the top band is the tell-tale
  that the overlap is wrong (§5, §"overlap zone" #2).
- **Audit divider tracking.** Verify the boundary between the bar's sidebar-side
  content and its content-side content stays glued to the live split divider on
  drag, collapse, and resize — natively this is the tracking separator's job
  (§7.3).
- **Audit the top inset and full-screen.** Verify the first sidebar item clears
  the traffic lights, that there's no double-gap in the detail pane, and that
  entering/exiting full screen recomputes the band/inset (the band height changes
  and native button positions reset) [Apple-Forum-746611, Tauri-4789].
- **Audit collapse reflow.** When the sidebar collapses, confirm the bar's
  leading inset animates back down and content reclaims the space, mirroring how
  Music/Mail re-home the buttons over content (§4).

Where this is uncertain: the exact "traffic lights re-home over content on
collapse" behavior is documented only as observed Music/Mail behavior and the
logical consequence of system-managed buttons, not a single cited Apple
sentence; and the SwiftUI safe-area double-count is an open/confirmed bug whose
fix may shift across OS versions [Apple-Forum-746611]. Verify against the macOS
version Nice targets.

---

## Sources

- [WWDC20-10104] Adopt the new look of macOS — WWDC20 session 10104:
  https://developer.apple.com/videos/play/wwdc2020/10104/
- [Apple-TrackingSep] NSTrackingSeparatorToolbarItem — Apple Developer Docs:
  https://developer.apple.com/documentation/appkit/nstrackingseparatortoolbaritem
- [Apple-TrackingSep-Init] init(identifier:splitView:dividerIndex:) — Apple Docs:
  https://developer.apple.com/documentation/appkit/nstrackingseparatortoolbaritem/init(identifier:splitview:dividerindex:)?language=objc
- [Apple-SidebarTrackingSep] NSToolbarItem.Identifier sidebarTrackingSeparator /
  NSToolbarSidebarTrackingSeparatorItemIdentifier — Apple Docs:
  https://developer.apple.com/documentation/appkit/nstoolbaritem/identifier/sidebartrackingseparator
  and https://developer.apple.com/documentation/appkit/nstoolbarsidebartrackingseparatoritemidentifier?language=objc
- [Giacomi] How to set up the NSTrackingSeparatorToolbarItem in macOS 11 —
  Christian Giacomi: https://cgiacomi.com/posts/setup-nstrackingseparatortoolbaritem-macos11/
  (orig. https://christiangiacomi.com/posts/setup-nstrackingseparatortoolbaritem-macos11/)
- [Apple-ToolbarItemForum] How to hide NSTrackingSeparatorToolbarItem — Apple
  Developer Forums: https://developer.apple.com/forums/thread/659260
- [Bancarel-Medium] macOS full-height sidebar window — Paul Bancarel, Medium:
  https://medium.com/@bancarel.paul/macos-full-height-sidebar-window-62a214309a80
- [Apple-NavSplitView] NavigationSplitView — Apple Developer Docs:
  https://developer.apple.com/documentation/swiftui/navigationsplitview
- [UseYourLoaf-SplitView] SwiftUI Split View Configuration — Use Your Loaf:
  https://useyourloaf.com/blog/swiftui-split-view-configuration/
- [CreateWithSwift-NSV] Exploring the NavigationSplitView — Create with Swift:
  https://www.createwithswift.com/exploring-the-navigationsplitview/
- [Apple-Forum-746611] SwiftUI NavigationSplitView on macOS (toolbar safe-area
  bug, rdar://122947424) — Apple Developer Forums:
  https://developer.apple.com/forums/thread/746611
- [Apple-Forum-746611-followups] (same thread, follow-up replies / workarounds)
- [Eon-NSV] NavigationSplitView in SwiftUI — Eon's Swift blog:
  https://eon.codes/blog/2024/02/02/NavigationSplitView-in-swiftui/
- [Apple-Forum-651807] Collapse sidebar in SwiftUI — Apple Developer Forums:
  https://developer.apple.com/forums/thread/651807
- [Sarunw-Toggle] How to show and hide a sidebar in a SwiftUI macOS app — Sarunw:
  https://sarunw.com/posts/how-to-toggle-sidebar-in-macos/
- [HWS-RestoreSidebar] How do you restore a collapsed sidebar on macOS? —
  Hacking with Swift forums:
  https://www.hackingwithswift.com/forums/swiftui/swiftui-how-do-you-restore-a-collapsed-sidebar-on-macos/2396
- [NSWindowStyles] NSWindowStyles (hidden title bar / full-size content / traffic
  lights showcase) — lukakerr, GitHub: https://github.com/lukakerr/NSWindowStyles
- [INAppStoreWindow] INAppStoreWindow (customizable title bar + traffic lights) —
  indragiek, GitHub: https://github.com/indragiek/INAppStoreWindow
- [ClyappTrafficLights] Fix NSWindow traffic light buttons reverting to origin
  after resize — Clyapp/Medium (page now 410 Gone; summarized from search index):
  https://medium.com/@clyapp/fix-the-problem-that-nswindow-traffic-light-buttons-always-revert-to-its-origin-position-after-6a13675df18a
- [Tauri-4789] Allowing native inset OS X traffic lights on NSWindow — tauri
  issue #4789 (resize/full-screen reposition discussion):
  https://github.com/tauri-apps/tauri/issues/4789
- [Tauri-RoundedCorners] tauri-plugin-mac-rounded-corners
  (repositionTrafficLights on resize/fullscreen) — npm:
  https://www.npmjs.com/package/@cloudworxx/tauri-plugin-mac-rounded-corners
- [Electron-CustomTitleBar] Custom Title Bar — Electron docs
  (trafficLightPosition): https://www.electronjs.org/docs/latest/tutorial/custom-title-bar
- [Apple-NSVisualEffectView] NSVisualEffectView — Apple Developer Docs:
  https://developer.apple.com/documentation/appkit/nsvisualeffectview
- [Mackuba-DarkSide] Dark Side of the Mac: Appearance & Materials — mackuba.eu:
  https://mackuba.eu/2018/07/04/dark-side-mac-1/
- [Apple-HIG-Materials] Materials — Apple Human Interface Guidelines:
  https://developer.apple.com/design/human-interface-guidelines/foundations/materials/
- [Apple-HIG-Sidebars] Sidebars — Apple HIG:
  https://developer.apple.com/design/human-interface-guidelines/sidebars
- [Apple-HIG-Toolbars] Toolbars — Apple HIG:
  https://developer.apple.com/design/human-interface-guidelines/toolbars
- [SwiftWithMajid-SafeArea] Managing safe area in SwiftUI — Swift with Majid:
  https://swiftwithmajid.com/2021/11/03/managing-safe-area-in-swiftui/
- [WWDC25-323] Build a SwiftUI app with the new design — WWDC25 session 323:
  https://developer.apple.com/videos/play/wwdc2025/323/
- [DevTo-LiquidGlass] Liquid Glass in Swift: Official Best Practices for iOS 26 &
  macOS Tahoe — dev.to:
  https://dev.to/diskcleankit/liquid-glass-in-swift-official-best-practices-for-ios-26-macos-tahoe-1coo
- [Lapcat-Corners] The evolution of Mac app window corners — Lapcat Software:
  https://lapcatsoftware.com/articles/2026/3/4.html
- [MacStories-Tahoe] macOS 26 Tahoe: The MacStories Review:
  https://www.macstories.net/stories/macos-26-tahoe-the-macstories-review/2/
- [Cloudship-Tahoe] macOS Tahoe's Liquid Glass and the death of consistency —
  Cloudship: https://cloudship.co.uk/blog/macos-tahoe-liquid-glass/
- [MacObserver-26] macOS 26 critics say the new UI feels cluttered —
  The Mac Observer: https://www.macobserver.com/news/macos-26-critics/
- [WebSearch-Toggle] `.toolbar(removing: .sidebarToggle)` — surfaced via search
  index of SwiftUI sidebar toggle discussions (see Sarunw / Apple forum threads
  above for primary).
- [WebSearch-NSWindow] traffic-light vertical centering with hidden title
  visibility / titlebar accessory views — surfaced via search index
  (corroborated by NSWindowStyles, INAppStoreWindow above).
- [WebSearch-Materials] Big Sur spacing/structure changes — search index
  (corroborated by WWDC20-10104, Mackuba above).
