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

    /// Captured `startProcess` arguments waiting for AppKit to lay this
    /// view out at its real frame. See `armDeferredSpawn(...)` for the
    /// motivation (TL;DR: spawning while `frame == .zero` makes the pty
    /// boot at SwiftTerm's 80×25 fallback, the shell prints its first
    /// prompt at the wrong cols, and the eventual real-geometry resize
    /// reflows it into a row-1 startup quirk).
    struct PendingSpawn: Equatable {
        let executable: String
        let args: [String]
        let environment: [String]?
        let execName: String?
        let currentDirectory: String?
    }
    /// Getters at `internal` so `@testable import Nice` can inspect
    /// the gate state directly without forking a real child. Setters
    /// are private — the only legitimate state transitions are
    /// "armed" (`armDeferredSpawn`), "fired" (`firePendingSpawnIfReady`),
    /// and "cancelled" (`cancelPendingSpawn`); production code
    /// outside this file should reach for those, not poke the fields.
    private(set) var pendingSpawn: PendingSpawn?
    private(set) var hasFiredPendingSpawn = false

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
        registerForDraggedTypes(Self.acceptedDragTypes)
        useStandardAnsi256Palette()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        registerForDraggedTypes(Self.acceptedDragTypes)
        useStandardAnsi256Palette()
    }

    /// Override SwiftTerm's `.base16Lab` default for the 256-color
    /// palette. That strategy interpolates the 240-entry color cube
    /// (codes 16–255) from the 16 theme colors via LAB, producing
    /// noticeably desaturated values compared to every other terminal
    /// emulator. Powerlevel10k / starship glyphs are typically painted
    /// with 256-color codes, so under `.base16Lab` the prompt reads
    /// washed out next to Apple Terminal / iTerm2 / Ghostty even when
    /// the 16 base ANSI colors are matched. `.xterm` uses the canonical
    /// xterm cube (the 0x00/0x5f/0x87/0xaf/0xd7/0xff levels) that those
    /// terminals all converge on.
    private func useStandardAnsi256Palette() {
        getTerminal().ansi256PaletteStrategy = .xterm
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
            // Disable Apple's `setShouldSmoothFonts` stem-darkening.
            // SwiftTerm's atlas-based GPU renderer applies the dilation
            // once at rasterization and then composites the cached
            // mask, which empirically over-thickens glyphs vs.
            // Apple Terminal's per-draw CG path. Turning smoothing
            // off makes Nice's strokes read closer to Terminal.app
            // (whose own per-draw CG dilation is in fact lighter than
            // SwiftTerm's atlas-baked version), at the cost of
            // matching Ghostty/Alacritty's thin aesthetic on glyphs
            // that depend on dilation for body. The Metal rasterizer
            // reads this flag each frame.
            fontSmoothing = false
        } catch {
            NSLog("NiceTerminalView: Metal renderer unavailable, falling back to CoreGraphics: \(error)")
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        enableGpuRendering()
        smoothScrollingEnabled = true
        claimFocusIfRequested()
        // Belt-and-suspenders: in the (rare) ordering where AppKit
        // assigns the real frame before window attachment, our
        // `setFrameSize` override would have skipped the spawn on the
        // `window != nil` gate. Re-check here so attachment unblocks
        // a view that's already laid out.
        firePendingSpawnIfReady()
    }

    override func viewDidMoveToSuperview() {
        super.viewDidMoveToSuperview()
        claimFocusIfRequested()
    }

    // MARK: - Deferred shell spawn

    /// Capture the shell-spawn parameters and hold them until the view
    /// has a real frame. Use this instead of `startProcess(...)` from
    /// pane-creation code paths (see `TabPtySession`).
    ///
    /// SwiftUI calls `NSViewRepresentable.makeNSView` with a freshly
    /// constructed `NiceTerminalView(frame: .zero)`. If we spawn the
    /// pty now, SwiftTerm's `setupOptions` falls back to
    /// `TerminalOptions.default` (80×25), `getWindowSize()` returns
    /// `winsize(rows: 25, cols: 80)`, and the pty's first
    /// `TIOCSWINSZ` is wrong. The shell prints its first prompt at 80
    /// cols. Then AppKit lays the view out at the real frame, the
    /// resize coalescer fires `terminal.resize`, reflow runs against
    /// the already-printed prompt, and (when the merged content
    /// overflows the new cols) the cursor lands on row 1 — leaving
    /// stranded fragments above the shell's `\r\e[J` redraw.
    ///
    /// Deferring until the first non-zero `setFrameSize` lets the pty
    /// boot at the correct cols on its very first ioctl. No reflow,
    /// no merge, no race.
    func armDeferredSpawn(
        executable: String,
        args: [String],
        environment: [String]?,
        execName: String?,
        currentDirectory: String?
    ) {
        // Single-arm contract. Calling twice would either silently
        // clobber a still-pending spawn (the original args never run)
        // or, worse, set `pendingSpawn` after the gate has already
        // fired (`hasFiredPendingSpawn == true`) — in which case the
        // new args would never run and the pane would just sit there
        // with a stale spawn. Hard-fail in development so this gets
        // caught immediately at the call site rather than as a
        // confusing silent miss in production.
        precondition(
            !hasFiredPendingSpawn && pendingSpawn == nil,
            "armDeferredSpawn called twice on the same NiceTerminalView"
        )
        pendingSpawn = PendingSpawn(
            executable: executable,
            args: args,
            environment: environment,
            execName: execName,
            currentDirectory: currentDirectory
        )
        // Cheap insurance: if AppKit somehow already laid us out
        // (recycled view, future code path), don't sit on the args.
        firePendingSpawnIfReady()
    }

    /// AppKit calls this whenever the layout system assigns a new
    /// frame. The first such call with a non-zero size is our cue
    /// that the view has real geometry. To make sure
    /// `terminal.cols × rows` reflects that geometry *before* the pty
    /// is forked, we briefly disable SwiftTerm's resize-debounce so
    /// `super.setFrameSize` applies the resize synchronously through
    /// `processSizeChange → applySizeChange → terminal.resize`. The
    /// 200 ms coalescer is restored immediately after — we only want
    /// the synchronous path for this one bootstrap apply, runtime
    /// fast-drag bursts still benefit from coalescing.
    override func setFrameSize(_ newSize: NSSize) {
        let needsImmediateApply = pendingSpawn != nil
            && !hasFiredPendingSpawn
            && newSize.width > 0
            && newSize.height > 0
        let savedDebounceMs = resizeDebounceMs
        if needsImmediateApply {
            resizeDebounceMs = 0
        }
        // `defer` so any future SwiftTerm change that introduces a
        // throw-through path can't strand the debounce at zero. The
        // restore is observably equivalent to running it inline today
        // because `super.setFrameSize` is synchronous on the main
        // thread and no timer can dispatch in the middle of its call
        // graph.
        defer {
            if needsImmediateApply {
                resizeDebounceMs = savedDebounceMs
            }
        }
        super.setFrameSize(newSize)
        firePendingSpawnIfReady()
    }

    /// Cancel a captured-but-unfired deferred spawn. Returns `true`
    /// iff the gate was armed and not yet fired — i.e. there were
    /// args to drop. No-op (returns `false`) once the gate has
    /// fired or before any spawn was armed.
    ///
    /// `TabPtySession.terminatePane` calls this when tearing down a
    /// pane whose pty never started. The cancellation is what stops
    /// a layout pass mid-teardown from forking a child after we've
    /// declared the pane gone — `firePendingSpawnIfReady` short-
    /// circuits when `pendingSpawn == nil`. Centralising the
    /// transition here (rather than letting callers poke the field)
    /// gives the cancellation a named callsite and keeps the gate's
    /// state machine entirely owned by this view.
    @discardableResult
    func cancelPendingSpawn() -> Bool {
        guard pendingSpawn != nil, !hasFiredPendingSpawn else { return false }
        pendingSpawn = nil
        return true
    }

    /// Single readiness gate. Fires the captured spawn exactly once,
    /// when the view has a non-zero frame *and* is in a window. The
    /// `process.running` guard inside SwiftTerm's `sizeChanged`
    /// handler already protects us against the synchronous
    /// `terminal.resize` triggering an ioctl on a non-existent pty
    /// (it bails when `process.running == false`), so calling
    /// `startProcess` *after* the synchronous resize is the right
    /// order.
    private func firePendingSpawnIfReady() {
        guard !hasFiredPendingSpawn,
              let spawn = pendingSpawn,
              window != nil,
              frame.width > 0, frame.height > 0
        else { return }
        hasFiredPendingSpawn = true
        pendingSpawn = nil
        startProcess(
            executable: spawn.executable,
            args: spawn.args,
            environment: spawn.environment,
            execName: spawn.execName,
            currentDirectory: spawn.currentDirectory
        )
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
