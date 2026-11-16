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
                    .foregroundStyle(Theme.Colors.fgSoft)
            }
            if attention.isMention && attention.unread == 0 {
                Image(systemName: "at")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(Theme.Colors.goldText)
            }
            UnreadBadge(count: attention.unread, isMention: attention.isMention)
        }
        .accessibilityHidden(true)
    }
}
