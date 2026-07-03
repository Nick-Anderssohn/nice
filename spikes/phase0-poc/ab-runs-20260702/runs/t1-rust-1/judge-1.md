# t1-rust-1 — judge 1 (claude-fable-5, blind, packet-r1)
edit_locality: 5 — single file gpui_term.rs, exactly where repo facts say interactive window lives; all hunks feature-relevant; /tmp harness never enters diff.
api_hallucination: 5 — count 0; no compile/run error citing nonexistent symbol; APIs verified against vendored source before use (seqs 10–15 titlebar_double_click, 41/52 performWindowDragWithEvent/currentEvent, 28–29 clipboard, 44–51 cursor_pointer); seqs 84–88 grepped invisible() before use and avoided it — hallucination avoided by checking.
iterations_to_green: 5 — count 2 build/run cycles (initial impl built seq 68 + one behavior-fix state edit 89 → build 90 green, live 91); seq 104 = final regression confirmation, not a fix cycle.
human_fixup_minutes: 5 — <5 min; verifier 9/9 live, O1–O4 PASS, log clean; at most cosmetic tweaks.
style_conformance: 5 — mirrors file idiom (rationale-heavy doc comments, kick_platform_display + cx.notify() discipline, unsafe objc2 matching file's AppKit helpers, builder-chain layout per vendored examples); import reflow only mechanical churn.
composite: 5.0
