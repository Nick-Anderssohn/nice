// ktype — type an ASCII string into a pid via CGEventPostToPid (US-ANSI map).
//
// Usage: ktype <pid> [--cps 40] [--enter] -- <text...>
//   Everything after `--` is joined with spaces and typed literally.
//   --enter posts Return at the end.

import CoreGraphics
import Foundation

func parseArg(_ name: String, _ def: Double) -> Double {
    let args = CommandLine.arguments
    if let i = args.firstIndex(of: name), i + 1 < args.count, let v = Double(args[i + 1]) {
        return v
    }
    return def
}

let argv = CommandLine.arguments
guard argv.count >= 2, let pid = Int32(argv[1]) else {
    FileHandle.standardError.write("usage: ktype <pid> [--cps N] [--enter] [--pre-ctrl-c] -- <text>\n".data(using: .utf8)!)
    exit(2)
}
let cps = parseArg("--cps", 40)
let sendEnter = argv.contains("--enter")
let preCtrlC = argv.contains("--pre-ctrl-c")
guard let dashIdx = argv.firstIndex(of: "--"), dashIdx + 1 < argv.count else {
    FileHandle.standardError.write("ktype: no text after --\n".data(using: .utf8)!)
    exit(2)
}
let text = argv[(dashIdx + 1)...].joined(separator: " ")

// US-ANSI keycodes: char -> (keycode, needsShift)
let plain: [Character: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9,
    "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17,
    "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "=": 24, "9": 25, "7": 26,
    "-": 27, "8": 28, "0": 29, "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35,
    "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44, "n": 45,
    "m": 46, ".": 47, " ": 49, "`": 50,
]
let shifted: [Character: CGKeyCode] = [
    "A": 0, "S": 1, "D": 2, "F": 3, "H": 4, "G": 5, "Z": 6, "X": 7, "C": 8, "V": 9,
    "B": 11, "Q": 12, "W": 13, "E": 14, "R": 15, "Y": 16, "T": 17,
    "!": 18, "@": 19, "#": 20, "$": 21, "^": 22, "%": 23, "+": 24, "(": 25, "&": 26,
    "_": 27, "*": 28, ")": 29, "}": 30, "O": 31, "U": 32, "{": 33, "I": 34, "P": 35,
    "L": 37, "J": 38, "\"": 39, "K": 40, ":": 41, "|": 42, "<": 43, "?": 44, "N": 45,
    "M": 46, ">": 47, "~": 50,
]

guard let source = CGEventSource(stateID: .hidSystemState) else { exit(3) }

func post(_ key: CGKeyCode, shift: Bool, ctrl: Bool = false) {
    for down in [true, false] {
        guard let ev = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: down) else { continue }
        if shift { ev.flags = .maskShift }
        if ctrl { ev.flags = .maskControl }
        ev.postToPid(pid)
    }
}

let interval = useconds_t(1_000_000.0 / cps)
if preCtrlC {
    post(8, shift: false, ctrl: true) // Ctrl+C — abort any pending shell input line
    usleep(300_000)
}
for ch in text {
    if let k = plain[ch] {
        post(k, shift: false)
    } else if let k = shifted[ch] {
        post(k, shift: true)
    } else {
        FileHandle.standardError.write("ktype: unmapped char '\(ch)' — skipped\n".data(using: .utf8)!)
        continue
    }
    usleep(interval)
}
if sendEnter {
    usleep(100_000)
    post(36, shift: false)
}
// Trailing settle: CGEventPostToPid delivery is async — exiting immediately
// after the last post can drop it (the leg-A lost Return).
usleep(250_000)
print("ktype: typed \(text.count) chars\(sendEnter ? " + Return" : "")")
