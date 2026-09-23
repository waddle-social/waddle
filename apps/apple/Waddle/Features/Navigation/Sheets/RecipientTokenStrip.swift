import SwiftUI
import WaddleKit

/// Picked people as removable chips.
struct RecipientTokenStrip: View {
    let tokens: [RecipientToken]
    let onRemove: (RecipientToken) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Spacing.xs) {
                ForEach(tokens) { token in
                    RecipientChip(token: token) {
                        onRemove(token)
                    }
                }
            }
            .padding(.vertical, Theme.Spacing.xxs)
        }
    }
}

/// One picked person.
struct RecipientChip: View {
    let token: RecipientToken
    let onRemove: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.xs) {
            JIDAvatar(jid: token.jid, name: token.name, size: 20)
            Text(token.name)
                .font(.subheadline)
                .lineLimit(1)
            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Remove \(token.name)"))
        }
        .padding(.leading, Theme.Spacing.xxs)
        .padding(.trailing, Theme.Spacing.s)
        .padding(.vertical, Theme.Spacing.xxs)
        .background(Capsule().fill(Color.accentColor.opacity(0.14)))
    }
}
