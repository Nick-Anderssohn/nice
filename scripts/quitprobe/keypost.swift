// keypost <keycode> [cmd] [pid] — posts keyDown+keyUp as real CGEvents.
// With a trailing pid, posts via CGEventPostToPid to that process (the standing
// rule for driving nice-rs — see the tranche testNotes); without one, posts
// globally via the HID tap. Global posting is environment-fragile: an ACTIVE
// third-party event tap (e.g. Wispr Flow's dictation tap, 2026-07-07) can
// consume specific synthetic keys (Escape) system-wide while physical presses
// and pid-targeted posts pass — that false-failed this probe against a healthy
// app. Prefer the pid form.
import CoreGraphics
import Foundation
let code = CGKeyCode(UInt16(CommandLine.arguments[1])!)
var withCmd = false
var pid: pid_t? = nil
for arg in CommandLine.arguments.dropFirst(2) {
    if arg == "cmd" { withCmd = true } else if let n = Int32(arg) { pid = pid_t(n) }
}
let src = CGEventSource(stateID: .hidSystemState)
let down = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!
let up = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!
if withCmd { down.flags = .maskCommand; up.flags = .maskCommand }
if let pid {
    down.postToPid(pid)
    usleep(60_000)
    up.postToPid(pid)
} else {
    down.post(tap: .cghidEventTap)
    usleep(60_000)
    up.post(tap: .cghidEventTap)
}
