import SwiftUI
import PhotosUI

struct ChatConversationHeaderView: View {
    let room: ChatRoomSelection?
    let bannerState: ChatConnectionBannerState
    let memberCount: Int
    var messageCount: Int = 0
    var showsMemberButton: Bool = false
    var onShowMembers: (() -> Void)? = nil
    var usesOperationalChrome: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: usesOperationalChrome ? 14 : 10) {
            HStack(alignment: .top, spacing: 14) {
                if usesOperationalChrome {
                    RoundedRectangle(cornerRadius: 16, style: .continuous)
                        .fill(Color.accentColor.opacity(0.14))
                        .frame(width: 44, height: 44)
                        .overlay {
                            Image(systemName: "number")
                                .font(.headline.weight(.semibold))
                                .foregroundStyle(Color.accentColor)
                        }
                }

                VStack(alignment: .leading, spacing: usesOperationalChrome ? 6 : 4) {
                    if usesOperationalChrome {
                        Text("Conversation")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .textCase(.uppercase)
                    }

                    HStack(spacing: 8) {
                        Text(room?.title ?? "Chat")
                            .font(usesOperationalChrome ? .title3.weight(.semibold) : .title2.weight(.semibold))

                        if let room, room.isMuted {
                            headerPill(title: "Muted", systemImage: "bell.slash.fill", tint: .secondary)
                        }

                        if !usesOperationalChrome, let room, room.unreadCount > 0 {
                            headerPill(title: "\(room.unreadCount)", systemImage: nil, tint: .secondary)
                        }
                    }

                    if let subtitle = room?.subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .foregroundStyle(.secondary)
                            .font(.subheadline)
                            .lineLimit(2)
                    } else {
                        Text("\(memberCount) member\(memberCount == 1 ? "" : "s")")
                            .foregroundStyle(.secondary)
                            .font(.subheadline)
                    }

                    HStack(spacing: 8) {
                        headerMetaLabel(systemImage: "person.2.fill", text: "\(memberCount)")

                        if messageCount > 0 {
                            headerMetaLabel(systemImage: "text.bubble.fill", text: "\(messageCount)")
                        }

                        if let room, room.unreadCount > 0, usesOperationalChrome {
                            headerMetaLabel(systemImage: "circle.fill", text: "\(room.unreadCount) new", tint: .accentColor, emphasized: true)
                        }

                        if let lastActivityAt = room?.lastActivityAt {
                            headerMetaLabel(systemImage: "clock", text: RelativeDateTimeFormatter().localizedString(for: lastActivityAt, relativeTo: Date()))
                        }
                    }
                }

                Spacer(minLength: 12)

                if showsMemberButton, let onShowMembers {
                    Button(action: onShowMembers) {
                        Label("Members", systemImage: "person.2.fill")
                            .font(.footnote.weight(.medium))
                    }
                    .buttonStyle(.bordered)
                }
            }

            if bannerState.isVisible {
                ChatConnectionBannerView(state: bannerState, usesOperationalChrome: usesOperationalChrome)
            }
        }
        .padding(usesOperationalChrome ? 18 : 16)
        .background(headerBackground)
    }

    @ViewBuilder
    private func headerMetaLabel(
        systemImage: String,
        text: String,
        tint: Color = .secondary,
        emphasized: Bool = false
    ) -> some View {
        Label {
            Text(text)
        } icon: {
            Image(systemName: systemImage)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(tint)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background(tint.opacity(emphasized ? 0.12 : 0.10), in: Capsule())
    }

    @ViewBuilder
    private func headerPill(title: String, systemImage: String?, tint: Color) -> some View {
        HStack(spacing: 5) {
            if let systemImage {
                Image(systemName: systemImage)
            }
            Text(title)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(tint.opacity(0.12), in: Capsule())
    }

    @ViewBuilder
    private var headerBackground: some View {
        if usesOperationalChrome {
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .fill(Color.primary.opacity(0.04))
        } else {
            Color.clear
        }
    }
}

struct ChatConnectionBannerView: View {
    let state: ChatConnectionBannerState
    var usesOperationalChrome: Bool = false

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: state.symbolName)
                .font(.caption.weight(.semibold))
                .foregroundStyle(tint)
            Text(state.message)
                .font(.footnote.weight(.medium))
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, usesOperationalChrome ? 14 : 12)
        .padding(.vertical, usesOperationalChrome ? 10 : 8)
        .background(tint.opacity(usesOperationalChrome ? 0.08 : 0.10), in: RoundedRectangle(cornerRadius: usesOperationalChrome ? 14 : 999, style: .continuous))
    }

    private var tint: Color {
        switch state {
        case .connected:
            return .green
        case .connecting, .reconnecting:
            return .blue
        case .disconnected:
            return .orange
        case .error:
            return .red
        case .hidden:
            return .secondary
        }
    }
}

struct ChatTimelineView: View {
    let messages: [ChatTimelineMessage]
    var historyState: ChatRoomHistoryState = .init()
    var onLoadOlderMessages: (() -> Void)? = nil
    var onReply: ((ChatTimelineMessage) -> Void)? = nil
    var onRetract: ((ChatTimelineMessage) -> Void)? = nil
    var onOpenThread: ((ChatTimelineMessage) -> Void)? = nil
    /// Parent-message-id → ordered list of thread-child message ids. The
    /// timeline uses this to render the "💬 N replies" chip on root messages.
    var childrenByThreadID: [String: [String]] = [:]
    /// Id of the first message to flag as unread. When non-nil the timeline
    /// inserts a red-accented "New" divider directly above this message.
    var firstUnreadMessageID: String? = nil
    /// XEP-0084 avatar bytes keyed by message `senderID`. Rows call
    /// `onRequestAvatar` on first appear; we render whatever data is
    /// currently in this map and fall back to initials otherwise.
    var avatarDataBySenderID: (String) -> Data? = { _ in nil }
    var onRequestAvatar: ((String) -> Void)? = nil
    var emptyState: AnyView? = nil
    var usesOperationalDensity: Bool = false
    var usesCompactConversationStyle: Bool = false
    @AppStorage(AppConfig.scrollDirectionKey) private var scrollDirectionRaw = ChatScrollDirection.chat.rawValue

    private var scrollDirection: ChatScrollDirection {
        ChatScrollDirection(rawValue: scrollDirectionRaw) ?? .chat
    }

    private var displayedMessages: [ChatTimelineMessage] {
        switch scrollDirection {
        case .chat:
            return messages
        case .social:
            return Array(messages.reversed())
        }
    }

    private var stackSpacing: CGFloat {
        if usesCompactConversationStyle {
            return 0
        }
        return usesOperationalDensity ? 8 : 12
    }

    private var horizontalPadding: CGFloat {
        if usesCompactConversationStyle {
            return 12
        }
        return usesOperationalDensity ? 20 : 16
    }

    private var verticalPadding: CGFloat {
        if usesCompactConversationStyle {
            return 12
        }
        return usesOperationalDensity ? 18 : 16
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: stackSpacing) {
                if messages.isEmpty {
                    if historyState.isLoadingInitialHistory {
                        HStack(spacing: 10) {
                            ProgressView()
                            Text("Loading room history…")
                                .foregroundStyle(.secondary)
                        }
                        .font(.footnote)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.vertical, 8)
                    } else if let emptyState {
                        emptyState
                    } else {
                        ChatEmptyStateView(
                            title: "No messages yet",
                            message: "Be the first to say hello."
                        )
                    }
                } else {
                    if scrollDirection == .chat {
                        historyControls
                    }

                    if let errorMessage = historyState.errorMessage {
                        Text(errorMessage)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .center)
                    }

