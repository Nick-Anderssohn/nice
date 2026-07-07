// keypost <keycode> [cmd]  — posts keyDown+keyUp globally via HID event tap
import CoreGraphics
import Foundation
let code = CGKeyCode(UInt16(CommandLine.arguments[1])!)
let withCmd = CommandLine.arguments.count > 2 && CommandLine.arguments[2] == "cmd"
let src = CGEventSource(stateID: .hidSystemState)
let down = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!
let up = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!
if withCmd { down.flags = .maskCommand; up.flags = .maskCommand }
down.post(tap: .cghidEventTap)
usleep(60_000)
up.post(tap: .cghidEventTap)
