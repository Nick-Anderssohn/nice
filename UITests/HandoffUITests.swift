//
//  HandoffUITests.swift
//  NiceUITests
//
//  UITests for the user-facing surfaces of the Nice Handoff feature:
//  the Settings toggle and the first-launch install prompt. Both drive
//  the real app (Nice Dev build) with a sandboxed HOME and
//  `NICE_HOME_OVERRIDE` pointed at that same HOME so `SkillInstaller`
//  writes the skill/helper under the sandbox instead of the developer's
//  real `~/.claude/` and `~/.nice/`.
//
//  Scope note — the socket → nested-tab path is intentionally NOT
//  covered here. Posting to the per-window control socket from the
//  XCUITest runner proved unreliable (the runner's connect() to the
//  app's AF_UNIX socket is denied under the test harness), and that
//  path's logic is already covered end-to-end by unit tests that drive
//  the real code directly:
//    • NiceControlSocketHandoffTests — the `handoff` socket parse.
//    • TabModelInsertHandoffChildTests — depth-1 nesting / lineage.
//    • SessionsModelHandoffRequestTests — handler builds a nested tab
//      with the locked "[HANDOFF] <title>", prefix de-dup, cwd, and
//      the "ok" reply.
//  The sidebar's rendering of a nested tab (indent + lineage marker)
//  is the same code path exercised by the existing /branch tests.
//
//  Tests covered here:
//
//  1. Settings toggle installs / uninstalls files:
//       Open Settings, select the Claude section, flip the
//       `settings.claude.installHandoffSkill` toggle on, and assert that
//       SKILL.md and nice-handoff.sh appear under the sandbox HOME. Flip
//       off and assert they're removed.
//
//  2. First-launch prompt:
//       Force the alert by passing UserDefaults-domain overrides via
//       launchArguments (`-handoffSkillPromptSeen NO -installHandoffSkill
//       NO`). macOS registers launchArguments as UserDefaults entries so
//       `Tweaks.init` sees them in `.standard`. Assert the alert appears;
//       tap `handoffPrompt.install`; assert the skill file is created.
//       NOTE: the "fires exactly once" guarantee is covered by the
//       NiceServicesHandoffPromptTests (`consumeHandoffSkillPromptSlot`)
//       and TweaksHandoffFlagsTests (`handoffSkillPromptSeen`) units.
//

import Foundation
import XCTest

final class HandoffUITests: NiceUITestCase {