                    ForEach(Array(displayedMessages.enumerated()), id: \.element.id) { index, message in
                        let previousMessage = index > 0 ? displayedMessages[index - 1] : nil
                        let nextMessage = index + 1 < displayedMessages.count ? displayedMessages[index + 1] : nil

                        if usesCompactConversationStyle, message.startsTimelineDay(after: previousMessage) {
                            ChatTimelineDayDividerView(date: message.sentAt)
                                .padding(.top, previousMessage == nil ? 0 : 10)
                        }

                        if message.id == firstUnreadMessageID {
                            ChatUnreadDividerView()
                                .id("unread-divider")
                                .transition(.opacity)
                        }

                        ChatMessageRowView(
                            message: message,
                            previousMessage: previousMessage,
                            nextMessage: nextMessage,
                            usesCompactConversationStyle: usesCompactConversationStyle,
                            threadChildCount: childrenByThreadID[message.id]?.count ?? 0,
                            onReply: onReply,
                            onRetract: onRetract,
                            onOpenThread: onOpenThread,
                            avatarData: avatarDataBySenderID(message.senderID),
                            onRequestAvatar: onRequestAvatar
                        )
                        .id(message.id)
                        .transition(.asymmetric(
                            insertion: .opacity.combined(with: .move(edge: .bottom)),
                            removal: .opacity
                        ))
                    }

                    if scrollDirection == .social {
                        historyControls
                    }
                }
            }
            .frame(maxWidth: usesOperationalDensity ? 860 : .infinity, alignment: .leading)
            .padding(.horizontal, horizontalPadding)
            .padding(.vertical, verticalPadding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .animation(.smooth(duration: 0.22), value: displayedMessages.map(\.id))
        }
        .background(WaddleTheme.chatBackground)
        #if os(iOS)
        // Dismiss the software keyboard when the user drags the message list
        // or taps anywhere outside the composer/keyboard. `.immediately` hides
        // the keyboard as soon as scrolling starts; the simultaneous tap
        // gesture covers the static-tap case without consuming taps meant for
        // message rows (reactions, reply buttons, links).
        .scrollDismissesKeyboard(.immediately)
        .simultaneousGesture(
            TapGesture().onEnded {
                UIApplication.shared.sendAction(
                    #selector(UIResponder.resignFirstResponder),
                    to: nil, from: nil, for: nil
                )
            }
        )
        #endif
    }

    @ViewBuilder
    private var historyControls: some View {
        if historyState.isLoadingOlderMessages {
            HStack(spacing: 10) {
                ProgressView()
                Text("Loading older messages…")
                    .foregroundStyle(.secondary)
            }
            .font(.footnote)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.vertical, 8)
        } else if historyState.canLoadOlderMessages, let onLoadOlderMessages {
            Button(action: onLoadOlderMessages) {
                Label("Load older messages", systemImage: "arrow.up.circle")
                    .font(.footnote.weight(.medium))
            }
            .buttonStyle(.bordered)
            .frame(maxWidth: .infinity, alignment: .center)
        }
    }
}

/// Thin "New" rail placed directly above the first unread message in a room.
/// Uses a warning-tinted hairline with a small pill label on the trailing
/// edge so it reads as "you haven't seen below this line" without competing
/// with day dividers.
struct ChatUnreadDividerView: View {
    var body: some View {
        HStack(spacing: 8) {
            VStack { Divider().overlay(WaddleTheme.unreadBadge) }
            Text("New")
                .font(.caption2.weight(.bold))
                .foregroundStyle(.white)
                .padding(.horizontal, 8)
                .padding(.vertical, 2)
                .background(WaddleTheme.unreadBadge, in: Capsule())
        }
        .padding(.top, 10)
        .padding(.bottom, 4)
        .padding(.horizontal, 16)
    }
}

struct ChatTimelineDayDividerView: View {
    let date: Date

    private var label: String {
        if Calendar.current.isDateInToday(date) { return "Today" }
        if Calendar.current.isDateInYesterday(date) { return "Yesterday" }
        return date.formatted(.dateTime.weekday(.wide).month(.abbreviated).day())
    }

    var body: some View {
        // Centred pill flanked by hairlines — the Slack/Discord pattern that
        // reads as "new day" without dominating the scroll view.
        HStack(spacing: 10) {
            VStack { Divider().overlay(WaddleTheme.divider) }
            Text(label)
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .padding(.horizontal, 10)
                .padding(.vertical, 3)
                .background(
                    WaddleTheme.surfaceRaised.opacity(0.8),
                    in: Capsule()
                )
                .overlay(
                    Capsule()
                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                )
            VStack { Divider().overlay(WaddleTheme.divider) }
        }
        .padding(.top, 18)
        .padding(.bottom, 6)
        .padding(.horizontal, 16)
    }
}

struct ChatMessageRowView: View {
    let message: ChatTimelineMessage
    var previousMessage: ChatTimelineMessage? = nil
    var nextMessage: ChatTimelineMessage? = nil
    var usesCompactConversationStyle: Bool = false
    /// Number of XEP-0201 thread children keyed off this message's id. When
    /// greater than zero the row renders a "💬 N replies" chip and exposes a
    /// "View thread" context-menu action.
    var threadChildCount: Int = 0
    var onReply: ((ChatTimelineMessage) -> Void)? = nil
    var onRetract: ((ChatTimelineMessage) -> Void)? = nil
    var onOpenThread: ((ChatTimelineMessage) -> Void)? = nil
    /// XEP-0084 PEP avatar bytes for `message.senderID`. When non-nil the
    /// avatar tile renders the image; otherwise it falls back to initials.
    var avatarData: Data? = nil
    /// Called on first appearance so the app model can kick off a PEP avatar
    /// fetch for this sender if one hasn't been retrieved yet.
    var onRequestAvatar: ((String) -> Void)? = nil
    @State private var lightboxImage: XMPPSharedFile?
    @State private var isHovering: Bool = false

