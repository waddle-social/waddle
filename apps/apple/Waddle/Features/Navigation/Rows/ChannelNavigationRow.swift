import SwiftUI
import WaddleKit

/// A channel or group DM row in the sidebar or the phone home list.
struct ChannelNavigationRow: View {
    @Environment(SessionCoordinator.self) private var session
    let channel: Channel
    let style: ConversationLinkStyle

    var body: some View {
        let attention = ConversationAttention(session: session, conversation: channel.conversation)
        ConversationLink(conversation: channel.conversation, style: style) {
            ChannelRowLabel(channel: channel, attention: attention)
        }
        .conversationRowActions(channel.conversation, style: style)
    }
}

/// Symbol, name and trailing state of a room row.
struct ChannelRowLabel: View {
    let channel: Channel
    let attention: ConversationAttention

    var body: some View {
        Label {
            HStack(spacing: Theme.Spacing.s) {
                ConversationRowTitle(title: channel.name, attention: attention)
                Spacer(minLength: Theme.Spacing.xs)
                ConversationRowTrailing(attention: attention)
            }
        } icon: {
            Image(systemName: ChannelSymbol.name(for: channel))
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(attention.accessibilityLabel(title: channel.name)))
    }
}
