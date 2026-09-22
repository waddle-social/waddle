import SwiftUI
import WaddleKit

/// A 1:1 conversation row: compact in the sidebar, two lines on iPhone.
struct DirectNavigationRow: View {
    @Environment(SessionCoordinator.self) private var session
    let direct: DirectConversation
    let style: ConversationLinkStyle

    var body: some View {
        let attention = ConversationAttention(session: session, conversation: direct.conversation)
        let title = session.directory.title(for: direct.conversation)
        let availability = session.presence.availability(of: direct.peer)
        ConversationLink(conversation: direct.conversation, style: style) {
            switch style {
            case .sidebar:
                SidebarDirectRowLabel(direct: direct, title: title, availability: availability, attention: attention)
            case .phone:
                PhoneDirectRowLabel(direct: direct, title: title, availability: availability, attention: attention)
            }
        }
        .conversationRowActions(direct.conversation, style: style)
    }
}

/// Mail/Slack-density DM row for the sidebar.
struct SidebarDirectRowLabel: View {
    let direct: DirectConversation
    let title: String
    let availability: Availability
    let attention: ConversationAttention

    var body: some View {
        Label {
            HStack(spacing: Theme.Spacing.s) {
                ConversationRowTitle(title: title, attention: attention)
                Spacer(minLength: Theme.Spacing.xs)
                ConversationRowTrailing(attention: attention)
            }
        } icon: {
            PeerPresenceAvatar(jid: direct.peer, name: title, availability: availability, size: 20)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
    }

    private var accessibilityText: String {
        "\(attention.accessibilityLabel(title: title)), \(AvailabilityTitle.title(availability))"
    }
}

/// Two-line DM row for iPhone lists: name and time, preview and badge.
struct PhoneDirectRowLabel: View {
    let direct: DirectConversation
    let title: String
    let availability: Availability
    let attention: ConversationAttention

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            PeerPresenceAvatar(jid: direct.peer, name: title, availability: availability, size: 44)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                    ConversationRowTitle(title: title, attention: attention)
                    Spacer(minLength: Theme.Spacing.xs)
                    Text(ListTimestamp.string(for: direct.lastActivity))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                    Text(direct.preview ?? " ")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.xs)
                    ConversationRowTrailing(attention: attention)
                }
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
    }

    private var accessibilityText: String {
        var parts = [attention.accessibilityLabel(title: title), AvailabilityTitle.title(availability)]
        if let preview = direct.preview, !preview.isEmpty {
            parts.append(preview)
        }
        parts.append(ListTimestamp.string(for: direct.lastActivity))
        return parts.joined(separator: ", ")
    }
}