    var body: some View {
        if message.isAction {
            Text(message.body)
                .font(.caption)
                .foregroundStyle(WaddleTheme.textMuted)
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .center)
        } else {
            slackStyleRow
        }
    }

    @ViewBuilder
    private var slackStyleRow: some View {
        let showsHeader = !message.formsCompactCluster(with: previousMessage)

        HStack(alignment: .top, spacing: 10) {
            if showsHeader {
                avatar
                    .frame(width: WaddleTheme.messageAvatarSize, height: WaddleTheme.messageAvatarSize)
            } else {
                Color.clear
                    .frame(width: WaddleTheme.messageAvatarSize, height: 1)
            }

            VStack(alignment: .leading, spacing: 3) {
                if showsHeader {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(message.senderDisplayName)
                            .font(WaddleTheme.senderFont)
                            .foregroundStyle(WaddleTheme.textPrimary)

                        Text(message.sentAt, style: .time)
                            .font(WaddleTheme.timestampFont)
                            .foregroundStyle(WaddleTheme.textMuted)

                        if message.editedAt != nil {
                            Text("(edited)")
                                .font(WaddleTheme.timestampFont)
                                .foregroundStyle(WaddleTheme.textMuted)
                        }

                        if let hats = message.hatTitles {
                            ForEach(hats, id: \.self) { hat in
                                Text(hat)
                                    .font(.caption2.weight(.semibold))
                                    .foregroundStyle(hatColor(for: hat))
                                    .padding(.horizontal, 5)
                                    .padding(.vertical, 1)
                                    .background(hatColor(for: hat).opacity(0.15), in: Capsule())
                            }
                        }

                        if let mention = message.broadcastMention {
                            Text("@\(mention)")
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(.white)
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(Color.orange, in: Capsule())
                        }
                    }
                }

                if let replyToID = message.replyToID, !replyToID.isEmpty {
                    replyIndicator
                }

                if message.isRetracted {
                    Text("This message was deleted.")
                        .font(WaddleTheme.bodyFont)
                        .italic()
                        .foregroundStyle(WaddleTheme.textMuted)
                } else {
                    if !message.displayBody.isEmpty, !message.bodyIsSingleImageURL {
                        Text(message.styledBody)
                            .font(WaddleTheme.bodyFont)
                            .foregroundStyle(WaddleTheme.textPrimary)
                            .textSelection(.enabled)
                    }

                    inlineImagesView(for: message, maxWidth: 300)
                    bodyImageURLsView(for: message, maxWidth: 300)
                    downloadableFilesView(for: message)
                }

                if let reactions = message.reactions, !reactions.isEmpty {
                    reactionBar(reactions)
                }

                if threadChildCount > 0, onOpenThread != nil {
                    threadRepliesChip
                }
            }

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.top, showsHeader ? 14 : 2)
        .padding(.bottom, 2)
        .background(isHovering ? WaddleTheme.messageHover : Color.clear)
        .overlay(alignment: .topTrailing) {
            if isHovering, !message.isRetracted, !message.isAction {
                messageActionToolbar
                    .padding(.trailing, 20)
                    .offset(y: -14)
                    .transition(.opacity.combined(with: .scale(scale: 0.95, anchor: .topTrailing)))
            }
        }
        .animation(.easeOut(duration: 0.12), value: isHovering)
        .onHover { isHovering = $0 }
        .task(id: message.senderID) {
            // Kick off a XEP-0084 PEP avatar fetch the first time this row
            // appears; the callback is idempotent in the app model.
            onRequestAvatar?(message.senderID)
        }
        .contextMenu {
            if !message.isAction, !message.isRetracted {
                if onReply != nil {
                    Button { onReply?(message) } label: {
                        Label("Reply", systemImage: "arrowshape.turn.up.left")
                    }
                }
                if onOpenThread != nil {
                    Button { onOpenThread?(message) } label: {
                        Label(threadChildCount > 0 ? "View thread (\(threadChildCount))" : "Start thread",
                              systemImage: "bubble.left.and.bubble.right")
                    }
                }
                if message.isOutgoing, onRetract != nil {
                    Button(role: .destructive) { onRetract?(message) } label: {
                        Label("Delete", systemImage: "trash")
                    }
                }
            }
        }
    }

    /// Floating action toolbar anchored to a message row's top-trailing edge.
    /// Appears on pointer hover (macOS / iPad with trackpad); touch-only
    /// devices continue to surface the same actions through the contextMenu.
    @ViewBuilder
    private var messageActionToolbar: some View {
        HStack(spacing: 2) {
            if onReply != nil {
                messageActionButton(symbol: "arrowshape.turn.up.left", accessibility: "Reply") {
                    onReply?(message)
                }
            }
            if onOpenThread != nil {
                messageActionButton(symbol: "bubble.left.and.bubble.right", accessibility: "Start or open thread") {
                    onOpenThread?(message)
                }
            }
            if message.isOutgoing, onRetract != nil {
                messageActionButton(symbol: "trash", accessibility: "Delete message", destructive: true) {
                    onRetract?(message)
                }
            }
        }
        .padding(3)
        .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
        )
        .shadow(color: .black.opacity(0.25), radius: 6, x: 0, y: 2)
    }

    private func messageActionButton(
        symbol: String,
        accessibility: String,
        destructive: Bool = false,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(destructive ? Color.red : WaddleTheme.textPrimary)
                .frame(width: 26, height: 26)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(accessibility)
    }

    /// Compact "N replies" pill rendered under a root message when the timeline
    /// has at least one XEP-0201 thread child keyed off this message's id.
    @ViewBuilder
    private var threadRepliesChip: some View {
        Button {
            onOpenThread?(message)
        } label: {
            HStack(spacing: 5) {
                Image(systemName: "bubble.left.and.bubble.right.fill")
                    .font(.caption2.weight(.semibold))
                Text(threadChildCount == 1 ? "1 reply" : "\(threadChildCount) replies")
                    .font(.caption.weight(.medium))
            }
            .foregroundStyle(WaddleTheme.accent)
            .padding(.horizontal, 9)
            .padding(.vertical, 4)
            .background(WaddleTheme.accent.opacity(0.10), in: Capsule())
        }
        .buttonStyle(.plain)
        .padding(.top, 2)
    }

    private func reactionBar(_ reactions: [String: [String]]) -> some View {
        HStack(spacing: 4) {
            ForEach(reactions.keys.sorted(), id: \.self) { emoji in
                let count = reactions[emoji]?.count ?? 0
                HStack(spacing: 3) {
                    Text(emoji).font(.caption)
                    Text("\(count)").font(.caption2.weight(.medium)).foregroundStyle(WaddleTheme.textSecondary)
                }
                .padding(.horizontal, 7)
                .padding(.vertical, 3)
                .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(WaddleTheme.divider))
            }
        }
    }

    @ViewBuilder
    private var avatar: some View {
        // Slack-style rounded-square avatar with a subtle inner stroke so the
        // tile reads as a "tile" rather than a flat colour block. Same size
        // footprint as the previous circle, so layout is unaffected.
        if let data = avatarData, !data.isEmpty, let image = avatarSwiftUIImage(data) {
            image
                .resizable()
                .aspectRatio(contentMode: .fill)
                .frame(width: WaddleTheme.messageAvatarSize, height: WaddleTheme.messageAvatarSize)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 0.5)
                )
        } else {
            Text(message.senderInitials ?? initials(from: message.senderDisplayName))
                .font(.caption.weight(.bold))
                .foregroundStyle(.white)
                .frame(width: WaddleTheme.messageAvatarSize, height: WaddleTheme.messageAvatarSize)
                .background(
                    WaddleTheme.avatarColor(for: message.senderDisplayName),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 0.5)
                )
        }
    }

    /// Decode XEP-0084 avatar bytes into a cross-platform SwiftUI `Image`.
    /// Returns nil when the bytes aren't decodable as an image (e.g. the
    /// sender published malformed PEP data).
    private func avatarSwiftUIImage(_ data: Data) -> Image? {
        #if os(iOS)
        guard let ui = UIImage(data: data) else { return nil }
        return Image(uiImage: ui)
        #elseif os(macOS)
        guard let ns = NSImage(data: data) else { return nil }
        return Image(nsImage: ns)
        #else
        return nil
        #endif
    }

    @ViewBuilder
    private var replyIndicator: some View {
        // Slack/Discord-style reply preview: corner-returned arrow icon,
        // sender name in accent colour, quoted body truncated to one line.
        HStack(alignment: .center, spacing: 6) {
            Image(systemName: "arrowshape.turn.up.left.fill")
                .font(.caption2.weight(.bold))
                .foregroundStyle(WaddleTheme.accent.opacity(0.85))

            if let senderName = message.replyToSenderName, !senderName.isEmpty {
                Text(senderName)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(WaddleTheme.accent)
            }

            Text(message.replyToBody ?? "View original message")
                .font(.caption)
                .foregroundStyle(WaddleTheme.textSecondary)
                .lineLimit(1)
                .truncationMode(.tail)
        }
        .padding(.leading, 10)
        .padding(.trailing, 12)
        .padding(.vertical, 5)
        .background(
            WaddleTheme.accent.opacity(0.08),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(WaddleTheme.accent.opacity(0.18), lineWidth: 0.5)
        )
        .padding(.bottom, 1)
    }

    @ViewBuilder
    private func inlineImagesView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let images = message.inlineImages
        if !images.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(images, id: \.url) { file in
                    Button {
                        lightboxImage = file
                    } label: {
                        AsyncImage(url: URL(string: file.url)) { phase in
                            switch phase {
                            case .success(let image):
                                image
                                    .resizable()
                                    .aspectRatio(contentMode: .fit)
                                    .frame(maxWidth: maxWidth, maxHeight: 240)
                                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                            case .failure:
                                Label(file.name ?? "Image", systemImage: "photo")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .padding(8)
                                    .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
                            case .empty:
                                RoundedRectangle(cornerRadius: 12, style: .continuous)
                                    .fill(Color.secondary.opacity(0.08))
                                    .frame(width: min(CGFloat(file.width ?? 200), maxWidth), height: min(CGFloat(file.height ?? 150), 240))
                                    .overlay { ProgressView() }
                            @unknown default:
                                EmptyView()
                            }
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
#if os(iOS)
            .fullScreenCover(item: $lightboxImage) { file in
                ChatImageLightboxView(file: file)
            }
#else
            .sheet(item: $lightboxImage) { file in
                ChatImageLightboxView(file: file)
                    .frame(minWidth: 600, minHeight: 500)
            }
#endif
        }
    }

    @ViewBuilder
    private func downloadableFilesView(for message: ChatTimelineMessage) -> some View {
        let files = message.downloadableFiles
        if !files.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(files, id: \.url) { file in
                    if let url = URL(string: file.url) {
                        Link(destination: url) {
                            HStack(spacing: 8) {
                                Image(systemName: "arrow.down.circle")
                                    .font(.subheadline)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(file.name ?? "File")
                                        .font(.caption.weight(.medium))
                                        .lineLimit(1)
                                    HStack(spacing: 4) {
                                        Text(file.mediaType ?? "file")
                                            .font(.caption2)
                                        if let size = file.size {
                                            Text("·")
                                            Text(formatFileSize(size))
                                                .font(.caption2)
                                        }
                                    }
                                    .foregroundStyle(.secondary)
                                }
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 8)
                            .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                        }
                    }
                }
            }
        }
    }

    /// Render each URL in the message body that resolves to an image/GIF as
    /// an inline preview. Complements the XEP-0385 `sharedFiles` path for
    /// the common case where a sender just pasted a Tenor/Giphy/CDN link
    /// rather than attaching via file-sharing.
    @ViewBuilder
    private func bodyImageURLsView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let urls = message.detectedImageURLs
        if !urls.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(urls, id: \.absoluteString) { url in
                    AsyncImage(url: url) { phase in
                        switch phase {
                        case .success(let image):
                            image
                                .resizable()
                                .aspectRatio(contentMode: .fit)
                                .frame(maxWidth: maxWidth, maxHeight: 240)
                                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                .overlay(
                                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                                )
                        case .failure:
                            Label(url.lastPathComponent.isEmpty ? "Image" : url.lastPathComponent, systemImage: "photo")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .padding(8)
                                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
                        case .empty:
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .fill(Color.secondary.opacity(0.08))
                                .frame(width: 180, height: 120)
                                .overlay(ProgressView())
                        @unknown default:
                            EmptyView()
                        }
                    }
                }
            }
        }
    }

    private func hatColor(for title: String) -> Color {
        switch title.lowercased() {
        case "owner": return .purple
        case "admin": return .blue
        case "moderator", "mod": return .green
        case "bot": return .mint
        case "verified": return .cyan
        default: return .secondary
        }
    }

    private func formatFileSize(_ bytes: Int) -> String {
        if bytes < 1024 { return "\(bytes) B" }
        if bytes < 1024 * 1024 { return "\(bytes / 1024) KB" }
        return String(format: "%.1f MB", Double(bytes) / (1024 * 1024))
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }
}

