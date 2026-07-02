---
title: Claude Swift-vs-Rust A/B tractability experiment — protocol (§13 spike 11, part 3)
date: 2026-07-02
status: DESIGN — awaiting Nick's approval; no runs executed
closes: rewrite-stack-research.md §13 gap #3 / audit G3
---

# The Claude tractability A/B: SwiftUI-on-Nice vs GPUI-on-PoC

## 1. The question, sharply

The rewrite case reduces to the language axis: the report concedes framework
AI-velocity is a wash (D2 = 2 both sides) and rests the entire velocity payoff
on **D1 Swift 3 vs Rust 5** — a bare assertion, never measured, never dated,
serving as the §11 **hard gate** ("the stack must be one Claude can actually
build and maintain"). The audit (G3) additionally notes Xcode 26.3 ships
native Claude Agent SDK integration (Feb 2026), eroding the premise from the
Swift side. This experiment measures the skipped quantity before a
multi-month commitment.

**What is actually measurable.** No same-feature task can isolate D1
(language) from D2 (framework) from codebase shape — every session exercises
all three at once. So the experiment measures the **composite operational
construct the hard gate actually gates**: *Claude's end-to-end effectiveness
doing Nice-chrome work in each stack as it would really be practiced.*
Because the report itself scores D2 a wash, a composite difference is
attributed (per the report's own model) to language + tooling; that inference
is stated, not smuggled.

**Decision rule — fixed now, before any data:**

Let each arm's result be (a) the **objective gate** — feature-complete: builds
clean, all functional-checklist items pass, no regressions — and (b) the
**graded composite** — mean of the five 1–5 anchored dimensions in §4, median
across judges, then across runs.

1. **Swift ≥ Rust** (Swift passes the objective gate wherever Rust does, and
   graded composite Swift ≥ Rust): the D2→D1 tractability premium claimed for
   Rust is **unsupported**. Consequence: re-score §11's Claude-velocity
   column with the measured values, date-stamped; the rewrite's hard-gate
   justification is void on current evidence, and — combined with G4 (the
   still-unwritten list of user-visible quality deltas over shipping Nice) —
   the Path B recommendation must be re-argued from scratch before any
   commitment.
2. **Rust > Swift, weakly** (both arms pass the objective gate; graded gap
   < 1.0 point, or not replicated on the second task): premise **weakly
   supported**. D1 is re-scored to a one-point gap (≈4 vs 5, not 3 vs 5);
   the rewrite case must then carry on its other drivers (SwiftUI↔AppKit
   impedance escape, §11 quality criteria), and that shift is flagged to
   Nick explicitly.
3. **Rust ≫ Swift** (Swift DNFs the objective gate where Rust passes on the
   same task, or a graded gap ≥ 1.0 replicated on both tasks): premise
   **supported**. Date-stamp D1 3-vs-5 (or the measured values) into the
   report and proceed with the remaining spike program.
4. **Xcode contingency (asymmetric on purpose):** both arms run in Claude
   Code CLI (§3), which is the Swift arm's *weaker* real-world tooling. So a
   Swift win is conclusive as-is (it won handicapped), but any Rust-favoring
   outcome that would be cited in a commitment memo first requires **one
   Swift replication inside Xcode 26.3's agent integration** to test whether
   the tooling closes the gap.

**Power honesty.** At n=2 runs/arm (primary task) this is a screen, not a
measurement of small effects — and that is the right instrument: the premise
under test is a claimed *decisive* gap (3 vs 5, gating a multi-month bet). A
decisive gap should show decisively at n=2; an effect too subtle to show here
is too subtle to carry a hard gate.

---

## 2. Task design

Requirements: chrome-flavored (the rewrite is about chrome), implementable in
**both** stacks in one agent-session, present in **neither** codebase today
(verified by grep), with a comparable data-source seam on both sides, and
minimal dependence on scaffolding only one side has.

### T1 (PRIMARY) — Stream-activity badge with interaction + persistence

*Brief (identical text both arms; only the repo-path/build-command block
differs — full brief in §3):*

> Add a small **activity badge** to the window chrome, adjacent to the
> existing top-bar content: a dot plus a `NN KB/s` label showing terminal
> output throughput over a rolling ~1 s window. While bytes are arriving the
> badge renders in the active/accent style; after ~2 s of silence it dims to
> the idle style. **Clicking** the badge toggles between the full (dot +
> label) and compact (dot-only) presentation, and the chosen presentation
> **persists across app relaunch**. Match the surrounding chrome's visual
> style (colors, typography, spacing). Do not regress existing behavior.

- **Exercises:** layout insertion into existing chrome; streaming state →
  UI updates (pty-reader thread → UI thread, ~1 Hz label + activity
  debounce); hit-testing/interaction; persistence; theme-matched styling.
- **Why fair:** neither side has it (grep: no throughput/byte-rate UI in
  `Sources/Nice`, none in `spikes/phase0-poc`). Both sides have the byte
  seam: Nice's `TabPtySession`/`NiceTerminalView` feed path; the PoC's
  `PtySession` reader thread + wake channel (`gpui_term.rs`). Both sides
  have a top region to extend (Nice `WindowToolbarView`; the PoC interactive
  window's root `div()` — the agent adds a strip above `grid_canvas`).
  Persistence is deliberately mechanism-unspecified: `UserDefaults` is the
  house pattern on the Swift side (`Tweaks.swift`), the Rust side has no
  prefs infra and must choose (a dotfile/JSON) — that asymmetry is the real
  world of both stacks and is part of the construct (§6-T3).
- **Rough size:** ~150–250 LOC Swift; ~200–350 LOC Rust.

### T2 (REPLICATION) — Bell attention flash

*Brief:*

> When the terminal receives BEL (0x07), flash a **bell indicator** in the
> window chrome: it appears in the attention/accent style and decays to
> invisible over ~2.5 s; a new bell during decay restarts the flash;
> clicking it dismisses immediately. No persistence. Match the surrounding
> chrome's style. (Test with `printf '\a'`.) Do not regress existing
> behavior.

- **Exercises:** VT-event plumbing (SwiftTerm's `bell(source:)` delegate —
  present in the fork at `Terminal.swift:97`, unhandled by Nice; alacritty's
  `Event::Bell` — currently discarded by the PoC's `EventProxy`,
  `gpui_term.rs:167-170`); time-based animation (SwiftUI `withAnimation` /
  repeat-forever idioms vs GPUI's animation/timer facilities — a known-thin
  GPUI surface, which is exactly the point); interaction. Different
  sub-skills from T1 (animation + event plumbing vs persistence + rolling
  state), so a replicated direction generalizes better than re-running T1.
- **Why fair:** the hook exists but is unused on both sides; zero existing
  handling either side (grep-verified 2026-07-02).
- **Rough size:** ~80–150 LOC each side.

### T3 (documented, NOT recommended as primary) — Settings toggle with visual effect

"Dim the terminal when the window is unfocused" toggle, persisted. Rejected
as primary: Nice has a complete `SettingsView` + `Tweaks` persistence
machine (784 + 806 LOC of exactly this pattern) while the PoC has zero
settings UI — the measurement would be dominated by scaffolding asymmetry
rather than tractability. Keep as a spare if T1 or T2 turns out miscalibrated.

**Recommendation: run T1 (n=2 per arm) + T2 (n=1 per arm).**

---

## 3. Fairness controls

- **Identical briefs modulo stack.** The exact brief text above is frozen by
  this document *before* any run. Each arm's brief appends only a
  three-line factual block: repo worktree path; where the chrome lives
  ("the top bar is `Sources/Nice/Views/WindowToolbarView.swift`" / "the
  interactive window is built in `spikes/phase0-poc/src/gpui_term.rs`,
  `run_interactive()`"); and the standard build/run command
  (`scripts/install.sh` + launch Nice Dev, under the worktree lock /
  `NICE_POC_INTERACTIVE=1 cargo run --bin gpui-term`). These are
  infrastructure pointers of equal specificity, not solution hints. No other
  hints; no mid-run coaching; course corrections only for harness breakage,
  applied symmetrically.
- **Real codebases, both arms.** Swift arm: the actual Nice app
  (`Sources/Nice`, ~27k LOC SwiftUI/AppKit). Rust arm: the actual Path-B
  seed — `spikes/phase0-poc`, binary `gpui-term` (`src/gpui_term.rs`, ~3.1k
  LOC on vendored gpui 0.2.2 via `[patch.crates-io]`). *Why not
  `aa-gamma/gpui-term-main`:* that workspace is a headless pixel-readback
  fixture (833 LOC, no interactive chrome) — the wrong shape for a chrome
  feature. Named limitation: production Path B would pin zed `main` (audit
  G10), and 0.2.2's API is ~9 months stale; this slightly *helps* the Rust
  arm (0.2.2 matches the model's training snapshot better than churning
  main), so a Rust loss is conclusive and a narrow Rust win on 0.2.2 is an
  upper bound. Record the gpui rev in the date-stamp.
- **Mature-app vs PoC asymmetry — named, two-sided.** Nice gives the Swift
  arm house patterns to imitate (also more machinery to break); the PoC
  gives the Rust arm a blank slate (less to learn, but no prefs/settings/
  component infrastructure, spike-grade style). This is not removable — and
  it is **ecologically valid in both directions**: the null hypothesis is
  "keep maintaining mature Nice" (the Swift arm's actual job) and the
  rewrite would start from roughly this PoC (the Rust arm's actual job). The
  experiment measures the two *real* jobs, not two synthetic ones.
  Mitigations where it would otherwise contaminate scores: tasks chosen to
  need minimal one-sided scaffolding (T1/T2, not T3), and judges anchor
  style-conformance to the *surrounding file's* idiom, not absolute polish.
- **Same model, effort, budget.** Both arms: the same current frontier model
  (claude-fable-5 as of this design), same reasoning-effort setting, same
  cap of **100 tool-call turns / 3 h wall-clock** per session, isolated git
  worktrees (`isolation: worktree`), no web access needed either side (allow
  or deny it symmetrically — recommend deny, so the corpus-in-weights is
  what's measured).
- **Same definition of done, told to both agents:** compiles clean; feature
  works per the brief (agent self-verifies by running the app); existing
  behavior unregressed (Swift: `scripts/test.sh` targeted suites still
  green; Rust: existing `gpui-term` headless modes still build and run);
  code matches the style of the files it touches.
- **Xcode 26.3 Agent-SDK tooling: OUT of the main run, IN as contingency.**
  Gap #3 names it, but running one arm inside Xcode's agent harness and the
  other in Claude Code makes the harness a confound with unknown sign.
  Decision: both arms run in Claude Code CLI (identical harness), which
  handicaps only the Swift arm's best real-world workflow — a conservative
  bias with a known sign, exploited by decision-rule branch 4 (Swift winning
  handicapped is conclusive; Rust winning obligates the Xcode replication).
- **Hypothesis blinding.** Implementer briefs are written as ordinary
  feature requests — no mention of an experiment, comparison, or rewrite.
  Both briefs scope the agent to its code dirs and instruct it not to read
  `notes/` (symmetric; the rewrite-research docs live in this repo and would
  reveal the hypothesis). Residual inference risk accepted (§6-T6). Judges
  are blind to the decision framing (§4).
- **Broken-brief protocol.** If a run reveals the brief references something
  wrong (a misnamed file, a missing seam), abort that run, fix the brief for
  both arms identically, restart both arms of that task fresh. No partial
  credit, no in-flight patching of one arm.

---

## 4. Scoring rubric

### Objective gate (per run; pass/fail + counts — no judgment)

| Item | How measured |
|---|---|
| O1 Builds clean | final tree compiles with zero errors (warnings noted) |
| O2 Feature functional | per-task checklist (below), executed on the running app by Nick (~5–10 min/run) or a verifier agent driving it; every item must pass |
| O3 No regressions | Swift: targeted `scripts/test.sh` suites green, app launches, existing chrome interacts normally; Rust: `cargo build` all bins + one headless `gpui-term` run produces a sane CSV |
| O4 DNF | budget exhausted before O1–O3 → recorded as failure-with-artifact (that is data, not an abort) |

**T1 checklist:** badge visible in chrome; label tracks throughput within ~2×
during a `yes`-burst; active→idle dimming ≤ ~3 s after stream stops; click
toggles full↔compact; relaunch restores the chosen mode; window resize
doesn't break layout; existing chrome (pills/traffic lights/tabs, or grid
rendering) unaffected.
**T2 checklist:** `printf '\a'` triggers the flash; decay completes ≈2–3 s;
re-bell during decay restarts; click dismisses; no flash on ordinary output;
existing behavior unaffected.

### Graded dimensions (1–5 each, anchored; judged from the diff + transcript)

| Dimension | 5 | 3 | 1 |
|---|---|---|---|
| **Edit locality** | touches only files a maintainer would expect | one or two gratuitous excursions | sprawling/refactors unrelated code |
| **API hallucination count** | 0 invented APIs across the session | 1–2, self-corrected via compiler | ≥5, or one survives into the final diff |
| **Iterations-to-green** | ≤2 build/run cycles to done | 3–5 | >8 or never green |
| **Human-fixup minutes** (judge's estimate to make it mergeable) | <5 min | 15–30 min | >60 min |
| **Style conformance** | indistinguishable from surrounding code's idiom | recognizably foreign but acceptable | fights the house patterns |

Hallucinations and iterations are *counted* from the transcript (compile
errors citing nonexistent symbols the agent invented = hallucination;
misremembered arg order on a real API = half-weight), then mapped to the
anchor; the other three are judged.

### Judge protocol

- **3 independent judge agents per run-artifact** (6 artifacts → 18 short
  sessions). Input per judge: the frozen brief, the final diff, the build
  log, the transcript's tool-call sequence, and the objective-gate results.
  Judges score each artifact **independently against the anchors** — no
  head-to-head, so no cross-arm contamination (stack identity is visible in
  a diff and cannot be blinded; the *decision* framing can be and is: judges
  are told "score this feature-implementation session," nothing about
  rewrite, A/B, or which stack 'should' win).
- Per-dimension score + 2-sentence rationale, median across judges per
  dimension, mean across dimensions = the run's graded composite.
- **Disagreement flag:** any dimension with judge range ≥2 gets a fourth
  tie-break judge and a note in the results file.

### Date-stamping into the report

Results land in `spikes/phase0-poc/RESULTS-spike11-<date>.md` (raw scores,
checklists, judge rationales, transcripts' locations), and the report gets:
a new §12-scorecard row ("Claude tractability A/B — spike 11.3") plus an
annotation on every D1 score it touches: **"measured YYYY-MM-DD,
claude-fable-5, Xcode <ver>, gpui 0.2.2-vendored (prod would pin zed main),
n=6 sessions"** — so the next audit can see when the evidence expires.
Model/date matter: this measures *today's* frontier model, and both stacks'
corpora move.

---

## 5. Execution plan + budget

**Roles.** The main session orchestrates only: prepares worktrees, pre-warms
builds, launches implementers, collects artifacts, launches judges, writes
the results file + report edits. It never implements or scores.

1. **Prep (~1 h wall, mostly waiting on builds).** Two worktrees per task
   run off `worktree-rewrite`. Pre-warm each: one full build before the
   session starts (Swift: `scripts/install.sh` under the worktree lock;
   Rust: `cargo build --bin gpui-term` — cold gpui builds are ~10 min and
   would otherwise tax whichever arm's cache is colder; pre-warm is
   stack-neutral and uncounted). Verify the app launches on both sides.
2. **Implementer sessions (6):** T1×2 per arm, T2×1 per arm. Caps: 100
   tool-turns / 3 h / hard-stop. Rust sessions may run concurrently; Swift
   sessions serialize on the worktree lock (install/test contention) —
   schedule Swift runs back-to-back, Rust runs in parallel with them.
3. **Objective verification:** Nick (preferred — GUI ground truth, ~1 h
   total for all 6) or a verifier agent per artifact.
4. **Judging (18 short sessions, parallel).**
5. **Write-up + decision-rule application + report edits.**

**Token budget (assumptions stated):** implementer ≈ 300–500k tokens/session
(≈4k/turn incl. build output; observed range for comparable dev-cycle
sessions) → ≤3.0M; judges ≈ 60–120k each → ≤2.0M; orchestration + write-up
≈ 0.5M. **Total ≈ 4–5.5M tokens.** Wall-clock: ~1 working day active
(~8–10 h), spread over ≤2 days; Nick's own time ≈ 1–1.5 h (verification +
approval).

**Abort criteria (experiment-level; per-run DNFs are data):**
- harness failure (lock deadlock, disk, model outage) → pause, fix, restart
  the affected runs fresh;
- broken brief (§3 protocol) → symmetric fix, restart that task's runs;
- both arms DNF T1 → task miscalibrated; demote T1, promote T2 to primary,
  activate T3 as replication;
- budget overrun >1.5× the estimate with runs still incomplete → stop, report
  partial results labeled as such, decide with Nick whether to fund the rest.

---

## 6. Threats to validity

| # | Threat | Disposition |
|---|---|---|
| T1 | **Single-task generalization** — two small chrome features can't represent a 55+-feature rewrite. | Mitigated, not removed: two tasks with disjoint sub-skills (persistence/state vs animation/event-plumbing), n=2 on the primary. **Accepted residual:** this screens the *hard gate's decisive claim*, it does not forecast month-6 velocity. If results land in branch 2 (weak support), that residual is itself the finding — the gate can't carry the decision. |
| T2 | **Training-data familiarity** — SwiftUI's corpus is vast, GPUI's is niche and ~stale. | **Not a confound — part of the measured construct.** The report's own thesis is that the composite (deep-Rust-language + thin-GPUI-framework) beats (mid-Swift + thin-SwiftUI-for-Nice's-chrome). The A/B measures exactly that composite as practiced. Stated so nobody "corrects" for it post hoc. |
| T3 | **PoC-vs-mature-app asymmetry.** | Named and two-sided (§3): both directions are the real jobs being compared (maintain Nice vs start the rewrite from the PoC). Task selection minimizes one-sided scaffolding; judges anchor style to surrounding idiom. Accepted residual: T1's persistence is infrastructure-free on the Rust side by necessity — which *is* Path B's day-1 reality. |
| T4 | **Judge bias** (LLM judges may share the implementer's priors, e.g. pro-Rust lore). | Judges blind to the decision framing; anchored scales with counted (not vibed) hallucination/iteration dimensions; objective gate sits above all judged dimensions; 3 judges + range≥2 tie-break; judge rationales published in the results file for Nick's spot-audit. |
| T5 | **One-shot variance** — a single bad sample decides a gate. | n=2 on T1 per arm; direction must replicate on T2 for the strong branches (1 and 3) of the decision rule; branch 2 exists precisely so a noisy small gap can't masquerade as decisive. If budget allows one more session pair, add a third T1 run per arm before widening the task set. |
| T6 | **Demand effects** — the implementer infers it's being compared and behaves atypically. | Briefs written as routine feature requests; `notes/` scoped out; residual inference from repo contents accepted (symmetric across arms, so it biases level, not difference). |
| T7 | **Harness confound vs the real Swift workflow** (Xcode 26.3 Agent SDK excluded from the main run). | Known-sign bias against Swift, exploited by decision-rule branch 4: Swift-wins is conclusive as-is; Rust-wins triggers the one-session Xcode replication before the result may appear in a commitment memo. |
| T8 | **gpui 0.2.2 vs production zed-main pin** (audit G10). | Known-sign bias *for* Rust (stale API matches model weights better than churning main). Rust-loses is therefore conclusive; a narrow Rust win is an upper bound and is labeled as such in the date-stamp. If branch 3 fires and the margin matters, re-run one Rust T1 session against the `aa-gamma` pinned-main workspace as a sensitivity check. |
| T9 | **Verification asymmetry** — Nice has 14 XCUITest suites, the PoC has none. | DoD deliberately uses a *manual functional checklist* for both arms (not test-suite authorship), so neither arm is graded on infrastructure the other lacks. Regression checks use each side's native cheap mechanism (targeted suites vs bins-still-run). |

---

## Approval asks (Nick)

1. Approve tasks: **T1 primary (n=2/arm), T2 replication (n=1/arm)**, T3 spare.
2. Approve the decision rule of §1 as binding before data.
3. Approve budget: ~4–5.5M tokens, ~1 active day, ~1–1.5 h of your time.
4. Confirm Xcode 26.3 handling (CLI-only main run + contingent Xcode replication) — or order the Xcode arm up front (+1 session, adds a harness confound the design then has to carry).
