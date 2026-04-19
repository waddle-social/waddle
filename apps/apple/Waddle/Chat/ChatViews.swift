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

                        ChatMessageRowView(
                            message: message,
                            previousMessage: previousMessage,
                            nextMessage: nextMessage,
                            usesCompactConversationStyle: usesCompactConversationStyle,
                            onReply: onReply,
                            onRetract: onRetract
                        )
                        .id(message.id)
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
        }
        .background(WaddleTheme.chatBackground)
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

struct ChatTimelineDayDividerView: View {
    let date: Date

    var body: some View {
        HStack(spacing: 12) {
            Rectangle().fill(WaddleTheme.divider).frame(height: 1)
            Text(date, format: .dateTime.weekday(.abbreviated).month(.abbreviated).day())
                .font(.caption2.weight(.semibold))
                .foregroundStyle(WaddleTheme.textMuted)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(WaddleTheme.surfaceRaised, in: Capsule())
            Rectangle().fill(WaddleTheme.divider).frame(height: 1)
        }
        .padding(.vertical, 12)
        .padding(.horizontal, 16)
    }
}

struct ChatMessageRowView: View {
    let message: ChatTimelineMessage
    var previousMessage: ChatTimelineMessage? = nil
    var nextMessage: ChatTimelineMessage? = nil
    var usesCompactConversationStyle: Bool = false
    var onReply: ((ChatTimelineMessage) -> Void)? = nil
    var onRetract: ((ChatTimelineMessage) -> Void)? = nil
    @State private var lightboxImage: XMPPSharedFile?

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
                    if !message.displayBody.isEmpty {
                        Text(message.styledBody)
                            .font(WaddleTheme.bodyFont)
                            .foregroundStyle(WaddleTheme.textPrimary)
                            .textSelection(.enabled)
                    }

                    inlineImagesView(for: message, maxWidth: 300)
                    downloadableFilesView(for: message)
                }

                if let reactions = message.reactions, !reactions.isEmpty {
                    reactionBar(reactions)
                }
            }

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.top, showsHeader ? 8 : 1)
        .padding(.bottom, 1)
        .contextMenu {
            if !message.isAction, !message.isRetracted {
                if onReply != nil {
                    Button { onReply?(message) } label: {
                        Label("Reply", systemImage: "arrowshape.turn.up.left")
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

    private var avatar: some View {
        Text(message.senderInitials ?? initials(from: message.senderDisplayName))
            .font(.caption.weight(.bold))
            .foregroundStyle(.white)
            .frame(width: WaddleTheme.messageAvatarSize, height: WaddleTheme.messageAvatarSize)
            .background(
                WaddleTheme.accent.opacity(0.6),
                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
            )
    }

    @ViewBuilder
    private var replyIndicator: some View {
        HStack(spacing: 6) {
            RoundedRectangle(cornerRadius: 1.5)
                .fill(WaddleTheme.accent)
                .frame(width: 2)

            VStack(alignment: .leading, spacing: 1) {
                if let senderName = message.replyToSenderName, !senderName.isEmpty {
                    Text(senderName)
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(WaddleTheme.accent)
                }
                Text(message.replyToBody ?? "Original message")
                    .font(.caption2)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .lineLimit(1)
            }
        }
        .padding(.leading, 8)
        .padding(.vertical, 4)
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

            WaddleTheme.divider.frame(height: 1)

            VStack(spacing: 0) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .lineLimit(1...6)
                    .font(.body)
                    .foregroundStyle(WaddleTheme.textPrimary)
                    .padding(.horizontal, 14)
                    .padding(.top, 10)
                    .padding(.bottom, 6)
                    .onSubmit { if hasSendableText { onSend() } }

                HStack(spacing: 16) {
                    attachmentPickerButton
                    gifPickerButton
                    emojiPickerButton

                    Spacer()

                    Button(action: onSend) {
                        Image(systemName: "paperplane.fill")
                            .font(.body)
                            .foregroundStyle(hasSendableText ? WaddleTheme.accent : WaddleTheme.textMuted)
                    }
                    .disabled(!canSend || isSending || !hasSendableText)
                }
                .padding(.horizontal, 14)
                .padding(.bottom, 8)
            }
            .background(WaddleTheme.composerBackground)
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

    var body: some View {
        if !typingUsers.isEmpty {
            HStack(spacing: 6) {
                ProgressView()
                    .scaleEffect(0.6)
                    .tint(WaddleTheme.textMuted)
                Text(typingLabel)
                    .font(.caption2)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
        }
    }

    private var typingLabel: String {
        switch typingUsers.count {
        case 1:
            return "\(typingUsers[0]) is typing"
        case 2:
            return "\(typingUsers[0]) and \(typingUsers[1]) are typing"
        default:
            return "\(typingUsers[0]) and \(typingUsers.count - 1) others are typing"
        }
    }
}

struct ChatGifPickerView: View {
    var onSelect: (String) -> Void
    @State private var searchText = ""
    @State private var results: [GiphyGif] = []
    @State private var isLoading = false
    @State private var searchTask: Task<Void, Never>?

    private let apiKey = "dc6zaTOxFJmzC"

    struct GiphyGif: Identifiable, Decodable {
        let id: String
        let images: GiphyImages

        struct GiphyImages: Decodable {
            let fixed_height_small: GiphyImage
            let original: GiphyImage
        }

        struct GiphyImage: Decodable {
            let url: String
            let width: String?
            let height: String?
        }
    }

    struct GiphyResponse: Decodable {
        let data: [GiphyGif]
    }

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

        guard let url = URL(string: endpoint) else { return }
        do {
            let (data, _) = try await URLSession.shared.data(from: url)
            let response = try JSONDecoder().decode(GiphyResponse.self, from: data)
            results = response.data
        } catch {
            results = []
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
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .shadow(color: .black.opacity(0.1), radius: 8, y: 4)
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
