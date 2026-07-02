# Accessibility (TCC) grant — prep for the keystroke-latency spikes

The keystroke-latency measurements (spike 4 second half, spike 5) inject a
**real** OS key event via `CGEventPost` and time the resulting present frame
(`latency = present − post`). `CGEventPost` at the session/HID tap requires the
**posting process** to hold macOS **Accessibility** trust. It does not today —
`AXIsProcessTrusted()` == false.

## Why we grant the app, not the harness binary

- The keystroke harness is a **cargo-built binary** → `codesign` shows
  `adhoc, linker-signed`, `TeamIdentifier=not set`. TCC keys an unbundled
  binary on its **cdhash**, which changes on **every `cargo build`**. So a grant
  made directly to the harness binary **dies on the next rebuild** (matches the
  known "prod Nice adhoc signing breaks TCC on rebuild" gotcha).
- When a CLI tool with no TCC entry calls `CGEventPost`, macOS consults its
  **responsible process** — for anything launched in this session that is
  **prod `Nice.app`** (pid 58741; verified: this shell's ancestry is
  `zsh → claude → /Applications/Nice.app`). Grant Accessibility to **Nice**
  once and every harness run in this session inherits it, across cargo rebuilds,
  because TCC checks Nice's identity, not the child binary's.
- Nice's own grant is stable **as long as prod Nice is not rebuilt** (we never
  rebuild prod). If prod Nice is ever rebuilt, its adhoc cdhash changes and the
  Accessibility toggle can show ON-but-dead → **remove + re-add** to fix.

This is a **System Settings toggle only** — not a build/install/test/kill of
prod Nice, so it stays inside the CLAUDE.md prod guardrails.

## Steps (≈30 s)

1. Open the pane (Claude can open it for you):
   `open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"`
2. Under **Privacy & Security → Accessibility**, find **Nice** and turn it **ON**.
   - If **Nice** isn't listed: click **+**, go to `/Applications`, add `Nice.app`.
   - If **Nice** is already ON but the verify below still says `false`, toggle it
     **OFF then ON** (or remove with **−** and re-add) — that clears a stale
     adhoc grant.
3. Verify (Claude runs this — it executes as a child of Nice, so it tests Nice's
   grant): 
   `swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'`
   → must print `true`.

## Target vs poster (why only one grant)

Only the **poster** (the harness, responsible = Nice) needs AX. The **targets**
— `gpui-term` (spike 5) and Nice Dev (spike 4) — merely *receive* the injected
events and need no permission. So a single grant to Nice covers both spikes.

## After granting

Claude will build the keystroke harness (posts `CGEventPost` keyDown to the
focused target window, N=500, single-in-flight, stamps present via the same
signpost/RAF path each side uses), re-check `AXIsProcessTrusted()`==true, then
run it against both gpui-term and Nice Dev on a display.
