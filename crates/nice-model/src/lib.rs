//! # nice-model
//!
//! Nice's per-window document model as pure Rust — no behavior tied to a
//! window, no `gpui` dependency (crates/README.md "Layering rule"). Ported
//! from `Sources/Nice/State/Models.swift`, the UI-free value tree that a
//! sidebar row (`Session`, a session) and its toolbar pills (`TermWindow`) render over.
//!
//! The crate splits into two layers, both ported verbatim from Swift:
//!
//! **Value types + status model** (`Models.swift`):
//!
//! * [`TermWindowKind`], [`SessionStatus`] — the window kind + per-window Claude status.
//! * [`TermWindow`] — a single toolbar pill: status transitions, the waiting-pulse
//!   acknowledgment model, and [`TermWindow::needs_attention`].
//! * [`Session`] — a session: the derived aggregate [`Session::status`] over its live
//!   Claude windows, [`Session::waiting_acknowledged`], and the
//!   [`Session::recover_next_terminal_index`] hydration helper.
//! * [`Project`] — an ordered group of sessions.
//! * [`PaneLayout`] / [`Pane`] ([`pane_layout`]) — the tmux-port Phase 2 split
//!   tree hanging off each [`TermWindow`]: the binary `Leaf`/`Split` shape, its
//!   mutations (split / remove / swap / resize), and the one geometry
//!   ([`PaneLayout::leaf_rects`], [`directional_neighbor`],
//!   [`spatial_refocus`]) that render, hit-testing, directional focus, and
//!   close-refocus all share. Pre-splits every window is a single-leaf tree, so
//!   nothing about a never-split pill changes.
//!
//! **The persistence leaves** (`SessionStore.swift`, model-shaped half):
//!
//! * [`PersistedTermWindow`] / [`PersistedSession`] / [`PersistedProject`] — the v3
//!   schema value types (camelCase JSON, Swift's shape **minus `branch`**) plus
//!   `from_model` snapshot / `hydrate` with the exact restore defaults, and
//!   [`snapshot_projects`] (the empty-project drop rule). The window envelope
//!   (`PersistedState`/`PersistedWindow`/`PersistedFrame`) + the store I/O live
//!   in `crates/nice`; these gpui-free leaves are what round-trip and hydrate.
//!
//! **The document** (`TabModel.swift`):
//!
//! * [`WorkspaceModel`] — the per-window projects/sessions/windows tree: seeding + the
//!   pinned Terminals group, selection ([`WorkspaceModel::select_session`], the single
//!   `active_session_id` writer), reorder, window insert/extract/move, renames +
//!   title locks + auto-title, cwd bucketing/repair/resolution, depth-1
//!   `/branch`+handoff lineage, single-entry removal + parent-pointer sweep,
//!   the arg parsers, and the did-mutate signal.
//! * [`FsProbe`] — the injected filesystem seam (existence + home) that keeps
//!   the document a pure value-tree.
//!
//! ## The asymmetries are deliberate
//!
//! Several behaviors in this model look inconsistent and are each intentional
//! and test-pinned. A reader "cleaning them up" is introducing a bug:
//!
//! 1. **"At most one *running* Claude per session" is a creation-edge rule, not a
//!    struct invariant.** The promotion guard keys on [`TermWindow::is_claude_running`]
//!    ([`Session::has_running_claude`]), so a running Claude and a deferred-resume
//!    Claude (`is_claude_running == false`) legitimately coexist in one session
//!    transiently. [`Session::status`] and the aggregations are written to tolerate
//!    that — there is deliberately **no** type-level "one Claude window" rule
//!    here, because one would break promotion and deferred resume.
//! 2. **The per-session "Terminal N" counter ([`Session::next_terminal_index`]) is
//!    monotonic** — never decremented, never reused. Closing "Terminal 2" does
//!    not free the name; the next add becomes "Terminal 4".
//!    [`Session::recover_next_terminal_index`] rebuilds it from window titles.
//! 3. **Empty-input rename is asymmetric.** [`WorkspaceModel::rename_session`] with empty
//!    input is a no-op; [`WorkspaceModel::rename_window`] with empty input resets to
//!    the per-kind default, clears the lock, and (for terminals) consumes a
//!    counter slot.
//! 4. **Two cwd writers, two policies.** OSC 7 writes `TermWindow.cwd` only;
//!    [`WorkspaceModel::adopt_session_cwd`] (the SessionStart-hook path) moves the session and
//!    pulls along only windows still tracking the old cwd — diverged windows stay,
//!    per-window, not all-or-nothing.
//!
//! And in the lineage: [`WorkspaceModel::insert_branch_parent`] re-parents an
//! originating root's former children on first-branch promotion, while
//! [`WorkspaceModel::insert_handoff_child`] deliberately does **not** re-parent — the
//! anchor stays root.
//!
//! ## Sidebar UI state (R10 pure ports)
//!
//! More gpui-free value-state modules the R10 sidebar builds over — ported
//! case-for-case from the pure-Swift seams and unit-testable exactly like the
//! tree above:
//!
//! * [`selection`] — [`SidebarSessionSelection`], the Finder-style multi-select
//!   model and the "selection ⊇ {active_session_id}" invariant.
//! * [`rename_gate`] — [`InlineRenameClickGate`], the injected-clock
//!   click-to-rename time gate (R11 reuses it).
//! * [`sidebar`] — [`SidebarModel`] (+ [`SidebarMode`]): collapsed/mode/peek
//!   state and the toggle + peek render/clear methods (R12 triggers them).
//! * [`key_hint`] — [`KeyHintModel`], the tmux-port Phase 1 hold-to-hint
//!   overlay flag (D5). Same shape as the peek flag: transient, never
//!   persisted, set/cleared by the keymap's modifier observer and rendered by
//!   the toolbar. The debounce timer behind it is gpui-side and stays in
//!   `crates/nice`.
//!
//! ## TermWindow strip geometry (R11 pure port)
//!
//! * [`strip_geometry`] — [`StripGeometry`], the toolbar window strip's pure
//!   visibility math (edge fades + the offscreen id set), ported from
//!   `Sources/Nice/Views/PaneStripGeometry.swift`, plus
//!   [`should_show_overflow_chevron`], the reservation + `>=2`-windows overflow
//!   rule ported *behaviorally* from `PaneStripOverflowEstimator.swift` (its
//!   width-estimation machinery does not survive — GPUI's real layout
//!   replaces it). The overflow chevron's attention badge is **not** a third
//!   predicate here: it reuses [`Session::has_offscreen_attention`] (R8) fed by
//!   [`StripGeometry::offscreen_window_ids`]. [`center_offset_x`] is the pure
//!   auto-center-on-activate offset math (the GPUI-real-layout replacement for
//!   SwiftUI's `scrollTo(anchor: .center)`), kept here so the R11 view and the
//!   in-process itests share one arithmetic.
//!
//! ## Keyboard-shortcut data (R12 pure port)
//!
//! ## File-browser model family (R19 pure ports)
//!
//! * [`file_browser`] — the gpui-free model family behind the sidebar's files
//!   mode: [`file_browser::listing`] (dirs-first filter + sort + visible-order
//!   flatten), [`file_browser::sort`] (the persisted sort-preference value
//!   type), [`file_browser::state`]/[`file_browser::store`] (per-session
//!   in-memory root/expansion/hidden state + the per-window catalog),
//!   [`file_browser::selection`] (the Finder-style multi-select model keyed by
//!   path), [`file_browser::click_router`] (the hand-rolled 280 ms
//!   double-click detector + `activated_at` rename hook),
//!   [`file_browser::menu`] (the context-menu visibility matrix),
//!   [`file_browser::open_with`] (the pure "Open With ▸" ordering function),
//!   and [`file_browser::header::file_browser_header_title`]. Ported
//!   case-for-case from the pure-Swift `FileBrowser*` seams; the views,
//!   kqueue watcher, and objc2 platform calls stay in `crates/nice`.
//!
//! * [`shortcuts`] — [`ShortcutAction`] (the closed 13-action rebindable set) +
//!   [`default_bindings`] (the default-combo table as data), ported from
//!   `Sources/Nice/State/KeyboardShortcuts.swift`. Gpui-free: R12's keymap slice
//!   generates the `actions!` / `bind_keys` wiring from this table, and R24's
//!   rebinding UI consumes the same data. Matching is character-token based at
//!   the gpui pin (a documented divergence from Swift's physical-keycode match —
//!   see the module docs).
//!
//! ## Update-checker version compare (R27 pure port)
//!
//! * [`SemanticVersion`] — the dotted-integer version parser + component-wise
//!   compare `ReleaseChecker` (crates/nice) uses to decide whether a GitHub
//!   release tag is newer than the running app, ported from
//!   `Sources/Nice/State/SemanticVersion.swift`. Not full semver: no
//!   prerelease/build-metadata handling, non-negative dotted integers only.

