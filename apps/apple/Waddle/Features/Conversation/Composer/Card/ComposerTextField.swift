import SwiftUI
import WaddleKit

/// Which key asked to accept the first suggestion.
enum ComposerSuggestionKey {
    case tab
    case returnKey
}

/// Multi-line field. On Mac, Return sends, Shift or Option with Return
/// adds a line, Tab or Return accepts the first suggestion, and pasting
/// files or images attaches them. Escape cancels an edit or reply
/// everywhere a hardware keyboard is attached.
///
/// On iOS 18 and macOS 15 the field reports its selection (as Unicode
/// scalar offsets) so formatting wraps the selected text; on earlier
/// systems `selection` stays nil and formatting wraps the whole draft.
struct ComposerTextField: View {
    @Binding var text: String
    @Binding var selection: Range<Int>?
    let placeholder: String
    var isFocused: FocusState<Bool>.Binding
    let onSubmit: () -> Void
    /// Returns true when a suggestion was inserted.
    let onAcceptSuggestion: (ComposerSuggestionKey) -> Bool
    /// Returns true when an edit or reply was cancelled.
    let onCancel: () -> Bool
    /// A paste the field cannot take as text (Mac only).
    let onPaste: () -> Void

    var body: some View {
        field
            .textFieldStyle(.plain)
            .lineLimit(1...8)
            .font(.body)
            .focused(isFocused)
            .padding(.vertical, Theme.Spacing.xs + 2)
            #if os(macOS)
            .onKeyPress(.return, phases: .down) { press in
                handleReturn(press)
            }
            .onKeyPress(.tab, phases: .down) { _ in
                onAcceptSuggestion(.tab) ? .handled : .ignored
            }
            .modifier(ComposerPasteKeyMonitor(isFocused: isFocused.wrappedValue, onPaste: onPaste))
            #endif
            .onKeyPress(.escape) {
                onCancel() ? .handled : .ignored
            }
    }

    @ViewBuilder
    private var field: some View {
        if #available(iOS 18.0, macOS 15.0, *) {
            ComposerSelectableField(text: $text, selection: $selection, placeholder: placeholder)
        } else {
            TextField(placeholder, text: $text, axis: .vertical)
        }
    }

    #if os(macOS)
    private func handleReturn(_ press: KeyPress) -> KeyPress.Result {
        // Option-Return is the text system's own newline, inserted at the
        // caret; let it through.
        if press.modifiers.contains(.option) {
            return .ignored
        }
        if press.modifiers.contains(.shift) {
            let edit = ComposerInsertion.replacingSelection(with: "\n", in: text, selection: selection)
            text = edit.text
            if ComposerSelectionSupport.isAvailable {
                selection = edit.selection
            }
            return .handled
        }
        if onAcceptSuggestion(.returnKey) {
            return .handled
        }
        onSubmit()
        return .handled
    }
    #endif
}

/// The text field with its own `TextSelection`, bridged to the composer's
/// scalar offsets.
///
/// The field owns the caret. Every selection it reports is translated to
/// offsets for the composer; the composer's `selection` is written back
/// into the field only when the composer moved the caret itself (a
/// formatting edit, an inserted emoji, a completed mention). The field is
/// never handed a selection derived from state it did not write: SwiftUI
/// reports `text` and `selection` separately, and a render between the
/// two would otherwise push the previous keystroke's caret back into the
/// field, so the next character lands before the last one.
@available(iOS 18.0, macOS 15.0, *)
private struct ComposerSelectableField: View {
    @Binding var text: String
    @Binding var selection: Range<Int>?
    let placeholder: String

    /// What the field holds; the field moves it as it edits.
    @State private var fieldSelection: TextSelection?
    /// The offsets last passed between the field and the composer in
    /// either direction. A `selection` equal to this is an echo of the
    /// field's own report, not a request to move the caret.
    @State private var applied: Range<Int>?
    /// A selection the field reported for text that has not arrived yet;
    /// resolved when it does.
    @State private var unresolved: TextSelection?

    var body: some View {
        TextField(placeholder, text: $text, selection: $fieldSelection, axis: .vertical)
            .onChange(of: fieldSelection) { _, reported in
                report(reported)
            }
            .onChange(of: text) { _, _ in
                textChanged()
            }
            .onChange(of: selection) { _, requested in
                guard requested != applied else { return }
                applied = requested
                unresolved = nil
                fieldSelection = ComposerTextSelection.selection(for: requested, in: text)
            }
    }

    private func report(_ reported: TextSelection?) {
        unresolved = nil
        guard let reported else {
            guard applied != nil else { return }
            applied = nil
            selection = nil
            return
        }
        guard case .selection = reported.indices else { return }
        guard let range = ComposerTextSelection.range(of: reported, in: text) else {
            unresolved = reported
            return
        }
        pass(range)
    }

    private func textChanged() {
        if let unresolved {
            guard let range = ComposerTextSelection.range(of: unresolved, in: text) else { return }
            self.unresolved = nil
            pass(range)
        } else if let fieldSelection, ComposerTextSelection.range(of: fieldSelection, in: text) == nil {
            // The composer replaced the draft under a caret the field had
            // placed in the old one. Put the field on the composer's
            // offsets, clamped to the new text, so it never holds indices
            // the text cannot.
            self.fieldSelection = ComposerTextSelection.selection(for: applied, in: text)
        }
    }

    private func pass(_ range: Range<Int>) {
        guard range != applied else { return }
        applied = range
        selection = range
    }
}
