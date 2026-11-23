import SwiftUI
import WaddleKit

/// A 1:1 conversation row. One line in the sidebar and on the phone's
/// Home; two lines, with a preview, in the phone's DMs tab.
struct DirectNavigationRow: View {
    @Environment(SessionCoordinator.self) private var session
    let direct: DirectConversation
    let style: ConversationLinkStyle
    /// One line even on the phone.
    var isCompact = false

    var body: some View {
        let attention = ConversationAttention(session: session, conversation: direct.conversation)
        let title = session.directory.title(for: direct.conversation)
        let availability = session.presence.availability(of: direct.peer)
        let isInCall = session.presence.isInCall(direct.peer)
        ConversationLink(conversation: direct.conversation, style: style) {
            if style == .sidebar || isCompact {
                CompactDirectRowLabel(
                    direct: direct, title: title, availability: availability, isInCall: isInCall, attention: attention
                )
            } else {
                PhoneDirectRowLabel(
                    direct: direct, title: title, availability: availability, isInCall: isInCall, attention: attention
                )
            }
        }
        .conversationRowActions(direct.conversation, style: style)
    }
}

/// One-line DM row: avatar with presence, name, unread.
struct CompactDirectRowLabel: View {
    let direct: DirectConversation
    let title: String
    let availability: Availability
    let isInCall: Bool
    let attention: ConversationAttention

    var body: some View {
        NavigationRowLabel(title: title, isEmphasised: attention.isUnread, isDimmed: attention.isDimmed) {
            PeerPresenceAvatar(
                jid: direct.peer, name: title, availability: availability, size: 20, isInCall: isInCall
            )
        } trailing: {
            ConversationRowTrailing(attention: attention)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
    }

    private var accessibilityText: String {
        ([attention.accessibilityLabel(title: title)] + AvailabilityTitle.spoken(availability, isInCall: isInCall))
            .joined(separator: ", ")
    }
}

/// Two-line DM row for the phone's DMs tab: name and time, preview and badge.
struct PhoneDirectRowLabel: View {
    let direct: DirectConversation
    let title: String
    let availability: Availability
    let isInCall: Bool
    let attention: ConversationAttention

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            PeerPresenceAvatar(
                jid: direct.peer, name: title, availability: availability, size: 40, isInCall: isInCall
            )
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                    NavigationRowTitle(title: title, isEmphasised: attention.isUnread, isDimmed: attention.isDimmed)
                    Spacer(minLength: Theme.Spacing.xs)
                    Text(ListTimestamp.string(for: direct.lastActivity))
                        .huddleText(Theme.Typography.timestamp)
                        .foregroundStyle(Theme.Colors.fgSoft)
                }
                HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                    Text(direct.preview ?? " ")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.xs)
                    ConversationRowTrailing(attention: attention)
                }
            }
        }
        .padding(.vertical, Theme.Spacing.s)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
    }

    private var accessibilityText: String {
        var parts = [attention.accessibilityLabel(title: title)]
        parts += AvailabilityTitle.spoken(availability, isInCall: isInCall)
        if let preview = direct.preview, !preview.isEmpty {
            parts.append(preview)
        }
        parts.append(ListTimestamp.string(for: direct.lastActivity))
        return parts.joined(separator: ", ")
    }
}
