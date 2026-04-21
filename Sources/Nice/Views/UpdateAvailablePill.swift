//
//  UpdateAvailablePill.swift
//  Nice
//
//  Trailing-edge toolbar pill that appears only when `ReleaseChecker`
//  has seen a newer version on GitHub. Clicking opens a popover with
//  the two brew commands needed to upgrade and a reminder that Nice
//  must be restarted afterwards.
//
//  When no update is available, the view returns `EmptyView` — the
//  toolbar layout with `updateAvailable == false` is byte-identical to
//  before this feature. No hidden placeholder, no reserved space.
//

import AppKit
import SwiftUI

struct UpdateAvailablePill: View {
    @EnvironmentObject private var checker: ReleaseChecker
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @State private var hovering = false
    @State private var showPopover = false

    var body: some View {
        if checker.updateAvailable {
            pillButton
        }
    }

    private var pillButton: some View {
        Button(action: { showPopover.toggle() }) {
            HStack(spacing: 5) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 11, weight: .semibold))
                Text("Update available")
                    .font(.system(size: 12, weight: .medium))
                    .lineLimit(1)
            }
            .foregroundStyle(Color.niceAccent)
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(hovering
                        ? Color.niceAccent.opacity(0.12)
                        : Color.niceAccent.opacity(0.07))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .stroke(Color.niceAccent.opacity(0.3), lineWidth: 1)
            )
            .contentShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .accessibilityIdentifier("toolbar.updateAvailable")
        .accessibilityLabel("Update available")
        .popover(isPresented: $showPopover, arrowEdge: .bottom) {
            UpdateAvailablePopoverContent(latestVersion: checker.latestVersion)
                .environment(\.colorScheme, scheme)
                .environment(\.palette, palette)
        }
    }
}

// MARK: - Popover

private struct UpdateAvailablePopoverContent: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let latestVersion: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header

            VStack(alignment: .leading, spacing: 8) {
                CommandRow(command: "brew update")
                CommandRow(command: "brew upgrade --cask nice")
            }

            Text("Restart Nice after upgrading.")
                .font(.system(size: 11))
                .foregroundStyle(Color.niceInk2(scheme, palette))
        }
        .padding(16)
        .frame(width: 320, alignment: .leading)
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.up.circle.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.niceAccent)
            Text(titleText)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.niceInk(scheme, palette))
        }
    }

    private var titleText: String {
        if let version = latestVersion.flatMap(Self.stripLeadingV) {
            return "Update available: \(version)"
        }
        return "Update available"
    }

    private static func stripLeadingV(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if trimmed.first == "v" || trimmed.first == "V" {
            return String(trimmed.dropFirst())
        }
        return trimmed
    }
}

private struct CommandRow: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette
    @State private var justCopied = false

    let command: String

    var body: some View {
        HStack(spacing: 8) {
            Text(command)
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(Color.niceInk(scheme, palette))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)

            Button(action: copy) {
                Text(justCopied ? "Copied" : "Copy")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Color.niceInk2(scheme, palette))
                    .padding(.horizontal, 8)
                    .frame(height: 22)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(Color.niceInk(scheme, palette).opacity(0.07))
                    )
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Copy \(command)")
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(Color.nicePanel(scheme, palette))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .stroke(Color.niceLine(scheme, palette), lineWidth: 1)
        )
    }

    private func copy() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(command, forType: .string)
        justCopied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            justCopied = false
        }
    }
}
