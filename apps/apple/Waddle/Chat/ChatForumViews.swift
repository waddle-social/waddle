import SwiftUI

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
                            .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12, style: .continuous)
                                    .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                            )
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
                    .background(WaddleTheme.ownMessageBubble, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(WaddleTheme.accent.opacity(0.15), lineWidth: 0.5)
                    )

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
                .overlay(WaddleTheme.divider)

            HStack(spacing: 10) {
                TextField("Reply to thread…", text: $replyText, axis: .vertical)
                    .lineLimit(1...4)
                    .font(.body)
                    .foregroundStyle(WaddleTheme.textPrimary)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)

                Button(action: onSendReply) {
                    Image(systemName: "paperplane.fill")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? WaddleTheme.textMuted : .white)
                        .frame(width: 34, height: 34)
                        .background(
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .fill(replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? WaddleTheme.surfaceRaised : WaddleTheme.accent)
                        )
                }
                .disabled(replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSending)
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
    }
}
