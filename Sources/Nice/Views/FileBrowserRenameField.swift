//
//  FileBrowserRenameField.swift
//  Nice
//
//  Inline rename text field for file-tree rows. Wraps `NSTextField`
//  so we can pre-select the basename portion of a filename only —
//  SwiftUI's `TextField` exposes no way to set the initial selection
//  range. The basename-only selection mirrors Finder: when the user
//  hits Return, the extension is preserved unless they explicitly
//  delete it.
//
//  Rendering: borderless, transparent background, transparent ring
//  on focus — the field has to sit inside the row's existing pill
//  background without painting a second one. Sized via SwiftUI; the
//  representable adopts whatever frame the parent gives it.
//
//  Lifecycle:
//    • `controlTextDidChange`         → update binding.
//    • `controlTextDidEndEditing`     → onCommit (Return / focus-loss).
//    • `cancelOperation:` (Esc)       → onCancel.
//
//  The `becomeFirstResponder` override on the `RenameTextField`
//  subclass is the whole reason this representable exists. We look
//  up the window's field editor (the shared `NSTextView` cellbacking
//  every NSTextField) and set its `selectedRange` to the basename
//  length, so when SwiftUI flips `isEditing` to `true` and the field
//  becomes first responder, the user sees only the basename
//  highlighted.
//

import AppKit
import SwiftUI

struct FileBrowserRenameField: NSViewRepresentable {
    @Binding var text: String
    /// Length (in unicode-scalar count) to pre-select on first focus.
    /// For files with an extension this is the basename portion only;
    /// for folders / extension-less files it equals the full string
    /// length. The parent computes this from
    /// `FileOperationsService.splitNameAndExtension`.
    let initialSelectionLength: Int
    let onCommit: () -> Void
    let onCancel: () -> Void

    /// Stable accessibility id so XCUITest can target the field.
    let accessibilityId: String

    func makeCoordinator() -> Coordinator {
        Coordinator(text: $text, onCommit: onCommit, onCancel: onCancel)
    }

    func makeNSView(context: Context) -> RenameTextField {
        let field = RenameTextField()
        field.delegate = context.coordinator
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.isBezeled = false
        field.usesSingleLineMode = true
        field.cell?.wraps = false
        field.cell?.isScrollable = true
        field.font = NSFont.systemFont(ofSize: 13)
        field.stringValue = text
        field.initialSelectionLength = initialSelectionLength
        field.setAccessibilityIdentifier(accessibilityId)
        // Defer making first responder until the view is in a
        // window — `becomeFirstResponder` looks up the window's
        // field editor, which is nil before that.
        DispatchQueue.main.async { [weak field] in
            field?.window?.makeFirstResponder(field)
        }
        return field
    }

    func updateNSView(_ nsView: RenameTextField, context: Context) {
        if nsView.stringValue != text {
            nsView.stringValue = text
        }
        nsView.initialSelectionLength = initialSelectionLength
        context.coordinator.text = $text
        context.coordinator.onCommit = onCommit
        context.coordinator.onCancel = onCancel
    }

    @MainActor
    final class Coordinator: NSObject, NSTextFieldDelegate {
        var text: Binding<String>
        var onCommit: () -> Void
        var onCancel: () -> Void
        /// One-shot guard. Esc routes through `cancelOperation` →
        /// `onCancel()`, but AppKit also fires `controlTextDidEndEditing`
        /// when the field resigns first responder — without this flag,
        /// commit would fire right after cancel and clobber the cancel.
        var didCancel = false
        /// Same one-shot guard for commit, so a programmatic teardown
        /// (focus shift, view leaving the hierarchy) doesn't fire
        /// commit twice.
        var didCommit = false

        init(text: Binding<String>, onCommit: @escaping () -> Void, onCancel: @escaping () -> Void) {
            self.text = text
            self.onCommit = onCommit
            self.onCancel = onCancel
        }

        func controlTextDidChange(_ obj: Notification) {
            guard let field = obj.object as? NSTextField else { return }
            text.wrappedValue = field.stringValue
        }

        func controlTextDidEndEditing(_ obj: Notification) {
            guard !didCancel, !didCommit else { return }
            didCommit = true
            onCommit()
        }

        /// Routed by `RenameTextField.cancelOperation(_:)` when the
        /// user hits Esc. Setting `didCancel` first prevents the
        /// follow-on `controlTextDidEndEditing` from firing commit.
        func cancel() {
            guard !didCancel, !didCommit else { return }
            didCancel = true
            onCancel()
        }
    }

    /// `NSTextField` subclass with two responsibilities:
    ///   • Pre-select the basename portion on first focus, by setting
    ///     `selectedRange` on the window's field editor.
    ///   • Route Esc to the coordinator's `cancel()` rather than the
    ///     default field-editor undo/clear behaviour.
    final class RenameTextField: NSTextField {
        var initialSelectionLength: Int = 0
        private var didApplyInitialSelection = false

        override func becomeFirstResponder() -> Bool {
            let ok = super.becomeFirstResponder()
            if ok, !didApplyInitialSelection,
               let editor = window?.fieldEditor(true, for: self) as? NSTextView {
                let length = max(0, min(initialSelectionLength, stringValue.count))
                editor.selectedRange = NSRange(location: 0, length: length)
                didApplyInitialSelection = true
            }
            return ok
        }

        override func cancelOperation(_ sender: Any?) {
            (delegate as? Coordinator)?.cancel()
        }
    }
}
