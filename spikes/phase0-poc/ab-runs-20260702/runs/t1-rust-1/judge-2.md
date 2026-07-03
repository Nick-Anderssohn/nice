# t1-rust-1 — judge 2 (claude-fable-5, blind, packet-r1)
edit_locality: 5 — diff confined to gpui_term.rs (exact file repo facts name); every hunk serves feature; /tmp harness artifacts (seq 71–73) never touch tree.
api_hallucination: 5 — count 0; every nontrivial API verified against vendored sources before use (titlebar_double_click 10/13; performWindowDragWithEvent/currentEvent 41/52; styled methods 43–55; checked invisible/inset_0/transparent_black 84–88, found absent in this gpui, used rgba(0x00000000)+explicit edges instead). No build followed by compile-error fix edit.
iterations_to_green: 5 — count 2 edit→build cycles (first impl seq 68 green; one polish edit 89 → rebuild 90 clean, re-verified 91–102; 104 re-verifies same tree).
human_fixup_minutes: 5 — 0–5 min; verifier 9/9 with frame differentials; log only pre-existing warnings; nits (libc localtime_r, App Nap caveat) documented in-line, appropriate for a spike.
style_conformance: 5 — matches idiom: rationale doc comments in neighbors' voice, section divider matching existing, kick_platform_display reuse, unsafe objc2 shaped like adjacent screen_info_of_view, consts alongside FONT_PX/LINE_PX; dependency-conservative.
composite: 5.0
