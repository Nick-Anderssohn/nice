//
//  ChromePinButton.swift
//  Nice
//
//  The custom "pin" toggle that `TrafficLightPlacer` lays out inline with
//  the three standard window buttons (close / miniaturize / zoom),
//  immediately to the zoom button's right. It shares the lights' superview
//  and coordinate space, so the placer can size + position it exactly like
//  a fourth light (same visual diameter, same window-y 26 top-bar row,
//  one native inter-button pitch to the right of zoom).
//
//  It is an `NSButton` rather than a bare `NSView` on purpose: AppKit's
//  button tracking loop CONSUMES the press (mouseDown → drag → up), so a
//  click on the pin can never leak into the title-bar's window-move drag
//  the way a press on an inert title-bar subview could. Click handling and
//  accessibility come for free.
//
//  All visuals are custom-drawn in `draw(_:)` (the cell never draws): a
//  filled accent disc + white pin glyph when active, a faint ringed disc +
//  dim glyph when inactive — a clearly-visible two-state toggle that still
//  reads as part of the traffic-light cluster.
//

import AppKit

@MainActor
final class ChromePinButton: NSButton {

    /// The toggle state. Flipped on each click; drives the active/inactive
    /// rendering. `private(set)` so only a click (or a test's
    /// `performClick`) can change it.
    private(set) var isActive = false

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        isBordered = false
        bezelStyle = .regularSquare
        setButtonType(.momentaryChange)
        title = ""
        imagePosition = .noImage
        focusRingType = .none
        // Fire our own toggle on click; AppKit's tracking loop consumes the
        // press so it never turns into a window-move drag.
        target = self
        action = #selector(handleClick)

        setAccessibilityRole(.checkBox)
        setAccessibilityLabel("Pin window")
        setAccessibilityIdentifier("chrome.pinToggle")
        toolTip = "Pin window"
        refreshAccessibilityValue()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    @objc private func handleClick() {
        isActive.toggle()
        needsDisplay = true
        refreshAccessibilityValue()
    }

    private func refreshAccessibilityValue() {
        setAccessibilityValue(NSNumber(value: isActive))
    }

    // MARK: - Drawing

    override func draw(_ dirtyRect: NSRect) {
        // Match a traffic light: a disc filling the shorter dimension,
        // centered, inset ~0.5pt so the ring doesn't clip.
        let diameter = min(bounds.width, bounds.height)
        let discRect = NSRect(
            x: (bounds.width - diameter) / 2,
            y: (bounds.height - diameter) / 2,
            width: diameter,
            height: diameter
        ).insetBy(dx: 0.5, dy: 0.5)
        let disc = NSBezierPath(ovalIn: discRect)

        if isActive {
            NSColor.controlAccentColor.setFill()
            disc.fill()
        } else {
            NSColor.tertiaryLabelColor.withAlphaComponent(0.16).setFill()
            disc.fill()
            NSColor.tertiaryLabelColor.setStroke()
            disc.lineWidth = 1
            disc.stroke()
        }

        let glyphColor: NSColor = isActive ? .white : .secondaryLabelColor
        if let glyph = Self.pinGlyph(diameter: diameter, color: glyphColor) {
            let size = glyph.size
            let origin = NSPoint(
                x: (bounds.width - size.width) / 2,
                y: (bounds.height - size.height) / 2
            )
            glyph.draw(at: origin, from: .zero, operation: .sourceOver, fraction: 1)
        }
    }

    /// A `pin.fill` SF Symbol rendered at roughly half the disc diameter and
    /// tinted to `color`. Template-tinted via a `.sourceAtop` fill so it
    /// paints solid in the requested color regardless of appearance.
    private static func pinGlyph(diameter: CGFloat, color: NSColor) -> NSImage? {
        guard let base = NSImage(systemSymbolName: "pin.fill", accessibilityDescription: nil) else {
            return nil
        }
        let config = NSImage.SymbolConfiguration(pointSize: diameter * 0.5, weight: .bold)
        let sized = base.withSymbolConfiguration(config) ?? base
        let size = sized.size
        return NSImage(size: size, flipped: false) { rect in
            sized.draw(in: rect)
            color.set()
            rect.fill(using: .sourceAtop)
            return true
        }
    }
}
