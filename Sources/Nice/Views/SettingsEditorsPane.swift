//
//  SettingsEditorsPane.swift
//  Nice
//
//  Settings pane for terminal-editor configuration. Three sections:
//
//    1. Editors            — user-defined name+command rows.
//    2. Detected on system — auto-detected editors (read-only) with
//                            an "Add" button to promote them into the
//                            user list.
//    3. File extension routing — extension → editor mappings used by
//                            File Explorer double-click.
//
//  All bindings flow through centralised mutators on `Tweaks`
//  (addEditor, updateEditor, removeEditor, setMapping, …) so the
//  "no orphaned mapping" invariant — kill an editor, drop its
//  mappings — can never be bypassed by the view layer.
//

import AppKit
import SwiftUI

struct SettingsEditorsPane: View {
    @Environment(Tweaks.self) private var tweaks
    @Environment(EditorDetector.self) private var editorDetector
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    var body: some View {
        Group {
            SettingTitle("Editors")

            editorsList

            SettingSubtitle("Detected on your system")
            detectedList

            SettingSubtitle("File extension routing")
            extensionMappings
        }
    }

    // MARK: - Editors list

    @ViewBuilder
    private var editorsList: some View {
        if tweaks.editorCommands.isEmpty {
            SettingRow(
                label: "No editors configured",
                hint: "Add a terminal editor below, or promote one from the detected list."
            ) {
                EmptyView()
            }
        } else {
            ForEach(tweaks.editorCommands) { editor in
                EditorRow(editor: editor) { name, command in
                    tweaks.updateEditor(id: editor.id, name: name, command: command)
                } onDelete: {
                    tweaks.removeEditor(id: editor.id)
                }
            }
        }
        SettingRow(
            label: "Add editor",
            hint: "Name and a shell command (e.g. `vim`, `nvim -p`, `emacs -nw`). The file path is appended at invocation time."
        ) {
            Button("Add Editor") {
                tweaks.addEditor(EditorCommand(
                    id: UUID(),
                    name: "New editor",
                    command: ""
                ))
            }
            .controlSize(.small)
            .accessibilityIdentifier("settings.editors.add")
        }
    }

    // MARK: - Detected list

