---
title: Claude chrome-tractability A/B — SwiftUI/AppKit-on-Nice vs GPUI-on-PoC — protocol v2
date: 2026-07-02 (v2, same-day revision; v1 in git history)
status: DESIGN v2 — awaiting Nick's approval; no runs executed
closes: rewrite-stack-research.md §13 gap #3 / audit G3
supersedes: v1 of this file — v1 mis-aimed the premise at the language axis and chose seam-free tasks
---

# The chrome-tractability A/B: SwiftUI/AppKit-on-Nice vs GPUI-on-PoC

## 0. Why v2 — the premise correction (Nick, 2026-07-02)

v1 framed the tested premise as the language axis ("D1 Swift 3 vs Rust 5"),
inheriting audit G3's "Claude weak at Swift" phrasing. Nick's correction:
**that is not the claim the rewrite rests on.** Claude is fine at raw Swift;
the documented pain is the UI-framework layer — SwiftUI, AppKit, and above
all the SwiftUI↔AppKit seam. The repo's own record confirms this decisively:
the chrome-pain catalog (`notes/chrome-pain-catalog-20260702.md`, extracted
2026-07-02 from the 14 pill-drag/reorder/tear-off/title-bar docs) classifies
~24 documented failure/blocker items as **80%+ framework-layer, with the
seam alone the plurality (~11 items)**, versus exactly **one**
language-caused item in the entire record (a Swift 6 `@MainActor`/`setUp()`
gotcha, worked around inline, never a blocker). The same features'
pure-Swift model layers (drop resolver, move logic, migration registry,
persistence) were "done and green" first-pass in every handoff, while the
event-routing halves consumed ~11 approaches and three "final" solutions
(the window-drag arbitration alone: gesture-yield → `WindowDragGate` →
`ChromeEventRouter`, plus a PreToolUse guard hook to stop regressions).
This also matches the report's own §2 correction ("every item an artifact
of SwiftUI↔AppKit impedance, which is precisely what a non-SwiftUI chrome
removes") — v1 drifted from it.

Two consequences:

1. **Premise restated (§1).** The tested claim is the impedance cap, not
   language competence. Raw-Swift competence is conceded, not under test.
2. **Tasks resampled (§2).** v1's tasks (activity badge, bell flash) were
   seam-free widget tasks — state→view, a click handler, persistence —
   exactly the class the record shows Swift handles fine. Running them
   would likely have produced a cheap, confident, and **wrong** "premise
   unsupported" verdict without ever touching the layer that motivated the
   rewrite. v2's tasks are drawn from the catalog's documented pain
   mechanisms (v1's badge survives as an optional seam-free *control*).

## 1. The question, sharply

The rewrite's velocity driver, correctly stated: **Claude's effectiveness
building and maintaining Nice-class chrome is capped by the SwiftUI↔AppKit
seam** — press arbitration against the native window-drag tracker, hosting
internals opaque to hit-testing, drag-session lifecycles split across two
frameworks, native chrome re-synthesized by hand — **and a single-framework
GPUI chrome removes that cap** (one owner for window, event routing, and
view tree). That claim gates the rewrite; it has never been measured
head-to-head. The audit's secondary note stands: Xcode 26.3's agent
integration erodes the Swift side's *tooling* handicap, which §3/branch 4
handles.

**What is actually measurable.** As in v1: no task isolates "the seam" from
framework corpus or codebase shape; every session exercises the composite.
The experiment measures *Claude's end-to-end effectiveness on
chrome-interaction work in each stack as it would really be practiced* —
with the tasks sampled **from the documented failure distribution** (the
pain class that motivated the rewrite), not from the safe widget interior.
A composite difference on this class is what the decision actually needs.

**Decision rule — fixed now, before any data:**

Per run: (a) the **objective gate** — feature-complete: builds clean, all
functional-checklist items pass **including the differential invariants**
(§4), no regressions; and (b) the **graded composite** — mean of the five
anchored dimensions in §4, median across judges, then across runs.

1. **Swift ≥ Rust** (Swift passes the objective gate wherever Rust does,
   and graded composite Swift ≥ Rust) **on the seam-class tasks**: the
   impedance-escape driver is **unsupported on current evidence**. The
   rewrite's velocity justification is void; Path B must be re-argued from
   the §11 quality criteria alone (and G4's still-unwritten user-visible
   quality deltas) before any commitment.
2. **Rust > Swift, weakly** (both pass the objective gate; graded gap
   < 1.0, or direction not replicated on T2): premise **weakly supported**.
   The rewrite case carries on the quality criteria plus a measured-but-
   small velocity edge; that shift is flagged to Nick explicitly.
3. **Rust ≫ Swift** (Swift DNFs the objective gate — including failing a
   differential invariant — where Rust passes on the same task, or a graded
   gap ≥ 1.0 replicated on both tasks): premise **supported**. Date-stamp
   the measured values into the report and proceed.
4. **Xcode contingency (asymmetric on purpose, unchanged from v1):** both
   arms run in Claude Code CLI, the Swift arm's weaker real-world tooling.
   A Swift win is conclusive as-is (it won handicapped); any Rust-favoring
   outcome cited in a commitment memo first requires **one Swift
   replication inside Xcode 26.3's agent integration**.

**Power honesty (unchanged).** n=2/arm on the primary is a screen for a
claimed *decisive* gap, not a measurement of small effects. A gap too
subtle to show here is too subtle to carry a hard gate.

**v2 scoring note.** The record's recurring failure shape was "compiles +
unit-passes while behaviorally wrong" (the tear-off yield regression
shipped green twice). Hence the objective gate carries paired differential
invariants — "the new interaction works" AND "the old surfaces still
behave" — checked in the same run, not just feature checklists.

