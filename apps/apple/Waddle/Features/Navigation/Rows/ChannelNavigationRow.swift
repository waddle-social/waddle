import SwiftUI
import WaddleKit

/// A room or group DM row in the sidebar or the phone home list.
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
        NavigationRowLabel(title: channel.name, isUnread: attention.isUnread, isDimmed: attention.isDimmed) {
            Image(systemName: ChannelSymbol.name(for: channel))
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(attention.isUnread ? Theme.Colors.fgMuted : Theme.Colors.fgSoft)
        } trailing: {
            ConversationRowTrailing(attention: attention)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(attention.accessibilityLabel(title: channel.name)))
    }
}
