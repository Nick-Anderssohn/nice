---
title: Nice Chrome Rewrite — Stack Research
generated: 2026-06-26
workflow_run: wf_3ce45d97-e8f
agents: 31
note: >
  Produced by a multi-agent research workflow (feature inventory -> candidate
  enumeration -> per-candidate deep web research -> adversarial verification ->
  4 compiled spikes -> weighted synthesis + completeness critic). One candidate
  (Slint + alacritty_terminal) failed its structured deep-dive in the workflow
  (StructuredOutput retry cap) and is therefore ABSENT from the head-to-head
  synthesis below; it was researched out-of-band and appended as an addendum.
---

# Nice Chrome Rewrite — Architecture Decision Report

## 1. Executive recommendation

- **Chrome stack: Rust + GPUI** (Zed's GPU UI framework, now shippable standalone from crates.io). This gets the entire UI chrome — the actual Swift pain point — off Swift/SwiftUI/AppKit.
- **Terminal decision: REUSE the SwiftTerm Metal NSView, embedded under GPUI via objc2, as the primary plan** (the renderer is the crown jewel and is not a pain point). **Two fallbacks if the embed can't hit FPS/input/IME parity: (Path B)** `alacritty_terminal` + a GPUI‑native `TerminalView` (zero Swift, rebuild renderer); **(objc2‑hybrid)** hand‑built AppKit chrome that keeps the renderer with **no z‑order seam** (see §4). **`libghostty` is out** (zig‑gated, parser‑only — buys nothing over alacritty today; spike‑proven).
- **Shape: hybrid** (non‑Swift chrome over the reused Swift terminal view), with a clean‑native or objc2 fallback for the terminal layer.
- **Confidence:** Medium‑high on the chrome pick (GPUI) *given that a rewrite happens at all*; **Medium on the Path‑A reuse‑embed, and the Path‑A efficiency PASS is *provisional*** — both gated on one PoC (§10).
- **Recommend the rewrite only if:** (a) Rust‑**language** velocity on the chrome is weighted above the multi‑month rewrite cost and regression risk across 55+ must‑support features; **and/or** (b) shedding the SwiftUI‑fighting machinery and Linux‑capability are valued. If neither holds, the residual case for *not* rewriting is the multi‑month cost + regression risk — **not** a cheaper in‑place chrome win: per the repo's prior research (§2), the native‑title‑bar escape is spiked‑out (`spike/native‑titlebar`) and the in‑place chrome re‑architecture has already shipped, so chrome AI‑assistability **cannot be lifted in place**. The rewrite's framework‑level AI‑velocity gain over the status quo is **zero** (D2 2→2, §2); the case rests entirely on the language axis plus structural cleanup.
- **Gate before commitment (§10):** the Phase‑0 PoC must (i) **measure** burst FPS, keystroke latency, and idle+under‑load memory of the **dual‑Metal‑stack hybrid** against the current Nice baseline on this machine — the Path‑A efficiency PASS is provisional until this number exists — and (ii) drive **real OS keyboard/mouse/IME/focus routing through a live key window**, plus cross‑window Metal rebind. **RUN & VERIFIED (2026-06-27, see §10):** proofs 4/6/7 + memory PASS (native; proof 5 routing solid but end-to-end intermittent). FPS surfaced a **real, CSV-verified yellow flag** — on a recorded **60 Hz** panel, **none of the three naive present schemes tested** (`sync`/`async`/`link`) drives both the terminal *and* animated chrome at refresh: best symmetric is 30/30, and decoupling the terminal to 60 starves GPUI to ~1.4 fps. Cause: two `CAMetalLayer`s in one `NSWindow` share a ~one-commit-per-vsync main-thread budget (not a compute tax — `draw-attempt` ≈ 0.02 ms). **UPDATE (2026-06-27, gating experiment (a) now RUN — see §10 "transactional co-paced present"):** the 30/30 ceiling is a **double-present artifact, not an irreducible tax.** A transactional co-paced present (`presentsWithTransaction` — `commit→waitUntilScheduled→drawable.present()` inside one CA transaction; an OPT-IN, default-OFF fork patch on branch `phase0-txn-present`) lifts **both** stacks from 30/30 to **~54–56/~54–56 fps** (near refresh), collapses the main-thread present block 16.3→3.6 ms, and quarters keystroke latency (16.7→3.9 ms), memory still native. **This refutes the Path-B tilt:** two Metal layers in one window *can* co-run near refresh if they co-commit. The flag is downgraded from "Path-B-favoring" to "viable for Path A; a locked 60 (vs the measured ~55) is a tuning question, not a feasibility one." Remaining open: (b) close proof 5 end-to-end, (c) a 120 Hz ProMotion re-measure (this machine's built-in is hard 60 Hz).
- **⤷ Criteria caveat (added 2026‑06‑27, see §11):** this recommendation holds under the report's *original* criteria (AI velocity primary; reuse + bounded migration cost are real assets). Under a **deliberately re‑weighted** set — *maximize end‑product quality, give reuse zero credit, accept any rebuild cost* — the call **flips to Path B** (all‑Rust single Metal stack). §11 re‑ranks all options under those criteria; §12 reports the de‑risking research that gates Path B vs Path A.

---

## 2. The baseline you must beat: stay on Swift (null hypothesis)

> **⚠️ Correction (post‑synthesis, 2026‑06‑26): the original synthesis was written from the code inventory and never ingested the repo's prior chrome research — it overstates this baseline.** Sources: `docs/research/refactor-recommendation.md`, `docs/done/in-place-refactor-handoff.md`, `docs/window-chrome-architecture.md`, git tag `spike/native-titlebar`. Two missed facts: **(1)** the *native* AppKit title bar (`NSTitlebarAccessoryViewController` / `NSToolbar`) was **spiked and proven unviable** for Nice's design — accessory height is capped at 32/36pt (the band needs 52pt), the system title area is unthemeable (seams against non‑system palettes), and it breaks the full‑height floating sidebar — so the clean, Claude‑friendly *"go native"* refactor **does not exist**; and **(2)** the in‑place chrome **re‑architecture has already shipped** (`WindowChromeController` / `ChromeEventRouter` / `TrafficLightPlacer`, single‑owner, computed‑per‑event, fully tested). So the "in‑place AppKit refactor" is **not a fresh, cheap lever — it is the painful status quo you already live in**, and the baseline's chrome stays **D2 ≈ 2 (ceiling reached)**. This *strengthens* the rewrite case below.

The honest comparator for a rewrite is **not another stack — it is the shipping app essentially as it already is.** Nice works today, its terminal layer is explicitly *pleasant / not a pain point*, and the pain is localized to specific SwiftUI chrome. Scored on the same rubric:

| Option | D1 | D2 | D3 | D4 | D5 | D6 | D7 | D8 | D9 |
|---|---|---|---|---|---|---|---|---|---|
| **Stay on Swift (in‑place chrome refactor — already shipped)** | 3 | **2 (ceiling reached)** | 5 | 5 | 5 | 4 | 5 | 1 | 5 |

What the baseline wins **by definition**: efficiency (it *is* the bar), styling (pixel‑perfect), terminal (the crown jewel untouched), maturity (Apple‑supported), and migration (near‑zero — incremental, no 55+‑feature reimplementation, no regression surface, no FFI seam).

Where it loses: **D1 language velocity (Swift 3 vs Rust 5)** and **D8 Linux (1)**. The decisive fact the rest of this report is built around: **D2 — framework AI‑velocity — is a wash.** This report scores GPUI's framework corpus **D2 = 2, "comparable to SwiftUI."** So *the recommended rewrite delivers no framework‑level AI‑assistability gain over the status quo.* The entire primary‑driver payoff is on the **language axis** (Rust vs Swift), plus secondary structural wins (shedding SwiftUI workarounds, Linux‑capability).

The in‑place lever this report must not ignore: the worst SwiftUI chrome can be moved to **imperative AppKit in place**, which Claude knows materially better than SwiftUI's churning API (the objc2 evaluation notes "Claude knows AppKit concepts well"). That alone could lift the chrome's framework‑assistability. **But the repo's prior chrome research (folded in post‑synthesis) shows that in‑place lift is *not actually available*:** the native AppKit title bar is spiked‑out (`spike/native-titlebar`) and the in‑place chrome re‑architecture has *already shipped* (`WindowChromeController` / `ChromeEventRouter` / `TrafficLightPlacer`, single‑owner + fully tested), so the chrome is **already at its in‑place ceiling (~2, not ~3)**. What that in‑place path actually produced is exactly the SwiftUI‑fighting machinery the report flags elsewhere (the `WindowGroup` / nil‑value de‑dup protocol; the `LivePaneRegistry` / `WindowClaimLedger` token‑seed dance that exists *only* to fight SwiftUI value types; the `WindowBridge` runloop‑ordering dance; the router's attribute‑walk fallback that exists *only* because SwiftUI buries the drag strip in a sibling wrapper a class‑walk can't reach) — every item an artifact of SwiftUI↔AppKit impedance, which is precisely what a non‑SwiftUI chrome removes.

**So the rewrite's case reduces to one question:**

> Is a Swift→Rust **language** jump (D1 3→5 on the chrome) plus **escaping the SwiftUI↔AppKit impedance** the prior chrome docs identify as the real pain, plus Linux‑capability, worth a multi‑month rewrite of ~64k LOC and 55+ features — given that the **framework** AI‑velocity is unchanged (D2 2→2, on *both* sides) and the recommended Path A *keeps* the Swift toolchain anyway (§3)? Note the in‑place baseline is **not** a cheaper way to capture the chrome win: the native‑title‑bar escape is spiked‑out and the in‑place re‑architecture is already done (above).

This report recommends the rewrite — and with the baseline corrected, the case is **stronger** than the original §1 hedge implied: the "just refactor in place" alternative is largely *already spent* (native‑title‑bar escape spiked‑out; in‑place chrome re‑architecture shipped). The honest residual reasons *not* to rewrite are now narrower: (i) the multi‑month cost + regression risk across 55+ features; (ii) the unproven Path‑A efficiency/seam (§10); and (iii) if Rust‑language velocity is not valued, the language‑axis win shrinks. The chrome‑assistability win itself is **not** recoverable in place — it requires leaving SwiftUI's view tree.

---

## 3. Why GPUI (if rewriting)

### Hard constraint first: native efficiency — clean for the *framework*, **provisional for the recommended hybrid**
**GPUI the framework** clears "match or beat the current native Swift app" more cleanly than any other candidate: it is the exact stack Zed ships (compiled Rust, no GC, no JS runtime, GPU‑composited via its own Metal/blade‑graphics pipeline), and the spike confirmed it builds and runs standalone on this machine (macOS 26.5.1, Apple silicon) with no native code written by us. **Path B inherits that Zed‑class efficiency directly.**

**But the *recommended* Path A is not "GPUI" — it is a dual‑Metal‑stack hybrid:** GPUI's blade‑graphics pipeline compositing chrome *over* SwiftTerm's bespoke Metal renderer, in one process, across an objc2 z‑order seam. That is precisely the "runs a second GPU stack" objection this report uses to mark **down** `flutter-reuse-swiftterm` and to caution `flutter-ffi` — and honesty requires applying it to our own hybrid. **No FPS, latency, or memory has been measured for the dual‑stack hybrid;** the spikes only confirmed that GPUI launches and that an NSView can be hosted under objc2 — *not* that two live Metal stacks coexist at burst FPS inside the Swift baseline's memory envelope, and *not* through a live responder chain (the embed spike used a Swift **stub** view and invoked overridden selectors directly). **Path A's efficiency PASS is therefore provisional** (scored `5†` in §5) and must be confirmed by Phase‑0 against the current Nice baseline before commitment. If the hybrid regresses FPS or memory, the efficiency‑clean options are **Path B** (single GPUI stack) or **objc2‑hybrid** (single AppKit‑owned tree, §4).

Below the hybrid, nothing changes: **Tauri (`tauri-xtermjs`) and Dioxus (`dioxus-alacritty`) remain eliminated** on this hard bar (WKWebView capped at 60 Hz on ProMotion + jitter under burst; Dioxus's only native path, Blitz, is alpha and explicitly "not performance‑tuned"). **Flutter** passes but sits *slightly above* the Swift idle baseline (~30–40 MB engine/Skia base), and its reuse variant runs the same second‑GPU‑stack risk.

### Primary driver: AI velocity — D1 vs D2 honestly separated
- **D1 (language corpus):** Rust is top‑tier for Claude (deep corpus + a borrow‑checker/`rustc` self‑correction loop that beats Swift's slower, sometimes‑silent SwiftUI failure mode). **GPUI scores D1 = 5.**
- **D2 (UI‑framework corpus):** **GPUI is genuinely weak — comparable to SwiftUI** (pre‑1.0, Zed‑internal, churning API, thin public corpus). **D2 = 2.** Rust's D1 strength does **not** mask this.

So GPUI's AI‑velocity improvement over SwiftUI is **language‑driven, not framework‑driven.** Two mechanisms are often conflated here, and **only one drives Claude's default reliability**:

- **(a) Training‑corpus depth — what actually shapes Claude's in‑weights priors — is genuinely thin and churning for GPUI (D2 = 2, unchanged from SwiftUI).** This is the number that governs how reliably Claude writes correct GPUI 0.7 chrome *by default*, and it does not beat SwiftUI.
- **(b) Dev‑time augmentation — feeding current Zed / `gpui-component` source into context — is real but weaker than the framing implies.** The entire Zed source is open, high‑quality GPUI usage, and `longbridge/gpui-component` adds 60+ documented components; a developer or a retrieval‑augmented session can lean on them. **But** this requires the human to supply *current* sources, and because GPUI's API churns (the tree already moved `crates/gpui → crates/gpui_macos`; 0.2.2 is ~8 months stale), fed‑in current source can actively *conflict* with the model's stale in‑weights snapshot. So (b) mitigates but does **not** lift the near‑hard primary‑driver bar the way a deep native corpus (Flutter/React) would. **D2 stays 2.**

Net: the chrome becomes more AI‑assistable **because of the language, not the framework** — a defensible win that clears the near‑hard bar, not the Electron/React‑class jump it is sometimes painted as.

### Why GPUI over Flutter (the genuinely stronger primary‑driver candidate)
**Flutter is the stronger two‑axis AI bet** (D1 = 4 Dart, **D2 = 5** — one of the best‑documented UI toolkits anywhere). It was weighed seriously. It loses to GPUI on three grounds:
1. It is weaker on the **hard** efficiency constraint (above) — though note GPUI's own Path‑A efficiency is itself provisional.
2. Its huge D2 corpus is concentrated in **mobile/general widgets**; Nice's hardest chrome (vibrancy wiring, native traffic‑light geometry, NSDraggingSource tear‑off, multi‑window) lives in Flutter desktop's *thin* corpus **and crosses into hand‑written Swift/ObjC**, where Claude loses the Dart‑analyzer loop. The AI win is weakest exactly where Nice is hardest.
3. **Zero Swift is *not* a Path‑A differentiator.** Flutter keeps vibrancy, traffic‑light placement, tear‑off detection, and the close‑confirm delegate in a residual Swift/ObjC shim. **GPUI reaches zero Swift only on Path B** (rebuild the terminal on `alacritty_terminal`); the **recommended Path A keeps swiftc in the build permanently** (the `@_cdecl` shim + reverse‑FFI + cargo+swiftc two‑toolchain — spike‑confirmed). So on Path A, *both* stacks keep Swift; zero‑Swift is a real GPUI edge **only in the Path‑B world**, which — like Flutter‑FFI — sacrifices the renderer reuse. Treat zero‑Swift as a tie‑breaker for Path B, not a virtue of the recommended hybrid.

### Where the spikes changed the picture
1. **GPUI is no longer "Zed‑internal, can't depend on it."** `gpui = "0.2.2"` is on crates.io, compiled standalone in ~36 s, ran first try. (`spike-gpui-glass`: `…/scratchpad/spike-gpui-glass/glassdemo`)
2. **Vibrancy + native traffic lights are built into GPUI with zero native code.** `WindowBackgroundAppearance::Blurred` inserts a real `NSVisualEffectView`; `TitlebarOptions.traffic_light_position` repositions the *real* native buttons — refuting the desk claim that GPUI relies on a private CGS API. (`gpui-0.2.2/src/platform/mac/window.rs:1257‑1311`, `:2500‑2510`)
3. **`libghostty` is dead on arrival; `alacritty_terminal` is the proven Rust core.** `alacritty_terminal` + `portable-pty` ran a real `/bin/sh`, parsed the PTY stream, decoded per‑cell SGR with zero build friction; `libghostty-vt` failed at `build.rs:365` (zig absent) and is parser‑only. (`spike-rust-term`; `ghostty-probe`)
4. **Hosting the SwiftTerm NSView under a Rust/objc2 host is mechanically solved — but only at the stub/selector level.** A pure‑objc2 NSView and a Swift‑compiled NSView were hosted, made first responder, resized, and driven across a C ABI (~150 lines, PASS). **Crucially, the spike dispatched events by invoking overridden selectors directly, *not* through a live windowserver responder chain, and used a stub — not the real SwiftTerm view** (`spike-reuse-swiftterm` "whatFailed"). The remaining cost is the FFI shim + two‑toolchain build **and** the unproven live‑responder/IME and z‑order seams (§4, §10).

---

## 4. GPUI‑hybrid vs objc2‑hybrid: the near‑twin the report must not dodge

The recommended pick and the *rejected* `objc2-reuse-swiftterm` candidate are **nearly the same architecture.** Both embed the identical SwiftTerm Metal NSView; both keep the Swift toolchain under Path A; both reach Rust for the chrome; **both score D2 = 2.** The recommended GPUI hybrid is, structurally, *the objc2 candidate with GPUI swapped in for hand‑written AppKit chrome.*

So the report's stated reason for rejecting objc2 — "it fails the primary driver (D2 = 2, hand‑writing AppKit)" — **cannot be the real differentiator, because GPUI fails that same framework‑corpus bar.** The honest head‑to‑head:

| | GPUI‑hybrid (recommended) | objc2‑hybrid (`objc2-reuse-swiftterm`) |
|---|---|---|
| Framework corpus (D2) | **2** | **2** |
| Language safety (D1) | **5** (pure safe Rust) | 4 (unsafe `msg_send!` holes in the AppKit‑touching code) |
| Chrome authoring model | **Declarative** element/component tree — better DX for the high‑volume chrome iteration that is the whole point | **Imperative**, verbose, hand‑written AppKit through objc2; example‑thin, no rich‑chrome reference app |
| SwiftUI‑fighting machinery | Gone (not SwiftUI) | Gone (not SwiftUI) |
| SwiftTerm NSView embed | **Unproven z‑order/transparency/responder seam** — GPUI draws its *whole* content view itself; a foreign terminal view must sit at the right z‑order with GPUI compositing transparently over it | **Trivially proven (D5 = 5)** — objc2 owns the entire NSView tree, so the terminal is just `addSubview`; **no seam** |
| Maturity (D7) | 2 (pre‑1.0, single‑vendor, churning) | 3 (objc2 is the de‑facto Rust↔Apple standard, broadly adopted) |

**The real, defensible case for GPUI over objc2 is NOT "objc2 fails the primary driver" — it is the declarative‑DX/maintainability axis plus full safe‑Rust (D1 5 vs 4).** GPUI gives a declarative component framework for the high‑volume chrome (tabs, pills, settings, theming, file tree); objc2 makes you hand‑write every bit of that imperatively in a niche, unsafe, example‑thin dialect.

**But that edge is bought at a price the report must own:** GPUI *uniquely introduces* the z‑order/transparency/responder seam that is this report's single load‑bearing risk (§9, §10). objc2 *avoids that seam entirely* by owning the view tree. We are therefore recommending the option that carries its own biggest open risk over a sibling that doesn't — justified **only** if (a) the declarative‑DX win outweighs the seam risk, and (b) Phase‑0 retires the seam.

**Consequence for the fallback tree:** if Phase‑0 fails *specifically on the GPUI z‑order/responder seam* (rather than on AppKit event embedding in general), the natural fallback is **objc2‑hybrid** — it keeps the proven renderer *and* avoids the seam, at the cost of hand‑written imperative chrome — **not only Path B.** objc2 is a first‑class fallback, not a dismissed also‑ran.

---

## 5. Ranked comparison (verified/adjusted scores)

Scores 1–5, adjusted for the adversarial verdicts and spikes. Dimensions: **D1** AI‑Language · **D2** AI‑Framework · **D3** Efficiency (hard) · **D4** Styling · **D5** Terminal · **D6** Windowing · **D7** Maturity · **D8** Linux · **D9** Migration.

| # | Candidate | D1 | D2 | D3 | D4 | D5 | D6 | D7 | D8 | D9 | Net |
|---|-----------|----|----|----|----|----|----|----|----|----|-----|
| 0 | **Baseline: stay on Swift (in‑place chrome refactor — already shipped)** | 3 | **2** | 5 | 5 | 5 | 4 | 5 | 1 | 5 | **The bar to beat (§2) — but lower than first scored.** Lowest risk; pleasant terminal untouched; pixel‑perfect. **Correction:** native‑title‑bar escape is spiked‑out (`spike/native-titlebar`) and the in‑place re‑architecture already shipped, so chrome D2 is **stuck at 2, not liftable to ~3**. Rewrite's gains: language‑axis velocity (D1 3→5), **escaping SwiftUI↔AppKit impedance** (the documented chrome pain), and Linux. |
| 1 | **Rust + GPUI** (+ reuse SwiftTerm / alacritty / objc2 fallback) | 5 | 2 | **5†** | **4** | 4 | 3 | 2 | 4 | 2 | **Recommended (given a rewrite).** Clears hard efficiency as a framework / Path B; **Path A dual‑stack efficiency provisional pending Phase‑0.** AI win is language‑driven (D2=2 wash vs SwiftUI); vibrancy+traffic lights spike‑proven built‑in. Load‑bearing risks: GPUI z‑order/responder seam, pre‑1.0 churn, reuse % conditional. |
| 2 | **Flutter + Rust‑FFI** (`flutter-ffi`) | 4 | **5** | 4 | 4 | 3 | 3 | **4** | 4 | 2 | **Runner‑up.** Best two‑axis AI velocity, mature, real vibrancy + traffic lights. Slight idle overhead, residual Swift shim, macOS multi‑window lags, renderer rebuilt (no NSView gestures). |
| 3 | **C++/Qt (QML)** (+ alacritty via C‑ABI) | 4 | 4 | 4 | 4 | 3 | 4 | **5** | **5** | 2 | Maturity + Linux champion. Real NSWindow → vibrancy reachable, but proven only for Qt **Widgets** (private API, macOS‑26‑gated); QML path unproven. C++ weak safety net; stock macOS controls fall back to Fusion. |
| 4 | **objc2 hand‑built AppKit** (`objc2-reuse-swiftterm`) | 4 | **2** | **5** | **5** | 5 | 5 | 3 | 1 | 4 | **Near‑twin of the pick (§4), not a primary‑driver loser** — shares D2=2. Differs by imperative hand‑written chrome (worse DX) but **avoids GPUI's z‑order seam**. macOS‑only. **First‑class fallback if the seam PoC fails.** |
| 5 | **Flutter + embedded SwiftTerm** (`flutter-reuse-swiftterm`) | 4 | 5 | 3 | 4 | 3 | 2 | 4 | 2 | 4 | Right reuse intent, but macOS platform views are **experimental with NO gesture support** — fatal for terminal selection/mouse — plus hybrid‑composition FPS risk and experimental multi‑window. |
| 6 | **Rust + Dioxus** (`dioxus-alacritty`) | 4 | 3 | **2** | 3 | 3 | 2 | 3 | 4 | 2 | React‑shaped RSX helps, but default renderer is a webview and native Blitz is alpha → **hard efficiency unproven today.** Multi‑window buggy. |
| 7 | **Tauri + xterm.js** (`tauri-xtermjs`) | **5** | **5** | **2** | 4 | 2 | 4 | **5** | 3 | 2 | Best AI velocity + maturity, real vibrancy — but **WKWebView 60 Hz ProMotion cap + jitter under burst → fails hard native efficiency**, and discards the renderer. Eliminated. |

**† Path‑A efficiency is provisional.** GPUI‑the‑framework and Path B clear the hard bar with Zed‑class evidence; the recommended **Path A dual‑Metal‑stack hybrid is unmeasured** and scored on that basis only until Phase‑0 (§10) confirms FPS/latency/memory against the current Nice baseline. The same "second GPU stack" caveat applied to the Flutter‑reuse row applies here.

---

## 6. The liquid‑glass verdict

**The signature look can be preserved — and the spikes prove it is *not* a GPUI‑specific or AppKit‑exclusive problem.**

- **GPUI, free, today:** real `NSVisualEffectView` behind‑window vibrancy + rounded corners + a frosted translucent panel + native traffic lights repositioned over a transparent titlebar — ~90 lines of safe Rust, zero objc/Metal. **~80–90 % of Nice's vibrancy look for free.** (`spike-gpui-glass`)
- **The one gap:** GPUI hardcodes the effect material to `.Selection`; Nice leans on `.sidebar`. Closing it is a **bounded ~20‑line GPUI fork** of `set_background_appearance`, **or** one `objc2`/`cocoa` subview (already transitive GPUI deps — Rust‑objc, *not* a new toolchain or a Metal shader).
- **macOS 26 "Liquid Glass" (`NSGlassEffectView`)** is reachable from Rust if ever wanted (alt‑UI spike instantiated it from a raw winit+objc2 app at runtime, `GLASS_PRESENT=1`). **But Nice uses zero `NSGlassEffectView` today** (grep = 0 hits), so the parity bar is **classic vibrancy with a chosen material — cheaply reachable.** (`spike-altui-vibrancy`)

**Net:** preserve, not simplify. Desktop‑tinting wallpaper‑sampling vibrancy and *genuine* native traffic lights survive on GPUI. The four‑palette semantic‑NSColor pixel‑match and continuous‑squircle corners are the hand‑maintained tail, as on any non‑AppKit stack.

---

## 7. Runner‑up: Flutter + Rust‑FFI core (`flutter-ffi`)

**Pick Flutter instead if any of these hold:**

1. **AI velocity is weighted strictly above the last increment of native efficiency.** Flutter's two‑axis corpus win (D2 = 5) is the strongest primary‑driver story in the field — and, unlike GPUI, it is a *framework* win, not just a language win. If broad, well‑documented chrome iteration matters more than GPUI's language‑only edge, Flutter is the better primary‑driver bet.
2. **Linux is promoted from bonus to a real target.** Flutter's first‑class GTK embedder beats GPUI's flaky Wayland blur (`#27683`, won't‑fix).
3. **GPUI's pre‑1.0, single‑vendor (Zed) churn is judged unacceptable.** Flutter is mature, 1.0+, broadly‑stewarded — far lower bus‑factor/API‑churn risk.
4. **The GPUI‑hosts‑SwiftTerm PoC fails *and* the team prefers not to rebuild the renderer on GPUI's text pipeline** — Flutter‑FFI's external‑texture Metal path is a comparable rebuild with a friendlier chrome corpus. (Note: if the PoC fails *only on the z‑order seam*, **objc2‑hybrid** preserves the renderer with no rebuild — weigh that against Flutter.)

Accept, if choosing Flutter: a residual Swift/ObjC shim (no "zero Swift" — but the recommended GPUI Path A keeps Swift too, §3), a rebuilt renderer (platform‑view gesture gap is fatal for live NSView reuse), and macOS multi‑window/tear‑off riding the least‑mature surface.

---

## 8. Migration strategy

**Shape: hybrid (recommended) with a clean‑native or objc2 fallback for the terminal layer.** The chrome is a full rewrite either way — that is the goal. The decision fork is the *terminal layer* (§4, §10).

### What of the Metal‑renderer work is reusable, and the interop cost

**Path A — REUSE (hybrid, recommended primary):** embed the existing SwiftTerm fork + ~2,970‑line Metal renderer as an NSView subview under GPUI.
- **Reusable: renderer + full input stack — *conditional on the Phase‑0 seam passing*, not an unconditional ~100 %.** *If* first‑responder/IME/focus arbitration between GPUI's focus system and the embedded terminal routes correctly through a **live responder chain**, then the renderer (sub‑line smooth GPU scroll, selection‑across‑eviction, triple‑buffered pacing, dual atlases, kitty graphics/sixel) **and** the entire input stack baked into that NSView (kitty keyboard, legacy VT, `NSTextInputClient`/IME, VT mouse, copy/paste, drag‑and‑drop) ride along with no rebuild. **That "if" is load‑bearing.** The embedding spike used a Swift **stub** view and dispatched events by **invoking overridden selectors directly**, *not* through a live windowserver responder chain (`spike-reuse-swiftterm` "whatFailed"). IME marked‑text and kitty‑keyboard correctness depend on the genuine responder chain, so **the reuse percentage is unproven until Phase‑0 drives real OS events through a live key window with the real SwiftTerm view.**
- **Interop cost (spike‑measured):** the NSView host mechanics are a solved ~150‑line problem. The real *ongoing* cost is (1) a hand‑written `@_cdecl` C‑ABI shim over SwiftTerm's Swift‑native API (feed/resize/font/scroll/copy/colors), (2) reverse‑FFI for its delegate callbacks (title/bell/OSC/sizeChanged/clipboard), and (3) a permanent **cargo + swiftc** two‑toolchain build. The Swift *runtime* is OS‑provided and ABI‑stable (`otool` confirmed `/usr/lib/swift/libswiftCore.dylib`), so there is **no runtime‑shipping concern.**
- **The two unproven seams:** GPUI draws its *whole* content view itself, so (a) a foreign terminal NSView must sit at the right z‑order with GPUI rendering transparent over it, and (b) the responder chain/IME/focus must route correctly through a live key window — *plus* the cross‑window `setUseMetal` off→on rebind on tear‑off. These are the Phase‑0 gate (§10).

**Path B — NATIVE (fallback): `alacritty_terminal` + a GPUI‑native `TerminalView`.** Zero reuse of the Metal renderer, but because **GPUI owns GPU glyph rasterization** you build a `TerminalView` on GPUI's text pipeline (as Zed/Paneflow do), *not* a Metal engine from scratch. Must add kitty graphics + sixel (alacritty lacks both) and re‑derive sub‑line smooth scroll / selection‑across‑eviction / color tuning. **Upside:** zero Swift, single toolchain, no FFI shim, no z‑order seam.

**objc2‑hybrid — NATIVE‑CHROME fallback (§4):** keep the SwiftTerm renderer, **no z‑order seam** (objc2 owns the view tree), but hand‑write the chrome imperatively. The right fallback when Phase‑0 fails *on the seam specifically* rather than on FPS.

### AppKit‑deep chrome subsystems vs GPUI's event/window model (unverified — fold into Phase‑0 + D9 cost)

Several must‑support items are **chrome, not terminal features**, and depend on a single process‑wide NSEvent monitor and live NSWindow manipulation. None is yet verified against GPUI's actual API. Each is GPUI‑native, objc2‑bespoke, or unproven:

| Subsystem | Likely status on GPUI | Note |
|---|---|---|
| Process‑wide local NSEvent keyboard monitor with `swallow(nil)`/`passthrough(event)` + **layout‑independent virtual keyCode** matching (rebindable shortcuts) | **Unproven → likely objc2** | GPUI has a keymap/action system, but a single arbitrating monitor that swallows‑or‑passes each event and matches *physical* keyCodes (Dvorak‑safe) is an AppKit `addLocalMonitorForEvents` idiom; may need objc2 against the underlying NSApplication or a re‑expression in GPUI's keybinding layer. |
| `ChromeEventRouter`: per‑event ancestor hit‑test classifying every `leftMouseDown` into pill/strip/passthrough; drag‑empty‑chrome‑to‑move via `performDrag`; double‑click zoom/minimize; `isMovable` KVO defense | **Partly GPUI‑native, partly objc2** | Zed ships a custom draggable title bar, so window‑move‑on‑drag has a GPUI path; per‑event hit‑test classification, double‑click zoom synthesis, and `isMovable` KVO are bespoke. |
| `CloseConfirmation` via `NSWindowDelegate` proxy | **Likely GPUI‑native** | Zed prompts before closing dirty windows, so a should‑close hook almost certainly exists. |
| File‑browser sidebar mode | **GPUI‑native** | Ordinary GPUI components. |

**Two consequences:** (1) every subsystem in the **objc2‑bespoke** column *erodes the "off AppKit" benefit* — the move relocates some AppKit behind objc2 rather than escaping it — and adds to the D9 cost. (2) The process‑wide‑monitor question **overlaps** the GPUI‑vs‑terminal focus‑arbitration gate; verify them together in Phase‑0.

### Stack‑agnostic, ports cleanly any path
The deep Claude integration — `claude` shell shadow + ZDOTDIR injection + per‑window control socket, `--session-id`/resume prefill, `ClaudeThemeSync` (~80‑token map + WCAG remap), SessionStart hook merge, worktree‑cwd transcript healing — is shell/socket/file/JSON logic re‑implementable in Rust (`nix`/`serde`/sockets). Several SwiftUI‑only workarounds **disappear** (`LivePaneRegistry` tri‑state claim, `WindowClaimLedger` token seeds, nil‑value `WindowGroup` de‑dup defeats).

### Phased plan
1. **Phase 0 — Gate (§10).** Resolve Path A vs Path B vs objc2‑hybrid with one measured experiment before committing.
2. **Phase 1 — Chrome skeleton.** GPUI window with built‑in vibrancy + repositioned native traffic lights (spike‑proven), the single‑visible‑pane pill model, settings, Project→Tab→Pane data model in plain Rust. Highest‑volume, highest‑AI‑velocity surface. **Also stand up the AppKit‑deep chrome subsystems early** (process‑wide monitor, ChromeEventRouter) since several may need objc2.
3. **Phase 2 — Terminal integration** per the Phase‑0 verdict. Wire IME/mouse/kitty‑keyboard routing through a live responder chain.
4. **Phase 3 — Windowing & flagship tear‑off.** Direct `cx.open_window` multi‑window + per‑window isolation; bespoke `NSDraggingSource` "released‑over‑desktop" detection via objc2 (no Zed precedent — `#6722` open); live‑pty handoff as in‑process Rust.
5. **Phase 4 — Claude integration** + the release/notarize/Homebrew pipeline.

---

## 9. Risks & open questions (incl. skeptic‑flagged items)

- **The rewrite's residual cost/risk vs the (now‑weaker) baseline (§2).** Framework AI‑velocity is a wash (D2 2→2); the win is language + **escaping SwiftUI↔AppKit impedance**. **Correction:** the "cheaper in‑place AppKit refactor" is largely *not available* — the native title bar is spiked‑out (`spike/native-titlebar`) and the in‑place chrome re‑architecture already shipped, so chrome assistability can't be lifted in place. The real open question is narrower: is the language‑axis + impedance‑escape win worth the multi‑month rewrite + Phase‑0 risk — not "rewrite vs a cheap refactor."
- **Path‑A efficiency is unmeasured — provisional.** The recommended architecture is a **dual‑Metal‑stack hybrid** (GPUI blade‑graphics + SwiftTerm Metal) plus an objc2 z‑order seam — the same "second GPU stack" objection used against the Flutter‑reuse candidate. No FPS/latency/memory exists for it. **Must be measured against the current Nice baseline (§10) before commitment.**
- **GPUI z‑order/transparency/responder seam — the load‑bearing unknown.** GPUI hosting a *foreign* terminal NSView over its own Metal content view, at FPS, with clean live‑responder event routing and tear‑off Metal rebind, is **not proven** (the embed spike used a stub + direct selector calls). This single risk decides Path A vs B vs objc2‑hybrid.
- **Reuse % is conditional, not ~100 %.** The renderer+input‑stack reuse holds *only if* the live‑responder/IME PoC passes (§8).
- **objc2‑bespoke chrome surfaces erode the "off AppKit" benefit.** Some borderless‑window event synthesis (process‑wide swallow/passthrough monitor, layout‑independent keyCode, per‑event hit‑test, `isMovable` KVO) likely lands behind objc2, adding to D9 (§8 inventory).
- **GPUI framework corpus (D2 = 2) is the honest weak point.** The AI win is language‑driven; the readable Zed source helps a developer at dev time but does **not** lift Claude's in‑weights priors and can conflict with current churning API (§3).
- **GPUI vs objc2 is a near‑tie on the primary driver (§4).** GPUI's edge is declarative DX + safe Rust, *not* framework corpus; it uniquely carries the z‑order seam objc2 avoids.
- **GPUI is pre‑1.0, single‑vendor (Zed), churning.** `0.2.2` is ~8 months stale with explicit "expect breaking changes"; the tree already moved `crates/gpui → crates/gpui_macos`. Named, accepted risk.
- **Skeptic corrections that landed in GPUI's favor:** the desk research wrongly claimed GPUI uses a private CGS blur API and that vibrancy (`#7955`) is unreachable/open — the spike shows modern GPUI uses `NSVisualEffectView` and ships vibrancy built‑in. Styling ceiling is *higher* than the eval portrayed.
- **Terminal backend gaps if Path B:** `alacritty_terminal` lacks kitty graphics/sixel; both must be built. `libghostty` is *not* a rescue (zig‑gated, parser‑only). Spike‑confirmed.
- **Flagship tear‑off has no GPUI precedent** (`#6722` open) — fully bespoke objc2 `NSDraggingSource` work with real correctness/leak risk.
- **Efficiency numbers are directional, not load‑bearing.** The "222 MB vs 3.5 GB" figure is a weak source; the *magnitude* (native, GC‑free, crushes Electron) is defensible — but it does **not** substitute for measuring the Path‑A hybrid.
- **Linux degrades** (bonus only): no wallpaper‑vibrancy equivalent; GPUI Wayland blur is a won't‑fix. Acceptable.
- **Claude integration is version‑coupled** to a CLI that changes silently — must be re‑derived and re‑validated regardless of stack.

---

## 10. Recommended next PoC (single highest‑value spike)

**Build a GPUI window that hosts the *real* SwiftTerm Metal NSView (not the stub) as a subview, drive it through a *live key window*, and *measure* — do not assume — the numbers that decide the architecture.**

### ✅ BUILT & MEASURED (2026-06-26) — runnable PoC at `spikes/phase0-poc/`

This PoC was built (a GPUI window hosting the **real** SwiftTerm Metal `NSView` via objc2, plus an FPS/latency/memory harness) and run on the actual machine (debug **and** release). Results:

- **Proofs (4,6,7) PASS on the real renderer; (5) is intermittent.** Keyboard/IME routing, transparent-over-terminal compositing (also visually confirmed), and same-window Metal rebind all PASS. **(5) the mouse hit-test seam** — the Path-A-vs-objc2-hybrid decider — routes correctly at the hit-test layer (a `class_addMethod` `hitTest:` override on GPUI's `GPUIView` sends terminal-region points to the terminal, chrome to GPUI), but its end-to-end corroboration (a synthetic drag producing a real terminal selection) is **run-to-run intermittent** — PASS on some runs, UNPROVEN on others. The routing is solid; the deterministic end-to-end selection still needs closing (see §10 re-measure, "still open").
- **Memory: native, no unbounded growth.** Load-peaks are flat across all modes (~138–151 MiB; Electron-class terminals run 300 MB–1 GB+) and idle is flat across the three *presenting* modes (~48–52 MiB); the non-presenting `none` control idles higher (~71 MiB). **The memory story is strong.**
- **Frame rate: the original 30 fps was *not* a trivially-fixable pacing artifact.** The first run paced the terminal present once per GPUI frame and read **~33 ms (~30 fps)** on both stacks. The handoff hypothesized this was naive pacing that a decoupled `CVDisplayLink` would lift to refresh. **A follow-up decoupling pass (2026-06-27) measured it directly: the terminal *can* hit refresh, but decoupling *starves the chrome* — see the verified re-measure below.**

### ✅ RE-MEASURED & VERIFIED — present-loop decoupling (2026-06-27)

> **Methodology / provenance.** An earlier cut of this sweep was contaminated by a mid-run external-monitor hot-plug with the display unrecorded. The harness now captures the window's `NSScreen` + `maximumFramesPerSecond` and flags a mid-run change, and the numbers below are from a **re-run on a single stable display — "Built-in Retina Display [1470×956], max 60 Hz", identical across all runs, zero mid-run-change flags.** The clean numbers reproduce the contaminated ones to <0.2 %, so the hot-plug did not change the verdict. A 4-agent adversarial verification re-derived every cell from the raw per-sample CSVs (sample counts + p50/p95/p99 match the harness output under nearest-rank) and confirmed the GPUI starvation is a **real compositor stall, not a counting artifact**: two independent counters agree (~33 GPUI renders vs ~1042 terminal presents over 18 s), and `stamp_gpui_frame` is the unconditional first line of `render()`, which re-arms `request_animation_frame` every frame.

Added a real **`CADisplayLink`** present driver (`st_start_present_link`, `NSView.displayLink`) and an A/B/C/D toggle (`NICE_POC_PRESENT=link|sync|async|none`). Runs: real bridge, 18 s continuous max-rate streaming, **60 Hz** panel (one vsync = 16.67 ms), display recorded per run. In the coupled modes **`draw-attempt` p50 ≈ 0.02 ms — per-frame Metal encode is genuinely ~free** (in `link` mode `draw-attempt` is itself vsync-bound at ~16.6 ms *by design* — the synchronous `mtk.draw()` consuming the per-vsync commit budget *is* the mechanism). The limiter is **scheduling/compositing, not compute**, and is display-independent.

| Present scheme (60 Hz panel) | Term present p50/p95 (ms) | GPUI composite p50/p95 (ms) | What it shows |
|---|---|---|---|
| **`sync`** — `present_now()` inline per GPUI frame (original) | 33.38 / 47.18 (~30 fps) | 33.39 / 49.25 (~30 fps) | both stacks at **half** refresh |
| **`async`** — coalesced `DispatchQueue.main.async` (**fork's production path**) + GPUI RAF | 33.34 / 44.77 (~30 fps) | 33.35 / 49.19 (~30 fps) | **same** — deferring the present does not escape it |
| **`link`** — terminal on its own `CADisplayLink`, decoupled | **16.68 / 17.13 (~60 fps)** ✅ | **700.08 / 715.33 (~1.4 fps)** ❌ | terminal reaches refresh, but its main-thread present **starves GPUI** (reproduced 3×, incl. clean single-display) |
| **`none`** — terminal never presents (GPUI-alone control) | — | **16.70 / 19.18 (~60 fps)** | GPUI's compositor **is** refresh-capable |

Latency seam p50: `sync` 16.8 ms (~1 frame), `link` 27.8 ms, `async` 33.1 ms (the rise tracks *where* the present that closes the loop happens). Memory: **native across all modes** (every peak ≤ 150.7 MiB vs Electron's 300 MB–1 GB+), **no unbounded growth**; idle is flat across the three *presenting* modes (~48–52 MiB) — the non-presenting `none` control idles higher (~71 MiB) and is excluded.

**Diagnosis (verified).** Each Metal stack *alone* reaches the panel's 60 Hz refresh (terminal in `link`, GPUI in `none`), so neither renderer is individually the bottleneck. But **none of the three present schemes tested drives both at refresh simultaneously** — `sync`/`async` split the budget 30/30, `link` gives the terminal 60 by starving GPUI to ~1.4. The cause is structural, not compute: **two `CAMetalLayer`s compositing into one `NSWindow`, presented from one main thread, contend for a shared ~one-commit-per-vsync budget** — the renderer's present (`view.currentDrawable` → vsync-gated; `frameSemaphore`=3 non-blocking; `present(drawable)` async) blocks the main thread ~1 vsync, and the schemes only redistribute that single budget. The handoff's "two vsync-locked presents per cycle" hypothesis was *directionally* right; the conclusion that decoupling trivially fixes it was *wrong* — decoupling moves the starvation, it doesn't remove it.

**Scope — proven vs still open.**
- **Proven (load-bearing, CSV-verified):** on a **60 Hz** panel, the dual-Metal-layer hybrid (Path A *and* objc2-hybrid — both put two layers in one window) cannot, under **any of the three naive present schemes tested**, hold both the terminal *and* animated chrome at refresh under continuous load; best symmetric result is 30/30. Memory is native; the limiter is compositing/scheduling, not compute.
- **Deliberately NOT claimed:** the *universal* "no scheme can drive both." A **co-paced single-clock present** (one vsync source committing **both** layers, or a single shared `CAMetalLayer`/drawable) was **not built or measured** — a future co-paced PoC could overturn the 30/30. The 60/1.4-vs-30/30 spread shows the outcome is highly scheduling-sensitive, so headroom plausibly exists.
- **Still open:** (a) **120 Hz ProMotion is untested** — this machine's built-in panel is hard 60 Hz, so all budget math is 60 Hz-specific; an 8.3 ms vsync could relax *or* worsen the contention (needs real 120 Hz hardware). (b) **Proof 5** — the load-bearing mouse hit-test *routing* seam — passes at the hit-test layer but its end-to-end synthetic-drag selection is **intermittent** (UNPROVEN on the clean `link` run, PASS on `sync`/`async`); must be closed deterministically. (c) the Nice Dev baseline columns.

**Point for Path B.** Path B (`alacritty_terminal` + a GPUI-native `TerminalView`) renders the terminal into GPUI's **single** Metal layer — one compositor, one drawable, **no shared-commit contention** — so it structurally sidesteps the exact ceiling this sweep demonstrated.

**Net (calibrated stance — corrected).** Against the spike's own decision rubric (FPS *or* memory FAIL of the dual stack → Path B), the clean sweep is **mixed, not a clean Path A**: memory PASS, latency PASS, but the dual-stack present shows a **real tax** and Proof 5 is unproven. The result **neither validates nor refutes Path A — it isolates the actual risk.** **Do not lock Path A on this** (the earlier commit "record measured Path A result" was over-confident). Treat Path A as *"not refuted; the simultaneous-60 goal is unmet under naive presents,"* and **gate the A-vs-B decision on three concrete experiments**: (a) a **co-paced single-clock present** PoC (one vsync committing both layers, or a single shared layer) to learn whether 30/30 is fundamental or a double-present artifact; (b) **close Proof 5** end-to-end on a real display; (c) a **120 Hz ProMotion** re-measure. If the co-paced present still can't drive both at refresh, that is genuine irreducible dual-stack tax and **Path B becomes favored**. Re-run any scheme: `cd spikes/phase0-poc && NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=link cargo run` (see its README). The original methodology/decision-tree below stands.

### ✅ FOLLOW-UP — transactional co-paced present (`txn`) breaks the 30/30 ceiling (2026-06-27)

> **This resolves gating experiment (a) above.** The earlier `copace` lever (toggling the terminal layer's `displaySyncEnabled=false`) **FAILED** — `present_now()` still blocked a full vsync (16.4 ms), because the layer flag does not remove the window-composite-gated `nextDrawable` wait. The proper co-paced present is the textbook **`presentsWithTransaction`** fix, which required a **fork patch** (authorized; on branch `phase0-txn-present` in `/Users/nick/Projects/SwiftTerm`, **OFF by default**, so prod Nice's pinned revision is unaffected). The renderer's present path becomes `commit() → waitUntilScheduled() → drawable.present()` *inside the current CoreAnimation transaction* (`CAMetalLayer.presentsWithTransaction = true`), opted in via the new `MacTerminalView.setMetalPresentsWithTransaction(_:)` and the bridge's `st_set_presents_with_transaction`. PoC mode `NICE_POC_PRESENT=txn`.

Re-measured on the **same clean 60 Hz** panel (Built-in Retina, max 60 Hz), same fresh build, with `sync` re-run as an in-session A/B control. `txn` reproduced across 3 runs (≤0.5 % spread):

| Present scheme (60 Hz panel) | Term present p50/p95 (ms) | GPUI composite p50/p95 (ms) | `present_now()` wall p50 (ms) | Latency seam p50 (ms) | What it shows |
|---|---|---|---|---|---|
| **`sync`** (control, re-run) | 33.30 / 42.59 (~30 fps) | 33.40 / 42.87 (~30 fps) | **16.33** (full vsync block) | 16.73 | reproduces the 30/30 stall |
| **`txn`** (transactional present) | **18.35 / 31.18 (~54 fps)** | **17.94 / 31.42 (~56 fps)** | **3.56** (block GONE) | **3.94** | **both stacks ≈ refresh; co-commit removes the block** |

**What changed.** The main-thread present block collapses **16.3 ms → 3.6 ms** (the `waitUntilScheduled()` is a scheduling wait, not a vsync/display wait; `drawable.present()` defers to the CA transaction instead of issuing an independent vsync-gated surface flush). Throughput rose **+63 %** (529 → ~862 presents / 18 s). GPUI, previously pinned to 30 by the terminal's blocking present, runs near its standalone refresh again (~56 fps, vs ~60 in the `none` control). Keystroke latency more than quartered (16.7 → 3.9 ms). Memory stayed native (peak ~155 MiB).

**Verdict — the 30/30 ceiling is a double-present *artifact*, not an irreducible dual-stack tax.** This **refutes** the reading that tilted toward Path B. Two `CAMetalLayer`s in one `NSWindow` *can* both run near refresh under continuous load — they just must **co-commit in one CA transaction** rather than each issuing an independent async present. **Path A's dual-Metal-stack present is viable.**

**Honest caveats (do not over-claim a locked 60).**
- **~54–56 fps median, not a locked 60.** `p50 18.3 ms`, `p95 ≈ 31 ms`, mean interval ~20.9 ms (≈48 fps) — so roughly a **quarter** of frames drop toward ~33 ms while the rest sit near refresh. (The harness's raw "cliff" count is `>16.6 ms`, calibrated for a 120 Hz frame, so on this 60 Hz panel it over-counts a ~18 ms median as ~680 "cliffs" — read p95/mean, not that count.) This is near-refresh with a periodic dropped-frame cadence, *not* the clean 16.7 ms the terminal-alone (`link`) or GPUI-alone (`none`) controls hit. Most likely the residual is GPUI's **own** present cost stacking on the terminal's `waitUntilScheduled()` within each run-loop turn; closing the last gap to a locked 60/60 would likely need GPUI **also** presenting transactionally (a GPUI-side change this PoC deliberately did not make). Near-refresh is almost certainly fine for the product; a locked 60 is a tuning question, no longer a feasibility one.
- **60 Hz only.** 120 Hz ProMotion still untested (this machine's built-in is hard 60 Hz); 8.3 ms vsync budgets are unmeasured.
- **Requires the fork patch in prod.** It is opt-in and OFF by default (prod behaviour unchanged at the current pin), but *shipping* Path A means enabling `presentsWithTransaction` in the real app and validating it against the standalone terminal (no tearing / latency regression under heavy scroll) — the fork is shared and pinned by revision in `project.yml`.
- Proof 5 PASSED on both the `sync` and `txn` runs here (synthetic drag → real selection), but it remains run-to-run intermittent across the broader sweep — still flagged open below.

**Updated stance.** Gating experiment **(a) is resolved in Path A's favor**: the simultaneous-near-60 goal *is* reachable with a one-line-of-mechanism present change, so the dual-stack present is **no longer a Path-B-favoring risk**. The remaining open items are (b) close Proof 5 deterministically, (c) the 120 Hz re-measure, and the Nice Dev baseline columns — none of which currently point away from Path A. Re-run: `cd spikes/phase0-poc && NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 NICE_POC_PRESENT=txn NICE_POC_SECS=18 ./target/debug/phase0-poc` (needs a display; build once with `NICE_POC_REAL_BRIDGE=1 cargo build`).

### ✅ FOLLOW-UP — Proof 5 closed + baseline columns measured (2026-06-27)

**Proof 5 (mouse hit-test seam) is now deterministic — resolves open item (b).** The hit-test *routing* was always deterministic; only the end-to-end corroboration flaked, because `run_mouse_seam` read the buffer *text* under a fixed-geometry synthetic drag and rejected whitespace — and whether those cells held glyphs vs blanks depended on incidental run-to-run frame pacing (event delivery + selection commit are fully synchronous, never a race). Fixed two ways, both content-independent: (1) feed a deterministic full-screen fill before the drag so any region selects printable glyphs; (2) add `st_selection_has_range` (backed by SwiftTerm's public `TerminalView.selectionActive`) and assert PASS = `routing_ok && (text non-empty || selection range non-empty)`. **Confirmed 6/6 deterministic PASS** on the clean 60 Hz display (was run-to-run intermittent). So the load-bearing Path-A-vs-objc2-hybrid routing seam is now a clean, repeatable PASS.

**Baseline (Nice Dev) columns — measured; resolves the baseline open item (with provenance).** Baseline = the current Swift Nice (single Metal terminal layer + AppKit/SwiftUI chrome), same fixture, same 60 Hz panel.

| Metric | Baseline (current Nice) | PoC dual-stack (`sync` / `txn`) | §10 gate | Verdict |
|---|---|---|---|---|
| Term present p50/p95 (ms) | **~16.7 / 17.1** (~60 fps)† | sync 33.3/42.6 · **txn 18.3/31.2** | PoC p95 ≤ baseline p95 ×1.15 (≈19.7) | txn **p50 within** (18.3≤19.2), **p95 over** (31>19.7); sync fails |
| phys_footprint idle (MiB) | **~69**† | sync 44.5 · txn 71.4 | ≤ baseline ×1.2 (≈83) | **PASS** (both) |
| phys_footprint under-load steady/peak (MiB) | **~111 / 114**† | sync 145.6/147.8 · txn 151.8/155.5 | ≤ baseline ×1.2 (≈133) | **over** (~1.3–1.4×) |

† **Memory** = `phys_footprint` of Nice Dev 0.29.0 under the fixture, Metal active, 1 pane (measured live). **Term FPS** is *not* a fresh direct Nice measurement: the installed 0.29.0 predates the `SWIFTTERM_PROFILE` `Metal.Draw` signpost, and a fresh signpost-emitting Nice Dev build is currently **blocked by a missing Xcode Metal Toolchain component** (`xcodebuild -downloadComponent MetalToolchain` — a user-environment change, not done autonomously). Instead the baseline term FPS is taken from **this PoC's own single-stack controls on the identical SwiftTerm Metal renderer / display / workload** — `link`-mode terminal-alone 16.68/17.13 and `none`-mode GPUI-alone 16.70/19.18 — which *is* the single-Metal-layer-at-refresh rate the baseline equals by construction (Nice's SwiftUI chrome is not a second Metal layer, so it adds no shared-commit contention). Keystroke-latency pty-echo baseline deferred (needs Accessibility TCC for Nice Dev).

**Baseline-anchored verdict (tempers, but does not reverse, the txn result).** `txn` removes the catastrophic 30/30 FPS *stall* (the big win), and its **median** term FPS reaches the single-stack baseline (18.3 vs 16.7 ms, within the ×1.15 gate). But against the real baseline the dual stack still shows two **quantified, non-fatal costs**: (1) **tail FPS** — txn p95 31 ms misses the gate (periodic frame-pairs still collapse to ~30; the locked-60 gap), and (2) **memory** — under-load ~1.3–1.4× the baseline (the second Metal stack + the GPUI framework), just over the ×1.2 gate, though both remain **native** (≤155 MiB vs Electron 300 MB–1 GB+). So Path A is **viable but not free**: reusing the renderer costs measurable tail-FPS jitter and ~⅓ more memory than today's Nice. **Path B** (single Metal layer) structurally avoids both — it would inherit the baseline's FPS and ~baseline memory. The A-vs-B call is therefore now a **product tradeoff** (reuse the proven renderer at a bounded, measured overhead vs a clean single stack with a from-scratch terminal view), **no longer a feasibility question** — the original "is the dual stack even efficient enough" risk is retired into known numbers.

**§10 remaining open:** (c) a **120 Hz ProMotion** re-measure (this machine is hard 60 Hz) — the only un-retired efficiency unknown; plus two low-value/blocked extras: a **direct** Nice term-FPS measurement (needs the Xcode Metal Toolchain component) and the keystroke-latency pty-echo baseline (needs Accessibility TCC). None currently points away from Path A on feasibility.

**Measure (against the current Nice baseline on this same machine):**
1. **Sustained burst FPS** under a synthetic Claude‑streaming workload — does the embedded Metal view hold smooth GPU scroll while GPUI composites chrome over/around it? Compare to baseline.
2. **Keystroke latency** end‑to‑end through the GPUI↔AppKit seam.
3. **Idle and under‑load memory** of the **dual‑Metal‑stack** process vs the baseline. *This is the number that converts the provisional Path‑A efficiency PASS (§5 `5†`) into a real one — or fails it.*

**Prove (through real OS routing, not direct selector invocation — the stub spike did not):**
4. **Input routing through a live responder chain** — keyDown/keyUp/flagsChanged, `NSTextInputClient`/IME marked text, VT mouse selection, and first‑responder arbitration between GPUI focus and the terminal view, dispatched via `NSApp.sendEvent` through a genuine key window. IME and kitty‑keyboard correctness depend on this.
5. **Transparent GPUI region over the terminal** — GPUI renders non‑opaque exactly where the terminal NSView shows through, no z‑order/blanking artifacts (the content‑view‑ownership seam).
6. **Cross‑window Metal‑layer rebind on tear‑off** — move the live terminal NSView to a second `cx.open_window` and toggle `setUseMetal` off→on.
7. **One AppKit‑deep chrome probe** — confirm whether a process‑wide swallow/passthrough NSEvent monitor with layout‑independent keyCode matching coexists with GPUI's focus model, or requires objc2 (§8). This overlaps (4).

**Foundation already in place:** `spike-reuse-swiftterm/swift-embed/` (NSView‑under‑Rust/objc2 mechanics + C‑ABI shim); `spike-gpui-glass/glassdemo` (GPUI standalone + built‑in vibrancy + native traffic lights). This PoC stitches them and adds the *real view + live routing + measurement* the earlier spikes deliberately skipped.

**Decision tree:**
- **PASS (FPS + memory + live‑input all clear):** commit to the **hybrid (Path A)** — preserve the entire proven renderer, accept the bounded Swift‑FFI drag, chrome on GPUI/Rust. Recommended outcome.
- **FAIL on the z‑order/responder *seam* (but AppKit embedding otherwise fine):** fall back to **objc2‑hybrid (§4)** — keep the renderer, no seam, hand‑written imperative chrome.
- **FAIL on FPS/memory of the dual stack:** fall back to **Path B** — `alacritty_terminal` + a GPUI‑native `TerminalView` (single stack, zero Swift), then run a secondary spike proving sub‑line smooth scroll + selection‑across‑eviction at burst FPS before committing.
- **FAIL broadly / Rust‑velocity not worth it:** revert to the **in‑place AppKit refactor baseline (§2)** — the cheapest, lowest‑risk path, and the default the whole rewrite must beat.

---

## 11. Re‑prioritization under quality‑max / no‑reuse‑credit criteria (2026‑06‑27)

> **What this section is — and is not.** §1–§10 rank the options under the *original* criteria (AI velocity primary, with reuse and bounded migration cost as real assets), and under those criteria the recommendation is **Path A** (reuse SwiftTerm). This section re‑ranks **the same options under deliberately changed criteria** Nick set this session. It does **not** retract §1; it answers a *different* question. **Under the re‑weighted criteria the recommendation flips to Path B.** Which section governs depends on which criteria you accept.

**The re‑weighted criteria (Nick's, this session):**
1. **Maximize end‑product QUALITY** — rendering fidelity, native‑macOS feel (vibrancy/glass, traffic lights, IME, window/tab/pane/tear‑off), performance ceiling (sustained at‑refresh FPS, low latency), polish — **regardless of implementation time.**
2. **Reuse earns ZERO credit.** Reuse of the current Nice or the SwiftTerm Metal fork is not an asset, and is a *negative* wherever it imposes an irreducible architectural tax. Rebuilding everything — including a from‑scratch terminal renderer — is fully acceptable.
3. **Claude‑Code tractability remains a HARD GATE.** The stack must be one Claude can actually build and maintain (language corpus, framework corpus, tooling, debuggability).

**Method.** A 4‑agent workflow: three *independent* re‑ranking lenses — **quality‑ceiling**, **Claude‑velocity (re‑weighted)**, and **architecture‑risk** — plus one **adversary** that stress‑tested the converged front‑runner. All three lenses independently put **Path B at 9/10, ranked #1**; the adversary then argued the strongest case against it (and it survived, narrowly and conditionally).

### Re‑ranking (three lenses + mean)

| Option | Quality‑ceiling | Claude‑velocity (re‑wt) | Architecture‑risk | **Mean** | **Rank** |
|---|:--:|:--:|:--:|:--:|:--:|
| **Path B — all‑Rust, single Metal stack** (GPUI chrome + from‑scratch GPUI‑native terminal) | 9 | 9 | 9 | **9.0** | **1** |
| **objc2‑hybrid** (imperative AppKit chrome via objc2, reuse SwiftTerm) | 7.5 | 4 | 6 | **5.8** | **2** |
| **Bespoke wgpu/Metal** (custom renderer for chrome *and* terminal) | 6 | 6 | 3.5 | **5.2** | **3** |
| **Path A — GPUI + reuse SwiftTerm via objc2** (dual‑Metal hybrid) | 6.5 | 4 | 4 | **4.8** | **4** |
| **Flutter + Rust‑FFI core** | 4 | 5 | 5 | **4.7** | **5** |
| **In‑place AppKit/SwiftUI refactor** (stay on Swift, null hypothesis) | 4.5 | 3 | 5.5 | **4.3** | **6** |

**The inversion.** Removing reuse credit is the whole story. Path A's *entire* original case was "keep the proven crown‑jewel renderer at bounded cost." Zero that out and what remains is exactly the set of liabilities the new criteria name as quality‑capping — and Path A carries **all three at once**: the **measured dual‑Metal compositing tax** (even with the `txn` fix: ~54–56 fps median but **p95 ~31 ms** with periodic drops to 30, **~1.3–1.4× memory** — §10), the **objc2 z‑order/responder seam** (two sources of truth for input: GPUI focus vs the terminal's `NSTextInputClient`), and a permanent **Rust+Swift two‑toolchain FFI**. So the *old* #1 falls to #4. The **objc2‑hybrid** rises to the sleeper #2 because it is *effectively single‑Metal‑stack* (AppKit chrome is CoreAnimation, not a second `CAMetalLayer`) and owns the whole NSView tree (no z‑order seam) — its only caps are imperative/unsafe objc2 (Claude's weakest Rust surface) and the two‑toolchain. **Bespoke wgpu** has the highest *theoretical* ceiling but the lowest *achievable* one under the Claude hard gate (it reinvents IME/accessibility/vibrancy/window‑mgmt with no framework corpus). **Flutter** and **in‑place Swift** both fail the *primary* criterion they most need to win (canvas‑capped native fidelity; SwiftUI‑impedance‑capped chrome ceiling).

### Why Path B wins (under these criteria)

A **single `CAMetalLayer`** structurally eliminates the only *measured*, irreducible quality defect in the whole field — the dual‑stack compositing tax — because the terminal renders into GPUI's own compositor: one drawable, one commit, no shared‑commit contention, so it inherits the §10 single‑stack controls' locked‑refresh behaviour (`none`/`link` each hit ~60) and ~baseline memory, rather than Path A's permanent ~55 median / p95‑31 ms tail. It carries **none** of the three named liabilities — no second Metal stack, no objc2 z‑order/responder seam, no Swift toolchain, no FFI boundary — and keeps GPUI's spike‑proven native fidelity (real `NSVisualEffectView` vibrancy, repositioned real traffic lights, reachable `NSGlassEffectView`). On the hard Claude gate it is the cleanest whole‑project story: **100% safe Rust, one cargo toolchain, a `rustc`/borrow‑checker loop everywhere, and a declarative component tree** for the high‑volume chrome iteration where SwiftUI‑fighting simply disappears. GPUI's thin framework corpus (D2 = 2) is the one weakness, but it merely *equals* SwiftUI's floor — the Claude win here is the **language axis** plus a single clean stack, not framework familiarity.

> **Theoretical vs achievable ceiling — the distinction all three lenses drew.** The highest *theoretical* ceiling is bespoke wgpu (one unified pipeline, total control). But the highest *achievable* ceiling — the one that matters, because Claude must actually build it and native feel must be *realized*, not merely possible — is Path B: a no‑tax single stack **plus** GPUI's proven native vibrancy/traffic‑lights/glass **plus** the Zed/ZTerm‑demonstrated GPUI‑native terminal architecture, whereas bespoke would ship the IME/accessibility/vibrancy surface visibly sub‑native.

### The one live risk — the adversary's strong counter‑case

Path B is the **only** option that voluntarily discards a terminal renderer already *at* the quality ceiling and bets a from‑scratch one can climb back to it — on the worst terrain for the Claude hard gate. Three load‑bearing points:

1. **Ceiling *downgrade* risk on the surface users stare at all day.** `alacritty_terminal` ships **no renderer** and lacks **both** kitty graphics and sixel. GPUI's glyph pipeline is *editor‑grade*, not terminal‑optimized. And the flagship existence proof — Zed's own terminal — is demonstrably **below** Nice/SwiftTerm fidelity (no sub‑line smooth scroll, no sixel, no kitty graphics). "Optimize the ceiling" cuts *against* starting below it and hoping to climb.
2. **Hidden GPUI‑fork trap.** If terminal‑grade rendering (image cells compositing with text + damage + scrollback eviction, ligatures across cell boundaries, COLR/sbix color‑emoji at cell density, sub‑pixel smooth scroll) is **not expressible in GPUI's element/text model without forking GPUI's GPU text/atlas core**, then Path B's headline advantage ("no fork, single toolchain, clean") *inverts* into maintaining a fork of a pre‑1.0, churning, single‑vendor framework's **hardest** subsystem — strictly worse than a self‑contained Metal renderer.
3. **It maximizes the thin‑corpus Claude risk exactly where it's most dangerous.** A bespoke GPU terminal renderer + IME marked‑text + kitty keyboard + selection‑across‑eviction is the single hardest, lowest‑corpus task in the project, on a D2 = 2 framework. Path A/objc2 keep that code a proven black box Claude never touches; Path B forces Claude into the highest‑difficulty / lowest‑support quadrant on the most quality‑critical surface.

And the reason the lenses flipped — the dual‑stack tax — has been **measured down to non‑fatal** (§10 `txn`): Path A's terminal median sits at the single‑stack baseline with only tail jitter + ~⅓ memory remaining. So Path B trades a **small, measured, bounded** tail‑jitter risk for a **large, unretired** renderer‑fidelity‑regression risk.

**Residual risks of the front‑runner (carry into the de‑risking spikes):**
- Renderer‑fidelity regression vs SwiftTerm: sub‑line smooth scroll, sixel/kitty graphics, COLR/sbix emoji, cross‑cell ligatures, selection‑across‑eviction at burst FPS.
- The GPUI‑core‑fork trap (the decision‑flipping unknown).
- Claude velocity on the worst quadrant (bespoke GPU renderer + VT integration on a churning, ~8‑month‑stale framework API).
- IME marked‑text + kitty keyboard rebuilt from scratch (SwiftTerm baked them into `NSTextInputClient`).
- Tear‑off / `NSDraggingSource` "released‑over‑desktop" has no GPUI precedent (Zed #6722) — bespoke objc2 windowing regardless of path, so Path B's "zero objc2" claim is not fully true for windowing.
- Sustained‑at‑refresh FPS of the *new* renderer under burst Claude‑streaming is unproven (only GPUI‑alone and SwiftTerm‑alone were each proven to hit 60 — not a from‑scratch GPUI terminal under load).

### Verdict (re‑weighted)

**Adopt Path B as the front‑runner, but treat it as PROVISIONAL** pending the renderer‑fidelity de‑risking spikes — the win is real but **narrow and conditional**, much closer than the three converging 9s imply. **Decision rule:**
- If the spikes show GPUI's pipeline can **match SwiftTerm fidelity *without* forking GPUI's text/atlas core** → **Path B** (highest‑ceiling, cleanest‑architecture pick).
- If they reveal a **fidelity gap or force a GPUI‑core fork** → fall back to **Path A** (accept the bounded, now‑measured dual‑stack tax to *guarantee* the proven renderer).

**And before locking either, run the possibly‑superior unexplored option:** port SwiftTerm's *proven Metal rendering technique* into GPUI's **single shared drawable/clock** (one Metal layer, GPUI chrome + a proven‑technique terminal renderer, **not** GPUI's editor text pipeline). If feasible, this dodges **both** the dual‑stack tax **and** the fidelity‑regression risk, and would **dominate both A and B** on the quality ceiling. **§12 reports the de‑risking research that tests this rule.**

---

## 12. De‑risking the Path B provisional + replacement‑library survey (2026‑06‑27)

> **What this is.** A 6‑lane research pass that resolves the §11 decision rule *as far as source can settle it*, by reading the **actual primary source** — GPUI 0.2.2 (the shipping crates.io cut) **and** current Zed `main` (`crates/gpui_macos`, `crates/terminal`, `crates/terminal_view`), `alacritty_terminal` 0.26.0, `cosmic-text`, plus the web ecosystem. Each lane was **adversarially verified** by an independent agent that re‑read the source to refute it. **All six key findings held (high confidence).** This pass settles every *feasibility/architecture* question; it explicitly does **not** settle the handful of questions that need a *built prototype + live‑GPU measurement* (flagged at the end).

### Headline: the load‑bearing question is RESOLVED — no GPUI‑core fork is required

**A GPUI‑native terminal can match SwiftTerm rendering fidelity WITHOUT forking GPUI's GPU text/atlas core.** This was the single unknown that decided Path B vs Path A (§11 decision rule), and it resolves **in Path B's favor** on verified source. The feared "hidden GPUI‑fork trap" — that image cells / color emoji / smooth scroll would force forking GPUI's hardest subsystem (`text_system`/`open_type`/`metal_atlas`) — **does not materialize.**

### De‑risking spikes — status against the §11 rule

| Spike | Status | Finding |
|---|---|---|
| **(b) kitty graphics + sixel image cells — fork?** *(the biggest unknown)* | ✅ **resolved — no fork** | Public `Window::paint_image(bounds, corner_radii, Arc<RenderImage>, frame_index, grayscale)` (window.rs:3129‑3175) keys an `AtlasKey::Image` into the **BGRA8 polychrome atlas** via `PlatformAtlas::get_or_insert_with` (arbitrary bytes) and emits a content‑mask‑clipped, corner‑radii `PolychromeSprite`; the polychrome shader applies clip + corner mask (shaders.metal:697‑719). Scene order + `content_mask` let image cells z‑interleave behind/between text and clip per‑cell. **No atlas/renderer fork.** |
| **(c) glyph fidelity — ligatures, COLR/sbix emoji, ZWJ — fork?** | ✅ **resolved — no fork** | Color emoji renders **in color** via public `paint_emoji → rasterize_glyph(is_emoji:true)` (CoreText `draw_glyphs` → polychrome atlas; window.rs:3010‑3060, text_system.rs:344‑431) — covers sbix **and** COLR. Ligatures/ZWJ/combining/wide glyphs via public `shape_line().paint()` (text_system.rs:433‑548). Current GPUI `main` even **adds** `SubpixelSprite` LCD‑subpixel AA — a fidelity *gain* over 0.2.2. |
| **(a) sub‑line smooth scroll — fork?** | ✅ **expressible** / ⏳ **FPS unmeasured** | All primitive bounds are `Bounds<ScaledPixels>` (f32); `paint_glyph` caches per‑`subpixel_variant` atlas keys (window.rs:2948‑2998), so fractional per‑frame re‑origin via a custom `Element`/`canvas()` (elements/canvas.rs:10‑19) is fully public — **no fork.** The *quality* half (sustained at‑refresh / burst FPS + present pacing on one GPUI Metal stack) is a clock/present matter source cannot settle → live spike. |
| **(d) IME marked‑text + kitty keyboard + selection** | ✅ **IME no fork** / ⚠️ **kitty‑kbd = input‑layer, not renderer** | GPUI registers the **full `NSTextInputClient`** protocol → public `InputHandler`/`EntityInputHandler`; Zed's terminal already renders inline underlined marked‑text and anchors the candidate window via `bounds_for_range` (window.rs:196,1660,2193‑2310) — **no fork.** Gap: GPUI's public `Keystroke` discards raw `NSEvent` keyCode/location, so **kitty‑keyboard CSI‑u** needs a small `mac/events.rs` patch *or* an objc2 `[NSApp currentEvent].keyCode` side‑channel — a **platform‑input** concern, categorically **not** the GPU text/atlas core, so it does not bear on A‑vs‑B. Selection‑across‑eviction lives in the alacritty `Selection` VT core — left **unknown**. |
| **(e) single‑drawable "port proven renderer into one GPUI drawable"** | ❌ **the "no‑fork" premise is FALSE** | GPUI's Metal renderer is **closed**: `MetalRenderer` is `pub(crate)`, the scene lowers to a fixed `PrimitiveBatch` enum, and there is **no public hook** for a custom pipeline/shader/render‑pass or raw drawable access (metal_renderer.rs:103,353‑548; unchanged on `main`). The only texture‑composite primitive (`surface`) hard‑asserts **YUV420 biplanar** (`kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`) and YCbCr→RGB converts — so it **cannot** composite an RGBA terminal texture and would chroma‑subsample colored text. Every single‑`CAMetalLayer` proven‑technique route therefore **requires a GPUI fork.** *But see below — the additive form is cheaper than feared.* |
| **(f) end‑to‑end keystroke‑to‑glyph latency** | ⏳ **needs live measurement** | No fork implied (input + render paths both public); actual latency on a built single‑stack prototype vs SwiftTerm and vs Path A's p95~31 ms requires a live‑GPU run. |

### Why there's no fork — the mechanisms (and the one boundary)

Every SwiftTerm fidelity feature maps onto GPUI's **fixed‑but‑rich** public primitive set — quads, shadows, MSAA paths (box‑drawing/powerline), wavy underlines, **monochrome glyph sprites** (subpixel‑positioned, per‑glyph transform), **polychrome sprites** (images/emoji, clipped, corner‑radii, grayscale, opacity), and a **content‑addressed atlas** that takes arbitrary bytes — all reachable through public `Window::paint_*` and the `canvas()`/custom‑`Element` seam. **Zed's own production terminal is the existence proof**: it paints every cell through `text_system().shape_line(...).paint()` + `paint_quad(fill(...))` with **zero renderer fork**.

The **one** genuine fork boundary is a **bespoke Metal shader/pipeline** — GPUI exposes no public hook to register one. That only flips the call to Path A *if* some specific SwiftTerm effect (e.g. a particular subpixel/gamma/blend AA technique) has **no** expression in the fixed primitive set. Because current GPUI already ships `SubpixelSprite` LCD‑AA, the fixed set is **very likely** sufficient — but this is the lone unverified **pixel‑comparison** that a live AA/gamma side‑by‑side must close.

### The non‑fork costs Path B must still budget (orthogonal to the renderer)

These are real, but none touches GPUI's GPU text/atlas core:
- **Graphics‑protocol parsing is absent from `alacritty_terminal`.** `Cell` is a fixed 24 bytes with no image payload, and (verified) the drop happens at the **`vte` ANSI layer**, not just `Term`'s `Handler` — so overriding the handler is insufficient; you need a **pty‑stream side‑channel** that splits DCS‑sixel + APC‑kitty‑graphics out *before* `vte` desyncs. Adopt **`sixel-image` + `sixel-tokenizer`** (MIT, Zellij‑proven) for sixel; **`termwiz` `escape::apc::KittyImage`** (MIT) or a hand‑rolled APC parser for kitty graphics.
- **Kitty‑keyboard CSI‑u frontend encoding** — small `mac/events.rs` patch *or* objc2 keyCode side‑channel (above). Zed still ships only legacy xterm sequences here, so there is no off‑the‑shelf encoder to borrow.
- **Selection‑anchor survival across scrollback eviction** — a VT‑core (`Selection`) behaviour; Zed pins its own `alacritty` fork. Left **unverified** — confirm or build.

### Replacement‑library survey — the explicit ask: does anything slot in cleaner than from‑scratch?

**No.** Under the binding constraint — *single GPUI Metal stack + SwiftTerm‑class fidelity* — **nothing on the market beats building the renderer on GPUI's own primitives.** The field splits into two buckets that both fail the constraint, leaving only VT‑core reuse (which is exactly what Path B already assumes):

| Library | Role | Single‑stack on GPUI‑macOS? | Verdict |
|---|---|:--:|---|
| **`alacritty_terminal` 0.26** | VT core (grid/scrollback/damage/selection/search/vi/OSC‑8/**kitty‑keyboard**/pty) | ✅ headless, owns no surface | **KEEP — Path B's pick.** Apache‑2.0, already vendored by Zed (strong Claude corpus). Lacks sixel/kitty‑graphics parsing (bolt on). |
| **Zed `terminal_view`/`terminal`** | GPUI‑native TerminalView reference | ✅ all paint via GPUI scene | **ADOPT AS STARTING POINT.** Renders on public `shape_line().paint()` + `paint_quad`, ships text+emoji+IME+pixel scrollback scroll. Bar is *below* SwiftTerm (no sixel/kitty, ligatures off, line‑stepped scroll) but closing the gap is **additive app code, not a fork.** |
| **`sixel-image` + `sixel-tokenizer`** | Sixel decode companion | ✅ pure Rust | **ADOPT for sixel.** Streaming on‑the‑wire decode → feeds `paint_image`. |
| **`termwiz` (`apc::KittyImage`, `Sixel`)** | Graphics‑protocol parser companion | ✅ parser only | **USE for kitty‑graphics parsing** (or hand‑roll). Not a scrollback‑grid `Term` — a companion to alacritty, not a core swap. |
| **`sugarloaf` (RIO renderer)** | Full GPU terminal renderer (kitty+sixel+smooth scroll) | ❌ **surface owner** | **REJECT for single‑stack; KEEP as the fidelity benchmark.** Highest‑fidelity OSS renderer surveyed, but `Sugarloaf::new(SugarloafWindow{RawWindowHandle})` mints its **own** Metal device/`CAMetalLayer` with no external‑device/offscreen API — embedding it **= Path A's dual‑Metal tax in one process.** (Now defaults to native Metal, not wgpu — but still a surface owner.) |
| **`glyphon` (+`cosmic-text`/`swash`)** | wgpu GPU‑text atlas | ❌ separate wgpu device | **REJECT.** On GPUI‑macOS (native Metal) it spins its own device; duplicates GPUI's `text_system` on the wrong backend. `cosmic-term` built on it has no kitty/sixel — same fidelity gap. |
| **`libghostty` / `libghostty-vt`** | VT core; renderer unshipped | ❌ future renderer is a surface owner | **REJECT NOW (watch‑item).** Renderer not exposed, C ABI explicitly unstable, **Zig is a third toolchain** that dings the Claude hard gate harder than Path A. As a VT core, no better than `alacritty_terminal`. |
| **`wezterm-term` / `wezterm-gui`** | Full VT core w/ native sixel+kitty / app renderer | ❌ core unpublished; renderer is the app | **REJECT as a dependency.** `wezterm-term` is deliberately git‑rev‑only (no API stability, #6663); `wezterm-gui` is a surface owner. Only `termwiz` (its parser) is reusable. |
| **`vt100`/`vtparse`/`parley`/`swash`/`rustybuzz`** | Building blocks | — | **REJECT — no advantage** over `alacritty_terminal` + GPUI's built‑in text core. |

**The pattern:** every *high‑fidelity renderer* (sugarloaf, wezterm‑gui, libghostty's future renderer, and SwiftTerm itself) is a **surface owner** that reintroduces the exact dual‑stack/embed problem the rewrite is trying to escape; every *wgpu text stack* spins its own device on macOS. The only clean single‑stack reuse is a **headless VT core** — which is precisely Path B. **Recommended component stack: `alacritty_terminal` (VT core) + a from‑scratch GPUI‑native renderer on the public primitives + `sixel-image` (sixel) + `termwiz`/hand‑rolled (kitty graphics).**

### The single‑drawable option — feasible only as an *additive* GPUI fork

Its defining premise ("single GPUI drawable + proven renderer + **no** fork") is **refuted** at the source level (closed renderer, no custom‑shader hook, YUV‑only surface). **But** the strongest form — an **additive sibling terminal pipeline** (a new `PrimitiveBatch` variant + `Scene` field + `draw_` method + `.metal` functions + a pipeline in `MetalRenderer::new`) that reuses GPUI's command buffer, drawable, and present clock and carries its **own** atlas — does **not** touch GPUI's hardest subsystem (`text_system`/`open_type`/`metal_atlas`). So it is **materially cheaper than the feared "fork the GPU text core,"** and architecturally it would beat **both** A (one present clock — no dual‑Metal co‑pacing tax, no objc2 seam, no Swift FFI) and B (ports SwiftTerm's proven cell/atlas/sub‑line‑scroll technique — no fidelity‑regression risk). It does **not auto‑dominate**, because it trades those for a **permanently‑maintained `gpui_macos` fork** — squarely on the Claude‑maintainability hard gate. **Worth a cheap parallel spike** before final commitment; not a prerequisite for choosing B over A.

### Decision‑rule outcome and what remains

**Path B (provisional → now strongly favored on source).** All six verified lanes converge that the §11 rule's load‑bearing condition is **met**: SwiftTerm‑class fidelity is reachable on GPUI's **public** primitive/atlas/text API with **no** fork of `text_system`/`open_type`/`metal_atlas`, and Zed's own terminal proves the text path. So the call points to **Path B**, not Path A. It stays *provisional* only on the items source genuinely cannot settle — all of which need a **built prototype + live‑GPU/display measurement** (this research pass was deliberately source‑only):

1. **Sustained burst FPS + present pacing** of sub‑line scroll on a *built* single‑stack GPUI‑native terminal (vs SwiftTerm and vs Path A's Phase‑0 p95~31 ms). *The clock/present question.*
2. **Keystroke‑to‑glyph latency** on the same prototype.
3. **AA/gamma/subpixel pixel comparison** vs SwiftTerm — the **one** path that, if it needs a bespoke blend/shader, forces a fork and flips toward Path A.
4. **kitty/sixel terminal‑grid compositing** — per‑cell z‑interleave + clip + **atlas pressure** for large/**animated** images at refresh (the polychrome atlas was not built for video‑in‑terminal).
5. **Clean pty‑stream DCS/APC tap** that splits sixel+kitty out without desyncing `vte`.
6. **kitty‑keyboard keyCode recovery** via objc2 side‑channel (avoid a GPUI patch); verify GPUI's keystroke normalization doesn't corrupt multi‑layout alternate‑key reporting.
7. **Selection‑anchor survival across scrollback eviction** in the VT/`Selection` model.
8. **Single‑drawable additive‑fork spike** — build the minimal sibling pipeline, confirm one shared present clock with no added latency, and quantify fork merge‑conflict cost against N months of Zed `main` history (the maintainability gate).

**Net:** the architecture decision is **de‑risked from "feasibility unknown" to "favored, pending live measurement."** Items 1–4 are one focused build‑and‑measure session on the existing `spikes/phase0-poc/` harness; item 8 is the parallel "could‑dominate‑both" probe.

### ✅ LIVE MEASUREMENT — the single‑stack GPUI‑native terminal locks 60 fps (2026‑06‑27)

**Built and measured remaining‑item #1 (the headline live unknown).** New runnable PoC `spikes/phase0-poc` binary **`gpui-term`** (`src/gpui_term.rs`, ~470 LOC): a **single‑stack, GPUI‑native terminal** — an `alacritty_terminal` VT core rendered through GPUI's **public** paint API (`window.text_system().shape_line(...).paint()` for glyphs + `paint_quad` for cell backgrounds), inside GPUI's own **one** `CAMetalLayer`. **No SwiftTerm, no objc2 embed, no second Metal layer** — literally the Path‑B architecture. It **reuses `harness.rs` verbatim** (same mach clock, FPS reducer, memory sampler, and synthetic Claude‑stream workload as the Path‑A PoC), so the numbers are apples‑to‑apples with §10. Same clean 60 Hz panel, debug build, 18 s continuous max‑rate stream into a 120×40 grid, plus an animated sub‑pixel vertical offset (the **sub‑line smooth‑scroll** path). Reproduced across 3 runs:

| Run | Frame interval p50 / p95 / p99 (ms) | Frames / 18 s | Steady mem (MiB) |
|---|---|---|---|
| 1 | **16.66 / 16.83 / 17.57** | 1078 (59.9 fps) | 148.7 |
| 2 | **16.66 / 17.10 / 17.57** | 1079 (59.9 fps) | 147.6 |
| 3 | **16.67 / 16.81 / 17.67** | 1078 (59.9 fps) | 147.6 |
| **§10 ref — single‑stack baseline** | ~16.7 / 17.1 | — | ~111 / 114 |
| **§10 ref — Path A dual‑stack `txn`** | 18.3 / **31.2** (~54 fps) | — | peak ~155 |
| **§10 ref — Path A dual‑stack `sync`** | 33.3 / 42.6 (~30 fps) | — | — |

**Verdict — Path B locks refresh; the dual‑stack tail is gone.** The single GPUI stack holds the terminal at **p50 16.66 ms / p95 ~16.8 ms (~60 fps) under continuous burst load** — it reaches the single‑stack baseline and, crucially, its **p95 has essentially no tail** (≈16.8 ms) where Path A's best result (`txn`) still collapsed to **p95 31 ms**. The structural prediction in §11/§12 — *one `CAMetalLayer` ⇒ no shared‑commit contention ⇒ locked refresh* — is now **measured, not asserted.** (The harness's raw `cliffs>16.6ms ≈ 1019` count is the same 120 Hz‑calibrated over‑count §10 warned about — on a 60 Hz panel a ~16.66 ms median trivially trips a 16.6 ms threshold; read p95/p99.) This holds even though the prototype is the *worst case* for the renderer: a **debug build** that **re‑shapes every row every frame** with **no damage tracking / line caching** — a production renderer would only be cheaper.

**Honest caveats (what this run does and does NOT settle):**
- **Memory is not yet a clean win.** Steady ~**148 MiB** (debug + full‑reshape‑per‑frame) sits *between* the Nice baseline (~111–114) and Path A `txn` (~155) — native and far from Electron, but the §11 "Path B inherits ~baseline memory" claim needs a **release build + damage‑tracked renderer** re‑measure to actually test. (The "idle 13.7 MiB" the harness prints is a frame‑1, pre‑Metal‑allocation artifact, not a real idle baseline.)
- **Still owed (unchanged):** keystroke‑to‑glyph **latency** (needs real key injection / TCC; item 2); the **AA/gamma/subpixel pixel comparison** vs SwiftTerm (item 3) — now the **single most important remaining gate**, because it is the *only* path that could expose a bespoke‑shader need and flip the call toward Path A; **kitty/sixel image‑cell atlas pressure** at refresh (item 4); and the **single‑drawable additive‑fork** probe (item 8).

**Updated stance.** The decisive performance unknown — *can one GPUI stack hold the terminal at refresh under burst load?* — is **resolved YES**, and Path B's structural FPS advantage over Path A is now a measured fact. The A‑vs‑B call rests on a single remaining could‑flip item (the AA/gamma pixel comparison); everything else points to **Path B**.

---

# Addendum — backfilled candidate (failed in workflow)

## Slint + alacritty_terminal (backfilled evaluation)

> Note: this candidate's deep-dive agent failed (its structured deep-dive exceeded the StructuredOutput schema-retry cap — 5 schema-invalid attempts) in the main workflow, so Slint is absent from the workflow's head-to-head synthesis table. This section was researched out-of-band and is merged in here. Its conclusion (lower-middle of the Rust shortlist) means its absence did not change the top recommendation.

**Overall verdict.** A technically *native-efficient*, genuinely cross-platform Rust stack whose terminal half is excellent but whose chrome half is the wrong tool for *this* rewrite's primary driver. The core problem is that Slint's UI is written in a **bespoke `.slint` DSL** that is exactly the kind of niche, data-poor framework the rewrite is trying to escape SwiftUI for — so the AI-assistability win over SwiftUI is marginal — and its GPLv3/commercial licensing plus alacritty's "you rebuild the renderer" shape add real cost. It is a *viable* but **lower-middle** Rust option for Nice: weaker than GPUI or a winit/objc2 DIY for this specific app.

### Scored dimensions

| # | Dimension | Score | Conf. | One-line rationale |
|---|-----------|:---:|:---:|---|
| D1 | AI-language (Rust) | **4/5** | high | Rust is a top-tier, very-well-represented language in Claude's corpus; borrow-checker/lifetime friction is the only drag, not knowledge. |
| D2 | AI-UI-framework (Slint + `.slint` DSL) | **2/5** | medium | ~23k-star lib with decent first-party docs, but the bespoke `.slint` markup DSL is niche with thin Stack Overflow / real-repo presence — the same "data-poor framework" trap as SwiftUI. |
| D3 | Efficiency (mem/CPU) | **4/5** | medium | Compiled native toolkit, Skia/Metal GPU renderer, tiny runtime core; firmly native-class, nowhere near Electron. No public Slint-vs-SwiftUI RSS benchmark exists — parity is inferred from architecture, not measured. |
| D4 | Styling (vibrancy/blur/chrome) | **2/5** | med-high | Slint draws its *own* widgets; no native NSVisualEffectView/vibrancy exposure — true liquid-glass needs a hand-written unsafe AppKit bridge, and you fight Slint's window-property overriding. |
| D5 | Terminal (alacritty_terminal) | **3/5** | medium | `alacritty_terminal` is a proven, excellent state engine (Zed/ZTerm/Termy), but ships no renderer; high-perf path is a from-scratch wgpu renderer imported via Slint's experimental `Image::try_from<wgpu::Texture>`. |
| D6 | Windowing (multi-window + tear-off) | **3/5** | med-low | Real multi-window since 1.7 (winit-backed); pane tear-off is feasible because the terminal is *data* (move the `Term` to a new window, no NSView to reparent), but Slint aggressively overrides window props (no first-class custom window mgmt). |
| D7 | Maturity + licensing | **3/5** | high | Stable 1.x, frequent releases (1.17, Jun 2026), commercial backing, production use — but GPLv3 / royalty-free-with-strings / paid-commercial, a real adoption cost vs MIT/Apache peers; desktop window APIs still unstable. |
| D8 | Linux (bonus) | **5/5** | high | First-class X11/Wayland via winit; `alacritty_terminal` is fully cross-platform. Strong free win. |
| D9 | Migration surface | **2/5** | high | Full rewrite; the alacritty path means the SwiftTerm Metal renderer is not reusable — rebuild the GPU text renderer from scratch and discard most of the ~64k Swift LOC. |

### Styling verdict
Slint renders every widget itself through Skia/FemtoVG into a single winit-owned root `NSView`; it has no native-material element and no public API for NSVisualEffectView vibrancy, rounded native chrome, or custom traffic-lights. What *is* supported: transparent/non-opaque window backgrounds (PR #2649) and an optional native macOS menu bar. Everything beyond that — blur/vibrancy, hiding/replacing the titlebar, traffic-light placement — is an unsupported manual bridge: reach the raw window handle through the private `i-slint-backend-winit` crate, cast to `NSWindow`/`NSView`, and drive AppKit with `objc2`/`cocoa` yourself (community recipe in discussion #5710; custom-titlebar is still only proposal #2521; hide-titlebar is a private-API workaround in #4284). Worse, Slint is documented to override window properties you set (issue #11001, "impossible to get real full screen on macOS"), so you partly fight the framework to get bespoke chrome. Net: Nice's liquid-glass look is *achievable* only by writing roughly the same AppKit glue a DIY objc2 approach needs — so Slint buys you little here while still constraining you.

Sources: discussion #5710 · PR #2649 · issue #2521 · discussion #4284 · issue #11001 · Slint MenuBar docs

### Biggest risks
1. **Marginal AI win over SwiftUI (kills the primary driver).** The `.slint` DSL is as niche/data-poor as SwiftUI; Claude leans on first-party docs, not a deep public corpus. D1 (Rust) is strong, but the rewrite's value lives in D2, and D2 is weak.
2. **Styling is DIY anyway.** You write nearly the same unsafe AppKit/objc2 bridge for vibrancy/chrome as a from-scratch winit approach — without the full control that approach gives.
3. **High-perf terminal rendering rides an experimental surface.** wgpu-texture import (`Image::try_from<wgpu::Texture>`) landed in 1.12 behind an `unstable-wgpu-NN` feature, version-pinned to specific wgpu releases — a moving target for a long-lived app.
4. **Throws away the proven Metal work.** alacritty path = rebuild smooth-scroll, selection-across-stream, and resize-coalescing on a new renderer; the SwiftTerm investment is sunk.
5. **Licensing.** GPLv3 forces open-sourcing a proprietary app; the royalty-free desktop license carries Slint's own terms; commercial is paid — a standing constraint vs permissive peers.

### Adversarial self-check (load-bearing claims vs primary sources)
- "Slint has no native vibrancy; you must bridge AppKit yourself." — **Supported.** Only community raw-handle/`cocoa` recipes exist; PR #2649 adds background transparency only, no NSVisualEffectView.
- "`.slint` DSL is a niche, data-poor corpus." — **Supported (with nuance).** Repo healthy at ~23k stars and Rust well-covered, but the *DSL specifically* has little third-party corpus; "data-poor" is a reasoned inference, not a measured count (confidence medium).
- "alacritty_terminal provides no renderer; you rebuild it." — **Supported.** Docs scope it to grid/term/selection/pty/parser; Zed/ZTerm/Termy each pair it with their own GPU renderer.
- "Native efficiency, far from Electron, ~parity with SwiftUI." — **Partly uncertain.** Native-class/non-Electron well-supported; the "<300 KiB runtime" figure is the embedded *core*, not a desktop app's RSS, and no Slint-vs-SwiftUI benchmark was found (D3 confidence capped at medium).
- "wgpu texture import is experimental." — **Supported.** Exposed via `unstable-wgpu-NN` feature gates, wgpu-version-pinned.
- "Real multi-window exists but custom window mgmt is constrained." — **Supported.** Multi-window since 1.7; window-property-override limitation documented in #11001.
- "Embedding the existing SwiftTerm NSView under Slint isn't viable." — **Uncertain/likely-true.** Slint owns a single winit root NSView and exposes no AppKit-view-host element; foreign-NSView subview insertion is undocumented and unsupported.

### Ranking note
Likely **below GPUI and below a winit/objc2 DIY** for Nice, and roughly **on par with or just under Iced**: GPUI is the proven Rust-terminal path (Zed/ZTerm/Termy = GPUI + `alacritty_terminal`) with permissive Apache licensing; a winit/objc2 DIY gives true vibrancy and could even re-host the existing SwiftTerm Metal NSView (preserving the Metal work); Iced avoids a bespoke DSL (pure-Rust UI = better Claude leverage on the framework) at some cost to native styling. Slint's bespoke `.slint` DSL (weak D2) + GPL/commercial licensing + sunk Metal work are what push it down the Rust shortlist.


---

## Spike artifacts (compiled, in scratchpad)

The synthesis above is grounded in four hands-on spikes. Their code lives under the session scratchpad:

- `spike-gpui-glass` — built: **true** — `/private/tmp/claude-501/-Users-nick-Projects-nice/41cca6d5-0ce3-4af0-9871-f71d823dd4d8/scratchpad/spike-gpui-glass/glassdemo (src/main.rs + Cargo.toml; built binary at target/debug/glassdemo). Mechanism reference: gpui-0.2.2/src/platform/mac/window.rs:1257-1311 (set_background_appearance) and :2500-2510 (BlurredView = NSVisualEffectView subclass, material .Selection). No tools were brew-installed; only crates.io dependencies were used.`
- `spike-rust-term` — built: **true** — `alacritty spike (PASSing, built + run): /private/tmp/claude-501/-Users-nick-Projects-nice/41cca6d5-0ce3-4af0-9871-f71d823dd4d8/scratchpad/spike-rust-term/src/main.rs (deps in .../spike-rust-term/Cargo.toml: alacritty_terminal=0.26.0, portable-pty=0.9.0; binary at .../spike-rust-term/target/debug/spike_rust_term). libghostty zig-dependency probe (build fails without zig, by design): /private/tmp/claude-501/-Users-nick-Projects-nice/41cca6d5-0ce3-4af0-9871-f71d823dd4d8/scratchpad/ghostty-probe (Cargo.toml adds libghostty-vt=0.2.0). alacritty 0.26 API reference source extracted at: /private/tmp/claude-501/-Users-nick-Projects-nice/41cca6d5-0ce3-4af0-9871-f71d823dd4d8/scratchpad/at-src/alacritty_terminal-0.26.0. All work confined to the scratchpad; the Nice worktree, the Nice/Nice Dev apps, and /Applications were not touched.`
- `spike-reuse-swiftterm` — built: **true** — `/private/tmp/claude-501/-Users-nick-Projects-nice/41cca6d5-0ce3-4af0-9871-f71d823dd4d8/scratchpad/spike-reuse-swiftterm/ — Phase A (pure objc2 embedding, event/resize/first-responder): src/main.rs (binary: target/debug/spike). Phase B (Swift NSView under Rust chrome across C ABI): swift-embed/SwiftTermStub.swift (Swift NSView + @_cdecl C ABI, compiled to swift-embed/libswifttermstub.dylib via `swiftc -emit-library`), swift-embed/src/main.rs (Rust host that links + drives it), swift-embed/build.rs (cargo:rustc-link-lib + @rpath). Build/run: `cargo run` in each dir; set SPIKE_RUN=1 to open a real window on a machine with a display. No brew installs, no network installs, nothing touched outside the scratchpad.`
- `spike-altui-vibrancy` — built: **true** — `/private/tmp/claude-501/-Users-nick-Projects-nice/41cca6d5-0ce3-4af0-9871-f71d823dd4d8/scratchpad/spike-altui-vibrancy/ — bridging code: src/main.rs (winit window -> raw-window-handle -> NSView/NSWindow; attaches NSVisualEffectView + runtime NSGlassEffectView; lines ~84-178 in attach_native_effects); Cargo.toml (winit 0.30, objc2 0.6, objc2-app-kit/objc2-foundation 0.3); built binary: target/debug/altui_vibrancy; runtime proof: run.log (EFFECT_ATTACHED=1 / GLASS_PRESENT=1 / WINDOW_NUMBER=12130).`
