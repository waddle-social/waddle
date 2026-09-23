import SwiftUI
import WaddleKit

/// The XEP-0461 quote above a reply; tapping scrolls to the parent when it
/// is loaded.
struct MessageReplyPreview: View {
    let target: WireMessage.ReplyTarget
    let parent: TimelineItem?
    let isRoom: Bool
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: Theme.Spacing.s - 2) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Color.secondary.opacity(0.45))
                    .frame(width: 3)
                Image(systemName: "arrowshape.turn.up.left.fill")
                    .font(.caption2)
                    .accessibilityHidden(true)
                Text(MessageReplySummary.author(parent: parent, target: target, isRoom: isRoom))
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                Text(MessageReplySummary.snippet(parent: parent))
                    .font(.caption)
                    .lineLimit(1)
            }
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(parent == nil)
    }
}

/// XEP-0424 / XEP-0425 placeholder for removed content.
struct MessageTombstoneView: View {
    let tombstone: Tombstone

    var body: some View {
        Label {
            Text(text)
                .italic()
        } icon: {
            Image(systemName: symbol)
        }
        .font(.callout)
        .foregroundStyle(.secondary)
    }

    private var text: String {
        switch tombstone {
        case .retracted:
            return "This message was deleted"
        case let .moderated(_, reason):
            if let reason, !reason.isEmpty {
                return "Removed by a moderator: \(reason)"
            }
            return "Removed by a moderator"
        }
    }

    private var symbol: String {
        switch tombstone {
        case .retracted: return "trash"
        case .moderated: return "shield.lefthalf.filled"
        }
    }
}

/// Delivery state of an own send: sending, queued while offline, or
/// failed with retry and discard.
struct MessageDeliveryView: View {
    @Environment(SessionCoordinator.self) private var session
    let item: TimelineItem

    var body: some View {
        switch session.deliveries.state(of: item.id) {
        case .some(.sending):
            HStack(spacing: Theme.Spacing.xs) {
                ProgressView().controlSize(.mini)
                Text("Sending…")
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        case .some(.queued):
            Label("Waiting for connection", systemImage: "clock")
                .font(.caption2)
                .foregroundStyle(.secondary)
        case .some(.failed):
            failed
        default:
            EmptyView()
        }
    }

    private var failed: some View {
        HStack(spacing: Theme.Spacing.s) {
            Label("Not sent", systemImage: "exclamationmark.circle.fill")
                .foregroundStyle(Color.red)
            Button("Retry") {
                Task { await session.retry(clientID: item.id) }
            }
            Button("Discard", role: .destructive) {
                session.discard(clientID: item.id, in: item.conversation)
            }
        }
        .font(.caption)
        .buttonStyle(.borderless)
    }
}

/// "3 replies" under a thread root.
struct MessageThreadChip: View {
    let count: Int
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.Spacing.xs) {
                Image(systemName: "bubble.left.and.bubble.right")
                    .accessibilityHidden(true)
                Text(count == 1 ? "1 reply" : "\(count) replies")
                Image(systemName: "chevron.right")
                    .font(.caption2)
                    .accessibilityHidden(true)
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(Color.accentColor)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint(Text("Opens the thread"))
    }
}

/// Server-attached link card; opens the page.
struct MessageLinkPreviewCard: View {
    @Environment(\.openURL) private var openURL
    let preview: LinkPreview

    var body: some View {
        Button {
            openURL(preview.url)
        } label: {
            HStack(alignment: .top, spacing: Theme.Spacing.s + 2) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Color.accentColor.opacity(0.6))
                    .frame(width: 3)
                VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                    Text(preview.url.host ?? preview.url.absoluteString)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    if let title = preview.title, !title.isEmpty {
                        Text(title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Color.accentColor)
                            .lineLimit(2)
                    }
                    if let summary = preview.summary, !summary.isEmpty {
                        Text(summary)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    }
                    if let imageURL = preview.imageURL {
                        previewImage(imageURL)
                    }
                }
            }
            .multilineTextAlignment(.leading)
            .fixedSize(horizontal: false, vertical: true)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .frame(maxWidth: Theme.Size.mediaMaxWidth + 40, alignment: .leading)
        .accessibilityLabel(Text(preview.title ?? preview.url.absoluteString))
        .accessibilityAddTraits(.isLink)
    }

    private func previewImage(_ url: URL) -> some View {
        AsyncImage(url: url) { image in
            image
                .resizable()
                .scaledToFill()
        } placeholder: {
            Color.secondary.opacity(0.1)
        }
        .frame(maxWidth: Theme.Size.mediaMaxWidth, minHeight: 120, maxHeight: 160)
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous))
        .accessibilityLabel(Text(preview.imageAlt ?? ""))
    }
}
