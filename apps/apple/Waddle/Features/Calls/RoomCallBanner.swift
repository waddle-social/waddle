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
        let hasCall = calls.groupPresence.hasCall(in: room)
        ZStack {
            if hasCall, !calls.state.isInGroupCall(in: room) {
                RoomCallBannerView(
                    rows: GroupCallRoster.rows(
                        presence: calls.groupPresence.participants[room] ?? [], live: [], speaking: [], own: session.account.jid
                    ),
                    canJoin: session.canJoinGroupCall(in: room),
                    onJoin: { Task { await session.joinGroupCall(in: room, video: false) } }
                )
                .transition(.opacity)
            }
        }
        .huddleAnimation(.settle, value: hasCall)
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
    /// The glyph grows with the text beside it.
    @ScaledMetric(relativeTo: .body) private var glyph: CGFloat = 15
    @ScaledMetric(relativeTo: .body) private var glyphBox: CGFloat = 20

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
                .font(.system(size: glyph, weight: .semibold))
                .foregroundStyle(Theme.Colors.liveText)
                .frame(width: glyphBox, height: glyphBox)
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
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Call in progress, \(people): \(names)"))
    }

    /// Who is there, for VoiceOver: the faces are only pictures.
    private var names: String {
        let shown = rows.prefix(Self.maxFaces).map { $0.isSelf ? "You" : $0.nick }
        let rest = rows.count - shown.count
        return (rest > 0 ? shown + ["\(rest) more"] : shown).formatted(.list(type: .and))
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
}
