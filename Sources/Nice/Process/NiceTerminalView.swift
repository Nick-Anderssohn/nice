//
//  NiceTerminalView.swift
//  Nice
//
//  Subclass of `LocalProcessTerminalView` that carries the Nice-side
//  pane behaviour:
//
//  1. Opts the terminal into SwiftTerm's Metal renderer (PR #484,
//     March 2026) once it's attached to a window. The Metal path
//     requires window attachment because the `MTKView` it installs
//     needs a live `CAMetalLayer`; calling `setUseMetal(true)` from
//     `init` would crash. Metal is always on; if the device can't
//     create one (e.g. on a VM or some CI runner), SwiftTerm falls
//     back to its CoreGraphics path silently.
//
//  2. Accepts file drops (any file URL from Finder or the in-app
//     File Explorer, plus raw image data from browsers / Messages /
//     Preview) and types the resulting path into the pty. When the
//     hosted app has bracketed paste enabled (DEC 2004 — Claude
//     Code's TUI does), the path is wrapped in `ESC [200~ … ESC
//     [201~` so Claude treats the drop as a paste — and for image
//     paths, substitutes `[Image #N]` instead of echoing the raw
//     characters. With bracketed paste off it falls back to a
//     single-quoted path so spaces survive at a plain zsh prompt.
//

import AppKit
import SwiftTerm

@MainActor
final class NiceTerminalView: LocalProcessTerminalView {
    /// Latch set by `TerminalHost.makeNSView` when this pane is the one
    /// SwiftUI is about to mount as the active pane. Consumed on the
    /// next `viewDidMoveToWindow` / `viewDidMoveToSuperview` so focus
    /// transfers atomically during AppKit's reattach — closing the race
    /// where a key pressed between the old pane's exit and
    /// `updateNSView`'s async `makeFirstResponder` drops off the end of
    /// the responder chain and beeps.
    var wantsFocusOnAttach: Bool = false

    /// Fires exactly once, the first time the hosted process writes any
    /// byte to the pty. Cleared on first invocation so a later chunk
    /// can't retrigger it. `TabPtySession` uses this to dismiss the
    /// "Launching…" overlay — the placeholder we render while a slow
    /// child (e.g. `claude -w foo` with heavy post-checkout git hooks)
    /// is still silent. The callback runs on the main actor because
    /// SwiftTerm's pty read loop hops to `DispatchQueue.main` before
    /// invoking `dataReceived(slice:)`.
    var onFirstData: (@MainActor () -> Void)?

    private static let acceptedDragTypes: [NSPasteboard.PasteboardType] = [
        .fileURL,
        .png,
        .tiff,
    ]

    // ESC [ 2 0 0 ~ / ESC [ 2 0 1 ~ — DEC mode 2004 bracketed-paste
    // markers. Hardcoded here rather than reusing SwiftTerm's
    // `EscapeSequences.bracketedPasteStart`, which is declared
    // `public static var` and therefore not Swift 6 concurrency-safe
    // to read from a MainActor context.
    private static let bracketedPasteStart: [UInt8] = [0x1b, 0x5b, 0x32, 0x30, 0x30, 0x7e]
    private static let bracketedPasteEnd: [UInt8] = [0x1b, 0x5b, 0x32, 0x30, 0x31, 0x7e]

    override init(frame: NSRect) {
        super.init(frame: frame)
        applyNiceTerminalOptions()
        registerForDraggedTypes(Self.acceptedDragTypes)
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        applyNiceTerminalOptions()
        registerForDraggedTypes(Self.acceptedDragTypes)
    }

    /// Flips SwiftTerm options that Nice wants different from the
    /// upstream defaults. Must run after `super.init(frame:)` (which
    /// constructs the underlying `Terminal`) and before the first
    /// reflow.
    ///
    /// Currently:
    /// - `reflowCursorLine = true` so wrapped blocks containing the
    ///   cursor are joined (and the cursor is translated into the
    ///   merged layout) on widen. Without this, the brief tiny
    ///   initial layout of a fresh `NiceTerminalView` — before the
    ///   sidebar/window geometry resolves — causes the shell's first
    ///   prompt to fragment across many wrapped 2-char rows; on
    ///   widen those fragments stay stranded above the redrawn
    ///   prompt because zsh/bash/fish only clear below cursor on
    ///   SIGWINCH (`\r\e[J`). This produces the "stack of partial
    ///   prompts at startup" variant of the resize-duplication bug.
    ///   The flag is upstream-default `false` to match xterm
    ///   semantics; we opt in.
    private func applyNiceTerminalOptions() {
        // Empirically required (verified 2026-05-07): with
        // `reflowCursorLine = false`, startup duplication still
        // reproduces — coalescing the resize bursts isn't enough
        // because the shell prints its first prompt into the brief
        // tiny initial buffer BEFORE the coalesced apply fires. The
        // merge on widen + cursor translation is what reattaches
        // the fragmented prompt rows into the parent line, so the
        // shell's `\r\e[J` redraw doesn't leave them stranded above.
        // SwiftTerm default is `false` (xterm semantics); we opt in.
        terminal.reflowCursorLine = true
    }

