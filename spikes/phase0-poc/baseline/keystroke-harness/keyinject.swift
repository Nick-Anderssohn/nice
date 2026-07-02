// keyinject.swift — keystroke-latency injector for Phase-0 spikes 4b/5.
//
// Posts REAL keyDown+keyUp pairs to ONE target pid via CGEventPostToPid and
// records a timestamp per keyDown, so a concurrent xctrace 'Logging' recording
// can be reduced to keyDown→present latency (see reduce-latency.py + RUN.md).
//
// SAFETY INVARIANTS (do not weaken):
//  * Events are posted ONLY with CGEvent.postToPid(_:) — NEVER CGEventPost /
//    the global kCGHIDEventTap. The user may be typing on this machine while a
//    run is in progress; injected events must reach exactly one process.
//  * Refuses to target prod Nice (/Applications/Nice.app) unless --allow-prod.
//  * Exits nonzero (2) if AXIsProcessTrusted() == false — without the
//    Accessibility grant CGEventPostToPid is silently dropped and every
//    latency sample would be garbage.
//
// TIMEBASE / CORRELATION DESIGN (see RUN.md §Timebase):
//  * Around every keyDown post the injector ALSO emits an os_signpost interval
//    (subsystem "nice.keyharness", category "inject", name "KeyPost",
//    signpostID == seq). When xctrace records with --all-processes, those
//    KeyPost intervals land in the SAME os-signpost-interval table (and the
//    same "ns since trace start" timebase) as the target's present signposts,
//    so the reduction needs NO clock-domain conversion at all. The signpostID
//    (exported as the "identifier" column) carries the sequence number for an
//    exact join with this CSV.
//  * The CSV additionally records mach_absolute_time, mach_continuous_time
//    (both raw ticks and ns via mach_timebase_info: ns = ticks*numer/denom)
//    and CLOCK_REALTIME epoch-ns per keyDown. These are the FALLBACK
//    correlation path (attach-mode traces omit other processes' signposts;
//    join via the trace TOC's wall-clock <start-date>, ±ms precision) and the
//    cross-check path (reduce-latency.py prints the per-sample offset spread
//    between trace time and each mach clock).
//  * Stamps are taken immediately BEFORE the signpost .begin, which is
//    immediately before the keyDown post — CSV stamp, KeyPost.start and the
//    actual post agree to single-digit µs, i.e. noise at the ms scales we
//    measure (NOTES.md §3: "timestamp before the post").
//
// PACING / SINGLE-IN-FLIGHT: one keyDown every --gap-ms (default 100 ms),
// which is comfortably above the worst present latency ever observed on this
// machine (spike-4: p95 inter-draw 66 ms UNDER heavy load; idle echo draws
// land well under 40 ms). So at most one keystroke is unanswered at any time
// (NOTES.md §3 / Harness §C single-in-flight). The reducer additionally DROPS
// any sample whose matched present lands after the next keyDown.
//
// Build: make            (xcrun swiftc -O -o keyinject keyinject.swift)
// Usage: ./keyinject --pid <pid> [--n 500] [--gap-ms 100] [--warmup 5]
//                    [--keycode 0] [--char a] [--out FILE.csv]
//                    [--no-activate] [--check] [--allow-prod]

import AppKit
import ApplicationServices
import CoreGraphics
import Darwin
import Foundation
import os
import os.signpost

// Fixed signpost identity. The NAME must be a StaticString at every
// os_signpost call site, so it cannot be a CLI parameter; the reducer's
// --inject-* flags default to exactly these values.
let injectSubsystem = "nice.keyharness"
let injectCategory = "inject"
// name: "KeyPost" (literal at the call sites below)

let usageText = """
keyinject — post real keyDown/keyUp pairs to ONE pid and log post timestamps.

usage: keyinject --pid <pid> [options]

options:
  --pid <pid>       REQUIRED. Target process id (the app under test).
  --n <int>         Measured keystrokes to post (default 500).
  --warmup <int>    Extra unmeasured keystrokes posted first (default 5);
                    flagged warmup=1 in the CSV, excluded by the reducer.
  --gap-ms <float>  Pause between keyDown posts (default 100). Values below
                    50 void the single-in-flight guarantee (warned).
  --keycode <int>   CGKeyCode virtual key (default 0 = ANSI 'a').
  --char <string>   Unicode string attached to the events (default "a";
                    makes the insertion keyboard-layout independent).
  --out <path>      CSV output (default /tmp/keyinject-<pid>-<epochs>.csv).
  --no-activate     Do not activate (bring frontmost) the target app first.
                    Default is to activate: a non-key window may drop keys.
  --check           Preflight only: verify AX trust + pid and exit 0.
  --allow-prod      Permit targeting prod /Applications/Nice.app (NEVER do
                    this while it hosts live sessions).
  --help            This text.

exit codes: 0 ok · 2 Accessibility not granted · 3 bad target pid · 64 usage
"""

