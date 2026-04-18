import SwiftUI

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
                            onReply: onReply
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
        HStack(spacing: 10) {
            Rectangle()
                .fill(Color.secondary.opacity(0.14))
                .frame(height: 1)

            Text(date, format: .dateTime.weekday(.abbreviated).month(.abbreviated).day())
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .background(Color.secondary.opacity(0.08), in: Capsule())

            Rectangle()
                .fill(Color.secondary.opacity(0.14))
                .frame(height: 1)
        }
        .padding(.vertical, 10)
    }
}

struct ChatMessageRowView: View {
    let message: ChatTimelineMessage
    var previousMessage: ChatTimelineMessage? = nil
    var nextMessage: ChatTimelineMessage? = nil
    var usesCompactConversationStyle: Bool = false
    var onReply: ((ChatTimelineMessage) -> Void)? = nil

    var body: some View {
        if usesOperationalLayout {
            operationalRow
        } else if usesCompactConversationStyle {
            compactPhoneRow
        } else {
            bubbleRow
        }
    }

    private var usesOperationalLayout: Bool {
#if os(macOS)
        return true
#else
        return false
#endif
    }

    @ViewBuilder
    private var operationalRow: some View {
        if message.isAction {
            Text(message.body)
                .font(.footnote.weight(.medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color.secondary.opacity(0.10), in: Capsule())
                .frame(maxWidth: .infinity, alignment: .center)
        } else {
            HStack(alignment: .top, spacing: 12) {
                if message.isOutgoing {
                    Spacer(minLength: 72)
                    messageCard(applyBackground: true, horizontalPadding: 14, verticalPadding: 10, maxWidth: 620)
                } else {
                    avatar
                        .frame(width: 36, height: 36)
                    messageCard(applyBackground: false, horizontalPadding: 0, verticalPadding: 2, maxWidth: 760)
                    Spacer(minLength: 0)
                }
            }
            .frame(maxWidth: .infinity, alignment: message.isOutgoing ? .trailing : .leading)
        }
    }

    @ViewBuilder
    private var compactPhoneRow: some View {
        if message.isAction {
            Text(message.body)
                .font(.footnote.weight(.medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color.secondary.opacity(0.10), in: Capsule())
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 4)
        } else {
            HStack(alignment: .top, spacing: 10) {
                if showsCompactAvatar {
                    avatar
                } else {
                    Color.clear
                        .frame(width: 32, height: 1)
                }

                VStack(alignment: .leading, spacing: 4) {
                    if showsCompactMetadata {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(message.isOutgoing ? "You" : message.senderDisplayName)
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(.primary)
                                .lineLimit(1)

                            Text(message.sentAt, style: .time)
                                .font(.caption)
                                .foregroundStyle(.secondary)

                            if message.editedAt != nil {
                                Text("edited")
                                    .font(.caption2.weight(.medium))
                                    .foregroundStyle(.secondary)
                            }

                            Spacer(minLength: 0)
                        }
                    }

                    compactMessageCard
                }

                Spacer(minLength: 0)
            }
            .padding(.top, showsCompactMetadata ? 2 : 1)
            .padding(.bottom, endsCompactCluster ? 10 : 2)
        }
    }

    private var bubbleRow: some View {
        HStack(alignment: .top, spacing: 10) {
            if message.isOutgoing {
                Spacer(minLength: 28)
                messageCard(applyBackground: true, horizontalPadding: 14, verticalPadding: 11, maxWidth: 520)
            } else {
                avatar
                messageCard(applyBackground: true, horizontalPadding: 14, verticalPadding: 11, maxWidth: 520)
                Spacer(minLength: 28)
            }
        }
    }

    private var avatar: some View {
        Text(message.senderInitials ?? initials(from: message.senderDisplayName))
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .frame(width: 32, height: 32)
            .background(.quaternary.opacity(0.5), in: Circle())
    }

