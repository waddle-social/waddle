import SwiftUI
import WaddleKit

/// Which key asked to accept the first suggestion.
enum ComposerSuggestionKey {
    case tab
    case returnKey
}

/// Multi-line field. Return sends on a hardware keyboard (the on-screen
/// keyboard's Return adds a line), Shift or Option with Return adds a
/// line, Tab or Return accepts the first suggestion, Escape cancels an
/// edit or reply, and pasting files or images attaches them.
///
/// The field reports its selection (as Unicode scalar offsets) so
/// formatting wraps the selected text.
struct ComposerTextField: View {
    @Binding var text: String
    @Binding var selection: Range<Int>?
    let placeholder: String
    /// Kept in step with the system's focus both ways.
    @Binding var isFocused: Bool
    let onSubmit: () -> Void
    /// Returns true when a suggestion was inserted.
    let onAcceptSuggestion: (ComposerSuggestionKey) -> Bool
    /// Returns true when an edit or reply was cancelled.
    let onCancel: () -> Bool
    /// A paste the field cannot take as text: a copied file, GIF or picture.
    let onPaste: () -> Void

    var body: some View {
        ComposerTextView(
            text: $text,
            selection: $selection,
            isFocused: $isFocused,
            placeholder: placeholder,
            onSubmit: onSubmit,
            onAcceptSuggestion: onAcceptSuggestion,
            onCancel: onCancel,
            onPasteAttachments: onPaste
        )
        .overlay(alignment: .topLeading) {
            if text.isEmpty {
                Text(placeholder)
                    .font(.body)
                    .foregroundStyle(placeholderColor)
                    .lineLimit(1)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
        }
        .padding(.vertical, Theme.Spacing.xs + 2)
    }

    private var placeholderColor: Color {
        #if os(iOS)
        Color(uiColor: .placeholderText)
        #else
        Color(nsColor: .placeholderTextColor)
        #endif
    }
}
