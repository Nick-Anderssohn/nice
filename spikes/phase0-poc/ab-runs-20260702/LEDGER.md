# Chrome-tractability A/B — run ledger (2026-07-02)

Protocol: `notes/tractability-ab-design-20260702.md` v2.2 (APPROVED) @ `fba72a4`.
Baseline both arms: `8f0dd7f`. Implementers: claude-opus-4-8. Judges: claude-fable-5.
Orchestrating session: 87ed41d3 (spikes/phase0-poc cwd). AXIsProcessTrusted: true.

## Run plan (8 implementer sessions)

Swift chain (sequential, shared worktree + Nice Dev bundle ID):
t1-swift-1 → t1-swift-2 → t2-swift-1 → c0-swift-1
Rust chain (sequential, shared worktree; concurrent with Swift chain):
t1-rust-1 → t1-rust-2 → t2-rust-1 → c0-rust-1

Per-run pipeline: implementer (opus, bg, 100-turn/3h cap w/ 3h timer) →
canonical build log (Swift: `scripts/install.sh` under lock; Rust: `cargo build
--bin gpui-term`) → verifier agent drives real app against §4 checklist +
differential invariants → collect diff + toolcalls from transcript → commit on
arm branch + preserve as branch `ab/<runid>` → `git reset --hard 8f0dd7f` +
`git clean -fd` (KEEPS ignored build-dev/, target/, vendor/) → next run.

Judging: 3× fable judges per T1/T2 artifact (18 sessions), inputs inlined
(brief, diff, build log tail, tool-call sequence, objective results), blind to
decision framing. C0 objective-gate only. v1 anchors (recovered from cf4de11):

| Dimension | 5 | 3 | 1 |
|---|---|---|---|
| Edit locality | touches only files a maintainer would expect | one or two gratuitous excursions | sprawling/refactors unrelated code |
| API hallucination count | 0 invented APIs across the session | 1–2, self-corrected via compiler | ≥5, or one survives into the final diff |
| Iterations-to-green | ≤2 build/run cycles to done | 3–5 | >8 or never green |
| Human-fixup minutes | <5 min | 15–30 min | >60 min |
| Style conformance | indistinguishable from surrounding idiom | recognizably foreign but acceptable | fights the house patterns |

(Hallucinations + iterations counted from transcript; invented symbol = 1,
misremembered arg order on real API = half-weight.)

## Run table

