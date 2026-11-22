import SwiftUI

/// The `+` menu: attach a photo, file, GIF or sticker, paste (Mac), or
/// browse slash commands. In a compact composer it also holds what a wider
/// one shows as buttons: formatting, emoji and mentions. While editing only
/// what an edit can use stays enabled.
struct ComposerPlusMenu: View {
    let isEditing: Bool
    let showsMention: Bool
    @Binding var showsFormatting: Bool
    /// A compact composer has no room for the extra buttons, so the menu
    /// holds them.
    let holdsExtraButtons: Bool
    let actions: ComposerActions
    @State private var showsEmoji = false

    var body: some View {
        Menu {
            attachments
                .disabled(isEditing)
            Divider()
            if holdsExtraButtons {
                Toggle(isOn: $showsFormatting) {
                    Label("Formatting", systemImage: "textformat")
                }
                Button { showsEmoji = true } label: {
                    Label("Emoji", systemImage: "face.smiling")
                }
                if showsMention {
                    Button(action: actions.startMention) {
                        Label("Mention someone", systemImage: "at")
                    }
                    .disabled(isEditing)
                }
            }
            Button(action: actions.startCommand) {
                Label("Commands…", systemImage: "slash.circle")
            }
            .disabled(isEditing)
        } label: {
            ComposerIconLabel(symbol: "plus")
        }
        .menuStyle(.button)
        .buttonStyle(ComposerIconButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Add")
        .accessibilityLabel(Text("Add"))
        .popover(isPresented: $showsEmoji) {
            MessageEmojiPicker { emoji in
                actions.insertEmoji(emoji)
                showsEmoji = false
            }
            .presentationDetents([.medium, .large])
        }
    }

    @ViewBuilder
    private var attachments: some View {
        Button(action: actions.pickPhoto) {
            Label("Photo", systemImage: "photo.on.rectangle")
        }
        Button(action: actions.pickFile) {
            Label("File", systemImage: "doc")
        }
        Button(action: actions.pickGIF) {
            Label("GIF", systemImage: "play.rectangle.on.rectangle")
        }
        Button(action: actions.pickSticker) {
            Label("Sticker", systemImage: "face.smiling")
        }
        #if os(macOS)
        // iOS pastes pictures and files straight into the field, from
        // the edit menu or Cmd-V; reading the pasteboard from a menu
        // item there would stop on the system's paste prompt.
        Button(action: actions.paste) {
            Label("Paste", systemImage: "doc.on.clipboard")
        }
        #endif
    }
}
