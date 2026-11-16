import SwiftUI
import WaddleKit

/// One room in the Inbox: its name and count, then its unread messages and
/// the threads with unread replies.
struct InboxRoomCard: View {
    @Environment(SessionCoordinator.self) private var session
    let group: UnreadOverviewGroup
    let onOpenRoom: () -> Void
    let onOpenThread: (ThreadKey) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            ForEach(group.messages, id: \.presentationID) { item in
                divider
                InboxMessageRow(item: item, onOpen: onOpenRoom)
            }
            ForEach(group.threads) { thread in
                divider
                InboxThreadRow(thread: thread) { onOpenThread(thread.key) }
                ForEach(thread.messages, id: \.presentationID) { item in
                    InboxMessageRow(item: item, isThreadReply: true) { onOpenThread(thread.key) }
                }
            }
            if group.isIncomplete {
                divider
                Label("Some messages didn't load. Pull down to try again.", systemImage: "exclamationmark.triangle")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.warningText)
                    .padding(Theme.Spacing.m)
            }
        }
        .huddleSurface(.card, radius: Theme.Radius.card)
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: session.directory.channel(for: group.room).map { ChannelSymbol.name(for: $0) } ?? "number")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Theme.Colors.fgSoft)
                .accessibilityHidden(true)
            Text(group.title)
                .huddleText(Theme.Typography.author)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
            UnreadBadge(count: group.unread, isMention: group.mentionsMe)
            Spacer(minLength: Theme.Spacing.s)
            Button("Open", action: onOpenRoom)
                .buttonStyle(.huddle(size: .small))
                .accessibilityLabel(Text("Open \(group.title)"))
        }
        .padding(.leading, Theme.Spacing.l)
        .padding(.trailing, Theme.Spacing.s)
        .padding(.vertical, Theme.Spacing.xs)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(InboxCopy.groupLabel(group)))
    }

    private var divider: some View {
        Rectangle()
            .fill(Theme.Colors.borderSoft)
            .frame(height: WaddleTokens.BorderWidth.hair)
    }
}

/// An unread thread inside a room: its title and unread count.
struct InboxThreadRow: View {
    let thread: UnreadOverviewThread
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: Theme.Spacing.s) {
                Image(systemName: "bubble.left.and.text.bubble.right")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Theme.Colors.fgSoft)
                    .accessibilityHidden(true)
                Text(thread.title)
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
                Spacer(minLength: Theme.Spacing.xs)
                UnreadBadge(count: thread.unread)
            }
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.top, Theme.Spacing.m)
            .padding(.bottom, Theme.Spacing.xs)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(InboxCopy.threadLabel(thread)))
        .accessibilityAddTraits(.isButton)
    }
}

/// One unread message: author avatar, name, time and a few lines of its
/// text.
struct InboxMessageRow: View {
    @Environment(SessionCoordinator.self) private var session
    let item: TimelineItem
    var isThreadReply = false
    let onOpen: () -> Void

    var body: some View {
        let author = MessageAuthor.resolve(item, occupant: nil, authorJID: session.authorJID(of: item))
        Button(action: onOpen) {
            HStack(alignment: .top, spacing: Theme.Spacing.m) {
                MessageAvatar(author: author, size: Theme.Size.rowAvatar)
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
                        Text(item.authorName)
                            .huddleText(Theme.Typography.author)
                            .foregroundStyle(Theme.Colors.fg)
                            .lineLimit(1)
                        Text(ListTimestamp.string(for: item.sentAt))
                            .huddleText(Theme.Typography.timestamp)
                            .foregroundStyle(Theme.Colors.fgSoft)
                    }
                    Text(RowPreview.content(of: item) ?? "")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(3)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
            }
            .padding(.leading, isThreadReply ? Theme.Spacing.xl + Theme.Spacing.l : Theme.Spacing.l)
            .padding(.trailing, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.m)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(InboxCopy.messageLabel(item, isThreadReply: isThreadReply)))
        .accessibilityAddTraits(.isButton)
    }
}

/// A room glyph on a quiet rounded tile, sized like an avatar.
struct RoomSymbolTile: View {
    let symbol: String
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.3, style: .continuous)
            .fill(Theme.Colors.surface2)
            .frame(width: size, height: size)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: size * 0.45, weight: .semibold))
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .accessibilityHidden(true)
    }
}
