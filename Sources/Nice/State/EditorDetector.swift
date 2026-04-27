//
//  EditorDetector.swift
//  Nice
//
//  Probes the user's PATH at startup for well-known terminal editors
//  (vim, nvim, hx, …) so the File Explorer's "Open in Editor Pane"
//  context menu surfaces them without the user having to configure
//  anything in Settings. Detection runs in a single login-interactive
//  zsh invocation so the same PATH the spawned editor pane sees
//  (Homebrew, asdf, mise, etc.) is respected. Results are session-only
//  (not persisted) — re-scanned on every launch off the main thread.
//

import CryptoKit
import Foundation

/// Runs a shell script and returns its stdout. Production wires in
/// `EditorDetector.zshLoginRunner`; tests inject deterministic
/// closures (canned output, simulated timeouts, throwing). The
/// closure must be `@Sendable` because it executes off the main
/// actor inside `EditorDetector.performScan`'s detached task.
typealias ShellRunner = @Sendable (_ script: String, _ timeout: TimeInterval) throws -> String

@MainActor
final class EditorDetector: ObservableObject {
    /// Detected editors discovered by the most recent `scan()`. Empty
    /// before the first scan completes; UI should treat this as
    /// authoritative once published.
    @Published private(set) var detected: [EditorCommand] = []

    /// Curated list of well-known terminal editors. `binary` is what
    /// `command -v` is asked about; `command` is the full invocation
    /// (with args where needed, e.g. `emacs -nw` to stay in the
    /// terminal instead of spawning a windowed Emacs).
    nonisolated static let candidates: [Candidate] = [
        Candidate(binary: "vim",   name: "Vim",     command: "vim"),
        Candidate(binary: "nvim",  name: "Neovim",  command: "nvim"),
        Candidate(binary: "hx",    name: "Helix",   command: "hx"),
        Candidate(binary: "nano",  name: "Nano",    command: "nano"),
        Candidate(binary: "emacs", name: "Emacs",   command: "emacs -nw"),
        Candidate(binary: "micro", name: "Micro",   command: "micro"),
        Candidate(binary: "kak",   name: "Kakoune", command: "kak"),
    ]

    struct Candidate: Sendable, Hashable {
        let binary: String
        let name: String
        let command: String
    }

    /// Wall-clock budget the production runner gives a single zsh
    /// invocation before SIGTERM-ing it. 5 s is well over a normal
    /// rc-file load (typically ~100 ms even on a noisy zsh) but short
    /// enough that a broken `.zshrc` doesn't push detection past the
    /// user's first interaction with the menu.
    nonisolated static let defaultTimeout: TimeInterval = 5

    private let shellRunner: ShellRunner

    /// Production callers use the default zsh runner; tests pass a
    /// stub closure to simulate canned output, throwing, or hangs
    /// without spawning a real shell.
    init(shellRunner: @escaping ShellRunner = EditorDetector.zshLoginRunner) {
        self.shellRunner = shellRunner
    }

    /// Fire-and-forget: kicks off detection on a background queue and
    /// publishes the result on the main actor when it returns. Safe
    /// to call multiple times — later calls just refresh the
    /// published list.
    func scan() {
        Task.detached(priority: .utility) { [shellRunner] in
            let found = Self.runDetection(
                candidates: Self.candidates,
                shellRunner: shellRunner,
                timeout: Self.defaultTimeout
            )
            await MainActor.run { [weak self] in
                self?.detected = found
            }
        }
    }

    /// Awaitable form for tests. Runs `shellRunner` directly on the
    /// caller's task and publishes the result on the main actor. Use
    /// `scan()` from production code; this entry point exists so
    /// tests don't have to poll `@Published` Combine state.
    func performScan() async {
        let runner = shellRunner
        let found = await Task.detached(priority: .utility) {
            Self.runDetection(
                candidates: Self.candidates,
                shellRunner: runner,
                timeout: Self.defaultTimeout
            )
        }.value
        self.detected = found
    }