---

## 2. Task design (v2)

Requirements: chrome-INTERACTION-flavored (the documented pain class, per
the catalog), implementable in **both** stacks in one agent-session,
present in **neither** codebase today (grep-verified 2026-07-02: no
bottom/status bar and no pin/always-on-top widget on either side), a
comparable seam on both sides, minimal dependence on scaffolding only one
side has, and each task must cite the documented pain mechanism it
replays (catalog Part 2 numbering).

### T1 (PRIMARY) — Bottom status bar with press arbitration

*Brief (identical text both arms; only the repo-path/build-command block
differs):*

> Add a **bottom status bar** to the main window: full window width, ~28 pt
> tall, visually matched to the existing chrome (colors, typography,
> spacing). Left side: a text widget showing the active session's working
> directory (a sensible static placeholder is acceptable if the codebase
> does not expose a cwd). Right side: a clock widget (HH:MM, updating each
> minute). Behavior: (1) clicking the left widget copies its text to the
> clipboard with a brief visible confirmation; (2) pressing and dragging on
> any empty (non-widget) area of the bar **moves the window**, exactly like
> the title bar; (3) double-clicking an empty area performs the same action
> as double-clicking the title bar (honoring the user's system preference
> where the platform exposes one); (4) pressing, dragging, or clicking the
> widgets must **never** move the window. Do not regress existing behavior
> — in particular existing window-drag surfaces, pill interactions, and
> terminal input.

- **Replays:** catalog mechanism **#1** (press arbitration between the
  native drag tracker and in-content interactive views — the most recurrent
  documented failure, ≥6 episodes across S2/S3/S5) and part of **#4**
  (re-synthesized title-bar behaviors: the double-click action, the
  documented `AppleActionOnDoubleClick` gap). On the Swift side it also
  tests whether the shipped one-arbitration-point architecture *extends* to
  a second chrome surface or was overfit to the top band (catalog family
  5). On the GPUI side, window-drag regions are explicit API — if that
  asymmetry is real, it is precisely the measured construct.
- **Why fair:** neither side has any bottom bar (grep-verified). Both arms
  build the surface fresh; both arms' briefs carry the same acceptance
  criteria, including the differential invariants, as part of the feature
  request itself (not as coaching).
- **Rough size:** ~200–350 LOC Swift; ~150–300 LOC Rust.

### T2 (REPLICATION) — Inline traffic-light-row widget

*Brief:*

> Add a small **"pin" toggle button** rendered inline with the window's
> standard traffic-light buttons, immediately to their right, matching
> their size and vertical alignment. Clicking toggles a visible
> active/inactive state on the button. The button must hold its exact
> placement through window resize, focus loss/regain, and full-screen
> enter/exit. Do not regress traffic-light behavior (close/minimize/zoom
> keep working, with their normal hover effects).

