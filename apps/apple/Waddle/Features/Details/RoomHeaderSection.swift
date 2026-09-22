import SwiftUI
import WaddleKit

/// Room identity: glyph, name, `#address` and description.
struct RoomHeaderSection: View {
    let room: BareJID
    let channel: Channel?

    var body: some View {
        Section {
            VStack(spacing: Theme.Spacing.s) {
                RoomSymbolTile(
                    symbol: channel.map { ChannelSymbol.name(for: $0) } ?? "number",
                    colorKey: room.description,
                    size: 64
                )
                Text(title)
                    .font(.title2.weight(.bold))
                    .multilineTextAlignment(.center)
                if let subtitle {
                    Text(subtitle)
                        .font(.subheadline.monospaced())
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                if let summary {
                    Text(summary)
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.s)
            .accessibilityElement(children: .combine)
        }
    }

    private var title: String {
        channel?.name ?? room.localpart ?? room.description
    }

    /// `#slug` for channels; group chats have no public address.
    private var subtitle: String? {
        if channel?.isGroupDM == true { return "Group chat" }
        guard let localpart = room.localpart else { return nil }
        return "#\(localpart)"
    }

    private var summary: String? {
        guard let text = channel?.summary?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }
}
