import SwiftUI
import WaddleKit

/// A feed row: optional day separator and unread divider, then the
/// message itself.
struct MessageRow: View {
    let entry: TimelineFeedEntry
    var showsThreadChip = true
    let openImage: (MediaGallery.Image.ID) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let day = entry.daySeparator {
                TimelineDaySeparator(day: day)
            }
            if let edge = entry.unreadDivider {
                TimelineUnreadDivider(edge: edge)
            }
            if let anchor = entry.item.callAnchor, entry.item.tombstone == nil {
                // The room's announcement of a call is a card, not a message from anyone.
                CallAnchorRow(item: entry.item, anchor: anchor)
            } else {
                MessageRowContent(entry: entry, showsThreadChip: showsThreadChip) { source in
                    openImage(.init(row: entry.id, source: source))
                }
            }
        }
    }
}

/// The message: avatar gutter, header, content, and its actions.
private struct MessageRowContent: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(AppState.self) private var app
    @Environment(NavigationModel.self) private var navigation
    @Environment(MessageActionModel.self) private var actions
    @Environment(\.openURL) private var openURL
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// The pointer is over the row (iPad with a pointer too; touch never
    /// hovers). The row's only hover state.
    @State private var isHovering = false
    @State private var bodyLayout = MessageBodyPresentationMemo()
    @State private var attachmentToOpen: MessageMedia.Attachment.ID?
    private var placement = ConversationInspectorPlacement()

    let entry: TimelineFeedEntry
    let showsThreadChip: Bool
    let openImage: (MediaGallery.Source) -> Void

    private var item: TimelineItem { entry.item }
    private var isCompact: Bool { app.preferences.compactMessages }
    private var avatarSize: CGFloat { isCompact ? Theme.Size.rowAvatar : Theme.Size.avatar }

    var body: some View {
        let authorJID = session.authorJID(of: item)
        let media = MessageMedia(item: item, decision: session.remoteMediaDecision(for: item))
        let loadMedia = { session.loadRemoteMedia(for: item) }
        let author = MessageAuthor.resolve(
            item,
            occupant: occupant,
            authorJID: authorJID,
            isBot: authorJID.map { session.roomBots.isDeclared($0, in: item.conversation.jid) } ?? false
        )
        let available = MessageActions.available(for: item, in: actionContext)
        let presentation: MessageBodyPresentation? = if media.inlineBody == .text {
            bodyLayout.presentation(
                item: item, account: session.account, channels: session.directory.channels, actor: author.name
            )
        } else {
            nil
        }
        let openRoom: (BareJID) -> Void = { MessageRoomLinks.open($0, using: navigation, channels: session.directory.channels) }
        HStack(alignment: .top, spacing: isCompact ? Theme.Spacing.s : Theme.Spacing.m) {
            gutter(author: author)
                .frame(width: avatarSize, alignment: .trailing)
            VStack(alignment: .leading, spacing: contentSpacing) {
                if entry.startsGroup {
                    MessageRowHeader(
                        author: author,
                        item: item,
                        topSlop: topPadding,
                        bottomSlop: contentSpacing
                    ) {
                        openProfile(author)
                    }
                }
                MessageRowBody(
                    entry: entry,
                    presentation: presentation,
                    showsThreadChip: showsThreadChip,
                    media: media,
                    loadMedia: loadMedia,
                    openThread: openThread,
                    openImage: openImage, openRoom: openRoom, attachmentToOpen: $attachmentToOpen
                )
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let row = MessageSafetyScoresButton.markerRow(for: item.safetyScores) {
                MessageSafetyScoresButton(row: row) {
                    actions.showSafetyScores(for: item)
                }
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.top, topPadding)
        .padding(.bottom, Theme.Spacing.xxs)
        .background(rowSurface)
        .padding(.horizontal, Theme.Spacing.s)
        .overlay(alignment: .topTrailing) {
            MessageRowToolbar(item: item, context: actionContext, isRowHovered: isHovering, perform: perform)
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.3), value: actions.highlightedID == item.presentationID)
        .contentShape(Rectangle())
        #if os(iOS)
        .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        // A tap on a post with replies opens its thread. Attached with
        // `gesture`, not `simultaneousGesture`, so a child control (link,
        // image, reaction, reply quote, link card, avatar, name, reply
        // card) takes its own tap alone; scrolls and the context menu's
        // long press never end a tap.
        .gesture(TapGesture().onEnded { openThread() }, including: opensThreadOnTap ? .all : .subviews)
        #endif
        .onHover { inside in
            isHovering = inside
            if !inside {
                actions.endTextSelection(of: item.presentationID)
            }
        }
        .onDisappear {
            actions.releaseToolbar(of: item.presentationID)
            actions.endTextSelection(of: item.presentationID)
        }
        .contextMenu {
            MessageContextMenu(
                item: item, media: media, available: available, session: session, actions: actions, perform: perform,
                openURL: openURL, openEncryptedAttachment: openEncryptedAttachment
            )
        }
        .modifier(MessageRowAccessibility(
            label: MessageAccessibilityText.label(
                for: item, media: media, authorName: author.name, thread: entry.thread, insideThread: actions.isThread,
                time: item.sentAt.formatted(date: .omitted, time: .shortened)
            ),
            actions: MessageAccessibilityActions(
                item: item, media: media, available: available, perform: perform, opensThreadOnActivate: opensThreadOnTap,
                authorName: author.name, openThread: openThread,
                hasSubThreadLink: entry.replyCount > 0 && item.threadTarget(inside: actions.thread) != nil,
                openProfile: { openProfile(author) }, openImage: openImage, openEncryptedAttachment: openEncryptedAttachment,
                roomActions: presentation?.roomLinks.actions ?? [], openRoom: openRoom
            ),
            media: media, loadMedia: loadMedia,
            hasInlineControls: media.attachments.contains { $0.presentation.hasInlineControls }
        ))
    }

    private func openEncryptedAttachment(_ id: MessageMedia.Attachment.ID) {
        attachmentToOpen = id
    }

    private var actionContext: MessageActionContext {
        MessageActionContext(
            thread: actions.thread,
            replyCount: entry.replyCount,
            isPinned: session.isPinned(item),
            pinning: session.pinAccess(for: item)
        )
    }

    private func perform(_ action: MessageAction) {
        actions.perform(action, on: item, session: session, openThread: openThread)
    }

    private var occupant: Occupant? {
        guard item.conversation.isRoom, let nick = item.from?.resource else { return nil }
        return session.presence.occupant(named: nick, in: item.conversation.jid)
    }

    /// Touch only, and never inside the thread view, where a reply's
    /// sub-thread opens from its own link.
    private var opensThreadOnTap: Bool {
        #if os(iOS)
        return showsThreadChip && entry.replyCount > 0
        #else
        return false
        #endif
    }

    /// Between the header and the body's parts.
    private var contentSpacing: CGFloat {
        isCompact ? Theme.Spacing.xxs : Theme.Spacing.xs
    }

    private var topPadding: CGFloat {
        guard entry.startsGroup else { return Theme.Spacing.xxs }
        return isCompact ? Theme.Spacing.xs : Theme.Spacing.m
    }

    @ViewBuilder
    private func gutter(author: MessageAuthor) -> some View {
        if entry.startsGroup {
            // Up only into the row's own top padding; below the avatar the
            // gutter is empty.
            let slop = max(0, (Theme.Size.minHitTarget - avatarSize) / 2)
            MessageAvatar(author: author, size: avatarSize)
                .authorProfileButton(
                    name: author.name,
                    horizontalSlop: slop,
                    topSlop: min(slop, topPadding),
                    bottomSlop: slop
                ) {
                    openProfile(author)
                }
        } else if isHovering {
            Text(item.sentAt.formatted(date: .omitted, time: .shortened))
                .huddleText(Theme.Typography.timestamp.sized(WaddleTokens.FontSize.xxs))
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.fgMuted)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .padding(.top, Theme.Spacing.xxs)
        } else {
            Color.clear.frame(height: 1)
        }
    }

    /// The row's ground (web `wd-msg`): a mention of us in gold with a
    /// rail on the leading edge, then the scroll target, then hover.
    private var rowSurface: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        let mentionsMe = session.mentionsMe(item)
        return shape
            .fill(mentionsMe ? Theme.Colors.goldSubtle : Color.clear)
            .overlay(shape.fill(stateTint))
            .overlay(alignment: .leading) {
                if mentionsMe {
                    Theme.Colors.gold.frame(width: Theme.Size.rail)
                }
            }
            .clipShape(shape)
    }

    private var stateTint: Color {
        if actions.highlightedID == item.presentationID { return Theme.State.selected }
        return isHovering ? Theme.State.hover : .clear
    }

    /// The row's thread from the feed; inside a thread, a reply's
    /// sub-thread.
    private func openThread() {
        guard let root = item.threadTarget(inside: actions.thread) else { return }
        navigation.openThread(root, in: item.conversation, usesInspector: placement.usesInspector)
    }

    private func openProfile(_ author: MessageAuthor) {
        let nick = item.conversation.isRoom ? item.from?.resource : nil
        navigation.showProfile(PeerProfileRoute(
            name: author.name,
            jid: author.avatarJID,
            room: nick == nil ? nil : item.conversation.jid,
            nick: nick
        ))
    }
}