- **Replays:** catalog mechanism **#4** — the BUG-B territory:
  `standardWindowButton` geometry, AppKit's re-layout of the button cluster
  on focus/resize (explicitly called "unsupported territory" in the
  audits; Nice's placement code is on its third generation), full-screen
  transitions (catalog family 7). GPUI side: the titlebar row is
  framework-owned (`TitlebarOptions`), so this is *expected* to be nearly
  trivial there — again, the asymmetry is the construct. Different
  mechanism family from T1 (chrome re-synthesis vs press arbitration), so
  a replicated direction generalizes.
- **Why fair:** no such widget on either side (grep-verified). The button's
  action is deliberately self-contained (toggles its own visual state) so
  neither arm depends on window-level features the other may not expose.
- **Rough size:** ~120–220 LOC Swift; ~60–140 LOC Rust.

### C0 (OPTIONAL CONTROL) — v1's stream-activity badge, seam-free

v1's frozen T1 brief (activity badge: throughput label, active/idle
dimming, click-to-toggle presentation, persisted), retained unchanged as a
**seam-free control**: pure state→view + interaction + persistence, no
window-drag arbitration, no native-chrome geometry. Purpose: calibrate the
instrument. Per the corrected premise the expectation is **both arms pass
comfortably**. If the Swift arm struggles even on C0, the impedance framing
(or the harness) is wrong and the seam results can't be read at face value;
if Swift aces C0 and loses T1/T2, the pain is isolated to the seam — the
cleanest possible signature. Objective gate only (no judge panel), n=1/arm,
to keep it cheap.

### T3 (spare) — Option-drag pill export (OS drag-out)

Catalog family 8: Option-dragging a pill to the desktop exports a
transcript file (`NSFilePromiseProvider` on the Swift side) while plain
drag keeps today's tear-off; the GPUI equivalent probes GPUI's *own*
platform drag-out maturity — a deliberately GPUI-weak surface, kept as the
fairness spare. Activates only via the broken-brief/miscalibration
protocol (§5).

**Recommendation: run T1 (n=2/arm) + T2 (n=1/arm); C0 optional (n=1/arm).**

---

## 3. Fairness controls

Carried from v1 verbatim unless noted:

