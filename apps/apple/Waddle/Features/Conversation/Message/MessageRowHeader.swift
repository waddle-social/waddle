import SwiftUI
import WaddleKit

/// Author name, role badge, time and edited mark (web `wd-msg__meta`).
/// The name opens the author's profile. Its hit area grows sideways, up
/// into the row's top padding (`topSlop`) and down only across the gap
/// above the body (`bottomSlop`), so it never covers the body's controls.
struct MessageRowHeader: View {
    let author: MessageAuthor
    let item: TimelineItem
    let topSlop: CGFloat
    let bottomSlop: CGFloat
    let openProfile: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
            Text(author.name)
                .huddleText(Theme.Typography.author)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
                .authorProfileButton(
                    name: author.name,
                    horizontalSlop: Theme.Spacing.s,
                    topSlop: topSlop,
                    bottomSlop: bottomSlop,
                    action: openProfile
                )
            if let badge = author.badge {
                MessageRoleBadgeView(badge: badge)
            }
            Text(item.sentAt.formatted(date: .omitted, time: .shortened))
                .huddleText(Theme.Typography.timestamp)
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.fgMuted)
                .help(item.sentAt.formatted(date: .complete, time: .shortened))
            if item.isEdited, item.tombstone == nil {
                Text("(edited)")
                    .huddleText(Theme.Typography.timestamp)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        }
    }
}

/// Small caps label for a bot, hat, owner, admin or moderator (web
/// message badge): no fill, toned by role.
struct MessageRoleBadgeView: View {
    let badge: MessageRoleBadge

    var body: some View {
        Text(badge.title)
            .font(HuddleFonts.swiftUIFont(.body, size: 10, weight: 700))
            .tracking(0.6)
            .textCase(.uppercase)
            .lineLimit(1)
            .foregroundStyle(tint)
    }

    private var tint: Color {
        switch badge {
        case .owner: return Theme.Colors.warningText
        case .admin, .moderator: return Theme.Colors.actionText
        case .bot: return Theme.Colors.successText
        case .hat: return Theme.Colors.fgMuted
        }
    }
}

/// The author's XEP-0084 avatar when their JID is known, else initials in
/// their XEP-0392 color.
struct MessageAvatar: View {
    let author: MessageAuthor
    let size: CGFloat

    var body: some View {
        if let jid = author.avatarJID {
            JIDAvatar(jid: jid, name: author.name, size: size)
        } else {
            AvatarView(name: author.name, size: size)
        }
    }
}
