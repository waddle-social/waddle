import SwiftUI
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

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

                    ChatMessageAttachmentsView(message: message, maxWidth: 300)
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
                let senders = reactions[emoji] ?? []
                HStack(spacing: 3) {
                    Text(emoji).font(.caption)
                    Text("\(count)").font(.caption2.weight(.medium)).foregroundStyle(WaddleTheme.textSecondary)
                }
                .padding(.horizontal, 7)
                .padding(.vertical, 3)
                .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(WaddleTheme.divider))
                .help(senders.isEmpty ? "" : senders.joined(separator: ", "))
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