pub mod file_browser;
pub mod key_hint;
pub mod pane_layout;
mod persisted;
mod project;
pub mod rename_gate;
pub mod selection;
mod semantic_version;
mod session;
pub mod shortcuts;
pub mod sidebar;
pub mod strip_geometry;
mod term_window;
mod window_strip_drop;
mod workspace_model;

pub use pane_layout::{
    directional_neighbor, spatial_refocus, Pane, PaneDirection, PaneLayout, PaneRect, Side,
    SplitOrient,
};
pub use persisted::{
    snapshot_projects, PersistedPane, PersistedPaneLayout, PersistedProject, PersistedSession,
    PersistedTermWindow,
};
pub use project::Project;
pub use key_hint::KeyHintModel;
pub use rename_gate::InlineRenameClickGate;
pub use selection::SidebarSessionSelection;
pub use semantic_version::SemanticVersion;
pub use session::Session;
pub use shortcuts::{default_bindings, default_combo, KeyCombo, Modifiers, ShortcutAction};
pub use sidebar::{SidebarMode, SidebarModel};
pub use strip_geometry::{
    center_offset_x, should_show_overflow_chevron, Rect, StripGeometry, EDGE_TOLERANCE,
};
pub use term_window::{SessionStatus, TermWindow, TermWindowKind};
pub use window_strip_drop::{resolve, window_target};
pub use workspace_model::{FsProbe, WorkspaceModel};
