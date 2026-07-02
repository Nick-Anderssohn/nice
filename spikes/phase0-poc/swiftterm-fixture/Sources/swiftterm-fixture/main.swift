//
//  swiftterm-fixture — AA/gamma spike (rank-1), SwiftTerm reference side.
//
//  Hosts the SwiftTerm fork's TerminalView with the Metal renderer enabled and
//  Nice's EXACT shipping config (extracted from Sources/Nice, not guessed):
//
//    * font chain first hit: SFMono-Regular @ 13pt
//      (TabPtySession.terminalFont / FontSettings.defaultTerminalSize).
//      The fixture registers Terminal.app's bundled SF-Mono-Regular.otf /
//      SF-Mono-Bold.otf process-scoped so "SFMono-Regular" resolves even on
//      machines without SF Mono installed; the GPUI side loads the SAME files.
//    * fontSmoothing = false            (NiceTerminalView.swift:216)
//    * ansi256PaletteStrategy = .xterm  (NiceTerminalView.useStandardAnsi256Palette)
//    * steady block cursor              (NiceTerminalView.useSteadyCursor)
//      — and the scene hides the cursor via CSI ?25l anyway.
//    * smoothScrollingEnabled = false   (Nice default, Tweaks.smoothScrolling)
//    * theme: Nice Default Light/Dark   (BuiltInTerminalThemes.swift), applied
//      exactly like TabPtySession.applyTerminalTheme (nativeBackground/
//      Foreground + installColors of the 16 ANSI entries).
//
//  Axes (CLI):
//    --theme light|dark
//    --curve appleApprox|identity   (the fork's TextCompositionStrategy —
//                                    .appleApprox is SwiftTerm's macOS default
//                                    and Nice's shipping value)
//    --scene PATH --out DIR
//    [--font PSNAME] [--font-px N] [--cols N] [--rows N]
//
//  Readback: CAMetalLayer.nextDrawable is swizzled to stash the most recent
//  drawable (and force framebufferOnly=false before the pool is built). After
//  the scene settles, the presented drawable's texture is blitted into a
//  shared MTLBuffer and written as PNG. The scene is static, so reading the
//  last presented drawable is race-free.
//
//  REQUIRES A DISPLAY. Never run from a sandboxed subagent; the main session
//  runs it per ../aa-gamma/RUNBOOK.md.
//

import AppKit
import CoreText
import Metal
import MetalKit
import ObjectiveC
import QuartzCore
import SwiftTerm

// MARK: - CAMetalLayer nextDrawable swizzle (readback tap)

final class DrawableStash: @unchecked Sendable {
    static let shared = DrawableStash()
    var last: CAMetalDrawable?
    var count = 0
}

extension CAMetalLayer {
    @objc dynamic func aaGamma_nextDrawable() -> CAMetalDrawable? {
        // Must be false BEFORE the drawable pool is created so the textures
        // are blit-readable (also disables lossless framebuffer compression).
        if framebufferOnly {
            framebufferOnly = false
        }
        // Swizzled: calls the ORIGINAL nextDrawable.
        let d = aaGamma_nextDrawable()
        if let d {
            DrawableStash.shared.last = d
            DrawableStash.shared.count += 1
        }
        return d
    }
}

