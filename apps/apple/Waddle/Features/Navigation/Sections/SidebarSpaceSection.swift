import SwiftUI
import WaddleKit

/// One disclosure row and, while open, its rooms and unread discussions.
struct SidebarSpaceSection: View {
    let group: SidebarRoomGroups.Group
    @Binding var isExpanded: Bool
    let createRoom: (() -> Void)?

    var body: some View {
        header
        if isExpanded {
            if group.rooms.isEmpty {
                Text("No rooms yet.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .padding(.leading, Theme.Spacing.l)
                    .navigationListRow()
            }
            ForEach(group.rooms) { room in
                ChannelNavigationRow(
                    channel: room.channel, style: .sidebar,
                    huddleParticipantCount: room.huddleParticipantCount,
                    leadingIndent: Theme.Spacing.s
                )
                ForEach(room.threads) { thread in
                    SidebarDiscussionRow(thread: thread)
                }
            }
        }
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.xxs) {
            Button {
                isExpanded.toggle()
            } label: {
                NavigationRowChrome {
                    HStack(spacing: Theme.Spacing.s) {
                        Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                            .font(.caption.weight(.semibold))
                            .frame(width: Theme.Spacing.m)
                            .accessibilityHidden(true)
                        Text(group.title)
                            .huddleText(Theme.Typography.sidebarSection)
                            .lineLimit(1)
                        Spacer(minLength: 0)
                        if !isExpanded {
                            activity
                        }
                        Text(group.rooms.count.formatted())
                            .huddleText(Theme.Typography.caption)
                            .monospacedDigit()
                            .fixedSize()
                    }
                    .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text(group.title))
            .accessibilityValue(Text(accessibilityValue))
            .accessibilityAddTraits(.isHeader)
            if let createRoom {
                HuddleIconButton(
                    group.space == nil ? "Create standalone room" : "Create room in \(group.title)",
                    symbol: "plus", side: NavigationIconLabel.target, action: createRoom
                )
                .foregroundStyle(Theme.Colors.fgSoft)
            }
        }
        .navigationListRow(top: Theme.Spacing.xs)
    }

    @ViewBuilder
    private var activity: some View {
        SidebarHuddleCount(count: group.activity.huddleParticipantCount)
            .accessibilityHidden(true)
        switch group.activity.badge {
        case let .mentions(count):
            UnreadBadge(count: count, isMention: true).accessibilityHidden(true)
        case let .unread(count):
            UnreadBadge(count: count).accessibilityHidden(true)
        case nil:
            EmptyView()
        }
    }

    private var accessibilityValue: String {
        var parts = [isExpanded ? "Expanded" : "Collapsed", group.rooms.count == 1 ? "1 room" : "\(group.rooms.count) rooms"]
        if !isExpanded {
            let activity = group.activity
            if activity.huddleParticipantCount > 0 {
                parts.append(SidebarHuddleCount.description(for: activity.huddleParticipantCount))
            }
            if activity.mentionedRoomCount > 0 {
                parts.append(activity.mentionedRoomCount == 1 ? "Mentions in 1 room" : "Mentions in \(activity.mentionedRoomCount) rooms")
            }
            if activity.unread > 0 { parts.append("\(activity.unread) unread") }
        }
        return parts.joined(separator: ", ")
    }
}

struct SidebarHuddleCount: View {
    let count: Int

    var body: some View {
        if count > 0 {
            HStack(spacing: Theme.Spacing.xxs) {
                CallBadge()
                Text(count.formatted())
                    .huddleText(Theme.Typography.badge)
                    .foregroundStyle(Theme.Colors.liveText)
                    .monospacedDigit()
            }
            .fixedSize()
        }
    }

    static func description(for count: Int) -> String {
        count == 1 ? "1 person in a huddle" : "\(count) people in huddles"
    }
}

private struct SidebarDiscussionRow: View {
    @Environment(NavigationModel.self) private var navigation
    let thread: SidebarRoomGroups.Thread

    var body: some View {
        Button {
            let conversation = ConversationID.room(thread.key.room)
            navigation.open(conversation)
            navigation.openThread(thread.key.threadID, in: conversation, usesInspector: true)
        } label: {
            NavigationRowChrome {
                NavigationRowLabel(title: thread.title, isEmphasised: true) {
                    Image(systemName: "bubble.left.and.text.bubble.right")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.Colors.fgSoft)
                } trailing: {
                    UnreadBadge(count: thread.unread)
                }
                .padding(.leading, Theme.Spacing.l + Theme.Spacing.m)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Discussion \(thread.title)"))
        .accessibilityValue(Text(thread.unread == 1 ? "1 unread reply" : "\(thread.unread) unread replies"))
        .navigationListRow()
    }
}