struct ChatComposerView: View {
    @Binding var text: String
    var placeholder: String = "Write a message"
    var isSending: Bool = false
    var canSend: Bool = true
    var channelName: String? = nil
    var replyingToMessage: ChatTimelineMessage? = nil
    var onCancelReply: (() -> Void)? = nil
    var onFileSelected: ((_ data: Data, _ fileName: String, _ mediaType: String) -> Void)? = nil
    var onGifSelected: ((_ url: String) -> Void)? = nil
    var isUploadingFile: Bool = false
    var mentionSuggestions: [ChatRoomMember] = []
    var onMentionQueryChanged: ((String?) -> Void)? = nil
    var usesOperationalChrome: Bool = false
    var usesCompactConversationChrome: Bool = false
    var onSend: () -> Void
    @State private var showEmojiPicker = false
    @State private var showGifPicker = false
    @State private var selectedPhoto: PhotosPickerItem?

    private var hasSendableText: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(spacing: 0) {
            mentionSuggestionList

            composerReplyPreview

            VStack(spacing: 0) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .lineLimit(1...6)
                    .font(.body)
                    .foregroundStyle(WaddleTheme.textPrimary)
                    .padding(.horizontal, 14)
                    .padding(.top, 12)
                    .padding(.bottom, 8)
                    .onSubmit { if hasSendableText { onSend() } }

                HStack(spacing: 18) {
                    attachmentPickerButton
                    gifPickerButton
                    emojiPickerButton

                    Spacer()

                    Button(action: onSend) {
                        Image(systemName: "paperplane.fill")
                            .font(.title3)
                            .foregroundStyle(hasSendableText ? WaddleTheme.accent : WaddleTheme.textMuted)
                    }
                    .disabled(!canSend || isSending || !hasSendableText)
                }
                .padding(.horizontal, 14)
                .padding(.bottom, 10)
            }
            .waddleGlass(in: .rect(cornerRadius: 16))
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
        }
        .onChange(of: text) { _, newValue in
            updateMentionQuery(newValue)
        }
    }

    @ViewBuilder
    private var mentionSuggestionList: some View {
        let emojis = emojiSuggestions
        if !mentionSuggestions.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(mentionSuggestions.prefix(8)) { member in
                        Button {
                            insertMention(member.displayName)
                        } label: {
                            Text("@\(member.displayName)")
                                .font(.caption.weight(.medium))
                                .padding(.horizontal, 10)
                                .padding(.vertical, 6)
                                .background(Color.accentColor.opacity(0.1), in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
        } else if !emojis.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(emojis, id: \.name) { item in
                        Button {
                            insertEmoji(item.emoji)
                        } label: {
                            HStack(spacing: 4) {
                                Text(item.emoji)
                                Text(":\(item.name):")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 6)
                            .background(Color.secondary.opacity(0.08), in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
        }
    }

    private func updateMentionQuery(_ text: String) {
        guard let atIndex = text.lastIndex(of: "@") else {
            onMentionQueryChanged?(nil)
            return
        }
        let afterAt = text[text.index(after: atIndex)...]
        if afterAt.contains(" ") || afterAt.contains("\n") {
            onMentionQueryChanged?(nil)
            return
        }
        let query = String(afterAt)
        onMentionQueryChanged?(query)
    }

    private func insertMention(_ username: String) {
        guard let atIndex = text.lastIndex(of: "@") else { return }
        text = String(text[text.startIndex..<atIndex]) + "@\(username) "
        onMentionQueryChanged?(nil)
    }

    private var emojiSuggestions: [(name: String, emoji: String)] {
        guard let colonIndex = text.lastIndex(of: ":") else { return [] }
        let afterColon = text[text.index(after: colonIndex)...]
        if afterColon.contains(" ") || afterColon.contains("\n") || afterColon.contains(":") { return [] }
        let query = String(afterColon).lowercased()
        guard query.count >= 2 else { return [] }
        return Self.emojiShortcodes.filter { $0.name.contains(query) }.prefix(8).map { $0 }
    }

    private func insertEmoji(_ emoji: String) {
        guard let colonIndex = text.lastIndex(of: ":") else { return }
        text = String(text[text.startIndex..<colonIndex]) + emoji
    }

    private static let emojiShortcodes: [(name: String, emoji: String)] = [
        ("thumbsup", "👍"), ("thumbsdown", "👎"), ("heart", "❤️"), ("fire", "🔥"),
        ("smile", "😊"), ("laugh", "😂"), ("cry", "😢"), ("angry", "😤"),
        ("think", "🤔"), ("cool", "😎"), ("love", "😍"), ("wink", "😉"),
        ("clap", "👏"), ("pray", "🙏"), ("wave", "👋"), ("muscle", "💪"),
        ("rocket", "🚀"), ("star", "⭐"), ("check", "✅"), ("cross", "❌"),
        ("100", "💯"), ("eyes", "👀"), ("party", "🎉"), ("tada", "🎉"),
        ("sparkles", "✨"), ("warning", "⚠️"), ("bug", "🐛"), ("bulb", "💡"),
        ("pin", "📌"), ("link", "🔗"), ("lock", "🔒"), ("key", "🔑"),
        ("bell", "🔔"), ("memo", "📝"), ("gear", "⚙️"), ("hammer", "🔨"),
        ("package", "📦"), ("truck", "🚚"), ("calendar", "📅"), ("clock", "⏰"),
        ("sun", "☀️"), ("moon", "🌙"), ("rainbow", "🌈"), ("umbrella", "☂️"),
        ("coffee", "☕"), ("pizza", "🍕"), ("beer", "🍺"), ("cake", "🎂"),
        ("penguin", "🐧"), ("duck", "🦆"), ("dog", "🐶"), ("cat", "🐱"),
        ("skull", "💀"), ("ghost", "👻"), ("robot", "🤖"), ("alien", "👽"),
        ("confused", "😕"), ("shrug", "🤷"), ("facepalm", "🤦"), ("salute", "🫡"),
        ("ok", "👌"), ("point_up", "☝️"), ("point_down", "👇"), ("raised_hands", "🙌"),
    ]


    @ViewBuilder
    private var composerReplyPreview: some View {
        if let reply = replyingToMessage {
            HStack(spacing: 8) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(WaddleTheme.accent)
                    .frame(width: 2)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Replying to \(reply.senderDisplayName)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(WaddleTheme.accent)
                    Text(reply.body)
                        .font(.caption)
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .lineLimit(1)
                }
                Spacer()
                Button { onCancelReply?() } label: {
                    Image(systemName: "xmark")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(WaddleTheme.textMuted)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(WaddleTheme.surfaceRaised)
        }
    }

    private var attachmentPickerButton: some View {
        Group {
            if isUploadingFile {
                ProgressView()
                    .frame(width: 20, height: 20)
            } else {
                PhotosPicker(selection: $selectedPhoto, matching: .any(of: [.images, .videos])) {
                    Image(systemName: "paperclip")
                        .font(.body)
                        .foregroundStyle(WaddleTheme.textSecondary)
                }
                .buttonStyle(.plain)
                .onChange(of: selectedPhoto) { _, newValue in
                    guard let newValue else { return }
                    Task {
                        guard let data = try? await newValue.loadTransferable(type: Data.self) else { return }
                        let mediaType = newValue.supportedContentTypes.first?.preferredMIMEType ?? "application/octet-stream"
                        let fileName = "upload.\(newValue.supportedContentTypes.first?.preferredFilenameExtension ?? "bin")"
                        onFileSelected?(data, fileName, mediaType)
                        selectedPhoto = nil
                    }
                }
            }
        }
    }

    private var gifPickerButton: some View {
        Button {
            showGifPicker.toggle()
        } label: {
            Image(systemName: "play.rectangle")
                .font(.body)
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showGifPicker) {
            ChatGifPickerView { url in
                showGifPicker = false
                onGifSelected?(url)
            }
        }
    }

    private var emojiPickerButton: some View {
        Button {
            showEmojiPicker.toggle()
        } label: {
            Image(systemName: "face.smiling")
                .font(.body)
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showEmojiPicker) {
            ChatEmojiPickerView { emoji in
                text += emoji
                showEmojiPicker = false
            }
        }
    }

}

struct ChatMemberListSection: View {
    let members: [ChatRoomMember]
    var title: String = "Members"

    var body: some View {
        GroupBox(title) {
            if members.isEmpty {
                Text("No members loaded yet.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(members) { member in
                        HStack(spacing: 10) {
                            memberBadge(for: member)

                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(member.displayName)
                                    if member.isSelf {
                                        Text("you")
                                            .font(.caption2.weight(.semibold))
                                            .foregroundStyle(.secondary)
                                            .padding(.horizontal, 5)
                                            .padding(.vertical, 2)
                                            .background(.quaternary.opacity(0.5), in: Capsule())
                                    }
                                }
                                .font(.subheadline.weight(.medium))

                                HStack(spacing: 6) {
                                    Text(member.presence.label)
                                    if let role = member.role, !role.isEmpty {
                                        Text("•")
                                        Text(role)
                                    }
                                }
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            }

                            Spacer(minLength: 0)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func memberBadge(for member: ChatRoomMember) -> some View {
        Text(member.avatarInitials ?? initials(from: member.displayName))
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .frame(width: 30, height: 30)
            .background(presenceColor(for: member.presence).opacity(0.18), in: Circle())
            .overlay(alignment: .bottomTrailing) {
                Circle()
                    .fill(presenceColor(for: member.presence))
                    .frame(width: 9, height: 9)
                    .offset(x: 1, y: 1)
            }
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }

    private func presenceColor(for state: ChatPresenceState) -> Color {
        switch state {
        case .available:
            return .green
        case .away:
            return .orange
        case .dnd:
            return .red
        case .offline, .unknown:
            return .secondary
        }
    }
}

struct ChatTypingIndicatorView: View {
    let typingUsers: [String]
    @State private var animate = false

    var body: some View {
        if !typingUsers.isEmpty {
            HStack(spacing: 8) {
                // Three small dots with a staggered pulse — same motif Slack
                // and Discord use, and cheaper to render than a ProgressView.
                HStack(spacing: 3) {
                    ForEach(0..<3, id: \.self) { index in
                        Circle()
                            .fill(WaddleTheme.textMuted)
                            .frame(width: 5, height: 5)
                            .opacity(animate ? 1.0 : 0.35)
                            .animation(
                                .easeInOut(duration: 0.55)
                                    .repeatForever()
                                    .delay(Double(index) * 0.14),
                                value: animate
                            )
                    }
                }
                Text(typingLabel)
                    .font(.caption2)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
            .onAppear { animate = true }
            .onDisappear { animate = false }
        }
    }

    private var typingLabel: String {
        switch typingUsers.count {
        case 1:
            return "\(typingUsers[0]) is typing…"
        case 2:
            return "\(typingUsers[0]) and \(typingUsers[1]) are typing…"
        default:
            return "\(typingUsers[0]) and \(typingUsers.count - 1) others are typing…"
        }
    }
}

struct ChatGifPickerView: View {
    var onSelect: (String) -> Void
    @State private var searchText = ""
    @State private var results: [GiphyGif] = []
    @State private var isLoading = false
    @State private var searchTask: Task<Void, Never>?

    struct GiphyGif: Identifiable {
        let id: String
        let keywords: [String]
        let images: GiphyImages

        init(id: String, previewURL: String, originalURL: String, keywords: [String]) {
            self.id = id
            self.keywords = keywords
            self.images = GiphyImages(
                fixed_height_small: GiphyImage(url: previewURL, width: nil, height: nil),
                original: GiphyImage(url: originalURL, width: nil, height: nil)
            )
        }

        struct GiphyImages {
            let fixed_height_small: GiphyImage
            let original: GiphyImage
        }

        struct GiphyImage {
            let url: String
            let width: String?
            let height: String?
        }
    }

    private static let sampleGifs: [GiphyGif] = [
        GiphyGif(
            id: "celebrate",
            previewURL: "https://media1.giphy.com/media/111ebonMs90YLu/200w.gif",
            originalURL: "https://media1.giphy.com/media/111ebonMs90YLu/giphy.gif",
            keywords: ["celebrate", "party", "yay", "confetti"]
        ),
        GiphyGif(
            id: "thumbs-up",
            previewURL: "https://media1.giphy.com/media/XreQmk7ETCak0/200w.gif",
            originalURL: "https://media1.giphy.com/media/XreQmk7ETCak0/giphy.gif",
            keywords: ["thumbs", "up", "approve", "yes", "nice"]
        ),
        GiphyGif(
            id: "mind-blown",
            previewURL: "https://media1.giphy.com/media/OK27wINdQS5YQ/200w.gif",
            originalURL: "https://media1.giphy.com/media/OK27wINdQS5YQ/giphy.gif",
            keywords: ["wow", "mind", "blown", "surprised", "amazed"]
        ),
        GiphyGif(
            id: "laughing",
            previewURL: "https://media1.giphy.com/media/10JhviFuU2gWD6/200w.gif",
            originalURL: "https://media1.giphy.com/media/10JhviFuU2gWD6/giphy.gif",
            keywords: ["laugh", "funny", "lol", "haha"]
        ),
        GiphyGif(
            id: "wave",
            previewURL: "https://media1.giphy.com/media/ASd0Ukj0y3qMM/200w.gif",
            originalURL: "https://media1.giphy.com/media/ASd0Ukj0y3qMM/giphy.gif",
            keywords: ["hello", "wave", "hi", "welcome"]
        ),
        GiphyGif(
            id: "coffee",
            previewURL: "https://media1.giphy.com/media/oZEBLugoTthxS/200w.gif",
            originalURL: "https://media1.giphy.com/media/oZEBLugoTthxS/giphy.gif",
            keywords: ["coffee", "morning", "caffeine", "break"]
        ),
    ]

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
                TextField("Search GIFs", text: $searchText)
                    .textFieldStyle(.plain)
                    .foregroundStyle(WaddleTheme.textPrimary)
                if !searchText.isEmpty {
                    Button {
                        searchText = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.caption)
                            .foregroundStyle(WaddleTheme.textMuted)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8))
            .padding(10)
            .onChange(of: searchText) { _, query in
                searchTask?.cancel()
                searchTask = Task {
                    try? await Task.sleep(nanoseconds: 300_000_000)
                    guard !Task.isCancelled else { return }
                    await fetchGifs(query: query)
                }
            }

            if isLoading {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if results.isEmpty {
                Text("No GIFs found")
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 4)], spacing: 4) {
                        ForEach(results) { gif in
                            Button {
                                onSelect(gif.images.original.url)
                            } label: {
                                AsyncImage(url: URL(string: gif.images.fixed_height_small.url)) { phase in
                                    switch phase {
                                    case .success(let image):
                                        image
                                            .resizable()
                                            .aspectRatio(contentMode: .fill)
                                            .frame(height: 80)
                                            .clipped()
                                    case .empty:
                                        WaddleTheme.surfaceRaised
                                            .frame(height: 80)
                                            .overlay { ProgressView() }
                                    default:
                                        WaddleTheme.surfaceRaised
                                            .frame(height: 80)
                                    }
                                }
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(8)
                }
            }
        }
        .frame(width: 360, height: 340)
        .background(WaddleTheme.sidebarBackground)
        .task {
            await fetchGifs(query: "")
        }
    }

    private func fetchGifs(query: String) async {
        isLoading = true
        defer { isLoading = false }

        let endpoint: String
        if query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            endpoint = "https://api.giphy.com/v1/gifs/trending?api_key=\(apiKey)&limit=24&rating=g"
        } else {
            let encoded = query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? query
            endpoint = "https://api.giphy.com/v1/gifs/search?q=\(encoded)&api_key=\(apiKey)&limit=24&rating=g"
        }

            if isLoading {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if results.isEmpty {
                Text("No GIFs found")
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 4)], spacing: 4) {
                        ForEach(results) { gif in
                            Button {
                                onSelect(gif.images.original.url)
                            } label: {
                                AsyncImage(url: URL(string: gif.images.fixed_height_small.url)) { phase in
                                    switch phase {
                                    case .success(let image):
                                        image
                                            .resizable()
                                            .aspectRatio(contentMode: .fill)
                                            .frame(height: 80)
                                            .clipped()
                                    case .empty:
                                        WaddleTheme.surfaceRaised
                                            .frame(height: 80)
                                            .overlay { ProgressView() }
                                    default:
                                        WaddleTheme.surfaceRaised
                                            .frame(height: 80)
                                    }
                                }
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(8)
                }
            }
        }
        .frame(width: 360, height: 340)
        .background(WaddleTheme.sidebarBackground)
        .task {
            await fetchGifs(query: "")
        }
    }

    private func fetchGifs(query: String) async {
        isLoading = true
        defer { isLoading = false }

        let searchTerms = query
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)

        if searchTerms.isEmpty {
            results = Self.sampleGifs
            return
        }

        results = Self.sampleGifs.filter { gif in
            let haystack = ([gif.id] + gif.keywords).map { $0.lowercased() }
            return searchTerms.allSatisfy { term in
                haystack.contains { $0.contains(term) }
            }
        }
    }
}

struct ChatDmListView: View {
    let conversations: [DmConversation]
    var onSelect: ((DmConversation) -> Void)? = nil
    var onNewDm: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Direct Messages")
                    .font(.headline)
                Spacer()
                if let onNewDm {
                    Button(action: onNewDm) {
                        Image(systemName: "plus.circle.fill")
                            .font(.body)
                            .foregroundStyle(Color.accentColor)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(12)

            Divider()

            if conversations.isEmpty {
                ChatEmptyStateView(
                    title: "No conversations",
                    message: "Start a direct message with a member.",
                    systemImage: "person.2"
                )
            } else {
                ScrollView {
                    LazyVStack(spacing: 2) {
                        ForEach(conversations) { convo in
                            Button {
                                onSelect?(convo)
                            } label: {
                                HStack(spacing: 10) {
                                    Text(convo.peerUsername.prefix(2).uppercased())
                                        .font(.caption.weight(.semibold))
                                        .foregroundStyle(.secondary)
                                        .frame(width: 32, height: 32)
                                        .background(dmPresenceColor(convo.presenceShow).opacity(0.16), in: Circle())
                                        .overlay(alignment: .bottomTrailing) {
                                            Circle()
                                                .fill(dmPresenceColor(convo.presenceShow))
                                                .frame(width: 9, height: 9)
                                        }

                                    VStack(alignment: .leading, spacing: 2) {
                                        HStack {
                                            Text(convo.peerUsername)
                                                .font(.subheadline.weight(.medium))
                                                .lineLimit(1)
                                            Spacer()
                                            if let date = convo.lastMessageAt {
                                                Text(date, style: .relative)
                                                    .font(.caption2)
                                                    .foregroundStyle(.secondary)
                                            }
                                        }
                                        if let body = convo.lastMessageBody {
                                            Text(body)
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                                .lineLimit(1)
                                        }
                                    }

                                    if convo.unreadCount > 0 {
                                        Text("\(convo.unreadCount)")
                                            .font(.caption2.weight(.bold))
                                            .foregroundStyle(.white)
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 2)
                                            .background(Color.accentColor, in: Capsule())
                                    }
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.vertical, 4)
                }
            }
        }
    }

    private func dmPresenceColor(_ state: ChatPresenceState) -> Color {
        switch state {
        case .available: return .green
        case .away: return .orange
        case .dnd: return .red
        case .offline, .unknown: return .secondary
        }
    }
}

struct ChatDmConversationView: View {
    let peerUsername: String
    let messages: [ChatTimelineMessage]
    @Binding var composerText: String
    var isSending: Bool = false
    var onSend: () -> Void
    var onBack: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Button(action: onBack) {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.semibold))
                }
                .buttonStyle(.plain)

                Text(peerUsername)
                    .font(.headline)
                Spacer()
            }
            .padding(12)

            Divider()

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(messages) { message in
                        HStack(alignment: .top, spacing: 8) {
                            if message.isOutgoing { Spacer(minLength: 40) }

                            VStack(alignment: message.isOutgoing ? .trailing : .leading, spacing: 4) {
                                Text(message.styledBody)
                                    .font(.subheadline)
                                    .textSelection(.enabled)

                                Text(message.sentAt, style: .time)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 8)
                            .background(
                                message.isOutgoing ? Color.accentColor.opacity(0.12) : Color.secondary.opacity(0.08),
                                in: RoundedRectangle(cornerRadius: 14, style: .continuous)
                            )

                            if !message.isOutgoing { Spacer(minLength: 40) }
                        }
                    }
                }
                .padding(12)
            }

            Divider()

            HStack(spacing: 10) {
                TextField("Message \(peerUsername)", text: $composerText)
                    .textFieldStyle(.roundedBorder)

                Button(action: onSend) {
                    Image(systemName: "paperplane.fill")
                }
                .disabled(composerText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSending)
                .buttonStyle(.borderedProminent)
            }
            .padding(12)
        }
    }
}

