import SwiftUI
import WaddleKit

/// One person on the member list: avatar with presence, name, role line
/// and where they are. Opens their profile; the context menu holds the
/// moderation actions.
struct MemberCardRow: View {
    @Environment(NavigationModel.self) private var navigation
    let room: BareJID
    let card: MemberCard
    let model: RoomMembersModel

    var body: some View {
        Button {
            navigation.showProfile(PeerProfileRoute(
                name: card.name,
                jid: card.jid,
                room: card.isHere ? room : nil,
                nick: card.isHere ? card.nick : nil
            ))
        } label: {
            MemberCardLabel(
                name: card.name,
                avatarJID: card.jid,
                availability: card.isHere || card.place == .elsewhere ? card.availability : nil
            ) {
                MemberRoleLine(affiliation: card.affiliation, hats: card.hats)
                Text(MemberLabels.place(card.place))
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
        }
        .buttonStyle(MemberCardButtonStyle())
        .accessibilityHint(Text("Opens their profile"))
        .memberActions(
            MemberSubject(nick: card.nick, jid: card.jid, affiliation: card.affiliation, isPresent: card.isHere),
            room: room,
            model: model
        )
    }
}

/// A bot the room's server declares: its name and "Bot", no presence and
/// no actions.
struct BotCardRow: View {
    let bot: RoomBot

    var body: some View {
        MemberCardLabel(name: bot.displayName, avatarJID: bot.jid, availability: nil) {
            Text("Bot")
                .huddleText(Theme.Typography.eyebrow)
                .foregroundStyle(Theme.Colors.successText)
        }
        .memberCardSurface()
    }
}

/// Avatar, name, then the lines under it.
private struct MemberCardLabel<Lines: View>: View {
    let name: String
    let avatarJID: BareJID?
    /// Nil hides the presence dot: a bot, or someone who is not around.
    let availability: Availability?
    @ViewBuilder let lines: Lines

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            avatar
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(name)
                    .huddleText(Theme.Typography.author)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                lines
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var avatar: some View {
        let size = Theme.Size.memberAvatar
        if let avatarJID, let availability {
            PeerPresenceAvatar(jid: avatarJID, name: name, availability: availability, size: size)
        } else if let avatarJID {
            JIDAvatar(jid: avatarJID, name: name, size: size)
        } else {
            AvatarView(name: name, size: size)
                .overlay(alignment: .bottomTrailing) {
                    if let availability {
                        PresenceDot(availability: availability, size: size * 0.3)
                            .offset(x: size * 0.075, y: size * 0.075)
                    }
                }
        }
    }
}

/// Affiliation and hats in their colours (Android `RoleLine`): owner in
/// gold, admin in action, banned in danger, member muted.
struct MemberRoleLine: View {
    let affiliation: RoomAffiliation
    let hats: [Hat]

    var body: some View {
        let label = MemberLabels.badge(affiliation)
        let titles = hats.filter { $0.uri != Hat.botURI }.map(\.title)
        if label != nil || !titles.isEmpty {
            HStack(spacing: Theme.Spacing.xs) {
                if let label {
                    Text(label)
                        .foregroundStyle(MemberLabels.tone(affiliation))
                }
                ForEach(titles, id: \.self) { title in
                    Text(title)
                        .foregroundStyle(Theme.Colors.actionText)
                }
            }
            .huddleText(Theme.Typography.eyebrow)
            .lineLimit(1)
        }
    }
}

private struct MemberCardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .memberCardSurface(tint: configuration.isPressed ? Theme.State.press : .clear)
    }
}

private extension View {
    func memberCardSurface(tint: Color = .clear) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
        return padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .huddleSurface(.card, in: shape)
            .overlay(shape.fill(tint))
            .contentShape(shape)
    }
}
