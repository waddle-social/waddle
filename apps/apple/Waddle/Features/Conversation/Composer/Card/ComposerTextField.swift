import SwiftUI
import WaddleKit

/// Multi-line field. Return sends on a hardware keyboard (the on-screen
/// keyboard's Return adds a line), Shift or Option with Return adds a
/// line, Tab or Return accepts the selected suggestion, Escape closes the
/// list or cancels an edit or reply, and pasting files or images attaches them.
///
/// The field reports its selection (as Unicode scalar offsets) so
/// formatting wraps the selected text.
struct ComposerTextField: View {
    @Binding var text: String
    @Binding var selection: ComposerScalarRange?
    let placeholder: String
    /// Kept in step with the system's focus both ways.
    @Binding var isFocused: Bool
    let onSubmit: () -> Void
    /// Returns true when an open suggestion list used the key.
    let onSuggestionKey: (ComposerHardwareKey) -> Bool
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
            onSuggestionKey: onSuggestionKey,
            onCancel: onCancel,
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
