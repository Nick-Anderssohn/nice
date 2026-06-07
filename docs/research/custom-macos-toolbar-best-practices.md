# Custom Top Bar / Toolbar on macOS — Best Practices Research

Research compiled for the **Nice** macOS app (SwiftUI + AppKit interop). The
app hides the native title bar (`.hiddenTitleBar`) and draws its own 52pt top
bar containing a horizontally-scrolling strip of "pane pills", a "+" button, an
overflow chevron menu, and per-pill close buttons. It re-implements
drag-to-move-window and double-click-to-zoom. The motivating goal is
**drag-to-reorder the pane pills**, which the current custom bar makes risky to
extend.

> Terminology note: this report leads with Apple's standard terms — *toolbar*
> (`NSToolbar` / SwiftUI `.toolbar`), *title bar*, *title bar accessory*
> (`NSTitlebarAccessoryViewController`), *window tabs* (`NSWindowTabGroup`) — and
> maps the project's "pane pills / top bar" onto them where useful.

---

## Executive summary

- **There is no native control that does exactly what Nice's top bar does.**
  Apple ships three relevant pieces of machinery — `NSToolbar` (toolbar items),
  `NSTitlebarAccessoryViewController` (custom views injected into the *native*
  title bar), and `NSWindowTabGroup` (browser-style window tabs) — but none is a
  drop-in for a freely reorderable horizontal strip of custom tab-like pills.
  So a custom view is a legitimate choice; the real question is *how much* native
  machinery to keep underneath it.

- **The biggest cost of `.hiddenTitleBar` + a fully custom bar is that you take
  over responsibilities the system normally handles for free**: traffic-light
  button placement, full-screen layout transitions, window-move and
  double-click-to-zoom semantics, and accessibility wiring. Each of these is a
  known source of bugs in custom bars. Apple's HIG explicitly warns that when you
  replace standard controls you "inherit the responsibility of replicating all
  feedback behavior correctly." [HIG-Acc][HIG-Tb]

- **`NSToolbar` *does* support user reordering out of the box** via
  `allowsUserCustomization` + `autosavesConfiguration` — but only through the
  "Customize Toolbar…" sheet (a modal palette), **not** live in-place
  drag-to-reorder of the kind browsers use for tabs. It also can't natively model
  per-item close buttons / tab semantics well. So it does not satisfy the pane-pill
  use case. [Toolbar-Cust][HIG-Tb]

- **`NSTitlebarAccessoryViewController` is the most under-used middle path**: it
  lets you inject a custom SwiftUI/AppKit view into the *real* title bar (top,
  bottom, left, or right) while AppKit keeps owning the title bar itself — so you
  keep correct traffic-light layout, full-screen handling, and window-move/zoom for
  free, and only hand-roll the pill strip. Available since macOS 10.10. [TitleAcc]

