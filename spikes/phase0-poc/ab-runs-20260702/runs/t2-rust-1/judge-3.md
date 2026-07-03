# t2-rust-1 — judge 3 (claude-fable-5, blind, packet-t2r)
edit_locality: 5 — one file (home of run_interactive); every hunk serves the feature; /tmp helpers (50/53/68) cleaned at 77, never in diff.
api_hallucination: 5 — count 0; every API pre-verified against vendored source + gpui-macros (12–34; styles.rs 26–28; traffic_light_position/move_traffic_light in mac/window.rs 9–11, 30–31); no compile error citing any symbol; zero Edits after first build (45).
iterations_to_green: 5 — 2 cycles to green (build 45 clean first attempt; live run 47 already working, pin1/pin2 screenshots); relaunches 59/70 = watchdog/FS-friction restarts, no code change; headless 46 first-try.
human_fixup_minutes: 5 — ~0–5 min; every item passed (+23.0pt across all states); only quibble = documented PIN_LEFT 78.0.
style_conformance: 5 — banners, doc-commented consts/fields, cx.listener closures, kick_platform_display with rationale matching existing wake/kick machinery; rgb(u32) + underscore hex matches.
composite: 5.0
