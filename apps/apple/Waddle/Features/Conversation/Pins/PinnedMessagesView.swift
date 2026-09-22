import SwiftUI
import WaddleKit

/// Pinned messages of a room (`urn:waddle:pin:0`). Shown in the inspector
/// on iPad and Mac and as a sheet on iPhone.
struct PinnedMessagesView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let conversation: ConversationID
    /// Called after a pin was opened, so a presenting sheet can close.
    let onOpen: (() -> Void)?

    init(conversation: ConversationID, onOpen: (() -> Void)? = nil) {
        self.conversation = conversation
        self.onOpen = onOpen
    }

    var body: some View {
        content
            .navigationTitle("Pinned messages")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
    }

    @ViewBuilder
    private var content: some View {
        let pins = conversation.isRoom ? session.pins.pins(in: conversation.jid) : []
        if !conversation.isRoom {
            EmptyStateView(
                title: "No pinned messages",
                message: "Pinning is available in channels and group conversations.",
                symbol: "pin.slash"
            )
        } else if pins.isEmpty {
            EmptyStateView(
                title: "No pinned messages",
                message: "Pin important messages to keep them here.",
                symbol: "pin"
            )
        } else {
            List(pins, id: \.targetStanzaID) { pin in
                Button {
                    open()
                } label: {
                    PinnedMessageRow(pin: pin, room: conversation.jid)
                }
                .buttonStyle(.plain)
                .contextMenu {
                    unpinButton(for: pin)
                }
            }
            .listStyle(.plain)
        }
    }

    @ViewBuilder
    private func unpinButton(for pin: PinEntry) -> some View {
        let timeline = session.timelines.timeline(for: conversation)
        if let item = timeline.item(withID: pin.targetStanzaID) {
            Button(role: .destructive) {
                Task { _ = await session.setPinned(false, item) }
            } label: {
                Label("Unpin", systemImage: "pin.slash")
            }
        }
    }

    private func open() {
        if navigation.selection != conversation {
            navigation.open(conversation)
        }
        onOpen?()
    }
}

/// Author, text and when it was pinned.
private struct PinnedMessageRow: View {
    let pin: PinEntry
    let room: BareJID

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(alignment: .firstTextBaseline) {
                Text(author)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.consistent(for: colorKey))
                    .lineLimit(1)
                Spacer(minLength: Theme.Spacing.s)
                if let pinnedAt = pin.pinnedAt {
                    Text(pinnedAt.formatted(.relative(presentation: .named)))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Text(pin.preview.text.isEmpty ? "Attachment" : pin.preview.text)
                .font(.callout)
                .lineLimit(3)
        }
        .padding(.vertical, Theme.Spacing.xs)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var author: String {
        if let nick = pin.preview.authorNick, !nick.isEmpty { return nick }
        if let jid = pin.preview.author { return jid.resource ?? jid.bare.localpart ?? jid.bare.domain }
        return "Unknown"
    }

    /// XEP-0392: the author's bare JID unless it is only the room
    /// occupant address, then the nick.
    private var colorKey: String {
        guard let jid = pin.preview.author, jid.bare != room else { return author }
        return jid.bare.description
    }
}

/// Phone presentation of the pins list with its own stack and Done.
struct PinnedMessagesSheet: View {
    @Environment(\.dismiss) private var dismiss
    let conversation: ConversationID

    var body: some View {
        NavigationStack {
            PinnedMessagesView(conversation: conversation) { dismiss() }
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { dismiss() }
                    }
                }
        }
        .presentationDetents([.medium, .large])
    }
}
