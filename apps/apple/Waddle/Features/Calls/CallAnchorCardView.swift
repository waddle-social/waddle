import SwiftUI
import WaddleKit

/// A room's call announcement in its conversation (web `CallAnchorCard`):
/// while the call goes on, who is in it and a way in; afterwards, that it
/// ended and how long it ran.
///
/// Pure: everything it shows is passed in.
struct CallAnchorCardView: View {
    let card: CallAnchorCard
    /// The hour it began, as the rows around it show theirs.
    var time: String?
    let join: @MainActor () -> Void

    @ScaledMetric(relativeTo: .body) private var glyph: CGFloat = 15
    @ScaledMetric(relativeTo: .body) private var glyphBox: CGFloat = 32

    var body: some View {
        HStack(alignment: .center, spacing: Theme.Spacing.m) {
            Image(systemName: card.hasVideo ? "video.fill" : "phone.fill")
                .font(.system(size: glyph, weight: .semibold))
                .foregroundStyle(card.isLive ? Theme.Colors.liveText : Theme.Colors.fgSoft)
                .frame(width: glyphBox, height: glyphBox)
                .background(Circle().fill(card.isLive ? Theme.Colors.liveSubtle : Theme.Colors.surface2))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s0Half) {
                HStack(spacing: WaddleTokens.Spacing.s1Half) {
                    if card.isLive {
                        Circle()
                            .fill(Theme.Colors.live)
                            .frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                    }
                    Text(card.title)
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(card.isLive ? Theme.Colors.fg : Theme.Colors.fgMuted)
                    if let time {
                        Text(time)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgSoft)
                    }
                }
                Text(card.summary)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(2)
            }
            Spacer(minLength: Theme.Spacing.s)
            if let action = card.action {
                Button(action: join) {
                    Label(action.title, systemImage: action == .join ? "phone.fill" : "phone.down.fill")
                }
                .buttonStyle(.huddle(.live, size: .small))
                .disabled(action != .join)
            }
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.vertical, card.isLive ? Theme.Spacing.m : Theme.Spacing.s)
        .frame(maxWidth: Metrics.maxWidth, alignment: .leading)
        .background(shape.fill(card.isLive ? Theme.Colors.surface : Color.clear))
        .overlay(shape.strokeBorder(
            card.isLive ? Theme.Colors.borderSoft : Theme.Colors.border,
            style: StrokeStyle(lineWidth: WaddleTokens.BorderWidth.hair, dash: card.isLive ? [] : Metrics.dash)
        ))
        .opacity(card.isLive ? 1 : Metrics.endedOpacity)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(card.spoken))
    }

    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: Metrics.corner, style: .continuous) }

    private enum Metrics {
        /// Web: the card's 460 px limit and 16 px radius.
        static let maxWidth: CGFloat = 460
        static let corner: CGFloat = 16
        static let dash: [CGFloat] = [4, 3]
        static let endedOpacity = 0.7
    }
}

/// The feed row for a room's call announcement: reads the room's call out
/// of the session for `CallAnchorCardView`, and joins it.
struct CallAnchorRow: View {
    @Environment(SessionCoordinator.self) private var session
    let item: TimelineItem
    let anchor: CallThreadAnchor

    var body: some View {
        CallAnchorCardView(
            card: CallAnchorCard.make(anchor: anchor, said: item.body, ended: item.callEnded, live: live, isNewest: isNewest),
            time: item.sentAt.formatted(date: .omitted, time: .shortened)
        ) {
            // As the room's banner joins: with the microphone, and the camera for the person to turn on.
            Task { await session.joinGroupCall(in: item.conversation.jid, video: false) }
        }
        .padding(.leading, Theme.Spacing.l)
        .padding(.trailing, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.xs)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The call going on in the room now, as the banner and "Live now" read it.
    private var live: CallAnchorCard.Live? {
        let room = item.conversation.jid
        let calls = session.calls
        guard calls.groupPresence.hasCall(in: room) else { return nil }
        let rows = GroupCallRoster.rows(
            presence: calls.groupPresence.participants[room] ?? [], live: [], speaking: [], own: session.account.jid
        )
        return CallAnchorCard.Live(
            people: rows.map { $0.isSelf ? "You" : $0.nick },
            hasVideo: calls.groupPresence.media[room]?.video ?? anchor.hasVideo,
            isOurs: calls.state.isInGroupCall(in: room),
            isBusy: calls.state.isLive,
            canJoin: session.canJoinGroupCall(in: room)
        )
    }

    /// No later announcement in what is loaded of the room.
    private var isNewest: Bool {
        session.timelines.timeline(for: item.conversation).items.last { $0.callAnchor != nil }?.id == item.id
    }
}
