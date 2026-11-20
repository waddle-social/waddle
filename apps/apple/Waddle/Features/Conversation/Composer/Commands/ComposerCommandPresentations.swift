import SwiftUI
import WaddleKit

/// What slash commands and the formatting bar present over the composer:
/// the GIF picker, the sticker picker, an extension command's form, and
/// the link prompt.
struct ComposerCommandPresentations: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Binding var gifSearch: ComposerGifSearch?
    @Binding var showsStickers: Bool
    @Binding var linkPrompt: ComposerLinkPrompt
    let commands: ComposerCommandCenter
    let onPickGIF: (GifSearchItem) -> Void
    let onPickSticker: (Sticker, StickerPack) -> Void
    /// The typed link, validated by the composer.
    let onAddLink: (String) -> Void

    func body(content: Content) -> some View {
        content
            .sheet(item: $gifSearch) { search in
                GifPickerView(
                    initialQuery: search.query,
                    onPick: onPickGIF,
                    onCancel: { gifSearch = nil }
                )
                .carryingCallDock()
                .environment(session)
            }
            .sheet(isPresented: $showsStickers) {
                StickerPickerSheet(onPick: onPickSticker, onCancel: { showsStickers = false })
                    .carryingCallDock()
                    .environment(session)
            }
            .sheet(item: formStage) { _ in
                ExtensionCommandFormSheet(center: commands)
                    .carryingCallDock()
                    .environment(session)
            }
            .alert("Add a link", isPresented: $linkPrompt.isPresented) {
                TextField("https://", text: $linkPrompt.text)
                Button("Add") {
                    onAddLink(linkPrompt.text)
                    linkPrompt.text = ""
                }
                Button("Cancel", role: .cancel) {
                    linkPrompt.text = ""
                }
            } message: {
                Text("The address is sent as is; it shows as a link.")
            }
    }

    /// Dismissing the sheet cancels a command still awaiting input.
    private var formStage: Binding<ExtensionCommandStage?> {
        Binding(
            get: { commands.stage },
            set: { stage in
                if stage == nil {
                    commands.dismiss(session: session)
                }
            }
        )
    }
}
