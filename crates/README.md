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

**Writing a new test?** Read `../docs/testing.md` first — it's the
placement rulebook (unit vs. in-process integration vs. live ground truth),
the differential-pair convention for seam-y interactions, the live-run
environmental preconditions, and the AX decision record. This file stays
the crate-map + self-test-scenario reference `testing.md` points back to;
it doesn't re-derive the layer story itself.

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
  nice-itests    — dev/test-only in-process gpui integration-test harness
                   (T2): behavior fixtures on mocked TestAppContext + visual/
                   pixel fixtures on real-MacPlatform VisualTestAppContext. The
                   shared bed the Stage-2 chrome/pane cycles write tests on. Not
                   depended on by the shipped `nice` binary (`publish = false`).
                   Depends on gpui. See `../docs/testing.md`.
```

### `crates/nice` (bin `nice-rs`)

The GPUI application. Structure (grows over later cycles):

- `app` — owns window creation and the two root paths: the shipped window
  (`run` → `open_managed_window` → `build_window_root`) and the self-test
  scenario windows (registered in `selftest_scenarios()`). **R13.5** makes the
  shipped window — and every ⌘N window — mount the full Swift-parity app shell
  (`AppShellView`, below) over ONE per-window `WindowState`, replacing the bare R9
  chrome band over a single terminal (the composition gap the launched app exposed).
  `open_managed_window` mints + seeds the window's `WindowState`, **arms the
  window's R14 control socket** (`arm_window_control_socket` — mint the socket path,
  set the `SessionManager`'s shell-injection env, start the accept thread, spawn the
  waker-woken foreground drain) BEFORE spawning the Main pane (the "env before fork"
  invariant: the pane inherits `NICE_SOCKET` / `ZDOTDIR` / `NICE_USER_ZDOTDIR` from
  launch), spawns the Main tab's pane into its `SessionManager` up front with the
  full shipped spec (the login shell `zsh -il` by default, or a one-off
  `NICE_RS_COMMAND`; later panes get a plain login shell via the R13 deferred-spawn
  path), opens the window, and hands back the shell `WindowHandle` (`run` / the ⌘N
  handler discard it — the `app-shell` scenario keeps it). **`run`'s R14 bootstrap**
  (`install_shell_inject_bootstrap`, before the first `open_managed_window` — and
  NEVER under `run_selftest`, so the regression suite never writes real user files):
  sweep stale `$TMPDIR` debris → write the `ZDOTDIR` stubs → capture Nice's own
  inherited `ZDOTDIR`, stored as an app global (`ShellInjectConfig`) every window's
  `arm_window_control_socket` threads into its shell env (R15 slots its orphan-shell
  reaper into this order). `build_window_root` registers the state in the
  `WindowRegistry`, tracks activation, mounts the shell, and keeps the View-menu
  full-screen title in sync. `RootView` (the solid-background + version-line animated
  view) is the `smoke` scenario's root. R9 still gives the shipped window Nice's
  **window chrome**: `window_options()` flips to a hidden (transparent) titlebar with
  the native traffic lights repositioned onto the y-26 row (`traffic_light_position` =
  the absolute close leading 17 — see the chrome-geometry divergence note above). The
  R9 band behaviour (drag past ~2pt → `start_window_move`; double-click →
  `titlebar_double_click`, both gated on `!is_fullscreen()`; the `ToggleFullScreen`
  action ⌃⌘F + a native View menu whose title flips via an `observe_window_bounds`
  callback) is now carried **inside the shell** by the toolbar band + the sidebar top
  strip, each replicating the same press-arbitration + drag-threshold + full-screen
  gate. **`WindowChromeView` is unchanged** — the R9 single-band chrome view — but is
  now mounted **only by the `chrome` self-test scenario**; the shipped window no
  longer wraps a lone terminal in it. The band's press-arbitration convention —
  interactive children (R10/R11) consume their own presses with `stop_propagation`,
  the band acts only on the remainder — is the reusable pattern the shell composes
  with.
- `app_shell` — the R13.5 **per-window composition root**. `AppShellView` renders the
  shell subtree (sidebar card + toolbar band + pane content), carries the window-level
  peek-clear modifier observer (moved off `WindowChromeView`), and observes the shared
  `WindowState`, the toolbar, and the sidebar — re-rendering the whole subtree on any
  notify, so a pill/row click (which notifies only its own view) still switches the
  `PaneHostView` sibling's content. `PaneHostView` is the pane-content host (the
  PROTECTED activation decision): it maps the active `(tab_id, pane_id)` →
  `SessionManager::pane_handle` → a per-pane, lazily-created, cached `TerminalView`
  (shared theme/accent/font + the same platform probe injections `open_managed_window`
  used), with activation flowing **only** through `SessionManager::activate_pane` (R13
  deferred-spawn + focus preserved verbatim, no view-side spawn), dropping a departed
  pane's view and re-pointing the demand-present kick to the active pane on every
  switch. The layout tree roots in `SidebarShellView` (it owns the collapse/peek/resize
  geometry) with the toolbar band + pane host threaded into its content slots, mirroring
  Swift's `AppShellView` expanded/collapsed layout — no ChromeEventRouter /
  LivePaneRegistry seam ports. **The composition invariant (PROTECTED):**
  `WindowState.model` is the ONLY `TabModel` a shipped window holds — `AppShellView` /
  `SidebarShellView` / `WindowToolbarView` / `PaneHostView` all render from and mutate
  that one shared state, so no mounted view carries a divergent model copy and every
  mutation flows through the `sidebar_actions` / `pane_strip_actions` / `session` seams.
  Exports the AX-anchor label constants `nice-rs-sidebar-root` / `nice-rs-pane-strip-root`
  (the §6 shipped-surface assertion hooks), placed as `.id()` + `.role(Group)` +
  `.aria_label(..)` on the sidebar-card root (`sidebar_shell`) and the pane-strip root
  (`toolbar`).
- `app_shell_live` — the R13.5 live app-shell composition self-test scenario
  (`app-shell`, see the table below). Opens through the SHIPPED builder
  (`open_managed_window` / `build_window_root`), not a hand-rolled root, so it can
  never drift from what `run` mounts; registered **before** `multiwindow` (it does not
  install the `WindowRegistry` close observer that scenario relies on being last).
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
  idle/occluded app), injected via `set_launch_deadline`. T2 adds one more FFI
  surface here — the AX-tree walk `ax_find_titled_role`
  (`AXUIElementCreateApplication` + a depth-/node-bounded `AXChildren` traversal
  reading `AXTitle`/`AXRole` via `AXUIElementCopyAttributeValue`) that the
  `ax-probe` self-test scenario calls, on the gpui main thread, to read **this
  process's own** macOS Accessibility tree and confirm AccessKit still exposes the
  tagged root (role + label, never identifier). R9 adds the window-chrome FFI:
  `standard_window_button_frames` reads the live close/minimize/zoom button frames
  (in content-view coords, y-from-top) so the `chrome` scenario asserts the REAL
  rendered traffic-light geometry, and a `chrome`-scenario validation block posts
  synthetic **mouse** CGEvents (down / drag / up / double-click, one-pid only,
  same safety invariant as the R5 keyboard block), reads the live NSWindow
  frame + zoom/miniaturize state, maps a content-view point to CG-global
  coordinates for the posted events, and reads (never writes)
  `AppleActionOnDoubleClick`.
- `input_live` — the R5 live input self-test scenarios (`input-live` /
  `input-shell`): real CGEvents posted to our own pid, byte-exact pty receipt,
  the item-4 candidate anchor, and the IME go/no-go probe (see the scenario
  table under "Self-test harness").
- `chrome_live` — the R9 live window-chrome self-test scenario (`chrome`): real
  mouse CGEvents drive the shipped `WindowChromeView` band + repositioned traffic
  lights + full-screen wiring, judged against AppKit frame/state reads — the
  traffic-light geometry (baseline + after resize / focus bounce / full-screen
  exit), the drag differential, the double-click vs `AppleActionOnDoubleClick`,
  and the full-screen toggle + View-menu title flip (see the scenario table).
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
- `theme` — the token → `gpui::Rgba` colour adapter (`srgba_to_rgba`,
  `slot_to_rgba`, `slot_srgba`, `srgba_with_alpha`) the R10/R11 chrome
  components (`status_dot`, `context_menu`, the sidebar) convert through, per
  the Layering rule (the adapter lives here, downstream of the gpui-free
  `nice-theme`). The R9 chrome band is not a caller — it still owns its own
  token → gpui adapter (`app::slot_rgba` in `app.rs`); this shared adapter
  serves only the R10/R11 components.
- **Sidebar (R10 sessions mode).** The sessions-mode sidebar — project groups,
  status dots, Finder-style multi-select, inline rename, the collapsed
  full-width band (M2 replaced the floating cap), the resize handle, the peek
  overlay, the mode/collapse toggles — over the R8 model.
  Its pure state ports gpui-free into `nice-model` (`selection` / `rename_gate` /
  `sidebar` — see that crate below); the views are GPUI-native here, and
  create/close/select actions bind to an injected seam that mutates the R8 model
  only (R13 rewires it to real sessions). **S7 drag-reorder is excluded, not
  missing — R25 owns it** (`SidebarDragState`, the drop delegates, the insertion
  line); absent drag support is by design, and a reviewer must not flag it.
  Modules:
  - `status_dot` — the `StatusDot` component (per-`TabStatus` colour + the
    ring/breathe pulse animations), which reads the R8 predicates
    (`Tab::status` / `Tab::waiting_acknowledged`) and never recomputes them;
    reused by R11's toolbar pills.
  - `context_menu` — the in-house context-menu popup (`anchored()` + `deferred()`
    + right-button open + click-away/Esc dismiss; the pinned gpui has no
    context-menu widget). Reused by R11.
  - `sidebar_actions` — the `SidebarActions` create/close/select seam (dossier
    G3): the single nameable surface R13 rewires. `ModelSidebarActions` is the
    R10 model-only impl (nothing spawns; no busy-pane confirmation — that is
    W5/R18); removal always goes through the single `TabModel::remove_tab` entry
    point so the parent-pointer sweep can't be skipped.
  - `sidebar_shell` — the `SidebarShellView` entity. **R13.5 (slice 1)** moved it off
    its private `TabModel` copy: it now holds the shared `Entity<WindowState>` + a
    `cx.observe` subscription and reads/mutates the shared model, sidebar
    (mode/collapse/peek), selection, and seam through it (the "one TabModel per window"
    invariant); it keeps only its own view-local render state (resize width, disclosure
    set, inline-rename draft, open menu). Two constructors: `new(state, cx)` (the
    isolated `sidebar` scenario — placeholder content, layout byte-identical to before)
    and **`new_composed(state, main_toolbar, main_body, cx)`** (the shell — the toolbar
    band + pane host injected as `AnyView`s into the top-bar-accessory + body slots of
    both shell modes, mirroring Swift's expanded/collapsed layout). Renders the whole
    shell (expanded floating card / collapsed full-width band — the M2 design; the
    floating cap is gone / peek overlay / resize handle) + card
    (project groups, tab rows, footer, toggles), and carries the exported
    `nice-rs-sidebar-root` AX anchor on the card root. The DO-NOT-PORT SwiftUI seams are
    replaced per the plan: the Esc `NSEvent` monitor → a `CollapseSidebarSelection` gpui
    key **binding** (dispatched before key listeners; collapses a >1 selection, else
    `cx.propagate()`s so Esc still reaches the terminal), and the rename click-away
    monitor → a `cx.on_blur` focus-out subscription.
  - `sidebar_live` — the R10 live sidebar self-test scenario (`sidebar`, see the
    table below).
- **Toolbar (R11 pane strip).** The window toolbar's pane strip — the brand
  block (logo + "Nice" + separator), the horizontally-scrolling row of pane
  pills (leading status dot / terminal glyph, truncating title, hover/active ✕
  with an always-reserved 16pt slot, active styling, inline rename, per-kind
  context menus), the overflow chevron with its 6pt attention badge, the 16pt
  edge fades, and the trailing `+` — all riding the R9 chrome band and driving
  the R8 model through an injected seam. The Swift `PaneStripOverflowEstimator`
  width-estimation machinery does **not** survive: GPUI reads real layout, so a
  tracked `ScrollHandle` drives overflow / fades / offscreen / auto-center
  directly (the pure predicates live in `nice-model`'s `strip_geometry`; R8's
  `Tab::has_offscreen_attention` is reused for the badge, never re-derived — one
  status model, dossier G2). The reservation rule that kills the
  show-chevron→shrink→hide feedback loop survives behaviorally: the chevron + `+`
  slots are unconditionally reserved, so the overflow decision never depends on
  the chevron's own visibility. **P4–P6 pill drag (reorder / cross-window /
  tear-off) is excluded — R25 owns it** (it adds drag on GPUI's own drag API,
  including the pure `PaneStripDropResolver`); **the trailing update pill (P7) is
  R27** (its slot stays empty). Absent drag support and the empty update-pill
  slot are by design; a reviewer must not flag them. Real session wiring for
  select/close/create is R13 (it rewires the seam without touching strip
  internals); OSC auto-titles reaching pills is R13/R15; busy-close confirmation
  is R18. Modules:
  - `inline_rename` — the shared inline-rename field (the char-by-char editor +
    caret + the pure `apply_rename_key` editing rule) extracted from the R10
    sidebar so the sidebar row and the toolbar pill mount the *same* field (R11's
    H2 pre-work); the rename *gate* stays R10's `InlineRenameClickGate`.
  - `pane_strip_actions` — the `PaneStripActions` select/close/add-terminal seam
    (the pane-level sibling of `SidebarActions`). `ModelPaneStripActions` is the
    R11 model-only impl (select moves `active_pane_id`; close routes through the
    single `TabModel::extract_pane`; add through the R8 `add_pane` "Terminal N"
    counter — nothing spawns until R13).
  - `toolbar` — the `WindowToolbarView` entity. **R13.5 (slice 1)** moved it off its
    private `TabModel` copy the same way: it now holds the shared `Entity<WindowState>`
    + a `cx.observe` subscription, renders the shared model's active-tab panes, and
    routes select/close/add/rename through the shared `pane_strip_actions` seam; it
    keeps only its own view-local state (the `ScrollHandle`, hovered pill, rename draft,
    open menu). Constructor `new(state, cx)`. Renders the whole strip and carries the
    exported `nice-rs-pane-strip-root` AX anchor on the strip root. Empty-submit pill
    rename resets to the per-kind auto-default via the R8 `rename_pane` (the pill
    reimplements no title policy).
  - `pane_strip_live` — the R11 live pane-strip self-test scenario (`pane-strip`,
    see the table below). Its in-process real-layout differentials (overflow
    onset, fades, badge, ✕-slot reservation, select/close/rename routing,
    centering) live in `nice-itests`' `pane_strip` cases — a simulated event
    can't move a real frame, and real Taffy layout is deterministic in-process.
- **Multi-window + shortcut dispatch (R12).** ⌘N opens a fully isolated window
  (its own tabs / panes / sidebar), a process-wide registry routes focused-window
  concerns, and the 13 default shortcuts dispatch through GPUI's action/keymap
  system with the terminal pass-through contract intact. The `WindowGroup` token
  dance, `NewWindowButton` UUID minting, the `WindowClaimLedger`, and the
  process-wide `KeyboardShortcutMonitor` `NSEvent` machinery are all DO-NOT-PORT:
  in GPUI the app calls `open_window` itself and hands each window its state as a
  **constructor argument**. Modules:
  - `window_state` — `WindowState`, the per-window composition root mirroring
    Swift's `AppState`: the R8 `TabModel` document + the R10 `SidebarModel` /
    `SidebarTabSelection` + the R10/R11 `SidebarActions` / `PaneStripActions`
    seams + the R13 `SessionManager` (in the slot R12 reserved) + a unique
    per-window session id. `WindowState::new(cwd)` mints a fresh default window;
    **R13.5 (slice 1)** factored out **`WindowState::with_model(model)`** — it seeds a
    window around a pre-built `TabModel` (re-syncing the selection from its active tab
    so the "selection ⊇ {active tab}" invariant holds), the seam the isolated `sidebar`
    / `pane-strip` scenarios mount the shipped views over (and R18 restore will reuse);
    `new` delegates to it. Handed to `app::build_window_root` — the seam R18 threads
    restored state and R25 an adopted pane through. `teardown` (a no-op in R12) is what
    the registry calls on close; R13 makes it drop the window's `SessionManager`
    sessions (SIGHUP→SIGKILL, no orphan zsh).
  - `session_manager` — `SessionManager`, the per-window pty/session subsystem
    (one per `WindowState`), the Rust twin of Swift's `SessionsModel` pane-lifecycle
    half. It wires the R3–R7 terminal stack (`nice_term_view::TerminalSessionHandle`
    entities) to the R8 `TabModel`: it owns the live pane sessions (tab-keyed
    `pane_id → session`, mirroring Swift's `ptySessions`), spawns deferred panes on
    focus, and routes each session entity's OSC title/cwd + exit events back into
    the model. **Pure model routing** (unit-tested, no gpui): `pane_cwd_changed`
    (OSC 7 → `Pane.cwd` only, never `Tab.cwd`), `pane_title_changed` (terminal-branch
    title policy — empty ignored, manual-rename lock respected, clip at 40; the
    Claude branch gated on `is_claude_running`, false all of R13, lands in R15),
    `set_active_pane` (active + ack-when-viewed), `select_next`/`prev_pane` +
    `step_active_pane` (wrap, <2-pane no-op), `add_pane` /
    `add_terminal_to_active_tab` (terminal-kind only — the ≤1-Claude creation edge),
    and `route_terminal_event` (map a decoded `TerminalEvent` to the routing call).
    **Lifecycle** (the exact Swift ordering): `pane_exited` — (1) clear overlay,
    (2) model removal + neighbor refocus, (3) pty release, then (5) the synchronous
    dissolve check, returning a `PaneExitResolution` so the live caller runs the
    two gpui-only side effects Swift runs inline (step-4 deferred-companion spawn on
    a surviving tab, and the every-project-empty terminus); `pane_held` flips the
    pane dead-but-mounted (`is_alive = false`, idle status, keep it in the strip).
    **Dissolve cascade** (`finalize_dissolved_tab`) — `remove_tab` + parent-pointer
    sweep → pty release → selection prune → active-tab fallback in
    `navigable_sidebar_tab_ids` order → the every-project-empty terminus; three
    entry points share it (pane-exit, `close_tab` = R10's action unconditional this
    cycle, and the unused-until-R25 cross-window `dissolve_tab_if_empty`).
    **Launch-overlay registry** (`register`/`promote`/`clear_pane_launch`, grace
    default `DEFAULT_LAUNCH_OVERLAY_GRACE`, `≤ 0` promotes synchronously) mirrors
    Swift's `paneLaunchStates`; the grace deadline arms R7's App-Nap-safe
    `LaunchDeadline`. **Termination** — `terminate_pane` (synthetic-held /
    synthetic-armed / live fast paths, always drops), `terminate_all` (snapshots ids
    first — held synthesized exits re-enter removal mid-loop), `teardown`. Test
    seams: injectable `mint_id`, `launch_overlay_grace`, and synthetic held/armed
    pane markers so close-flow tests build all three tri-state shapes (model-only /
    spawning / held) without a real child. The gpui composition primitives the
    action seams call — `spawn_pane`, `ensure_active_pane_spawned` (deferred spawn),
    `focus_active_pane`, `activate_pane` (the full `setActivePane`: model +
    deferred-spawn + focus), `pane_handle` (the slice-3 subscription seam a
    `cx.subscribe` reads to feed `route_terminal_event` from a live entity), and
    `apply_dissolve_terminus` (close-this-window-or-quit via the registry).
    **R14 shell-injection env** (`set_window_shell_env` / `spawn_pane`): a window's
    `SessionManager` carries a `WindowShellEnv` (socket path / `ZDOTDIR` /
    `NICE_USER_ZDOTDIR`), set once at construction before the Main pane forks;
    `spawn_pane` — the single choke point every pty spawn passes through — merges it
    plus per-pane `NICE_TAB_ID` / `NICE_PANE_ID` into `spec.env` **spec-wins**
    (`merge_env_spec_wins`: a key already on the caller-built spec, e.g. a blanked
    `ZDOTDIR`, survives), so the ~10 landed ZDOTDIR-blanked scenarios are untouched.
    `build_claude_extra_env` is the pure port of the Claude-pane env matrix
    (`TERM_PROGRAM` + ids + `NICE_SOCKET` always; `ZDOTDIR` + `NICE_USER_ZDOTDIR` +
    the frozen `NICE_PREFILL_COMMAND` only for `ResumeDeferred`) — production-unused
    until R15 wires it, its `settings_path` arg threaded now (always `None` until
    R17). Exported for later rows: the per-pane event routing R15 extends with
    status parsing, teardown/restore for R18, and the dissolve cascade's
    declared-but-inert R18/R19 subscriber hooks. The action-seam
    rewiring + the live `cx.subscribe` + the launch-deadline arming are exercised
    end-to-end by the `session-lifecycle` scenario (below); the OSC title/cwd events
    reach this manager through the three `nice-term-view` `TerminalEvent` variants
    R13 added (`TitleChanged` / `TitleReset` / `CwdChanged`, plain-typed — the
    boundary rule).
  - `window_registry` — `WindowRegistry`, the process-wide
    `WindowId → Entity<WindowState>` gpui global (the thin Rust port of Swift's
    `WindowRegistry`). `register` / `note_active` (MRU via
    `observe_window_activation` — its own list, since the pin's `window_stack()`
    is only a z-order assist) / the four-consumer lookup contract
    (`active_state(prefer_key)` = key window → most-recently-keyed → first;
    `state_for_window`; `state_for_session_id` for Stage-5 undo routing; `count`).
    A single `on_window_closed` observer deregisters, runs `WindowState::teardown`,
    and quits **only when the last window closes** — replacing the old
    unconditional quit-on-any-close so a multi-window app survives closing one of
    several windows. Registration bakes in **no** close-confirm behavior (that is
    R18's `on_window_should_close`).
  - `keymap` — the shortcut dispatch: 13 gpui `actions!` + key bindings generated
    from the gpui-free `nice_model::shortcuts` table (`ShortcutAction` +
    `default_bindings`), the Rust replacement for the `NSEvent` monitor. The
    dispatch-order split: font zoom (⌘=/⌘−/⌘0) + the deferred undo/redo register
    **app-level** (`cx.on_action`) so they fire with no Nice window key, fanning
    out through the hoisted process-level `FontSettings` (one entity every
    `TerminalView` observes — the plan's font fan-out); the 8 window-scoped actions
    (sidebar toggle/mode, sidebar-tab cycle, pane step, new pane, hidden-files)
    route through `WindowRegistry::active_state`. Three actions are declared
    deferred no-op markers with owners (`toggleHiddenFiles` → R19, undo/redo →
    R20) — consumed, not silently missing (a matched chord always leaks **zero**
    bytes to the pty). R9's ⌃⌘F folds into the same `bind_keys` call. `install_
    shortcuts` is idempotent (a process-level guard) so the self-test suite — which
    runs several keymap-installing scenarios in one process — registers the handlers
    once. **Documented divergence — character-based matching at the gpui pin:**
    Swift matched layout-independent physical `keyCode`s, but GPUI keymaps match on
    the produced key **character** (there is no keycode-binding API at the pin,
    verified). So the combos bind from the table's gpui key *tokens* with
    `use_key_equivalents` semantics (via `KeyBinding::load` + the app's
    `PlatformKeyboardMapper`, re-resolved on `on_keyboard_layout_change`); full
    layout-parity is R24's question (it owns rebinding). We do not patch gpui for
    this — a pin change is a human decision. The peek trigger's clear half
    (`on_window_modifiers_changed`) is the window-level modifier-release observer
    the shipped `WindowChromeView` installs.
  - `multiwindow` — the R12 live multi-window self-test scenario (`multiwindow`,
    see the table below). Its in-process isolation / routing / all-13-fire / peek
    **differentials** live in `nice-itests`' `multiwindow` cases (mirrors over the
    real `nice-model` types — a dev/test crate can't import the `nice` binary's
    `WindowState` / `WindowRegistry` / `keymap`, the same constraint the
    `chrome_band` / `sidebar_multiselect` / `pane_strip` cases carry).
  - `session_lifecycle` — the R13 live session-manager scenario
    (`session-lifecycle`, see the table below). Drives the real `SessionManager` on
    a real `WindowState` over real ptys, headless (no view — every assertion is
    model + session state, which `route_terminal_event` resolves in full). It holds
    the slice-3 action-seam wiring — the create-and-spawn / activate / project-`+`
    compositions the R10/R11 seams route through, and the live `cx.subscribe`
    (reading `SessionManager::pane_handle`) that feeds `route_terminal_event` from
    each pane's session entity. Registered **before** `multiwindow` (it installs no
    `WindowRegistry`, so it doesn't disturb the quit-when-empty close observer that
    scenario relies on being last).
  - `shell_inject` — the R14 synthetic `ZDOTDIR` rc chain (port of Swift
    `MainTerminalShellInject`). The four **FROZEN** stub bodies
    (`.zshenv`/`.zprofile`/`.zlogin`/`.zshrc`) as byte-for-byte `pub const`s — the
    `claude()` shadow (passthrough gates + `nc -U … -w 2` handshake + newtab/inplace
    dispatch), the `_nice_json_escape` dialect, the load-bearing OSC 7 `\%` escape,
    and the `print -z "$NICE_PREFILL_COMMAND"` tail — pinned by both static-text and
    real-`/bin/zsh` end-to-end tests (XDG / launchctl / login-shell chains). Plus
    `write_stubs` (self-healing atomic overwrite-always writer), the per-variant
    `default_location` (`<app support>/<CFBundleName>/zdotdir`, NOT `$TMPDIR`), and
    the `NICE_APPLICATION_SUPPORT_ROOT` override seam. The `app::run` bootstrap wires
    the writer (below); tests / scenarios call it against injected temp paths.
  - `control_socket` — the R14 per-window AF_UNIX control socket (port of Swift
    `NiceControlSocket`). Two-phase path mint (`$TMPDIR/nice-<pid>-<8hex>.sock` or a
    `NICE_SOCKET_PATH` override, minted before bind so it rides pty env), the
    `socket → unlink → bind → chmod 0600 → listen(8)` sequence (bind failure
    non-fatal — shells fall back to direct `claude`), a dedicated `poll()`-driven
    accept thread with per-client read timeout, and the complete **FROZEN**
    `SocketMessage` enum + parser (every normalization rule for
    `claude`/`session_update`/`handoff`). Self-heals accept-error / forced-cancel /
    missing-file into one capped-backoff rebind path; idempotent `stop()` unlinks.
    The consume-on-use `Reply` (owns the client `UnixStream`, at-most-once by move).
    The waker-based `mpsc` → gpui foreground-drain bridge (`socket_channel` /
    `SocketSender::post` / `SocketReceiver::readable`) fires a stored `Waker` +
    `CFRunLoopWakeUp` on every enqueue (App-Nap-safe — the `nc -w 2` reply deadline),
    never a coalescable timer. The window routing point + the three R15/R16/R26 stub
    handlers live on `window_state`; `app::arm_window_control_socket` mints + starts
    + drains + stores the socket (teardown stops it).
  - `tmp_sweep` — the R14 stale-`$TMPDIR` sweep (port of Swift
    `NiceServices.cleanupStaleTempFiles`). The pure `temp_file_decision` classifier
    (`nice-zdotdir-<pid>` dirs + `nice-<pid>-<uuid8>.sock` sockets, pid parsed from
    the name) with an injected `kill(pid,0)` liveness probe — a live sibling app's
    debris is kept (`EPERM` counts as alive), only a crashed run's is reaped. Run
    once from the `app::run` bootstrap, before the first window's socket is minted.
  - `shell_socket_live` — the R14 live shell-injection + control-socket transport
    scenario (`shell-socket`, see the table below). Headless (its own RAF root, no
    view assertions); registered **before** `multiwindow` (it installs no
    `WindowRegistry`). Reuses `app::arm_window_control_socket` — the exact production
    wiring — so a socket / env-injection regression surfaces here.
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

`Tab.branch` (vestigial, roadmap M5) is deliberately **not** ported here.

**Sidebar UI state (R10 pure ports).** Three more gpui-free value-state modules
the R10 sidebar builds over, ported case-for-case from the pure-Swift seams and
unit-tested exactly like the tree above (R11 reuses the rename gate; R12
dispatches into the sidebar + selection; R13 prunes the selection in the
dissolve cascade):

- `selection` — `SidebarTabSelection`, the Finder-style multi-select model and
  the "selection ⊇ {active_tab_id}" invariant (⌘-click on the only-and-active
  row refused; ⇧ keeps the original anchor; the right-click snap policy; prune
  on removal).
- `rename_gate` — `InlineRenameClickGate`, the injected-clock click-to-rename
  time gate (edit iff `now − activated_at ≥ interval`, `>=` boundary).
- `sidebar` — `SidebarModel` (+ `SidebarMode`): collapsed/mode/peek state and
  the toggle + peek render/clear methods. `SidebarMode` carries serde derives
  for R18 persistence + Swift `Codable` parity; the `SceneStorage` bridge stays
  view-layer.

**Keyboard-shortcut data (R12 pure port).** `shortcuts` — `ShortcutAction` (the
closed 13-action user-rebindable set) + `default_bindings` (the default-combo
table as data), ported from `KeyboardShortcuts.swift`. Gpui-free: R12's `keymap`
slice in `crates/nice` generates the `actions!` / `bind_keys` wiring from this
table via `KeyCombo::chord_str` (the canonical gpui keystroke string), and R24's
rebinding UI consumes the same data. Combos are a modifier set + a gpui key
*token* — **character-token based, not physical-keycode based** (the documented
divergence from Swift's layout-independent `keyCode` match; there is no
keycode-binding API at the gpui pin). Window-management accelerators that are not
rebindable (New Window ⌘N, Toggle Full Screen ⌃⌘F) are deliberately absent from
this table — they live as fixed actions in `crates/nice`.

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
  `reference_traffic_light_geometry_macos26` instead of a Swift line — the only
  place a citation points somewhere other than a Swift source line. R9 makes
  `MACOS26_TRAFFIC_LIGHT_LEADINGS[0]` (the close leading) **load-bearing**: GPUI
  takes an *absolute* close-button origin rather than reading each button's live
  default and adding a nudge (Swift's captured-default-plus-8), so the shipped
  close-x is `[0] + TRAFFIC_LIGHT_NUDGE_X` = 17 (`crates/nice`'s
  `window_options`). The other leadings + the pitch stay documentary
  sanity-check values (GPUI derives minimize/zoom x and preserves the pitch
  itself); the `chrome` live scenario asserts the *rendered* geometry from
  `standard_window_button_frames()`, so a future OS shift surfaces as a token
  change plus a live-scenario failure, not silent drift.

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
| `ax-probe` | The T2 AccessKit-wired canary (see "The AX decision record" in `../docs/testing.md`). Tags one stable root element (`AxProbeView`, id `ax-probe-root`, role `Group`, aria_label `nice-rs-ax-probe-root`) and walks **this process's own** macOS AX tree via `crate::platform::ax_find_titled_role` (`AXUIElementCreateApplication` + a bounded `AXChildren`/`AXTitle`/`AXRole` traversal) to assert the node is exposed with role `AXGroup` — **role + label matching only, never identifier matching** (gpui never sets `author_id`, so `AXIdentifier` matching is unreachable without a vendor patch). Polls until AccessKit (lazily activated by the first AX query, run on the gpui main thread so it doesn't race gpui's per-frame `RefCell` borrow) surfaces the node. A canary that AccessKit stays wired as gpui evolves across pin bumps — **not** an a11y test suite, and not a general-purpose black-box matcher to build chrome/pane tests on. `Gate::SelfReported`. |
| `chrome` | The R9 live window-chrome gate (Validation §1–§4). Opens the R9 chrome band (`WindowChromeView`) + repositioned native traffic lights + full-screen wiring over a silent live pane and drives it with **real mouse CGEvents** to nice-rs's own pid, ground-truthed against AppKit reads. **§1** — via `platform::standard_window_button_frames`, asserts all three buttons exist, the close button's visual centre sits on the y-26 row and its x-origin at 17, and the three are equally pitched (pitch read from the live frames), **re-asserted after a resize, a focus bounce, and a full-screen enter+exit** (the BUG-B stale-capture guard). **§2** — a CGEvent press-drag on the empty band vs the terminal content area, judged by real NSWindow frame reads (the content drag must leave the window put). **§3** — reads (never writes) `AppleActionOnDoubleClick`, posts a CGEvent double-click on the band, and checks the window state matches the predicted zoom / miniaturize / none, plus a double-click while full screen is a no-op (the band's `!is_fullscreen` gate). **§4** — dispatches `ToggleFullScreen` and asserts `is_fullscreen()` + the View-menu title flip, both ways. Preflights `AXIsProcessTrusted()` and FAILs loudly if the grant is missing. Effects a synthetic CGEvent provably can't drive (a window drag via `performWindowDragWithEvent:` follows the *physical* cursor, which `CGEventPostToPid` doesn't move) are recorded as a **DEFERRED HUMAN PASS**, not fail-looped — the same honest-deferral pattern `input-live` uses for synthetic IME composition. `Gate::SelfReported`. |
| `pane-strip` | The R11 live toolbar pane-strip gate (Validation §3). Mounts the real `WindowToolbarView` over a seeded Main tab and drives it with **real mouse CGEvents** to nice-rs's own pid, ground-truthed against AppKit frame reads. Asserts the drag differential with pills present — a CGEvent press-drag starting on a pill **selects** the pill AND leaves the NSWindow frame **put** (hard-asserted only when the select confirms the synthetic press LANDED, else DEFERRED — a `CGEventPostToPid` mouse event need not land on a gpui hitbox), while the same drag on the empty toolbar band **moves** the window (DEFERRED — `performWindowDragWithEvent:` tracks the physical cursor `CGEventPostToPid` doesn't move) — plus the reserved-width overflow **showing the chevron** on a real window (hard, real layout), an **activate-from-elsewhere** that makes an offscreen pane active (hard) and auto-centers it into view (DEFERRED on repaint timing), and the **overflow menu opening** on a real chevron click (DEFERRED on a synthetic miss). Preflights `AXIsProcessTrusted()` and FAILs loudly if the grant is missing — the same honest-deferral for synthetic mouse gestures `chrome` / `sidebar` use. The in-process overflow-onset / edge-fades / attention-badge / ✕-slot-reservation / select-close-rename / centering **real-layout** differentials live in `nice-itests`' `pane_strip` cases (a simulated event can't move a real frame; real Taffy layout is deterministic in-process). `Gate::SelfReported`. |
| `session-lifecycle` | The R13 live session-manager gate (Validation §4). Drives the real per-window `SessionManager` on a real `WindowState` over **real ptys**, headless (no `TerminalView` — every assertion is model + session state, which `route_terminal_event` resolves in full; the two gpui-only side effects the pane-exit resolution carries, the deferred-companion spawn on refocus and the every-project-empty terminus, are composed by the live window root, and the scenario is built so the terminus stays `None` and no refocus lands on an unspawned companion). Asserts the six lifecycle behaviours Milestone 2 rests on: **immediate explicit-add spawn** — the `Terminals +` / ⌘T create-and-spawn path and the strip `+` (`add_terminal_to_active_tab`) path each fork their pty **synchronously** (Swift `addPane` semantics — an explicit add is never deferred); **deferred companion spawn on focus** — the project `+` seam builds the `[Claude, Terminal 1]` shape + registers the tab's session container but spawns **neither** pane (Claude = model-only until R15, companion = deferred), and selecting the companion forks its pty on that first focus via `ensure_active_pane_spawned`; **clean-exit neighbor refocus** — a shell `exit 0` (not held) removes the pane and re-points the active pane to the slot neighbor through the live `Exited{held:false}` subscription; **last-pane dissolve + Terminals-order fallback** — exiting the tab's last pane dissolves the tab and the active-tab selection falls back to the first navigable tab (the pinned Terminals group's Main tab); **held detour** — a `sh -c 'echo FINAL; exit 3'` pane exits non-zero, so the `Exited{held:true}` subscription flips it dead-but-mounted (`is_alive == false`, still in the strip) rather than removing it; and **orphan sweep** — `WindowState::teardown` drops every session (SIGHUP→SIGKILL), so no zsh survives (asserted externally by `ps`, the R3 teardown contract). The action-seam rewiring (create-and-spawn / activate / close+dissolve) and the live `cx.subscribe` that feeds `route_terminal_event` from each pane's session entity (via `SessionManager::pane_handle`) are the slice-3 wiring this exercises. Fixture shells poll the grid for a `READY` marker before the driver triggers their exit (never sleep-and-hope, per the ZDOTDIR-blanked-shell rule). Needs **no** Accessibility grant (it drives the manager directly, not via CGEvents). Registered before `multiwindow` (it installs no `WindowRegistry`). `Gate::SelfReported`. |
| `app-shell` | The R13.5 app-shell composition gate (What-to-build #3). Opens through the **shipped builder** (`crate::app::open_managed_window` / `build_window_root` — the exact path `run` and every ⌘N take, not a hand-rolled root: a scenario mounting its own composition would re-create the blind spot R13.5 closes) and asserts the mounted shell over ONE shared `WindowState`. **The AX anchors are exposed** — an AX-tree walk (`ax_find_titled_role`, the `ax-probe` pattern) finds the sidebar-card root (`nice-rs-sidebar-root`) and the pane-strip root (`nice-rs-pane-strip-root`) each as an `AXGroup`; the poll forces a repaint per tick (a `WindowState` notify) because the shipped shell doesn't RAF, keeping AccessKit's lazily-activated tree current. **⌘T adds a visible pill AND switches pane content** — a real ⌘T CGEvent (`CGEventPostToPid`, own pid) routes through the shipped keymap to the key window: the toolbar gains one *laid-out* pill, the new pane becomes active, and the `PaneHostView` follows the switch and spawns+hosts its pty (proving the slice-2 `cx.notify()` wiring makes a window-scoped chord produce a visible result in the shipped shell). **The strip `+` spawns a real pty whose output renders** — the real toolbar `+` seam adds a terminal pane, the pane host spawns its login shell, and a marker echoed into that pty renders back in the pane's live grid. **Closing the extra pane refocuses a neighbor** — the real pill-× close removes the active extra pane from the model, the active pane refocuses to a surviving neighbor, and the pane host re-hosts it (the departed pane's view is dissolved from the composition). **⌘B collapses/expands the card** — a real ⌘B CGEvent (the R12 table binds *toggle-sidebar* to `cmd-b`; the plan's "⌘S" for this step predates that table) collapses the card and its intended leading-column width drops 240 → 0 (the M2 collapsed design reserves no leading column; `SidebarShellView::scenario_leading_column_width`, re-derived from the collapse flag — not a laid-out `Bounds` read), a second ⌘B restores it. **Teardown releases every session; the closed pane's pty is reaped** — `WindowState::teardown` clears the SessionManager's session map (asserted: every session released), and SIGHUP→SIGKILLs (via `PtyProcess::drop`, which joins the reaper — no zombie) any pane whose handle it held the *last* ref to: the closed pane, whose cached `TerminalView` the pane host already dropped, is reaped here (asserted: `kill(pid, 0)` → ESRCH). The still-*hosted* panes keep a `TerminalView` ref in the mounted `PaneHostView`, so their pty's final reap lands on window close (dropping the shell view tree) — confirmed by the external `ps` sweep, per the R3 teardown contract (reaping a view-hosted pane inside the still-open window is not possible; the assertion says so honestly). Preflights `AXIsProcessTrusted()` and FAILs loudly if the grant is missing (a dropped CGEvent would make ⌘T / ⌘B no-ops). Registered **before** `multiwindow`: it does not install the `WindowRegistry` close observer (its `build_window_root` only `register`s, via `default_global`), so closing its window never trips the quit-when-empty terminus `multiwindow` — which DOES install it — relies on being last. `Gate::SelfReported`. |
| `shell-socket` | The R14 shell-injection + control-socket **transport** gate (Validation §4). Spawns real `zsh -il` login shells through the live spawn path (`SessionManager::spawn_pane`) with the window's manager env injection active — the synthetic `ZDOTDIR` rc chain (written by the R14 stub writer directly against a temp dir) + per-pane `NICE_SOCKET` / `NICE_TAB_ID` / `NICE_PANE_ID` — over a fully sandboxed fixture (fake `$HOME` + marker `.zshrc`, a stub `claude` on `PATH` also exported as `NICE_CLAUDE_OVERRIDE`, a temp `ZDOTDIR`). Asserts **transport only** (never a handler's decision, so it survives R15 replacing the `claude` stub body): **chain-back** — the login shell restores the user `ZDOTDIR` and sources the fixture `~/.zshrc` (polls the grid for `USER_RC_RAN`); **`claude --help` bypass** — the shadow's non-interactive short-circuit runs the stub `claude` (grid shows its argv echo) and sends NO socket message; **`claude` handshake** — a bare `claude` handshakes over `NICE_SOCKET` and the window routing point records a `claude` message carrying the pane's exact injected `tabId`/`paneId` + its `cwd`, with a raw-`UnixStream` probe confirming exactly ONE newline-terminated reply line (the `Reply` one-line contract over the wire); **raw `session_update`** — a raw-`UnixStream` `session_update` line surfaces at the routing point parsed + normalized (the headless app-level driver TRANCHE-2-NOTES §1 asks for); **prefill** — a pane spawned with `NICE_PREFILL_COMMAND` in its spec env shows the pre-typed command via the stub's `print -z` tail and its side effect never runs (proof nothing executed); **self-heal** — deleting the socket file autonomously rebinds it at the same path (the health `stat()`, shortened here); **teardown** — `WindowState::teardown` unlinks the socket file. Grid-poll readiness with bounded fail-loud timeouts (never sleep-and-hope). Never launches the machine's real `claude`, never writes the real `~` / Application Support. Needs **no** Accessibility grant (raw sockets + pty writes, not CGEvents). Reuses `app::arm_window_control_socket` (the production wiring). Registered **before** `multiwindow` (it installs no `WindowRegistry`). `Gate::SelfReported`. |
| `multiwindow` | The R12 live multi-window + shortcut-dispatch gate (Validation §2–§5). Drives the shipped `WindowRegistry` / `WindowState` / `keymap` on **real `NSWindow`s** with **real CGEvents** to nice-rs's own pid. Opens window A as a capture-tee managed window (the `input-live` pattern) registered in the process-wide registry, then asserts: **⌘N** opens a second, isolated, registry-tracked window (the registry count **and** `App::windows()` both step 1 → 2); **⌘T** posted while window B is key adds a pane to B's `WindowState` model only, leaving A's model signature unchanged (isolation + focused-window routing through `active_state`); **⌘=** grows the one process-level `FontSettings` every window observes (the font fan-out) and leaks **zero** bytes into A's capture-tee pty; the **pass-through differential** — a plain `x` reaches the pty as `x`, while **⌘⌥↓** cycles the sidebar and leaks **zero** capture bytes (a matched chord is consumed, an unmatched key falls through byte-identically); **live peek** — with A's sidebar collapsed, ⌘⌥↓ floats the peek and a modifiers-release clears it via the window-level `on_modifiers_changed` observer; and **close/deregister/fallback** — closing B deregisters it (registry + `NSWindow` count drop) and a window-scoped action then falls back to the surviving window A. Matching is **character-based** at the gpui pin (the documented divergence from Swift's physical-keycode match — see the `keymap` module notes). Preflights `AXIsProcessTrusted()` and FAILs loudly if the grant is missing (a dropped CGEvent would make every chord a no-op). The per-pid flagsChanged the peek-clear needs is not synthesizable via `CGEventPostToPid`, so the modifier release is driven as a real `ModifiersChangedEvent` through GPUI's own dispatch (the same `on_modifiers_changed` path). The in-process isolation / routing / all-13-fire / peek **differentials** live in `nice-itests`' `multiwindow` cases. Registered **last** in `selftest_scenarios` (it installs the registry whose close observer quits when the registry empties). `Gate::SelfReported`. |
| `sidebar` | The R10 live sessions-sidebar gate (Validation §3–§4). Mounts the real `SidebarShellView` (no pty — the shell hosts no terminal this cycle) and drives it with **real mouse CGEvents** to nice-rs's own pid, ground-truthed against AppKit reads. Asserts the expanded card reports the **240pt** default width; a CGEvent drag on the trailing resize handle **clamps at 160 and 480** and a CGEvent double-click **resets to 240**; **collapse** removes the leading column entirely (the M2 design: no cap card; `scenario_leading_column_width` reports 0) and the full-width band's drift guards hold — the 82pt traffic-light spacer clears the LIVE zoom button's trailing edge, the bare restore button's rect has **zero x-overlap** with any traffic light, and R9's close-x / y-26 / equal-pitch geometry is **re-asserted** (`standard_window_button_frames`, the BUG-B guard); **restore** returns the column; a CGEvent drag on the sidebar top strip moves the window (R9 band pattern) while the **same drag inside the card body leaves the frame put** (hard). **§4 dots** — with the model driven into all four states (thinking / waiting-unacked / waiting-acked / idle), the dot colour per token and the pulse-presence rule are asserted at the state level off the view's own R8 predicates (`SidebarShellView::tab_dot_inputs`; pixel corroboration is best-effort under `capture`). Preflights `AXIsProcessTrusted()` and FAILs loudly if the grant is missing; the resize clamps/reset and the strip window-move hard-assert when the synthetic gesture drives the real behaviour, else **DEFER** (the 6pt resize handle a synthetic press may miss; the `performWindowDragWithEvent:` physical-cursor limitation), the same honest-deferral pattern `chrome` uses. The in-process multi-select / rename-gate / Esc / band-arm **classification** differentials live in `nice-itests`' `sidebar_multiselect` cases (a simulated event can't move a real frame). `Gate::SelfReported`. |

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
(their pass criterion is byte-exact pty receipt from posted CGEvents, not cadence),
as does the R9 `chrome` scenario (its criterion is AppKit frame/geometry/menu
state after posted gestures, not cadence — and it self-activates + preflights the
Accessibility grant like the other CGEvent scenarios).
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
