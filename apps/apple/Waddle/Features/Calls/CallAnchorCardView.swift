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
    /// The way to the call's chat (`CallChat.linkTitle`); `nil` where there is none to offer.
    var chatLink: String?
    var openChat: @MainActor () -> Void = { /* a card with no chat */ }
    let join: @MainActor () -> Void

    @ScaledMetric(relativeTo: .body) private var glyph: CGFloat = 15
    @ScaledMetric(relativeTo: .body) private var glyphBox: CGFloat = 32

    var body: some View {
        // Beside the words where there is room; under them in a narrow pane or at a large text size.
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: Theme.Spacing.m) {
                words
                Spacer(minLength: Theme.Spacing.s)
                actionButton
            }
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                words
                actionButton
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
        // One thing to VoiceOver, as a message row is: the card in a sentence, and joining as its action.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(card.spoken))
        .accessibilityAddTraits(card.action == .join ? .isButton : [])
        .accessibilityAction { if card.action == .join { join() } }
        .accessibilityActions {
            if let chatLink { Button("Open call chat: \(chatLink)", action: openChat) }
        }
    }

    private var words: some View {
        HStack(alignment: .center, spacing: Theme.Spacing.m) {
            Image(systemName: card.hasVideo ? "video.fill" : "phone.fill")
                .font(.system(size: glyph, weight: .semibold))
                .foregroundStyle(card.isLive ? Theme.Colors.liveText : Theme.Colors.fgSoft)
                .frame(width: glyphBox, height: glyphBox)
                .background(Circle().fill(card.isLive ? Theme.Colors.liveSubtle : Theme.Colors.surface2))
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
                        .lineLimit(1)
                        .layoutPriority(1)
                    if let time {
                        Text(time)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgSoft)
                            .lineLimit(1)
                    }
                }
                Text(card.summary)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(2)
                if let chatLink {
                    Button(action: openChat) {
                        HStack(spacing: WaddleTokens.Spacing.s0Half) {
                            Text(chatLink)
                            Image(systemName: "chevron.right").imageScale(.small)
                        }
                        .huddleText(Theme.Typography.caption.weight(700))
                        .foregroundStyle(Theme.Colors.actionText)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .padding(.top, WaddleTokens.Spacing.s0Half)
                }
            }
        }
    }

    @ViewBuilder
    private var actionButton: some View {
        if let action = card.action {
            Button(action: join) {
                Label(action.title, systemImage: action == .join ? "phone.fill" : "phone.down.fill")
            }
            .buttonStyle(.huddle(.live, size: .small))
            .disabled(action != .join)
            .fixedSize()
        }
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
    @Environment(AppState.self) private var app
    @Environment(NavigationModel.self) private var navigation
    @Environment(MessageActionModel.self) private var actions
    private var placement = ConversationInspectorPlacement()
    let item: TimelineItem
    let anchor: CallThreadAnchor

    init(item: TimelineItem, anchor: CallThreadAnchor) {
        self.item = item
        self.anchor = anchor
    }

    var body: some View {
        // At the head of its own thread the card has nowhere further to lead.
        let thread = actions.isThread ? nil : item.message.thread
        CallAnchorCardView(
            card: CallAnchorCard.make(anchor: anchor, said: item.body, ended: item.callEnded, live: live, isNewest: isNewest),
            time: item.sentAt.formatted(date: .omitted, time: .shortened),
            chatLink: thread.map { CallChat.linkTitle(messages: chatMessages(in: $0)) },
            openChat: {
                guard let thread else { return }
                navigation.openThread(thread, in: item.conversation, usesInspector: placement.usesInspector)
            }
        ) {
            // As the room's banner joins: with the microphone, and the camera for the person to turn on.
            Task { await session.joinGroupCall(in: item.conversation.jid, video: false) }
        }
        // In the column the messages' words are in, past the avatars (`MessageRowContent`).
        .padding(.leading, avatarColumn)
        .padding(.horizontal, Theme.Spacing.l + Theme.Spacing.s)
        .padding(.vertical, Theme.Spacing.xs)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var avatarColumn: CGFloat {
        let isCompact = app.preferences.compactMessages
        return (isCompact ? Theme.Size.rowAvatar : Theme.Size.avatar) + (isCompact ? Theme.Spacing.s : Theme.Spacing.m)
    }

    /// The call going on in the room now, as the banner and "Live now" read it.
    private var live: CallAnchorCard.Live? {
        let room = item.conversation.jid
        let calls = session.calls
        // The call we are in is going on, whatever a room we are still
        // coming back into has had time to tell us of it.
        let isOurs = calls.state.isInGroupCall(in: room)
        guard isOurs || calls.groupPresence.hasCall(in: room) else { return nil }
        let rows = GroupCallRoster.rows(
            presence: calls.groupPresence.participants[room] ?? [], live: [], speaking: [], own: session.account.jid
        )
        return CallAnchorCard.Live(
            people: rows.isEmpty && isOurs ? ["You"] : rows.map { $0.isSelf ? "You" : $0.nick },
            hasVideo: calls.groupPresence.media[room]?.video ?? anchor.hasVideo,
            isOurs: isOurs,
            isBusy: calls.state.isLive,
            canJoin: session.canJoinGroupCall(in: room)
        )
    }

    /// The messages of the call's chat that are loaded and still there.
    private func chatMessages(in thread: String) -> Int {
        session.timelines.timeline(for: item.conversation).threadReplies(threadID: thread).count { $0.tombstone == nil }
    }

    /// No announcement of a later call in what is loaded of the room.
    private var isNewest: Bool {
        CallAnchorCard.newestAnnouncementID(in: session.timelines.timeline(for: item.conversation).items) == item.id
    }
}