- **Native window tabbing (`NSWindowTabGroup` / `addTabbedWindow`) gives you
  reorderable tabs, a "+" button, overflow, and full accessibility for free** —
  but it is "one tab = one `NSWindow`", which is a heavy architectural commitment
  and constrains your tab visuals to the system look. Most browsers/terminals that
  want custom tab visuals (Chrome, Safari's older custom bar era, iTerm, Warp)
  roll their own rather than use it. [Tabbing][TabGroup][WWDC16]

- **macOS SwiftUI drag-and-drop is materially flakier than on iOS** for
  reordering: no `onMove` for arbitrary `HStack`s, opaque `NSItemProvider`,
  custom UTIs that "don't appear to work" as drop targets on macOS, no built-in
  drop-target highlight, and `performDrop` async/return-value foot-guns. The
  reliable community pattern is a custom `ForEach` + `onDrag`/`onDrop` with a
  `DropDelegate` that moves items on `dropEntered`. [Eclectic][DropDelegate][AppleFM-Drag]

- **Window-move and double-click-to-zoom should lean on AppKit, not be hand-rolled.**
  `mouseDownCanMoveWindow` / `isMovableByWindowBackground` are the supported hooks;
  double-click-to-zoom must honor the system `AppleActionOnDoubleClick` default
  (in `NSGlobalDomain`, values `Maximize`/`Minimize`/`None`) — a detail custom bars
  routinely get wrong. [MovableBg][MouseDown][DblClick30166][DblClick677889]

- **Net recommendation:** the strongest path for *this* project is to **stop fully
  replacing the title bar** and instead host the SwiftUI pill strip inside an
  `NSTitlebarAccessoryViewController` (or keep `.hiddenTitleBar` but recover the
  native behaviors deliberately). That removes the move/zoom/full-screen/traffic-light
  bug surface and lets you focus the reorder work on a well-trodden SwiftUI
  drag-delegate pattern. See "Recommendations" below.

---

## 1. Native vs. custom toolbars on macOS

### The native options

SwiftUI exposes the native toolbar via `.toolbar { ToolbarItem(...) }` plus
`windowToolbarStyle(_:)`. Styles are `.unified` (default; title + items in one
row), `.unifiedCompact` (good for windows with few/no items, can hide the title),
`.expanded` (title on its own row above items), and `.automatic`. You can hide
the title with `.toolbar(removing: .title)` while keeping the toolbar background,
or remove the whole bar area with `.windowStyle(.hiddenTitleBar)`. Crucially,
the SwiftUI toolbar API "doesn't discuss reordering toolbar items or extensive
programmatic control — the focus remains on styling and visibility rather than
restructuring the toolbar layout." [ToolbarStyles]

Under SwiftUI, the underlying machinery is still `NSToolbar`. `NSToolbar` is
described by experienced Mac devs as "an amazing API with incredible
flexibility" that is also "too verbose … spread throughout your code with the use
of delegates and callbacks," which is why wrappers like DSFToolbar exist.
[DSFToolbar]

### What you lose by hiding the native title bar and drawing your own bar

`lukakerr/NSWindowStyles` catalogs the standard customization knobs without
private API: `titleVisibility = .hidden`, `styleMask.remove(.titled)`,
`titlebarAppearsTransparent = true`, `styleMask.insert(.fullSizeContentView)`,
hiding/repositioning traffic lights via `standardWindowButton(.closeButton)` etc.,
and `isMovableByWindowBackground = true`. The repo demonstrates "17 different
combinations … without using private APIs." [NSWindowStyles]

The trade-offs when you go fully custom (`.hiddenTitleBar` + own bar):

- **Traffic-light (window-control) layout.** With a hidden/transparent title bar
  and full-size content view, the close/minimize/zoom buttons float over your
  content and you become responsible for vertical centering against your bar
  height, and for re-laying them out on full-screen and resize. Apple's HIG says
  to keep these controls visible and unobstructed. [NSWindowStyles][HIG-Tb]
- **Full-screen behavior.** The system normally animates the title bar/toolbar in
  and out of full screen. `NSTitlebarAccessoryViewController` is "contained in a
  visual effect view, which automatically handles … size and location changes when
  a window enters/exits full screen mode." A fully custom bar gets none of that
  and must handle the transition itself. [TitleAcc]
- **Safe-area / inset handling.** With a custom bar you must inset your content
  manually; SwiftUI's `safeAreaInset(...)` is the supported tool, and insets
  "work like a stack" so multiple bars combine. Getting this wrong causes content
  to slide under the bar or under the traffic lights. [SafeArea]
- **System appearance & vibrancy.** Native title bars/toolbars get the system blur
  ("visual effect view") behind them automatically; custom bars must reproduce it
  with `NSVisualEffectView` (`.behindWindow`). [NSWindowStyles][TitleAcc]
- **Accessibility.** Native toolbar items are accessible automatically; custom
  views must be wired up by hand (see §7).

**Guidance to weigh against the project:** prefer native machinery when its
behavior matches your need; reach for custom only for the part that genuinely has
no native equivalent (here, the reorderable tab-pill visuals). The HIG frames
system components as the default precisely because replacing them transfers a long
tail of feedback/behavior responsibilities to you. [HIG-Acc]

---

## 2. `NSToolbar` capabilities relevant here

**User reordering is supported — but only via the customization sheet, not live
in-place drag.**

- `allowsUserCustomization` (default `false`): when `true`, enables the
  "Customize Toolbar…" menu item, which lets people "Change the items on the
  toolbar," "Rearrange their positions," and "Change the toolbar's display mode."
  [Toolbar-Cust]
- You should pair it with `autosavesConfiguration = true` so reordering persists
  across launches. [Toolbar-Cust]
- Reordering happens inside a **customization palette** (a modal-ish sheet). The
  palette's contents come from the delegate methods
  `toolbarAllowedItemIdentifiers(_:)` and `toolbarDefaultItemIdentifiers(_:)`;
  "every allowed item must be explicitly listed." [NSToolbarRef][CocoaDev]

**Limits for this use case:**

- The customization flow is the *Customize Toolbar* sheet — it is **not** the
  browser-style "grab a tab and slide it" interaction the pane pills want.
  [Toolbar-Cust]
- `NSToolbar` items are a fixed vocabulary (button, segmented, space, flexible
  space, custom view). It has no first-class notion of a tab with its own close
  button, selection state, or an overflow chevron tied to per-item models — those
  would all be custom-view items you still manage yourself.
- It *can* host custom views (an `NSToolbarItem` whose `view` is any `NSView`,
  including an `NSHostingView`), so a hybrid is technically possible, but you'd be
  fighting the toolbar's layout model to get a freely-reorderable strip.

**Bottom line:** `NSToolbar` solves *persisted user customization of a fixed
command set*, not *live drag-reorderable tabs*. It does not satisfy the pane-pill
requirement on its own. [Toolbar-Cust][NSToolbarRef]

---

## 3. Title bar accessories — `NSTitlebarAccessoryViewController`

This is the "keep the native title bar, inject your own view" middle path, and
it is the most relevant native facility for Nice.

- **What it is:** "An object that manages a custom view — known as an accessory
  view — in the title bar–toolbar area of a window." Introduced **macOS 10.10**.
  [TitleAcc]
- **Placement:** `layoutAttribute` accepts `.bottom` (a strip below the title bar
  — you can set its height), `.left`, or `.right` (you set width; left/right on
  10.11+). Default is `.bottom`. [TitleAcc][TitleAcc-Search]
- **It is wrapped in a visual effect view that auto-handles blur and
  size/location changes on full-screen enter/exit** — i.e. you inherit correct
  full-screen behavior for free. It is also Apple's recommended replacement for
  the deprecated `NSToolbar.fullScreenAccessoryView` APIs. [TitleAcc]
- **Height caveat:** the title bar's own height is fixed; setting the accessory
  height does not change the title height and the accessory is clipped by the
  title bar. Set `layoutAttribute` *before* adding the controller. There is also
  `fullScreenMinHeight` for the below-title strip in full screen. [TitleAcc][TitleAcc-Search]
- **Hosting SwiftUI:** `vc.view = NSHostingView(rootView: yourSwiftUIView)` then
  `window.addTitlebarAccessoryViewController(vc)` (override `loadView()` rather
  than the `view` property directly). [TitleAcc][TitleAcc-Search]

**Pros vs. a full custom bar:**
- AppKit still owns the title bar → traffic lights laid out correctly,
  window-move and double-click-to-zoom work natively over the title bar region,
  full-screen transitions handled.
- You hand-roll only the strip's *contents*, not the chrome.

**Cons / caveats:**
- The fixed title-bar height constrains how tall a `.bottom`-area strip can be
  before clipping; a 52pt bar may need a `.bottom` accessory under a (possibly
  hidden) title rather than living *in* the title row. [TitleAcc-Search]
- You still draw the pills yourself, so the reorder work is unchanged — but the
  surrounding bug surface (move/zoom/full-screen/traffic-lights) largely
  disappears.

---

## 4. Tab-like UI in the title bar

### Native window tabbing (`NSWindowTabGroup`)

macOS Sierra (10.12) added system window tabbing. Key facts:

- A tab-able window has an `NSWindowTabGroup`; each `NSWindowTab` is associated
  with a window in the group. [TabGroup]
- **"It suffices to call `NSWindow.addTabbedWindow(_:ordered:)` to add a window to
  the native tab bar and get everything tabs do for free."** To enable the "+"
  button, implement `newWindowForTab(_:)` somewhere in the responder chain; AppKit
  auto-enables the button when it finds that override. [Tabbing]
- Architecturally, **switching tabs swaps the on-screen `NSWindow`** for the
  selected tab's window; the title bar and traffic lights stay put, so to the user
  it looks like content changing. This means **one tab == one `NSWindow`**.
  [Tabbing][WWDC16]
- `tabbingMode` is `.automatic` (follow user prefs), `.preferred` (force tab bar),
  or `.disallowed`. [TabGroup][Tabbing]
- Caveat from a long-form guide: maintaining the responder chain is fiddly — you
  must set `newWindow.windowController = self` so new tabs respond to "+"/menu
  actions, "the world's most comprehensive guide … that you shouldn't adhere to"
  is itself a warning about how thorny this gets. [TietzeSingle][Tabbing]

**What you get for free:** reorderable tabs (drag within the tab bar), the "+"
button, overflow handling, full keyboard/VoiceOver support, and the native look.

**Why real apps often don't use it:** native tabbing forces the system tab visual
and the one-window-per-tab model. Apps wanting custom tab visuals or a custom
session/pane model (Chrome, Warp, iTerm2, and SwiftUI apps drawing pill strips)
typically roll their own horizontal strip — accepting that they must re-implement
reorder, "+", overflow, and accessibility. This is exactly the trade Nice has
already made.

### Implication for Nice

If Nice's "panes" are not 1:1 with `NSWindow`s (likely, since they live in a
scrolling strip inside one window), **native window tabbing is a poor fit** and
the custom strip is justified. The decision then is §3 (host the custom strip in
a title-bar accessory and recover native chrome behaviors) vs. status quo (fully
custom bar).

