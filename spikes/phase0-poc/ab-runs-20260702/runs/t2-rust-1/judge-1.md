# t2-rust-1 — judge 1 (claude-fable-5, blind, packet-t2r)
edit_locality: 5 — one file (gpui_term.rs, where run_interactive lives); /tmp verification tooling deleted at seq 77; vendor reads research-only.
api_hallucination: 5 — count 0; front-loaded verification of exact builder methods (rounded_full, border_1, flex_shrink_0, on_click, Stateful, From<Rgba>; seqs 12–34); first build (45) zero corrective edits; final diff uses only confirmed APIs.
iterations_to_green: 5 — 1 build cycle: edits 35–44 → build 45 green, headless 46 pass, first live run 47 already working (screenshots 52/54); relaunches 59/70 were watchdog restarts, code never edited after 44.
human_fixup_minutes: 5 — ~0–5 min; verifier passed every gate incl. exact 23.0pt pitch, FS round-trip, 10× storm; only residual cosmetic: PIN_LEFT 78.0 hardcodes macOS 26 geometry, documented with derivation and measured correct.
style_conformance: 5 — dense doc comments matching ns_view/render commentary, section banner, documented const geometry, kick_platform_display reuse in click handler, builder chains consistent with grid_canvas. Nothing foreign.
composite: 5.0