    @ViewBuilder
    private var detectedList: some View {
        let userCommands = Set(tweaks.editorCommands.map(\.command))
        let unpromoted = editorDetector.detected.filter {
            !userCommands.contains($0.command)
        }
        if editorDetector.detected.isEmpty {
            SettingRow(
                label: "Nothing detected",
                hint: "Nice probed your shell at startup but didn't find any of vim, nvim, hx, nano, emacs, micro, or kak on your PATH."
            ) {
                EmptyView()
            }
        } else if unpromoted.isEmpty {
            SettingRow(
                label: "All detected editors are added",
                hint: "Detected editors that aren't in your list above appear here for one-click promotion."
            ) {
                EmptyView()
            }
        } else {
            ForEach(unpromoted) { editor in
                SettingRow(
                    label: editor.name,
                    hint: editor.command
                ) {
                    Button("Add") {
                        tweaks.addEditor(EditorCommand(
                            id: UUID(),
                            name: editor.name,
                            command: editor.command
                        ))
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    // MARK: - Extension mappings

    @ViewBuilder
    private var extensionMappings: some View {
        if tweaks.extensionEditorMap.isEmpty {
            SettingRow(
                label: "No mappings",
                hint: "Add a mapping below to make double-clicking a file open it in an editor pane instead of the OS default app."
            ) {
                EmptyView()
            }
        } else {
            // Sort by extension key so the list order is stable across
            // re-renders (dicts have no inherent order).
            let entries = tweaks.extensionEditorMap
                .sorted { $0.key < $1.key }
            ForEach(entries, id: \.key) { ext, editorId in
                ExtensionMappingRow(
                    ext: ext,
                    editorId: editorId,
                    onChangeEditor: { newId in
                        tweaks.setMapping(extension: ext, editorId: newId)
                    },
                    onDelete: {
                        tweaks.removeMapping(forExtension: ext)
                    }
                )
            }
        }
        SettingRow(
            label: "Add mapping",
            hint: tweaks.editorCommands.isEmpty
                ? "Add an editor above to start mapping extensions."
                : "Type an extension (with or without a leading dot) and pick which editor handles it."
        ) {
            Button("Add Mapping") {
                guard let firstEditor = tweaks.editorCommands.first else { return }
                let key = nextUnusedExtensionKey()
                tweaks.setMapping(extension: key, editorId: firstEditor.id)
            }
            .controlSize(.small)
            .disabled(tweaks.editorCommands.isEmpty)
            .accessibilityIdentifier("settings.editors.mapping.add")
        }
    }

    /// Synthesise a placeholder extension that doesn't already exist
    /// in the map, so the new row is editable rather than overwriting
    /// an existing mapping. Tries `new`, then `new1`, `new2`, …
    private func nextUnusedExtensionKey() -> String {
        var candidate = "new"
        var n = 1
        while tweaks.extensionEditorMap[candidate] != nil {
            candidate = "new\(n)"
            n += 1
        }
        return candidate
    }
}

// MARK: - Editor row

private struct EditorRow: View {
    let editor: EditorCommand
    let onCommit: (_ name: String, _ command: String) -> Void
    let onDelete: () -> Void

    @State private var name: String
    @State private var command: String

    init(
        editor: EditorCommand,
        onCommit: @escaping (String, String) -> Void,
        onDelete: @escaping () -> Void
    ) {
        self.editor = editor
        self.onCommit = onCommit
        self.onDelete = onDelete
        _name = State(initialValue: editor.name)
        _command = State(initialValue: editor.command)
    }

    var body: some View {
        SettingRow(
            label: editor.name.isEmpty ? "(unnamed editor)" : editor.name,
            hint: editor.command.isEmpty ? "No command set" : nil
        ) {
            HStack(alignment: .bottom, spacing: 6) {
                LabeledField(
                    caption: "Name",
                    text: $name,
                    placeholder: "Display name",
                    width: 110,
                    accessibilityId: "settings.editors.row.\(editor.id.uuidString).name",
                    onSubmit: { onCommit(name, command) }
                )
                LabeledField(
                    caption: "Command",
                    text: $command,
                    placeholder: "vim, nvim -p, …",
                    width: 140,
                    accessibilityId: "settings.editors.row.\(editor.id.uuidString).command",
                    onSubmit: { onCommit(name, command) }
                )
                Button {
                    onCommit(name, command)
                } label: {
                    Image(systemName: "checkmark")
                }
                .controlSize(.small)
                .help("Save")

                Button(role: .destructive) {
                    onDelete()
                } label: {
                    Image(systemName: "trash")
                }
                .controlSize(.small)
                .help("Delete editor")
                .accessibilityIdentifier("settings.editors.row.\(editor.id.uuidString).delete")
            }
        }
    }
}

/// Compact captioned text field used inside `EditorRow`. The caption
/// (10.5pt secondary ink) sits above the input so the field's purpose
/// stays legible once the user types over the placeholder.
private struct LabeledField: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let caption: String
    @Binding var text: String
    let placeholder: String
    let width: CGFloat
    let accessibilityId: String
    let onSubmit: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(caption)
                .font(.system(size: 10.5, weight: .medium))
                .foregroundStyle(Color.niceInk3(scheme, palette))
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
            TextField(placeholder, text: $text)
                .textFieldStyle(.roundedBorder)
                .frame(width: width)
                .onSubmit(onSubmit)
                .accessibilityIdentifier(accessibilityId)
        }
    }
}

// MARK: - Extension mapping row

private struct ExtensionMappingRow: View {
    @Environment(Tweaks.self) private var tweaks

    let ext: String
    let editorId: UUID
    let onChangeEditor: (UUID) -> Void
    let onDelete: () -> Void

    @State private var extDraft: String

    init(
        ext: String,
        editorId: UUID,
        onChangeEditor: @escaping (UUID) -> Void,
        onDelete: @escaping () -> Void
    ) {
        self.ext = ext
        self.editorId = editorId
        self.onChangeEditor = onChangeEditor
        self.onDelete = onDelete
        _extDraft = State(initialValue: ext)
    }

    var body: some View {
        SettingRow(
            label: ".\(ext)",
            hint: tweaks.editor(for: editorId)?.name ?? "Editor missing"
        ) {
            HStack(spacing: 6) {
                TextField("Extension", text: $extDraft)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 80)
                    .onSubmit { commitExtensionRename() }
                    .accessibilityIdentifier("settings.editors.mapping.\(ext).extension")
                Picker("", selection: Binding(
                    get: { editorId },
                    set: { onChangeEditor($0) }
                )) {
                    ForEach(tweaks.editorCommands) { editor in
                        Text(editor.name.isEmpty ? "(unnamed)" : editor.name)
                            .tag(editor.id)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
                .accessibilityIdentifier("settings.editors.mapping.\(ext).editor")

                Button(role: .destructive) {
                    onDelete()
                } label: {
                    Image(systemName: "trash")
                }
                .controlSize(.small)
                .help("Delete mapping")
                .accessibilityIdentifier("settings.editors.mapping.\(ext).delete")
            }
        }
    }

    /// Commit the extension rename: drop the old key, set the new one.
    /// `Tweaks.setMapping` normalises the extension string, so user
    /// input like `.MD` or `MD` collapses to `md`.
    private func commitExtensionRename() {
        let normalised = Tweaks.normalizeExtension(extDraft)
        guard !normalised.isEmpty, normalised != ext else {
            extDraft = ext
            return
        }
        tweaks.removeMapping(forExtension: ext)
        tweaks.setMapping(extension: normalised, editorId: editorId)
    }
}
