# t1-swift-2 — judge 3 (claude-fable-5, blind, packet-s2)
edit_locality: 5 — expected files; banner padding necessitated; no unrelated refactoring.
api_hallucination: 5 — count 0; research-heavy front (1–29), single build (52) zero errors; edits 46–49 self-directed refinements.
iterations_to_green: 1 — one build/install + test cycles but NEVER green: O3/item 7 FAIL (3× repro); mechanism in final diff (Text(copied ? ...) hitbox shrink); rubric "never green" → 1.
human_fixup_minutes: 3 — ~20–25 min; fix known (~10 lines geometry-stable overlay in StatusBarCwdWidget) but human must implement, rebuild/install, manually re-verify click matrix since unit suite was green while behavior wrong.
style_conformance: 5 — marker pattern, palette funcs, shared-constant+guard-test convention (statusBarHeight test matches topBarHeight test verbatim in shape), header/doc style matches router. Indistinguishable.
composite: 3.8
