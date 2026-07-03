# t1-rust-1 implementer final report (claude-opus-4-8, 2026-07-02, 71 tool uses, ~27.6 min, ~197k tokens)
# NOTE: self-report — NOT a judge input; objective gate comes from the independent verifier.

## Files changed
- spikes/phase0-poc/src/gpui_term.rs — only file touched (+234/−12), uncommitted on ab-rust-arm. All changes inside the gui module, scoped to run_interactive/InteractiveView; run_live/TermView untouched.

## What was added
- Status bar (28 pt, full width) below the terminal grid via flex column root; window height grew by exactly 28 pt (40-row grid unchanged). Styled to repo chrome idiom: 0x10141C bar, 0xFFFFFF22 hairline top border, Menlo 11 px, 0xC8C8C8 text.
- Left widget: real cwd via std::env::current_dir(). Click copies via cx.write_to_clipboard, green "Copied" overlay 1.5 s; path stays laid out under overlay so widget geometry never changes (see bug below).
- Right widget: HH:MM clock (libc localtime_r, no new deps), minute-boundary timer task doing cx.notify() + kick_platform_display().
- Window move: empty-bar left-press calls performWindowDragWithEvent: with the window's currentEvent (gpui handle_view_event dispatches synchronously inside mouseDown:) — literal titlebar drag mechanism. gpui-0.2.2 mac backend has no start_window_move/WindowControlArea, so the AppKit path is correct.
- Double-click on empty bar: gpui Window::titlebar_double_click() → reads AppleActionOnDoubleClick, defaults zoom.
- Widgets swallow left mouse-downs (cx.stop_propagation()).

## Self-verification (live CGEvents via AX-trusted Swift harness; frames via CGWindowList)
- Empty-area drag: (214,84)→(334,33) on (+120,−60) drag (y clamped at menu bar, native-like); on fixed build (214,84)→(114,136).
- Widget drags: frame bit-identical before/after (both widgets, both builds).
- Click-to-copy: pbpaste returned the exact cwd; screenshots show green "Copied" flash + reversion after 1.5 s; frame unchanged. Clipboard saved/restored.
- Double-click empty: zoomed to full visible frame (pref unset → Zoom); second double-click restored.
- Widget double-click: caught a REAL BUG in first build — "Copied" label shrank the widget, second press hit bared bar → zoom. Fixed with geometry-stable overlay; re-test frame unchanged, copy fired.
- Terminal input: 5×'a' + Return → grid shows aaaaa twice (echo+cat); keys sent 6, pty bytes 14.
- Clock: 16:12 → 16:14 across two boundaries with zero interaction; stayed demand-driven (metal draws 77 == scene rebuilds 77); watchdog auto-exit on time, exit 0.
- Headless: cargo build --bins clean; all headless self-tests PASS (default, INTERACTIVE, TRACE=selftest, ATLAS, WATCHDOG_SELFTEST).

## Caveats
- Bar on the interactive window only; run_live measurement windows left chrome-free to protect audited FPS/energy numbers.
- Minute ticker adds ≤1 present/minute to metal-draws count; App Nap may defer a tick when occluded (catches up on wake).
- Synthetic drags: window-server grab engages a few events after press → final delta slightly under synthetic cursor delta; real mouse tracks continuously.
- Test artifacts in /tmp/nice-ab-harness/ (outside repo).
