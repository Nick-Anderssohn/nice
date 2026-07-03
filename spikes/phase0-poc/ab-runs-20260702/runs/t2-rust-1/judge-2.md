# t2-rust-1 — judge 2 (claude-fable-5, blind, packet-t2r)
edit_locality: 5 — one file; all hunks scoped to interactive-window gui module; appears_transparent/traffic_light_position + flex-column wrap necessary, not gratuitous.
api_hallucination: 5 — count 0; API surface verified before editing (12–34, incl. gpui-macros generated names 24–28); first build (45) after all eight edits, no fix loop; all diff APIs real gpui 0.2.2.
iterations_to_green: 5 — 1 build cycle; edits (35–44) precede build (45); headless (46) + first live (47) confirmed immediately; relaunches 59/70 were NICE_POC_SECS timeout restarts (58/69), zero edits after 44; final build 75 reconfirmed unchanged tree.
human_fixup_minutes: 5 — ~0–5 min; full objective PASS with ground-truthed behaviors (23.0pt pitch, 14×14 discs, persistence, resize/storm/FS pixel-identical, lights unregressed); at most bikeshed of documented geometry constants.
style_conformance: 5 — section banners, doc comments on every const/field, rationale voice (kick_platform_display note echoing demand-driven pattern), underscore-grouped hex literals, standard builder chains. Nothing foreign.
composite: 5.0
