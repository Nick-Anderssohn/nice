# Rewrite-stack spikes

Throwaway proof-of-concept spikes that underpin the chrome-rewrite
investigation in [`../notes/rewrite-stack-research.md`](../notes/rewrite-stack-research.md)
(generated 2026-06-26). They are **not production code** — they exist to turn
desk-research claims into compiled evidence.

**Source only.** The (multi-GB) `target/` build dirs and the nested `.git`
folders from `cargo new` were stripped; rebuild each from its own directory with
`cargo build` / `cargo run`. Toolchains used: `cargo`/`rustc` 1.96, `swiftc`
6.3, on macOS 26 / Apple silicon. Some open a real window and need a display
(see per-spike notes).

| Spike | Question it answered | Verdict | Report |
|---|---|---|---|
| `spike-gpui-glass/glassdemo` | Can Rust+GPUI reproduce Nice's liquid-glass look? | **Yes** — built-in vibrancy + native traffic lights | §3, §6 |
| `spike-rust-term` | Is there a proven Rust terminal core? | **Yes** (`alacritty_terminal`), but it ships no renderer | §5, §8 |
| `ghostty-probe` | Can `libghostty` be embedded from Rust today? | **No** — zig-gated, parser-only | §3 |
| `spike-reuse-swiftterm` | Can the SwiftTerm NSView be hosted under a Rust chrome? | **Mechanically yes** (stub-level); live-responder seam unproven | §4, §8, §10 |
| `spike-altui-vibrancy` | Is the glass look GPUI-specific, or reachable from any Rust UI? | **Any Rust UI** (winit+objc2) | §6 |

---

## `spike-gpui-glass/glassdemo`
Minimal standalone GPUI app. Proves modern GPUI (`gpui = "0.2.2"`, on crates.io)
opens a window with **real `NSVisualEffectView` behind-window vibrancy**
(`WindowBackgroundAppearance::Blurred`), rounded corners, a frosted panel, and
**repositioned native traffic lights** (`TitlebarOptions.traffic_light_position`)
— ~90 lines of safe Rust, zero objc/Metal. Refutes the earlier desk claim that
GPUI needs a private CGS blur API. One gap: GPUI hardcodes the material to
`.Selection`; Nice leans on `.sidebar` (a bounded ~20-line GPUI fork of
`set_background_appearance`, or one `objc2` subview). Mechanism reference:
`gpui-0.2.2/src/platform/mac/window.rs:1257-1311` and `:2500-2510`.
Run: `cd glassdemo && cargo run` (opens a window).

## `spike-rust-term`
`alacritty_terminal` 0.26 + `portable-pty` 0.9 spawn a real `/bin/sh`, feed it a
command, and parse the PTY stream incl. per-cell SGR decode. Proves the Rust
terminal state engine is solid and low-friction — but it provides **no
rendering**; a GPU text view is yours to build (as Zed/ZTerm/Termy do).
Run: `cargo run`.

## `ghostty-probe`
A one-line `cargo add libghostty-vt` probe. Build **fails by design** at the zig
dependency (`build.rs`), because `libghostty` builds with zig (not installed
here) and its public surface is parser-only — no win over `alacritty_terminal`
today. Building this requires installing zig.

## `spike-reuse-swiftterm`
Tests reusing Nice's proven SwiftTerm Metal `NSView` under a non-Swift chrome.
- **Phase A** (`src/main.rs`): a pure `objc2` host embeds, first-responds,
  resizes, and drives a live AppKit `NSView` as a subview — no Swift.
- **Phase B** (`swift-embed/`): a Swift `NSView` exposed over a `@_cdecl` C ABI,
  compiled to a dylib and linked + driven from Rust (`build.rs` wires the link).
  The `libswifttermstub.dylib` artifact was excluded; regenerate with
  `swiftc -emit-library SwiftTermStub.swift -o libswifttermstub.dylib` then
  `cargo run` (set `SPIKE_RUN=1` to open a real window).

⚠️ **Caveat that defines Phase-0:** this spike dispatched events by invoking
overridden selectors **directly**, on a **stub** view — *not* through a live
windowserver responder chain with the real SwiftTerm view. IME/kitty-keyboard
correctness and the GPUI z-order seam are therefore still unproven (report §10).

## `spike-altui-vibrancy`
Non-GPUI path: a raw `winit` 0.30 + `objc2` 0.6 app reaches the `NSWindow`/
`NSView` via `raw-window-handle` and attaches both a classic `NSVisualEffectView`
**and** the macOS 26 `NSGlassEffectView` ("Liquid Glass") at runtime
(`src/main.rs`, `attach_native_effects`). `run.log` records `EFFECT_ATTACHED=1` /
`GLASS_PRESENT=1`. Shows the glass look is reachable from any Rust UI, not just
GPUI. Run: `cargo run`.
