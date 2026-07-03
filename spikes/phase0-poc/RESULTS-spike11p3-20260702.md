---
title: "Spike 11.3 RESULTS — Claude chrome-tractability A/B: SwiftUI/AppKit-on-Nice vs GPUI-on-PoC"
date: 2026-07-02
protocol: notes/tractability-ab-design-20260702.md v2.2 (APPROVED; §1 decision rule pre-registered)
status: RUN — implementers claude-opus-4-8, judges claude-fable-5, gpui 0.2.2-vendored (prod would pin zed main), n=8 implementer sessions
---

# Spike 11.3 — chrome-tractability A/B results

Eight implementer sessions (T1 bottom-status-bar ×2/arm, T2 traffic-light-row
pin ×1/arm, C0 seam-free control ×1/arm), claude-opus-4-8 both arms, in the
real arm worktrees (`ab-swift-arm` off Nice `Sources/Nice` ~27k LOC;
`ab-rust-arm` off `spikes/phase0-poc` gpui-term ~3.1k LOC, vendored gpui
0.2.2), baseline `8f0dd7f` (Swift baseline advanced to `a1c7e43` for
T2/C0 by two logged harness fixes to `scripts/install.sh` only). Objective
gates executed by independent verifier agents driving the real apps with
CGEvents (AX-trusted), with measured window-frame/pixel evidence; graded
artifacts scored by 3 independent blind claude-fable-5 judges each (18 judge
sessions; per-dimension median across judges; C0 objective-only by design).
Full raw materials per run (frozen briefs as sent, diffs, canonical build
logs, tool-call sequences, objective checklists, judge scorecards,
implementer self-reports, cap analyses): `spikes/phase0-poc/ab-runs-20260702/`.
Artifact code preserved as branches `ab/<run-id>` in the arm worktrees.

## Result matrix

| run | objective gate (§4) | composite (median of 3 blind judges) | turns / wall | notes |
|---|---|---|---|---|
| t1-swift-1 | PASS (verifier 10/10 incl. pill invariant) | **5.0** (unanimous) | 139* / ~51 min | *code final at turn 45; overrun was verification tail incl. 3 TCC-blocked UITest attempts — see Deviations |
| t1-swift-2 | **FAIL — O3 differential invariant**: cwd-widget double-click zooms the window ("✓ Copied" swap shrinks the hitbox; click 2 routes as empty bar; 3× repro + mechanism probe) | **3.8** (unanimous; iterations-to-green anchored 1 = never green) | 67 / ~23 min | unit suite 1318/0 was green — the documented "compiles + unit-passes while behaviorally wrong" shape; identical bug caught+fixed in-session by t1-rust-1 |
| t2-swift-1 | **FAIL — O3 regression**: traffic-light hover glyphs gone (baseline-differential probe: baseline shows glyphs under identical synthetic hover incl. slow-path, artifact doesn't; Finder positive control) | **3.6** (unanimous) | 77 / ~26 min | placement itself flawless: exact +23 px pitch through resize/zoom/focus/2× full-screen round-trips/10× storm; implementer self-reported hover as working — contradicted by ground truth |
| c0-swift-1 | PASS (verifier 6/6) | — (C0 unjudged) | 78 / ~23 min | full mature-app integration (meter through pty→sessions→AppState→toolbar, 7 files); 16/16 + 61/61 targeted tests; throughput tracked live at 1154–1311 KB/s; double relaunch-persistence loop verified |
| t1-rust-1 | PASS (verifier 9/9) | **5.0** (unanimous) | 94 / ~28 min | caught + fixed its own copied-overlay geometry bug via live CGEvent self-verification |
| t1-rust-2 | PASS (verifier 9/9) | **5.0** (unanimous) | 86 / ~22 min | structural `.occlude()` never-move guarantee held behaviorally |
| t2-rust-1 | PASS (verifier 6/6 incl. FS enter/exit round-trip + 10× storm bit-identical) | **5.0** (unanimous) | 74 / ~24.5 min | titlebar row framework-owned (`TitlebarOptions`); pin placement-stable by construction, pitch exactly 23.0 pt |
| c0-rust-1 | PASS (verifier 6/6) | — (C0 unjudged) | 65 / ~20 min | throughput within 2×, dim ≤2 s, persistence via real kill/relaunch |

Per-task aggregation (mean of five §4 dimensions, median across judges, then across runs):

| task (mechanism family) | Swift | Rust | graded gap | objective |
|---|---|---|---|---|
| T1 press arbitration (primary, n=2/arm) | 4.4 (5.0, 3.8) | 5.0 (5.0, 5.0) | 0.6 | Swift 1/2 PASS; Rust 2/2 PASS |
| T2 chrome re-synthesis (replication, n=1/arm) | 3.6 | 5.0 | 1.4 | Swift FAIL; Rust PASS |
| C0 seam-free control (calibration, n=1/arm) | **PASS** | **PASS** | — | both arms comfortable — instrument calibrated |

## Decision rule §1 — applied mechanically

- **Branch 1 (Swift ≥ Rust → premise unsupported):** requires "Swift passes
  the objective gate wherever Rust does". FALSE — Rust passed T1 twice and T2
  once; Swift failed one T1 run and its T2 run. Excluded.
- **Branch 2 (weak support):** requires "both pass the objective gate".
  FALSE. Excluded.
- **Branch 3 (Rust ≫ Swift → premise supported):** first clause — "Swift
  DNFs the objective gate — **including failing a differential invariant** —
  where Rust passes on the same task" — SATISFIED on T2 outright (Swift's
  only run failed; Rust's passed) and on T1 by one of two runs
  (t1-swift-2 failed invariant (ii)-class behavior; both Rust runs passed).
  Second (alternative) clause — graded gap ≥ 1.0 replicated on both tasks —
  NOT met (T2 1.4; T1 0.6).

**Mechanical outcome: Branch 3 — premise SUPPORTED**, on the objective-gate
clause, replicated across both mechanism families.

**Pre-registered-text caveats (stated, not smoothed over):**
1. §1 does not say how many of n=2 runs must fail for "Swift DNFs where Rust
   passes"; on T1 the failure is 1-of-2 (the other T1 Swift run passed at
   composite 5.0). On T2 (n=1/arm) the clause is unambiguous.