---

## 5. Drag-to-reorder best practices (horizontal strip, SwiftUI on macOS)

### The approaches, ranked for macOS reliability

1. **`List` + `.onMove` (`EditButton`/`move(fromOffsets:toOffset:)`).** The most
   reliable reorder API — but it is a vertical `List` affordance and **does not
   apply to a custom horizontal `HStack`/`LazyHStack`**. Not usable for a pill
   strip. [DropDelegate]
2. **Custom `ForEach` + `onDrag` + `onDrop(of:delegate:)` with a `DropDelegate`
   (the community standard).** Works in any layout (HStack/LazyHStack/grids).
   Each item gets `.onDrag { NSItemProvider(...) }`; a `DropDelegate`
   (`ReorderableDragRelocateDelegate`-style) performs the move **immediately in
   `dropEntered`** as the dragged item passes over a neighbor, wrapped in
   `withAnimation`. A second delegate on the container handles drops outside.
   This is the pattern in Daniel Saidi's `ReorderableForEach`, Canopas, and
   globulus/swiftui-reorderable-foreach. [DropDelegate][Canopas][Globulus]
3. **`.draggable` / `.dropDestination` + `Transferable` (modern API).** Cleaner,
   but `Transferable` requires **macOS 13+** and the richer
   `dropDestination(for:action:)` insertion-index sample targets **macOS 15+**.
   Apple's own sample uses `contacts.move(fromOffsets:toOffset:)` on drop.
   [AppleDrag]