    /// Enables SwiftTerm's Metal renderer. No-op when the view isn't
    /// yet in a window — `viewDidMoveToWindow` calls this once
    /// attachment happens. Idempotent: SwiftTerm's `setUseMetal`
    /// short-circuits when the renderer is already in the requested
    /// state. Falls back silently to CoreGraphics on devices where
    /// Metal isn't available (VMs, some CI runners).
    private func enableGpuRendering() {
        guard window != nil else { return }
        do {
            try setUseMetal(true)
        } catch {
            NSLog("NiceTerminalView: Metal renderer unavailable, falling back to CoreGraphics: \(error)")
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        enableGpuRendering()
        smoothScrollingEnabled = true
        claimFocusIfRequested()
    }

    override func viewDidMoveToSuperview() {
        super.viewDidMoveToSuperview()
        claimFocusIfRequested()
    }

    /// If a host marked us as the incoming focus target, grab first
    /// responder now that AppKit has us live in a window. One-shot: the
    /// latch clears on first success so later reattaches (tab switches
    /// while we're already focused, etc.) don't fight the user.
    private func claimFocusIfRequested() {
        guard wantsFocusOnAttach, let window, window.firstResponder !== self else { return }
        if window.makeFirstResponder(self) {
            wantsFocusOnAttach = false
        }
    }

    // MARK: - First-byte hook

    /// SwiftTerm's `LocalProcessTerminalView` forwards pty bytes to the
    /// renderer here. Call `super` first so the byte actually paints
    /// before the overlay lifts — otherwise there's a visible flash of
    /// empty terminal between dismiss and first-paint — then fire the
    /// one-shot `onFirstData` callback. Nil'd immediately so a later
    /// chunk can't retrigger; skipped entirely for empty slices (some
    /// pty reads deliver a zero-length chunk on EOF).
    override func dataReceived(slice: ArraySlice<UInt8>) {
        super.dataReceived(slice: slice)
        guard !slice.isEmpty, let callback = onFirstData else { return }
        onFirstData = nil
        callback()
    }

    // MARK: - File drag-and-drop

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        hasFilePayload(sender) ? .copy : super.draggingEntered(sender)
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        hasFilePayload(sender) ? .copy : super.draggingUpdated(sender)
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        let paths = extractDroppedPaths(from: sender.draggingPasteboard)
            .filter(Self.isSafePath)
        guard !paths.isEmpty else { return super.performDragOperation(sender) }

        let bytes: [UInt8]
        if getTerminal().bracketedPasteMode {
            // Unquoted, wrapped in bracketed-paste markers — Claude
            // Code (and other paste-aware TUIs) treat this as a single
            // pasted path and can swap it for `[Image #N]` (or other
            // attachment indicators). Adding quotes here would defeat
            // the filepath detection.
            var buf = Self.bracketedPasteStart
            buf.append(contentsOf: Array(paths.joined(separator: " ").utf8))
            buf.append(contentsOf: Self.bracketedPasteEnd)
            bytes = buf
        } else {
            // Shell prompt with bracketed paste off — quote the path so
            // spaces survive, and pad with spaces so it separates from
            // surrounding text. No newline: drop must not auto-submit.
            let quoted = paths.map { shellSingleQuote($0) }.joined(separator: " ")
            bytes = Array((" " + quoted + " ").utf8)
        }
        send(data: ArraySlice(bytes))
        return true
    }

    private func hasFilePayload(_ sender: any NSDraggingInfo) -> Bool {
        let pb = sender.draggingPasteboard
        if let urls = pb.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], !urls.isEmpty {
            return true
        }
        return pb.data(forType: .png) != nil || pb.data(forType: .tiff) != nil
    }

    private func extractDroppedPaths(from pb: NSPasteboard) -> [String] {
        var paths: [String] = []
        if let urls = pb.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL] {
            for url in urls {
                paths.append(url.path)
            }
        }
        // Raw image data fallback (browser drag, Preview screenshot,
        // Messages thumbnail) — no file URL on the pasteboard, so we
        // transcode to PNG and stash in a cache directory, then paste
        // the temp path. Only runs when no file URLs were present;
        // otherwise the explicit file drop wins.
        if paths.isEmpty,
           let pngData = Self.pngData(from: pb),
           let tempPath = Self.writeDroppedImage(pngData) {
            paths.append(tempPath)
        }
        return paths
    }

    /// Reject paths containing C0 control bytes (ESC, LF, CR, tab, etc.)
    /// or DEL. macOS filenames legally contain those bytes; letting them
    /// through would break out of the `ESC [200~ … ESC [201~` paste
    /// frame we wrap the path in, delivering crafted input to the TUI
    /// as if typed at the prompt.
    private static func isSafePath(_ path: String) -> Bool {
        for scalar in path.unicodeScalars {
            if scalar.value < 0x20 || scalar.value == 0x7f { return false }
        }
        return true
    }

    /// Re-encode whatever's on the pasteboard to PNG so the file Claude
    /// reads is in a format every image library handles. TIFF is the
    /// canonical AppKit representation; browsers usually drop either
    /// PNG directly or a TIFF that NSBitmapImageRep can transcode.
    private static func pngData(from pb: NSPasteboard) -> Data? {
        if let tiff = pb.data(forType: .tiff),
           let rep = NSBitmapImageRep(data: tiff),
           let png = rep.representation(using: .png, properties: [:])
        {
            return png
        }
        return pb.data(forType: .png)
    }

    private static func writeDroppedImage(_ data: Data) -> String? {
        let cachesRoot = NSSearchPathForDirectoriesInDomains(
            .cachesDirectory, .userDomainMask, true
        ).first ?? NSTemporaryDirectory()
        let dir = (cachesRoot as NSString)
            .appendingPathComponent("Nice/dropped-images")
        let fm = FileManager.default
        do {
            try fm.createDirectory(
                atPath: dir, withIntermediateDirectories: true, attributes: nil
            )
        } catch {
            return nil
        }
        let path = (dir as NSString)
            .appendingPathComponent("\(UUID().uuidString).png")
        do {
            try data.write(to: URL(fileURLWithPath: path))
            return path
        } catch {
            return nil
        }
    }
}
