import SwiftUI
import WaddleKit

/// Preserve the message's actions and expose controls that need their own focus.
struct MessageRowAccessibility: ViewModifier {
    let label: String
    let actions: MessageAccessibilityActions
    let media: MessageMedia
    let loadMedia: () -> Void
    let hasInlineControls: Bool

    func body(content: Content) -> some View {
        if let withheld = media.withheld {
            content.accessibilityRepresentation {
                VStack(alignment: .leading) {
                    messageElement(Text(label))
                    WithheldMediaCard(media: withheld, load: loadMedia)
                }
            }
        } else if hasInlineControls {
            content
                .accessibilityElement(children: .contain)
                .accessibilityLabel(Text(label))
                .modifier(actions)
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
