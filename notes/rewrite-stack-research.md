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
- **Gate before commitment (§10):** the Phase‑0 PoC must (i) **measure** burst FPS, keystroke latency, and idle+under‑load memory of the **dual‑Metal‑stack hybrid** against the current Nice baseline on this machine — the Path‑A efficiency PASS is provisional until this number exists — and (ii) drive **real OS keyboard/mouse/IME/focus routing through a live key window**, plus cross‑window Metal rebind.

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

- **All four proofs (4–7) PASS on the real renderer** — including **(5) the mouse hit-test seam**, the Path-A-vs-objc2-hybrid decider: a `class_addMethod` `hitTest:` override on GPUI's `GPUIView` routes terminal-region points to the terminal and chrome points to GPUI, and a synthetic drag produced a real terminal selection. Keyboard/IME routing, transparent-over-terminal compositing (also visually confirmed), and same-window Metal rebind all PASS.
- **Memory: native and flat** — idle ~37–53 MiB, under-load **143.6 / 145.9 MiB** steady/peak, no growth over the run. (Electron-class terminals run 300 MB–1 GB+.)
- **Frame rate: the one open item — but it is *pacing*, not throughput.** Term/GPUI present p50 ≈ **33 ms (~30 fps)**, **identical in debug and release** (so *not* a debug artifact) — yet `draw-attempt p50 = 0.01 ms` (the real per-frame Metal encode is ~free). The 30 fps is therefore the PoC's **naive present scheme** (a synchronous `present_now()` once per GPUI frame ⇒ two vsync-locked presents per cycle ≈ half of 60 Hz), **not** a dual-stack compute tax. The stack has large headroom; hitting refresh needs a **decoupled present loop** (e.g. drive the terminal off a `CVDisplayLink` instead of synchronously per GPUI frame) — a bounded follow-up.

**Net:** the dual-stack architecture is **validated** (every correctness/seam unknown cleared; memory native) and the provisional §5 `5†` efficiency mark is **substantially de-risked**. Remaining work: (a) a non-naive present scheme to prove sustained at-refresh FPS, and (b) the Nice Dev baseline for the comparison columns. **Leaning Path A.** Re-run: `cd spikes/phase0-poc && NICE_POC_REAL_BRIDGE=1 NICE_POC_RUN=1 cargo run` (see its README). The original methodology/decision-tree below stands.

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