    /// Pure-ish detection: feeds the curated candidate set to the
    /// supplied runner, parses what came back. The runner is the only
    /// side-effect; everything else is deterministic, so this method
    /// is testable end-to-end with a stub `shellRunner`.
    nonisolated static func runDetection(
        candidates: [Candidate],
        shellRunner: ShellRunner,
        timeout: TimeInterval
    ) -> [EditorCommand] {
        let script = buildProbeScript(candidates: candidates)
        let output = (try? shellRunner(script, timeout)) ?? ""
        return parseDetected(output: output, candidates: candidates)
    }

    /// Build the `for c in vim nvim hx; …` probe script. Carved out
    /// so a unit test can pin the exact wire format independent of
    /// either the runner or the parser.
    nonisolated static func buildProbeScript(candidates: [Candidate]) -> String {
        let names = candidates.map(\.binary).joined(separator: " ")
        return "for c in \(names); do command -v \"$c\" >/dev/null 2>&1 && echo \"$c\"; done"
    }

    /// Splits the script output into a set of binary names and projects
    /// the candidates to `EditorCommand`s with deterministic UUIDs.
    /// Pulled out for direct unit testing without spawning a shell.
    nonisolated static func parseDetected(
        output: String,
        candidates: [Candidate]
    ) -> [EditorCommand] {
        let present = Set(
            output
                .split(whereSeparator: \.isNewline)
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }
        )
        return candidates
            .filter { present.contains($0.binary) }
            .map { EditorCommand(
                id: detectedId(forBinary: $0.binary),
                name: $0.name,
                command: $0.command
            )}
    }

    /// Stable, deterministic UUID per detected binary. Hashes a
    /// namespace-qualified seed via SHA-256 and takes the first 16
    /// bytes — collisions across the curated candidate list are
    /// vanishingly unlikely. Detected ids are session-only and never
    /// persisted, but they need to round-trip through the
    /// `(url, editorId)` -> `openInEditorPane` callback, so they must
    /// be stable within a session.
    nonisolated static func detectedId(forBinary binary: String) -> UUID {
        // Namespace prefix keeps detected ids from ever colliding with
        // user-configured editor UUIDs (random in the version space)
        // even if they're treated as the same dictionary keys.
        let seed = "nice.editor-detector:\(binary)"
        let digest = SHA256.hash(data: Data(seed.utf8))
        let bytes = Array(digest.prefix(16))
        return UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }
}

// MARK: - Production zsh runner

extension EditorDetector {
    /// Production `ShellRunner`: spawns `/bin/zsh -ilc <script>`,
    /// captures stdout, enforces a wall-clock timeout. Login-
    /// interactive (`-il`) so the user's `.zshenv` / `.zshrc` PATH
    /// additions are respected — Nice launched from Finder/Spotlight
    /// otherwise inherits only macOS's default PATH.
    ///
    /// The watchdog SIGTERMs the child if it doesn't exit within
    /// `timeout` seconds, returning whatever was captured up to
    /// that point (typically empty). Throws only on `Process.run()`
    /// failure.
    ///
    /// Watchdog cancellation: a `DispatchSourceTimer` is scheduled
    /// for the deadline and explicitly `cancel()`ed as soon as
    /// `waitUntilExit` returns, so the SIGTERM never fires after a
    /// clean exit. This eliminates the PID-recycle hazard the
    /// `asyncAfter` form had — `kill` on a recycled pid would have
    /// signalled an unrelated process belonging to the same uid.
    @Sendable nonisolated static func zshLoginRunner(
        script: String,
        timeout: TimeInterval
    ) throws -> String {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/zsh")
        proc.arguments = ["-ilc", script]
        let stdout = Pipe()
        proc.standardOutput = stdout
        proc.standardError = Pipe()

        try proc.run()

        let pid = proc.processIdentifier
        let timer = DispatchSource.makeTimerSource(queue: .global())
        timer.schedule(deadline: .now() + timeout)
        timer.setEventHandler {
            kill(pid, SIGTERM)
        }
        timer.resume()
        defer { timer.cancel() }

        proc.waitUntilExit()
        let data = stdout.fileHandleForReading.readDataToEndOfFile()
        return String(data: data, encoding: .utf8) ?? ""
    }
}
