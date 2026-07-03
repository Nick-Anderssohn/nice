# t1-rust-2 — judge 2 (claude-fable-5, blind, packet-r2)
edit_locality: 5 — single file gpui_term.rs, exactly where a maintainer expects; vendor reading (seqs 7–60) read-only API verification; no unrelated modification.
api_hallucination: 5 — count 0; ALL edits (61,64–68,71–73) precede first build (74); first compile green, no error ever cited an invented symbol; APIs pre-verified to gpui-macros source level (13–20, 23, 29, 56–60).
iterations_to_green: 5 — count 1 build cycle to green (74), headless (75) + interactive (76) immediately pass, zero fix edits after; later runs pure verification.
human_fixup_minutes: 5 — ~0–5 min; verifier 9/9 with real CGEvents; only nits: unjoined clock ticker thread, libc::localtime_r vs time crate — debatable-preference in a spike, not blockers.
style_conformance: 5 — banner comments, dense rationale docs matching existing demand-driven commentary, SCREAMING_CASE rgb consts, msg_send! unsafe patterns identical to surrounding objc2, spawn/detach + wake-channel mirrors existing pty listener; style-helper naming confirmed against gpui-macros first.
composite: 5.0
