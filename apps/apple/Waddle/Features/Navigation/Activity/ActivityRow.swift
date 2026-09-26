import SwiftUI
import WaddleKit

/// A room's heading in Activity: its symbol, name, recency and count.
struct ActivityGroupRow: View {
    @Environment(SessionCoordinator.self) private var session
    let group: UnreadOverviewGroup
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: Theme.Spacing.m) {
                RoomSymbolTile(
                    symbol: session.directory.channel(for: group.room).map { ChannelSymbol.name(for: $0) } ?? "number",
                    colorKey: group.room.description,
                    size: Theme.Size.avatar
                )
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(group.title)
                        .font(.body.weight(.semibold))
                        .lineLimit(1)
                    if let updated = group.lastUpdated {
                        Text(ListTimestamp.string(for: Date(timeIntervalSince1970: TimeInterval(updated))))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: Theme.Spacing.xs)
                UnreadBadge(count: group.unread, isMention: group.mentionsMe)
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .padding(.vertical, Theme.Spacing.xs)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(ActivityCopy.groupLabel(group)))
        .accessibilityAddTraits(.isButton)
    }
}

/// An unread thread inside a room: its title and unread count.
struct ActivityThreadRow: View {
    let thread: UnreadOverviewThread
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: Theme.Spacing.s) {
                Image(systemName: "bubble.left.and.text.bubble.right")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(thread.title)
                    .font(.subheadline.weight(.medium))
                    .lineLimit(1)
                Spacer(minLength: Theme.Spacing.xs)
                UnreadBadge(count: thread.unread)
            }
            .padding(.leading, Theme.Spacing.l)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(ActivityCopy.threadLabel(thread)))
        .accessibilityAddTraits(.isButton)
    }
}

/// One unread message: author, time and a few lines of its text.
struct ActivityMessageRow: View {
    let item: TimelineItem
    var isThreadReply = false
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(alignment: .firstTextBaseline) {
                    Text(item.authorName)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.consistent(for: item.authorName))
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.s)
                    Text(ListTimestamp.string(for: item.sentAt))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(RowPreview.content(of: item) ?? "")
                    .font(.callout)
                    .lineLimit(3)
            }
            .padding(.leading, isThreadReply ? Theme.Spacing.l : 0)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(ActivityCopy.messageLabel(item, isThreadReply: isThreadReply)))
        .accessibilityAddTraits(.isButton)
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
