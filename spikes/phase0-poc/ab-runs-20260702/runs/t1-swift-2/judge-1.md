# t1-swift-2 — judge 1 (claude-fable-5, blind, packet-s2)
edit_locality: 5 — expected files only; drift-banner padding is consequent, not excursion; no unrelated refactoring.
api_hallucination: 5 — count 0; single build (seq 52) BUILD SUCCEEDED (log 539), no prior failed compiles, no post-build source re-edits; edits 30–49 preceded first build, exploration-driven (25+ reads, 1–29).
iterations_to_green: 1 — 1 build cycle BUT never reached green as defined: objective gate FAIL (item 7, 3× repro — cwd-widget double-click zooms, violating stated behavior (4)); mechanism in final diff (Text(copied ? "Copied" : displayPath) inside .fixedSize() shrinks hitbox); rubric anchors "never green" at 1 despite low cycle count.
human_fixup_minutes: 3 — ~20–25 min: defect small and fully diagnosed (geometry-stable overlay fix in BottomStatusBarView.swift), but mergeable requires rebuild/install + manual re-verify of double-click/copy-flash on this xcodebuild pipeline → 15–30 band. Everything else clean.
style_conformance: 5 — ChromeWidgetGuard/Hosting modeled on PaneDragSource/Hosting (and says so); WindowChrome constants like topBarHeight; palette helpers; rationale comments match router commentary in tone; tests mirror adjacent shape incl. assertion-message idiom.
composite: 3.8
