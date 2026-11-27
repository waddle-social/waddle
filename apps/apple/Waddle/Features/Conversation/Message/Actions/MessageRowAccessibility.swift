import SwiftUI
import WaddleKit

/// Give the withheld notice and Load media button their own VoiceOver elements.
struct MessageRowAccessibility: ViewModifier {
    let label: String
    let actions: MessageAccessibilityActions
    let media: MessageMedia
    let loadMedia: () -> Void

    func body(content: Content) -> some View {
        if let withheld = media.withheld {
            content.accessibilityRepresentation {
                VStack(alignment: .leading) {
                    messageElement(Text(label))
                    WithheldMediaCard(media: withheld, load: loadMedia)
                }
            }
        } else {
            messageElement(content, children: media.linkPreviews.contains { $0.playback != nil } ? .contain : .ignore)
        }
    }

    private func messageElement(_ content: some View, children: AccessibilityChildBehavior = .ignore) -> some View {
        content
            .accessibilityElement(children: children)
            .accessibilityLabel(Text(label))
            .modifier(actions)
    }
}
