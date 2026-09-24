import SwiftUI

/// The row under the text: `+`, formatting, emoji, mention and command
/// buttons, then Send.
struct ComposerActionRow: View {
    let isEditing: Bool
    let canSend: Bool
    /// Rooms only: 1:1 conversations have nobody else to mention.
    let showsMention: Bool
    @Binding var showsFormatting: Bool
    let actions: ComposerActions

    var body: some View {
        HStack(spacing: Theme.Spacing.xxs) {
            ComposerPlusMenu(isEditing: isEditing, actions: actions)
            ComposerIconButton(
                symbol: "textformat",
                label: showsFormatting ? "Hide formatting" : "Show formatting",
                isOn: showsFormatting
            ) {
                showsFormatting.toggle()
            }
            ComposerEmojiButton(onPick: actions.insertEmoji)
            if showsMention {
                ComposerIconButton(symbol: "at", label: "Mention someone", action: actions.startMention)
                    .disabled(isEditing)
            }
            ComposerIconButton(symbol: "slash.circle", label: "Run a command", action: actions.startCommand)
                .disabled(isEditing)
            Spacer(minLength: Theme.Spacing.s)
            ComposerSendButton(isEditing: isEditing, isEnabled: canSend, action: actions.submit)
        }
    }
}
