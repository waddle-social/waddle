import SwiftUI
import WaddleKit

/// The people in a room's call as a row of faces, ours first: a ring for
/// whoever is speaking, a badge for a muted microphone or a raised hand.
/// It draws as many as fit and ends with "+N" for the rest.
struct GroupCallRosterStrip: View {
    let rows: [GroupCallRosterRow]
    var avatarSize = Theme.Size.avatar

    /// The most faces drawn, however wide the dock is.
    static let maxShown = 12

    var body: some View {
        ViewThatFits(in: .horizontal) {
            ForEach(candidates, id: \.self) { shown in
                strip(showing: shown)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("People in the call"))
    }

    /// How many faces to try, most first, down to one. "+1" would take the
    /// room of the face it hides, so it is never offered.
    private var candidates: [Int] {
        let all = rows.count
        guard all > 0 else { return [0] }
        return (1...min(all, Self.maxShown)).reversed().filter { $0 == all || $0 < all - 1 }
    }

    private func strip(showing shown: Int) -> some View {
        let hidden = rows.count - shown
        return HStack(spacing: Theme.Spacing.xs) {
            ForEach(rows.prefix(shown)) { row in
                GroupCallRosterFace(row: row, size: avatarSize)
            }
            if hidden > 0 {
                overflow(hidden)
            }
        }
        .fixedSize()
    }

    private func overflow(_ count: Int) -> some View {
        Text("+\(count)")
            .huddleText(Theme.Typography.badge)
            .monospacedDigit()
            .foregroundStyle(Theme.Colors.fgMuted)
            .frame(width: avatarSize, height: avatarSize)
            .background(Circle().fill(Theme.Colors.surface2))
            .padding(GroupCallRosterFace.ringInset)
            .accessibilityElement()
            .accessibilityLabel(Text(count == 1 ? "1 more person" : "\(count) more people"))
    }
}

/// One person's face on the roster strip.
struct GroupCallRosterFace: View {
    let row: GroupCallRosterRow
    let size: CGFloat
    @Environment(\.huddleGround) private var ground

    /// The room around a face for the speaking ring.
    static let ringInset = WaddleTokens.BorderWidth.strong * 2

    var body: some View {
        CallPersonAvatar(name: row.nick, jid: row.jid, size: size)
            .padding(Self.ringInset)
            .overlay {
                Circle()
                    .strokeBorder(Theme.Colors.live, lineWidth: WaddleTokens.BorderWidth.strong)
                    .opacity(row.isSpeaking ? 1 : 0)
            }
            .huddleAnimation(.press, value: row.isSpeaking)
            .overlay(alignment: .topTrailing) {
                if row.isHandRaised {
                    badge("hand.raised.fill", glyph: Theme.Colors.goldFg, fill: Theme.Colors.gold)
                }
            }
            .overlay(alignment: .bottomTrailing) {
                if row.isMuted {
                    badge("mic.slash.fill", glyph: Theme.Colors.dangerText, fill: Theme.Colors.surface2)
                }
            }
            .accessibilityElement()
            .accessibilityLabel(Text(label))
    }

    /// Cut into the face by a ring of the ground it sits on.
    private func badge(_ symbol: String, glyph: Color, fill: Color) -> some View {
        Image(systemName: symbol)
            .font(.system(size: Metrics.badgeGlyph, weight: .bold))
            .foregroundStyle(glyph)
            .frame(width: Metrics.badge, height: Metrics.badge)
            .background(Circle().fill(fill))
            .overlay(Circle().strokeBorder(ground, lineWidth: WaddleTokens.BorderWidth.strong))
            .accessibilityHidden(true)
    }

    private var label: String {
        var parts = [row.isSelf ? "You" : row.nick]
        if row.isSpeaking { parts.append("speaking") }
        if row.isHandRaised { parts.append("hand raised") }
        if row.isMuted { parts.append("muted") }
        return parts.joined(separator: ", ")
    }

    private enum Metrics {
        static let badge: CGFloat = 18
        static let badgeGlyph: CGFloat = 9
    }
}

/// A person's avatar, from the session's avatar store when there is one.
struct CallPersonAvatar: View {
    /// Absent in a snapshot, which draws the initials.
    @Environment(SessionCoordinator.self) private var session: SessionCoordinator?
    let name: String
    let jid: BareJID?
    let size: CGFloat

    var body: some View {
        if let jid, session != nil {
            JIDAvatar(jid: jid, name: name, size: size)
        } else {
            AvatarView(name: name, size: size)
        }
    }
}
