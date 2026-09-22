import SwiftUI
import WaddleKit

/// The scrolling result list; keeps the highlighted row in view.
struct QuickSwitcherResults: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let entries: [QuickSwitcherEntry]
    let highlighted: ConversationID?
    let query: String
    let onHover: (ConversationID) -> Void
    let onOpen: (ConversationID) -> Void

    var body: some View {
        if entries.isEmpty {
            emptyState
        } else {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: Theme.Spacing.xxs) {
                        ForEach(entries) { entry in
                            QuickSwitcherRow(
                                entry: entry,
                                isHighlighted: entry.id == highlighted,
                                onHover: { onHover(entry.id) },
                                onOpen: { onOpen(entry.id) }
                            )
                            .id(entry.id)
                        }
                    }
                    .padding(Theme.Spacing.s)
                }
                .onChange(of: highlighted) { _, target in
                    guard let target else { return }
                    if reduceMotion {
                        proxy.scrollTo(target)
                    } else {
                        withAnimation(.easeOut(duration: 0.15)) {
                            proxy.scrollTo(target)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        if ConversationNameFilter.normalized(query).isEmpty {
            ContentUnavailableView(
                "No conversations yet",
                systemImage: "bubble.left.and.bubble.right",
                description: Text("Channels and messages you join show up here.")
            )
        } else {
            ContentUnavailableView.search(text: query)
        }
    }
}

/// One result: icon, name, context and unread state.
struct QuickSwitcherRow: View {
    let entry: QuickSwitcherEntry
    let isHighlighted: Bool
    let onHover: () -> Void
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: Theme.Spacing.m) {
                QuickSwitcherIcon(entry: entry)
                VStack(alignment: .leading, spacing: 0) {
                    Text(entry.title)
                        .fontWeight(entry.unread > 0 || entry.mentionsMe ? .semibold : .regular)
                        .lineLimit(1)
                    if let subtitle = entry.subtitle {
                        Text(subtitle)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: Theme.Spacing.s)
                UnreadBadge(count: entry.unread, isMention: entry.mentionsMe)
            }
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.vertical, Theme.Spacing.s)
            .frame(minHeight: 44)
            .background(highlight)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering in
            if hovering { onHover() }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
        .accessibilityAddTraits(traits)
    }

    private var traits: AccessibilityTraits {
        isHighlighted ? [.isButton, .isSelected] : [.isButton]
    }

    private var highlight: some View {
        RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
            .fill(isHighlighted ? Color.accentColor.opacity(0.18) : Color.clear)
    }

    private var accessibilityText: String {
        let label = RowAccessibility.label(title: entry.title, unread: entry.unread, isMention: entry.mentionsMe, isMuted: false)
        guard let subtitle = entry.subtitle else { return label }
        return "\(label), \(subtitle)"
    }
}

/// Room symbol or DM avatar for a quick switcher result.
struct QuickSwitcherIcon: View {
    @Environment(SessionCoordinator.self) private var session
    let entry: QuickSwitcherEntry

    var body: some View {
        switch entry.kind {
        case let .channel(kind):
            RoomSymbolTile(symbol: ChannelSymbol.name(for: kind), colorKey: entry.conversation.jid.description, size: 28)
        case .groupDM:
            RoomSymbolTile(symbol: "person.2", colorKey: entry.conversation.jid.description, size: 28)
        case .direct:
            PeerPresenceAvatar(
                jid: entry.conversation.jid,
                name: entry.title,
                availability: session.presence.availability(of: entry.conversation.jid),
                size: 28
            )
        }
    }
}
