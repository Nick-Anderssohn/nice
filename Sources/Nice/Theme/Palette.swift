//
//  Palette.swift
//  Nice
//
//  Source of truth: the `<style>` block in
//  /tmp/nice-design/nice/project/Nice.html — the CSS variables defined
//  under `.theme-light` and `.theme-dark` scopes (plus `--accent`).
//
//  The design mock uses `oklch(L C H)` values. Swift's `Color(.sRGB, ...)`
//  initializer doesn't accept oklch directly, so each value below was
//  converted to sRGB via Björn Ottosson's standard oklch -> oklab ->
//  linear sRGB -> gamma-encoded sRGB pipeline, rounded to 3 decimals.
//  (Verified against https://oklch.com for spot-checks; drift is < 0.5%.)
//
//  When the design CSS changes, re-run the conversion and update the
//  corresponding literals below. Keep this file and the CSS in sync.
//

import SwiftUI

public extension Color {

    // MARK: - Accent

    /// Default terracotta accent (`#c96442`).
    static let niceAccent = Color(.sRGB, red: 0.788, green: 0.392, blue: 0.259, opacity: 1.0)

    // MARK: - Backgrounds

    static func niceBg(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.080, green: 0.066, blue: 0.055, opacity: 1.0)
            : Color(.sRGB, red: 0.989, green: 0.978, blue: 0.970, opacity: 1.0)
    }

    static func niceBg2(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.058, green: 0.045, blue: 0.035, opacity: 1.0)
            : Color(.sRGB, red: 0.965, green: 0.952, blue: 0.942, opacity: 1.0)
    }

    static func niceBg3(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.037, green: 0.026, blue: 0.019, opacity: 1.0)
            : Color(.sRGB, red: 0.934, green: 0.919, blue: 0.907, opacity: 1.0)
    }

    static func nicePanel(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.097, green: 0.083, blue: 0.072, opacity: 1.0)
            : Color(.sRGB, red: 1.000, green: 0.992, blue: 0.986, opacity: 1.0)
    }

    // MARK: - Ink (foreground text)

    static func niceInk(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.956, green: 0.946, blue: 0.938, opacity: 1.0)
            : Color(.sRGB, red: 0.091, green: 0.074, blue: 0.060, opacity: 1.0)
    }

    static func niceInk2(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.693, green: 0.679, blue: 0.667, opacity: 1.0)
            : Color(.sRGB, red: 0.273, green: 0.257, blue: 0.244, opacity: 1.0)
    }

    static func niceInk3(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.460, green: 0.441, blue: 0.427, opacity: 1.0)
            : Color(.sRGB, red: 0.494, green: 0.475, blue: 0.461, opacity: 1.0)
    }

    // MARK: - Lines / dividers

    static func niceLine(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.172, green: 0.157, blue: 0.145, opacity: 1.0)
            : Color(.sRGB, red: 0.857, green: 0.841, blue: 0.829, opacity: 1.0)
    }

    static func niceLineStrong(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.252, green: 0.236, blue: 0.223, opacity: 1.0)
            : Color(.sRGB, red: 0.735, green: 0.715, blue: 0.699, opacity: 1.0)
    }

    // MARK: - Selection / bubble / chrome

    /// CSS: `color-mix(in oklch, var(--accent) 14%, transparent)` (light),
    /// `22%` in dark. Approximated here by applying the accent with the
    /// same alpha against a transparent base.
    static func niceSel(_ scheme: ColorScheme) -> Color {
        let alpha: Double = scheme == .dark ? 0.22 : 0.14
        return Color(.sRGB, red: 0.788, green: 0.392, blue: 0.259, opacity: alpha)
    }

    static func niceUserBubble(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.134, green: 0.119, blue: 0.108, opacity: 1.0)
            : Color(.sRGB, red: 0.939, green: 0.918, blue: 0.902, opacity: 1.0)
    }

    /// CSS: `color-mix(in oklch, var(--bg) 70%, transparent)`. We mirror
    /// that by taking `--bg` and dropping alpha to 0.7.
    static func niceChrome(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(.sRGB, red: 0.080, green: 0.066, blue: 0.055, opacity: 0.70)
            : Color(.sRGB, red: 0.989, green: 0.978, blue: 0.970, opacity: 0.70)
    }
}
