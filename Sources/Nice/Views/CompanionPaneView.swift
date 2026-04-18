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
        HStack(spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 4) {
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
                    .foregroundStyle(Color.niceInk(scheme))
                    .frame(width: 22, height: 20)
                    .background(
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .fill(Color.niceBg2(scheme))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .strokeBorder(Color.niceLine(scheme), lineWidth: 1)
                    )
            }
            .buttonStyle(.plain)
            .padding(.trailing, 6)
            .accessibilityIdentifier("companion.add")
        }
        .frame(height: 28)
        .background(Color.niceBg3(scheme))
    }

    private func pill(for companion: CompanionTerminal, isActive: Bool) -> some View {
        let bg = isActive ? Color.niceBg3(scheme) : Color.niceBg2(scheme)
        let font: Font = {
            if NSFont(name: "JetBrainsMono-Regular", size: 11) != nil {
                return .custom("JetBrainsMono-Regular", size: 11)
            }
            return .system(size: 11)
        }()
        return HStack(spacing: 4) {
            Text(companion.title)
                .font(font)
                .foregroundStyle(Color.niceInk(scheme))
                .lineLimit(1)
                .onTapGesture {
                    appState.setActiveCompanion(tabId: tabId, companionId: companion.id)
                }
            Button(action: {
                appState.requestCloseCompanion(tabId: tabId, companionId: companion.id)
            }) {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Color.niceInk(scheme))
                    .frame(width: 14, height: 14)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("companion.close.\(companion.id)")
        }
        .padding(.leading, 8)
        .padding(.trailing, 4)
        .padding(.vertical, 3)
        .background(
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(bg)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .strokeBorder(Color.niceLine(scheme), lineWidth: 1)
        )
        .contentShape(Rectangle())
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