    private func messageCard(
        applyBackground: Bool,
        horizontalPadding: CGFloat,
        verticalPadding: CGFloat,
        maxWidth: CGFloat
    ) -> some View {
        VStack(alignment: message.isOutgoing ? .trailing : .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(message.senderDisplayName)
                    .font(.subheadline.weight(.semibold))
                Text(message.sentAt, style: .time)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if message.editedAt != nil {
                    Text("edited")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            if let replyToID = message.replyToID, !replyToID.isEmpty {
                replyIndicator
            }

            if message.isRetracted {
                Text("Message removed")
                    .font(.body)
                    .italic()
                    .foregroundStyle(.secondary)
            } else {
                Text(message.styledBody)
                    .font(.body)
                    .lineSpacing(usesOperationalLayout ? 3 : 0)
                    .multilineTextAlignment(message.isOutgoing ? .trailing : .leading)
                    .textSelection(.enabled)
            }

            if let reactions = message.reactions, !reactions.isEmpty {
                HStack(spacing: 8) {
                    ForEach(reactions.keys.sorted(), id: \.self) { emoji in
                        let count = reactions[emoji]?.count ?? 0
                        Text("\(emoji) \(count)")
                            .font(.caption)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(.quaternary.opacity(0.6), in: Capsule())
                    }
                }
            }

            if message.deliveryState != .sent || message.isOutgoing {
                HStack(spacing: 6) {
                    Image(systemName: deliverySymbolName(for: message.deliveryState))
                        .font(.caption2)
                    Text(message.deliveryState.label)
                        .font(.caption2)
                }
                .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, horizontalPadding)
        .padding(.vertical, verticalPadding)
        .frame(maxWidth: maxWidth, alignment: message.isOutgoing ? .trailing : .leading)
        .background {
            if applyBackground {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .fill(message.isOutgoing ? Color.accentColor.opacity(0.11) : Color.secondary.opacity(0.08))
            }
        }
        .contextMenu {
            if !message.isAction, !message.isRetracted, onReply != nil {
                Button {
                    onReply?(message)
                } label: {
                    Label("Reply", systemImage: "arrowshape.turn.up.left")
                }
            }
        }
    }

    @ViewBuilder
    private var replyIndicator: some View {
        HStack(spacing: 6) {
            RoundedRectangle(cornerRadius: 2)
                .fill(Color.accentColor.opacity(0.5))
                .frame(width: 3)

            VStack(alignment: .leading, spacing: 2) {
                if let senderName = message.replyToSenderName, !senderName.isEmpty {
                    Text(senderName)
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(Color.accentColor)
                }
                Text(message.replyToBody ?? "Original message")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(Color.accentColor.opacity(0.06), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }

    private var compactMessageCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let replyToID = message.replyToID, !replyToID.isEmpty {
                replyIndicator
            }

            if message.isRetracted {
                Text("Message removed")
                    .font(.subheadline)
                    .italic()
                    .foregroundStyle(.secondary)
            } else {
                Text(message.styledBody)
                    .font(.subheadline)
                    .foregroundStyle(.primary)
                    .lineSpacing(3)
                    .multilineTextAlignment(.leading)
                    .textSelection(.enabled)
            }

            if let reactions = message.reactions, !reactions.isEmpty {
                HStack(spacing: 6) {
                    ForEach(reactions.keys.sorted(), id: \.self) { emoji in
                        let count = reactions[emoji]?.count ?? 0
                        Text("\(emoji) \(count)")
                            .font(.caption.weight(.medium))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(Color.secondary.opacity(0.10), in: Capsule())
                    }
                }
            }

            if shouldShowCompactDelivery {
                HStack(spacing: 6) {
                    Image(systemName: deliverySymbolName(for: message.deliveryState))
                        .font(.caption2)
                    Text(message.deliveryState.label)
                        .font(.caption2.weight(.medium))
                }
                .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: 560, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(compactCardFill)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(compactCardStroke, lineWidth: 1)
        }
        .contextMenu {
            if !message.isAction, !message.isRetracted, onReply != nil {
                Button {
                    onReply?(message)
                } label: {
                    Label("Reply", systemImage: "arrowshape.turn.up.left")
                }
            }
        }
    }

    private var showsCompactAvatar: Bool {
        !message.formsCompactCluster(with: previousMessage)
    }

    private var showsCompactMetadata: Bool {
        !message.formsCompactCluster(with: previousMessage)
    }

    private var endsCompactCluster: Bool {
        nextMessage?.formsCompactCluster(with: message) != true
    }

    private var shouldShowCompactDelivery: Bool {
        (message.deliveryState != .sent || message.isOutgoing) && endsCompactCluster
    }

    private var compactCardFill: Color {
#if os(iOS)
        if message.isOutgoing {
            return Color.accentColor.opacity(0.12)
        }
        return Color(.secondarySystemGroupedBackground)
#else
        return message.isOutgoing ? Color.accentColor.opacity(0.10) : Color.secondary.opacity(0.08)
#endif
    }

    private var compactCardStroke: Color {
        if message.isOutgoing {
            return Color.accentColor.opacity(0.14)
        }
        return Color.secondary.opacity(0.10)
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }

    private func deliverySymbolName(for state: ChatDeliveryState) -> String {
        switch state {
        case .pending:
            return "clock"
        case .sending:
            return "arrow.up.circle"
        case .sent:
            return "checkmark"
        case .delivered:
            return "checkmark.circle"
        case .read:
            return "eye"
        case .failed:
            return "exclamationmark.circle"
        }
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
    var usesOperationalChrome: Bool = false
    var usesCompactConversationChrome: Bool = false
    var onSend: () -> Void
    @State private var showEmojiPicker = false

    private var hasSendableText: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        Group {
            if usesCompactConversationChrome {
                compactConversationComposer
            } else {
                standardComposer
            }
        }
    }

    private var standardComposer: some View {
        VStack(spacing: usesOperationalChrome ? 10 : 12) {
            if usesOperationalChrome {
                HStack {
                    Label("Compose", systemImage: "square.and.pencil")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                    Text("Clear, durable conversation")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }

            composerReplyPreview

            HStack(alignment: .bottom, spacing: 12) {
                editor(minHeight: usesOperationalChrome ? 56 : 44, maxHeight: 140)
                    .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 16, style: .continuous))

                emojiPickerButton

                sendButton
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(usesOperationalChrome ? 14 : 12)
        .background(standardComposerBackground)
    }

    private var compactConversationComposer: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                if let channelName, canSend {
                    Label("#\(channelName)", systemImage: "number")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                } else {
                    Label("Choose a channel", systemImage: "bubble.left.and.bubble.right")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                }

                Spacer(minLength: 0)

                Text("Tap send")
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.tertiary)
            }

            composerReplyPreview

            HStack(alignment: .bottom, spacing: 10) {
                editor(minHeight: 52, maxHeight: 120)
                    .background(compactComposerFieldFill, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 18, style: .continuous)
                            .strokeBorder(Color.secondary.opacity(0.10), lineWidth: 1)
                    }

                emojiPickerButton

                sendButton
                    .buttonStyle(.plain)
                    .frame(width: 44, height: 44)
                    .background(sendButtonFill, in: Circle())
                    .foregroundStyle(sendButtonForeground)
            }
        }
        .padding(14)
        .background(compactComposerBackground)
    }

