# t1-rust-1 — judge 3 (claude-fable-5, blind, packet-r1)
edit_locality: 5 — exactly one file (gpui_term.rs = home of run_interactive per repo facts); every hunk serves feature; no vendored source/README/Cargo.toml touched; /tmp harness only (seq 71).
api_hallucination: 5 — count 0; APIs verified against vendored gpui 0.2.2 + objc2-app-kit bindings before writing (titlebar_double_click 10/13; drag APIs 41/52; ClipboardItem 28; ClickEvent 29; cursor_pointer 44–51; styled 53–55; invisible/transparent_black 84–88); no error citing nonexistent symbol; final log clean.
iterations_to_green: 5 — count 2: build at 68 green → straight to headless (69–70) + live CGEvent verification (74–83); one refinement (89, copied-flash) → build 90 green → live 91–101 + final pass 104.
human_fixup_minutes: 5 — ~0–5 min; verifier all 9 items incl. bit-identical widget frames + menu-bar-clamp parity with title bar; O1–O3 PASS; remaining nits documented tradeoffs.
style_conformance: 5 — dense rationale comments on every mechanism, doc-commented constants, section divider, reuse of file's own helpers + objc2/NSView conventions; context lines read continuously with new code.
composite: 5.0
