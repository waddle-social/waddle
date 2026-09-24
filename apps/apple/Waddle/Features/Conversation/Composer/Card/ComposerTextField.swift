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
            TextField(placeholder, text: $text, selection: textSelection, axis: .vertical)
        } else {
            TextField(placeholder, text: $text, axis: .vertical)
        }
    }

    @available(iOS 18.0, macOS 15.0, *)
    private var textSelection: Binding<TextSelection?> {
        Binding(
            get: { ComposerTextSelection.selection(for: selection, in: text) },
            set: { newValue in
                guard let newValue else {
                    selection = nil
                    return
                }
                // A selection reported before its text arrives keeps the
                // previous one until the next report.
                if let range = ComposerTextSelection.range(of: newValue, in: text) {
                    selection = range
                }
            }
        )
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
