//! # nice-model
//!
//! Nice's per-window document model as pure Rust — no behavior tied to a
//! window, no `gpui` dependency (crates/README.md "Layering rule"). Ported
//! from `Sources/Nice/State/Models.swift`, the UI-free value tree that a
//! sidebar row (`Tab`, a session) and its toolbar pills (`Pane`) render over.
//!
//! The crate splits into two layers, both ported verbatim from Swift:
//!
//! **Value types + status model** (`Models.swift`):
//!
//! * [`PaneKind`], [`TabStatus`] — the pane kind + per-pane Claude status.
//! * [`Pane`] — a single toolbar pill: status transitions, the waiting-pulse
//!   acknowledgment model, and [`Pane::needs_attention`].
//! * [`Tab`] — a session: the derived aggregate [`Tab::status`] over its live
//!   Claude panes, [`Tab::waiting_acknowledged`], and the
//!   [`Tab::recover_next_terminal_index`] hydration helper.
//! * [`Project`] — an ordered group of tabs.
//!
//! **The document** (`TabModel.swift`):
//!
//! * [`TabModel`] — the per-window projects/tabs/panes tree: seeding + the
//!   pinned Terminals group, selection ([`TabModel::select_tab`], the single
//!   `active_tab_id` writer), reorder, pane insert/extract/move, renames +
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
//! 1. **"At most one *running* Claude per tab" is a creation-edge rule, not a
//!    struct invariant.** The promotion guard keys on [`Pane::is_claude_running`]
//!    ([`Tab::has_running_claude`]), so a running Claude and a deferred-resume
//!    Claude (`is_claude_running == false`) legitimately coexist in one tab
//!    transiently. [`Tab::status`] and the aggregations are written to tolerate
//!    that — there is deliberately **no** type-level "one Claude pane" rule
//!    here, because one would break promotion and deferred resume.
//! 2. **The per-tab "Terminal N" counter ([`Tab::next_terminal_index`]) is
//!    monotonic** — never decremented, never reused. Closing "Terminal 2" does
//!    not free the name; the next add becomes "Terminal 4".
//!    [`Tab::recover_next_terminal_index`] rebuilds it from pane titles.
//! 3. **Empty-input rename is asymmetric.** [`TabModel::rename_tab`] with empty
//!    input is a no-op; [`TabModel::rename_pane`] with empty input resets to
//!    the per-kind default, clears the lock, and (for terminals) consumes a
//!    counter slot.
//! 4. **Two cwd writers, two policies.** OSC 7 writes `Pane.cwd` only;
//!    [`TabModel::adopt_tab_cwd`] (the SessionStart-hook path) moves the tab and
//!    pulls along only panes still tracking the old cwd — diverged panes stay,
//!    per-pane, not all-or-nothing.
//!
//! And in the lineage: [`TabModel::insert_branch_parent`] re-parents an
//! originating root's former children on first-branch promotion, while
//! [`TabModel::insert_handoff_child`] deliberately does **not** re-parent — the
//! anchor stays root.

mod pane;
mod project;
mod tab;
mod tab_model;

pub use pane::{Pane, PaneKind, TabStatus};
pub use project::Project;
pub use tab::Tab;
pub use tab_model::{FsProbe, TabModel};