4. **Manual gesture-based reordering (`DragGesture` + offset math).** No
   `NSItemProvider` at all — you track the drag translation, compute target index,
   and animate. Avoids the macOS DnD quirks entirely at the cost of writing the
   hit-testing/auto-scroll yourself. A common choice when system DnD proves
   unreliable.

### macOS-specific pitfalls (these are why custom DnD feels buggy)

- **No drop-target highlight for free.** "AppKit conventions change the colour of
  the view background … but there doesn't appear to be a straightforward way to
  implement that in SwiftUI." You must draw your own insertion indicator.
  [AppleFM-Drag][Eclectic]
- **Custom UTIs don't reliably work as drop targets on macOS** — "attempting to
  use custom UTIs with dropzones does not result in the dropzone accepting them."
  Prefer a built-in type (e.g. plain `String`/`Int` id) for the provider.
  [AppleFM-Drag]
- **`NSItemProvider` is opaque and async.** Completion handlers run on background
  threads; UI updates must be dispatched to the main queue. A classic bug:
  `performDrop` returns `false` because the success flag is only set inside an
  async block, so the drop animation always cancels. [Eclectic]
- **Transparent areas don't initiate drags.** A draggable view needs a non-clear
  background (even `Color.white.opacity(0.0001)` / `.contentShape`) or the drag
  won't start. [Canopas]
