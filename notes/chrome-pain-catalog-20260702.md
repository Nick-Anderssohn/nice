---
title: Chrome-pain catalog — what actually went wrong in Nice's pill-drag / reorder / tear-off work
date: 2026-07-02
provenance: extracted 2026-07-02 by a subagent commissioned to re-aim the tractability A/B (notes/tractability-ab-design-20260702.md v2) after Nick's correction that the documented chrome pain is the UI-framework layer, not the Swift language. Sources = the 14 research/handoff docs listed below, read in full.
---

# Catalog: what actually went wrong in Nice's pill-drag / reorder / tear-off work

Sources: all paths relative to the repo root. The docs span an abandoned first attempt (branch `worktree-draggable-panes`), a title-bar audit cycle (5 research docs + synthesis), a rebuilt reorder (4 handoffs + a decision doc with 3 updates), a tear-off cycle (2 handoffs), and a final chrome redesign (`docs/window-chrome-architecture.md`).

---

## Part 1 — The struggles, one by one

### S1. First draggable-panes attempt (branch `worktree-draggable-panes`) — abandoned wholesale

**Attempted:** pill reorder + cross-window drag + tear-off, all at once (~2232 insertions, 17 files).

**Symptom:** "The user deliberately did not merge it because it wasn't working well — possibly not at all" (`docs/research/draggable-pane-pills-handoff.md`). The concrete failure catalog is `docs/research/audit-draggable-tabs.md` "Anti-patterns present": a process-wide `pendingTearOff` slot with "no TTL, overwrite-while-pending leaks panes, source mutation committed before destination ever absorbs"; a `didDropOnTarget` flag existing "because the SwiftUI side cannot directly tell the AppKit drag source 'I accepted, don't tear off'"; drag session state "cleared *only* by the AppKit `endedAt` callback" so a failed `beginDraggingSession` "leaves `session` set indefinitely"; and coordinate-space ambiguity because SwiftUI `DropDelegate` uses `info.location` while the AppKit source works in screen points.

**Layer:** mixed. The tear-off slot design is (f) self-inflicted (`audit-draggable-tabs.md` "Recommended direction" fixes it without framework changes) — but the *hybrid itself* is (c): "AppKit gives the tear-off-detection signal that SwiftUI doesn't expose, but SwiftUI's drop delegates compose more easily into a SwiftUI view tree. The cost is the `didDropOnTarget` flag and the coordinate-space ambiguity" (`audit-draggable-tabs.md` "Novel choices" §1). The three-layer window-drag suppression (`isMovable=false` + `mouseDownCanMoveWindow=false` + `hitTest` override returning `self` for every in-bounds point) is (c): "The hit-test override is a *workaround* for not being able to reach inside SwiftUI's hosting machinery... SwiftUI's internals are private NSViews you can't subclass" (`audit-draggable-tabs.md` "Coexistence" §).

**Resolution:** abandoned; the feature was rebuilt from scratch (S3–S6). The audit and `docs/research/draggable-tabs-best-practices.md` were the only salvage.

### S2. `isMovable = false` collapsed the window's draggable surface

**Attempted:** on the old branch, stop pills from dragging the window while keeping empty chrome draggable.

**Symptom:** user report: "we can no longer drag the window via the top bar at all — only where the sidebar and top bar overlap" (`docs/research/audit-title-bar.md` "Diagnosis"). Two compounding causes: `window.isMovable = false` "not only kills cooperative drag, it also prevents AppKit from computing *any* drag region from the hit-test chain"; and the `WindowDragRegion` in the toolbar `.background` was occluded by the opaque `Color.niceChrome` fill, which "wins hit-testing" — so the only draggable pixels left were the sidebar's own drag strip.

**Layer:** (f) in execution — "reaches for the most destructive lever rather than the surgical one" (`audit-title-bar.md`) — but the pressure came from (c): the comment being critiqued "correctly identifies the *symptom* it's defending against (transparent SwiftUI hosting NSViews inheriting `mouseDownCanMoveWindow == true`...)". You cannot set `mouseDownCanMoveWindow=false` on SwiftUI's private internal wrappers, so widget-level suppression (the AppKit-correct fix per `draggable-tabs-best-practices.md` [7][8]) was not straightforwardly available. Note the irony recorded across docs: `docs/research/synthesis.md` recommended deleting `isMovable=false` ("the only thing it got wrong was reaching for a window-level switch"), yet the eventually shipped architecture (S3 UPDATE 3, then `docs/window-chrome-architecture.md`) **re-adopted `isMovable=false`** as the load-bearing fix, now re-asserted per event. The docs genuinely disagree over time about which lever is correct — evidence that the seam offers no principled answer, only empirics.

