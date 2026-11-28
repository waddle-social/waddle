import SwiftUI
import WaddleKit

/// A room or group DM row in the sidebar or the phone home list.
struct ChannelNavigationRow: View {
    @Environment(SessionCoordinator.self) private var session
    let channel: Channel
    let style: ConversationLinkStyle
    var huddleParticipantCount = 0
    var leadingIndent: CGFloat = 0

    var body: some View {
        let attention = ConversationAttention(session: session, conversation: channel.conversation)
        ConversationLink(conversation: channel.conversation, style: style) {
            ChannelRowLabel(channel: channel, attention: attention, huddleParticipantCount: huddleParticipantCount)
                .padding(.leading, leadingIndent)
        }
        .conversationRowActions(channel.conversation, style: style)
    }
}

/// Symbol, name and trailing state of a room row.
struct ChannelRowLabel: View {
    let channel: Channel
    let attention: ConversationAttention
    let huddleParticipantCount: Int

    var body: some View {
        NavigationRowLabel(title: channel.name, isEmphasised: attention.isUnread, isDimmed: attention.isDimmed) {
            Image(systemName: ChannelSymbol.name(for: channel))
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(attention.isUnread ? Theme.Colors.fgMuted : Theme.Colors.fgSoft)
        } trailing: {
            if huddleParticipantCount > 0 {
                HStack(spacing: Theme.Spacing.xs) {
                    SidebarHuddleCount(count: huddleParticipantCount)
                    ConversationRowTrailing(attention: attention)
                }
            } else {
                ConversationRowTrailing(attention: attention)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityLabel))
    }

    private var accessibilityLabel: String {
        let label = attention.accessibilityLabel(title: channel.name)
        guard huddleParticipantCount > 0 else { return label }
        return "\(label), \(SidebarHuddleCount.description(for: huddleParticipantCount))"
    }
}
