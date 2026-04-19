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
//     `init` would crash. The current GPU preference is read through
//     a closure rather than captured by value so a Settings toggle
//     can flip it live without rebuilding the view.
//
//  2. Accepts image drops (file URLs from Finder, raw image data
//     from browsers / Messages / Preview) and types the resulting
//     path into the pty. When the hosted app has bracketed paste
//     enabled (DEC 2004 — Claude Code's TUI does), the path is
//     wrapped in `ESC [200~ … ESC [201~` so Claude treats the drop
//     as a paste and substitutes `[Image #N]` instead of echoing the
//     raw characters. With bracketed paste off it falls back to a
//     single-quoted path so spaces survive at a plain zsh prompt.
//

import AppKit
import SwiftTerm

@MainActor
final class NiceTerminalView: LocalProcessTerminalView {
    /// Reads the live "GPU rendering" preference. `nil` means "no
    /// session has wired this up yet" — treated as on, matching the
    /// `Tweaks.gpuRendering` default.
    var gpuPreferenceProvider: (() -> Bool)?

    private static let acceptedDragTypes: [NSPasteboard.PasteboardType] = [
        .fileURL,
        .png,
        .tiff,
    ]

    private static let imageExtensions: Set<String> = [
        "png", "jpg", "jpeg", "gif", "tiff", "tif", "bmp", "webp", "heic", "heif",
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
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        registerForDraggedTypes(Self.acceptedDragTypes)
    }

    /// Re-evaluates the GPU preference and toggles the Metal renderer
    /// to match. No-op when the view isn't yet in a window — the
    /// `viewDidMoveToWindow` override applies the current preference
    /// once attachment happens. Idempotent: SwiftTerm's `setUseMetal`
    /// short-circuits when the renderer is already in the requested state.
    func applyGpuPreference() {
        guard window != nil else { return }
        let desired = gpuPreferenceProvider?() ?? true
        do {
            try setUseMetal(desired)
        } catch {
            // Metal unavailable (deviceUnavailable on VMs / CI).
            // Stay on the CG path silently — `setUseMetal(false)`
            // is also a no-op when Metal was never enabled.
            NSLog("NiceTerminalView: Metal renderer unavailable, falling back to CoreGraphics: \(error)")
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        applyGpuPreference()
    }

    // MARK: - Image drag-and-drop

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        hasImagePayload(sender) ? .copy : super.draggingEntered(sender)
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        hasImagePayload(sender) ? .copy : super.draggingUpdated(sender)
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        let paths = extractImagePaths(from: sender.draggingPasteboard)
        guard !paths.isEmpty else { return super.performDragOperation(sender) }

        let bytes: [UInt8]
        if getTerminal().bracketedPasteMode {
            // Unquoted, wrapped in bracketed-paste markers — Claude
            // Code (and other paste-aware TUIs) treat this as a single
            // pasted path and can swap it for `[Image #N]`. Adding
            // quotes here would defeat the filepath detection.
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

    private func hasImagePayload(_ sender: any NSDraggingInfo) -> Bool {
        let pb = sender.draggingPasteboard
        if let urls = pb.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], urls.contains(where: Self.isImageFile) {
            return true
        }
        return pb.data(forType: .png) != nil || pb.data(forType: .tiff) != nil
    }

    private func extractImagePaths(from pb: NSPasteboard) -> [String] {
        var paths: [String] = []
        if let urls = pb.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL] {
            for url in urls where Self.isImageFile(url) {
                paths.append(url.path)
            }
        }
        if paths.isEmpty,
           let pngData = Self.pngData(from: pb),
           let tempPath = Self.writeDroppedImage(pngData) {
            paths.append(tempPath)
        }
        return paths
    }

    private static func isImageFile(_ url: URL) -> Bool {
        imageExtensions.contains(url.pathExtension.lowercased())
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
