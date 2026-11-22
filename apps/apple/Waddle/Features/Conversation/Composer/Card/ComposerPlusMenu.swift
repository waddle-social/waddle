import SwiftUI

/// The `+` menu: attach a photo, file, GIF or sticker, paste (Mac), or
/// browse slash commands. On a phone it also holds what a wider composer
/// shows as buttons: formatting and mentions.
struct ComposerPlusMenu: View {
    let isEditing: Bool
    let showsMention: Bool
    @Binding var showsFormatting: Bool
    /// The phone's one-row composer has no room for the other buttons.
    let collectsActions: Bool
    let actions: ComposerActions

    var body: some View {
        Menu {
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
            Divider()
            if collectsActions {
                Toggle(isOn: $showsFormatting) {
                    Label("Formatting", systemImage: "textformat")
                }
                if showsMention {
                    Button(action: actions.startMention) {
                        Label("Mention someone", systemImage: "at")
                    }
                }
            }
            Button(action: actions.startCommand) {
                Label("Commands…", systemImage: "slash.circle")
            }
        } label: {
            ComposerIconLabel(symbol: "plus")
        }
        .menuStyle(.button)
        .buttonStyle(ComposerIconButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(isEditing)
        .help("Add")
        .accessibilityLabel(Text("Add"))
    }
}
