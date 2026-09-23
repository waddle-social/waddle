import SwiftUI
import WaddleKit

/// Author name in its XEP-0392 color, role badge, time and edited mark.
struct MessageRowHeader: View {
    let author: MessageAuthor
    let item: TimelineItem

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s - 2) {
            Text(author.name)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.consistent(for: author.colorKey))
                .lineLimit(1)
            if let badge = author.badge {
                MessageRoleBadgeView(badge: badge)
            }
            Text(item.sentAt.formatted(date: .omitted, time: .shortened))
                .font(.caption)
                .foregroundStyle(.secondary)
                .help(item.sentAt.formatted(date: .complete, time: .shortened))
            if item.isEdited, item.tombstone == nil {
                Text("edited")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
    }
}

/// Small capsule for a hat, owner, admin or moderator.
struct MessageRoleBadgeView: View {
    let badge: MessageRoleBadge

    var body: some View {
        Text(badge.title)
            .font(.caption2.weight(.semibold))
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .foregroundStyle(tint)
            .background(Capsule().fill(tint.opacity(0.14)))
    }

    private var tint: Color {
        switch badge {
        case .owner: return .orange
        case .admin: return .purple
        case .moderator: return .blue
        case .hat: return .teal
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
            AvatarView(name: author.name, colorKey: author.colorKey, size: size)
        }
    }
}