- **Xcode Previews don't run `DropDelegate` actions** — you must test in the
  running app. [Eclectic]
- **Empty-collection / drop-after-last-item** cases need an explicit container
  drop delegate; otherwise items can't be dropped at the ends. [DropDelegate]

### Reorderable tabs/pills specifically

For a horizontal pill strip, approach **#2** (custom `ForEach` + `DropDelegate`
moving on `dropEntered`) is the most-cited reliable recipe; if you hit macOS DnD
flakiness, fall back to **#4** (gesture-based) which several teams adopt for tab
strips precisely to avoid `NSItemProvider`. Whichever you pick, make the move
operation a pure `array.move(fromOffsets:toOffset:)` on the source-of-truth model
and let SwiftUI animate from the data change. [DropDelegate][AppleDrag]

---

## 6. Window drag & double-click-to-zoom done right

### Window drag

- **`isMovableByWindowBackground`** — "indicates whether the window is movable by
  clicking and dragging anywhere in its background." Simplest hook; turn it on and
  the whole background drags the window. Sheets/drawers can't use it. [MovableBg]
- **`mouseDownCanMoveWindow`** (on `NSView`) — "indicates whether the view can
  pass mouse down events through to its superviews," i.e. it "lets you determine
  the region by which a window can be moved." Default is `false` for opaque views,
  `true` for non-opaque. **Subclasses can override it to return different values
  per event** — this is how you make *specific* regions of a custom bar draggable
  while keeping buttons clickable. [MouseDown]