**Resolution:** `synthesis.md` Step 1 (revert + layout swap) for the old branch; the rebuilt line converged on keeping `isMovable=false` with explicit `performDrag`.

### S3. Rebuilt reorder: "dragging a pill also drags the whole window" — 9 ruled-out approaches, 2 measured spikes

**Attempted:** intra-strip pill reorder under `.hiddenTitleBar` (the 52pt band is the native title bar).

**Symptom & the attempt trail** (`docs/research/pill-drag-window-move-decision.md`, numbered list): baseline pill drag moves window ~170pt; then, all failing:
1. SwiftUI `.onDrag` on the pill — window still moves.
2. `.highPriorityGesture(DragGesture())` — "SwiftUI gestures lose to the AppKit title-bar tracker, which sits below them."
3. `mouseDownCanMoveWindow=false` NSView as pill `.background` — "a sibling/behind view, not in the event-propagation chain."
4. Same veto as frontmost `.overlay` — "AppKit's title-bar hit-test doesn't reliably descend into SwiftUI-embedded NSViews."
5. macOS 15 `windowBackgroundDragBehavior(.enabled)` under `.hiddenTitleBar` — pill still drags, zoom broke.
6. Diagnostic: removing `WindowDragRegion` entirely — empty chrome still drags (it was the native title-bar drag all along).
7. `NSEvent` monitor toggling `isMovable` on press-over-control — still moves.
8. Diagnostic: monitor consuming (`return nil`) every left-mouse-down — empty chrome STILL drags under XCUITest: "App-level `NSEvent` monitors cannot intercept XCUITest's synthesized title-bar drag at all."

Then spike 1, `.windowStyle(.plain)` (UPDATE 1): "viable but expensive" — it strips traffic lights, rounded corners, shadow, zoom, and even window drag ("the modifier makes the *window background* draggable, but our toolbar paints an opaque `Color.niceChrome(...)` over the whole window... no exposed background region"). Rejected (see `docs/research/pill-drag-plain-approach-handoff.md`, marked SUPERSEDED). AX bonus finding: under `.plain` the window's AX node is `Disabled`, so `windows.firstMatch.waitForExistence` returns false.

Spike 2, `isMovable=false` + `performDrag` (UPDATE 2): worked, with two of the session's own prior conclusions instrument-disproven: "the earlier hunch that the sidebar's `mdcmw=true` 'survives' `isMovable=false` was wrong — `isMovable=false` gates the `mdcmw` drag path too," and the "not XCUITest-automatable" claim about `performDrag` was "**wrong**. It only held for drag handlers on the background `DragView`, which the pane strip's `NSClipView` occludes at the test's click point." Also: no monitor-level selectivity is possible because "`hitTest` returns SwiftUI-internal classes that vary inconsistently (`NSClipView`, `PlatformGroupContainer`, `DragView`, ...)" and "`accessibilityHitTest(...)` returns the top-level hosting view as `AXGroup` for *every* point — it does not descend into SwiftUI."

**Layer:** (c), as cleanly documented as it gets — AppKit's title-bar drag tracker operates below SwiftUI's gesture system, AppKit hit-testing doesn't descend into SwiftUI hosting internals, and SwiftUI internals can't be introspected from the AppKit side. Attempt 5 and the `.plain` findings are (a) SwiftUI framework limitations (`windowBackgroundDragBehavior` semantics; `.plain` strips all chrome). Attempt 8 and the zoom-test flake are (e) tooling — XCUITest's synthesized events bypass app-level monitors, and the doc records that this *distorted design choices*: "A monitor/`isMovable`-style fix might work for **real** users but **cannot be verified by XCUITest**, which conflicts with the 'fully automated' requirement."

