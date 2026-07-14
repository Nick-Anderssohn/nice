# Nice — Known Bugs (priority order)

Post-Rust-rewrite bug hunt. Each entry was found by static review and then **independently verified** by an adversarial reviewer whose default stance was to refute it; all entries below survived that pass (severities/mechanisms were corrected during verification where noted). Full per-bug write-ups (mechanism, byte/line traces, verdicts) live in the scratchpad findings dir referenced at the bottom.

Severity scale: **critical** = crash/SIGABRT/data loss/whole-app freeze · **high** = feature broken or wrong persisted state in realistic use · **medium** = edge-case misbehavior, leaks · **low** = minor correctness.

Status legend: 🔴 open · 🟡 open (family/tracked together) · ⚪ open (low)

> **Fix round 1 (2026-07-13, dev-cycle run `bughunt-round1`):** #1–#4, #8, and #13
> are FIXED and merged to main — see the "Recently fixed" table for their SHAs.
>
> **Fix round 2 (2026-07-13, hand fixes + per-plan subagent implementers):** #5–#7
> and the six user-reported bugs from the vault list (scrolled-selection drift,
> cursor-glyph occlusion, rename select-all + caps lock, drag-ghost offset,
> option+arrow word-skip) are FIXED and merged to main — see "Recently fixed".
> Remaining open: #9–#12, #14–#17.

---

## High

