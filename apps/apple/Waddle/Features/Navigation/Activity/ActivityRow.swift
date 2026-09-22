import SwiftUI
import WaddleKit

/// One conversation in Activity: who, the newest message, and the count.
struct ActivityRow: View {
    @Environment(SessionCoordinator.self) private var session
    let entry: ActivityEntry
    let onOpen: () -> Void

    var body: some View {
        let preview = session.timelines.timeline(for: entry.conversation).lastContentItem.flatMap(RowPreview.text(for:))
        Button(action: onOpen) {
            HStack(alignment: .top, spacing: Theme.Spacing.m) {
                ActivityRowIcon(conversation: entry.conversation, title: entry.title)
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                        Text(entry.title)
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                        Spacer(minLength: Theme.Spacing.xs)
                        if let recency = entry.recency {
                            Text(ListTimestamp.string(for: recency))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                        Text(preview ?? fallbackPreview)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                        Spacer(minLength: Theme.Spacing.xs)
                        UnreadBadge(count: entry.unread, isMention: entry.isMention)
                    }
                }
            }
            .padding(.vertical, Theme.Spacing.xs)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText(preview: preview)))
        .accessibilityAddTraits(.isButton)
    }

    private var fallbackPreview: String {
        entry.isMention ? "You were mentioned" : "New messages"
    }

    private func accessibilityText(preview: String?) -> String {
        let label = RowAccessibility.label(title: entry.title, unread: entry.unread, isMention: entry.isMention, isMuted: false)
        return "\(label), \(preview ?? fallbackPreview)"
    }
}

/// Room symbol or DM avatar for an Activity row.
struct ActivityRowIcon: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID
    let title: String

    var body: some View {
        switch conversation.kind {
        case .direct:
            PeerPresenceAvatar(
                jid: conversation.jid,
                name: title,
                availability: session.presence.availability(of: conversation.jid),
                size: Theme.Size.avatar
            )
        case .room:
            RoomSymbolTile(
                symbol: session.directory.channel(for: conversation.jid).map { ChannelSymbol.name(for: $0) } ?? "number",
                colorKey: conversation.jid.description,
                size: Theme.Size.avatar
            )
        }
    }
}

/// A room glyph on a tinted rounded tile, sized like an avatar.
struct RoomSymbolTile: View {
    let symbol: String
    let colorKey: String
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.3, style: .continuous)
            .fill(Color.consistent(for: colorKey).opacity(0.18))
            .frame(width: size, height: size)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: size * 0.45, weight: .semibold))
                    .foregroundStyle(Color.consistent(for: colorKey))
            }
            .accessibilityHidden(true)
    }
}
