//
//  Logo.swift
//  Nice
//
//  Port of the `Logo` function in
//  /tmp/nice-design/nice/project/nice/sidebar.jsx (lines ~189–198).
//
//  Original SVG:
//      <rect x=1 y=1 width=20 height=20 rx=6 fill=var(--accent)/>
//      <path d="M7 8.5 L10.5 11.5 L7 14.5  M11.5 14 L15.5 14"
//            stroke=white strokeWidth=1.6
//            strokeLinecap=round strokeLinejoin=round fill=none/>
//
//  SwiftUI version: a terracotta rounded-square with a white inline
//  chevron + underline rendered as a single `Path`. The geometry is
//  scaled from the JSX's 22×22 viewBox so callers can request any size.
//

import SwiftUI

struct Logo: View {
    var size: CGFloat = 22

    var body: some View {
        // Source viewBox is 22×22; scale factor lets us render at any size
        // while preserving stroke metrics proportionally.
        let scale = size / 22

        ZStack {
            // Rounded square fill — rect is 20×20 inset 1pt from each edge
            // (so 0…22 viewBox with rx=6 becomes a 20pt square with r=6).
            RoundedRectangle(cornerRadius: 6 * scale, style: .continuous)
                .fill(Color.niceAccent)
                .frame(width: 20 * scale, height: 20 * scale)

            // Inline chevron + trailing underline.
            Path { p in
                // Chevron: M7 8.5 L10.5 11.5 L7 14.5
                p.move(to: CGPoint(x: 7, y: 8.5))
                p.addLine(to: CGPoint(x: 10.5, y: 11.5))
                p.addLine(to: CGPoint(x: 7, y: 14.5))
                // Underline: M11.5 14 L15.5 14
                p.move(to: CGPoint(x: 11.5, y: 14))
                p.addLine(to: CGPoint(x: 15.5, y: 14))
            }
            .stroke(
                Color.white,
                style: StrokeStyle(
                    lineWidth: 1.6 * scale,
                    lineCap: .round,
                    lineJoin: .round
                )
            )
            // Scale the 22×22 path space into our requested `size`.
            .frame(width: 22, height: 22)
            .scaleEffect(scale)
            .frame(width: 22 * scale, height: 22 * scale)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

#Preview("Logo") {
    HStack(spacing: 16) {
        Logo()
        Logo(size: 32)
        Logo(size: 48)
    }
    .padding(40)
}
