//
//  OrphanShellReaper.swift
//  Nice
//
//  Reap zsh processes orphaned by prior crashed Nice runs. After a
//  normal quit each pane's zsh is SIGTERM'd via `LocalProcess.terminate`
//  and exits before the parent does. After a crash, SIGKILL of the
//  parent (Xcode "Stop", abort signal, XCUITest tearDown that didn't
//  call `XCUIApplication.terminate()`), or any path where
//  `applicationWillTerminate` doesn't fire, the children are reparented
//  to launchd (PPID == 1) and sit idle holding pty slots.
//
//  macOS caps `kern.tty.ptmx_max` at 511. Repeated dev iterations and
//  aborted UITest runs accumulate orphans; once the cap is approached,
//  fresh `forkpty()` calls inside SwiftTerm fail. SwiftTerm's
//  `startProcessWithForkpty` returns silently on failure, so the
//  caller has no signal — the pane shows "Launching terminal…"
//  forever. See `docs/todos/restored-secondary-pane-hangs.md`.
//
//  Filter: PPID == 1, current uid, command basename == "zsh", env
//  contains `NICE_TAB_ID=`. The env check is the load-bearing one —
//  it ensures we never SIGKILL a non-Nice zsh the user may have
//  intentionally daemonized (nohup'd from a now-closed terminal,
//  detached from a launchd job, etc.). Sibling live Nice instances
//  are filtered by PPID — their children's PPID is the live Nice's
//  pid, not 1.
//

import Darwin
import Foundation

enum OrphanShellReaper {
    /// SIGKILL every Nice-spawned zsh whose parent died without
    /// terminating us cleanly. Synchronous; expected to run in the low
    /// tens of milliseconds even with hundreds of orphans (libproc and
    /// `KERN_PROCARGS2` reads are page-sized). Idempotent — safe to
    /// call repeatedly. Returns the number of processes it killed for
    /// logging.
    @discardableResult
    static func reap() -> Int {
        var killed = 0
        for pid in candidatePids() {
            guard let env = environment(of: pid) else { continue }
            guard env.contains(where: { $0.hasPrefix("NICE_TAB_ID=") }) else {
                continue
            }
            if kill(pid, SIGKILL) == 0 {
                killed += 1
            }
        }
        return killed
    }

    // MARK: - Process enumeration

    /// Enumerate every process where PPID == 1, uid matches ours, and
    /// the executable name is `zsh`. Uses libproc rather than
    /// `sysctl(KERN_PROC_ALL)` so the filter logic doesn't have to
    /// reach into Swift's quirky import of the BSD `kinfo_proc` struct
    /// (whose `p_comm` is a CChar tuple and whose `e_ucred` field path
    /// differs across SDKs).
    private static func candidatePids() -> [pid_t] {
        let allPids = listAllPids()
        let myUid = getuid()
        var results: [pid_t] = []
        for pid in allPids {
            guard pid > 1 else { continue }
            var info = proc_bsdinfo()
            let size = MemoryLayout<proc_bsdinfo>.size
            let result = proc_pidinfo(
                pid, PROC_PIDTBSDINFO, 0, &info, Int32(size)
            )
            guard result == Int32(size) else { continue }
            guard info.pbi_ppid == 1 else { continue }
            guard info.pbi_uid == myUid else { continue }
            // `pbi_comm` is a fixed C-string tuple (MAXCOMLEN+1 = 17).
            // SwiftTerm spawns `/bin/zsh`, so the kernel's truncated
            // command name is exactly "zsh".
            var comm = info.pbi_comm
            let name = withUnsafeBytes(of: &comm) { raw -> String in
                let bound = raw.bindMemory(to: CChar.self)
                return String(cString: bound.baseAddress!)
            }
            guard name == "zsh" else { continue }
            results.append(pid)
        }
        return results
    }

    /// Wrap `proc_listallpids` — call once with `nil` to size the
    /// buffer, then again to fill it. Returns an empty array on any
    /// error (the reaper is best-effort; a failed enumeration just
    /// means we don't reap this launch).
    private static func listAllPids() -> [pid_t] {
        let bytes = proc_listallpids(nil, 0)
        guard bytes > 0 else { return [] }
        // Pad in case the table grew between the size probe and fill.
        let capacity = Int(bytes) / MemoryLayout<pid_t>.stride + 64
        var pids = [pid_t](repeating: 0, count: capacity)
        let filled = pids.withUnsafeMutableBufferPointer { buf -> Int32 in
            proc_listallpids(buf.baseAddress, Int32(buf.count) * Int32(MemoryLayout<pid_t>.stride))
        }
        guard filled > 0 else { return [] }
        let count = Int(filled) / MemoryLayout<pid_t>.stride
        return Array(pids.prefix(count))
    }

    // MARK: - KERN_PROCARGS2 parsing

    /// Read a process's environment via `sysctl(KERN_PROCARGS2)`.
    /// Returns nil if the process is gone, the buffer is malformed, or
    /// the kernel refused (different uid — shouldn't happen given the
    /// uid filter above, but the call can still fail if the process
    /// exited between the enumeration and this read).
    private static func environment(of pid: pid_t) -> [String]? {
        var argMax: Int32 = 1024 * 1024
        var argMaxMib: [Int32] = [CTL_KERN, KERN_ARGMAX]
        var argMaxSize = MemoryLayout<Int32>.size
        _ = sysctl(&argMaxMib, UInt32(argMaxMib.count), &argMax, &argMaxSize, nil, 0)

        var mib: [Int32] = [CTL_KERN, KERN_PROCARGS2, pid]
        var bufSize = Int(argMax)
        var buf = [UInt8](repeating: 0, count: bufSize)
        let result = buf.withUnsafeMutableBufferPointer { ptr -> Int32 in
            sysctl(&mib, UInt32(mib.count), ptr.baseAddress, &bufSize, nil, 0)
        }
        if result != 0 { return nil }
        return parseArgsBuffer(buf, length: bufSize)
    }

    /// Parse a `KERN_PROCARGS2` buffer into the env strings only.
    /// Layout: `int argc | exec_path\0 | NUL pad | argv[0]\0 ... argv[argc-1]\0 | env[0]\0 ... env[N-1]\0`.
    /// Internal so unit tests can validate the parser against
    /// synthetic buffers without spawning a child.
    static func parseArgsBuffer(_ buf: [UInt8], length: Int) -> [String]? {
        guard length >= MemoryLayout<Int32>.size else { return nil }
        let argc: Int32 = buf.withUnsafeBufferPointer { ptr in
            ptr.baseAddress!.withMemoryRebound(to: Int32.self, capacity: 1) {
                $0.pointee
            }
        }
        var idx = MemoryLayout<Int32>.size
        // exec_path: NUL-terminated string immediately after argc.
        while idx < length, buf[idx] != 0 { idx += 1 }
        // Skip alignment padding (additional NUL bytes).
        while idx < length, buf[idx] == 0 { idx += 1 }
        // Skip argv strings.
        var consumed: Int32 = 0
        while consumed < argc, idx < length {
            while idx < length, buf[idx] != 0 { idx += 1 }
            if idx < length { idx += 1 }
            consumed += 1
        }
        // Remaining bytes are env strings up to a terminating empty
        // string or end of buffer.
        var env: [String] = []
        while idx < length {
            if buf[idx] == 0 { break }
            let start = idx
            while idx < length, buf[idx] != 0 { idx += 1 }
            let bytes = Array(buf[start..<idx])
            if let s = String(bytes: bytes, encoding: .utf8) {
                env.append(s)
            }
            if idx < length { idx += 1 }
        }
        return env
    }
}
