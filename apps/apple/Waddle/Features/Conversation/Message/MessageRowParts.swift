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
        status
            .sensoryFeedback(.error, trigger: isFailed) { _, failed in failed }
    }

    private var isFailed: Bool {
        if case .some(.failed) = session.deliveries.state(of: item.id) { return true }
        return false
    }

    @ViewBuilder
    private var status: some View {
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

/// The attached reply card under a thread root: reply faces, "N replies ·
/// time", then the newest loaded reply as "author: snippet". The whole
/// card is one button that opens the thread, as wide as the content
/// column. It publishes its bounds so the row can draw the thread rail
/// from the avatar gutter into it (`MessageThreadRail`). Re-rendered each
/// minute so the relative time stays current.
struct MessageThreadCard: View {
    let summary: ThreadSummary
    let action: () -> Void

    var body: some View {
        TimelineView(.everyMinute) { _ in
            Button(action: action) {
                label
            }
            .buttonStyle(MessageThreadCardStyle())
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(MessageThreadCardText.accessibilityLabel(summary)))
            .accessibilityAddTraits(.isButton)
        }
        .anchorPreference(key: MessageThreadCardBoundsKey.self, value: .bounds) { $0 }
        .padding(.top, Theme.Spacing.xxs)
    }

    private var label: some View {
        HStack(spacing: Theme.Spacing.s) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(spacing: Theme.Spacing.s - 2) {
                    if !summary.faces.isEmpty {
                        MessageThreadFaces(faces: summary.faces, overflow: summary.otherParticipantCount)
                    }
                    Text(MessageThreadCardText.count(summary.replyCount))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Color.accentColor)
                        .lineLimit(1)
                    if let latest = summary.latestReply {
                        Text("·")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(MessageThreadCardText.time(latest.sentAt))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                if let latest = summary.latestReply {
                    latestLine(latest)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
        }
    }

    private func latestLine(_ latest: ThreadSummary.LatestReply) -> Text {
        let snippet = Text(MessageThreadCardText.snippet(latest.snippet))
        guard MessageThreadCardText.showsAuthor(latest) else { return snippet }
        return Text("\(MessageThreadCardText.author(latest)): ").fontWeight(.semibold) + snippet
    }
}

/// Raised card surface with a hairline border, and the design system's
/// hover and press tints.
private struct MessageThreadCardStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        MessageThreadCardSurface(configuration: configuration)
    }
}

private struct MessageThreadCardSurface: View {
    let configuration: ButtonStyleConfiguration
    @State private var isHovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        configuration.label
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.vertical, Theme.Spacing.s - 2)
            .frame(maxWidth: .infinity, minHeight: Theme.Size.minHitTarget, alignment: .leading)
            .background(shape.fill(Color.secondaryBackground))
            .overlay(shape.fill(Color.primary.opacity(highlight)))
            .overlay(shape.strokeBorder(Color.cardBorder, lineWidth: 1))
            .contentShape(shape)
            .onHover { isHovering = $0 }
    }

    private var highlight: Double {
        if configuration.isPressed { return 0.08 }
        return isHovering ? 0.04 : 0
    }
}

/// The reply card's bounds, for the row's thread rail.
struct MessageThreadCardBoundsKey: PreferenceKey {
    static var defaultValue: Anchor<CGRect>? { nil }

    static func reduce(value: inout Anchor<CGRect>?, nextValue: () -> Anchor<CGRect>?) {
        value = value ?? nextValue()
    }
}

/// The thread rail in the avatar gutter: down from under the avatar,
/// then across into the reply card's leading edge. Overlaid on the row's
/// content (gutter and column), so `gutterX` and `top` are in that space.
struct MessageThreadRail: View {
    let card: Anchor<CGRect>?
    /// Horizontal center of the avatar gutter.
    let gutterX: CGFloat
    /// Where the rail starts, below whatever the gutter shows.
    let top: CGFloat

    /// Down to the middle of the card's first line.
    private static let joinDepth: CGFloat = 20

    var body: some View {
        GeometryReader { proxy in
            if let card {
                let rect = proxy[card]
                MessageThreadElbow(
                    start: CGPoint(x: gutterX, y: top),
                    end: CGPoint(x: rect.minX, y: rect.minY + min(Self.joinDepth, rect.height / 2))
                )
                .stroke(Color.cardBorder, style: StrokeStyle(lineWidth: 1.5, lineCap: .round, lineJoin: .round))
            }
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Down from `start`, then a rounded corner across to `end`.
private struct MessageThreadElbow: Shape {
    let start: CGPoint
    let end: CGPoint

    func path(in rect: CGRect) -> Path {
        var path = Path()
        guard end.x > start.x, end.y > start.y else { return path }
        let radius = min(8, end.x - start.x, end.y - start.y)
        path.move(to: start)
        path.addLine(to: CGPoint(x: start.x, y: end.y - radius))
        path.addQuadCurve(
            to: CGPoint(x: start.x + radius, y: end.y),
            control: CGPoint(x: start.x, y: end.y)
        )
        path.addLine(to: end)
        return path
    }
}

/// Up to three overlapping reply authors, then "+N".
private struct MessageThreadFaces: View {
    let faces: [ThreadSummary.Participant]
    let overflow: Int

    private static let size: CGFloat = 18

    var body: some View {
        HStack(spacing: -Self.size * 0.3) {
            ForEach(Array(faces.enumerated()), id: \.offset) { index, face in
                avatar(face)
                    .overlay(
                        RoundedRectangle(cornerRadius: Self.size * 0.3, style: .continuous)
                            .strokeBorder(Color.secondaryBackground, lineWidth: 1.5)
                    )
                    .zIndex(Double(faces.count - index))
            }
            if overflow > 0 {
                Text("+\(overflow)")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .padding(.leading, Self.size * 0.3 + 2)
            }
        }
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private func avatar(_ face: ThreadSummary.Participant) -> some View {
        if let jid = face.jid {
            JIDAvatar(jid: jid, name: face.name, size: Self.size)
        } else {
            AvatarView(name: face.name, colorKey: face.colorKey, size: Self.size)
        }
    }
}

extension View {
    /// Wraps this view in a plain button that opens `name`'s profile. The
    /// hit area grows by `horizontalSlop` on each side and by `topSlop`
    /// and `bottomSlop`, toward the minimum tap target, without moving the
    /// layout. Callers keep the slop inside empty space they own, so the
    /// grown area never covers a neighbouring control.
    func authorProfileButton(
        name: String,
        horizontalSlop: CGFloat,
        topSlop: CGFloat,
        bottomSlop: CGFloat,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            self
                .padding(.horizontal, horizontalSlop)
                .padding(.top, topSlop)
                .padding(.bottom, bottomSlop)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, -horizontalSlop)
        .padding(.top, -topSlop)
        .padding(.bottom, -bottomSlop)
        .accessibilityLabel(Text("\(name), open profile"))
        .help(Text("Open profile"))
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
