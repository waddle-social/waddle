import SwiftUI
import WaddleKit

struct RoomBrowseRow: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    let row: BrowseRooms.Row
    let isJoining: Bool
    let showsFaces: Bool
    let action: () -> Void

    var body: some View {
        let layout =
            typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: Theme.Spacing.s))
            : AnyLayout(HStackLayout(spacing: Theme.Spacing.m))
        return layout {
            titleButton
            HStack(spacing: Theme.Spacing.s) {
                if row.isLive && showsFaces && !typeSize.isAccessibilitySize {
                    faces
                }
                UnreadBadge(count: row.unread, isMention: row.mentionsMe)
                if isJoining {
                    ProgressView()
                        .controlSize(.small)
                        .accessibilityLabel(Text("Joining \(row.channel.name)"))
                } else {
                    Button(row.action.rawValue, action: action)
                        .buttonStyle(.huddle(size: .small))
                        .accessibilityLabel(Text("\(row.action.rawValue) \(row.channel.name)"))
                }
            }
            .fixedSize(horizontal: true, vertical: false)
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.s)
        .frame(minHeight: WaddleTokens.Spacing.s16)
        .background(row.isLive ? Theme.Colors.liveSubtle : Theme.Colors.surface)
    }

    private var titleButton: some View {
        Button(action: action) {
            HStack(spacing: Theme.Spacing.m) {
                Image(systemName: ChannelSymbol.name(for: row.channel))
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .frame(width: Theme.Size.rowGlyph)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(row.channel.name)
                        .huddleText(Theme.Typography.author)
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(typeSize.isAccessibilitySize ? nil : 1)
                    Text(detail)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(row.isLive ? Theme.Colors.liveText : Theme.Colors.fgMuted)
                        .lineLimit(typeSize.isAccessibilitySize ? nil : 1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .huddleTouchTarget()
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(row.channel.name))
        .accessibilityValue(Text(detail))
    }

    private var detail: String {
        [row.metadata, row.preview].compactMap { $0 }.joined(separator: " · ")
    }

    private var faces: some View {
        HStack(spacing: -WaddleTokens.Spacing.s1Half) {
            ForEach(row.people.prefix(4)) { person in
                CallPersonAvatar(name: person.nick, jid: person.jid, size: Theme.Size.smallAvatar)
                    .overlay { Circle().strokeBorder(Theme.Colors.liveSubtle, lineWidth: WaddleTokens.BorderWidth.strong) }
            }
        }
        .accessibilityHidden(true)
    }
}
