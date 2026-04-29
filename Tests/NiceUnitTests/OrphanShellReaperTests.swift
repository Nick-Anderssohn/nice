//
//  OrphanShellReaperTests.swift
//  NiceUnitTests
//
//  Two layers of coverage:
//
//  1. `parseArgsBuffer` — pure parser for `KERN_PROCARGS2` buffers.
//     The buffer layout is BSD-defined and quirky enough (argc int,
//     exec_path, NUL alignment padding, argv entries, env entries)
//     that a regression here would silently keep the reaper from
//     matching the `NICE_TAB_ID=` filter.
//
//  2. `reap(env:)` — the reaper's filter + kill-counting logic, with
//     `OrphanShellReaper.Env` substituted by closures returning canned
//     data. Pins the load-bearing safety filter (the env check that
//     keeps us from SIGKILLing a non-Nice user-daemonized zsh).
//

import XCTest
@testable import Nice

final class OrphanShellReaperTests: XCTestCase {

    // MARK: - Helpers

    /// Build a synthetic `KERN_PROCARGS2` buffer:
    ///   int32 argc | exec_path\0 | NUL pad | argv strings | env strings
    /// `padBytes` is the count of extra NULs between the exec_path
    /// terminator and the first argv entry, matching the alignment
    /// padding the kernel inserts.
    private func buildBuffer(
        execPath: String,
        argv: [String],
        env: [String],
        padBytes: Int = 7
    ) -> [UInt8] {
        var buf: [UInt8] = []
        var argc = Int32(argv.count)
        withUnsafeBytes(of: &argc) { raw in
            buf.append(contentsOf: raw)
        }
        buf.append(contentsOf: execPath.utf8)
        buf.append(0)
        buf.append(contentsOf: Array(repeating: 0, count: padBytes))
        for arg in argv {
            buf.append(contentsOf: arg.utf8)
            buf.append(0)
        }
        for envVar in env {
            buf.append(contentsOf: envVar.utf8)
            buf.append(0)
        }
        return buf
    }

    // MARK: - Tests

    func test_parsesEnv_typicalLayout() {
        let buf = buildBuffer(
            execPath: "/bin/zsh",
            argv: ["zsh", "-il"],
            env: ["TERM=xterm-256color", "HOME=/Users/nick", "NICE_TAB_ID=tab-abc"]
        )
        let env = OrphanShellReaper.parseArgsBuffer(buf, length: buf.count)
        XCTAssertEqual(env, [
            "TERM=xterm-256color",
            "HOME=/Users/nick",
            "NICE_TAB_ID=tab-abc",
        ])
    }

    func test_zeroArgv_stillReadsEnv() {
        let buf = buildBuffer(
            execPath: "/bin/zsh",
            argv: [],
            env: ["NICE_TAB_ID=t1"]
        )
        let env = OrphanShellReaper.parseArgsBuffer(buf, length: buf.count)
        XCTAssertEqual(env, ["NICE_TAB_ID=t1"])
    }

    func test_emptyEnv_returnsEmptyArray() {
        let buf = buildBuffer(
            execPath: "/bin/zsh",
            argv: ["zsh"],
            env: []
        )
        let env = OrphanShellReaper.parseArgsBuffer(buf, length: buf.count)
        XCTAssertEqual(env, [])
    }

    /// The kernel's alignment between exec_path and argv[0] is at
    /// least one NUL but can be more — the parser must skip every
    /// trailing NUL byte before reading the first argv string.
    func test_handlesVariablePadding() {
        for pad in 0...32 {
            let buf = buildBuffer(
                execPath: "/bin/zsh",
                argv: ["zsh", "-il"],
                env: ["NICE_TAB_ID=t"],
                padBytes: pad
            )
            let env = OrphanShellReaper.parseArgsBuffer(buf, length: buf.count)
            XCTAssertEqual(env, ["NICE_TAB_ID=t"], "failed at pad=\(pad)")
        }
    }

    func test_truncatedBufferBeforeArgc_returnsNil() {
        let truncated: [UInt8] = [0x01, 0x00] // less than sizeof(int32)
        XCTAssertNil(OrphanShellReaper.parseArgsBuffer(truncated, length: truncated.count))
    }

    /// A buffer truncated mid-argv (short read) should return whatever
    /// env it managed to find — empty in this case, and not crash.
    func test_truncatedBufferMidArgv_doesNotCrash() {
        var buf = buildBuffer(
            execPath: "/bin/zsh",
            argv: ["zsh", "-il"],
            env: ["NICE_TAB_ID=t"]
        )
        // Cut the buffer in the middle of the argv strings.
        let cut = MemoryLayout<Int32>.size + 8
        buf = Array(buf.prefix(cut))
        let env = OrphanShellReaper.parseArgsBuffer(buf, length: buf.count)
        // Either nil or empty is acceptable — the contract is "don't
        // crash and don't return phantom env strings."
        if let env {
            XCTAssertTrue(env.isEmpty)
        }
    }

