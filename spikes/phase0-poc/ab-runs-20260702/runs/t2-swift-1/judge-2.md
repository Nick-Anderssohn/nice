# t2-swift-1 — judge 2 (claude-fable-5, blind, packet-t2s)
edit_locality: 5 — three expected files; placer edits additive; exploration correctly steered past the WindowToolbarView hint to the actual chrome locus.
api_hallucination: 5 — count 0; only build succeeded first attempt; all APIs real AppKit.
iterations_to_green: 1 — 1 compile cycle (27; 30/34 = verifier-confirmed harness bug, environment-attributed; test 37; live 39–79 no further edits) BUT ended not-green (O3 FAIL, baseline-confirmed hover regression vs explicit requirement) → anchor 1.
human_fixup_minutes: 2 — ~45–60 min: diagnose why pin in lights' superview suppresses private hover-glyph tracking, experiment with parenting/tracking areas, re-verify with real hover + baseline control; fixer starts from misleading self-report (59–60).
style_conformance: 5 — heavy why-commentary matching placer; same absolute-target/0.5pt-tolerance convergence; tests continue lettered MARKs reusing file's helpers.
composite: 3.6
