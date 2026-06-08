//
//  SkillInstallerTests.swift
//  NiceUnitTests
//
//  Pins `SkillInstaller.install`, `uninstall`, and `sync` with injected
//  sandbox paths so tests never touch the developer's real
//  `~/.claude/skills/` or `~/.nice/`. Mirrors the pattern from
//  ClaudeHookInstallerTests.
//
//  Coverage:
//    • `install` writes `SKILL.md` in skillDir and `nice-handoff.sh` in
//      helperDir; helper is executable (0o755); SKILL.md frontmatter
//      contains `name: nice-handoff` and does NOT contain
//      `disable-model-invocation`.
//    • Idempotency: second `install` is a no-op (mtime unchanged).
//    • `uninstall` removes both files (and the skill dir); safe when
//      already absent.
//    • `sync(enabled:true)` routes to install; `sync(enabled:false)`
//      routes to uninstall.
//

import Darwin
import Foundation
import XCTest
@testable import Nice

final class SkillInstallerTests: XCTestCase {

    private var tmpRoot: URL!
    private var skillDir: URL!
    private var helperDir: URL!

    override func setUpWithError() throws {
        tmpRoot = URL(
            fileURLWithPath: NSTemporaryDirectory(),
            isDirectory: true
        ).appendingPathComponent("nice-skill-installer-\(UUID().uuidString)")
        try FileManager.default.createDirectory(
            at: tmpRoot, withIntermediateDirectories: true
        )
        // Mirrors the production layout:
        //   skillDir  = ~/.claude/skills/nice-handoff/
        //   helperDir = ~/.nice/
        // Both are under our sandbox tmpRoot so we never touch the
        // developer's real home directories.
        skillDir  = tmpRoot.appendingPathComponent("claude/skills/nice-handoff")
        helperDir = tmpRoot.appendingPathComponent("nice")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmpRoot)
    }

    // MARK: - Install: file presence and content

    func test_install_writesSkillMarkdownInSkillDir() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let skillURL = skillDir.appendingPathComponent("SKILL.md")
        XCTAssertTrue(FileManager.default.fileExists(atPath: skillURL.path),
                      "install must create SKILL.md inside skillDir")

        let content = try String(contentsOf: skillURL, encoding: .utf8)
        XCTAssertFalse(content.isEmpty, "SKILL.md must not be empty")
    }

    func test_install_skillMarkdownFrontmatterContainsHandoffName() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let skillURL = skillDir.appendingPathComponent("SKILL.md")
        let content = try String(contentsOf: skillURL, encoding: .utf8)

        XCTAssertTrue(content.contains("name: nice-handoff"),
                      "SKILL.md frontmatter must declare `name: nice-handoff` so Claude discovers it as /nice-handoff")
    }

    func test_install_skillMarkdownDoesNotContainDisableModelInvocation() throws {
        // `disable-model-invocation` would prevent agents from
        // auto-invoking the skill on context-window overflow — that is
        // exactly the primary use case. The key must be absent.
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let skillURL = skillDir.appendingPathComponent("SKILL.md")
        let content = try String(contentsOf: skillURL, encoding: .utf8)

        XCTAssertFalse(content.contains("disable-model-invocation"),
                       "SKILL.md must NOT contain disable-model-invocation — agents must be able to auto-invoke the handoff skill")
    }

    func test_install_writesHelperScriptInHelperDir() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let helperURL = helperDir.appendingPathComponent("nice-handoff.sh")
        XCTAssertTrue(FileManager.default.fileExists(atPath: helperURL.path),
                      "install must create nice-handoff.sh inside helperDir")
    }

    func test_install_helperScriptIsExecutable() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let helperURL = helperDir.appendingPathComponent("nice-handoff.sh")
        let attrs = try FileManager.default.attributesOfItem(atPath: helperURL.path)
        let perms = (attrs[.posixPermissions] as? NSNumber)?.int16Value ?? 0
        XCTAssertEqual(perms & 0o777, 0o755, "helper script permissions must be exactly 0755")
    }

    func test_install_helperScriptHasNoBashSyntaxError() throws {
        // Quick sanity: the script must at least parse cleanly under
        // bash --norc -n (syntax-check only). Catches trivial quoting
        // mistakes introduced by a future edit. Uses /bin/bash (the
        // system bash that always exists on macOS); /usr/bin/bash is
        // not present on stock macOS.
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        let helperURL = helperDir.appendingPathComponent("nice-handoff.sh")

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/bash")
        proc.arguments = ["--norc", "-n", helperURL.path]
        proc.standardOutput = Pipe()
        proc.standardError = Pipe()
        try proc.run()
        proc.waitUntilExit()
        XCTAssertEqual(proc.terminationStatus, 0,
                       "nice-handoff.sh must pass bash -n syntax check")
    }

    // MARK: - Idempotency

    func test_install_secondCallDoesNotRewriteSkillMarkdown() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        let skillURL = skillDir.appendingPathComponent("SKILL.md")
        let firstMtime = try mtime(of: skillURL)

        // Coarse filesystem timestamp granularity on some shared CI
        // environments can be under 10ms; sleep 50ms to guarantee a
        // rewrite would land on a different mtime.
        Thread.sleep(forTimeInterval: 0.05)

        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        XCTAssertEqual(try mtime(of: skillURL), firstMtime,
                       "second install must skip the SKILL.md write entirely (mtime unchanged)")
    }

    func test_install_secondCallDoesNotRewriteHelperScript() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        let helperURL = helperDir.appendingPathComponent("nice-handoff.sh")
        let firstMtime = try mtime(of: helperURL)
        Thread.sleep(forTimeInterval: 0.05)

        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        XCTAssertEqual(try mtime(of: helperURL), firstMtime,
                       "second install must skip the helper-script write entirely (mtime unchanged)")
    }

    func test_install_tripleCallIsIdempotent() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        // Both files still exist and their content matches the expected
        // static values. No duplicate files, no corruption.
        let skillContent = try String(
            contentsOf: skillDir.appendingPathComponent("SKILL.md"), encoding: .utf8
        )
        XCTAssertEqual(skillContent, SkillInstaller.skillMarkdown,
                       "repeated install must not corrupt SKILL.md content")
    }

    // MARK: - Content-drift / self-healing rewrite

    func test_install_rewritesDriftedHelperScript_andRestoresPermissions() throws {
        // Pre-plant a stale nice-handoff.sh with wrong content and
        // wrong permissions (0o600). install() must detect the content
        // mismatch, rewrite the file with the canonical helperScript, and
        // reset permissions to 0o755. This is the self-healing path that
        // fires when the on-disk script diverges from the embedded source
        // (e.g. after a Nice update that changes the script body).
        try FileManager.default.createDirectory(
            at: helperDir, withIntermediateDirectories: true
        )
        let helperURL = helperDir.appendingPathComponent("nice-handoff.sh")
        let stalePath = helperURL.path
        try "#!/bin/sh\necho stale\n".write(to: helperURL, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o600)],
            ofItemAtPath: stalePath
        )

        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let rewrittenContent = try String(contentsOf: helperURL, encoding: .utf8)
        XCTAssertEqual(rewrittenContent, SkillInstaller.helperScript,
                       "install must rewrite a drifted helper script with the canonical content")

        let attrs = try FileManager.default.attributesOfItem(atPath: stalePath)
        let perms = (attrs[.posixPermissions] as? NSNumber)?.int16Value ?? 0
        XCTAssertEqual(perms & 0o777, 0o755,
                       "install must restore permissions to 0o755 after rewriting a drifted helper")
    }

    func test_install_rewritesDriftedSkillMarkdown() throws {
        // Pre-plant a stale SKILL.md with wrong content. install() must
        // detect the mismatch and rewrite it with the canonical skillMarkdown.
        try FileManager.default.createDirectory(
            at: skillDir, withIntermediateDirectories: true
        )
        let skillURL = skillDir.appendingPathComponent("SKILL.md")
        try "# stale\nThis content is outdated.\n".write(
            to: skillURL, atomically: true, encoding: .utf8
        )

        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        let rewrittenContent = try String(contentsOf: skillURL, encoding: .utf8)
        XCTAssertEqual(rewrittenContent, SkillInstaller.skillMarkdown,
                       "install must rewrite a drifted SKILL.md with the canonical content")
    }

    // MARK: - Uninstall

    func test_uninstall_removesSkillDirAndHelperScript() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        XCTAssertTrue(FileManager.default.fileExists(atPath: skillDir.path),
                      "precondition: skillDir must exist after install")

        SkillInstaller.uninstall(skillDir: skillDir, helperDir: helperDir)

        XCTAssertFalse(FileManager.default.fileExists(atPath: skillDir.path),
                       "uninstall must remove the entire skill directory")
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: helperDir.appendingPathComponent("nice-handoff.sh").path
            ),
            "uninstall must remove nice-handoff.sh"
        )
    }

    func test_uninstall_safeWhenAlreadyAbsent() {
        // Calling uninstall on a directory that was never installed must
        // not throw or crash — mirrors the ClaudeHookInstaller's
        // idempotent uninstall contract.
        SkillInstaller.uninstall(skillDir: skillDir, helperDir: helperDir)
        // Reaching this line is the assertion (no crash / no throw).
    }

    func test_uninstall_doesNotRemoveHelperDirOtherFiles() throws {
        // The helperDir is shared with ClaudeHookInstaller. Uninstall
        // must remove only nice-handoff.sh, leaving any sibling files
        // (e.g. nice-claude-hook.sh) intact.
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)

        // Plant a sibling file that must survive the uninstall.
        let siblingURL = helperDir.appendingPathComponent("nice-claude-hook.sh")
        try "#!/usr/bin/env bash\n# sibling\n".write(
            to: siblingURL, atomically: true, encoding: .utf8
        )

        SkillInstaller.uninstall(skillDir: skillDir, helperDir: helperDir)

        XCTAssertTrue(FileManager.default.fileExists(atPath: siblingURL.path),
                      "sibling nice-claude-hook.sh must survive SkillInstaller.uninstall")
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: helperDir.appendingPathComponent("nice-handoff.sh").path
            ),
            "nice-handoff.sh must be removed by uninstall"
        )
    }

    // MARK: - sync(enabled:)

    func test_sync_enabledTrue_callsInstall() throws {
        SkillInstaller.sync(enabled: true, skillDir: skillDir, helperDir: helperDir)

        XCTAssertTrue(
            FileManager.default.fileExists(
                atPath: skillDir.appendingPathComponent("SKILL.md").path
            ),
            "sync(enabled: true) must install the skill"
        )
    }

    func test_sync_enabledFalse_callsUninstall() throws {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        XCTAssertTrue(FileManager.default.fileExists(atPath: skillDir.path),
                      "precondition: skill installed")

        SkillInstaller.sync(enabled: false, skillDir: skillDir, helperDir: helperDir)

        XCTAssertFalse(FileManager.default.fileExists(atPath: skillDir.path),
                       "sync(enabled: false) must uninstall the skill")
    }

    // MARK: - Path invariant
    //
    // Claude's skill discovery is path-based. The skillDir tail must not
    // contain spaces because shell helpers invoke it as a bare path;
    // same no-space requirement as ClaudeHookInstaller's script dir.

    func test_defaultSkillDir_pathComponentHasNoSpaces() {
        let component = SkillInstaller.defaultSkillDir().lastPathComponent
        XCTAssertFalse(
            component.contains(" "),
            "defaultSkillDir's last path component must have no spaces — shell path would break"
        )
    }

    // MARK: - helpers

    private func mtime(of url: URL) throws -> Date {
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        return try XCTUnwrap(attrs[.modificationDate] as? Date)
    }
}