func installDrawableTap() {
    guard
        let orig = class_getInstanceMethod(CAMetalLayer.self, #selector(CAMetalLayer.nextDrawable)),
        let repl = class_getInstanceMethod(
            CAMetalLayer.self, #selector(CAMetalLayer.aaGamma_nextDrawable))
    else {
        fatalError("failed to install CAMetalLayer.nextDrawable tap")
    }
    method_exchangeImplementations(orig, repl)
}

// MARK: - Nice's shipping themes (Sources/Nice/Theme/BuiltInTerminalThemes.swift)

struct FixtureTheme {
    let bg: (UInt8, UInt8, UInt8)
    let fg: (UInt8, UInt8, UInt8)
    let ansi: [(UInt8, UInt8, UInt8)]
}

let niceLight = FixtureTheme(
    bg: (0xff, 0xfc, 0xfc),
    fg: (0x17, 0x13, 0x0f),
    ansi: [
        (0x17, 0x13, 0x0f), (0xb7, 0x40, 0x20), (0x30, 0x81, 0x30), (0xa6, 0x71, 0x0d),
        (0x28, 0x60, 0xaf), (0x9b, 0x3b, 0x98), (0x23, 0x85, 0x9b), (0x7e, 0x76, 0x6c),
        (0x5c, 0x53, 0x48), (0xd4, 0x4c, 0x25), (0x38, 0x9f, 0x38), (0xc4, 0x8c, 0x18),
        (0x34, 0x75, 0xcd), (0xb5, 0x47, 0xaf), (0x28, 0x9c, 0xb2), (0x17, 0x13, 0x0f),
    ]
)

let niceDark = FixtureTheme(
    bg: (0x09, 0x07, 0x05),
    fg: (0xf4, 0xf0, 0xef),
    ansi: [
        (0x09, 0x07, 0x05), (0xc2, 0x36, 0x21), (0x25, 0xbc, 0x24), (0xad, 0xad, 0x27),
        (0x49, 0x6e, 0xe1), (0xd3, 0x38, 0xd3), (0x33, 0xbb, 0xc8), (0xcb, 0xcc, 0xcd),
        (0x81, 0x83, 0x83), (0xfc, 0x5b, 0x47), (0x31, 0xe7, 0x22), (0xea, 0xd4, 0x23),
        (0x6c, 0x8d, 0xff), (0xf9, 0x65, 0xf8), (0x64, 0xe6, 0xe6), (0xf4, 0xf0, 0xef),
    ]
)

// Same conversions Nice uses (TerminalTheme.swift): NSColor srgb /255,
// SwiftTerm.Color UInt16 * 257.
func nsColor(_ c: (UInt8, UInt8, UInt8)) -> NSColor {
    NSColor(
        srgbRed: CGFloat(c.0) / 255, green: CGFloat(c.1) / 255, blue: CGFloat(c.2) / 255, alpha: 1)
}
func stColor(_ c: (UInt8, UInt8, UInt8)) -> SwiftTerm.Color {
    SwiftTerm.Color(red: UInt16(c.0) * 257, green: UInt16(c.1) * 257, blue: UInt16(c.2) * 257)
}

// MARK: - args

struct Args {
    var scene = ""
    var out = ""
    var theme = "light"
    var curve = "appleApprox"
    var font = "SFMono-Regular"
    var fontPx: CGFloat = 13
    var cols = 60
    var rows = 16
}

func parseArgs() -> Args {
    var a = Args()
    var it = CommandLine.arguments.dropFirst().makeIterator()
    while let k = it.next() {
        func val() -> String {
            guard let v = it.next() else { fatalError("missing value for \(k)") }
            return v
        }
        switch k {
        case "--scene": a.scene = val()
        case "--out": a.out = val()
        case "--theme": a.theme = val()
        case "--curve": a.curve = val()
        case "--font": a.font = val()
        case "--font-px": a.fontPx = CGFloat(Double(val())!)
        case "--cols": a.cols = Int(val())!
        case "--rows": a.rows = Int(val())!
        default: fatalError("unknown arg \(k)")
        }
    }
    guard !a.scene.isEmpty, !a.out.isEmpty else {
        fatalError(
            "usage: swiftterm-fixture --scene scene.bin --out DIR --theme light|dark --curve appleApprox|identity"
        )
    }
    guard a.theme == "light" || a.theme == "dark" else { fatalError("--theme light|dark") }
    guard a.curve == "appleApprox" || a.curve == "identity" else {
        fatalError("--curve appleApprox|identity")
    }
    return a
}

// MARK: - font registration (same files the GPUI side loads)

let fontDir = "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts"

func registerSFMono() {
    for name in ["SF-Mono-Regular.otf", "SF-Mono-Bold.otf"] {
        let url = URL(fileURLWithPath: "\(fontDir)/\(name)") as CFURL
        var err: Unmanaged<CFError>?
        if !CTFontManagerRegisterFontsForURL(url, .process, &err) {
            // Already registered / installed system-wide is fine — the
            // NSFont(name:) lookup below is the real gate.
            FileHandle.standardError.write(
                "WARN: CTFontManagerRegisterFontsForURL(\(name)) failed (may already be available)\n"
                    .data(using: .utf8)!)
        }
    }
}

// MARK: - PNG writing

func writePNG(bgra: [UInt8], width: Int, height: Int, to path: String) {
    var rgba = bgra
    for i in stride(from: 0, to: rgba.count, by: 4) {
        rgba.swapAt(i, i + 2)  // BGRA -> RGBA
    }
    let cs = CGColorSpace(name: CGColorSpace.sRGB)!
    let info = CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.premultipliedLast.rawValue
    let data = Data(rgba)
    let provider = CGDataProvider(data: data as CFData)!
    let img = CGImage(
        width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
        bytesPerRow: width * 4, space: cs, bitmapInfo: CGBitmapInfo(rawValue: info),
        provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
    let rep = NSBitmapImageRep(cgImage: img)
    guard let png = rep.representation(using: .png, properties: [:]) else {
        fatalError("PNG encode failed")
    }
    try! png.write(to: URL(fileURLWithPath: path))
}

// MARK: - app delegate driving the capture

final class FixtureDelegate: NSObject, NSApplicationDelegate {
    let args: Args
    var window: NSWindow!
    var terminalView: TerminalView!

    init(args: Args) {
        self.args = args
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let theme = args.theme == "dark" ? niceDark : niceLight

        guard let font = NSFont(name: args.font, size: args.fontPx) else {
            fatalError("font \(args.font) did not resolve after registration")
        }

        // Cell geometry exactly as SwiftTerm computes it (AppleTerminalView.
        // computeFontDimensions): h = ceil(asc+desc+leading), w = advance('W'),
        // both snapped to the backing pixel grid. Used only to size the window;
        // the view recomputes it internally from the same font.
        let ct = font as CTFont
        let cellHPt = ceil(
            CTFontGetAscent(ct) + CTFontGetDescent(ct) + CTFontGetLeading(ct))
        let glyph = font.glyph(withName: "W")
        let advW = font.advancement(forGlyph: glyph).width
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        let cellW = ceil(advW * scale) / scale
        let cellH = ceil(cellHPt * scale) / scale

        let gridSize = NSSize(
            width: cellW * CGFloat(args.cols), height: cellH * CGFloat(args.rows))

        window = NSWindow(
            contentRect: NSRect(origin: NSPoint(x: 200, y: 200), size: gridSize),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.title = "swiftterm-fixture"
        window.colorSpace = NSColorSpace.sRGB

        terminalView = TerminalView(
            frame: NSRect(origin: .zero, size: gridSize))

        // ---- Nice's shipping config, in Nice's order ----
        terminalView.getTerminal().ansi256PaletteStrategy = .xterm
        terminalView.getTerminal().setCursorStyle(.steadyBlock)
        terminalView.font = font
        terminalView.nativeBackgroundColor = nsColor(theme.bg)
        terminalView.nativeForegroundColor = nsColor(theme.fg)
        terminalView.installColors(theme.ansi.map(stColor))
        terminalView.smoothScrollingEnabled = false

        window.contentView?.addSubview(terminalView)
        window.makeKeyAndOrderFront(nil)

        // Metal renderer, as Nice enables it after window attachment
        // (NiceTerminalView.enableGpuRendering).
        do {
            try terminalView.setUseMetal(true)
        } catch {
            fatalError("Metal renderer unavailable: \(error)")
        }
        // Nice sets this AFTER Metal is enabled (viewDidMoveToWindow).
        terminalView.fontSmoothing = false
        // The spike axis: SwiftTerm's macOS default is .appleApprox (what Nice
        // ships); .identity is the "analytically equivalent to GPUI" claim.
        terminalView.textCompositionStrategy =
            args.curve == "identity" ? .identity : .appleApprox

        // Belt and suspenders: the swizzle also forces this before pool build.
        if let mtk = terminalView.subviews.compactMap({ $0 as? MTKView }).first {
            mtk.framebufferOnly = false
        }

        // Feed the deterministic scene (no pty, no shell).
        let sceneBytes = [UInt8](try! Data(contentsOf: URL(fileURLWithPath: args.scene)))
        terminalView.feed(byteArray: sceneBytes[...])

        NSApp.activate(ignoringOtherApps: true)

        // Let the renderer draw + present the settled scene, then read back.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [self] in
            capture(cellW: cellW, cellH: cellH, advW: advW, scale: scale)
        }
    }

    func capture(cellW: CGFloat, cellH: CGFloat, advW: CGFloat, scale: CGFloat) {
        guard let drawable = DrawableStash.shared.last else {
            fatalError("no drawable was stashed — did the Metal renderer draw?")
        }
        let texture = drawable.texture
        let w = texture.width
        let h = texture.height

        // Blit into a shared buffer (drawable textures may not be CPU-mapped).
        let device = texture.device
        let queue = device.makeCommandQueue()!
        let bytesPerRow = w * 4
        let buffer = device.makeBuffer(length: bytesPerRow * h, options: .storageModeShared)!
        let cmd = queue.makeCommandBuffer()!
        let blit = cmd.makeBlitCommandEncoder()!
        blit.copy(
            from: texture, sourceSlice: 0, sourceLevel: 0,
            sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
            sourceSize: MTLSize(width: w, height: h, depth: 1),
            to: buffer, destinationOffset: 0,
            destinationBytesPerRow: bytesPerRow,
            destinationBytesPerImage: bytesPerRow * h)
        blit.endEncoding()
        cmd.commit()
        cmd.waitUntilCompleted()

        var bgra = [UInt8](repeating: 0, count: bytesPerRow * h)
        memcpy(&bgra, buffer.contents(), bytesPerRow * h)

        let label = "swiftterm-\(args.theme)-\(args.curve)"
        let pngPath = "\(args.out)/\(label).png"
        writePNG(bgra: bgra, width: w, height: h, to: pngPath)

        let theme = args.theme == "dark" ? niceDark : niceLight
        func hex(_ c: (UInt8, UInt8, UInt8)) -> String {
            String(format: "#%02x%02x%02x", c.0, c.1, c.2)
        }
        let meta = """
            {
              "side": "swiftterm-fork",
              "fork_rev": "583551f (phase0-txn-present; Nice pin 5f07dc6 + docs + off-by-default txn present)",
              "theme": "\(args.theme)",
              "curve": "\(args.curve)",
              "fontSmoothing": false,
              "font_ps": "\(args.font)",
              "font_px": \(args.fontPx),
              "cell_w_pt": \(cellW),
              "cell_h_pt": \(cellH),
              "advance_w_pt": \(advW),
              "cols": \(args.cols),
              "rows": \(args.rows),
              "scale_factor": \(scale),
              "drawable_w": \(w),
              "drawable_h": \(h),
              "drawables_seen": \(DrawableStash.shared.count),
              "bg": "\(hex(theme.bg))",
              "fg": "\(hex(theme.fg))"
            }
            """
        try! meta.write(
            toFile: "\(args.out)/\(label).meta.json", atomically: true, encoding: .utf8)

        FileHandle.standardError.write(
            "[swiftterm-fixture] wrote \(pngPath) (\(w)x\(h) @ \(scale)x), cell \(cellW)x\(cellH)pt\n"
                .data(using: .utf8)!)
        exit(0)
    }
}

// MARK: - main

let args = parseArgs()
try? FileManager.default.createDirectory(
    atPath: args.out, withIntermediateDirectories: true)
installDrawableTap()
registerSFMono()

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = FixtureDelegate(args: args)
app.delegate = delegate
app.run()
