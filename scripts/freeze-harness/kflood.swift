// kflood — synthetic keystroke flood posted directly to a pid.
//
// Posts keydown/keyup pairs via CGEventPostToPid (NOT the global HID tap —
// Wispr Flow eats globally-posted synthetic keys on this machine). Cycles
// through a small set of letter keycodes to emulate typing/dictation floods.
//
// Usage: kflood <pid> [--cps 100] [--duration-s 10] [--enter-every 0 (0 = never)]
//
// Prints a summary line at the end: chars posted, elapsed, effective cps.

import CoreGraphics
import Foundation

func parseArg(_ name: String, _ def: Double) -> Double {
    let args = CommandLine.arguments
    if let i = args.firstIndex(of: name), i + 1 < args.count, let v = Double(args[i + 1]) {
        return v
    }
    return def
}

guard CommandLine.arguments.count >= 2, let pid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write("usage: kflood <pid> [--cps N] [--duration-s N] [--enter-every N]\n".data(using: .utf8)!)
    exit(2)
}

let cps = parseArg("--cps", 100)
let durationS = parseArg("--duration-s", 10)
let enterEvery = Int(parseArg("--enter-every", 0))

// ANSI letter keycodes: a s d f g h j k l  — plus space.
let keycodes: [CGKeyCode] = [0, 1, 2, 3, 5, 4, 38, 40, 37, 49]
let returnKey: CGKeyCode = 36

guard let source = CGEventSource(stateID: .hidSystemState) else {
    FileHandle.standardError.write("kflood: cannot create event source\n".data(using: .utf8)!)
    exit(3)
}

let interval = 1.0 / cps
let t0 = DispatchTime.now()
var posted = 0
var next = 0.0

func post(_ key: CGKeyCode) {
    if let down = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: true) {
        down.postToPid(pid)
    }
    if let up = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false) {
        up.postToPid(pid)
    }
}

while true {
    let rel = Double(DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1e9
    if rel >= durationS { break }
    if kill(pid, 0) != 0 {
        FileHandle.standardError.write("kflood: target pid \(pid) gone\n".data(using: .utf8)!)
        break
    }
    if enterEvery > 0 && posted > 0 && posted % enterEvery == 0 {
        post(returnKey)
    } else {
        post(keycodes[posted % keycodes.count])
    }
    posted += 1
    next += interval
    let ahead = next - (Double(DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1e9)
    if ahead > 0 { usleep(useconds_t(ahead * 1e6)) }
}

// Trailing settle so the final posted events deliver before exit.
usleep(250_000)
let elapsed = Double(DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1e9
print("kflood: posted=\(posted) elapsed_s=\(String(format: "%.2f", elapsed)) effective_cps=\(String(format: "%.0f", Double(posted) / elapsed))")