    private func editor(minHeight: CGFloat, maxHeight: CGFloat) -> some View {
        ZStack(alignment: .topLeading) {
            if !hasSendableText {
                Text(placeholder)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, usesCompactConversationChrome ? 12 : 14)
                    .padding(.vertical, 14)
            }

            TextEditor(text: $text)
                .scrollContentBackground(.hidden)
                .frame(minHeight: minHeight, maxHeight: maxHeight)
                .padding(.horizontal, usesCompactConversationChrome ? 8 : 10)
                .padding(.vertical, usesCompactConversationChrome ? 6 : 8)
        }
    }

    @ViewBuilder
    private var composerReplyPreview: some View {
        if let reply = replyingToMessage {
            HStack(spacing: 8) {
                RoundedRectangle(cornerRadius: 2)
                    .fill(Color.accentColor)
                    .frame(width: 3)

                VStack(alignment: .leading, spacing: 2) {
                    Text("Replying to \(reply.senderDisplayName)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Color.accentColor)

                    Text(reply.body)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }

                Spacer(minLength: 0)

                Button {
                    onCancelReply?()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.body)
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }

    private var emojiPickerButton: some View {
        Button {
            showEmojiPicker.toggle()
        } label: {
            Image(systemName: "face.smiling")
                .font(usesCompactConversationChrome ? .title3 : .body)
                .foregroundStyle(.secondary)
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showEmojiPicker) {
            ChatEmojiPickerView { emoji in
                text += emoji
                showEmojiPicker = false
            }
        }
    }

    private var sendButton: some View {
        Button(action: onSend) {
            if isSending {
                ProgressView()
                    .frame(width: 18, height: 18)
            } else {
                Image(systemName: usesCompactConversationChrome ? "paperplane.circle.fill" : "paperplane.fill")
                    .font(usesCompactConversationChrome ? .title3 : .body)
            }
        }
        .disabled(!canSend || isSending || !hasSendableText)
    }

    @ViewBuilder
    private var standardComposerBackground: some View {
        if usesOperationalChrome {
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .fill(Color.primary.opacity(0.04))
        } else {
            Color.clear
        }
    }

    private var compactComposerBackground: some View {
#if os(iOS)
        Color(.secondarySystemGroupedBackground)
#else
        Color.secondary.opacity(0.08)
#endif
    }

    private var compactComposerFieldFill: Color {
#if os(iOS)
        Color(.systemBackground)
#else
        Color(nsColor: .windowBackgroundColor)
#endif
    }

    private var sendButtonFill: Color {
        canSend && hasSendableText && !isSending ? Color.accentColor : Color.secondary.opacity(0.12)
    }

    private var sendButtonForeground: Color {
        canSend && hasSendableText && !isSending ? enabledSendButtonForeground : .secondary
    }

    private var enabledSendButtonForeground: Color {
#if os(iOS)
        Color(.systemBackground)
#else
        Color(nsColor: .windowBackgroundColor)
#endif
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
                Image(systemName: "ellipsis.bubble")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .symbolEffect(.pulse)
                Text(typingLabel)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
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
                .foregroundStyle(.secondary)
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
                .foregroundStyle(.secondary)
            Text(title)
                .font(.headline)
            if let message {
                Text(message)
                    .foregroundStyle(.secondary)
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
                .foregroundStyle(.secondary)
            Text(title)
                .font(.headline)
            Text(message)
                .foregroundStyle(.secondary)
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
