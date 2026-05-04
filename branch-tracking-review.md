# /branch tracking — reviewer feedback

Two reviewer subagents passed over the feature on 2026-05-04. One focused
on code quality (low coupling / high cohesion / separation of concerns);
the other on testability and coverage. Findings are consolidated and
ranked below.

## Critical (real bugs / invariant gaps)

**1. Tab-id collision risk** — `SessionsModel.swift:821` mints
`t<timestamp_ms>`. Two `/branch`es in the same millisecond produce
duplicate ids. Same smell at lines 464, 504, 584, 614. **Fix:** inject
an `idMinter: () -> String` on `SessionsModel.init` (default keeps
current behavior), or just use a UUID prefix.

**2. Dangling-pointer invariant is enforced at the wrong layer** —
`clearDanglingParentReferences` is called only by
`AppState.finalizeDissolvedTab`. Today that's the only tab-removal
path, but the invariant is fragile: any future remover that does
`tabs.projects[pi].tabs.remove(at:)` directly leaks dangling pointers.
**Fix:** move the sweep inside a `TabModel.removeTab(...)` helper so
removal + sweep are atomic.

**3. Missing coverage: restart round-trip end-to-end** — codec test
covers `PersistedTab.parentTabId` but no test drives
`WindowSession.snapshotForSave → restoreSavedWindow` for a /branch
lineage. A regression in the snapshot/restore code would slip past
the codec test.

**4. Missing coverage: `/branch` on a tab with nil
`claudeSessionId`** — guard short-circuits today but is unpinned.
Easy to seed via `TabModelFixtures.injectTab(kind: .claude)` and
assert no parent is created.

**5. Missing coverage: closing a *child* (not parent)** —
`clearDanglingParentReferences` walks the whole project but is only
tested with parent removal. Mirror the existing test but exit the
child's panes.

## Design concerns

**6. `materializeBranchParent` straddles three concerns** (tree
mutation + pty creation + sidebar order). Split the tree-mutation
half (insert + stamp pointer + first-branch lineage rule) into
`TabModel.insertBranchParent(...)` so the model invariant lives with
the model.

**7. Branch classification rule is inline** —
`source == "resume" && oldId != nil && oldId != sessionId` lives
inside `handleClaudeSessionUpdate`. Extract to a pure
`enum BranchClassifier` so the rule is testable without spinning up
`AppState`.

**8. `Tab.parentTabId` representation encodes two relationships
through one field** — both "I'm the depth-1 child originating tab"
and "I'm an accumulated parent under root R" render as
`parentTabId == R`. Doc comment glosses over the asymmetry after
the first `/branch`. An explicit
`enum LineageRole { case root, branchChild(rootId), branchParent(rootId) }`
would self-document. Skip if depth-1 is the permanent ceiling.

**9. Hook script ↔ receiver schema is implicit** — script literal
in Swift, hand-rolled JSON unwrap on receive, magic discriminator
strings. Plus: sed regex `[a-zA-Z0-9_-]+` is narrower than JSON's
source-string grammar — `source: "branch.auto"` would silently
truncate to `branch`. Cheap mitigations: (a) widen regex to
`[^"]+`; (b) add a Swift-side `struct SessionUpdatePayload: Codable`
consumed by both the parser and a contract test that round-trips
the script's printf output.

**10. Default-nil `source` on `handleClaudeSessionUpdate`** — purely
backward-compat for one test file. **Fix:** make it required; one
find-and-replace updates existing test callers and removes the risk
of a future production caller silently bypassing branch detection.

## Should-do

**11. Test `/branch` on root after deferred resume** — real risk:
branching the root (parentTabId=nil) flips it to having a parent,
but pre-existing children of root still point at the old root →
depth 2 silently. Spec-violating; needs a test AND a model fix
(re-parent former root-children to the new root).

**12. UITest proxy via control socket** — real-Claude UITest is
impractical, but launching `Nice Dev`, `nc -U` a `session_update`
JSON to its socket, then asserting via `XCUIApplication.sidebar.tabs[...]`
would catch UI regressions. Currently the indent isn't assertable
from accessibility — `TabRow` doesn't expose nesting. Adding
`.accessibilityIdentifier("sidebar.tab.\(tab.id).child")` when
`parentTabId != nil` is a one-line unblock for that.

**13. `WindowSession.restoreSavedWindow` should validate
`parentTabId` references** — a hand-edited or partially corrupt
sessions.json could leave a child pointing at a vanished parent.
The sidebar tolerates it (just shows indent) but no sweep runs on
restore.

**14. Cross-project move would break `parentTabId`** — `moveTab`
is same-project today, but no comment or assertion prevents it.
Add a "must reference same project" assertion in
`materializeBranchParent` and a doc note on `Tab.parentTabId`.

**15. Per-window scoping for the branch-materialization side** —
existing test pins id-update scoping across windows; needs an
equivalent for the parent-spawn side.

## Nits

**16. `Tab.parentTabId` doc-comment lies** about "both parents and
originating tab point at the same root" — only true after the second
`/branch`. Tighten or fold into the LineageRole enum.

**17. Sidebar magic numbers** `22 / 38` could move to a
`Tab.sidebarIndent` helper if the LineageRole enum lands.

**18. Adversarial source-value tests** for the hook script —
hyphen, space, JSON-injection-shaped string. Documents the contract
more than catches bugs.

**19. Title frozen on parent** at `/branch` time — parent inherits
`titleAutoGenerated`/`titleManuallySet` and the live OSC stream
only updates the originating tab afterward. Probably the right
behavior, but worth a comment so reviewers don't re-derive it.

## Triage

- **Acting on now:** 1, 2, 3, 4, 5, 10, 11, 12 (with 17's
  accessibilityIdentifier prerequisite).
- **Deferred polish:** 6, 7, 8, 9, 13, 14, 15, 16, 18, 19.
