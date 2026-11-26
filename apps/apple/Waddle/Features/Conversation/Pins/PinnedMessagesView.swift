import SwiftUI
import WaddleKit

/// Pinned messages of a room or of a conversation of two (`urn:waddle:pin:0`). Shown in the inspector
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
        let pins = session.pins.pins(in: conversation.jid)
        if pins.isEmpty {
            EmptyStateView(
                title: "No pinned messages",
                message: "Pin important messages to keep them here.",
                symbol: "pin"
            )
        } else {
            List(pins, id: \.targetStanzaID) { pin in
                Button {
                    open(pin)
                } label: {
                    PinnedMessageRow(pin: pin, room: conversation.jid, author: author(of: pin))
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
        // By the pin's own id: the message may be far back and not loaded.
        if session.pinAccess(in: conversation) == .allowed {
            Button(role: .destructive) {
                Task { _ = await session.unpin(pin, in: conversation) }
            } label: {
                Label("Unpin", systemImage: "pin.slash")
            }
        }
    }

    /// A room's occupant by their nick; in a conversation of two, the person by the name the app has for them.
    private func author(of pin: PinEntry) -> String {
        if let nick = pin.preview.authorNick, !nick.isEmpty { return nick }
        guard let jid = pin.preview.author else { return "Unknown" }
        if !conversation.isRoom { return session.directory.title(for: .direct(jid.bare)) }
        return jid.resource ?? jid.bare.localpart ?? jid.bare.domain
    }

    private func open(_ pin: PinEntry) {
        navigation.focus(pin.targetStanzaID, in: conversation)
        onOpen?()
    }
}

/// Author, text and when it was pinned.
private struct PinnedMessageRow: View {
    let pin: PinEntry
    let room: BareJID
    let author: String

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(alignment: .firstTextBaseline) {
                Text(author)
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                Spacer(minLength: Theme.Spacing.s)
                if let pinnedAt = pin.pinnedAt {
                    Text(pinnedAt.formatted(.relative(presentation: .named)))
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            Text(pin.preview.text.isEmpty ? "Attachment" : pin.preview.text)
                .huddleText(Theme.Typography.body)
                .lineLimit(3)
        }
        .padding(.vertical, Theme.Spacing.xs)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
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
