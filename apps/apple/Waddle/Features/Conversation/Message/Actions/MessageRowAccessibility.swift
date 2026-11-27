import SwiftUI
import WaddleKit

/// Give the withheld notice and Load media button their own VoiceOver elements.
struct MessageRowAccessibility: ViewModifier {
    let label: String
    let actions: MessageAccessibilityActions
    let withheld: MessageMedia.Withheld?
    let loadMedia: () -> Void

    func body(content: Content) -> some View {
        if let withheld {
            content.accessibilityRepresentation {
                VStack(alignment: .leading) {
                    messageElement(Text(label))
                    WithheldMediaCard(media: withheld, load: loadMedia)
                }
            }
        } else {
            messageElement(content)
        }
    }

    private func messageElement(_ content: some View) -> some View {
        content
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(label))
            .modifier(actions)
    }
}
