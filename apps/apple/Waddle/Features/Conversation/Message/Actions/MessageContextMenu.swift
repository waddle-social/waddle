import SwiftUI
import WaddleKit

/// Row context menu: the row's actions from `MessageActions` (quick
/// reactions, add reaction, reply, the thread, pin, select text on a Mac,
/// edit, delete), with copy text, the sticker pack, safety scores and
/// moderator removal between them. Dependencies are passed in rather than
/// read from the environment, which menu content does not reliably
/// inherit.
struct MessageContextMenu: View {
    let item: TimelineItem
    let media: MessageMedia
    let available: [MessageAction]
    let session: SessionCoordinator
    let actions: MessageActionModel
    let perform: (MessageAction) -> Void

    var body: some View {
        if item.tombstone == nil {
            let reactions = available.filter { if case .react = $0 { true } else { false } }
            if !reactions.isEmpty {
                ControlGroup {
                    ForEach(reactions, id: \.self) { action in
                        Button {
                            perform(action)
                        } label: {
                            if case let .react(emoji) = action { Text(emoji) }
                        }
                        .accessibilityLabel(Text(action.accessibilityLabel))
                    }
                }
                .controlGroupStyle(.palette)
            }
            items([.addReaction, .reply, .startThread, .openThread])
            if let text = MessageContent.visibleBody(of: item) {
                Button {
                    MessagePasteboard.copy(text)
                } label: {
                    Label("Copy text", systemImage: "doc.on.doc")
                }
            }
            if let pack = media.stickerPackLocation {
                Button {
                    actions.showStickerPack(pack)
                } label: {
                    Label("View sticker pack", systemImage: "face.smiling")
                }
            }
            items([.pin, .unpin])
            if let row = MessageSafetyScoresButton.markerRow(for: item.safetyScores) {
                Button {
                    actions.showSafetyScores(for: item)
                } label: {
                    Label(MessageSafetyScoresButton.title + "…", systemImage: MessageSafetyScoreStyle.symbol(for: row.category))
                }
            }
            #if os(macOS)
            items([.selectText])
            #endif
            if available.contains(where: \.isOwn) {
                Divider()
                items([.edit, .delete])
            }
            moderatorActions
        }
        Text(item.sentAt.formatted(date: .abbreviated, time: .shortened))
    }

    /// The available ones of `wanted`, in the model's order.
    @ViewBuilder
    private func items(_ wanted: [MessageAction]) -> some View {
        ForEach(available.filter(wanted.contains), id: \.self) { action in
            Button(role: action == .delete ? .destructive : nil) {
                perform(action)
            } label: {
                Label(action.title, systemImage: action.symbol)
            }
        }
    }

    @ViewBuilder
    private var moderatorActions: some View {
        if !item.isMine, item.actionTargetID != nil, session.canModerate(in: item.conversation) {
            Divider()
            Button(role: .destructive) {
                actions.requestRemoval(of: item)
            } label: {
                Label("Remove message", systemImage: "shield.lefthalf.filled")
            }
        }
    }
}

extension MessageAction {
    /// The SF Symbol the toolbar and the menu show.
    var symbol: String {
        switch self {
        case .react: "face.smiling"
        case .addReaction: "face.smiling"
        case .reply: "arrowshape.turn.up.left"
        case .startThread, .openThread: "bubble.left.and.bubble.right"
        case .pin: "pin"
        case .unpin: "pin.slash"
        case .selectText: "character.cursor.ibeam"
        case .edit: "pencil"
        case .delete: "trash"
        }
    }
}

/// VoiceOver custom actions mirroring the context menu and the row's
/// buttons, since the row is one combined element. With
/// `opensThreadOnActivate` (a post with replies on touch), activating the
/// row opens its thread, as a tap does.
struct MessageAccessibilityActions: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @Environment(\.openURL) private var openURL
    let item: TimelineItem
    let media: MessageMedia
    /// The row's actions; VoiceOver gets every one but the quick
    /// reactions and select text.
    let available: [MessageAction]
    let perform: (MessageAction) -> Void
    let opensThreadOnActivate: Bool
    let authorName: String
    let openThread: () -> Void
    /// A row with a thread of its own to open: a post with replies in the
    /// feed, or a reply in a thread view whose sub-thread has messages.
    let hasSubThreadLink: Bool
    let openProfile: () -> Void
    let openImage: (MediaGallery.Source) -> Void
    let roomActions: [MessageRoomLinks.Action]
    let openRoom: (BareJID) -> Void

    @ViewBuilder
    func body(content: Content) -> some View {
        if opensThreadOnActivate {
            customActions(content)
                .accessibilityAction(.default) { openThread() }
        } else {
            customActions(content)
        }
    }

    private var spoken: [MessageAction] {
        available.filter { action in
            switch action {
            case .react, .selectText: false
            default: true
            }
        }
    }

    private func customActions(_ content: Content) -> some View {
        content.accessibilityActions {
            ForEach(spoken, id: \.self) { action in
                Button(action.accessibilityLabel) { perform(action) }
            }
            // A deleted message has no actions but keeps its thread, on
            // every platform and in the feed as well as inside a thread.
            if item.tombstone != nil, hasSubThreadLink {
                Button("Open thread", action: openThread)
            }
            Button("\(authorName), open profile", action: openProfile)
            if let row = MessageSafetyScoresButton.markerRow(for: item.safetyScores) {
                Button("Show \(row.title.lowercased()) content signal") { actions.showSafetyScores(for: item) }
            }
            if let pack = media.stickerPackLocation {
                Button("View sticker pack") { actions.showStickerPack(pack) }
            }
            ForEach(MediaGallery.images(in: item, media: media)) { image in
                Button("Open image \(image.accessibilityName)") { openImage(image.id.source) }
            }
            if let file = media.visibleAttachments.first, file.encrypted == nil, !file.isImage {
                Button("Open attachment") { openURL(file.url) }
            }
            if let preview = media.linkPreviews.first {
                Button("Open link") { openURL(preview.url) }
            }
            ForEach(roomActions) { action in
                Button("Open room \(action.name)") { openRoom(action.room) }
            }
            if session.deliveries.state(of: item.id) == .failed {
                Button("Retry sending") { Task { await session.retry(clientID: item.id) } }
                Button("Discard") { session.discard(clientID: item.id, in: item.conversation) }
            }
        }
    }
}

/// Copies message text to the system pasteboard.
enum MessagePasteboard {
    @MainActor
    static func copy(_ text: String) {
        #if os(iOS)
        UIPasteboard.general.string = text
        #elseif os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        #endif
    }
}
