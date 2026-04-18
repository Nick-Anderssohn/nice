//
//  CompanionPaneView.swift
//  Nice
//
//  Phase 3: right-hand column that hosts a tab's companion terminals.
//  A 28pt horizontal pill bar across the top (one pill per companion +
//  a trailing "+" button) with the currently-active companion view
//  rendered below via `TerminalHost`. Used by `AppShellView` in two
//  modes — bolted to the right of the Claude pane on Claude tabs, and
//  full-width on terminal-only tabs.
//

import SwiftUI

struct CompanionPaneView: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme
    @State private var hoveredPillId: String?
    @State private var addHovered = false

    let tabId: String

    var body: some View {
        VStack(spacing: 0) {
            tabBar
            Rectangle()
                .fill(Color.niceLine(scheme))
                .frame(height: 1)
            terminalArea
        }
        .background(Color.niceBg3(scheme))
    }

    // MARK: - Tab bar

    private var tabBar: some View {
        HStack(spacing: 2) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 2) {
                    if let tab = appState.tab(for: tabId) {
                        ForEach(tab.companions) { companion in
                            pill(
                                for: companion,
                                isActive: companion.id == tab.activeCompanionId
                            )
                        }
                    }
                }
                .padding(.horizontal, 6)
            }
            Spacer(minLength: 0)
            Button(action: { appState.addCompanion(tabId: tabId) }) {
                Image(systemName: "plus")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Color.niceInk2(scheme))
                    .frame(width: 22, height: 22)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(addHovered
                                  ? Color.niceInk(scheme).opacity(0.07)
                                  : Color.clear)
                    )
            }
            .buttonStyle(.plain)
            .onHover { addHovered = $0 }
            .padding(.trailing, 6)
            .accessibilityIdentifier("companion.add")
        }
        .frame(height: 32)
        .background(Color.niceBg2(scheme))
    }

    private func pill(for companion: CompanionTerminal, isActive: Bool) -> some View {
        let isHovered = hoveredPillId == companion.id
        let font: Font = {
            if NSFont(name: "JetBrainsMono-Regular", size: 11.5) != nil {
                return .custom("JetBrainsMono-Regular", size: 11.5)
            }
            return .system(size: 11.5)
        }()
        let bg: Color = isActive
            ? Color.nicePanel(scheme)
            : isHovered
                ? Color.niceInk(scheme).opacity(0.05)
                : Color.clear
        return HStack(spacing: 6) {
            Circle()
                .fill(isActive ? Color.niceAccentDynamic : Color.niceInk3(scheme).opacity(0.6))
                .frame(width: 6, height: 6)
            Text(companion.title)
                .font(font)
                .fontWeight(isActive ? .semibold : .medium)
                .foregroundStyle(isActive ? Color.niceInk(scheme) : Color.niceInk2(scheme))
                .lineLimit(1)
            if isActive || isHovered {
                Button(action: {
                    appState.requestCloseCompanion(tabId: tabId, companionId: companion.id)
                }) {
                    Image(systemName: "xmark")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(Color.niceInk3(scheme))
                        .frame(width: 14, height: 14)
                        .background(
                            RoundedRectangle(cornerRadius: 3, style: .continuous)
                                .fill(Color.clear)
                        )
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("companion.close.\(companion.id)")
            }
        }
        .padding(.leading, 10)
        .padding(.trailing, 8)
        .frame(height: 22)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(bg)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(isActive ? Color.niceLine(scheme) : Color.clear, lineWidth: 1)
        )
        .contentShape(Rectangle())
        .onHover { hoveredPillId = $0 ? companion.id : nil }
        .onTapGesture {
            appState.setActiveCompanion(tabId: tabId, companionId: companion.id)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("companion.pill.\(companion.id)")
    }

    // MARK: - Terminal area

    @ViewBuilder
    private var terminalArea: some View {
        if let tab = appState.tab(for: tabId),
           let active = tab.activeCompanionId,
           let session = appState.ptySessions[tabId],
           let view = session.terminals[active] {
            TerminalHost(view: view, focus: !tab.hasClaudePane)
                .id(active)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            // No companion to host (transient during teardown). Fill
            // the slot with the pane background so the layout stays put.
            Color.niceBg3(scheme)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}
