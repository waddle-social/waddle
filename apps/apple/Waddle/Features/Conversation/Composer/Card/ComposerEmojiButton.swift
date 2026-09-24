import SwiftUI

/// Opens the emoji grid and inserts the picked emoji into the draft.
struct ComposerEmojiButton: View {
    let onPick: (String) -> Void
    @State private var isPresented = false

    var body: some View {
        ComposerIconButton(symbol: "face.smiling", label: "Emoji") {
            isPresented = true
        }
        .popover(isPresented: $isPresented) {
            MessageEmojiPicker { emoji in
                onPick(emoji)
                isPresented = false
            }
            .presentationDetents([.medium, .large])
        }
    }
}
