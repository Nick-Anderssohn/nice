# t2-swift-1 — judge 1 (claude-fable-5, blind, packet-t2s)
edit_locality: 5 — exactly three expected files (new ChromePinButton, TrafficLightPlacer owner, its tests); exploration read-only; nothing extraneous.
api_hallucination: 5 — count 0; no compile errors citing invented symbols; BUILD SUCCEEDED (log 428); all diff APIs real AppKit.
iterations_to_green: 1 — low cycle count (install 27, harness-attributed COPY abort re-run 30, manual staging 34, test 37, live 39) but final state never green: item-4 hover-glyph regression, baseline-differentially attributed, against explicit criterion. Install failure environment-attributed, not counted.
human_fixup_minutes: 2 — 30–60 min: subtle AppKit-internals problem (fourth subview in lights' superview disrupting theme-frame rollover), manual hover repro + experimentation; aggravated by implementer self-reporting hover as working (human must first discover the bug).
style_conformance: 5 — why-focused header narration matching GEOMETRY commentary; absolute-target/convergence framing; tolerance-guarded frame writes; tests continue lettered MARK pattern w/ same helpers.
composite: 3.6