struct ChatForumTopicListView: View {
    let topics: [ChatTimelineMessage]
    var onSelectTopic: ((ChatTimelineMessage) -> Void)? = nil
    var onCreateTopic: (() -> Void)? = nil

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 10) {
                if topics.isEmpty {
                    ChatEmptyStateView(
                        title: "No topics yet",
                        message: "Start a discussion by creating a topic.",
                        systemImage: "text.bubble"
                    )
                } else {
                    ForEach(topics) { topic in
                        Button {
                            onSelectTopic?(topic)
                        } label: {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(topic.forumTitle ?? topic.body)
                                    .font(.subheadline.weight(.semibold))
                                    .lineLimit(2)
                                    .multilineTextAlignment(.leading)

                                HStack(spacing: 8) {
                                    Text(topic.senderDisplayName)
                                        .font(.caption.weight(.medium))
                                    Text(topic.sentAt, style: .relative)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                .foregroundStyle(.secondary)

                                if !topic.body.isEmpty, topic.forumTitle != nil, topic.body != topic.forumTitle {
                                    Text(topic.body)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(2)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(12)
                            .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .padding(16)
        }
    }
}

struct ChatForumThreadView: View {
    let topic: ChatTimelineMessage
    let replies: [ChatTimelineMessage]
    @Binding var replyText: String
    var isSending: Bool = false
    var onSendReply: () -> Void
    var onBack: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Button(action: onBack) {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.semibold))
                }
                .buttonStyle(.plain)

                VStack(alignment: .leading, spacing: 2) {
                    Text(topic.forumTitle ?? "Thread")
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    Text("\(replies.count) replies")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Spacer()
            }
            .padding(12)

            Divider()

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 6) {
                            Text(topic.senderDisplayName)
                                .font(.subheadline.weight(.semibold))
                            Text(topic.sentAt, style: .relative)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Text(topic.body)
                            .font(.body)
                            .textSelection(.enabled)
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.accentColor.opacity(0.06), in: RoundedRectangle(cornerRadius: 12, style: .continuous))

                    ForEach(replies) { reply in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 6) {
                                Text(reply.senderDisplayName)
                                    .font(.caption.weight(.semibold))
                                Text(reply.sentAt, style: .relative)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Text(reply.styledBody)
                                .font(.subheadline)
                                .textSelection(.enabled)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                    }
                }
                .padding(12)
            }

            Divider()

            HStack(spacing: 10) {
                TextField("Reply to thread…", text: $replyText)
                    .textFieldStyle(.roundedBorder)

                Button(action: onSendReply) {
                    Image(systemName: "paperplane.fill")
                }
                .disabled(replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSending)
                .buttonStyle(.borderedProminent)
            }
            .padding(12)
        }
    }
}

