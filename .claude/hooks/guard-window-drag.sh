#!/bin/sh
# PreToolUse guard for the window-drag-vs-pill-drag invariant.
#
# Fires on Edit/Write/MultiEdit. When the EDIT TARGET is
# WindowToolbarView.swift, it injects a reminder (NOT a block) so any agent
# about to change the pane-pill drag mechanism is told the invariant and
# the required UITest gate up front. The invariant is behavioral — the code
# compiles and unit tests pass while it's broken — so this plus the UITests
# in Sources/Nice/Views/CLAUDE.md are the safety net.
#
# Matches on the tool_input.file_path field specifically (parsed from the
# stdin JSON) so merely *mentioning* the filename in some other file's
# content doesn't trip it.

input=$(cat)
path=$(printf '%s' "$input" | /usr/bin/python3 -c 'import sys,json
try:
    print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))
except Exception:
    print("")' 2>/dev/null)

case "$path" in
  */WindowToolbarView.swift)
    printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"WINDOW-DRAG INVARIANT (WindowToolbarView.swift): The pane pill MUST use SwiftUI .onDrag so a pill drag claims the gesture and the toolbar windowDragGesture yields — i.e. dragging a pill must NOT move the window. Do NOT replace .onDrag with a background NSDraggingSource / hitTest-nil NSView without ALSO gating windowDragGesture to ignore presses that begin over a pill; doing so re-introduces dragging-a-pill-moves-the-window (compiles + unit-passes while behaviorally wrong). Any change to the pill drag mechanism MUST keep NiceUITests/WindowDragUITests and NiceUITests/PaneReorderUITests green. Read Sources/Nice/Views/CLAUDE.md before proceeding."}}'
    ;;
esac
exit 0