/// Everything under the header: reply quote, body or tombstone, files,
/// link cards, reactions, the attached reply card and delivery state.
private struct MessageRowBody: View {
    @Environment(MessageActionModel.self) private var actions
    @Environment(SessionCoordinator.self) private var session
    let entry: TimelineFeedEntry
    let presentation: MessageBodyPresentation?
    let showsThreadChip: Bool
    let media: MessageMedia
    let loadMedia: () -> Void
    let openThread: () -> Void
    let openImage: (MediaGallery.Source) -> Void
    let openRoom: (BareJID) -> Void
    @Binding var attachmentToOpen: MessageMedia.Attachment.ID?

    private var item: TimelineItem { entry.item }

    var body: some View {
        // Inside a thread the quote only repeats the thread's own messages,
        // most often its first one, on every reply.
        if !actions.isThread, let target = item.message.reply {
            MessageReplyPreview(target: target, parent: entry.replyParent, isRoom: item.conversation.isRoom) {
                if let parent = entry.replyParent {
                    actions.showMessage(parent.presentationID)
                }
            }
        }
        if let tombstone = item.tombstone {
            MessageTombstoneView(tombstone: tombstone)
        } else {
            content
        }
        // A deleted reply cannot start a sub-thread, but one it has stays
        // reachable.
        if actions.isThread, let thread = entry.thread, thread.replyCount > 0 {
            MessageSubThreadLink(count: thread.replyCount, action: openThread)
        }
        if item.isMine {
            MessageDeliveryView(item: item)
        }
    }

