// winmove — list displays / move+size a pid's first window via AX.
//
// Usage:
//   winmove displays                 — print id,bounds,refresh,main for each display
//   winmove <pid>                    — print the pid's windows (title + frame)
//   winmove <pid> <x> <y> [w h]      — move (and optionally resize) window 0
//
// Needs AX trust (same as axping).

import ApplicationServices
import CoreGraphics
import Foundation

let args = CommandLine.arguments

if args.count >= 2 && args[1] == "displays" {
    var ids = [CGDirectDisplayID](repeating: 0, count: 16)
    var count: UInt32 = 0
    CGGetActiveDisplayList(16, &ids, &count)
    for i in 0..<Int(count) {
        let d = ids[i]
        let b = CGDisplayBounds(d)
        var hz = 0.0
        if let mode = CGDisplayCopyDisplayMode(d) { hz = mode.refreshRate }
        let main = (d == CGMainDisplayID()) ? " MAIN" : ""
        print("display id=\(d) origin=(\(Int(b.origin.x)),\(Int(b.origin.y))) size=\(Int(b.width))x\(Int(b.height)) refresh=\(hz)Hz\(main)")
    }
    exit(0)
}

guard args.count >= 2, let pid = Int32(args[1]) else {
    FileHandle.standardError.write("usage: winmove displays | winmove <pid> [x y [w h]]\n".data(using: .utf8)!)
    exit(2)
}
guard AXIsProcessTrusted() else {
    FileHandle.standardError.write("winmove: not AX-trusted\n".data(using: .utf8)!)
    exit(3)
}

let app = AXUIElementCreateApplication(pid)
AXUIElementSetMessagingTimeout(app, 10.0) // avoid -25205 under system load
var winsRef: CFTypeRef?
guard AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &winsRef) == .success,
      let wins = winsRef as? [AXUIElement], !wins.isEmpty else {
    FileHandle.standardError.write("winmove: no AX windows for pid \(pid)\n".data(using: .utf8)!)
    exit(4)
}

func frame(_ w: AXUIElement) -> (CGPoint, CGSize) {
    var posRef: CFTypeRef?, sizeRef: CFTypeRef?
    var p = CGPoint.zero, s = CGSize.zero
    if AXUIElementCopyAttributeValue(w, kAXPositionAttribute as CFString, &posRef) == .success {
        AXValueGetValue(posRef as! AXValue, .cgPoint, &p)
    }
    if AXUIElementCopyAttributeValue(w, kAXSizeAttribute as CFString, &sizeRef) == .success {
        AXValueGetValue(sizeRef as! AXValue, .cgSize, &s)
    }
    return (p, s)
}

if args.count < 4 {
    for (i, w) in wins.enumerated() {
        var titleRef: CFTypeRef?
        AXUIElementCopyAttributeValue(w, kAXTitleAttribute as CFString, &titleRef)
        let (p, s) = frame(w)
        print("window \(i): '\((titleRef as? String) ?? "?")' origin=(\(Int(p.x)),\(Int(p.y))) size=\(Int(s.width))x\(Int(s.height))")
    }
    exit(0)
}

guard let x = Double(args[2]), let y = Double(args[3]) else {
    FileHandle.standardError.write("winmove: bad x/y\n".data(using: .utf8)!)
    exit(2)
}
var pt = CGPoint(x: x, y: y)
let ptVal = AXValueCreate(.cgPoint, &pt)!
let win = wins[0]
let perr = AXUIElementSetAttributeValue(win, kAXPositionAttribute as CFString, ptVal)
var serr = AXError.success
if args.count >= 6, let w = Double(args[4]), let h = Double(args[5]) {
    var sz = CGSize(width: w, height: h)
    let szVal = AXValueCreate(.cgSize, &sz)!
    serr = AXUIElementSetAttributeValue(win, kAXSizeAttribute as CFString, szVal)
}
let (p, s) = frame(win)
print("moved: err=\(perr.rawValue)/\(serr.rawValue) now origin=(\(Int(p.x)),\(Int(p.y))) size=\(Int(s.width))x\(Int(s.height))")
