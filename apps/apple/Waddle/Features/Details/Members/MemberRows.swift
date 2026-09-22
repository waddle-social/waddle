import SwiftUI
import WaddleKit

/// A present occupant with moderation actions in its context menu.
struct OccupantMemberRow: View {
    let room: BareJID
    let occupant: Occupant
    let model: RoomMembersModel

    var body: some View {
        MemberRowLabel(
            name: occupant.nick,
            avatarJID: occupant.realJID,
            colorKey: "\(room)/\(occupant.nick)",
            availability: occupant.availability,
            status: occupant.status,
            hats: occupant.hats,
            affiliation: occupant.affiliation,
            isAbsent: false
        )
        .memberActions(
            MemberSubject(nick: occupant.nick, jid: occupant.realJID, affiliation: occupant.affiliation, isPresent: true),
            room: room,
            model: model
        )
    }
}

/// An affiliated member who is not in the room, shown offline.
struct AbsentMemberRow: View {
    let room: BareJID
    let member: RoomMember
    let model: RoomMembersModel

    var body: some View {
        MemberRowLabel(
            name: MemberRoster.displayName(of: member),
            avatarJID: member.jid,
            colorKey: member.jid.description,
            availability: .offline,
            status: nil,
            hats: [],
            affiliation: member.affiliation,
            isAbsent: true
        )
        .memberActions(
            MemberSubject(nick: member.nick, jid: member.jid, affiliation: member.affiliation, isPresent: false),
            room: room,
            model: model
        )
    }
}

/// Avatar with presence, nick, hats, status and affiliation badge.
struct MemberRowLabel: View {
    let name: String
    let avatarJID: BareJID?
    let colorKey: String
    let availability: Availability
    let status: String?
    let hats: [Hat]
    let affiliation: RoomAffiliation
    let isAbsent: Bool

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            avatar
                .overlay(alignment: .bottomTrailing) {
                    PresenceDot(availability: availability, size: 10)
                        .offset(x: 3, y: 3)
                }
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(spacing: Theme.Spacing.xs) {
                    Text(name)
                        .lineLimit(1)
                    ForEach(hats.prefix(2), id: \.uri) { hat in
                        HatChip(hat: hat)
                    }
                }
                if let secondary = secondaryLine {
                    Text(secondary)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: Theme.Spacing.s)
            if let badge = MemberLabels.badge(affiliation) {
                AffiliationBadge(text: badge)
            }
        }
        .opacity(isAbsent ? 0.65 : 1)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var avatar: some View {
        if let avatarJID {
            JIDAvatar(jid: avatarJID, name: name, size: Theme.Size.rowAvatar)
        } else {
            AvatarView(name: name, colorKey: colorKey, size: Theme.Size.rowAvatar)
        }
    }

    private var secondaryLine: String? {
        if let status = status?.trimmingCharacters(in: .whitespacesAndNewlines), !status.isEmpty {
            return status
        }
        return isAbsent ? "Offline" : nil
    }
}

/// XEP-0317 hat title in the hat's consistent color.
struct HatChip: View {
    let hat: Hat

    var body: some View {
        Text(hat.title)
            .font(.caption2.weight(.medium))
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .foregroundStyle(Color.consistent(for: hat.uri))
            .background(Capsule().fill(Color.consistent(for: hat.uri).opacity(0.15)))
    }
}

/// Owner / Admin / Member badge.
struct AffiliationBadge: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption2.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().strokeBorder(Color.secondary.opacity(0.4), lineWidth: 1))
    }
}