_None open — all four HIGH bugs (#1–#4) fixed in round 1; see "Recently fixed"._

---

## Medium

### 9. 🔴 Toolbar pane pills never track hover — dead highlight, inactive pills' close "×" is unreachable `[MEDIUM]`
- **Where:** `crates/nice/src/toolbar.rs:381` (field `hovered_pane_id`), read at `:529/:1324/:1329`.
- **What:** `hovered_pane_id` is declared, initialized `None`, and read to compute `is_hovered` — but **never written** (no `on_hover`/`.hover()` on the pill body). So `is_hovered` is permanently `false`: the hover-background tier is dead code, and `show_close = is_hovered || is_active` collapses to `is_active`, so `render_close_button` is called with `visible=false` for every inactive pill (rendered at `opacity(0.0)` with no click handler). The hover-to-close affordance and hover highlight are dead on all non-active pills. Sidebar rows wire hover correctly, confirming a dropped wire in the rewrite.
- **Repro:** Tab with >1 pane → hover a non-active pill → no highlight, no "×". Close requires activating it first or right-click → Close.
- **Fix:** Add `on_hover`/`.hover()` to the pill setting `hovered_pane_id` + `cx.notify()`, mirroring the sidebar; or apply the pill background and reveal the "×" via gpui `.hover()`/group-hover.
- _findings: uishell-1_

### 10. 🔴 Sidebar Esc action swallows Esc over an open context menu / pane-rename when a multi-selection is active `[MEDIUM]`
- **Where:** `crates/nice/src/sidebar_shell.rs:1003` (`on_collapse_esc`), binding at `:2483`.
- **What:** The shell binds `escape → CollapseSidebarSelection` in the `SidebarShell` context (an ancestor of the context menu and composed toolbar). `on_collapse_esc` propagates Esc only for a single selection with no *sidebar* tab-rename in flight; for a **>1 selection it consumes** Esc, checking only `editing_tab_id` — never whether a context menu or the *toolbar* pane-rename field is open. Verified against pinned gpui: `dispatch_key_event` runs matched action bindings *before* descendant `on_key_down`, so the ancestor action pre-empts the menu's / rename field's own Esc handler.
- **Repro:** Multi-select 3 sidebar tabs → right-click a selected row (menu shows "Close 3 Tabs", selection intact) → Esc collapses the selection and leaves the menu open. Same with an in-flight toolbar pane rename.
- **Fix:** In `on_collapse_esc`, short-circuit to `cx.propagate()` when `self.context_menu.is_some()` or a toolbar pane rename is in flight.
- _findings: uishell-2_

---

## Low

### 11. ⚪ Quit/window-close confirmations clobber an existing pending modal `[LOW]`
- **Where:** `crates/nice/src/app.rs:897/742` (`request_window_close`/`request_quit`) call `present_confirmation` with no `pending_modal()` guard, unlike the R20.5 busy-close gates.
- **What:** `present_confirmation` unconditionally overwrites `pending_modal`/`modal_sub`. Pressing ⌘W while a busy-pane "Force quit" modal is up (or ⌘W while the ⌘Q dialog is up) drops the earlier modal's completion un-run — its "runs exactly once" contract runs zero times. No state corruption (dropping a completion == a cancel), so low; a UX surprise only.
- **Fix:** Give both W5 paths the same `pending_modal()` guard, or resolve the existing modal as cancelled first.
- _findings: deep-4_

### 12. ⚪ `WindowState::teardown` leaves the pending modal installed → entity-reference-cycle leak `[LOW]`
- **Where:** `crates/nice/src/window_state.rs:1712` (`teardown`).
- **What:** `present_confirmation` justifies capturing the window's raw `NSView` pointer with a comment claiming "teardown drops the subscription instead of emitting" — but `teardown` never clears `pending_modal`/`modal_sub`. The modal↔`WindowState` completion closure form an Entity reference cycle (gpui has no cycle collection), broken only by the `DismissEvent` path. **Reachable** (verifier corrected "latent"): a state-capturing close modal up on a window that is then emptied by busy-pane self-exit (with a second window open) leaks one `WindowState` + modal + subscription for the process lifetime. The use-after-free leg the finder feared is *not* reachable (no emitter resolves the un-rendered modal).
- **Fix:** Set `self.pending_modal = None; self.modal_sub = None;` at the top of `teardown()` — honors the comment and breaks the cycle on every close path.
- _findings: deep-5_

### 14. ⚪ Imported-theme boot enumeration doesn't dedup by slug `[LOW]`
- **Where:** `crates/nice/src/terminal_theme_catalog.rs:304` (`enumerate`).
- **What:** `enumerate` pushes one entry per file keyed by `slug(stem)` with no dedup, while `import_theme` dedups via `retain`. Two files that slug identically (e.g. a hand-placed `cool-theme.conf` alongside Nice's `cool-theme.ghostty`) yield two catalog entries with the same id → duplicate picker rows, **duplicate a11y ids** in one AccessKit tree, and a one-click "Remove" that deletes *both* backing files. Requires a hand-placed second file.
- **Fix:** Dedup by id in `enumerate` before sorting, mirroring `import_theme`.
- _findings: platform-2_

### 15. ⚪ Combining marks / ZWJ-emoji sequences are dropped from every rendered cell `[LOW]`
- **Where:** `crates/nice-term-view/src/element.rs:1606` (`fill_row`).
- **What:** `PaintCell` is built from `cell.c` alone and never reads alacritty's per-cell `zerowidth()` attachment list, so NFD combining diacritics, emoji variation selectors, skin-tone modifiers, and ZWJ continuations are discarded before shaping. `café` (NFD) renders `cafe`; `👍🏽` renders default-yellow `👍`; a family emoji renders a lone `👨`. Pure render-fidelity loss — the grid retains correct codepoints, no data loss.
- **Fix:** Give `PaintCell` the base char + its zerowidth attachments and emit the full grapheme in `plan_row` for isolated single-cell runs.
- _findings: render-1_

### 16. ⚪ OSC 7 accepts a cwd with interior NUL / control bytes `[LOW]`
- **Where:** `crates/nice-term-core/src/osc7.rs:189` (`parse_osc7_payload`).
- **What:** After percent-decoding, the only validation is non-emptiness. `%00` decodes to a real `0x00` byte and is emitted as `CwdChanged("/a\0b")`, stored into `pane.cwd`. A later respawn at that cwd hits `cstr()` (`CString::new`), which returns an `io::Error` (graceful spawn failure, not a crash). A single hostile `ESC]7;file:///a%00b BEL` taints the tracked cwd.
- **Fix:** Reject a decoded path containing NUL (and optionally other C0 controls) → return `None`, matching how every other malformed OSC 7 is dropped.
- _findings: termcore-3_

### 17. ⚪ Claude pane spawned when the `claude` binary is unresolved carries no `NICE_TAB_ID`, so the reaper can never reap it `[LOW]`
- **Where:** `crates/nice/src/session_manager.rs:1595` (probe-unresolved arm → bare `SpawnSpec::shell(cwd)` via `spawn_session_raw`).
- **What:** The orphan reaper only SIGKILLs a reparented zsh whose env contains `NICE_TAB_ID=`. Every normal pane injects it, but the probe-unresolved Claude fallback spawns a bare shell with no env and bypasses window injection, so that zsh has no `NICE_TAB_ID`. After a Nice crash it reparents to launchd and the reaper skips it forever → pty slot leaks (contributes to the `ptmx_max` starvation the reaper exists to prevent). Compound-rare: needs `claude` unresolved at spawn *and* a Nice crash.
- **Fix:** Inject at least `NICE_TAB_ID`/`NICE_PANE_ID` even on the probe-unresolved fallback (route through `spawn_pane` / merge `window_pane_env_pairs`).
- _findings: sessions-4_

---

## Automated checks (this hunt)

- **cargo build `--workspace --tests`:** ✅ clean.
- **cargo test `--workspace`:** ✅ **1678 passed, 0 failed, 0 ignored** across 21 test binaries. The existing suite is green — none of the bugs above are caught by current tests (each write-up notes the coverage gap).
- **cargo clippy `--workspace --all-targets`:** 39 lints, **no real correctness bug**. All are style/pedantic (`useless_conversion`, `manual_repeat_n`, `doc_lazy_continuation`, etc.). Two looked correctness-relevant and were checked out:
  - `non_canonical_partial_ord_impl` on `SemanticVersion` (`crates/nice-model/src/semantic_version.rs:82`) — **false alarm**: `Ord::cmp`, `partial_cmp`, and `PartialEq::eq` all delegate to the same `compare()` helper, so they agree; the version-compare logic is correct.
  - `reversed_empty_ranges` deny-level **error** at `crates/nice-term-input/src/ime_state.rs:350` (`set_marked_text(Some(99..1), …)`) — **intentional test fixture** (`// reversed+overlong range`, exercising clamping), not a bug. ⚠️ *Minor CI hygiene note:* because it's deny-by-default, `cargo clippy` currently exits non-zero on the workspace — if clippy is ever gated in CI, this test line needs an `#[allow(clippy::reversed_empty_ranges)]`.

## Areas audited clean

- **Control socket / shell inject / installers** (`control_socket.rs`, `shell_inject.rs`, `claude_hook_installer.rs`, `skill_installer.rs`, `settings_import.rs`, `claude_theme_sync.rs`, `keymap.rs`, `release_check/`) — swept twice (first pass + a skeptical second-pass reviewer), **no bug found**. Specifically confirmed guarded: NDJSON framing drops partial/oversized/invalid-UTF-8 frames without panicking and never leaks the accepted fd; the per-window socket path self-heals stale files and rebinds a swept socket at the same path; ZDOTDIR path quoting goes through `_nice_json_escape` and is real-zsh e2e-tested; both `~/.claude` installers are idempotent (only-if-changed byte compare), preserve foreign hooks/user content, refuse to clobber a non-object/unparseable `settings.json`, and write atomically (temp+rename); `settings_import` is a one-shot fail-soft gate whose emitted shortcut tokens round-trip through `OwnedCombo::from_token`; `claude_theme_sync` uses atomic rename + a `_niceManaged` marker guard; `keymap` rebuild/dispatch are single-foreground-thread (no race) and the protected non-rebindable set is complete; `SemanticVersion` compares component-wise with zero-padding and treats unparseable as "no pill". _(Note: the one keymap edge that IS broken — recorder stand-down not restored on Settings-window **close** — lives in `shortcuts_pane.rs` + the Settings close plumbing, not `keymap.rs`, and is captured as bug #1.)_

---

## Bug-pattern watchlist (derived from recently fixed + newly found bugs)

Classes with at least one confirmed member since the rewrite — new code should be checked against these:

1. **"Registered windows ≠ all windows"** — lifecycle checks that resolve/gate on `WindowRegistry` treat the unregistered Settings window as absent → quit-confirmation bypass (#4), quit-with-Settings-open (#13). Fixed: f197b35.
2. **Mutation site missing its `save_to_store()`** — because `on_tree_mutation` was wired nowhere, every model mutation had to save at its site; several didn't (#8). Fixed systemically (observer wired to the debounced save): b08e991.
3. **gpui entity double-lease SIGABRT** — synchronously re-entering a leased entity inside a subscription. Fixed: 908f217.
4. **Quit/teardown races** — async events after a latch (`AppQuitting`/`user_initiated_close`) re-mutating frozen state. Fixed: d4ab1b8, 91b6f7f.
5. **Cross-thread channel ordering** — consumers assuming order across producer threads. Fixed: 9072144.
6. **Wake/drain starvation** — edge-gated wakes that swallow the only signal. Fixed: ec0b8f3.
7. **Presentation gating** — `cx.notify()` doesn't paint while a window's CVDisplayLink is stopped. Fixed: ec0b8f3, dcb7670.
8. **Synchronous work on the main thread** — teardown grace waits + unbounded joins freeze the UI (#6). Fixed: 9da1426.
9. **Single-pass sanitizers** — non-re-scanning removal fuses neighbours into a new marker (#5). Fixed: 2ef4044.
10. **Forward-guard without inverse-guard** — `apply` guards clobber, `undo` doesn't (#2). Fixed: e01a08e.
11. **Focus/selection off-by-ones & orphaned handles.** Fixed: 0ae0744, 4616768, 0108a6c, 7a44c17, 1008500.

## Recently fixed (do not re-report)

| Commit | Bug |
|---|---|
| 2ef4044 | **#5** bracketed-paste marker-overlap reconstruction: `strip_end_marker` now iterates to a fixed point |
| 9da1426 | **#6 + #7** pty teardown off the main thread (SIGHUP + detached SIGKILL escalation, finished-only joins, janitor holds the master fd for the feeder) + reaper-spawn-failure child cleanup |
| e589e0f | **#7 follow-up**: the cleanup's `killpg` raced the child's `setsid` (lost SIGKILL → `waitpid` hang); now also `kill(pid)` directly |
| 4484af1 | user-reported: legacy Option+←/→ printed `D`/`C`; now send `ESC b`/`ESC f` word-skip (kitty mode unchanged) |
| 63b6080 | user-reported: drag-selection anchor drifted downward while scrolled up with streaming output; anchor now tracks the viewport row |
| 99eef37 | user-reported: solid block cursor hid the glyph; now reverse-video over the accent with a WCAG contrast fallback |
| cdb4360 | 99eef37 follow-up: cell-bg glyph read muddy on a mid-tone accent; glyph is now always max-contrast black/white |
| 9b0666d | user-reported: tab/pill rename now selects the whole title on entry; all three rename surfaces respect Caps Lock |
| 52bf3fc | user-reported: drag ghosts (file rows, pane pills, tab rows) drifted left of the pointer; now ride at pointer+12px |
| b08e991 | **#8** persistence save-trigger gaps (family, all 5 sites): `TabModel` tree-mutation observer now fired from every mutator and wired to the debounced session save |
| bd27e0c | **#1** shortcut-recorder wedge: keymap restored when Settings closes mid-record |
| f197b35 | **#4 + #13** quit/close lifecycle with unregistered key windows (Settings): MRU quit fallback, ⌘W close, gated quit-when-empty |
| e01a08e | **#2 + #3** file browser: undo Move/Trash restore-target guard + stale drag-session mechanism deleted |
| d4ab1b8 | quit freeze: pane-exit events after AppQuitting re-flushed the shrunken model, losing tabs |
| 91b6f7f | quit flush upserted a user-closed window's snapshot → broken empty-window restore |
| 908f217 | two-window ctrl+d double-lease SIGABRT + empty-window restore |
| 9072144 | OSC test drain raced Exited vs feeder-thread events |
| 0ae0744 | leftward drag-selection dropped endpoint cells |
| 1008500 / 7a44c17 / 0108a6c | focus-handle bugs (drop focus, mouse-down focus, orphaned handle) |
| f161d56 | monospace fallback + PostScript→family import + picker filter |
| dcb7670 | input-flood whole-app freeze + presentation wedge |
| ec0b8f3 | drain wake starvation (typing freeze), invisible quit/close confirmation modal |
| 089239e | file-browser symlink classification |

---

_Full write-ups + adversarial verdicts: `scratchpad/bughunt/findings/<id>.md` and `<id>.verdict.md`. Every bug above was confirmed by an independent verifier (default-refute stance); none were refuted._