struct ChatNotificationToastView: View {
    let toast: ChatNotificationToast
    var onDismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "bell.badge.fill")
                .font(.body)
                .foregroundStyle(Color.accentColor)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(toast.senderName)
                        .font(.caption.weight(.semibold))
                    if let channel = toast.channelName {
                        Text("in #\(channel)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Text(toast.body)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 0)

            Button {
                onDismiss()
            } label: {
                Image(systemName: "xmark")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
        }
        .padding(12)
        .waddleGlass(in: .rect(cornerRadius: 14))
        .padding(.horizontal, 16)
        .transition(.move(edge: .top).combined(with: .opacity))
    }
}

struct ChatImageLightboxView: View {
    let file: XMPPSharedFile
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            AsyncImage(url: URL(string: file.url)) { phase in
                switch phase {
                case .success(let image):
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .ignoresSafeArea()
                case .failure:
                    VStack(spacing: 12) {
                        Image(systemName: "photo.badge.exclamationmark")
                            .font(.largeTitle)
                        Text("Failed to load image")
                            .font(.subheadline)
                    }
                    .foregroundStyle(.white.opacity(0.6))
                case .empty:
                    ProgressView()
                        .tint(.white)
                @unknown default:
                    EmptyView()
                }
            }
        }
        .overlay(alignment: .topTrailing) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title2)
                    .foregroundStyle(.white.opacity(0.8))
                    .padding(16)
            }
        }
        .overlay(alignment: .bottom) {
            if let name = file.name, !name.isEmpty {
                Text(name)
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.7))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(.black.opacity(0.5), in: Capsule())
                    .padding(.bottom, 20)
            }
        }
