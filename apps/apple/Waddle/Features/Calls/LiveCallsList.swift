import SwiftUI
import WaddleKit

/// The sidebar's "Live now" (web `CallActivityDock`): a card for each call
/// going on in a room of ours that we are not in, and the way into it.
///
/// Pure: everything it shows is passed in.
struct LiveCallsList: View {
    let calls: [LiveCall]
    let choose: @MainActor (LiveCall) -> Void

    /// Faces drawn before the count says the rest.
    static let maxFaces = 3

    var body: some View {
        VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
            Text(LiveCalls.heading(count: calls.count))
                .huddleText(Theme.Typography.sidebarSection)
                .foregroundStyle(Theme.Colors.liveText)
                .padding(.leading, Theme.Spacing.xs)
                .accessibilityAddTraits(.isHeader)
                .accessibilityLabel(Text("Live now, \(calls.count)"))
            ForEach(calls) { call in
                LiveCallCard(call: call) { choose(call) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Active calls"))
    }
}

private struct LiveCallCard: View {
    let call: LiveCall
    let action: @MainActor () -> Void
    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: WaddleTokens.Spacing.s2Half) {
                VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
                    HStack(spacing: WaddleTokens.Spacing.s1Half) {
                        Circle()
                            .fill(Theme.Colors.live)
                            .frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                        Text(call.title)
                            .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                            .foregroundStyle(Theme.Colors.fg)
                            .lineLimit(1)
                    }
                    HStack(spacing: Theme.Spacing.xs) {
                        faces
                        // How many is what matters: it is not the part to cut short.
                        ViewThatFits(in: .horizontal) {
                            Text(call.meta).lineLimit(1)
                            Text(call.headcount).lineLimit(1)
                        }
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                    }
                }
                Spacer(minLength: 0)
                Text(call.action.title)
                    .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                    .foregroundStyle(Theme.Colors.liveText)
                    .padding(.horizontal, WaddleTokens.Spacing.s2Half)
                    .padding(.vertical, Theme.Spacing.xs)
                    .background(Capsule().fill(Theme.Colors.live.opacity(isHovered ? Metrics.pillHoverOpacity : Metrics.pillOpacity)))
                    .overlay(Capsule().strokeBorder(Theme.Colors.live.opacity(Metrics.pillBorderOpacity)))
            }
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.vertical, WaddleTokens.Spacing.s2Half)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: Metrics.corner, style: .continuous).fill(Theme.Colors.liveSubtle))
            .contentShape(RoundedRectangle(cornerRadius: Metrics.corner, style: .continuous))
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .environment(\.huddleGround, Theme.Colors.liveSubtle)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(call.spoken))
        .accessibilityAddTraits(.isButton)
        .help(call.spoken)
    }

    private var faces: some View {
        HStack(spacing: -Theme.Spacing.xs) {
            ForEach(call.people.prefix(LiveCallsList.maxFaces)) { person in
                CallPersonAvatar(name: person.nick, jid: person.jid, size: Metrics.face)
                    .padding(WaddleTokens.BorderWidth.strong)
                    .background(Circle().fill(Theme.Colors.bgRail))
            }
        }
    }

    private enum Metrics {
        /// Web: the card's 14 px radius.
        static let corner: CGFloat = 14
        static let face: CGFloat = 18
        static let pillBorderOpacity = 0.35
        static let pillOpacity = 0.12
        static let pillHoverOpacity = 0.24
    }
}

/// Reads the rooms with a call out of the session for `LiveCallsList`, and
/// joins or opens the one chosen. Draws nothing while no other call is on.
struct LiveCallsHost: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// Taller than this and the cards scroll: the rooms' own list must keep its place.
    private static let maxHeight: CGFloat = 232

    var body: some View {
        let calls = LiveCalls.list(rooms, state: session.calls.state)
        ZStack {
            if !calls.isEmpty {
                let list = LiveCallsList(calls: calls, choose: choose)
                    .padding(.horizontal, Theme.Spacing.m)
                    .padding(.vertical, Theme.Spacing.s)
                ViewThatFits(in: .vertical) {
                    list
                    ScrollView { list }
                }
                .frame(maxHeight: Self.maxHeight)
                .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .huddleAnimation(.settle, value: calls.map(\.id))
        .onChange(
            of: LiveCalls.Change(count: calls.count, ownCall: session.calls.state.sid, isOnline: session.connection == .online)
        ) { before, after in
            guard LiveCalls.isNews(before: before, after: after) else { return }
            // Said when there is a pause: it is not worth interrupting anything for.
            var text = AttributedString(LiveCalls.announcement(count: after.count, besidesOurs: after.ownCall != nil))
            text.accessibilitySpeechAnnouncementPriority = .low
            AccessibilityNotification.Announcement(text).post()
        }
    }

    private var rooms: [LiveCalls.Room] {
        let presence = session.calls.groupPresence
        return presence.participants.map { room, participants in
            LiveCalls.Room(
                room: room,
                title: ConversationHeaderText.make(for: .room(room), session: session).title,
                people: GroupCallRoster.rows(presence: participants, live: [], speaking: [], own: session.account.jid),
                hasVideo: presence.media[room]?.video ?? false,
                canJoin: session.canJoinGroupCall(in: room)
            )
        }
    }

    private func choose(_ call: LiveCall) {
        navigation.open(.room(call.room))
        guard call.action == .join else { return }
        // As the room's own banner joins: with the microphone, and the camera for the person to turn on.
        Task { await session.joinGroupCall(in: call.room, video: false) }
    }
}