**Resolution & cost:** UPDATE 3 — `isMovable=false` + toolbar-wide SwiftUI `DragGesture` → `performDrag(with: NSApp.currentEvent!)` + pill `.onDrag` claiming the gesture for selectivity. Cost across docs: 1 planning handoff (`draggable-pane-pills-handoff.md`), 1 decision doc with 3 dated updates, 1 superseded handoff (`.plain`), 1 continuation handoff (`pill-reorder-continuation-handoff.md`) — about 11 distinct approaches/spikes and ~4 session handoffs for the window-drag conflict alone, on a feature (reorder) whose model/resolver logic (pure Swift: `PaneStripDropResolver`, `TabModel.movePane`, 34 unit tests) was **done and green from the start**.

### S4. Tear-off: no SwiftUI signal for "dropped outside any window," and the yield regression

**Attempted:** tear-off to a new window + cross-window live-pane move (`docs/research/pill-tearoff-handoff.md`, `pill-tearoff-continuation-handoff.md`).

**Symptoms:**
- (a) limitation, stated flat: "SwiftUI's `.onDrop` only fires when the drag ends **over a drop target**. There is **no SwiftUI callback for 'dropped on empty desktop.'** " and "Pure SwiftUI `.onDrag` cannot detect 'dropped off the window' (SwiftUI owns the drag session and exposes no end callback)" — forcing a switch to AppKit `NSDraggingSource`.
- One reverted failed attempt: "A prior attempt did this but **re-introduced the window-drag bug** by removing the pill's `.onDrag` (which is what made `windowDragGesture` yield) without re-solving the yield. That attempt has been reverted" (`pill-tearoff-continuation-handoff.md`). The fix required a `WindowDragGate` flag the AppKit source sets so the SwiftUI gesture yields — a cross-framework hand-signal.
- The invariant was fragile enough that process-level guardrails were installed: "`.claude/hooks/guard-window-drag.sh` — PreToolUse hook that injects the invariant + required UITests whenever an agent edits `WindowToolbarView.swift`," plus "a loud `⚠️ INVARIANT` comment on the pill's `.onDrag`," plus instructions to keep the work on the strongest model because "It compiles + unit-passes while behaviorally wrong; the UITest gate is the only real check."
- (e): "the cross-window DROP isn't drivable by synthesized XCUITest drags, so it verifies the two-window setup and skips the drop."
- Live migration constraint (c/b): "A live `NiceTerminalView` + pty **cannot ride the pasteboard**" → in-process `LivePaneRegistry` side channel; NSView reparent needed dedicated guards ("no-respawn after window change, focus re-arm, Metal layer rebind" — `NiceTerminalViewReparentTests`), and the handoff warns "SwiftTerm views can be finicky about `setFrameSize`/first-responder on reparent... so you don't trip the 'spawn on first non-zero frame' path again."

**Layer:** the trigger problem is (a) (missing SwiftUI API); the yield regression and the gate flag are (c) (drag selectivity split between an AppKit `NSDraggingSource` and a SwiftUI gesture, coordinated by a mutable flag); reparent guards are (c)/(b); the migration registry design itself went fine (the model-layer work — detach/adopt, `PaneTearOffController`, persistence — is repeatedly reported "fully built and tested" without drama).

**Resolution & cost:** shipped (`PaneDragSource.swift` NSDraggingSource + `WindowDragGate` + pure `PaneDragEnd.outcome(...)`), 2 handoffs, 1 reverted attempt. But note S5: the `WindowDragGate` fix later failed again.

### S5. The recurrence: three bugs that "kept recurring for years" and the final redesign

`docs/window-chrome-architecture.md` opens: "For years three bugs kept recurring because each of those behaviours was implemented by a separate band-aid that *remembered* state and could drift out of sync."

