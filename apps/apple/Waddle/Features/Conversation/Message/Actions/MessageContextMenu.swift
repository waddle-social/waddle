import SwiftUI
import WaddleKit

/// Row context menu: quick reactions, reply, thread, copy, pin, edit,
/// delete and moderator removal. Actions that target the message on the
/// wire are disabled until it has an id others know (a local echo not yet
/// reflected has none). Dependencies are passed in rather than read from
/// the environment, which menu content does not reliably inherit.
struct MessageContextMenu: View {
    let item: TimelineItem
    let session: SessionCoordinator
    let actions: MessageActionModel
    let openThread: () -> Void

    private var canTarget: Bool { item.actionTargetID != nil }

    var body: some View {
        if item.tombstone == nil {
            if canTarget {
                ControlGroup {
                    ForEach(Theme.quickReactions, id: \.self) { emoji in
                        Button {
                            actions.react(emoji, to: item, session: session)
                        } label: {
                            Text(emoji)
                        }
                    }
                }
                .controlGroupStyle(.palette)
                Button {
                    actions.pickReaction(for: item)
                } label: {
                    Label("Add reaction…", systemImage: "face.smiling")
                }
            }
            Button {
                actions.reply(to: item)
            } label: {
                Label("Reply", systemImage: "arrowshape.turn.up.left")
            }
            .disabled(!canTarget)
            if !actions.isThread, item.threadRootID != nil {
                Button(action: openThread) {
                    Label("Reply in thread", systemImage: "bubble.left.and.bubble.right")
                }
            }
            if let text = MessageContent.visibleBody(of: item) {
                Button {
                    MessagePasteboard.copy(text)
                } label: {
                    Label("Copy text", systemImage: "doc.on.doc")
                }
            }
            if item.conversation.isRoom {
                pinButton
            }
            ownActions
            moderatorActions
        }
        Text(item.sentAt.formatted(date: .abbreviated, time: .shortened))
    }

    private var pinButton: some View {
        let pinned = session.isPinned(item)
        return Button {
            actions.setPinned(!pinned, item, session: session)
        } label: {
            Label(pinned ? "Unpin" : "Pin", systemImage: pinned ? "pin.slash" : "pin")
        }
        .disabled(!canTarget)
    }

    @ViewBuilder
    private var ownActions: some View {
        if item.isMine {
            Divider()
            Button {
                actions.edit(item)
            } label: {
                Label("Edit", systemImage: "pencil")
            }
            .disabled(item.correctionTargetID == nil || item.isLocalEcho)
            Button(role: .destructive) {
                actions.requestDeletion(of: item)
            } label: {
                Label("Delete", systemImage: "trash")
            }
            .disabled(item.retractionTargetID == nil)
        }
    }

    @ViewBuilder
    private var moderatorActions: some View {
        if !item.isMine, session.canModerate(in: item.conversation) {
            Divider()
            Button(role: .destructive) {
                actions.requestRemoval(of: item)
            } label: {
                Label("Remove message", systemImage: "shield.lefthalf.filled")
            }
            .disabled(!canTarget)
        }
    }
}

/// VoiceOver custom actions mirroring the context menu, since the row is
/// one combined element.
struct MessageAccessibilityActions: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @Environment(\.openURL) private var openURL
    let item: TimelineItem
    let replyCount: Int
    let openThread: () -> Void
    let openImage: (SharedFile) -> Void

    func body(content: Content) -> some View {
        content.accessibilityActions {
            if item.tombstone == nil, item.actionTargetID != nil {
                Button("Reply") { actions.reply(to: item) }
                Button("Add reaction") { actions.pickReaction(for: item) }
            }
            if replyCount > 0 {
                Button("Open thread", action: openThread)
            }
            if let file = item.message.sharedFiles.first, file.encrypted == nil {
                Button("Open attachment") {
                    if MessageAttachmentKind(file) == .image {
                        openImage(file)
                    } else {
                        openURL(file.url)
                    }
                }
            }
            if let preview = item.message.linkPreviews.first {
                Button("Open link") { openURL(preview.url) }
            }
            if item.isMine, item.tombstone == nil {
                if item.correctionTargetID != nil, !item.isLocalEcho {
                    Button("Edit") { actions.edit(item) }
                }
                if item.retractionTargetID != nil {
                    Button("Delete") { actions.requestDeletion(of: item) }
                }
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