    /// Always on iOS. On a Mac selectable words would take the right-click
    /// for the text's own menu, so only the row chosen with "Select text".
    private var isSelectable: Bool {
        #if os(iOS)
        true
        #else
        actions.selectableRow == item.presentationID
        #endif
    }

    @ViewBuilder
    private var content: some View {
        switch media.inlineBody {
        case .withheld: EmptyView()
        case let .image(url):
            MessageImageAttachment(file: MessageContent.inlineImageFile(url)) { openImage(.inline(url)) }
        case .text:
            if let presentation {
                MessageRichBody(
                    presentation: presentation, showsEditedMark: item.isEdited && !entry.startsGroup,
                    isSelectable: isSelectable, openRoom: openRoom
                )
            }
        }
        if let withheld = media.withheld { WithheldMediaCard(media: withheld, load: loadMedia) }
        if !media.visibleAttachments.isEmpty {
            MessageAttachmentsView(
                attachments: media.attachments,
                isSticker: item.message.isSticker,
                openImage: openImage,
                attachmentToOpen: $attachmentToOpen,
                openStickerPack: media.stickerPackLocation.map { pack in
                    { actions.showStickerPack(pack) }
                }
            )
        }
        ForEach(Array(media.linkPreviews.enumerated()), id: \.offset) { _, preview in
            MessageLinkPreviewCard(preview: preview) { session.calls.state.isLive }
                .id(preview)
        }
        if !item.reactions.isEmpty {
            MessageReactionsBar(item: item)
        }
        if showsThreadChip, !actions.isThread, let thread = entry.thread, thread.replyCount > 0 {
            MessageThreadCard(summary: thread, action: openThread)
        }
    }
}
