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
                    size: Theme.Size.emptyMark
                )
                Text(title)
                    .huddleText(Theme.Typography.title)
                    .foregroundStyle(Theme.Colors.fg)
                    .multilineTextAlignment(.center)
                if let subtitle {
                    Text(subtitle)
                        .huddleText(Theme.Typography.code)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .textSelection(.enabled)
                }
                if let summary {
                    Text(summary)
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fgMuted)
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
