# t1-rust-2 — judge 3 (claude-fable-5, blind, packet-r2)
edit_locality: 5 — one file (gpui_term.rs = interactive-window home); edits seq 61, 64–73 all there; vendor/gpui-macros reads (7–60) read-only; layout change confined to interactive view's render path.
api_hallucination: 5 — count 0; every nontrivial API verified against vendored source pre-write (titlebar_double_click 13/20; occlude/hitbox 23/33; ClipboardItem 29; styled via gpui-macros 56–60; performWindowDragWithEvent 13–19); first build (74) needed no follow-up edits.
iterations_to_green: 5 — 1 cycle: build 74 green; 75 headless + 76 interactive pass, no edits after; 79/82/85 verification + warning check of unchanged tree.
human_fixup_minutes: 5 — ~0–5 min; verifier 9/9 with real CGEvents; log clean bar pre-existing note; optional nitpicks only (libc clock; badge-dismiss redraw reliance).
style_conformance: 5 — heavily narrated rationale comments matching "deadline must NOT live in render()" voice; msg_send! like existing ns_view plumbing; module consts; same notify + present-kick pattern as pty echo path. Nothing foreign.
composite: 5.0
