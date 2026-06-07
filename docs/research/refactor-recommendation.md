# Top-bar + Sidebar Refactor — Single Reconciled Recommendation

Synthesis of the five research/audit docs in this folder:

1. `custom-macos-toolbar-best-practices.md` — toolbar best-practices research
2. `macos-full-height-sidebar-best-practices.md` — sidebar best-practices research
3. `toolbar-gap-analysis.md` — toolbar code audit vs. best practices
4. `sidebar-gap-analysis.md` — sidebar code audit vs. best practices
5. `divergence-justifications.md` — adversarial defense of the current design

Scope: **refactoring only.** Drag-to-reorder pane pills is explicitly out of scope
here (it's a feature that rides on top of a healthy bar; see
`toolbar-gap-analysis.md` for that track when we get to it).

---

## ✅ SPIKE RESULTS (2026-06-06) — native accessory ruled out, with evidence

We built a throwaway `NSTitlebarAccessoryViewController` spike
(`Sources/Nice/TitlebarSpike.swift`, preserved at git tag `spike/native-titlebar`)
to settle, with real evidence, whether the native title-bar accessory could
reproduce Nice's custom 52pt band. **It cannot.** Three of the five questions came
back as hard blockers. Screenshots in `docs/research/spike/`; raw logs in
`docs/research/spike/quantitative-log-results.txt`.

- **Q1 — Height: BLOCKER (measured).** The accessory height is **capped by the
  title bar** — `.top` = **32pt**, `.bottom` = **36pt** — and is *invariant* to the
  requested content height (52pt and 80pt produced identical results) across three
  independent sizing techniques (Auto Layout constraint, SwiftUI intrinsic frame,
  explicit `NSView.frame`). It will not host a 52pt unified band. (This also
  corrects the earlier hand-wavy "~28pt" figure to the measured 32/36.)
- **Q2 — Traffic-light position / unified band: BLOCKER (screenshots).** Neither
  layout reproduces the single unified band with inline-centered lights.
  `.bottom` is overtly **two-tier**: a white system title row with the traffic
  lights on top, themed band as a separate strip below
  (`spike-bottom-themed-sidebar.png`). `.top` puts the band in the title row beside
  the lights but leaves a **white notch** where the lights are — the band can't
  paint behind/around them (`spike-top-themed-sidebar.png`).
