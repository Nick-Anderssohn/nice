// imgdiff <a.png> <b.png> — prints fraction of central-region pixels that differ
// noticeably (per-channel delta > 24). Central region = middle 60% x 60%.
import CoreGraphics
import Foundation
import ImageIO
func load(_ p: String) -> CGImage {
    let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: p) as CFURL, nil)!
    return CGImageSourceCreateImageAtIndex(src, 0, nil)!
}
let a = load(CommandLine.arguments[1]), b = load(CommandLine.arguments[2])
guard a.width == b.width, a.height == b.height else { print("SIZE-MISMATCH"); exit(2) }
let w = a.width, h = a.height
let cs = CGColorSpaceCreateDeviceRGB()
let info = CGImageAlphaInfo.premultipliedLast.rawValue
func pixels(_ img: CGImage) -> [UInt8] {
    var buf = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(data: &buf, width: w, height: h, bitsPerComponent: 8,
                        bytesPerRow: w * 4, space: cs, bitmapInfo: info)!
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    return buf
}
let pa = pixels(a), pb = pixels(b)
let x0 = w / 5, x1 = w - w / 5, y0 = h / 5, y1 = h - h / 5
var diff = 0, total = 0
for y in y0..<y1 {
    for x in x0..<x1 {
        let i = (y * w + x) * 4
        total += 1
        if abs(Int(pa[i]) - Int(pb[i])) > 24 || abs(Int(pa[i+1]) - Int(pb[i+1])) > 24
            || abs(Int(pa[i+2]) - Int(pb[i+2])) > 24 { diff += 1 }
    }
}
print(String(format: "%.4f", Double(diff) / Double(total)))
