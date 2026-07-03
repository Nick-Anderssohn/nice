# t1-swift-1 turn-cap analysis (orchestrator, 2026-07-02)

Stated cap: 100 tool-call turns / 3 h. Actual: 139 tool-calling turns (153 tool
calls), ~51 min wall clock. THE CAP WAS EXCEEDED (live enforcement was not yet
in place for the first run pair — orchestrator monitoring gap, symmetric for
both first runs; t1-rust-1 finished at 94 turns).

Transcript analysis (defensible from the JSONL):
- Last mutation of any repo file: turn 45 (Edit WindowStatusBarView.swift).
  Turn 56 + turn 132 Writes were /tmp verification helpers. Zero shell-based
  repo mutations (no sed -i / redirects into Sources|Tests).
- Therefore the committed artifact (ab/t1-swift-1 @ 50d101a) is IDENTICAL to
  the tree at the 100-turn cap point. Turns 46–139 were exclusively
  build/install/unit-test/GUI-verification/reporting, materially inflated by
  three UITest attempts blocked pre-launch by the macOS "Enable UI Automation"
  TCC prompt (Developer Mode disabled — pre-existing machine state, not run
  behavior).

Disposition: objective gate evaluated on the artifact (== at-cap tree); run is
NOT an O4 DNF. Overrun recorded here + in RESULTS threats section. Judges
receive the full tool-call sequence (iterations-to-green counts to green, so
the verification tail is visible to them as-is). Live 100-turn watchdogs are
armed for every subsequent run (t1-rust-2 onward), which stops a run at cap so
this ambiguity cannot recur.
