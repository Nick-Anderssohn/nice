// axping — external main-thread responsiveness probe.
//
// Measures the round-trip latency of a trivial AX attribute read against a
// target pid at a fixed cadence. AX requests are serviced on the target app's
// main run loop, so a blocked/saturated main thread shows up as a latency
// spike here — from OUTSIDE the process, no cooperation needed.
//
// Output: one CSV line per probe on stdout:  t_rel_s,latency_ms,status
//   status: ok | slow (>slowMs) | hang (>hangMs)
// On the FIRST transition into `hang`, prints "HANG-DETECTED t=..." on stderr
// (a wrapper script can trigger `sample <pid>` off that line).
// On recovery (hang -> ok), prints "RECOVERED t=... after_ms=..." on stderr.
//
// Usage: axping <pid> [--interval-ms 250] [--slow-ms 500] [--hang-ms 2000]
//        [--duration-s 0 (0 = forever)] [--timeout-s 30]

import ApplicationServices
import Foundation

func parseArg(_ name: String, _ def: Double) -> Double {
    let args = CommandLine.arguments
    if let i = args.firstIndex(of: name), i + 1 < args.count, let v = Double(args[i + 1]) {
        return v
    }
    return def
}

guard CommandLine.arguments.count >= 2, let pid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write("usage: axping <pid> [--interval-ms N] [--slow-ms N] [--hang-ms N] [--duration-s N] [--timeout-s N]\n".data(using: .utf8)!)
    exit(2)
}

let intervalMs = parseArg("--interval-ms", 250)
let slowMs = parseArg("--slow-ms", 500)
let hangMs = parseArg("--hang-ms", 2000)
let durationS = parseArg("--duration-s", 0)
let timeoutS = parseArg("--timeout-s", 30)

guard AXIsProcessTrusted() else {
    FileHandle.standardError.write("axping: this process is not AX-trusted (grant Accessibility to the host)\n".data(using: .utf8)!)
    exit(3)
}

let app = AXUIElementCreateApplication(pid)
// Long messaging timeout: we want to MEASURE the stall, not time out under it.
AXUIElementSetMessagingTimeout(app, Float(timeoutS))

let t0 = DispatchTime.now()
var inHang = false
var hangStartRel = 0.0
print("t_rel_s,latency_ms,status")

while true {
    let rel = Double(DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1e9
    if durationS > 0 && rel > durationS { break }

    var value: CFTypeRef?
    let start = DispatchTime.now()
    let err = AXUIElementCopyAttributeValue(app, kAXRoleAttribute as CFString, &value)
    let ms = Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1e6

    if err == .invalidUIElement || err == .cannotComplete && kill(pid, 0) != 0 {
        FileHandle.standardError.write("axping: target pid \(pid) gone (err=\(err.rawValue))\n".data(using: .utf8)!)
        exit(4)
    }

    let status: String
    if ms >= hangMs {
        status = "hang"
        if !inHang {
            inHang = true
            hangStartRel = rel
            FileHandle.standardError.write("HANG-DETECTED t=\(String(format: "%.2f", rel)) latency_ms=\(String(format: "%.0f", ms))\n".data(using: .utf8)!)
        }
    } else if ms >= slowMs {
        status = "slow"
    } else {
        status = "ok"
        if inHang {
            inHang = false
            let heldMs = (rel - hangStartRel) * 1000 + ms
            FileHandle.standardError.write("RECOVERED t=\(String(format: "%.2f", rel)) after_ms=\(String(format: "%.0f", heldMs))\n".data(using: .utf8)!)
        }
    }
    print("\(String(format: "%.3f", rel)),\(String(format: "%.1f", ms)),\(status)")
    fflush(stdout)

    let sleepMs = max(0, intervalMs - ms)
    if sleepMs > 0 { usleep(useconds_t(sleepMs * 1000)) }
}
