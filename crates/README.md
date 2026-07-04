# Nice rewrite — Rust + GPUI workspace

This is the permanent home of the Nice rewrite (decision report:
`../notes/rewrite-stack-research.md`, Path B — all-Rust, single Metal stack
via [GPUI](https://www.gpui.rs/), zed's UI framework). It coexists with the
Swift app at the repo root; nothing Swift moves, and this workspace never
builds, installs, or touches `/Applications/Nice.app` or
`/Applications/Nice Dev.app`.

The roadmap for the rewrite lives at
`../notes/rewrite-feature-roadmap-20260702.md`; this file documents the
workspace as it exists, and every later cycle that adds a crate or a
self-test scenario should extend it in place rather than leaving the map
stale.

## Crate map

```
crates/
  nice           — the app binary (GPUI). Process name `nice-rs`.
  nice-harness   — measurement + self-test library. No app logic lives here.
  nice-model     — per-window document model as pure data: the projects/tabs/
                   panes value tree + the Claude status model (R8). The
                   documented asymmetries are deliberate + test-pinned. No gpui
                   dependency.
  nice-theme     — design tokens as pure data (palettes, accents, typography,
                   chrome geometry). No gpui dependency.
  nice-term-core — headless terminal core: pty spawn semantics + the
                   alacritty_terminal VT (grid/scrollback/damage) + the pane
                   session state machine (deferred spawn, events, held panes).
                   No gpui dependency.
  nice-term-input— pure input layer (R5): keyboard encoder (kitty CSI-u +
                   legacy VT fallback), VT mouse (X10/SGR/UTF-8),
                   bracketed-paste wrap, option-as-meta config, and the IME
                   marked-text state machine (the five G1 gating behaviours as
                   pure transitions). Plain key/mouse structs in, bytes out;
                   byte-exact unit tests. No gpui dependency.
  nice-term-view — the GPUI-native terminal renderer (R4): the core->GPUI
                   adapter entity (TerminalSessionHandle), the terminal-theme
                   value type, and the TerminalView/TerminalElement cell
                   painter. A UI crate — depends on gpui.
```

### `crates/nice` (bin `nice-rs`)

The GPUI application. Structure (grows over later cycles):

- `app` — owns window creation and the two root paths: the shipped window
  (`run` → `open_live_terminal`) now hosts a single live terminal pane — the
  login shell (`zsh -il`) by default, or a one-off `NICE_RS_COMMAND` command —
  in a `TerminalView` wired to the demand-present kick; and the self-test
  scenario windows (registered in `selftest_scenarios()`). `RootView` (the
  solid-background + version-line animated view) is now just the `smoke`
  scenario's root, no longer the shipped window.
- `platform` — the single home for foreign AppKit / `objc2` / CoreGraphics
  access (see "All-Rust rule" below): the demand-present kick (`present_kick`)
  plus the two present-timing facts that motivate it (R1), the macOS keyCode
  side-channel feeding the R5 keyboard encoder, and (R5) the CGEvent / `AXIsProcessTrusted`
  / TIS-input-source FFI the live input scenarios drive — synthetic events are
  posted **only** with `CGEventPostToPid` to nice-rs's own pid, never the global
  HID tap. R7 adds two more FFI surfaces here (keeping the view crates objc2-free
  via the same injection pattern as the present-kick): `read_dropped_image_to_temp`
  reads the **drag pasteboard** for a raw-image drag (browser / Messages / Preview,
  no file URL), transcodes it to a temp PNG, and returns that path (the T7 raw-image
  drop fallback, injected via `set_image_drop_provider`); and `launch_deadline`
  builds the **App-Nap-safe** grace-deadline future the T9 launch overlay arms —
  a dedicated OS-thread `nanosleep` that wakes the main runloop (the spike-6
  finding that a coalescable libdispatch timer can be deferred indefinitely on an
  idle/occluded app), injected via `set_launch_deadline`.
- `input_live` — the R5 live input self-test scenarios (`input-live` /
  `input-shell`): real CGEvents posted to our own pid, byte-exact pty receipt,
  the item-4 candidate anchor, and the IME go/no-go probe (see the scenario
  table under "Self-test harness").
- `niceties_zoom` — the R7/T11 live zoom + pty re-metric self-test
  (`niceties-zoom`): real ⌘+/⌘−/⌘0 CGEvents grow the shared font, the grid
  re-fits, and the pty winsize follows.
- `niceties_drop` — the R7/T7 file/image drag-drop self-test (`niceties-drop`):
  the drop handler is driven with constructed `ExternalPaths` events, asserting
  byte-exact escaped-path typing.
- `niceties_overlay` — the R7/T9 "Launching…" overlay self-test
  (`niceties-overlay`): a slow silent pane shows the overlay past a short grace
  window and clears it on first output, while an instant-prompt pane never
  flashes it.
- `niceties_held` — the R7/T10 held-pane self-test (`niceties-held`): a non-zero
  exit stays mounted with the dim in-buffer footer + the dismiss affordance,
  typing is inert, and dismiss respawns a fresh shell.
- `main.rs` — dispatches on `NICE_RS_SELFTEST`: unset runs the normal app,
  set runs the self-test driver.

### `crates/nice-harness` (lib)

The measurement + self-test library every later cycle reuses. Modules:

- `clock` — monotonic mach clock (`mach_absolute_time` + timebase), the
  single time source for every frame stamp and measurement.
- `mem` — `task_info(TASK_VM_INFO)` `phys_footprint` + `resident_size`
  sampler (hand-declared `struct task_vm_info`; `mach2` 0.4 doesn't ship it).
- `signpost` — `os_signpost` emission on subsystem
  `dev.nickanderssohn.nice-rs` (category `selftest`, name `Frame`). The
  actual emission is a C shim (`src/signpost.c`, compiled + linked by
  `build.rs`) because the `os_signpost` macros must run from C to place
  their strings in the `__TEXT` sections Instruments reads.
- `frame` — the frame-stamp stream, the percentile reducer (p50/p95/p99 over
  frame intervals), and the cadence gate (`assess_cadence`): passes when a
  scenario sustains enough frames and p95 interval `< 2×` the median.
- `watchdog` — an App-Nap-immune OS-thread deadline. macOS App Nap
  indefinitely defers coalescable timers in an idle, occluded gpui app (a 60s
  libdispatch deadline was observed not firing within 8 minutes — phase-0
  spike-6 finding), so self-test exit logic cannot rely on a coalescable
  timer or the gpui render path. The watchdog sleeps on a dedicated OS thread
  in drift-corrected slices, then forces the deadline callback onto the main
  thread via `dispatch_async_f` + `CFRunLoopWakeUp` (both immune to App Nap),
  and hard-exits(3) if the main thread still hasn't serviced it ~20s later.
  One watchdog per process; `arm()` must be called on the main thread.
- `capture` — screenshot capture via `Window::render_to_image()`, behind the
  `capture` cargo feature (see "Screenshot capture" below).
- `selftest` — the `NICE_RS_SELFTEST` driver + `all` suite runner, and the
  `Scenario` registry later cycles extend (see "Self-test scenarios" below).
  Each scenario declares a `Gate`: `Cadence` (the default — the driver measures
  a fixed window and asserts jitter sanity) or `SelfReported` (the scenario runs
  its own long measurement + gate and posts the verdict; the driver just waits).
  `term-perf` uses `SelfReported` for its absolute frame-time + memory budget.
- `workload` — the deterministic synthetic "Claude-streaming" stressor (seeded
  xorshift + a weighted SGR/reflow/long-line/unicode/plain content mix, ported
  from the phase-0 spike) that `term-perf` floods a pane with.

### `crates/nice-model` (lib)

Nice's per-window document model ported to **pure Rust** — no window, no timer,
and **no `gpui` dependency** (it mirrors today's pure-Swift model code; see the
"Layering rule" below). The R8 cycle ports it in two layers, both verbatim.

**The value types + status model** (`Sources/Nice/State/Models.swift`):

- `PaneKind` / `TabStatus` — the pane kind and per-pane Claude status.
- `Pane` — a toolbar pill: `apply_status_transition` (the waiting-pulse
  acknowledgment truth table — a same-status re-report is a no-op that
  preserves acknowledgment), `mark_acknowledged_if_waiting`, `needs_attention`.
- `Tab` — a session: the derived aggregate `status()` over its live Claude
  panes (thinking > waiting > idle), `waiting_acknowledged()`,
  `has_running_claude()` (the promotion-refusal predicate), and the pure
  `recover_next_terminal_index` hydration helper (`^terminal\s+(\d+)$`,
  case-insensitive).
- `Project` — an ordered group of tabs.

**The document** (`Sources/Nice/State/TabModel.swift`):

- `TabModel` — the per-window projects/tabs/panes tree: seeding + the pinned
  Terminals group, `select_tab` (the single `active_tab_id` writer) +
  `navigable_sidebar_tab_ids`, tab/pane reorder, pane insert/extract + the
  shared neighbor-refocus rule, `add_pane`, renames + title locks +
  `apply_auto_title`, cwd bucketing (`add_tab_to_projects`/`find_git_root`) +
  `repair_project_structure`, the cwd resolution chain + `adopt_tab_cwd`,
  depth-1 `/branch` + handoff lineage, single-entry `remove_tab` + the
  parent-pointer sweep, and the two arg parsers.
- `FsProbe` — the injected filesystem seam (`exists` / `home`) that keeps the
  document a pure value-tree; production uses `std::fs`, tests inject a fake so
  the git-root/repair/bucketing ports stay hermetic (the Swift tests planted
  real temp dirs). Swift's `onTreeMutation` closure + `@Observable` write-back
  are consolidated into one explicit did-mutate signal whose observable
  contract survives verbatim: **a no-op transform produces no mutation event; a
  real change produces exactly one.**

**The asymmetries are deliberate.** This model contains behaviors that look
inconsistent and are each intentional + test-pinned (`Models.swift`,
`TabModel.swift`, and the ~180 ported unit cases are the spec) — a reader
"cleaning them up" is introducing a bug:

1. "At most one *running* Claude per tab" is a creation-edge rule keyed on
   `Pane::is_claude_running` (`Tab::has_running_claude`), **not** a struct-level
   uniqueness invariant, so a running Claude and a deferred-resume Claude
   coexist transiently and the aggregations tolerate it.
2. The per-tab "Terminal N" counter (`Tab::next_terminal_index`) is monotonic —
   never decremented, never reused.
3. Empty-input rename is asymmetric: `TabModel::rename_tab` with empty input is
   a no-op, while `TabModel::rename_pane` resets to the per-kind default, clears
   the lock, and (for terminals) consumes a counter slot.
4. Two cwd writers, two policies: OSC 7 writes `Pane.cwd` only, while
   `TabModel::adopt_tab_cwd` moves the tab and pulls along only panes still
   tracking the old cwd (diverged panes stay — per-pane, not all-or-nothing).

And in the lineage, `insert_branch_parent` re-parents an originating root's
former children on first-branch promotion, while `insert_handoff_child`
deliberately does **not** re-parent (the anchor stays root). `is_claude_running`
is `#[serde(skip)]` (runtime only; restores always come back `false`), mirroring
`Models.swift`'s `CodingKeys` exclusion.

`Tab.branch` (vestigial, roadmap M5) and `SidebarMode` (window UI state → R10)
are deliberately **not** ported here.

### `crates/nice-theme` (lib)

Nice's design system ported to **pure Rust data** — no behavior, no UI, and
**no `gpui` dependency** (it mirrors today's pure-Swift design code; see the
"Layering rule" below). Everything is ported verbatim from the Swift sources
and pinned by literal-equality tests that cite their Swift provenance (see
"Fixture-provenance convention" below). Modules:

- `color` — `Srgba`, the plain gamma-encoded sRGB value type the palette
  tables use (`f32` channels, same representation gpui's `Rgba` uses so the R9
  adapter converts losslessly).
- `palette` — the chrome palettes from `Sources/Nice/Theme/Palette.swift`.
  Structured exactly as today's model has them (no invented variants): `Nice`
  and `MacOs` accept either scheme; `CatppuccinLatte` is light-only and
  `CatppuccinMocha` dark-only (`Palette.matches(scheme:)`). Slot names mirror
  `Palette.swift`'s slots (`background`, `ink`, `line`, …), not SwiftUI view
  names. Nice/Catppuccin slots carry precomputed sRGB literals; the `MacOs`
  table carries `SystemColor` NSColor roles that resolve dynamically against
  the pinned `NSApp.appearance` at paint time (so it has one scheme-independent
  literal table). `slots(palette, scheme)` returns the table for a valid pair
  or `None` for the two off-scheme Catppuccin combos.
- `accent` — `AccentPreset` (terracotta / ocean / fern / iris / graphite) from
  `Sources/Nice/State/Tweaks.swift`. The `#rrggbb` hex is the source of record;
  `.color()` derives sRGB from it the way Swift's `Color(hex:)` does. Also the
  selection-tint alpha ratios (14% light / 22% dark).
- `typography` — the three font *aliases* (`niceUI`, `niceMono`,
  `niceMonoSmall`) from `Sources/Nice/Theme/Typography.swift` as
  `(text-style, design)` data. Font *resolution* (family chain, point size) is
  R7's job, not recorded here.
- `chrome_geometry` — every chrome magic number the R9–R11 plans need, named
  once: top-bar height (52), sidebar default 240 + resize clamp 160–480,
  traffic-light offsets, card corner radii / inset / shadow, from
  `WindowChrome.swift` and `AppShellView.swift`.

The tiny adapter from these plain types into gpui color types lives downstream
(`crates/nice`, R9), NOT here — that is what keeps this crate gpui-free and
unit-testable by plain arithmetic.

#### Fixture-provenance convention

`nice-theme` is a **verbatim port** of the Swift design system, so every ported
literal must stay traceable to its source. The convention every current and
future token in this crate follows:

- **Every ported literal cites its Swift source line** in a trailing comment,
  e.g. `Srgba::rgb(0.080, 0.066, 0.055), // Palette.swift:81`.
- **Tests are literal equality against fixtures, and each fixture repeats the
  Swift citation.** The expected value in a test is an *independent*
  transcription from the cited Swift line (double-entry bookkeeping): a
  fat-fingered literal in either the token table or the fixture fails the
  build. To audit, open the cited Swift line and confirm the value matches.
- **One documented exception:** the macOS-26 native traffic-light defaults
  (`MACOS26_TRAFFIC_LIGHT_LEADINGS` / `_PITCH` in `chrome_geometry`) are
  OS-owned *runtime* values the Swift code deliberately does not hardcode, so
  they cite the project-memory note
  `reference_traffic_light_geometry_macos26` instead of a Swift line. They are
  documentary (R9 must still read each button's live default), and this is the
  only place a citation points somewhere other than a Swift source line.

### `crates/nice-term-core` (lib)

The headless heart of the terminal (R3): Nice's exact spawn semantics plus the
`alacritty_terminal` VT core, all UI-free and testable under plain `cargo test`
(no window). **No `gpui` dependency** — the renderer (R4) consumes it through a
narrow API. Modules, bottom-up:

- `quoting` — `shell_single_quote` / `shell_backslash_escape`, ported
  test-for-test from `Sources/Nice/Process/ShellQuoting.swift`.
- `spawn` — the `SpawnSpec` (shell-only vs command, cwd, env pairs, rows/cols)
  and the pure projections of the PROTECTED spawn contract: `build_argv`
  (`None → ["-il"]`; `Some(cmd) → ["-ilc", "exec <cmd>"]`), cwd tilde-expansion
  (the command is never tilde-expanded), and the curated base env (SwiftTerm's
  set; PATH deliberately not forwarded so the login shell rebuilds it).
- `pty` — `PtyProcess`: real pty spawn (`openpty` + `fork` + `login_tty` +
  `execve`) honouring that contract, plus write-input, resize (SIGWINCH),
  child-exit reaping (a dedicated `waitpid` reaper thread → `ExitStatus`), and
  process-group SIGHUP-then-SIGKILL teardown so no orphaned zsh survives.
- `vt` — the `alacritty_terminal` glue: `SharedTerm =
  Arc<FairMutex<Term<EventProxy>>>` (the lock the R4 renderer holds only to
  read cells for one frame), the `EventProxy` that forwards `PtyWrite` replies
  (DA/DSR) back to the child **and** relays OSC 0/2 title events
  (`Event::Title` / `ResetTitle`) onto the owning `Session`'s outward stream
  (R6), the `DEFAULT_SCROLLBACK_LINES = 500` parity knob, and the owned
  `GridSnapshot` read helpers (lock briefly, copy, unlock — never held across a
  paint).
- `osc7` — the OSC 7 cwd **tee** (R6): a self-contained, byte-transparent
  scanner the feeder runs over each raw pty read chunk *alongside* (never in
  place of) the VT parser. vte 0.15 has no OSC 7 arm, so cwd cannot ride the
  parser's event stream; the tee recognises a complete
  `ESC ] 7 ; file://<host>/<path> ST|BEL` sequence (tolerant of split reads,
  matching vte's terminator set — BEL / `ESC \`), percent-decodes the path,
  validates the host is local, and emits `CwdChanged`. It never alters the bytes
  handed to the parser — the "never alters bytes" property is the contract R15's
  status parsing may later extend.
- `session` — `TermSession`: one *eager, already-live* session owning the
  `PtyProcess` + `SharedTerm` + the per-session feeder thread. Owns the two R6
  escape-sequence side-channels that straddle the VT core — OSC 0/2 titles (via
  the `EventProxy`) and OSC 7 cwd (via the feeder's `osc7` tee) — and exposes the
  synchronous `bracketed_paste_active()` DECSET-2004 query the R5 paste / R7 drop
  paths consult.
- `deferred` — `Session`: the value-owning pane session the renderer (R4) and
  the session manager (R13) consume, wrapping `TermSession` into the deferred
  spawn state machine, the outward event stream, and held-pane classification
  (below).

#### Threading model

Each live session runs its VT work **off the render thread**, the shape proven
in the phase-0 spike (`spikes/phase0-poc`, RESULTS-spike8):

- a **feeder** thread is the sole reader of the pty master; it blocking-reads
  bytes, runs the OSC 7 cwd tee (`osc7`) over the raw chunk, then parses the
  **same** bytes into the `Term` under a *brief* lock, then — **after releasing
  the lock** — fires the damage-wake so the UI grabs the lock and paints. The
  wake is a signal only (async/non-blocking, never under the lock, never
  re-entering the UI framework) — R4's session-host owns the receiving end;
- a **reaper** thread is the sole `waitpid` caller, recording the child's
  `ExitStatus` (no zombies, no double-reap);
- an **exit-watcher** thread blocks on the reaped status and pushes the outward
  `Exited` event, so the caller learns of an exit even though it produces no
  pty output.

The renderer never parses; it locks the shared `Term` only to copy the cells it
paints. `Session` layers the pane lifecycle on top of that: an explicit deferred
spawn state machine — `NotSpawned{spec} → Spawning → Live → Exited{status,
held}` — so a not-yet-focused pane is a real, matchable state, never a nil pty a
caller force-reads (the fix for BUG A in `docs/window-chrome-architecture.md`); a
typed, `#[non_exhaustive]` outward event stream (`OutputStarted`, `Exited{status,
held}`, and — landed in R6 — `TitleChanged`/`TitleReset` from OSC 0/2 via the
`EventProxy` and `CwdChanged` from OSC 7 via the feeder's tee); and held-pane
classification
(`should_hold_on_exit`, ported from `TabPtySession.shouldHoldOnExit`): a
non-zero or signalled exit the user didn't ask for is *held* — the `Term` and
its scrollback are kept alive so the failed output stays readable — while a
clean `exit 0` or an explicit user close is dropped.

### `crates/nice-term-view` (lib)

The GPUI-native terminal renderer (R4): it paints a `nice-term-core` `Session`'s
grid through gpui's **public** paint API inside gpui's single Metal stack. A UI
crate (it drives real gpui windows), so — like `nice-harness` — it depends on
`gpui`; there is deliberately **no AppKit bridging** here (the terminal is an
ordinary element in gpui's own tree, so the `NSViewRepresentable` dance today's
`TerminalHost.swift` needs does not exist). Modules:

- `theme` — `TerminalTheme` / `TerminalColor`, the render-half theme value (16
  ANSI + bg/fg/cursor/selection) shaped like `TerminalTheme.swift`. The two
  Nice built-in defaults are ported here; the catalog / import UI is R22.
- `color` — the full color-model resolver: 16 themed ANSI (through the theme),
  256-color indexed (computed xterm cube + grayscale ramp), and 24-bit
  truecolor, from an `alacritty_terminal` `vte::ansi::Color`.
- `session_handle` — `TerminalSessionHandle`, the core→GPUI adapter **entity**.
  It owns the `Session` and one task that drains the session's event stream +
  damage-wake, re-emitting typed gpui `TerminalEvent`s (`EventEmitter`) +
  `cx.notify()`. View-independent (title / cwd / exit events flow with no view
  attached — R6 / R7 ride this entity). Damage drives `cx.notify()` plus the
  injected demand-present kick (`set_present_kick`, whose `setNeedsDisplay` body
  lives in `crates/nice/src/platform`) on a short poll; replacing the poll with
  an event-driven wake is a later optimization.
- `element` / `view` — `TerminalElement` (the per-frame paint element: whole-bg
  fill + coalesced per-cell background quads + per-cell foreground glyph runs
  carrying `background_color` so the bg-luminance curve engages + a block
  cursor) and `TerminalView` (owns a `FocusHandle`; the caret's solid/hollow
  state is **computed** from `is_focused && window active`, never a stored flag).
- `font` (R7/T11) — `FontSettings`, the shared **app-level** terminal-font state
  (family chain + point size) every view `cx.observe`s so a ⌘+/⌘−/⌘0 zoom fans out
  to all panes; owns the SF Mono → JetBrains Mono NL → system-mono chain
  resolution through gpui's text system and the derived cell metrics. The type
  lives here (Rust's `nice → nice-term-view` graph forces it) but is constructed
  and owned once at the app root in `crates/nice` — no view creates its own.
- `drop` (R7/T7) — the pure escaped-path byte builder + path-safety filter behind
  the drag-drop handler (`NiceTerminalView.performDragOperation` port): dropped
  POSIX paths are backslash-escaped and space-joined in drop order, framed in
  `ESC[200~…ESC[201~` when the app enabled DECSET 2004 (else space-padded), never
  a trailing newline. Table-tested against the Swift semantics.
- `overlay` (R7/T9+T10) — the two niceties state machines split from paint for
  windowless unit testing: `LaunchOverlay` (the "Launching…" timing machine —
  `Pending → Visible` past the grace window, cleared on first output / exit) and
  `HeldPane` (latches `Exited { held: true }`, keeps the view mounted + scrollback
  readable, writes the dim in-buffer exit footer, and gates the dismiss respawn).
  Also the `LaunchDeadline` factory type the App-Nap-safe grace deadline is
  injected through (its objc2 body lives in `crates/nice/src/platform`).

R4 is now complete: the full color model, text attributes, selection,
box-drawing / block elements, wide glyphs, the row-quantized bottom-anchored
layout (T4), line-stepped scrollback scroll, and damage-driven present (the
injected `setNeedsDisplay` kick) all live here, and `crates/nice`'s shipped
window hosts a live zsh pane over this crate. The `term-perf` self-test gates
streaming frame time + memory under the synthetic workload. Out of R4's scope
(later cycles): keyboard/IME/mouse input (R5), OSC title/cwd (now landed in R6),
fonts/zoom + drag-drop + the launch overlay + held panes (now landed in R7 — the
`font`/`drop`/`overlay` modules above), and sub-line smooth scroll (deferred).

## Layering rule

**Crates that mirror today's pure-Swift model code must not depend on
`gpui`.** That purity is what made the Swift model layer painless to test and
reason about (`../notes/chrome-pain-catalog-20260702.md` §2), and the rewrite
means to keep it. `nice-harness` is not one of those crates — it is
inherently a gpui/measurement library (it drives and inspects real gpui
windows) — so it depends on `gpui` directly. `nice-theme` **is** one of those
crates — the first — and carries no `gpui` dependency (its color→gpui adapter
lives downstream in `crates/nice`). `nice-term-core` (R3) is the second — the
terminal session state + VT parsing carry no `gpui` dependency either; the
renderer (R4) consumes it through a narrow API and the damage-wake callback.
`nice-term-input` (R5) is the third gpui-free model crate — the input encoders
and the IME marked-text state machine are pure logic over plain key/mouse
structs, deliberately kept out of `nice-term-view` (which links gpui) so the
byte-exact encoder tests and the G1 IME-transition tests build without the gpui
stack; the R5 event-edge (`nice-term-view/src/input.rs`) translates gpui events
into these plain types at the boundary and hosts the platform `InputHandler`.
`nice-model` (R8) is the fourth gpui-free model crate — the projects/tabs/panes
value tree + the Claude status model carry no `gpui` dependency; the gpui
adapter that wraps the document in an Entity lives downstream (R12/R13).
`nice-term-view` (R4) **is** a UI crate —
like `nice-harness` it depends on
`gpui` directly (it is the renderer), so it is not one of the gpui-free model
crates. When a later cycle adds another model crate (parsing, session state,
config,
anything that doesn't paint pixels), it must likewise NOT gain a `gpui`
dependency; if it needs to talk to the UI layer, that's a sign the boundary
belongs in `crates/nice` instead.

## All-Rust rule

Path B means no Swift sources and no second language toolchain in this
workspace. Foreign AppKit access, when unavoidable, goes through `objc2` /
`objc2-app-kit` and lives behind exactly one platform module per binary
crate (`crates/nice/src/platform.rs` today). Don't scatter `objc2` calls
across view/business logic — add to `platform.rs`, or add a sibling
`platform` module in a new binary crate if one appears later.

## Vendoring GPUI: pin, patch, and provenance

GPUI is **not** a workspace member — it's vendored via a pinned git checkout
under `vendor/zed/` (gitignored, not committed; ~1 GB). The crates below
path-depend into it:

```toml
gpui          = { path = "../../vendor/zed/crates/gpui" }
gpui_platform = { path = "../../vendor/zed/crates/gpui_platform", features = ["font-kit"] }
gpui_macos    = { path = "../../vendor/zed/crates/gpui_macos" }
```

**Pin:** zed main revision `10b07951838e422722e34641f4a9c0bfec9037ff`, plus
the bg-luminance patch (`../patches/zed-bg-luminance.patch` — the phase-0
spike's closure patch that makes GPUI text anti-aliasing match SwiftTerm on
pixels; 65+/7− across 6 zed files). The patch file was copied byte-identical
(sha256-verified) from
`../spikes/phase0-poc/aa-gamma/bg-luminance-applied.patch` and must never be
hand-edited — regenerate and re-copy it from the spike if it ever needs to
change.

`crates.io` publishes `gpui 0.2.2`; that crate is **spike-only** and must
never be used for production code in this workspace — the pin above is the
only source of truth. **Changing the pin or dropping the patch is a human
decision, not something a later cycle or the reconciler should do silently.**

**Reproducing the checkout:** run `../scripts/vendor-zed.sh` (idempotent —
safe to re-run; a second run with the pin already checked out and patched is
a fast no-op). It:

1. Maintains a shared bare mirror at `~/.cache/nice/zed-mirror.git` (cloned
   from `zed-industries/zed` once; `git fetch`ed only when the pin is
   missing — override the mirror path with `NICE_ZED_MIRROR`).
2. Local-clones (hardlinked objects, cheap) the mirror into `vendor/zed`.
3. Checks out the pinned revision (detached).
4. Applies `patches/zed-bg-luminance.patch`, using a marker file
   (`vendor/zed/.nice-bg-luminance-applied`) plus `git apply --check` so a
   second run doesn't try to re-apply an already-patched tree.

Add `exclude = ["vendor"]` to the root `Cargo.toml` is **load-bearing**:
`vendor/zed` is itself a cargo workspace, and without the exclude, cargo
would try to auto-attach it as a member of *this* workspace.

**Licensing — binding, read before touching anything under `vendor/zed`:**
Zed's `crates/terminal`, `crates/terminal_view`, and the Zed app-layer crates
(`crates/title_bar`, `crates/workspace`, `crates/editor`, …) are
**GPL-3.0-or-later**. Never open, read, copy, or feed them to code
generation — not even "for reference." The allowed reference/reuse surface
is `vendor/zed/crates/gpui`, `gpui_platform`, `gpui_macos`, `gpui_macros`
(Apache-2.0 — verify a crate's license file before reading anything else in
the zed tree). Nice is MIT and publicly distributed; GPL taint is
unshippable. See the R1 plan's "Ground rules" section for the full allowed
list (alacritty frontend code, termwiz, gpui-ghostty, gpui-component,
sixel-image/sixel-tokenizer).

## Self-test harness

### Env contract

| Env var | Effect |
|---|---|
| `NICE_RS_SELFTEST=<scenario>` | Run one named scenario. Prints exactly `SELFTEST PASS <scenario>` and exits 0 on success, or `SELFTEST FAIL <scenario>` (+ a detail line on stderr) and exits nonzero on failure. |
| `NICE_RS_SELFTEST=all` | Run every registered scenario sequentially. Prints a PASS/FAIL table, exits nonzero if any scenario failed. This is the standing UI regression gate — every later plan's validation re-runs it, so a later cycle cannot silently break an earlier scenario. **Requires building with `--features selftest`:** at least one registered scenario (`tokens`) reads pixels back through `Window::render_to_image()`, which is gated behind that feature, so without it the suite FAILs (see "Screenshot capture" below). |
| `NICE_RS_SELFTEST_SECS=<f64>` | Override the per-scenario measurement window (default 2.5s). Applies after a fixed 0.5s warm-up that's always discarded. |
| `NICE_RS_CAPTURE=<path>` | Additionally write a PNG of each scenario's window to `<path>`. Requires building `crates/nice` with `--features selftest` (see "Screenshot capture" below) — without it, capture is a hard error, not a silent no-op. |

The whole self-test run — every scenario, in sequence — happens inside a
single `Application::run` call (`nice_harness::selftest::drive`, invoked from
`crates/nice`'s `run_selftest`). The driver activates the app so scenario
windows are frontmost and focused (see "Why frontmost & focused" below),
arms the watchdog, then spawns one async orchestrator that opens each
scenario's window in turn, warms up, measures, optionally captures, closes
the window, and moves to the next scenario.

### Registered scenarios

| Name | What it exercises |
|---|---|
| `smoke` | Opens the window, drives continuous animated repaint via `request_animation_frame`, and asserts frame-cadence sanity (`p95 < 2× median` interval, at least 30 sampled frames). The minimal "the window opens and paints at a sane cadence" gate. |
| `tokens` | Renders a deterministic swatch grid from the `nice-theme` design tokens (every Nice/Dark palette slot plus the five accents), then reads each swatch centre back through `Window::render_to_image()` and asserts it matches the token's sRGB value within ±8/255 per channel — proving the tokens survive gpui's fill pipeline + Metal compositing, not just unit arithmetic. The pixel read-back needs the `selftest` feature (same `render_to_image` path as `NICE_RS_CAPTURE`); without it the scenario FAILs. The scenario samples pixels and hard-exits nonzero on mismatch itself — the `Scenario` shape and driver are unchanged (no post-capture hook). |
| `term-render` | Drives the `nice-term-view` renderer (R4) over a fixture-fed `nice_term_core` `Session` (a byte stream piped in via `cat`, with `ZDOTDIR` pointed at an empty dir so no user zsh rc pollutes the grid): a 16-color themed-ANSI swatch row, a 256-color indexed cube/ramp row, a 24-bit truecolor row, a parked block cursor, and two same-glyph cells (dark-on-light / light-on-dark), plus inverse-video, box-drawing / block, wide-glyph / emoji, underline / strikethrough, and a programmatic selection row. It captures and asserts those cell pixels within ±8/255, the cursor center matches the accent, and the **bg-luminance patch ENGAGES** (dark-on-light antialiased coverage exceeds light-on-dark — a check that fails on an unpatched vendor tree). Needs the `selftest` feature (pixel read-back) and a frontmost, focused window. |
| `term-layout` | The T4 row-quantized, bottom-anchored layout gate: resizes the window shorter than the grid and asserts (via capture) the bottom prompt row stays pinned at the bottom gap while the top rows clip under the chrome. |
| `term-scroll` | The scrollback scroll + park/snap gate: feeds >1 screen of numbered lines into an echo-off `cat`, then asserts (via the core's display offset + visible snapshot) parked-at-bottom, offset-3 after scroll-up, no auto-snap while scrolled, and snap-to-bottom resuming. |
| `term-perf` | The streaming frame-time + memory budget gate (Validation §5). Floods a live ~120×40 pane (scrollback 10 000) with 15 s of the deterministic `nice_harness::workload` synthetic stream through a raw-mode `cat` while the RAF-animated `TerminalView` stamps frames; self-activates its window, reduces the frame stream to interval percentiles, samples memory, and gates on **absolute** frame times (p50 ≤ 17.5 ms, p95 ≤ 20 ms) plus the pane's own memory **growth** over its entry baseline (< 120 MiB) — a criterion the cadence-jitter gate can't express. (Growth, not absolute, because inside the `all` suite the process already carries ~140 MiB from the five prior scenarios' retained windows/atlas/readbacks; the absolute < 200 MiB "steady" budget is validated by the dedicated `NICE_RS_SELFTEST=term-perf` run — a fresh process, ≈142 MiB.) Runs up to 3 times, gates on the best run, prints the percentiles + memory in the transcript. Uses `Gate::SelfReported` (it runs its own measurement and posts the verdict). |
| `input-live` | The R5 live keyboard/paste/IME-anchor gate (Validation §2–§4). Spawns a capture-tee session (`sh -c 'stty raw -echo; exec tee <cap>'`), posts **real CGEvents** to nice-rs's own pid (`crate::platform`, `CGEventPostToPid` — never the global HID tap), and asserts the bytes appended to the capture file match exactly: plain ASCII (rides the IME `insertText` path → pty), ⌘V paste with DECSET 2004 **off** (raw) then **on** (`ESC[200~…ESC[201~`), and arrow keys (`ESC[A/B/C/D`). Then the G1 **item-4 candidate anchor** is asserted programmatically — park the grid cursor mid-grid (CUP), drive a composition through the real `TermInputHandler`, and check `bounds_for_range` returns a rect at the grid-cursor cell (never `None`, the zed#46055 failure mode). Finally the **IME go/no-go probe** (TIS → Pinyin): if synthetic composition engages, items 1–3 + 5 are asserted mechanically; if not (plan-flagged UNPROVEN — and on this machine Pinyin is installed-but-not-enabled, so `TISSelectInputSource` refuses it), it records a **DEFERRED HUMAN PASS** (stderr checklist) rather than fail-looping. The user's keyboard input source is **always** restored (on `Drop`). Preflights `AXIsProcessTrusted()` and FAILs loudly (never silently skips) if the Accessibility grant is missing. `Gate::SelfReported` (byte-exact receipt, not cadence). |
| `input-shell` | The R5 real-shell CGEvent sanity gate (Validation §5). A real `zsh -il` (user rc suppressed via an empty `ZDOTDIR`): polls the grid until the shell prints its prompt, then types `echo <marker>` + Enter entirely via CGEvents and asserts the marker appears ≥ 2× in the grid (the typed command echo **and** the command output), proving the whole path reaches a real login shell and its output round-trips. `Gate::SelfReported`. |
| `niceties-zoom` | The R7/T11 live zoom + pty re-metric gate (Validation §2). Drives the shipped ⌘+/⌘−/⌘0 zoom keybindings with **real CGEvents** to nice-rs's own pid over a real login shell and asserts the whole T11 chain: after ⌘+ ×3 the shared `FontSettings` reports a larger point size + cell box, the view re-fits the grid and pushes `(rows, cols)` to the pty (asserted both by the core `Term`'s grid dimensions matching an independent `fit_grid` **and** `stty size` in the child echoing them — proving SIGWINCH reached the shell), and ⌘0 restores the baseline exactly. Preflights the Accessibility grant and FAILs loudly if it is missing (a dropped CGEvent would make every zoom a no-op). `Gate::SelfReported` (state assertions, not cadence). |
| `niceties-drop` | The R7/T7 file/image drag-drop gate (Validation §3). Drives the view's drop handler through its test seam (`handle_external_paths_drop`) with **constructed** `ExternalPaths` events over a real pty (a real OS drag is impractical headless, and gpui's macOS backend only accepts filename drags) and asserts the exact bytes typed into the child: one escaped, space-padded path (DECSET 2004 off); multiple paths space-joined in drop order; a path with spaces / shell metacharacters backslash-escaped; the **raw-image fallback** (a drop with no file URLs consults the injected image-drop provider — a stub path here); the `ESC[200~ … ESC[201~` frame with 2004 **on**; and never a trailing newline. Reuses the `input-live` capture-tee child; drives the handler directly, so it needs **no** Accessibility grant. `Gate::SelfReported` (byte-exact receipt). |
| `niceties-overlay` | The R7/T9 "Launching…" overlay timing gate (Validation §4). Two cases over the real overlay state machine + the App-Nap-safe grace deadline, asserted via the view's exposed overlay state (feature-independent) and, when the `capture` feature is compiled, a pixel probe of the accent status dot: a **slow silent pane** (`sh -c 'sleep 3; echo up'`, a short grace) stays silent past the grace window so the overlay shows, then the first-output `up` clears it; an **instant-prompt pane** (a normal `zsh -il`, the default grace) beats the window so the overlay **never** flashes (`overlay_ever_visible` stays `false`). `Gate::SelfReported` (state transitions, not cadence). |
| `niceties-held` | The R7/T10 held-pane gate (Validation §5). A pane running `sh -c 'echo FINAL; exit 3'` exits non-zero, so the R3 classification holds it; asserts the whole contract over a real session: the pane latches held, `FINAL` stays in the grid, the dim `[Process exited (status 3)]` footer is fed into the held term, a **real CGEvent** keystroke is inert (grid unchanged, still held, no crash — the dead pty is never written and no AppKit beep), and dismiss respawns a fresh `zsh -il` in place (the grid no longer holds `FINAL` / the footer, a new prompt appears). Posts a real CGEvent for the inert-typing check, so it preflights the Accessibility grant and FAILs loudly if it is missing. `Gate::SelfReported`. |

Later cycles add scenarios by pushing onto the `Vec<Scenario>` returned from
`crates/nice/src/app.rs`'s `selftest_scenarios()`. A `Cadence`-gated scenario
needs no driver change — its view stamps a frame (`nice_harness::frame::stamp()`)
and requests the next animation frame every render, and the driver measures a
fixed window + asserts jitter sanity. A scenario whose pass criterion the jitter
gate can't express (an absolute frame-time / memory budget, a multi-run best-of)
declares `Gate::SelfReported { budget }`: it runs its own measurement in its
`open` task and posts the verdict via `nice_harness::selftest::report_gate`, and
the driver waits for it (up to `budget`) instead of measuring. `term-perf` was the
first such scenario; the R5 `input-live` / `input-shell` scenarios also self-report
(their pass criterion is byte-exact pty receipt from posted CGEvents, not cadence).
**Keep this table in sync** — it's the map a future cycle
(or a reconciler) reads to know what regression coverage already exists before
adding more.

### Why frontmost & focused

Two present-timing facts about the pinned zed-main revision govern every
scenario (documented in code at `crates/nice-harness/src/frame.rs` and
`crates/nice/src/platform.rs`):

1. `cx.notify()` alone never **presents** while a window's CVDisplayLink is
   stopped (gpui stops it on occlusion). A demand-driven repaint on an
   occluded window needs an explicit `setNeedsDisplay` kick to the `NSView`
   + its `CAMetalLayer` — that's `platform::present_kick`. The `smoke`
   scenario sidesteps this by driving continuous RAF repaints on a visible
   window; later demand-driven scenarios must issue the kick themselves.
2. zed-main frame-caps **inactive** windows at ~33ms (`min_frame_interval`),
   so a backgrounded window animates at ~30fps regardless of the panel
   refresh rate. Frame-cadence assertions must therefore run on a
   frontmost, focused window — which is why `selftest::drive` calls
   `cx.activate(true)` and why any manual self-test run needs the app in the
   foreground.

### Screenshot capture

`Window::render_to_image()` is public but gated
`#[cfg(any(test, feature = "test-support"))]` in gpui; the macOS renderer
implements it by reading the drawable texture back, which requires
`CAMetalLayer.framebufferOnly = false` — a flag `gpui_macos` only clears
under that same cfg, **process-wide**. Turning it on for the shipped app
would leave the live window's Metal layer non-framebuffer-only forever, so
capture is entirely opt-in via a cargo feature:

- `crates/nice`'s `selftest` feature is what you build with to get capture:
  `cargo build -p nice --features selftest` (or
  `cargo run -p nice --features selftest`).
- It forwards to **two** features that are both load-bearing:
  - `nice-harness/capture` → `gpui/test-support` — compiles the outer
    `Window::render_to_image()` method + the PNG encoder (`image` crate).
  - `gpui_platform/test-support` → `gpui_macos/test-support` — compiles the
    macOS `MacWindow::render_to_image` **override** (the one that actually
    reads the drawable texture). Without this half, the default trait impl
    bails with "render_to_image not implemented for this platform" even
    though the outer method compiled.
- The shipped bundle (`scripts/rust-bundle.sh`, no `--features`) omits both,
  so the live app's Metal layer stays framebuffer-only.
- We deliberately do **not** use `VisualTestAppContext::capture_screenshot`
  for this — that's a `TestDispatcher` context (off-screen windows,
  deterministic scheduling) and would invalidate the live cadence
  assertions the same scenarios make. Capture always runs against the real,
  on-screen window.

Perf thresholds (the cadence gate) were measured with `test-support` on in
the phase-0 spike, so they stay comparable whether or not `--features
selftest` is set.

## Running the self-tests

From the repo root, on a Mac with a GUI session (the app window must become
frontmost — see above):

```sh
# one scenario — smoke needs no feature; a scenario that reads pixels back
# (e.g. tokens) requires --features selftest, or it FAILs (see the scenario
# table above)
NICE_RS_SELFTEST=smoke cargo run -p nice
NICE_RS_SELFTEST=tokens cargo run -p nice --features selftest

# the full regression suite — --features selftest is required because at least
# one registered scenario (tokens) reads pixels back through render_to_image;
# without it the suite FAILs, exit nonzero
NICE_RS_SELFTEST=all cargo run -p nice --features selftest

# with a screenshot capture (needs the selftest feature)
NICE_RS_SELFTEST=smoke NICE_RS_CAPTURE=/tmp/nice-rs-smoke.png \
    cargo run -p nice --features selftest
```

Ordinary build/test commands:

```sh
cargo build --workspace          # debug build, all crates
cargo test --workspace           # unit tests
cargo build --workspace --release  # perf-gated validations should use this
```

The first build in a fresh worktree is a cold build of the whole gpui
dependency stack (after `scripts/vendor-zed.sh` has produced `vendor/zed/`)
— several minutes is normal, not a hang. `[profile.dev.package."*"]` in the
root `Cargo.toml` builds dependencies at opt-level 2 even in dev builds so
this cost is paid once per dependency version, not on every iteration of
your own code (which stays opt-level 0 for fast rebuilds).

## Bundling + installing

```sh
scripts/rust-bundle.sh    # cargo build --release -p nice, assemble + ad-hoc
                           # codesign build-rs/Nice RS Dev.app, verify
scripts/rust-install.sh   # (re)builds via rust-bundle.sh, force-quits a
                           # running nice-rs, installs to
                           # /Applications/Nice RS Dev.app
```

App identity (deliberately distinct from both Swift installs so nothing
collides in `/Applications`, UserDefaults, or process-name greps — renaming
to `Nice.app` happens at parity, Stage 8, not now):

| | |
|---|---|
| Bundle | `Nice RS Dev.app` |
| Bundle id | `dev.nickanderssohn.nice-rs-dev` |
| Display name | `Nice RS Dev` |
| Executable / process name | `nice-rs` |

Signing is **ad-hoc only** (`codesign -s -`), verified with
`codesign --verify --deep --strict`. This is deliberate and recorded, not an
oversight: R1 promises local installability, nothing more. Notarization and
release-CI wiring are Stage 8 (R27-adjacent) work — see the header comment
in `scripts/rust-bundle.sh` and the R1 plan's "Binding technical decisions."
**Do not** add Developer ID signing / notarytool / stapling to these scripts
before Stage 8.

`scripts/rust-install.sh` only ever touches
`/Applications/Nice RS Dev.app` — it has no flag that points it at
`/Applications/Nice.app` or `/Applications/Nice Dev.app` (the Swift builds).
Its running-instance detection uses `ps -Aww -o pid=,args=` + a path-scoped
grep, never `pgrep`/`pkill -f` (macOS truncates a GUI app's `comm` to 16
chars, which makes `pgrep`/`pkill -f` silently miss a running instance), and
it force-quits with SIGTERM → poll → SIGKILL rather than an AppleScript
`quit` (which would raise a confirmation dialog and stall an unattended
install) — mirroring `../scripts/install.sh`'s approach for the Swift `Nice
Dev` build as of commit `2c08c51`.
