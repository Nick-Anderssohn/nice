# t1-rust-2 implementer final report (claude-opus-4-8, 2026-07-02, 86 turns/87 uses, ~22.4 min, ~189k tokens)
# NOTE: self-report — NOT a judge input; objective gate comes from the independent verifier.

## Files changed
- spikes/phase0-poc/src/gpui_term.rs (+241/−11) — only file touched. run_live/TermView and all headless modes byte-identical.

## What was built
- Status bar: full-width, BAR_HEIGHT=28pt, flex column (grid flex_1 above, bar fixed below). #161616 bg, 1px top rule, Menlo 12pt, DEFAULT_FG text. Window height bumped by BAR_HEIGHT.
- Left widget (cwd): session_cwd() = process cwd (cat pty child inherits — working_directory: None), $HOME→~ abbreviation. Click copies via cx.write_to_clipboard + green "Copied" badge ~1.2s (one-shot bg timer redraws once).
- Right widget (clock): HH:MM via libc::localtime_r, recomputed each render. Dedicated OS thread (poc-clock) wakes at minute boundaries → channel → cx.notify() + present-kick. OS thread chosen deliberately over gpui executor timer (demand-driven mode; gpui timers coalesce under App Nap).
- Drag/double-click on empty bar: on_mouse_down(Left) → perform_native_window_drag() handing [NSApp currentEvent] to performWindowDragWithEvent: (the title bar's exact API — native drag loop + snapping + system double-click action). click_count>=2 also explicitly calls window.titlebar_double_click() (reads AppleActionOnDoubleClick) so pref honored either way.
- Widgets never move window: .occlude() (HitboxBehavior::BlockMouse); verified against gpui window.rs hit_test — first BlockMouse hitbox breaks; bar's on_mouse_down guarded by hitbox.is_hovered() → structural guarantee.

## Self-verification
- cargo build --bin gpui-term: zero errors, zero new warnings.
- Headless: default self-test PASS exit 0; INTERACTIVE=1 pty self-test PASS (bytes_echoed=14, grid "hello").
- Live: real window launched twice (6s, 45/60s), renders new tree, self-exits exit 0. Demand-driven intact: 4 metal draws / 4 scene rebuilds over 45s (no render loop; damage-gated present preserved).
- Visual: window-scoped screenshot shows bar, left = ~/Projects/nice/.claude/worktrees/ab-rust/spikes/phase0-poc, right = 16:48 correct local time.
- Mouse gestures NOT driven synthetically (self-imposed: offline, no injection tooling found, risk to user's windows) — rests on source-level analysis of gpui mouse-dispatch/hit-test + API wiring. Flagged for human/verifier close-out.

## Caveats
- Native title bar drags as before; bar is a second drag surface at bottom.
- Two verification PoC windows self-exited exit 0. Git untouched; change uncommitted.