- **BUG A — tear-off of an unspawned pane silently no-op'd.** "The old tear-off/migration path force-read a *live* pty entry, got `nil` for a deferred pane, and bailed." Layer: (f) — a nil-force-read against the app's own deferred-spawn design. Fixed with a closed `PaneClaim` enum switched exhaustively.
- **BUG B — traffic lights intermittently double-spaced.** "The old `TrafficLightNudger` **captured** a button's origin once and **pinned** it. When AppKit relaid the cluster... the nudger re-applied a *stale captured* origin." Layer: (f) state bug inside (b) unsupported territory — `audit-title-bar.md`: "AppKit's repeated re-laying-out of these buttons on focus/resize (which the code explicitly defends against) is exactly the warning sign that this is unsupported territory." Third generation of this code: nudger → nudger + full-screen observers (`docs/done/in-place-refactor-handoff.md` #2) → `TrafficLightPlacer` (compute absolute target per frame event).
- **BUG C — pill drag moved the window (again).** "The old machinery cooperated badly: a process-wide double-click monitor, a SwiftUI `DragGesture` that fished `NSApp.keyWindow`, and a one-bit `WindowDragGate` flag the pill press flipped to 'yield.' When the flag stuck (or in a torn-off window where the veto failed), a pill drag dragged the window." So the S3 UPDATE 3 solution + S4 gate — both "solved, tested" in their day — failed a third time. Layer: (f) band-aid design, but each band-aid existed only because of (c) (no single place on either side of the boundary sees the whole press). Final fix, `ChromeEventRouter`: one process-wide monitor classifying each `.leftMouseDown` once. Even the final router hit a fresh (c) landmine: "SwiftUI resolves an empty-toolbar press to a transparent hosting wrapper that is a sibling *above* the strip — never a descendant — so a pure class-walk dead-spotted empty-toolbar drag + double-click zoom," requiring an attribute-walk fallback on `mouseDownCanMoveWindow == true`.

**Cost of the window-drag arbitration alone, end to end:** solved three separate times (UPDATE 3 gesture-yield → `WindowDragGate` → `ChromeEventRouter`), across at least 6 handoff/decision docs, before landing on "chrome state is computed per event, never remembered" and "one arbitration point per press."

### S6. The native-container escape hatch was measured and doesn't exist

**Attempted:** move the custom 52pt band into `NSTitlebarAccessoryViewController` so AppKit provides drag regions, traffic lights, zoom, and full-screen behavior natively (`docs/research/refactor-recommendation.md` SPIKE RESULTS).

**Symptom:** hard blockers, measured: accessory height "**capped by the title bar** — `.top` = **32pt**, `.bottom` = **36pt** — and is *invariant* to the requested content height... across three independent sizing techniques"; layouts are two-tier or leave "a **white notch** where the lights are"; "the surrounding **system title-bar area stays system-colored**... and is not themeable"; the full-height sidebar breaks. "Decision: rejected, on spike evidence (not deferred)."

**Layer:** (b)/(a) — the native framework structurally cannot render the app's design. This matters for classification: several audits call the hand-rolled toolbar an anti-pattern and "the upstream cause of nearly every other finding" (`audit-title-bar.md`), which would make the pain (f). The spike closes that argument: the hand-rolled chrome was **forced**, so the downstream pain (drag regions, traffic lights, zoom, full-screen — all re-synthesized by hand) is properly charged to the frameworks, not the app.

### S7. Double-click-to-zoom

**Attempted:** double-click empty chrome zooms, like a real title bar.

**Symptoms:** the native `mouseDown`-override approach was "abandoned as unviable under SwiftUI hosting" (documented in `WindowDragRegion.swift`'s header, cited in `draggable-pane-pills-handoff.md`), forcing a process-wide `NSEvent` monitor (`TitleBarZoomMonitor`); the monitor ignored the user's `AppleActionOnDoubleClick` preference (`refactor-recommendation.md` High #1 — (f) completeness gap in a hand-re-implementation of free native behavior); the zoom UITest was "environmentally red" (launches maximized, so zoom toggles to the same size) and later "leaves the window zoomed/full-screen for the next test" (`pill-drag-window-move-decision.md`) — (e). Resolution: pref honored in the in-place refactor; monitor eventually absorbed into `ChromeEventRouter`.

### S8. Restored secondary pane hangs (`docs/done/restored-secondary-pane-hangs.md`)

**Attempted/symptom:** after relaunch, clicking a non-active restored pane pill hangs on "Launching terminal…" forever; 234 orphaned `/bin/zsh` processes with PPID 1 found on the machine; bug is machine-environmental (same build works on prod install and another machine).

**Layer:** (f) — pty/process lifecycle (children not terminated on teardown, accumulating orphans) plus possibly a SwiftUI-mounting interaction hypothesis ("the click happens before the view is mounted... maybe SwiftTerm needs the view live to pump bytes"). Included for honesty: this is a real documented struggle that is **not** UI-framework-caused. It's process/resource lifecycle in app code.

### S9. Swift-the-language incidents (the complete list)

Searching all 14 docs, the only language-caused friction documented is one item, repeated verbatim across three handoffs: "**Swift 6 + `@MainActor` XCTestCase gotcha:** calling an actor-isolated helper from `setUp()` trips 'Sending `self` risks causing data races'. Seed fixtures **inline in `setUp`**" (`pill-drag-plain-approach-handoff.md`; also `pill-drag-window-move-decision.md`, `draggable-pane-pills-handoff.md` era). It was diagnosed once, worked around in-line, and never blocked anything. No doc records a type-system, generics, compiler-performance, or language-semantics failure. The recurring **tooling** noise is separate: SourceKit phantom diagnostics after adding files ("the build is the source of truth — don't chase those") appears in six docs; that is (e) and was friction, not failure.

---

## Part 2 — Synthesis

### Recurring failure mechanisms, deduplicated and ranked by recurrence

1. **Press arbitration between the native window-drag tracker and in-content interactive views** (≥6 distinct episodes: S1 three-layer suppression, S2 collapse, S3's 9 attempts, S4 yield regression, S5 BUG C, final router dead-spot). API surfaces: `NSWindow.isMovable`, `NSView.mouseDownCanMoveWindow`, `NSWindow.performDrag(with:)`, the native title-bar drag tracker under `.hiddenTitleBar`/`.fullSizeContentView`, SwiftUI `DragGesture` / `.onDrag` / `.highPriorityGesture`, local `NSEvent` monitors, `NSView.hitTest`.
2. **SwiftUI hosting internals are opaque and untaggable from AppKit** (≥5 episodes: hit-test won't descend into embedded veto views; private hosting wrappers inherit `mouseDownCanMoveWindow=true` and can't be subclassed; `hitTest` chains return inconsistent internal classes (`NSClipView`, `PlatformGroupContainer`); `accessibilityHitTest` returns a flat `AXGroup` everywhere; the final router's sibling-not-descendant dead spot). API surfaces: `NSHostingView` and its private descendants, `NSView.hitTest`, `accessibilityHitTest`, `NSClipView` occlusion of background views.
3. **Drag-session lifecycle split across the two frameworks** (≥4: `didDropOnTarget` flag because `DropDelegate` can't return an `NSDragOperation` the source sees; cleanup owned only by `endedAt`; coordinate-space ambiguity `DropInfo.location` vs screen points; missing `concludeDragOperation` analog; the tear-off trigger existing only on the AppKit side while drops live on the SwiftUI side). API surfaces: `NSDraggingSource.draggingSession(_:endedAt:operation:)`, SwiftUI `DropDelegate`/`.onDrop`/`NSItemProvider`, `NSPasteboard` custom UTIs, `performDragOperation`.
4. **Re-synthesizing native chrome by hand because native containers can't host the design** (≥4: accessory spike blockers; traffic-light nudging across three generations incl. BUG B; zoom monitor incl. `AppleActionOnDoubleClick`; full-screen band handling absent). API surfaces: `NSTitlebarAccessoryViewController`, `standardWindowButton(_:)` + `setFrameOrigin`, full-screen `NSWindow` notifications, `.windowStyle(.hiddenTitleBar)`/`.plain`, `windowBackgroundDragBehavior`.
5. **State-flag band-aids that drift** (≥4: `WindowDragGate` stuck bit; `pendingTearOff` global slot / FIFO pairing; captured-then-pinned traffic-light origins; drag-session set-early-cleared-late). These are app code (f), but every one was invented to bridge a signal the boundary doesn't carry. The final architecture's stated cure is telling: "chrome state is computed per event, never remembered."
6. **Bridge/timing of NSWindow handoff and NSView reparent** (≥3: `WindowAccessor`'s one-runloop deferral replaced by synchronous `WindowBridge`, with two writes still deferred "because they only stick after SwiftUI finalizes the window"; `viewDidMoveToWindow` firing before SwiftUI applies the styleMask; `NiceTerminalView` reparent guards — respawn suppression, Metal layer rebind, first-responder re-arm). API surfaces: `NSViewRepresentable`, `viewDidMoveToWindow`, `NSWindow.styleMask`, `makeFirstResponder`.
7. **Test-harness event synthesis diverging from real events** (≥4: XCUITest drags bypass `NSEvent` monitors; cross-window drop undrivable; `.plain` AX nodes `Disabled`; zoom test environmentally flaky). This is (e), and it materially bent design decisions (the "must be XCUITest-verifiable" constraint nearly forced adoption of `.plain`).
8. **pty/process lifecycle** (1 doc: orphaned shells, deferred-spawn nil trap feeding BUG A). (f), unrelated to UI frameworks.

### Layer verdict (honest count)

Classifying the ~24 distinct documented failure/blocker items above by root cause:

| Layer | Count | Items |
|---|---|---|
| (c) SwiftUI↔AppKit seam | ~11 | hit-test non-descent, untaggable hosting internals, gesture-vs-tracker precedence, monitor selectivity impossibility, hybrid drag lifecycle (`didDropOnTarget`, coordinate spaces, endedAt-only cleanup), yield regression, router sibling dead-spot, window-handoff timing, NSView reparent guards |
| (a) SwiftUI framework limitation | ~5 | no drag-ended-outside callback, `.plain` strips all chrome, `windowBackgroundDragBehavior` semantics (needs exposed background; inert under `.hiddenTitleBar`), `.draggable` can't move live views, `.onDrag` owns session with no end signal |
| (b) AppKit API complexity | ~4 | accessory height caps/unthemeable title area, traffic-light relayout being unsupported territory, `isMovable=false` gating the `mdcmw` path (undocumented interaction), title-bar tracker semantics under `.fullSizeContentView` |
| (f) app architecture / self-inflicted | ~6 | `pendingTearOff` slot design, source-persists-before-destination, captured-then-pinned nudger, BUG A nil force-read, orphaned ptys/pane hang, magic-number duplication + `AppleActionOnDoubleClick` gap |
| (e) tooling/build | ~4 clusters | XCUITest synthesis gaps, flaky zoom test, SourceKit phantoms, slow builds/lock choreography |
| (d) Swift the language | 1 | the Swift 6 `@MainActor`/`setUp()` isolation gotcha — diagnosed once, worked around inline, never a blocker |

So: roughly **80%+ of the documented struggle is framework-layer (a+b+c), with the seam (c) alone the plurality**. The (f) items are real and should be said plainly: the tear-off slot design, the stale-capture nudger, and the pty orphan leak were app bugs an experienced team could have avoided in any stack — but four of the six (f) items are band-aids invented specifically to carry signals across the seam, and the two audits that blamed architecture (`audit-title-bar.md`'s "hand-rolled fake toolbar is the upstream cause of nearly every other finding") were later overturned by measurement (`refactor-recommendation.md`'s spike proved the native container cannot host the design, and `divergence-justifications.md` scored the custom design as justified). Swift the language is essentially exonerated by this record: the pure-Swift model layer (`PaneStripDropResolver`, `TabModel.movePane`, `LivePaneRegistry`, migration/persistence logic) is consistently reported "done and green" on the first pass in every handoff, while the identical feature's *event-routing* half consumed ~11 approaches and three "final" solutions. Nick's assertion matches the evidence.

### Notable second-order finding

The docs also show a **debuggability** cost distinct from any single bug: three separate recorded instances of confidently-wrong intermediate conclusions later reversed by instrumentation ("mdcmw survives isMovable=false" — wrong; "performDrag isn't XCUITest-automatable" — wrong; the plan's "do NOT reintroduce isMovable=false" — reversed), plus the need for a PreToolUse hook + invariant comments to stop future sessions from silently regressing the yield. The seam's behavior was un-reason-about-able enough that the project resorted to guardrail automation and differential UITest pairs as the only source of truth.

---

## Part 3 — Task families to seed the A/B experiment

Criteria applied: NEW features (reorder, tear-off, cross-window move, traffic-light placement, double-click zoom all shipped), each sized to one agent session, each exercising a seam that the docs show caused documented pain, each with a meaningful Rust+GPUI equivalent.

1. **Drop-to-split:** drag a pill into the terminal content area; live half-pane highlight overlays (left/right) show the split target; drop creates a split. Seam: drag session over non-strip content, overlay hit-testing (`allowsHitTesting(false)` layering), `DropInfo.location` coordinate conversion between the strip's coordinate space and content views — mechanisms #1/#3 (the `NSClipView` occlusion and coordinate-space episodes). GPUI side: drag payloads + hover-target overlays are first-class.
2. **Animated placeholder-gap reorder + edge auto-scroll:** during a pill drag, neighboring pills slide aside to open a gap, and the strip auto-scrolls when the drag nears an edge. Explicitly documented as unimplemented: "pills never animate aside / leave a placeholder gap. This is the Chromium [9] anti-pattern" and "the strip *can* scroll, but doesn't auto-scroll under a drag" (`audit-draggable-tabs.md`). Seam: animation during an active drag session, hit-testing against mid-animation frames (`draggable-tabs-best-practices.md`'s bisection caveat), ScrollView vs drag-event routing — mechanisms #1/#2. GPUI: animated layout during drag is the classic tab-strip demo.
3. **Eager tear-off preview:** once a pill drag leaves the strip by a tear distance, show a translucent floating preview window that follows the cursor and re-docks on hover over a strip (Chromium's overlay-window model). Documented as the deferred Phase 3 that "would also obviate `pendingTearOff` plumbing entirely" (`audit-draggable-tabs.md`), with the docs warning what goes wrong ("a window glued to the cursor, an unfinished drag session, and weird key-window state" — `draggable-tabs-best-practices.md`). Seam: NSWindow creation mid-drag-session, key/main state, drag-session continuity — mechanisms #1/#4/#6. GPUI: spawn/move a borderless window during a drag.
4. **Pill hover preview panel:** hovering a pill for 500ms shows a non-activating floating panel with a pane snapshot/summary, anchored under the pill and dismissed on drag start. Seam: extracting a SwiftUI view's screen frame across the bridge, `NSPanel` non-activating key/main behavior, hover vs drag vs tap disambiguation on the same pill — mechanisms #2/#6 plus the existing pill-gesture pileup (`onTapGesture`, rename gate, close-X, `.onDrag`). GPUI: anchored overlays/popovers are native to the framework.
5. **Second draggable chrome surface:** a new bottom status bar (or per-pane footer) with interactive widgets, where empty areas drag the window and double-click follows the system title-bar action. This replays the entire press-arbitration cluster (mechanism #1 — the single most recurrent pain, S2/S3/S5) on a surface the `ChromeEventRouter` wasn't built for, testing whether the "one arbitration point" architecture extends or was overfit to the top band. GPUI: window-drag regions are explicit API.
6. **Cross-window drag-target glow:** while a pill drag is in flight, every *other* window's strip renders a highlighted drop-affordance live, cleared on drag end however it ends. Seam: broadcasting in-flight drag state across per-window state trees, cleanup on the `endedAt`-only path (the documented session-leak shape, mechanism #3/#5), multi-window testability (the documented XCUITest cross-window gap). GPUI: multi-window shared state in one process.
7. **Widget in the traffic-light row:** a small pin/lock button rendered inline with the traffic lights that keeps exact placement across focus changes, resize, and full-screen enter/exit. Replays the BUG B territory (mechanism #4: `standardWindowButton` geometry, full-screen notifications, per-event recompute) on a new element without touching shipped code. GPUI: it's just a button in the titlebar area — which is precisely the asymmetry the experiment wants to measure.
8. **Drag-out as file export:** dragging a pill to the desktop with Option held produces a transcript file (`NSFilePromiseProvider`) instead of a tear-off; without Option, tear-off behaves as today. Seam: two drag semantics from one source, `sourceOperationMaskFor` inside/outside application, coexistence with the shipped `NSDraggingSource` — mechanism #3. GPUI equivalent (drag-out to the OS) is also a real test of GPUI's platform-drag maturity, which makes this family double as a probe of where GPUI is *weaker*.

Where the docs say the pain concentrated, families 1/2/5 hit it hardest (press arbitration + hit-test opacity + coordinate spaces account for the plurality of documented episodes); 3 and 6 hit the tear-off/window-birth and lifecycle-cleanup cluster; 4 and 7 hit chrome re-synthesis; 8 probes the drag-session lifecycle split and doubles as a GPUI-weakness control. For A/B fidelity, pair each Swift-side task with the same acceptance UITest shape the docs converged on (differential pairs: "the new interaction works" AND "empty chrome still drags / window doesn't move"), since the record shows compiles-and-unit-passes was repeatedly consistent with behaviorally wrong.