    func test_envOrderPreserved() {
        let envIn = (0..<10).map { "VAR\($0)=value\($0)" }
        let buf = buildBuffer(execPath: "/bin/zsh", argv: ["zsh"], env: envIn)
        let envOut = OrphanShellReaper.parseArgsBuffer(buf, length: buf.count)
        XCTAssertEqual(envOut, envIn)
    }

    // MARK: - reap(env:) injection tests

    /// Build a fake `Env` whose closures return canned data and record
    /// every kill the reaper requested. `envByPid == nil` simulates an
    /// env read that failed (process exited between enumeration and
    /// read). `failedKills` lists pids whose `kill` closure should
    /// return false, modelling EPERM / ESRCH.
    private struct FakeEnv {
        let env: OrphanShellReaper.Env
        let killed: () -> [pid_t]
    }
    private func makeFakeEnv(
        candidates: [pid_t],
        envByPid: [pid_t: [String]?],
        failedKills: Set<pid_t> = []
    ) -> FakeEnv {
        var killedPids: [pid_t] = []
        let env = OrphanShellReaper.Env(
            listCandidates: { candidates },
            environment: { envByPid[$0] ?? nil },
            kill: { pid in
                if failedKills.contains(pid) { return false }
                killedPids.append(pid)
                return true
            }
        )
        return FakeEnv(env: env, killed: { killedPids })
    }

    func test_reap_emptyCandidates_returnsZero_noKills() {
        let fake = makeFakeEnv(candidates: [], envByPid: [:])
        XCTAssertEqual(OrphanShellReaper.reap(env: fake.env), 0)
        XCTAssertEqual(fake.killed(), [])
    }

    /// The env filter is the load-bearing safety check: a zsh under
    /// `nohup` or detached from a launchd job has PPID==1 and uid==me
    /// but no `NICE_TAB_ID=` in its env. Reaping it would be a real
    /// regression.
    func test_reap_skipsCandidatesWithoutNiceTabIdEnv() {
        let fake = makeFakeEnv(
            candidates: [100, 200, 300],
            envByPid: [
                100: ["TERM=xterm-256color", "HOME=/Users/x"],
                200: ["NICE_TAB_ID=tab-a", "HOME=/Users/x"],
                300: ["PATH=/usr/bin", "USER=x"],
            ]
        )
        let killed = OrphanShellReaper.reap(env: fake.env)
        XCTAssertEqual(killed, 1)
        XCTAssertEqual(fake.killed(), [200])
    }

    func test_reap_killsAllNiceTabIdMatches_returnsCount() {
        let fake = makeFakeEnv(
            candidates: [10, 20, 30],
            envByPid: [
                10: ["NICE_TAB_ID=t1"],
                20: ["NICE_TAB_ID=t2", "FOO=bar"],
                30: ["NICE_TAB_ID=t3"],
            ]
        )
        XCTAssertEqual(OrphanShellReaper.reap(env: fake.env), 3)
        XCTAssertEqual(fake.killed(), [10, 20, 30])
    }

    /// `environment(of:)` returning nil simulates the kernel refusing
    /// `KERN_PROCARGS2` (different uid, exited mid-read). The pid must
    /// be skipped — both the env-check and the kill should not fire.
    func test_reap_skipsProcessWhoseEnvReadFails() {
        let fake = makeFakeEnv(
            candidates: [10, 20, 30],
            envByPid: [
                10: ["NICE_TAB_ID=t1"],
                20: nil,                  // env read failed
                30: ["NICE_TAB_ID=t3"],
            ]
        )
        XCTAssertEqual(OrphanShellReaper.reap(env: fake.env), 2)
        XCTAssertEqual(fake.killed(), [10, 30])
    }

    /// `kill` returning false (EPERM/ESRCH) must not be counted. The
    /// pid was *attempted* — the test verifies the count is the number
    /// of *successful* kills, matching what the call site logs.
    func test_reap_kill_failure_doesNotCount() {
        let fake = makeFakeEnv(
            candidates: [10, 20],
            envByPid: [
                10: ["NICE_TAB_ID=t1"],
                20: ["NICE_TAB_ID=t2"],
            ],
            failedKills: [10]
        )
        XCTAssertEqual(OrphanShellReaper.reap(env: fake.env), 1)
        XCTAssertEqual(fake.killed(), [20])
    }

    /// The empty-env case has to walk the env loop without matching
    /// the prefix — modelling a process that has *no* env vars at all
    /// (extreme edge case, but parse-able by `parseArgsBuffer`).
    func test_reap_skipsCandidateWithEmptyEnv() {
        let fake = makeFakeEnv(
            candidates: [10],
            envByPid: [10: []]
        )
        XCTAssertEqual(OrphanShellReaper.reap(env: fake.env), 0)
        XCTAssertEqual(fake.killed(), [])
    }
}