- **Identical briefs modulo stack.** The brief texts above are frozen by
  this document before any run. Each arm's brief appends only a three-line
  factual block: repo worktree path; where the chrome lives ("the top bar
  is `Sources/Nice/Views/WindowToolbarView.swift`; the window container is
  built under `Sources/Nice/`" / "the interactive window is built in
  `spikes/phase0-poc/src/gpui_term.rs`, `run_interactive()`"); and the
  standard build/run command (`scripts/install.sh` + launch Nice Dev under
  the worktree lock / `NICE_POC_INTERACTIVE=1 cargo run --bin gpui-term`).
  Equal-specificity infrastructure pointers, not solution hints (the Swift
  block does NOT name `ChromeEventRouter` or the drag machinery; finding
  the house arbitration pattern is part of the measured work, exactly as
  finding GPUI's drag-region API is on the other side). No mid-run
  coaching; symmetric fixes only for harness breakage.
- **Real codebases, both arms** (unchanged): Nice `Sources/Nice` (~27k LOC)
  vs `spikes/phase0-poc` bin `gpui-term` (~3.1k LOC on vendored gpui
  0.2.2). The 0.2.2-vs-production-pin limitation and its known-sign bias
  stand (§6-T8), with the same sensitivity check if the margin matters.
- **Each arm keeps its native institutional environment (v2 clarification).**
  The Swift arm may read `docs/` (including `docs/research/` — that lore is
  the arm's real institutional memory, and a real maintenance session would
  use it) and runs with the repo's guard hooks active
  (`.claude/hooks/guard-window-drag.sh` fires if it edits the toolbar —
  that is the real environment, not contamination). Both arms are scoped
  OUT of `notes/` (the rewrite research + this protocol live there and
  would reveal the hypothesis). The GPUI arm's institutional environment is
  the PoC's README/harness docs. Asymmetric in content, symmetric in kind:
  each arm gets what its real job would have.
- **Mature-app vs PoC asymmetry — named, two-sided (unchanged in
  disposition).** v2 tasks are new-surface tasks on both sides (no bottom
  bar, no titlebar widget anywhere), so neither arm inherits a scaffolding
  head start beyond its real-world one: Nice's arbitration machinery is
  simultaneously an asset (pattern to extend) and a hazard (invariants to
  regress); the PoC's blank slate is simultaneously freedom and missing
  infrastructure. Both directions are the two real jobs being compared.
- **Same model, effort, budget** (unchanged): same frontier model
  (claude-fable-5 as of this design), same reasoning effort, 100
  tool-call-turn / 3 h caps, isolated worktrees, web access denied
  symmetrically.
- **Same definition of done, told to both agents** (unchanged in shape):
  compiles clean; feature works per the brief incl. its stated invariants
  (agent self-verifies by running the app); existing behavior unregressed
  (Swift: targeted `scripts/test.sh` suites green; Rust: existing
  `gpui-term` headless modes still build and run); code matches the style
  of the files it touches.
- **Xcode 26.3 Agent-SDK tooling: OUT of the main run, IN as contingency**
  (unchanged; decision-rule branch 4).
- **Hypothesis blinding** (unchanged): briefs are routine feature requests;
  no mention of experiment, comparison, or rewrite; `notes/` scoped out;
  residual inference risk accepted (§6-T6); judges blind to the decision
  framing (§4).
- **Broken-brief protocol** (unchanged): abort the run, fix both arms'
  briefs identically, restart both arms of that task fresh.

---

## 4. Scoring rubric

### Objective gate (per run; pass/fail + counts — no judgment)

| Item | How measured |
|---|---|
| O1 Builds clean | final tree compiles, zero errors (warnings noted) |
| O2 Feature functional | per-task checklist below, executed on the running app by Nick (~10 min/run) or a verifier agent |
| O3 No regressions + differential invariants | the paired invariants below, same run; plus Swift: targeted `scripts/test.sh` suites green, app launches, existing chrome interacts normally; Rust: `cargo build` all bins + one headless `gpui-term` run produces a sane CSV |
| O4 DNF | budget exhausted before O1–O3 → failure-with-artifact (data, not an abort) |

**T1 checklist:** bar renders, themed plausibly; left widget shows
cwd/placeholder, clock ticks; widget click copies + confirms **without
moving the window**; empty-area drag moves the window; double-click on
empty area performs the title-bar action; window resize doesn't break the
layout.
**T1 differential invariants (all in the same run):** (i) the new bar's
empty-area drag moves the window; (ii) widget presses never do; (iii)
pre-existing drag surfaces unchanged (title-bar/top-band drag still works;
Swift additionally: pill drag still reorders and still never moves the
window — the documented three-times-regressed invariant).
**T2 checklist:** button renders inline with the lights, size/alignment
plausible; toggle state flips visibly; placement exact after: a resize, 3×
focus loss/regain cycles, and a full-screen enter/exit round-trip;
close/minimize/zoom still work with normal hover effects.
**T2 differential invariant:** a 10× focus-cycle storm produces no drift
or double-spacing of the cluster (the documented BUG-B shape) — Swift arm
especially; same check run on the GPUI arm.
**C0 checklist:** as v1 (badge visible; label tracks a `yes`-burst within
~2×; dims ≤ ~3 s after silence; click toggles full↔compact; relaunch
restores; resize OK; existing chrome unaffected).

### Graded dimensions (unchanged from v1; 1–5, anchored)

Edit locality · API hallucination count (counted from transcript) ·
Iterations-to-green (counted) · Human-fixup minutes (judged) · Style
conformance (judged). Same anchors as v1.

### Judge protocol (unchanged)

3 independent judges per judged artifact (T1/T2 only: 6 artifacts → 18
short sessions; C0 is objective-gate-only). Input: frozen brief, final
diff, build log, tool-call sequence, objective-gate results. Judges score
against anchors, no head-to-head; blind to the decision framing.
Per-dimension median across judges, mean across dimensions = the run's
composite. Range ≥2 on any dimension → fourth tie-break judge + note.

### Date-stamping into the report

Results land in `spikes/phase0-poc/RESULTS-spike11p3-<date>.md` (raw
scores, checklists, judge rationales, transcript locations); the report
gets a §12-scorecard row ("Claude chrome-tractability A/B — spike 11.3")
and an annotation on the impedance-driver claims it touches: **"measured
YYYY-MM-DD, claude-fable-5, gpui 0.2.2-vendored (prod would pin zed main),
n=6(+2) sessions"**. (v1's "date-stamp the D1 scores" language is
superseded: what gets stamped is the impedance-axis evidence.)

---

## 5. Execution plan + budget

Roles, prep, caps unchanged from v1 (main session orchestrates only;
worktrees pre-warmed; Swift runs serialize on the worktree lock, Rust runs
parallel to them).

Sessions: **T1×2/arm + T2×1/arm = 6 implementer sessions** (+ C0×1/arm = 8
if the control is approved). Judged artifacts 6 → 18 judge sessions.

**Token budget:** implementers ≤3.0M (+~0.6–1.0M for C0); judges ≤2.0M;
orchestration + write-up ≈0.5M → **≈4–5.5M tokens (≈5–6.5M with C0)**.
Wall-clock ~1 active day; Nick's time ≈1–1.5 h (verification + approval;
T1's double-click-preference check and T2's full-screen/focus cycling are
GUI-manual, ~10 min/run).

