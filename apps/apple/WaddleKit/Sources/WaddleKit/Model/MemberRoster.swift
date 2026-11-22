import Foundation

/// Builds a room's member list (`cards(present:members:contacts:)`). The
/// room's bots are not here: `RoomBotStore` holds the ones the server
/// declares.
public enum MemberRoster {
    public static func displayName(of member: RoomMember) -> String {
        if let nick = member.nick?.trimmingCharacters(in: .whitespacesAndNewlines), !nick.isEmpty {
            return nick
        }
        return member.jid.localpart ?? member.jid.domain
    }

    /// Applies a successful affiliation change to a loaded member list.
    public static func updating(_ members: [RoomMember], jid: BareJID, to affiliation: RoomAffiliation) -> [RoomMember] {
        guard affiliation != .none else { return members.filter { $0.jid != jid } }
        guard let index = members.firstIndex(where: { $0.jid == jid }) else {
            return members + [RoomMember(jid: jid, nick: nil, affiliation: affiliation)]
        }
        var updated = members
        let existing = members[index]
        updated[index] = RoomMember(jid: existing.jid, nick: existing.nick, affiliation: affiliation)
        return updated
    }

    /// An occupant exposes the member's real JID or uses its reserved nick.
    static func isPresent(_ member: RoomMember, among occupants: [Occupant]) -> Bool {
        occupants.contains { occupant in
            occupant.realJID == member.jid
                || member.nick.map { $0.caseInsensitiveCompare(occupant.nick) == .orderedSame } == true
        }
    }

    static func memberOrder(_ lhs: RoomMember, _ rhs: RoomMember) -> Bool {
        if lhs.affiliation != rhs.affiliation { return lhs.affiliation < rhs.affiliation }
        return displayName(of: lhs).localizedCaseInsensitiveCompare(displayName(of: rhs)) == .orderedAscending
    }
}