The clean approach: let the bar's empty/background regions report
`mouseDownCanMoveWindow = true` and let interactive controls (pills, "+", close,
chevron) report `false`, rather than hand-implementing a `DragGesture` that
repositions the window. Manual drag re-implementations tend to mishandle
multi-monitor coordinates, menu-bar snapping, and Split View. (Apple provides the
above hooks specifically so you don't have to.) [MovableBg][MouseDown]

### Double-click-to-zoom

- The action is **a user preference, not a constant.** It's stored in
  **`NSGlobalDomain`** under key **`AppleActionOnDoubleClick`** with values
  **`Maximize`**, **`Minimize`**, or **`None`**. [DblClick30166]
- A correct custom bar must **read that default and branch** — call
  `window.performZoom(nil)` for `Maximize`, `performMiniaturize(nil)` for
  `Minimize`, do nothing for `None` — rather than hard-coding zoom. `performZoom`
  "simulates the user clicking the zoom box." [DblClick677889][DblClick30166]
- The legacy `AppleMiniaturizeOnDoubleClick` key is **deprecated since OS X 10.11**;
  don't rely on it. [DblClick30166]
- Known platform bug: opening the Dock preference pane can flip
  `AppleActionOnDoubleClick` from `None` to `Maximize` (Radar #24094110) — i.e.
  the value really can change at runtime, so read it live, not once at launch.
  [DblClick30166]

**Pitfall summary:** the two most common custom-bar regressions are (a) ignoring
`AppleActionOnDoubleClick` (zooming when the user set "Do Nothing"), and (b)
making the *entire* bar draggable so double-clicks on/near pills get swallowed.
Both are avoided by leaning on `mouseDownCanMoveWindow` regions + reading the
global default. Hosting the strip in a title-bar accessory (§3) sidesteps both
entirely, because AppKit applies the native title-bar behavior over the title
region for you.

---

## 7. Accessibility & system-integration considerations

- **macOS apps must support VoiceOver, Full Keyboard Access, and Switch Control.**
  For toolbars specifically, "icon-only toolbar items and image buttons must
  provide labels." The "+" button, close buttons, and chevron each need a clear
  accessibility label. [HIG-Acc][HIG-Tb]
- **Custom controls inherit full accessibility responsibility.** "When you replace
  standard controls with custom ones, you inherit the responsibility of replicating
  all feedback behavior correctly, which is why the HIG pushes you toward system
  components as the default." Characterize custom elements with system APIs so
  VoiceOver announces role/state (e.g. announces "button"). For the pills, expose
  selected state, and a draggable trait if reordering is VoiceOver-accessible.
  [HIG-Acc]
- **Parity rule:** "If you can tap, click, or drag something … you should strive
  to make it work with VoiceOver, too." A drag-to-reorder gesture should have a
  non-drag alternative (e.g. a context-menu "Move Left/Right" or keyboard
  shortcut), since pointer-drag reorder is not VoiceOver/Switch-Control friendly.
  [HIG-Acc][VoiceOver]
- **Keyboard navigation:** native toolbars/tab groups are keyboard-navigable for
  free; a custom strip must implement focus, Tab traversal, and selection keys
  itself.
- **Reduce Motion:** reorder and tab-switch animations should be gated on the
  Reduce Motion accessibility setting (`accessibilityReduceMotion` /
  `NSWorkspace.shared.accessibilityDisplayShouldReduceMotion`). (Standard HIG
  accessibility guidance; flag as best-practice rather than a toolbar-specific
  rule.) [HIG-Acc]
- **Traffic-light positioning:** keep the close/minimize/zoom buttons in their
  expected top-left location and vertically centered against the bar; don't let
  pills overlap them. The HIG calls for window controls to remain accessible.
  [HIG-Tb][NSWindowStyles]

> Native machinery (`NSToolbar`, `NSWindowTabGroup`, title-bar accessory chrome)
> gets most of the above for free. A fully custom bar must reproduce all of it —
> this is the strongest non-visual argument for keeping native chrome underneath.

---

## Recommendations for a reorderable custom top bar (for Nice)

Tying the findings to the goal (safe, extensible drag-to-reorder of pane pills):

1. **Keep the pill strip custom — but stop fully replacing the title bar.**
   Native window tabbing (§4) doesn't fit a multi-pane-in-one-window model, and
   `NSToolbar` customization (§2) isn't live drag-reorder. So custom pills are
   correct. The high-leverage change is to **host that custom SwiftUI strip in an
   `NSTitlebarAccessoryViewController`** (§3) instead of `.hiddenTitleBar` + a
   from-scratch bar. That instantly returns correct traffic-light layout,
   full-screen transitions, and native window-move + double-click-to-zoom over the
   title region — eliminating most of the bug surface that makes the current bar
   "hard to extend." [TitleAcc]
   - Mind the fixed title-bar height: a 52pt strip likely belongs in the
     `.bottom` accessory area (below the title row), possibly with the title
     hidden, rather than crammed into the title row itself. [TitleAcc-Search]

2. **If you keep `.hiddenTitleBar`, recover the native behaviors deliberately,
   don't hand-roll them.** Use `mouseDownCanMoveWindow` per-region (background =
   movable, controls = not) instead of a `DragGesture`, and implement
   double-click by reading `AppleActionOnDoubleClick` from `NSGlobalDomain` live
   and calling `performZoom`/`performMiniaturize` accordingly. [MouseDown][DblClick30166]

3. **For the reorder itself, use the custom `ForEach` + `DropDelegate` pattern
   that moves on `dropEntered`** (approach §5.2), with the move expressed as
   `array.move(fromOffsets:toOffset:)` on your source-of-truth model so SwiftUI
   animates from the data change. Use a **built-in provider payload (e.g. the
   pill's id as String/Int), not a custom UTI** (custom UTIs are unreliable as
   macOS drop targets), give each pill a non-transparent `contentShape`, and add a
   **container-level drop delegate** so drops at the ends/empty work.
   [DropDelegate][AppleFM-Drag][Canopas]

4. **Have a gesture-based fallback ready.** If `NSItemProvider`-based DnD proves
   flaky in the strip (a real risk on macOS), switch to a `DragGesture`+offset
   reorder (§5.4) — many tab-strip implementations do exactly this to dodge the
   `NSItemProvider` quirks. Decide early, because it changes the architecture.
   [Eclectic][AppleFM-Drag]

5. **Adopt `Transferable` only if you can require macOS 13+/15+** for the cleaner
   `draggable`/`dropDestination(for:action:)` insertion-index API; otherwise stay
   on `onDrag`/`onDrop`. Confirm against Nice's deployment target. [AppleDrag]

6. **Bake in accessibility from the start of the reorder work:** label "+",
   close, and chevron; expose pill role/selection; gate animations on Reduce
   Motion; and provide a **non-drag reorder path** (context menu or keyboard) for
   VoiceOver/Switch-Control users. [HIG-Acc][VoiceOver]

7. **Persist order in your own model** (not toolbar autosave) since this is a
   custom strip; `NSToolbar`'s `autosavesConfiguration` only applies if you were
   using real toolbar items. [Toolbar-Cust]

**Keep-custom vs. go-native verdict for this project:** keep the *visuals*
custom; move the *chrome* native. The cheapest large reduction in bug risk is
adopting `NSTitlebarAccessoryViewController` so AppKit owns move/zoom/full-screen/
traffic-lights again, leaving you a clean, isolated SwiftUI surface in which to
implement reorder with the proven drag-delegate (or gesture) pattern. Going fully
native (window tabbing / NSToolbar customization) is not recommended — it doesn't
match the pane model or deliver the desired interaction.

---

## Uncertainties / conflicts to flag

- **Exact max height of a title-bar accessory before clipping** isn't given as a
  number; sources only say the title height is fixed and the accessory is clipped
  by it. Validate that a 52pt strip renders un-clipped in your placement
  (likely `.bottom`) before committing. [TitleAcc][TitleAcc-Search]
- **macOS SwiftUI DnD reliability is anecdotal.** Multiple reputable sources
  report the quirks above, but severity varies by macOS version; treat approach
  §5.2 as "usually works" and keep §5.4 as fallback. Sources don't pin the quirks
  to specific macOS point releases. [Eclectic][AppleFM-Drag]
- **`Transferable` insertion-index drop sample** is documented at macOS 15 in
  Apple's sample metadata, while `Transferable`/`draggable` themselves are macOS
  13+. The minimum for *reorder with index* specifically is therefore version-
  sensitive — verify against your target. [AppleDrag]
- **HIG specifics** here are drawn from search-engine extracts of the Toolbars and
  Accessibility HIG pages (the live pages are JS-rendered and didn't return full
  body text to the fetch tool); the quoted phrases are consistent across extracts
  but were not independently re-verified line-by-line against the rendered page.
  [HIG-Tb][HIG-Acc]

---

## Sources

Apple documentation & HIG:
- [HIG-Tb] HIG — Toolbars: https://developer.apple.com/design/human-interface-guidelines/toolbars
- [HIG-Acc] HIG — Accessibility: https://developer.apple.com/design/human-interface-guidelines/accessibility
- [VoiceOver] HIG — VoiceOver: https://developer.apple.com/design/human-interface-guidelines/voiceover
- [TitleAcc] NSTitlebarAccessoryViewController: https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller
- [Toolbar-Cust] NSToolbar.allowsUserCustomization: https://developer.apple.com/documentation/appkit/nstoolbar/allowsusercustomization
- [MovableBg] NSWindow.isMovableByWindowBackground: https://developer.apple.com/documentation/appkit/nswindow/ismovablebywindowbackground
- [MouseDown] NSView.mouseDownCanMoveWindow: https://developer.apple.com/documentation/appkit/nsview/mousedowncanmovewindow
- [TabGroup] NSWindowTab / NSWindowTabGroup: https://developer.apple.com/documentation/appkit/nswindowtab
- [AppleDrag] Adopting drag and drop using SwiftUI (Apple sample): https://developer.apple.com/documentation/SwiftUI/Adopting-drag-and-drop-using-SwiftUI
- [AppleFM-Drag] Apple Developer Forums — How to drag custom items in SwiftUI on macOS: https://developer.apple.com/forums/thread/649451
- [DblClick30166] Apple Developer Forums — double-click title bar zoom / AppleActionOnDoubleClick: https://developer.apple.com/forums/thread/30166
- [DblClick677889] Apple Developer Forums — Catalyst double-click to zoom / performZoom: https://developer.apple.com/forums/thread/677889
- [WWDC16] WWDC 2016 Session 203, What's New in Cocoa (window tabbing): https://asciiwwdc.com/2016/sessions/203

Engineering blogs, references & libraries:
- [ToolbarStyles] Nil Coalescing — A guide to macOS window toolbar styles in SwiftUI: https://nilcoalescing.com/blog/AGuideToMacOSToolbarStylesInSwiftUI/
- [NSWindowStyles] lukakerr/NSWindowStyles: https://github.com/lukakerr/NSWindowStyles
- [DSFToolbar] dagronf/DSFToolbar: https://github.com/dagronf/DSFToolbar
- [TabbingShowcase] robin/TitlebarAndToolbar: https://github.com/robin/TitlebarAndToolbar
- [Tabbing] Christian Tietze — Programmatically Add Tabs to NSWindows without NSDocument: https://christiantietze.de/posts/2019/01/programmatically-add-nswindow-tabs/
- [TietzeSingle] Christian Tietze — Comprehensive guide to programmatic NSWindow tabs (single controller): https://christiantietze.de/posts/2019/07/nswindow-tabbing-single-nswindowcontroller/
- [NSWindowTabbingRepo] DivineDominion/NSWindow-Tabbing: https://github.com/DivineDominion/NSWindow-Tabbing
- [Eclectic] The Eclectic Light Company — SwiftUI on macOS: Drag and drop, and more: https://eclecticlight.co/2024/05/21/swiftui-on-macos-drag-and-drop-and-more/
- [DropDelegate] Daniel Saidi — Enabling drag reordering in SwiftUI lazy grids and stacks: https://danielsaidi.com/blog/2023/08/30/enabling-drag-reordering-in-swiftui-lazy-grids-and-stacks
- [Canopas] Canopas — Reorder items with Drag and Drop using SwiftUI: https://canopas.com/reorder-items-with-drag-and-drop-using-swiftui-e336d44b9d02
- [Globulus] globulus/swiftui-reorderable-foreach: https://github.com/globulus/swiftui-reorderable-foreach
- [SafeArea] Hacking with Swift — How to inset the safe area with custom content (safeAreaInset): https://www.hackingwithswift.com/quick-start/swiftui/how-to-inset-the-safe-area-with-custom-content
- [NSToolbarRef] NSToolbar Class Reference (legacy ADC mirror): https://leopard-adc.pepas.com/documentation/Cocoa/Reference/ApplicationKit/Classes/NSToolbar_Class/Reference/Reference.html
- [CocoaDev] CocoaDev — NSToolbar: https://cocoadev.github.io/NSToolbar/
- [TitleAcc-Search] onmyway133/blog #899 — show view below title bar in SwiftUI (title-bar accessory usage): https://github.com/onmyway133/blog/issues/899