Abort criteria unchanged, with v2 renames: both arms DNF T1 → T1
miscalibrated; promote T2 to primary, activate T3. Budget overrun >1.5× →
stop, report partial, decide with Nick.

---

## 6. Threats to validity

| # | Threat | Disposition |
|---|---|---|
| T1 | **Single-task generalization.** | Now sampled *from the documented failure distribution* (two disjoint mechanism families: press arbitration; chrome re-synthesis), n=2 on the primary, plus the C0 control. Still a screen, not a month-6 forecast — unchanged residual. |
| T2 | **Training-data familiarity** (SwiftUI corpus vast, GPUI niche). | Unchanged: part of the measured construct, not a confound. Stated so nobody "corrects" for it post hoc. |
| T3 | **PoC-vs-mature-app asymmetry.** | Unchanged disposition; v2 tasks are new surfaces on both sides, so no shipped scaffolding is directly extended on either arm (Nice's arbitration machinery is discoverable pattern + regression hazard; the PoC has neither). |
| T4 | **Judge bias.** | Unchanged: blind framing, anchored + counted dimensions, objective gate supreme, 3 judges + tie-break, rationales published. |
| T5 | **One-shot variance.** | Unchanged: n=2 primary, replication required for the strong branches, branch 2 absorbs noisy small gaps. |
| T6 | **Demand effects.** | Unchanged: routine-feature-request briefs, `notes/` scoped out, residual accepted (symmetric). |
| T7 | **Harness confound vs real Swift workflow** (no Xcode agent). | Unchanged: known-sign bias against Swift; branch 4 obligates the Xcode replication before any Rust-favoring commitment citation. |
| T8 | **gpui 0.2.2 vs production zed-main pin.** | Unchanged: known-sign bias *for* Rust; Rust-loses is conclusive; narrow Rust win labeled an upper bound + optional pinned-main sensitivity re-run. |
| T9 | **Verification asymmetry** (XCUITest suites vs none). | Unchanged: DoD uses manual functional checklists both arms; each side's native cheap regression mechanism only. Note: XCUITest's documented event-synthesis gaps are exactly why the checklists are executed manually/by driving the real app. |
| T10 | **Task selection bias (NEW)** — tasks chosen from Swift's own failure record could overfit the experiment against Swift. | Named openly: the documented failure distribution *is* the distribution the rewrite decision is about — sampling anywhere else measures the wrong thing (v1's mistake, inverted). Mitigations: C0 seam-free control bounds the instrument; T3 spare targets a GPUI-weak surface (platform drag-out); T1/T2 also engage GPUI's genuinely thin surfaces (window-drag behavior, titlebar geometry, timers/animation), so the tasks are not one-sided-by-construction; and branch 1 of the decision rule gives Swift full credit for winning on its home turf. |

---

## Approval asks (Nick)

1. Approve the **re-aimed premise** (§0/§1): the impedance axis is what's
   tested; raw-Swift competence is conceded, not measured.
2. Approve tasks: **T1 bottom status bar (n=2/arm) primary, T2
   traffic-light-row widget (n=1/arm) replication**, T3 spare. And: run the
   **C0 control** (+2 sessions, ≈+1M tokens) — yes/no?
3. Approve the §1 decision rule as binding before data.
4. Approve budget: ~4–5.5M tokens (~5–6.5M with C0), ~1 active day, ~1–1.5 h
   of your time.
5. Confirm Xcode 26.3 handling (CLI-only main run + contingent Xcode
   replication on any Rust-favoring outcome) — unchanged from v1.
