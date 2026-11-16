import SwiftUI
import WaddleKit

/// A room's call that we are not in, at the top of its conversation: who
/// is there and a way in. Absent while the room has no call, or while our
/// call slot already holds this room's call.
struct RoomCallBanner: View {
    @Environment(SessionCoordinator.self) private var session
    let room: BareJID

    var body: some View {
        let calls = session.calls
        let presence = calls.groupPresence.participants[room] ?? []
        ZStack {
            if !presence.isEmpty, !calls.state.isInGroupCall(in: room) {
                RoomCallBannerView(
                    rows: GroupCallRoster.rows(presence: presence, live: [], speaking: [], own: session.account.jid),
                    canJoin: !calls.state.isLive && session.connection == .online,
                    onJoin: { session.joinRoomCall(in: room, video: false) }
                )
                .transition(.opacity)
            }
        }
        .huddleAnimation(.settle, value: presence.isEmpty)
    }
}

/// The banner itself. Pure: everything it shows is passed in.
struct RoomCallBannerView: View {
    /// Who is in the call (`GroupCallRoster`).
    let rows: [GroupCallRosterRow]
    /// No other call is in the slot and the session is online.
    var canJoin = true
    let onJoin: @MainActor () -> Void

    /// Faces drawn before the count says the rest.
    static let maxFaces = 3

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: Theme.Spacing.m) {
                summary
                Spacer(minLength: Theme.Spacing.s)
                faces
                joinButton
            }
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                summary
                HStack(spacing: Theme.Spacing.m) {
                    faces
                    Spacer(minLength: Theme.Spacing.s)
                    joinButton
                }
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.s)
        .background(Theme.Colors.liveSubtle)
        .overlay(alignment: .bottom) { HuddleDivider() }
        .environment(\.huddleGround, Theme.Colors.liveSubtle)
        .accessibilityElement(children: .contain)
    }

    private var summary: some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: "waveform")
                .font(.system(size: Metrics.glyph, weight: .semibold))
                .foregroundStyle(Theme.Colors.liveText)
                .frame(width: Metrics.glyphBox, height: Metrics.glyphBox)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 0) {
                Text("Call in progress")
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.liveText)
                Text(people)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }

    /// A few overlapping faces; the count beside them says how many in all.
    private var faces: some View {
        HStack(spacing: -Theme.Spacing.xs) {
            ForEach(rows.prefix(Self.maxFaces)) { row in
                CallPersonAvatar(name: row.nick, jid: row.jid, size: Theme.Size.smallAvatar)
                    .padding(WaddleTokens.BorderWidth.strong)
                    .background(Circle().fill(Theme.Colors.liveSubtle))
            }
        }
        .accessibilityHidden(true)
    }

    private var joinButton: some View {
        Button(action: onJoin) {
            Label("Join", systemImage: "phone.fill")
        }
        .buttonStyle(.huddle(.live, size: .small))
        .disabled(!canJoin)
        .accessibilityLabel(Text("Join call"))
        .help("Join call")
    }

    private var people: String {
        rows.count == 1 ? "1 person" : "\(rows.count) people"
    }

    private enum Metrics {
        static let glyph: CGFloat = 15
        static let glyphBox: CGFloat = 20
    }
}

extension CallState {
    /// The slot holds `room`'s call, being joined or joined.
    func isInGroupCall(in room: BareJID) -> Bool {
        switch self {
        case .joiningGroup(let call): call.room == room
        case .inGroup(let call): call.room == room
        case .idle, .incoming, .outgoing, .active, .ended: false
        }
    }
}

extension SessionCoordinator {
    /// Starts or joins `room`'s call under the nick we hold there, or the
    /// account's own when the room has not told us one. The call shows in
    /// the dock above the shell.
    func joinRoomCall(in room: BareJID, video: Bool) {
        let calls = calls
        let nick = presence.selfNicks[room] ?? account.nick
        Task { _ = await calls.joinGroupCall(in: room, media: CallMedia(audio: true, video: video), nick: nick) }
    }
}
