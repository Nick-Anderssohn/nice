# t1-swift-2 — judge 2 (claude-fable-5, blind, packet-s2)
edit_locality: 5 — expected files; FileOperationDriftBanner padding necessitated by bar; no unrelated refactoring.
api_hallucination: 5 — count 0; only build (52) BUILD SUCCEEDED; pre-build edits exploratory not compiler-driven; real APIs throughout.
iterations_to_green: 1 — 2 locally-green build cycles BUT per counting rule green = meets acceptance per objective verification; gate FAIL (item 7, 3×) → never green → anchor 1.
human_fixup_minutes: 3 — ~15–25 min; defect fully diagnosed; small geometry-stable overlay fix in one file; rebuild under lock + manual re-verify dominates.
style_conformance: 5 — ChromeWidgetHosting modeled on PaneDragHosting; WindowChrome constant beside topBarHeight w/ desync rationale; palette helpers; tests match suites' naming/comment idiom.
composite: 3.8