    // MARK: - Fake home

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
            fakeHomeURL = nil
        }
        try super.tearDownWithError()
    }

    // MARK: - 1. Settings toggle installs / uninstalls files

    func test_settingsToggle_installsAndUninstallsSkillFiles() throws {
        let home = makeFakeHome()
        // Hermetic start via the UserDefaults argument domain (which
        // overrides cfprefsd on read, so a previous test that flipped
        // installHandoffSkill in the shared dev bundle can't leak in):
        //   - installHandoffSkill=NO  → bootstrap leaves the skill
        //     uninstalled, so the pre-toggle assertion holds.
        //   - handoffSkillPromptSeen=YES → the first-launch alert does
        //     not appear and cover the Settings UI.
        let app = launchApp(
            homePath: home,
            extraEnv: ["NICE_CLAUDE_OVERRIDE": "/bin/cat"],
            extraArgs: [
                "-installHandoffSkill", "NO",
                "-handoffSkillPromptSeen", "YES",
            ]
        )

        let terminalsRow = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(terminalsRow.waitForExistence(timeout: 8))

        // Open Settings via the sidebar gear. The gear click can
        // occasionally not register (window focus / first-responder
        // races), so retry the click a couple of times until the
        // Settings window's root appears.
        let gear = app.descendants(matching: .any)["sidebar.settings"]
        XCTAssertTrue(gear.waitForExistence(timeout: 5), "sidebar.settings gear must exist")
        let settingsRoot = app.descendants(matching: .any)["settings.root"]
        var settingsOpened = false
        for _ in 0..<3 {
            gear.click()
            if settingsRoot.waitForExistence(timeout: 4) {
                settingsOpened = true
                break
            }
        }
        XCTAssertTrue(settingsOpened, "Settings window must open after clicking the gear")

        // The installHandoffSkill toggle lives in the Claude section, whose
        // content only renders once that section is selected — select it
        // first (SettingsSectionRow emits `settings.section.<slug>`).
        let claudeSection = app.descendants(matching: .any)["settings.section.claude"]
        XCTAssertTrue(claudeSection.waitForExistence(timeout: 5),
                      "Settings must have a Claude section row")
        claudeSection.click()

        let toggle = app.descendants(matching: .any)["settings.claude.installHandoffSkill"]
        XCTAssertTrue(
            toggle.waitForExistence(timeout: 5),
            "settings.claude.installHandoffSkill toggle must exist in the Claude settings section"
        )

        let skillFile = self.skillFile(home: home)
        let helperFile = (home as NSString)
            .appendingPathComponent(".nice/nice-handoff.sh")

        // Fresh sandbox HOME → the toggle starts OFF and no files exist.
        XCTAssertFalse(FileManager.default.fileExists(atPath: skillFile),
                       "SKILL.md must not exist before the toggle is turned on")

        // Turn ON → both files appear.
        toggle.click()
        let skillAppeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                FileManager.default.fileExists(atPath: skillFile)
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [skillAppeared], timeout: 5), .completed,
            "Turning the installHandoffSkill toggle ON must create \(skillFile)"
        )
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: helperFile),
            "Turning the toggle ON must also create \(helperFile)"
        )

        // Turn OFF → both files are removed.
        toggle.click()
        let skillGone = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                !FileManager.default.fileExists(atPath: skillFile)
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [skillGone], timeout: 5), .completed,
            "Turning the installHandoffSkill toggle OFF must remove SKILL.md"
        )
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: helperFile),
            "Turning the toggle OFF must also remove nice-handoff.sh"
        )
    }

    // MARK: - 2. First-launch prompt

    func test_firstLaunchPrompt_installButtonCreatesSkillFile() throws {
        // Force the prompt by passing UserDefaults-domain overrides via
        // launchArguments. macOS registers every "-Key Value" pair from
        // launchArguments as a UserDefaults entry visible in .standard,
        // so Tweaks.init reads handoffSkillPromptSeen=false and
        // installHandoffSkill=false regardless of any persisted state,
        // which opens the first-launch gate in AppShellView.task.
        let home = makeFakeHome()
        let app = launchApp(
            homePath: home,
            extraEnv: [
                "NICE_CLAUDE_OVERRIDE": "/bin/cat",
                // Opt back in to the first-launch prompt, which is
                // otherwise suppressed under the UITest harness (see
                // AppShellHost.shouldSuppressFirstLaunchPrompt).
                "NICE_FORCE_FIRST_LAUNCH_PROMPT": "1",
            ],
            extraArgs: [
                "-handoffSkillPromptSeen", "NO",
                "-installHandoffSkill", "NO",
            ]
        )

        let terminalsRow = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(terminalsRow.waitForExistence(timeout: 8),
                      "App must launch and show the Terminals sidebar row")

        // The alert appears automatically once the task block runs.
        let installButton = app.descendants(matching: .any)["handoffPrompt.install"]
        guard installButton.waitForExistence(timeout: 8) else {
            // If this fires the argument-domain UserDefaults override is
            // likely not reaching Tweaks.init — verify installHandoffSkill /
            // handoffSkillPromptSeen are read from UserDefaults.standard.
            XCTFail("handoffPrompt.install button did not appear within 8s.")
            return
        }

        installButton.click()

        // After tapping Install, the skill file must appear under the
        // sandbox HOME (SkillInstaller honors NICE_HOME_OVERRIDE).
        let skillFile = self.skillFile(home: home)
        let skillCreated = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                FileManager.default.fileExists(atPath: skillFile)
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [skillCreated], timeout: 5), .completed,
            "Tapping 'handoffPrompt.install' must create SKILL.md under the sandbox HOME"
        )
    }

    // MARK: - Launch helpers

    private func makeFakeHome() -> String {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-handoff-uitest-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url.path
    }

    @discardableResult
    private func launchApp(
        homePath: String? = nil,
        extraEnv: [String: String] = [:],
        extraArgs: [String] = []
    ) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchArguments += extraArgs

        let home = homePath ?? makeFakeHome()
        app.launchEnvironment["HOME"] = home
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        // Redirect SkillInstaller's ~/.claude and ~/.nice writes into the
        // sandbox HOME so the toggle/prompt never touch the developer's
        // real home. See SkillInstaller.homeBase().
        app.launchEnvironment["NICE_HOME_OVERRIDE"] = home
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"]    { app.launchEnvironment["USER"] = user }
        if let logname = hostEnv["LOGNAME"] { app.launchEnvironment["LOGNAME"] = logname }
        for (k, v) in extraEnv { app.launchEnvironment[k] = v }

        app.launch()
        track(app)
        return app
    }

    // MARK: - File path helpers

    private func skillFile(home: String) -> String {
        (home as NSString).appendingPathComponent(".claude/skills/nice-handoff/SKILL.md")
    }
}
