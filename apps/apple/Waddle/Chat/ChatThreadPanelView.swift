import SwiftUI
import UniformTypeIdentifiers

struct ChatThreadPanelView: View {
    let root: ChatTimelineMessage
    let replies: [ChatTimelineMessage]
    @Binding var composerText: String
    var isSending: Bool = false
    var isUploadingFile: Bool = false
    var canGoBack: Bool = false
    var threadChildCount: (String) -> Int = { _ in 0 }
    var onOpenNestedThread: ((ChatTimelineMessage) -> Void)? = nil
    var onBack: (() -> Void)? = nil
    var onFileSelected: ((_ data: Data, _ fileName: String, _ mediaType: String) -> Void)? = nil
    var onSend: () -> Void
    var onClose: () -> Void
    /// Forwarded through to each `ChatMessageRowView` so thread replies
    /// render XEP-0084 PEP avatars just like the main timeline.
    var avatarDataBySenderID: (String) -> Data? = { _ in nil }
    var onRequestAvatar: ((String) -> Void)? = nil
    @State private var showFileImporter = false

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
        .fileImporter(
            isPresented: $showFileImporter,
            allowedContentTypes: [.item],
            allowsMultipleSelection: true
        ) { result in
            handleImportedChatAttachments(result, onFileSelected: onFileSelected)
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
            if isUploadingFile {
                ProgressView()
                    .frame(width: 20, height: 20)
            } else {
                Button {
                    showFileImporter = true
                } label: {
                    Image(systemName: "paperclip")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                }
                .buttonStyle(.plain)
            }

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