| run | arm | task | status | agent task id | started | ended | artifact | notes |
|---|---|---|---|---|---|---|---|---|
| t1-swift-1 | swift | T1 | **objective PASS** | (scratchpad state file) | 2026-07-02 15:48 | 16:39 | branch ab/t1-swift-1 @ 50d101a | 139 turns (cap-note.md: code final turn 45), ~51 min, ~243k+50k tok; O1–O3 ALL PASS (verifier 10/10); unit 1326/0; judges DONE: 5.0×3 unanimous |
| t1-swift-2 | swift | T1 | **objective FAIL** | (scratchpad state file) | 2026-07-02 17:04 | 17:14 | branch ab/t1-swift-2 @ e319029 | 67 turns, ~23 min, ~193k+79k tok; O1/O2 PASS but O3 FAIL: cwd-widget double-click ZOOMS window (Copied-label hitbox shrink; 3× repro) — the documented compiles+unit-green-but-behaviorally-wrong shape; same bug caught+fixed in-session by t1-rust-1; judges DONE: 3.8×3 unanimous (5,5,1,3,5) |
| t2-swift-1 | swift | T2 | **objective FAIL** | (scratchpad state file) | 2026-07-02 17:44 | 18:10 | branch ab/t2-swift-1 @ 3eda9da | 77 turns, ~26 min, ~169k+58k+45k(probe) tok; O1/O2 PASS, **O3 FAIL: traffic-light hover glyphs regressed** (baseline-attributed via differential probe: baseline shows glyphs, artifact doesn't, Finder control validates method); placement invariants + 10× storm all PASS w/ exact +23px pitch; judges launched |
| c0-swift-1 | swift | C0 | **objective PASS** | (scratchpad state file) | 2026-07-02 18:34 | 18:57 | branch ab/c0-swift-1 @ 96707e1 | 78 turns (≤100 ✓), ~23 min, ~268k tok; O1 clean (install exit 0 — dev_pids empty-case hotfix exercised OK); 16/16 + 61/61 targeted tests; verifier 6/6 PASS (+109k) — C0 CONTROL PASSED BOTH ARMS |
| t1-rust-1 | rust | T1 | **objective PASS** | (scratchpad state file) | 2026-07-02 15:48 | 16:16 | branch ab/t1-rust-1 @ 02f1739 | 71 uses, ~28 min, ~197k tok impl + ~73k verify; O1–O3 ALL PASS (verifier 9/9); judges DONE: 5.0×3 unanimous |
| t1-rust-2 | rust | T1 | **objective PASS** | (scratchpad state file) | 2026-07-02 16:31 | 16:53 | branch ab/t1-rust-2 @ 531352c | 86 turns, ~22 min, ~189k+47k tok; O1–O3 ALL PASS (verifier 9/9); judges DONE: 5.0×3 unanimous |
| t2-rust-1 | rust | T2 | **objective PASS** | (scratchpad state file) | 2026-07-02 17:12 | 17:36 | branch ab/t2-rust-1 @ 5457400 | 74 turns, ~24.5 min, ~204k+66k tok; O1–O3 ALL PASS (verifier 6/6 incl. FS round-trip + 10× storm bit-identical); judges DONE: 5.0×3 unanimous |
| c0-rust-1 | rust | C0 | **objective PASS** | (scratchpad state file) | 2026-07-02 18:02 | 18:22 | branch ab/c0-rust-1 @ 761c62b | 65 turns (≤100 ✓), ~20 min, ~183k tok; O1 clean; badge selftest PASS + all headless modes PASS; O1–O3 ALL PASS (verifier 6/6, +54k tok); C0 objective-only ✓ |

## Phase checklist

- [x] Prep verified (arms @ 8f0dd7f clean, caches warm, prod untouched, AX ok)
- [x] 8 implementer runs complete, artifacts preserved (branches ab/*)
- [x] Objective gates executed — verifier agents ground-truthed every item incl. GUI; Nick batch list EMPTY (nothing left UNVERIFIED)
- [x] 18 judge sessions + medians computed (all unanimous; zero tie-breaks needed)
- [ ] Decision rule applied mechanically (§1)
- [ ] RESULTS-spike11p3 written; report §12 folded; memory updated; pushed
- [ ] Reported to Nick (incl. sensitivity-pair offer if surprising)

## Harness fixes (logged per §3 — symmetric fixes for harness breakage only)

1. Live 100-turn cap watchdogs from t1-rust-2 onward (t1-swift-1 overran during
   its verification tail; see runs/t1-swift-1/cap-note.md).
2. install.sh dev-quit path force-quits Nice Dev via signals (ps-args detection)
   instead of AppleScript quit — the quit-confirmation dialog stalled unattended
   installs (Nick observed the stuck dialog live; requested the fix). Committed
   2c08c51 on worktree-rewrite, 63ac675 on ab-swift-arm. NEW SWIFT ARM BASELINE
   = 63ac675 for t2-swift-1 / c0-swift-1 (launch + reset target). T1 swift
   artifacts remain diffs against 8f0dd7f. No Rust-side counterpart needed
   (no install.sh usage; no quit dialog exists there).
3. install.sh dev_pids exit-status hotfix (|| true): the force-quit fix aborted
   installs when NO dev instance was running (set -euo pipefail + grep 1).
   Caught by the t2-swift-1 implementer (correctly attributed + worked around;
   documented as orchestrator fault in its run record so judges don't count it
   against the session). Fixed post-run: rewrite aa08715, ab-swift a1c7e43 =
   new baseline for c0-swift-1. Canonical t2-swift-1 install exercised the
   corrected force-quit path successfully (no dialog, exit 0).
4. t2-swift-1 / c0-swift-1 house rules get one added sentence: if a
   quit-confirmation dialog appears when quitting Nice Dev, click its Quit
   button via System Events. (Operational, outside frozen §2 text.)

## Budget watch

Approved ≈5–6.5M tokens. Abort criterion: >1.5× estimate → stop, report, ask.
Track per-run approximate usage from task notifications here:

| item | tokens (approx) |
|---|---|

## Nick GUI batch verification list (accumulates)

(items verifier agents could not ground-truth; batch for Nick, ~10 min/run cap)
