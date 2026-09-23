import SwiftUI
import WaddleKit

/// Mute glyph and unread badge at the end of a row.
struct ConversationRowTrailing: View {
    let attention: ConversationAttention

    var body: some View {
        HStack(spacing: Theme.Spacing.xs) {
            if attention.isMuted {
                Image(systemName: "bell.slash")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            if attention.isMention && attention.unread == 0 {
                Image(systemName: "at")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(Color.accentColor)
            }
            UnreadBadge(count: attention.unread, isMention: attention.isMention)
        }
        .accessibilityHidden(true)
    }
}

/// The row title: bold while unread, dimmed while muted and read.
struct ConversationRowTitle: View {
    let title: String
    let attention: ConversationAttention

    var body: some View {
        Text(title)
            .fontWeight(attention.isUnread ? .semibold : .regular)
            .foregroundStyle(isDimmed ? HierarchicalShapeStyle.secondary : HierarchicalShapeStyle.primary)
            .lineLimit(1)
    }

    private var isDimmed: Bool {
        attention.isMuted && !attention.isUnread
    }
}