#if os(iOS)
        .statusBarHidden()
#endif
    }
}

struct ChatEmojiPickerView: View {
    var onSelect: (String) -> Void
    @State private var searchText = ""

    private static let emojiCategories: [(name: String, emojis: [String])] = [
        ("Smileys", ["😀", "😂", "🥹", "😍", "🤩", "😎", "🤔", "😅", "😢", "😤", "🥺", "😱", "🤗", "🫡", "🤝", "🙏"]),
        ("Reactions", ["👍", "👎", "❤️", "🔥", "🎉", "✅", "❌", "💯", "👀", "🚀", "💪", "🙌", "👏", "🤷", "💀", "😭"]),
        ("Objects", ["💬", "📎", "📌", "🔗", "💡", "⚡", "🎯", "🏷️", "📝", "🔔", "⭐", "🌟", "💎", "🛠️", "🔒", "🔑"]),
        ("Nature", ["🌈", "☀️", "🌙", "⭐", "🌊", "🌸", "🍀", "🌻", "🐧", "🦆", "🐝", "🦋", "🐳", "🌴", "🍄", "🌵"]),
    ]

    private var filteredEmojis: [(name: String, emojis: [String])] {
        if searchText.isEmpty { return Self.emojiCategories }
        return Self.emojiCategories.compactMap { category in
            let filtered = category.emojis.filter { _ in true }
            return filtered.isEmpty ? nil : (category.name, filtered)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            TextField("Search emoji", text: $searchText)
                .textFieldStyle(.roundedBorder)
                .padding(10)

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(filteredEmojis, id: \.name) { category in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(category.name)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 4)

                            LazyVGrid(columns: Array(repeating: GridItem(.fixed(36), spacing: 4), count: 8), spacing: 4) {
                                ForEach(category.emojis, id: \.self) { emoji in
                                    Button {
                                        onSelect(emoji)
                                    } label: {
                                        Text(emoji)
                                            .font(.title2)
                                            .frame(width: 36, height: 36)
                                    }
                                    .buttonStyle(.plain)
                                }
                            }
                        }
                    }
                }
                .padding(10)
            }
        }
        .frame(width: 340, height: 320)
    }
}