2. The graded-gap replication clause is unmet; support rests on the
   objective gate — which §1 names supreme, and which the v2 scoring note
   built the differential invariants into for exactly this failure shape.
3. Both Swift failures are single-mechanism bugs a human fixes in
   ~20–60 min (judges' estimates). The measured claim is about what ships
   *believed-verified-but-wrong* per agent session on seam-class work — the
   documented failure shape (tear-off yield regression shipped green twice)
   — not about unfixability.
4. C0 calibration: CLEAN. Both arms passed the seam-free control
   comfortably (Swift: 78 turns, 6/6 verifier items, tests 16/16 + 61/61,
   including a wider-but-uneventful 7-file integration). Per §2 this is
   the cleanest possible signature: Swift aces the widget-interior task
   and fails only on the seam tasks — the pain is isolated to the seam,
   and the instrument is not simply "hard tasks make agents fail".

## What the failures were (mechanism, not moralizing)

Both Swift objective-gate failures are SwiftUI↔AppKit-seam artifacts of the
catalog's documented classes, and both shipped with green unit suites and
confident (incorrect) self-verification:

- **t1-swift-2** (catalog mechanism #1, press arbitration): the router
  classifies by live hit-testing of `ChromeWidgetHosting` views; the widget's
  content swap ("✓ Copied") shrank its hosted hitbox mid-interaction, so the
  double-click's second press fell through to the empty-bar band and fired
  the title-bar action. The Rust run hit the *identical* design hazard,
  caught it via its own live-event self-test, and fixed it with a
  geometry-stable overlay (its hit-blocking is structural: gpui `.occlude()`
  hitboxes, not per-event ancestor-chain classification).
- **t2-swift-1** (catalog mechanism #4, chrome re-synthesis / BUG-B
  territory): parenting a fourth custom button into the traffic-light
  container suppressed AppKit's private group-hover glyph rendering on all
  three native lights (baseline-differentially attributed; placement math
  itself was flawless). On the GPUI side the titlebar row is framework-owned
  and the same feature introduced no regression surface at all.

Auxiliary observations (outside the pre-registered dimensions; recorded, not
decision inputs): Rust runs were uniformly faster (65–94 turns, 20–28 min)
than Swift runs (67–139 turns, 23–51 min) and confined to one file vs 3–7;
both partly reflect the codebases' sizes — which is the real-jobs comparison
by design (§3, §6-T3). Rust implementers' self-verification was also more
often *ground-truth-correct*: both Swift failures came with self-reports
claiming the failed behavior verified.

## Deviations & harness notes (all logged in ab-runs-20260702/LEDGER.md)

1. **t1-swift-1 cap overrun** (139 turns vs 100): live enforcement wasn't in
   place for the first pair; transcript forensics show the last repo
   mutation at turn 45 → artifact == at-cap tree; scored as completed, not
   O4. Live 100-turn watchdogs enforced from t1-rust-2 onward (no other run
   approached the cap; t1-rust-1 finished at 94).
2. **UITests TCC-blocked machine-wide** (Developer Mode off; "Enable UI
   Automation" prompt with no human present): 0 UITests ran in any Swift
   session; §4 O3's Swift gate (targeted `scripts/test.sh` suites green +
   driven-app verification) was satisfied via the unit suites + independent
   CGEvent verification. Environment note, applies symmetrically to all
   Swift runs; not attributed to any session.
3. **install.sh harness fixes mid-experiment** (both logged, Swift-arm only,
   between runs, outside frozen brief text): (a) force-quit dev instances
   via signals instead of AppleScript quit — the quit-confirmation dialog
   stalled unattended installs (observed live by Nick; his requested fix);
   (b) hotfix of (a)'s empty-case abort (`dev_pids` grep exit 1 under
   `set -euo pipefail`) — this one aborted t2-swift-1's install copy step
   mid-session; the implementer diagnosed it as a latent script issue and
   completed the install via the script's own staging steps. Recorded as
   orchestrator fault in that run's record; judges were instructed to
   attribute environment faults accordingly (all three did).
4. **Judge-prompt counting-rule tightening** after the first 9 judge
   sessions: "green" explicitly defined as meeting acceptance criteria per
   objective results (not merely compiling). The first 9 sessions all graded
   artifacts whose gates were full PASS, where the distinction cannot
   change a score; all four artifacts graded under the tightened wording had
   their gate verdicts included in-packet. No re-grading was needed.
5. **Blinding mechanics:** judge packets scrubbed both arm identifiers to
   the same neutral token; judges got no experiment framing, no catalog, no
   other-arm data; verifiers got behavior checklists only. Implementer
   briefs were routine feature requests; `notes/` scoped out both arms.
6. **GUI contention:** concurrent arms occasionally fought for focus
   (documented in verifier reports); every affected check was re-run with
   frontmost confirmed and sentinel values. T2's focus-storm verification
   was serialized against other GUI work.

## Budget

≈3.05M subagent tokens against ≈5–6.5M approved — implementers 1.65M
(8 runs: Swift 873k, Rust 773k), verifiers 0.58M (8 runs + the baseline
hover probe), judges 0.82M (18 sessions, zero tie-breaks needed) — plus
orchestration/write-up (total ≈3.3–3.5M). No abort criterion approached;
the pre-registered Fable sensitivity pair was NOT spent (Nick's call,
offered in the closing report).

## Verdict

**§1 Branch 3 — the impedance premise is SUPPORTED on current evidence**,
via the objective-gate clause (Swift differential-invariant/no-regression
failures on both seam-task families where Rust passed, with a clean C0
control both arms), not via the graded-gap clause (unmet: 1.4 on T2, 0.6 on
T1). The four caveats above are part of this verdict, in particular: the T1
Swift failure is 1-of-2 with §1 silent on split runs, and both failures are
humanly-small bugs whose significance is that they shipped
*believed-verified-and-green* — the documented failure shape the rewrite
case is about. Per the pre-registered rule: date-stamp the measured values
into the report and proceed; no tooling replication owed. Measured
2026-07-02, implementers claude-opus-4-8, judges claude-fable-5, gpui
0.2.2-vendored (prod would pin zed main), n=8 sessions.
