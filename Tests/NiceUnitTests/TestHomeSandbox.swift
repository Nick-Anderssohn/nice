//
//  TestHomeSandbox.swift
//  NiceUnitTests
//
//  Point `$HOME` at a throwaway temp directory for the lifetime of one
//  test. Unit tests that construct `AppState` spawn real pty sessions
//  via `makeSession`; the spawned zsh runs under a per-launch ZDOTDIR
//  whose `.zshenv`/`.zprofile`/`.zshrc` stubs chain back to
//  `$HOME/.zsh*` (see `Sources/Nice/Process/MainTerminalShellInject.swift`).
//  Under the user's real `$HOME`, those chain-backs load their real
//  dotfiles, which tend to fan out into `~/Documents` / `~/Downloads` /
//  `~/Music` (plugin caches, history tools, completion indexers) and
//  trip macOS TCC prompts against the DerivedData test binary on every
//  run.
//
//  Redirecting `$HOME` to an empty temp dir makes the chain-back probes
//  (`[[ -f "$HOME/.zshrc" ]] && source ...`) silently skip, so nothing
//  reaches the protected folders and no prompts appear.
//

import Darwin
import Foundation

/// Swap `$HOME` to a temp directory in setUp and restore it in tearDown.
/// Each instance manages one redirection; create a fresh one per-test.
///
/// Also pins `NICE_APPLICATION_SUPPORT_ROOT` inside the temp home so
/// `SessionStore` lands its `sessions.json` in the sandbox — without
/// this, `FileManager.url(for: .applicationSupportDirectory)` bypasses
/// `$HOME` (it resolves via the user record) and writes to the user's
/// real `~/Library/Application Support/Nice`, letting tests read and
/// potentially corrupt live session state.
final class TestHomeSandbox {
    private let tempHome: URL
    private let originalHome: String?
    private let originalAppSupportRoot: String?

    init() {
        self.tempHome = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-unittest-home-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: tempHome, withIntermediateDirectories: true
        )
        self.originalHome = ProcessInfo.processInfo.environment["HOME"]
        self.originalAppSupportRoot =
            ProcessInfo.processInfo.environment["NICE_APPLICATION_SUPPORT_ROOT"]
        setenv("HOME", tempHome.path, 1)
        let appSupport = tempHome
            .appendingPathComponent("Library/Application Support", isDirectory: true)
        setenv("NICE_APPLICATION_SUPPORT_ROOT", appSupport.path, 1)
    }

    /// Restore the prior `$HOME` and remove the temp directory. Safe to
    /// call from tearDown regardless of test outcome.
    func teardown() {
        if let originalHome {
            setenv("HOME", originalHome, 1)
        } else {
            unsetenv("HOME")
        }
        if let originalAppSupportRoot {
            setenv("NICE_APPLICATION_SUPPORT_ROOT", originalAppSupportRoot, 1)
        } else {
            unsetenv("NICE_APPLICATION_SUPPORT_ROOT")
        }
        try? FileManager.default.removeItem(at: tempHome)
    }
}