struct ChatLoadingStateView: View {
    var title: String = "Loading conversation…"

    var body: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text(title)
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ChatEmptyStateView: View {
    var title: String
    var message: String?
    var systemImage: String = "bubble.left.and.bubble.right"

    init(title: String, message: String? = nil, systemImage: String = "bubble.left.and.bubble.right") {
        self.title = title
        self.message = message
        self.systemImage = systemImage
    }

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: systemImage)
                .font(.title2)
                .foregroundStyle(WaddleTheme.textMuted)
            Text(title)
                .font(.headline)
                .foregroundStyle(WaddleTheme.textSecondary)
            if let message {
                Text(message)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

struct ChatErrorStateView: View {
    var title: String = "Something went wrong"
    var message: String
    var retryTitle: String = "Try again"
    var onRetry: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.title2)
                .foregroundStyle(WaddleTheme.textMuted)
            Text(title)
                .font(.headline)
            Text(message)
                .foregroundStyle(WaddleTheme.textSecondary)
                .multilineTextAlignment(.center)
            if let onRetry {
                Button(retryTitle, action: onRetry)
                    .buttonStyle(.borderedProminent)
                    .padding(.top, 4)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

/// Sheet-presented panel showing a thread root plus its XEP-0201 children.
/// The panel composer posts replies that carry `<thread>root.id</thread>` so
/// the resulting messages cluster back into this view on arrival.
struct ChatThreadPanelView: View {
    let root: ChatTimelineMessage
    let replies: [ChatTimelineMessage]
    @Binding var composerText: String
    var isSending: Bool = false
    var canGoBack: Bool = false
    var threadChildCount: (String) -> Int = { _ in 0 }
    var onOpenNestedThread: ((ChatTimelineMessage) -> Void)? = nil
    var onBack: (() -> Void)? = nil
    var onSend: () -> Void
    var onClose: () -> Void
    /// Forwarded through to each `ChatMessageRowView` so thread replies
    /// render XEP-0084 PEP avatars just like the main timeline.
    var avatarDataBySenderID: (String) -> Data? = { _ in nil }
    var onRequestAvatar: ((String) -> Void)? = nil

    var body: some View {
        VStack(spacing: 0) {
            header

            Divider()

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        ChatMessageRowView(
                            message: root,
                            usesCompactConversationStyle: true,
                            avatarData: avatarDataBySenderID(root.senderID),
                            onRequestAvatar: onRequestAvatar
                        )

                        threadDivider

                        ForEach(Array(replies.enumerated()), id: \.element.id) { index, reply in
                            let previous = index > 0 ? replies[index - 1] : nil
                            let next = index + 1 < replies.count ? replies[index + 1] : nil
                            ChatMessageRowView(
                                message: reply,
                                previousMessage: previous,
                                nextMessage: next,
                                usesCompactConversationStyle: true,
                                threadChildCount: threadChildCount(reply.id),
                                onOpenThread: onOpenNestedThread,
                                avatarData: avatarDataBySenderID(reply.senderID),
                                onRequestAvatar: onRequestAvatar
                            )
                            .id(reply.id)
                        }

                        if replies.isEmpty {
                            Text("No replies yet — start the thread below.")
                                .font(.footnote)
                                .foregroundStyle(WaddleTheme.textMuted)
                                .frame(maxWidth: .infinity, alignment: .center)
                                .padding(.top, 20)
                        }

                        Color.clear.frame(height: 1).id("thread-bottom")
                    }
                    .padding(.vertical, 8)
                }
                .background(WaddleTheme.chatBackground)
                .onChange(of: replies.count) { _, _ in
                    withAnimation { proxy.scrollTo("thread-bottom", anchor: .bottom) }
                }
            }

            Divider()

            threadComposer
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            if canGoBack, let onBack {
                Button(action: onBack) {
                    Image(systemName: "chevron.backward.circle.fill")
                        .font(.title3)
                        .foregroundStyle(WaddleTheme.accent)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back to parent thread")
            }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Image(systemName: "bubble.left.and.bubble.right.fill")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(WaddleTheme.accent)
                    Text("Thread")
                        .font(.headline)
                }
                Text(replies.count == 1 ? "1 reply" : "\(replies.count) replies")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                onClose()
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .buttonStyle(.plain)
        }
        .padding(12)
    }

    private var threadDivider: some View {
        HStack(spacing: 10) {
            Text(replies.isEmpty ? "Replies" : (replies.count == 1 ? "1 reply" : "\(replies.count) replies"))
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textMuted)
            VStack { Divider().overlay(WaddleTheme.divider) }
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 4)
    }

    private var threadComposer: some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField("Reply to thread…", text: $composerText, axis: .vertical)
                .lineLimit(1...5)
                .textFieldStyle(.plain)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(
                    WaddleTheme.surfaceRaised,
                    in: RoundedRectangle(cornerRadius: 14, style: .continuous)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                )
                .onSubmit {
                    if canSend { onSend() }
                }

            Button(action: onSend) {
                if isSending {
                    ProgressView()
                        .frame(width: 20, height: 20)
                } else {
                    Image(systemName: "paperplane.fill")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(canSend ? WaddleTheme.accent : WaddleTheme.textMuted)
                }
            }
            .buttonStyle(.plain)
            .disabled(!canSend || isSending)
        }
        .padding(12)
    }

    private var canSend: Bool {
        !composerText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}
