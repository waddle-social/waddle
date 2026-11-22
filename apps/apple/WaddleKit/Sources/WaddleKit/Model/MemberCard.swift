import Foundation

/// One person on a room's member list: who they are, their role and
/// where they are now.
public struct MemberCard: Identifiable, Hashable, Sendable {
    /// Where someone is, as the member list says it in words.
    public enum Place: Hashable, Sendable {
        /// In the room and available.
        case here
        /// In the room, away or extended away.
        case hereAway
        /// In the room, do not disturb.
        case hereBusy
        /// Not in the room but online.
        case elsewhere
        /// Not in the room and not online, or their presence is unknown.
        case notHere
    }

    public let name: String
    /// The occupant's nick, or an absent member's reserved nick.
    public let nick: String?
    public let jid: BareJID?
    public let affiliation: RoomAffiliation
    public let hats: [Hat]
    /// What the avatar's presence dot shows.
    public let availability: Availability
    public let place: Place

    public var id: String {
        if let jid { return "jid:\(jid)" }
        return "nick:\(nick ?? name)"
    }

    public var isHere: Bool {
        switch place {
        case .here, .hereAway, .hereBusy: return true
        case .elsewhere, .notHere: return false
        }
    }
}

extension MemberRoster {
    /// The room's member list: the people here now by name, then, once
    /// the affiliation lists are loaded, everyone else by affiliation and
    /// name. Bots are left out; they list apart, from `RoomBotStore`.
    public static func cards(
        present occupants: [Occupant],
        members: [RoomMember]?,
        contacts: [BareJID: PresenceStore.ContactPresence]
    ) -> [MemberCard] {
        let here = presentCards(occupants)
        let away = (members ?? [])
            .filter { !isPresent($0, among: occupants) }
            .sorted(by: memberOrder)
            .map { member in
                let availability = contacts[member.jid]?.availability ?? .offline
                return MemberCard(
                    name: displayName(of: member),
                    nick: member.nick,
                    jid: member.jid,
                    affiliation: member.affiliation,
                    hats: [],
                    availability: availability,
                    place: member.affiliation != .outcast && availability.isOnline ? .elsewhere : .notHere
                )
            }
        return here + away
    }

    private static func presentCards(_ occupants: [Occupant]) -> [MemberCard] {
        occupants
            .filter { !$0.isBot }
            .sorted { $0.nick.localizedCaseInsensitiveCompare($1.nick) == .orderedAscending }
            .map { occupant in
                MemberCard(
                    name: occupant.nick,
                    nick: occupant.nick,
                    jid: occupant.realJID,
                    affiliation: occupant.affiliation,
                    hats: occupant.hats,
                    availability: occupant.availability,
                    place: place(here: occupant.availability)
                )
            }
    }

    private static func place(here availability: Availability) -> MemberCard.Place {
        switch availability {
        case .away, .extendedAway: return .hereAway
        case .doNotDisturb: return .hereBusy
        case .available, .chat, .offline: return .here
        }
    }
}
