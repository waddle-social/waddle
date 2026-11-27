import SwiftUI
import WaddleKit

/// Multi-line field. Native views forward keys and pastes to the composer.
///
/// The field reports its selection (as Unicode scalar offsets) so
/// formatting wraps the selected text.
struct ComposerTextField: View {
    let text: String
    let draft: ComposerFieldEdits.Draft
    let edits: ComposerFieldEdits
    @Binding var selection: ComposerScalarRange?
    let placeholder: String
    /// Kept in step with the system's focus both ways.
    @Binding var isFocused: Bool
    let onTextChange: (ComposerTextReplacement) -> Void
    let keys: ComposerKeyInput
    /// A paste the field cannot take as text: a copied file, GIF or picture.
    let onPaste: () -> Void

    var body: some View {
        ComposerTextView(
            text: text,
            draft: draft,
            edits: edits,
            selection: $selection,
            isFocused: $isFocused,
            placeholder: placeholder,
            onTextChange: onTextChange,
            keys: keys,
            onPasteAttachments: onPaste
        )
        .overlay(alignment: .topLeading) {
            if text.isEmpty {
                Text(placeholder)
                    .huddleText(Theme.Typography.message)
                    .foregroundStyle(Theme.Colors.fgSoft)
                    .lineLimit(1)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
        }
        .padding(.vertical, Theme.Spacing.s)
    }
}

/// Text edits from the composer can arrive together in one SwiftUI update.
@MainActor
final class ComposerFieldEdits {
    struct Draft: Equatable {
        let model: ObjectIdentifier
        let editing: String?
        let revision: Int
    }

    struct Edit {
        let original: String
        let replacement: ComposerTextReplacement
        let selection: ComposerScalarRange
    }

    private var draft: Draft?
    private var pending: [Edit] = []

    func append(_ edit: Edit, in draft: Draft) {
        if self.draft != draft {
            self.draft = draft
            pending.removeAll()
        }
        pending.append(edit)
    }

    func take(in draft: Draft, from original: String, to text: String) -> [Edit] {
        defer { pending.removeAll() }
        guard self.draft == draft else { return [] }
        var expected = original
        for edit in pending {
            guard expected.utf16.elementsEqual(edit.original.utf16) else { return [] }
            expected = edit.replacement.text
        }
        return expected.utf16.elementsEqual(text.utf16) ? pending : []
    }
}

@MainActor
extension UndoManager {
    func groupComposerEdit(_ edit: () -> Void) {
        // With no open event group, give this command its own top-level
        // group even if SwiftUI delivers multiple commands in one update.
        let suspendEventGrouping = groupsByEvent && groupingLevel == 0
        if suspendEventGrouping { groupsByEvent = false }
        beginUndoGrouping()
        edit()
        endUndoGrouping()
        if suspendEventGrouping { groupsByEvent = true }
    }

    /// Bookend the native undo actions so each selection lands after its
    /// corresponding text, whether the group is undone or redone.
    func registerComposerSelection(applyOnUndo: Bool, _ restore: @escaping () -> Void) {
        registerUndo(withTarget: self) { manager in
            if applyOnUndo { restore() }
            manager.registerComposerSelection(applyOnUndo: !applyOnUndo, restore)
        }
    }
}
