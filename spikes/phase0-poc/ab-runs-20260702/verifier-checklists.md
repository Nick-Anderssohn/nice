# Objective-gate checklists (protocol §4) — per task/arm

Verifier agents get ONLY: the feature request text, the app launch command,
the relevant checklist below, and mechanics notes. No experiment framing, no
catalog, no notes/. Every item scored PASS / FAIL / UNVERIFIED + one-line
evidence. UNVERIFIED items accumulate into Nick's GUI batch list.

Gate items (every run):
- O1 Builds clean: final tree compiles, zero errors (warnings noted) — from canonical build log (orchestrator provides).
- O2 Feature functional: per-task checklist below, executed against the running app.
- O3 No regressions + differential invariants: paired invariants below, same run. Swift: app launches, existing chrome interacts normally (targeted test suites already reflected in implementer report; verifier re-checks launch + chrome). Rust: `cargo build` all bins + one headless `gpui-term` run produces sane output/CSV.
- O4 DNF: budget exhausted before O1–O3 (orchestrator records).

## T1 checklist (both arms)

1. Bar renders at window bottom, full width, ~28pt, visually plausible against the chrome.
2. Left widget shows a working directory or sensible placeholder; right clock shows HH:MM and ticks at the minute boundary.
3. Clicking the left widget copies its text to clipboard (verify via pbpaste) AND shows a brief visible confirmation AND does not move the window.
4. Press-drag on empty (non-widget) bar area moves the window (window origin delta ≥ drag delta ± few px).
5. Double-click on empty bar area performs the title-bar double-click action (read `defaults read -g AppleActionOnDoubleClick` — Maximize→zoom, Minimize→miniaturize; check window state change matches).
6. Window resize does not break the bar layout.

T1 differential invariants (same run):
- (i) new bar empty-area drag moves the window;
- (ii) widget presses NEVER move the window (press-drag ON the widget: window must not move);
- (iii) pre-existing drag surfaces unchanged: title-bar/top-band drag still moves the window; terminal input still works (keystrokes echo);
- (iii-Swift only) pill drag still reorders pills and never moves the window.

## T2 checklist (both arms)

1. Pin button renders inline with traffic lights, immediately right, size/vertical alignment plausible.
2. Click toggles a visible active/inactive state.
3. Placement exact (measure position relative to lights) after: one window resize; 3× focus loss/regain cycles; one full-screen enter/exit round-trip.
4. Close/minimize/zoom still work with normal hover effects (hover: best-effort visual; behavior: minimize + reopen, zoom toggle; close LAST or on a second window).

T2 differential invariant (same run):
- 10× rapid focus-cycle storm → no drift or double-spacing of the button cluster (compare positions before/after; screenshot or AX-position reads).

## C0 checklist (both arms; objective gate ONLY, no judges)

1. Badge visible adjacent to top-bar content.
2. Label tracks a throughput burst within ~2× (e.g. run `yes` briefly in the terminal / feed the pty): active style while bytes arrive.
3. Dims to idle style ≤ ~3 s after silence.
4. Click toggles full (dot+label) ↔ compact (dot-only).
5. Relaunch restores the chosen presentation.
6. Window resize OK; existing chrome unaffected.

## Mechanics notes for verifiers (include in prompt)

- AXIsProcessTrusted is TRUE for this session's processes: synthetic events via CGEvent (compile a tiny Swift helper with `swiftc` in the scratchpad, or `osascript` System Events) are available. A keystroke-injection reference exists at `spikes/phase0-poc/baseline/keystroke-harness/keyinject.swift` (in the worktree being verified).
- Window position/size: `osascript -e 'tell application "System Events" to get position of window 1 of process "<name>"'` (process: "Nice Dev" / "gpui-term").
- Screenshots: `screencapture -x` variants; agents can Read PNG files to inspect them. If Screen Recording TCC blocks window capture, fall back to AX reads + behavioral probes and mark visual-only items UNVERIFIED rather than guessing.
- Swift app: launch `open "/Applications/Nice Dev.app"`, quit gracefully via `osascript -e 'tell application "Nice Dev" to quit'` when done. NEVER touch `/Applications/Nice.app` (production, running) or its processes; never use pgrep/pkill for Nice; use `ps -Aww -o pid=,args=` + grep if needed.
- Rust app: launch `NICE_POC_RUN=1 NICE_POC_INTERACTIVE=1 ./target/debug/gpui-term` from `spikes/phase0-poc` (both env vars; already built). It auto-exits after a measurement window unless interacted with — check README run-mode notes; kill only the gpui-term process you started (by PID).
- Do NOT modify any source in the worktree. Helper scripts go in your scratchpad only.
- Report format: one line per checklist item: `<item#> PASS|FAIL|UNVERIFIED — <evidence>`; then a 3-line summary (feature functional? invariants held? regressions?).