- **Q3 — Theming: BLOCKER (screenshots).** The accessory band *itself* takes an
  arbitrary non-system color fine (it's a SwiftUI fill). But the surrounding
  **system title-bar area stays system-colored** (white in light mode) and is not
  themeable — producing a seam between the Catppuccin band and the system chrome
  (`spike-top-themed-material.png`). The system material does not "fix" this.
- **Q5 — Sidebar overlap: NEGATIVE.** With the accessory present, the full-height
  floating sidebar no longer reaches the top — a white title strip sits above it,
  breaking the floating-card-with-lights-overlaid look Nice has today.
- **Q4 — Free behaviors: not separately verified, and moot.** The native title bar
  would provide full-screen reflow / move / zoom, but Q1–Q3 already rule the
  approach out, so this gain is irrelevant.

**Decision: keep the custom `.hiddenTitleBar` chrome. Do NOT migrate to
`NSTitlebarAccessoryViewController`.** This is no longer a judgment call — the
native accessory provably cannot render Nice's design. Proceed with the in-place
correctness refactor below. The `NSTitlebarAccessoryViewController` option from the
gap-analysis docs is **closed** (evidenced), not merely deferred.

---

## TL;DR — the single recommendation

**Do a focused "chrome-correctness + de-duplication" refactor. Do NOT do a
foundational rewrite.**

The thing that makes this toolbar scary to extend is **not** its architecture —
it's a small, well-defined cluster of latent window-chrome bugs plus duplicated
magic numbers and implicit coupling. Fix those and establish single sources of
truth, and the bar becomes safe to build on. Leave the custom pills, the floating
sidebar card, the custom source list, and the custom palette/materials alone —
they're required by what Nice is, or already on-pattern.

Concretely, in priority order:

- **High (real bugs):** honor `AppleActionOnDoubleClick`; re-apply traffic-light
  offsets on full-screen transitions; handle the band in full screen at all.
- **Medium (structural fragility, and a prerequisite for the High items):** one
  shared constant for the 52pt band height; couple the collapsed-cap geometry to
  the traffic-light offset; make the zoom region derive from the shared constant.
- **Low (polish):** animate sidebar collapse/expand, gated on Reduce Motion.

And the one big contested call: **defer the `NSTitlebarAccessoryViewController`
migration** (the gap docs' headline recommendation). It's a large, risky rewrite
with a real height-mismatch problem and unclear payoff, and every correctness bug
above is fixable in place without it.

---

## The reframe (why the original hypothesis was mostly wrong)

The investigation started from "our implementation is flawed / non-standard, and
that's why it's hard to extend." Reconciling all five docs, that's **mostly not
true**:

- The **visual/architectural divergences are justified or on-pattern.** The
  adversarial pass scored them 4 Strong / 1 Moderate / 1 Weak / 2 conceded, and
  the 2 conceded are *bugs*, not architecture. (`divergence-justifications.md`.)
- The sidebar is the **same floating-card pattern as Apple Music / Finder / Xcode**
  (and Tahoe's default) — not a divergence at all. (`sidebar-gap-analysis.md`,
  after correction.)
- The real debt is a **chrome-correctness cluster** that *both* gap analyses
  independently converged on from opposite ends (toolbar side and sidebar side).

So the refactor is narrow and targeted, not a re-foundation.

---

## Keep as-is (do NOT "fix" toward native)

Each of these would *cost* Nice something concrete if migrated; leave them.

| Element | Why it stays | Evidence |
|---|---|---|
| Custom pane pills (not `NSWindowTabGroup`) | Native tabbing needs one `NSWindow` per tab → would invert a single-window app into N-windows-per-pane, and forfeit theming + per-pill status/rename/close/overflow. Panes do **not** share a pty; each has its own — but only the active one is mounted. | `divergence-justifications.md` §2; `AppShellView.swift:639-643` |
| Floating inset sidebar card (not flush `NavigationSplitView`) | It *is* the modern canonical pattern (Music/Finder/Xcode/Tahoe). No seam, no tracking-separator needed. | `sidebar-gap-analysis.md` P1 |
| Custom `ScrollView`+`VStack` source list (not `List(.sidebar)`) | Branch-lineage tree, per-row pty status dots, custom DnD, theme tints exceed what `.listStyle(.sidebar)` cleanly supports. | `divergence-justifications.md` §6 |
| Custom palette/materials (not native vibrancy only) | Nice ships non-system themes (Catppuccin/Nord/…). It already uses native `.sidebar`/`.behindWindow` vibrancy **where correct** (the `.macOS` palette). "Native only" is incompatible with multi-theme. | `SidebarBackground.swift:29-33`; `divergence-justifications.md` §7 |
| Window-drag region (`mouseDownCanMoveWindow`) | Already correct and on best-practice; sits behind the pills so it won't fight pill interaction. | `WindowDragRegion.swift:56-58` |

---

## The refactor plan (prioritized)

### High — correctness bugs (conceded; no requirements defense)

1. **Honor `AppleActionOnDoubleClick` in double-click-to-zoom.**
   `TitleBarZoomMonitor` unconditionally calls `performZoom`
   (`WindowDragRegion.swift:99`), ignoring the user's preference
   (`Maximize`/`Minimize`/`None`). Read the pref live from `NSGlobalDomain` and
   branch to `performZoom` / `performMiniaturize` / no-op. This is the toolbar
   best-practices doc's single most common custom-bar regression.

2. **Re-apply traffic-light offsets on full-screen transitions.**
   `TrafficLightNudger` re-applies on `didBecomeKey` + `didResize` only
   (`TrafficLightNudger.swift:89-109`); macOS also resets custom button positions
   on full-screen enter/exit. Add the four `NSWindow` full-screen notifications
   alongside the existing observers. **Smallest, highest-value change** — one file.

3. **Handle the custom band in full screen at all.**
   No full-screen handling exists anywhere (`grep FullScreen Sources/` is empty).
   The title-bar band changes/auto-hides in full screen; the hard-coded 52pt band,
   safe zone, and zoom gate don't track it. Needs a manual pass to confirm the
   exact misbehavior, then a branch that recomputes (or hides) the band. Depends on
   Medium #4.

### Medium — structural fragility (reduces future bug surface; unblocks the above)

4. **One shared constant for the 52pt band height.**
   `52` is duplicated independently in the card spacer (`AppShellView.swift:390`),
   the window background band (`AppShellView.swift:608`), and the zoom-monitor gate
   (`WindowDragRegion.swift:88`), with no shared source of truth. Fold into one
   constant. **Prerequisite for #3** (you can't make the band full-screen-aware
   while its height lives in three places).

5. **Couple the collapsed-cap geometry to the traffic-light offset.**
   The collapsed cap's leading reserve (`AppShellView.swift:549`) and the nudge
   offset (`AppShellView.swift:189`) are independently tuned magic numbers that
   silently drift if either changes. Derive both from one geometry source.

6. **Make the zoom region derive from the shared constant.**
   The zoom monitor is a process-wide `leftMouseDown` hook gated on the magic `52`
   (`WindowDragRegion.swift:87-88`). At minimum, source the gate from #4 so it
   can't desync; consider whether the process-wide hook is still the right shape
   once #1/#3 are in. (Note: the monitor has a UITest-backed rationale that the
   "obvious" native `mouseDown` fix fails in SwiftUI hosting — don't naively
   replace it; see `divergence-justifications.md` §3.)

