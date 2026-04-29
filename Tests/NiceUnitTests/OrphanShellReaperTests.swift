//
//  OrphanShellReaperTests.swift
//  NiceUnitTests
//
//  Exercises `OrphanShellReaper.parseArgsBuffer`, the parser for
//  `KERN_PROCARGS2` buffers. The reaper itself runs against live
//  processes and so isn't unit-testable in isolation, but the parser
//  is pure — and the buffer layout is BSD-defined and quirky enough
//  (argc int, exec_path, NUL alignment padding, argv entries, env
//  entries) that a regression here would silently keep the reaper
//  from matching the `NICE_TAB_ID=` filter.
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
}