func die(_ msg: String, code: Int32) -> Never {
    FileHandle.standardError.write(Data(("keyinject: error: " + msg + "\n").utf8))
    exit(code)
}

func note(_ msg: String) {
    FileHandle.standardError.write(Data(("keyinject: " + msg + "\n").utf8))
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

struct Config {
    var pid: pid_t = -1
    var n = 500
    var warmup = 5
    var gapMs = 100.0
    var keycode: CGKeyCode = 0 // kVK_ANSI_A
    var chars = "a"
    var out: String? = nil
    var activate = true
    var checkOnly = false
    var allowProd = false
}

var cfg = Config()
let argv = Array(CommandLine.arguments.dropFirst())
var i = 0
func argValue(_ flag: String) -> String {
    i += 1
    guard i < argv.count else { die("\(flag) needs a value\n\n\(usageText)", code: 64) }
    return argv[i]
}
while i < argv.count {
    let a = argv[i]
    switch a {
    case "--help", "-h":
        print(usageText)
        exit(0)
    case "--pid":
        guard let v = Int32(argValue(a)) else { die("--pid must be an integer", code: 64) }
        cfg.pid = v
    case "--n":
        guard let v = Int(argValue(a)), v > 0 else { die("--n must be a positive integer", code: 64) }
        cfg.n = v
    case "--warmup":
        guard let v = Int(argValue(a)), v >= 0 else { die("--warmup must be >= 0", code: 64) }
        cfg.warmup = v
    case "--gap-ms":
        guard let v = Double(argValue(a)), v > 0 else { die("--gap-ms must be > 0", code: 64) }
        cfg.gapMs = v
    case "--keycode":
        guard let v = UInt16(argValue(a)) else { die("--keycode must be a UInt16", code: 64) }
        cfg.keycode = CGKeyCode(v)
    case "--char":
        cfg.chars = argValue(a)
    case "--out":
        cfg.out = argValue(a)
    case "--no-activate":
        cfg.activate = false
    case "--check":
        cfg.checkOnly = true
    case "--allow-prod":
        cfg.allowProd = true
    default:
        die("unknown argument '\(a)'\n\n\(usageText)", code: 64)
    }
    i += 1
}

guard cfg.pid > 0 else { die("--pid is required\n\n\(usageText)", code: 64) }
if cfg.gapMs < 50 {
    note("WARNING: --gap-ms \(cfg.gapMs) < 50 — below the worst-case present latency; " +
         "single-in-flight is NOT guaranteed and the reducer will drop overlapping samples.")
}

// ---------------------------------------------------------------------------
// Preflight 1: Accessibility trust. Without it CGEventPostToPid is a silent
// no-op and every latency sample would be garbage — fail loudly instead.
// ---------------------------------------------------------------------------

if !AXIsProcessTrusted() {
    die("""
    AXIsProcessTrusted() == false — this process lacks the Accessibility (TCC)
    grant, so CGEventPostToPid would be SILENTLY DROPPED.

    Fix (see baseline/ACCESSIBILITY-GRANT.md): the grant belongs to this
    session's RESPONSIBLE PROCESS — normally prod Nice.app, which hosts the
    shell this tool runs in. Open System Settings → Privacy & Security →
    Accessibility and enable Nice. If Nice already shows ON but this error
    persists, the grant is STALE (prod Nice was rebuilt, so its ad-hoc cdhash
    changed): REMOVE Nice with the '−' button and RE-ADD it, then re-run.

    Verify with:  swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'
    """, code: 2)
}

// ---------------------------------------------------------------------------
// Preflight 2: the target pid must exist, and must not be prod Nice.
// ---------------------------------------------------------------------------

if kill(cfg.pid, 0) != 0 && errno != EPERM {
    die("no process with pid \(cfg.pid)", code: 3)
}

let runningApp = NSRunningApplication(processIdentifier: cfg.pid)
if let exe = runningApp?.executableURL?.path {
    note("target pid \(cfg.pid) = \(exe)")
    if exe == "/Applications/Nice.app/Contents/MacOS/Nice" && !cfg.allowProd {
        die("""
        target pid \(cfg.pid) is PROD Nice (/Applications/Nice.app) — it hosts
        live Claude Code sessions; injected keystrokes would type into them.
        Target 'Nice Dev' instead. (--allow-prod overrides; don't.)
        """, code: 3)
    }
} else {
    note("target pid \(cfg.pid) exists (not a session GUI app, or not visible " +
         "to NSRunningApplication — prod-guard and activation unavailable)")
}

if cfg.checkOnly {
    note("preflight OK: AX trusted, pid \(cfg.pid) valid. (--check: not posting)")
    exit(0)
}

// ---------------------------------------------------------------------------
// Output CSV
// ---------------------------------------------------------------------------

var tb = mach_timebase_info_data_t()
mach_timebase_info(&tb)
@inline(__always) func machNs(_ ticks: UInt64) -> UInt64 {
    // Exact while ticks*numer fits UInt64: numer/denom = 125/3 on Apple
    // Silicon → overflow only after centuries of uptime. Truncation < 1 ns.
    ticks &* UInt64(tb.numer) / UInt64(tb.denom)
}

let outPath = cfg.out ?? "/tmp/keyinject-\(cfg.pid)-\(UInt64(Date().timeIntervalSince1970)).csv"
FileManager.default.createFile(atPath: outPath, contents: nil)
guard let fh = FileHandle(forWritingAtPath: outPath) else {
    die("cannot open \(outPath) for writing", code: 1)
}
func w(_ s: String) { fh.write(Data(s.utf8)) }

let anchorAbs = mach_absolute_time()
let anchorCont = mach_continuous_time()
let anchorWall = clock_gettime_nsec_np(CLOCK_REALTIME)
w("# keyinject v1\n")
w("# pid=\(cfg.pid) n=\(cfg.n) warmup=\(cfg.warmup) gap_ms=\(cfg.gapMs) keycode=\(cfg.keycode) char=\(cfg.chars)\n")
w("# inject_subsystem=\(injectSubsystem) inject_category=\(injectCategory) inject_name=KeyPost signpost_id=seq\n")
w("# mach_timebase numer=\(tb.numer) denom=\(tb.denom)  (ns = ticks*numer/denom)\n")
w("# anchor mach_abs_ticks=\(anchorAbs) mach_cont_ticks=\(anchorCont) wall_epoch_ns=\(anchorWall)\n")
w("seq,warmup,keycode,mach_abs_ticks,mach_abs_ns,mach_cont_ticks,mach_cont_ns,wall_epoch_ns\n")

// ---------------------------------------------------------------------------
// Activate the target (its key window must accept keys), then inject.
// ---------------------------------------------------------------------------

if cfg.activate, let app = runningApp {
    note("activating target app (use --no-activate to skip)")
    app.activate()
    usleep(300_000) // let focus settle before the first post
}

let log = OSLog(subsystem: injectSubsystem, category: injectCategory)
var utf16 = Array(cfg.chars.utf16)
let total = cfg.warmup + cfg.n
let gapUs = useconds_t(cfg.gapMs * 1000.0)

note("posting \(cfg.n) keystrokes (+\(cfg.warmup) warmup) to pid \(cfg.pid), " +
     "gap \(cfg.gapMs) ms — ~\(String(format: "%.0f", Double(total) * cfg.gapMs / 1000.0)) s total")

for seq in 1...total {
    let isWarm = seq <= cfg.warmup
    guard
        let down = CGEvent(keyboardEventSource: nil, virtualKey: cfg.keycode, keyDown: true),
        let up = CGEvent(keyboardEventSource: nil, virtualKey: cfg.keycode, keyDown: false)
    else {
        die("CGEvent creation failed at seq \(seq)", code: 1)
    }
    down.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
    up.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)

    let sid = OSSignpostID(UInt64(seq))
    let tAbs = mach_absolute_time()
    let tCont = mach_continuous_time()
    let wall = clock_gettime_nsec_np(CLOCK_REALTIME)
    os_signpost(.begin, log: log, name: "KeyPost", signpostID: sid,
                "seq=%llu warmup=%d", UInt64(seq), Int32(isWarm ? 1 : 0))
    down.postToPid(cfg.pid) // pid-targeted — NEVER the global HID tap
    up.postToPid(cfg.pid)
    os_signpost(.end, log: log, name: "KeyPost", signpostID: sid)

    w("\(seq),\(isWarm ? 1 : 0),\(cfg.keycode),\(tAbs),\(machNs(tAbs)),\(tCont),\(machNs(tCont)),\(wall)\n")
    if seq % 50 == 0 { note("  posted \(seq)/\(total)") }
    usleep(gapUs)
}

try? fh.close()
note("done: \(cfg.n) measured keydowns (+\(cfg.warmup) warmup) posted to pid \(cfg.pid)")
note("csv: \(outPath)")
print(outPath)
