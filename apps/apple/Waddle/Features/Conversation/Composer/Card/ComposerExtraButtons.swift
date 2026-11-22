import SwiftUI

/// The buttons after the text on a Mac or iPad (web composer): formatting,
/// emoji, mention and commands. A phone has the keyboard's emoji key and
/// keeps the rest in the `+` menu.
struct ComposerExtraButtons: View {
    let isEditing: Bool
    /// Rooms only: 1:1 conversations have nobody else to mention.
    let showsMention: Bool
    @Binding var showsFormatting: Bool
    let actions: ComposerActions

    var body: some View {
        HStack(spacing: WaddleTokens.Spacing.s0Half) {
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
        }
    }
}
