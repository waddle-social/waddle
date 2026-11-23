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
                nick: card.isHere ? card.nick : nil,
                affiliation: card.isHere ? nil : card.affiliation
            ))
        } label: {
            MemberCardLabel(name: card.name, avatarJID: card.jid, availability: card.shownAvailability) {
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
            PeerPresenceAvatar(jid: avatarJID, name: name, availability: availability, size: Theme.Size.memberAvatar)
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

}

/// Affiliation and up to two hats in their colours (Android `RoleLine`):
/// owner in gold, admin and hats in action, banned in danger, the rest
/// muted. Gold and action text are one colour, so owner, admin and hats
/// look alike, as in the Penpot member frames: the word tells them apart.
struct MemberRoleLine: View {
    let affiliation: RoomAffiliation
    let hats: [Hat]

    var body: some View {
        let titles = hats.filter { $0.uri != Hat.botURI }.prefix(2).map(\.title)
        HStack(spacing: Theme.Spacing.xs) {
            Text(MemberLabels.role(affiliation))
                .foregroundStyle(tone)
            ForEach(titles, id: \.self) { title in
                Text(title)
                    .foregroundStyle(Theme.Colors.actionText)
            }
        }
        .huddleText(Theme.Typography.eyebrow)
        .lineLimit(1)
    }

    private var tone: Color {
        switch affiliation {
        case .owner: return Theme.Colors.goldText
        case .admin: return Theme.Colors.actionText
        case .outcast: return Theme.Colors.dangerText
        case .member, .none: return Theme.Colors.fgMuted
        }
    }
}

private struct MemberCardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        MemberCardButtonBody(configuration: configuration)
    }
}

/// The card with the press tint, and the hover tint under a pointer.
private struct MemberCardButtonBody: View {
    let configuration: ButtonStyleConfiguration
    @State private var isHovered = false

    var body: some View {
        configuration.label
            .memberCardSurface(tint: tint)
            .onHover { isHovered = $0 }
    }

    private var tint: Color {
        if configuration.isPressed { return Theme.State.press }
        return isHovered ? Theme.State.hover : .clear
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
