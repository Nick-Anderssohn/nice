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
//  forever. See `docs/done/restored-secondary-pane-hangs.md`.
//
//  Filter: PPID == 1, current uid, command basename == "zsh", env
//  contains `NICE_TAB_ID=`. The env check is the load-bearing one —
//  it ensures we never SIGKILL a non-Nice zsh the user may have
//  intentionally daemonized (nohup'd from a now-closed terminal,
//  detached from a launchd job, etc.). Sibling live Nice instances
//  are filtered by PPID — their children's PPID is the live Nice's
//  pid, not 1.
//
//  System surface (libproc / sysctl / kill) is injected via
//  `OrphanShellReaper.Env` so the reaper logic — the env filter and
//  kill-counting — is unit-testable without running real zshes.
//

import Darwin
import Foundation

enum OrphanShellReaper {
    /// Side-effecting surface the reaper depends on. Tests substitute
    /// closures returning canned data; production uses `.live`, which
    /// wires through to libproc + sysctl + `kill(2)`. Kept as a struct
    /// of closures (not a protocol) so test fakes are a one-liner.
    struct Env {
        /// Pids that match the orphan filter (PPID==1, uid==me,
        /// comm=="zsh"). Empty array on enumeration failure.
        var listCandidates: () -> [pid_t]
        /// Read process environment, or nil if the process is gone or
        /// the kernel refused.
        var environment: (pid_t) -> [String]?
        /// SIGKILL the pid. Returns true on success.
        var kill: (pid_t) -> Bool
    }

    /// SIGKILL every Nice-spawned zsh whose parent died without
    /// terminating us cleanly. Synchronous; expected to run in the low
    /// tens of milliseconds even with hundreds of orphans (libproc and
    /// `KERN_PROCARGS2` reads are page-sized). Idempotent — safe to
    /// call repeatedly. Returns the number of processes it killed for
    /// logging.
    @discardableResult
    static func reap(env: Env = .live) -> Int {
        var killed = 0
        for pid in env.listCandidates() {
            guard let envVars = env.environment(pid) else { continue }
            guard envVars.contains(where: { $0.hasPrefix("NICE_TAB_ID=") })
            else { continue }
            if env.kill(pid) {
                killed += 1
            }
        }
        return killed
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

extension OrphanShellReaper.Env {
    /// Production wiring: libproc + sysctl + `kill(2)`. The captured
    /// closures only call thread-safe Darwin primitives (libproc /
    /// sysctl / kill are kernel-mediated and reentrant), so a single
    /// process-wide instance is safe. Marking it `nonisolated(unsafe)`
    /// avoids forcing every test fake to construct `@Sendable`
    /// closures even though no real concurrent access can occur — the
    /// reaper is invoked once at bootstrap and tests run their fakes
    /// inline on the test thread.
    nonisolated(unsafe) static let live = OrphanShellReaper.Env(
        listCandidates: liveCandidatePids,
        environment: liveEnvironment,
        kill: { Darwin.kill($0, SIGKILL) == 0 }
    )

    // MARK: - libproc enumeration

    /// Enumerate every process where PPID == 1, uid matches ours, and
    /// the executable name is `zsh`. Uses libproc rather than
    /// `sysctl(KERN_PROC_ALL)` so the filter logic doesn't have to
    /// reach into Swift's quirky import of the BSD `kinfo_proc` struct
    /// (whose `p_comm` is a CChar tuple and whose `e_ucred` field path
    /// differs across SDKs).
    fileprivate static func liveCandidatePids() -> [pid_t] {
        let allPids = liveAllPids()
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

    /// `proc_listallpids` two-call dance: probe for size, then fill.
    /// Returns an empty array on enumeration failure and emits an NSLog
    /// breadcrumb so a "the reaper never fires" investigation has a
    /// trail; the reaper itself is best-effort and never throws.
    private static func liveAllPids() -> [pid_t] {
        let bytes = proc_listallpids(nil, 0)
        guard bytes > 0 else {
            NSLog("OrphanShellReaper: proc_listallpids size probe failed (errno=\(errno))")
            return []
        }
        // Pad in case the table grew between the size probe and fill.
        let capacity = Int(bytes) / MemoryLayout<pid_t>.stride + 64
        var pids = [pid_t](repeating: 0, count: capacity)
        let filled = pids.withUnsafeMutableBufferPointer { buf -> Int32 in
            proc_listallpids(
                buf.baseAddress,
                Int32(buf.count) * Int32(MemoryLayout<pid_t>.stride)
            )
        }
        guard filled > 0 else {
            NSLog("OrphanShellReaper: proc_listallpids fill failed (errno=\(errno))")
            return []
        }
        let count = Int(filled) / MemoryLayout<pid_t>.stride
        return Array(pids.prefix(count))
    }

    // MARK: - KERN_PROCARGS2

    /// Read a process's environment via `sysctl(KERN_PROCARGS2)`.
    /// Returns nil if the process is gone, the buffer is malformed, or
    /// the kernel refused (different uid — shouldn't happen given the
    /// uid filter above, but the call can still fail if the process
    /// exited between the enumeration and this read).
    fileprivate static func liveEnvironment(of pid: pid_t) -> [String]? {
        var argMax: Int32 = 1024 * 1024
        var argMaxMib: [Int32] = [CTL_KERN, KERN_ARGMAX]
        var argMaxSize = MemoryLayout<Int32>.size
        // Best-effort probe. If it fails, fall through with the 1 MB
        // default — generous for any real-world env block (macOS
        // typically reports KERN_ARGMAX at 1 MB).
        _ = sysctl(
            &argMaxMib, UInt32(argMaxMib.count),
            &argMax, &argMaxSize, nil, 0
        )

        var mib: [Int32] = [CTL_KERN, KERN_PROCARGS2, pid]
        var bufSize = Int(argMax)
        var buf = [UInt8](repeating: 0, count: bufSize)
        let result = buf.withUnsafeMutableBufferPointer { ptr -> Int32 in
            sysctl(
                &mib, UInt32(mib.count),
                ptr.baseAddress, &bufSize, nil, 0
            )
        }
        if result != 0 { return nil }
        return OrphanShellReaper.parseArgsBuffer(buf, length: bufSize)
    }
}
