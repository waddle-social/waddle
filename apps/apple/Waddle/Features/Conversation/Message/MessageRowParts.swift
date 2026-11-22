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
            HStack(spacing: WaddleTokens.Spacing.s1Half) {
                Image(systemName: "arrowshape.turn.up.left.fill")
                    .huddleText(Theme.Typography.badge)
                    .foregroundStyle(Theme.Colors.actionText)
                    .accessibilityHidden(true)
                Text(MessageReplySummary.author(parent: parent, target: target, isRoom: isRoom))
                    .huddleText(Theme.Typography.caption.weight(700))
                    .foregroundStyle(Theme.Colors.actionText)
                    .lineLimit(1)
                Text(MessageReplySummary.snippet(parent: parent))
                    .huddleText(Theme.Typography.caption)
                    .italic()
                    .foregroundStyle(Theme.Colors.fgMuted.opacity(0.8))
                    .lineLimit(1)
            }
            .padding(.horizontal, Theme.Spacing.s)
            .frame(minHeight: WaddleTokens.ControlHeight.xs)
            .background(MessageReplyChipSurface())
            .fixedSize(horizontal: false, vertical: true)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(parent == nil)
    }
}

/// The reply chip's ground: surface 2 with a 3pt action rail on the leading
/// edge (web `replyChip`).
private struct MessageReplyChipSurface: View {
    var body: some View {
        let shape = RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous)
        shape
            .fill(Theme.Colors.surface2)
            .overlay(alignment: .leading) {
                Theme.Colors.actionText.frame(width: Theme.Size.rail)
            }
            .clipShape(shape)
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
                .opacity(0.7)
        }
        .huddleText(Theme.Typography.body.sized(WaddleTokens.FontSize.sm))
        .foregroundStyle(Theme.Colors.fgMuted)
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
            .huddleText(Theme.Typography.timestamp)
            .foregroundStyle(Theme.Colors.fgSoft)
        case .some(.queued):
            Label("Sends when you're back online", systemImage: "clock")
                .huddleText(Theme.Typography.timestamp)
                .foregroundStyle(Theme.Colors.fgSoft)
        case .some(.failed):
            failed
        default:
            EmptyView()
        }
    }

    private var failed: some View {
        HStack(spacing: Theme.Spacing.s) {
            Label("Not sent", systemImage: "exclamationmark.circle.fill")
                .foregroundStyle(Theme.Colors.dangerText)
            Button("Retry") {
                Task { await session.retry(clientID: item.id) }
            }
            Button("Discard", role: .destructive) {
                session.discard(clientID: item.id, in: item.conversation)
            }
        }
        .huddleText(Theme.Typography.timestamp)
        .tint(Theme.Colors.actionText)
        .buttonStyle(.borderless)
    }
}

/// The reply summary under a thread root (web `threadCard`): reply
/// faces, "N replies · time", then the newest loaded reply as "author:
/// snippet". The whole summary is one button that opens the thread.
/// Re-rendered each minute so the relative time stays current.
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
        .padding(.top, Theme.Spacing.xs)
    }

    private var label: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(spacing: WaddleTokens.Spacing.s1Half) {
                if !summary.faces.isEmpty {
                    MessageThreadFaces(faces: summary.faces, overflow: summary.otherParticipantCount)
                }
                Text(MessageThreadCardText.count(summary.replyCount))
                    .huddleText(Theme.Typography.caption.weight(700))
                    .foregroundStyle(Theme.Colors.actionText)
                    .lineLimit(1)
                if let latest = summary.latestReply {
                    Text("· \(MessageThreadCardText.time(latest.sentAt))")
                        .huddleText(Theme.Typography.caption)
                        .monospacedDigit()
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                }
            }
            if let latest = summary.latestReply {
                latestLine(latest)
                    .huddleText(Theme.Typography.body.sized(WaddleTokens.FontSize.sm))
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
        }
    }

    private func latestLine(_ latest: ThreadSummary.LatestReply) -> Text {
        let snippet = MessageThreadCardText.snippet(latest.snippet)
        guard MessageThreadCardText.showsAuthor(latest) else { return Text(snippet) }
        let author = Text("\(MessageThreadCardText.author(latest)):").fontWeight(.bold).foregroundStyle(Theme.Colors.fg)
        return Text("\(author) \(snippet)")
    }
}

