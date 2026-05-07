import SwiftUI
import UniformTypeIdentifiers

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
                                            .background(WaddleTheme.unreadBadge, in: Capsule())
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
        case .available: return WaddleTheme.presenceOnline
        case .away:      return WaddleTheme.presenceAway
        case .dnd:       return WaddleTheme.presenceDnd
        case .offline, .unknown: return WaddleTheme.presenceOffline
        }
    }
}

struct ChatDmConversationView: View {
    let peerUsername: String
    let messages: [ChatTimelineMessage]
    @Binding var composerText: String
    var isSending: Bool = false
    var isUploadingFile: Bool = false
    var onFileSelected: ((_ data: Data, _ fileName: String, _ mediaType: String) -> Void)? = nil
    var avatarDataBySenderID: (String) -> Data? = { _ in nil }
    var onRequestAvatar: ((String) -> Void)? = nil
    var onSend: () -> Void
    var onBack: () -> Void
    @State private var showFileImporter = false

    private var canSend: Bool {
        !composerText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

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
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(messages.enumerated()), id: \.element.id) { index, message in
                        let previous = index > 0 ? messages[index - 1] : nil
                        let next = index + 1 < messages.count ? messages[index + 1] : nil
                        ChatMessageRowView(
                            message: message,
                            previousMessage: previous,
                            nextMessage: next,
                            usesCompactConversationStyle: true,
                            avatarData: avatarDataBySenderID(message.senderID),
                            onRequestAvatar: onRequestAvatar
                        )
                    }
                }
                .padding(.vertical, 8)
            }
            .background(WaddleTheme.chatBackground)

            Divider()
                .overlay(WaddleTheme.divider)

            HStack(spacing: 10) {
                if isUploadingFile {
                    ProgressView()
                        .frame(width: 20, height: 20)
                } else {
                    Button {
                        showFileImporter = true
                    } label: {
                        Image(systemName: "paperclip")
                            .font(.body.weight(.medium))
                            .foregroundStyle(WaddleTheme.textSecondary)
                    }
                    .buttonStyle(.plain)
                }

                TextField("Message \(peerUsername)", text: $composerText, axis: .vertical)
                    .lineLimit(1...4)
                    .font(.body)
                    .foregroundStyle(WaddleTheme.textPrimary)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)

                Button(action: onSend) {
                    Image(systemName: "paperplane.fill")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(canSend ? .white : WaddleTheme.textMuted)
                        .frame(width: 34, height: 34)
                        .background(
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .fill(canSend ? WaddleTheme.accent : WaddleTheme.surfaceRaised)
                        )
                }
                .disabled(!canSend || isSending)
                .buttonStyle(.plain)
                .padding(.trailing, 10)
            }
            .background(
                WaddleTheme.composerBackground,
                in: RoundedRectangle(cornerRadius: 18, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(WaddleTheme.divider, lineWidth: 1)
            }
            .padding(12)
        }
        .fileImporter(
            isPresented: $showFileImporter,
            allowedContentTypes: [.item],
            allowsMultipleSelection: true
        ) { result in
            handleImportedChatAttachments(result, onFileSelected: onFileSelected)
        }
    }
}