### Low — polish

7. **Animate sidebar collapse/expand.**
   `toggleSidebar()` flips a Bool and the shell swaps with a bare `if`
   (`SidebarModel.swift:44-46`, `AppShellView.swift:340-347`) — the sidebar snaps.
   Wrap in `withAnimation` / add `.animation(value:)`, gated on
   `accessibilityReduceMotion`. Brings it in line with native `toggleSidebar` feel.

---

## The one big decision: `NSTitlebarAccessoryViewController` — CLOSED (spiked)

**The gap docs' headline recommendation was to stop using `.hiddenTitleBar` + a
from-scratch bar and host the custom strip in an
`NSTitlebarAccessoryViewController`, to get traffic-light placement, full-screen
transitions, window-move, and double-click-zoom "for free."**

**Decision: rejected, on spike evidence (not deferred).** See the SPIKE RESULTS
section at the top. The spike proved the accessory **cannot** reproduce Nice's
design, on three independent grounds:

- **Height is capped, measured.** `.top` = 32pt, `.bottom` = 36pt, invariant to the
  requested height across three sizing techniques — it cannot host the 52pt band.
  (The earlier "~28pt would clip" worry was directionally right but imprecise; the
  real numbers are 32/36.)
- **Forced two-tier / unthemeable system chrome.** `.bottom` gives a white system
  title row above a separate band; `.top` leaves a white notch at the traffic
  lights. The system title area can't be themed, so non-system palettes seam
  against it. (Screenshots in `docs/research/spike/`.)
- **Breaks the full-height floating sidebar.** A white title strip ends up above the
  sidebar card.

This closes the question that the adversarial pass had only flagged as
*medium-confidence*. No revisit criteria — the constraint is structural to the
native title bar, not a tuning problem. The in-place correctness refactor (#1–#7)
is the path.

---

## Suggested sequencing

1. **#4 (shared 52pt constant)** first — small, and it unblocks #3 and #6.
2. **#2 (full-screen traffic-light observers)** — isolated one-file quick win,
   closes the highest-value named pitfall.
3. **#1 (`AppleActionOnDoubleClick`)** — isolated, in the same file you're already
   touching for #6.
4. **#3 (full-screen band)** — needs a manual full-screen test pass first to see
   the actual misbehavior; do after #4.
5. **#5, #6** — fragility cleanup once the constant exists.
6. **#7 (collapse animation)** — independent polish, any time.

This order front-loads the cheap, high-confidence wins and defers the one item
(#3) that needs manual observation before its scope is known.

---

## Must verify manually (not determinable from code)

- **Full-screen behavior** of the band + traffic lights — no observers exist, so
  the exact breakage (button revert, stale band) is *predicted* but unconfirmed.
  Test enter/exit full screen before sizing #3.
- **`AppleActionOnDoubleClick` = Minimize / None** paths after #1 — confirm zoom,
  minimize, and no-op all behave per the system pref.

---

## Bottom line

The "move toward best practices" plan is **right but narrow**. Almost everything
custom about Nice's bar and sidebar is justified or already on-pattern; the payoff
is concentrated in fixing a handful of chrome bugs and removing the duplicated
constants / implicit coupling that make the code feel fragile. That cleanup — not
a foundational rewrite — is what will make the bar safe to extend (including the
later pill-reorder work). Skip the `NSTitlebarAccessoryViewController` migration
unless the in-place fixes prove insufficient.
