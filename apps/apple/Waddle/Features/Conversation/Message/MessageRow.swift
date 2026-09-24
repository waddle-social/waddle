import SwiftUI
import WaddleKit

/// A feed row: optional day separator and unread divider, then the
/// message itself.
struct MessageRow: View {
    let entry: TimelineFeedEntry
    var showsThreadChip = true

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let day = entry.daySeparator {
                TimelineDaySeparator(day: day)
            }
            if entry.showsUnreadDivider {
                TimelineUnreadDivider()
            }
            MessageRowContent(entry: entry, showsThreadChip: showsThreadChip)
        }
    }
}

/// The message: avatar gutter, header, content, and its actions.
private struct MessageRowContent: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(AppState.self) private var app
    @Environment(NavigationModel.self) private var navigation
    @Environment(MessageActionModel.self) private var actions
    @State private var isHovering = false
    @State private var imagePreviewFile: SharedFile?
    @State private var isImagePreviewPresented = false
    private var placement = ConversationInspectorPlacement()

    let entry: TimelineFeedEntry
    let showsThreadChip: Bool

    init(entry: TimelineFeedEntry, showsThreadChip: Bool) {
        self.entry = entry
        self.showsThreadChip = showsThreadChip
    }

    private var item: TimelineItem { entry.item }
    private var isCompact: Bool { app.preferences.compactMessages }
    private var avatarSize: CGFloat { isCompact ? Theme.Size.rowAvatar : Theme.Size.avatar }

    var body: some View {
        let author = MessageAuthor.resolve(item, occupant: occupant)
        HStack(alignment: .top, spacing: isCompact ? Theme.Spacing.s : Theme.Spacing.m) {
            gutter(author: author)
                .frame(width: avatarSize, alignment: .trailing)
            VStack(alignment: .leading, spacing: isCompact ? Theme.Spacing.xxs : Theme.Spacing.xs) {
                if entry.startsGroup {
                    MessageRowHeader(author: author, item: item)
                }
                MessageRowBody(
                    entry: entry,
                    authorName: author.name,
                    showsThreadChip: showsThreadChip,
                    openThread: openThread,
                    openImage: openImage
                )
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.top, topPadding)
        .padding(.bottom, Theme.Spacing.xxs)
        .background(background)
        .contentShape(Rectangle())
        .onHover { isHovering = $0 }
        .contextMenu {
            MessageContextMenu(item: item, session: session, actions: actions, openThread: openThread)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(MessageAccessibilityText.label(
            for: item,
            authorName: author.name,
            replyCount: entry.replyCount,
            time: item.sentAt.formatted(date: .omitted, time: .shortened)
        )))
        .modifier(MessageAccessibilityActions(
            item: item,
            replyCount: entry.replyCount,
            openThread: openThread,
            openImage: openImage
        ))
        #if os(iOS)
        .fullScreenCover(isPresented: $isImagePreviewPresented, onDismiss: { imagePreviewFile = nil }) {
            if let imagePreviewFile {
                MessageImagePreviewView(file: imagePreviewFile)
            }
        }
        #elseif os(macOS)
        .sheet(isPresented: $isImagePreviewPresented, onDismiss: { imagePreviewFile = nil }) {
            if let imagePreviewFile {
                MessageImagePreviewView(file: imagePreviewFile)
            }
        }
        #endif
    }

    private var occupant: Occupant? {
        guard item.conversation.isRoom, let nick = item.from?.resource else { return nil }
        return session.presence.occupant(named: nick, in: item.conversation.jid)
    }

    private var topPadding: CGFloat {
        guard entry.startsGroup else { return Theme.Spacing.xxs }
        return isCompact ? Theme.Spacing.xs : Theme.Spacing.m
    }

    @ViewBuilder
    private func gutter(author: MessageAuthor) -> some View {
        if entry.startsGroup {
            MessageAvatar(author: author, size: avatarSize)
        } else if isHovering {
            Text(item.sentAt.formatted(date: .omitted, time: .shortened))
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .padding(.top, 2)
        } else {
            Color.clear.frame(height: 1)
        }
    }

    private var background: Color {
        if actions.highlightedID == item.id {
            return Color.accentColor.opacity(0.14)
        }
        return isHovering ? Color.secondary.opacity(0.07) : Color.clear
    }

    private func openThread() {
        guard let root = item.threadRootID else { return }
        navigation.openThread(root, in: item.conversation, usesInspector: placement.usesInspector)
    }

    private func openImage(_ file: SharedFile) {
        imagePreviewFile = file
        isImagePreviewPresented = true
    }
}

/// Everything under the header: reply quote, body or tombstone, files,
/// link cards, reactions, thread chip and delivery state.
private struct MessageRowBody: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    let entry: TimelineFeedEntry
    let authorName: String
    let showsThreadChip: Bool
    let openThread: () -> Void
    let openImage: (SharedFile) -> Void

    private var item: TimelineItem { entry.item }

    var body: some View {
        if let target = item.message.reply {
            MessageReplyPreview(target: target, parent: entry.replyParent, isRoom: item.conversation.isRoom) {
                if let parent = entry.replyParent {
                    actions.showMessage(parent.id)
                }
            }
        }
        if let tombstone = item.tombstone {
            MessageTombstoneView(tombstone: tombstone)
        } else {
            content
        }
        if item.isMine {
            MessageDeliveryView(item: item)
        }
    }

    @ViewBuilder
    private var content: some View {
        if MessageContent.visibleBody(of: item) != nil {
            if let imageURL = MessageContent.inlineImageURL(of: item) {
                MessageImageAttachment(file: MessageContent.inlineImageFile(imageURL), open: openImage)
            } else {
                MessageRichBody(
                    item: item,
                    account: session.account,
                    showsEditedMark: item.isEdited && !entry.startsGroup,
                    authorName: authorName
                )
            }
        }
        if !item.message.sharedFiles.isEmpty {
            MessageAttachmentsView(
                files: item.message.sharedFiles,
                isSticker: item.message.isSticker,
                openImage: openImage
            )
        }
        ForEach(item.message.linkPreviews, id: \.url) { preview in
            MessageLinkPreviewCard(preview: preview)
        }
        if !item.reactions.isEmpty {
            MessageReactionsBar(item: item)
        }
        if showsThreadChip, entry.replyCount > 0 {
            MessageThreadChip(count: entry.replyCount, action: openThread)
        }
    }
}
