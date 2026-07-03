# t1-rust-2 — judge 1 (claude-fable-5, blind, packet-r2)
edit_locality: 5 — one file (gpui_term.rs); seqs 7–60 read-only vendored-API verification; edits 61–73 all target the one file; minimal restructuring only where feature required.
api_hallucination: 5 — count 0; ~50 front-loaded verification reads (9–60) confirming every API before writing (titlebar_double_click 13/20; occlude/hitbox 23/39; ClipboardItem 29; listener sigs 38; px helpers via gpui-macros 56–60); no build error citing invented symbol; no post-build corrective edits; runtime verifier exercised risky calls, all PASS.
iterations_to_green: 5 — count 1: first build (74) green, no edits follow (75–76 headless runs, 79–84 live verification; 85 warning check only). Zero edit-after-failure iterations.
human_fixup_minutes: 5 — <5 min; build clean (only pre-existing block v0.1.6 note, log 348–349); verifier 9/9 incl. negative cases; only quibble libc::localtime_r unsafe, defensible in a msg_send!-heavy spike.
style_conformance: 5 — rationale comments in surrounding voice (App Nap, no-RAF, kick_platform_display), null-checked msg_send! matching patterns, module consts, SharedString, clock ticker wired through same wake-channel + notify + present-kick as pty reader. Indistinguishable from host file.
composite: 5.0
