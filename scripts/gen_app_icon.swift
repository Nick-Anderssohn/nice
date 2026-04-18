#!/usr/bin/env swift
//
//  gen_app_icon.swift
//  Renders the Logo view's geometry (Sources/Nice/Views/Logo.swift) as
//  PNG images at every size required by Resources/Assets.xcassets/
//  AppIcon.appiconset. The accent is hard-coded to `ocean`
//  (#3b82f6) — the app's default accent preset. If you change the
//  default preset in AccentPreset, re-run this script to regenerate.
//
//  Usage: from the repo root,
//      ./scripts/gen_app_icon.swift
//
//  Writes icon_16.png … icon_1024.png into the AppIcon.appiconset
//  directory and (re)writes Contents.json to reference them.
//

import AppKit
import CoreGraphics
import Foundation

let repoRoot = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
let iconDir = repoRoot
    .appendingPathComponent("Resources")
    .appendingPathComponent("Assets.xcassets")
    .appendingPathComponent("AppIcon.appiconset")

guard FileManager.default.fileExists(atPath: iconDir.path) else {
    FileHandle.standardError.write(
        "AppIcon.appiconset not found at \(iconDir.path) — run this from the repo root.\n".data(using: .utf8)!
    )
    exit(1)
}

// Default accent: ocean (#3b82f6).
let accent = CGColor(srgbRed: 59.0 / 255.0, green: 130.0 / 255.0, blue: 246.0 / 255.0, alpha: 1.0)
let ink = CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1)

/// Renders the 22×22 Logo viewBox at the given pixel size.
func renderIcon(size: Int) -> Data {
    let w = CGFloat(size)
    let scale = w / 22.0

    let cs = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: cs,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        fatalError("Failed to allocate CGContext at size \(size)")
    }

    // CoreGraphics origin is bottom-left; flip to match SwiftUI's top-left
    // coordinates used in Logo.swift.
    ctx.translateBy(x: 0, y: w)
    ctx.scaleBy(x: 1, y: -1)

    // High-quality antialiasing for the small sizes where it matters most.
    ctx.setAllowsAntialiasing(true)
    ctx.setShouldAntialias(true)
    ctx.interpolationQuality = .high

    // Rounded-square fill — 20×20 inset 1pt from each edge of the 22×22
    // viewBox with corner radius 6.
    let rect = CGRect(x: 1 * scale, y: 1 * scale, width: 20 * scale, height: 20 * scale)
    let radius = 6 * scale
    let fillPath = CGPath(
        roundedRect: rect,
        cornerWidth: radius,
        cornerHeight: radius,
        transform: nil
    )
    ctx.setFillColor(accent)
    ctx.addPath(fillPath)
    ctx.fillPath()

    // Inline chevron + trailing underline.
    let stroke = CGMutablePath()
    // Chevron: M7 8.5 L10.5 11.5 L7 14.5
    stroke.move(to: CGPoint(x: 7 * scale, y: 8.5 * scale))
    stroke.addLine(to: CGPoint(x: 10.5 * scale, y: 11.5 * scale))
    stroke.addLine(to: CGPoint(x: 7 * scale, y: 14.5 * scale))
    // Underline: M11.5 14 L15.5 14
    stroke.move(to: CGPoint(x: 11.5 * scale, y: 14 * scale))
    stroke.addLine(to: CGPoint(x: 15.5 * scale, y: 14 * scale))

    ctx.setStrokeColor(ink)
    ctx.setLineWidth(1.6 * scale)
    ctx.setLineCap(.round)
    ctx.setLineJoin(.round)
    ctx.addPath(stroke)
    ctx.strokePath()

    guard let cgImage = ctx.makeImage() else {
        fatalError("Failed to snapshot CGContext at size \(size)")
    }
    let rep = NSBitmapImageRep(cgImage: cgImage)
    guard let png = rep.representation(using: .png, properties: [:]) else {
        fatalError("Failed to encode PNG at size \(size)")
    }
    return png
}

// Pixel sizes needed by Apple's AppIcon slots (16 is the smallest, 1024 the
// largest). Each Contents.json entry below maps an (idiom, scale, size)
// tuple to one of these files.
let pixelSizes = [16, 32, 64, 128, 256, 512, 1024]

for size in pixelSizes {
    let data = renderIcon(size: size)
    let file = iconDir.appendingPathComponent("icon_\(size).png")
    try data.write(to: file, options: [.atomic])
    print("Wrote \(file.lastPathComponent) (\(data.count) bytes)")
}

// Rewrite Contents.json so Xcode picks up the new image references.
struct ImageEntry: Encodable {
    let filename: String
    let idiom: String
    let scale: String
    let size: String
}
struct Info: Encodable { let author: String; let version: Int }
struct ContentsJSON: Encodable { let images: [ImageEntry]; let info: Info }

let entries: [ImageEntry] = [
    ImageEntry(filename: "icon_16.png",   idiom: "mac", scale: "1x", size: "16x16"),
    ImageEntry(filename: "icon_32.png",   idiom: "mac", scale: "2x", size: "16x16"),
    ImageEntry(filename: "icon_32.png",   idiom: "mac", scale: "1x", size: "32x32"),
    ImageEntry(filename: "icon_64.png",   idiom: "mac", scale: "2x", size: "32x32"),
    ImageEntry(filename: "icon_128.png",  idiom: "mac", scale: "1x", size: "128x128"),
    ImageEntry(filename: "icon_256.png",  idiom: "mac", scale: "2x", size: "128x128"),
    ImageEntry(filename: "icon_256.png",  idiom: "mac", scale: "1x", size: "256x256"),
    ImageEntry(filename: "icon_512.png",  idiom: "mac", scale: "2x", size: "256x256"),
    ImageEntry(filename: "icon_512.png",  idiom: "mac", scale: "1x", size: "512x512"),
    ImageEntry(filename: "icon_1024.png", idiom: "mac", scale: "2x", size: "512x512"),
]
let payload = ContentsJSON(images: entries, info: Info(author: "xcode", version: 1))

let encoder = JSONEncoder()
encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
let json = try encoder.encode(payload)
try json.write(to: iconDir.appendingPathComponent("Contents.json"), options: [.atomic])

print("Wrote Contents.json")