/// Under a reply inside a thread: "2 replies in sub-thread", which opens
/// the sub-thread rooted at that reply.
struct MessageSubThreadLink: View {
    let count: Int
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(MessageSubThreadText.count(count), systemImage: "arrow.turn.down.right")
                .huddleText(Theme.Typography.caption.weight(600))
                .foregroundStyle(Theme.Colors.actionText)
                .padding(.vertical, Theme.Spacing.xxs)
                .huddleTouchTarget()
        }
        .buttonStyle(.plain)
        .accessibilityHint(Text("Opens the sub-thread"))
    }
}

/// No ground until hovered or pressed; the summary sits flush with the
/// body text, its padding hanging into the gutter.
private struct MessageThreadCardStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        MessageThreadCardSurface(configuration: configuration)
    }
}

private struct MessageThreadCardSurface: View {
    let configuration: ButtonStyleConfiguration
    @State private var isHovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous)
        configuration.label
            .padding(.horizontal, Theme.Spacing.s)
            .padding(.vertical, Theme.Spacing.xs)
            .background(shape.fill(tint))
            .contentShape(shape)
            .padding(.leading, -Theme.Spacing.s)
            .onHover { isHovering = $0 }
    }

    private var tint: Color {
        if configuration.isPressed { return Theme.State.press }
        return isHovering ? Theme.State.hover : .clear
    }
}

/// Overlapping thread participants, then "+N": on the reply summary and
/// in the thread header. Each face cuts into the ground it sits on.
struct MessageThreadFaces: View {
    @Environment(\.huddleGround) private var ground
    let faces: [ThreadSummary.Participant]
    let overflow: Int
    var size: CGFloat = 20

    var body: some View {
        HStack(spacing: -WaddleTokens.Spacing.s1Half) {
            ForEach(Array(faces.enumerated()), id: \.offset) { index, face in
                avatar(face)
                    .padding(2)
                    .background(Circle().fill(ground))
                    .padding(-2)
                    .zIndex(Double(faces.count - index))
            }
            if overflow > 0 {
                Text("+\(overflow)")
                    .huddleText(Theme.Typography.tag)
                    .monospacedDigit()
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .padding(.horizontal, Theme.Spacing.xs)
                    .frame(minWidth: size, minHeight: size)
                    .background(Capsule().fill(Theme.Colors.surface2))
                    .padding(2)
                    .background(Capsule().fill(ground))
                    .padding(-2)
            }
        }
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private func avatar(_ face: ThreadSummary.Participant) -> some View {
        if let jid = face.jid {
            JIDAvatar(jid: jid, name: face.name, size: size)
        } else {
            AvatarView(name: face.name, size: size)
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
            VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                Label {
                    Text(preview.url.host ?? preview.url.absoluteString)
                } icon: {
                    Image(systemName: "arrow.up.forward.square")
                }
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .lineLimit(1)
                if let title = preview.title, !title.isEmpty {
                    Text(title)
                        .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.md).weight(700))
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(2)
                }
                if let summary = preview.summary, !summary.isEmpty {
                    Text(summary)
                        .huddleText(Theme.Typography.caption.sized(WaddleTokens.FontSize.sm))
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(3)
                }
                if let imageURL = preview.imageURL {
                    previewImage(imageURL)
                        .padding(.top, Theme.Spacing.xs)
                }
            }
            .multilineTextAlignment(.leading)
            .fixedSize(horizontal: false, vertical: true)
            .padding(Theme.Spacing.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .fill(Theme.Colors.surface)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
            )
            .contentShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        }
        .buttonStyle(.plain)
        .frame(maxWidth: Theme.Size.mediaMaxWidth + Theme.Spacing.m * 2, alignment: .leading)
        .accessibilityLabel(Text(preview.title ?? preview.url.absoluteString))
        .accessibilityAddTraits(.isLink)
    }

    private func previewImage(_ url: URL) -> some View {
        AsyncImage(url: url) { image in
            image
                .resizable()
                .scaledToFill()
        } placeholder: {
            Theme.Colors.surface2
        }
        .frame(maxWidth: .infinity, minHeight: 120, maxHeight: 160)
        .clipShape(RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous))
        .accessibilityLabel(Text(preview.imageAlt ?? ""))
    }
}
